//! The composer under the thread: the prompt, its settings, the plan and context meters, and Send
//! or Stop (ConversationComposer.tsx, WorkspaceComposer.tsx, composer-input.ts, composer-recall.ts).

use std::collections::HashSet;

use act_core::domain::conversation::{OutgoingAttachment, RecalledMessage, MAX_ATTACHMENTS};
use act_core::domain::engine::{engine_label, thread_model};
use act_core::domain::platform::DictationModel;
use act_core::reducer::sending::prompt_key;
use act_core::state::WorkspaceState;
use act_core::workspace::activity::waiting_on;
use act_core::workspace::location::project_for;
use act_core::AppCommand;
use act_ui::components::button::{IconButton, IconButtonSize};
use act_ui::components::notice::{Notice, NoticeTone};
use act_ui::components::scrollbar::{model_gutter, Scrollbar};
use act_ui::input::{self as text_input, Autosize, InputEvent, TextInput};
use act_ui::overlay::{bounds_cell, bounds_probe, BoundsCell};
use act_ui::style::{Corners, Raised};
use act_ui::theme::ActiveTheme;
use act_ui::tooltip::Tippable;
use act_ui::IconName;
use gpui::{div, prelude::*, px, relative, BoxShadow, Context, Entity, ExternalPaths, KeyDownEvent, Subscription, Window};

use super::common::icon;
use super::root::Root;
use super::{composer_attached, composer_dictation, composer_menus, composer_meters, composer_question, model_library, queued_row};
use crate::workspace::dispatch;

/// A paste past either of these rides as a pill rather than filling the prompt.
const PASTE_PILL_LINES: usize = 12;
const PASTE_PILL_CHARS: usize = 900;

pub fn paste_rides_as_pill(text: &str) -> bool {
    !text.trim().is_empty() && (text.matches('\n').count() + 1 > PASTE_PILL_LINES || text.encode_utf16().count() > PASTE_PILL_CHARS)
}

/// Where ↑/↓ sits in the sent history, with the draft it replaced and the text it put on screen.
struct Recall {
    index: usize,
    draft: RecalledMessage,
    shown: String,
}

pub struct ComposerState {
    /// The dock tab whose thread this composer writes to; None for the page's own composer.
    pub task: Option<String>,
    /// A dock composer that is a side chat rather than a thread tab.
    pub side: bool,
    /// A dock composer that cannot send, as a side chat whose main thread has no context yet.
    pub held: bool,
    pub input: Entity<TextInput>,
    /// The strip the composer stands in, which the transcript leaves clear at its foot.
    pub probe: BoundsCell,
    pub settings_probe: BoundsCell,
    /// The composer's own pane, whose width the bar fits its controls to.
    pub pane_probe: BoundsCell,
    /// The composer whose draft the field holds.
    key: String,
    /// The draft as last read from the workspace, so typing does not echo back into the field.
    text: String,
    pub caret: usize,
    focus_token: i64,
    was_focused_window: bool,
    pub focused: bool,
    /// The prompt whose menus were dismissed, so typing on brings them back.
    pub dismissed: Option<String>,
    pasting: bool,
    recall: Option<Recall>,
    pub menus: composer_menus::MenuState,
    /// What the composer itself has to say that is not a failure, such as a limit it kept to.
    pub note: Option<String>,
    pub queued_open: HashSet<String>,
    pub plan_card: bool,
    pub context_card: bool,
    /// The plan and context meters, which the keyboard reaches to show their cards.
    pub plan_focus: gpui::FocusHandle,
    pub context_focus: gpui::FocusHandle,
    pub paste_viewing: Option<String>,
    pub dropping: bool,
    pub dictation: Option<composer_dictation::Dictation>,
    pub question: composer_question::QuestionState,
    pub needs_you: super::needs_you::NeedsYouState,
    pub annotator: Option<super::annotator::Annotator>,
    /// Marks drawn on staged screenshots, by image id.
    pub marks: std::collections::HashMap<String, super::annotator::ImageMarks>,
    /// Set while marks are drawn into a send, so a second send cannot start on top of it.
    drawing: bool,
    pub goal_open: bool,
    pub annotation_hover: Option<String>,
    pub chip_hover: bool,
    /// The model chip's keyboard stop, which takes focus back as the library closes.
    pub chip_focus: Option<gpui::FocusHandle>,
    /// Whether the send button showed Stop when last drawn.
    pub stop_shown: bool,
    pub start: super::thread_start::StartState,
    pub review: super::review_picker::ReviewState,
    sends_seen: Option<i64>,
    _subscriptions: Vec<Subscription>,
}

/// The padding above and below the prompt, which its text scrolls through.
const FIELD_TOP: f32 = 15.0;
const FIELD_BOTTOM: f32 = 5.0;

impl ComposerState {
    pub fn new(window: &mut Window, cx: &mut Context<Root>) -> ComposerState {
        ComposerState::scoped(None, window, cx)
    }

