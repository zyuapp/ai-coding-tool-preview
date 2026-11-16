//! One warm session per thread, shared by a channel's engines so the cap bounds their processes together.

use std::any::Any;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use futures::future::{join_all, BoxFuture, FutureExt, Shared};
use tokio::sync::watch;
use tokio::task::AbortHandle;

use crate::provider::{ProviderResult, RunInput};
use crate::session_resources::{in_resource_scope, in_resource_scope_sync, release_resource_scope, report_resource_busy, resource_scope, ResourceScope};
use crate::types::CoordinationRole;

/// How long a thread's session waits, with nothing left running under it, before giving its process back.
pub const IDLE_SESSION: Duration = Duration::from_secs(5 * 60);
/// How many threads keep a session warm. Beyond this the least recently used idle one is let go.
pub const MAX_LIVE_SESSIONS: usize = 3;

/// What the pool asks of a session, whichever engine it runs on. None of these may call back into
/// the pool synchronously except through the hooks the session was opened with.
pub trait PooledSession: Send + Sync + 'static {
    fn key(&self) -> String;
    fn live(&self) -> bool;
    /// Anything closing the session would cut short.
    fn busy(&self) -> bool;
    /// A turn is in flight, so the session owes an answer before it can take another.
    fn answering(&self) -> bool;
    fn continues(&self, continuation: Option<&str>) -> bool;
    fn run(self: Arc<Self>, input: RunInput) -> BoxFuture<'static, ProviderResult>;
    /// Idempotent. Starts closing at once; the future settles once the process has exited.
    fn close(&self) -> BoxFuture<'static, ()>;
    fn as_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync>;
}

/// What a session is built with: `ended` forgets it, `rested` restarts its idle clock.
#[derive(Clone)]
pub struct SessionHooks {
    pub ended: Arc<dyn Fn() + Send + Sync>,
    pub rested: Arc<dyn Fn() + Send + Sync>,
}

impl SessionHooks {
    pub fn detached() -> Self {
        SessionHooks {
            ended: Arc::new(|| {}),
            rested: Arc::new(|| {}),
        }
    }
}

/// A one-shot gate a filing or a reload waits on.
#[derive(Clone)]
struct Latch(Arc<watch::Sender<bool>>);

impl Latch {
    fn new() -> Self {
        Latch(Arc::new(watch::channel(false).0))
    }

    fn open(&self) {
        self.0.send_replace(true);
    }

    fn wait(&self) -> BoxFuture<'static, ()> {
        let mut receiver = self.0.subscribe();
        async move {
            let _ = receiver.wait_for(|open| *open).await;
        }
        .boxed()
    }
}

struct Held {
    generation: u64,
    resources: Arc<ResourceScope>,
    session: Arc<dyn PooledSession>,
    role: Option<CoordinationRole>,
    used_at: Instant,
    running: Option<(u64, watch::Receiver<bool>)>,
    idle: Option<AbortHandle>,
    reload: Option<Latch>,
    retire: Option<Latch>,
}

struct Inner {
    sessions: Mutex<HashMap<String, Held>>,
    idle: Duration,
    next: AtomicU64,
}

/// What a session is opened with when the thread has none that fits.
pub struct SessionOpener<S, O, St>
where
    O: FnOnce(SessionHooks) -> Arc<S>,
    St: FnOnce(&Arc<S>),
{
    pub open: O,
    /// Runs once the pool holds the session, so a session that ends at once is already known to it.
    pub start: St,
}

#[derive(Clone)]
pub struct SessionPool {
    inner: Arc<Inner>,
}

impl Default for SessionPool {
    fn default() -> Self {
        SessionPool::new(IDLE_SESSION)
    }
}

/// A session released from the pool, closed outside its lock.
struct Released {
    resources: Arc<ResourceScope>,
    session: Arc<dyn PooledSession>,
    reload: Option<Latch>,
    retire: Option<Latch>,
    idle: Option<AbortHandle>,
}

