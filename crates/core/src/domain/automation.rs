//! A thread's recurring prompt.

use serde::{Deserialize, Serialize};

use super::run::ExecutionPolicy;
use crate::util::js_len;

pub const MAX_AUTOMATION_PROMPT: usize = 100_000;
pub const MAX_AUTOMATION_SCHEDULE: usize = 200;
pub const MAX_AUTOMATION_TIMEZONE: usize = 100;
pub const MAX_SURFACE_WHEN: usize = 500;
pub const MAX_ENDS_WHEN: usize = 500;

/// `missed` is not a run: it marks a one-shot whose moment passed without one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AutomationRunStatus {
    Succeeded,
    Failed,
    Cancelled,
    Skipped,
    Missed,
}

/// A thread's recurring prompt. One per thread: re-creating replaces the previous one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Automation {
    pub id: String,
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
    pub paused: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub run_count: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status: Option<AutomationRunStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consecutive_declines: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overrun_count: Option<i64>,
}

/// What the renderer renders: the record plus the firing time only the scheduler can compute.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationView {
    #[serde(flatten)]
    pub automation: Automation,
    pub next_run_at: Option<i64>,
}

/// The fields the automation panel edits, each empty when unset.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationFields {
    pub schedule: String,
    pub prompt: String,
    pub surface_when: String,
    pub ends_when: String,
}

/// Edits the panel holds unsaved, with the saved fields they began from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationEdit {
    pub base: AutomationFields,
    pub fields: AutomationFields,
}

pub fn automation_fields(automation: Option<&Automation>) -> AutomationFields {
    match automation {
        None => AutomationFields::default(),
        Some(automation) => AutomationFields {
            schedule: automation.schedule.clone(),
            prompt: automation.prompt.clone(),
            surface_when: automation.surface_when.clone().unwrap_or_default(),
            ends_when: automation.ends_when.clone().unwrap_or_default(),
        },
    }
}

/// The unsaved edits while the saved fields they began from still stand, else the saved fields.
pub fn edited_automation_fields(automation: Option<&Automation>, edit: Option<&AutomationEdit>) -> AutomationFields {
    let saved = automation_fields(automation);
    match edit {
        Some(edit) if edit.base == saved => edit.fields.clone(),
        _ => saved,
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationDraft {
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

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_when: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ends_when: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<bool>,
}

fn is_text(value: &str, max: usize) -> bool {
    !value.is_empty() && js_len(value) <= max
}

pub fn is_automation_draft(draft: &AutomationDraft) -> bool {
    is_text(&draft.prompt, MAX_AUTOMATION_PROMPT)
        && is_text(&draft.schedule, MAX_AUTOMATION_SCHEDULE)
        && draft.timezone.as_deref().is_none_or(|zone| is_text(zone, MAX_AUTOMATION_TIMEZONE))
        && draft.surface_when.as_deref().is_none_or(|text| is_text(text, MAX_SURFACE_WHEN))
        && draft.ends_when.as_deref().is_none_or(|text| is_text(text, MAX_ENDS_WHEN))
}

pub fn is_automation_patch(patch: &AutomationPatch) -> bool {
    patch.prompt.as_deref().is_none_or(|text| is_text(text, MAX_AUTOMATION_PROMPT))
        && patch.schedule.as_deref().is_none_or(|text| is_text(text, MAX_AUTOMATION_SCHEDULE))
        && patch.timezone.as_deref().is_none_or(|text| is_text(text, MAX_AUTOMATION_TIMEZONE))
        && patch.surface_when.as_deref().is_none_or(|text| text.is_empty() || is_text(text, MAX_SURFACE_WHEN))
        && patch.ends_when.as_deref().is_none_or(|text| text.is_empty() || is_text(text, MAX_ENDS_WHEN))
}

/// A tick that never ran leaves the counters alone so "last run" stays truthful.
pub fn automation_after_run(automation: &Automation, status: AutomationRunStatus, at: i64) -> Automation {
    let mut next = automation.clone();
    next.last_status = Some(status);
    next.last_status_at = Some(at);
    next.updated_at = at;
    match status {
        AutomationRunStatus::Missed => {}
        AutomationRunStatus::Skipped => next.consecutive_declines = Some(decline_count(automation) + 1),
        _ => {
            next.consecutive_declines = None;
            next.run_count = automation.run_count + 1;
            next.last_run_at = Some(at);
        }
    }
    next
}

pub fn decline_count(automation: &Automation) -> i64 {
    automation.consecutive_declines.unwrap_or(0)
}

pub const DECLINES_BEFORE_SURFACING: i64 = 3;

/// Whether this tick may settle unseen: only a cron tick of a schedule that says what it surfaces for.
pub fn quiet_tick(automation: &Automation, manual: bool) -> bool {
    !manual && automation.surface_when.is_some() && !is_one_shot_schedule(&automation.schedule)
}

/// A paused watch is waiting on the user, not on what it watches.
pub fn is_watching(automation: &Automation) -> bool {
    automation.ends_when.is_some() && !automation.paused
}

/// The busy threads plus those a live watch holds in Running between its ticks.
pub fn with_watched_threads(busy: &std::collections::HashSet<String>, automations: &[AutomationView]) -> std::collections::HashSet<String> {
    let mut all = busy.clone();
    for view in automations {
        if is_watching(&view.automation) {
            all.insert(view.automation.task_id.clone());
        }
    }
    all
}

pub fn did_run(status: AutomationRunStatus) -> bool {
    !matches!(status, AutomationRunStatus::Skipped | AutomationRunStatus::Missed)
}

/// A one-shot automation is an ISO 8601 timestamp rather than a cron expression.
pub fn is_one_shot_schedule(schedule: &str) -> bool {
    schedule.contains('-') && (chrono::DateTime::parse_from_rfc3339(schedule).is_ok() || chrono::NaiveDateTime::parse_from_str(schedule, "%Y-%m-%dT%H:%M:%S").is_ok() || chrono::NaiveDateTime::parse_from_str(schedule, "%Y-%m-%dT%H:%M").is_ok() || chrono::NaiveDate::parse_from_str(schedule, "%Y-%m-%d").is_ok())
}

pub fn schedule_field_count(schedule: &str) -> usize {
    schedule.split_whitespace().count()
}
