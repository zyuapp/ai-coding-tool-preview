//! What coordination says: the context a coordinator's run reads, the message that wakes it, and the
//! headlines the desktop shows.

use crate::domain::conversation::MessageKind;
use crate::domain::coordination::{open_decisions, CoordinationNote, MemoryNote, OverviewSection, ThreadProposal};
use crate::domain::message_origin::origin_label;
use crate::domain::thread::{thread_last_output_at, Thread};
use crate::state::WorkspaceState;
use crate::util::{js_len, js_slice_to};
use crate::workspace::activity::{thread_activity, thread_status, ThreadStatus};
use crate::workspace::coordinated_status::{coordinated_thread_status, group_of, CoordinatedThreadStatus};
use crate::workspace::coordination::{coordination_members, unheard_work};

fn clipped(text: &str, max: usize) -> String {
    if js_len(text) > max {
        format!("{}…", js_slice_to(text, max - 1))
    } else {
        text.to_string()
    }
}

fn note_line(note: &MemoryNote) -> String {
    format!("- [{}] {}", note.id, note.text)
}

fn roster_status(status: ThreadStatus) -> &'static str {
    match status {
        ThreadStatus::Blocked => "waiting for the user's approval",
        ThreadStatus::Paused => "paused until its usage limit lifts",
        ThreadStatus::Working => "working",
        ThreadStatus::Idle => "idle",
    }
}

/// How many done threads the coordinator reads one by one; the rest are only counted.
const ROSTER_DONE: usize = 10;

fn elsewhere(name: &str, reached: bool, status: ThreadStatus) -> String {
    if reached {
        format!(" on {name} · {}", roster_status(status))
    } else {
        format!(" on {name}, which cannot be reached now · {} when last heard", roster_status(status))
    }
}

fn uncounted(finished: usize, resolved: usize) -> Option<String> {
    let total = finished + resolved;
    if total == 0 {
        return None;
    }
    let mut parts = Vec::new();
    if finished > 0 {
        parts.push(format!("{finished} more finished"));
    }
    if resolved > 0 {
        parts.push(format!("{resolved} resolved"));
    }
    Some(format!("{} {}", parts.join(" and "), if total == 1 { "thread; read_thread reaches it." } else { "threads; read_thread reaches them." }))
}

/// One line per thread under the coordinator that needs the user, is working, or is ready for review,
/// and per thread among the most recently done. Every other thread is counted.
fn roster(state: &WorkspaceState, lead: &Thread) -> Vec<String> {
    let members = coordination_members(state, &lead.id);
    let here = thread_activity(state);
    struct Standing<'a> {
        thread: &'a Thread,
        status: CoordinatedThreadStatus,
        summary: Option<String>,
        computer: Option<(String, bool)>,
        busy: bool,
        stands: ThreadStatus,
    }
    let standing: Vec<Standing> = members
        .iter()
        .map(|member| {
            let there = member.holder().map(thread_activity);
            let activity = there.as_ref().unwrap_or(&here);
            let found = coordinated_thread_status(member.thread, &activity.working, &activity.blocked);
            Standing {
                thread: member.thread,
                status: found.status,
                summary: found.summary,
                computer: member.computer.map(|computer| (computer.link.name.clone(), computer.link.status == crate::domain::platform::ComputerStatus::Connected)),
                busy: activity.working.contains(&member.thread.id),
                stands: thread_status(activity, &member.thread.id),
            }
        })
        .collect();
    let mut done: Vec<&Standing> = standing.iter().filter(|item| group_of(item.status) == OverviewSection::Done).collect();
    done.sort_by_key(|item| std::cmp::Reverse(thread_last_output_at(item.thread)));
    let recent: Vec<&str> = done.iter().take(ROSTER_DONE).map(|item| item.thread.id.as_str()).collect();
    let mut lines: Vec<String> = Vec::new();
    for item in &standing {
        let group = group_of(item.status);
        if group == OverviewSection::Resolved || (group == OverviewSection::Done && !recent.contains(&item.thread.id.as_str())) {
            continue;
        }
        let place = match &item.computer {
            Some((name, reached)) => elsewhere(name, *reached, item.stands),
            None => format!(" · {}", roster_status(item.stands)),
        };
        let mut parts = vec![format!("- \"{}\" [{}]{place}", item.thread.title, item.thread.id)];
        let report = item.thread.report.as_ref().filter(|report| item.busy || report.state != crate::domain::coordination::CoordinationState::Working);
        if let Some(report) = report {
            let state = serde_json::to_value(report.state).ok().and_then(|value| value.as_str().map(String::from)).unwrap_or_default();
            parts.push(format!("reported {state}: {}", report.summary));
        } else if item.status == CoordinatedThreadStatus::Done {
            parts.push(format!("ended its turn without reporting{}", item.summary.as_ref().map(|summary| format!("; it last said: {summary}")).unwrap_or_default()));
        }
        if let (CoordinatedThreadStatus::NeedsWork, Some(summary)) = (item.status, &item.summary) {
            parts.push(format!("its pull request needs work: {summary}"));
        }
        if let (CoordinatedThreadStatus::Review, Some(summary)) = (item.status, &item.summary) {
            parts.push(format!("ready for review: {summary}"));
        }
        for decision in open_decisions(item.thread) {
            parts.push(format!("waiting on the user to decide: {}", decision.question));
        }
        lines.push(parts.join(" · "));
    }
    for (thread_id, title, computer) in unheard_work(state, lead) {
        let name = computer.map(|computer| computer.link.name.clone()).unwrap_or_else(|| "a paired computer".into());
        lines.push(format!("- \"{title}\" [{thread_id}]{}", elsewhere(&name, false, ThreadStatus::Working)));
    }
    let resolved = standing.iter().filter(|item| item.status == CoordinatedThreadStatus::Resolved).count();
    if let Some(rest) = uncounted(done.len() - recent.len(), resolved) {
        lines.push(rest);
    }
    lines
}