    /// A composer writing to `task`, or the page's own composer when None.
    pub fn scoped(task: Option<String>, window: &mut Window, cx: &mut Context<Root>) -> ComposerState {
        let input = cx.new(|cx| {
            let mut input = TextInput::new(cx).multiline(Autosize { min_rows: 1, max_height: Some(px(420.0)) }).submit_on_enter(true).placeholder(format!("Ask {} anything", engine_label(act_core::domain::engine::DEFAULT_ENGINE))).label(if task.is_none() { "Task prompt" } else { "Thread prompt" });
            // The prompt scrolls through the field's padding, as a textarea's text does.
            input.set_bleed(px(FIELD_TOP), px(FIELD_BOTTOM), cx);
            input
        });
        let scope = task.clone();
        let events = cx.subscribe_in(&input, window, move |root, input, event: &InputEvent, _, cx| {
            root.in_composer(scope.as_deref(), |root| match event {
                InputEvent::Changed => {
                    let text = input.read(cx).text().to_string();
                    let composer = &mut root.composer;
                    composer.caret = input.read(cx).cursor();
                    composer.dismissed = composer.pasting.then(|| text.clone());
                    composer.pasting = false;
                    if text != composer.text {
                        composer.text = text.clone();
                        if composer.recall.as_ref().is_some_and(|recall| recall.shown != text) {
                            composer.recall = None;
                        }
                        dispatch(AppCommand::SetPrompt { task_id: root.composer.task.clone(), prompt: text }, cx);
                    }
                    cx.notify();
                }
                InputEvent::Submit => {
                    let running = root.running(cx);
                    root.submit(running, cx);
                }
                InputEvent::Focus => {
                    root.composer.focused = true;
                    cx.notify();
                }
                InputEvent::Blur => {
                    root.composer.focused = false;
                    cx.notify();
                }
            });
        });
        let scope = task.clone();
        let caret = cx.observe(&input, move |root, input, cx| {
            root.in_composer(scope.as_deref(), |root| {
                let caret = input.read(cx).cursor();
                if caret != root.composer.caret {
                    root.composer.caret = caret;
                    cx.notify();
                }
            });
        });
        let dictation = if task.is_none() { composer_dictation::Dictation::new(cx) } else { None };
        ComposerState {
            task,
            side: false,
            held: false,
            input,
            probe: bounds_cell(),
            settings_probe: bounds_cell(),
            pane_probe: bounds_cell(),
            key: String::new(),
            text: String::new(),
            caret: 0,
            focus_token: -1,
            was_focused_window: true,
            focused: false,
            dismissed: None,
            pasting: false,
            recall: None,
            menus: composer_menus::MenuState::default(),
            note: None,
            queued_open: HashSet::new(),
            plan_card: false,
            context_card: false,
            plan_focus: cx.focus_handle().tab_stop(true),
            context_focus: cx.focus_handle().tab_stop(true),
            paste_viewing: None,
            dropping: false,
            dictation,
            question: composer_question::QuestionState::default(),
            needs_you: Default::default(),
            annotator: None,
            marks: Default::default(),
            drawing: false,
            goal_open: false,
            annotation_hover: None,
            chip_hover: false,
            chip_focus: None,
            stop_shown: false,
            start: Default::default(),
            review: Default::default(),
            sends_seen: None,
            _subscriptions: vec![events, caret],
        }
    }

