//! What a coordinator here hears of the threads it started on paired computers, and how they are let
//! go once it is gone.

use std::collections::HashSet;

use crate::commands::AppCommand;
use crate::domain::coordination::{ends_as_stopped, is_coordinator, with_coordination_note, with_remote_heard, away_lead_of};
use crate::domain::conversation::{MessageKind, MessageTone};
use crate::domain::platform::ComputerStatus;
use crate::domain::run::RunStatus;
use crate::domain::thread::{last_run_outcome, thread_created_at, HeardWork, Thread, ThreadOutcome};
use crate::domain::usage_limit::resumes_on_its_own;
use crate::effects::{Effect, EffectWork};
use crate::events::WorkspaceInput;
use crate::reducer::settlement::{ends_quietly, StopIntent};
use crate::state::{PairedComputer, RunOperationKind, RunOrigin, RunPhase, WorkspaceState};
use crate::util::{now, Map};
use crate::validation::supports_computer_command;
use crate::workspace::activity::{doing_work, thread_activity};
use crate::workspace::coordination::{coordination_members, remotely_led_threads};
use crate::workspace::member_facts::{heard_facts, MemberFact, WaitingOn};
use crate::workspace::notices::RunSurfacing;
use crate::workspace::outstanding_stops::{catches_start, remote_turn_stop, with_stops_kept};

/// The paired computers as they stood before an input, as far as what a coordinator hears goes.
#[derive(Clone, Debug, Default)]
pub struct PairedBefore {
    /// Whether the input could change the paired computers at all.
    pub changed: bool,
    /// Each paired computer before: its id, its line, and whether it had said what it holds.
    pub computers: Vec<(String, ComputerStatus, bool)>,
    /// The computer whose state the input replaced, with what it held before.
    pub replaced: Option<(String, Option<Box<WorkspaceState>>)>,
    /// Computers the input took off the list, with what they last held.
    pub gone: Vec<PairedComputer>,
}

impl PairedBefore {
    pub fn of(state: &WorkspaceState, changed: bool) -> PairedBefore {
        if !changed {
            return PairedBefore::default();
        }
        PairedBefore { changed, computers: state.computers.paired.iter().map(|computer| (computer.link.id.clone(), computer.link.status, computer.state.is_some())).collect(), replaced: None, gone: Vec::new() }
    }

    fn was(&self, id: &str) -> Option<&(String, ComputerStatus, bool)> {
        self.computers.iter().find(|(held, _, _)| held == id)
    }

    fn replaced_state(&self, id: &str) -> Option<Option<&WorkspaceState>> {
        self.replaced.as_ref().filter(|(held, _)| held == id).map(|(_, prior)| prior.as_deref())
    }
}

/// What a coordinator here hears of its threads on paired computers, and the coordinators that may now be due a wake.
#[derive(Clone, Debug, Default)]
pub struct RemoteNews {
    pub effects: Vec<Effect>,
    pub leads: HashSet<String>,
}

/// The threads a paired computer last said are doing work, whether or not its line is up now.
pub fn last_working_ids(holder: &WorkspaceState) -> HashSet<String> {
    let activity = thread_activity(holder);
    activity.working.iter().filter(|id| doing_work(&activity, id)).cloned().collect()
}

struct Landing {
    lead_id: String,
    proposal_id: String,
    thread_id: String,
    run_ended_at: Option<i64>,
    computer: String,
}