/// What a coordinator reads of its memory: all of it in a session that has not read what it holds now.
fn memory_context(lead: &Thread, fresh: bool) -> Vec<String> {
    let Some(memory) = &lead.memory else { return Vec::new() };
    let resumed = !fresh && lead.continuation.is_some();
    if resumed && memory.heard == Some(memory.revision) {
        return Vec::new();
    }
    let changed = resumed && memory.heard.is_some();
    let purpose = memory.purpose.clone().or_else(|| (changed && memory.purpose_cleared.unwrap_or(0) > memory.heard.unwrap_or(0)).then(|| "none".to_string()));
    let notes: Vec<String> = if !memory.notes.is_empty() {
        memory.notes.iter().map(note_line).collect()
    } else if changed {
        vec!["- none".into()]
    } else {
        Vec::new()
    };
    let mut lines = Vec::new();
    if let Some(purpose) = purpose {
        lines.push(format!("Your purpose: {purpose}"));
    }
    if !notes.is_empty() {
        lines.push(if changed { "Your memory (the user changed it since you last read it):".into() } else { "Your memory:".into() });
        lines.extend(notes);
    }
    lines
}

const RECENT_MESSAGES: usize = 12;
const RECENT_MESSAGE: usize = 800;
const RECENT_TOTAL: usize = 10_000;

/// The words a coordinator and whoever wrote to it last exchanged, oldest first, each clipped.
fn recent_conversation(lead: &Thread) -> Vec<String> {
    if lead.history_summary.is_some() {
        return Vec::new();
    }
    let mut lines: Vec<String> = Vec::new();
    for message in lead.messages.iter().rev() {
        if lines.len() >= RECENT_MESSAGES {
            break;
        }
        if !matches!(message.kind, MessageKind::User | MessageKind::Assistant) || message.withdrawn || message.artifact || message.text.trim().is_empty() {
            continue;
        }
        let sender = match (&message.kind, &message.origin) {
            (MessageKind::Assistant, _) => "You".to_string(),
            (_, Some(origin)) => origin_label(origin),
            _ => "The user".to_string(),
        };
        lines.insert(0, format!("{sender}: {}", clipped(message.text.trim(), RECENT_MESSAGE)));
    }
    let mut total: usize = lines.iter().map(|line| js_len(line) + 1).sum();
    while !lines.is_empty() && total > RECENT_TOTAL {
        total -= js_len(&lines.remove(0)) + 1;
    }
    lines
}