    /// The composer's height over the page, which the transcript leaves clear at its foot.
    /// How tall the composer's footer stands, its 12px top and 20px bottom padding included.
    pub fn height(&self) -> f32 {
        self.probe.get().map(|bounds| f32::from(bounds.size.height)).unwrap_or(118.0) + 32.0
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// The thread this composer sends to.
    pub fn task_id(&self, state: &WorkspaceState) -> Option<String> {
        self.task.clone().or_else(|| state.current_id.clone())
    }

    pub fn thread<'a>(&self, state: &'a WorkspaceState) -> Option<&'a act_core::domain::thread::Thread> {
        match &self.task {
            Some(id) => state.thread(id),
            None => state.current_thread(),
        }
    }

    /// Where this composer's draft is kept.
    pub fn key(&self, state: &WorkspaceState) -> String {
        self.task.clone().unwrap_or_else(|| prompt_key(state))
    }

    /// Whether this composer cannot take a message.
    pub fn disabled(&self, state: &WorkspaceState) -> bool {
        match self.task {
            Some(_) => self.held,
            None => !state.restored && state.current_thread().is_none(),
        }
    }

    /// Takes the draft of whichever composer is in front, and the caret when the workspace asks for it.
    /// `shown` is the state the thread on screen is drawn from, whose engine and folder the placeholder names.
    pub fn sync(&mut self, state: &WorkspaceState, shown: &WorkspaceState, cx: &mut Context<Root>) {
        self.sync_draft(state, cx);
        let engine = shown.current_thread().map(|thread| thread.engine).unwrap_or(shown.draft_engine);
        let folder = match shown.current_thread() {
            Some(thread) => project_for(shown, Some(thread)).is_some(),
            None => shown.draft_project_id.is_some(),
        };
        let placeholder = if folder { format!("Ask {} to work on anything", engine_label(engine)) } else { format!("Ask {} anything", engine_label(engine)) };
        self.sync_field(placeholder, !state.restored && shown.current_thread().is_none(), cx);
        self.sync_rest(state, cx);
    }

    /// Shows the placeholder and disabled state asked for, touching the field only when they change.
    pub fn sync_field(&mut self, placeholder: String, disabled: bool, cx: &mut Context<Root>) {
        if self.input.read(cx).placeholder_text().as_ref() != placeholder {
            self.input.update(cx, |input, cx| input.set_placeholder(placeholder, cx));
        }
        if self.input.read(cx).is_disabled() != disabled {
            self.input.update(cx, |input, cx| input.set_disabled(disabled, cx));
        }
    }

    /// Takes this composer's draft from the workspace, unless the field already shows it.
    pub fn sync_draft(&mut self, state: &WorkspaceState, cx: &mut Context<Root>) {
        let key = self.key(state);
        let text = state.prompts.get(&key).cloned().unwrap_or_default();
        if key != self.key || text != self.text {
            let current = self.input.read(cx).text().to_string();
            if current != text {
                self.input.update(cx, |input, cx| input.reset(&text, cx));
            }
            if key != self.key {
                self.note = None;
                self.dismissed = None;
                self.recall = None;
                self.paste_viewing = None;
            }
            if self.recall.as_ref().is_some_and(|recall| recall.shown != text) {
                self.recall = None;
            }
            self.key = key;
            self.text = text;
            self.caret = self.input.read(cx).cursor();
        }
    }

    fn sync_rest(&mut self, state: &WorkspaceState, cx: &mut Context<Root>) {
        if state.composer_focus != self.focus_token {
            let first = self.focus_token < 0;
            self.focus_token = state.composer_focus;
            let input = self.input.clone();
            if let Some(window) = cx.windows().first().copied().filter(|_| !first || state.composer_focus > 0) {
                cx.defer(move |cx| {
                    let _ = window.update(cx, |_, window, cx| {
                        input.update(cx, |input, cx| {
                            input.move_to_end(cx);
                            input.focus(window, cx);
                        })
                    });
                });
            }
        }
        if state.focused && !self.was_focused_window {
            self.menus.commands.stale = true;
        }
        self.was_focused_window = state.focused;
        let sends = state.dictation.sends;
        if self.sends_seen.is_some_and(|seen| seen != sends) {
            // Spoken words landed with a send asked of them, so the draft goes as Enter sends it.
            let root = cx.entity().downgrade();
            cx.defer(move |cx| {
                let _ = root.update(cx, |root, cx| {
                    let running = root.running(cx);
                    root.submit_now(running, cx);
                });
            });
        }
        self.sends_seen = Some(sends);
        if let Some(dictation) = &mut self.dictation {
            dictation.sync(state, cx);
            dictation.window_left(state, cx);
        }
    }
}

/// Whether anything rides the next send besides the text.
pub fn carries_drafts(state: &WorkspaceState, key: &str) -> bool {
    state.annotations.get(key).is_some_and(|items| !items.is_empty())
        || state.pastes.get(key).is_some_and(|items| !items.is_empty())
        || state.files.get(key).is_some_and(|items| !items.is_empty())
        || state.images.get(key).is_some_and(|items| !items.is_empty())
}

impl Root {
    /// Runs `f` with `task`'s dock composer standing in as `root.composer`; None runs it on the
    /// composer in place. Nothing runs once that composer is gone.
    pub fn in_composer<R>(&mut self, task: Option<&str>, f: impl FnOnce(&mut Root) -> R) -> Option<R> {
        let Some(task) = task.filter(|task| self.composer.task.as_deref() != Some(*task)) else { return Some(f(self)) };
        let mut held = self.dock.chats.take_composer(task)?;
        std::mem::swap(&mut self.composer, &mut held);
        let result = f(self);
        std::mem::swap(&mut self.composer, &mut held);
        self.dock.chats.put_composer(task, held);
        Some(result)
    }

    /// `cx.listener` for the composer in place, which runs against that same composer.
    pub fn composer_listener<E: ?Sized, F: Fn(&mut Root, &E, &mut Window, &mut Context<Root>) + 'static>(&self, cx: &Context<Root>, f: F) -> impl Fn(&E, &mut Window, &mut gpui::App) + 'static + use<E, F> {
        let task = self.composer.task.clone();
        let root = cx.entity().downgrade();
        move |event: &E, window: &mut Window, cx: &mut gpui::App| {
            let _ = root.update(cx, |root, cx| {
                root.in_composer(task.as_deref(), |root| f(root, event, window, cx));
            });
        }
    }

    pub fn running(&self, cx: &gpui::App) -> bool {
        let state = self.shown(cx);
        self.composer.task_id(&state).is_some_and(|id| state.active_runs.contains_key(&id))
    }

    fn hands_free(&self, cx: &gpui::App) -> bool {
        self.composer.task.is_none() && act_core::view::workspace::dictation_view(&self.state(cx)).hands_free
    }

    /// While a run is going the message joins the queue, so only steering needs the run to be active.
    /// Hands-free dictation sends once its words are in.
    pub fn submit(&mut self, steer: bool, cx: &mut Context<Root>) {
        if self.hands_free(cx) {
            dispatch(AppCommand::DictationStop { send: Some(true) }, cx);
            return;
        }
        self.submit_now(steer, cx);
    }

