//! Paired computers: the links, what each one's workspace now is, and which computer a command goes
//! to. Nothing is decided here about another computer's threads; a command aimed at one is carried
//! to the computer that holds it, and what comes back is its new state.

use std::collections::HashSet;

use crate::commands::{AppCommand, TaskSend};
use crate::domain::coordination::{is_coordinator, remote_lead_of};
use crate::domain::engine::unopenable_engine;
use crate::domain::platform::{offline_message, ComputerLink, ComputerStatus, MAX_COMPUTER_NAME};
use crate::domain::project::{find_project, folder_name, matching_projects, project_name, Project};
use crate::effects::{Effect, EffectWork};
use crate::events::{AttachmentStrip, CommandResult, SentDraft, WorkspaceEvent, WorkspaceInput};
use crate::reducer::composer::retain_drafts;
use crate::reducer::run_queue::text_after_send;
use crate::reducer::sending::prompt_key;
use crate::reducer::Step;
use crate::state::{draining_queued, editable_queued, front_dock, AttachmentSendState, PairedComputer, WorkspaceState, MAIN_COMPOSER, NEW_THREAD_DRAFT, QUEUED_SENT};
use crate::util::js_slice_to;
use crate::validation::{declaration, supports_computer_command, Placement, REMOTE_UNSUPPORTED};
use crate::view::computers::{computer_of_project, computer_of_thread};
use crate::workspace::notices::announced_notice;

pub const PANEL_ELSEWHERE: &str = "The browser panel opens only for threads on this computer.";
pub const ATTACHMENTS_ELSEWHERE: &str = "Files and folders attached by local path cannot be sent to another computer.";
pub const QUEUED_ATTACHMENTS_ELSEWHERE: &str = "A message with images or files cannot be edited from another computer.";
pub const FILES_ELSEWHERE: &str = "That folder is on another computer, so it cannot be opened here.";

/// Inputs on their way to the computer that holds what they are about.
pub fn forward(id: &str, inputs: Vec<WorkspaceInput>) -> Effect {
    EffectWork::ComputerForward { id: id.into(), inputs, project_add_request: None, draft: None, proposal: None, silent: false }.into()
}

/// Where a command goes: this computer's reducer, a paired computer's, or nowhere with a reason.
#[derive(Clone, Debug, PartialEq)]
pub enum InputRoute {
    Local,
    Computer {
        computer: String,
        inputs: Vec<WorkspaceInput>,
        /// Puts the computer on screen.
        select: bool,
        /// Applies the command here as well.
        also: bool,
        draft: Option<SentDraft>,
    },
    Refuse {
        message: String,
        unsupported: bool,
    },
}

fn refuse(message: impl Into<String>) -> InputRoute {
    InputRoute::Refuse { message: message.into(), unsupported: false }
}

fn command_of(input: &WorkspaceInput) -> Option<&AppCommand> {
    match input {
        WorkspaceInput::Command(command) => Some(command),
        WorkspaceInput::Event(_) => None,
    }
}

fn forwarded_route(computer: &PairedComputer, inputs: Vec<WorkspaceInput>, select: bool, also: bool, draft: Option<SentDraft>) -> InputRoute {
    let capabilities = computer.link.capabilities.as_deref();
    if inputs.iter().filter_map(command_of).any(|command| !supports_computer_command(capabilities, command)) {
        return InputRoute::Refuse { message: REMOTE_UNSUPPORTED.into(), unsupported: true };
    }
    InputRoute::Computer { computer: computer.link.id.clone(), inputs, select, also, draft }
}

fn toward(computer: Option<&PairedComputer>, route: impl FnOnce(&PairedComputer) -> InputRoute) -> InputRoute {
    let Some(computer) = computer else { return InputRoute::Local };
    if computer.link.status != ComputerStatus::Connected {
        return refuse(offline_message(&computer.link.name, computer.link.status, computer.link.error.as_deref()));
    }
    route(computer)
}

fn selecting(state: &WorkspaceState, computer: &PairedComputer, inputs: Vec<WorkspaceInput>) -> InputRoute {
    let mut all: Vec<WorkspaceInput> = vec![AppCommand::SetFocused { focused: state.focused }.into()];
    all.extend(inputs);
    forwarded_route(computer, all, true, false, None)
}

fn holds_terminal(state: &WorkspaceState, id: &str) -> bool {
    state.docks.values().any(|dock| dock.terminals.iter().any(|terminal| terminal.id == id))
}

