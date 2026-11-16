//! What a Stop all still answers for after it is pressed: the runs it cancelled here, the session work
//! it stopped, what it stopped on paired computers, and each start it caught on its way to one.

use crate::commands::AppCommand;
use crate::domain::coordination::is_coordinator;
use crate::domain::thread::{CaughtStart, RemoteStop, StopAnswer, Thread};
use crate::domain::usage_limit::resumes_on_its_own;
use crate::effects::Effect;
use crate::reducer::runs::answers_in_time;
use crate::state::{ActiveRun, QuietSession, WorkspaceState};
use crate::util::Map;
use crate::validation::supports_computer_command;
use crate::workspace::coordination::{remotely_led_threads, RemoteWork};

/// A thread under a coordinator on a paired computer with work going there, as Stop all reaches it.
#[derive(Clone, Debug, PartialEq)]
pub struct StopElsewhere {
    pub thread_id: String,
    pub computer: String,
    pub capabilities: Option<Vec<String>>,
    pub run_id: Option<String>,
    pub process_ids: Vec<String>,
    pub limit_pause: Option<crate::domain::usage_limit::LimitPause>,
}

impl From<&RemoteWork<'_>> for StopElsewhere {
    fn from(work: &RemoteWork) -> Self {
        StopElsewhere {
            thread_id: work.thread.id.clone(),
            computer: work.computer.link.id.clone(),
            capabilities: work.computer.link.capabilities.clone(),
            run_id: work.run_id.clone(),
            process_ids: work.process_ids.clone(),
            limit_pause: work.thread.limit_pause.clone(),
        }
    }
}

/// What one Stop all covers under a coordinator.
pub struct StopAllCover {
    pub cancelled: Vec<String>,
    pub hushed: Vec<String>,
    pub elsewhere: Vec<StopElsewhere>,
    /// Each start on its way to a paired computer it catches: proposal id and computer id.
    pub landing: Vec<(String, String)>,
}

/// What stops the work of a coordinator's threads on paired computers: one batch per computer.
fn remote_stops(work: &[StopElsewhere], at: i64) -> (Vec<Effect>, Map<RemoteStop>) {
    let mut batches: Map<Vec<crate::events::WorkspaceInput>> = Map::new();
    let mut stops: Map<RemoteStop> = Map::new();
    for item in work {
        let thread_id = &item.thread_id;
        let capabilities = item.capabilities.as_deref();
        let batch = batches.entry(item.computer.clone()).or_default();
        let cancel = AppCommand::RunCancel { task_id: Some(thread_id.clone()) };
        if let Some(run_id) = &item.run_id {
            if supports_computer_command(capabilities, &cancel) {
                batch.push(cancel.into());
                stops.insert(thread_id.clone(), RemoteStop { computer: item.computer.clone(), run_id: Some(run_id.clone()), answer: None });
            }
        }
        let lifted = AppCommand::LimitCancel { task_id: thread_id.clone() };
        if let Some(pause) = &item.limit_pause {
            if supports_computer_command(capabilities, &lifted) {
                batch.push(lifted.into());
                if resumes_on_its_own(pause.window) && !stops.contains_key(thread_id) {
                    stops.insert(thread_id.clone(), RemoteStop { computer: item.computer.clone(), run_id: None, answer: None });
                }
            }
        }
        for process_id in &item.process_ids {
            batch.push(AppCommand::RunStopProcess { task_id: Some(thread_id.clone()), process_id: process_id.clone() }.into());
        }
        if !item.process_ids.is_empty() {
            let mut stop = stops.get(thread_id).cloned().unwrap_or(RemoteStop { computer: item.computer.clone(), run_id: None, answer: None });
            stop.answer = Some(StopAnswer { at, run_id: None });
            stops.insert(thread_id.clone(), stop);
        }
    }
    let effects = batches.into_iter().filter(|(_, inputs)| !inputs.is_empty()).map(|(id, inputs)| crate::reducer::computers::forward(&id, inputs)).collect();
    (effects, stops)
}

