//! A coordinator and the threads it has handed work to.

use crate::shared::Shared;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use super::engine::{
    default_effort_for, default_model_in, effort_for_model, engine_has_model, model_engine, model_takes_effort, thread_model, AgentEngine, AgentModel, ModelCatalog,
};
use super::message_origin::RemoteSender;
use super::run::AgentEffort;
use super::thread::{last_run_outcome, wants_attention, Thread, ThreadOutcome};
use super::thread_role::ThreadRole;
use crate::util::{js_len, Map};

/// A coordinator's run delegates and changes no files; a thread under one reports back to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CoordinationRole {
    Coordinator,
    Member,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Delivery {
    PullRequest,
    Commit,
    Report,
}

impl Delivery {
    pub fn label(self) -> &'static str {
        match self {
            Delivery::PullRequest => "Pull request",
            Delivery::Commit => "Commits",
            Delivery::Report => "Report",
        }
    }
}

/// What a coordinator hands a thread with its first message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadBrief {
    pub intent: String,
    pub done_when: String,
    pub delivers: Delivery,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CoordinationState {
    Working,
    Blocked,
    Done,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CoordinationReport {
    pub state: CoordinationState,
    pub summary: String,
    pub at: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DecisionOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub recommended: bool,
}

/// A choice only the user can make.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    pub id: String,
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    pub options: Vec<DecisionOption>,
    pub raised_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DecisionRequest {
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    pub options: Vec<DecisionOption>,
}

/// Something a coordinator has yet to hear about one of its threads.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoordinationNote {
    pub id: String,
    pub thread_id: String,
    pub text: String,
    pub at: i64,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub urgent: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub quiet: bool,
}

pub const MAX_BRIEF_FIELD: usize = 4_000;
pub const MAX_SUMMARY: usize = 500;
pub const MAX_QUESTION: usize = 300;
pub const MAX_DECISION_CONTEXT: usize = 2_000;
pub const MAX_OPTION_LABEL: usize = 120;
pub const MAX_OPTION_DESCRIPTION: usize = 300;
pub const MAX_OPTIONS: usize = 5;
pub const MAX_ANSWER: usize = 4_000;
const MAX_DECISIONS: usize = 20;
pub const MAX_OPEN_DECISIONS: usize = 10;
const MAX_NOTES: usize = 50;
pub const MAX_PURPOSE: usize = 200;
pub const MAX_MEMORY_NOTE: usize = 500;
pub const MAX_MEMORY_NOTES: usize = 30;
pub const MAX_MEMORY_NOTE_ID: usize = 200;
pub const MAX_PROPOSALS: usize = 12;
pub const MAX_PROPOSAL_TITLE: usize = 120;
pub const MAX_PROPOSAL_PROJECT: usize = 4_096;
pub const MAX_PROPOSAL_COMPUTER: usize = 256;
const MAX_REMOTE_HEARD: usize = 100;
const MOST_REMOTE_HEARD: usize = 10 * MAX_REMOTE_HEARD;
pub const MAX_MEMORY_PROMPT: usize = 64_000;
pub const COORDINATOR_CONTEXT_TOKENS: i64 = 80_000;

/// The groups a coordinator's Overview sorts its threads into, most pressing first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OverviewSection {
    Needs,
    Review,
    Working,
    Done,
    Resolved,
    Memory,
    Usage,
}

pub const OVERVIEW_GROUPS: [OverviewSection; 5] = [OverviewSection::Needs, OverviewSection::Review, OverviewSection::Working, OverviewSection::Done, OverviewSection::Resolved];

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OverviewFold {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<bool>,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub all: bool,
}

pub type OverviewFolds = HashMap<OverviewSection, OverviewFold>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemoryNote {
    pub id: String,
    pub text: String,
    pub at: i64,
}