fn holder_of(state: &WorkspaceState, holds: impl Fn(&WorkspaceState) -> bool) -> Option<&PairedComputer> {
    if holds(state) {
        return None;
    }
    state.computers.paired.iter().find(|computer| computer.state.as_deref().is_some_and(&holds))
}

fn computer_of_worktree<'a>(state: &'a WorkspaceState, worktree_id: Option<&str>) -> Option<&'a PairedComputer> {
    let worktree_id = worktree_id?;
    holder_of(state, |held| held.worktrees.iter().any(|worktree| worktree.id == worktree_id))
}

fn computer_of_worktree_root<'a>(state: &'a WorkspaceState, root: Option<&str>) -> Option<&'a PairedComputer> {
    let root = root?;
    holder_of(state, |held| held.worktrees.iter().any(|worktree| worktree.root == root) || held.managed_worktrees.as_ref().is_some_and(|managed| managed.iter().any(|worktree| worktree.root == root)))
}

/// The computer holding the checkout or project a new thread is named into: Some(None) for this computer.
pub fn computer_of_place<'a>(state: &'a WorkspaceState, project: Option<&str>, worktree_id: Option<&str>) -> Option<Option<&'a PairedComputer>> {
    if let Some(worktree_id) = worktree_id {
        if state.worktrees.iter().any(|worktree| worktree.id == worktree_id) {
            return Some(None);
        }
        if let Some(holder) = computer_of_worktree(state, Some(worktree_id)) {
            return Some(Some(holder));
        }
    }
    if let Some(project) = project {
        if state.projects.iter().any(|item| item.id == project) {
            return Some(None);
        }
        if let Some(holder) = computer_of_project(state, Some(project)) {
            return Some(Some(holder));
        }
    }
    None
}

/// The project on a paired computer a new thread starts in.
pub fn project_on_computer(computer: &PairedComputer, named: Option<&str>, own: Option<&Project>) -> Result<Project, String> {
    let projects = computer.state.as_deref().map(|state| state.projects.as_slice()).unwrap_or(&[]);
    let name = &computer.link.name;
    if let Some(named) = named {
        return find_project(projects, named).cloned().map_err(|error| format!("On {name}: {error}"));
    }
    let Some(own) = own else { return Err(format!("Name the project on {name} to start the thread in.")) };
    let at_root = matching_projects(projects, &own.root);
    let matches = if at_root.is_empty() { matching_projects(projects, &folder_name(&own.root)) } else { at_root };
    if matches.len() == 1 {
        return Ok(matches[0].clone());
    }
    Err(format!("{} project on {name} matches {}. Name the project to start the thread in.", if matches.is_empty() { "No" } else { "More than one" }, project_name(own)))
}

const LED_SEND: [&str; 2] = ["command:task.send:lead", "command:task.send:memory"];

/// Why a paired computer cannot take a thread under a coordinator here right now, or None when it can.
pub fn led_start_refusal(computer: Option<&PairedComputer>) -> Option<String> {
    let Some(computer) = computer else { return Some("That computer is no longer paired.".into()) };
    if computer.link.status != ComputerStatus::Connected {
        return Some(offline_message(&computer.link.name, computer.link.status, computer.link.error.as_deref()));
    }
    let takes = computer.link.capabilities.as_ref().is_some_and(|names| LED_SEND.iter().all(|name| names.iter().any(|held| held == name)));
    (!takes).then(|| format!("Update AI Coding Tool on {} to run threads there.", computer.link.name))
}

/// A send as a paired computer's build can take it: a name or an engine it cannot take is left off.
pub fn send_for(computer: &PairedComputer, send: &TaskSend) -> TaskSend {
    let capabilities = computer.link.capabilities.as_deref();
    let takes = |field: TaskSend| supports_computer_command(capabilities, &AppCommand::TaskSend(Box::new(field)));
    let mut rest = send.clone();
    if send.title.is_some() && !takes(TaskSend { title: send.title.clone(), ..TaskSend::default() }) {
        rest.title = None;
    }
    if send.engine.is_some() && !takes(TaskSend { engine: send.engine, ..TaskSend::default() }) {
        rest.engine = None;
    }
    rest
}

