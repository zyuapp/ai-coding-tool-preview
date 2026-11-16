//! Each bridge is one run's reach into the window, scoped to the thread it runs as.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use act_core::domain::automation::is_one_shot_schedule;
use act_core::domain::coordination::{is_coordinator, ProposalRequest as CoreProposalRequest, ThreadProposal, MAX_PROPOSALS};
use act_core::domain::platform::{ComputerStatus, SimulatorAvailability, SimulatorDeviceState};
use act_core::domain::thread::{is_news, unread_findings, Thread};
use act_core::domain::thread_role::ThreadRole;
use act_core::domain::worktree::{MoveAfterRun, WorktreeDestination};
use act_core::reducer::errors::{coordinator_ask_refusal, CoordinatorAsk};
use act_core::state::{dock_for, dock_owner, ActiveRun, PairedComputer, RunOrigin};
use act_core::validation::REMOTE_UNSUPPORTED;
use act_core::workspace::activity::thread_activity;
use act_core::workspace::location::{project_for, worktree_by_id, worktree_for};
use act_core::{CommandResult, WorkspaceState};
use futures::future::BoxFuture;
use futures::FutureExt;
use serde_json::{json, Value};

use super::projection::{resolve_scope, thread_summaries, thread_summary, thread_wait_result, ThreadFilter};
use super::reads::{list_across_computers, named_computer, online, read_across_computers, remote_summary, resolve_thread_read, tag, MAX_REMOTE_ITEMS};
use super::starts::{new_thread_send, project_on_computer, thread_proposal, NewThreadStart};
use super::{input, result_message, result_task_id, wait_for, with_state, RequestHost};
use crate::tools::automation::FindingBridge;
use crate::tools::browser::BrowserBridge;
use crate::tools::coordination::CoordinationBridge;
use crate::tools::simulator::SimulatorBridge;
use crate::tools::terminal::TerminalBridge;
use crate::tools::threads::ThreadBridge;
use crate::tools::types::*;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const REMOTE_THREAD_READ_TIMEOUT: Duration = Duration::from_secs(40);
/// Deleting a worktree commits its loose work and removes the folder, which a large checkout drags out.
const WORKTREE_DELETE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Typing a long string or reading a busy screen takes the device a while.
const SIMULATOR_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
/// A wait is answered when the thread settles, so it outlasts an ordinary request.
const WAIT_SLACK: Duration = Duration::from_secs(10);
/// How long a thread just started on a paired computer is given to show up in the state it publishes.
const ARRIVAL: Duration = Duration::from_secs(5);
const DEFAULT_PAGE_TEXT: f64 = 4_000.0;

const MOVE_FOLLOW_UP_ERROR: &str = "Only a thread moving itself passes then. To have another thread go on after its move, message it with message_thread.";
const UNSCHEDULED: &str = "This turn is not a scheduled run, so there is nothing to surface or to silence and nothing was recorded. Say what you found in your reply instead; these two tools are only for a run the automation's schedule started.";
const TAKEN_OVER: &str = "The user joined this run while the report was going in, so it is theirs to answer now and nothing was recorded. Tell them what you found in your reply instead.";
const READ_FROM_NOW: &str = "You and every thread you start from now on read it.";
const SIGNED_SEND: &str = "command:task.send:sender";
const SIGNED_MOVE: &str = "command:task.move-worktree:sender";
const SIGNED_ROLE: &str = "command:task.set-role:sender";

fn proposals_full() -> String {
    format!("At most {MAX_PROPOSALS} suggestions wait at once. Wait for the user to start or dismiss some.")
}

/// One run's bridges into the window, for every server `serve_run` offers it.
#[derive(Clone)]
pub struct RunBridges {
    host: Arc<dyn RequestHost>,
    task_id: String,
}

impl RunBridges {
    pub fn new(host: Arc<dyn RequestHost>, task_id: impl Into<String>) -> Self {
        Self { host, task_id: task_id.into() }
    }

    pub fn threads(&self) -> Arc<dyn ThreadBridge> {
        Arc::new(self.clone())
    }
    pub fn findings(&self) -> Arc<dyn FindingBridge> {
        Arc::new(self.clone())
    }
    pub fn coordination(&self) -> Arc<dyn CoordinationBridge> {
        Arc::new(self.clone())
    }
    pub fn browser(&self) -> Arc<dyn BrowserBridge> {
        Arc::new(self.clone())
    }
    pub fn simulator(&self) -> Arc<dyn SimulatorBridge> {
        Arc::new(self.clone())
    }
    pub fn terminal(&self) -> Arc<dyn TerminalBridge> {
        Arc::new(self.clone())
    }

    fn host(&self) -> &dyn RequestHost {
        self.host.as_ref()
    }
}

/// A lost answer surfaces as a tool error; a hung tool call reports nothing at all.
async fn within<T>(op: &str, limit: Duration, work: impl std::future::Future<Output = Result<T, String>>) -> Result<T, String> {
    match tokio::time::timeout(limit, work).await {
        Ok(result) => result,
        Err(_) => Err(format!("AICodingTool did not answer the thread \"{op}\" request within {}ms.", limit.as_millis())),
    }
}

fn scheduled_run<'a>(state: &'a WorkspaceState, task_id: &str) -> Option<&'a ActiveRun> {
    state.active_runs.get(task_id).filter(|run| run.origin == RunOrigin::Automation)
}

fn is_working(state: &WorkspaceState, task_id: &str) -> bool {
    thread_activity(state).working.contains(task_id)
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn paired<'a>(state: &'a WorkspaceState, id: &str) -> Option<&'a PairedComputer> {
    state.computers.paired.iter().find(|computer| computer.link.id == id)
}

fn link_of(computer: &PairedComputer) -> PairedComputer {
    PairedComputer { link: computer.link.clone(), state: None }
}

impl RunBridges {
    async fn completed(&self, command: Value) -> Result<CommandResult, String> {
        let execution = self.host().execute(input(command)?);
        Ok(execution.completed.await)
    }

