//! Every order a thread list is drawn in: where the user put each row, newest first, and the
//! activity sections that rank threads by what wants the user.

use crate::shared::Shared;
use std::cmp::Ordering;
use std::collections::HashSet;

use crate::domain::coordination::coordinated_thread_ids;
use crate::domain::project::ThreadDropTarget;
use crate::domain::sidebar::SidebarSections;
use crate::domain::thread::{wants_attention, Thread};
use crate::domain::thread_role::ThreadRole;

/// The activity sidebar's lists, in the order they are drawn.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActivitySections<'a> {
    pub priority: Vec<&'a Thread>,
    pub running: Vec<&'a Thread>,
    pub threads: Vec<&'a Thread>,
    pub snoozed: Vec<&'a Thread>,
}

impl<'a> ActivitySections<'a> {
    pub fn all(&self) -> impl Iterator<Item = &'a Thread> + '_ {
        self.priority.iter().chain(&self.running).chain(&self.threads).chain(&self.snoozed).copied()
    }
}

/// Newest `ranked_at` first, in a new list.
pub fn recent_first<'a>(threads: &[&'a Thread]) -> Vec<&'a Thread> {
    let mut sorted = threads.to_vec();
    sorted.sort_by(|left, right| right.ranked_at.cmp(&left.ranked_at));
    sorted
}

/// The threads last active where a new thread is about to start.
pub fn recent_in<'a>(threads: &'a [Shared<Thread>], project_id: Option<&str>, limit: usize) -> Vec<&'a Thread> {
    let members = coordinated_thread_ids(threads);
    let matching: Vec<&Thread> = threads.iter().map(|thread| &**thread).filter(|thread| thread.project_id.as_deref() == project_id && thread.archived_at.is_none() && !members.contains(&thread.id)).collect();
    recent_first(&matching).into_iter().take(limit).collect()
}

/// Ranks threads by what wants the user rather than by where they live.
pub fn activity_sections<'a>(threads: &[&'a Thread], busy: &HashSet<String>, blocked: &HashSet<String>) -> ActivitySections<'a> {
    let mut priority = Vec::new();
    let mut running = Vec::new();
    let mut settled = Vec::new();
    let mut snoozed = Vec::new();
    for thread in threads.iter().copied() {
        if thread.snoozed_until.is_some() {
            snoozed.push(thread);
        } else if blocked.contains(&thread.id) {
            priority.push(thread);
        } else if thread.role == Some(ThreadRole::Reviewer) {
            settled.push(thread);
        } else if busy.contains(&thread.id) {
            running.push(thread);
        } else if wants_attention(thread) {
            priority.push(thread);
        } else {
            settled.push(thread);
        }
    }
    snoozed.sort_by(|left, right| left.snoozed_until.cmp(&right.snoozed_until));
    ActivitySections { priority: recent_first(&priority), running: order_refs(&running), threads: recent_first(&settled), snoozed }
}

/// `order_threads` over borrowed rows.
pub fn order_refs<'a>(threads: &[&'a Thread]) -> Vec<&'a Thread> {
    let mut sorted = threads.to_vec();
    sorted.sort_by(|left, right| compare_threads(left, right));
    sorted
}

fn compare_threads(left: &Thread, right: &Thread) -> Ordering {
    match (left.sort_index, right.sort_index) {
        (Some(a), Some(b)) => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => right.ranked_at.cmp(&left.ranked_at),
    }
}

/// Sidebar order: `sort_index` wins so rows never move on their own.
pub fn order_threads(threads: &[Shared<Thread>]) -> Vec<Shared<Thread>> {
    let mut sorted = threads.to_vec();
    sorted.sort_by(|left, right| compare_threads(left, right));
    sorted
}

/// Freezes the order threads loaded in, so a first launch after upgrading keeps the list the user last saw.
pub fn backfill_sort_index(threads: &mut [Shared<Thread>]) {
    if threads.iter().all(|thread| thread.sort_index.is_some()) {
        return;
    }
    let positions: std::collections::HashMap<String, usize> = order_threads(threads).iter().enumerate().map(|(index, thread)| (thread.id.clone(), index)).collect();
    for thread in threads.iter_mut() {
        if thread.sort_index.is_none() {
            thread.sort_index = Some(positions[&thread.id] as f64);
        }
    }
}

pub fn next_sort_index(threads: &[Shared<Thread>]) -> f64 {
    let lowest = threads.iter().map(|thread| thread.sort_index.unwrap_or(0.0)).fold(0.0, f64::min);
    lowest - 1.0
}

