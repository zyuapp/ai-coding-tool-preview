//! The remaining features: engines, composer images, side chats, the review and the terminal routed
//! to the dock, the desktop around the window, automations, and checkouts a thread asks for.

use crate::commands::{AppCommand, TaskSend};
use crate::domain::conversation::MAX_ATTACHMENTS;
use crate::domain::diff::DiffRange;
use crate::domain::engine::{engine_label, engine_needs_attention, engine_update, AgentEngine, EngineAccess, AGENT_ENGINES};
use crate::domain::run::RunStatus;
use crate::domain::toast::{Toast, ToastAction, ToastCommand, ToastTone};
use crate::effects::{Effect, EffectWork};
use crate::events::{CommandResult, WorkspaceEvent};
use crate::reducer::dock::{diff_read_from, reduce_diffs, reduce_diff_event, reduce_terminal, reviews_reread, side_chat_close, side_chat_open, terminal_updated, DiffReset};
use crate::reducer::errors::{APP_FOLDER_ERROR, FILE_FOLDER_ERROR};
use crate::reducer::sending::{prompt_key, reduce_sending};
use crate::reducer::toasts::with_toast;
use crate::reducer::{target_id, Step};
use crate::state::{AgentSettingsReload, AttachmentSendState, WorkspaceState, MAIN_COMPOSER};
use crate::util::{now, random_id};
use crate::workspace::location::thread_file_roots;

/// Asks the engines again when one needs attention and nobody is asking already.
pub fn refreshed_engines(state: &mut WorkspaceState) -> Vec<Effect> {
    if state.engine_checking || !engine_needs_attention(state.engine_status.as_ref()) {
        return Vec::new();
    }
    state.engine_checking = true;
    vec![EffectWork::EngineRead { refresh: Some(true) }.into()]
}

pub fn engine_update_key(engine: AgentEngine, target: &str) -> String {
    format!("engine-update:{}@{target}", engine.as_str())
}

fn toast(tone: ToastTone, title: String, message: Option<String>, subject: Option<String>, persistent: bool) -> Toast {
    Toast { id: 0, tone, title, message, subject, persistent, action: None, remember: None }
}

fn update_offer(state: &WorkspaceState, engine: AgentEngine) -> Option<Toast> {
    let readiness = state.engine_readiness(engine);
    let update = engine_update(&readiness)?;
    let key = engine_update_key(engine, &update.target);
    if state.dismissed_toasts.contains(&key) {
        return None;
    }
    let have = update.version.as_ref().map(|version| format!("You have {version}.")).unwrap_or_default();
    let needed = match &readiness.required {
        Some(required) => format!("This app needs {required} or newer. {have}"),
        None => have,
    };
    let message = if update.command.is_some() { needed.trim().to_string() } else { format!("{needed} Update it the way you installed it.").trim().to_string() };
    Some(Toast {
        id: 0,
        tone: ToastTone::Update,
        title: format!("{} {} is available", engine_label(engine), update.target),
        message: Some(message),
        subject: Some(engine.as_str().into()),
        persistent: true,
        action: update.command.as_ref().map(|_| ToastAction { label: "Upgrade".into(), command: ToastCommand::EngineUpdate { engine } }),
        remember: Some(key),
    })
}

fn start_update(state: &mut WorkspaceState, engine: AgentEngine) -> Vec<Effect> {
    let update = engine_update(&state.engine_readiness(engine));
    let message = match &update {
        Some(update) => match &update.version {
            Some(version) => format!("Moving from {version} to {}.", update.target),
            None => format!("Moving to {}.", update.target),
        },
        None => "Moving to undefined.".into(),
    };
    state.engine_updating = Some(engine);
    state.action_error = None;
    state.action_error_page = None;
    let mut effects: Vec<Effect> = vec![EffectWork::EngineUpdate { engine, computer_id: None }.into()];
    effects.extend(with_toast(state, toast(ToastTone::Progress, format!("Updating {}", engine_label(engine)), Some(message), Some(engine.as_str().into()), true), now()));
    effects
}

fn next_update(state: &mut WorkspaceState) -> Vec<Effect> {
    let queue: Vec<AgentEngine> = state.engine_update_queue.iter().copied().filter(|engine| engine_update(&state.engine_readiness(*engine)).is_some_and(|update| update.command.is_some())).collect();
    let Some((engine, rest)) = queue.split_first() else {
        state.engine_update_queue = queue;
        return Vec::new();
    };
    state.engine_update_queue = rest.to_vec();
    start_update(state, *engine)
}

