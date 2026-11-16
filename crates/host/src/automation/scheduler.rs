//! Keeps every automation's timer, fires its ticks, and records what each tick did.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use super::schedule::Schedule;
use super::{
    automation_after_run, did_run, is_one_shot_schedule, patched_sentence, permitted_policy, quiet_sentence, quiet_tick, schedule_field_count, Automation, AutomationAuthority, AutomationDraft, AutomationError,
    AutomationPatch, AutomationRunStatus, AutomationView, TickKind,
};

/// Where automations are kept between launches. Writes are small, so they run inline.
pub trait AutomationStore: Send + Sync {
    fn list_automations(&self) -> Result<Vec<Automation>, AutomationError>;
    fn save_automation(&self, automation: &Automation) -> Result<(), AutomationError>;
    fn delete_automation(&self, id: &str) -> Result<(), AutomationError>;
}

/// Resolves when the scheduled run reaches a terminal state, so overrun protection can hold the next tick.
pub type AutomationDispatch = Arc<dyn Fn(Automation, TickKind) -> Pin<Box<dyn Future<Output = AutomationRunStatus> + Send>> + Send + Sync>;

pub type AutomationListener = Arc<dyn Fn(Vec<AutomationView>) + Send + Sync>;

/// The answer to a manual run, which declines while a run of the automation is still in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunNow {
    Ran(AutomationRunStatus),
    Busy,
}

/// The wall clock a timer re-reads at least this often, so a machine that slept fires on waking.
const MAX_DELAY: Duration = Duration::from_secs(30);

struct Timer {
    schedule: Schedule,
    handle: JoinHandle<()>,
    /// The last moment this timer fired for, so a one-shot never fires twice.
    fired: Arc<StdMutex<Option<DateTime<Utc>>>>,
    /// While this timer's own tick runs, which is what the overrun guard holds its next tick on.
    busy: Arc<AtomicBool>,
}

impl Timer {
    fn next_run(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let fired = *self.fired.lock().unwrap();
        let next = self.schedule.next_after(now)?;
        if fired.is_some_and(|fired| next <= fired) {
            return None;
        }
        Some(next)
    }
}

struct State {
    automations: HashMap<String, Automation>,
    timers: HashMap<String, Timer>,
    stopped: bool,
}

struct Inner {
    store: Arc<dyn AutomationStore>,
    dispatch: AutomationDispatch,
    listener: Option<AutomationListener>,
    now: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
    state: StdMutex<State>,
    /// Store mutations settle in order so concurrent requests cannot replace each other's updates.
    changes: Mutex<()>,
    /// One run per automation at a time, whether a tick or the button started it.
    firing: StdMutex<HashSet<String>>,
}

#[derive(Clone)]
pub struct AutomationScheduler {
    inner: Arc<Inner>,
}

pub struct SchedulerOptions {
    pub listener: Option<AutomationListener>,
    pub now: Option<Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>>,
}

impl AutomationScheduler {
    pub fn new(store: Arc<dyn AutomationStore>, dispatch: AutomationDispatch, options: SchedulerOptions) -> Self {
        Self {
            inner: Arc::new(Inner {
                store,
                dispatch,
                listener: options.listener,
                now: options.now.unwrap_or_else(|| Arc::new(Utc::now)),
                state: StdMutex::new(State { automations: HashMap::new(), timers: HashMap::new(), stopped: true }),
                changes: Mutex::new(()),
                firing: StdMutex::new(HashSet::new()),
            }),
        }
    }

    /// Rebuilds the timers from storage. Ticks missed while the app was closed are not replayed.
    /// Must be called within a Tokio runtime.
    pub async fn start(&self) -> Result<(), AutomationError> {
        let _change = self.inner.changes.lock().await;
        let automations = self.inner.store.list_automations()?;
        let unarmed: Vec<String> = {
            let mut state = self.inner.state.lock().unwrap();
            state.stopped = false;
            for timer in state.timers.drain() {
                timer.1.handle.abort();
            }
            state.automations = automations.into_iter().map(|automation| (automation.id.clone(), automation)).collect();
            let all: Vec<Automation> = state.automations.values().cloned().collect();
            all.into_iter().filter(|automation| !self.arm(&mut state, automation)).map(|automation| automation.id).collect()
        };
        for id in unarmed {
            self.mark_missed(&id)?;
        }
        self.notify();
        Ok(())
    }

