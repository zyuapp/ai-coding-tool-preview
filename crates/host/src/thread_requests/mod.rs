//! What a run's tools ask of the window: reads come from its workspace state, writes go through the
//! window's own command path. Each answer here mirrors what the window would say.

pub mod bridges;
pub mod projection;
pub mod reads;
pub mod starts;

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

use act_core::domain::platform::BrowserPermissions;
use act_core::{CommandResult, WorkspaceInput, WorkspaceState};
use futures::future::BoxFuture;
use futures::FutureExt;
use serde_json::Value;
use tokio::sync::oneshot;

use crate::tools::types::{BrowserRead, BrowserReadResult, BrowserShot, BrowserSnapshot, SimulatorShot, SimulatorSnapshot};

pub use bridges::RunBridges;

/// One input's run through the window: what the reducer said at once, and what came of its effects.
pub struct Execution {
    pub accepted: BoxFuture<'static, CommandResult>,
    pub completed: BoxFuture<'static, CommandResult>,
}

impl Execution {
    /// An input that was refused before it reached the reducer.
    pub fn refused(message: impl Into<String>) -> Self {
        let result = CommandResult::failed(message);
        Execution { accepted: futures::future::ready(result.clone()).boxed(), completed: futures::future::ready(result).boxed() }
    }
}

/// A terminal's text as the panel reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalText {
    pub lines: Vec<String>,
    pub omitted: u64,
    pub matched: Option<u64>,
}

/// The window, as the tools reach it. `read` and the command path are required; each desktop read
/// answers as unavailable until the window provides it.
pub trait RequestHost: Send + Sync + 'static {
    /// Runs `read` against the window's current state.
    fn read(&self, read: &mut dyn FnMut(&WorkspaceState));
    /// Dispatches an input and resolves once its effects have run.
    fn dispatch(&self, input: WorkspaceInput) -> BoxFuture<'static, ()>;
    fn execute(&self, input: WorkspaceInput) -> Execution;
    /// The tool calls held open on a state still to come. The window calls `release` after every change.
    fn waiters(&self) -> &ThreadWaiters;

    /// Loads a thread's stored history into the workspace, which an unopened thread has yet to do.
    fn hydrate(&self, _thread_id: String) -> BoxFuture<'static, ()> {
        futures::future::ready(()).boxed()
    }
    /// Of `ids`, the unloaded threads whose stored messages contain `search`.
    fn search_stored(&self, _search: String, _ids: Vec<String>) -> BoxFuture<'static, HashSet<String>> {
        futures::future::ready(HashSet::new()).boxed()
    }
    fn configure_browser_permissions(&self, _permissions: BrowserPermissions) -> BoxFuture<'static, ()> {
        futures::future::ready(()).boxed()
    }
    fn capture_browser_page(&self, _tab_id: String, _full_page: bool, _timeout_ms: u64, _task_id: String) -> BoxFuture<'static, Option<BrowserShot>> {
        futures::future::ready(None).boxed()
    }
    /// A console, network or wait read of one tab.
    fn inspect_browser_page(&self, _tab_id: String, _read: BrowserRead, _task_id: String) -> BoxFuture<'static, Option<BrowserReadResult>> {
        futures::future::ready(None).boxed()
    }
    fn read_browser_page(&self, _tab_id: String, _text_limit: f64, _timeout_ms: u64, _task_id: String) -> BoxFuture<'static, Option<BrowserSnapshot>> {
        futures::future::ready(None).boxed()
    }
    fn read_terminal(&self, _terminal_id: String, _lines: usize, _matching: Option<String>) -> BoxFuture<'static, Option<TerminalText>> {
        futures::future::ready(None).boxed()
    }
    /// A thread read or listing answered by a paired computer.
    fn query_computer_threads(&self, _computer_id: String, _query: Value) -> BoxFuture<'static, Result<Value, String>> {
        futures::future::ready(Err("That computer is unavailable.".to_string())).boxed()
    }
    fn describe_simulator(&self, _udid: String, _name: String) -> BoxFuture<'static, Result<SimulatorSnapshot, String>> {
        futures::future::ready(Err("The simulator is unavailable.".to_string())).boxed()
    }
    fn capture_simulator(&self, _udid: String) -> BoxFuture<'static, Option<SimulatorShot>> {
        futures::future::ready(None).boxed()
    }
}

/// Reads the window's state through `host`.
pub fn with_state<R>(host: &dyn RequestHost, read: impl FnOnce(&WorkspaceState) -> R) -> R {
    let mut read = Some(read);
    let mut result = None;
    host.read(&mut |state| {
        if let Some(read) = read.take() {
            result = Some(read(state));
        }
    });
    result.expect("the host reads its state")
}