fn nothing_blocked(state: &WorkspaceState) -> bool {
    AGENT_ENGINES.iter().all(|engine| state.engine_status.as_ref().and_then(|status| status.get(*engine)).is_none_or(|readiness| !crate::domain::engine::engine_is_blocked(*engine, readiness)))
}

fn reduce_engine_command(state: &mut WorkspaceState, command: &AppCommand) -> Step {
    match command {
        AppCommand::EngineReloadSettings => {
            if matches!(state.agent_settings_reload, AgentSettingsReload::Reloading | AgentSettingsReload::Pending) {
                return Step::none();
            }
            if state.agent_settings_reload == AgentSettingsReload::Failed && state.action_error_page == Some(crate::domain::sidebar::SettingsSection::Engines) {
                state.action_error = None;
                state.action_error_page = None;
            }
            state.agent_settings_reload = AgentSettingsReload::Reloading;
            Step::effect(EffectWork::EngineReloadSettings)
        }
        AppCommand::EngineRead { refresh } => {
            if state.engine_checking {
                return Step::none();
            }
            if state.engine_status.is_none() {
                state.engine_status = Some(Default::default());
                state.engine_checking = true;
                return Step::effect(EffectWork::EngineRead { refresh: None });
            }
            if *refresh != Some(true) && !engine_needs_attention(state.engine_status.as_ref()) {
                return Step::none();
            }
            state.engine_checking = true;
            Step::effect(EffectWork::EngineRead { refresh: Some(true) })
        }
        AppCommand::EngineUpdate { engine, .. } => {
            let engine = *engine;
            if !engine_update(&state.engine_readiness(engine)).is_some_and(|update| update.command.is_some()) || state.engine_updating == Some(engine) || state.engine_update_queue.contains(&engine) {
                return Step::none();
            }
            match state.engine_updating {
                None => Step::effects(start_update(state, engine)),
                Some(updating) => {
                    state.engine_update_queue.push(engine);
                    Step::effects(with_toast(state, toast(ToastTone::Progress, format!("Updating {}", engine_label(engine)), Some(format!("Waiting for {} to finish.", engine_label(updating))), Some(engine.as_str().into()), true), now()))
                }
            }
        }
        AppCommand::EngineSignIn { engine } => {
            if state.engine_readiness(*engine).access != EngineAccess::SignedOut {
                return Step::none();
            }
            state.action_error = None;
            Step::effect(EffectWork::EngineSignIn { engine: *engine })
        }
        _ => Step::none(),
    }
}

fn reduce_engine_event(state: &mut WorkspaceState, event: &WorkspaceEvent) -> Step {
    match event {
        WorkspaceEvent::SettingsReloadStatus { status, message } => {
            state.agent_settings_reload = match status {
                crate::events::SettingsReloadStatus::Pending => AgentSettingsReload::Pending,
                crate::events::SettingsReloadStatus::Reloaded => AgentSettingsReload::Reloaded,
                crate::events::SettingsReloadStatus::Failed => AgentSettingsReload::Failed,
            };
            if *status == crate::events::SettingsReloadStatus::Failed {
                state.action_error = Some(message.clone().unwrap_or_else(|| "Could not reload agent settings.".into()));
                state.action_error_page = Some(crate::domain::sidebar::SettingsSection::Engines);
            }
            Step::none()
        }
        WorkspaceEvent::EngineStatus { status } | WorkspaceEvent::EngineUpdated { status, .. } => {
            let mut merged = state.engine_status.clone().unwrap_or_default();
            for engine in AGENT_ENGINES {
                if let Some(readiness) = status.get(engine) {
                    merged.set(engine, Some(readiness.clone()));
                }
            }
            state.engine_status = Some(merged);
            if state.action_error_page == Some(crate::domain::sidebar::SettingsSection::Engines) && nothing_blocked(state) {
                state.action_error = None;
                state.action_error_page = None;
            }
            if let WorkspaceEvent::EngineUpdated { engine, .. } = event {
                state.engine_updating = None;
                let label = engine_label(*engine);
                let readiness = state.engine_readiness(*engine);
                let result = match engine_update(&readiness) {
                    Some(update) => {
                        let run = update.command.map(|command| format!(" Run `{command}` in your terminal.")).unwrap_or_default();
                        toast(ToastTone::Error, format!("Couldn't update {label}"), Some(format!("{label} is still on {}.{run}", update.version.unwrap_or_else(|| "an older version".into()))), Some(engine.as_str().into()), true)
                    }
                    None => toast(ToastTone::Success, format!("{label} updated"), readiness.version.map(|version| format!("Now on {version}.")), Some(engine.as_str().into()), false),
                };
                let mut effects = with_toast(state, result, now());
                effects.extend(next_update(state));
                return Step::effects(effects);
            }
            state.engine_checking = false;
            if state.engine_updates_offered {
                return Step::none();
            }
            state.engine_updates_offered = true;
            let mut effects = Vec::new();
            for engine in AGENT_ENGINES {
                if let Some(offer) = update_offer(state, engine) {
                    effects.extend(with_toast(state, offer, now()));
                }
            }
            Step::effects(effects)
        }
        WorkspaceEvent::EngineFailed { message } => {
            state.engine_checking = false;
            Step::refuse(message.clone())
        }
        WorkspaceEvent::EngineUpdateFailed { engine, message } => {
            state.engine_updating = None;
            let mut effects = with_toast(state, toast(ToastTone::Error, format!("Couldn't update {}", engine_label(*engine)), Some(message.clone()), Some(engine.as_str().into()), true), now());
            effects.extend(next_update(state));
            Step::effects(effects).then(Step::result(CommandResult::failed(message.clone())))
        }
        _ => Step::none(),
    }
}