impl Released {
    fn finish(self) {
        if let Some(idle) = self.idle {
            idle.abort();
        }
        release_resource_scope(&self.resources);
        let closed = self.session.close();
        if let Some(reload) = self.reload {
            reload.open();
        }
        match self.retire {
            Some(retire) => {
                tokio::spawn(async move {
                    closed.await;
                    retire.open();
                });
            }
            None => {
                tokio::spawn(closed);
            }
        }
    }
}

impl SessionPool {
    pub fn new(idle: Duration) -> Self {
        SessionPool {
            inner: Arc::new(Inner {
                sessions: Mutex::new(HashMap::new()),
                idle,
                next: AtomicU64::new(1),
            }),
        }
    }

    /// Runs on the thread's session where it fits the run, and on a fresh one otherwise.
    pub async fn execute<S, O, St>(
        &self,
        input: RunInput,
        key: String,
        opener: SessionOpener<S, O, St>,
    ) -> ProviderResult
    where
        S: PooledSession,
        O: FnOnce(SessionHooks) -> Arc<S>,
        St: FnOnce(&Arc<S>),
    {
        if let Some(answering) = self.answering(&input.task_id) {
            answering.await;
            if input.is_cancelled() {
                return ProviderResult::cancelled();
            }
        }
        let task_id = input.task_id.clone();
        let (generation, session, resources) = self.session_for(&input, &key, opener);
        report_resource_busy(&resources, true);
        let run_id = self.inner.next.fetch_add(1, Ordering::Relaxed);
        let (done, running) = watch::channel(false);
        {
            let mut sessions = self.inner.sessions.lock().unwrap();
            if let Some(held) = sessions
                .get_mut(&task_id)
                .filter(|held| held.generation == generation)
            {
                held.running = Some((run_id, running));
            }
        }
        let result = in_resource_scope(Some(resources), session.run(input)).await;
        done.send_replace(true);
        {
            let mut sessions = self.inner.sessions.lock().unwrap();
            if let Some(held) = sessions
                .get_mut(&task_id)
                .filter(|held| held.generation == generation)
            {
                if held.running.as_ref().is_some_and(|(id, _)| *id == run_id) {
                    held.running = None;
                }
            }
        }
        self.rest(&task_id, generation);
        result
    }