/// The paired computer holding a thread's queue, with the thread a command without one means there.
pub fn queue_holder<'a>(state: &'a WorkspaceState, task_id: Option<&str>) -> Option<(&'a PairedComputer, Option<String>)> {
    let computer = match task_id {
        None => state.selected_computer(),
        Some(task_id) => computer_of_thread(state, Some(task_id)),
    }?;
    Some((computer, task_id.map(String::from).or_else(|| computer.state.as_deref().and_then(|held| held.current_id.clone()))))
}

/// A composer send carries the draft this computer holds for the thread, whole.
fn forwarded_send(state: &WorkspaceState, computer: &PairedComputer, command: &AppCommand) -> InputRoute {
    if let AppCommand::TaskSend(send) = command {
        if send.attachments.as_ref().is_some_and(|attachments| !attachments.is_empty()) {
            return refuse(ATTACHMENTS_ELSEWHERE);
        }
    }
    let Some(remote) = computer.state.as_deref() else { return refuse("That computer has not answered yet.") };
    let task_id = command.task_id().map(String::from);
    let typed = matches!(command, AppCommand::TaskSend(send) if send.text.is_some());
    let aimed = task_id.clone().or_else(|| (!typed).then(|| prompt_key(remote)));
    if let Some(engine) = aimed.and_then(|id| remote.threads.iter().find(|thread| thread.id == id)).map(|thread| thread.engine).filter(|engine| !engine.is_known()) {
        return refuse(unopenable_engine(engine.as_str()));
    }
    if let AppCommand::TaskSend(send) = command {
        let mut send = send_for(computer, send);
        send.attachments = None;
        if send.text.is_some() {
            return forwarded_route(computer, vec![AppCommand::TaskSend(Box::new(send)).into()], false, false, None);
        }
    }
    let key = task_id.clone().unwrap_or_else(|| prompt_key(remote));
    if state.files.get(&key).is_some_and(|files| !files.is_empty()) {
        return refuse(ATTACHMENTS_ELSEWHERE);
    }
    let prompt = state.prompts.get(&key).cloned().unwrap_or_default();
    let annotations = state.annotations.get(&key).cloned().unwrap_or_default();
    let pastes = state.pastes.get(&key).cloned().unwrap_or_default();
    let target = (key != NEW_THREAD_DRAFT).then(|| key.clone());
    let outgoing = match command {
        AppCommand::AttachmentsSend { steer, attachments, text, .. } => AppCommand::AttachmentsSend {
            task_id: target.clone(),
            steer: *steer,
            // The forward effect reads each staged image from its path into its source, then drops the path.
            attachments: attachments.clone(),
            text: text.clone(),
        },
        AppCommand::TaskSend(send) => {
            let mut send = send_for(computer, send);
            send.attachments = None;
            send.task_id = target.clone();
            AppCommand::TaskSend(Box::new(send))
        }
        other => other.clone(),
    };
    let inputs: Vec<WorkspaceInput> = vec![
        AppCommand::SetPrompt { task_id: target.clone(), prompt: prompt.clone() }.into(),
        AppCommand::AnnotationRecall { task_id: target.clone(), annotations: annotations.clone() }.into(),
        AppCommand::PasteRecall { task_id: target.clone(), pastes: pastes.clone() }.into(),
        outgoing.into(),
    ];
    let attachments = match command {
        AppCommand::AttachmentsSend { task_id, attachments, .. } => Some(AttachmentStrip { key: task_id.clone().unwrap_or_else(|| MAIN_COMPOSER.into()), ids: attachments.iter().map(|attachment| attachment.id.clone()).collect() }),
        _ => None,
    };
    let draft = SentDraft { key, prompt, pastes, annotations, attachments };
    forwarded_route(computer, inputs, false, false, Some(draft))
}

/// A command as the computer holding its thread can take it.
fn as_holder_takes(holder: &PairedComputer, input: &AppCommand) -> Vec<WorkspaceInput> {
    let capabilities = holder.link.capabilities.as_deref();
    match input {
        AppCommand::CoordinationSetThreadModel { engine: Some(_), .. } if !supports_computer_command(capabilities, input) => {
            let mut named = input.clone();
            if let AppCommand::CoordinationSetThreadModel { engine, .. } = &mut named {
                *engine = None;
            }
            vec![named.into()]
        }
        AppCommand::ProposalStart { proposal_ids: Some(ids), .. } | AppCommand::ProposalDismiss { proposal_ids: Some(ids), .. } if !supports_computer_command(capabilities, input) => ids
            .iter()
            .map(|id| {
                let mut one = input.clone();
                match &mut one {
                    AppCommand::ProposalStart { proposal_ids, proposal_id, .. } | AppCommand::ProposalDismiss { proposal_ids, proposal_id, .. } => {
                        *proposal_ids = None;
                        *proposal_id = Some(id.clone());
                    }
                    _ => {}
                }
                one.into()
            })
            .collect(),
        _ => vec![input.clone().into()],
    }
}