    async fn answer_list(&self, query: ThreadListQuery) -> Result<Vec<ThreadSummary>, String> {
        let remote_only = query.computer.as_deref().is_some_and(|computer| computer != "this" && computer != "all");
        let search = query.search.as_deref().map(|search| search.trim().to_lowercase()).filter(|search| !search.is_empty());
        let mut stored = None;
        if let (false, Some(search)) = (remote_only, search) {
            let project = query.project.clone().or_else(|| (query.computer.as_deref() == Some("all")).then(|| "all".to_string()));
            let ids = with_state(self.host(), |state| {
                let Ok(scope) = resolve_scope(state, &self.task_id, project.as_deref()) else { return None };
                let filter = ThreadFilter { archived: query.archived.unwrap_or(false), attachments: query.attachments.unwrap_or(false), idle_for_ms: query.idle_for_ms, ..ThreadFilter::default() };
                let unloaded: HashSet<&str> = state.threads.iter().filter(|thread| thread.history_summary.is_some()).map(|thread| thread.id.as_str()).collect();
                Some(thread_summaries(state, &scope, &filter, chrono::Utc::now().timestamp_millis()).into_iter().filter(|thread| unloaded.contains(thread.id.as_str()) && !thread.title.to_lowercase().contains(&search)).map(|thread| thread.id).collect::<Vec<_>>())
            });
            if let Some(ids) = ids {
                stored = Some(if ids.is_empty() { HashSet::new() } else { self.host().search_stored(search, ids).await });
            }
        }
        list_across_computers(self.host(), &self.task_id, query, stored).await
    }

    async fn hydrate_local(&self, reference: &str, selected: &str) {
        if !matches!(selected, "this" | "all") {
            return;
        }
        let local = with_state(self.host(), |state| resolve_thread_read(state, reference, selected).ok().filter(|found| found.computer.is_none()).map(|found| found.thread.id));
        if let Some(id) = local {
            self.host().hydrate(id).await;
        }
    }

    async fn answer_wait(&self, reference: String, timeout_ms: u64) -> Result<ThreadWaitResult, String> {
        self.hydrate_local(&reference, "all").await;
        let found = with_state(self.host(), |state| resolve_thread_read(state, &reference, "all"))?;
        let thread_id = found.thread.id.clone();
        let timeout = Duration::from_millis(timeout_ms);
        let Some(computer) = found.computer else {
            let watched = thread_id.clone();
            let timed_out = wait_for(self.host(), move |state| is_working(state, &watched), timeout).await;
            let waited = with_state(self.host(), |state| thread_wait_result(state, &thread_id, timed_out)).ok_or_else(|| format!("No thread has the ID {thread_id}."))?;
            if !timed_out && thread_id != self.task_id {
                if let Ok(collected) = input(json!({ "type": "task.result-collected", "taskId": thread_id })) {
                    self.host().dispatch(collected).await;
                }
            }
            return Ok(waited);
        };
        online(&computer, "waited on")?;
        let computer_id = computer.link.id.clone();
        let watched = thread_id.clone();
        let holder_id = computer_id.clone();
        let timed_out = wait_for(
            self.host(),
            move |state| paired(state, &holder_id).is_some_and(|computer| computer.link.status == ComputerStatus::Connected && computer.state.as_deref().is_some_and(|held| is_working(held, &watched))),
            timeout,
        )
        .await;
        let settled = with_state(self.host(), |state| {
            let computer = paired(state, &computer_id)?;
            let waited = computer.state.as_deref().and_then(|held| thread_wait_result(held, &thread_id, timed_out));
            let unloaded = computer.state.as_deref().and_then(|held| held.thread(&thread_id)).is_some_and(|thread| thread.history_summary.is_some());
            Some((link_of(computer), waited, unloaded))
        });
        let Some((computer, waited, unloaded)) = settled else { return Err("That computer is no longer paired.".into()) };
        online(&computer, "waited on")?;
        let Some(mut waited) = waited else { return Err(format!("No thread has the ID {thread_id}.")) };
        if waited.reply.is_none() && unloaded {
            let transcript = read_across_computers(self.host(), &thread_id, Some(MAX_REMOTE_ITEMS as f64), &computer_id).await?;
            waited.reply = transcript.messages.iter().rev().find(|message| message.kind == "assistant").map(|message| message.text.clone());
        }
        waited.thread.computer = Some(tag(&computer));
        Ok(waited)
    }

