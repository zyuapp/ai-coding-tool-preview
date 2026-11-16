//! Every interaction the application supports, named as data. The UI dispatches these instead of
//! mutating state, so any other caller can drive the same behaviour through the same door. Anything
//! that reaches `AppCommand` from outside the window is validated first (see `validation`).

use serde::{Deserialize, Serialize};

use crate::domain::appearance::{CaptureOptions, ThemeMode};
use crate::domain::automation::{AutomationDraft, AutomationFields, AutomationPatch};
use crate::domain::conversation::{Annotation, AnnotationAnchor, AttachedFile, AttachedFileDraft, OutgoingAttachment, PastedText, RunAttachment};
use crate::domain::coordination::{NextStep, OverviewSection, RemoteLead, ThreadBrief};
use crate::domain::diff::{DiffMode, DiffRange};
use crate::domain::engine::{AgentEngine, AgentModel};
use crate::domain::find::FindTarget;
use crate::domain::message_origin::RemoteSender;
use crate::domain::platform::{BrowserAction, ComputerUsePermission, SimulatorInput};
use crate::domain::project::ThreadDropTarget;
use crate::domain::question::ReviewTarget;
use crate::domain::run::{AgentEffort, ExecutionPolicy, SubagentGroup};
use crate::domain::shortcuts::ShortcutSurface;
use crate::domain::sidebar::{SettingsSection, SidebarSection};
use crate::domain::thread_role::ThreadRole;
use crate::domain::worktree::WorktreeDestination;
use crate::state::{ReadingPoint, ReviewStep, WorktreeMenuList};

