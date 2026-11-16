//! Which threads, messages and subagents a save has to write: the workspace compared with what the
//! store last took. Side chats are never written; a thread that became one is removed.

use crate::shared::Shared;
use std::collections::{HashMap, HashSet};

use crate::domain::project::Project;
use crate::domain::run::Subagent;
use crate::domain::thread::Thread;
use crate::domain::worktree::Worktree;
use crate::persisted::{IndexedActivity, IndexedMessage, IndexedSubagent, PersistedSubagent, PersistedTask, TaskChange, TaskStoreDelta, ThreadStoreData};
use crate::state::WorkspaceState;
use crate::util::Map;

/// What the store holds, as far as a save compares against it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PersistenceSnapshot {
    pub threads: Vec<Shared<Thread>>,
    pub subagents: Map<Vec<Subagent>>,
    pub side_chats: HashSet<String>,
    pub projects: Vec<Project>,
    pub worktrees: Vec<Worktree>,
    pub last_folder: Option<String>,
}

impl PersistenceSnapshot {
    /// The parts of the workspace the store keeps.
    pub fn of(state: &WorkspaceState) -> PersistenceSnapshot {
        PersistenceSnapshot {
            threads: state.threads.clone(),
            subagents: state.subagents.clone(),
            side_chats: state.side_chat_ids(),
            projects: state.projects.clone(),
            worktrees: state.worktrees.clone(),
            last_folder: state.last_folder.clone(),
        }
    }

    /// The durable baseline: the data read from disk, never a later workspace snapshot.
    pub fn stored(data: &ThreadStoreData) -> PersistenceSnapshot {
        let mut threads = Vec::new();
        let mut subagents = Map::new();
        for stored in &data.tasks {
            let Some((thread, held, _)) = crate::reducer::store::thread_from_stored(stored) else { continue };
            if !held.is_empty() {
                subagents.insert(thread.id.clone(), held);
            }
            threads.push(Shared::new(thread));
        }
        PersistenceSnapshot {
            threads,
            subagents,
            side_chats: HashSet::new(),
            projects: data.projects.iter().filter_map(|project| serde_json::from_value(serde_json::to_value(project).ok()?).ok()).collect(),
            worktrees: data.worktrees.iter().filter_map(|worktree| serde_json::from_value(serde_json::to_value(worktree).ok()?).ok()).collect(),
            last_folder: data.last_folder.clone(),
        }
    }

    /// A thread whose conversation was read from disk after this snapshot was taken holds it now.
    pub fn adopt_messages(&mut self, task_id: &str, messages: &[Shared<crate::domain::conversation::ConversationMessage>]) {
        if let Some(thread) = self.threads.iter_mut().find(|thread| thread.id == task_id && thread.history_summary.is_some()) {
            thread.history_summary = None;
            thread.messages = messages.to_vec();
        }
    }
}

fn persisted_task(thread: &Thread) -> Option<PersistedTask> {
    let mut value = serde_json::to_value(thread).ok()?;
    if let Some(fields) = value.as_object_mut() {
        fields.remove("messages");
        fields.remove("historySummary");
    }
    serde_json::from_value(value).ok()
}

fn persisted_subagent(subagent: &Subagent) -> Option<PersistedSubagent> {
    let mut value = serde_json::to_value(subagent).ok()?;
    if let Some(fields) = value.as_object_mut() {
        fields.remove("activity");
    }
    serde_json::from_value(value).ok()
}

fn subagent_delta(before: Option<&Vec<Subagent>>, held: Option<&Vec<Subagent>>) -> (Vec<IndexedSubagent>, Vec<IndexedActivity>) {
    let mut subagents = Vec::new();
    let mut activity = Vec::new();
    if before == held {
        return (subagents, activity);
    }
    let previous: HashMap<&str, &Subagent> = before.into_iter().flatten().map(|subagent| (subagent.id.as_str(), subagent)).collect();
    for (index, subagent) in held.into_iter().flatten().enumerate() {
        let stored = previous.get(subagent.id.as_str()).copied();
        if stored == Some(subagent) {
            continue;
        }
        if let Some(record) = persisted_subagent(subagent) {
            subagents.push(IndexedSubagent { index, subagent: record });
        }
        if stored.is_some_and(|stored| stored.activity == subagent.activity) {
            continue;
        }
        for (position, item) in subagent.activity.iter().enumerate() {
            if stored.and_then(|stored| stored.activity.get(position)) == Some(item) {
                continue;
            }
            if let Ok(item) = serde_json::from_value(serde_json::to_value(item).unwrap_or_default()) {
                activity.push(IndexedActivity { subagent_id: subagent.id.clone(), index: position, item });
            }
        }
    }
    (subagents, activity)
}

