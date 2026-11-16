//! What GitHub says about the pull request a checkout's work belongs to.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PullRequestState {
    Draft,
    Open,
    Merged,
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PullRequestChecks {
    Pending,
    Passing,
    Failing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PullRequestReview {
    Approved,
    ChangesRequested,
    Required,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PullRequestRef {
    pub number: i64,
    pub title: String,
    pub url: String,
    pub state: PullRequestState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<PullRequestChecks>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<PullRequestReview>,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub conflicts: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum PullRequestAnswer {
    Found { pull_request: PullRequestRef },
    None,
    GhMissing,
}

/// An answer and what it answers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestRead {
    pub workspace_id: String,
    pub branch: Option<String>,
    pub read: i64,
    pub answer: PullRequestAnswer,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub answered: bool,
}

/// The pull request a thread's work was last seen on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PullRequestLink {
    pub number: i64,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<PullRequestState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<PullRequestChecks>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<PullRequestReview>,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub conflicts: bool,
}

pub fn link_of(pull_request: &PullRequestRef) -> PullRequestLink {
    PullRequestLink {
        number: pull_request.number,
        url: pull_request.url.clone(),
        state: Some(pull_request.state),
        title: Some(pull_request.title.clone()),
        checks: pull_request.checks,
        review: pull_request.review,
        conflicts: pull_request.conflicts,
    }
}

pub fn link_matches(link: Option<&PullRequestLink>, pull_request: &PullRequestRef) -> bool {
    link.is_some_and(|link| *link == link_of(pull_request))
}

#[derive(Clone, Debug, PartialEq)]
pub struct PullRequestQuery {
    pub number: i64,
    pub repository: Option<String>,
}

fn link_repository(url: &str) -> Option<(String, String)> {
    let pattern = regex::Regex::new(r"(?i)^(?:https?://)?([^/\s]+/[^/\s]+/[^/\s]+)/pull/(\d+)(?:[/?#]\S*)?$").ok()?;
    let found = pattern.captures(url)?;
    Some((found[1].to_string(), found[2].to_string()))
}

/// What a search names when it names a pull request: `12`, `#12`, or a link to one.
pub fn pull_request_query(query: &str) -> Option<PullRequestQuery> {
    let wanted = query.trim();
    let link = link_repository(wanted);
    let digits = match &link {
        Some((_, number)) => number.clone(),
        None => {
            let bare = wanted.strip_prefix('#').unwrap_or(wanted);
            if bare.is_empty() || !bare.chars().all(|ch| ch.is_ascii_digit()) {
                return None;
            }
            bare.to_string()
        }
    };
    let number: i64 = digits.parse().ok()?;
    if number <= 0 || number > 9_007_199_254_740_991 {
        return None;
    }
    Some(PullRequestQuery { number, repository: link.map(|(repository, _)| repository.to_lowercase()) })
}

pub fn links_pull_request(link: &PullRequestLink, query: &PullRequestQuery) -> bool {
    if link.number != query.number {
        return false;
    }
    match &query.repository {
        None => true,
        Some(repository) => link_repository(&link.url).is_some_and(|(found, _)| found.to_lowercase() == *repository),
    }
}

/// How often an unsettled pull request is asked about again.
pub const PULL_REQUEST_POLL_MS: i64 = 60_000;

/// True once nothing local or remote will move the pull request again.
pub fn pull_request_settled(answer: &PullRequestAnswer) -> bool {
    matches!(answer, PullRequestAnswer::Found { pull_request } if matches!(pull_request.state, PullRequestState::Merged | PullRequestState::Closed))
}

pub fn same_pull_request(left: &PullRequestAnswer, right: &PullRequestAnswer) -> bool {
    left == right
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_read_numbers_and_links() {
        assert_eq!(pull_request_query("#12").unwrap().number, 12);
        let link = pull_request_query("https://github.com/Owner/Repo/pull/7/files").unwrap();
        assert_eq!(link.repository.as_deref(), Some("github.com/owner/repo"));
        assert!(pull_request_query("abc").is_none());
    }
}