const MOVED_DRAFT_ERROR: &str = "The images were not sent because you left the new thread. Send them again from there.";

fn set_attachment_send(state: &mut WorkspaceState, task_id: Option<&str>, send: AttachmentSendState) {
    state.attachment_sends.insert(task_id.unwrap_or(MAIN_COMPOSER).to_string(), send);
}

fn attachments_sent(state: &mut WorkspaceState, task_id: Option<String>, attachments: Vec<crate::domain::conversation::RunAttachment>, steer: Option<bool>, text: Option<String>) -> Step {
    let send = TaskSend { task_id, attachments: Some(attachments), steer: steer.filter(|steer| *steer), text: text.filter(|text| !text.is_empty()), ..TaskSend::default() };
    reduce_sending(state, &AppCommand::TaskSend(Box::new(send)), None)
}

/// A checkout asked for or given up by a thread, from its menu or a fork.
pub fn worktree_set(state: &mut WorkspaceState, task_id: &str, worktree: bool) -> Step {
    crate::reducer::worktrees::worktree_set(state, Some(task_id), worktree)
}

/// A thread that asked to move once its run ended goes now, ahead of anything queued behind that run.
pub fn move_after_run_settled(state: &mut WorkspaceState, task_id: &str, status: RunStatus) -> Step {
    crate::reducer::worktrees::move_after_run_settled(state, task_id, status)
}

/// Reviews whose subject is this thread, read again now that its turn wrote under them.
pub fn reread_reviews_of(state: &mut WorkspaceState, task_id: &str, workspace_id: &str) -> Vec<Effect> {
    reviews_reread(state, task_id, workspace_id)
}

/// A review read from a given checkout, as a thread just created from the draft takes it over.
pub fn read_diff_from(state: &mut WorkspaceState, owner: &str, workspace_id: &str, range: &DiffRange) -> Vec<Effect> {
    diff_read_from(state, owner, Some(workspace_id), range, DiffReset::default())
}