    /// A cancelled run ends before its turn does; the next run waits for the turn's answer rather
    /// than closing the session under it.
    fn answering(&self, task_id: &str) -> Option<BoxFuture<'static, ()>> {
        let sessions = self.inner.sessions.lock().unwrap();
        let held = sessions.get(task_id)?;
        let (_, running) = held.running.as_ref()?;
        if !held.session.answering() {
            return None;
        }
        let mut running = running.clone();
        Some(
            async move {
                let _ = running.wait_for(|done| *done).await;
            }
            .boxed(),
        )
    }

    /// The thread's session, while it is live.
    pub fn live_session(&self, task_id: &str) -> Option<Arc<dyn PooledSession>> {
        let sessions = self.inner.sessions.lock().unwrap();
        sessions
            .get(task_id)
            .filter(|held| held.session.live())
            .map(|held| held.session.clone())
    }

    /// Retires the current sessions once their work finishes; new sessions load settings on launch.
    pub fn reload_settings(&self) -> BoxFuture<'static, ()> {
        let mut waits = Vec::new();
        let mut released = Vec::new();
        {
            let mut sessions = self.inner.sessions.lock().unwrap();
            let ids: Vec<String> = sessions.keys().cloned().collect();
            for task_id in ids {
                let held = sessions.get_mut(&task_id).unwrap();
                let reload = held.reload.get_or_insert_with(Latch::new).clone();
                waits.push(reload.wait());
                if !held.session.busy() {
                    released.extend(Self::take(&mut sessions, &task_id, None));
                }
            }
        }
        for release in released {
            release.finish();
        }
        join_all(waits).map(|_| ()).boxed()
    }

    /// Lets the thread's session go once its turn has answered, and resolves when its process is gone.
    pub fn retire(&self, task_id: &str) -> BoxFuture<'static, ()> {
        let (done, released) = {
            let mut sessions = self.inner.sessions.lock().unwrap();
            let Some(held) = sessions.get_mut(task_id) else {
                return futures::future::ready(()).boxed();
            };
            let done = held.retire.get_or_insert_with(Latch::new).wait();
            let released = if held.session.answering() {
                None
            } else {
                Self::take(&mut sessions, task_id, None)
            };
            (done, released)
        };
        if let Some(released) = released {
            released.finish();
        }
        done
    }

    /// Retires the thread's session when it was opened for a coordination role other than `role`.
    pub fn retire_out_of_role(
        &self,
        task_id: &str,
        role: Option<CoordinationRole>,
    ) -> BoxFuture<'static, ()> {
        let differs = self
            .inner
            .sessions
            .lock()
            .unwrap()
            .get(task_id)
            .is_some_and(|held| held.role != role);
        if differs {
            self.retire(task_id)
        } else {
            futures::future::ready(()).boxed()
        }
    }

    /// Lets every session go, which is what ends the processes they hold.
    pub fn close_all(&self) {
        let released: Vec<Released> = {
            let mut sessions = self.inner.sessions.lock().unwrap();
            let ids: Vec<String> = sessions.keys().cloned().collect();
            ids.iter()
                .filter_map(|task_id| Self::take(&mut sessions, task_id, None))
                .collect()
        };
        for release in released {
            release.finish();
        }
    }

    fn take(
        sessions: &mut HashMap<String, Held>,
        task_id: &str,
        generation: Option<u64>,
    ) -> Option<Released> {
        if generation.is_some_and(|generation| {
            sessions
                .get(task_id)
                .is_none_or(|held| held.generation != generation)
        }) {
            return None;
        }
        let held = sessions.remove(task_id)?;
        Some(Released {
            resources: held.resources,
            session: held.session,
            reload: held.reload,
            retire: held.retire,
            idle: held.idle,
        })
    }

    fn session_for<S, O, St>(
        &self,
        input: &RunInput,
        key: &str,
        opener: SessionOpener<S, O, St>,
    ) -> (u64, Arc<dyn PooledSession>, Arc<ResourceScope>)
    where
        S: PooledSession,
        O: FnOnce(SessionHooks) -> Arc<S>,
        St: FnOnce(&Arc<S>),
    {
        let task_id = input.task_id.clone();
        let mut released = Vec::new();
        let generation = self.inner.next.fetch_add(1, Ordering::Relaxed);
        let session = {
            let mut sessions = self.inner.sessions.lock().unwrap();
            if let Some(held) = sessions.get_mut(&task_id) {
                let reusable = held.session.live()
                    && held.retire.is_none()
                    && (held.reload.is_none() || held.session.busy())
                    && held.session.key() == key
                    && !held.session.answering()
                    && !input.fork_continuation
                    && !input.fresh_session
                    && held.session.continues(input.continuation_value());
                if reusable {
                    if let Some(idle) = held.idle.take() {
                        idle.abort();
                    }
                    held.used_at = Instant::now();
                    return (held.generation, held.session.clone(), held.resources.clone());
                }
                released.extend(Self::take(&mut sessions, &task_id, None));
            }
            released.extend(self.evict(&mut sessions));
            let weak = Arc::downgrade(&self.inner);
            let (ended_task, rested_task) = (task_id.clone(), task_id.clone());
            let ended_pool = weak.clone();
            let hooks = SessionHooks {
                ended: Arc::new(move || {
                    if let Some(inner) = ended_pool.upgrade() {
                        SessionPool { inner }.ended(&ended_task, generation);
                    }
                }),
                rested: Arc::new(move || {
                    if let Some(inner) = weak.upgrade() {
                        SessionPool { inner }.rest(&rested_task, generation);
                    }
                }),
            };
            let resources = resource_scope(&task_id);
            let session = in_resource_scope_sync(Some(resources.clone()), || (opener.open)(hooks));
            let pooled: Arc<dyn PooledSession> = session.clone();
            sessions.insert(
                task_id.clone(),
                Held {
                    generation,
                    resources: resources.clone(),
                    session: pooled,
                    role: input.coordination_role,
                    used_at: Instant::now(),
                    running: None,
                    idle: None,
                    reload: None,
                    retire: None,
                },
            );
            (session, resources)
        };
        let (session, resources) = session;
        for release in released {
            release.finish();
        }
        in_resource_scope_sync(Some(resources.clone()), || (opener.start)(&session));
        (generation, session, resources)
    }

    /// Lets the session named go, as memory protection asks; `idle_only` leaves one at work alone.
    pub fn terminate(&self, task_id: &str, session_id: &str, idle_only: bool) -> bool {
        let released = {
            let mut sessions = self.inner.sessions.lock().unwrap();
            let Some(held) = sessions.get(task_id).filter(|held| held.resources.session_id == session_id) else { return false };
            if idle_only && held.session.busy() {
                report_resource_busy(&held.resources, true);
                return false;
            }
            Self::take(&mut sessions, task_id, None)
        };
        if let Some(released) = released {
            released.finish();
        }
        true
    }

    fn ended(&self, task_id: &str, generation: u64) {
        let released = Self::take(
            &mut self.inner.sessions.lock().unwrap(),
            task_id,
            Some(generation),
        );
        if let Some(released) = released {
            released.finish();
        }
    }

    /// A session with nothing left to do is kept warm for a while, then handed back. Work the agent
    /// left running is not nothing: the deadline finds the session busy and starts over.
    fn rest(&self, task_id: &str, generation: u64) {
        let released = {
            let mut sessions = self.inner.sessions.lock().unwrap();
            let Some(held) = sessions
                .get_mut(task_id)
                .filter(|held| held.generation == generation)
            else {
                return;
            };
            if !held.session.live() {
                return;
            }
            if (held.retire.is_some() && !held.session.answering())
                || (held.reload.is_some() && !held.session.busy())
            {
                Self::take(&mut sessions, task_id, Some(generation))
            } else {
                report_resource_busy(&held.resources, held.session.busy());
                held.used_at = Instant::now();
                if let Some(idle) = held.idle.take() {
                    idle.abort();
                }
                let weak: Weak<Inner> = Arc::downgrade(&self.inner);
                let task = task_id.to_string();
                let idle = self.inner.idle;
                held.idle = Some(
                    tokio::spawn(async move {
                        tokio::time::sleep(idle).await;
                        if let Some(inner) = weak.upgrade() {
                            SessionPool { inner }.idle_expired(&task, generation);
                        }
                    })
                    .abort_handle(),
                );
                None
            }
        };
        if let Some(released) = released {
            released.finish();
        }
    }

    fn idle_expired(&self, task_id: &str, generation: u64) {
        let busy = {
            let mut sessions = self.inner.sessions.lock().unwrap();
            let Some(held) = sessions
                .get_mut(task_id)
                .filter(|held| held.generation == generation)
            else {
                return;
            };
            held.idle = None;
            held.session.busy()
        };
        if busy {
            self.rest(task_id, generation);
        } else {
            self.ended(task_id, generation);
        }
    }

    fn evict(&self, sessions: &mut HashMap<String, Held>) -> Vec<Released> {
        let mut idle: Vec<(String, Instant)> = sessions
            .iter()
            .filter(|(_, held)| !held.session.busy())
            .map(|(id, held)| (id.clone(), held.used_at))
            .collect();
        idle.sort_by_key(|(_, used)| *used);
        let mut over = (sessions.len() + 1).saturating_sub(MAX_LIVE_SESSIONS);
        let mut released = Vec::new();
        for (task_id, _) in idle {
            if over == 0 {
                break;
            }
            released.extend(Self::take(sessions, &task_id, None));
            over -= 1;
        }
        released
    }
}

