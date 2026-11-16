//! Where a thread under a coordinator stands, what its own pull request says, and what the user can
//! tell it to do next.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::domain::coordination::{open_decisions, stuck_verdict, CoordinationState, Delivery, NextStep, OverviewSection, MAX_SUMMARY};
use crate::domain::pull_request::{PullRequestAnswer, PullRequestChecks, PullRequestLink, PullRequestReview, PullRequestState};
use crate::domain::thread::{last_run_outcome, Thread, ThreadOutcome};
use crate::domain::tool_call::reply_line;
use crate::state::WorkspaceState;
use crate::workspace::location::thread_workspace_id;
use crate::workspace::notices::last_reply;

/// Where a thread under a coordinator stands, in the words its card shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CoordinatedThreadStatus {
    Approval,
    Asking,
    Working,
    Resolved,
    Blocked,
    Failed,
    NeedsWork,
    Review,
    Done,
    Idle,
}

/// A pull request that is a thread's own work, as last read: its state and title are known.
#[derive(Clone, Debug, PartialEq)]
pub struct OwnPullRequest {
    pub number: i64,
    pub url: String,
    pub state: PullRequestState,
    pub title: String,
    pub checks: Option<PullRequestChecks>,
    pub review: Option<PullRequestReview>,
    pub conflicts: bool,
}

/// Whether the pull request a thread's checkout is on would be its own work.
pub fn owns_pull_request(member: &Thread) -> bool {
    match &member.brief {
        Some(brief) => brief.delivers == Delivery::PullRequest,
        None => member.worktree_id.is_some(),
    }
}

/// The pull request that is a thread's own work, if it has one.
pub fn own_pull_request(member: &Thread) -> Option<OwnPullRequest> {
    let link: &PullRequestLink = member.pull_request.as_ref()?;
    let (state, title) = (link.state?, link.title.clone()?);
    owns_pull_request(member).then(|| OwnPullRequest { number: link.number, url: link.url.clone(), state, title, checks: link.checks, review: link.review, conflicts: link.conflicts })
}

/// A thread's own pull request in a few words.
pub fn pull_request_line(pull_request: &OwnPullRequest) -> String {
    let mut parts = vec![format!("#{}", pull_request.number)];
    match pull_request.state {
        PullRequestState::Draft => parts.push("Draft".into()),
        PullRequestState::Merged => parts.push("Merged".into()),
        PullRequestState::Closed => parts.push("Closed".into()),
        PullRequestState::Open => {}
    }
    if matches!(pull_request.state, PullRequestState::Open | PullRequestState::Draft) {
        if pull_request.conflicts {
            parts.push("Merge conflicts".into());
        }
        match pull_request.checks {
            Some(PullRequestChecks::Failing) => parts.push("Checks failing".into()),
            Some(PullRequestChecks::Passing) => parts.push("Checks passing".into()),
            Some(PullRequestChecks::Pending) => parts.push("Checks pending".into()),
            None => {}
        }
        if pull_request.review == Some(PullRequestReview::ChangesRequested) {
            parts.push("Changes requested".into());
        }
    }
    parts.join(" · ")
}

/// What a thread's own open or draft pull request needs fixed, most pressing first.
fn pull_request_fixes(pull_request: &OwnPullRequest) -> Vec<NextStep> {
    if !matches!(pull_request.state, PullRequestState::Open | PullRequestState::Draft) {
        return Vec::new();
    }
    let mut fixes = Vec::new();
    if pull_request.conflicts {
        fixes.push(NextStep::ResolveConflicts);
    }
    if pull_request.checks == Some(PullRequestChecks::Failing) {
        fixes.push(NextStep::FixCi);
    }
    if pull_request.review == Some(PullRequestReview::ChangesRequested) {
        fixes.push(NextStep::AddressComments);
    }
    fixes
}

/// Whether the user can file a thread away under Resolved as it stands now.
pub fn can_resolve(status: CoordinatedThreadStatus) -> bool {
    !matches!(status, CoordinatedThreadStatus::Approval | CoordinatedThreadStatus::Asking | CoordinatedThreadStatus::Working | CoordinatedThreadStatus::Resolved)
}

/// What GitHub last said of the pull request of the checkout a thread works in.
pub fn checkout_answer<'a>(state: &'a WorkspaceState, member: &Thread) -> Option<&'a PullRequestAnswer> {
    let workspace_id = thread_workspace_id(state, Some(member))?;
    state.member_pull_requests.get(&workspace_id).filter(|read| read.answered).map(|read| &read.answer)
}

/// What the user can tell a thread under a coordinator to do next about its own pull request.
pub fn next_steps(member: &Thread, status: CoordinatedThreadStatus, checkout: Option<&PullRequestAnswer>) -> Vec<NextStep> {
    if !can_resolve(status) {
        return Vec::new();
    }
    let Some(pull_request) = own_pull_request(member) else {
        let delivers = member.brief.as_ref().is_some_and(|brief| brief.delivers == Delivery::PullRequest);
        return if delivers && matches!(checkout, Some(PullRequestAnswer::None)) { vec![NextStep::CreatePullRequest] } else { Vec::new() };
    };
    let steps = pull_request_fixes(&pull_request);
    let mergeable = pull_request.state == PullRequestState::Open
        && steps.is_empty()
        && pull_request.checks != Some(PullRequestChecks::Pending)
        && matches!(pull_request.review, None | Some(PullRequestReview::Approved));
    if mergeable {
        vec![NextStep::Merge]
    } else {
        steps
    }
}

/// Where a thread under a coordinator stands, as its status says it.
#[derive(Clone, Debug, PartialEq)]
pub struct CoordinatedThreadStanding<'a> {
    pub thread: &'a Thread,
    pub status: CoordinatedThreadStatus,
    pub summary: Option<String>,
}