    pub fn stop(&self) {
        let mut state = self.inner.state.lock().unwrap();
        state.stopped = true;
        for (_, timer) in state.timers.drain() {
            timer.handle.abort();
        }
    }

    pub fn list(&self) -> Vec<AutomationView> {
        let state = self.inner.state.lock().unwrap();
        self.views(&state)
    }

    pub fn for_thread(&self, task_id: &str) -> Option<AutomationView> {
        let state = self.inner.state.lock().unwrap();
        find(&state, task_id).map(|automation| self.view(&state, automation))
    }

    /// One automation per thread: creating a second one replaces the first.
    pub async fn save(&self, draft: AutomationDraft, authority: Option<AutomationAuthority<'_>>) -> Result<AutomationView, AutomationError> {
        let _change = self.inner.changes.lock().await;
        let policy = permitted_policy(draft.policy, authority)?;
        assert_schedule(&draft.schedule, draft.timezone.as_deref())?;
        let existing = {
            let state = self.inner.state.lock().unwrap();
            find(&state, &draft.task_id).cloned()
        };
        let at = (self.inner.now)().timestamp_millis();
        // Rewriting a schedule leaves its quiet where it was: an empty sentence is how the quiet is taken off.
        let kept = draft.surface_when.clone().or_else(|| existing.as_ref().and_then(|existing| existing.surface_when.clone()));
        let surface_when = quiet_sentence(&draft.schedule, draft.surface_when.as_deref(), kept)?.filter(|sentence| !sentence.is_empty());
        let ends_when = draft.ends_when.clone().or_else(|| existing.as_ref().and_then(|existing| existing.ends_when.clone())).filter(|sentence| !sentence.is_empty());
        let automation = Automation {
            id: existing.as_ref().map(|existing| existing.id.clone()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            task_id: draft.task_id,
            prompt: draft.prompt,
            schedule: draft.schedule,
            timezone: draft.timezone,
            policy,
            surface_when,
            ends_when,
            paused: draft.paused.unwrap_or(false),
            created_at: existing.as_ref().map(|existing| existing.created_at).unwrap_or(at),
            updated_at: at,
            run_count: existing.as_ref().map(|existing| existing.run_count).unwrap_or(0),
            last_run_at: existing.as_ref().and_then(|existing| existing.last_run_at),
            last_status: existing.as_ref().and_then(|existing| existing.last_status),
            last_status_at: existing.as_ref().and_then(|existing| existing.last_status_at),
            consecutive_declines: existing.as_ref().and_then(|existing| existing.consecutive_declines),
            overrun_count: existing.as_ref().and_then(|existing| existing.overrun_count),
        };
        self.commit(automation)
    }

    pub async fn update(&self, task_id: &str, patch: AutomationPatch, authority: Option<AutomationAuthority<'_>>) -> Result<AutomationView, AutomationError> {
        let _change = self.inner.changes.lock().await;
        let existing = {
            let state = self.inner.state.lock().unwrap();
            find(&state, task_id).cloned()
        }
        .ok_or_else(|| AutomationError::new("This task has no automation."))?;
        let pausing_only = patch.only_pauses();
        let policy = if pausing_only {
            if let Some(authority) = authority {
                authority()?;
            }
            existing.policy
        } else {
            permitted_policy(patch.policy.or(existing.policy), authority)?
        };
        let schedule = patch.schedule.clone().unwrap_or_else(|| existing.schedule.clone());
        let timezone = patch.timezone.clone().or_else(|| existing.timezone.clone());
        if patch.schedule.is_some() || patch.timezone.is_some() {
            assert_schedule(&schedule, timezone.as_deref())?;
        }
        let surface_when = quiet_sentence(&schedule, patch.surface_when.as_deref(), patched_sentence(patch.surface_when.as_deref(), existing.surface_when.as_deref()))?;
        let ends_when = patched_sentence(patch.ends_when.as_deref(), existing.ends_when.as_deref());
        let automation = Automation {
            surface_when: surface_when.filter(|sentence| !sentence.is_empty()),
            ends_when: ends_when.filter(|sentence| !sentence.is_empty()),
            prompt: patch.prompt.unwrap_or_else(|| existing.prompt.clone()),
            schedule,
            timezone,
            policy,
            paused: patch.paused.unwrap_or(existing.paused),
            updated_at: (self.inner.now)().timestamp_millis(),
            ..existing
        };
        self.commit(automation)
    }

    pub async fn remove(&self, task_id: &str) -> Result<bool, AutomationError> {
        let _change = self.inner.changes.lock().await;
        self.remove_stored(task_id)
    }

    /// Fires outside the schedule. Declines while a run is still in flight, like a real tick would.
    /// The button is the only way here, so the user is watching by construction and it is loud.
    pub async fn run_now(&self, task_id: &str) -> Result<RunNow, AutomationError> {
        let id = {
            let state = self.inner.state.lock().unwrap();
            find(&state, task_id).map(|automation| automation.id.clone())
        }
        .ok_or_else(|| AutomationError::new("This task has no automation."))?;
        let ticking = self.inner.state.lock().unwrap().timers.get(&id).is_some_and(|timer| timer.busy.load(Ordering::SeqCst));
        if self.inner.firing.lock().unwrap().contains(&id) || ticking {
            return Ok(RunNow::Busy);
        }
        Ok(RunNow::Ran(fire(&self.inner, &id, true).await))
    }

    /// Waits for every mutation already queued to settle.
    pub async fn flush(&self) {
        let _change = self.inner.changes.lock().await;
    }

    fn commit(&self, automation: Automation) -> Result<AutomationView, AutomationError> {
        self.inner.store.save_automation(&automation)?;
        let view = {
            let mut state = self.inner.state.lock().unwrap();
            state.automations.insert(automation.id.clone(), automation.clone());
            if !state.stopped {
                self.arm(&mut state, &automation);
            }
            self.view(&state, &automation)
        };
        self.notify();
        Ok(view)
    }

    fn remove_stored(&self, task_id: &str) -> Result<bool, AutomationError> {
        remove_stored(&self.inner, task_id)
    }

    fn mark_missed(&self, id: &str) -> Result<(), AutomationError> {
        mark_missed(&self.inner, id)
    }

    /// False when the schedule cannot fire again, which must never fail `start`.
    fn arm(&self, state: &mut State, automation: &Automation) -> bool {
        arm(&self.inner, state, automation)
    }

    fn view(&self, state: &State, automation: &Automation) -> AutomationView {
        view(&self.inner, state, automation)
    }

    fn views(&self, state: &State) -> Vec<AutomationView> {
        views(&self.inner, state)
    }

    fn notify(&self) {
        notify(&self.inner);
    }
}

fn find<'a>(state: &'a State, task_id: &str) -> Option<&'a Automation> {
    state.automations.values().find(|automation| automation.task_id == task_id)
}