    async fn answer_command(&self, command: Value, computer: Option<String>) -> Result<ThreadCommandResult, String> {
        act_core::validation::external_command(&command)?;
        let kind = text(&command, "type").unwrap_or_default();
        if kind.starts_with("browser.") || kind.starts_with("simulator.") {
            let result = self.completed(command).await?;
            return if result.is_ok() { Ok(ThreadCommandResult { thread: None, notice: None }) } else { Err(result_message(&result)) };
        }
        if kind == "worktree.delete" {
            let worktree_id = text(&command, "worktreeId").unwrap_or_default();
            let deleting = with_state(self.host(), |state| state.worktree(&worktree_id).is_some_and(|worktree| state.deleting_worktrees.contains(&worktree.root)));
            if deleting {
                return Err("That worktree is already being deleted.".into());
            }
            let result = self.completed(command).await?;
            if !result.is_ok() {
                return Err(result_message(&result));
            }
            let notice = with_state(self.host(), |state| state.worktree_management_notice.clone());
            return Ok(ThreadCommandResult { thread: None, notice });
        }
        let named_task = text(&command, "taskId");
        let caller_id = self.task_id.clone();
        enum Plan {
            Here(Value, bool),
            Elsewhere { computer: Box<PairedComputer>, command: Value, caller: Option<Box<Thread>> },
            Propose { caller: Box<Thread>, lead: Box<Thread>, computer: Box<PairedComputer>, command: Value },
        }
        let plan = with_state(self.host(), |state| -> Result<Plan, String> {
            let target = match &named_task {
                Some(reference) => Some(resolve_thread_read(state, reference, "all")?),
                None => None,
            };
            let caller = state.thread(&caller_id);
            if matches!(kind.as_str(), "task.send" | "task.move-worktree" | "task.dismiss") && named_task.is_none() && caller.is_none() {
                return Err(format!("No thread has the ID {caller_id}."));
            }
            let own_move = kind == "task.move-worktree" && caller.is_some_and(|caller| named_task.is_none() || target.as_ref().is_some_and(|target| target.thread.id == caller.id && target.computer.is_none()));
            if kind == "task.move-worktree" && command.get("then").is_some_and(|then| !then.is_null()) && !own_move {
                return Err(MOVE_FOLLOW_UP_ERROR.into());
            }
            let mut named = command.clone();
            if own_move {
                named["taskId"] = json!(caller_id);
                named["afterRun"] = json!(true);
            } else if kind == "task.dismiss" && named_task.is_none() && caller.is_some() {
                named["taskId"] = json!(caller_id);
            } else if let Some(target) = &target {
                named["taskId"] = json!(target.thread.id);
            }
            let named_id = text(&named, "taskId");
            let remote_subject = target.as_ref().filter(|target| target.computer.is_some()).map(|target| &target.thread);
            let subject = remote_subject.or_else(|| named_id.as_deref().and_then(|id| state.thread(id)));
            if let Some(subject) = subject {
                let creating = remote_subject.is_none() && state.creating_worktrees.contains(&subject.id);
                let refusal = match kind.as_str() {
                    "task.move-worktree" => serde_json::from_value::<WorktreeDestination>(named.get("destination").cloned().unwrap_or(Value::Null)).ok().and_then(|destination| coordinator_ask_refusal(subject, CoordinatorAsk::MoveWorktree(&destination), creating)),
                    "task.set-role" => {
                        let role: Option<ThreadRole> = serde_json::from_value(named.get("role").cloned().unwrap_or(Value::Null)).unwrap_or(None);
                        coordinator_ask_refusal(subject, CoordinatorAsk::SetRole(role), creating)
                    }
                    _ => None,
                };
                if let Some(refusal) = refusal {
                    return Err(refusal.into());
                }
            }
            if kind == "task.send" && named_id.is_none() {
                if let Some(caller) = caller {
                    let asked = match &computer {
                        Some(selected) => Some(named_computer(state, selected)?),
                        None => None,
                    };
                    let send: act_core::commands::TaskSend = serde_json::from_value(named.clone()).map_err(|error| format!("Invalid request: {error}"))?;
                    let start = new_thread_send(state, caller, &send, asked)?;
                    return Ok(match start {
                        NewThreadStart::Here(send) => {
                            let mut value = serde_json::to_value(&*send).map_err(|error| error.to_string())?;
                            value["type"] = json!("task.send");
                            Plan::Here(value, false)
                        }
                        NewThreadStart::Elsewhere { computer, command, lead } => {
                            let mut value = serde_json::to_value(&*command).map_err(|error| error.to_string())?;
                            value["type"] = json!("task.send");
                            match lead {
                                Some(lead) => Plan::Propose { caller: Box::new(caller.clone()), lead: Box::new(lead.clone()), computer: Box::new(computer.clone()), command: value },
                                None => Plan::Elsewhere { computer: Box::new(computer.clone()), command: value, caller: Some(Box::new(caller.clone())) },
                            }
                        }
                    });
                }
            }
            if let Some(target) = target.filter(|target| target.computer.is_some()) {
                let held = paired(state, &target.computer.as_ref().unwrap().link.id).cloned().unwrap_or_else(|| target.computer.clone().unwrap());
                return Ok(Plan::Elsewhere { computer: Box::new(held), command: named, caller: caller.cloned().map(Box::new) });
            }
            if kind == "task.send" && caller.is_some() {
                named["from"] = json!(caller_id);
            }
            Ok(Plan::Here(named, own_move))
        })?;
        match plan {
            Plan::Propose { caller, lead, computer, command } => self.propose_elsewhere(&caller, &lead, &computer, command).await,
            Plan::Elsewhere { computer, command, caller } => self.command_elsewhere(&computer, command, caller.as_deref()).await,
            Plan::Here(signed, own_move) => {
                let named_id = text(&signed, "taskId");
                let result = self.completed(signed).await?;
                if !result.is_ok() {
                    return Err(result_message(&result));
                }
                let task_id = result_task_id(&result).or(named_id);
                Ok(with_state(self.host(), |state| {
                    let thread = task_id.as_deref().and_then(|id| state.thread(id));
                    let notice = thread.filter(|_| own_move).and_then(|thread| state.moves_after_run.get(&thread.id).map(|waiting| moves_after_turn(state, thread, waiting)));
                    ThreadCommandResult { thread: thread.map(|thread| thread_summary(state, thread)), notice }
                }))
            }
        }
    }

