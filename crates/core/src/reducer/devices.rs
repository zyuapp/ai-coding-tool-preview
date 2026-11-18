//! What the machine around the window offers: the browser panel and its sign-in import, the
//! Simulator panel, dictation, computer use, plan limits and app updates.

use std::collections::HashSet;

use crate::commands::AppCommand;
use crate::domain::engine::{engine_update, AGENT_ENGINES};
use crate::domain::platform::{
    browser_origin, browser_url, default_simulator, dictation_refusal, with_spoken_words, AppUpdate, AppUpdateRequest, BrowserApproval, BrowserTab, ComputerStatus, ComputerUsePermissions, DictationModel, DictationPhase,
    DictationSession, SimulatorAvailability, SimulatorDevice, SimulatorDeviceState, SimulatorDriver, DICTATION_HEARD_NOTHING,
};
use crate::domain::terminal::{BrowserImportMemory, BrowserImportResult, BrowserImportSource, MAX_IMPORT_SITES};
use crate::domain::toast::{Toast, ToastTone};
use crate::effects::{Effect, EffectWork};
use crate::events::{CommandResult, WorkspaceEvent, WorkspaceInput};
use crate::reducer::dock::{browser_effects_for_tab, focus_dock_tab, show_dock_tab};
use crate::reducer::errors::{BROWSER_TAB_ERROR, BROWSER_URL_ERROR};
use crate::reducer::sending::prompt_key;
use crate::reducer::toasts::with_toast;
use crate::reducer::Step;
use crate::state::{browser_target, dock_for, dock_mut, dock_owner, owner_of_browser_tab, BrowserImportPicker, WorkspaceState, MAIN_COMPOSER, SIMULATOR_PANEL};
use crate::util::{now, random_id};
use crate::workspace::activity::thread_activity;
use crate::workspace::preferences::view_preferences;

/// The window's preferences as they stand, saved.
pub fn persist_view(state: &WorkspaceState) -> Effect {
    EffectWork::PersistPreferences { preferences: Box::new(view_preferences(state)) }.into()
}

// Browser.

fn patch_tab(state: &mut WorkspaceState, owner: &str, tab_id: &str, change: impl FnOnce(&mut BrowserTab)) {
    if let Some(tab) = dock_mut(state, owner).browser_tabs.iter_mut().find(|tab| tab.id == tab_id) {
        change(tab);
    }
}

fn blank_tab(url: &str, loading: bool) -> BrowserTab {
    BrowserTab { id: random_id(), url: url.into(), title: String::new(), loading, can_go_back: false, can_go_forward: false, error: None }
}

/// Loads a page in the tab named or a new one. Only the user's own load takes the panel and remembers the origin.
pub fn load_browser_page(state: &mut WorkspaceState, owner: &str, url: &str, tab_id: Option<&str>, new_tab: bool, by_user: bool, task_id: Option<&str>) -> Step {
    let origin = browser_origin(url);
    if by_user {
        if let Some(origin) = &origin {
            if !state.browser_origins.contains(origin) {
                state.browser_origins.push(origin.clone());
            }
        }
    }
    state.action_error = None;
    let target = if new_tab { None } else { browser_target(dock_for(state, owner), tab_id).map(|tab| tab.id.clone()) };
    if state.browser_approval.as_ref().map(|approval| approval.tab_id.clone()) == Some(target.clone()) {
        state.browser_approval = None;
    }
    let task_id = task_id.map(String::from);
    if let Some(target) = target {
        if by_user {
            show_dock_tab(state, owner, &target);
        }
        patch_tab(state, owner, &target, |tab| {
            tab.url = url.into();
            tab.loading = true;
            tab.error = None;
        });
        dock_mut(state, owner).browser_tab_id = Some(target.clone());
        let focus = if by_user { focus_dock_tab(state, owner, &target) } else { Vec::new() };
        let mut effects: Vec<Effect> = vec![EffectWork::BrowserNavigate { task_id, tab_id: target, url: url.into() }.into(), persist_view(state)];
        effects.extend(focus);
        return Step::effects(effects);
    }
    let tab = blank_tab(url, true);
    let id = tab.id.clone();
    if by_user {
        show_dock_tab(state, owner, &id);
    }
    let dock = dock_mut(state, owner);
    dock.browser_tabs.push(tab);
    dock.browser_tab_id = Some(id.clone());
    let focus = if by_user { focus_dock_tab(state, owner, &id) } else { Vec::new() };
    let mut effects: Vec<Effect> = vec![EffectWork::BrowserOpen { task_id, tab_id: id.clone(), url: Some(url.into()) }.into()];
    if by_user {
        effects.push(EffectWork::BrowserShow { tab_id: Some(id) }.into());
    }
    effects.push(persist_view(state));
    effects.extend(focus);
    Step::effects(effects)
}

fn with_blank_tab(state: &mut WorkspaceState, owner: &str) -> String {
    let tab = blank_tab("", false);
    let id = tab.id.clone();
    show_dock_tab(state, owner, &id);
    let dock = dock_mut(state, owner);
    dock.browser_tabs.push(tab);
    dock.browser_tab_id = Some(id.clone());
    id
}

