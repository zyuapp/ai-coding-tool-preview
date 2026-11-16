//! Transcripts on disk: a thread's conversation is read when it comes on screen, and an input that
//! reads or appends to one waits for it (src/host/runtime-history.ts).

use std::collections::HashSet;

use act_core::events::WorkspaceInput;
use act_core::reducer::shortcut_commands;
use act_core::state::WorkspaceState;
use act_core::AppCommand;
use serde_json::Value;

#[derive(Default)]
pub struct History {
    loading: HashSet<String>,
    held: Vec<(Vec<String>, WorkspaceInput, Reply)>,
}

/// Who waits on a held input's answer, if anyone.
pub enum Reply {
    None,
    /// A reply counted toward the input it answers.
    Tracked(crate::effects::tracking::Tracker),
    /// An input from outside the window, whose answer is awaited.
    Execute(crate::effects::tracking::Tracker, futures::channel::oneshot::Sender<act_core::CommandResult>),
}

impl History {
    /// Whether a read should start: false while one for the thread is already out.
    pub fn begin(&mut self, task_id: &str) -> bool {
        self.loading.insert(task_id.to_string())
    }

    pub fn loaded(&mut self, task_id: &str) {
        self.loading.remove(task_id);
    }

    pub fn hold(&mut self, ids: Vec<String>, input: WorkspaceInput, reply: Reply) {
        self.held.push((ids, input, reply));
    }

    /// The inputs no longer waiting on any transcript, in the order they arrived.
    pub fn release(&mut self, task_id: &str, state: &WorkspaceState) -> Vec<(WorkspaceInput, Reply)> {
        let mut ready = Vec::new();
        let mut waiting = Vec::new();
        for (mut ids, input, reply) in std::mem::take(&mut self.held) {
            ids.retain(|id| id != task_id && state.thread(id).is_some_and(|thread| thread.history_summary.is_some()));
            if ids.is_empty() {
                ready.push((input, reply));
            } else {
                waiting.push((ids, input, reply));
            }
        }
        self.held = waiting;
        ready
    }

    /// The unloaded threads an input reads or appends to. Selection does not wait: it lands at once
    /// and the thread on screen loads behind its loading state.
    pub fn needed(&self, state: &WorkspaceState, input: &WorkspaceInput) -> Vec<String> {
        let mut ids: Vec<String> = Vec::new();
        collect(state, input, &mut ids);
        let mut unique = Vec::new();
        for id in ids {
            if !unique.contains(&id) && state.thread(&id).is_some_and(|thread| thread.history_summary.is_some()) {
                unique.push(id);
            }
        }
        unique
    }
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(String::from)
}

fn collect(state: &WorkspaceState, input: &WorkspaceInput, ids: &mut Vec<String>) {
    if let WorkspaceInput::Command(AppCommand::Shortcut { action, surface }) = input {
        let commands = shortcut_commands(state, action, *surface);
        if commands.iter().any(|command| matches!(command, AppCommand::TaskNew { .. })) {
            return;
        }
        for command in commands {
            collect(state, &WorkspaceInput::Command(command), ids);
        }
        return;
    }
    let Ok(value) = serde_json::to_value(input) else { return };
    let current = state.current_id.clone();
    let task_or_current = || text(&value, "taskId").or_else(|| current.clone());
    let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "task.send" => {
            if let Some(id) = text(&value, "taskId").or_else(|| if value.get("text").is_none() { current.clone() } else { None }) {
                ids.push(id);
            }
        }
        "task.fork" | "task.set-worktree" | "task.move-worktree" | "run.compact" | "review.start" => ids.extend(task_or_current()),
        "view.move-worktree" => {
            if !value.get("worktree").is_none_or(Value::is_null) {
                ids.extend(current);
            }
        }
        "view.fork-confirm" => ids.extend(state.fork_name.as_ref().map(|fork| fork.task_id.clone())),
        "side-chat.open" => ids.extend(current),
        "view.find-open" | "view.find-query" | "view.find-step" => {
            let target = value.get("target").filter(|target| !target.is_null()).cloned().or_else(|| state.find.as_ref().and_then(|find| serde_json::to_value(&find.target).ok()));
            if let Some(target) = target {
                if target.get("kind").and_then(Value::as_str) == Some("thread") {
                    ids.extend(text(&target, "taskId"));
                }
            } else if kind == "view.find-open" {
                ids.extend(current);
            }
        }
        "agent.events" => {
            for event in value.get("events").and_then(Value::as_array).into_iter().flatten() {
                ids.extend(text(event, "taskId"));
            }
        }
        "run.event" | "thread.event" => ids.extend(value.get("event").and_then(|event| text(event, "taskId"))),
        "automation.fired" => ids.extend(value.get("fire").and_then(|fire| text(fire, "taskId"))),
        "run.resolved" => ids.extend(text(&value, "pendingId").and_then(|pending| state.pending_runs.get(&pending)).and_then(|pending| pending.task_id.clone())),
        "worktree.created" => ids.extend(text(&value, "taskId")),
        "worktree.released" => {
            let worktree = text(&value, "taskId").and_then(|id| state.thread(&id).and_then(|thread| thread.worktree_id.clone()));
            claimants(state, worktree.as_deref(), ids);
        }
        "worktree.deleted" => {
            let root = text(&value, "root");
            let id = text(&value, "worktreeId");
            let worktree = state.worktrees.iter().find(|worktree| Some(&worktree.id) == id.as_ref() || Some(&worktree.root) == root.as_ref()).map(|worktree| worktree.id.clone());
            claimants(state, worktree.as_deref(), ids);
        }
        _ => {}
    }
}

fn claimants(state: &WorkspaceState, worktree_id: Option<&str>, ids: &mut Vec<String>) {
    let Some(worktree_id) = worktree_id else { return };
    ids.extend(state.threads.iter().filter(|thread| thread.worktree_id.as_deref() == Some(worktree_id)).map(|thread| thread.id.clone()));
}
