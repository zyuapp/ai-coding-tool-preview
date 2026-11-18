//! What the settings pages draw from every computer: the Projects page's rows, each paired computer's
//! plan limits and upkeep, and the checkouts a coordinator's threads work in.

use crate::shared::Shared;
use std::collections::{HashMap, HashSet};

use crate::commands::AppCommand;
use crate::domain::engine::{engine_label, engine_update, AgentEngine, EngineAccess, EngineReadiness, AGENT_ENGINES};
use crate::domain::platform::{AppBuild, AppUpdate, AppUpdateRequest, ComputerStatus, PlanUsageState};
use crate::domain::project::{project_name, Project};
use crate::domain::thread::Thread;
use crate::domain::thread_role::ThreadRole;
use crate::domain::worktree::worktree_name;
use crate::events::ChangedFilesResult;
use crate::reducer::devices::running_threads;
use crate::state::{PairedComputer, WorkspaceState};
use crate::util::locale_cmp;
use crate::validation::supports_computer_command;
use crate::view::computers::thread_host_of;
use crate::view::sidebar::ThreadHost;

/// A row on the Projects page: the project, the paired computer holding it, and how many threads removing it archives.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectSettingsRow<'a> {
    pub project: &'a Project,
    pub host: Option<ThreadHost>,
    pub threads: usize,
}

fn rows_of<'a>(projects: &'a [Project], threads: &[Shared<Thread>], host: Option<ThreadHost>) -> Vec<ProjectSettingsRow<'a>> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for thread in threads {
        if let (Some(project), None) = (thread.project_id.as_deref(), thread.archived_at) {
            *counts.entry(project).or_default() += 1;
        }
    }
    let mut rows: Vec<ProjectSettingsRow> = projects.iter().filter(|project| !project.unlisted).map(|project| ProjectSettingsRow { project, host: host.clone(), threads: counts.get(project.id.as_str()).copied().unwrap_or(0) }).collect();
    rows.sort_by(|a, b| locale_cmp(&project_name(a.project), &project_name(b.project)).then_with(|| locale_cmp(&a.project.root, &b.project.root)));
    rows
}

/// Every computer's listed projects: this computer's first, then each paired computer's by name.
pub fn project_settings_rows(state: &WorkspaceState) -> Vec<ProjectSettingsRow<'_>> {
    let mut paired: Vec<&PairedComputer> = state.computers.paired.iter().collect();
    paired.sort_by(|a, b| locale_cmp(&a.link.name, &b.link.name).then_with(|| a.link.id.cmp(&b.link.id)));
    let mut rows = rows_of(&state.projects, &state.threads, None);
    for computer in paired {
        if let Some(held) = computer.state.as_deref() {
            rows.extend(rows_of(&held.projects, &held.threads, Some(thread_host_of(computer))));
        }
    }
    rows
}

/// A paired computer's limits as the Usage page draws them.
#[derive(Clone, Debug, PartialEq)]
pub struct ComputerUsage<'a> {
    pub id: String,
    pub name: String,
    pub status: ComputerStatus,
    pub usage: Option<&'a PlanUsageState>,
    pub supported: bool,
}

pub fn computer_usage(paired: &[PairedComputer]) -> Vec<ComputerUsage<'_>> {
    paired
        .iter()
        .map(|computer| ComputerUsage {
            id: computer.link.id.clone(),
            name: computer.link.name.clone(),
            status: computer.link.status,
            usage: computer.state.as_deref().map(|held| &held.plan_usage),
            supported: supports_computer_command(computer.link.capabilities.as_deref(), &AppCommand::UsageRead { everywhere: None }),
        })
        .collect()
}