/// A coordinator's Stop all made outstanding: what the coordinator keeps of it, and what the runs here do.
pub fn stopped_all(state: &mut WorkspaceState, lead_id: &str, cover: StopAllCover, at: i64) -> Vec<Effect> {
    let (effects, stops) = remote_stops(&cover.elsewhere, at);
    if !stops.is_empty() || !cover.landing.is_empty() {
        if let Some(lead) = state.thread_mut(lead_id) {
            if !stops.is_empty() {
                let held = lead.remote_stops.get_or_insert_with(Map::new);
                held.extend(stops);
            }
            if !cover.landing.is_empty() {
                let held = lead.caught_starts.get_or_insert_with(Map::new);
                for (id, computer) in &cover.landing {
                    held.insert(id.clone(), CaughtStart { computer: computer.clone(), thread_id: None });
                }
            }
        }
    }
    let going: std::collections::HashSet<String> = state.active_runs.values().map(|run| run.run_id.clone()).collect();
    state.quiet_stops.retain(|run_id| going.contains(run_id));
    state.quiet_stops.extend(cover.cancelled.iter().cloned());
    let hushed: std::collections::HashSet<&String> = cover.hushed.iter().collect();
    let threads: std::collections::HashSet<&str> = state.threads.iter().map(|thread| thread.id.as_str()).collect();
    let kept: Vec<QuietSession> = state.quiet_sessions.iter().filter(|session| !hushed.contains(&session.task_id) && answers_in_time(session.at, at) && threads.contains(session.task_id.as_str())).cloned().collect();
    let mut seen = std::collections::HashSet::new();
    state.quiet_sessions = kept.into_iter().chain(cover.hushed.iter().filter(|id| seen.insert((*id).clone())).map(|task_id| QuietSession { task_id: task_id.clone(), at })).collect();
    effects
}

/// What Stop all makes of a turn of a coordinator's thread on a paired computer.
pub struct RemoteTurnStop {
    pub stop_all: bool,
    pub answered: bool,
    pub spent: bool,
    landed: Option<String>,
    kept: Option<RemoteStop>,
    member_id: String,
}

impl RemoteTurnStop {
    /// The coordinator with what is left of the stop.
    pub fn held(&self, lead: &mut Thread) {
        if let Some(landed) = &self.landed {
            without_caught_start(lead, landed);
        }
        if self.spent {
            with_remote_stop(lead, &self.member_id, self.kept.clone());
        }
    }
}

pub fn remote_turn_stop(lead: &Thread, member: &Thread, computer: &str, run: Option<&ActiveRun>, ended: bool, at: i64) -> RemoteTurnStop {
    let stop = lead.remote_stops.as_ref().and_then(|stops| stops.get(&member.id)).filter(|stop| stop.computer == computer).cloned();
    let answered = ended && stop.as_ref().and_then(|stop| stop.answer.as_ref()).and_then(|answer| answer.run_id.as_ref()).is_some();
    let kept = stop.as_ref().and_then(|stop| kept_stop(stop, run, ended, answered, at));
    let spent = kept != stop;
    let landed = if ended { caught_start_of(lead, member, computer) } else { None };
    RemoteTurnStop { stop_all: landed.is_some() || stop.is_some(), answered, spent, landed, kept, member_id: member.id.clone() }
}

fn kept_stop(stop: &RemoteStop, run: Option<&ActiveRun>, ended: bool, answered: bool, at: i64) -> Option<RemoteStop> {
    let answer = stop.answer.as_ref();
    if let Some(run) = run {
        if Some(&run.run_id) == stop.run_id.as_ref() || Some(&run.run_id) == answer.and_then(|answer| answer.run_id.as_ref()) {
            return Some(stop.clone());
        }
        return match answer {
            Some(answer) if answer.run_id.is_none() && run.agent_initiated && answers_in_time(answer.at, at) => Some(RemoteStop { computer: stop.computer.clone(), run_id: None, answer: Some(StopAnswer { at: answer.at, run_id: Some(run.run_id.clone()) }) }),
            _ => None,
        };
    }
    if !ended {
        return Some(stop.clone());
    }
    match answer {
        Some(answer) if !answered && answer.run_id.is_none() && stop.run_id.is_some() => Some(RemoteStop { computer: stop.computer.clone(), run_id: None, answer: Some(answer.clone()) }),
        _ => None,
    }
}

fn with_remote_stop(lead: &mut Thread, thread_id: &str, stop: Option<RemoteStop>) {
    let mut kept = lead.remote_stops.take().unwrap_or_default();
    kept.shift_remove(thread_id);
    if let Some(stop) = stop {
        kept.insert(thread_id.into(), stop);
    }
    lead.remote_stops = (!kept.is_empty()).then_some(kept);
}