type Pending = Box<dyn Fn(&WorkspaceState) -> bool + Send>;

struct Waiter {
    id: u64,
    pending: Pending,
    settle: oneshot::Sender<()>,
}

/// Tool calls held open until the state they wait on arrives, such as the thread they name stopping.
#[derive(Default)]
pub struct ThreadWaiters {
    list: Mutex<Vec<Waiter>>,
    next: std::sync::atomic::AtomicU64,
}

impl ThreadWaiters {
    /// Called by the window after each state change: every call no longer waiting answers.
    pub fn release(&self, state: &WorkspaceState) {
        let mut list = self.list.lock().unwrap();
        if list.is_empty() {
            return;
        }
        let (settled, waiting): (Vec<Waiter>, Vec<Waiter>) = list.drain(..).partition(|waiter| !(waiter.pending)(state));
        *list = waiting;
        drop(list);
        for waiter in settled {
            let _ = waiter.settle.send(());
        }
    }

    fn add(&self, pending: Pending) -> (u64, oneshot::Receiver<()>) {
        let (settle, settled) = oneshot::channel();
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.list.lock().unwrap().push(Waiter { id, pending, settle });
        (id, settled)
    }

    fn remove(&self, id: u64) {
        self.list.lock().unwrap().retain(|waiter| waiter.id != id);
    }
}

/// Holds the call until `pending` stops holding, or the time runs out; answers whether it timed out.
pub async fn wait_for(host: &dyn RequestHost, pending: impl Fn(&WorkspaceState) -> bool + Send + 'static, timeout: Duration) -> bool {
    if !with_state(host, &pending) {
        return false;
    }
    let (id, settled) = host.waiters().add(Box::new(pending));
    tokio::select! {
        _ = settled => false,
        _ = tokio::time::sleep(timeout) => {
            host.waiters().remove(id);
            true
        }
    }
}

pub(crate) fn input(value: Value) -> Result<WorkspaceInput, String> {
    WorkspaceInput::from_json(&value).map_err(|error| format!("Invalid request: {error}"))
}

pub(crate) fn result_message(result: &CommandResult) -> String {
    result.message().unwrap_or("The command failed.").to_string()
}

pub(crate) fn result_task_id(result: &CommandResult) -> Option<String> {
    match result {
        CommandResult::Ok { task_id, .. } => task_id.clone(),
        CommandResult::Failed { .. } => None,
    }
}