fn thread_holder<'a>(state: &'a WorkspaceState, input: &AppCommand, active: Option<&'a PairedComputer>) -> Option<&'a PairedComputer> {
    match input.task_id() {
        None => active,
        Some(task_id) => computer_of_thread(state, Some(task_id)),
    }
}

fn send_holder<'a>(state: &'a WorkspaceState, send: &TaskSend, active: Option<&'a PairedComputer>) -> Option<&'a PairedComputer> {
    if let Some(task_id) = &send.task_id {
        return computer_of_thread(state, Some(task_id));
    }
    if let Some(placed) = computer_of_place(state, send.project.as_deref(), send.worktree_id.as_deref()) {
        return placed;
    }
    if send.text.is_none() {
        active
    } else {
        None
    }
}

fn named_computer(state: &WorkspaceState, computer_id: Option<&str>, command: AppCommand) -> InputRoute {
    let Some(computer_id) = computer_id.filter(|id| !id.is_empty() && *id != "this") else { return InputRoute::Local };
    let Some(computer) = state.computers.paired.iter().find(|item| item.link.id == computer_id) else { return refuse("That computer is no longer paired.") };
    toward(Some(computer), |holder| forwarded_route(holder, vec![command.into()], false, false, None))
}

fn adding_project(state: &WorkspaceState, computer_id: Option<&str>, root: &str, start: bool) -> InputRoute {
    let Some(computer_id) = computer_id.filter(|id| !id.is_empty() && *id != "this") else { return InputRoute::Local };
    let Some(computer) = state.computers.paired.iter().find(|item| item.link.id == computer_id) else { return refuse("That computer is no longer paired.") };
    toward(Some(computer), |holder| {
        let add = AppCommand::ProjectAdd { root: root.into(), start, computer_id: None };
        if start {
            selecting(state, holder, vec![add.into()])
        } else {
            forwarded_route(holder, vec![add.into()], false, false, None)
        }
    })
}