fn caught_start_of(lead: &Thread, member: &Thread, computer: &str) -> Option<String> {
    let proposal_id = member.remote_lead.as_ref().and_then(|lead| lead.proposal_id.as_deref());
    lead.caught_starts.as_ref()?.iter().find(|(id, start)| start.computer == computer && match &start.thread_id {
        None => Some(id.as_str()) == proposal_id,
        Some(thread_id) => thread_id == &member.id,
    }).map(|(id, _)| id.clone())
}

/// What Stop all stopped on paired computers is kept only for a thread that computer still holds under the coordinator.
pub fn with_stops_kept(state: &mut WorkspaceState) {
    type Kept = (String, Option<Map<RemoteStop>>, Option<Map<CaughtStart>>);
    let mut changes: Vec<Kept> = Vec::new();
    for lead in &state.threads {
        if lead.remote_stops.is_none() && lead.caught_starts.is_none() {
            continue;
        }
        let holder_of = |id: &str| state.computers.paired.iter().find(|computer| computer.link.id == id);
        let under = |holder: &WorkspaceState, thread_id: &str| remotely_led_threads(&holder.threads).iter().any(|thread| thread.id == thread_id && thread.remote_lead.as_ref().is_some_and(|remote| remote.thread_id == lead.id));
        let stops: Map<RemoteStop> = lead
            .remote_stops
            .iter()
            .flatten()
            .filter(|(thread_id, stop)| holder_of(&stop.computer).is_some_and(|holder| holder.state.as_deref().is_none_or(|held| under(held, thread_id))))
            .map(|(id, stop)| (id.clone(), stop.clone()))
            .collect();
        let starts: Map<CaughtStart> = lead
            .caught_starts
            .iter()
            .flatten()
            .filter(|(_, start)| {
                holder_of(&start.computer).is_some_and(|holder| match (holder.state.as_deref(), &start.thread_id) {
                    (None, _) | (_, None) => true,
                    (Some(held), Some(thread_id)) => !held.threads.iter().any(|thread| &thread.id == thread_id) || under(held, thread_id),
                })
            })
            .map(|(id, start)| (id.clone(), start.clone()))
            .collect();
        if stops.len() == lead.remote_stops.as_ref().map_or(0, |held| held.len()) && starts.len() == lead.caught_starts.as_ref().map_or(0, |held| held.len()) {
            continue;
        }
        changes.push((lead.id.clone(), (!stops.is_empty()).then_some(stops), (!starts.is_empty()).then_some(starts)));
    }
    for (id, stops, starts) in changes {
        if let Some(lead) = state.thread_mut(&id) {
            lead.remote_stops = stops;
            lead.caught_starts = starts;
        }
    }
}

/// A coordinator that is no longer one lets go of what Stop all stopped under it.
pub fn without_lapsed_stops(state: &mut WorkspaceState) {
    let lapsed: Vec<String> = state.threads.iter().filter(|thread| thread.remote_stops.is_some() && !is_coordinator(Some(thread))).map(|thread| thread.id.clone()).collect();
    for id in lapsed {
        if let Some(thread) = state.thread_mut(&id) {
            thread.remote_stops = None;
        }
    }
}

/// The paired computer a coordinator's start went to, when Stop all caught it on its way there.
pub fn caught_start_computer(lead: Option<&Thread>, proposal_id: &str) -> Option<String> {
    lead?.caught_starts.as_ref()?.get(proposal_id).map(|start| start.computer.clone())
}

/// Whether Stop all caught a coordinator's start elsewhere on its way, and it has yet to be heard landing.
pub fn catches_start(lead: Option<&Thread>, proposal_id: &str) -> bool {
    lead.and_then(|lead| lead.caught_starts.as_ref()).and_then(|starts| starts.get(proposal_id)).is_some_and(|start| start.thread_id.is_none())
}

/// A caught start heard landing as `thread_id`.
pub fn with_start_landed(lead: &mut Thread, proposal_id: &str, thread_id: &str) {
    if !is_coordinator(Some(lead)) {
        without_caught_start(lead, proposal_id);
        return;
    }
    if let Some(start) = lead.caught_starts.as_mut().and_then(|starts| starts.get_mut(proposal_id)) {
        start.thread_id = Some(thread_id.into());
    }
}

/// A coordinator done with a start elsewhere Stop all caught on its way.
pub fn without_caught_start(lead: &mut Thread, proposal_id: &str) {
    let Some(starts) = lead.caught_starts.as_mut() else { return };
    starts.shift_remove(proposal_id);
    if starts.is_empty() {
        lead.caught_starts = None;
    }
}
