//! What a coordinator hears of one of its threads, from its own events here or from what a paired
//! computer says it holds: the notes it wakes to and what the user is told.

use crate::domain::coordination::{with_coordination_note, CoordinationNote, CoordinationReport, CoordinationState, Decision};
use crate::domain::run::RunStatus;
use crate::domain::thread::Thread;
use crate::effects::Effect;
use crate::state::WorkspaceState;
use crate::workspace::notices::{announced, coordination_note, decision_headline, decision_note, report_headline, report_note, settled_headline, turn_note, waiting_headline_tool, NoticeReason, RunSurfacing};

/// What a run that is waiting on the user waits on.
#[derive(Clone, Debug, PartialEq)]
pub enum WaitingOn {
    Tool(String),
    Question(String),
}

/// One thing a coordinator's thread did. `run_id` is the run it happened in, whose end it is heard with.
#[derive(Clone, Debug, PartialEq)]
pub enum MemberFact {
    /// It showed up on the paired computer named, which is heard quietly.
    Arrived { computer: String },
    Raised { decision: Decision, run_id: Option<String> },
    Reported { report: CoordinationReport, run_id: Option<String> },
    Found { headline: String },
    Waiting { on: WaitingOn },
    /// Its turn ended; `tell` is how the user is told: why it surfaces and the failure it ended with.
    Ended { status: RunStatus, quiet: bool, tell: Option<(Option<RunSurfacing>, Option<String>)> },
}

pub fn waiting_headline(on: &WaitingOn) -> String {
    match on {
        WaitingOn::Tool(tool) => waiting_headline_tool(tool),
        WaitingOn::Question(question) => question.clone(),
    }
}

/// What a coordinator hears of `member`'s facts, in order.
pub fn heard_facts(state: &WorkspaceState, lead: &Thread, member: &Thread, facts: &[MemberFact], at: i64) -> (Vec<CoordinationNote>, Vec<Effect>) {
    let mut notes = Vec::new();
    let mut effects = Vec::new();
    for fact in facts {
        match fact {
            MemberFact::Arrived { computer } => notes.push(coordination_note(&member.id, format!("\"{}\" started on {computer}.", member.title), at, false, None, true)),
            MemberFact::Raised { decision, run_id } => {
                notes.push(decision_note(member, decision, at, run_id.clone()));
                effects.extend(announced(state, lead, &decision_headline(decision), NoticeReason::Decision));
            }
            MemberFact::Reported { report, run_id } => {
                if report.state == CoordinationState::Working {
                    continue;
                }
                notes.push(report_note(member, report, at, run_id.clone()));
                if report.state != CoordinationState::Done {
                    effects.extend(announced(state, member, &report_headline(report), NoticeReason::Blocked));
                }
            }
            MemberFact::Found { headline } => effects.extend(announced(state, member, headline, NoticeReason::Finding)),
            MemberFact::Waiting { on } => effects.extend(announced(state, member, &waiting_headline(on), NoticeReason::Waiting)),
            MemberFact::Ended { status, quiet, tell } => {
                notes.push(turn_note(member, *status, at, *quiet));
                if let Some((surfacing, failure)) = tell {
                    if let Some(headline) = settled_headline(*surfacing, failure.as_deref()) {
                        let reason = if *surfacing == Some(RunSurfacing::Failed) { NoticeReason::Failed } else { NoticeReason::Finished };
                        effects.extend(announced(state, member, &headline, reason));
                    }
                }
            }
        }
    }
    (notes, effects)
}

/// Its coordinator here hearing `member`'s facts: each note kept for its next wake, and what the user is told.
pub fn with_facts_heard(state: &mut WorkspaceState, lead_id: &str, member_id: &str, facts: &[MemberFact], at: i64) -> Vec<Effect> {
    let (Some(lead), Some(member)) = (state.thread(lead_id), state.thread(member_id)) else { return Vec::new() };
    let (notes, effects) = heard_facts(state, lead, member, facts, at);
    if let Some(lead) = state.thread_mut(lead_id) {
        for note in notes {
            with_coordination_note(lead, note);
        }
    }
    effects
}