/// Puts a run's navigation to the user, in the tab it would load in.
pub fn ask_to_browse(state: &mut WorkspaceState, owner: &str, url: &str, task_id: &str, tab_id: Option<&str>, new_tab: bool) -> Step {
    if state.browser_approval.is_some() {
        return Step::refuse("A site is already waiting for approval. Wait for that decision before opening another site.");
    }
    let approval_id = random_id();
    let target = if new_tab { None } else { browser_target(dock_for(state, owner), tab_id).map(|tab| tab.id.clone()) };
    if let Some(target) = target {
        state.browser_approval = Some(BrowserApproval { approval_id, url: url.into(), task_id: task_id.into(), tab_id: Some(target.clone()) });
        show_dock_tab(state, owner, &target);
        return Step::none();
    }
    let id = with_blank_tab(state, owner);
    state.browser_approval = Some(BrowserApproval { approval_id, url: url.into(), task_id: task_id.into(), tab_id: Some(id.clone()) });
    Step::effects(vec![EffectWork::BrowserOpen { task_id: None, tab_id: id.clone(), url: None }.into(), EffectWork::BrowserShow { tab_id: Some(id) }.into()])
}

/// Closing a page hands the dock its neighbour, and the panel whichever page that turns out to be.
pub fn close_browser_tab(state: &mut WorkspaceState, owner: &str, tab_id: &str, only_if_blank: bool) -> Step {
    let dock = dock_for(state, owner).clone();
    let Some(index) = dock.browser_tabs.iter().position(|tab| tab.id == tab_id) else { return Step::none() };
    if only_if_blank && !dock.browser_tabs[index].url.is_empty() {
        return Step::none();
    }
    let tab = if dock.tab == tab_id { crate::state::dock_tab_after_closing(state, owner, tab_id) } else { dock.tab.clone() };
    let tabs: Vec<BrowserTab> = dock.browser_tabs.iter().filter(|page| page.id != tab_id).cloned().collect();
    let next = if dock.browser_tab_id.as_deref() == Some(tab_id) {
        index.checked_sub(1).and_then(|left| tabs.get(left)).or(tabs.get(index)).map(|page| page.id.clone())
    } else {
        tabs.iter().find(|page| Some(&page.id) == dock.browser_tab_id.as_ref()).map(|page| page.id.clone())
    };
    if state.browser_approval.as_ref().and_then(|approval| approval.tab_id.as_deref()) == Some(tab_id) {
        state.browser_approval = None;
    }
    let held = dock_mut(state, owner);
    held.browser_tabs = tabs.clone();
    held.browser_tab_id = next;
    held.tab = tab.clone();
    let mut effects: Vec<Effect> = vec![EffectWork::BrowserClose { tab_id: tab_id.into() }.into()];
    if owner == dock_owner(state, None) {
        if tabs.iter().any(|page| page.id == tab) {
            effects.extend(browser_effects_for_tab(state, owner, &tab));
        } else {
            effects.push(EffectWork::BrowserShow { tab_id: None }.into());
        }
    }
    effects.push(persist_view(state));
    Step::effects(effects)
}

/// Whether a run may load this page without asking.
pub fn browser_allowed(state: &WorkspaceState, task_id: &str, url: &str) -> bool {
    if browser_origin(url).is_some_and(|origin| state.browser_origins.contains(&origin)) {
        return true;
    }
    state.thread(task_id).is_some_and(|thread| thread.execution_policy == crate::domain::run::ExecutionPolicy::Autonomous)
}

