//! When the window asks after pull requests (pull-request-reads.ts): as each panel that shows one
//! appears or what it asks about changes, when the window comes back, and on a slow poll until the
//! answers settle. Only what is on screen asks.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use act_core::domain::coordination::is_coordinator;
use act_core::domain::pull_request::{pull_request_settled, PULL_REQUEST_POLL_MS};
use act_core::state::{front_dock, WorkspaceState};
use act_core::view::pull_request::{member_pull_requests_view, pull_request_for, thread_asking};
use act_core::workspace::location::thread_workspace_id;
use act_core::AppCommand;
use gpui::Context;

use super::root::Root;
use crate::workspace::dispatch;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Asker {
    /// The thread in front, for its session panel.
    Thread,
    /// A coordinator in front, for its threads' rows.
    Members,
    /// The thread tab on screen in a coordinator's dock.
    Tab,
}

struct Asked {
    asking: String,
    at: Instant,
}

#[derive(Default)]
pub struct PullRequestReads {
    asked: HashMap<Asker, Asked>,
    focused: bool,
    /// Wakes the window when the next poll falls due.
    poll: Option<gpui::Task<()>>,
}

impl PullRequestReads {
    /// Whether an asker should ask now, noting it if so.
    fn due(&mut self, asker: Asker, asking: String, settled: bool, back: bool) -> bool {
        let poll = Duration::from_millis(PULL_REQUEST_POLL_MS as u64);
        let ask = match self.asked.get(&asker) {
            None => true,
            Some(held) => held.asking != asking || back || (!settled && held.at.elapsed() >= poll),
        };
        if ask {
            self.asked.insert(asker, Asked { asking, at: Instant::now() });
        }
        ask
    }
}

/// Asks what each panel on screen is owed, called as the window draws.
pub fn sync(root: &mut Root, state: &WorkspaceState, cx: &mut Context<Root>) {
    let reads = &mut root.pull_requests;
    let back = state.focused && !reads.focused;
    reads.focused = state.focused;
    if !state.focused || !state.restored {
        return;
    }
    let mut on_screen = Vec::new();
    let mut commands = Vec::new();
    if let Some(thread) = state.current_thread() {
        on_screen.push(Asker::Thread);
        let workspace_id = thread_workspace_id(state, Some(thread));
        let environment = workspace_id.as_ref().and_then(|id| state.environments.get(id));
        let settled = pull_request_settled(&pull_request_for(state.pull_request.as_ref(), workspace_id.as_deref(), environment));
        if reads.due(Asker::Thread, thread_asking(workspace_id.as_deref(), environment, Some(&thread.id)), settled, back) {
            commands.push(AppCommand::PullRequestRead { task_id: None });
        }
        if is_coordinator(Some(thread)) {
            on_screen.push(Asker::Members);
            let view = member_pull_requests_view(state, Some(thread));
            if reads.due(Asker::Members, view.asking.clone(), view.settled, back) {
                commands.push(AppCommand::PullRequestReadMembers { task_id: None });
            }
        }
    }
    let (_, dock) = front_dock(state);
    if let Some(tab) = dock.thread_tabs.iter().find(|id| **id == dock.tab).filter(|_| dock.open && !state.settings_shown()).and_then(|id| state.thread(id)) {
        on_screen.push(Asker::Tab);
        let workspace_id = thread_workspace_id(state, Some(tab));
        let environment = workspace_id.as_ref().and_then(|id| state.environments.get(id));
        let settled = pull_request_settled(&pull_request_for(state.pull_request.as_ref(), workspace_id.as_deref(), environment));
        if reads.due(Asker::Tab, thread_asking(workspace_id.as_deref(), environment, Some(&tab.id)), settled, back) {
            commands.push(AppCommand::PullRequestRead { task_id: Some(tab.id.clone()) });
        }
    }
    reads.asked.retain(|asker, _| on_screen.contains(asker));
    if reads.poll.is_none() && !reads.asked.is_empty() {
        reads.poll = Some(cx.spawn(async |root, cx| {
            cx.background_executor().timer(Duration::from_millis(PULL_REQUEST_POLL_MS as u64)).await;
            let _ = root.update(cx, |root, cx| {
                root.pull_requests.poll = None;
                cx.notify();
            });
        }));
    }
    if !commands.is_empty() {
        cx.defer(move |cx| {
            for command in commands {
                dispatch(command, cx);
            }
        });
    }
}
