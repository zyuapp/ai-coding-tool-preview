//! The boundary every command from outside the window passes: which commands an agent or a paired
//! phone may send, what each must satisfy, where each is carried out once another computer is
//! paired, and which commands a paired computer's build can take.

use serde_json::{Map as JsonMap, Value};

use crate::commands::AppCommand;
use crate::util::js_len;

/// Where a command is carried out once another computer is paired.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// Stays with this window and computer whatever is on screen.
    Local,
    /// Goes to the computer holding the thread it names, else to the one showing the thread on screen.
    Thread,
    /// As `Thread`, but only this computer's own panels can carry it out.
    Panel,
    /// Goes to the computer holding the project it names.
    Project,
    /// Goes to the computer holding the shell it names.
    Terminal,
    /// Puts the computer holding the thread it names on screen.
    Select,
    /// Follows a rule of its own.
    Own,
}

type AgentRule = fn(&AppCommand) -> bool;
type PhoneRule = fn(&JsonMap<String, Value>) -> bool;

/// How a command is declared: the fields it carries, where it is carried out, and who outside may send it.
pub struct Declaration {
    pub kind: &'static str,
    pub fields: &'static [&'static str],
    pub at: Placement,
    /// Moves this window to one of its own threads, which takes a paired computer's thread off screen.
    pub leaves: bool,
    /// What an agent's command must also satisfy. A command without one is the window's alone.
    pub agent: Option<AgentRule>,
    /// What a paired phone's command must satisfy. A command without one is never a phone's.
    pub phone: Option<PhoneRule>,
}