/// What a coordinator keeps for the user: what it is for, and notes on how they want it to work.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoordinatorMemory {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose_cleared: Option<i64>,
    pub notes: Vec<MemoryNote>,
    pub revision: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heard: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", rename_all_fields = "camelCase")]
pub enum MemoryChange {
    Purpose { purpose: Option<String> },
    Remember { text: String },
    Edit { note_id: String, text: String },
    Forget { note_id: String },
}

/// A thread a coordinator suggests rather than starts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposalRequest {
    pub title: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<ThreadBrief>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<AgentEngine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<AgentModel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<AgentEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<ThreadRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub computer: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProposalComputer {
    pub id: String,
    pub name: String,
}

/// Where a suggested thread starts.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposalPlace {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub computer: Option<ProposalComputer>,
}

/// A thread a coordinator suggested, waiting for the user to start, allow or dismiss it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadProposal {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub named: bool,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<ThreadBrief>,
    #[serde(default)]
    pub place: ProposalPlace,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<AgentModel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<AgentEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<ThreadRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    pub proposed_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub carried: bool,
}

/// What the user can tell a thread under a coordinator to do next about its pull request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NextStep {
    ResolveConflicts,
    FixCi,
    AddressComments,
    Merge,
    CreatePullRequest,
}

pub const NEXT_STEPS: [NextStep; 5] = [NextStep::ResolveConflicts, NextStep::FixCi, NextStep::AddressComments, NextStep::Merge, NextStep::CreatePullRequest];

impl NextStep {
    pub fn label(self) -> &'static str {
        match self {
            NextStep::ResolveConflicts => "Resolve conflicts",
            NextStep::FixCi => "Fix CI",
            NextStep::AddressComments => "Address comments",
            NextStep::Merge => "Merge it",
            NextStep::CreatePullRequest => "Create PR",
        }
    }

    /// What a button for it is read as beside other threads' buttons.
    pub fn name(self, title: &str) -> String {
        match self {
            NextStep::ResolveConflicts => format!("Resolve conflicts in {title}'s pull request"),
            NextStep::FixCi => format!("Fix CI for {title}"),
            NextStep::AddressComments => format!("Address comments on {title}'s pull request"),
            NextStep::Merge => format!("Merge it: {title}'s pull request"),
            NextStep::CreatePullRequest => format!("Create PR for {title}"),
        }
    }

    pub fn text(self) -> &'static str {
        match self {
            NextStep::ResolveConflicts => "The pull request has merge conflicts with its base branch. Resolve them and push.",
            NextStep::FixCi => "The pull request's checks are failing. Find out why, fix them, and push.",
            NextStep::AddressComments => "The pull request has review comments asking for changes. Address each one, push, and reply on the pull request.",
            NextStep::Merge => "Merge the pull request.",
            NextStep::CreatePullRequest => "Open a pull request for this work.",
        }
    }
}

/// The coordinator on another computer a thread works under.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteLead {
    pub thread_id: String,
    pub title: String,
    pub computer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal_id: Option<String>,
}

impl RemoteLead {
    pub fn sender(&self) -> RemoteSender {
        RemoteSender { thread_id: self.thread_id.clone(), title: self.title.clone(), computer: self.computer.clone() }
    }
}

/// The coordinator on another computer as a thread holds it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeldLead {
    pub thread_id: String,
    pub title: String,
    pub computer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal_id: Option<String>,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub away: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub back_at: Option<i64>,
}

/// What a coordinator's threads run on unless one is started on something else.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThreadModelChoice {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<AgentEngine>,
    pub model: AgentModel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<AgentEffort>,
}

/// Whether a turn Stop all asked to end is heard quietly once it has.
pub fn ends_as_stopped(ending: Option<ThreadOutcome>) -> bool {
    !matches!(ending, Some(ThreadOutcome::Failed) | Some(ThreadOutcome::Finished))
}

pub fn is_coordinator(thread: Option<&Thread>) -> bool {
    thread.is_some_and(|thread| thread.role == Some(ThreadRole::Coordinator) && thread.archived_at.is_none())
}