/// Why a thread can reorder within its folder but not be carried to another, or None when it can.
pub fn thread_move_block(thread: &Thread, host_name: Option<&str>, coordinated: bool) -> Option<String> {
    if let Some(host) = host_name {
        return Some(format!("Stays in its folder on {host}"));
    }
    if thread.worktree_id.is_some() {
        return Some("Stays in its worktree's folder".into());
    }
    if coordinated {
        return Some("Stays with its coordinator".into());
    }
    None
}

/// Moves one thread into `target` and renumbers the visible list. Returns whether anything changed.
pub fn move_thread(threads: &mut [Shared<Thread>], thread_id: &str, target: &ThreadDropTarget) -> bool {
    let members = coordinated_thread_ids(threads);
    let Some(moving) = threads.iter().find(|thread| thread.id == thread_id && thread.archived_at.is_none()).cloned() else { return false };
    let project_id = target.project_id.clone();
    if project_id != moving.project_id && thread_move_block(&moving, None, members.contains(&moving.id)).is_some() {
        return false;
    }
    let visible: Vec<Shared<Thread>> = order_threads(&threads.iter().filter(|thread| thread.archived_at.is_none() && !members.contains(&thread.id)).cloned().collect::<Vec<_>>());
    if !visible.iter().any(|thread| thread.id == moving.id) {
        return false;
    }
    let mut rest: Vec<Shared<Thread>> = visible.into_iter().filter(|thread| thread.id != moving.id).collect();
    let group: Vec<usize> = rest.iter().enumerate().filter(|(_, thread)| thread.project_id == project_id).map(|(index, _)| index).collect();
    let index = match group.get(target.index.max(0) as usize) {
        Some(anchor) => *anchor,
        None => group.last().map(|last| last + 1).unwrap_or(0),
    };
    let mut placed = moving.clone();
    placed.project_id = project_id;
    rest.insert(index, placed);
    let mut changed = false;
    for (position, next) in rest.into_iter().enumerate() {
        if let Some(thread) = threads.iter_mut().find(|thread| thread.id == next.id) {
            let sort_index = Some(position as f64);
            if thread.sort_index != sort_index || thread.project_id != next.project_id {
                thread.sort_index = sort_index;
                thread.project_id = next.project_id.clone();
                changed = true;
            }
        }
    }
    changed
}

/// The threads a digit reaches: the first rows the sidebar draws, top to bottom, skipping folded sections.
pub fn slot_thread_ids(sections: &SidebarSections, activity: &ActivitySections<'_>, limit: usize) -> Vec<String> {
    let mut ids = Vec::new();
    let lists = [(sections.priority, &activity.priority), (sections.running, &activity.running), (sections.threads, &activity.threads), (sections.snoozed, &activity.snoozed)];
    for (open, threads) in lists {
        if !open {
            continue;
        }
        for thread in threads {
            if ids.len() == limit {
                return ids;
            }
            ids.push(thread.id.clone());
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::engine::AgentEngine;
    use crate::domain::run::ExecutionPolicy;
    use crate::domain::thread::ThreadOutcome;

    fn thread(id: &str, ranked: i64) -> Thread {
        let mut thread = Thread::new(id, id, AgentEngine::Claude, ExecutionPolicy::Confirm, ranked);
        thread.ranked_at = ranked;
        thread
    }

    #[test]
    fn sections_rank_by_what_wants_the_user() {
        let mut finished = thread("finished", 1);
        finished.outcome = Some(ThreadOutcome::Finished);
        let mut reviewer = thread("reviewer", 2);
        reviewer.role = Some(ThreadRole::Reviewer);
        reviewer.outcome = Some(ThreadOutcome::Finished);
        let mut snoozed = thread("snoozed", 3);
        snoozed.snoozed_until = Some(10);
        let busy: HashSet<String> = ["working".to_string()].into();
        let all = [finished, reviewer, snoozed, thread("working", 4), thread("idle", 5)];
        let refs: Vec<&Thread> = all.iter().collect();
        let sections = activity_sections(&refs, &busy, &HashSet::new());
        assert_eq!(sections.priority.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["finished"]);
        assert_eq!(sections.running.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["working"]);
        assert_eq!(sections.threads.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["idle", "reviewer"]);
        assert_eq!(sections.snoozed.len(), 1);
    }

    #[test]
    fn moving_renumbers_and_reassigns_project() {
        let mut threads: Vec<Shared<Thread>> = vec![thread("a", 3).into(), thread("b", 2).into(), thread("c", 1).into()];
        assert!(move_thread(&mut threads, "c", &ThreadDropTarget { project_id: Some("p".into()), index: 0 }));
        let moved = threads.iter().find(|t| t.id == "c").unwrap();
        assert_eq!(moved.project_id.as_deref(), Some("p"));
        let order: Vec<String> = order_threads(&threads).iter().map(|t| t.id.clone()).collect();
        assert_eq!(order, ["c", "a", "b"]);
    }
}