/// A paired computer's thread read or listing, answered from this computer's own threads without
/// following any paired links.
pub async fn query_local_threads(host: &dyn RequestHost, query: Value) -> Result<Value, String> {
    if !crate::mobile::messages::is_computer_query(&query) {
        return Err("Invalid thread query.".into());
    }
    match query.get("kind").and_then(Value::as_str) {
        Some("thread-read") => {
            let thread_id = query.get("threadId").and_then(Value::as_str).unwrap_or("").to_string();
            let limit = query.get("limit").and_then(Value::as_f64).map(|limit| limit as usize).unwrap_or(30);
            let local = with_state(host, |state| reads::resolve_thread_read(state, &thread_id, "this").ok().map(|found| found.thread.id));
            if let Some(id) = local {
                host.hydrate(id).await;
            }
            let transcript = with_state(host, |state| projection::thread_transcript(state, &thread_id, Some(limit))).ok_or_else(|| format!("No thread has the ID {thread_id}."))?;
            serde_json::to_value(transcript).map_err(|error| error.to_string())
        }
        Some("thread-list") => {
            let mut filter: crate::tools::types::ThreadListQuery = serde_json::from_value(query.clone()).map_err(|error| error.to_string())?;
            filter.computer = None;
            filter.project = Some(filter.project.unwrap_or_else(|| "all".into()));
            filter.limit = Some(filter.limit.unwrap_or(20.0));
            let search = filter.search.as_deref().map(|search| search.trim().to_lowercase()).filter(|search| !search.is_empty());
            let mut stored = None;
            if let Some(search) = search {
                let ids = with_state(host, |state| {
                    let scope = projection::resolve_scope(state, "paired", filter.project.as_deref()).ok()?;
                    let scan = projection::ThreadFilter { archived: filter.archived.unwrap_or(false), attachments: filter.attachments.unwrap_or(false), idle_for_ms: filter.idle_for_ms, ..projection::ThreadFilter::default() };
                    Some(projection::thread_summaries(state, &scope, &scan, chrono::Utc::now().timestamp_millis()).into_iter().filter(|thread| state.thread(&thread.id).is_some_and(|held| held.history_summary.is_some()) && !thread.title.to_lowercase().contains(&search)).map(|thread| thread.id).collect::<Vec<_>>())
                });
                if let Some(ids) = ids {
                    stored = Some(if ids.is_empty() { HashSet::new() } else { host.search_stored(search, ids).await });
                }
            }
            let list = with_state(host, |state| reads::local_thread_list(state, "paired", &filter, stored))?;
            serde_json::to_value(list).map_err(|error| error.to_string())
        }
        _ => Err("Invalid thread query.".into()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use futures::future::BoxFuture;
    use futures::FutureExt;
    use serde_json::{json, Map, Value};

    use super::input;
    use crate::tools::schema::{Field, Kind};
    use crate::tools::threads::{thread_definitions, ThreadBridge, ThreadToolContext};
    use crate::tools::types::*;

    #[derive(Default)]
    struct Capture(Mutex<Vec<Value>>);

    impl ThreadBridge for Capture {
        fn list(&self, _: ThreadListQuery) -> BoxFuture<'_, Result<Vec<ThreadSummary>, String>> {
            async { Err("answered".into()) }.boxed()
        }
        fn read(&self, _: String, _: Option<f64>, _: Option<String>) -> BoxFuture<'_, Result<ThreadTranscript, String>> {
            async { Err("answered".into()) }.boxed()
        }
        fn wait(&self, _: String, _: u64) -> BoxFuture<'_, Result<ThreadWaitResult, String>> {
            async { Err("answered".into()) }.boxed()
        }
        fn command(&self, command: ExternalCommand, _: Option<String>) -> BoxFuture<'_, Result<ThreadCommandResult, String>> {
            self.0.lock().unwrap().push(command);
            async { Err("answered".into()) }.boxed()
        }
    }

    fn sample(field: &Field) -> Value {
        match &field.kind {
            Kind::String { min, .. } => json!("x".repeat(min.unwrap_or(1).max(1))),
            Kind::Number { .. } => json!(1),
            Kind::Boolean => json!(true),
            Kind::Enum(values) => json!(values[0]),
            Kind::Array { item, .. } => json!([sample(item)]),
            Kind::Object(fields) => Value::Object(fields.iter().map(|(name, field)| (name.to_string(), sample(field))).collect()),
        }
    }

    fn edges(field: &Field) -> Vec<Value> {
        let mut edges = if field.nullable { vec![Value::Null] } else { Vec::new() };
        match &field.kind {
            Kind::String { max, .. } => {
                let max = max.unwrap_or(MAX_PROMPT_LENGTH + 1);
                edges.extend([json!(""), json!(" "), json!("x".repeat(max)), json!("x".repeat(max + 1))]);
            }
            Kind::Number { .. } => edges.extend([json!(-1), json!(0), json!(0.5), json!(1e12)]),
            Kind::Boolean => edges.extend([json!(false), json!(true)]),
            Kind::Enum(values) => edges.extend(values.iter().map(|value| json!(value))),
            _ => {}
        }
        edges
    }

    /// Whatever a thread tool's schema takes, the window reads, so an agent is only refused on the field it got wrong.
    #[tokio::test]
    async fn every_command_a_thread_tool_takes_is_one_the_window_reads() {
        let capture = Arc::new(Capture::default());
        let context = Arc::new(ThreadToolContext { bridge: capture.clone(), now: || 0 });
        let mut unread = Vec::new();
        let mut sent = 0;
        for role in [None, Some(CoordinationRole::Coordinator), Some(CoordinationRole::Member)] {
            for tool in thread_definitions(role) {
                let base: Map<String, Value> = tool.input.fields.iter().map(|(name, field)| (name.to_string(), sample(field))).collect();
                for (name, field) in &tool.input.fields {
                    for edge in edges(field) {
                        let mut args = base.clone();
                        args.insert(name.to_string(), edge.clone());
                        let Ok(parsed) = tool.input.parse(&args) else { continue };
                        capture.0.lock().unwrap().clear();
                        (tool.run)(context.clone(), parsed).await;
                        for command in capture.0.lock().unwrap().drain(..) {
                            sent += 1;
                            if let Err(error) = input(command) {
                                let shown = edge.as_str().filter(|text| text.len() > 20).map(|text| format!("{} characters", text.len())).unwrap_or_else(|| edge.to_string());
                                unread.push(format!("{} {name} = {shown}: {error}", tool.name));
                            }
                        }
                    }
                }
            }
        }
        assert!(sent > 100, "the tools were driven");
        assert_eq!(unread, Vec::<String>::new());
    }
}