/// Whose efforts a thread's own effort menu offers, or the draft's while there is no thread.
pub fn effort_role_of(thread: Option<&Thread>, draft_role: Option<ThreadRole>) -> Option<CoordinationRole> {
    let coordinating = match thread {
        Some(thread) => is_coordinator(Some(thread)),
        None => draft_role == Some(ThreadRole::Coordinator),
    };
    coordinating.then_some(CoordinationRole::Coordinator)
}

/// Whether threads on paired computers may still work under this one.
pub fn may_lead_elsewhere(thread: &Thread) -> bool {
    thread.role == Some(ThreadRole::Coordinator) || thread.remote_heard.as_ref().is_some_and(|heard| !heard.is_empty())
}

/// Whether a coordinator's next run starts a fresh session.
pub fn wants_fresh_session(thread: &Thread, session_holds: bool) -> bool {
    is_coordinator(Some(thread)) && thread.continuation.is_some() && thread.context_usage.as_ref().map(|usage| usage.tokens).unwrap_or(0) >= COORDINATOR_CONTEXT_TOKENS && !session_holds
}

/// What a coordinator's threads run on.
pub fn thread_model_of(catalog: &ModelCatalog, lead: &Thread) -> Option<ThreadModelChoice> {
    let saved = lead.thread_model.as_ref();
    let chosen = saved.map(|saved| (saved.engine.or_else(|| model_engine(catalog, &saved.model, Some(lead.engine))), saved.model.clone()));
    let own = thread_model(catalog, lead.engine, lead.model.as_deref()).map(|model| (Some(lead.engine), model));
    let fallback = default_model_in(catalog, lead.engine).map(|model| (Some(lead.engine), model));
    let (engine, model) = chosen.or(own).or(fallback)?;
    let known = engine.is_some_and(|engine| engine_has_model(catalog, engine, &model));
    if !known {
        let effort = saved.filter(|saved| saved.model == model).and_then(|saved| saved.effort);
        return Some(ThreadModelChoice { engine, model, effort });
    }
    let engine = engine.expect("known engine");
    if !model_takes_effort(catalog, engine, &model) {
        return Some(ThreadModelChoice { engine: Some(engine), model, effort: None });
    }
    let asked = saved.and_then(|saved| saved.effort).or(lead.effort).unwrap_or(default_effort_for(engine));
    let effort = effort_for_model(catalog, engine, &model, asked, None);
    Some(ThreadModelChoice { engine: Some(engine), model, effort: Some(effort) })
}

/// A thread taking the coordinator role, its threads to run on what it runs on.
pub fn as_coordinator(catalog: &ModelCatalog, thread: &mut Thread) {
    let choice = thread.thread_model.clone().or_else(|| thread_model_of(catalog, thread));
    thread.role = Some(ThreadRole::Coordinator);
    if choice.is_some() {
        thread.thread_model = choice;
    }
}

/// The coordinator a thread works under, while that thread is still one.
pub fn coordinator_of<'a>(threads: &'a [Shared<Thread>], thread: Option<&Thread>) -> Option<&'a Thread> {
    let thread = thread?;
    let parent = thread.parent_id.as_ref()?;
    if thread.role == Some(ThreadRole::Coordinator) {
        return None;
    }
    threads.iter().map(|item| &**item).find(|item| &item.id == parent).filter(|lead| is_coordinator(Some(lead)))
}

/// The id of the coordinator a thread names as the one it works under.
pub fn lead_id_of(thread: &Thread) -> Option<&str> {
    if let Some(parent) = &thread.parent_id {
        return Some(parent);
    }
    match &thread.remote_lead {
        Some(lead) if !lead.away => Some(&lead.thread_id),
        _ => None,
    }
}

