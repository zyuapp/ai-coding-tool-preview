//! Memory protection: samples the processes each thread's engine session owns and stops the largest
//! one when the computer runs short of memory (src/main/session-resource-monitor.mts).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use act_core::domain::session_resources::{unsafe_memory, MemorySample, ResourceCleanup, ResourceStop};
use serde_json::{json, Value};
use act_platform::process::Signal;
use act_platform::system::{available_memory, processes, ProcessSample};
use tokio::sync::Notify;

const MIB: u64 = 1024 * 1024;
const CLEANUP_LIMIT: i64 = 15_000;
const SAMPLE_EVERY: Duration = Duration::from_secs(5);
const HISTORY: usize = 12;

fn total_memory() -> u64 {
    act_platform::system::total_memory().unwrap_or(16 * 1024 * MIB)
}

/// What the monitor tells the run host.
pub struct MonitorHooks {
    /// A thread to stop for its memory; its session's processes are being ended.
    pub stop: Box<dyn Fn(&str, &str, ResourceStop) + Send + Sync>,
    /// An idle session to let go because the computer is short of memory.
    pub release: Box<dyn Fn(&str, &str) + Send + Sync>,
    /// How far a stopped thread's cleanup has got.
    pub changed: Box<dyn Fn(&str, ResourceStop) + Send + Sync>,
}

struct Owned {
    task_id: String,
    session_id: String,
    roots: Vec<u32>,
    known: Vec<(u32, String)>,
    history: Vec<MemorySample>,
    busy: Option<bool>,
    released: Option<i64>,
    stopped: Option<ResourceStop>,
}

enum Call {
    Stop(String, String, ResourceStop),
    Release(String, String),
    Changed(String, ResourceStop),
}

struct Inner {
    sessions: Mutex<Vec<Owned>>,
    enabled: AtomicBool,
    closed: AtomicBool,
    wake: Notify,
    sampling: tokio::sync::Mutex<()>,
    journal: Option<PathBuf>,
    last_journal: Mutex<String>,
    hooks: MonitorHooks,
}

/// Runs in the host, apart from the engines it watches.
#[derive(Clone)]
pub struct SessionMonitor {
    inner: Arc<Inner>,
}

fn now() -> i64 {
    act_core::util::now()
}

impl SessionMonitor {
    /// Must be called inside a Tokio runtime. A journal left by a crash names processes to end first.
    pub fn new(journal: Option<PathBuf>, hooks: MonitorHooks) -> SessionMonitor {
        let sessions = journal.as_ref().map(restore).unwrap_or_default();
        let monitor = SessionMonitor {
            inner: Arc::new(Inner { sessions: Mutex::new(sessions), enabled: AtomicBool::new(true), closed: AtomicBool::new(false), wake: Notify::new(), sampling: tokio::sync::Mutex::new(()), journal, last_journal: Mutex::new(String::new()), hooks }),
        };
        let looping = monitor.clone();
        tokio::spawn(async move { looping.run().await });
        monitor
    }