    /// A command for a paired computer's thread, or for a new thread there, goes through the reducer,
    /// which carries it to that computer, signed with the thread that sent it.
    async fn command_elsewhere(&self, computer: &PairedComputer, mut command: Value, caller: Option<&Thread>) -> Result<ThreadCommandResult, String> {
        online(computer, "reached")?;
        let Some(held) = computer.state.as_deref() else { return Err(format!("{} has not sent its threads yet. Try again once it is connected.", computer.link.name)) };
        let own_name = with_state(self.host(), |state| state.computers.name.clone());
        let sender = caller.map(|caller| json!({ "threadId": caller.id, "title": caller.title, "computer": if own_name.is_empty() { "another computer".to_string() } else { own_name.clone() } }));
        let capable = |name: &str| computer.link.capabilities.as_ref().is_some_and(|capabilities| capabilities.iter().any(|capability| capability == name));
        let kind = text(&command, "type").unwrap_or_default();
        if kind == "task.send" && sender.is_some() && !capable(SIGNED_SEND) {
            return Err(REMOTE_UNSUPPORTED.into());
        }
        if kind == "task.send" && text(&command, "taskId").is_none() {
            let own = with_state(self.host(), |state| caller.and_then(|caller| caller.project_id.as_deref()).and_then(|id| state.project(id)).cloned());
            let project = project_on_computer(computer, text(&command, "project").as_deref(), own.as_ref())?;
            command["project"] = json!(project.id);
            if let Some(sender) = &sender {
                command["sender"] = sender.clone();
            }
            let result = self.completed(command).await?;
            if !result.is_ok() {
                return Err(result_message(&result));
            }
            let Some(task_id) = result_task_id(&result) else { return Err(format!("{} did not say which thread it started.", computer.link.name)) };
            let computer_id = computer.link.id.clone();
            let arriving = task_id.clone();
            wait_for(self.host(), move |state| remote_summary(state, &computer_id, &arriving).is_none(), ARRIVAL).await;
            let thread = with_state(self.host(), |state| remote_summary(state, &computer.link.id, &task_id));
            return Ok(match thread {
                Some(thread) => ThreadCommandResult { thread: Some(thread), notice: None },
                None => ThreadCommandResult { thread: None, notice: Some(format!("Started thread {task_id} on {}.", computer.link.name)) },
            });
        }
        let _ = held;
        if let Some(sender) = sender {
            let signs = match kind.as_str() {
                "task.send" => true,
                "task.move-worktree" => capable(SIGNED_MOVE),
                "task.set-role" => capable(SIGNED_ROLE),
                _ => false,
            };
            if signs {
                command["sender"] = sender;
            }
        }
        let named = text(&command, "taskId");
        let result = self.completed(command).await?;
        if !result.is_ok() {
            return Err(result_message(&result));
        }
        Ok(ThreadCommandResult { thread: named.and_then(|id| with_state(self.host(), |state| remote_summary(state, &computer.link.id, &id))), notice: None })
    }

    /// A thread asked for on a paired computer by a coordinator or one of its threads waits for the
    /// user to allow it, beside the coordinator's suggestions.
    async fn propose_elsewhere(&self, caller: &Thread, lead: &Thread, computer: &PairedComputer, command: Value) -> Result<ThreadCommandResult, String> {
        let mut asked = command.clone();
        if let Some(fields) = asked.as_object_mut() {
            for key in ["type", "text", "taskId", "attachments", "steer"] {
                fields.remove(key);
            }
            fields.insert("prompt".into(), json!(text(&command, "text").unwrap_or_default()));
            fields.entry("title").or_insert(json!(""));
        }
        let request: CoreProposalRequest = serde_json::from_value(asked).map_err(|error| format!("Invalid request: {error}"))?;
        let title = text(&command, "title");
        let at = chrono::Utc::now().timestamp_millis();
        let proposal = with_state(self.host(), |state| {
            let holder = paired(state, &computer.link.id);
            thread_proposal(state, caller, &request, title.as_deref(), at, Some(holder))
        })?;
        let kept = self.file_proposals(&lead.id, vec![proposal.clone()]).await;
        if !kept.contains(&proposal.id) {
            return Err(proposals_full());
        }
        let folder = computer.state.as_deref().and_then(|held| proposal.place.project.as_deref().and_then(|id| held.project(id))).map(|project| project.root.clone()).unwrap_or_else(|| "its project".into());
        let news = if caller.id == lead.id { "You hear once it starts and when it ends a turn." } else { "Your coordinator hears once it starts." };
        Ok(ThreadCommandResult { thread: None, notice: Some(format!("Waiting for the user to allow it on {} in {folder}. {news}", computer.link.name)) })
    }

    /// Files suggestions on their coordinator and reads back the ones it kept.
    async fn file_proposals(&self, lead_id: &str, proposals: Vec<ThreadProposal>) -> HashSet<String> {
        if !proposals.is_empty() {
            if let Ok(filed) = input(json!({ "type": "coordination.proposed", "taskId": lead_id, "proposals": proposals })) {
                self.host().dispatch(filed).await;
            }
        }
        let held: HashSet<String> = with_state(self.host(), |state| state.thread(lead_id).and_then(|lead| lead.proposals.as_ref()).map(|proposals| proposals.iter().map(|proposal| proposal.id.clone()).collect()).unwrap_or_default());
        proposals.into_iter().filter(|proposal| held.contains(&proposal.id)).map(|proposal| proposal.id).collect()
    }

    async fn raise_finding(&self, report: FindingReport) -> Result<FindingResult, String> {
        let thread_id = self.task_id.clone();
        let before = with_state(self.host(), |state| {
            let thread = state.thread(&thread_id)?;
            scheduled_run(state, &thread_id)?;
            Some((!is_news(thread, report.key.as_deref()), newest_finding(state, &thread_id)))
        });
        let Some((known, newest_before)) = before else { return Ok(FindingResult { recorded: false, note: UNSCHEDULED.into() }) };
        let notify = json!({ "type": "automation.notify", "taskId": thread_id, "headline": report.headline, "detail": report.detail, "key": report.key });
        self.host().dispatch(input(strip_nulls(notify))?).await;
        let (newest, unread) = with_state(self.host(), |state| (newest_finding(state, &thread_id), state.thread(&thread_id).map(|thread| unread_findings(thread).len()).unwrap_or(0)));
        if newest == newest_before {
            if known {
                return Ok(FindingResult { recorded: false, note: format!("This thread already carries a finding keyed \"{}\", so the same one was held back. Raising only what it already knows lets this run settle unseen.", report.key.unwrap_or_default()) });
            }
            return Ok(FindingResult { recorded: false, note: TAKEN_OVER.into() });
        }
        let carried = if unread == 0 { "The user is looking at this thread, so it is already seen".to_string() } else { format!("This thread now carries {unread} unread {}", if unread == 1 { "finding" } else { "findings" }) };
        Ok(FindingResult { recorded: true, note: format!("Raised. {carried}, and the run surfaces when it settles.") })
    }