/// The coordinator on another computer a thread works under, unless one here outranks it.
pub fn remote_lead_of(thread: Option<&Thread>) -> Option<&HeldLead> {
    let thread = thread?;
    let lead = thread.remote_lead.as_ref()?;
    (!lead.away && thread.parent_id.is_none() && thread.role != Some(ThreadRole::Coordinator)).then_some(lead)
}

pub fn away_lead_of(thread: Option<&Thread>) -> Option<&HeldLead> {
    let thread = thread?;
    let lead = thread.remote_lead.as_ref()?;
    (lead.away && thread.parent_id.is_none() && thread.role != Some(ThreadRole::Coordinator)).then_some(lead)
}

pub fn works_under_coordinator(threads: &[Shared<Thread>], thread: Option<&Thread>) -> bool {
    coordinator_of(threads, thread).is_some() || remote_lead_of(thread).is_some()
}

/// The threads working under a coordinator, in the order the list holds them.
pub fn coordinated_threads<'a>(threads: &'a [Shared<Thread>], lead_id: &str) -> Vec<&'a Thread> {
    coordinated_refs(&threads.iter().map(|thread| &**thread).collect::<Vec<_>>(), lead_id)
}

/// `coordinated_threads` over a list that may hold several computers' threads.
pub fn coordinated_refs<'a>(threads: &[&'a Thread], lead_id: &str) -> Vec<&'a Thread> {
    let lead = threads.iter().find(|thread| thread.id == lead_id).copied();
    if !is_coordinator(lead) {
        return Vec::new();
    }
    threads.iter().copied().filter(|thread| lead_id_of(thread) == Some(lead_id) && thread.archived_at.is_none() && thread.role != Some(ThreadRole::Coordinator)).collect()
}

/// Every thread drawn under a coordinator rather than in a list of its own.
pub fn coordinated_thread_ids(threads: &[Shared<Thread>]) -> HashSet<String> {
    coordinated_ids(&threads.iter().map(|thread| &**thread).collect::<Vec<_>>())
}

/// `coordinated_thread_ids` over a list that may hold several computers' threads.
pub fn coordinated_ids(threads: &[&Thread]) -> HashSet<String> {
    let leads: HashSet<&str> = threads.iter().filter(|thread| is_coordinator(Some(thread))).map(|thread| thread.id.as_str()).collect();
    threads
        .iter()
        .filter(|thread| lead_id_of(thread).is_some_and(|lead| leads.contains(lead)) && thread.role != Some(ThreadRole::Coordinator))
        .map(|thread| thread.id.clone())
        .collect()
}

/// The threads working under each coordinator, in the order they come.
pub fn threads_by_coordinator<'a>(threads: &[&'a Thread]) -> Map<Vec<&'a Thread>> {
    let members = coordinated_ids(threads);
    let mut by_lead: Map<Vec<&Thread>> = Map::new();
    for thread in threads.iter().copied() {
        if !members.contains(&thread.id) {
            continue;
        }
        let lead = lead_id_of(thread).expect("member has a lead").to_string();
        by_lead.entry(lead).or_default().push(thread);
    }
    by_lead
}

/// The named threads and every thread working under the coordinators among them.
pub fn with_coordinated(threads: &[Shared<Thread>], ids: &HashSet<String>) -> HashSet<String> {
    let leads: HashSet<&str> = threads.iter().filter(|thread| ids.contains(&thread.id) && is_coordinator(Some(thread))).map(|thread| thread.id.as_str()).collect();
    if leads.is_empty() {
        return ids.clone();
    }
    let mut all = ids.clone();
    for thread in threads {
        if lead_id_of(thread).is_some_and(|lead| leads.contains(lead)) && thread.role != Some(ThreadRole::Coordinator) {
            all.insert(thread.id.clone());
        }
    }
    all
}

pub fn can_join_coordinator(thread: &Thread, lead: Option<&Thread>) -> bool {
    lead.is_some_and(|lead| is_coordinator(Some(lead)) && lead.id != thread.id) && thread.role != Some(ThreadRole::Coordinator) && thread.archived_at.is_none()
}