fn own_route(state: &WorkspaceState, command: &AppCommand, active: Option<&PairedComputer>) -> InputRoute {
    match command {
        AppCommand::TaskNew { project_id, worktree_id } => {
            let holder = computer_of_project(state, project_id.as_deref()).or_else(|| computer_of_worktree(state, worktree_id.as_deref()));
            toward(holder, |holder| selecting(state, holder, vec![command.clone().into()]))
        }
        AppCommand::TaskSend(send) => toward(send_holder(state, send, active), |holder| forwarded_send(state, holder, command)),
        AppCommand::AttachmentsSend { .. } => toward(thread_holder(state, command, active), |holder| forwarded_send(state, holder, command)),
        AppCommand::TaskEditQueued { task_id, message_id } => toward(thread_holder(state, command, active), |holder| {
            let Some(task_id) = task_id.clone().or_else(|| holder.state.as_deref().and_then(|held| held.current_id.clone())) else { return refuse(QUEUED_SENT) };
            let queued = holder.state.as_deref().and_then(|held| held.queued_messages.get(&task_id)).cloned().unwrap_or_default();
            let message = match editable_queued(&queued, message_id) {
                Ok(message) => message.clone(),
                Err(refusal) => return refuse(refusal),
            };
            if holder.state.as_deref().is_some_and(|held| draining_queued(held, &task_id, &message.held.id)) {
                return refuse(QUEUED_SENT);
            }
            if !message.held.attachments.is_empty() || message.held.files.as_ref().is_some_and(|files| !files.is_empty()) {
                return refuse(QUEUED_ATTACHMENTS_ELSEWHERE);
            }
            forwarded_route(holder, vec![AppCommand::TaskDropQueued { task_id: Some(task_id), message_id: message_id.clone(), if_waiting: false }.into()], false, true, None)
        }),
        AppCommand::SetFocused { .. } => match active {
            Some(active) if active.link.status == ComputerStatus::Connected => forwarded_route(active, vec![command.clone().into()], false, true, None),
            _ => InputRoute::Local,
        },
        AppCommand::FileOpen { .. } => {
            if thread_holder(state, command, active).is_some() {
                refuse(FILES_ELSEWHERE)
            } else {
                InputRoute::Local
            }
        }
        AppCommand::AppOpenFolder { .. } => {
            if active.is_some() {
                refuse(FILES_ELSEWHERE)
            } else {
                InputRoute::Local
            }
        }
        AppCommand::WorktreeReveal { root } => {
            if computer_of_worktree_root(state, Some(root)).is_some() {
                refuse(FILES_ELSEWHERE)
            } else {
                InputRoute::Local
            }
        }
        AppCommand::WorktreeMenuOpen { list } => match active {
            Some(active) if *list == crate::state::WorktreeMenuList::Destinations => forwarded_route(active, vec![command.clone().into()], false, true, None),
            _ => InputRoute::Local,
        },
        AppCommand::WorktreeDelete { task_id, root, worktree_id, .. } => {
            let computer = if worktree_id.is_some() {
                computer_of_worktree(state, worktree_id.as_deref())
            } else if root.is_some() {
                computer_of_worktree_root(state, root.as_deref())
            } else if task_id.is_some() {
                computer_of_thread(state, task_id.as_deref())
            } else {
                active
            };
            toward(computer, |holder| forwarded_route(holder, vec![command.clone().into()], false, false, None))
        }
        AppCommand::WorktreeConfirmDelete { root } => {
            let Some(root) = root else { return InputRoute::Local };
            toward(computer_of_worktree_root(state, Some(root)), |holder| {
                if holder.state.as_deref().and_then(|held| held.managed_worktrees.as_ref()).is_some_and(|managed| managed.iter().any(|worktree| &worktree.root == root)) {
                    InputRoute::Local
                } else {
                    forwarded_route(holder, vec![AppCommand::WorktreeRefresh.into()], false, true, None)
                }
            })
        }
        _ => InputRoute::Local,
    }
}

/// Where a command goes, by the placement it declares. Events, and everything once no computer is paired, stay here.
pub fn route_input(state: &WorkspaceState, input: &WorkspaceInput) -> InputRoute {
    let Some(command) = command_of(input) else { return InputRoute::Local };
    match command {
        AppCommand::ProjectAdd { root, start, computer_id } => return adding_project(state, computer_id.as_deref(), root, *start),
        AppCommand::AppUpdate { computer_id } => return named_computer(state, computer_id.as_deref(), AppCommand::AppUpdate { computer_id: None }),
        AppCommand::AppCancelUpdate { computer_id } => return named_computer(state, computer_id.as_deref(), AppCommand::AppCancelUpdate { computer_id: None }),
        AppCommand::EngineUpdate { engine, computer_id } => return named_computer(state, computer_id.as_deref(), AppCommand::EngineUpdate { engine: *engine, computer_id: None }),
        _ => {}
    }
    if state.computers.paired.is_empty() {
        return InputRoute::Local;
    }
    let Some(declared) = declaration(&command.type_name()) else { return InputRoute::Local };
    let active = state.selected_computer();
    match declared.at {
        Placement::Local => InputRoute::Local,
        Placement::Thread => toward(thread_holder(state, command, active), |holder| forwarded_route(holder, as_holder_takes(holder, command), false, false, None)),
        Placement::Panel => toward(thread_holder(state, command, active), |_| refuse(PANEL_ELSEWHERE)),
        Placement::Project => {
            let project_id = serde_json::to_value(command).ok().and_then(|value| value.get("projectId").and_then(|id| id.as_str()).map(String::from));
            toward(computer_of_project(state, project_id.as_deref()), |holder| forwarded_route(holder, vec![command.clone().into()], false, false, None))
        }
        Placement::Terminal => {
            let terminal_id = serde_json::to_value(command).ok().and_then(|value| value.get("terminalId").and_then(|id| id.as_str()).map(String::from)).unwrap_or_default();
            if holds_terminal(state, &terminal_id) {
                return InputRoute::Local;
            }
            match state.computers.paired.iter().find(|computer| computer.state.as_deref().is_some_and(|held| holds_terminal(held, &terminal_id))) {
                Some(holder) => toward(Some(holder), |computer| forwarded_route(computer, vec![command.clone().into()], false, false, None)),
                None => refuse("That terminal is no longer available."),
            }
        }
        Placement::Select => {
            let task_id = command.task_id().map(String::from);
            toward(computer_of_thread(state, task_id.as_deref()), |holder| selecting(state, holder, vec![AppCommand::TaskSelect { task_id: task_id.clone().unwrap_or_default() }.into()]))
        }
        Placement::Own => own_route(state, command, active),
    }
}