    async fn report_nothing(&self, checked: String) -> Result<FindingResult, String> {
        let thread_id = self.task_id.clone();
        let active = with_state(self.host(), |state| scheduled_run(state, &thread_id).map(|run| (run.notified, run.quiet)));
        let Some((notified, quiet)) = active else { return Ok(FindingResult { recorded: false, note: UNSCHEDULED.into() }) };
        self.host().dispatch(input(json!({ "type": "automation.nothing-to-report", "taskId": thread_id, "checked": checked }))?).await;
        if notified {
            return Ok(FindingResult { recorded: false, note: "This run already raised something new, so it surfaces anyway and what it found stands.".into() });
        }
        if !quiet {
            let note = with_state(self.host(), |state| loud_run_note(state, &thread_id));
            return Ok(FindingResult { recorded: true, note: format!("Noted. {note}") });
        }
        Ok(FindingResult { recorded: true, note: "Noted. This run settles without reaching the user.".into() })
    }

    /// A thread's own report, decision or memory change, answered with what the window recorded.
    async fn answered(&self, event: Value) -> Result<(bool, Option<String>, Option<String>), String> {
        let execution = self.host().execute(input(event)?);
        let accepted = execution.accepted.await;
        execution.completed.await;
        match accepted {
            CommandResult::Failed { message, .. } => Err(message),
            CommandResult::Ok { answer, .. } => {
                let answer = answer.map(|answer| serde_json::to_value(answer).unwrap_or(Value::Null)).unwrap_or_else(|| json!({ "recorded": true }));
                let recorded = answer.get("recorded").and_then(Value::as_bool).unwrap_or(true);
                Ok((recorded, text(&answer, "noteId"), text(&answer, "reason")))
            }
        }
    }

    async fn report_to_coordinator(&self, state: CoordinationState, summary: String) -> Result<FindingResult, String> {
        let (recorded, _, reason) = self.answered(json!({ "type": "coordination.reported", "taskId": self.task_id, "state": state, "summary": summary })).await?;
        if !recorded {
            return Ok(FindingResult { recorded: false, note: reason.unwrap_or_default() });
        }
        if state == CoordinationState::Working {
            return Ok(FindingResult { recorded: true, note: "Noted. Progress is not passed on; report again when you are blocked, done, or failed.".into() });
        }
        Ok(FindingResult { recorded: true, note: "Reported. Your coordinator hears it when this turn ends.".into() })
    }

    async fn raise_decision(&self, request: DecisionRequest) -> Result<FindingResult, String> {
        let (recorded, _, reason) = self.answered(json!({ "type": "coordination.decision-raised", "taskId": self.task_id, "request": strip_nulls(serde_json::to_value(request).unwrap_or(Value::Null)) })).await?;
        if !recorded {
            return Ok(FindingResult { recorded: false, note: reason.unwrap_or_default() });
        }
        Ok(FindingResult { recorded: true, note: "Raised. The user sees it now, and their answer arrives in this thread as a message. If you cannot go on without it, end your turn.".into() })
    }

    async fn remember_for(&self, change: MemoryChange) -> Result<String, String> {
        let (recorded, note_id, reason) = self.answered(json!({ "type": "coordination.remembered", "taskId": self.task_id, "change": strip_nulls(serde_json::to_value(&change).unwrap_or(Value::Null)) })).await?;
        if !recorded {
            return Err(reason.unwrap_or_default());
        }
        let thread_id = self.task_id.clone();
        let memory = with_state(self.host(), |state| state.thread(&thread_id).and_then(|thread| thread.memory.clone()));
        match change {
            MemoryChange::Purpose { .. } => Ok(if memory.as_ref().is_some_and(|memory| memory.purpose.as_deref().is_some_and(|purpose| !purpose.is_empty())) { format!("Purpose set. {READ_FROM_NOW}") } else { "Purpose cleared.".into() }),
            MemoryChange::Forget { note_id } => Ok(format!("Forgot [{note_id}].")),
            MemoryChange::Remember { text } => {
                let saved = note_id.and_then(|id| memory.and_then(|memory| memory.notes.into_iter().find(|note| note.id == id)));
                match saved {
                    Some(saved) if saved.text == text.trim() => Ok(format!("Saved as [{}]. {READ_FROM_NOW}", saved.id)),
                    _ => Err("The note could not be saved. Try again.".into()),
                }
            }
        }
    }

    /// A coordinator suggesting threads, each checked now as its start would be.
    async fn propose_threads(&self, requests: Vec<Value>) -> Result<String, String> {
        let thread_id = self.task_id.clone();
        let at = chrono::Utc::now().timestamp_millis();
        let checked = with_state(self.host(), |state| {
            let lead = state.thread(&thread_id).filter(|lead| is_coordinator(Some(lead))).ok_or("Only a coordinator suggests threads.")?;
            Ok::<_, String>(
                requests
                    .iter()
                    .map(|request| {
                        let title = text(request, "title").unwrap_or_default();
                        let made = serde_json::from_value::<CoreProposalRequest>(request.clone()).map_err(|error| error.to_string()).and_then(|parsed| {
                            let asked = match &parsed.computer {
                                Some(selected) => Some(named_computer(state, selected)?),
                                None => None,
                            };
                            thread_proposal(state, lead, &parsed, Some(&parsed.title), at, asked)
                        });
                        (title, made)
                    })
                    .collect::<Vec<_>>(),
            )
        })?;
        let filed: Vec<ThreadProposal> = checked.iter().filter_map(|(_, made)| made.as_ref().ok().cloned()).collect();
        let kept = self.file_proposals(&thread_id, filed).await;
        let lines: Vec<String> = checked
            .iter()
            .filter_map(|(title, made)| match made {
                Err(error) => Some(format!("Not suggested \"{title}\": {error}")),
                Ok(proposal) if !kept.contains(&proposal.id) => Some(format!("Not suggested \"{}\": {}", proposal.title, proposals_full())),
                Ok(_) => None,
            })
            .collect();
        if kept.is_empty() {
            return Err(lines.join("\n"));
        }
        let said = format!("Suggested {}. The user starts the ones they want; you hear from each once it ends a turn.", if kept.len() == 1 { "1 thread".to_string() } else { format!("{} threads", kept.len()) });
        Ok(std::iter::once(said).chain(lines).collect::<Vec<_>>().join("\n"))
    }