fn view(inner: &Inner, state: &State, automation: &Automation) -> AutomationView {
    let next = if automation.paused { None } else { state.timers.get(&automation.id).and_then(|timer| timer.next_run((inner.now)())) };
    AutomationView { automation: automation.clone(), next_run_at: next.map(|next| next.timestamp_millis()) }
}

fn views(inner: &Inner, state: &State) -> Vec<AutomationView> {
    let mut views: Vec<AutomationView> = state.automations.values().map(|automation| view(inner, state, automation)).collect();
    views.sort_by(|left, right| right.automation.updated_at.cmp(&left.automation.updated_at));
    views
}

fn notify(inner: &Inner) {
    let Some(listener) = &inner.listener else { return };
    let views = {
        let state = inner.state.lock().unwrap();
        if state.stopped {
            return;
        }
        views(inner, &state)
    };
    listener(views);
}

fn arm(inner: &Arc<Inner>, state: &mut State, automation: &Automation) -> bool {
    if let Some(timer) = state.timers.remove(&automation.id) {
        timer.handle.abort();
    }
    let Ok(schedule) = Schedule::parse(&automation.schedule, automation.timezone.as_deref()) else { return false };
    if schedule.next_after((inner.now)()).is_none() {
        return false;
    }
    let fired = Arc::new(StdMutex::new(None));
    let busy = Arc::new(AtomicBool::new(false));
    let handle = tokio::spawn(tick_loop(Arc::downgrade(inner), automation.id.clone(), schedule.clone(), fired.clone(), busy.clone()));
    state.timers.insert(automation.id.clone(), Timer { schedule, handle, fired, busy });
    true
}