/// The user closing one page of the browser panel.
pub fn browser_tab_closed(state: &mut WorkspaceState, owner: &str, tab_id: &str) -> Step {
    let dock = crate::state::dock_for(state, owner).clone();
    let Some(index) = dock.browser_tabs.iter().position(|tab| tab.id == tab_id) else { return Step::none() };
    let tab = if dock.tab == tab_id { crate::state::dock_tab_after_closing(state, owner, tab_id) } else { dock.tab.clone() };
    let tabs: Vec<_> = dock.browser_tabs.iter().filter(|page| page.id != tab_id).cloned().collect();
    let next = if dock.browser_tab_id.as_deref() == Some(tab_id) {
        index.checked_sub(1).and_then(|left| tabs.get(left)).or(tabs.get(index)).map(|page| page.id.clone())
    } else {
        tabs.iter().find(|page| Some(&page.id) == dock.browser_tab_id.as_ref()).map(|page| page.id.clone())
    };
    if state.browser_approval.as_ref().and_then(|approval| approval.tab_id.as_deref()) == Some(tab_id) {
        state.browser_approval = None;
    }
    let held = crate::state::dock_mut(state, owner);
    held.browser_tabs = tabs.clone();
    held.browser_tab_id = next;
    held.tab = tab.clone();
    let mut effects: Vec<Effect> = vec![EffectWork::BrowserClose { tab_id: tab_id.into() }.into()];
    if owner == crate::state::dock_owner(state, None) {
        if tabs.iter().any(|page| page.id == tab) {
            effects.extend(crate::reducer::dock::browser_effects_for_tab(state, owner, &tab));
        } else {
            effects.push(EffectWork::BrowserShow { tab_id: None }.into());
        }
    }
    effects.push(EffectWork::PersistPreferences { preferences: Box::new(crate::workspace::preferences::view_preferences(state)) }.into());
    Step::effects(effects)
}

/// An empty tab in the dock in front, waiting for an address.
pub fn new_browser_tab(state: &mut WorkspaceState) -> Step {
    let owner = crate::state::dock_owner(state, None);
    let tab = crate::domain::platform::BrowserTab { id: random_id(), url: String::new(), title: String::new(), loading: false, can_go_back: false, can_go_forward: false, error: None };
    let tab_id = tab.id.clone();
    let dock = crate::state::dock_mut(state, &owner);
    dock.browser_tabs.push(tab);
    dock.browser_tab_id = Some(tab_id.clone());
    dock.open = true;
    dock.tab = tab_id.clone();
    let mut effects: Vec<Effect> = vec![EffectWork::BrowserOpen { task_id: None, tab_id: tab_id.clone(), url: None }.into(), EffectWork::BrowserShow { tab_id: Some(tab_id.clone()) }.into()];
    effects.extend(crate::reducer::dock::focus_dock_tab(state, &owner, &tab_id));
    Step::effects(effects)
}