/// How a phrase on a computer's row should read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpkeepTone {
    Live,
    Quiet,
    Progress,
    Offer,
    Error,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UpkeepPart {
    pub text: String,
    pub tone: Option<UpkeepTone>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UpkeepAction {
    pub label: String,
    pub command: AppCommand,
}

/// One piece of software on a computer: the app, or an engine.
#[derive(Clone, Debug, PartialEq)]
pub struct UpkeepItem {
    /// "app", or the engine's id.
    pub key: String,
    pub label: String,
    pub version: Option<String>,
    pub state: UpkeepPart,
    pub action: Option<UpkeepAction>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ComputerUpkeep {
    pub id: String,
    pub status: Vec<UpkeepPart>,
    pub action: Option<UpkeepAction>,
    pub items: Vec<UpkeepItem>,
}

fn part(text: impl Into<String>, tone: Option<UpkeepTone>) -> UpkeepPart {
    UpkeepPart { text: text.into(), tone }
}

fn up_to_date() -> UpkeepPart {
    part("Up to date", Some(UpkeepTone::Quiet))
}

fn threads(count: usize) -> String {
    if count == 1 {
        "1 thread".into()
    } else {
        format!("{count} threads")
    }
}

/// Negative when `left` is older, zero when they match, positive when `left` is newer.
pub fn compare_versions(left: &str, right: &str) -> i64 {
    let parts = |text: &str| -> Vec<i64> { text.split('.').map(|part| part.parse::<i64>().unwrap_or(0)).collect() };
    let (left, right) = (parts(left), parts(right));
    for index in 0..left.len().max(right.len()) {
        let difference = left.get(index).copied().unwrap_or(0) - right.get(index).copied().unwrap_or(0);
        if difference != 0 {
            return difference;
        }
    }
    0
}

struct Target<'a> {
    computer_id: Option<String>,
    capabilities: Option<&'a [String]>,
    paired: bool,
}

impl Target<'_> {
    fn supports(&self, command: &AppCommand) -> bool {
        !self.paired || supports_computer_command(self.capabilities, command)
    }

    fn app_update(&self) -> AppCommand {
        AppCommand::AppUpdate { computer_id: self.computer_id.clone() }
    }
}

fn app_item(target: &Target, remote: &WorkspaceState, own: Option<&AppBuild>) -> UpkeepItem {
    let item = |state: UpkeepPart, action: Option<UpkeepAction>| UpkeepItem { key: "app".into(), label: "AI Coding Tool".into(), version: remote.app_build.as_ref().map(|build| build.version.clone()), state, action };
    if !target.supports(&AppCommand::AppUpdate { computer_id: None }) {
        return item(part("Update it there once to manage it from here", None), None);
    }
    let request = remote.app_update_request.as_ref();
    if let Some(AppUpdateRequest::Installing { version }) = request {
        return item(part(format!("Installing {version}…"), Some(UpkeepTone::Progress)), None);
    }
    if let AppUpdate::Downloading { version, percent } = &remote.app_update {
        return item(part(format!("Downloading {version} · {percent}%"), Some(UpkeepTone::Progress)), None);
    }
    if let Some(AppUpdateRequest::Failed { message }) = request {
        return item(part(message.trim_end_matches('.'), Some(UpkeepTone::Error)), Some(UpkeepAction { label: "Try again".into(), command: target.app_update() }));
    }
    if let AppUpdate::Available { version } | AppUpdate::Ready { version } = &remote.app_update {
        let running = running_threads(remote);
        if matches!(request, Some(AppUpdateRequest::Waiting)) {
            return item(part(format!("Waiting for {}", threads(running)), Some(UpkeepTone::Progress)), Some(UpkeepAction { label: "Cancel".into(), command: AppCommand::AppCancelUpdate { computer_id: target.computer_id.clone() } }));
        }
        let waits = if running > 0 { format!(" · waits for {}", threads(running)) } else { String::new() };
        return item(part(format!("{version} available{waits}"), Some(UpkeepTone::Offer)), Some(UpkeepAction { label: "Update".into(), command: target.app_update() }));
    }
    if let (Some(build), Some(own)) = (&remote.app_build, own) {
        if !build.self_updating && compare_versions(&own.version, &build.version) > 0 {
            return item(part(format!("{} available · update it there", own.version), Some(UpkeepTone::Offer)), None);
        }
    }
    item(up_to_date(), None)
}