    async fn read_browser(&self, read: BrowserRead) -> Result<BrowserReadResult, String> {
        let task_id = self.task_id.clone();
        enum Next {
            Done(BrowserReadResult),
            Tab(String, act_core::domain::platform::BrowserPermissions),
        }
        let next = with_state(self.host(), |state| {
            if let Some(approval) = state.browser_approval.as_ref().filter(|approval| approval.task_id == task_id) {
                return Next::Done(BrowserReadResult::AwaitingApproval { url: approval.url.clone() });
            }
            let dock = dock_for(state, &dock_owner(state, Some(&task_id)));
            if matches!(read, BrowserRead::Tabs) {
                let tabs = dock.browser_tabs.iter().filter_map(|tab| serde_json::to_value(tab).ok().and_then(|value| serde_json::from_value(value).ok())).collect();
                return Next::Done(BrowserReadResult::Tabs { tabs });
            }
            let tab_id = match &read {
                BrowserRead::Snapshot { tab_id, .. } | BrowserRead::Screenshot { tab_id, .. } | BrowserRead::Console { tab_id, .. } | BrowserRead::Network { tab_id, .. } | BrowserRead::Wait { tab_id, .. } => tab_id.clone(),
                BrowserRead::Tabs => None,
            };
            match act_core::state::browser_target(dock, tab_id.as_deref()) {
                Some(tab) => Next::Tab(tab.id.clone(), act_core::reducer::browser_permissions(state)),
                None => Next::Done(BrowserReadResult::NoTab),
            }
        });
        let (tab_id, permissions) = match next {
            Next::Done(result) => return Ok(result),
            Next::Tab(tab_id, permissions) => (tab_id, permissions),
        };
        self.host().configure_browser_permissions(permissions).await;
        Ok(match read {
            BrowserRead::Screenshot { full_page, timeout_ms, .. } => match self.host().capture_browser_page(tab_id, full_page == Some(true), timeout_ms, task_id).await {
                Some(shot) => BrowserReadResult::Shot { shot },
                None => BrowserReadResult::NoTab,
            },
            BrowserRead::Snapshot { text_limit, timeout_ms, .. } => match self.host().read_browser_page(tab_id, text_limit.unwrap_or(DEFAULT_PAGE_TEXT), timeout_ms, task_id).await {
                Some(snapshot) => BrowserReadResult::Snapshot { snapshot },
                None => BrowserReadResult::NoTab,
            },
            inspection => self.host().inspect_browser_page(tab_id, inspection, task_id).await.unwrap_or(BrowserReadResult::NoTab),
        })
    }

    async fn read_terminal(&self, read: TerminalRead) -> Result<TerminalReadResult, String> {
        let task_id = self.task_id.clone();
        let found = with_state(self.host(), |state| {
            let dock = dock_for(state, &dock_owner(state, Some(&task_id)));
            match &read {
                TerminalRead::Terminals => Err(dock.terminals.iter().filter_map(|terminal| serde_json::to_value(terminal).ok().and_then(|value| serde_json::from_value(value).ok())).collect::<Vec<TerminalSession>>()),
                TerminalRead::Snapshot { terminal_id, .. } => Ok(act_core::state::terminal_target(dock, terminal_id.as_deref(), Some(&task_id)).and_then(|terminal| serde_json::to_value(terminal).ok()).and_then(|value| serde_json::from_value::<TerminalSession>(value).ok())),
            }
        });
        let terminal = match found {
            Err(terminals) => return Ok(TerminalReadResult::Terminals { terminals }),
            Ok(None) => return Ok(TerminalReadResult::NoTerminal),
            Ok(Some(terminal)) => terminal,
        };
        let (lines, matching) = match read {
            TerminalRead::Snapshot { lines, r#match, .. } => (lines, r#match.filter(|matching| !matching.is_empty())),
            TerminalRead::Terminals => (None, None),
        };
        let lines = match lines {
            None => DEFAULT_TERMINAL_LINES,
            Some(lines) => (lines.floor().max(1.0) as usize).min(MAX_TERMINAL_LINES),
        };
        let Some(text) = self.host().read_terminal(terminal.id.clone(), lines, matching).await else { return Ok(TerminalReadResult::NoTerminal) };
        Ok(TerminalReadResult::Snapshot {
            snapshot: TerminalSnapshot { terminal_id: terminal.id, title: terminal.title, cwd: terminal.cwd, status: terminal.status, exit_code: terminal.exit_code, error: terminal.error, lines: text.lines, omitted: text.omitted, matched: text.matched },
        })
    }

    async fn read_simulator(&self, read: SimulatorRead) -> Result<SimulatorReadResult, String> {
        let task_id = self.task_id.clone();
        let dock_device = |state: &WorkspaceState, task_id: &str| {
            let udid = dock_for(state, &dock_owner(state, Some(task_id))).simulator_id.clone()?;
            state.simulator.devices.iter().find(|device| device.udid == udid).cloned()
        };
        let before = with_state(self.host(), |state| {
            if let SimulatorAvailability::Unsupported { reason } = &state.simulator.availability {
                return Err(reason.clone());
            }
            if state.simulator.devices.is_empty() {
                return Err("This Mac has no iOS simulators. Add one in Xcode.".into());
            }
            dock_device(state, &task_id).ok_or_else(|| "Open the simulator first with simulator_open.".to_string())
        });
        let device = match before {
            Err(reason) => return Ok(SimulatorReadResult::NoSimulator { reason }),
            Ok(device) => device,
        };
        let as_tool = |device: &act_core::domain::platform::SimulatorDevice| -> SimulatorDevice { serde_json::to_value(device).ok().and_then(|value| serde_json::from_value(value).ok()).unwrap_or(SimulatorDevice { udid: device.udid.clone(), name: device.name.clone(), runtime: device.runtime.clone(), state: String::new() }) };
        match read {
            SimulatorRead::Status { timeout_ms } => {
                let watched = task_id.clone();
                wait_for(self.host(), move |state| dock_device(state, &watched).is_some_and(|device| state.simulator.booting.contains(&device.udid)), Duration::from_millis(timeout_ms)).await;
                let settled = with_state(self.host(), |state| dock_device(state, &task_id).map(|device| (state.simulator.errors.get(&device.udid).cloned(), device)));
                let Some((error, device)) = settled else { return Ok(SimulatorReadResult::NoSimulator { reason: "Open the simulator first with simulator_open.".into() }) };
                Ok(SimulatorReadResult::Status { booted: device.state == SimulatorDeviceState::Booted, device: as_tool(&device), error })
            }
            SimulatorRead::Snapshot { screenshot } => {
                if device.state != SimulatorDeviceState::Booted {
                    return Ok(SimulatorReadResult::NoSimulator { reason: format!("{} is not running yet. Call simulator_open and wait for it.", device.name) });
                }
                let (snapshot, shot) = futures::join!(self.host().describe_simulator(device.udid.clone(), device.name.clone()), async {
                    if screenshot {
                        self.host().capture_simulator(device.udid.clone()).await
                    } else {
                        None
                    }
                });
                Ok(SimulatorReadResult::Snapshot { snapshot: snapshot?, shot })
            }
        }
    }

    async fn write_panel(&self, write: Value, limit: Duration) -> Result<(), String> {
        let mut command = write;
        command["taskId"] = json!(self.task_id);
        within("command", limit, async {
            self.answer_command(command, None).await.map(|_| ())
        })
        .await
    }
}

fn strip_nulls(mut value: Value) -> Value {
    if let Some(fields) = value.as_object_mut() {
        fields.retain(|_, field| !field.is_null());
    }
    value
}

fn newest_finding(state: &WorkspaceState, thread_id: &str) -> Option<String> {
    state.thread(thread_id).and_then(|thread| thread.findings.as_ref()).and_then(|findings| findings.last()).map(|finding| finding.id.clone())
}

/// Why a scheduled run surfaces however it ends.
fn loud_run_note(state: &WorkspaceState, thread_id: &str) -> &'static str {
    let automation = state.automations.iter().find(|view| view.automation.task_id == thread_id);
    if automation.is_some_and(|view| is_one_shot_schedule(&view.automation.schedule)) {
        return "A one-time run always surfaces, this one included.";
    }
    if automation.is_some_and(|view| view.automation.surface_when.is_some()) {
        return "The user started this run by hand, so it surfaces however it ends.";
    }
    "This automation has no quiet sentence, so every run of it surfaces, this one included."
}

