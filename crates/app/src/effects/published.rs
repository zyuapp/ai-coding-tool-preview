//! The workspace as last handed to paired computers, kept as JSON by thread and message so a change
//! serializes and compares only the threads and messages it touched.

use act_core::domain::conversation::ConversationMessage;
use act_core::domain::thread::Thread;
use act_core::shared::Shared;
use act_core::WorkspaceState;
use act_host::workspace_patches::{workspace_patches, WorkspacePatch, WorkspaceSplice};
use serde_json::Value;

struct PublishedThread {
    thread: Shared<Thread>,
    /// Every field but its messages.
    fields: Value,
    messages: Vec<(Shared<ConversationMessage>, Value)>,
}

impl PublishedThread {
    fn new(thread: &Shared<Thread>, earlier: Option<PublishedThread>) -> PublishedThread {
        let mut held = earlier.map(|earlier| earlier.messages).unwrap_or_default().into_iter();
        let messages = thread
            .messages
            .iter()
            .map(|message| match held.find(|(was, _)| Shared::same(was, message)) {
                Some(kept) => kept,
                None => (message.clone(), message_value(message)),
            })
            .collect();
        PublishedThread { thread: thread.clone(), fields: thread_fields(thread), messages }
    }

    fn value(&self) -> Value {
        let mut value = self.fields.clone();
        if let Value::Object(fields) = &mut value {
            fields.insert("messages".into(), Value::Array(self.messages.iter().map(|(_, value)| value.clone()).collect()));
        }
        value
    }
}

fn message_value(message: &ConversationMessage) -> Value {
    serde_json::to_value(message).unwrap_or(Value::Null)
}

fn thread_fields(thread: &Thread) -> Value {
    let bare = Thread { messages: Vec::new(), ..thread.clone() };
    let mut value = bare.to_json().unwrap_or(Value::Null);
    if let Value::Object(fields) = &mut value {
        fields.remove("messages");
    }
    value
}

fn rest_value(state: &WorkspaceState) -> Value {
    let bare = WorkspaceState { threads: Vec::new(), ..state.clone() };
    let mut value = bare.to_json().unwrap_or(Value::Null);
    if let Value::Object(fields) = &mut value {
        fields.remove("threads");
    }
    value
}

fn at(path: &[Value], patches: Vec<WorkspacePatch>) -> impl Iterator<Item = WorkspacePatch> + '_ {
    patches.into_iter().map(move |patch| match patch {
        WorkspacePatch::Splice { path: inner, splice } => WorkspacePatch::Splice { path: path.iter().cloned().chain(inner).collect(), splice },
        WorkspacePatch::Set { path: inner, value, remove } => WorkspacePatch::Set { path: path.iter().cloned().chain(inner).collect(), value, remove },
    })
}