/// What paired computers' states say of the threads coordinators here started there.
pub fn remote_news(before: &PairedBefore, state: &mut WorkspaceState) -> RemoteNews {
    let mut news = RemoteNews::default();
    if !before.changed {
        return news;
    }
    let ids: Vec<String> = state.computers.paired.iter().map(|computer| computer.link.id.clone()).collect();
    for computer_id in ids {
        let Some((members, away)) = members_of(before, state, &computer_id, &mut news.leads) else { continue };
        for member_id in members {
            let Some(planned) = member_news(before, state, &computer_id, &member_id) else { continue };
            news.effects.extend(planned.effects);
            news.leads.extend(planned.wakes);
            if let Some(change) = planned.change {
                news.leads.insert(change.lead_id.clone());
                if let Some(lead) = state.thread_mut(&change.lead_id) {
                    change.stop.held(lead);
                    with_remote_heard(lead, &member_id, change.newest, &change.led);
                    for note in change.notes {
                        with_coordination_note(lead, note);
                    }
                }
            }
            if let Some(landing) = planned.landing {
                news.effects.extend(proposal_landed(state, &landing));
            }
        }
        for thread_id in away {
            let landing = {
                let Some(holder) = state.computers.paired.iter().find(|computer| computer.link.id == computer_id).and_then(|computer| computer.state.as_deref()) else { continue };
                let Some(thread) = holder.thread(&thread_id) else { continue };
                caught_landing(state, thread, &computer_id)
            };
            if let Some(landing) = landing {
                news.effects.extend(proposal_landed(state, &landing));
            }
        }
    }
    for gone in &before.gone {
        let Some(held) = gone.state.as_deref() else { continue };
        let working = last_working_ids(held);
        for thread in remotely_led_threads(&held.threads) {
            if working.contains(&thread.id) {
                if let Some(lead) = &thread.remote_lead {
                    news.leads.insert(lead.thread_id.clone());
                }
            }
        }
    }
    kept_work(before, state, &mut news.leads);
    with_stops_kept(state);
    news
}

/// The members to hear and the away threads to look at on one computer, or None when it said nothing new.
fn members_of(before: &PairedBefore, state: &WorkspaceState, computer_id: &str, leads: &mut HashSet<String>) -> Option<(Vec<String>, Vec<String>)> {
    let computer = state.computers.paired.iter().find(|computer| computer.link.id == computer_id)?;
    let holder = computer.state.as_deref()?;
    let was = before.was(computer_id);
    let replaced = before.replaced_state(computer_id);
    let prior: Option<&WorkspaceState> = match replaced {
        Some(prior) => prior,
        None => was.filter(|(_, _, had)| *had).map(|_| holder),
    };
    let same_line = was.map(|(_, status, _)| *status) == Some(computer.link.status);
    if same_line && prior.is_some_and(|prior| replaced.is_none() || (prior.threads == holder.threads && prior.active_runs == holder.active_runs)) {
        return None;
    }
    let members: Vec<&Thread> = remotely_led_threads(&holder.threads);
    let earlier: Vec<&Thread> = prior.map(|prior| remotely_led_threads(&prior.threads)).unwrap_or_default();
    let away: Vec<String> = holder.threads.iter().filter(|thread| away_lead_of(Some(thread)).is_some() && thread.archived_at.is_none()).map(|thread| thread.id.clone()).collect();
    if members.is_empty() && earlier.is_empty() && away.is_empty() {
        return None;
    }
    let worked_before = prior.map(last_working_ids).unwrap_or_default();
    let kept: HashSet<&str> = members.iter().map(|thread| thread.id.as_str()).collect();
    for thread in earlier {
        if !kept.contains(thread.id.as_str()) && worked_before.contains(&thread.id) {
            if let Some(lead) = &thread.remote_lead {
                leads.insert(lead.thread_id.clone());
            }
        }
    }
    Some((members.iter().map(|thread| thread.id.clone()).collect(), away))
}

struct LeadChange {
    lead_id: String,
    notes: Vec<crate::domain::coordination::CoordinationNote>,
    newest: i64,
    led: HashSet<String>,
    stop: crate::workspace::outstanding_stops::RemoteTurnStop,
}

struct MemberPlan {
    effects: Vec<Effect>,
    change: Option<LeadChange>,
    landing: Option<Landing>,
    /// The coordinator whose wake the thread's work starting or stopping may free or hold.
    wakes: Option<String>,
}

/// A start Stop all caught on its way, showing up under a coordinator that is no longer one, is stopped all the same.
fn caught_landing(state: &WorkspaceState, thread: &Thread, computer: &str) -> Option<Landing> {
    let remote = thread.remote_lead.as_ref()?;
    let proposal_id = remote.proposal_id.clone()?;
    if !catches_start(state.thread(&remote.thread_id), &proposal_id) {
        return None;
    }
    Some(Landing { lead_id: remote.thread_id.clone(), proposal_id, thread_id: thread.id.clone(), run_ended_at: thread.run_ended_at, computer: computer.into() })
}

