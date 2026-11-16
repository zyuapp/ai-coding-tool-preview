//! Conversations drawn in dock tabs, and the location menu with the checkouts a thread or draft can
//! work in.

use std::collections::HashSet;

use crate::commands::AppCommand;
use crate::domain::conversation::{Annotation, AttachedFile, PastedText, StagedImage};
use crate::domain::automation::AutomationView;
use crate::domain::pull_request::PullRequestAnswer;
use crate::domain::question::PendingQuestion;
use crate::domain::run::RetryNotice;
use crate::domain::thread::{Thread, ThreadOutcome};
use crate::domain::worktree::{worktree_name, WorktreeDestination};
use crate::events::ChangedFilesResult;
use crate::state::{queued_view, AnnotationReveal, ApprovalView, QueuedMessageView, ReadingPoint, RunPhase, SideChat, StreamingTail, ThreadRunStatus, WorkspaceState, WorktreeMenuSearch};
use crate::view::pull_request::pull_request_for;
use crate::workspace::activity::{run_status, thread_activity, thread_status, ThreadStatus};
use crate::workspace::coordinated_status::{coordinated_thread_status, CoordinatedThreadStatus};
use crate::workspace::location::{location_of, project_for, thread_workspace_id, worktree_for, ThreadLocation};

/// A conversation drawn in a dock tab: its thread, its composer's draft, and its run as it stands.
#[derive(Clone, Debug, PartialEq)]
pub struct DockConversationView<'a> {
    pub id: String,
    pub title: String,
    pub thread: &'a Thread,
    pub model: Option<String>,
    pub prompt: String,
    pub annotations: &'a [Annotation],
    pub annotation_reveal: Option<&'a AnnotationReveal>,
    pub pastes: &'a [PastedText],
    pub images: &'a [StagedImage],
    pub files: &'a [AttachedFile],
    pub running: bool,
    pub compacting: bool,
    pub retrying: Option<&'a RetryNotice>,
    pub status: ThreadRunStatus,
    pub streaming_tail: Option<&'a StreamingTail>,
    pub queued_messages: Vec<QueuedMessageView>,
    pub approval: Option<&'a ApprovalView>,
    pub question: Option<&'a PendingQuestion>,
    pub reading_point: Option<&'a ReadingPoint>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SideChatView<'a> {
    pub chat: &'a SideChat,
    pub conversation: DockConversationView<'a>,
}

/// A thread under the coordinator in front, opened as a tab in its dock.
#[derive(Clone, Debug, PartialEq)]
pub struct ThreadTabView<'a> {
    pub conversation: DockConversationView<'a>,
    pub standing: CoordinatedThreadStatus,
    pub summary: Option<String>,
    pub folder: String,
    pub workspace_id: Option<String>,
    pub environment: Option<&'a ChangedFilesResult>,
    pub pull_request: PullRequestAnswer,
    pub automation: Option<&'a AutomationView>,
}

fn drafts<'a, T>(map: &'a crate::util::Map<Vec<T>>, key: &str) -> &'a [T] {
    map.get(key).map(Vec::as_slice).unwrap_or(&[])
}

/// One thread drawn in a dock tab, projected from the thread and its active run.
pub fn dock_conversation_view<'a>(state: &'a WorkspaceState, thread: &'a Thread) -> DockConversationView<'a> {
    let active = state.active_runs.get(&thread.id);
    let approval = active.filter(|run| run.phase == RunPhase::AwaitingApproval).and_then(|run| state.approvals.get(&run.run_id));
    DockConversationView {
        id: thread.id.clone(),
        title: thread.title.clone(),
        thread,
        model: crate::domain::engine::thread_model(&state.catalog(), thread.engine, thread.model.as_deref()),
        prompt: state.prompts.get(&thread.id).cloned().unwrap_or_default(),
        annotations: drafts(&state.annotations, &thread.id),
        annotation_reveal: state.annotation_reveal.as_ref().filter(|reveal| reveal.task_id == thread.id),
        pastes: drafts(&state.pastes, &thread.id),
        images: drafts(&state.images, &thread.id),
        files: drafts(&state.files, &thread.id),
        running: active.is_some(),
        question: active.and_then(|run| run.questions.as_ref()).and_then(|questions| questions.first()),
        compacting: active.is_some_and(|run| run.phase == RunPhase::Compacting),
        retrying: active.and_then(|run| match &run.phase {
            RunPhase::Retrying { retry } => Some(retry),
            _ => None,
        }),
        status: run_status(state, Some(&thread.id)),
        streaming_tail: state.streaming_tails.get(&thread.id),
        queued_messages: queued_view(state, &thread.id).unwrap_or_default(),
        reading_point: state.reading_points.get(&thread.id).and_then(Option::as_ref),
        approval,
    }
}