/// What a thread moving itself is told: where it is until this turn ends, and where its next turn starts.
fn moves_after_turn(state: &WorkspaceState, thread: &Thread, waiting: &MoveAfterRun) -> String {
    let project = project_for(state, Some(thread)).map(|project| project.root.clone());
    let there = match &waiting.destination {
        WorktreeDestination::Worktree { id } => worktree_by_id(state, Some(id)).map(|worktree| worktree.root.clone()),
        WorktreeDestination::New => Some("a new worktree".to_string()),
        WorktreeDestination::Local => project.clone(),
    };
    let here = worktree_for(state, Some(thread)).map(|worktree| worktree.root.clone()).or(project);
    let next = if waiting.then.is_some() { "its next turn starts there on its own with what you passed as then. End this turn now, and do that work there" } else { "its next turn starts there" };
    format!("This thread moves to {} when this turn ends, and {next}. Until then it stays in {}, and file changes already made there stay behind.", there.unwrap_or_else(|| "undefined".into()), here.unwrap_or_else(|| "undefined".into()))
}

impl ThreadBridge for RunBridges {
    fn list(&self, query: ThreadListQuery) -> BoxFuture<'_, Result<Vec<ThreadSummary>, String>> {
        let limit = if query.computer.as_deref().is_some_and(|computer| computer != "this") { REMOTE_THREAD_READ_TIMEOUT } else { REQUEST_TIMEOUT };
        async move { within("list", limit, self.answer_list(query)).await }.boxed()
    }

    fn read(&self, thread_id: String, limit: Option<f64>, computer: Option<String>) -> BoxFuture<'_, Result<ThreadTranscript, String>> {
        let timeout = if computer.as_deref() == Some("this") { REQUEST_TIMEOUT } else { REMOTE_THREAD_READ_TIMEOUT };
        async move {
            within("read", timeout, async {
                let selected = computer.unwrap_or_else(|| "all".into());
                self.hydrate_local(&thread_id, &selected).await;
                read_across_computers(self.host(), &thread_id, limit, &selected).await
            })
            .await
        }
        .boxed()
    }

    fn wait(&self, thread_id: String, timeout_ms: u64) -> BoxFuture<'_, Result<ThreadWaitResult, String>> {
        async move { within("wait", Duration::from_millis(timeout_ms) + WAIT_SLACK, self.answer_wait(thread_id, timeout_ms)).await }.boxed()
    }

    fn command(&self, command: ExternalCommand, computer: Option<String>) -> BoxFuture<'_, Result<ThreadCommandResult, String>> {
        let kind = text(&command, "type").unwrap_or_default();
        let limit = if kind == "worktree.delete" { WORKTREE_DELETE_TIMEOUT } else if kind.starts_with("browser.") { REQUEST_TIMEOUT } else { REMOTE_THREAD_READ_TIMEOUT };
        async move { within("command", limit, self.answer_command(command, computer)).await }.boxed()
    }
}

impl FindingBridge for RunBridges {
    fn notify(&self, report: FindingReport) -> BoxFuture<'_, Result<FindingResult, String>> {
        async move { within("notify", REQUEST_TIMEOUT, self.raise_finding(report)).await }.boxed()
    }

    fn nothing_to_report(&self, checked: String) -> BoxFuture<'_, Result<FindingResult, String>> {
        async move { within("nothing-to-report", REQUEST_TIMEOUT, self.report_nothing(checked)).await }.boxed()
    }
}