/// What every engine's provider shares: the pool, and the filings that hold a thread while it is
/// archived or restored so an archive and a restore land in the order given and a run waits for both.
#[derive(Clone, Default)]
pub struct Filings {
    filings: Arc<Mutex<HashMap<String, (u64, Shared<BoxFuture<'static, ()>>)>>>,
    next: Arc<AtomicU64>,
}

impl Filings {
    /// What a run on the thread waits for first.
    pub fn pending(&self, task_id: &str) -> Option<Shared<BoxFuture<'static, ()>>> {
        self.filings
            .lock()
            .unwrap()
            .get(task_id)
            .map(|(_, filing)| filing.clone())
    }

    /// Files the thread after its last filing has landed. A filing that fails changes nothing.
    pub fn file(&self, task_id: &str, filing: BoxFuture<'static, anyhow::Result<()>>) {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let previous = self.pending(task_id);
        let filings = self.filings.clone();
        let task = task_id.to_string();
        let queued: Shared<BoxFuture<'static, ()>> = async move {
            if let Some(previous) = previous {
                previous.await;
            }
            let _ = filing.await;
            let mut filings = filings.lock().unwrap();
            if filings
                .get(&task)
                .is_some_and(|(current, _)| *current == id)
            {
                filings.remove(&task);
            }
        }
        .boxed()
        .shared();
        self.filings
            .lock()
            .unwrap()
            .insert(task_id.to_string(), (id, queued.clone()));
        tokio::spawn(queued);
    }
}