/// The writes that bring the store from `previous` up to `next`.
pub fn persistence_delta(previous: Option<&PersistenceSnapshot>, next: &PersistenceSnapshot) -> TaskStoreDelta {
    let mut delta = TaskStoreDelta::default();
    if previous.is_none_or(|previous| previous.projects != next.projects) {
        delta.projects = serde_json::from_value(serde_json::to_value(&next.projects).unwrap_or_default()).ok();
    }
    if previous.is_none_or(|previous| previous.worktrees != next.worktrees) {
        delta.worktrees = serde_json::from_value(serde_json::to_value(&next.worktrees).unwrap_or_default()).ok();
    }
    if previous.is_none_or(|previous| previous.last_folder != next.last_folder) {
        delta.last_folder = Some(next.last_folder.clone());
    }
    let empty = PersistenceSnapshot::default();
    let before = previous.unwrap_or(&empty);
    let previous_threads: HashMap<&str, &Thread> = before.threads.iter().map(|thread| (thread.id.as_str(), &**thread)).collect();
    let next_ids: HashSet<&str> = next.threads.iter().map(|thread| thread.id.as_str()).collect();
    for id in before.threads.iter().map(|thread| thread.id.as_str()) {
        if !before.side_chats.contains(id) && (!next_ids.contains(id) || next.side_chats.contains(id)) {
            delta.removed_tasks.push(id.to_string());
        }
    }
    for thread in &next.threads {
        if next.side_chats.contains(&thread.id) {
            continue;
        }
        let was = if before.side_chats.contains(&thread.id) { None } else { previous_threads.get(thread.id.as_str()).copied() };
        let held_before = before.subagents.get(&thread.id);
        let held = next.subagents.get(&thread.id);
        if was == Some(thread) && held_before == held {
            continue;
        }
        let mut messages = Vec::new();
        let before_messages = was.map(|was| was.messages.as_slice()).unwrap_or(&[]);
        if was.is_none_or(|was| was.messages != thread.messages) {
            for (index, message) in thread.messages.iter().enumerate() {
                if before_messages.get(index) == Some(message) {
                    continue;
                }
                if let Ok(message) = serde_json::from_value(serde_json::to_value(message).unwrap_or_default()) {
                    messages.push(IndexedMessage { index, message });
                }
            }
        }
        let (subagents, activity) = subagent_delta(held_before, held);
        if was == Some(thread) && subagents.is_empty() && activity.is_empty() {
            continue;
        }
        let Some(task) = persisted_task(thread) else { continue };
        delta.tasks.push(TaskChange { task, messages, subagents, activity });
    }
    delta
}

/// Whether a delta writes anything at all.
pub fn has_persistence_delta(delta: &TaskStoreDelta) -> bool {
    !delta.tasks.is_empty() || !delta.removed_tasks.is_empty() || delta.projects.is_some() || delta.worktrees.is_some() || delta.last_folder.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::conversation::{create_conversation_message, MessageKind};
    use crate::domain::engine::AgentEngine;
    use crate::domain::run::ExecutionPolicy;

    #[test]
    fn a_save_writes_only_what_changed_and_never_a_side_chat() {
        let mut state = WorkspaceState::default();
        let mut thread = Thread::new("a", "A", AgentEngine::Claude, ExecutionPolicy::Confirm, 1);
        thread.messages.push(Shared::new(create_conversation_message(MessageKind::User, "one", None)));
        state.threads.push(thread.into());
        state.threads.push(Thread::new("b", "B", AgentEngine::Claude, ExecutionPolicy::Confirm, 1).into());
        let first = PersistenceSnapshot::of(&state);
        let all = persistence_delta(None, &first);
        assert_eq!(all.tasks.len(), 2);
        assert!(persistence_delta(Some(&first), &first).tasks.is_empty());
        state.threads[0].messages.push(Shared::new(create_conversation_message(MessageKind::Assistant, "two", None)));
        state.side_chats.push(crate::state::SideChat { id: "b".into(), source_thread_id: "a".into(), error: None });
        let delta = persistence_delta(Some(&first), &PersistenceSnapshot::of(&state));
        assert_eq!(delta.tasks.len(), 1);
        assert_eq!(delta.tasks[0].messages.iter().map(|message| message.index).collect::<Vec<_>>(), vec![1]);
        assert_eq!(delta.removed_tasks, vec!["b".to_string()]);
    }
}