pub fn reduce_browser(state: &mut WorkspaceState, command: &AppCommand) -> Step {
    match command {
        AppCommand::BrowserOpen { task_id, url, tab_id, new_tab } => {
            let owner = dock_owner(state, task_id.as_deref());
            let Some(url) = browser_url(url) else { return Step::refuse(BROWSER_URL_ERROR) };
            let new_tab = *new_tab == Some(true);
            match task_id {
                Some(task_id) if !browser_allowed(state, task_id, &url) => ask_to_browse(state, &owner, &url, task_id, tab_id.as_deref(), new_tab),
                _ => load_browser_page(state, &owner, &url, tab_id.as_deref(), new_tab, task_id.is_none(), task_id.as_deref()),
            }
        }
        AppCommand::BrowserNewTab => crate::reducer::misc::new_browser_tab(state),
        AppCommand::BrowserDecide { allow, approval_id } => {
            let Some(approval) = state.browser_approval.clone().filter(|approval| &approval.approval_id == approval_id) else { return Step::none() };
            let owner = dock_owner(state, Some(&approval.task_id));
            if !allow {
                state.browser_approval = None;
                return match approval.tab_id.as_deref() {
                    Some(tab_id) => close_browser_tab(state, &owner, tab_id, true),
                    None => Step::none(),
                };
            }
            if let Some(origin) = browser_origin(&approval.url) {
                state.browser_origins.push(origin);
            }
            load_browser_page(state, &owner, &approval.url, approval.tab_id.as_deref(), approval.tab_id.is_none(), false, Some(&approval.task_id))
        }
        AppCommand::BrowserSelectTab { task_id, tab_id } => {
            let owner = owner_of_browser_tab(state, tab_id).unwrap_or_else(|| dock_owner(state, task_id.as_deref()));
            if !dock_for(state, &owner).browser_tabs.iter().any(|tab| &tab.id == tab_id) {
                return Step::none();
            }
            show_dock_tab(state, &owner, tab_id);
            dock_mut(state, &owner).browser_tab_id = Some(tab_id.clone());
            Step::effects(browser_effects_for_tab(state, &owner, tab_id))
        }
        AppCommand::BrowserCloseTab { task_id, tab_id } => {
            let owner = owner_of_browser_tab(state, tab_id).unwrap_or_else(|| dock_owner(state, task_id.as_deref()));
            close_browser_tab(state, &owner, tab_id, false)
        }
        AppCommand::BrowserGo { task_id, tab_id, .. } | AppCommand::BrowserReload { task_id, tab_id } | AppCommand::BrowserAct { task_id, tab_id, .. } => {
            let owner = tab_id.as_deref().and_then(|tab| owner_of_browser_tab(state, tab)).unwrap_or_else(|| dock_owner(state, task_id.as_deref()));
            let Some(target) = browser_target(dock_for(state, &owner), tab_id.as_deref()).map(|tab| tab.id.clone()) else { return Step::refuse(BROWSER_TAB_ERROR) };
            let work = match command {
                AppCommand::BrowserAct { action, .. } => return Step::effect(EffectWork::BrowserAct { task_id: task_id.clone(), tab_id: target, action: action.clone() }),
                AppCommand::BrowserGo { delta, .. } => EffectWork::BrowserHistory { task_id: task_id.clone(), tab_id: target.clone(), delta: *delta },
                _ => EffectWork::BrowserReload { task_id: task_id.clone(), tab_id: target.clone() },
            };
            patch_tab(state, &owner, &target, |tab| {
                tab.loading = true;
                tab.error = None;
            });
            Step::effect(work)
        }
        AppCommand::BrowserClearData => {
            state.browser_origins.clear();
            Step::effect(EffectWork::BrowserClearData).then(Step::persisting())
        }
        _ => Step::none(),
    }
}

pub fn browser_updated(state: &mut WorkspaceState, page: &crate::events::BrowserPageEvent) -> Step {
    let Some(owner) = owner_of_browser_tab(state, &page.tab_id) else { return Step::none() };
    let Some(current) = dock_for(state, &owner).browser_tabs.iter().find(|tab| tab.id == page.tab_id).cloned() else { return Step::none() };
    if let Some(request) = &page.navigation_request {
        if owner == dock_owner(state, Some(&request.task_id)) {
            return ask_to_browse(state, &owner, &request.url, &request.task_id, Some(&page.tab_id), false);
        }
    }
    let clearing = current.error.is_some() && page.error.is_none() && page.url.as_ref().is_some_and(|url| *url != current.url);
    patch_tab(state, &owner, &page.tab_id, |tab| {
        if let Some(url) = &page.url {
            tab.url = url.clone();
        }
        if let Some(title) = &page.title {
            tab.title = title.clone();
        }
        if let Some(loading) = page.loading {
            tab.loading = loading;
        }
        if let Some(back) = page.can_go_back {
            tab.can_go_back = back;
        }
        if let Some(forward) = page.can_go_forward {
            tab.can_go_forward = forward;
        }
        if page.error.is_some() {
            tab.error = page.error.clone();
        }
        if clearing {
            tab.error = None;
        }
    });
    if page.url.is_none() {
        return Step::none();
    }
    Step::effects(vec![persist_view(state)])
}

// Browser import.

/// What a profile is called: its browser, and its profile where the browser has several.
pub fn source_name(source: Option<&BrowserImportSource>, sources: &[BrowserImportSource]) -> String {
    let Some(source) = source else { return "that browser".into() };
    if sources.iter().filter(|item| item.browser == source.browser).count() > 1 {
        format!("{} ({})", source.browser, source.profile)
    } else {
        source.browser.clone()
    }
}

pub fn import_notice(result: &BrowserImportResult, sites: &[String], from: &str) -> String {
    let place = if sites.len() == 1 { sites[0].clone() } else { format!("{} sites", sites.len()) };
    let copied = format!("Copied {} {} for {place} from {from}.", result.imported, if result.imported == 1 { "cookie" } else { "cookies" });
    if result.skipped > 0 {
        format!("{copied} {} could not be read.", result.skipped)
    } else {
        copied
    }
}