    async fn run(&self) {
        loop {
            if self.inner.closed.load(Ordering::SeqCst) {
                return;
            }
            if self.inner.sessions.lock().unwrap().is_empty() {
                self.inner.wake.notified().await;
                continue;
            }
            if let Err(error) = self.sample().await {
                eprintln!("Memory protection: {error}");
            }
            tokio::select! {
                _ = self.inner.wake.notified() => {}
                _ = tokio::time::sleep(SAMPLE_EVERY) => {}
            }
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.inner.enabled.store(enabled, Ordering::SeqCst);
        for owner in self.inner.sessions.lock().unwrap().iter_mut() {
            owner.history.clear();
        }
    }

    pub fn process(&self, task_id: &str, session_id: &str, pid: u32) {
        self.receive(task_id, session_id, |owner| owner.roots.push(pid), true);
    }

    pub fn state(&self, task_id: &str, session_id: &str, busy: bool) {
        self.receive(task_id, session_id, |owner| owner.busy = Some(busy), false);
    }

    pub fn released(&self, task_id: &str, session_id: &str) {
        self.receive(task_id, session_id, |owner| { owner.released.get_or_insert_with(now); }, false);
    }

    fn receive(&self, task_id: &str, session_id: &str, change: impl FnOnce(&mut Owned), opens: bool) {
        if self.inner.closed.load(Ordering::SeqCst) {
            return;
        }
        {
            let mut sessions = self.inner.sessions.lock().unwrap();
            match sessions.iter_mut().find(|owner| owner.session_id == session_id) {
                Some(owner) if owner.task_id != task_id => return,
                Some(owner) => change(owner),
                None if opens => {
                    let mut owner = Owned { task_id: task_id.into(), session_id: session_id.into(), roots: Vec::new(), known: Vec::new(), history: Vec::new(), busy: None, released: None, stopped: None };
                    change(&mut owner);
                    sessions.push(owner);
                }
                None => return,
            }
        }
        self.inner.wake.notify_one();
    }

    /// Shutdown releases only owned processes; shared services never enter this registry.
    pub fn release_all(&self) {
        for owner in self.inner.sessions.lock().unwrap().iter_mut() {
            owner.released.get_or_insert_with(now);
        }
        self.inner.wake.notify_one();
    }

    pub fn close(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        self.inner.wake.notify_one();
    }

    /// A crash's leftover processes must exit before the same thread starts another engine.
    pub async fn before_start(&self, task_id: &str) -> Result<(), String> {
        for _ in 0..20 {
            let waiting = self.inner.sessions.lock().unwrap().iter().any(|owner| owner.task_id == task_id && owner.released.is_some());
            if !waiting {
                return Ok(());
            }
            if let Err(error) = self.sample().await {
                eprintln!("Memory protection cleanup retry: {error}");
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        Err("The previous session still has processes running. Wait for them to exit before resuming.".into())
    }

    pub async fn sample(&self) -> std::io::Result<()> {
        let _sampling = self.inner.sampling.lock().await;
        let rows = tokio::task::spawn_blocking(processes).await.map_err(std::io::Error::other)??;
        let (total, available) = tokio::task::spawn_blocking(|| (total_memory(), available_memory())).await.map_err(std::io::Error::other)?;
        let calls = self.poll(&rows, now(), total, available, &|pid, signal| {
            act_platform::process::signal(pid, signal);
        });
        for call in calls {
            match call {
                Call::Stop(task, session, stop) => (self.inner.hooks.stop)(&task, &session, stop),
                Call::Release(task, session) => (self.inner.hooks.release)(&task, &session),
                Call::Changed(task, stop) => (self.inner.hooks.changed)(&task, stop),
            }
        }
        self.persist();
        Ok(())
    }

    fn poll(&self, rows: &[ProcessSample], now: i64, total: u64, available: u64, signal: &dyn Fn(u32, Signal)) -> Vec<Call> {
        let by_pid: HashMap<u32, &ProcessSample> = rows.iter().map(|row| (row.pid, row)).collect();
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for row in rows {
            children.entry(row.parent).or_default().push(row.pid);
        }
        let own = std::process::id();
        let enabled = self.inner.enabled.load(Ordering::SeqCst);
        let mut calls = Vec::new();
        let mut sessions = self.inner.sessions.lock().unwrap();
        let mut claimed: HashSet<u32> = HashSet::new();
        let mut totals: Vec<(String, u64)> = Vec::new();
        let mut index = 0;
        while index < sessions.len() {
            let owner = &mut sessions[index];
            // Descendants are remembered before they are reparented; a pid whose birth changed is never adopted.
            let mut live: Vec<(u32, String)> = Vec::new();
            let mut visit = |pid: u32, born: Option<&str>, live: &mut Vec<(u32, String)>| {
                let mut stack = vec![(pid, born.map(str::to_string))];
                while let Some((pid, born)) = stack.pop() {
                    let Some(row) = by_pid.get(&pid) else { continue };
                    if row.pid == own || born.as_ref().is_some_and(|born| born != &row.born) || !claimed.insert(pid) {
                        continue;
                    }
                    live.push((pid, row.born.clone()));
                    for child in children.get(&pid).into_iter().flatten().rev() {
                        stack.push((*child, None));
                    }
                }
            };
            for (pid, born) in owner.known.clone() {
                visit(pid, Some(&born), &mut live);
            }
            for pid in std::mem::take(&mut owner.roots) {
                // A root is adopted once; a stale root cannot claim a later reuse of its pid.
                if !owner.known.iter().any(|(known, _)| *known == pid) {
                    visit(pid, None, &mut live);
                }
            }
            owner.known = live;
            if owner.known.is_empty() {
                if let Some(stopped) = &owner.stopped {
                    calls.push(Call::Changed(owner.task_id.clone(), ResourceStop { cleanup: ResourceCleanup::Complete, ..stopped.clone() }));
                }
                sessions.remove(index);
                continue;
            }
            let bytes: u64 = owner.known.iter().filter_map(|(pid, _)| by_pid.get(pid)).map(|row| row.bytes).sum();
            totals.push((owner.session_id.clone(), bytes));
            owner.history.push(MemorySample { at: now, bytes, available });
            if owner.history.len() > HISTORY {
                owner.history.remove(0);
            }
            index += 1;
        }
        // Pressure is system-wide, so each sample stops at most the one largest owner.
        let find = |sessions: &Vec<Owned>, id: &str| sessions.iter().position(|owner| owner.session_id == id);
        let mut active: Vec<(String, u64)> = totals.iter().filter(|(id, _)| find(&sessions, id).is_some_and(|at| sessions[at].released.is_none())).cloned().collect();
        active.sort_by(|a, b| b.1.cmp(&a.1));
        let largest = active.first().map(|(id, _)| id.clone());
        let idle = active.iter().find(|(id, _)| find(&sessions, id).is_some_and(|at| sessions[at].busy == Some(false))).map(|(id, _)| id.clone());
        let releasing = if enabled && available < 512 * MIB { idle } else { None };
        if let Some(at) = releasing.as_deref().and_then(|id| find(&sessions, id)) {
            sessions[at].busy = Some(true);
            calls.push(Call::Release(sessions[at].task_id.clone(), sessions[at].session_id.clone()));
        }
        // An owner that outlives the cleanup limit cannot be freed, so it no longer defers the next stop.
        let stopping = releasing.is_some() || sessions.iter().any(|owner| owner.released.is_some_and(|released| now - released < CLEANUP_LIMIT));
        if let Some(at) = largest.as_deref().and_then(|id| find(&sessions, id)).filter(|_| enabled && !stopping) {
            let owner = &mut sessions[at];
            if let Some(verdict) = unsafe_memory(&owner.history, total) {
                let bytes = owner.history.last().map(|sample| sample.bytes).unwrap_or(0);
                eprintln!("Memory protection stopped thread {} ({verdict:?}): {} used, {} available of {}.", owner.task_id, gib(bytes), gib(available), gib(total));
                let stop = ResourceStop { at: now, bytes: bytes as i64, cleanup: ResourceCleanup::Stopping };
                owner.stopped = Some(stop.clone());
                owner.released = Some(now);
                calls.push(Call::Stop(owner.task_id.clone(), owner.session_id.clone(), stop));
            }
        }
        for (id, _) in &totals {
            let Some(at) = find(&sessions, id) else { continue };
            let owner = &mut sessions[at];
            let Some(released) = owner.released else { continue };
            let elapsed = now - released;
            if elapsed < 1000 {
                continue;
            }
            let kind = if elapsed >= 5000 { Signal::Kill } else { Signal::Terminate };
            for (pid, born) in &owner.known {
                if by_pid.get(pid).is_some_and(|row| &row.born == born) {
                    signal(*pid, kind);
                }
            }
            if elapsed >= CLEANUP_LIMIT {
                if let Some(stopped) = owner.stopped.as_mut().filter(|stopped| stopped.cleanup != ResourceCleanup::Incomplete) {
                    stopped.cleanup = ResourceCleanup::Incomplete;
                    calls.push(Call::Changed(owner.task_id.clone(), stopped.clone()));
                }
            }
        }
        calls
    }

    fn persist(&self) {
        let Some(journal) = &self.inner.journal else { return };
        let data: Vec<Value> = self.inner.sessions.lock().unwrap().iter().filter(|owner| !owner.known.is_empty()).map(|owner| json!({ "taskId": owner.task_id, "sessionId": owner.session_id, "known": owner.known.iter().map(|(pid, born)| json!([pid, born])).collect::<Vec<_>>() })).collect();
        let text = Value::Array(data).to_string();
        let mut last = self.inner.last_journal.lock().unwrap();
        if *last == text {
            return;
        }
        let temporary = journal.with_extension(format!("json.{}.tmp", std::process::id()));
        let written = std::fs::write(&temporary, &text).and_then(|_| {
            act_platform::fs::make_private_file(&temporary)?;
            std::fs::rename(&temporary, journal)
        });
        match written {
            Ok(()) => *last = text,
            Err(error) => eprintln!("Memory protection journal: {error}"),
        }
    }
}

fn restore(journal: &PathBuf) -> Vec<Owned> {
    let text = match std::fs::read_to_string(journal) {
        Ok(text) => text,
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!("Memory protection recovery: {error}");
            }
            return Vec::new();
        }
    };
    let Ok(Value::Array(entries)) = serde_json::from_str::<Value>(&text) else { return Vec::new() };
    let mut sessions: Vec<Owned> = Vec::new();
    for entry in entries {
        let (Some(task_id), Some(session_id), Some(pairs)) = (entry["taskId"].as_str(), entry["sessionId"].as_str(), entry["known"].as_array()) else { continue };
        let known: Vec<(u32, String)> = pairs.iter().filter_map(|pair| Some((u32::try_from(pair.get(0)?.as_u64()?).ok().filter(|pid| *pid > 1)?, pair.get(1)?.as_str()?.to_string()))).collect();
        if !known.is_empty() && !sessions.iter().any(|owner| owner.session_id == session_id) {
            sessions.push(Owned { task_id: task_id.into(), session_id: session_id.into(), roots: Vec::new(), known, history: Vec::new(), busy: None, released: Some(now()), stopped: None });
        }
    }
    sessions
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1024 * MIB) as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * MIB;