/// Whether a command is available on the computer it would go to; only a missing capability hides a control.
pub fn computer_command_available(state: &WorkspaceState, command: &AppCommand) -> bool {
    !matches!(route_input(state, &command.clone().into()), InputRoute::Refuse { unsupported: true, .. })
}

/// Whether a command routed here means the window is leaving the paired computer it was showing.
pub fn leaves_computer(state: &WorkspaceState, input: &WorkspaceInput) -> bool {
    match input {
        WorkspaceInput::Event(WorkspaceEvent::ProjectAdded { start, request, .. }) => *start && (request.is_none() || state.project_add.as_ref().map(|add| add.request) == *request),
        WorkspaceInput::Command(command) => declaration(&command.type_name()).is_some_and(|declared| declared.leaves),
        _ => false,
    }
}

/// A computer whose thread the window has just left is told nobody here is looking any more.
pub fn left_behind(before_active: Option<&str>, state: &WorkspaceState) -> Vec<Effect> {
    match before_active {
        Some(left) if state.computers.active.as_deref() != Some(left) => vec![forward(left, vec![AppCommand::SetFocused { focused: false }.into()])],
        _ => Vec::new(),
    }
}

pub fn forward_effect(computer: &str, inputs: Vec<WorkspaceInput>, draft: Option<SentDraft>) -> Effect {
    EffectWork::ComputerForward { id: computer.into(), inputs, project_add_request: None, draft, proposal: None, silent: false }.into()
}

/// A command on its way to the computer holding its thread. Selecting one of that computer's threads
/// puts it on screen; a send carries the draft typed here, which stays until it is taken.
pub fn forwarded(state: &mut WorkspaceState, computer: &str, inputs: Vec<WorkspaceInput>, select: bool, draft: Option<SentDraft>) -> crate::reducer::Transition {
    let before_active = state.active_computer().map(|computer| computer.link.id.clone());
    if let Some(strip) = draft.as_ref().and_then(|draft| draft.attachments.as_ref()) {
        let send = state.attachment_sends.get(&strip.key).cloned().unwrap_or_default();
        if send.busy {
            return crate::reducer::Transition::default();
        }
        state.attachment_sends.insert(strip.key.clone(), AttachmentSendState { busy: true, error: None, ..send });
    }
    if select {
        state.computers.active = Some(computer.into());
        state.computers.dropped = None;
        state.action_error = None;
    }
    let mut effects = left_behind(before_active.as_deref(), state);
    effects.push(forward_effect(computer, inputs, draft));
    crate::reducer::Transition { effects, result: Some(CommandResult::ok()) }
}

/// The links as the host reports them, each keeping the state this window already holds for it.
fn linked(state: &mut WorkspaceState, links: &[ComputerLink]) -> Vec<PairedComputer> {
    let mut held: Vec<PairedComputer> = std::mem::take(&mut state.computers.paired);
    links
        .iter()
        .map(|link| {
            let known = held.iter().position(|computer| computer.link.id == link.id).map(|index| held.swap_remove(index));
            PairedComputer { link: link.clone(), state: known.and_then(|known| known.state) }
        })
        .collect()
}

/// Which paired computer the conversation shows as the lines move.
fn on_screen(active: Option<&str>, dropped: Option<&str>, paired: &[PairedComputer]) -> (Option<String>, Option<String>) {
    if let Some(selected) = paired.iter().find(|computer| Some(computer.link.id.as_str()) == active) {
        let dock = selected.state.as_deref().map(|held| front_dock(held).1.clone());
        let terminal_visible = dock.is_some_and(|dock| dock.open && dock.terminals.iter().any(|terminal| terminal.id == dock.tab));
        return if selected.link.status == ComputerStatus::Connected || terminal_visible { (Some(selected.link.id.clone()), None) } else { (None, Some(selected.link.id.clone())) };
    }
    if active.is_some() {
        return (None, None);
    }
    match paired.iter().find(|computer| Some(computer.link.id.as_str()) == dropped) {
        None => (None, None),
        Some(returning) if returning.link.status == ComputerStatus::Connected => (Some(returning.link.id.clone()), None),
        Some(returning) => (None, Some(returning.link.id.clone())),
    }
}

