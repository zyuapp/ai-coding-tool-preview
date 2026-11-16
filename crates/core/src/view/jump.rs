//! The jump panel's list: every thread it can offer, every settings page and control, and the ones a name matches.

use std::collections::HashSet;

use crate::domain::project::project_name;
use crate::domain::pull_request::{links_pull_request, pull_request_query};
use crate::domain::settings_catalog::{rank_settings_jumps, rank_thread_jumps, SettingsJumpOption, ThreadJumpOption, SETTINGS_JUMP_ROWS, THREAD_JUMP_ROWS};
use crate::domain::thread::Thread;
use crate::state::WorkspaceState;

#[derive(Clone, Debug, PartialEq)]
pub enum JumpRow {
    Thread { option: ThreadJumpOption, running: bool, named_pull_request: Option<i64> },
    Setting(SettingsJumpOption),
}

#[derive(Clone, Debug, PartialEq)]
pub struct JumpView {
    pub query: String,
    pub index: i64,
    pub options: Vec<JumpRow>,
}

fn thread_jump_options(state: &WorkspaceState) -> Vec<ThreadJumpOption> {
    let forked = state.side_chat_ids();
    let mut threads: Vec<&Thread> = state.threads.iter().map(|thread| &**thread).filter(|thread| thread.archived_at.is_none() && !forked.contains(&thread.id)).collect();
    threads.sort_by(|left, right| right.ranked_at.cmp(&left.ranked_at));
    threads
        .into_iter()
        .map(|thread| ThreadJumpOption {
            id: thread.id.clone(),
            title: thread.title.clone(),
            project: thread.project_id.as_deref().and_then(|id| state.project(id)).map(project_name),
            engine: thread.engine,
            pull_request: thread.pull_request.clone(),
            ranked_at: thread.ranked_at,
        })
        .collect()
}

/// The jump panel as it is drawn: the threads its name matches, and which row is picked.
pub fn jump_view(state: &WorkspaceState, busy: &HashSet<String>) -> Option<JumpView> {
    let jump = state.jump.as_ref()?;
    let named = pull_request_query(&jump.query);
    let mut options: Vec<JumpRow> = rank_thread_jumps(&thread_jump_options(state), &jump.query, THREAD_JUMP_ROWS)
        .into_iter()
        .map(|option| {
            let named_pull_request = match (&named, &option.pull_request) {
                (Some(query), Some(link)) if links_pull_request(link, query) => Some(link.number),
                _ => None,
            };
            JumpRow::Thread { running: busy.contains(&option.id), named_pull_request, option }
        })
        .collect();
    options.extend(rank_settings_jumps(&jump.query, SETTINGS_JUMP_ROWS).into_iter().map(JumpRow::Setting));
    let index = if options.is_empty() { 0 } else { jump.index.min(options.len() as i64 - 1) };
    Some(JumpView { query: jump.query.clone(), index, options })
}