/// What a coordinator hears of one of its threads on a paired computer since it last heard of it.
fn member_news(before: &PairedBefore, state: &WorkspaceState, computer_id: &str, member_id: &str) -> Option<MemberPlan> {
    let computer = state.computers.paired.iter().find(|computer| computer.link.id == computer_id)?;
    let holder = computer.state.as_deref()?;
    let member = holder.thread(member_id)?;
    let remote = member.remote_lead.as_ref()?;
    let lead = state.thread(&remote.thread_id);
    let Some(lead) = lead.filter(|lead| is_coordinator(Some(lead))) else {
        return Some(MemberPlan { effects: Vec::new(), change: None, landing: caught_landing(state, member, computer_id), wakes: None });
    };
    let prior: Option<&WorkspaceState> = match before.replaced_state(computer_id) {
        Some(prior) => prior,
        None => before.was(computer_id).filter(|(_, _, had)| *had).map(|_| holder),
    };
    let mut leads_changed = false;
    let working = last_working_ids(holder).contains(member_id);
    let worked = prior.map(|prior| last_working_ids(prior).contains(member_id)).unwrap_or(false);
    if working != worked {
        leads_changed = true;
    }
    let heard = lead.remote_heard.as_ref().and_then(|heard| heard.get(member_id)).copied();
    let back = remote.back_at;
    let since = heard.unwrap_or(0).max(back.unwrap_or(0));
    let at = now();
    let mut facts = Vec::new();
    let mut newest = heard.unwrap_or_else(|| thread_created_at(member)).max(back.unwrap_or(0));
    let run = holder.active_runs.get(member_id);
    let ran = prior.and_then(|prior| prior.active_runs.get(member_id));
    if heard.is_none() && back.is_none() {
        facts.push(MemberFact::Arrived { computer: computer.link.name.clone() });
    }
    for decision in crate::domain::coordination::open_decisions(member) {
        if decision.raised_at <= since {
            continue;
        }
        facts.push(MemberFact::Raised { decision: decision.clone(), run_id: run.map(|run| run.run_id.clone()) });
        newest = newest.max(decision.raised_at);
    }
    if let Some(report) = member.report.as_ref().filter(|report| report.at > since && report.state != crate::domain::coordination::CoordinationState::Working) {
        facts.push(MemberFact::Reported { report: report.clone(), run_id: run.map(|run| run.run_id.clone()) });
        newest = newest.max(report.at);
    }
    for finding in member.findings.iter().flatten() {
        if finding.at <= since {
            continue;
        }
        facts.push(MemberFact::Found { headline: finding.headline.clone() });
        newest = newest.max(finding.at);
    }
    let ended_at = member.run_ended_at.filter(|ended| *ended > since);
    let lost = ran.is_some_and(|ran| !ran.quiet && ran.operation != Some(RunOperationKind::Compact)) && ended_at.is_none();
    let ended = (ended_at.is_some() || lost) && run.is_none() && !member.limit_pause.as_ref().is_some_and(|pause| resumes_on_its_own(pause.window));
    let stop = remote_turn_stop(lead, member, computer_id, run, ended, at);
    if ended {
        let ending = last_run_outcome(member);
        let stopped = !stop.answered && stop.stop_all && ends_as_stopped(ending);
        let status = if stopped || ending == Some(ThreadOutcome::Stopped) {
            RunStatus::Cancelled
        } else if ending == Some(ThreadOutcome::Failed) {
            RunStatus::Failed
        } else {
            RunStatus::Succeeded
        };
        let surfacing = match status {
            RunStatus::Failed => Some(RunSurfacing::Failed),
            RunStatus::Succeeded if ran.is_some_and(|ran| ran.origin != RunOrigin::Automation) && !lost => Some(RunSurfacing::Attended),
            _ => None,
        };
        let failure = member.messages.last().filter(|message| message.kind == MessageKind::System && message.tone == Some(MessageTone::Error)).map(|message| message.text.clone());
        let quiet = ends_quietly(status, stop.stop_all.then_some(StopIntent::StopAll), stop.answered, ran.is_some_and(|ran| ran.users_own));
        facts.push(MemberFact::Ended { status, quiet, tell: Some((surfacing, failure)) });
        newest = newest.max(ended_at.unwrap_or(newest));
    }
    if let Some(run) = run.filter(|run| run.origin == RunOrigin::Composer && computer.link.status == ComputerStatus::Connected) {
        let approval = if run.phase == RunPhase::AwaitingApproval { holder.approvals.get(&run.run_id) } else { None };
        if let Some(approval) = approval {
            if ran.is_none_or(|ran| ran.run_id != run.run_id || ran.phase != RunPhase::AwaitingApproval) {
                facts.push(MemberFact::Waiting { on: WaitingOn::Tool(approval.tool_name.clone()) });
            }
        }
        if let Some(question) = run.questions.as_ref().and_then(|questions| questions.first()) {
            let asked_before = ran.is_some_and(|ran| ran.run_id == run.run_id && ran.questions.as_ref().is_some_and(|questions| !questions.is_empty()));
            if !asked_before {
                facts.push(MemberFact::Waiting { on: WaitingOn::Question(question.question.question.clone()) });
            }
        }
    }
    let (notes, effects) = heard_facts(state, lead, member, &facts, at);
    let landing = remote.proposal_id.clone().map(|proposal_id| Landing { lead_id: lead.id.clone(), proposal_id, thread_id: member.id.clone(), run_ended_at: member.run_ended_at, computer: computer_id.into() });
    let wakes = leads_changed.then(|| lead.id.clone());
    if notes.is_empty() && Some(newest) == heard && !stop.spent {
        return Some(MemberPlan { effects, change: None, landing, wakes });
    }
    let mut led: HashSet<String> = coordination_members(state, &lead.id).iter().filter(|member| member.computer.is_some()).map(|member| member.thread.id.clone()).collect();
    if state.computers.paired.iter().any(|computer| computer.state.is_none()) {
        let held: HashSet<&str> = state.computers.paired.iter().flat_map(|computer| computer.state.iter().flat_map(|held| held.threads.iter().map(|thread| thread.id.as_str()))).collect();
        for thread_id in lead.remote_heard.iter().flat_map(|heard| heard.keys()) {
            if !held.contains(thread_id.as_str()) {
                led.insert(thread_id.clone());
            }
        }
    }
    Some(MemberPlan { effects, change: Some(LeadChange { lead_id: lead.id.clone(), notes, newest, led, stop }), landing, wakes })
}