pub fn reduce_browser_import(state: &mut WorkspaceState, input: &WorkspaceInput) -> Step {
    let current = &mut state.browser_import;
    match input {
        WorkspaceInput::Command(AppCommand::BrowserImportRead) => Step::effect(EffectWork::BrowserImportListSources),
        WorkspaceInput::Event(WorkspaceEvent::BrowserImportSources { sources }) => {
            if current.picker.as_ref().is_some_and(|picker| !sources.iter().any(|source| source.id == picker.source_id)) {
                current.picker = None;
            }
            current.sources = Some(sources.clone());
            Step::none()
        }
        WorkspaceInput::Command(AppCommand::BrowserImportChoose { source_id }) => {
            let Some(source_id) = source_id else {
                current.picker = None;
                return Step::none();
            };
            if !current.sources.as_ref().is_some_and(|sources| sources.iter().any(|source| &source.id == source_id)) {
                return Step::none();
            }
            let selected = current.last.as_ref().filter(|last| &last.source_id == source_id).map(|last| last.sites.clone()).unwrap_or_default();
            current.picker = Some(BrowserImportPicker { source_id: source_id.clone(), sites: None, selected, filter: String::new() });
            current.notice = None;
            current.error = None;
            Step::effect(EffectWork::BrowserImportListSites { source_id: source_id.clone() })
        }
        WorkspaceInput::Event(WorkspaceEvent::BrowserImportSites { source_id, sites }) => {
            let Some(picker) = current.picker.as_mut().filter(|picker| &picker.source_id == source_id) else { return Step::none() };
            let offered: HashSet<&str> = sites.iter().map(|site| site.domain.as_str()).collect();
            picker.selected.retain(|site| offered.contains(site.as_str()));
            picker.sites = Some(sites.clone());
            Step::none()
        }
        WorkspaceInput::Command(AppCommand::BrowserImportToggle { site }) => {
            let Some(picker) = current.picker.as_mut().filter(|picker| picker.sites.as_ref().is_some_and(|sites| sites.iter().any(|item| &item.domain == site))) else { return Step::none() };
            let on = picker.selected.contains(site);
            if !on && picker.selected.len() >= MAX_IMPORT_SITES {
                return Step::none();
            }
            if on {
                picker.selected.retain(|item| item != site);
            } else {
                picker.selected.push(site.clone());
            }
            Step::none()
        }
        WorkspaceInput::Command(AppCommand::BrowserImportFilter { text }) => {
            if let Some(picker) = current.picker.as_mut() {
                picker.filter = text.clone();
            }
            Step::none()
        }
        WorkspaceInput::Command(AppCommand::BrowserImportRun) | WorkspaceInput::Command(AppCommand::BrowserImportAgain) => {
            let target = match input {
                WorkspaceInput::Command(AppCommand::BrowserImportRun) => current.picker.as_ref().map(|picker| BrowserImportMemory { source_id: picker.source_id.clone(), sites: picker.selected.clone() }),
                _ => current.last.clone(),
            };
            let Some(target) = target.filter(|target| !target.sites.is_empty()) else { return Step::none() };
            if current.importing {
                return Step::none();
            }
            current.importing = true;
            current.notice = None;
            current.error = None;
            Step::effect(EffectWork::BrowserImportImport { source_id: target.source_id, sites: target.sites })
        }
        WorkspaceInput::Event(WorkspaceEvent::BrowserImportDone { source_id, sites, result }) => {
            let sources = current.sources.clone().unwrap_or_default();
            let from = source_name(sources.iter().find(|source| &source.id == source_id), &sources);
            let failed = result.imported == 0;
            current.importing = false;
            if failed {
                current.notice = None;
                current.error = Some(format!("No cookies could be copied from {from}."));
                return Step::none();
            }
            current.picker = None;
            current.notice = Some(import_notice(result, sites, &from));
            current.error = None;
            current.last = Some(BrowserImportMemory { source_id: source_id.clone(), sites: sites.clone() });
            Step::persisting()
        }
        WorkspaceInput::Event(WorkspaceEvent::BrowserImportFailed { message }) => {
            current.importing = false;
            if current.picker.as_ref().is_some_and(|picker| picker.sites.is_none()) {
                current.picker = None;
            }
            current.error = Some(message.clone());
            Step::none()
        }
        _ => Step::none(),
    }
}

// Simulator.

pub const SIMULATOR_UNSUPPORTED: &str = "The simulator needs Xcode on a Mac.";
pub const SIMULATOR_NOT_OPEN: &str = "Open the simulator first.";
pub const SIMULATOR_STARTING: &str = "The simulator is still starting.";

/// The devices some dock's Simulator panel shows.
pub fn shown_devices(state: &WorkspaceState) -> HashSet<String> {
    state.docks.values().filter(|dock| dock.panels.iter().any(|panel| panel == SIMULATOR_PANEL)).filter_map(|dock| dock.simulator_id.clone()).collect()
}

/// A device no dock shows any more is let go, however its panel closed or moved on.
pub fn reconcile_simulators(before: &HashSet<String>, state: &WorkspaceState) -> Vec<Effect> {
    let after = shown_devices(state);
    let mut released: Vec<&String> = before.iter().filter(|udid| !after.contains(*udid)).collect();
    released.sort();
    released.into_iter().map(|udid| EffectWork::SimulatorRelease { udid: udid.clone() }.into()).collect()
}

