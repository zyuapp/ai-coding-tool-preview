//! A thread's recurring prompt, the scheduler that fires it, and the hand-off of each tick to the window.

pub mod dispatch;
pub mod schedule;
pub mod scheduler;

use serde::{Deserialize, Serialize};

pub const MAX_AUTOMATION_PROMPT: usize = 100_000;
pub const MAX_AUTOMATION_SCHEDULE: usize = 200;
pub const MAX_AUTOMATION_TIMEZONE: usize = 100;
pub const MAX_SURFACE_WHEN: usize = 500;
pub const MAX_ENDS_WHEN: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutionPolicy {
    Confirm,
    Plan,
    AllowEdits,
    Autonomous,
    Bypass,
}

impl ExecutionPolicy {
    pub const ALL: [ExecutionPolicy; 5] = [Self::Confirm, Self::Plan, Self::AllowEdits, Self::Autonomous, Self::Bypass];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Confirm => "confirm",
            Self::Plan => "plan",
            Self::AllowEdits => "allow-edits",
            Self::Autonomous => "autonomous",
            Self::Bypass => "bypass",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|policy| policy.as_str() == value)
    }

    /// How much a policy lets a run do unasked, which a scheduled run may never exceed its creator's.
    fn rank(self) -> u8 {
        match self {
            Self::Plan => 0,
            Self::Confirm => 1,
            Self::AllowEdits => 2,
            Self::Autonomous => 3,
            Self::Bypass => 4,
        }
    }
}

/// `missed` is not a run: it marks a one-shot whose moment passed without one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AutomationRunStatus {
    Succeeded,
    Failed,
    Cancelled,
    Skipped,
    Missed,
}

impl AutomationRunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Skipped => "skipped",
            Self::Missed => "missed",
        }
    }
}

/// A thread's recurring prompt. One per thread: re-creating replaces the previous one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Automation {
    pub id: String,
    pub task_id: String,
    pub prompt: String,
    /// A five-field cron expression, or an ISO 8601 timestamp for a single run.
    pub schedule: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// Overrides the thread's policy for scheduled runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<ExecutionPolicy>,
    /// Present makes the schedule quiet: a tick that says it found nothing settles without surfacing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_when: Option<String>,
    /// Present makes it a watch: its thread counts as running until the automation is removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ends_when: Option<String>,
    pub paused: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub run_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status: Option<AutomationRunStatus>,
    /// When `last_status` was recorded. A tick that never ran moves this without moving `last_run_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status_at: Option<i64>,
    /// Ticks turned away in a row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consecutive_declines: Option<u64>,
    /// Ticks dropped because the previous run was still going. Nothing else records these.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overrun_count: Option<u64>,
}

/// The record plus the firing time only the scheduler can compute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationView {
    #[serde(flatten)]
    pub automation: Automation,
    pub next_run_at: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationDraft {
    pub task_id: String,
    pub prompt: String,
    pub schedule: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<ExecutionPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_when: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ends_when: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<ExecutionPolicy>,
    /// An empty sentence makes the schedule loud again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_when: Option<String>,
    /// An empty sentence makes the automation a routine again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ends_when: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<bool>,
}

impl AutomationPatch {
    fn only_pauses(&self) -> bool {
        self.paused == Some(true)
            && self.prompt.is_none()
            && self.schedule.is_none()
            && self.timezone.is_none()
            && self.policy.is_none()
            && self.surface_when.is_none()
            && self.ends_when.is_none()
    }
}

/// What kind of tick this is. Only a cron tick of a quiet schedule may settle unseen; `unattended`
/// is a cron tick with nobody to answer its approvals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TickKind {
    pub quiet: bool,
    pub unattended: bool,
}

/// A tick that never ran leaves the counters alone so "last run" stays truthful.
pub fn automation_after_run(automation: &Automation, status: AutomationRunStatus, at: i64) -> Automation {
    let mut updated = automation.clone();
    updated.last_status = Some(status);
    updated.last_status_at = Some(at);
    updated.updated_at = at;
    match status {
        AutomationRunStatus::Missed => {}
        AutomationRunStatus::Skipped => updated.consecutive_declines = Some(automation.consecutive_declines.unwrap_or(0) + 1),
        _ => {
            updated.consecutive_declines = None;
            updated.run_count = automation.run_count + 1;
            updated.last_run_at = Some(at);
        }
    }
    updated
}

/// How many declines in a row it takes before the schedule's silence is itself worth surfacing.
pub const DECLINES_BEFORE_SURFACING: u64 = 3;

/// Whether this tick may settle unseen: only a cron tick of a schedule that says what it surfaces for.
pub fn quiet_tick(automation: &Automation, manual: bool) -> bool {
    !manual && automation.surface_when.is_some() && !is_one_shot_schedule(&automation.schedule)
}

