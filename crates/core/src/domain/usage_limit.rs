//! Usage limits: a thread paused until its account's allowance resets, and the line it waits in.

use crate::shared::Shared;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::conversation::{Annotation, AttachedFile, PastedText};
use super::engine::AgentEngine;
use super::message_origin::MessageOrigin;
use super::thread::Thread;
use super::thread_role::ThreadRole;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LimitWindow {
    Session,
    Weekly,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimit {
    pub resets_at: i64,
    pub window: LimitWindow,
}

/// A message written to a paused or busy thread, kept until the thread takes it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeldMessage {
    pub id: String,
    pub text: String,
    pub prompt: String,
    #[serde(default)]
    pub attachments: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Vec<Annotation>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pastes: Option<Vec<PastedText>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<AttachedFile>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<MessageOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<String>,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub users_own: bool,
}

/// A thread waiting out its account's usage limit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitPause {
    pub resets_at: i64,
    pub window: LimitWindow,
    pub paused_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nudged_at: Option<i64>,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub workflow: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<Vec<HeldMessage>>,
}

/// Whether the thread resumes on its own once its limit lifts. A weekly limit waits for the user.
pub fn resumes_on_its_own(window: LimitWindow) -> bool {
    window == LimitWindow::Session
}

fn line_order(left: &Thread, right: &Thread) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (a, b) = (left.limit_pause.as_ref().unwrap(), right.limit_pause.as_ref().unwrap());
    if a.nudged_at.is_none() != b.nudged_at.is_none() {
        return if a.nudged_at.is_none() { Ordering::Greater } else { Ordering::Less };
    }
    if let (Some(x), Some(y)) = (a.nudged_at, b.nudged_at) {
        if x != y {
            return y.cmp(&x);
        }
    }
    let (left_reviews, right_reviews) = (left.role == Some(ThreadRole::Reviewer), right.role == Some(ThreadRole::Reviewer));
    if left_reviews != right_reviews {
        return if left_reviews { Ordering::Less } else { Ordering::Greater };
    }
    if a.workflow != b.workflow {
        return if a.workflow { Ordering::Greater } else { Ordering::Less };
    }
    a.paused_at.cmp(&b.paused_at)
}

/// Every engine's line of threads that resume on their own, first to go first.
pub fn limit_lines(threads: &[Shared<Thread>]) -> HashMap<AgentEngine, Vec<&Thread>> {
    let mut by_engine: HashMap<AgentEngine, Vec<&Thread>> = HashMap::new();
    for thread in threads {
        let Some(pause) = &thread.limit_pause else { continue };
        if thread.archived_at.is_some() || !resumes_on_its_own(pause.window) {
            continue;
        }
        by_engine.entry(thread.engine).or_default().push(thread);
    }
    for line in by_engine.values_mut() {
        line.sort_by(|left, right| line_order(left, right));
    }
    by_engine
}

/// Where each paused thread stands in its engine's line, counting from 1.
pub fn line_positions(threads: &[Shared<Thread>]) -> HashMap<String, usize> {
    let mut places = HashMap::new();
    for line in limit_lines(threads).values() {
        for (index, thread) in line.iter().enumerate() {
            places.insert(thread.id.clone(), index + 1);
        }
    }
    places
}

/// A limit is the account's, so every thread of the engine waiting on an earlier reset waits for this one too.
pub fn with_engine_limit(threads: &mut [Shared<Thread>], engine: AgentEngine, limit: &UsageLimit) -> bool {
    let mut changed = false;
    for thread in threads.iter_mut() {
        if thread.engine != engine {
            continue;
        }
        if thread.limit_pause.as_ref().is_none_or(|pause| pause.resets_at >= limit.resets_at) {
            continue;
        }
        let Some(pause) = thread.limit_pause.as_mut() else { continue };
        changed = true;
        pause.resets_at = limit.resets_at;
        if limit.window == LimitWindow::Weekly {
            pause.window = LimitWindow::Weekly;
        }
    }
    changed
}

const WORKFLOW_NOTE: &str = "A dynamic workflow was cut short when the usage limit hit: resume it with the Workflow tool's resumeFromRunId, using the run ID its launch returned, so finished agents are not run again.";

/// What a resumed thread is told: the message written to it while it waited, or to carry on.
pub fn resume_prompt(pause: &LimitPause, written: Option<&str>) -> String {
    let prompt = written.unwrap_or("The usage limit that paused this thread has reset. Continue where you left off.");
    if pause.workflow {
        format!("{prompt}\n\n{WORKFLOW_NOTE}")
    } else {
        prompt.to_string()
    }
}

/// When a limit lifts, as a time today or a day and time further out.
pub fn lift_time(resets_at: i64, now: i64) -> String {
    use chrono::{Local, TimeZone};
    let at = Local.timestamp_millis_opt(resets_at).single();
    let today = Local.timestamp_millis_opt(now).single();
    let (Some(at), Some(today)) = (at, today) else { return String::new() };
    let time = at.format("%-I:%M %p").to_string();
    if at.date_naive() == today.date_naive() {
        time
    } else {
        format!("{} {time}", at.format("%a"))
    }
}

fn ordinal(position: usize) -> String {
    let tens = position % 100;
    let suffix = if (11..=13).contains(&tens) {
        "th"
    } else {
        match position % 10 {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        }
    };
    format!("{position}{suffix}")
}

/// What a paused thread's row says about it: when it goes, and behind how many others.
pub fn pause_summary(pause: &LimitPause, position: Option<usize>, now: i64) -> String {
    if !resumes_on_its_own(pause.window) {
        return if now < pause.resets_at { format!("Weekly limit · resets {}", lift_time(pause.resets_at, now)) } else { "Weekly limit reset".into() };
    }
    let place = match position {
        Some(position) if position > 1 => format!(" · {}", ordinal(position)),
        _ => String::new(),
    };
    if now < pause.resets_at {
        format!("Resumes {}{place}", lift_time(pause.resets_at, now))
    } else {
        format!("Resuming{place}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinals_cover_teens() {
        assert_eq!(ordinal(2), "2nd");
        assert_eq!(ordinal(12), "12th");
        assert_eq!(ordinal(23), "23rd");
    }
}