/// Waits for each firing on the wall clock, croner-style: a tick that arrives while the previous run
/// is still going is dropped and counted, and a paused schedule lets its ticks pass.
async fn tick_loop(inner: Weak<Inner>, id: String, schedule: Schedule, fired: Arc<StdMutex<Option<DateTime<Utc>>>>, busy: Arc<AtomicBool>) {
    loop {
        let Some(scheduler) = inner.upgrade() else { return };
        let now = (scheduler.now)();
        let reference = fired.lock().unwrap().map_or(now, |fired| fired.max(now));
        let Some(target) = schedule.next_after(reference) else { return };
        if fired.lock().unwrap().is_some_and(|fired| target <= fired) {
            return;
        }
        drop(scheduler);
        loop {
            let Some(scheduler) = inner.upgrade() else { return };
            let now = (scheduler.now)();
            drop(scheduler);
            if now >= target {
                break;
            }
            let wait = (target - now).to_std().unwrap_or_default().min(MAX_DELAY);
            tokio::time::sleep(wait).await;
        }
        let Some(scheduler) = inner.upgrade() else { return };
        *fired.lock().unwrap() = Some(target);
        let paused = scheduler.state.lock().unwrap().automations.get(&id).is_none_or(|automation| automation.paused);
        if paused {
            continue;
        }
        if busy.load(Ordering::SeqCst) {
            let counting = scheduler.clone();
            let overrun = id.clone();
            tokio::spawn(async move {
                if let Err(error) = count_overrun(&counting, &overrun).await {
                    tracing::error!("Could not record automation overrun: {error}");
                }
            });
            continue;
        }
        busy.store(true, Ordering::SeqCst);
        let running = scheduler.clone();
        let ticking = id.clone();
        let done = busy.clone();
        tokio::spawn(async move {
            fire(&running, &ticking, false).await;
            done.store(false, Ordering::SeqCst);
        });
    }
}

async fn fire(inner: &Arc<Inner>, id: &str, manual: bool) -> AutomationRunStatus {
    let automation = {
        let state = inner.state.lock().unwrap();
        if state.stopped {
            return AutomationRunStatus::Skipped;
        }
        let Some(automation) = state.automations.get(id).cloned() else { return AutomationRunStatus::Skipped };
        automation
    };
    if !inner.firing.lock().unwrap().insert(id.to_string()) {
        return AutomationRunStatus::Skipped;
    }
    let tick = TickKind { quiet: quiet_tick(&automation, manual), unattended: !manual };
    let status = (inner.dispatch)(automation, tick).await;
    if !inner.state.lock().unwrap().stopped {
        let _change = inner.changes.lock().await;
        if let Err(error) = record_run(inner, id, status) {
            tracing::error!("Could not record automation result: {error}");
        }
    }
    inner.firing.lock().unwrap().remove(id);
    status
}

fn record_run(inner: &Arc<Inner>, id: &str, status: AutomationRunStatus) -> Result<(), AutomationError> {
    // The run itself may have deleted the automation once its stop condition was met.
    let Some(current) = inner.state.lock().unwrap().automations.get(id).cloned() else { return Ok(()) };
    let updated = automation_after_run(&current, status, (inner.now)().timestamp_millis());
    inner.store.save_automation(&updated)?;
    let can_fire_again = {
        let mut state = inner.state.lock().unwrap();
        state.automations.insert(id.to_string(), updated.clone());
        if state.stopped {
            return Ok(());
        }
        state.timers.get(id).is_some_and(|timer| timer.next_run((inner.now)()).is_some())
    };
    // A spent schedule is only finished if this tick actually ran; otherwise the moment was missed.
    if can_fire_again {
        notify(inner);
    } else if did_run(status) {
        remove_stored(inner, &updated.task_id)?;
    } else {
        mark_missed(inner, id)?;
    }
    Ok(())
}

async fn count_overrun(inner: &Arc<Inner>, id: &str) -> Result<(), AutomationError> {
    let _change = inner.changes.lock().await;
    let Some(mut counted) = inner.state.lock().unwrap().automations.get(id).cloned() else { return Ok(()) };
    counted.overrun_count = Some(counted.overrun_count.unwrap_or(0) + 1);
    counted.updated_at = (inner.now)().timestamp_millis();
    inner.store.save_automation(&counted)?;
    inner.state.lock().unwrap().automations.insert(id.to_string(), counted);
    notify(inner);
    Ok(())
}

