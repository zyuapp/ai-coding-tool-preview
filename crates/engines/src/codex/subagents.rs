//! Routes Codex multi-agent notifications and folds each child thread into provider-neutral subagent
//! reports. A foreign thread is suppressed at once, but nothing appears in the roster until
//! `thread/started` names a `thread_spawn` source or `subAgentActivity` names the child.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, Weak};

use futures::future::BoxFuture;
use serde_json::Value;

use crate::types::{
    ActivityKind, SubagentLiveStatus, SubagentMetadata, SubagentReport, SubagentTerminalStatus,
};

use super::spend::CodexSpend;

const MAX_BUFFERED_ACTIVITY: usize = 100;
const MAX_SEEN_ACTIVITY: usize = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lifecycle {
    Unknown,
    Working,
    Idle,
    Failed,
    Stopped,
}

#[derive(Default)]
struct Child {
    id: String,
    discovered: bool,
    lifecycle: Option<Lifecycle>,
    active: bool,
    prompt: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    metadata_reads: u32,
    metadata_pending: bool,
    preview: Option<String>,
    nickname: Option<String>,
    role: Option<String>,
    path: Option<String>,
    total_tokens: u64,
    last_tool_name: Option<String>,
    summary: Option<String>,
    status_summary: Option<String>,
    reported_lifecycle: Option<Lifecycle>,
    reported_status_summary: Option<Option<String>>,
    progress_key: Option<String>,
    buffered_activity: VecDeque<SubagentReport>,
    seen_activity: HashSet<String>,
    seen_order: VecDeque<String>,
    generation: u64,
}

impl Child {
    fn lifecycle(&self) -> Lifecycle {
        self.lifecycle.unwrap_or(Lifecycle::Unknown)
    }
}

/// A child turn addressable by `turn/interrupt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildTurn {
    pub thread_id: String,
    pub turn_id: String,
}

pub enum StopTarget {
    Turn(ChildTurn),
    /// A working child with no turn yet: its stop waits for the turn it starts next.
    Held,
}

#[derive(Default)]
struct State {
    root_thread_id: Option<String>,
    children: HashMap<String, Child>,
    ignored: HashSet<String>,
    live_turns: HashMap<String, String>,
    held_stops: HashSet<String>,
    pending_items: Vec<(String, String, Value)>,
    metadata_queue: VecDeque<String>,
    reading_metadata: usize,
    active_children: i64,
    closed: bool,
    next_generation: u64,
    reports: Vec<SubagentReport>,
    /// Usage reports to count once the lock is let go.
    spends: Vec<(String, Value)>,
}

impl State {
    fn busy(&self) -> bool {
        !self.live_turns.is_empty() || self.active_children > 0
    }

    fn should_suppress(&self, thread_id: &str) -> bool {
        if self.root_thread_id.as_deref() == Some(thread_id) {
            return false;
        }
        self.children.contains_key(thread_id)
            || self.ignored.contains(thread_id)
            || self.root_thread_id.is_some()
    }

    fn child(&mut self, id: &str) -> &mut Child {
        if !self.children.contains_key(id) {
            self.next_generation += 1;
            let generation = self.next_generation;
            self.children.insert(
                id.to_string(),
                Child {
                    id: id.to_string(),
                    generation,
                    ..Default::default()
                },
            );
        }
        self.children.get_mut(id).unwrap()
    }

    fn drop_pending_items(&mut self, thread_id: &str) {
        self.pending_items
            .retain(|(_, thread, _)| thread != thread_id);
    }
}

pub type ReadThread = Arc<dyn Fn(String) -> BoxFuture<'static, Option<Value>> + Send + Sync>;

struct Inner {
    state: Mutex<State>,
    report: Arc<dyn Fn(SubagentReport) + Send + Sync>,
    on_busy_changed: Arc<dyn Fn(bool) + Send + Sync>,
    read_metadata: Option<ReadThread>,
    spend: Option<CodexSpend>,
    me: Weak<Inner>,
}

#[derive(Clone)]
pub struct CodexSubagents {
    inner: Arc<Inner>,
}

fn nonempty(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(String::from)
}

fn first_line(value: &str) -> String {
    value
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_string())
        .unwrap_or_default()
}