fn refused(message: impl Into<String>) -> Step {
    Step::result(CommandResult::failed(message.into()))
}

fn device<'a>(state: &'a WorkspaceState, udid: &str) -> Option<&'a SimulatorDevice> {
    state.simulator.devices.iter().find(|item| item.udid == udid)
}

fn named<'a>(state: &'a WorkspaceState, name: &str) -> Option<&'a SimulatorDevice> {
    let wanted = name.trim().to_lowercase();
    device(state, name).or_else(|| state.simulator.devices.iter().find(|item| item.udid.to_lowercase() == wanted || item.name.to_lowercase() == wanted))
}

fn boot(state: &mut WorkspaceState, effects: &mut Vec<Effect>, udid: &str) {
    if device(state, udid).is_some_and(|known| known.state == SimulatorDeviceState::Booted) || state.simulator.booting.iter().any(|id| id == udid) {
        return;
    }
    state.simulator.booting.push(udid.into());
    state.simulator.errors.shift_remove(udid);
    effects.push(Effect { work: EffectWork::SimulatorBoot { udid: udid.into() }, follow_up: true });
}

fn remember(state: &mut WorkspaceState, effects: &mut Vec<Effect>, udid: &str) {
    if state.simulator_device.as_deref() == Some(udid) {
        return;
    }
    state.simulator_device = Some(udid.into());
    effects.push(persist_view(state));
}

fn open_in(state: &mut WorkspaceState, effects: &mut Vec<Effect>, owner: &str, udid: Option<String>, by_user: bool) {
    let dock = dock_mut(state, owner);
    if !dock.panels.iter().any(|panel| panel == SIMULATOR_PANEL) {
        dock.panels.push(SIMULATOR_PANEL.into());
    }
    dock.simulator_id = udid.clone();
    let shown = if by_user {
        show_dock_tab(state, owner, SIMULATOR_PANEL);
        focus_dock_tab(state, owner, SIMULATOR_PANEL)
    } else {
        Vec::new()
    };
    effects.push(EffectWork::SimulatorList.into());
    effects.extend(shown);
    if let Some(udid) = udid {
        boot(state, effects, &udid);
    }
}

fn adopt_defaults(state: &mut WorkspaceState, effects: &mut Vec<Effect>) {
    let Some(fallback) = default_simulator(&state.simulator.devices, state.simulator_device.as_deref()).map(|device| device.udid.clone()) else { return };
    let owners: Vec<String> = state
        .docks
        .iter()
        .filter(|(_, dock)| dock.panels.iter().any(|panel| panel == SIMULATOR_PANEL))
        .filter(|(_, dock)| !dock.simulator_id.as_deref().is_some_and(|udid| state.simulator.devices.iter().any(|item| item.udid == udid)))
        .map(|(owner, _)| owner.clone())
        .collect();
    for owner in owners {
        dock_mut(state, &owner).simulator_id = Some(fallback.clone());
        boot(state, effects, &fallback);
    }
}