/// What a coordinator's run is told beyond its prompt.
pub fn coordination_context(state: &WorkspaceState, lead: &Thread, carried: &[String], fresh: bool) -> String {
    let notes: Vec<&CoordinationNote> = lead.coordination_notes.iter().flatten().filter(|note| !carried.contains(&note.id)).collect();
    let lines = roster(state, lead);
    let own: Vec<String> = open_decisions(lead).iter().map(|decision| format!("- {}", decision.question)).collect();
    let suggested: Vec<String> = lead.proposals.iter().flatten().map(|proposal| format!("- \"{}\" [{}]", proposal.title, proposal.id)).collect();
    let recent = if fresh { recent_conversation(lead) } else { Vec::new() };
    let mut parts: Vec<String> = Vec::new();
    if fresh {
        parts.push(format!("This coordinator continues in a fresh session to keep its context small; read_thread with its ID [{}] reaches its full history.", lead.id));
    }
    parts.extend(memory_context(lead, fresh));
    if !notes.is_empty() {
        parts.push("Updates from your threads you have not seen yet:".into());
        parts.extend(notes.iter().map(|note| format!("- {}", note.text)));
    }
    if !lines.is_empty() {
        parts.push("Your threads right now:".into());
        parts.extend(lines);
    }
    if !suggested.is_empty() {
        parts.push("Suggested threads waiting on the user:".into());
        parts.extend(suggested);
    }
    if !own.is_empty() {
        parts.push("Decisions you raised that the user has yet to answer:".into());
        parts.extend(own);
    }
    if !recent.is_empty() {
        parts.push("The last messages of your conversation, oldest first:".into());
        parts.extend(recent);
    }
    parts.join("\n")
}

/// What a coordinator's run is sent: its prompt with its context after it, or in a fresh session the seed first.
pub fn coordinator_prompt(state: &WorkspaceState, lead: &Thread, prompt: &str, carried: &[String], fresh: bool) -> String {
    let context = coordination_context(state, lead, carried, fresh);
    if context.is_empty() {
        return prompt.to_string();
    }
    if fresh {
        format!("{context}\n\n---\n{prompt}")
    } else {
        format!("{prompt}\n\n---\n{context}")
    }
}

/// The message that wakes a coordinator with its threads' news, as the user sees it and as the agent reads it.
pub fn coordination_update(notes: &[CoordinationNote], still_working: usize, waiting: Option<&str>) -> (String, String) {
    let text = notes.iter().map(|note| note.text.as_str()).collect::<Vec<_>>().join("\n");
    let reply = if still_working > 0 {
        format!(
            "{} still working, so tell the user only what cannot wait for {}, in a line or two.",
            if still_working == 1 { "One other thread is".to_string() } else { format!("{still_working} other threads are") },
            if still_working == 1 { "it" } else { "them" }
        )
    } else {
        "None of your threads is working now, so give the user one answer that brings together everything they delivered, what is left, and anything waiting on them.".into()
    };
    let owed = format!(" {}.", waiting.unwrap_or("Nothing is waiting on the user"));
    let prompt = format!("{text}\n\nThese are updates from threads working under you, not a message from the user. {reply}{owed} Say nothing about progress. Start a thread for any follow-up work.");
    (text, prompt)
}

/// A coordinator's held news quieted, so a coordinator the user stopped wakes nothing by itself.
pub fn with_news_quieted(state: &mut WorkspaceState, lead_id: &str) {
    let Some(lead) = state.thread_mut(lead_id) else { return };
    for note in lead.coordination_notes.iter_mut().flatten() {
        note.quiet = true;
    }
}

/// What a thread asking to start one on another computer says on the desktop.
pub fn request_headline(proposal: &ThreadProposal, computer: &str) -> String {
    format!("Needs you: Run \"{}\" on {computer}?", proposal.title)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::coordination::CoordinatorMemory;
    use crate::domain::engine::AgentEngine;
    use crate::domain::run::ExecutionPolicy;

    #[test]
    fn a_resumed_session_hears_only_memory_it_has_not_read() {
        let mut lead = Thread::new("lead", "Lead", AgentEngine::Claude, ExecutionPolicy::Confirm, 1);
        lead.memory = Some(CoordinatorMemory { purpose: None, purpose_cleared: Some(3), notes: Vec::new(), revision: 3, heard: Some(2) });
        lead.continuation = Some(crate::domain::run::Continuation { provider: "claude".into(), value: "s".into() });
        assert_eq!(memory_context(&lead, false), vec!["Your purpose: none", "Your memory (the user changed it since you last read it):", "- none"]);
        lead.memory.as_mut().unwrap().heard = Some(3);
        assert!(memory_context(&lead, false).is_empty());
        assert!(memory_context(&lead, true).is_empty());
    }

    #[test]
    fn the_wake_says_how_many_threads_still_work() {
        let (_, prompt) = coordination_update(&[], 2, Some("A decision is waiting on the user"));
        assert!(prompt.contains("2 other threads are still working, so tell the user only what cannot wait for them"));
        assert!(prompt.contains(" A decision is waiting on the user."));
    }
}