/// A field that may be absent, null, or a value: `None`, `Some(None)`, `Some(Some(v))`.
pub mod double_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<T: Serialize, S: Serializer>(value: &Option<Option<T>>, serializer: S) -> Result<S::Ok, S::Error> {
        match value {
            Some(inner) => inner.serialize(serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, T: Deserialize<'de>, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Option<T>>, D::Error> {
        Option::<T>::deserialize(deserializer).map(Some)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProposalStartPlace {
    pub project: String,
    pub worktree: bool,
}

/// A pasted image's encoded bytes, in base64.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PastedImage {
    pub data: String,
    /// Whether the bytes are a PNG already, which is kept as it is when small enough.
    pub png: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectAddKey {
    ArrowUp,
    ArrowDown,
    Tab,
    Enter,
    Escape,
}

/// Snooze lengths the menu offers, in hours.
pub type SnoozeHours = u32;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum AppCommand {
    // Threads.
    #[serde(rename = "task.new")]
    TaskNew {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worktree_id: Option<String>,
    },
    #[serde(rename = "task.select")]
    TaskSelect { task_id: String },
    #[serde(rename = "task.archive")]
    TaskArchive { task_id: String },
    #[serde(rename = "task.restore")]
    TaskRestore { task_id: String },
    #[serde(rename = "task.clear-archive")]
    TaskClearArchive,
    #[serde(rename = "task.rename")]
    TaskRename { task_id: String, title: String },
    /// `None` takes the role off. With no thread yet, the role waits for the first message.
    #[serde(rename = "task.set-role")]
    TaskSetRole {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        role: Option<ThreadRole>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sender: Option<RemoteSender>,
    },
    #[serde(rename = "task.set-coordinator")]
    TaskSetCoordinator { task_id: String, coordinator_id: Option<String> },
    #[serde(rename = "task.set-lead-away")]
    TaskSetLeadAway { task_id: String, away: bool },
    #[serde(rename = "task.resolve")]
    TaskResolve { task_id: String },
    #[serde(rename = "task.reopen")]
    TaskReopen { task_id: String },
    #[serde(rename = "decision.answer")]
    DecisionAnswer { task_id: String, decision_id: String, answer: String },
    #[serde(rename = "coordination.set-thread-model")]
    CoordinationSetThreadModel {
        task_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        engine: Option<AgentEngine>,
        model: AgentModel,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effort: Option<AgentEffort>,
    },
    #[serde(rename = "coordination.set-muted")]
    CoordinationSetMuted { task_id: String, muted: bool },
    #[serde(rename = "coordination.stop-all")]
    CoordinationStopAll { task_id: String },
    #[serde(rename = "memory.set-purpose")]
    MemorySetPurpose { task_id: String, purpose: Option<String> },
    #[serde(rename = "memory.add")]
    MemoryAdd { task_id: String, text: String },
    #[serde(rename = "memory.edit")]
    MemoryEdit { task_id: String, note_id: String, text: String },
    #[serde(rename = "memory.forget")]
    MemoryForget { task_id: String, note_id: String },
    #[serde(rename = "coordination.next-step")]
    CoordinationNextStep { task_id: String, step: NextStep },
    #[serde(rename = "proposal.start")]
    ProposalStart {
        task_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        proposal_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        proposal_ids: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        place: Option<ProposalStartPlace>,
    },
    #[serde(rename = "proposal.dismiss")]
    ProposalDismiss {
        task_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        proposal_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        proposal_ids: Option<Vec<String>>,
    },
    #[serde(rename = "task.dismiss")]
    TaskDismiss {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
    },
    #[serde(rename = "task.snooze")]
    TaskSnooze { task_id: String, hours: SnoozeHours },
    #[serde(rename = "task.unsnooze")]
    TaskUnsnooze { task_id: String },
    #[serde(rename = "task.dismiss-all")]
    TaskDismissAll {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        local_only: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project_id: Option<String>,
    },
    #[serde(rename = "task.read-all")]
    TaskReadAll {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        local_only: Option<bool>,
    },
    #[serde(rename = "task.move")]
    TaskMove { task_id: String, target: ThreadDropTarget },
    #[serde(rename = "task.fork")]
    TaskFork {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worktree: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    #[serde(rename = "task.set-policy")]
    TaskSetPolicy {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        policy: ExecutionPolicy,
    },
    #[serde(rename = "task.set-model")]
    TaskSetModel {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        engine: AgentEngine,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<AgentModel>,
    },
    #[serde(rename = "task.set-fast-mode")]
    TaskSetFastMode {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        fast_mode: bool,
    },
    #[serde(rename = "task.set-effort")]
    TaskSetEffort {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        engine: AgentEngine,
        effort: AgentEffort,
    },
    #[serde(rename = "task.set-worktree")]
    TaskSetWorktree {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        worktree: bool,
    },
    #[serde(rename = "task.move-worktree")]
    TaskMoveWorktree {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        destination: WorktreeDestination,
        #[serde(default, skip_serializing_if = "crate::util::is_false")]
        after_run: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sender: Option<RemoteSender>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        then: Option<String>,
    },
    #[serde(rename = "task.set-branch")]
    TaskSetBranch {
        branch: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        create: Option<bool>,
    },
    #[serde(rename = "task.checkout-branch")]
    TaskCheckoutBranch {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        branch: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        create: Option<bool>,
    },
    #[serde(rename = "task.send")]
    TaskSend(Box<TaskSend>),
    #[serde(rename = "attachments.send")]
    AttachmentsSend {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        steer: Option<bool>,
        attachments: Vec<OutgoingAttachment>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    #[serde(rename = "attachments.notice")]
    AttachmentsNotice {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        message: Option<String>,
    },
    /// Files dropped, pasted or picked into a composer: an image is kept in the app's own images, anything
    /// else is named where it sits (dropped-files.ts).
    #[serde(rename = "attachments.drop")]
    AttachmentsDrop {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        paths: Vec<String>,
    },
    /// Images pasted from the clipboard, kept like dropped ones.
    #[serde(rename = "attachments.paste-images")]
    AttachmentsPasteImages {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        images: Vec<PastedImage>,
    },
    /// A web page opened outside the app, as a link's `target="_blank"` does.
    #[serde(rename = "link.open")]
    LinkOpen { url: String },
    #[serde(rename = "task.steer-queued")]
    TaskSteerQueued {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        message_id: String,
    },
    #[serde(rename = "task.drop-queued")]
    TaskDropQueued {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        message_id: String,
        #[serde(default, skip_serializing_if = "crate::util::is_false")]
        if_waiting: bool,
    },
    #[serde(rename = "task.edit-queued")]
    TaskEditQueued {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        message_id: String,
    },

    // Side chats.
    #[serde(rename = "side-chat.open")]
    SideChatOpen { chat_id: String },
    #[serde(rename = "side-chat.close")]
    SideChatClose { chat_id: String },

    // Annotations, pastes, images, files.
    #[serde(rename = "annotation.add")]
    AnnotationAdd {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        quote: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        anchor: Option<AnnotationAnchor>,
    },
    #[serde(rename = "annotation.note")]
    AnnotationNote {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        annotation_id: String,
        note: String,
    },
    #[serde(rename = "annotation.remove")]
    AnnotationRemove {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        annotation_id: String,
    },
    #[serde(rename = "annotation.recall")]
    AnnotationRecall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        annotations: Vec<Annotation>,
    },
    #[serde(rename = "annotation.reveal")]
    AnnotationReveal {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        annotation_id: String,
    },
    #[serde(rename = "paste.add")]
    PasteAdd {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        text: String,
    },
    #[serde(rename = "paste.remove")]
    PasteRemove {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        paste_id: String,
    },
    #[serde(rename = "paste.recall")]
    PasteRecall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        pastes: Vec<PastedText>,
    },
    #[serde(rename = "image.add")]
    ImageAdd {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        path: String,
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<String>,
    },
    #[serde(rename = "image.remove")]
    ImageRemove {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        image_id: String,
    },
    #[serde(rename = "image.recall")]
    ImageRecall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        paths: Vec<String>,
    },
    #[serde(rename = "image.open")]
    ImageOpen {
        source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gallery: Option<Vec<String>>,
    },
    #[serde(rename = "image.step")]
    ImageStep { delta: i8 },
    #[serde(rename = "image.close")]
    ImageClose,
    #[serde(rename = "image.download")]
    ImageDownload,
    #[serde(rename = "file.open")]
    FileOpen {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        line: Option<i64>,
    },
    #[serde(rename = "file.attach")]
    FileAttach {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        files: Vec<AttachedFileDraft>,
    },
    #[serde(rename = "file.detach")]
    FileDetach {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        file_id: String,
    },
    #[serde(rename = "file.recall")]
    FileRecall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        files: Vec<AttachedFile>,
    },

    // The terminal command, computer use, plan usage.
    #[serde(rename = "cli.read")]
    CliRead,
    #[serde(rename = "cli.install")]
    CliInstall,
    #[serde(rename = "cli.uninstall")]
    CliUninstall,
    #[serde(rename = "computer-use.read")]
    ComputerUseRead,
    #[serde(rename = "computer-use.enable")]
    ComputerUseEnable { permission: ComputerUsePermission },
    #[serde(rename = "computer-use.restart")]
    ComputerUseRestart,
    #[serde(rename = "usage.read")]
    UsageRead {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        everywhere: Option<bool>,
    },
    #[serde(rename = "usage.dismiss-warning")]
    UsageDismissWarning { warning: String },

    // Projects.
    #[serde(rename = "project.open")]
    ProjectOpen {
        #[serde(default, skip_serializing_if = "crate::util::is_false")]
        start: bool,
    },
    #[serde(rename = "project.add")]
    ProjectAdd {
        root: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        computer_id: Option<String>,
        #[serde(default, skip_serializing_if = "crate::util::is_false")]
        start: bool,
    },
    #[serde(rename = "view.add-project-close")]
    AddProjectClose,
    #[serde(rename = "view.add-project-device")]
    AddProjectDevice { computer_id: String },
    #[serde(rename = "view.add-project-path")]
    AddProjectPath { root: String },
    #[serde(rename = "view.add-project-pick")]
    AddProjectPick,
    #[serde(rename = "view.add-project-submit")]
    AddProjectSubmit,
    #[serde(rename = "view.add-project-key")]
    AddProjectKey { key: ProjectAddKey },
    #[serde(rename = "view.add-project-accept")]
    AddProjectAccept { index: i64 },
    #[serde(rename = "project.edit")]
    ProjectEdit {
        project_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none", with = "double_option")]
        name: Option<Option<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        root: Option<String>,
    },
    #[serde(rename = "project.remove")]
    ProjectRemove { project_id: String },

    // Pull requests.
    #[serde(rename = "pull-request.read")]
    PullRequestRead {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
    },
    #[serde(rename = "pull-request.read-members")]
    PullRequestReadMembers {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
    },
    #[serde(rename = "pull-request.read-member")]
    PullRequestReadMember { task_id: String },

    // Run control.
    #[serde(rename = "resource.resume")]
    ResourceResume { task_id: String },
    #[serde(rename = "run.cancel")]
    RunCancel {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
    },
    #[serde(rename = "run.compact")]
    RunCompact {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
    },
    #[serde(rename = "question.set-answer")]
    QuestionSetAnswer { task_id: String, run_id: String, request_id: String, question_id: String, text: String },
    #[serde(rename = "question.answer")]
    QuestionAnswer {
        task_id: String,
        run_id: String,
        request_id: String,
        question_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    #[serde(rename = "run.decide")]
    RunDecide { allow: bool, task_id: String, run_id: String, approval_id: String },
    #[serde(rename = "run.stop-process")]
    RunStopProcess {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        process_id: String,
    },
    #[serde(rename = "limit.resume")]
    LimitResume { task_id: String },
    #[serde(rename = "limit.cancel")]
    LimitCancel { task_id: String },

    // Review.
    #[serde(rename = "review.open")]
    ReviewOpen {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
    },
    #[serde(rename = "review.close")]
    ReviewClose,
    #[serde(rename = "review.set-step")]
    ReviewSetStep { step: ReviewStep },
    #[serde(rename = "review.start")]
    ReviewStart {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        target: ReviewTarget,
    },

    // Worktrees.
    #[serde(rename = "worktree.menu-open")]
    WorktreeMenuOpen { list: WorktreeMenuList },
    #[serde(rename = "worktree.menu-search")]
    WorktreeMenuSearch { list: WorktreeMenuList, query: String },
    #[serde(rename = "worktree.refresh")]
    WorktreeRefresh,
    #[serde(rename = "worktree.filter-project")]
    WorktreeFilterProject { project: Option<String> },
    #[serde(rename = "worktree.confirm-delete")]
    WorktreeConfirmDelete { root: Option<String> },
    #[serde(rename = "worktree.confirm-delete-safe")]
    WorktreeConfirmDeleteSafe { open: bool },
    #[serde(rename = "worktree.delete-safe")]
    WorktreeDeleteSafe { roots: Vec<String> },
    #[serde(rename = "worktree.set-missing-open")]
    WorktreeSetMissingOpen { open: bool },
    #[serde(rename = "worktree.set-threads-open")]
    WorktreeSetThreadsOpen { root: String, open: bool },
    #[serde(rename = "worktree.open-thread")]
    WorktreeOpenThread { task_id: String },
    #[serde(rename = "worktree.reveal")]
    WorktreeReveal { root: String },
    #[serde(rename = "worktree.delete")]
    WorktreeDelete {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        root: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worktree_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        missing_only: Option<bool>,
    },

    // Automations.
    #[serde(rename = "automation.notify")]
    AutomationNotify {
        task_id: String,
        headline: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
    },
    #[serde(rename = "automation.nothing-to-report")]
    AutomationNothingToReport { task_id: String, checked: String },
    #[serde(rename = "automation.save")]
    AutomationSave {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        draft: AutomationDraft,
    },
    #[serde(rename = "automation.update")]
    AutomationUpdate {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        patch: AutomationPatch,
    },
    #[serde(rename = "automation.delete")]
    AutomationDelete {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
    },
    #[serde(rename = "automation.run-now")]
    AutomationRunNow {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
    },

    // Browser.
    #[serde(rename = "browser.open")]
    BrowserOpen {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tab_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        new_tab: Option<bool>,
    },
    #[serde(rename = "browser.new-tab")]
    BrowserNewTab,
    #[serde(rename = "browser.close-tab")]
    BrowserCloseTab {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        tab_id: String,
    },
    #[serde(rename = "browser.select-tab")]
    BrowserSelectTab {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        tab_id: String,
    },
    #[serde(rename = "browser.go")]
    BrowserGo {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tab_id: Option<String>,
        delta: i8,
    },
    #[serde(rename = "browser.reload")]
    BrowserReload {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tab_id: Option<String>,
    },
    #[serde(rename = "browser.act")]
    BrowserAct {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tab_id: Option<String>,
        action: BrowserAction,
    },
    #[serde(rename = "browser.decide")]
    BrowserDecide { allow: bool, approval_id: String },
    #[serde(rename = "browser.clear-data")]
    BrowserClearData,
    #[serde(rename = "browser-import.read")]
    BrowserImportRead,
    #[serde(rename = "browser-import.choose")]
    BrowserImportChoose { source_id: Option<String> },
    #[serde(rename = "browser-import.toggle")]
    BrowserImportToggle { site: String },
    #[serde(rename = "browser-import.filter")]
    BrowserImportFilter { text: String },
    #[serde(rename = "browser-import.run")]
    BrowserImportRun,
    #[serde(rename = "browser-import.again")]
    BrowserImportAgain,

    // Simulator.
    #[serde(rename = "simulator.open")]
    SimulatorOpen {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },
    #[serde(rename = "simulator.refresh")]
    SimulatorRefresh,
    #[serde(rename = "simulator.select")]
    SimulatorSelect { udid: String },
    #[serde(rename = "simulator.input")]
    SimulatorInput {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        udid: Option<String>,
        input: SimulatorInput,
    },

    // Dictation.
    #[serde(rename = "dictation.start")]
    DictationStart,
    #[serde(rename = "dictation.hands-free")]
    DictationHandsFree,
    #[serde(rename = "dictation.stop")]
    DictationStop {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        send: Option<bool>,
    },
    #[serde(rename = "dictation.cancel")]
    DictationCancel,
    #[serde(rename = "dictation.download")]
    DictationDownload,
    #[serde(rename = "dictation.cancel-download")]
    DictationCancelDownload,

    // Diffs.
    #[serde(rename = "diff.toggle")]
    DiffToggle {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
    },
    #[serde(rename = "diff.refresh")]
    DiffRefresh,
    #[serde(rename = "diff.open-commit")]
    DiffOpenCommit {
        commit: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
    },
    #[serde(rename = "diff.set-range")]
    DiffSetRange { range: DiffRange },
    #[serde(rename = "diff.set-mode")]
    DiffSetMode { mode: DiffMode },
    #[serde(rename = "diff.set-collapsed")]
    DiffSetCollapsed { path: String, collapsed: bool },
    #[serde(rename = "diff.set-viewed")]
    DiffSetViewed { path: String, viewed: bool },
    #[serde(rename = "diff.set-split")]
    DiffSetSplit { split: bool },
    #[serde(rename = "diff.set-ignore-whitespace")]
    DiffSetIgnoreWhitespace { ignore: bool },

    // Applications on the machine, and app updates.
    #[serde(rename = "app.list")]
    AppList,
    #[serde(rename = "app.open-folder")]
    AppOpenFolder { app_id: String },
    #[serde(rename = "app.check-for-updates")]
    AppCheckForUpdates {
        #[serde(default, skip_serializing_if = "crate::util::is_false")]
        quiet: bool,
    },
    #[serde(rename = "app.download-update")]
    AppDownloadUpdate,
    #[serde(rename = "app.install-update")]
    AppInstallUpdate,
    #[serde(rename = "app.update")]
    AppUpdate {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        computer_id: Option<String>,
    },
    #[serde(rename = "app.cancel-update")]
    AppCancelUpdate {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        computer_id: Option<String>,
    },
    #[serde(rename = "app.open-source-licenses")]
    AppOpenSourceLicenses,

    // Terminal.
    #[serde(rename = "terminal.open")]
    TerminalOpen {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },
    #[serde(rename = "terminal.select")]
    TerminalSelect { terminal_id: String },
    #[serde(rename = "terminal.close")]
    TerminalClose { terminal_id: String },
    #[serde(rename = "terminal.restart")]
    TerminalRestart { terminal_id: String },
    #[serde(rename = "terminal.input")]
    TerminalInput { terminal_id: String, data: String },
    #[serde(rename = "terminal.resize")]
    TerminalResize { terminal_id: String, cols: i64, rows: i64 },

    // Phone bridge.
    #[serde(rename = "remote.set-enabled")]
    RemoteSetEnabled { enabled: bool },
    #[serde(rename = "remote.create-pairing-code")]
    RemoteCreatePairingCode,
    #[serde(rename = "remote.revoke-device")]
    RemoteRevokeDevice { device_id: String },
    #[serde(rename = "remote.refresh")]
    RemoteRefresh,

    // Paired computers.
    #[serde(rename = "computers.discover")]
    ComputersDiscover,
    #[serde(rename = "computers.pair")]
    ComputersPair { host: String, name: String, code: String },
    #[serde(rename = "computers.cancel-pairing")]
    ComputersCancelPairing,
    #[serde(rename = "computers.forget")]
    ComputersForget { id: String },
    #[serde(rename = "computers.reconnect")]
    ComputersReconnect {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    #[serde(rename = "computers.rename")]
    ComputersRename { name: String },
    #[serde(rename = "computers.label")]
    ComputersLabel { id: String, name: String },
    #[serde(rename = "computers.filter")]
    ComputersFilter { filter: String },

    // Engines.
    #[serde(rename = "engine.read")]
    EngineRead {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        refresh: Option<bool>,
    },
    #[serde(rename = "engine.sign-in")]
    EngineSignIn { engine: AgentEngine },
    #[serde(rename = "engine.update")]
    EngineUpdate {
        engine: AgentEngine,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        computer_id: Option<String>,
    },
    #[serde(rename = "engine.reload-settings")]
    EngineReloadSettings,

    // View.
    #[serde(rename = "view.mounted")]
    ViewMounted,
    #[serde(rename = "view.set-prompt")]
    SetPrompt {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        prompt: String,
    },
    #[serde(rename = "view.reading-point")]
    ReadingPoint { task_id: String, point: Option<ReadingPoint> },
    #[serde(rename = "view.dismiss-action-error")]
    DismissActionError,
    #[serde(rename = "view.dismiss-hidden-tasks")]
    DismissHiddenTasks,
    #[serde(rename = "view.dismiss-toast")]
    DismissToast { id: i64 },
    #[serde(rename = "view.edit-automation")]
    EditAutomation { task_id: String, base: AutomationFields, fields: AutomationFields },
    #[serde(rename = "view.hold-toasts")]
    HoldToasts { held: bool },
    #[serde(rename = "view.edit-project")]
    EditProject { project_id: Option<String> },
    #[serde(rename = "view.move-worktree")]
    MoveWorktree { worktree: Option<bool> },
    #[serde(rename = "view.fork-name")]
    ForkName {
        source_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worktree: Option<bool>,
    },
    #[serde(rename = "view.fork-confirm")]
    ForkConfirm { title: String },
    #[serde(rename = "view.archive-prompt")]
    ArchivePrompt { thread_id: Option<String> },
    #[serde(rename = "view.archive-confirm")]
    ArchiveConfirm,
    #[serde(rename = "view.set-section-open")]
    SetSectionOpen { section: SidebarSection, open: bool },
    #[serde(rename = "view.set-subagent-group")]
    SetSubagentGroup { group: SubagentGroup, open: bool },
    #[serde(rename = "view.set-overview-group")]
    SetOverviewGroup {
        task_id: String,
        group: OverviewSection,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        open: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        all: Option<bool>,
    },
    #[serde(rename = "view.set-model-favorite")]
    SetModelFavorite { engine: AgentEngine, model: AgentModel, favorite: bool },
    #[serde(rename = "view.remember-project")]
    RememberProject { project_id: String },
    #[serde(rename = "view.set-theme")]
    SetTheme { theme: String },
    #[serde(rename = "view.set-theme-family")]
    SetThemeFamily { family: String, system_dark: bool },
    #[serde(rename = "view.set-theme-mode")]
    SetThemeMode { mode: ThemeMode, system_dark: bool },
    #[serde(rename = "view.system-scheme")]
    SystemScheme { dark: bool },
    #[serde(rename = "view.set-ui-font")]
    SetUiFont { font: String },
    #[serde(rename = "view.set-mono-font")]
    SetMonoFont { font: String },
    #[serde(rename = "view.set-reading-size")]
    SetReadingSize { size: f64 },
    #[serde(rename = "view.set-terminal-size")]
    SetTerminalSize { size: f64 },
    #[serde(rename = "view.set-activity-project")]
    SetActivityProject { project_id: Option<String> },
    #[serde(rename = "view.set-sidebar-open")]
    SetSidebarOpen { open: bool },
    #[serde(rename = "view.set-session-panel-open")]
    SetSessionPanelOpen { open: bool },
    #[serde(rename = "view.set-zen")]
    SetZen { zen: bool },
    #[serde(rename = "view.set-capture-options")]
    SetCaptureOptions { options: CaptureOptions },
    #[serde(rename = "view.set-chrome-browser")]
    SetChromeBrowser { enabled: bool },
    #[serde(rename = "view.set-computer-use")]
    SetComputerUse { enabled: bool },
    #[serde(rename = "view.set-browser-tools")]
    SetBrowserTools { enabled: bool },
    #[serde(rename = "view.set-memory-protection")]
    SetMemoryProtection { enabled: bool },
    #[serde(rename = "view.set-notifications")]
    SetNotifications { enabled: bool },
    #[serde(rename = "view.inspect-subagent")]
    InspectSubagent {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        subagent_id: String,
    },
    #[serde(rename = "view.set-settings-open")]
    SetSettingsOpen {
        open: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        section: Option<SettingsSection>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        setting_id: Option<String>,
    },
    #[serde(rename = "view.set-usage-computer")]
    SetUsageComputer { computer_id: Option<String> },
    #[serde(rename = "view.close-tab")]
    CloseTab,
    #[serde(rename = "view.new-tab")]
    NewTab,
    #[serde(rename = "view.set-dock-open")]
    SetDockOpen { open: bool },
    #[serde(rename = "view.set-dock-expanded")]
    SetDockExpanded { expanded: bool },
    #[serde(rename = "view.open-dock-panel")]
    OpenDockPanel {
        panel: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
    },
    #[serde(rename = "view.close-dock-panel")]
    CloseDockPanel { panel: String },
    #[serde(rename = "view.close-thread-tab")]
    CloseThreadTab { task_id: String },
    #[serde(rename = "view.open-workflow")]
    OpenWorkflow { workflow_id: String },
    #[serde(rename = "view.select-dock-tab")]
    SelectDockTab { tab: String },
    #[serde(rename = "view.select-dock-index")]
    SelectDockIndex { index: i64 },
    #[serde(rename = "view.set-menu")]
    SetMenu { menu: Option<String> },
    #[serde(rename = "view.go-back")]
    GoBack,
    #[serde(rename = "view.go-forward")]
    GoForward,
    #[serde(rename = "view.set-focused")]
    SetFocused { focused: bool },
    #[serde(rename = "view.focus-composer")]
    FocusComposer {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recover: Option<bool>,
    },
    #[serde(rename = "view.shortcut")]
    Shortcut { action: String, surface: ShortcutSurface },
    #[serde(rename = "view.escape")]
    Escape,
    #[serde(rename = "view.set-shortcut")]
    SetShortcut { action: String, binding: Option<String> },
    #[serde(rename = "view.reset-shortcuts")]
    ResetShortcuts,
    #[serde(rename = "view.capture-shortcut")]
    CaptureShortcut { action: Option<String> },
    #[serde(rename = "view.dismiss-computer-use-setup")]
    DismissComputerUseSetup,
    #[serde(rename = "view.refresh-environment")]
    RefreshEnvironment,
    #[serde(rename = "view.dock-keys")]
    DockKeys { tab: Option<String> },
    #[serde(rename = "view.find-open")]
    FindOpen {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<FindTarget>,
    },
    #[serde(rename = "view.find-query")]
    FindQuery { query: String },
    #[serde(rename = "view.find-step")]
    FindStep { delta: i8 },
    #[serde(rename = "view.find-close")]
    FindClose,
    #[serde(rename = "view.jump-open")]
    JumpOpen,
    #[serde(rename = "view.jump-query")]
    JumpQuery { query: String },
    #[serde(rename = "view.jump-step")]
    JumpStep { delta: i8 },
    #[serde(rename = "view.jump-choose")]
    JumpChoose { task_id: String },
    #[serde(rename = "view.jump-choose-setting")]
    JumpChooseSetting {
        section: SettingsSection,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        setting_id: Option<String>,
    },
    #[serde(rename = "view.jump-close")]
    JumpClose,
}

/// A message on its way into a thread. Without `task_id` and with `text` it always starts a new thread.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskSend {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<RunAttachment>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steer: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<AgentModel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<AgentEngine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<AgentEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<ThreadRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<ThreadBrief>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<RemoteSender>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lead: Option<RemoteLead>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
}

impl AppCommand {
    /// The command's wire name, as its `type` field carries it.
    pub fn type_name(&self) -> String {
        serde_json::to_value(self).ok().and_then(|value| value.get("type").and_then(|kind| kind.as_str()).map(String::from)).unwrap_or_default()
    }

    /// The thread a command names, when it names one.
    pub fn task_id(&self) -> Option<&str> {
        use AppCommand::*;
        match self {
            TaskSelect { task_id }
            | TaskArchive { task_id }
            | TaskRestore { task_id }
            | TaskRename { task_id, .. }
            | TaskSetCoordinator { task_id, .. }
            | TaskSetLeadAway { task_id, .. }
            | TaskResolve { task_id }
            | TaskReopen { task_id }
            | DecisionAnswer { task_id, .. }
            | CoordinationSetThreadModel { task_id, .. }
            | CoordinationSetMuted { task_id, .. }
            | CoordinationStopAll { task_id }
            | MemorySetPurpose { task_id, .. }
            | MemoryAdd { task_id, .. }
            | MemoryEdit { task_id, .. }
            | MemoryForget { task_id, .. }
            | CoordinationNextStep { task_id, .. }
            | ProposalStart { task_id, .. }
            | ProposalDismiss { task_id, .. }
            | TaskSnooze { task_id, .. }
            | TaskUnsnooze { task_id }
            | TaskMove { task_id, .. }
            | PullRequestReadMember { task_id }
            | QuestionSetAnswer { task_id, .. }
            | QuestionAnswer { task_id, .. }
            | RunDecide { task_id, .. }
            | LimitResume { task_id }
            | ResourceResume { task_id }
            | LimitCancel { task_id }
            | WorktreeOpenThread { task_id }
            | AutomationNotify { task_id, .. }
            | AutomationNothingToReport { task_id, .. }
            | ReadingPoint { task_id, .. }
            | EditAutomation { task_id, .. }
            | SetOverviewGroup { task_id, .. }
            | CloseThreadTab { task_id }
            | JumpChoose { task_id } => Some(task_id),
            TaskSetRole { task_id, .. }
            | TaskDismiss { task_id }
            | TaskFork { task_id, .. }
            | TaskSetPolicy { task_id, .. }
            | TaskSetModel { task_id, .. }
            | TaskSetFastMode { task_id, .. }
            | TaskSetEffort { task_id, .. }
            | TaskSetWorktree { task_id, .. }
            | TaskMoveWorktree { task_id, .. }
            | TaskCheckoutBranch { task_id, .. }
            | AttachmentsSend { task_id, .. }
            | AttachmentsNotice { task_id, .. }
            | AttachmentsDrop { task_id, .. }
            | AttachmentsPasteImages { task_id, .. }
            | TaskSteerQueued { task_id, .. }
            | TaskDropQueued { task_id, .. }
            | TaskEditQueued { task_id, .. }
            | AnnotationAdd { task_id, .. }
            | AnnotationNote { task_id, .. }
            | AnnotationRemove { task_id, .. }
            | AnnotationRecall { task_id, .. }
            | AnnotationReveal { task_id, .. }
            | PasteAdd { task_id, .. }
            | PasteRemove { task_id, .. }
            | PasteRecall { task_id, .. }
            | ImageAdd { task_id, .. }
            | ImageRemove { task_id, .. }
            | ImageRecall { task_id, .. }
            | FileOpen { task_id, .. }
            | FileAttach { task_id, .. }
            | FileDetach { task_id, .. }
            | FileRecall { task_id, .. }
            | PullRequestRead { task_id }
            | PullRequestReadMembers { task_id }
            | RunCancel { task_id }
            | RunCompact { task_id }
            | RunStopProcess { task_id, .. }
            | ReviewOpen { task_id }
            | ReviewStart { task_id, .. }
            | WorktreeDelete { task_id, .. }
            | AutomationSave { task_id, .. }
            | AutomationUpdate { task_id, .. }
            | AutomationDelete { task_id }
            | AutomationRunNow { task_id }
            | BrowserOpen { task_id, .. }
            | BrowserCloseTab { task_id, .. }
            | BrowserSelectTab { task_id, .. }
            | BrowserGo { task_id, .. }
            | BrowserReload { task_id, .. }
            | BrowserAct { task_id, .. }
            | SimulatorOpen { task_id, .. }
            | SimulatorInput { task_id, .. }
            | DiffToggle { task_id }
            | DiffOpenCommit { task_id, .. }
            | SetPrompt { task_id, .. }
            | InspectSubagent { task_id, .. }
            | OpenDockPanel { task_id, .. } => task_id.as_deref(),
            TaskSend(send) => send.task_id.as_deref(),
            _ => None,
        }
    }
}