/// A thread a coordinator here allowed on another computer having landed there.
fn proposal_landed(state: &mut WorkspaceState, landing: &Landing) -> Vec<Effect> {
    let computer = state.computers.paired.iter().find(|computer| computer.link.id == landing.computer).map(|computer| (computer.link.id.clone(), computer.link.capabilities.clone()));
    crate::reducer::proposals::landed_step(state, &landing.lead_id, &landing.proposal_id, &landing.thread_id, landing.run_ended_at, computer.as_ref().map(|(id, capabilities)| (id.as_str(), capabilities.as_deref()))).effects
}

/// Each coordinator here keeps which of its threads each paired computer last said was working.
fn kept_work(before: &PairedBefore, state: &mut WorkspaceState, leads: &mut HashSet<String>) {
    let paired: HashSet<String> = state.computers.paired.iter().map(|computer| computer.link.id.clone()).collect();
    let mut fresh: HashSet<String> = HashSet::new();
    let mut said: Map<Vec<(String, HeardWork)>> = Map::new();
    for computer in &state.computers.paired {
        let Some(holder) = computer.state.as_deref() else { continue };
        let is_fresh = match before.replaced_state(&computer.link.id) {
            Some(prior) => prior != Some(holder),
            None => !before.was(&computer.link.id).is_some_and(|(_, _, had)| *had),
        };
        if !is_fresh {
            continue;
        }
        fresh.insert(computer.link.id.clone());
        let working = last_working_ids(holder);
        for thread in remotely_led_threads(&holder.threads) {
            if !working.contains(&thread.id) {
                continue;
            }
            let Some(remote) = &thread.remote_lead else { continue };
            said.entry(remote.thread_id.clone()).or_default().push((thread.id.clone(), HeardWork { computer: computer.link.id.clone(), title: thread.title.clone() }));
        }
    }
    let mut changes: Vec<(String, Option<Map<HeardWork>>)> = Vec::new();
    for lead in &state.threads {
        if lead.remote_working.is_none() && !said.contains_key(&lead.id) {
            continue;
        }
        let held = lead.remote_working.clone().unwrap_or_default();
        let mut working: Vec<(String, HeardWork)> = held.iter().filter(|(_, work)| paired.contains(&work.computer) && !fresh.contains(&work.computer)).map(|(id, work)| (id.clone(), work.clone())).collect();
        if is_coordinator(Some(lead)) {
            working.extend(said.get(&lead.id).cloned().unwrap_or_default());
        }
        let same = working.len() == held.len() && working.iter().all(|(id, work)| held.get(id) == Some(work));
        if same {
            continue;
        }
        changes.push((lead.id.clone(), (!working.is_empty()).then(|| working.into_iter().collect())));
    }
    for (id, working) in changes {
        if let Some(lead) = state.thread_mut(&id) {
            lead.remote_working = working;
        }
        leads.insert(id);
    }
}