fn detail(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

/// Metadata present only on a child thread's `thread/started` notification.
fn spawn_metadata(thread: &Value) -> Option<(Option<String>, Option<String>, Option<String>)> {
    let spawn = &thread["source"]["subAgent"]["thread_spawn"];
    if !spawn.is_object() {
        return None;
    }
    Some((
        nonempty(&spawn["agent_nickname"]),
        nonempty(&spawn["agent_role"]),
        nonempty(&spawn["agent_path"]),
    ))
}

/// Threads Codex owns for housekeeping stay out of both the parent conversation and the roster.
fn is_internal_thread(thread: &Value) -> bool {
    if thread["threadSource"] == "memory_consolidation" {
        return true;
    }
    let source = &thread["source"];
    source["subAgent"] == "memory_consolidation"
        || source["internal"] == "memory_consolidation"
        || source["internal"] == "guardian"
}

fn tool_activity(item: &Value) -> Option<(String, String, Option<String>)> {
    let summary_of = |value: &Value| {
        value
            .as_str()
            .filter(|text| !text.is_empty())
            .map(String::from)
    };
    match item["type"].as_str()? {
        "commandExecution" => Some((
            "command_execution".into(),
            detail(&serde_json::json!({ "command": item["command"], "cwd": item["cwd"] })),
            summary_of(&item["command"]),
        )),
        "fileChange" => Some((
            "file_change".into(),
            detail(&serde_json::json!({ "changes": item["changes"] })),
            summary_of(&item["changes"][0]["path"]),
        )),
        "mcpToolCall" | "dynamicToolCall" => {
            let tool = item["tool"].as_str().unwrap_or_default().to_string();
            Some((tool.clone(), detail(&item["arguments"]), Some(tool)))
        }
        "webSearch" => Some((
            "web_search".into(),
            detail(&serde_json::json!({ "query": item["query"] })),
            summary_of(&item["query"]),
        )),
        "imageView" => Some((
            "image_view".into(),
            detail(&serde_json::json!({ "path": item["path"] })),
            summary_of(&item["path"]),
        )),
        "imageGeneration" => Some((
            "image_generation".into(),
            detail(
                &serde_json::json!({ "revisedPrompt": item["revisedPrompt"], "savedPath": item["savedPath"] }),
            ),
            summary_of(&item["revisedPrompt"]),
        )),
        "sleep" => Some((
            "sleep".into(),
            detail(&serde_json::json!({ "durationMs": item["durationMs"] })),
            Some(format!("{} ms", item["durationMs"])),
        )),
        "collabAgentToolCall" => {
            let tool = item["tool"].as_str().unwrap_or_default().to_string();
            Some((
                tool,
                detail(
                    &serde_json::json!({ "prompt": item["prompt"], "receiverThreadIds": item["receiverThreadIds"] }),
                ),
                summary_of(&item["prompt"]),
            ))
        }
        _ => None,
    }
}

impl CodexSubagents {
    pub fn new(
        report: Arc<dyn Fn(SubagentReport) + Send + Sync>,
        on_busy_changed: Arc<dyn Fn(bool) + Send + Sync>,
        read_metadata: Option<ReadThread>,
        spend: Option<CodexSpend>,
    ) -> Self {
        CodexSubagents {
            inner: Arc::new_cyclic(|me| Inner {
                state: Mutex::new(State::default()),
                report,
                on_busy_changed,
                read_metadata,
                spend,
                me: me.clone(),
            }),
        }
    }

    /// Anything closing the app-server process would cut short.
    pub fn busy(&self) -> bool {
        self.inner.state.lock().unwrap().busy()
    }

    /// Child turns currently addressable by `turn/interrupt`.
    pub fn live_turns(&self) -> Vec<ChildTurn> {
        self.inner
            .state
            .lock()
            .unwrap()
            .live_turns
            .iter()
            .map(|(thread_id, turn_id)| ChildTurn {
                thread_id: thread_id.clone(),
                turn_id: turn_id.clone(),
            })
            .collect()
    }

    /// Runs one change, then reports what it produced and whether busyness changed, outside the lock.
    fn with_busy<T>(&self, change: impl FnOnce(&Inner, &mut State) -> T) -> T {
        let (result, reports, spends, was_busy, busy) = {
            let mut state = self.inner.state.lock().unwrap();
            let was_busy = state.busy();
            let result = change(&self.inner, &mut state);
            (
                result,
                std::mem::take(&mut state.reports),
                std::mem::take(&mut state.spends),
                was_busy,
                state.busy(),
            )
        };
        if let Some(spend) = &self.inner.spend {
            for (thread, usage) in spends {
                spend.report(&thread, &usage);
            }
        }
        for report in reports {
            (self.inner.report)(report);
        }
        if busy != was_busy {
            (self.inner.on_busy_changed)(busy);
        }
        self.drain_metadata();
        result
    }

    /// The turn a stop of this child interrupts now, Held when it has none, None for an unknown child.
    pub fn stop(&self, thread_id: &str) -> Option<StopTarget> {
        let mut state = self.inner.state.lock().unwrap();
        if !state
            .children
            .get(thread_id)
            .is_some_and(|child| child.discovered)
        {
            return None;
        }
        if let Some(turn_id) = state.live_turns.get(thread_id) {
            return Some(StopTarget::Turn(ChildTurn {
                thread_id: thread_id.to_string(),
                turn_id: turn_id.clone(),
            }));
        }
        if state.children[thread_id].active {
            state.held_stops.insert(thread_id.to_string());
        }
        Some(StopTarget::Held)
    }

    /// The turn just started by a child whose stop was held for it.
    pub fn take_held_stop(&self, thread_id: &str) -> Option<ChildTurn> {
        let mut state = self.inner.state.lock().unwrap();
        let turn_id = state.live_turns.get(thread_id).cloned()?;
        state.held_stops.remove(thread_id).then(|| ChildTurn {
            thread_id: thread_id.to_string(),
            turn_id,
        })
    }

    pub fn set_root_thread_id(&self, thread_id: &str) {
        self.with_busy(|_, state| {
            state.root_thread_id = Some(thread_id.to_string());
            state.ignored.remove(thread_id);
            if state
                .children
                .get(thread_id)
                .is_some_and(|child| child.active)
            {
                state.active_children -= 1;
            }
            state.children.remove(thread_id);
            state.live_turns.remove(thread_id);
            state.held_stops.remove(thread_id);
            state.drop_pending_items(thread_id);
        });
    }

    /// A child item kept for a later approval request, which may carry only the item id.
    pub fn pending_item(&self, thread_id: &str, item_id: &str) -> Option<Value> {
        self.inner
            .state
            .lock()
            .unwrap()
            .pending_items
            .iter()
            .find(|(id, thread, _)| id == item_id && thread == thread_id)
            .map(|(_, _, item)| item.clone())
    }

    /// The newest child call against one MCP server, for an elicitation approval with no item id.
    pub fn pending_mcp_call(&self, thread_id: &str, server: &str) -> Option<Value> {
        self.inner
            .state
            .lock()
            .unwrap()
            .pending_items
            .iter()
            .rev()
            .find(|(_, thread, item)| {
                thread == thread_id && item["type"] == "mcpToolCall" && item["server"] == server
            })
            .map(|(_, _, item)| item.clone())
    }

    pub fn thread_started(&self, params: &Value) -> bool {
        self.with_busy(|inner, state| {
            let thread = &params["thread"];
            let id = thread["id"].as_str().unwrap_or_default();
            if state.root_thread_id.as_deref() == Some(id) {
                return false;
            }
            if is_internal_thread(thread) {
                ignore(state, id);
                return true;
            }
            let spawn = spawn_metadata(thread);
            if spawn.is_none() && !state.should_suppress(id) {
                return false;
            }
            let (nickname, role, path) = spawn.clone().unwrap_or((None, None, None));
            state.child(id);
            merge_metadata(
                inner,
                state,
                id,
                Merge {
                    model: nonempty(&thread["model"]),
                    effort: nonempty(&thread["reasoningEffort"]),
                    preview: nonempty(&thread["preview"]),
                    nickname: nickname.or_else(|| nonempty(&thread["agentNickname"])),
                    role: role.or_else(|| nonempty(&thread["agentRole"])),
                    path,
                    prompt: None,
                },
            );
            if spawn.is_some() {
                let child = state.children.get(id).unwrap();
                if !child.discovered
                    && matches!(child.lifecycle(), Lifecycle::Unknown | Lifecycle::Idle)
                {
                    set_lifecycle(state, id, Lifecycle::Working, None);
                }
                discover(state, id);
            }
            true
        })
    }

    pub fn thread_status_changed(&self, params: &Value) -> bool {
        self.with_busy(|_, state| {
            let id = params["threadId"].as_str().unwrap_or_default();
            if !state.should_suppress(id) {
                return false;
            }
            let status = &params["status"];
            if state.ignored.contains(id) {
                if matches!(status["type"].as_str(), Some("idle" | "systemError")) {
                    state.live_turns.remove(id);
                }
                return true;
            }
            state.child(id);
            match status["type"].as_str() {
                Some("active") => {
                    let flags: Vec<&str> = status["activeFlags"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .collect();
                    let waiting = if flags.contains(&"waitingOnApproval") {
                        Some("Waiting for approval".to_string())
                    } else if flags.contains(&"waitingOnUserInput") {
                        Some("Waiting for input".to_string())
                    } else {
                        None
                    };
                    set_lifecycle(state, id, Lifecycle::Working, waiting);
                }
                Some("idle" | "notLoaded") => set_lifecycle(state, id, Lifecycle::Idle, None),
                Some("systemError") => set_lifecycle(state, id, Lifecycle::Failed, None),
                _ => {}
            }
            true
        })
    }

    pub fn turn_started(&self, params: &Value) -> bool {
        self.with_busy(|_, state| {
            let id = params["threadId"].as_str().unwrap_or_default();
            if !state.should_suppress(id) {
                return false;
            }
            if state.ignored.contains(id) {
                return true;
            }
            state.live_turns.insert(
                id.to_string(),
                params["turn"]["id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            );
            state.child(id);
            set_lifecycle(state, id, Lifecycle::Working, None);
            request_metadata(state, id);
            true
        })
    }

    pub fn turn_completed(&self, params: &Value) -> bool {
        self.with_busy(|_, state| {
            let id = params["threadId"].as_str().unwrap_or_default();
            if !state.should_suppress(id) {
                return false;
            }
            let turn_id = params["turn"]["id"].as_str().unwrap_or_default();
            let live = state.live_turns.get(id).cloned();
            if live.as_deref().is_none_or(|live| live == turn_id) {
                state.live_turns.remove(id);
            }
            if state.ignored.contains(id) || live.as_deref().is_some_and(|live| live != turn_id) {
                return true;
            }
            state.child(id);
            match params["turn"]["status"].as_str() {
                Some("completed") => set_lifecycle(state, id, Lifecycle::Idle, None),
                Some("interrupted") => set_lifecycle(state, id, Lifecycle::Stopped, None),
                Some("failed") => set_lifecycle(
                    state,
                    id,
                    Lifecycle::Failed,
                    params["turn"]["error"]["message"]
                        .as_str()
                        .map(String::from),
                ),
                _ => {}
            }
            true
        })
    }

    pub fn token_usage_updated(&self, params: &Value) -> bool {
        self.with_busy(|_, state| {
            let id = params["threadId"].as_str().unwrap_or_default();
            if !state.should_suppress(id) {
                return false;
            }
            if state.ignored.contains(id) {
                return true;
            }
            state
                .spends
                .push((id.to_string(), params["tokenUsage"].clone()));
            let total = params["tokenUsage"]["total"]["totalTokens"]
                .as_i64()
                .unwrap_or(-1);
            let child = state.child(id);
            if total >= 0 {
                child.total_tokens = child.total_tokens.max(total as u64);
            }
            emit_progress(state, id);
            true
        })
    }

    pub fn error(&self, params: &Value) -> bool {
        self.with_busy(|_, state| {
            let id = params["threadId"].as_str().unwrap_or_default();
            if !state.should_suppress(id) {
                return false;
            }
            if params["willRetry"] == true {
                return true;
            }
            if let Some(live) = state.live_turns.get(id) {
                if params["turnId"].as_str() != Some(live.as_str()) {
                    return true;
                }
            }
            state.live_turns.remove(id);
            if !state.ignored.contains(id) {
                state.child(id);
                set_lifecycle(
                    state,
                    id,
                    Lifecycle::Failed,
                    params["error"]["message"].as_str().map(String::from),
                );
            }
            true
        })
    }

    pub fn thread_closed(&self, params: &Value) -> bool {
        self.with_busy(|_, state| {
            let id = params["threadId"].as_str().unwrap_or_default();
            if !state.should_suppress(id) {
                return false;
            }
            state.live_turns.remove(id);
            if state.ignored.remove(id) {
                return true;
            }
            if state.children.contains_key(id) {
                state.drop_pending_items(id);
                if state.children[id].lifecycle() != Lifecycle::Failed {
                    set_lifecycle(state, id, Lifecycle::Stopped, None);
                }
            }
            true
        })
    }

    pub fn item_started(&self, params: &Value) -> bool {
        self.item(
            params["threadId"].as_str().unwrap_or_default(),
            &params["item"],
            true,
        )
    }

    pub fn item_completed(&self, params: &Value) -> bool {
        self.item(
            params["threadId"].as_str().unwrap_or_default(),
            &params["item"],
            false,
        )
    }

    /// Marks unfinished visible children stopped, then releases every session-only reference.
    pub fn close(&self) {
        self.with_busy(|_, state| {
            if state.closed {
                return;
            }
            let ids: Vec<String> = state
                .children
                .values()
                .filter(|child| {
                    child.discovered
                        && matches!(child.lifecycle(), Lifecycle::Unknown | Lifecycle::Working)
                })
                .map(|child| child.id.clone())
                .collect();
            for id in ids {
                set_lifecycle(state, &id, Lifecycle::Stopped, None);
            }
            state.closed = true;
            state.live_turns.clear();
            state.pending_items.clear();
            state.ignored.clear();
            state.children.clear();
            state.metadata_queue.clear();
            state.active_children = 0;
        });
    }

    fn item(&self, thread_id: &str, item: &Value, started: bool) -> bool {
        self.with_busy(|inner, state| {
            if item["type"] == "subAgentActivity" {
                receive_subagent_activity(state, item);
                return true;
            }
            if item["type"] == "collabAgentToolCall" {
                remember_collaboration(inner, state, item);
            }
            if !state.should_suppress(thread_id) {
                return false;
            }
            if state.ignored.contains(thread_id) {
                return true;
            }
            state.child(thread_id);
            let item_id = item["id"].as_str().unwrap_or_default().to_string();
            if started {
                state.pending_items.retain(|(id, _, _)| *id != item_id);
                state
                    .pending_items
                    .push((item_id.clone(), thread_id.to_string(), item.clone()));
            }
            receive_child_item(inner, state, thread_id, item, started);
            request_metadata(state, thread_id);
            if !started {
                state.pending_items.retain(|(id, _, _)| *id != item_id);
            }
            true
        })
    }

    /// A large spawn batch must not flood the app-server with simultaneous reads.
    fn drain_metadata(&self) {
        let Some(read) = self.inner.read_metadata.clone() else {
            return;
        };
        loop {
            let next = {
                let mut state = self.inner.state.lock().unwrap();
                if state.closed || state.reading_metadata >= 4 {
                    return;
                }
                let Some(id) = state.metadata_queue.pop_front() else {
                    return;
                };
                let Some(child) = state.children.get_mut(&id) else {
                    continue;
                };
                child.metadata_reads += 1;
                let generation = child.generation;
                state.reading_metadata += 1;
                (id, generation)
            };
            let weak = self.inner.me.clone();
            let read = read.clone();
            tokio::spawn(async move {
                let thread = read(next.0.clone()).await;
                let Some(inner) = weak.upgrade() else { return };
                let this = CodexSubagents { inner };
                this.with_busy(|inner, state| {
                    state.reading_metadata -= 1;
                    let current = state
                        .children
                        .get(&next.0)
                        .is_some_and(|child| child.generation == next.1);
                    if let Some(child) = state.children.get_mut(&next.0).filter(|_| current) {
                        child.metadata_pending = false;
                    }
                    let Some(thread) = thread.filter(|thread| thread["id"] == next.0.as_str())
                    else {
                        return;
                    };
                    if state.closed || !current {
                        return;
                    }
                    let child = &state.children[&next.0];
                    let (model, effort) = (
                        child.model.clone().or_else(|| nonempty(&thread["model"])),
                        child
                            .effort
                            .clone()
                            .or_else(|| nonempty(&thread["reasoningEffort"])),
                    );
                    merge_metadata(
                        inner,
                        state,
                        &next.0,
                        Merge {
                            model,
                            effort,
                            ..Default::default()
                        },
                    );
                });
            });
        }
    }
}

#[derive(Default)]
struct Merge {
    prompt: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    preview: Option<String>,
    nickname: Option<String>,
    role: Option<String>,
    path: Option<String>,
}

fn path_leaf(value: Option<&str>) -> Option<String> {
    value?
        .split('/')
        .rfind(|part| !part.is_empty())
        .map(String::from)
}

fn description(child: &Child) -> String {
    let text = child
        .prompt
        .as_deref()
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
        .map(String::from)
        .or_else(|| child.preview.clone())
        .or_else(|| child.nickname.clone())
        .or_else(|| path_leaf(child.path.as_deref()))
        .or_else(|| child.role.clone())
        .unwrap_or_else(|| "Subagent".into());
    text.chars().take(100_000).collect()
}

fn metadata(child: &Child) -> SubagentMetadata {
    SubagentMetadata {
        model: child.model.clone(),
        effort: child.effort.clone(),
        prompt: child.prompt.clone(),
    }
}

fn merge_metadata(_inner: &Inner, state: &mut State, id: &str, merge: Merge) {
    let Some(child) = state.children.get_mut(id) else {
        return;
    };
    let before = description(child);
    let role = child.role.clone();
    let changed = (child.prompt.is_none() && merge.prompt.is_some())
        || merge
            .model
            .as_ref()
            .is_some_and(|model| child.model.as_ref() != Some(model))
        || merge
            .effort
            .as_ref()
            .is_some_and(|effort| child.effort.as_ref() != Some(effort));
    if child.prompt.is_none() {
        child.prompt = merge.prompt;
    }
    if merge.model.is_some() {
        child.model = merge.model;
    }
    if merge.effort.is_some() {
        child.effort = merge.effort;
    }
    child.preview = child.preview.take().or(merge.preview);
    child.nickname = child.nickname.take().or(merge.nickname);
    child.role = child.role.take().or(merge.role);
    child.path = child.path.take().or(merge.path);
    let discovered = child.discovered;
    let progressed = description(child) != before || child.role != role;
    let report = SubagentReport::Metadata {
        id: id.to_string(),
        metadata: metadata(child),
    };
    if discovered && progressed {
        emit_progress(state, id);
    }
    if discovered && changed {
        state.reports.push(report);
    }
}

fn discover(state: &mut State, id: &str) {
    if state.closed {
        return;
    }
    let Some(child) = state.children.get_mut(id) else {
        return;
    };
    if child.discovered {
        request_metadata(state, id);
        return;
    }
    child.discovered = true;
    let started = SubagentReport::Started {
        id: id.to_string(),
        description: description(child),
        agent_type: child.role.clone(),
        session_scoped: true,
        metadata: metadata(child),
    };
    child.reported_lifecycle = Some(Lifecycle::Working);
    child.reported_status_summary = Some(None);
    let lifecycle = child.lifecycle();
    let status_summary = child.status_summary.is_some();
    let progressed =
        child.total_tokens > 0 || child.last_tool_name.is_some() || child.summary.is_some();
    let buffered: Vec<SubagentReport> = child.buffered_activity.drain(..).collect();
    state.reports.push(started);
    if (lifecycle != Lifecycle::Unknown && lifecycle != Lifecycle::Working)
        || (lifecycle == Lifecycle::Working && status_summary)
    {
        emit_lifecycle(state, id);
    }
    if progressed {
        emit_progress(state, id);
    }
    state.reports.extend(buffered);
    request_metadata(state, id);
}

/// V2 can announce only subAgentActivity. Read the child's settings without loading its transcript.
fn request_metadata(state: &mut State, id: &str) {
    if state.closed {
        return;
    }
    let Some(child) = state.children.get_mut(id) else {
        return;
    };
    if !child.discovered
        || child.metadata_pending
        || (child.model.is_some() && child.effort.is_some())
        || child.metadata_reads >= 3
    {
        return;
    }
    child.metadata_pending = true;
    state.metadata_queue.push_back(id.to_string());
}

fn set_lifecycle(state: &mut State, id: &str, lifecycle: Lifecycle, summary: Option<String>) {
    let Some(child) = state.children.get_mut(id) else {
        return;
    };
    let was_active = child.active;
    if lifecycle == Lifecycle::Working && child.lifecycle() != Lifecycle::Working {
        child.last_tool_name = None;
    }
    child.lifecycle = Some(lifecycle);
    child.active = lifecycle == Lifecycle::Working;
    let active = child.active;
    child.status_summary = if lifecycle == Lifecycle::Working {
        summary.clone()
    } else {
        None
    };
    if let Some(summary) = summary.filter(|_| lifecycle != Lifecycle::Working) {
        child.summary = Some(first_line(&summary));
    }
    if active != was_active {
        state.active_children += if active { 1 } else { -1 };
    }
    if lifecycle != Lifecycle::Working {
        state.live_turns.remove(id);
        state.held_stops.remove(id);
        state.drop_pending_items(id);
    }
    emit_lifecycle(state, id);
}

fn emit_lifecycle(state: &mut State, id: &str) {
    if state.closed {
        return;
    }
    let Some(child) = state.children.get_mut(id) else {
        return;
    };
    if !child.discovered {
        return;
    }
    let lifecycle = child.lifecycle();
    let report = match lifecycle {
        Lifecycle::Unknown => return,
        Lifecycle::Working | Lifecycle::Idle => {
            let summary = if lifecycle == Lifecycle::Working {
                child.status_summary.clone()
            } else {
                child.summary.clone()
            };
            if child.reported_lifecycle == Some(lifecycle)
                && child.reported_status_summary.as_ref() == Some(&summary)
            {
                return;
            }
            child.reported_lifecycle = Some(lifecycle);
            child.reported_status_summary = Some(summary.clone());
            SubagentReport::Status {
                id: id.to_string(),
                status: if lifecycle == Lifecycle::Working {
                    SubagentLiveStatus::Working
                } else {
                    SubagentLiveStatus::Idle
                },
                summary: summary.filter(|s| !s.is_empty()),
            }
        }
        Lifecycle::Failed | Lifecycle::Stopped => {
            let summary = child.summary.clone().unwrap_or_default();
            if child.reported_lifecycle == Some(lifecycle)
                && child.reported_status_summary.as_ref() == Some(&Some(summary.clone()))
            {
                return;
            }
            child.reported_lifecycle = Some(lifecycle);
            child.reported_status_summary = Some(Some(summary.clone()));
            let status = if lifecycle == Lifecycle::Failed {
                SubagentTerminalStatus::Failed
            } else {
                SubagentTerminalStatus::Stopped
            };
            SubagentReport::Finished {
                id: id.to_string(),
                status,
                summary,
            }
        }
    };
    state.reports.push(report);
}

fn emit_progress(state: &mut State, id: &str) {
    if state.closed {
        return;
    }
    let Some(child) = state.children.get_mut(id) else {
        return;
    };
    if !child.discovered {
        return;
    }
    let description = description(child);
    let key = serde_json::json!([
        description,
        child.last_tool_name,
        child.summary,
        child.total_tokens
    ])
    .to_string();
    if child.progress_key.as_deref() == Some(key.as_str()) {
        return;
    }
    child.progress_key = Some(key);
    let report = SubagentReport::Progress {
        id: id.to_string(),
        description,
        agent_type: child.role.clone(),
        last_tool_name: child.last_tool_name.clone(),
        summary: child.summary.clone(),
        total_tokens: child.total_tokens,
    };
    state.reports.push(report);
}

fn queue_activity(state: &mut State, id: &str, activity: SubagentReport) {
    let SubagentReport::Activity { activity_id, .. } = &activity else {
        return;
    };
    let closed = state.closed;
    let Some(child) = state.children.get_mut(id) else {
        return;
    };
    if !child.seen_activity.insert(activity_id.clone()) {
        return;
    }
    child.seen_order.push_back(activity_id.clone());
    if child.seen_order.len() > MAX_SEEN_ACTIVITY {
        if let Some(oldest) = child.seen_order.pop_front() {
            child.seen_activity.remove(&oldest);
        }
    }
    if child.discovered && !closed {
        state.reports.push(activity);
        return;
    }
    child.buffered_activity.push_back(activity);
    if child.buffered_activity.len() > MAX_BUFFERED_ACTIVITY {
        child.buffered_activity.pop_front();
    }
}

fn ignore(state: &mut State, id: &str) {
    state.ignored.insert(id.to_string());
    state.live_turns.remove(id);
    if state.children.get(id).is_some_and(|child| child.active) {
        state.active_children -= 1;
    }
    state.children.remove(id);
    state.drop_pending_items(id);
}

fn receive_subagent_activity(state: &mut State, item: &Value) {
    let raw = item["agentPath"]
        .as_str()
        .unwrap_or_default()
        .trim_end_matches('/');
    let path = if raw.is_empty() { "/" } else { raw };
    let id = item["agentThreadId"].as_str().unwrap_or_default();
    if state.root_thread_id.as_deref() == Some(id)
        || path == "/root"
        || path == "/"
        || state.ignored.contains(id)
    {
        return;
    }
    state.child(id);
    merge_metadata_path(state, id, path);
    match item["kind"].as_str() {
        Some("started") => set_lifecycle(state, id, Lifecycle::Working, None),
        Some("completed") => set_lifecycle(state, id, Lifecycle::Idle, None),
        Some("interrupted") => set_lifecycle(state, id, Lifecycle::Stopped, None),
        _ if state.children[id].lifecycle() == Lifecycle::Unknown => {
            set_lifecycle(state, id, Lifecycle::Idle, None)
        }
        _ => {}
    }
    discover(state, id);
}

fn merge_metadata_path(state: &mut State, id: &str, path: &str) {
    let Some(child) = state.children.get_mut(id) else {
        return;
    };
    let before = description(child);
    child.path = child.path.take().or(Some(path.to_string()));
    if child.discovered && description(child) != before {
        emit_progress(state, id);
    }
}

fn remember_collaboration(inner: &Inner, state: &mut State, item: &Value) {
    for receiver in item["receiverThreadIds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if state.root_thread_id.as_deref() == Some(receiver) || state.ignored.contains(receiver) {
            continue;
        }
        let child = state.child(receiver);
        if item["tool"] == "spawnAgent" {
            let merge = Merge {
                prompt: item["prompt"].as_str().map(String::from),
                model: child.model.clone().or_else(|| nonempty(&item["model"])),
                effort: child
                    .effort
                    .clone()
                    .or_else(|| nonempty(&item["reasoningEffort"])),
                ..Default::default()
            };
            merge_metadata(inner, state, receiver, merge);
        }
        let agent = &item["agentsStates"][receiver];
        if !agent.is_object() {
            continue;
        }
        let message = agent["message"].as_str().map(String::from);
        match agent["status"].as_str() {
            Some("pendingInit" | "running") => {
                set_lifecycle(state, receiver, Lifecycle::Working, message)
            }
            Some("completed") => set_lifecycle(state, receiver, Lifecycle::Idle, message),
            Some("errored") => set_lifecycle(state, receiver, Lifecycle::Failed, message),
            Some("interrupted" | "shutdown" | "notFound") => {
                set_lifecycle(state, receiver, Lifecycle::Stopped, message)
            }
            _ => {}
        }
    }
}

fn receive_child_item(inner: &Inner, state: &mut State, id: &str, item: &Value, started: bool) {
    let item_id = item["id"].as_str().unwrap_or_default();
    match item["type"].as_str() {
        Some("userMessage") => {
            let text = item["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|part| part["type"] == "text")
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            if !text.is_empty() && state.children[id].prompt.is_none() {
                merge_metadata(
                    inner,
                    state,
                    id,
                    Merge {
                        prompt: Some(text),
                        ..Default::default()
                    },
                );
            }
        }
        Some(kind @ ("exitedReviewMode" | "agentMessage")) => {
            if started {
                return;
            }
            let field = if kind == "exitedReviewMode" {
                "review"
            } else {
                "text"
            };
            let text = item[field].as_str().unwrap_or_default().trim().to_string();
            if text.is_empty() {
                return;
            }
            state.children.get_mut(id).unwrap().summary = Some(first_line(&text));
            let suffix = if kind == "exitedReviewMode" {
                "review"
            } else {
                "text"
            };
            queue_activity(
                state,
                id,
                SubagentReport::Activity {
                    id: id.to_string(),
                    activity_id: format!("{item_id}:{suffix}"),
                    kind: ActivityKind::Text,
                    title: None,
                    text,
                },
            );
            emit_progress(state, id);
        }
        _ => {
            let Some((title, text, summary)) = tool_activity(item) else {
                return;
            };
            if started {
                set_lifecycle(state, id, Lifecycle::Working, None);
            }
            let child = state.children.get_mut(id).unwrap();
            child.last_tool_name = Some(title.clone());
            if let Some(summary) = summary {
                child.summary = Some(first_line(&summary));
            }
            queue_activity(
                state,
                id,
                SubagentReport::Activity {
                    id: id.to_string(),
                    activity_id: item_id.to_string(),
                    kind: ActivityKind::Tool,
                    title: Some(title),
                    text,
                },
            );
            emit_progress(state, id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_spawned_child_reports_once_discovered_and_stops_with_its_session() {
        let reports = Arc::new(Mutex::new(Vec::new()));
        let sink = reports.clone();
        let subagents = CodexSubagents::new(
            Arc::new(move |report| sink.lock().unwrap().push(report)),
            Arc::new(|_| {}),
            None,
            None,
        );
        subagents.set_root_thread_id("root");
        assert!(subagents.item_started(&json!({ "threadId": "child", "item": { "type": "commandExecution", "id": "i1", "command": "ls", "cwd": "/" } })));
        assert!(reports.lock().unwrap().is_empty());
        subagents.thread_started(&json!({ "thread": { "id": "child", "source": { "subAgent": { "thread_spawn": { "agent_nickname": "Scout" } } } } }));
        let seen = reports.lock().unwrap().clone();
        assert!(
            matches!(&seen[0], SubagentReport::Started { id, description, .. } if id == "child" && description == "Scout")
        );
        assert!(seen.iter().any(|report| matches!(report, SubagentReport::Activity { activity_id, .. } if activity_id == "i1")));
        assert!(subagents.busy());
        subagents.close();
        assert!(reports.lock().unwrap().iter().any(|report| matches!(
            report,
            SubagentReport::Finished {
                status: SubagentTerminalStatus::Stopped,
                ..
            }
        )));
        assert!(!subagents.busy());
    }
}
