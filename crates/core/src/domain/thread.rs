//! The thread: the canonical conversation aggregate, with every feature's fields joined flat.

use crate::shared::Shared;
use serde::{Deserialize, Serialize};

use super::conversation::{create_conversation_message, ConversationMessage, MessageKind};
use super::coordination::{CoordinationNote, CoordinationReport, CoordinatorMemory, Decision, HeldLead, ThreadBrief, ThreadModelChoice, ThreadProposal};
use super::engine::{AgentEngine, AgentModel};
use super::finding::{AutomationFinding, LastChecked, MAX_FINDINGS, MAX_HANDLED_ISSUES};
use super::pull_request::PullRequestLink;
use super::run::{AgentEffort, Continuation, ExecutionPolicy, RunStatus};
use super::thread_role::ThreadRole;
use super::usage_limit::LimitPause;
use crate::util::{js_len, js_slice_to, random_id, Map};

/// A thread elsewhere last heard working: the paired computer holding it, and what it was called.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HeardWork {
    pub computer: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopAnswer {
    pub at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

/// What Stop all stopped of a thread elsewhere.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteStop {
    pub computer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<StopAnswer>,
}

/// A start on a paired computer Stop all caught on its way.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaughtStart {
    pub computer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChangeSnapshot {
    pub files: Vec<String>,
    #[serde(rename = "capturedAt")]
    pub captured_at: i64,
}