pub fn reduce_simulator(state: &mut WorkspaceState, input: &WorkspaceInput) -> Step {
    let mut effects: Vec<Effect> = Vec::new();
    match input {
        WorkspaceInput::Command(AppCommand::SimulatorOpen { task_id, device: asked }) => {
            if let SimulatorAvailability::Unsupported { reason } = &state.simulator.availability {
                return refused(reason.clone());
            }
            let owner = dock_owner(state, task_id.as_deref());
            let picked = asked.as_deref().and_then(|name| named(state, name)).map(|device| device.udid.clone());
            if let (Some(asked), None) = (asked, &picked) {
                return refused(format!("No simulator is called {asked}."));
            }
            let udid = picked.clone().or_else(|| dock_for(state, &owner).simulator_id.clone()).or_else(|| default_simulator(&state.simulator.devices, state.simulator_device.as_deref()).map(|device| device.udid.clone()));
            open_in(state, &mut effects, &owner, udid, task_id.is_none());
            if let Some(picked) = picked {
                remember(state, &mut effects, &picked);
            }
            Step::effects(effects)
        }
        WorkspaceInput::Command(AppCommand::SimulatorRefresh) => Step::effect(EffectWork::SimulatorList),
        WorkspaceInput::Command(AppCommand::SimulatorSelect { udid }) => {
            if device(state, udid).is_none() {
                return Step::none();
            }
            let owner = dock_owner(state, None);
            dock_mut(state, &owner).simulator_id = Some(udid.clone());
            boot(state, &mut effects, udid);
            remember(state, &mut effects, udid);
            Step::effects(effects)
        }
        WorkspaceInput::Command(AppCommand::SimulatorInput { task_id, udid, input }) => {
            let owner = dock_owner(state, task_id.as_deref());
            let Some(udid) = udid.clone().or_else(|| dock_for(state, &owner).simulator_id.clone()) else { return refused(SIMULATOR_NOT_OPEN) };
            if device(state, &udid).is_some_and(|target| target.state != SimulatorDeviceState::Booted) {
                return if task_id.is_some() { refused(SIMULATOR_STARTING) } else { Step::none() };
            }
            let Some(task_id) = task_id else { return Step::effect(EffectWork::SimulatorInput { udid, input: input.clone(), task_id: None, seq: None }) };
            let seq = state.simulator.driver.as_ref().map(|driver| driver.seq).unwrap_or(0) + 1;
            state.simulator.driver = Some(SimulatorDriver { task_id: task_id.clone(), udid: udid.clone(), seq });
            Step::effect(EffectWork::SimulatorInput { udid, input: input.clone(), task_id: Some(task_id.clone()), seq: Some(seq) })
        }
        WorkspaceInput::Event(WorkspaceEvent::SimulatorListed { availability, devices }) => {
            let sim = &mut state.simulator;
            sim.booting.retain(|udid| devices.iter().any(|item| &item.udid == udid && item.state != SimulatorDeviceState::Booted));
            sim.orientations.retain(|udid, _| devices.iter().any(|item| &item.udid == udid && item.state == SimulatorDeviceState::Booted));
            sim.availability = availability.clone();
            sim.devices = devices.clone();
            adopt_defaults(state, &mut effects);
            Step::effects(effects)
        }
        WorkspaceInput::Event(WorkspaceEvent::SimulatorBooted { udid }) => {
            let was_booted = device(state, udid).is_some_and(|known| known.state == SimulatorDeviceState::Booted);
            let sim = &mut state.simulator;
            for item in sim.devices.iter_mut().filter(|item| &item.udid == udid) {
                item.state = SimulatorDeviceState::Booted;
            }
            if !was_booted {
                sim.orientations.shift_remove(udid);
            }
            sim.booting.retain(|id| id != udid);
            sim.errors.shift_remove(udid);
            Step::none()
        }
        WorkspaceInput::Event(WorkspaceEvent::SimulatorFailed { udid, message }) => {
            state.simulator.booting.retain(|id| id != udid);
            state.simulator.errors.insert(udid.clone(), message.clone());
            refused(message.clone())
        }
        WorkspaceInput::Event(WorkspaceEvent::SimulatorRefused { message }) => refused(message.clone()),
        WorkspaceInput::Event(WorkspaceEvent::SimulatorTurned { udid, orientation }) => {
            state.simulator.orientations.insert(udid.clone(), *orientation);
            Step::none()
        }
        WorkspaceInput::Event(WorkspaceEvent::SimulatorIdle { seq }) => {
            if state.simulator.driver.as_ref().is_some_and(|driver| driver.seq == *seq) {
                state.simulator.driver = None;
            }
            Step::none()
        }
        _ => Step::none(),
    }
}

// Dictation.

fn noticed(state: &mut WorkspaceState, message: Option<&str>) {
    let held = state.attachment_sends.get(MAIN_COMPOSER).cloned().unwrap_or_default();
    if held.error.as_deref() == message {
        return;
    }
    state.attachment_sends.insert(MAIN_COMPOSER.into(), crate::state::AttachmentSendState { error: message.map(String::from), ..held });
}

fn current_session(state: &WorkspaceState, id: &str) -> Option<DictationSession> {
    state.dictation.session.clone().filter(|session| session.id == id)
}