pub fn reduce_command(state: &mut WorkspaceState, command: &AppCommand) -> Step {
    match command {
        AppCommand::EngineRead { .. } | AppCommand::EngineSignIn { .. } | AppCommand::EngineUpdate { .. } | AppCommand::EngineReloadSettings => reduce_engine_command(state, command),
        AppCommand::SideChatOpen { chat_id } => side_chat_open(state, chat_id),
        AppCommand::SideChatClose { chat_id } => side_chat_close(state, chat_id),
        AppCommand::DiffToggle { .. }
        | AppCommand::DiffRefresh
        | AppCommand::DiffOpenCommit { .. }
        | AppCommand::DiffSetRange { .. }
        | AppCommand::DiffSetMode { .. }
        | AppCommand::DiffSetCollapsed { .. }
        | AppCommand::DiffSetViewed { .. }
        | AppCommand::DiffSetSplit { .. }
        | AppCommand::DiffSetIgnoreWhitespace { .. }
        | AppCommand::RefreshEnvironment => reduce_diffs(state, command),
        AppCommand::TerminalOpen { .. } | AppCommand::TerminalSelect { .. } | AppCommand::TerminalClose { .. } | AppCommand::TerminalRestart { .. } | AppCommand::TerminalInput { .. } | AppCommand::TerminalResize { .. } => reduce_terminal(state, command),
        AppCommand::AttachmentsSend { task_id, steer, attachments, text } => {
            let key = task_id.clone().unwrap_or_else(|| prompt_key(state));
            let send = state.attachment_send_for(task_id.as_deref());
            if send.busy || state.pending_runs.values().any(|pending| pending.draft_key.as_ref() == Some(&key)) {
                return Step::none();
            }
            if attachments.len() > MAX_ATTACHMENTS {
                let message = format!("You can attach up to {MAX_ATTACHMENTS} images.");
                set_attachment_send(state, task_id.as_deref(), AttachmentSendState { error: Some(message.clone()), ..send });
                return Step::result(CommandResult::failed(message));
            }
            if attachments.is_empty() {
                return attachments_sent(state, task_id.clone(), Vec::new(), *steer, text.clone());
            }
            let draft_key = (task_id.is_none() && text.is_none()).then_some(key);
            set_attachment_send(state, task_id.as_deref(), AttachmentSendState { busy: true, error: None, sent: send.sent, draft_key });
            Step::effect(EffectWork::SendAttachments { task_id: task_id.clone(), steer: steer.filter(|steer| *steer), text: text.clone().filter(|text| !text.is_empty()), attachments: attachments.clone() })
        }
        AppCommand::ImageOpen { source, gallery } => {
            state.viewing_image = Some(source.clone());
            state.viewing_gallery = gallery.clone().filter(|gallery| gallery.contains(source)).unwrap_or_default();
            Step::none()
        }
        AppCommand::ImageStep { delta } => {
            let Some(current) = state.viewing_image.clone() else { return Step::none() };
            let at = state.viewing_gallery.iter().position(|item| item == &current).map(|index| index as i64).unwrap_or(-1) + *delta as i64;
            if let Some(next) = usize::try_from(at).ok().and_then(|at| state.viewing_gallery.get(at)).cloned() {
                state.viewing_image = Some(next);
            }
            Step::none()
        }
        AppCommand::ImageClose => {
            state.viewing_image = None;
            state.viewing_gallery.clear();
            Step::none()
        }
        AppCommand::LinkOpen { url } => Step::effect(EffectWork::OpenLink { url: url.clone() }),
        AppCommand::ImageDownload => {
            state.action_error = None;
            match state.viewing_image.clone() {
                Some(source) => Step::effect(EffectWork::ImageDownload { source }),
                None => Step::none(),
            }
        }
        AppCommand::ViewMounted => {
            state.toasts_held = false;
            let mut effects: Vec<Effect> = vec![EffectWork::ApplyShortcuts { overrides: state.shortcuts.clone() }.into(), EffectWork::ApplyCaptureOptions { options: state.capture_options() }.into()];
            effects.extend(crate::reducer::dock::shown_page_effects(state));
            effects.extend(crate::reducer::toasts::rescheduled_toasts(state, now()));
            Step::effects(effects)
        }
        AppCommand::FileOpen { task_id, path, line } => {
            let thread = state.thread(task_id.as_deref().or(state.current_id.as_deref()).unwrap_or("")).cloned();
            let roots = thread_file_roots(state, thread.as_ref());
            if roots.is_empty() && !path.starts_with('/') {
                return Step::refuse(FILE_FOLDER_ERROR);
            }
            state.action_error = None;
            Step::effect(EffectWork::FileOpen { roots, path: path.clone(), line: *line })
        }
        AppCommand::AppList => Step::effect(EffectWork::AppList),
        AppCommand::AppOpenFolder { app_id } => {
            let Some(root) = crate::workspace::location::current_folder(state) else { return Step::refuse(APP_FOLDER_ERROR) };
            state.action_error = None;
            state.open_menu = None;
            Step::effect(EffectWork::AppOpenFolder { root, app_id: app_id.clone() })
        }
        AppCommand::AppOpenSourceLicenses => Step::effect(EffectWork::AppOpenSourceLicenses),
        AppCommand::CliRead | AppCommand::CliInstall | AppCommand::CliUninstall => {
            let work = match command {
                AppCommand::CliRead => EffectWork::CliRead,
                AppCommand::CliInstall => EffectWork::CliInstall,
                _ => EffectWork::CliUninstall,
            };
            if !matches!(command, AppCommand::CliRead) {
                state.cli.busy = true;
                state.cli.error = None;
            }
            Step::effect(work)
        }
        AppCommand::RemoteSetEnabled { enabled } => {
            state.action_error = None;
            Step::effect(EffectWork::RemoteSetEnabled { enabled: *enabled })
        }
        AppCommand::RemoteCreatePairingCode => {
            state.action_error = None;
            Step::effect(EffectWork::RemoteCreatePairingCode)
        }
        AppCommand::RemoteRevokeDevice { device_id } => {
            state.action_error = None;
            Step::effect(EffectWork::RemoteRevokeDevice { device_id: device_id.clone() })
        }
        AppCommand::RemoteRefresh => {
            state.remote_checking = true;
            Step::effect(EffectWork::RemoteRefresh)
        }
        AppCommand::AutomationSave { task_id, draft } => match target_id(state, task_id.as_deref()) {
            Some(task_id) => Step::effect(EffectWork::AutomationSave { draft: crate::effects::ThreadAutomationDraft { draft: draft.clone(), task_id } }),
            None => Step::none(),
        },
        AppCommand::AutomationUpdate { task_id, patch } => match target_id(state, task_id.as_deref()) {
            Some(task_id) => Step::effect(EffectWork::AutomationUpdate { task_id, patch: patch.clone() }),
            None => Step::none(),
        },
        AppCommand::AutomationDelete { task_id } => match target_id(state, task_id.as_deref()) {
            Some(task_id) => {
                state.automation_edits.shift_remove(&task_id);
                Step::effect(EffectWork::AutomationDelete { task_id })
            }
            None => Step::none(),
        },
        AppCommand::AutomationRunNow { task_id } => match target_id(state, task_id.as_deref()) {
            Some(task_id) => Step::effect(EffectWork::AutomationRunNow { task_id }),
            None => Step::none(),
        },
        AppCommand::EditAutomation { task_id, base, fields } => {
            if fields == base {
                state.automation_edits.shift_remove(task_id);
            } else {
                state.automation_edits.insert(task_id.clone(), crate::domain::automation::AutomationEdit { base: base.clone(), fields: fields.clone() });
            }
            Step::none()
        }
        _ => Step::none(),
    }
}