    fn submit_now(&mut self, steer: bool, cx: &mut Context<Root>) {
        // The thread on screen, which may be another computer's; this window's own current thread is not it then.
        let state = self.shown(cx);
        let key = self.composer.key(&state);
        let task_id = self.composer.task_id(&state);
        let disabled = self.composer.disabled(&state);
        let sending = state.attachment_send_for(task_id.as_deref()).busy;
        let running = task_id.as_ref().is_some_and(|id| state.active_runs.contains_key(id));
        if sending || disabled || waiting_on(&state, task_id.as_deref(), &key).is_some() || (steer && !running) {
            return;
        }
        let prompt = self.composer.input.read(cx).text().to_string();
        if prompt.trim().is_empty() && !carries_drafts(&state, &key) {
            return;
        }
        if self.composer.drawing {
            return;
        }
        let images = state.images.get(&key).cloned().unwrap_or_default();
        let mut attachments: Vec<OutgoingAttachment> = images
            .iter()
            .map(|image| OutgoingAttachment { id: image.id.clone(), source: String::new(), annotations: self.composer.marks.get(&image.id).map(|marks| marks.annotations.clone()).unwrap_or_default(), path: Some(image.path.clone()), context: None })
            .collect();
        self.composer.note = None;
        self.composer.recall = None;
        if attachments.iter().all(|attachment| attachment.annotations.is_empty()) {
            dispatch(AppCommand::AttachmentsSend { task_id, steer: steer.then_some(true), attachments, text: None }, cx);
            return;
        }
        // A mark's letter follows the image's place in the send, so the marks are drawn as they go.
        let total = attachments.len();
        let drawn: Vec<_> = attachments
            .iter()
            .enumerate()
            .map(|(at, attachment)| (!attachment.annotations.is_empty()).then(|| super::mark_render::render(attachment.path.as_deref().unwrap_or_default(), &attachment.annotations, &act_core::reducer::run_queue::mark_prefix(at, total), cx)))
            .collect();
        self.composer.drawing = true;
        cx.spawn(async move |root, cx| {
            let mut failure = None;
            for (attachment, drawn) in attachments.iter_mut().zip(drawn) {
                let Some(drawn) = drawn else { continue };
                match drawn.await {
                    Ok(Ok(png)) => attachment.source = format!("data:image/png;base64,{png}"),
                    Ok(Err(error)) => failure = Some(error),
                    Err(_) => failure = Some("The marks could not be drawn.".to_string()),
                }
            }
            let _ = root.update(cx, |root, cx| {
                root.composer.drawing = false;
                let state = root.state(cx);
                if prompt_key(&state) != key {
                    return;
                }
                match failure {
                    Some(message) => dispatch(AppCommand::AttachmentsNotice { task_id: None, message: Some(message) }, cx),
                    None => dispatch(AppCommand::AttachmentsSend { task_id, steer: steer.then_some(true), attachments, text: None }, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Steps ↑/↓ through what was sent; the live draft is stashed and comes back below the newest.
    fn step_recall(&mut self, step: i64, cx: &mut Context<Root>) -> bool {
        let state = self.shown(cx);
        let prompt = self.composer.text.clone();
        if self.composer.recall.is_none() && (step == 1 || !prompt.is_empty()) {
            return false;
        }
        let key = self.composer.key(&state);
        let task = self.composer.task.clone();
        let mut sent: Vec<RecalledMessage> = self.composer.thread(&state).map(|thread| act_core::domain::conversation::sent_prompts(&thread.messages)).unwrap_or_default();
        if let Some(queued) = self.composer.task_id(&state).and_then(|id| state.queued_messages.get(&id)) {
            sent.extend(queued.iter().map(|message| RecalledMessage {
                text: message.held.text.clone(),
                annotations: message.held.annotations.clone().unwrap_or_default(),
                pastes: message.held.pastes.clone().unwrap_or_default(),
                files: message.held.files.clone().unwrap_or_default(),
                attachments: message.held.attachments.clone(),
            }));
        }
        let carries = |message: &RecalledMessage| !message.text.trim().is_empty() || !message.annotations.is_empty() || !message.pastes.is_empty() || !message.files.is_empty() || !message.attachments.is_empty();
        let recallable: Vec<RecalledMessage> = sent
            .iter()
            .enumerate()
            .filter(|(index, message)| carries(message) && !(!message.text.is_empty() && index.checked_sub(1).and_then(|previous| sent.get(previous)).is_some_and(|previous| previous.text == message.text)))
            .map(|(_, message)| message.clone())
            .collect();
        let at = self.composer.recall.as_ref().map(|recall| recall.index).unwrap_or(recallable.len()) as i64 + step;
        if at < 0 {
            return false;
        }
        let index = at as usize;
        let draft = match &self.composer.recall {
            Some(recall) => recall.draft.clone(),
            None => RecalledMessage {
                text: prompt,
                annotations: state.annotations.get(&key).cloned().unwrap_or_default(),
                pastes: state.pastes.get(&key).cloned().unwrap_or_default(),
                files: state.files.get(&key).cloned().unwrap_or_default(),
                attachments: state.images.get(&key).map(|images| images.iter().map(|image| image.path.clone()).collect()).unwrap_or_default(),
            },
        };
        let next = if index >= recallable.len() { draft.clone() } else { recallable[index].clone() };
        self.composer.recall = (index < recallable.len()).then(|| Recall { index, draft, shown: next.text.clone() });
        self.composer.text = next.text.clone();
        self.composer.input.update(cx, |input, cx| input.reset(&next.text, cx));
        self.composer.dismissed = Some(next.text.clone());
        dispatch(AppCommand::SetPrompt { task_id: task.clone(), prompt: next.text.clone() }, cx);
        dispatch(AppCommand::AnnotationRecall { task_id: task.clone(), annotations: next.annotations }, cx);
        dispatch(AppCommand::PasteRecall { task_id: task.clone(), pastes: next.pastes }, cx);
        dispatch(AppCommand::FileRecall { task_id: task.clone(), files: next.files }, cx);
        dispatch(AppCommand::ImageRecall { task_id: task.clone(), paths: next.attachments }, cx);
        true
    }

    /// A paste of images or files attaches them; a long paste rides as a pill; anything else is typed.
    fn paste(&mut self, cx: &mut Context<Root>) -> bool {
        let files = crate::effects::window_desktop::pasteboard_files();
        if !files.is_empty() {
            super::attachments::attach_paths(files, self.composer.task.clone(), cx);
            return true;
        }
        let Some(item) = cx.read_from_clipboard() else { return false };
        let images: Vec<gpui::Image> = item
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                gpui::ClipboardEntry::Image(image) => Some(image.clone()),
                _ => None,
            })
            .collect();
        if !images.is_empty() {
            let state = self.state(cx);
            let held = state.images.get(&self.composer.key(&state)).map(Vec::len).unwrap_or(0);
            let room = MAX_ATTACHMENTS.saturating_sub(held);
            if room == 0 {
                self.composer.note = Some(format!("You can attach up to {MAX_ATTACHMENTS} images."));
            } else {
                self.composer.note = (images.len() > room).then(|| format!("Only the first {room} image{} were attached.", if room == 1 { "" } else { "s" }));
                super::attachments::attach_images(images.into_iter().take(room).collect(), self.composer.task.clone(), cx);
            }
            dispatch(AppCommand::AttachmentsNotice { task_id: self.composer.task.clone(), message: None }, cx);
            cx.notify();
            return true;
        }
        let text = item.text().unwrap_or_default();
        if paste_rides_as_pill(&text) {
            dispatch(AppCommand::PasteAdd { task_id: self.composer.task.clone(), text }, cx);
            return true;
        }
        self.composer.pasting = true;
        false
    }

    /// Opens the system's file panel; what is picked rides the next send the way a drop would.
    pub fn attach_files(&mut self, _window: &mut Window, cx: &mut Context<Root>) {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions { files: true, directories: true, multiple: true, prompt: Some("Attach".into()) });
        let task = self.composer.task.clone();
        cx.spawn(async move |_, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                let _ = cx.update(|cx| super::attachments::attach_paths(paths, task, cx));
            }
        })
        .detach();
    }

    /// Files dropped anywhere on the workspace land in this composer.
    pub fn drop_files(&mut self, paths: &ExternalPaths, cx: &mut Context<Root>) {
        self.composer.dropping = false;
        super::attachments::attach_paths(paths.paths().to_vec(), self.composer.task.clone(), cx);
        cx.notify();
    }

    /// Whether the caret sits on the prompt's first (`up`) or last line, where ↑/↓ recall.
    fn at_edge(&self, up: bool, cx: &gpui::App) -> bool {
        let input = self.composer.input.read(cx);
        let text = input.text();
        let selection = input.selection();
        if up {
            !text[..selection.start].contains('\n')
        } else {
            !text[selection.end..].contains('\n')
        }
    }
}

pub fn render(root: &mut Root, state: &WorkspaceState, window: &mut Window, cx: &mut Context<Root>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let (inset, side) = super::session_panel::composer_insets(state, f32::from(window.viewport_size().width), theme.metrics.page_inset(f32::from(window.viewport_size().width)));
    let probe = root.composer.probe.clone();
    let column = panel(root, state, None, window, cx);
    // After the session card and the dock, as the composer follows them in the page.
    div()
        .tab_order(1)
        .absolute()
        .left_0()
        .right_0()
        .bottom_0()
        .pt(px(12.0))
        .pl(px(inset))
        .pr(px(inset + side))
        .pb(px(20.0))
        .rounded_b(px(theme.metrics.page_radius))
        .child(div().relative().occlude().child(column).child(bounds_probe(&probe)))
}

/// Send, Stop while a run is going, or a spinner while a send is on its way.
fn send_button(state: &WorkspaceState, task: Option<String>, side: bool, run_active: bool, disabled: bool, busy: bool, cx: &mut Context<Root>) -> gpui::Stateful<gpui::Div> {
    let t = cx.theme().tokens.clone();
    let page = task.is_none();
    let (stop_label, send_label) = if side {
        ("Stop side chat", "Send side chat message")
    } else if page {
        ("Stop task", "Send task")
    } else {
        ("Stop thread", "Send message")
    };
    let base = div().id("send").flex_none().size(px(32.0)).rounded_full().flex().items_center().justify_center().transition(act_ui::style::touch());
    let press = |button: gpui::Stateful<gpui::Div>| button.active(|style| style.transform(gpui::ElementTransform::scale(0.92)));
    if run_active {
        let task_id = task.clone().or_else(|| state.current_id.clone());
        return press(base)
            .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, stop_label))
            .bg(t.surface_inverse)
            .cursor_pointer()
            .child(div().size(px(10.0)).rounded(px(2.0)).bg(t.ink_inverse))
            .tip(super::chrome::key_tip("Stop", "Escape"))
            .on_click(move |_, _, cx| dispatch(AppCommand::RunCancel { task_id: task_id.clone() }, cx));
    }
    let off = disabled || busy;
    base.accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, send_label).disabled(off))
        .map(|button| if disabled { button.bg(t.surface_inverse_off) } else { button.bg(t.surface_inverse) })
        .when(!off, |button| press(button.cursor_pointer()))
        .child(if busy {
            act_ui::components::spinner::Spinner::new("send-spinner").size(px(14.0)).color(t.ink_inverse).into_any_element()
        } else {
            icon(IconName::ArrowUp, 16.0, if disabled { t.muted } else { t.ink_inverse }).stroke(2.5).into_any_element()
        })
        .tip(super::chrome::key_tip("Send", "Enter"))
        .when(!off, |button| {
            button.on_click(cx.listener(move |root, _, _, cx| {
                root.in_composer(task.as_deref(), |root| root.submit(false, cx));
            }))
        })
}