pub fn reduce_dictation(state: &mut WorkspaceState, input: &WorkspaceInput) -> Step {
    match input {
        WorkspaceInput::Command(AppCommand::DictationStart) => {
            if let Some(refusal) = dictation_refusal(&state.dictation) {
                return Step::refuse(refusal);
            }
            if state.dictation.session.is_some() {
                return Step::none();
            }
            let session = DictationSession { id: random_id(), key: prompt_key(state), phase: DictationPhase::Starting, hands_free: false, send: false };
            let id = session.id.clone();
            state.dictation.session = Some(session);
            Step::effect(EffectWork::DictationStart { id })
        }
        WorkspaceInput::Command(AppCommand::DictationHandsFree) => {
            let key = prompt_key(state);
            let Some(session) = state.dictation.session.as_mut().filter(|session| session.phase != DictationPhase::Transcribing && !session.hands_free) else { return Step::none() };
            session.hands_free = true;
            if session.key == key {
                state.composer_focus += 1;
            }
            Step::none()
        }
        WorkspaceInput::Command(AppCommand::DictationStop { send }) => {
            let Some(session) = state.dictation.session.as_mut().filter(|session| session.phase != DictationPhase::Transcribing) else { return Step::none() };
            session.phase = DictationPhase::Transcribing;
            session.send = *send == Some(true);
            Step::effect(EffectWork::DictationStop { id: session.id.clone() })
        }
        WorkspaceInput::Command(AppCommand::DictationCancel) => match state.dictation.session.take() {
            Some(session) => Step::effect(EffectWork::DictationCancel { id: session.id }),
            None => Step::none(),
        },
        WorkspaceInput::Command(AppCommand::DictationDownload) => {
            if !matches!(state.dictation.model, DictationModel::Missing | DictationModel::Failed) {
                return Step::none();
            }
            state.dictation.model = DictationModel::Downloading;
            state.dictation.message = None;
            state.dictation.progress = Some(0.0);
            Step::effect(EffectWork::DictationDownload)
        }
        WorkspaceInput::Command(AppCommand::DictationCancelDownload) => {
            if state.dictation.model != DictationModel::Downloading {
                return Step::none();
            }
            state.dictation.model = DictationModel::Missing;
            state.dictation.message = None;
            state.dictation.progress = None;
            Step::effect(EffectWork::DictationCancelDownload)
        }
        WorkspaceInput::Event(WorkspaceEvent::DictationModel { model, message, progress }) => {
            state.dictation.model = *model;
            state.dictation.message = message.clone();
            state.dictation.progress = if *model == DictationModel::Downloading { *progress } else { None };
            Step::none()
        }
        WorkspaceInput::Event(WorkspaceEvent::DictationStarted { id }) => {
            if let Some(session) = state.dictation.session.as_mut().filter(|session| &session.id == id && session.phase == DictationPhase::Starting) {
                session.phase = DictationPhase::Recording;
            }
            Step::none()
        }
        WorkspaceInput::Event(WorkspaceEvent::DictationFailed { id, message }) => {
            if current_session(state, id).is_none() {
                return Step::none();
            }
            state.dictation.session = None;
            noticed(state, Some(message));
            Step::none()
        }
        WorkspaceInput::Event(WorkspaceEvent::DictationTranscribed { id, text }) => {
            let Some(session) = current_session(state, id) else { return Step::none() };
            state.dictation.session = None;
            if text.trim().is_empty() {
                noticed(state, Some(DICTATION_HEARD_NOTHING));
                return Step::none();
            }
            let written = with_spoken_words(state.prompts.get(&session.key).map(String::as_str).unwrap_or(""), text);
            state.set_prompt(&session.key, &written);
            noticed(state, None);
            if session.key != prompt_key(state) {
                return Step::none();
            }
            if session.send {
                state.dictation.sends += 1;
            } else {
                state.composer_focus += 1;
            }
            Step::none()
        }
        _ => Step::none(),
    }
}

// Computer use.

fn same_permissions(left: &ComputerUsePermissions, right: &ComputerUsePermissions) -> bool {
    left == right
}

pub fn reduce_computer_use(state: &mut WorkspaceState, input: &WorkspaceInput) -> Step {
    let access = &mut state.computer_use_permissions;
    match input {
        WorkspaceInput::Command(AppCommand::ComputerUseRead) => Step::effect(EffectWork::ComputerUseRead),
        WorkspaceInput::Command(AppCommand::ComputerUseRestart) => Step::effect(EffectWork::ComputerUseRestart),
        WorkspaceInput::Command(AppCommand::ComputerUseEnable { permission }) => {
            access.busy = Some(*permission);
            access.error = None;
            access.requested = true;
            Step::effect(EffectWork::ComputerUseEnable { permission: *permission })
        }
        WorkspaceInput::Event(WorkspaceEvent::ComputerUsePermissions { permissions, enabling }) => {
            let restart_required = access.restart_required || (access.requested && permissions.accessibility && permissions.screen_recording);
            let unchanged = access.permissions.as_ref().is_some_and(|held| same_permissions(held, permissions)) && restart_required == access.restart_required && !(*enabling && access.busy.is_some());
            if unchanged {
                return Step::none();
            }
            access.permissions = Some(permissions.clone());
            access.restart_required = restart_required;
            if *enabling {
                access.busy = None;
            }
            Step::none()
        }
        WorkspaceInput::Event(WorkspaceEvent::ComputerUseFailed { message, enabling }) => {
            access.error = Some(message.clone());
            if *enabling {
                access.busy = None;
            }
            Step::none()
        }
        _ => Step::none(),
    }
}

// Plan usage.

pub fn reduce_plan_usage(state: &mut WorkspaceState, input: &WorkspaceInput) -> Step {
    match input {
        WorkspaceInput::Command(AppCommand::UsageRead { everywhere }) => {
            let read = state.plan_usage.read + 1;
            state.plan_usage.read = read;
            state.plan_usage.reading = AGENT_ENGINES.to_vec();
            let mut effects: Vec<Effect> = AGENT_ENGINES.iter().map(|engine| EffectWork::ReadPlanUsage { engine: *engine, read }.into()).collect();
            if *everywhere == Some(true) {
                let ask = AppCommand::UsageRead { everywhere: None };
                for computer in state.computers.paired.iter().filter(|computer| computer.link.status == ComputerStatus::Connected && crate::validation::supports_computer_command(computer.link.capabilities.as_deref(), &ask)) {
                    effects.push(crate::reducer::computers::forward(&computer.link.id, vec![ask.clone().into()]));
                }
            }
            Step::effects(effects)
        }
        WorkspaceInput::Command(AppCommand::UsageDismissWarning { warning }) => {
            if state.plan_usage.dismissed.contains(warning) {
                return Step::none();
            }
            state.plan_usage.dismissed.push(warning.clone());
            let excess = state.plan_usage.dismissed.len().saturating_sub(crate::domain::platform::MAX_DISMISSED_USAGE_WARNINGS);
            state.plan_usage.dismissed.drain(..excess);
            Step::persisting()
        }
        WorkspaceInput::Event(WorkspaceEvent::UsageReported { engine, read, usage }) => {
            if *read != state.plan_usage.read {
                return Step::none();
            }
            state.plan_usage.reports.insert(*engine, usage.clone());
            state.plan_usage.reading.retain(|item| item != engine);
            Step::none()
        }
        _ => Step::none(),
    }
}

