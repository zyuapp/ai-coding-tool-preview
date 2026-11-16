//! Reads across this computer and the paired ones: which computers a caller names, and which thread
//! a reference means on them.

use act_core::domain::platform::{away_status, ComputerStatus};
use act_core::domain::project::matching_projects;
use act_core::domain::thread::Thread;
use act_core::state::PairedComputer;
use act_core::WorkspaceState;
use serde_json::{json, Value};

use super::projection::{resolve_scope, thread_matches, thread_summaries, thread_summary, thread_transcript, ThreadFilter};
use super::{with_state, RequestHost};
use crate::tools::types::{ThreadComputer, ThreadListQuery, ThreadSummary, ThreadTranscript};

/// As many of the threads a loose reference matches as a refusal names.
const NAMED_CANDIDATES: usize = 5;
/// As many threads or messages as a paired computer hands over in one read.
pub const MAX_REMOTE_ITEMS: usize = 200;

pub struct Selected<'a> {
    pub local: bool,
    pub paired: Vec<&'a PairedComputer>,
}

pub fn computers<'a>(state: &'a WorkspaceState, selected: &str) -> Result<Selected<'a>, String> {
    if selected == "this" {
        return Ok(Selected { local: true, paired: Vec::new() });
    }
    if selected == "all" {
        return Ok(Selected { local: true, paired: state.computers.paired.iter().collect() });
    }
    let by_id: Vec<&PairedComputer> = state.computers.paired.iter().filter(|computer| computer.link.id == selected).take(1).collect();
    let found = if by_id.is_empty() {
        let wanted = selected.trim().to_lowercase();
        state.computers.paired.iter().filter(|computer| computer.link.name.to_lowercase() == wanted).collect()
    } else {
        by_id
    };
    if found.len() != 1 {
        return Err(if found.is_empty() { format!("No paired computer is named {selected}.") } else { format!("More than one computer is named {selected}. Use its computer ID.") });
    }
    Ok(Selected { local: false, paired: found })
}

/// The paired computer a caller names, or `None` for this one. A thread starts on one computer.
pub fn named_computer<'a>(state: &'a WorkspaceState, selected: &str) -> Result<Option<&'a PairedComputer>, String> {
    if selected == "all" {
        return Err("A thread runs on one computer. Use \"this\", or a paired computer's name or ID.".into());
    }
    Ok(computers(state, selected)?.paired.first().copied())
}

pub fn tag(computer: &PairedComputer) -> ThreadComputer {
    ThreadComputer { id: computer.link.id.clone(), name: computer.link.name.clone(), offline: computer.link.status != ComputerStatus::Connected }
}

pub fn online(computer: &PairedComputer, doing: &str) -> Result<(), String> {
    if computer.link.status != ComputerStatus::Connected {
        return Err(format!("{} is {}, so its threads cannot be {doing} right now.", computer.link.name, away_status(computer.link.status)));
    }
    Ok(())
}

/// A paired computer's thread as a listing shows it, or None while that computer's state does not hold it.
pub fn remote_summary(state: &WorkspaceState, computer_id: &str, thread_id: &str) -> Option<ThreadSummary> {
    let computer = state.computers.paired.iter().find(|computer| computer.link.id == computer_id)?;
    let held = computer.state.as_deref()?;
    let thread = held.thread(thread_id)?;
    Some(ThreadSummary { computer: Some(tag(computer)), ..thread_summary(held, thread) })
}

pub fn local_thread_list(state: &WorkspaceState, caller: &str, query: &ThreadListQuery, stored: Option<std::collections::HashSet<String>>) -> Result<Vec<ThreadSummary>, String> {
    let scope = resolve_scope(state, caller, query.project.as_deref())?;
    let filter = ThreadFilter { archived: query.archived.unwrap_or(false), idle_for_ms: query.idle_for_ms, search: query.search.clone(), attachments: query.attachments.unwrap_or(false), limit: query.limit, stored };
    Ok(thread_summaries(state, &scope, &filter, chrono::Utc::now().timestamp_millis()))
}

fn is_thread_summary(value: &Value) -> Option<ThreadSummary> {
    serde_json::from_value(value.clone()).ok()
}