/// The composer and what stands over it, centred in its column: the goal, the question, queued
/// messages, a usage warning, the start options a new thread offers, a notice, then the composer.
pub fn panel(root: &mut Root, state: &WorkspaceState, start: Option<gpui::AnyElement>, window: &mut Window, cx: &mut Context<Root>) -> gpui::Div {
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let m = &theme.metrics;
    let thread = root.composer.thread(state);
    let docked = root.composer.task.is_some();
    let engine = thread.map(|thread| thread.engine).unwrap_or(state.draft_engine);
    let catalog = state.catalog();
    let model = match thread {
        Some(thread) => thread_model(&catalog, engine, thread.model.as_deref()),
        None => Some(state.draft_model.clone()),
    };
    let key = root.composer.key(state);
    let task_id = root.composer.task_id(state);
    let run_active = thread.is_some_and(|thread| state.active_runs.contains_key(&thread.id));
    let waiting = waiting_on(state, thread.map(|thread| thread.id.as_str()), &key).is_some();
    let send_state = state.attachment_send_for(task_id.as_deref());
    let disabled = root.composer.disabled(state);
    let nothing = root.composer.text.trim().is_empty() && !carries_drafts(state, &key);
    let dictation = act_core::view::workspace::dictation_view(state);
    let hands_free = dictation.hands_free && !docked;
    let dictating = dictation.phase.filter(|_| !docked);
    let width = root.composer.pane_probe.get().map(|bounds| f32::from(bounds.size.width)).unwrap_or(860.0);
    // The bar's container queries read the width drawn last frame, so a frame that changes it draws again.
    let pane_probe = root.composer.pane_probe.clone();
    cx.on_next_frame(window, move |_, _, cx| {
        if pane_probe.get().map(|bounds| f32::from(bounds.size.width)).unwrap_or(860.0) != width {
            cx.notify();
        }
    });
    let viewport_height = f32::from(window.viewport_size().height);
    let max_height = px((viewport_height * 0.5).min(440.0) - 20.0);
    if root.composer.input.read(cx).autosize().max_height != Some(max_height) {
        root.composer.input.update(cx, |input, cx| input.set_autosize(Autosize { min_rows: 1, max_height: Some(max_height) }, cx));
    }
    composer_menus::ensure_commands(root, state, cx);

    let mut actions = div().flex_none().flex().items_center().gap(px(if width <= 470.0 { 8.0 } else { 12.0 }));
    if dictating.is_none() {
        if let Some(model) = model.as_ref().filter(|_| !docked) {
            if let Some(meter) = composer_meters::plan_meter(root, state, engine, model, width, window, cx) {
                actions = actions.child(meter);
            }
        }
        if let Some(meter) = composer_meters::context_meter(root, state, width, window, cx) {
            actions = actions.child(meter);
        }
    }
    if state.dictation.model == DictationModel::Ready && !docked {
        if let Some(mic) = composer_dictation::mic(root, state, disabled, cx) {
            actions = actions.child(mic);
        }
    }
    // A Stop the keyboard was on that ends with nothing left to send hands the keys to the field.
    let stopping = run_active && !hands_free;
    if std::mem::replace(&mut root.composer.stop_shown, stopping) && !stopping {
        let input = root.composer.input.clone();
        cx.on_next_frame(window, move |_, window, cx| {
            if window.focus_stranded(cx) {
                input.update(cx, |input, cx| input.focus(window, cx));
            }
        });
    }
    let send = send_button(state, root.composer.task.clone(), root.composer.side, run_active && !hands_free, disabled || (nothing && !hands_free), !disabled && (send_state.busy || waiting), cx);
    let send_hidden = dictating.is_some() && (!hands_free || run_active);
    actions = actions.child(if send_hidden { send.invisible() } else { send });

    let narrow = width <= 300.0;
    let mut leading = div().flex_1().min_w_0().overflow_hidden().flex().items_center().gap(px(if narrow { 6.0 } else { 8.0 }));
    if let Some(phase) = dictating {
        leading = leading.child(composer_dictation::strip(root, phase, hands_free, width, cx));
    } else {
        leading = leading
            .child(IconButton::new("attach", IconName::Paperclip, "Attach files").size(IconButtonSize::Md).round(true).disabled(disabled).on_click(root.composer_listener(cx, |root, _, window, cx| root.attach_files(window, cx))))
            .child(model_library::trigger(root, state, engine, model.as_deref(), width, cx));
    }
    let bar = div().flex().items_center().justify_between().gap(px(if narrow { 6.0 } else { 8.0 })).pt(px(7.0)).pr(px(9.0)).pb(px(9.0)).pl(px(if narrow { 10.0 } else { 8.0 })).child(leading).child(actions);

    let scroll = root.composer.input.read(cx).scroll_model();
    let field = div()
        .id("composer-field")
        .relative()
        .min_h(px(64.0))
        .pt(px(FIELD_TOP))
        .pl(px(16.0))
        .pr(px(16.0) + model_gutter(&scroll))
        .pb(px(FIELD_BOTTOM))
        .text_size(px(m.text_content))
        .line_height(relative(1.5))
        .text_color(t.ink)
        .cursor_text()
        // A press on the padding around the text places the caret as a press on the text does.
        .on_mouse_down(gpui::MouseButton::Left, {
            let input = root.composer.input.clone();
            move |event, window, cx| {
                if !window.default_prevented() {
                    window.prevent_default();
                    input.update(cx, |input, cx| input.press(event, window, cx));
                }
            }
        })
        .on_scroll({
            let input = root.composer.input.clone();
            move |event, window, cx| input.update(cx, |input, cx| input.scroll_wheel(event, window, cx))
        })
        .capture_action(root.composer_listener(cx, |root, _: &text_input::Enter, _, cx| {
            if root.menu_choose(cx) {
                cx.stop_propagation();
                return;
            }
            let state = root.state(cx);
            if state.attachment_send_for(root.composer.task_id(&state).as_deref()).busy {
                cx.stop_propagation();
            }
        }))
        .capture_action(root.composer_listener(cx, |root, _: &text_input::Up, _, cx| {
            if root.menu_step(-1, cx) || (!root.command_menu_open(cx) && root.at_edge(true, cx) && root.step_recall(-1, cx)) {
                cx.stop_propagation();
            }
        }))
        .capture_action(root.composer_listener(cx, |root, _: &text_input::Down, _, cx| {
            if root.menu_step(1, cx) || (!root.command_menu_open(cx) && root.at_edge(false, cx) && root.step_recall(1, cx)) {
                cx.stop_propagation();
            }
        }))
        .capture_action(root.composer_listener(cx, |root, _: &text_input::Paste, _, cx| {
            if root.paste(cx) {
                cx.stop_propagation();
            }
        }))
        .on_key_down(root.composer_listener(cx, |root, event: &KeyDownEvent, window, cx| {
            let keys = &event.keystroke;
            if keys.key == "enter" && (keys.modifiers.platform || keys.modifiers.control) {
                let state = root.state(cx);
                if !state.attachment_send_for(root.composer.task_id(&state).as_deref()).busy {
                    root.submit(false, cx);
                }
                cx.stop_propagation();
            } else if keys.key == "tab" && !keys.modifiers.shift && root.menu_choose(cx) {
                act_ui::focus::note_keystroke(keys, window, cx);
                cx.stop_propagation();
            } else if keys.key == "escape" && root.composer.paste_viewing.take().is_some() {
                act_ui::focus::note_keystroke(keys, window, cx);
                cx.notify();
                cx.stop_propagation();
            } else if keys.key == "escape" && root.menu_dismiss(cx) {
                act_ui::focus::note_keystroke(keys, window, cx);
                cx.stop_propagation();
            }
        }))
        .child(root.composer.input.clone())
        .child(Scrollbar::vertical_model("composer-scrollbar", &scroll));

    let away = root.composer.dictation.as_ref().is_some_and(|dictation| dictation.away(cx));
    let mut shadows: Vec<BoxShadow> = t.shadow_composer.box_shadows();
    let ring = if root.composer.dropping && cx.has_active_drag() {
        Some((t.accent, 2.0))
    } else if dictating.is_some() {
        Some((if away { t.danger_line } else { t.accent_line }, 1.0))
    } else if root.composer.focused && act_ui::focus::keyboard_focus_visible(cx) {
        Some((t.line_input, 1.0))
    } else {
        None
    };
    // Shadows paint in order, each over the last: the ring over the composer's own shadows, and
    // `.glass-pane::before`'s dark hairline outside the edge over both.
    if let Some((color, spread)) = ring {
        shadows.push(BoxShadow { color: color.into(), offset: gpui::point(px(0.0), px(0.0)), blur_radius: px(0.0), spread_radius: px(spread), inset: false });
    }
    shadows.push(act_ui::theme::ShadowLayer { x: 0.0, y: 0.0, blur: 0.0, spread: 0.5, color: t.glass_float_outline }.box_shadow());

    let pane_probe = root.composer.pane_probe.clone();
    let radius = if docked { 14.0 } else { 17.0 };
    let unopenable = thread.filter(|thread| !thread.engine.is_known()).map(|thread| act_core::domain::engine::unopenable_engine(thread.engine.as_str()));
    let mut composer = div()
        .id("composer")
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::SectionFooter, ""))
        .relative()
        .w_full()
        .max_w(px(860.0))
        .rounded(px(radius))
        .shadow(shadows)
        .glass_pane(t.glass, t, t.shadow_composer.glint.then_some(t.glint), Corners::Radius(radius))
        .child(bounds_probe(&pane_probe))
        .child(super::common::seen_by_backdrops());
    if let Some(reason) = unopenable {
        let update_available = !root.composer.side && state.app_update != act_core::domain::platform::AppUpdate::Idle;
        let task = root.composer.task.clone();
        composer = composer.flex().items_center().gap(px(12.0)).p(px(12.0)).pl(px(18.0)).child(unopenable_pane(&reason, update_available, cx));
        if run_active {
            composer = composer.child(send_button(state, task, root.composer.side, true, false, false, cx));
        }
    } else {
        composer = composer.children(composer_attached::drafts(root, state, window, cx)).child(field).child(bar);
        if let Some(menu) = composer_menus::render(root, state, window, cx) {
            composer = composer.child(menu);
        }
        if let Some(picker) = (!docked).then(|| super::review_picker::render(root, state, window, cx)).flatten() {
            composer = composer.child(picker);
        }
    }

    let mut wrap = div().w_full().flex().flex_col().items_center();
    if let Some(goal) = (!docked).then(|| composer_question::goal_bar(root, state, window, cx)).flatten() {
        wrap = wrap.child(goal);
    }
    if let Some(card) = (!docked).then(|| super::needs_you::render(root, state, window, cx)).flatten() {
        wrap = wrap.child(card);
    }
    if let Some(question) = composer_question::question(root, state, window, cx) {
        wrap = wrap.child(question);
    }
    if let Some(queue) = queued_row::render(root, state, window, cx) {
        wrap = wrap.child(queue);
    }
    if let Some(model) = model.as_ref().filter(|_| !docked) {
        if let Some(warning) = composer_meters::plan_warning(state, engine, model, width, cx) {
            wrap = wrap.child(warning);
        }
    }
    if let Some(start) = start {
        wrap = wrap.child(start);
    }
    let message = send_state.error.clone().map(|error| (error, true)).or_else(|| root.composer.note.clone().map(|note| (note, false)));
    if let Some((message, error)) = message {
        wrap = wrap.child(
            div().w_full().max_w(px(860.0)).mb(px(8.0)).child(
                Notice::new("send-notice", message)
                    .floating(true)
                    .tone(if error { NoticeTone::Danger } else { NoticeTone::Neutral })
                    .icon(if error { IconName::CircleAlert } else { IconName::Info })
                    .on_dismiss(root.composer_listener(cx, |root, _, _, cx| {
                        root.composer.note = None;
                        dispatch(AppCommand::AttachmentsNotice { task_id: root.composer.task.clone(), message: None }, cx);
                        cx.notify();
                    })),
            ),
        );
    }
    wrap.child(composer)
}