fn remove_stored(inner: &Arc<Inner>, task_id: &str) -> Result<bool, AutomationError> {
    let Some(existing) = find(&inner.state.lock().unwrap(), task_id).cloned() else { return Ok(false) };
    inner.store.delete_automation(&existing.id)?;
    {
        let mut state = inner.state.lock().unwrap();
        if let Some(timer) = state.timers.remove(&existing.id) {
            timer.handle.abort();
        }
        state.automations.remove(&existing.id);
    }
    notify(inner);
    Ok(true)
}

/// Keeps a one-shot that can no longer fire, plainly marked, so it never disappears unrun.
fn mark_missed(inner: &Arc<Inner>, id: &str) -> Result<(), AutomationError> {
    let Some(automation) = inner.state.lock().unwrap().automations.get(id).cloned() else { return Ok(()) };
    if automation.last_status == Some(AutomationRunStatus::Missed) {
        return Ok(());
    }
    let missed = automation_after_run(&automation, AutomationRunStatus::Missed, (inner.now)().timestamp_millis());
    inner.store.save_automation(&missed)?;
    inner.state.lock().unwrap().automations.insert(id.to_string(), missed);
    notify(inner);
    Ok(())
}

/// Rejects anything croner cannot parse, sub-minute cadence, and one-shots that can never fire.
pub fn assert_schedule(schedule: &str, timezone: Option<&str>) -> Result<(), AutomationError> {
    if !is_one_shot_schedule(schedule) && schedule_field_count(schedule) > 5 {
        return Err(AutomationError::new("Automations run at most once a minute, so seconds are not supported. Use a five-field cron expression."));
    }
    let parsed = Schedule::parse(schedule, timezone).map_err(|message| AutomationError::new(format!("\"{schedule}\" is not a valid schedule: {message}")))?;
    if parsed.next_after(Utc::now()).is_none() {
        return Err(AutomationError::new(format!("\"{schedule}\" has no future run.")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::ExecutionPolicy;

    #[derive(Default)]
    struct MemoryStore {
        saved: StdMutex<HashMap<String, Automation>>,
    }

    impl AutomationStore for MemoryStore {
        fn list_automations(&self) -> Result<Vec<Automation>, AutomationError> {
            Ok(self.saved.lock().unwrap().values().cloned().collect())
        }
        fn save_automation(&self, automation: &Automation) -> Result<(), AutomationError> {
            self.saved.lock().unwrap().insert(automation.id.clone(), automation.clone());
            Ok(())
        }
        fn delete_automation(&self, id: &str) -> Result<(), AutomationError> {
            self.saved.lock().unwrap().remove(id);
            Ok(())
        }
    }

    fn draft(task: &str, schedule: &str) -> AutomationDraft {
        AutomationDraft { task_id: task.into(), prompt: "check".into(), schedule: schedule.into(), ..Default::default() }
    }

    fn scheduler(store: Arc<MemoryStore>, status: AutomationRunStatus, hold: Arc<tokio::sync::Notify>) -> AutomationScheduler {
        let dispatch: AutomationDispatch = Arc::new(move |_, _| {
            let hold = hold.clone();
            Box::pin(async move {
                hold.notified().await;
                status
            })
        });
        AutomationScheduler::new(store, dispatch, SchedulerOptions { listener: None, now: None })
    }

    #[tokio::test]
    async fn one_automation_per_thread_and_quiet_survives_a_rewrite() {
        let store = Arc::new(MemoryStore::default());
        let scheduler = scheduler(store.clone(), AutomationRunStatus::Succeeded, Arc::new(tokio::sync::Notify::new()));
        scheduler.start().await.unwrap();
        let first = scheduler.save(AutomationDraft { surface_when: Some("an error".into()), ..draft("t", "0 8 * * *") }, None).await.unwrap();
        let second = scheduler.save(draft("t", "0 9 * * *"), None).await.unwrap();
        assert_eq!(first.automation.id, second.automation.id);
        assert_eq!(second.automation.surface_when.as_deref(), Some("an error"));
        assert!(second.next_run_at.is_some());
        assert_eq!(scheduler.list().len(), 1);
        let loud = scheduler.update("t", AutomationPatch { surface_when: Some(String::new()), ..Default::default() }, None).await.unwrap();
        assert_eq!(loud.automation.surface_when, None);
        let paused = scheduler.update("t", AutomationPatch { paused: Some(true), ..Default::default() }, None).await.unwrap();
        assert_eq!(paused.next_run_at, None);
        assert!(scheduler.remove("t").await.unwrap());
        assert!(store.saved.lock().unwrap().is_empty());
        scheduler.stop();
    }

    #[tokio::test]
    async fn refuses_seconds_quiet_one_shots_and_stronger_policies() {
        let scheduler = scheduler(Arc::new(MemoryStore::default()), AutomationRunStatus::Succeeded, Arc::new(tokio::sync::Notify::new()));
        scheduler.start().await.unwrap();
        assert!(scheduler.save(draft("t", "* * * * * *"), None).await.unwrap_err().0.contains("seconds are not supported"));
        let future = (Utc::now() + chrono::Duration::days(1)).to_rfc3339();
        let quiet = AutomationDraft { surface_when: Some("x".into()), ..draft("t", &future) };
        assert!(scheduler.save(quiet, None).await.unwrap_err().0.contains("always surfaces"));
        assert!(scheduler.save(draft("t", "2000-01-01T00:00:00Z"), None).await.unwrap_err().0.contains("has no future run"));
        let ceiling = || Ok(ExecutionPolicy::Confirm);
        let strong = AutomationDraft { policy: Some(ExecutionPolicy::Bypass), ..draft("t", "0 8 * * *") };
        assert!(scheduler.save(strong, Some(&ceiling)).await.is_err());
        scheduler.stop();
    }

    #[tokio::test]
    async fn a_manual_run_counts_and_a_second_one_meanwhile_is_busy() {
        let hold = Arc::new(tokio::sync::Notify::new());
        let scheduler = scheduler(Arc::new(MemoryStore::default()), AutomationRunStatus::Succeeded, hold.clone());
        scheduler.start().await.unwrap();
        scheduler.save(draft("t", "0 8 * * *"), None).await.unwrap();
        let running = tokio::spawn({
            let scheduler = scheduler.clone();
            async move { scheduler.run_now("t").await.unwrap() }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(scheduler.run_now("t").await.unwrap(), RunNow::Busy);
        hold.notify_one();
        assert_eq!(running.await.unwrap(), RunNow::Ran(AutomationRunStatus::Succeeded));
        let view = scheduler.for_thread("t").unwrap();
        assert_eq!(view.automation.run_count, 1);
        assert_eq!(view.automation.last_status, Some(AutomationRunStatus::Succeeded));
        scheduler.stop();
    }

    #[tokio::test]
    async fn a_spent_one_shot_is_kept_as_missed_on_start() {
        let store = Arc::new(MemoryStore::default());
        let past = Automation {
            id: "a".into(),
            task_id: "t".into(),
            prompt: "p".into(),
            schedule: "2000-01-01T00:00:00Z".into(),
            timezone: None,
            policy: None,
            surface_when: None,
            ends_when: None,
            paused: false,
            created_at: 0,
            updated_at: 0,
            run_count: 0,
            last_run_at: None,
            last_status: None,
            last_status_at: None,
            consecutive_declines: None,
            overrun_count: None,
        };
        store.save_automation(&past).unwrap();
        let scheduler = scheduler(store.clone(), AutomationRunStatus::Succeeded, Arc::new(tokio::sync::Notify::new()));
        scheduler.start().await.unwrap();
        assert_eq!(store.saved.lock().unwrap()["a"].last_status, Some(AutomationRunStatus::Missed));
        assert_eq!(scheduler.list()[0].next_run_at, None);
        scheduler.stop();
    }

    #[tokio::test]
    async fn a_one_shot_that_ran_removes_itself() {
        let store = Arc::new(MemoryStore::default());
        let dispatch: AutomationDispatch = Arc::new(|_, tick| {
            assert!(tick.unattended && !tick.quiet);
            Box::pin(async { AutomationRunStatus::Succeeded })
        });
        let scheduler = AutomationScheduler::new(store.clone(), dispatch, SchedulerOptions { listener: None, now: None });
        scheduler.start().await.unwrap();
        let soon = (Utc::now() + chrono::Duration::milliseconds(300)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        scheduler.save(draft("t", &soon), None).await.unwrap();
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if store.saved.lock().unwrap().is_empty() {
                break;
            }
        }
        assert!(store.saved.lock().unwrap().is_empty());
        assert!(scheduler.list().is_empty());
        scheduler.stop();
    }
}