/// Ordinary listings read the mirrored index. A listing across every computer leaves out one it cannot
/// answer for; naming that computer says why.
pub async fn list_across_computers(host: &dyn RequestHost, caller: &str, query: ThreadListQuery, stored: Option<std::collections::HashSet<String>>) -> Result<Vec<ThreadSummary>, String> {
    let selected_name = query.computer.clone().unwrap_or_else(|| "this".into());
    let every = query.computer.as_deref() == Some("all");
    let mut filter = ThreadListQuery { computer: None, ..query.clone() };
    if query.computer.as_deref().is_some_and(|computer| computer != "this") && filter.project.is_none() {
        filter.project = Some("all".into());
    }
    let covers = |state: &WorkspaceState| {
        !every || matches!(filter.project.as_deref(), None | Some("all") | Some("current")) || !matching_projects(&state.projects, filter.project.as_deref().unwrap_or("")).is_empty()
    };
    type Planned = (Vec<ThreadSummary>, Vec<(PairedComputer, Option<Vec<ThreadSummary>>)>);
    let planned: Result<Planned, String> = with_state(host, |state| {
        let selected = computers(state, &selected_name)?;
        if !selected.paired.is_empty() && filter.project.as_deref() == Some("current") {
            return Err("Project \"current\" belongs to this computer. Use computer \"this\", or name a remote project.".into());
        }
        let rows = if selected.local && covers(state) { local_thread_list(state, caller, &filter, stored.clone())? } else { Vec::new() };
        let mut remote = Vec::new();
        for computer in selected.paired {
            let Some(held) = computer.state.as_deref() else {
                if every {
                    continue;
                }
                return Err(format!("{} has not sent its thread list yet. Try again once it is connected.", computer.link.name));
            };
            if !covers(held) {
                continue;
            }
            let searching = filter.search.as_deref().is_some_and(|search| !search.trim().is_empty());
            let mirrored = if searching { None } else { Some(local_thread_list(held, "", &filter, None)?) };
            let link = PairedComputer { link: computer.link.clone(), state: None };
            remote.push((link, mirrored));
        }
        Ok((rows, remote))
    });
    let (mut rows, remote) = planned?;
    for (computer, mirrored) in remote {
        let summaries = match mirrored {
            Some(summaries) => summaries,
            None => {
                if every && computer.link.status != ComputerStatus::Connected {
                    continue;
                }
                online(&computer, "read")?;
                let limit = filter.limit.unwrap_or(20.0).min(MAX_REMOTE_ITEMS as f64);
                let mut asked = serde_json::to_value(&filter).unwrap_or_else(|_| json!({}));
                asked["kind"] = json!("thread-list");
                asked["limit"] = json!(limit.max(0.0) as u64);
                let answered = host.query_computer_threads(computer.link.id.clone(), asked).await.and_then(|result| {
                    let list = result.as_array().filter(|list| list.len() <= MAX_REMOTE_ITEMS).ok_or_else(|| format!("Invalid thread list from {}.", computer.link.name))?;
                    list.iter().map(is_thread_summary).collect::<Option<Vec<_>>>().ok_or_else(|| format!("Invalid thread list from {}.", computer.link.name))
                });
                match answered {
                    Ok(summaries) => summaries,
                    Err(_) if every => continue,
                    Err(error) => return Err(error),
                }
            }
        };
        rows.extend(summaries.into_iter().map(|thread| ThreadSummary { computer: Some(tag(&computer)), ..thread }));
    }
    rows.sort_by(|left, right| right.last_activity_at.cmp(&left.last_activity_at));
    if let Some(limit) = query.limit {
        rows.truncate(limit.max(0.0) as usize);
    }
    Ok(rows)
}

/// The thread a reference names, here or on a paired computer, and which computer holds it.
#[derive(Debug, Clone)]
pub struct ThreadMatch {
    pub thread: Thread,
    pub computer: Option<PairedComputer>,
}