pub fn open_decisions(thread: &Thread) -> Vec<&Decision> {
    thread.decisions.iter().flatten().filter(|decision| decision.answer.is_none()).collect()
}

pub fn decisions_full(thread: &Thread) -> bool {
    open_decisions(thread).len() >= MAX_OPEN_DECISIONS
}

/// What a thread's Needs-you card holds waiting on the user.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WaitingOnUser {
    pub decisions: usize,
    pub suggestions: usize,
    pub requests: Vec<String>,
    pub blocked: usize,
    pub failed: usize,
}

impl WaitingOnUser {
    pub fn any(&self) -> bool {
        self.decisions + self.suggestions + self.requests.len() + self.blocked + self.failed > 0
    }
}

pub fn waiting_on_user(thread: &Thread, members: &[&Thread], starting: &[String]) -> WaitingOnUser {
    let proposals: Vec<&ThreadProposal> = thread.proposals.iter().flatten().filter(|proposal| !starting.contains(&proposal.id)).collect();
    let stuck: Vec<Option<CoordinationState>> = members
        .iter()
        .filter(|member| member.resolved_at.is_none() && !member.run_open && wants_attention(member) && open_decisions(member).is_empty())
        .map(|member| stuck_verdict(member).map(|verdict| verdict.state))
        .collect();
    WaitingOnUser {
        decisions: std::iter::once(thread).chain(members.iter().copied()).map(|item| open_decisions(item).len()).sum(),
        suggestions: proposals.iter().filter(|proposal| proposal.place.computer.is_none()).count(),
        requests: proposals.iter().filter_map(|proposal| proposal.place.computer.as_ref().map(|computer| computer.name.clone())).collect(),
        blocked: stuck.iter().filter(|state| **state == Some(CoordinationState::Blocked)).count(),
        failed: stuck.iter().filter(|state| **state == Some(CoordinationState::Failed)).count(),
    }
}

/// Every decision waiting on the user from a coordinator and the threads under it, oldest first.
pub fn coordination_decisions<'a>(lead: &'a Thread, members: &[&'a Thread]) -> Vec<(&'a Thread, &'a Decision)> {
    if !is_coordinator(Some(lead)) {
        return Vec::new();
    }
    let mut all: Vec<(&Thread, &Decision)> = std::iter::once(lead).chain(members.iter().copied()).flat_map(|thread| open_decisions(thread).into_iter().map(move |decision| (thread, decision))).collect();
    all.sort_by_key(|(_, decision)| decision.raised_at);
    all
}

#[derive(Clone, Debug, PartialEq)]
pub struct StuckVerdict {
    pub state: CoordinationState,
    pub summary: Option<String>,
}

/// Whether a thread's latest turn left it stuck.
pub fn stuck_verdict(thread: &Thread) -> Option<StuckVerdict> {
    let report = thread.report.as_ref().filter(|report| report.state != CoordinationState::Working);
    if let Some(report) = report {
        return matches!(report.state, CoordinationState::Blocked | CoordinationState::Failed).then(|| StuckVerdict { state: report.state, summary: Some(report.summary.clone()) });
    }
    (last_run_outcome(thread) == Some(ThreadOutcome::Failed)).then_some(StuckVerdict { state: CoordinationState::Failed, summary: None })
}

/// A resolved thread that has just been left stuck is open again.
pub fn reopened_if_stuck(thread: &mut Thread) {
    if thread.resolved_at.is_some() && stuck_verdict(thread).is_some() {
        thread.resolved_at = None;
    }
}

pub fn with_decision(thread: &mut Thread, decision: Decision) {
    let mut kept = thread.decisions.take().unwrap_or_default();
    kept.push(decision);
    let excess = kept.len().saturating_sub(MAX_DECISIONS);
    let dropped: HashSet<String> = kept.iter().filter(|item| item.answer.is_some()).take(excess).map(|item| item.id.clone()).collect();
    kept.retain(|item| !dropped.contains(&item.id));
    thread.decisions = Some(kept);
}