/// The thread's live session when `S` opened it.
pub fn session_of<S: PooledSession>(pool: &SessionPool, task_id: &str) -> Option<Arc<S>> {
    pool.live_session(task_id)?.as_any().downcast::<S>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::RunHooks;
    use crate::steer::SteerChannel;
    use crate::types::*;
    use std::sync::atomic::AtomicBool;
    use tokio_util::sync::CancellationToken;

    struct Fake {
        key: String,
        closed: AtomicBool,
        hooks: SessionHooks,
    }

    impl PooledSession for Fake {
        fn key(&self) -> String {
            self.key.clone()
        }
        fn live(&self) -> bool {
            !self.closed.load(Ordering::SeqCst)
        }
        fn busy(&self) -> bool {
            false
        }
        fn answering(&self) -> bool {
            false
        }
        fn continues(&self, _: Option<&str>) -> bool {
            true
        }
        fn run(self: Arc<Self>, _: RunInput) -> BoxFuture<'static, ProviderResult> {
            async { ProviderResult::succeeded() }.boxed()
        }
        fn close(&self) -> BoxFuture<'static, ()> {
            if !self.closed.swap(true, Ordering::SeqCst) {
                (self.hooks.ended)();
            }
            async {}.boxed()
        }
        fn as_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
            self
        }
    }

    fn input(task: &str) -> RunInput {
        RunInput {
            channel: RunChannel::Main,
            task_id: task.into(),
            title: String::new(),
            prompt: String::new(),
            workspace_root: "/tmp".into(),
            project_root: None,
            projectless: false,
            computer_use: ComputerUseRunConfig::SetupRequired,
            policy: ExecutionPolicy::Confirm,
            engine: AgentEngine::Claude,
            model: "haiku".into(),
            effort: AgentEffort::High,
            fast_mode: None,
            operation: None,
            chrome_browser: false,
            continuation: None,
            fork_continuation: false,
            fresh_session: false,
            coordination_role: None,
            toolkit: None,
            steering: SteerChannel::new(),
            cancel: CancellationToken::new(),
            hooks: RunHooks::silent(),
        }
    }

    async fn run(pool: &SessionPool, task: &str, key: &str, opened: &Arc<AtomicU64>) {
        let opened = opened.clone();
        pool.execute(
            input(task),
            key.into(),
            SessionOpener {
                open: move |hooks| {
                    opened.fetch_add(1, Ordering::SeqCst);
                    Arc::new(Fake {
                        key: key.into(),
                        closed: AtomicBool::new(false),
                        hooks,
                    })
                },
                start: |_| {},
            },
        )
        .await;
    }

    #[tokio::test]
    async fn reuses_a_fitting_session_and_caps_live_ones() {
        let pool = SessionPool::new(Duration::from_secs(60));
        let opened = Arc::new(AtomicU64::new(0));
        run(&pool, "a", "k", &opened).await;
        run(&pool, "a", "k", &opened).await;
        assert_eq!(opened.load(Ordering::SeqCst), 1);
        run(&pool, "a", "other", &opened).await;
        assert_eq!(opened.load(Ordering::SeqCst), 2);
        for task in ["b", "c", "d"] {
            run(&pool, task, "k", &opened).await;
        }
        let live = ["a", "b", "c", "d"]
            .iter()
            .filter(|task| pool.live_session(task).is_some())
            .count();
        assert_eq!(live, MAX_LIVE_SESSIONS);
        assert!(pool.live_session("a").is_none());
    }
}