/// Stands where the text would go for a thread this build cannot open: why, and the update once
/// one is found, else a check for one.
fn unopenable_pane(reason: &str, update_available: bool, cx: &mut Context<Root>) -> impl IntoElement {
    use act_ui::components::button::{Button, ButtonSize, ButtonVariant};
    let theme = cx.theme().clone();
    let button = if update_available {
        Button::new("composer-unopenable-update", "Update").variant(ButtonVariant::Primary).size(ButtonSize::Lg).on_click(|_, _, cx| dispatch(AppCommand::AppUpdate { computer_id: None }, cx))
    } else {
        Button::new("composer-unopenable-check", "Check for updates").variant(ButtonVariant::Secondary).size(ButtonSize::Lg).on_click(|_, _, cx| dispatch(AppCommand::AppCheckForUpdates { quiet: false }, cx))
    };
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(12.0))
        .child(div().flex_1().min_w_0().text_size(px(theme.metrics.text_control)).text_color(theme.tokens.muted).child(reason.to_string()))
        .child(button)
}

/// The workspace takes files dragged in from the desktop, lighting the composer as their target.
pub fn drop_target(root: &mut Root, main: gpui::Div, cx: &mut Context<Root>) -> gpui::Div {
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    let over = root.composer.dropping && cx.has_active_drag();
    main.on_drag_move::<ExternalPaths>(cx.listener(|root, event: &gpui::DragMoveEvent<ExternalPaths>, _, cx| {
        let inside = event.bounds.contains(&event.event.position);
        if inside != root.composer.dropping {
            root.composer.dropping = inside;
            cx.notify();
        }
    }))
    .on_drop(cx.listener(|root, paths: &ExternalPaths, _, cx| root.drop_files(paths, cx)))
    .when(over, |main| {
        main.child(
            div()
                .absolute()
                .bottom(px(18.0))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(act_ui::style::glass(div(), &t, act_ui::style::GlassKind::Float, Some(999.0), None).border_0().px(px(14.0)).py(px(6.0)).text_color(t.ink_soft).text_size(px(m.text_meta)).child("Drop to attach")),
        )
    })
}