pub fn with_answer(thread: &mut Thread, decision_id: &str, answer: &str, at: i64) {
    for decision in thread.decisions.iter_mut().flatten() {
        if decision.id == decision_id {
            decision.answer = Some(answer.to_string());
            decision.answered_at = Some(at);
        }
    }
}

/// A decision whose answer never reached the thread is open again.
pub fn without_answer(thread: &mut Thread, decision_id: &str) {
    for decision in thread.decisions.iter_mut().flatten() {
        if decision.id == decision_id {
            decision.answer = None;
            decision.answered_at = None;
        }
    }
}

fn note_text(value: &str) -> Result<String, String> {
    let text = value.trim();
    if text.is_empty() {
        return Err("A note needs some text.".into());
    }
    if js_len(text) > MAX_MEMORY_NOTE {
        return Err(format!("A note can be at most {MAX_MEMORY_NOTE} characters."));
    }
    Ok(text.to_string())
}

/// A coordinator's memory with one change made, or why it cannot be. Returns the note it named.
pub fn with_memory_change(thread: &mut Thread, change: &MemoryChange, at: i64, heard: bool) -> Result<Option<String>, String> {
    let memory = thread.memory.clone().unwrap_or(CoordinatorMemory { purpose: None, purpose_cleared: None, notes: Vec::new(), revision: 0, heard: Some(0) });
    let revision = memory.revision + 1;
    let mut purpose = memory.purpose.clone();
    let mut purpose_cleared = memory.purpose_cleared;
    let mut notes = memory.notes.clone();
    let mut note_id = None;
    match change {
        MemoryChange::Purpose { purpose: asked } => {
            let asked = asked.as_deref().unwrap_or("").trim().to_string();
            if js_len(&asked) > MAX_PURPOSE {
                return Err(format!("A purpose can be at most {MAX_PURPOSE} characters."));
            }
            if asked == purpose.clone().unwrap_or_default() {
                return Ok(None);
            }
            purpose = (!asked.is_empty()).then_some(asked);
            purpose_cleared = if purpose.is_some() { None } else { Some(revision) };
        }
        MemoryChange::Remember { text } => {
            let text = note_text(text)?;
            if notes.len() >= MAX_MEMORY_NOTES {
                return Err(format!("Memory holds at most {MAX_MEMORY_NOTES} notes. Forget one first."));
            }
            let id = format!("n{revision}");
            notes.push(MemoryNote { id: id.clone(), text, at });
            note_id = Some(id);
        }
        MemoryChange::Edit { note_id: wanted, text } => {
            let Some(index) = notes.iter().position(|note| &note.id == wanted) else { return Err(format!("No note has the ID {wanted}.")) };
            let text = note_text(text)?;
            if text == notes[index].text {
                return Ok(None);
            }
            notes[index].text = text;
            notes[index].at = at;
            note_id = Some(wanted.clone());
        }
        MemoryChange::Forget { note_id: wanted } => {
            if !notes.iter().any(|note| &note.id == wanted) {
                return Err(format!("No note has the ID {wanted}."));
            }
            notes.retain(|note| &note.id != wanted);
            note_id = Some(wanted.clone());
        }
    }
    let read = if heard && memory.heard == Some(memory.revision) { Some(revision) } else { memory.heard };
    thread.memory = Some(CoordinatorMemory { purpose, purpose_cleared, notes, revision, heard: read });
    Ok(note_id)
}

pub fn with_memory_heard(lead: &mut Thread) {
    if let Some(memory) = lead.memory.as_mut() {
        memory.heard = Some(memory.revision);
    }
}

pub fn with_memory_unheard(thread: &mut Thread) {
    if let Some(memory) = thread.memory.as_mut() {
        memory.heard = None;
    }
}