/// Where a thread under a coordinator stands, most pressing first.
pub fn coordinated_thread_status<'a>(thread: &'a Thread, busy: &HashSet<String>, blocked: &HashSet<String>) -> CoordinatedThreadStanding<'a> {
    let standing = |status, summary| CoordinatedThreadStanding { thread, status, summary };
    if blocked.contains(&thread.id) {
        return standing(CoordinatedThreadStatus::Approval, None);
    }
    if let Some(decision) = open_decisions(thread).first() {
        return standing(CoordinatedThreadStatus::Asking, Some(decision.question.clone()));
    }
    if busy.contains(&thread.id) {
        let summary = thread.report.as_ref().filter(|report| report.state == CoordinationState::Working).map(|report| report.summary.clone());
        return standing(CoordinatedThreadStatus::Working, summary);
    }
    if thread.resolved_at.is_some() {
        return standing(CoordinatedThreadStatus::Resolved, None);
    }
    if let Some(stuck) = stuck_verdict(thread) {
        let status = if stuck.state == CoordinationState::Blocked { CoordinatedThreadStatus::Blocked } else { CoordinatedThreadStatus::Failed };
        return standing(status, stuck.summary);
    }
    let report = thread.report.as_ref().filter(|report| report.state != CoordinationState::Working);
    let ended = last_run_outcome(thread);
    let pull_request = own_pull_request(thread);
    if let Some(pull_request) = &pull_request {
        if !pull_request_fixes(pull_request).is_empty() {
            return standing(CoordinatedThreadStatus::NeedsWork, Some(pull_request_line(pull_request)));
        }
        if pull_request.state == PullRequestState::Open {
            return standing(CoordinatedThreadStatus::Review, Some(pull_request_line(pull_request)));
        }
    }
    if let Some(report) = report {
        return standing(CoordinatedThreadStatus::Done, Some(report.summary.clone()));
    }
    if ended == Some(ThreadOutcome::Finished) {
        return standing(CoordinatedThreadStatus::Done, closing_line(thread));
    }
    standing(CoordinatedThreadStatus::Idle, None)
}

fn closing_line(thread: &Thread) -> Option<String> {
    last_reply(thread, 0).and_then(|reply| reply_line(&reply, MAX_SUMMARY))
}

/// The Overview group each status files a thread under, which the coordinator's roster reads too.
pub fn group_of(status: CoordinatedThreadStatus) -> OverviewSection {
    match status {
        CoordinatedThreadStatus::Approval | CoordinatedThreadStatus::Asking | CoordinatedThreadStatus::Blocked | CoordinatedThreadStatus::Failed | CoordinatedThreadStatus::NeedsWork => OverviewSection::Needs,
        CoordinatedThreadStatus::Review => OverviewSection::Review,
        CoordinatedThreadStatus::Working => OverviewSection::Working,
        CoordinatedThreadStatus::Done | CoordinatedThreadStatus::Idle => OverviewSection::Done,
        CoordinatedThreadStatus::Resolved => OverviewSection::Resolved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::coordination::{CoordinationReport, ThreadBrief};
    use crate::domain::engine::AgentEngine;
    use crate::domain::run::ExecutionPolicy;

    fn member() -> Thread {
        let mut thread = Thread::new("t", "T", AgentEngine::Claude, ExecutionPolicy::Confirm, 1);
        thread.brief = Some(ThreadBrief { intent: "x".into(), done_when: "y".into(), delivers: Delivery::PullRequest });
        thread
    }

    fn link(state: PullRequestState, checks: Option<PullRequestChecks>, conflicts: bool) -> PullRequestLink {
        PullRequestLink { number: 7, url: "u".into(), state: Some(state), title: Some("PR".into()), checks, review: None, conflicts }
    }

    #[test]
    fn pull_request_fixes_outrank_review_and_open_ones_merge() {
        let none = HashSet::new();
        let mut thread = member();
        thread.pull_request = Some(link(PullRequestState::Open, Some(PullRequestChecks::Failing), true));
        let standing = coordinated_thread_status(&thread, &none, &none);
        assert_eq!(standing.status, CoordinatedThreadStatus::NeedsWork);
        assert_eq!(standing.summary.as_deref(), Some("#7 · Merge conflicts · Checks failing"));
        assert_eq!(next_steps(&thread, standing.status, None), vec![NextStep::ResolveConflicts, NextStep::FixCi]);
        thread.pull_request = Some(link(PullRequestState::Open, Some(PullRequestChecks::Passing), false));
        let standing = coordinated_thread_status(&thread, &none, &none);
        assert_eq!(standing.status, CoordinatedThreadStatus::Review);
        assert_eq!(next_steps(&thread, standing.status, None), vec![NextStep::Merge]);
    }

    #[test]
    fn working_and_stuck_threads_are_offered_nothing_until_settled() {
        let mut thread = member();
        thread.report = Some(CoordinationReport { state: CoordinationState::Blocked, summary: "needs a key".into(), at: 2 });
        let busy: HashSet<String> = ["t".to_string()].into();
        let none = HashSet::new();
        assert_eq!(coordinated_thread_status(&thread, &busy, &none).status, CoordinatedThreadStatus::Working);
        let stuck = coordinated_thread_status(&thread, &none, &none);
        assert_eq!(stuck.status, CoordinatedThreadStatus::Blocked);
        assert!(next_steps(&thread, CoordinatedThreadStatus::Working, Some(&PullRequestAnswer::None)).is_empty());
        assert_eq!(next_steps(&thread, stuck.status, Some(&PullRequestAnswer::None)), vec![NextStep::CreatePullRequest]);
    }
}