/// The engines that computer reports or is updating: the ones this build knows in their order, then
/// any only a newer build knows. Until it has reported any, this build's own.
fn reported_engines(remote: &WorkspaceState) -> Vec<AgentEngine> {
    let status = remote.engine_status.clone().unwrap_or_default();
    let mut reported: Vec<AgentEngine> = status.engines().filter(|engine| !engine.is_known()).collect();
    reported.extend(remote.engine_updating);
    reported.extend(remote.engine_update_queue.iter().copied());
    let known: HashSet<AgentEngine> = AGENT_ENGINES.iter().copied().filter(|engine| status.get(*engine).is_some() || reported.contains(engine)).collect();
    if known.is_empty() && reported.is_empty() {
        return AGENT_ENGINES.to_vec();
    }
    let mut engines: Vec<AgentEngine> = AGENT_ENGINES.iter().copied().filter(|engine| known.contains(engine)).collect();
    for engine in reported.into_iter().filter(|engine| !engine.is_known()) {
        if !engines.contains(&engine) {
            engines.push(engine);
        }
    }
    engines
}

fn engine_item(target: &Target, remote: &WorkspaceState, engine: AgentEngine) -> UpkeepItem {
    let readiness: Option<EngineReadiness> = remote.engine_status.as_ref().and_then(|status| status.get(engine).cloned());
    let item = |state: UpkeepPart, action: Option<UpkeepAction>| UpkeepItem { key: engine.as_str().into(), label: engine_label(engine).into(), version: readiness.as_ref().and_then(|readiness| readiness.version.clone()), state, action };
    if remote.engine_updating == Some(engine) {
        return item(part("Updating…", Some(UpkeepTone::Progress)), None);
    }
    if remote.engine_update_queue.contains(&engine) {
        return item(part("Waiting to update", Some(UpkeepTone::Progress)), None);
    }
    let Some(readiness) = readiness.clone() else { return item(part("Checking…", Some(UpkeepTone::Quiet)), None) };
    match readiness.access {
        EngineAccess::Missing => return item(part("Not installed", Some(UpkeepTone::Quiet)), None),
        EngineAccess::Unavailable => return item(part("Won't start", Some(UpkeepTone::Error)), None),
        _ => {}
    }
    let Some(update) = engine_update(&readiness) else { return item(up_to_date(), None) };
    let command = AppCommand::EngineUpdate { engine, computer_id: target.computer_id.clone() };
    let upgradable = engine.is_known() && update.command.is_some() && target.supports(&AppCommand::EngineUpdate { engine, computer_id: None });
    let action = upgradable.then(|| UpkeepAction { label: "Update".into(), command });
    let place = if upgradable { "" } else { " · update it there" };
    if let Some(required) = &readiness.required {
        let blocked = readiness.access == EngineAccess::Outdated;
        return item(part(format!("Needs {required} {}{place}", if blocked { "to run" } else { "for newer models" }), Some(if blocked { UpkeepTone::Error } else { UpkeepTone::Offer })), action);
    }
    item(part(format!("{} available{place}", update.target), Some(UpkeepTone::Offer)), action)
}

fn upkeep_items(target: &Target, remote: &WorkspaceState, own: Option<&AppBuild>) -> Vec<UpkeepItem> {
    let mut items = vec![app_item(target, remote, own)];
    items.extend(reported_engines(remote).into_iter().map(|engine| engine_item(target, remote, engine)));
    items
}

fn connection(computer: &PairedComputer, remote: Option<&WorkspaceState>) -> Vec<UpkeepPart> {
    if computer.link.status == ComputerStatus::Connected {
        return vec![part("Connected", Some(UpkeepTone::Live))];
    }
    if let Some(AppUpdateRequest::Installing { version }) = remote.and_then(|remote| remote.app_update_request.as_ref()) {
        return vec![part(format!("Restarting into {version}…"), Some(UpkeepTone::Progress))];
    }
    let away = part(if computer.link.status == ComputerStatus::Connecting { "Reconnecting…" } else { "Offline" }, None);
    match &computer.link.error {
        Some(error) => vec![away, part(error.trim_end_matches('.'), None)],
        None => vec![away],
    }
}