/// A coordinator with more suggestions waiting, as many as there is room for.
pub fn with_proposals(lead: &mut Thread, proposals: &[ThreadProposal]) {
    let held = lead.proposals.clone().unwrap_or_default();
    let room = MAX_PROPOSALS.saturating_sub(held.len());
    let added: Vec<ThreadProposal> = proposals.iter().take(room).cloned().collect();
    if !added.is_empty() {
        lead.proposals = Some(held.into_iter().chain(added).collect());
    }
}

/// A coordinator that has taken a thread on another computer's news up to `at`.
pub fn with_remote_heard(lead: &mut Thread, thread_id: &str, at: i64, led: &HashSet<String>) {
    let mut held = lead.remote_heard.clone().unwrap_or_default();
    if held.get(thread_id) == Some(&at) {
        return;
    }
    held.insert(thread_id.to_string(), at);
    if held.len() <= MAX_REMOTE_HEARD {
        lead.remote_heard = Some(held);
        return;
    }
    let oldest = |held: &Map<i64>, kept: &dyn Fn(&str) -> bool| -> Vec<String> {
        let mut entries: Vec<(&String, &i64)> = held.iter().filter(|(id, _)| id.as_str() != thread_id && kept(id)).collect();
        entries.sort_by_key(|(_, at)| **at);
        entries.into_iter().map(|(id, _)| id.clone()).collect()
    };
    let mut dropped: HashSet<String> = oldest(&held, &|id| !led.contains(id)).into_iter().take(held.len() - MAX_REMOTE_HEARD).collect();
    let left = held.len() - dropped.len();
    if left > MOST_REMOTE_HEARD {
        let more: Vec<String> = oldest(&held, &|id| !dropped.contains(id)).into_iter().take(left - MOST_REMOTE_HEARD).collect();
        dropped.extend(more);
    }
    held.retain(|id, _| !dropped.contains(id));
    lead.remote_heard = Some(held);
}

pub fn without_proposals(lead: &mut Thread, ids: &HashSet<String>) {
    let Some(held) = &lead.proposals else { return };
    let rest: Vec<ThreadProposal> = held.iter().filter(|proposal| !ids.contains(&proposal.id)).cloned().collect();
    if rest.len() == held.len() {
        return;
    }
    lead.proposals = if rest.is_empty() { None } else { Some(rest) };
}

pub fn with_proposal_error(lead: &mut Thread, id: &str, error: Option<String>) {
    if let Some(proposal) = lead.proposals.iter_mut().flatten().find(|proposal| proposal.id == id) {
        proposal.error = error;
    }
}

pub fn with_proposal_carried(lead: &mut Thread, id: &str) {
    if let Some(proposal) = lead.proposals.iter_mut().flatten().find(|proposal| proposal.id == id) {
        proposal.carried = true;
    }
}

/// A coordinator holds only so many notes it has yet to hear; the busiest thread gives up its oldest.
pub fn with_coordination_note(lead: &mut Thread, note: CoordinationNote) {
    let notes = lead.coordination_notes.get_or_insert_with(Vec::new);
    notes.push(note);
    while notes.len() > MAX_NOTES {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for note in notes.iter() {
            *counts.entry(note.thread_id.as_str()).or_default() += 1;
        }
        let most = counts.values().copied().max().unwrap_or(0);
        let busiest = notes.iter().find(|note| counts[note.thread_id.as_str()] == most).map(|note| note.thread_id.clone()).unwrap_or_default();
        let index = notes
            .iter()
            .position(|note| note.thread_id == busiest && !note.urgent)
            .or_else(|| notes.iter().position(|note| note.thread_id == busiest))
            .unwrap_or(0);
        notes.remove(index);
    }
}

pub fn without_coordination_notes(lead: &mut Thread, heard: &HashSet<String>) {
    let rest: Vec<CoordinationNote> = lead.coordination_notes.iter().flatten().filter(|note| !heard.contains(&note.id)).cloned().collect();
    lead.coordination_notes = if rest.is_empty() { None } else { Some(rest) };
}