fn trimmed_name(name: &str) -> String {
    js_slice_to(name.trim(), MAX_COMPUTER_NAME).to_string()
}

/// A paired computer took the draft a send carried, so this window lets go of it.
fn sent_elsewhere(state: &mut WorkspaceState, draft: &SentDraft) {
    let key = &draft.key;
    let typed = state.prompts.get(key).cloned().unwrap_or_default();
    let kept = text_after_send(&typed, &draft.prompt);
    state.set_prompt(key, &kept);
    let ids: HashSet<String> = draft.attachments.as_ref().map(|strip| strip.ids.iter().cloned().collect()).unwrap_or_default();
    retain_drafts(
        state,
        key,
        |annotation| !draft.annotations.iter().any(|sent| sent.id == annotation.id && sent.note == annotation.note),
        |paste| !draft.pastes.iter().any(|sent| sent.id == paste.id),
        |image| !ids.contains(&image.id),
        |_| true,
    );
    if let Some(strip) = &draft.attachments {
        state.attachment_sends.insert(strip.key.clone(), AttachmentSendState { busy: false, error: None, sent: strip.ids.clone(), draft_key: None });
    }
}

pub fn reduce_computers(state: &mut WorkspaceState, input: &WorkspaceInput) -> Step {
    match input {
        WorkspaceInput::Command(command) => reduce_computer_command(state, command),
        WorkspaceInput::Event(event) => reduce_computer_event(state, event),
    }
}

pub fn reduce_computer_command(state: &mut WorkspaceState, command: &AppCommand) -> Step {
    let computers = &mut state.computers;
    match command {
            AppCommand::ComputersDiscover => {
                computers.searching = true;
                computers.search_error = None;
                let mut effects: Vec<Effect> = vec![EffectWork::ComputerDiscover.into()];
                // Looking again also has each connected computer check for newer versions of itself and its engines.
                for computer in computers.paired.iter().filter(|computer| computer.link.status == ComputerStatus::Connected) {
                    let checks = [AppCommand::AppCheckForUpdates { quiet: true }, AppCommand::EngineRead { refresh: Some(true) }];
                    let inputs: Vec<WorkspaceInput> = checks.into_iter().filter(|check| supports_computer_command(computer.link.capabilities.as_deref(), check)).map(Into::into).collect();
                    if !inputs.is_empty() {
                        effects.push(EffectWork::ComputerForward { id: computer.link.id.clone(), inputs, project_add_request: None, draft: None, proposal: None, silent: true }.into());
                    }
                }
                Step::effects(effects)
            }
            AppCommand::ComputersPair { host, name, code } => {
                computers.pairing = Some(crate::domain::platform::ComputerPairing { host: host.clone(), name: name.clone(), busy: !code.is_empty(), error: None });
                if code.is_empty() {
                    Step::none()
                } else {
                    Step::effect(EffectWork::ComputerPair { host: host.clone(), name: name.clone(), code: code.clone() })
                }
            }
            AppCommand::ComputersCancelPairing => {
                computers.pairing = None;
                Step::none()
            }
            AppCommand::ComputersForget { id } => {
                if computers.active.as_ref() == Some(id) {
                    computers.active = None;
                }
                if computers.dropped.as_ref() == Some(id) {
                    computers.dropped = None;
                }
                if &computers.filter == id {
                    computers.filter = "all".into();
                }
                Step::effect(EffectWork::ComputerForget { id: id.clone() })
            }
            AppCommand::ComputersReconnect { id } => Step::effect(EffectWork::ComputerReconnect { id: id.clone() }),
            AppCommand::ComputersRename { name } => {
                let name = trimmed_name(name);
                if !name.is_empty() {
                    computers.name = name.clone();
                }
                Step::effect(EffectWork::ComputerRename { name })
            }
            AppCommand::ComputersLabel { id, name } => {
                if !computers.paired.iter().any(|computer| &computer.link.id == id) {
                    return Step::none();
                }
                let name = trimmed_name(name);
                if !name.is_empty() {
                    for computer in computers.paired.iter_mut().filter(|computer| &computer.link.id == id) {
                        computer.link.name = name.clone();
                    }
                }
                Step::effect(EffectWork::ComputerLabel { id: id.clone(), name })
            }
            AppCommand::ComputersFilter { filter } => {
                computers.filter = filter.clone();
                Step::none()
            }
            _ => Step::none(),
    }
}