pub static DECLARATIONS: &[Declaration] = &[
    Declaration { kind: "app.list", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "app.open-folder", fields: &["appId"], at: Placement::Own, leaves: false, agent: None, phone: None },
    Declaration { kind: "app.check-for-updates", fields: &["quiet"], at: Placement::Local, leaves: false, agent: None, phone: Some(phone::checks_quietly) },
    Declaration { kind: "app.download-update", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "app.install-update", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "app.update", fields: &["computerId"], at: Placement::Own, leaves: false, agent: None, phone: Some(phone::updates_here) },
    Declaration { kind: "app.cancel-update", fields: &["computerId"], at: Placement::Own, leaves: false, agent: None, phone: Some(phone::updates_here) },
    Declaration { kind: "app.open-source-licenses", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "annotation.add", fields: &["taskId", "quote", "note", "anchor"], at: Placement::Local, leaves: false, agent: None, phone: Some(phone::annotates) },
    Declaration { kind: "annotation.note", fields: &["taskId", "annotationId", "note"], at: Placement::Local, leaves: false, agent: None, phone: Some(phone::notes_annotation) },
    Declaration { kind: "annotation.remove", fields: &["taskId", "annotationId"], at: Placement::Local, leaves: false, agent: None, phone: Some(phone::removes_annotation) },
    Declaration { kind: "annotation.recall", fields: &["taskId", "annotations"], at: Placement::Local, leaves: false, agent: None, phone: Some(phone::recalls_annotations) },
    Declaration { kind: "annotation.reveal", fields: &["taskId", "annotationId"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "paste.add", fields: &["taskId", "text"], at: Placement::Local, leaves: false, agent: None, phone: Some(phone::pastes) },
    Declaration { kind: "paste.remove", fields: &["taskId", "pasteId"], at: Placement::Local, leaves: false, agent: None, phone: Some(phone::removes_paste) },
    Declaration { kind: "paste.recall", fields: &["taskId", "pastes"], at: Placement::Local, leaves: false, agent: None, phone: Some(phone::recalls_pastes) },
    Declaration { kind: "image.add", fields: &["taskId", "path", "label", "source"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "image.remove", fields: &["taskId", "imageId"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "image.recall", fields: &["taskId", "paths"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "image.open", fields: &["source", "gallery"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "image.step", fields: &["delta"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "image.close", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "image.download", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "file.open", fields: &["taskId", "path", "line"], at: Placement::Own, leaves: false, agent: None, phone: None },
    Declaration { kind: "file.attach", fields: &["taskId", "files"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "file.detach", fields: &["taskId", "fileId"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "file.recall", fields: &["taskId", "files"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "automation.notify", fields: &["taskId", "headline", "detail", "key"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "automation.nothing-to-report", fields: &["taskId", "checked"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "automation.save", fields: &["taskId", "draft"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "automation.update", fields: &["taskId", "patch"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "automation.delete", fields: &["taskId"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "automation.run-now", fields: &["taskId"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "browser.open", fields: &["taskId", "url", "tabId", "newTab"], at: Placement::Panel, leaves: false, agent: Some(agent::drives_browser), phone: None },
    Declaration { kind: "browser.new-tab", fields: &[], at: Placement::Panel, leaves: false, agent: None, phone: None },
    Declaration { kind: "browser.close-tab", fields: &["taskId", "tabId"], at: Placement::Panel, leaves: false, agent: Some(agent::drives_browser), phone: None },
    Declaration { kind: "browser.select-tab", fields: &["taskId", "tabId"], at: Placement::Panel, leaves: false, agent: Some(agent::drives_browser), phone: None },
    Declaration { kind: "browser.go", fields: &["taskId", "tabId", "delta"], at: Placement::Panel, leaves: false, agent: Some(agent::drives_browser), phone: None },
    Declaration { kind: "browser.reload", fields: &["taskId", "tabId"], at: Placement::Panel, leaves: false, agent: Some(agent::drives_browser), phone: None },
    Declaration { kind: "browser.act", fields: &["taskId", "tabId", "action"], at: Placement::Panel, leaves: false, agent: Some(agent::drives_browser), phone: None },
    Declaration { kind: "browser.decide", fields: &["allow", "approvalId"], at: Placement::Panel, leaves: false, agent: None, phone: None },
    Declaration { kind: "browser.clear-data", fields: &[], at: Placement::Panel, leaves: false, agent: None, phone: None },
    Declaration { kind: "browser-import.read", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "browser-import.choose", fields: &["sourceId"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "browser-import.toggle", fields: &["site"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "browser-import.filter", fields: &["text"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "browser-import.run", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "browser-import.again", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "cli.read", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "cli.install", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "cli.uninstall", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "computers.discover", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "computers.pair", fields: &["host", "name", "code"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "computers.cancel-pairing", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "computers.forget", fields: &["id"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "computers.reconnect", fields: &["id"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "computers.rename", fields: &["name"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "computers.label", fields: &["id", "name"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "computers.filter", fields: &["filter"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "computer-use.read", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "computer-use.enable", fields: &["permission"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "computer-use.restart", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "dictation.start", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "dictation.hands-free", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "dictation.stop", fields: &["send"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "dictation.cancel", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "dictation.download", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "dictation.cancel-download", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "diff.toggle", fields: &["taskId"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "diff.refresh", fields: &[], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "diff.open-commit", fields: &["commit", "taskId"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "diff.set-range", fields: &["range"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "diff.set-mode", fields: &["mode"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "diff.set-collapsed", fields: &["path", "collapsed"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "diff.set-viewed", fields: &["path", "viewed"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "diff.set-split", fields: &["split"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "diff.set-ignore-whitespace", fields: &["ignore"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "engine.read", fields: &["refresh"], at: Placement::Local, leaves: false, agent: None, phone: Some(phone::rereads_engines) },
    Declaration { kind: "engine.sign-in", fields: &["engine"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "engine.update", fields: &["engine", "computerId"], at: Placement::Own, leaves: false, agent: None, phone: Some(phone::updates_engine_here) },
    Declaration { kind: "engine.reload-settings", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "project.open", fields: &["start"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "project.add", fields: &["root", "computerId", "start"], at: Placement::Own, leaves: false, agent: None, phone: Some(phone::adds_project) },
    Declaration { kind: "view.add-project-close", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.add-project-device", fields: &["computerId"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.add-project-path", fields: &["root"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.add-project-pick", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.add-project-submit", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.add-project-key", fields: &["key"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.add-project-accept", fields: &["index"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "project.edit", fields: &["projectId", "name", "root"], at: Placement::Project, leaves: false, agent: None, phone: None },
    Declaration { kind: "project.remove", fields: &["projectId"], at: Placement::Project, leaves: false, agent: None, phone: None },
    Declaration { kind: "pull-request.read", fields: &["taskId"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "pull-request.read-members", fields: &["taskId"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "pull-request.read-member", fields: &["taskId"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "remote.set-enabled", fields: &["enabled"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "remote.create-pairing-code", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "remote.revoke-device", fields: &["deviceId"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "remote.refresh", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "run.cancel", fields: &["taskId"], at: Placement::Thread, leaves: false, agent: Some(agent::agent_thread), phone: Some(phone::cancels) },
    Declaration { kind: "resource.resume", fields: &["taskId"], at: Placement::Thread, leaves: false, agent: None, phone: Some(phone::names_thread) },
    Declaration { kind: "run.compact", fields: &["taskId"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "question.set-answer", fields: &["taskId", "runId", "requestId", "questionId", "text"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "question.answer", fields: &["taskId", "runId", "requestId", "questionId", "text"], at: Placement::Thread, leaves: false, agent: None, phone: Some(phone::answers) },
    Declaration { kind: "run.decide", fields: &["allow", "taskId", "runId", "approvalId"], at: Placement::Thread, leaves: false, agent: None, phone: Some(phone::decides) },
    Declaration { kind: "run.stop-process", fields: &["taskId", "processId"], at: Placement::Thread, leaves: false, agent: None, phone: Some(phone::stops_process) },
    Declaration { kind: "limit.resume", fields: &["taskId"], at: Placement::Thread, leaves: false, agent: None, phone: Some(phone::names_thread) },
    Declaration { kind: "limit.cancel", fields: &["taskId"], at: Placement::Thread, leaves: false, agent: None, phone: Some(phone::names_thread) },
    Declaration { kind: "review.open", fields: &["taskId"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "review.close", fields: &[], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "review.set-step", fields: &["step"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "review.start", fields: &["taskId", "target"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "simulator.open", fields: &["taskId", "device"], at: Placement::Panel, leaves: false, agent: Some(agent::names_thread), phone: None },
    Declaration { kind: "simulator.refresh", fields: &[], at: Placement::Panel, leaves: false, agent: None, phone: None },
    Declaration { kind: "simulator.select", fields: &["udid"], at: Placement::Panel, leaves: false, agent: None, phone: None },
    Declaration { kind: "simulator.input", fields: &["taskId", "udid", "input"], at: Placement::Panel, leaves: false, agent: Some(agent::drives_simulator), phone: None },
    Declaration { kind: "task.new", fields: &["projectId", "worktreeId"], at: Placement::Own, leaves: true, agent: None, phone: Some(phone::starts) },
    Declaration { kind: "task.select", fields: &["taskId"], at: Placement::Select, leaves: true, agent: None, phone: Some(phone::names_thread) },
    Declaration { kind: "task.archive", fields: &["taskId"], at: Placement::Thread, leaves: false, agent: Some(agent::names_thread), phone: Some(phone::names_thread) },
    Declaration { kind: "task.restore", fields: &["taskId"], at: Placement::Thread, leaves: false, agent: None, phone: Some(phone::names_thread) },
    Declaration { kind: "task.clear-archive", fields: &[], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "task.rename", fields: &["taskId", "title"], at: Placement::Thread, leaves: false, agent: None, phone: Some(phone::renames) },
    Declaration { kind: "task.set-role", fields: &["taskId", "role", "sender"], at: Placement::Thread, leaves: false, agent: Some(agent::gives_role), phone: Some(phone::sets_role) },
    Declaration { kind: "task.set-coordinator", fields: &["taskId", "coordinatorId"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "task.set-lead-away", fields: &["taskId", "away"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "task.resolve", fields: &["taskId"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "task.reopen", fields: &["taskId"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "decision.answer", fields: &["taskId", "decisionId", "answer"], at: Placement::Thread, leaves: false, agent: None, phone: Some(phone::answers_decision) },
    Declaration { kind: "coordination.set-thread-model", fields: &["taskId", "engine", "model", "effort"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "coordination.set-muted", fields: &["taskId", "muted"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "coordination.stop-all", fields: &["taskId"], at: Placement::Thread, leaves: false, agent: None, phone: Some(phone::names_thread) },
    Declaration { kind: "memory.set-purpose", fields: &["taskId", "purpose"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "memory.add", fields: &["taskId", "text"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "memory.edit", fields: &["taskId", "noteId", "text"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "memory.forget", fields: &["taskId", "noteId"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "coordination.next-step", fields: &["taskId", "step"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "proposal.start", fields: &["taskId", "proposalId", "proposalIds", "place"], at: Placement::Thread, leaves: false, agent: None, phone: Some(phone::starts_proposal) },
    Declaration { kind: "proposal.dismiss", fields: &["taskId", "proposalId", "proposalIds"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "task.dismiss", fields: &["taskId"], at: Placement::Thread, leaves: false, agent: Some(agent::agent_thread), phone: Some(phone::names_thread) },
    Declaration { kind: "task.snooze", fields: &["taskId", "hours"], at: Placement::Thread, leaves: false, agent: None, phone: Some(phone::snoozes) },
    Declaration { kind: "task.unsnooze", fields: &["taskId"], at: Placement::Thread, leaves: false, agent: None, phone: Some(phone::names_thread) },
    Declaration { kind: "task.dismiss-all", fields: &["localOnly", "projectId"], at: Placement::Local, leaves: false, agent: None, phone: Some(phone::dismisses_all) },
    Declaration { kind: "task.read-all", fields: &["localOnly"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "task.move", fields: &["taskId", "target"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "task.fork", fields: &["taskId", "worktree", "title"], at: Placement::Thread, leaves: false, agent: None, phone: Some(phone::forks) },
    Declaration { kind: "task.set-policy", fields: &["taskId", "policy"], at: Placement::Thread, leaves: false, agent: None, phone: Some(phone::sets_policy) },
    Declaration { kind: "task.set-model", fields: &["taskId", "engine", "model"], at: Placement::Thread, leaves: false, agent: None, phone: Some(phone::sets_model) },
    Declaration { kind: "task.set-fast-mode", fields: &["taskId", "fastMode"], at: Placement::Thread, leaves: false, agent: None, phone: Some(phone::sets_fast_mode) },
    Declaration { kind: "task.set-effort", fields: &["taskId", "engine", "effort"], at: Placement::Thread, leaves: false, agent: None, phone: Some(phone::sets_effort) },
    Declaration { kind: "task.set-worktree", fields: &["taskId", "worktree"], at: Placement::Thread, leaves: false, agent: None, phone: Some(phone::sets_worktree) },
    Declaration { kind: "task.move-worktree", fields: &["taskId", "destination", "afterRun", "sender", "then"], at: Placement::Thread, leaves: false, agent: Some(agent::moves_to_checkout), phone: Some(phone::moves_worktree) },
    Declaration { kind: "task.set-branch", fields: &["branch", "create"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "task.checkout-branch", fields: &["taskId", "branch", "create"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "task.send", fields: &["taskId", "project", "text", "attachments", "steer", "worktree", "worktreeId", "model", "engine", "effort", "role", "coordinatorId", "brief", "title", "from", "sender", "lead", "memory"], at: Placement::Own, leaves: false, agent: Some(agent::agent_send), phone: Some(phone::sends) },
    Declaration { kind: "attachments.send", fields: &["taskId", "steer", "attachments", "text"], at: Placement::Own, leaves: false, agent: None, phone: Some(phone::sends_images) },
    Declaration { kind: "attachments.notice", fields: &["taskId", "message"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "attachments.drop", fields: &["taskId", "paths"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "attachments.paste-images", fields: &["taskId", "images"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "link.open", fields: &["url"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "task.steer-queued", fields: &["taskId", "messageId"], at: Placement::Thread, leaves: false, agent: None, phone: Some(phone::names_queued) },
    Declaration { kind: "task.drop-queued", fields: &["taskId", "messageId", "ifWaiting"], at: Placement::Thread, leaves: false, agent: None, phone: Some(phone::names_queued) },
    Declaration { kind: "task.edit-queued", fields: &["taskId", "messageId"], at: Placement::Own, leaves: false, agent: None, phone: Some(phone::edits_queued) },
    Declaration { kind: "side-chat.open", fields: &["chatId"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "side-chat.close", fields: &["chatId"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "terminal.open", fields: &["cwd"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "terminal.select", fields: &["terminalId"], at: Placement::Terminal, leaves: false, agent: None, phone: None },
    Declaration { kind: "terminal.close", fields: &["terminalId"], at: Placement::Terminal, leaves: false, agent: None, phone: None },
    Declaration { kind: "terminal.restart", fields: &["terminalId"], at: Placement::Terminal, leaves: false, agent: None, phone: None },
    Declaration { kind: "terminal.input", fields: &["terminalId", "data"], at: Placement::Terminal, leaves: false, agent: None, phone: None },
    Declaration { kind: "terminal.resize", fields: &["terminalId", "cols", "rows"], at: Placement::Terminal, leaves: false, agent: None, phone: None },
    Declaration { kind: "usage.read", fields: &["everywhere"], at: Placement::Local, leaves: false, agent: None, phone: Some(phone::reads_usage_here) },
    Declaration { kind: "usage.dismiss-warning", fields: &["warning"], at: Placement::Local, leaves: false, agent: None, phone: Some(phone::dismisses_usage_warning) },
    Declaration { kind: "view.mounted", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.set-prompt", fields: &["taskId", "prompt"], at: Placement::Local, leaves: false, agent: None, phone: Some(phone::sets_prompt) },
    Declaration { kind: "view.reading-point", fields: &["taskId", "point"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.dismiss-action-error", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.dismiss-hidden-tasks", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.dismiss-toast", fields: &["id"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.edit-automation", fields: &["taskId", "base", "fields"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.hold-toasts", fields: &["held"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.edit-project", fields: &["projectId"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.move-worktree", fields: &["worktree"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.fork-name", fields: &["sourceId", "worktree"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.fork-confirm", fields: &["title"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.archive-prompt", fields: &["threadId"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.archive-confirm", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.set-section-open", fields: &["section", "open"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.set-subagent-group", fields: &["group", "open"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.set-overview-group", fields: &["taskId", "group", "open", "all"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.set-model-favorite", fields: &["engine", "model", "favorite"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.remember-project", fields: &["projectId"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.set-theme", fields: &["theme"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.set-theme-family", fields: &["family", "systemDark"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.set-theme-mode", fields: &["mode", "systemDark"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.system-scheme", fields: &["dark"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.set-ui-font", fields: &["font"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.set-mono-font", fields: &["font"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.set-reading-size", fields: &["size"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.set-terminal-size", fields: &["size"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.set-activity-project", fields: &["projectId"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.set-sidebar-open", fields: &["open"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.set-session-panel-open", fields: &["open"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.set-zen", fields: &["zen"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.set-capture-options", fields: &["options"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.set-chrome-browser", fields: &["enabled"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.set-computer-use", fields: &["enabled"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.set-browser-tools", fields: &["enabled"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.set-memory-protection", fields: &["enabled"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.set-notifications", fields: &["enabled"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.inspect-subagent", fields: &["taskId", "subagentId"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.set-settings-open", fields: &["open", "section", "settingId"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.set-usage-computer", fields: &["computerId"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.close-tab", fields: &[], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.new-tab", fields: &[], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.set-dock-open", fields: &["open"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.set-dock-expanded", fields: &["expanded"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.open-dock-panel", fields: &["panel", "taskId"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.close-dock-panel", fields: &["panel"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.close-thread-tab", fields: &["taskId"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.open-workflow", fields: &["workflowId"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.select-dock-tab", fields: &["tab"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.select-dock-index", fields: &["index"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.set-menu", fields: &["menu"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.go-back", fields: &[], at: Placement::Local, leaves: true, agent: None, phone: None },
    Declaration { kind: "view.go-forward", fields: &[], at: Placement::Local, leaves: true, agent: None, phone: None },
    Declaration { kind: "view.set-focused", fields: &["focused"], at: Placement::Own, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.focus-composer", fields: &["recover"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.shortcut", fields: &["action", "surface"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.escape", fields: &[], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.set-shortcut", fields: &["action", "binding"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.reset-shortcuts", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.capture-shortcut", fields: &["action"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.dismiss-computer-use-setup", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.refresh-environment", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.dock-keys", fields: &["tab"], at: Placement::Thread, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.find-open", fields: &["target"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.find-query", fields: &["query"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.find-step", fields: &["delta"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.find-close", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.jump-open", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.jump-query", fields: &["query"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.jump-step", fields: &["delta"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.jump-choose", fields: &["taskId"], at: Placement::Select, leaves: true, agent: None, phone: None },
    Declaration { kind: "view.jump-choose-setting", fields: &["section", "settingId"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "view.jump-close", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "worktree.menu-open", fields: &["list"], at: Placement::Own, leaves: false, agent: None, phone: None },
    Declaration { kind: "worktree.menu-search", fields: &["list", "query"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "worktree.refresh", fields: &[], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "worktree.filter-project", fields: &["project"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "worktree.confirm-delete", fields: &["root"], at: Placement::Own, leaves: false, agent: None, phone: None },
    Declaration { kind: "worktree.confirm-delete-safe", fields: &["open"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "worktree.delete-safe", fields: &["roots"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "worktree.set-missing-open", fields: &["open"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "worktree.set-threads-open", fields: &["root", "open"], at: Placement::Local, leaves: false, agent: None, phone: None },
    Declaration { kind: "worktree.open-thread", fields: &["taskId"], at: Placement::Select, leaves: true, agent: None, phone: None },
    Declaration { kind: "worktree.reveal", fields: &["root"], at: Placement::Own, leaves: false, agent: None, phone: None },
    Declaration { kind: "worktree.delete", fields: &["taskId", "root", "worktreeId", "missingOnly"], at: Placement::Own, leaves: false, agent: Some(agent::deletes_worktree_by_id), phone: None },
];

pub fn declaration(kind: &str) -> Option<&'static Declaration> {
    DECLARATIONS.iter().find(|declaration| declaration.kind == kind)
}

pub fn command_placement(command: &AppCommand) -> Placement {
    declaration(&command.type_name()).map(|declaration| declaration.at).unwrap_or(Placement::Local)
}

pub fn command_leaves(command: &AppCommand) -> bool {
    declaration(&command.type_name()).is_some_and(|declaration| declaration.leaves)
}

const MAX_TEXT: usize = 16_000_000;

/// The window's own checks on a command's fields, beyond the shape its type already holds it to.
fn fields_within_limits(value: &Value) -> bool {
    match value {
        Value::String(text) => text.len() <= MAX_TEXT * 4 && js_len(text) <= MAX_TEXT,
        Value::Array(items) => items.len() <= 10_000 && items.iter().all(fields_within_limits),
        Value::Object(fields) => fields.values().all(fields_within_limits),
        Value::Number(number) => number.as_f64().is_some_and(f64::is_finite),
        _ => true,
    }
}

/// Reads a command a view or another caller sent, refusing anything this build cannot take as a command.
pub fn parse_command(value: &Value) -> Result<AppCommand, String> {
    let Some(kind) = value.get("type").and_then(Value::as_str) else { return Err("A command names its type.".into()) };
    if declaration(kind).is_none() {
        return Err(format!("{kind} is not a command."));
    }
    if !fields_within_limits(value) {
        return Err(format!("{kind} carries a field this app cannot take."));
    }
    serde_json::from_value::<AppCommand>(value.clone()).map_err(|error| format!("{kind}: {error}"))
}

/// Whether a command from outside the window passes the window's field checks.
pub fn outside_fields_hold(value: &Value) -> bool {
    parse_command(value).is_ok_and(|command| crate::command_fields::fields_hold(value, &command))
}

/// The command surface open to agents: commands that declare an agent rule, passing it.
pub fn external_command(value: &Value) -> Result<AppCommand, String> {
    let command = parse_command(value)?;
    if !outside_fields_hold(value) {
        return Err(format!("{} carries a field this app cannot take.", command.type_name()));
    }
    let declaration = declaration(&command.type_name()).expect("parsed commands are declared");
    match declaration.agent {
        Some(rule) if rule(&command) => Ok(command),
        Some(_) => Err(format!("{} cannot be sent that way by an agent.", declaration.kind)),
        None => Err(format!("{} is the user's alone.", declaration.kind)),
    }
}

/// The command surface open to a paired phone: commands that declare a phone rule, passing it.
pub fn phone_command(value: &Value) -> Result<AppCommand, String> {
    let Some(fields) = value.as_object() else { return Err("A command is an object.".into()) };
    let kind = fields.get("type").and_then(Value::as_str).unwrap_or("");
    let Some(declaration) = declaration(kind) else { return Err(format!("{kind} is not a command.")) };
    let Some(rule) = declaration.phone else { return Err(format!("{kind} is not a phone's to send.")) };
    if !rule(fields) || !outside_fields_hold(value) {
        return Err(format!("{kind} cannot be sent that way from a phone."));
    }
    parse_command(value)
}

pub fn is_external_command(value: &Value) -> bool {
    external_command(value).is_ok()
}

/// The capability names this build advertises to paired computers.
pub fn computer_capabilities() -> Vec<String> {
    let mut names = Vec::new();
    for declaration in DECLARATIONS {
        names.push(format!("command:{}", declaration.kind));
        for field in declaration.fields {
            names.push(format!("command:{}:{field}", declaration.kind));
        }
    }
    names
}

pub const REMOTE_UNSUPPORTED: &str = "This feature is unavailable on that computer. Updating AI Coding Tool there may help.";

/// Whether a paired computer's build takes a command with every field it sets. None is a host from
/// before capability discovery, which keeps best-effort behaviour.
pub fn supports_computer_command(capabilities: Option<&[String]>, command: &AppCommand) -> bool {
    let Some(capabilities) = capabilities else { return true };
    let kind = command.type_name();
    let prefix = format!("command:{kind}");
    if !capabilities.iter().any(|name| *name == prefix) {
        return false;
    }
    let Ok(Value::Object(fields)) = serde_json::to_value(command) else { return false };
    fields.iter().filter(|(key, value)| key.as_str() != "type" && !value.is_null()).all(|(key, _)| capabilities.iter().any(|name| *name == format!("{prefix}:{key}")))
}

/// Shared field checks for agent and phone rules.
pub(crate) mod checks {
    use super::*;

    pub const MAX_ID_LENGTH: usize = 256;
    pub const MAX_PROMPT_LENGTH: usize = 1_000_000;
    pub const MAX_PATH_LENGTH: usize = 4_096;

    pub fn is_string(value: Option<&Value>, max: usize) -> bool {
        value.and_then(Value::as_str).is_some_and(|text| !text.is_empty() && js_len(text) <= max)
    }

    pub fn is_id(value: Option<&Value>) -> bool {
        is_string(value, MAX_ID_LENGTH)
    }

    pub fn is_blankable(value: Option<&Value>, max: usize) -> bool {
        value.and_then(Value::as_str).is_some_and(|text| js_len(text) <= max)
    }

    pub fn is_count(value: Option<&Value>) -> bool {
        value.and_then(Value::as_f64).is_some_and(|number| number.fract() == 0.0 && number >= 0.0)
    }

    pub fn absent(value: Option<&Value>) -> bool {
        value.is_none_or(Value::is_null)
    }

    pub fn is_bool(value: Option<&Value>) -> bool {
        value.is_some_and(Value::is_boolean)
    }

    pub fn absent_or_bool(value: Option<&Value>) -> bool {
        absent(value) || is_bool(value)
    }

    pub fn absent_or_id(value: Option<&Value>) -> bool {
        absent(value) || is_id(value)
    }

    /// A thread named by id, or none for the thread on screen.
    pub fn named(command: &JsonMap<String, Value>) -> bool {
        absent_or_id(command.get("taskId"))
    }

    pub fn agent_id(value: Option<&str>) -> bool {
        value.is_none_or(|id| !id.is_empty() && js_len(id) <= MAX_ID_LENGTH)
    }

    pub fn present_id(value: Option<&str>) -> bool {
        value.is_some_and(|id| !id.is_empty() && js_len(id) <= MAX_ID_LENGTH)
    }
}

/// Who may set each field of a send: the window's fields never, and a new thread's shape only an agent starting one.
fn send_stays_in_reach(command: &JsonMap<String, Value>, phone: bool) -> bool {
    const STARTING: [&str; 6] = ["model", "engine", "effort", "role", "brief", "title"];
    const WINDOW: [&str; 6] = ["attachments", "coordinatorId", "from", "sender", "lead", "memory"];
    let present = |field: &str| !checks::absent(command.get(field));
    if WINDOW.iter().any(|field| present(field)) {
        return false;
    }
    let names_thread = !checks::absent(command.get("taskId"));
    !(STARTING.iter().any(|field| present(field)) && (phone || names_thread))
}

/// What an agent's command must satisfy beyond its shape.
pub mod agent {
    use super::checks::*;
    use super::*;
    use crate::domain::worktree::WorktreeDestination;

    pub fn agent_thread(command: &AppCommand) -> bool {
        agent_id(command.task_id())
    }

    /// An agent acts on the thread it names, never on whichever one is on screen.
    pub fn names_thread(command: &AppCommand) -> bool {
        present_id(command.task_id())
    }

    /// Only the window says which thread on another computer asked.
    pub fn gives_role(command: &AppCommand) -> bool {
        matches!(command, AppCommand::TaskSetRole { sender: None, .. }) && names_thread(command)
    }

    pub fn agent_send(command: &AppCommand) -> bool {
        let AppCommand::TaskSend(send) = command else { return false };
        let Ok(Value::Object(fields)) = serde_json::to_value(command) else { return false };
        agent_id(send.task_id.as_deref())
            && send.project.as_deref().is_none_or(|project| !project.is_empty() && js_len(project) <= crate::domain::coordination::MAX_PROPOSAL_PROJECT)
            && send.text.as_deref().is_some_and(|text| !text.is_empty() && js_len(text) <= MAX_PROMPT_LENGTH && !text.trim().is_empty())
            && agent_id(send.worktree_id.as_deref())
            && send_stays_in_reach(&fields, false)
    }

    pub fn moves_to_checkout(command: &AppCommand) -> bool {
        let AppCommand::TaskMoveWorktree { task_id, destination, after_run, sender, .. } = command else { return false };
        agent_id(task_id.as_deref()) && !after_run && sender.is_none() && match destination {
            WorktreeDestination::Worktree { id } => present_id(Some(id)),
            _ => true,
        }
    }

    /// An id, never a path or the thread on screen, and never the folder-is-gone shortcut.
    pub fn deletes_worktree_by_id(command: &AppCommand) -> bool {
        matches!(command, AppCommand::WorktreeDelete { task_id: None, root: None, worktree_id: Some(id), missing_only: None } if present_id(Some(id)))
    }

    /// A run drives the browser as itself, so every browser command names the thread that asked.
    pub fn drives_browser(command: &AppCommand) -> bool {
        let (task_id, tab_id, url) = match command {
            AppCommand::BrowserOpen { task_id, tab_id, url, .. } => (task_id, tab_id.as_deref(), Some(url.as_str())),
            AppCommand::BrowserCloseTab { task_id, tab_id } | AppCommand::BrowserSelectTab { task_id, tab_id } => (task_id, Some(tab_id.as_str()), None),
            AppCommand::BrowserGo { task_id, tab_id, .. } | AppCommand::BrowserReload { task_id, tab_id } | AppCommand::BrowserAct { task_id, tab_id, .. } => (task_id, tab_id.as_deref(), None),
            _ => return false,
        };
        let action_holds = match command {
            AppCommand::BrowserAct { action, .. } => match action {
                crate::domain::platform::BrowserAction::Click { reference } => present_id(Some(reference)),
                crate::domain::platform::BrowserAction::Type { reference, text, .. } => present_id(Some(reference)) && js_len(text) <= MAX_PROMPT_LENGTH,
            },
            _ => true,
        };
        present_id(task_id.as_deref()) && agent_id(tab_id) && url.is_none_or(|url| !url.is_empty() && js_len(url) <= 8_192) && action_holds
    }

    /// A run's gestures go to the device its own dock shows, and never through the Mac's clipboard.
    pub fn drives_simulator(command: &AppCommand) -> bool {
        let AppCommand::SimulatorInput { task_id, udid, input } = command else { return false };
        present_id(task_id.as_deref()) && udid.is_none() && crate::domain::platform::is_simulator_input(input) && !crate::domain::platform::uses_mac_clipboard(input)
    }
}

/// What a paired phone's command must satisfy. A rule reads the command as sent.
pub mod phone {
    use super::checks::*;
    use super::*;

    type Fields = JsonMap<String, Value>;

    pub fn names_thread(command: &Fields) -> bool {
        is_id(command.get("taskId"))
    }

    pub fn checks_quietly(command: &Fields) -> bool {
        command.get("quiet") == Some(&Value::Bool(true))
    }

    pub fn updates_here(command: &Fields) -> bool {
        absent(command.get("computerId"))
    }

    pub fn updates_engine_here(command: &Fields) -> bool {
        command.get("engine").and_then(Value::as_str).is_some_and(|engine| crate::domain::engine::AgentEngine::parse(engine).is_some()) && absent(command.get("computerId"))
    }

    pub fn rereads_engines(command: &Fields) -> bool {
        command.get("refresh") == Some(&Value::Bool(true))
    }

    pub fn reads_usage_here(command: &Fields) -> bool {
        absent(command.get("everywhere"))
    }

    pub fn dismisses_usage_warning(command: &Fields) -> bool {
        is_id(command.get("warning"))
    }

    pub fn adds_project(command: &Fields) -> bool {
        is_string(command.get("root"), MAX_PATH_LENGTH)
            && command.get("root").and_then(Value::as_str).is_some_and(|root| !root.trim().is_empty())
            && absent(command.get("computerId"))
            && (absent(command.get("start")) || command.get("start") == Some(&Value::Bool(true)))
    }

    const MAX_QUOTE_LENGTH: usize = 100_000;
    const MAX_RECALL: usize = 100;

    fn is_anchor(value: Option<&Value>) -> bool {
        let Some(anchor) = value.and_then(Value::as_object) else { return false };
        match anchor.get("kind").and_then(Value::as_str) {
            Some("message") => is_id(anchor.get("messageId")) && is_count(anchor.get("start")) && is_count(anchor.get("end")),
            Some("diff") => {
                is_string(anchor.get("comparison"), 4_096)
                    && is_string(anchor.get("path"), 4_096)
                    && is_id(anchor.get("start"))
                    && is_id(anchor.get("end"))
                    && matches!(anchor.get("side").and_then(Value::as_str), Some("old") | Some("new"))
            }
            _ => false,
        }
    }

    fn is_annotation(value: &Value) -> bool {
        let Some(annotation) = value.as_object() else { return false };
        is_id(annotation.get("id"))
            && is_string(annotation.get("quote"), MAX_QUOTE_LENGTH)
            && is_blankable(annotation.get("note"), MAX_QUOTE_LENGTH)
            && (absent(annotation.get("anchor")) || is_anchor(annotation.get("anchor")))
    }

    fn is_pasted_text(value: &Value) -> bool {
        value.as_object().is_some_and(|paste| is_id(paste.get("id")) && is_string(paste.get("text"), MAX_PROMPT_LENGTH))
    }

    fn bounded_list(value: Option<&Value>, item: fn(&Value) -> bool) -> bool {
        value.and_then(Value::as_array).is_some_and(|items| items.len() <= MAX_RECALL && items.iter().all(item))
    }

    pub fn annotates(command: &Fields) -> bool {
        named(command)
            && is_string(command.get("quote"), MAX_QUOTE_LENGTH)
            && (absent(command.get("note")) || is_blankable(command.get("note"), MAX_QUOTE_LENGTH))
            && (absent(command.get("anchor")) || is_anchor(command.get("anchor")))
    }

    pub fn notes_annotation(command: &Fields) -> bool {
        named(command) && is_id(command.get("annotationId")) && is_blankable(command.get("note"), MAX_QUOTE_LENGTH)
    }

    pub fn removes_annotation(command: &Fields) -> bool {
        named(command) && is_id(command.get("annotationId"))
    }

    pub fn recalls_annotations(command: &Fields) -> bool {
        named(command) && bounded_list(command.get("annotations"), is_annotation)
    }

    pub fn pastes(command: &Fields) -> bool {
        named(command) && is_string(command.get("text"), MAX_PROMPT_LENGTH)
    }

    pub fn removes_paste(command: &Fields) -> bool {
        named(command) && is_id(command.get("pasteId"))
    }

    pub fn recalls_pastes(command: &Fields) -> bool {
        named(command) && bounded_list(command.get("pastes"), is_pasted_text)
    }

    pub fn cancels(command: &Fields) -> bool {
        named(command)
    }

    pub fn decides(command: &Fields) -> bool {
        is_id(command.get("taskId")) && is_id(command.get("runId")) && is_id(command.get("approvalId")) && is_bool(command.get("allow"))
    }

    pub fn stops_process(command: &Fields) -> bool {
        named(command) && is_id(command.get("processId"))
    }

    pub fn answers(command: &Fields) -> bool {
        is_id(command.get("taskId"))
            && is_id(command.get("runId"))
            && is_id(command.get("requestId"))
            && is_id(command.get("questionId"))
            && (absent(command.get("text")) || is_string(command.get("text"), MAX_PROMPT_LENGTH))
            && absent(command.get("attachments"))
    }

    /// A phone sends a message and nothing else.
    pub fn sends(command: &Fields) -> bool {
        named(command)
            && (absent(command.get("project")) || is_string(command.get("project"), MAX_PATH_LENGTH))
            && (absent(command.get("text")) || is_string(command.get("text"), MAX_PROMPT_LENGTH))
            && absent_or_bool(command.get("steer"))
            && absent_or_bool(command.get("worktree"))
            && absent_or_id(command.get("worktreeId"))
            && send_stays_in_reach(command, true)
    }

    pub fn starts(command: &Fields) -> bool {
        absent_or_id(command.get("projectId")) && absent_or_id(command.get("worktreeId"))
    }

    pub fn dismisses_all(command: &Fields) -> bool {
        absent_or_bool(command.get("localOnly")) && absent_or_id(command.get("projectId"))
    }

    const MAX_TITLE_LENGTH: usize = 1_000;

    pub fn renames(command: &Fields) -> bool {
        is_id(command.get("taskId")) && is_blankable(command.get("title"), MAX_TITLE_LENGTH)
    }

    pub fn forks(command: &Fields) -> bool {
        named(command) && absent_or_bool(command.get("worktree"))
    }

    pub fn sets_policy(command: &Fields) -> bool {
        named(command) && command.get("policy").is_some_and(|policy| serde_json::from_value::<crate::domain::run::ExecutionPolicy>(policy.clone()).is_ok())
    }

    pub fn sets_model(command: &Fields) -> bool {
        named(command)
            && command.get("engine").and_then(Value::as_str).is_some_and(|engine| crate::domain::engine::AgentEngine::parse(engine).is_some())
            && (absent(command.get("model")) || command.get("model").and_then(Value::as_str).is_some_and(crate::domain::engine::is_model_id))
    }

    pub fn sets_fast_mode(command: &Fields) -> bool {
        named(command) && is_bool(command.get("fastMode"))
    }

    pub fn sets_effort(command: &Fields) -> bool {
        named(command)
            && command.get("engine").and_then(Value::as_str).is_some_and(|engine| crate::domain::engine::AgentEngine::parse(engine).is_some())
            && command.get("effort").is_some_and(|effort| serde_json::from_value::<crate::domain::run::AgentEffort>(effort.clone()).is_ok())
    }

    pub fn sets_worktree(command: &Fields) -> bool {
        named(command) && is_bool(command.get("worktree"))
    }

    fn is_destination(value: Option<&Value>) -> bool {
        let Some(destination) = value.and_then(Value::as_object) else { return false };
        match destination.get("kind").and_then(Value::as_str) {
            Some("local") | Some("new") => true,
            Some("worktree") => is_id(destination.get("id")),
            _ => false,
        }
    }

    /// Only the host says whether a move waits for its run, what it goes on with, or who elsewhere asked.
    pub fn moves_worktree(command: &Fields) -> bool {
        named(command) && is_destination(command.get("destination")) && absent(command.get("afterRun")) && absent(command.get("sender")) && absent(command.get("then"))
    }

    pub fn names_queued(command: &Fields) -> bool {
        named(command) && is_id(command.get("messageId"))
    }

    pub fn edits_queued(command: &Fields) -> bool {
        is_id(command.get("taskId")) && is_id(command.get("messageId"))
    }

    pub fn answers_decision(command: &Fields) -> bool {
        is_id(command.get("taskId")) && is_id(command.get("decisionId")) && is_string(command.get("answer"), crate::domain::coordination::MAX_ANSWER)
    }

    pub fn starts_proposal(command: &Fields) -> bool {
        is_id(command.get("taskId")) && is_id(command.get("proposalId")) && absent(command.get("proposalIds")) && absent(command.get("place"))
    }

    pub fn snoozes(command: &Fields) -> bool {
        is_id(command.get("taskId")) && command.get("hours").and_then(Value::as_u64).is_some_and(|hours| crate::domain::thread::is_snooze_hours(hours as u32))
    }

    pub fn sets_role(command: &Fields) -> bool {
        named(command)
            && command.get("role").is_some_and(|role| role.is_null() || serde_json::from_value::<crate::domain::thread_role::ThreadRole>(role.clone()).is_ok())
            && absent(command.get("sender"))
    }

    pub fn sets_prompt(command: &Fields) -> bool {
        named(command) && is_blankable(command.get("prompt"), MAX_PROMPT_LENGTH)
    }

    pub const MAX_PHONE_IMAGE_LENGTH: usize = 8 * 1024 * 1024;
    pub const MAX_PHONE_IMAGES: usize = 6;

    fn phone_image(source: &str) -> bool {
        let Some(data) = source.strip_prefix("data:image/png;base64,") else { return false };
        let body = data.trim_end_matches('=');
        data.len() - body.len() <= 2 && !body.is_empty() && body.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '+' || ch == '/')
    }

    /// Images a phone sends: each a picture the phone holds, never a path here, a context, or marks.
    pub fn sends_images(command: &Fields) -> bool {
        named(command)
            && absent_or_bool(command.get("steer"))
            && (absent(command.get("text")) || is_string(command.get("text"), MAX_PROMPT_LENGTH))
            && command.get("attachments").and_then(Value::as_array).is_some_and(|attachments| {
                !attachments.is_empty()
                    && attachments.len() <= MAX_PHONE_IMAGES
                    && attachments.iter().all(|attachment| {
                        let Some(attachment) = attachment.as_object() else { return false };
                        is_id(attachment.get("id"))
                            && attachment.get("source").and_then(Value::as_str).is_some_and(|source| source.len() <= MAX_PHONE_IMAGE_LENGTH && phone_image(source))
                            && attachment.get("annotations").and_then(Value::as_array).is_some_and(Vec::is_empty)
                            && absent(attachment.get("path"))
                            && absent(attachment.get("context"))
                    })
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_declared_command_is_one_the_enum_reads() {
        let mut declared: Vec<&str> = DECLARATIONS.iter().map(|declaration| declaration.kind).collect();
        declared.sort();
        declared.dedup();
        assert_eq!(declared.len(), DECLARATIONS.len());
    }

    #[test]
    fn agents_reach_only_their_commands() {
        assert!(external_command(&json!({ "type": "task.send", "text": "hi" })).is_ok());
        assert!(external_command(&json!({ "type": "task.send", "text": "  " })).is_err());
        assert!(external_command(&json!({ "type": "task.send", "text": "hi", "taskId": "a", "model": "opus" })).is_err());
        assert!(external_command(&json!({ "type": "task.send", "text": "hi", "from": "x" })).is_err());
        assert!(external_command(&json!({ "type": "view.set-theme", "theme": "nord" })).is_err());
        assert!(external_command(&json!({ "type": "task.archive" })).is_err());
        assert!(external_command(&json!({ "type": "worktree.delete", "worktreeId": "w1" })).is_ok());
        assert!(external_command(&json!({ "type": "worktree.delete", "root": "/tmp" })).is_err());
    }

    #[test]
    fn phones_send_messages_but_never_shape_threads() {
        assert!(phone_command(&json!({ "type": "task.send", "text": "hi" })).is_ok());
        assert!(phone_command(&json!({ "type": "task.send", "text": "hi", "model": "opus" })).is_err());
        assert!(phone_command(&json!({ "type": "task.snooze", "taskId": "a", "hours": 4 })).is_ok());
        assert!(phone_command(&json!({ "type": "task.snooze", "taskId": "a", "hours": 5 })).is_err());
        assert!(phone_command(&json!({ "type": "terminal.input", "terminalId": "t", "data": "ls" })).is_err());
    }

    #[test]
    fn outside_commands_leave_optional_fields_out_rather_than_null() {
        assert!(external_command(&json!({ "type": "browser.go", "taskId": "a", "delta": 1 })).is_ok());
        assert!(external_command(&json!({ "type": "browser.go", "taskId": "a", "delta": 3 })).is_err());
        assert!(external_command(&json!({ "type": "task.send", "text": "Fix", "title": "   " })).is_err());
        assert!(external_command(&json!({ "type": "task.send", "text": "Fix", "title": "x".repeat(121) })).is_err());
        assert!(external_command(&json!({ "type": "task.send", "text": "Fix", "model": "two words" })).is_err());
        assert!(phone_command(&json!({ "type": "task.dismiss-all" })).is_ok());
        assert!(phone_command(&json!({ "type": "task.dismiss-all", "localOnly": null })).is_err());
    }

    #[test]
    fn capabilities_name_every_field_a_command_sets() {
        let caps = vec!["command:task.fork".to_string(), "command:task.fork:taskId".to_string()];
        assert!(supports_computer_command(Some(&caps), &AppCommand::TaskFork { task_id: Some("a".into()), worktree: None, title: None }));
        assert!(!supports_computer_command(Some(&caps), &AppCommand::TaskFork { task_id: Some("a".into()), worktree: None, title: Some("x".into()) }));
        assert!(supports_computer_command(None, &AppCommand::TaskClearArchive));
    }
}