/// One open side chat projected from its thread and its run.
pub fn side_chat_view<'a>(state: &'a WorkspaceState, chat: &'a SideChat) -> Option<SideChatView<'a>> {
    let thread = state.thread(&chat.id)?;
    Some(SideChatView { chat, conversation: dock_conversation_view(state, thread) })
}

/// One thread's tab in its coordinator's dock, with where its work stands.
pub fn thread_tab_view<'a>(state: &'a WorkspaceState, task_id: &str, busy: &HashSet<String>, blocked: &HashSet<String>) -> Option<ThreadTabView<'a>> {
    let thread = state.thread(task_id)?;
    let standing = coordinated_thread_status(thread, busy, blocked);
    let workspace_id = thread_workspace_id(state, Some(thread));
    let environment = workspace_id.as_ref().and_then(|id| state.environments.get(id));
    Some(ThreadTabView {
        conversation: dock_conversation_view(state, thread),
        standing: standing.status,
        summary: standing.summary,
        folder: project_for(state, Some(thread)).map(|project| project.root.clone()).unwrap_or_default(),
        pull_request: pull_request_for(state.pull_request.as_ref(), workspace_id.as_deref(), environment),
        workspace_id,
        environment,
        automation: state.automations.iter().find(|automation| automation.automation.task_id == thread.id),
    })
}

pub const WORKTREE_MENU: &str = "session:location";

/// One row of the location menu.
#[derive(Clone, Debug, PartialEq)]
pub struct WorktreeMenuChoice {
    pub id: String,
    pub title: String,
    pub detail: String,
    pub current: bool,
    pub disabled: bool,
    pub command: AppCommand,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorktreeMenuView {
    pub thread_id: String,
    pub project_id: String,
    pub location: ThreadLocation,
    pub worktree_id: Option<String>,
    pub count: usize,
    pub threads: Vec<WorktreeMenuChoice>,
    pub destinations: Vec<WorktreeMenuChoice>,
    pub search: WorktreeMenuSearch,
    pub can_move: bool,
    pub delete_root: Option<String>,
    pub busy_count: usize,
    pub can_delete: bool,
    pub loading: bool,
    pub error: Option<String>,
}

fn status_label(state: &WorkspaceState, thread: &Thread) -> &'static str {
    match thread_status(&thread_activity(state), &thread.id) {
        ThreadStatus::Blocked => "Needs input",
        ThreadStatus::Paused => "Paused",
        ThreadStatus::Working => "Working",
        ThreadStatus::Idle => match thread.outcome {
            Some(ThreadOutcome::Failed) => "Failed",
            Some(ThreadOutcome::Stopped) => "Stopped",
            Some(ThreadOutcome::Finished) => "Done",
            None => "Idle",
        },
    }
}