impl Default for ChangeSnapshot {
    fn default() -> Self {
        ChangeSnapshot { files: Vec::new(), captured_at: 0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ContinuationStatus {
    #[default]
    None,
    Available,
    Invalid,
}

/// How the thread's newest settled run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThreadOutcome {
    Finished,
    Failed,
    Stopped,
}

/// How a run that reached its end ended, or None while it has yet to.
pub fn run_outcome(status: RunStatus) -> Option<ThreadOutcome> {
    match status {
        RunStatus::Succeeded => Some(ThreadOutcome::Finished),
        RunStatus::Failed => Some(ThreadOutcome::Failed),
        RunStatus::Cancelled => Some(ThreadOutcome::Stopped),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextUsage {
    pub tokens: i64,
    pub limit: i64,
    /// The id the engine reported on the wire.
    pub model: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistorySummary {
    pub message_count: i64,
    pub attachment_count: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_message_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_audible_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub title_by_user: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<ThreadRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    pub execution_policy: ExecutionPolicy,
    pub engine: AgentEngine,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<AgentModel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<AgentEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_mode: Option<bool>,
    #[serde(default)]
    pub messages: Vec<crate::shared::Shared<ConversationMessage>>,
    /// Present while the persisted conversation has yet to be loaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_summary: Option<HistorySummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_index: Option<f64>,
    #[serde(default)]
    pub ranked_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snoozed_until: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<i64>,

    // Coordination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_lead: Option<HeldLead>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<ThreadBrief>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<CoordinationReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decisions: Option<Vec<Decision>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordination_notes: Option<Vec<CoordinationNote>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_model: Option<ThreadModelChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<CoordinatorMemory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposals: Option<Vec<ThreadProposal>>,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub muted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_heard: Option<Map<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_working: Option<Map<HeardWork>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_stops: Option<Map<RemoteStop>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caught_starts: Option<Map<CaughtStart>>,

    // Pull request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<PullRequestLink>,

    // Findings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub findings: Option<Vec<AutomationFinding>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handled_issues: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_finding_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked: Option<LastChecked>,

    // Runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_usage: Option<ContextUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Map<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<Continuation>,
    #[serde(default)]
    pub continuation_status: ContinuationStatus,
    #[serde(default)]
    pub last_change_snapshot: ChangeSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ThreadOutcome>,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub outcome_unread: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_pause: Option<LimitPause>,
    /// A resource stop blocks every automatic start until the user resumes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_stop: Option<super::session_resources::ResourceStop>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_ended_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_ended_as: Option<ThreadOutcome>,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub run_open: bool,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub inherited_continuation: bool,

    // Worktree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_entered_at: Option<i64>,
}

impl Thread {
    /// A fresh thread with every optional field empty.
    pub fn new(id: impl Into<String>, title: impl Into<String>, engine: AgentEngine, policy: ExecutionPolicy, at: i64) -> Thread {
        Thread {
            id: id.into(),
            title: title.into(),
            title_by_user: false,
            role: None,
            project_id: None,
            execution_policy: policy,
            engine,
            model: None,
            effort: None,
            fast_mode: None,
            messages: Vec::new(),
            history_summary: None,
            sort_index: None,
            ranked_at: at,
            snoozed_until: None,
            created_at: Some(at),
            updated_at: at,
            archived_at: None,
            parent_id: None,
            remote_lead: None,
            brief: None,
            report: None,
            resolved_at: None,
            decisions: None,
            coordination_notes: None,
            thread_model: None,
            memory: None,
            proposals: None,
            muted: false,
            remote_heard: None,
            remote_working: None,
            remote_stops: None,
            caught_starts: None,
            pull_request: None,
            findings: None,
            handled_issues: None,
            last_finding_at: None,
            last_checked: None,
            context_usage: None,
            usage: None,
            continuation: None,
            continuation_status: ContinuationStatus::None,
            last_change_snapshot: ChangeSnapshot::default(),
            outcome: None,
            outcome_unread: false,
            limit_pause: None,
            resource_stop: None,
            run_ended_at: None,
            run_ended_as: None,
            run_open: false,
            inherited_continuation: false,
            worktree_id: None,
            worktree_entered_at: None,
        }
    }

    pub fn is_archived(&self) -> bool {
        self.archived_at.is_some()
    }
}

pub const TITLE_LIMIT: usize = 52;

/// One length for every thread title, whoever wrote it.
pub fn clamp_title(text: &str) -> String {
    let trimmed = text.trim();
    if js_len(trimmed) > TITLE_LIMIT {
        format!("{}…", js_slice_to(trimmed, TITLE_LIMIT - 3))
    } else {
        trimmed.to_string()
    }
}

fn strip_fork_suffix(title: &str) -> &str {
    let pattern = regex::Regex::new(r"\s*\((?:fork|fork \d+)\)$").expect("fork suffix");
    match pattern.find(title) {
        Some(found) => &title[..found.start()],
        None => title,
    }
}

/// What a copy of a thread is called: the copied thread's own name, numbered past every name already taken.
pub fn fork_title<'a>(title: &str, taken: impl IntoIterator<Item = &'a str>) -> String {
    let stripped = strip_fork_suffix(title).trim();
    let base = if stripped.is_empty() { title.trim() } else { stripped };
    let names: std::collections::HashSet<&str> = taken.into_iter().collect();
    let mut number = 1;
    loop {
        let suffix = if number == 1 { " (fork)".to_string() } else { format!(" (fork {number})") };
        let room = TITLE_LIMIT - js_len(&suffix);
        let head = if js_len(base) > room { format!("{}…", js_slice_to(base, room - 1)) } else { base.to_string() };
        let candidate = format!("{head}{suffix}").trim().to_string();
        if !names.contains(candidate.as_str()) {
            return candidate;
        }
        number += 1;
    }
}

pub fn thread_created_at(thread: &Thread) -> i64 {
    thread
        .created_at
        .or_else(|| thread.history_summary.as_ref().and_then(|summary| summary.first_message_at))
        .or_else(|| thread.messages.first().map(|message| message.at))
        .unwrap_or(thread.updated_at)
}

fn last_audible_at(thread: &Thread) -> i64 {
    if let Some(summary) = &thread.history_summary {
        return summary.last_audible_at.unwrap_or(0);
    }
    thread.messages.iter().rev().find(|message| !message.withdrawn).map(|message| message.at).unwrap_or(0)
}

/// When the thread last said anything.
pub fn thread_last_output_at(thread: &Thread) -> i64 {
    thread_created_at(thread).max(last_audible_at(thread)).max(thread.run_ended_at.unwrap_or(0))
}

/// How the thread's newest run ended.
pub fn last_run_outcome(thread: &Thread) -> Option<ThreadOutcome> {
    thread.run_ended_as.or(thread.outcome)
}

// Attention: verdicts and findings.

pub fn unread_findings(thread: &Thread) -> Vec<&AutomationFinding> {
    thread.findings.iter().flatten().filter(|finding| !finding.read).collect()
}

pub fn newest_unread_finding(thread: &Thread) -> Option<&AutomationFinding> {
    unread_findings(thread).last().copied()
}

/// Whether the thread has anything to put to the user.
pub fn wants_attention(thread: &Thread) -> bool {
    thread.outcome.is_some() || thread.findings.as_ref().is_some_and(|findings| !findings.is_empty())
}

/// Whether any of that is still unseen.
pub fn has_unread_attention(thread: &Thread) -> bool {
    newest_unread_finding(thread).is_some() || (thread.outcome.is_some() && thread.outcome_unread)
}

fn finding_keys(thread: &Thread) -> Vec<String> {
    thread.findings.iter().flatten().filter_map(|finding| finding.key.clone()).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssueState {
    Unknown,
    Carried,
    Handled,
}

pub fn issue_state(thread: &Thread, key: Option<&str>) -> IssueState {
    let Some(key) = key.filter(|key| !key.is_empty()) else { return IssueState::Unknown };
    if finding_keys(thread).iter().any(|found| found == key) {
        return IssueState::Carried;
    }
    if thread.handled_issues.iter().flatten().any(|handled| handled == key) {
        IssueState::Handled
    } else {
        IssueState::Unknown
    }
}

pub fn is_news(thread: &Thread, key: Option<&str>) -> bool {
    issue_state(thread, key) == IssueState::Unknown
}

/// Records the finding, oldest dropped once the thread is at its fill, and says it in the thread.
pub fn with_finding(thread: &mut Thread, headline: &str, detail: Option<&str>, key: Option<&str>, at: i64, seen: bool) -> bool {
    if !is_news(thread, key) {
        return false;
    }
    let finding = AutomationFinding {
        id: random_id(),
        headline: headline.into(),
        detail: detail.filter(|detail| !detail.is_empty()).map(String::from),
        key: key.filter(|key| !key.is_empty()).map(String::from),
        at,
        read: seen,
    };
    let findings = thread.findings.get_or_insert_with(Vec::new);
    findings.push(finding);
    if findings.len() > MAX_FINDINGS {
        let excess = findings.len() - MAX_FINDINGS;
        findings.drain(..excess);
    }
    thread.last_finding_at = Some(at);
    thread.messages.push(Shared::new(create_conversation_message(MessageKind::System, headline, detail.map(String::from))));
    thread.updated_at = at;
    true
}

pub fn with_read_findings(thread: &mut Thread) {
    if let Some(findings) = thread.findings.as_mut() {
        for finding in findings.iter_mut() {
            finding.read = true;
        }
    }
}

/// Landing on a thread takes its marks off.
pub fn read_attention(threads: &mut [Shared<Thread>], thread_id: Option<&str>) -> bool {
    let Some(thread_id) = thread_id else { return false };
    let Some(seen) = threads.iter_mut().find(|thread| thread.id == thread_id) else { return false };
    if !seen.outcome_unread && unread_findings(seen).is_empty() {
        return false;
    }
    seen.outcome_unread = false;
    with_read_findings(seen);
    true
}

/// Takes every unseen mark off, leaving verdicts and findings where they rank.
pub fn with_all_read(threads: &mut [Shared<Thread>]) -> bool {
    let mut changed = false;
    for thread in threads.iter_mut() {
        if has_unread_attention(thread) {
            thread.outcome_unread = false;
            with_read_findings(thread);
            changed = true;
        }
    }
    changed
}

/// Filing a thread away retires what its runs found along with the verdict of the last one.
pub fn dismissed(threads: &mut [Shared<Thread>], dismissing: &std::collections::HashSet<String>) -> bool {
    let mut changed = false;
    for thread in threads.iter_mut() {
        if !dismissing.contains(&thread.id) {
            continue;
        }
        if thread.outcome.is_some() {
            thread.outcome = None;
            thread.outcome_unread = false;
            changed = true;
        }
        if thread.findings.is_some() {
            let keys = finding_keys(thread);
            if !keys.is_empty() {
                let mut handled: Vec<String> = thread.handled_issues.iter().flatten().filter(|key| issue_state(thread, Some(key)) != IssueState::Carried).cloned().collect();
                handled.extend(keys);
                if handled.len() > MAX_HANDLED_ISSUES {
                    handled.drain(..handled.len() - MAX_HANDLED_ISSUES);
                }
                thread.handled_issues = Some(handled);
            }
            thread.findings = None;
            changed = true;
        }
    }
    changed
}

/// What a run that has finished looking says about the keys it did not report.
pub fn with_closed_issues(thread: &mut Thread, reported: &[String]) {
    let Some(held) = &thread.handled_issues else { return };
    let still: Vec<String> = held.iter().filter(|key| reported.contains(key)).cloned().collect();
    if still.len() == held.len() {
        return;
    }
    thread.handled_issues = if still.is_empty() { None } else { Some(still) };
}

/// What a row's status mark stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AttentionTone {
    Finished,
    Failed,
    Stopped,
    Approval,
    Unread,
    Working,
    Paused,
    Monitoring,
}

impl From<ThreadOutcome> for AttentionTone {
    fn from(outcome: ThreadOutcome) -> Self {
        match outcome {
            ThreadOutcome::Finished => AttentionTone::Finished,
            ThreadOutcome::Failed => AttentionTone::Failed,
            ThreadOutcome::Stopped => AttentionTone::Stopped,
        }
    }
}

pub const MEMBER_BLOCKED_LABEL: &str = "A thread needs your approval";
pub const MEMBER_UNREAD_LABEL: &str = "A thread has something new";

// Snoozing.

pub const SNOOZE_OPTIONS: [(&str, u32); 5] = [("1 hour", 1), ("4 hours", 4), ("1 day", 24), ("3 days", 72), ("1 week", 168)];

pub fn is_snooze_hours(hours: u32) -> bool {
    SNOOZE_OPTIONS.iter().any(|(_, option)| *option == hours)
}

/// One wake-up for the entire workspace.
pub fn next_snooze_expiry(threads: &[Shared<Thread>]) -> Option<i64> {
    threads.iter().filter(|thread| thread.archived_at.is_none()).filter_map(|thread| thread.snoozed_until).min()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_clamp_with_an_ellipsis() {
        let long = "x".repeat(60);
        let clamped = clamp_title(&long);
        assert_eq!(js_len(&clamped), 50);
        assert!(clamped.ends_with('…'));
    }

    #[test]
    fn fork_titles_number_past_taken_names() {
        assert_eq!(fork_title("Fix CI", []), "Fix CI (fork)");
        assert_eq!(fork_title("Fix CI (fork)", ["Fix CI (fork)"]), "Fix CI (fork 2)");
    }
}
