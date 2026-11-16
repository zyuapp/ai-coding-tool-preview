//! Which thread's session owns each engine process, reported to the host that watches their memory.
//! Only conversation engines register; discovery and shared authentication run outside any scope.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum SessionResourceMessage {
    #[serde(rename = "session.process")]
    Process { task_id: String, session_id: String, pid: u32 },
    #[serde(rename = "session.released")]
    Released { task_id: String, session_id: String },
    #[serde(rename = "session.state")]
    State { task_id: String, session_id: String, busy: bool },
}

/// One session's ownership of the processes it starts.
#[derive(Debug)]
pub struct ResourceScope {
    pub task_id: String,
    pub session_id: String,
    released: AtomicBool,
}

type Reporter = Arc<dyn Fn(SessionResourceMessage) + Send + Sync>;

static REPORTER: RwLock<Option<Reporter>> = RwLock::new(None);

tokio::task_local! {
    static SCOPE: Option<Arc<ResourceScope>>;
}

std::thread_local! {
    static SYNC_SCOPE: std::cell::RefCell<Option<Option<Arc<ResourceScope>>>> = const { std::cell::RefCell::new(None) };
}

/// Where scope messages go. Without one they are dropped.
pub fn report_session_resources(report: impl Fn(SessionResourceMessage) + Send + Sync + 'static) {
    *REPORTER.write().unwrap() = Some(Arc::new(report));
}

fn report(message: SessionResourceMessage) {
    let reporter = REPORTER.read().unwrap().clone();
    if let Some(reporter) = reporter {
        reporter(message);
    }
}

pub fn resource_scope(task_id: &str) -> Arc<ResourceScope> {
    Arc::new(ResourceScope { task_id: task_id.into(), session_id: uuid::Uuid::new_v4().to_string(), released: AtomicBool::new(false) })
}

/// Runs `future` with the processes it starts owned by `scope`.
pub fn in_resource_scope<F: Future>(scope: Option<Arc<ResourceScope>>, future: F) -> impl Future<Output = F::Output> {
    SCOPE.scope(scope, future)
}

/// Runs `act` with the processes it starts owned by `scope`, or by no session when `scope` is None.
pub fn in_resource_scope_sync<T>(scope: Option<Arc<ResourceScope>>, act: impl FnOnce() -> T) -> T {
    let previous = SYNC_SCOPE.with(|cell| cell.replace(Some(scope)));
    let result = act();
    SYNC_SCOPE.with(|cell| *cell.borrow_mut() = previous);
    result
}

fn current() -> Option<Arc<ResourceScope>> {
    if let Some(scope) = SYNC_SCOPE.with(|cell| cell.borrow().clone()) {
        return scope;
    }
    SCOPE.try_with(Clone::clone).ok().flatten()
}

pub fn release_resource_scope(scope: &ResourceScope) {
    if scope.released.swap(true, Ordering::SeqCst) {
        return;
    }
    report(SessionResourceMessage::Released { task_id: scope.task_id.clone(), session_id: scope.session_id.clone() });
}

pub fn report_resource_busy(scope: &ResourceScope, busy: bool) {
    if !scope.released.load(Ordering::SeqCst) {
        report(SessionResourceMessage::State { task_id: scope.task_id.clone(), session_id: scope.session_id.clone(), busy });
    }
}

/// Registers an engine process with the session starting it, if any.
pub fn register_session_process(pid: Option<u32>) {
    let (Some(scope), Some(pid)) = (current(), pid) else { return };
    report(SessionResourceMessage::Process { task_id: scope.task_id.clone(), session_id: scope.session_id.clone(), pid });
    if scope.released.load(Ordering::SeqCst) {
        report(SessionResourceMessage::Released { task_id: scope.task_id.clone(), session_id: scope.session_id.clone() });
    }
}
