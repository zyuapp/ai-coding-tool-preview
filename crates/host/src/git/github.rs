//! The pull request a checkout's work belongs to, read through `gh`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use act_platform::process::{Exec, RunError};

/// Room for one round trip to GitHub. Nothing waits on this, so a slower network simply has no answer.
const TIMEOUT: Duration = Duration::from_secs(5);
/// How long one answer stands in for the next asker. Well under the poll interval.
const CACHE: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PullRequestState {
    Draft,
    Open,
    Merged,
    Closed,
}

impl PullRequestState {
    fn parse(state: &str) -> Option<Self> {
        match state {
            "draft" => Some(Self::Draft),
            "open" => Some(Self::Open),
            "merged" => Some(Self::Merged),
            "closed" => Some(Self::Closed),
            _ => None,
        }
    }

    /// A merged or closed pull request needs nothing more.
    pub fn settled(self) -> bool {
        matches!(self, Self::Merged | Self::Closed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PullRequestChecks {
    Pending,
    Passing,
    Failing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PullRequestReview {
    Approved,
    ChangesRequested,
    Required,
}

/// What a pull request still needs before it can merge, each part absent when GitHub says nothing of it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestNeeds {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checks: Option<PullRequestChecks>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review: Option<PullRequestReview>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflicts: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestRef {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: PullRequestState,
    #[serde(flatten)]
    pub needs: PullRequestNeeds,
}

/// The one a checkout's work belongs to, that it has none, or that `gh` is not installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum PullRequestAnswer {
    #[serde(rename_all = "camelCase")]
    Found { pull_request: PullRequestRef },
    None,
    GhMissing,
}

fn first_record(value: &Value) -> Option<&Map<String, Value>> {
    value.as_array()?.first()?.as_object()
}

fn lowercase(value: Option<&Value>) -> String {
    value.and_then(Value::as_str).map(str::to_lowercase).unwrap_or_default()
}

/// Every field a row draws has to be there and be itself, or there is no pull request to show.
fn pull_request(number: Option<&Value>, title: Option<&Value>, url: Option<&Value>, state: &str) -> Option<PullRequestRef> {
    let number = number?.as_f64().filter(|number| number.fract() == 0.0 && *number > 0.0)? as u64;
    let title = title?.as_str()?.to_string();
    let url = url?.as_str().filter(|url| !url.is_empty())?.to_string();
    Some(PullRequestRef { number, title, url, state: PullRequestState::parse(state)?, needs: PullRequestNeeds::default() })
}

/// `gh pr list --json number,title,url,state,isDraft,mergeable,reviewDecision,statusCheckRollup`.
pub fn pull_request_from_list(value: &Value) -> Option<PullRequestRef> {
    let found = first_record(value)?;
    let state = lowercase(found.get("state"));
    let state = if found.get("isDraft") == Some(&Value::Bool(true)) && state == "open" { "draft".to_string() } else { state };
    let mut reference = pull_request(found.get("number"), found.get("title"), found.get("url"), &state)?;
    reference.needs = needs_of(found);
    Some(reference)
}

/// `gh pr view --json mergeable,reviewDecision,statusCheckRollup`: what one pull request still needs.
pub fn pull_request_details(value: &Value) -> PullRequestNeeds {
    value.as_object().map(needs_of).unwrap_or_default()
}

/// `gh api repos/{owner}/{repo}/commits/{sha}/pulls`, which is REST and reads merged off its date.
pub fn pull_request_from_commit(value: &Value) -> Option<PullRequestRef> {
    let found = first_record(value)?;
    let merged = found.get("merged_at").is_some_and(|merged| !merged.is_null() && merged != &Value::Bool(false) && merged != "");
    let state = if merged {
        "merged".to_string()
    } else if found.get("draft") == Some(&Value::Bool(true)) {
        "draft".to_string()
    } else {
        lowercase(found.get("state"))
    };
    pull_request(found.get("number"), found.get("title"), found.get("html_url"), &state)
}

fn needs_of(found: &Map<String, Value>) -> PullRequestNeeds {
    let review = match found.get("reviewDecision").and_then(Value::as_str) {
        Some("APPROVED") => Some(PullRequestReview::Approved),
        Some("CHANGES_REQUESTED") => Some(PullRequestReview::ChangesRequested),
        Some("REVIEW_REQUIRED") => Some(PullRequestReview::Required),
        _ => None,
    };
    PullRequestNeeds {
        checks: found.get("statusCheckRollup").and_then(checks_of),
        review,
        conflicts: (found.get("mergeable").and_then(Value::as_str) == Some("CONFLICTING")).then_some(true),
    }
}

/// One entry of gh's `statusCheckRollup`: a check run, or a commit status.
fn check_of(entry: &Value) -> Option<PullRequestChecks> {
    let check = entry.as_object()?;
    let typename = check.get("__typename").and_then(Value::as_str);
    let state = check.get("state").and_then(Value::as_str);
    if typename == Some("StatusContext") || (typename.is_none() && check.get("status").is_none()) {
        return match state {
            Some("FAILURE" | "ERROR") => Some(PullRequestChecks::Failing),
            Some("PENDING" | "EXPECTED") => Some(PullRequestChecks::Pending),
            Some("SUCCESS") => Some(PullRequestChecks::Passing),
            _ => None,
        };
    }
    if check.get("status").and_then(Value::as_str) != Some("COMPLETED") {
        return Some(PullRequestChecks::Pending);
    }
    match check.get("conclusion").and_then(Value::as_str) {
        Some("FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE") => Some(PullRequestChecks::Failing),
        Some("SUCCESS" | "NEUTRAL" | "SKIPPED") => Some(PullRequestChecks::Passing),
        _ => None,
    }
}

/// One failing fails them all, else one still going keeps them pending. None is no answer.
pub fn checks_of(rollup: &Value) -> Option<PullRequestChecks> {
    let checks: Vec<Option<PullRequestChecks>> = rollup.as_array()?.iter().map(check_of).collect();
    [PullRequestChecks::Failing, PullRequestChecks::Pending, PullRequestChecks::Passing].into_iter().find(|wanted| checks.contains(&Some(*wanted)))
}

/// What a command said, and whether it said nothing because it is not installed at all.
struct Said {
    text: Option<String>,
    missing: bool,
}

async fn run(command: &str, args: &[&str], cwd: &Path) -> Said {
    match Exec::new(command).args(args.iter().copied()).cwd(cwd).timeout(TIMEOUT).run().await {
        Ok(output) => Said { text: Some(output.stdout_text().trim().to_string()).filter(|text| !text.is_empty()), missing: false },
        Err(error) => Said { text: None, missing: matches!(error, RunError::NotFound { .. }) },
    }
}

async fn read_json(args: &[&str], cwd: &Path) -> (Option<Value>, bool) {
    let said = run("gh", args, cwd).await;
    match said.text {
        Some(text) => (serde_json::from_str(&text).ok(), false),
        None => (None, said.missing),
    }
}

/// Only a checkout that has somewhere on GitHub to look loses anything when `gh` is not installed.
async fn on_github(root: &Path) -> bool {
    run("git", &["remote", "-v"], root).await.text.is_some_and(|remotes| remotes.contains("github.com"))
}

async fn answer_from(root: &Path, found: (Option<Value>, bool), parse: fn(&Value) -> Option<PullRequestRef>) -> PullRequestAnswer {
    if found.1 {
        return if on_github(root).await { PullRequestAnswer::GhMissing } else { PullRequestAnswer::None };
    }
    match found.0.as_ref().and_then(parse) {
        Some(pull_request) => PullRequestAnswer::Found { pull_request },
        None => PullRequestAnswer::None,
    }
}

type Pending = Arc<tokio::sync::OnceCell<PullRequestAnswer>>;

#[derive(Default)]
struct Cache {
    answers: HashMap<String, (Instant, PullRequestAnswer)>,
    asking: HashMap<String, Pending>,
}

fn cache() -> &'static Mutex<Cache> {
    static CACHE_CELL: std::sync::OnceLock<Mutex<Cache>> = std::sync::OnceLock::new();
    CACHE_CELL.get_or_init(Default::default)
}

/// Keyed by what was asked, never by the checkout alone, so moving to another branch never reads the
/// old answer. Concurrent askers share one `gh`.
async fn remembered<F: std::future::Future<Output = PullRequestAnswer>>(key: String, ask: impl FnOnce() -> F) -> PullRequestAnswer {
    let pending = {
        let mut cache = cache().lock().unwrap();
        cache.answers.retain(|_, (at, _)| at.elapsed() < CACHE);
        if let Some((_, answer)) = cache.answers.get(&key) {
            return answer.clone();
        }
        cache.asking.entry(key.clone()).or_default().clone()
    };
    let answer = pending.get_or_init(ask).await.clone();
    let mut cache = cache().lock().unwrap();
    if cache.asking.get(&key).is_some_and(|current| Arc::ptr_eq(current, &pending)) {
        cache.asking.remove(&key);
        cache.answers.insert(key, (Instant::now(), answer.clone()));
    }
    answer
}

/// The pull request the checkout's work belongs to. A branch is asked about by name, so unpushed
/// commits still find it; a detached worktree is asked about by its pushed commit, and one found that
/// way that can still move is asked about once more by number for its checks, review and conflicts.
pub async fn pull_request_for(root: &Path) -> PullRequestAnswer {
    if let Some(branch) = run("git", &["symbolic-ref", "--quiet", "--short", "HEAD"], root).await.text {
        let key = format!("{}\0{branch}", root.to_string_lossy());
        return remembered(key, || async move {
            let fields = "number,title,url,state,isDraft,mergeable,reviewDecision,statusCheckRollup";
            let found = read_json(&["pr", "list", "--head", &branch, "--state", "all", "--limit", "1", "--json", fields], root).await;
            answer_from(root, found, pull_request_from_list).await
        })
        .await;
    }
    let Some(commit) = run("git", &["rev-parse", "HEAD"], root).await.text else { return PullRequestAnswer::None };
    let key = format!("{}\0{commit}", root.to_string_lossy());
    remembered(key, || async move {
        let endpoint = format!("repos/{{owner}}/{{repo}}/commits/{commit}/pulls");
        let answer = answer_from(root, read_json(&["api", &endpoint], root).await, pull_request_from_commit).await;
        let PullRequestAnswer::Found { mut pull_request } = answer else { return answer };
        if pull_request.state.settled() {
            return PullRequestAnswer::Found { pull_request };
        }
        let number = pull_request.number.to_string();
        let (details, _) = read_json(&["pr", "view", &number, "--json", "mergeable,reviewDecision,statusCheckRollup"], root).await;
        pull_request.needs = details.as_ref().map(pull_request_details).unwrap_or_default();
        PullRequestAnswer::Found { pull_request }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_draft_is_an_open_pull_request_with_its_flag() {
        let listed = json!([{ "number": 7, "title": "Fix", "url": "https://github.com/o/r/pull/7", "state": "OPEN", "isDraft": true, "mergeable": "CONFLICTING", "reviewDecision": "CHANGES_REQUESTED", "statusCheckRollup": [] }]);
        let found = pull_request_from_list(&listed).unwrap();
        assert_eq!(found.state, PullRequestState::Draft);
        assert_eq!(found.needs, PullRequestNeeds { checks: None, review: Some(PullRequestReview::ChangesRequested), conflicts: Some(true) });
        assert!(pull_request_from_list(&json!([{ "number": 0, "title": "x", "url": "u", "state": "OPEN" }])).is_none());
    }

    #[test]
    fn one_failing_check_fails_them_all() {
        let rollup = json!([
            { "__typename": "CheckRun", "status": "COMPLETED", "conclusion": "SUCCESS" },
            { "__typename": "CheckRun", "status": "IN_PROGRESS" },
            { "__typename": "StatusContext", "state": "ERROR" },
        ]);
        assert_eq!(checks_of(&rollup), Some(PullRequestChecks::Failing));
        assert_eq!(checks_of(&json!([{ "status": "QUEUED" }, { "state": "SUCCESS" }])), Some(PullRequestChecks::Pending));
        assert_eq!(checks_of(&json!([])), None);
    }

    #[test]
    fn rest_reads_merged_off_its_date() {
        let pulls = json!([{ "number": 3, "title": "T", "html_url": "https://github.com/o/r/pull/3", "state": "closed", "merged_at": "2024-01-01T00:00:00Z" }]);
        assert_eq!(pull_request_from_commit(&pulls).unwrap().state, PullRequestState::Merged);
    }

    #[test]
    fn answers_serialize_as_the_renderer_reads_them() {
        let answer = PullRequestAnswer::Found {
            pull_request: PullRequestRef { number: 1, title: "t".into(), url: "u".into(), state: PullRequestState::Open, needs: PullRequestNeeds { checks: Some(PullRequestChecks::Passing), review: None, conflicts: None } },
        };
        assert_eq!(serde_json::to_value(&answer).unwrap(), json!({ "status": "found", "pullRequest": { "number": 1, "title": "t", "url": "u", "state": "open", "checks": "passing" } }));
        assert_eq!(serde_json::to_value(PullRequestAnswer::GhMissing).unwrap(), json!({ "status": "gh-missing" }));
    }
}