fn ambiguous(reference: &str, candidates: &[(Option<&PairedComputer>, &Thread)]) -> String {
    let places: std::collections::HashSet<Option<&str>> = candidates.iter().map(|(computer, _)| computer.map(|computer| computer.link.id.as_str())).collect();
    let many_places = places.len() > 1;
    let only = if places.len() == 1 { candidates[0].0 } else { None };
    let named: Vec<String> = candidates
        .iter()
        .take(NAMED_CANDIDATES)
        .map(|(computer, thread)| {
            let place = if many_places {
                match computer {
                    Some(computer) => format!(" on {} [{}]", computer.link.name, computer.link.id),
                    None => " on this computer".into(),
                }
            } else {
                String::new()
            };
            format!("{} \"{}\"{place}", thread.id, thread.title)
        })
        .collect();
    let more = candidates.len() - named.len();
    let which = if many_places {
        format!("More than one computer has a thread matching {reference}")
    } else {
        format!("More than one thread{} matches {reference}", only.map(|computer| format!(" on {}", computer.link.name)).unwrap_or_default())
    };
    format!("{which}: {}{}. Use the full thread ID.", named.join(", "), if more > 0 { format!(", and {more} more") } else { String::new() })
}

/// The same resolution runs before loading history, so a remote read never loads local messages. A
/// reference matching more than one thread is refused rather than guessed.
pub fn resolve_thread_read(state: &WorkspaceState, reference: &str, selected: &str) -> Result<ThreadMatch, String> {
    let scope = computers(state, selected)?;
    let mut sources: Vec<(&WorkspaceState, Option<&PairedComputer>)> = Vec::new();
    if scope.local {
        sources.push((state, None));
    }
    for computer in &scope.paired {
        if let Some(held) = computer.state.as_deref() {
            sources.push((held, Some(computer)));
        }
    }
    let ranked: Vec<(usize, Vec<&Thread>, Option<&PairedComputer>)> = sources.iter().map(|(held, computer)| {
        let (rank, threads) = thread_matches(held, reference);
        (rank, threads, *computer)
    }).collect();
    let closest = ranked.iter().map(|(rank, _, _)| *rank).min().unwrap_or(super::projection::MATCH_RANKS);
    let candidates: Vec<(Option<&PairedComputer>, &Thread)> = ranked.iter().filter(|(rank, _, _)| *rank == closest).flat_map(|(_, threads, computer)| threads.iter().map(move |thread| (*computer, *thread))).collect();
    if candidates.len() > 1 {
        return Err(ambiguous(reference, &candidates));
    }
    let Some((computer, thread)) = candidates.first() else {
        let missing: Vec<&str> = scope.paired.iter().filter(|computer| computer.state.is_none() || computer.link.status != ComputerStatus::Connected).map(|computer| computer.link.name.as_str()).collect();
        let note = if missing.is_empty() { String::new() } else { format!(" {} {} not connected, so newer threads there may be missing.", missing.join(", "), if missing.len() == 1 { "is" } else { "are" }) };
        return Err(format!("No thread has the ID {reference}.{note}"));
    };
    Ok(ThreadMatch { thread: (*thread).clone(), computer: computer.map(|computer| PairedComputer { link: computer.link.clone(), state: None }) })
}

/// Resolves across the paired indexes, then asks only the owning computer for the transcript.
pub async fn read_across_computers(host: &dyn RequestHost, reference: &str, limit: Option<f64>, selected: &str) -> Result<ThreadTranscript, String> {
    let found = with_state(host, |state| resolve_thread_read(state, reference, selected))?;
    let Some(computer) = found.computer else {
        let limit = limit.map(|limit| limit.max(0.0) as usize);
        return with_state(host, |state| thread_transcript(state, &found.thread.id, limit)).ok_or_else(|| format!("No thread has the ID {reference}."));
    };
    online(&computer, "read")?;
    let limit = limit.unwrap_or(30.0).min(MAX_REMOTE_ITEMS as f64);
    let result = host.query_computer_threads(computer.link.id.clone(), json!({ "kind": "thread-read", "threadId": found.thread.id, "limit": limit.max(0.0) as u64 })).await?;
    let transcript: ThreadTranscript = serde_json::from_value(result).ok().filter(|transcript: &ThreadTranscript| transcript.thread.id == found.thread.id).ok_or_else(|| format!("Invalid thread transcript from {}.", computer.link.name))?;
    Ok(ThreadTranscript { thread: ThreadSummary { computer: Some(tag(&computer)), ..transcript.thread }, ..transcript })
}
