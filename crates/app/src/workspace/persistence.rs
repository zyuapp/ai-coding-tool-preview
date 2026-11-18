//! What the store already holds, and the writes that bring it up to the workspace
//! (src/host/workspace-persistence.ts). Values are compared, where the TS app compared references.

use std::collections::{HashMap, HashSet};

use act_core::domain::project::Project;
use act_core::domain::run::Subagent;
use act_core::domain::thread::Thread;
use act_core::domain::worktree::Worktree;
use act_core::persisted::{IndexedActivity, IndexedMessage, IndexedSubagent, TaskChange, TaskStoreDelta, ThreadStoreData};
use act_core::reducer::store::thread_from_stored;
use act_core::shared::Shared;
use act_core::state::WorkspaceState;
use gpui::Task;
use serde_json::Value;

use crate::effects::convert;

#[derive(Default)]
struct Snapshot {
    /// In the workspace's order, sharing what has not changed since.
    threads: Vec<Shared<Thread>>,
    subagents: HashMap<String, Vec<Subagent>>,
    side_chats: HashSet<String>,
    projects: Vec<Project>,
    worktrees: Vec<Worktree>,
    last_folder: Option<String>,
}

#[derive(Default)]
pub struct Persistence {
    persisted: Option<Snapshot>,
    pub ready: bool,
    pub in_flight: bool,
    pub timer: Option<Task<()>>,
}

impl Persistence {
    /// The durable baseline is the data read from disk, never a later workspace snapshot.
    pub fn adopt_loaded(&mut self, data: &ThreadStoreData) {
        let mut snapshot = Snapshot { projects: data.projects.iter().filter_map(|project| convert(project).ok()).collect(), worktrees: data.worktrees.iter().filter_map(|worktree| convert(worktree).ok()).collect(), last_folder: data.last_folder.clone(), ..Snapshot::default() };
        for stored in &data.tasks {
            let Some((thread, subagents, _)) = thread_from_stored(stored) else { continue };
            if !subagents.is_empty() {
                snapshot.subagents.insert(thread.id.clone(), subagents);
            }
            snapshot.threads.push(Shared::new(thread));
        }
        self.persisted = Some(snapshot);
    }

    /// Messages read from disk become part of the baseline, so they are never written back.
    pub fn adopt_messages(&mut self, task_id: &str, messages: &[Shared<act_core::domain::conversation::ConversationMessage>]) {
        let Some(snapshot) = &mut self.persisted else { return };
        let Some(thread) = snapshot.threads.iter_mut().find(|thread| thread.id == task_id && thread.history_summary.is_some()) else { return };
        thread.history_summary = None;
        thread.messages = messages.to_vec();
    }