pub fn reduce_computer_event(state: &mut WorkspaceState, event: &WorkspaceEvent) -> Step {
    let computers = &mut state.computers;
    match event {
            WorkspaceEvent::ComputersFound { found } => {
                computers.found = found.clone();
                computers.searching = false;
                computers.search_error = None;
                Step::none()
            }
            WorkspaceEvent::ComputersSearchFailed { message } => {
                computers.searching = false;
                computers.search_error = Some(message.clone());
                Step::none()
            }
            WorkspaceEvent::ComputersPairFailed { message } => {
                if let Some(pairing) = computers.pairing.as_mut() {
                    pairing.busy = false;
                    pairing.error = Some(message.clone());
                }
                Step::none()
            }
            WorkspaceEvent::ComputersForwarded { draft } => {
                sent_elsewhere(state, draft);
                Step::none()
            }
            WorkspaceEvent::ComputersAnswered { task_id } => Step::result(CommandResult::ok_with_task(task_id.clone())),
            WorkspaceEvent::ComputersChanged { name, links } => {
                let before: Vec<(String, ComputerStatus)> = state.computers.paired.iter().map(|computer| (computer.link.id.clone(), computer.link.status)).collect();
                let was_active = state.computers.active.clone();
                let paired = linked(state, links);
                let computers = &mut state.computers;
                if computers.pairing.as_ref().is_some_and(|pairing| paired.iter().any(|computer| computer.link.host == pairing.host)) {
                    computers.pairing = None;
                }
                let (active, dropped) = on_screen(computers.active.as_deref(), computers.dropped.as_deref(), &paired);
                if !(computers.filter == "all" || computers.filter == "this" || paired.iter().any(|computer| computer.link.id == computers.filter)) {
                    computers.filter = "all".into();
                }
                computers.name = name.clone();
                computers.paired = paired;
                computers.active = active.clone();
                computers.dropped = dropped;
                let looking = match state.computers.paired.iter().find(|computer| Some(&computer.link.id) == active.as_ref()) {
                    Some(shown) if shown.link.status == ComputerStatus::Connected => {
                        let was_connected = before.iter().any(|(id, status)| id == &shown.link.id && *status == ComputerStatus::Connected);
                        if was_connected && was_active.as_ref() == Some(&shown.link.id) {
                            Vec::new()
                        } else {
                            vec![forward(&shown.link.id, vec![AppCommand::SetFocused { focused: state.focused }.into()])]
                        }
                    }
                    _ => Vec::new(),
                };
                Step::effects(looking)
            }
            WorkspaceEvent::ComputerState { id, state: held } => {
                if let Some(computer) = computers.paired.iter_mut().find(|item| &item.link.id == id) {
                    computer.state = Some(held.clone());
                }
                Step::none()
            }
            WorkspaceEvent::ComputerNotice { id, notice } => {
                let Some(computer) = computers.paired.iter().find(|item| &item.link.id == id) else { return Step::none() };
                let lead = remote_lead_of(computer.state.as_deref().and_then(|held| held.thread(&notice.task_id))).map(|lead| lead.thread_id.clone());
                if lead.is_some_and(|lead| is_coordinator(state.thread(&lead))) {
                    return Step::none();
                }
                Step::effects(announced_notice(state, notice.clone()))
            }
            _ => Step::none(),
    }
}

/// Whether an input belongs to the paired computers' own reducer.
pub fn is_computer_input(input: &WorkspaceInput) -> bool {
    match input {
        WorkspaceInput::Command(command) => command.type_name().starts_with("computers."),
        WorkspaceInput::Event(event) => matches!(
            event,
            WorkspaceEvent::ComputersChanged { .. }
                | WorkspaceEvent::ComputersFound { .. }
                | WorkspaceEvent::ComputersSearchFailed { .. }
                | WorkspaceEvent::ComputersPairFailed { .. }
                | WorkspaceEvent::ComputerState { .. }
                | WorkspaceEvent::ComputerNotice { .. }
                | WorkspaceEvent::ComputersForwarded { .. }
                | WorkspaceEvent::ComputersAnswered { .. }
        ),
    }
}