fn bounded_text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && js_len(value) <= max
}

pub fn is_thread_brief(brief: &ThreadBrief) -> bool {
    bounded_text(&brief.intent, MAX_BRIEF_FIELD) && bounded_text(&brief.done_when, MAX_BRIEF_FIELD)
}

pub fn is_report_summary(value: &str) -> bool {
    bounded_text(value, MAX_SUMMARY)
}

pub fn is_memory_change(change: &MemoryChange) -> bool {
    match change {
        MemoryChange::Purpose { purpose } => purpose.as_ref().is_none_or(|purpose| js_len(purpose) <= MAX_PURPOSE),
        MemoryChange::Remember { text } => bounded_text(text, MAX_MEMORY_NOTE),
        MemoryChange::Edit { note_id, text } => bounded_text(note_id, MAX_MEMORY_NOTE_ID) && bounded_text(text, MAX_MEMORY_NOTE),
        MemoryChange::Forget { note_id } => bounded_text(note_id, MAX_MEMORY_NOTE_ID),
    }
}

/// Whether option labels are distinct, read the way the user tells them apart.
pub fn distinct_labels(options: &[DecisionOption]) -> bool {
    let labels: HashSet<String> = options.iter().map(|option| option.label.trim().to_lowercase()).collect();
    labels.len() == options.len()
}

pub fn is_decision_request(request: &DecisionRequest) -> bool {
    bounded_text(&request.question, MAX_QUESTION)
        && request.context.as_ref().is_none_or(|context| bounded_text(context, MAX_DECISION_CONTEXT))
        && request.options.len() <= MAX_OPTIONS
        && request.options.iter().all(|option| bounded_text(&option.label, MAX_OPTION_LABEL) && option.description.as_ref().is_none_or(|description| bounded_text(description, MAX_OPTION_DESCRIPTION)))
        && distinct_labels(&request.options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::engine::AgentEngine;
    use crate::domain::run::ExecutionPolicy;

    fn thread(id: &str) -> Thread {
        Thread::new(id, id, AgentEngine::Claude, ExecutionPolicy::Confirm, 1)
    }

    #[test]
    fn memory_changes_number_notes_by_revision() {
        let mut lead = thread("lead");
        let id = with_memory_change(&mut lead, &MemoryChange::Remember { text: "use pnpm".into() }, 5, true).unwrap();
        assert_eq!(id.as_deref(), Some("n1"));
        assert_eq!(lead.memory.as_ref().unwrap().heard, Some(1));
        let unread = with_memory_change(&mut lead, &MemoryChange::Remember { text: "second".into() }, 6, false).unwrap();
        assert_eq!(unread.as_deref(), Some("n2"));
        assert_eq!(lead.memory.as_ref().unwrap().heard, Some(1));
        assert!(with_memory_change(&mut lead, &MemoryChange::Forget { note_id: "n9".into() }, 7, true).is_err());
    }

    #[test]
    fn busiest_thread_loses_its_oldest_note_that_can_wait() {
        let mut lead = thread("lead");
        for index in 0..MAX_NOTES {
            let owner = if index == 0 { "quiet" } else { "loud" };
            with_coordination_note(&mut lead, CoordinationNote { id: format!("{index}"), thread_id: owner.into(), text: String::new(), at: index as i64, urgent: index == 1, run_id: None, quiet: false });
        }
        with_coordination_note(&mut lead, CoordinationNote { id: "new".into(), thread_id: "loud".into(), text: String::new(), at: 99, urgent: false, run_id: None, quiet: false });
        let notes = lead.coordination_notes.unwrap();
        assert_eq!(notes.len(), MAX_NOTES);
        assert!(notes.iter().any(|note| note.id == "0"));
        assert!(notes.iter().any(|note| note.id == "1"));
        assert!(!notes.iter().any(|note| note.id == "2"));
    }
}