/// The location menu: the threads in the same checkout, and the checkouts this one can move to.
/// `open_menu` and `search` are the window's, which a paired computer's thread is drawn in.
pub fn worktree_menu_view(state: &WorkspaceState, current: Option<&Thread>, visible: &[&Thread], open_menu: Option<&str>, search: &WorktreeMenuSearch) -> Option<WorktreeMenuView> {
    let current = current?;
    let project = project_for(state, Some(current))?;
    let busy = thread_activity(state).working;
    let worktree = worktree_for(state, Some(current));
    let members: Vec<&Thread> = match worktree {
        Some(worktree) => visible.iter().copied().filter(|thread| thread.worktree_id.as_deref() == Some(worktree.id.as_str())).collect(),
        None => Vec::new(),
    };
    let open = open_menu == Some(WORKTREE_MENU);
    let mut threads = Vec::new();
    if open {
        let query = search.threads.trim().to_lowercase();
        let mut matching: Vec<&Thread> = members.iter().copied().filter(|thread| thread.title.to_lowercase().contains(&query)).collect();
        matching.sort_by(|a, b| (b.id == current.id).cmp(&(a.id == current.id)).then(b.ranked_at.cmp(&a.ranked_at)));
        for thread in matching {
            let is_current = thread.id == current.id;
            threads.push(WorktreeMenuChoice {
                id: thread.id.clone(),
                title: thread.title.clone(),
                detail: if is_current { "Current thread".into() } else { status_label(state, thread).into() },
                current: is_current,
                disabled: false,
                command: AppCommand::TaskSelect { task_id: thread.id.clone() },
            });
        }
    }
    let releasing: HashSet<&str> = state.threads.iter().filter(|thread| state.releasing_worktrees.contains(&thread.id)).filter_map(|thread| thread.worktree_id.as_deref()).collect();
    let mut destinations = Vec::new();
    if open {
        let query = search.destinations.trim().to_lowercase();
        let mut checkouts: Vec<&crate::domain::worktree::Worktree> = state.worktrees.iter().filter(|item| item.project_id == project.id && Some(&item.id) != current.worktree_id.as_ref()).collect();
        checkouts.sort_by_key(|item| std::cmp::Reverse(item.last_used_at));
        for checkout in checkouts {
            let record = state.managed_worktrees.as_ref().and_then(|managed| managed.iter().find(|item| item.root == checkout.root));
            if (state.managed_worktrees.is_some() && record.is_none()) || record.is_some_and(|record| record.repository.is_none()) {
                continue;
            }
            let name = worktree_name(checkout);
            let mut branch = match record {
                Some(record) => record.branch.clone().unwrap_or_else(|| "Detached".into()),
                None => "Branch not loaded".into(),
            };
            if let Some(ChangedFilesResult::Available { branch: known, .. }) = state.environments.get(&checkout.workspace_id) {
                branch = known.clone().unwrap_or_else(|| "Detached".into());
            }
            let folder = checkout.root.split('/').next_back().unwrap_or("");
            if !format!("{name} {branch} {folder} {}", checkout.id).to_lowercase().contains(&query) {
                continue;
            }
            let removing = state.deleting_worktrees.contains(&checkout.root) || releasing.contains(checkout.id.as_str());
            if removing {
                branch = "Deleting…".into();
            }
            destinations.push(WorktreeMenuChoice {
                id: checkout.id.clone(),
                title: name,
                detail: branch,
                current: false,
                disabled: state.worktree_management_loading || removing || busy.contains(&current.id),
                command: AppCommand::TaskMoveWorktree { task_id: Some(current.id.clone()), destination: WorktreeDestination::Worktree { id: checkout.id.clone() }, after_run: false, sender: None, then: None },
            });
        }
    }
    let busy_count = worktree.map(|worktree| state.threads.iter().filter(|thread| thread.worktree_id.as_deref() == Some(worktree.id.as_str()) && busy.contains(&thread.id)).count()).unwrap_or(0);
    Some(WorktreeMenuView {
        thread_id: current.id.clone(),
        project_id: project.id.clone(),
        location: location_of(state, Some(current)),
        worktree_id: worktree.map(|worktree| worktree.id.clone()),
        count: members.len(),
        threads,
        destinations,
        search: search.clone(),
        can_move: !busy.contains(&current.id) && project.workspace_id.is_some(),
        delete_root: worktree.map(|worktree| worktree.root.clone()),
        busy_count,
        can_delete: worktree.is_some_and(|worktree| busy_count == 0 && !state.deleting_worktrees.contains(&worktree.root) && !releasing.contains(worktree.id.as_str())),
        loading: state.worktree_management_loading,
        error: state.worktree_management_error.clone(),
    })
}

/// A checkout a draft can start in.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorktreeChoice {
    pub id: String,
    pub name: String,
    pub branch: Option<String>,
}

/// The project's checkouts a draft can start in, most recently used first, without the one it is in.
pub fn worktree_choices(state: &WorkspaceState, project_id: &str, except: Option<&str>) -> Vec<WorktreeChoice> {
    let mut worktrees: Vec<&crate::domain::worktree::Worktree> = state
        .worktrees
        .iter()
        .filter(|worktree| worktree.project_id == project_id && Some(worktree.id.as_str()) != except && !state.deleting_worktrees.contains(&worktree.root))
        .filter(|worktree| state.managed_worktrees.as_ref().is_none_or(|managed| managed.iter().any(|item| item.root == worktree.root && item.repository.is_some())))
        .collect();
    worktrees.sort_by_key(|worktree| std::cmp::Reverse(worktree.last_used_at));
    worktrees
        .into_iter()
        .map(|worktree| {
            let branch = match state.environments.get(&worktree.workspace_id) {
                Some(ChangedFilesResult::Available { branch, .. }) => branch.clone(),
                _ => state.managed_worktrees.as_ref().and_then(|managed| managed.iter().find(|item| item.root == worktree.root)).and_then(|item| item.branch.clone()),
            };
            WorktreeChoice { id: worktree.id.clone(), name: worktree_name(worktree), branch }
        })
        .collect()
}