pub fn reduce_event(state: &mut WorkspaceState, event: &WorkspaceEvent) -> Step {
    match event {
        WorkspaceEvent::SettingsReloadStatus { .. } | WorkspaceEvent::EngineStatus { .. } | WorkspaceEvent::EngineUpdated { .. } | WorkspaceEvent::EngineFailed { .. } | WorkspaceEvent::EngineUpdateFailed { .. } => reduce_engine_event(state, event),
        WorkspaceEvent::DiffLoaded { .. } | WorkspaceEvent::EnvironmentUpdated { .. } => reduce_diff_event(state, event),
        WorkspaceEvent::TerminalUpdated { update } => terminal_updated(state, update),
        WorkspaceEvent::ActionFailed { message, page } => {
            state.action_error_page = *page;
            Step::refuse(message.clone())
        }
        WorkspaceEvent::AttachmentsSaved { task_id, steer, text, ids, attachments } => {
            let send = state.attachment_send_for(task_id.as_deref());
            let left = send.draft_key.clone().filter(|key| *key != prompt_key(state));
            if let Some(left) = &left {
                if state.thread(left).is_none() {
                    set_attachment_send(state, task_id.as_deref(), AttachmentSendState { busy: false, error: Some(MOVED_DRAFT_ERROR.into()), sent: Vec::new(), draft_key: None });
                    return Step::result(CommandResult::failed(MOVED_DRAFT_ERROR));
                }
            }
            let key = task_id.clone().or(send.draft_key.clone()).unwrap_or_else(|| prompt_key(state));
            if let Some(images) = state.images.get_mut(&key) {
                for image in images.iter_mut() {
                    if let Some(at) = ids.iter().position(|id| id == &image.id) {
                        if let Some(written) = attachments.get(at) {
                            image.path = written.path.clone();
                        }
                    }
                }
            }
            set_attachment_send(state, task_id.as_deref(), AttachmentSendState { busy: false, error: None, sent: ids.clone(), draft_key: None });
            attachments_sent(state, left.or(task_id.clone()), attachments.clone(), *steer, text.clone())
        }
        WorkspaceEvent::AttachmentsFailed { task_id, message } => {
            let send = state.attachment_send_for(task_id.as_deref());
            set_attachment_send(state, task_id.as_deref(), AttachmentSendState { busy: false, error: Some(message.clone()), sent: send.sent, draft_key: None });
            Step::result(CommandResult::failed(message.clone()))
        }
        WorkspaceEvent::AppsListed { apps } => {
            state.installed_apps = Some(apps.clone());
            Step::none()
        }
        WorkspaceEvent::CliReadStatus { status } => {
            state.cli.status = Some(status.clone());
            state.cli.busy = false;
            Step::none()
        }
        WorkspaceEvent::CliFailed { message } => {
            state.cli.busy = false;
            state.cli.error = Some(message.clone());
            Step::none()
        }
        WorkspaceEvent::RemoteChanged { remote } => {
            state.remote = (**remote).clone();
            state.remote_checking = false;
            Step::none()
        }
        _ => Step::none(),
    }
}