    fn monitor() -> (SessionMonitor, Arc<Mutex<Vec<String>>>) {
        let calls: Arc<Mutex<Vec<String>>> = Arc::default();
        let (stopped, release, changed) = (calls.clone(), calls.clone(), calls.clone());
        let hooks = MonitorHooks {
            stop: Box::new(move |task, _, stop| stopped.lock().unwrap().push(format!("stop {task} {:?}", stop.cleanup))),
            release: Box::new(move |task, _| release.lock().unwrap().push(format!("release {task}"))),
            changed: Box::new(move |task, stop| changed.lock().unwrap().push(format!("changed {task} {:?}", stop.cleanup))),
        };
        (SessionMonitor::new(None, hooks), calls)
    }

    fn row(pid: u32, parent: u32, bytes: u64, born: &str) -> ProcessSample {
        ProcessSample { pid, parent, bytes, born: born.into() }
    }

    fn run(monitor: &SessionMonitor, rows: &[ProcessSample], at: i64, available: u64) -> Vec<String> {
        let signals: Arc<Mutex<Vec<String>>> = Arc::default();
        let sent = signals.clone();
        let calls = monitor.poll(rows, at, 32 * GIB, available, &move |pid, signal| sent.lock().unwrap().push(format!("signal {pid} {signal:?}")));
        let mut said: Vec<String> = calls.into_iter().map(|call| match call {
            Call::Stop(task, _, stop) => format!("stop {task} {:?}", stop.cleanup),
            Call::Release(task, _) => format!("release {task}"),
            Call::Changed(task, stop) => format!("changed {task} {:?}", stop.cleanup),
        }).collect();
        said.extend(signals.lock().unwrap().drain(..));
        said
    }