impl CoordinationBridge for RunBridges {
    fn report(&self, state: CoordinationState, summary: String) -> BoxFuture<'_, Result<FindingResult, String>> {
        async move { within("report", REQUEST_TIMEOUT, self.report_to_coordinator(state, summary)).await }.boxed()
    }

    fn decide(&self, request: DecisionRequest) -> BoxFuture<'_, Result<FindingResult, String>> {
        async move { within("decision", REQUEST_TIMEOUT, self.raise_decision(request)).await }.boxed()
    }

    fn remember(&self, change: MemoryChange) -> BoxFuture<'_, Result<String, String>> {
        async move { within("memory", REQUEST_TIMEOUT, self.remember_for(change)).await }.boxed()
    }

    fn propose(&self, proposals: Vec<ProposalRequest>) -> BoxFuture<'_, Result<String, String>> {
        async move { within("propose", REQUEST_TIMEOUT, self.propose_threads(proposals)).await }.boxed()
    }
}

impl BrowserBridge for RunBridges {
    fn command(&self, write: BrowserWrite) -> BoxFuture<'_, Result<(), String>> {
        async move { self.write_panel(write, REQUEST_TIMEOUT).await }.boxed()
    }

    fn read(&self, read: BrowserRead) -> BoxFuture<'_, Result<BrowserReadResult, String>> {
        let limit = match &read {
            BrowserRead::Snapshot { timeout_ms, .. } | BrowserRead::Screenshot { timeout_ms, .. } | BrowserRead::Wait { timeout_ms, .. } => Duration::from_millis(*timeout_ms) + WAIT_SLACK,
            _ => REQUEST_TIMEOUT,
        };
        async move { within("browser", limit, self.read_browser(read)).await }.boxed()
    }
}

impl SimulatorBridge for RunBridges {
    fn command(&self, write: SimulatorWrite) -> BoxFuture<'_, Result<(), String>> {
        async move { self.write_panel(write, SIMULATOR_COMMAND_TIMEOUT).await }.boxed()
    }

    fn read(&self, read: SimulatorRead) -> BoxFuture<'_, Result<SimulatorReadResult, String>> {
        let limit = match &read {
            SimulatorRead::Status { timeout_ms } => Duration::from_millis(*timeout_ms) + WAIT_SLACK,
            SimulatorRead::Snapshot { .. } => SIMULATOR_COMMAND_TIMEOUT,
        };
        async move { within("simulator", limit, self.read_simulator(read)).await }.boxed()
    }
}

impl TerminalBridge for RunBridges {
    fn read(&self, read: TerminalRead) -> BoxFuture<'_, Result<TerminalReadResult, String>> {
        async move { within("terminal", REQUEST_TIMEOUT, self.read_terminal(read)).await }.boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread_requests::{Execution, ThreadWaiters};
    use act_core::WorkspaceInput;
    use std::sync::Mutex;

    struct Fake {
        state: Mutex<WorkspaceState>,
        executed: Mutex<Vec<Value>>,
        waiters: ThreadWaiters,
    }

    impl RequestHost for Fake {
        fn read(&self, read: &mut dyn FnMut(&WorkspaceState)) {
            read(&self.state.lock().unwrap());
        }
        fn dispatch(&self, _input: WorkspaceInput) -> BoxFuture<'static, ()> {
            futures::future::ready(()).boxed()
        }
        fn execute(&self, input: WorkspaceInput) -> Execution {
            self.executed.lock().unwrap().push(serde_json::to_value(input).unwrap());
            let result = CommandResult::ok_with_task("t9");
            Execution { accepted: futures::future::ready(result.clone()).boxed(), completed: futures::future::ready(result).boxed() }
        }
        fn waiters(&self) -> &ThreadWaiters {
            &self.waiters
        }
    }

    fn thread(id: &str, title: &str, project: Option<&str>) -> Value {
        let mut thread = json!({ "id": id, "title": title, "executionPolicy": "confirm", "engine": "claude", "model": "opus", "messages": [], "updatedAt": 1 });
        if let Some(project) = project {
            thread["projectId"] = json!(project);
        }
        thread
    }

    fn fake() -> Arc<Fake> {
        let state = WorkspaceState {
            threads: serde_json::from_value(json!([thread("t1", "Caller", Some("p1")), thread("t2", "Fix bug", None), thread("t3", "Fix bug", None)])).unwrap(),
            projects: serde_json::from_value(json!([{ "id": "p1", "root": "/repo" }])).unwrap(),
            ..WorkspaceState::default()
        };
        Arc::new(Fake { state: Mutex::new(state), executed: Mutex::new(Vec::new()), waiters: ThreadWaiters::default() })
    }

    #[tokio::test]
    async fn a_loose_reference_is_refused_and_a_new_thread_starts_where_its_caller_lives() {
        let host = fake();
        let bridges = RunBridges::new(host.clone(), "t1");
        let refused = ThreadBridge::read(&bridges, "fix bug".into(), None, Some("this".into())).await.unwrap_err();
        assert!(refused.starts_with("More than one thread matches fix bug: t2 \"Fix bug\", t3 \"Fix bug\""), "{refused}");
        ThreadBridge::command(&bridges, json!({ "type": "task.send", "text": "Look into it" }), None).await.unwrap();
        let sent = host.executed.lock().unwrap()[0].clone();
        assert_eq!((sent["project"].as_str(), sent["from"].as_str(), sent["model"].as_str()), (Some("p1"), Some("t1"), Some("opus")));
    }

    #[tokio::test]
    async fn a_wait_answers_once_the_thread_stops_working() {
        let host = fake();
        host.state.lock().unwrap().creating_worktrees.push("t2".into());
        let bridges = RunBridges::new(host.clone(), "t1");
        let waiting = tokio::spawn(async move { ThreadBridge::wait(&bridges, "t2".into(), 60_000).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiting.is_finished());
        let state = {
            let mut state = host.state.lock().unwrap();
            state.creating_worktrees.clear();
            state.clone()
        };
        host.waiters.release(&state);
        let waited = tokio::time::timeout(Duration::from_secs(2), waiting).await.unwrap().unwrap().unwrap();
        assert!(!waited.timed_out);
    }
}