/// Each paired computer's row on the Computers page.
pub fn computer_upkeep(paired: &[PairedComputer], own: Option<&AppBuild>) -> Vec<ComputerUpkeep> {
    paired
        .iter()
        .map(|computer| {
            let remote = computer.state.as_deref();
            let status = connection(computer, remote);
            if computer.link.status != ComputerStatus::Connected {
                let restarting = matches!(remote.and_then(|remote| remote.app_update_request.as_ref()), Some(AppUpdateRequest::Installing { .. }));
                let action = (!restarting).then(|| UpkeepAction { label: "Reconnect".into(), command: AppCommand::ComputersReconnect { id: Some(computer.link.id.clone()) } });
                return ComputerUpkeep { id: computer.link.id.clone(), status, action, items: Vec::new() };
            }
            let items = match remote {
                Some(remote) => upkeep_items(&Target { computer_id: Some(computer.link.id.clone()), capabilities: computer.link.capabilities.as_deref(), paired: true }, remote, own),
                None => Vec::new(),
            };
            ComputerUpkeep { id: computer.link.id.clone(), status, action: None, items }
        })
        .collect()
}

/// This computer's own app and engines, as a paired phone reads them.
pub fn own_upkeep(state: &WorkspaceState) -> Vec<UpkeepItem> {
    upkeep_items(&Target { computer_id: None, capabilities: None, paired: false }, state, None)
}

/// A checkout a coordinator's threads work in, with what stands between it and deletion.
#[derive(Clone, Debug, PartialEq)]
pub struct MemberWorktree<'a> {
    pub id: String,
    pub root: String,
    pub name: String,
    pub branch: Option<String>,
    pub threads: Vec<&'a Thread>,
    pub busy: bool,
    pub deleting: bool,
}

pub fn member_worktrees_view<'a>(state: &'a WorkspaceState, lead: Option<&Thread>, busy: &HashSet<String>) -> Vec<MemberWorktree<'a>> {
    let Some(lead) = lead else { return Vec::new() };
    let mut members: HashMap<&str, Vec<&Thread>> = HashMap::new();
    let mut claimants: HashMap<&str, Vec<&Thread>> = HashMap::new();
    for thread in &state.threads {
        let Some(worktree_id) = thread.worktree_id.as_deref() else { continue };
        claimants.entry(worktree_id).or_default().push(thread);
        if thread.parent_id.as_deref() == Some(lead.id.as_str()) && thread.role != Some(ThreadRole::Coordinator) {
            members.entry(worktree_id).or_default().push(thread);
        }
    }
    if members.is_empty() {
        return Vec::new();
    }
    let mut checkouts: Vec<&crate::domain::worktree::Worktree> = state.worktrees.iter().filter(|worktree| members.contains_key(worktree.id.as_str())).collect();
    checkouts.sort_by_key(|worktree| std::cmp::Reverse(worktree.last_used_at));
    checkouts
        .into_iter()
        .map(|worktree| {
            let standing = claimants.get(worktree.id.as_str()).cloned().unwrap_or_default();
            let branch = match state.environments.get(&worktree.workspace_id) {
                Some(ChangedFilesResult::Available { branch, .. }) => branch.clone(),
                _ => state.managed_worktrees.as_ref().and_then(|managed| managed.iter().find(|item| item.root == worktree.root)).and_then(|item| item.branch.clone()),
            };
            MemberWorktree {
                id: worktree.id.clone(),
                root: worktree.root.clone(),
                name: worktree_name(worktree),
                branch,
                threads: members.get(worktree.id.as_str()).cloned().unwrap_or_default(),
                busy: standing.iter().any(|thread| busy.contains(&thread.id)),
                deleting: state.deleting_worktrees.contains(&worktree.root) || standing.iter().any(|thread| state.releasing_worktrees.contains(&thread.id)),
            }
        })
        .collect()
}