// App updates.

pub const NO_UPDATE_TO_INSTALL: &str = "There is no newer version to install.";

/// How many threads an install would cut off; one waiting out a usage limit resumes on its own.
pub fn running_threads(state: &WorkspaceState) -> usize {
    let activity = thread_activity(state);
    activity.working.iter().filter(|id| !activity.paused.contains(*id)).count()
}

pub fn reduce_app_update(state: &mut WorkspaceState, input: &WorkspaceInput) -> Step {
    match input {
        WorkspaceInput::Command(AppCommand::AppCheckForUpdates { quiet }) => Step::effect(EffectWork::AppCheckForUpdates { quiet: *quiet }),
        WorkspaceInput::Command(AppCommand::AppDownloadUpdate) => {
            if matches!(state.app_update, AppUpdate::Available { .. }) {
                Step::effect(EffectWork::AppDownloadUpdate { unattended: false })
            } else {
                Step::none()
            }
        }
        WorkspaceInput::Command(AppCommand::AppInstallUpdate) => {
            if matches!(state.app_update, AppUpdate::Ready { .. }) {
                Step::effect(EffectWork::AppInstallUpdate { unattended: false })
            } else {
                Step::none()
            }
        }
        WorkspaceInput::Event(WorkspaceEvent::AppUpdateChanged { update }) => {
            state.app_update = update.clone();
            Step::none()
        }
        WorkspaceInput::Event(WorkspaceEvent::AppBuild { build }) => {
            state.app_build = build.clone();
            Step::none()
        }
        WorkspaceInput::Event(WorkspaceEvent::AppLaunched { version }) => {
            state.engine_upgrades_owed = true;
            let toast = Toast { id: 0, tone: ToastTone::Success, title: concat!(crate::app_name!(), " updated").into(), message: Some(format!("Now on {version}.")), subject: None, persistent: false, action: None, remember: None };
            Step::effects(with_toast(state, toast, now()))
        }
        WorkspaceInput::Command(AppCommand::AppCancelUpdate { .. }) => {
            if !matches!(state.app_update_request, Some(AppUpdateRequest::Installing { .. })) {
                state.app_update_request = None;
            }
            Step::none()
        }
        WorkspaceInput::Command(AppCommand::AppUpdate { .. }) => {
            if state.app_update == AppUpdate::Idle {
                return Step::refuse(NO_UPDATE_TO_INSTALL);
            }
            if matches!(state.app_update_request, Some(AppUpdateRequest::Waiting) | Some(AppUpdateRequest::Installing { .. })) {
                return Step::none();
            }
            state.app_update_request = Some(AppUpdateRequest::Waiting);
            if matches!(state.app_update, AppUpdate::Available { .. }) {
                Step::effect(EffectWork::AppDownloadUpdate { unattended: true })
            } else {
                Step::none()
            }
        }
        _ => Step::none(),
    }
}

/// Moves an asked-for update along whatever input arrives, and upgrades the engines an update's launch owes.
pub fn reconcile_app_update(was_downloading: bool, state: &mut WorkspaceState) -> Vec<Effect> {
    let mut effects = Vec::new();
    if state.engine_upgrades_owed && state.engine_updates_offered && !state.engine_checking {
        state.engine_upgrades_owed = false;
        for engine in AGENT_ENGINES {
            let readiness = state.engine_readiness(engine);
            if readiness.required.is_none() || engine_update(&readiness).is_none_or(|update| update.command.is_none()) {
                continue;
            }
            let step = crate::reducer::misc::reduce_command(state, &AppCommand::EngineUpdate { engine, computer_id: None });
            effects.extend(step.effects);
        }
    }
    let update = state.app_update.clone();
    match state.app_update_request.clone() {
        Some(AppUpdateRequest::Installing { version }) if !matches!(update, AppUpdate::Ready { .. }) => {
            state.app_update_request = Some(AppUpdateRequest::Failed { message: format!("Couldn't install {version}.") });
            return effects;
        }
        Some(AppUpdateRequest::Waiting) => {}
        _ => return effects,
    }
    if let (true, AppUpdate::Available { version }) = (was_downloading, &update) {
        state.app_update_request = Some(AppUpdateRequest::Failed { message: format!("Couldn't download {version}.") });
        return effects;
    }
    let AppUpdate::Ready { version } = update else { return effects };
    if running_threads(state) > 0 {
        return effects;
    }
    state.app_update_request = Some(AppUpdateRequest::Installing { version });
    effects.push(EffectWork::AppInstallUpdate { unattended: true }.into());
    effects
}