    #[tokio::test]
    async fn sustained_pressure_stops_the_largest_session_and_ends_its_tree() {
        let (monitor, _) = monitor();
        monitor.process("big", "s-big", 100);
        monitor.process("small", "s-small", 200);
        let rows = |extra: u64| vec![row(100, 1, 3 * GIB, "a"), row(101, 100, extra, "b"), row(200, 1, GIB, "c")];
        for step in 0..3 {
            assert!(run(&monitor, &rows(GIB), step * 5_000, GIB).is_empty());
        }
        assert_eq!(run(&monitor, &rows(GIB), 15_000, GIB), vec!["stop big Stopping"]);
        assert!(run(&monitor, &rows(GIB), 15_500, GIB).is_empty());
        assert_eq!(run(&monitor, &rows(GIB), 16_000, GIB), vec!["signal 100 Terminate", "signal 101 Terminate"]);
        // A pid reused by another process is not signalled.
        let reused = vec![row(100, 1, 3 * GIB, "a"), row(101, 1, GIB, "other"), row(200, 1, GIB, "c")];
        assert_eq!(run(&monitor, &reused, 20_000, GIB), vec!["signal 100 Kill"]);
        assert_eq!(run(&monitor, &[row(200, 1, GIB, "c")], 21_000, GIB), vec!["changed big Complete"]);
    }

    #[tokio::test]
    async fn plenty_of_memory_never_stops_a_session_however_large() {
        let (monitor, _) = monitor();
        monitor.process("big", "s-big", 100);
        for step in 0..8 {
            assert!(run(&monitor, &[row(100, 1, 20 * GIB, "a")], step * 5_000, 8 * GIB).is_empty());
        }
        monitor.set_enabled(false);
        assert!(run(&monitor, &[row(100, 1, 20 * GIB, "a")], 50_000, 100 * MIB).is_empty());
    }

    #[tokio::test]
    async fn a_stopped_session_ends_its_real_processes() {
        let (monitor, _) = monitor();
        let mut child = std::process::Command::new("/bin/sh").args(["-c", "sleep 61 & wait"]).spawn().unwrap();
        monitor.process("t", "s", child.id());
        std::thread::sleep(Duration::from_millis(200));
        let kill = |pid: u32, signal: Signal| {
            act_platform::process::signal(pid, signal);
        };
        // So small a computer that the session's few megabytes are an eighth of it.
        let total = MIB;
        let mut stopped = false;
        for step in 0..4 {
            let rows = processes().unwrap();
            let calls = monitor.poll(&rows, step * 5_000, total, 100 * MIB, &kill);
            stopped |= calls.iter().any(|call| matches!(call, Call::Stop(task, _, _) if task == "t"));
            if stopped {
                break;
            }
        }
        assert!(stopped);
        let rows = processes().unwrap();
        monitor.poll(&rows, 30_000, total, 8 * GIB, &kill);
        let status = child.wait().unwrap();
        assert!(!status.success());
        std::thread::sleep(Duration::from_millis(200));
        let after = processes().unwrap();
        assert!(!after.iter().any(|row| row.parent == child.id()), "the sleep under the session was signalled too");
    }
}