/// A paused watch is waiting on the user, not on what it watches.
pub fn is_watching(automation: &Automation) -> bool {
    automation.ends_when.is_some() && !automation.paused
}

pub fn did_run(status: AutomationRunStatus) -> bool {
    !matches!(status, AutomationRunStatus::Skipped | AutomationRunStatus::Missed)
}

/// A one-shot automation is an ISO 8601 timestamp rather than a cron expression.
pub fn is_one_shot_schedule(schedule: &str) -> bool {
    schedule.contains('-') && schedule::parse_iso_instant(schedule, None).is_some()
}

pub fn schedule_field_count(schedule: &str) -> usize {
    schedule.split_whitespace().count().max(1)
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct AutomationError(pub String);

impl AutomationError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// The run's ceiling, rechecked inside the mutation queue; it fails once the run may no longer write.
pub type AutomationAuthority<'a> = &'a (dyn Fn() -> Result<ExecutionPolicy, AutomationError> + Send + Sync);

fn permitted_policy(policy: Option<ExecutionPolicy>, authority: Option<AutomationAuthority>) -> Result<Option<ExecutionPolicy>, AutomationError> {
    let Some(authority) = authority else { return Ok(policy) };
    let ceiling = authority()?;
    let effective = policy.unwrap_or(ceiling);
    if effective.rank() > ceiling.rank() {
        return Err(AutomationError::new("An automation cannot use stronger permissions than this run. Change the task's permissions before scheduling it."));
    }
    Ok(Some(effective))
}

/// An absent sentence keeps the stored one; an empty one drops it.
fn patched_sentence(patched: Option<&str>, stored: Option<&str>) -> Option<String> {
    match patched {
        Some("") => None,
        Some(sentence) => Some(sentence.to_string()),
        None => stored.map(String::from),
    }
}

/// A one-time run is never quiet, as it removes itself once it has run.
fn quiet_sentence(schedule: &str, given: Option<&str>, kept: Option<String>) -> Result<Option<String>, AutomationError> {
    if !is_one_shot_schedule(schedule) {
        return Ok(kept);
    }
    if given.is_some_and(|given| !given.is_empty()) {
        return Err(AutomationError::new("A one-time run always surfaces, so it takes no surfaceWhen. Leave surfaceWhen out, or use a cron schedule for runs that stay quiet."));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn automation() -> Automation {
        Automation {
            id: "a".into(),
            task_id: "t".into(),
            prompt: "p".into(),
            schedule: "* * * * *".into(),
            timezone: None,
            policy: None,
            surface_when: Some("x".into()),
            ends_when: None,
            paused: false,
            created_at: 0,
            updated_at: 0,
            run_count: 2,
            last_run_at: None,
            last_status: None,
            last_status_at: None,
            consecutive_declines: Some(1),
            overrun_count: None,
        }
    }

    #[test]
    fn a_tick_that_never_ran_leaves_the_run_count_alone() {
        let skipped = automation_after_run(&automation(), AutomationRunStatus::Skipped, 5);
        assert_eq!((skipped.run_count, skipped.consecutive_declines, skipped.last_run_at), (2, Some(2), None));
        let missed = automation_after_run(&automation(), AutomationRunStatus::Missed, 5);
        assert_eq!((missed.run_count, missed.consecutive_declines), (2, Some(1)));
        let ran = automation_after_run(&automation(), AutomationRunStatus::Failed, 5);
        assert_eq!((ran.run_count, ran.consecutive_declines, ran.last_run_at), (3, None, Some(5)));
    }

    #[test]
    fn only_a_cron_tick_of_a_quiet_schedule_is_quiet() {
        let mut quiet = automation();
        assert!(quiet_tick(&quiet, false));
        assert!(!quiet_tick(&quiet, true));
        quiet.schedule = "2030-01-01T08:00:00".into();
        assert!(!quiet_tick(&quiet, false));
    }

    #[test]
    fn a_policy_above_the_runs_ceiling_is_refused() {
        let ceiling = || Ok(ExecutionPolicy::AllowEdits);
        assert!(permitted_policy(Some(ExecutionPolicy::Bypass), Some(&ceiling)).is_err());
        assert_eq!(permitted_policy(None, Some(&ceiling)).unwrap(), Some(ExecutionPolicy::AllowEdits));
        assert_eq!(permitted_policy(Some(ExecutionPolicy::Plan), Some(&ceiling)).unwrap(), Some(ExecutionPolicy::Plan));
        assert_eq!(permitted_policy(None, None).unwrap(), None);
    }

    #[test]
    fn serializes_with_the_stored_field_names() {
        let value = serde_json::to_value(automation()).unwrap();
        assert_eq!(value["taskId"], "t");
        assert_eq!(value["consecutiveDeclines"], 1);
        assert!(value.get("lastRunAt").is_none());
    }
}