/// Each thread on a paired computer working under a thread here that is no longer a live coordinator
/// is told its coordinator is away, and told it is back once it is a coordinator again.
pub fn released(before: &PairedBefore, state: &WorkspaceState, coordinator_flipped: bool) -> Vec<Effect> {
    if !coordinator_flipped && !before.changed {
        return Vec::new();
    }
    let mut effects = Vec::new();
    for computer in &state.computers.paired {
        if computer.link.status != ComputerStatus::Connected {
            continue;
        }
        let Some(holder) = computer.state.as_deref() else { continue };
        if !coordinator_flipped {
            let was_connected = before.was(&computer.link.id).is_some_and(|(_, status, _)| *status == ComputerStatus::Connected);
            let same_threads = match before.replaced_state(&computer.link.id) {
                Some(prior) => prior.is_some_and(|prior| prior.threads == holder.threads),
                None => before.was(&computer.link.id).is_some_and(|(_, _, had)| *had),
            };
            if was_connected && same_threads {
                continue;
            }
        }
        let mut inputs: Vec<WorkspaceInput> = Vec::new();
        let led = remotely_led_threads(&holder.threads).into_iter().chain(holder.threads.iter().map(|thread| &**thread).filter(|thread| away_lead_of(Some(thread)).is_some() && thread.archived_at.is_none()));
        for thread in led {
            inputs.extend(lead_change(state, thread, computer).into_iter().map(WorkspaceInput::from));
        }
        if !inputs.is_empty() {
            effects.push(EffectWork::ComputerForward { id: computer.link.id.clone(), inputs, project_add_request: None, draft: None, proposal: None, silent: true }.into());
        }
    }
    effects
}

fn lead_change(state: &WorkspaceState, thread: &Thread, computer: &PairedComputer) -> Vec<AppCommand> {
    let Some(remote) = &thread.remote_lead else { return Vec::new() };
    let capabilities = computer.link.capabilities.as_deref();
    let release = AppCommand::TaskSetCoordinator { task_id: thread.id.clone(), coordinator_id: None };
    let Some(lead) = state.thread(&remote.thread_id) else {
        return if state.computers.discarded_leads.contains(&remote.thread_id) && supports_computer_command(capabilities, &release) { vec![release] } else { Vec::new() };
    };
    if is_coordinator(Some(lead)) != remote.away {
        return Vec::new();
    }
    let told = AppCommand::TaskSetLeadAway { task_id: thread.id.clone(), away: !remote.away };
    if supports_computer_command(capabilities, &told) {
        return vec![told];
    }
    if !remote.away && supports_computer_command(capabilities, &release) {
        vec![release]
    } else {
        Vec::new()
    }
}