    /// The writes since the last one, or None when nothing durable changed. Takes the workspace as
    /// the new baseline.
    pub fn delta(&mut self, state: &WorkspaceState) -> Option<TaskStoreDelta> {
        let side_chats = state.side_chat_ids();
        let previous = self.persisted.take().unwrap_or_default();
        let mut delta = TaskStoreDelta::default();
        if previous.projects != state.projects {
            delta.projects = Some(state.projects.iter().filter_map(|project| convert(project).ok()).collect());
        }
        if previous.worktrees != state.worktrees {
            delta.worktrees = Some(state.worktrees.iter().filter_map(|worktree| convert(worktree).ok()).collect());
        }
        if previous.last_folder != state.last_folder {
            delta.last_folder = Some(state.last_folder.clone());
        }
        // Threads still in the same order compare place by place; otherwise by id.
        let aligned = previous.threads.len() == state.threads.len() && previous.threads.iter().zip(&state.threads).all(|(was, is)| Shared::same(was, is) || was.id == is.id);
        let by_id: HashMap<&str, &Shared<Thread>> = if aligned { HashMap::new() } else { previous.threads.iter().map(|thread| (thread.id.as_str(), thread)).collect() };
        let present: HashSet<&str> = if aligned { HashSet::new() } else { state.threads.iter().map(|thread| thread.id.as_str()).collect() };
        let side_chat = |chats: &HashSet<String>, id: &String| !chats.is_empty() && chats.contains(id);
        for thread in &previous.threads {
            let id = &thread.id;
            if !side_chat(&previous.side_chats, id) && ((!aligned && !present.contains(id.as_str())) || side_chat(&side_chats, id)) {
                delta.removed_tasks.push(id.clone());
            }
        }
        let empty: Vec<Subagent> = Vec::new();
        let mut same_subagents: HashSet<&str> = HashSet::new();
        for (index, thread) in state.threads.iter().enumerate() {
            if side_chat(&side_chats, &thread.id) {
                continue;
            }
            let earlier = if aligned { previous.threads.get(index) } else { by_id.get(thread.id.as_str()).copied() };
            let before = if side_chat(&previous.side_chats, &thread.id) { None } else { earlier };
            let held_before = if previous.subagents.is_empty() { &empty } else { previous.subagents.get(&thread.id).unwrap_or(&empty) };
            let held = if state.subagents.is_empty() { &empty } else { state.subagents.get(&thread.id).unwrap_or(&empty) };
            let same = held_before == held;
            if same && !held.is_empty() {
                same_subagents.insert(thread.id.as_str());
            }
            if before == Some(thread) && same {
                continue;
            }
            let mut messages = Vec::new();
            for (index, message) in thread.messages.iter().enumerate() {
                if before.and_then(|before| before.messages.get(index)) != Some(message) {
                    if let Ok(message) = convert(message) {
                        messages.push(IndexedMessage { index, message });
                    }
                }
            }
            let (subagents, activity) = subagent_delta(held_before, held);
            if before == Some(thread) && subagents.is_empty() && activity.is_empty() {
                continue;
            }
            let Some(task) = persisted_task(thread) else { continue };
            delta.tasks.push(TaskChange { task, messages, subagents, activity });
        }
        // Subagents the store already holds keep their copy rather than being copied again.
        let mut kept = previous.subagents;
        let subagents = state
            .subagents
            .iter()
            .map(|(id, subagents)| {
                let held = kept.remove(id).filter(|held| same_subagents.contains(id.as_str()) || held == subagents);
                (id.clone(), held.unwrap_or_else(|| subagents.clone()))
            })
            .collect();
        self.persisted = Some(Snapshot {
            threads: state.threads.clone(),
            subagents,
            side_chats,
            projects: state.projects.clone(),
            worktrees: state.worktrees.clone(),
            last_folder: state.last_folder.clone(),
        });
        let changed = !delta.tasks.is_empty() || !delta.removed_tasks.is_empty() || delta.projects.is_some() || delta.worktrees.is_some() || delta.last_folder.is_some();
        changed.then_some(delta)
    }
}

fn persisted_task(thread: &Thread) -> Option<act_core::persisted::PersistedTask> {
    let mut value = thread.to_json().ok()?;
    if let Value::Object(fields) = &mut value {
        fields.remove("messages");
        fields.remove("historySummary");
    }
    serde_json::from_value(value).ok()
}

/// Only the subagents and activity items the last write did not already hold.
fn subagent_delta(before: &[Subagent], held: &[Subagent]) -> (Vec<IndexedSubagent>, Vec<IndexedActivity>) {
    let mut subagents = Vec::new();
    let mut activity = Vec::new();
    for (index, subagent) in held.iter().enumerate() {
        let stored = before.iter().find(|previous| previous.id == subagent.id);
        if stored == Some(subagent) {
            continue;
        }
        let mut record = serde_json::to_value(subagent).unwrap_or(Value::Null);
        if let Value::Object(fields) = &mut record {
            fields.remove("activity");
        }
        if let Ok(subagent_record) = serde_json::from_value(record) {
            subagents.push(IndexedSubagent { index, subagent: subagent_record });
        }
        if stored.is_some_and(|stored| stored.activity == subagent.activity) {
            continue;
        }
        for (position, item) in subagent.activity.iter().enumerate() {
            if stored.and_then(|stored| stored.activity.get(position)) == Some(item) {
                continue;
            }
            if let Ok(item) = convert(item) {
                activity.push(IndexedActivity { subagent_id: subagent.id.clone(), index: position, item });
            }
        }
    }
    (subagents, activity)
}