/// How many leading and trailing items two lists share, the trailing ones not overlapping the leading.
fn shared_ends<A, B>(before: &[A], after: &[B], same: impl Fn(&A, &B) -> bool) -> (usize, usize) {
    let mut prefix = 0;
    while prefix < before.len() && prefix < after.len() && same(&before[prefix], &after[prefix]) {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < before.len() - prefix && suffix < after.len() - prefix && same(&before[before.len() - suffix - 1], &after[after.len() - suffix - 1]) {
        suffix += 1;
    }
    (prefix, suffix)
}

#[derive(Default)]
pub struct Published {
    rest: Value,
    threads: Vec<PublishedThread>,
}

impl Published {
    pub fn new(state: &WorkspaceState) -> Published {
        Published { rest: rest_value(state), threads: state.threads.iter().map(|thread| PublishedThread::new(thread, None)).collect() }
    }

    /// The state whole, as `serde_json::to_value` writes it.
    pub fn value(&self) -> Value {
        let mut value = self.rest.clone();
        if let Value::Object(fields) = &mut value {
            fields.insert("threads".into(), Value::Array(self.threads.iter().map(PublishedThread::value).collect()));
        }
        value
    }

    /// Takes `state` as the published state; answers the patches that turn the last one into it.
    pub fn update(&mut self, state: &WorkspaceState) -> Vec<WorkspacePatch> {
        let rest = rest_value(state);
        let mut patches = workspace_patches(&self.rest, &rest);
        self.rest = rest;
        let before = std::mem::take(&mut self.threads);
        let (prefix, suffix) = shared_ends(&before, &state.threads, |was, is| was.thread.id == is.id);
        let mut before: Vec<Option<PublishedThread>> = before.into_iter().map(Some).collect();
        let changed = |index: usize, was: &mut Option<PublishedThread>, is: &Shared<Thread>, patches: &mut Vec<WorkspacePatch>| -> PublishedThread {
            let was = was.take().expect("each published thread is taken once");
            if Shared::same(&was.thread, is) {
                return was;
            }
            let path = [Value::from("threads"), Value::from(index)];
            let fields = thread_fields(is);
            patches.extend(at(&path, workspace_patches(&was.fields, &fields)));
            let PublishedThread { messages: held, .. } = was;
            let (head, tail) = shared_ends(&held, &is.messages, |(was, _), is| Shared::same(was, is));
            let path = [Value::from("threads"), Value::from(index), Value::from("messages")];
            let mut held: Vec<Option<(Shared<ConversationMessage>, Value)>> = held.into_iter().map(Some).collect();
            let mut messages = Vec::with_capacity(is.messages.len());
            messages.extend(held[..head].iter_mut().map(|kept| kept.take().expect("kept message")));
            if held.len() == is.messages.len() {
                for offset in head..is.messages.len() - tail {
                    let message = &is.messages[offset];
                    let (_, was) = held[offset].take().expect("held message");
                    let value = message_value(message);
                    let mut path = path.to_vec();
                    path.push(Value::from(offset));
                    patches.extend(at(&path, workspace_patches(&was, &value)));
                    messages.push((message.clone(), value));
                }
            } else {
                let fresh: Vec<(Shared<ConversationMessage>, Value)> = is.messages[head..is.messages.len() - tail].iter().map(|message| (message.clone(), message_value(message))).collect();
                let splice = WorkspaceSplice { index: head, delete_count: held.len() - head - tail, items: fresh.iter().map(|(_, value)| value.clone()).collect() };
                patches.push(WorkspacePatch::Splice { path: path.to_vec(), splice });
                messages.extend(fresh);
            }
            let start = held.len() - tail;
            messages.extend(held[start..].iter_mut().map(|kept| kept.take().expect("kept message")));
            PublishedThread { thread: is.clone(), fields, messages }
        };
        let mut threads = Vec::with_capacity(state.threads.len());
        for index in 0..prefix {
            let thread = changed(index, &mut before[index], &state.threads[index], &mut patches);
            threads.push(thread);
        }
        let middle = prefix..state.threads.len() - suffix;
        if before.len() != state.threads.len() || !middle.is_empty() {
            let removed = before.len() - prefix - suffix;
            let fresh: Vec<PublishedThread> = state.threads[middle.clone()]
                .iter()
                .map(|thread| {
                    let earlier = before[prefix..prefix + removed].iter_mut().find(|was| was.as_ref().is_some_and(|was| was.thread.id == thread.id)).and_then(Option::take);
                    PublishedThread::new(thread, earlier)
                })
                .collect();
            if removed > 0 || !fresh.is_empty() {
                let splice = WorkspaceSplice { index: prefix, delete_count: removed, items: fresh.iter().map(PublishedThread::value).collect() };
                patches.push(WorkspacePatch::Splice { path: vec![Value::from("threads")], splice });
            }
            threads.extend(fresh);
        }
        let first_after = before.len() - suffix;
        for offset in 0..suffix {
            let index = state.threads.len() - suffix + offset;
            let thread = changed(index, &mut before[first_after + offset], &state.threads[index], &mut patches);
            threads.push(thread);
        }
        self.threads = threads;
        patches
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use act_core::domain::conversation::{create_conversation_message, MessageKind};
    use act_core::domain::engine::AgentEngine;
    use act_core::domain::run::ExecutionPolicy;
    use act_host::workspace_patches::apply_workspace_patches;

    fn thread(id: &str, messages: usize) -> Thread {
        let mut thread = Thread::new(id, id, AgentEngine::Claude, ExecutionPolicy::Confirm, 1);
        for index in 0..messages {
            thread.messages.push(Shared::new(create_conversation_message(MessageKind::User, &format!("{id} {index}"), None)));
        }
        thread
    }

    fn check(published: &mut Published, replica: &mut Value, state: &WorkspaceState) {
        let patches = published.update(state);
        apply_workspace_patches(replica, &patches).expect("patches apply");
        let whole = serde_json::to_value(state).unwrap();
        assert_eq!(*replica, whole);
        assert_eq!(published.value(), whole);
    }

    #[test]
    fn patches_bring_a_replica_to_the_state() {
        let mut state = WorkspaceState::default();
        state.threads = vec![thread("a", 3).into(), thread("b", 2).into(), thread("c", 0).into()];
        let mut published = Published::new(&state);
        let mut replica = published.value();
        assert_eq!(replica, serde_json::to_value(&state).unwrap());

        if let Some(message) = state.threads[1].messages.last_mut() {
            message.text.push_str(" more");
        }
        check(&mut published, &mut replica, &state);

        state.threads[0].messages.push(Shared::new(create_conversation_message(MessageKind::Assistant, "new", None)));
        state.threads[2].title = "renamed".into();
        check(&mut published, &mut replica, &state);

        state.threads.insert(1, thread("d", 1).into());
        state.current_id = Some("d".into());
        check(&mut published, &mut replica, &state);

        state.threads.remove(0);
        state.threads[1].messages.remove(0);
        check(&mut published, &mut replica, &state);

        let moved = state.threads.remove(2);
        state.threads.insert(0, moved);
        check(&mut published, &mut replica, &state);
    }
}
