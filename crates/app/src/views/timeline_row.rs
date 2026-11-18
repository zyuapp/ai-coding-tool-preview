//! One row of the transcript: the user's words, a turn of agent work with its answer, a notice, the
//! threads a coordinator started, or its folded updates (TimelineRow.tsx, TurnWork.tsx,
//! SystemNotice.tsx, CopyButton.tsx).

use act_core::shared::Shared;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use act_core::domain::conversation::{Annotation, AnnotationAnchor, ConversationMessage, MessageKind, MessageTone};
use act_core::domain::engine::AgentEngine;
use act_core::domain::find::{match_offsets, FindField, FindHit};
use act_core::domain::message_origin::{message_label, MessageOrigin};
use act_core::domain::tool_call::{describe_tool_call, ToolCall, ToolFamily};
use act_core::state::StreamingTail;
use act_core::AppCommand;
use act_markdown::{selectable_area, Highlight, MarkdownView, SelectableText};
use act_ui::components::button::IconButton;
use act_ui::style::{Corners, PaneFill, Raised};
use act_ui::theme::ActiveTheme;
use act_ui::tooltip::Tippable;
use act_ui::IconName;
use gpui::{div, prelude::*, px, relative, AnyElement, App, ClipboardItem, Div, Entity, FontWeight, HighlightStyle, Hsla, SharedString, Stateful, StyledText, WeakEntity, Window};

use super::letter_spacing::LetterSpacing;
use super::common::{clock_time, count_chip, format_elapsed, full_moment, icon, ink_icon, rimmed, Ellipsis};
use super::coordination::{thread_cards, Card};
use super::grouping::{group_message_ids, time_steps, to_segments, Entry, Group, Segment, TimedStep};
use super::markdown::{message_view, Variant};
use super::root::Root;
use crate::workspace::dispatch;

/// Which folds the reader opened, and the ones they closed on a match that had opened them.
#[derive(Default)]
pub struct Folds {
    open: HashSet<String>,
    /// A fold closed by hand while the match it held was being read, by the message that match is in.
    dismissed: HashMap<String, String>,
}

impl Folds {
    fn forced(&self, revealed: Option<&str>, fold: &str, holds: &[String]) -> bool {
        revealed.is_some_and(|revealed| holds.iter().any(|held| held == revealed) && self.dismissed.get(fold).map(String::as_str) != Some(revealed))
    }

    /// Which of a row's folds the reader opened: the row's own, then each run's and step's it holds.
    pub fn opened_in(&self, row: &str, held: &[String]) -> Vec<bool> {
        std::iter::once(self.open.contains(row)).chain(held.iter().flat_map(|id| [self.open.contains(&run_fold(id)), self.open.contains(&step_fold(id))])).collect()
    }

    /// Whether a fold is shown: opened by hand, or holding the match being read unless closed on it.
    fn shown(&self, revealed: Option<&str>, fold: &str, holds: &[String]) -> bool {
        self.open.contains(fold) || self.forced(revealed, fold, holds)
    }

    fn toggle(&mut self, revealed: Option<&str>, fold: &str, holds: &[String]) {
        let forced = self.forced(revealed, fold, holds);
        if self.open.contains(fold) || forced {
            if let (true, Some(revealed)) = (forced, revealed) {
                self.dismissed.insert(fold.to_string(), revealed.to_string());
            }
            self.open.remove(fold);
        } else {
            self.open.insert(fold.to_string());
        }
    }
}

/// A run of tool calls and a single call share their first call's id, so their folds are told apart.
fn run_fold(id: &str) -> String {
    format!("run:{id}")
}

fn step_fold(id: &str) -> String {
    format!("step:{id}")
}

/// The match being read and what lights it, while this transcript is the one searched.
#[derive(Clone)]
pub struct FindPaint {
    /// The query, lowered and trimmed.
    pub needle: String,
    pub hit: Option<FindHit>,
}

/// What every row draws from, shared by the list's item renderer.
pub struct RowModel {
    pub engine: AgentEngine,
    pub task_id: String,
    pub groups: Rc<Vec<Group>>,
    pub tail: Option<StreamingTail>,
    pub folds: Rc<RefCell<Folds>>,
    pub root: WeakEntity<Root>,
    pub inset: f32,
    /// Room kept clear at the right for the session card.
    pub side: f32,
    pub now: i64,
    pub find: Option<FindPaint>,
    /// The composer's drafted annotations anchored in this transcript.
    pub annotations: Vec<Annotation>,
    /// A coordinator's threads, drawn as cards where each began.
    pub cards: Option<Rc<HashMap<String, Card>>>,
    /// Every thread's title by id, so a call naming a thread reads by what it is called.
    pub names: Rc<HashMap<String, String>>,
    /// Markdown views drawn this frame, by message id, for annotating and placing markers.
    pub drawn: Rc<RefCell<HashMap<String, Entity<MarkdownView>>>>,
    /// A transcript in a dock tab, whose messages sit closer and wider.
    pub docked: bool,
    /// The messages a drag selects across, and each one's place in reading order.
    pub selection: Option<(act_markdown::SelectionGroup, Rc<HashMap<String, u64>>)>,
    /// The checkout the thread works in, which a reply's linked images are read from.
    pub folder: Option<String>,
    /// The list's width as last laid out, or 0 before its first layout.
    pub width: f32,
}

/// Each message's place in the thread, which is the order a selection reads them in.
pub fn reading_order(messages: &[Shared<ConversationMessage>]) -> HashMap<String, u64> {
    messages.iter().enumerate().map(|(index, message)| (message.id.clone(), index as u64)).collect()
}

/// Within a message's place in reading order, the summaries heading it come first, from the
/// outermost fold in, then its own text.
const SETTLED_HEAD: u64 = 0;
const RUN_HEAD: u64 = 1;
const STEP_HEAD: u64 = 2;
const BODY: u64 = 3;

impl RowModel {
    /// The transcript's selection and where in it `slot` of a message sits.
    fn place(&self, message_id: &str, slot: u64) -> Option<(act_markdown::SelectionGroup, u64)> {
        let (group, order) = self.selection.as_ref()?;
        let place = order.get(message_id).copied().unwrap_or(order.len() as u64);
        Some((group.clone(), place * 4 + slot))
    }

    /// Joins a message's view to the transcript's selection.
    fn join_selection(&self, view: &Entity<MarkdownView>, message_id: &str, cx: &mut App) {
        if let Some((group, place)) = self.place(message_id, BODY) {
            view.update(cx, |view, _| view.set_selection_group(group, place));
        }
    }
}

/// Plain text that selects with the rest of the transcript, at `slot` of `message_id`'s place,
/// started afresh for this frame's pieces.
fn selectable(model: &RowModel, key: &str, message_id: &str, slot: u64, window: &mut Window, cx: &mut App) -> Entity<SelectableText> {
    let text = window.use_keyed_state(SharedString::from(format!("selectable-{slot}-{key}")), cx, SelectableText::new);
    let dark = cx.theme().tokens.variant == act_ui::ThemeVariant::Dark;
    let active: Hsla = cx.theme().tokens.text_selection.into();
    let inactive = if dark { act_markdown::MarkdownStyle::dark() } else { act_markdown::MarkdownStyle::light() }.selection_inactive;
    let place = model.place(message_id, slot);
    text.update(cx, |text, _| {
        if let Some((group, place)) = place {
            text.set_selection_group(group, place);
        }
        text.set_selection_colors(active, inactive);
        text.begin();
    });
    text
}

/// The next piece of `text`, lit by `lit`.
fn piece(text: &Entity<SelectableText>, words: impl Into<SharedString>, lit: Vec<(std::ops::Range<usize>, HighlightStyle)>, window: &Window, cx: &mut App) -> StyledText {
    text.update(cx, |text, _| text.part(words, lit, window))
}

impl RowModel {
    /// The message a match is in, which opens whatever fold holds it.
    fn revealed(&self) -> Option<&str> {
        self.find.as_ref().and_then(|find| find.hit.as_ref()).map(|hit| hit.message_id.as_str())
    }

    fn shown(&self, fold: &str, holds: &[String]) -> bool {
        self.folds.borrow().shown(self.revealed(), fold, holds)
    }
}

/// The transcript's column: centred, 860 wide at most, inset from the page's sides.
pub fn column(inset: f32, side: f32, content: impl IntoElement) -> Div {
    div().w_full().pl(px(inset)).pr(px(inset + side)).flex().justify_center().child(div().w_full().max_w(px(860.0)).child(content))
}

pub fn render(model: &Rc<RowModel>, index: usize, newest: bool, window: &mut Window, cx: &mut App) -> AnyElement {
    let group = &model.groups[index];
    let row_group: SharedString = format!("row-{}", group.id()).into();
    let (content, bottom) = match group {
        Group::Updates { id, entries, count } => (updates(model, index, id, group, entries, *count, &row_group, window, cx), 26.0),
        Group::Entry(entry) => {
            // A dock transcript keeps the row's padding and adds the message's own margin under it
            // (`.side-chat-transcript .message`).
            let bottom = match entry {
                Entry::Message { message, from: None, .. } if message.kind == MessageKind::User => if model.docked { 36.0 + 30.0 } else { 36.0 },
                _ => if model.docked { 26.0 + 25.0 } else { 26.0 },
            };
            (self::entry(model, index, entry, newest, &row_group, window, cx), bottom)
        }
    };
    // The row is the reading column's width, so the pointer beside it is not on the row.
    let focus = row_focus(&row_group, window, cx);
    let row = div().id(SharedString::from(format!("timeline-{}", group.id()))).track_focus(&focus).group(row_group).pb(px(bottom)).child(content);
    column(model.inset, model.side, row).into_any_element()
}

fn entry(model: &Rc<RowModel>, index: usize, entry: &Entry, newest: bool, row_group: &SharedString, window: &mut Window, cx: &mut App) -> AnyElement {
    match entry {
        Entry::Threads { thread_ids, .. } => match &model.cards {
            Some(cards) => thread_cards(thread_ids, cards, cx),
            None => div().into_any_element(),
        },
        Entry::Turn { id, steps, final_message, ends_at, live } => {
            let mut turn = div().flex().flex_col().gap(px(17.0)).accessible(gpui::AccessInfo::new(gpui::AccessRole::Article, ""));
            if *live {
                turn = turn.children(segments(model, index, &to_segments(time_steps(steps, None)), true, window, cx));
            } else if !steps.is_empty() {
                turn = turn.child(settled(model, index, id, steps, *ends_at, window, cx));
            }
            if let Some(answer) = final_message {
                let view = markdown(model, answer, &answer.text, false, Variant::Answer, window, cx);
                view.update(cx, |view, cx| view.set_emphasize_copy(newest, cx));
                // The previews sit in the answer's own block, under its last paragraph's margin.
                turn = turn.child(div().child(view).children(previews(model, answer, cx))).child(actions(answer, newest, "Copy the answer", row_group, window, cx).mt(px(9.0)));
            }
            turn.into_any_element()
        }
        Entry::Message { message, from, .. } => match message.kind {
            MessageKind::System => notice(model, message, window, cx),
            MessageKind::Assistant => div()
                .accessible(gpui::AccessInfo::new(gpui::AccessRole::Article, ""))
                .child(markdown(model, message, &message.text, false, Variant::Answer, window, cx))
                .children(previews(model, message, cx))
                .into_any_element(),
            _ => match from {
                Some(from) => thread_message(model, index, message, from, window, cx),
                None => user_message(model, message, newest, row_group, window, cx),
            },
        },
    }
}

/// A thread with no folder still resolves absolute image paths.
fn image_scope(model: &RowModel, message_id: &str) -> super::markdown::ImageScope {
    super::markdown::ImageScope { root: model.folder.clone().unwrap_or_default(), message_id: message_id.to_string() }
}

/// A settled message's previews of the local images it links.
fn previews(model: &RowModel, message: &ConversationMessage, cx: &mut App) -> Option<AnyElement> {
    super::markdown::image_previews(&message.text, &image_scope(model, &message.id), Some(&model.task_id), cx)
}

/// A message drawn as Markdown, lit where the find bar's matches and the drafted annotations fall.
fn markdown(model: &RowModel, message: &ConversationMessage, text: &str, streaming: bool, variant: Variant, window: &mut Window, cx: &mut App) -> Entity<MarkdownView> {
    let view = message_view(SharedString::from(format!("md-{}", message.id)), text, streaming, variant, Some(model.task_id.clone()), Some(image_scope(model, &message.id)), window, cx);
    let t = cx.theme().tokens.clone();
    let mut highlights = Vec::new();
    {
        let read = view.read(cx);
        for annotation in &model.annotations {
            if let Some(AnnotationAnchor::Message { message_id, start, end }) = &annotation.anchor {
                if message_id == &message.id {
                    if let (Some(start), Some(end)) = (read.position_at((*start).max(0) as usize), read.position_at((*end).max(0) as usize)) {
                        highlights.push(Highlight { start, end, background: t.info_surface.into(), color: Some(t.ink.into()) });
                    }
                }
            }
        }
        if let Some(find) = model.find.as_ref().filter(|find| !find.needle.is_empty()) {
            let active = find.hit.as_ref().filter(|hit| hit.message_id == message.id && hit.field == FindField::Text).map(|hit| hit.occurrence);
            for (occurrence, (start, end)) in read.find(&find.needle).into_iter().enumerate() {
                let (background, color) = if Some(occurrence) == active { (t.find_active, t.accent_ink) } else { (t.find_match, t.ink) };
                highlights.push(Highlight { start, end, background: background.into(), color: Some(color.into()) });
            }
        }
    }
    view.update(cx, |view, cx| view.set_highlights(highlights, cx));
    model.join_selection(&view, &message.id, cx);
    model.drawn.borrow_mut().insert(message.id.clone(), view.clone());
    view
}

/// What the user wrote, as written, selectable with the rest of the transcript.
fn user_text(model: &RowModel, message: &ConversationMessage, window: &mut Window, cx: &mut App) -> Entity<MarkdownView> {
    let view = message_view(SharedString::from(format!("md-user-{}", message.id)), &message.text, false, Variant::User, Some(model.task_id.clone()), None, window, cx);
    let t = cx.theme().tokens.clone();
    let mut highlights = Vec::new();
    if let Some(find) = model.find.as_ref().filter(|find| !find.needle.is_empty()) {
        let active = find.hit.as_ref().filter(|hit| hit.message_id == message.id && hit.field == FindField::Text).map(|hit| hit.occurrence);
        for (occurrence, (start, end)) in view.read(cx).find(&find.needle).into_iter().enumerate() {
            let (background, color) = if Some(occurrence) == active { (t.find_active, t.accent_ink) } else { (t.find_match, t.ink) };
            highlights.push(Highlight { start, end, background: background.into(), color: Some(color.into()) });
        }
    }
    view.update(cx, |view, cx| view.set_highlights(highlights, cx));
    model.join_selection(&view, &message.id, cx);
    view
}

/// A call's input as written, selectable with the rest of the transcript, its matches lit.
fn input_text(model: &RowModel, message: &ConversationMessage, detail: &str, window: &mut Window, cx: &mut App) -> Entity<MarkdownView> {
    let view = message_view(SharedString::from(format!("md-input-{}", message.id)), detail, false, Variant::Input, Some(model.task_id.clone()), None, window, cx);
    let t = cx.theme().tokens.clone();
    let mut highlights = Vec::new();
    if let Some(find) = model.find.as_ref().filter(|find| !find.needle.is_empty()) {
        let hit = find.hit.as_ref().filter(|hit| hit.message_id == message.id && hit.field == FindField::Detail).map(|hit| hit.start);
        let active = hit.and_then(|start| match_offsets(detail, &find.needle, act_core::domain::find::MAX_FIND_HITS).iter().position(|at| *at == start));
        for (occurrence, (start, end)) in view.read(cx).find(&find.needle).into_iter().enumerate() {
            let (background, color) = if Some(occurrence) == active { (t.find_active, t.accent_ink) } else { (t.find_match, t.ink) };
            highlights.push(Highlight { start, end, background: background.into(), color: Some(color.into()) });
        }
    }
    view.update(cx, |view, cx| view.set_highlights(highlights, cx));
    model.join_selection(&view, &message.id, cx);
    view
}

/// Plain text lit where the find bar's matches fall in one field of a message.
fn lit(model: &RowModel, message_id: &str, field: FindField, text: &str, cx: &App) -> StyledText {
    lit_at(model, message_id, field, text, Some(0), cx)
}

/// `text` drawn with its matches lit. `base` is where it starts in the field the hit counts in, or
/// `None` for text derived from the field, which never holds the current match.
fn lit_at(model: &RowModel, message_id: &str, field: FindField, text: &str, base: Option<usize>, cx: &App) -> StyledText {
    StyledText::new(SharedString::from(text.to_string())).with_highlights(lit_ranges(model, message_id, field, text, base, cx))
}

/// Where `text`'s find matches fall and how each is lit; see `lit_at`.
fn lit_ranges(model: &RowModel, message_id: &str, field: FindField, text: &str, base: Option<usize>, cx: &App) -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
    let Some(find) = model.find.as_ref().filter(|find| !find.needle.is_empty()) else { return Vec::new() };
    let t = cx.theme().tokens.clone();
    let length = find.needle.chars().count();
    let end_of = |at: usize| text[at..].char_indices().nth(length).map(|(end, _)| at + end).unwrap_or(text.len());
    let active = find.hit.as_ref().filter(|hit| hit.message_id == message_id && hit.field == field).zip(base).and_then(|(hit, base)| hit.start.checked_sub(base));
    match_offsets(text, &find.needle, act_core::domain::find::MAX_FIND_HITS)
        .into_iter()
        .map(|at| {
            let (background, color) = if Some(at) == active { (t.find_active, t.accent_ink) } else { (t.find_match, t.ink) };
            (at..end_of(at), HighlightStyle { background_color: Some(Hsla::from(background)), color: Some(Hsla::from(color)), ..Default::default() })
        })
        .collect()
}

/// The focus a row holds when anything in it does.
fn row_focus(row_group: &SharedString, window: &mut Window, cx: &mut App) -> gpui::FocusHandle {
    window.use_keyed_state(SharedString::from(format!("{row_group}-focus")), cx, |_, cx| cx.focus_handle()).read(cx).clone()
}

/// Whether a control in the row holds focus (`:focus-within`); text a press selects in does not count.
fn focus_within(row_group: &SharedString, window: &mut Window, cx: &mut App) -> bool {
    row_focus(row_group, window, cx).contains_focused(window, cx) && window.focused(cx).is_some_and(|focused| focused.tab_stop)
}

/// When a message was sent, and a copy of it, shown on the newest row, under the pointer, and
/// while the row holds focus.
fn actions(message: &ConversationMessage, newest: bool, label: &'static str, row_group: &SharedString, window: &mut Window, cx: &mut App) -> Stateful<Div> {
    let t = cx.theme().tokens.clone();
    let within = focus_within(row_group, window, cx);
    // Drawn clear rather than left out, so the keyboard still reaches the copy button.
    let shown = newest || within;
    div()
        .id(SharedString::from(format!("actions-{}", message.id)))
        .transition(gpui::Transition::ease_out(110))
        .flex()
        .items_center()
        .justify_end()
        .gap(px(8.0))
        .h(px(28.0))
        .pr(px(7.0))
        .when(!shown, |row| row.opacity(0.0).group_hover(row_group.clone(), |style| style.opacity(1.0)))
        .child(
            div()
                .id(SharedString::from(format!("time-{}", message.id)))
                .accessible(gpui::AccessInfo::new(gpui::AccessRole::Time, ""))
                .text_color(t.quiet_strong)
                .text_size(px(12.0))
                .tabular_nums()
                .child(div().accessible(gpui::AccessInfo::new(gpui::AccessRole::Label, clock_time(message.at))).child(clock_time(message.at)))
                .tip(full_moment(message.at)),
        )
        .when(!message.text.is_empty(), |row| {
            let tile = CopyTile { id: format!("copy-{}", message.id), text: message.text.clone(), label: label.into(), side: 28.0, ground: None };
            row.child(copy_tile(tile, CopyShow::Row(row_group.clone(), within), window, cx))
        })
}

struct Copied(Option<std::time::Instant>);

const COPIED_FOR: Duration = Duration::from_millis(1400);

const COPY_TILE: &str = "copy-tile";

/// The focus a copy button takes from the keyboard, which shows it as the pointer does.
fn copy_focus(id: &str, window: &mut Window, cx: &mut App) -> gpui::FocusHandle {
    window.use_keyed_state(SharedString::from(format!("{id}-focus")), cx, |_, cx| cx.focus_handle().tab_stop(true)).read(cx).clone()
}

fn copy_state(id: &str, window: &mut Window, cx: &mut App) -> Entity<Copied> {
    window.use_keyed_state(SharedString::from(format!("{id}-state")), cx, |_, _| Copied(None))
}

fn is_copied(state: &Entity<Copied>, cx: &App) -> bool {
    state.read(cx).0.is_some_and(|at| at.elapsed() < COPIED_FOR)
}

/// When a copy button's tile of glass is up.
enum CopyShow {
    Always,
    /// A message's: a bare glyph until its row is under the pointer or holds focus, which the
    /// flag says it does.
    Row(SharedString, bool),
    /// A block's on the newest answer: a bare glyph until the block is under the pointer.
    Block(SharedString),
    /// A bare glyph until it is itself under the pointer.
    Hover,
}

/// A copy button: what it copies, its name, and its tile.
struct CopyTile {
    id: String,
    text: String,
    label: SharedString,
    /// The tile's side; the glyph stays centred in it.
    side: f32,
    /// The translucent fill of the block under it, which Chromium's backdrop filter draws over itself once more.
    ground: Option<gpui::Rgba>,
}

/// Copies text on click, then shows a tick for a moment. With a `group`, it is a bare glyph until
/// that group is under the pointer, and a pane of glass then.
pub fn copy_button(id: &str, text: String, label: &'static str, group: Option<SharedString>, window: &mut Window, cx: &mut App) -> Div {
    let tile = CopyTile { id: id.to_string(), text, label: label.into(), side: 28.0, ground: None };
    copy_tile(tile, group.map_or(CopyShow::Always, |group| CopyShow::Row(group, false)), window, cx)
}

/// The glass variant's tile (`.icon-btn[data-variant="glass"]`).
fn copy_tile(tile: CopyTile, show: CopyShow, window: &mut Window, cx: &mut App) -> Div {
    let CopyTile { id, text, label, side, ground } = tile;
    let state = copy_state(&id, window, cx);
    let focus = copy_focus(&id, window, cx);
    let copied = is_copied(&state, cx);
    let lifted = copied || focus.is_focused(window) || matches!(show, CopyShow::Row(_, true));
    let t = cx.theme().tokens.clone();
    let clear = gpui::Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };
    let group = match &show {
        CopyShow::Always => None,
        CopyShow::Hover => Some(SharedString::from(COPY_TILE)),
        CopyShow::Row(group, _) | CopyShow::Block(group) => Some(group.clone()),
    };
    // The glyph at rest, and once its row or block is under the pointer.
    let (rest, raised) = match (&show, copied) {
        (_, true) => (t.success_soft, t.success_soft),
        (CopyShow::Always, _) => (t.ink_dim, t.ink_dim),
        (CopyShow::Row(..) | CopyShow::Hover, _) => (t.muted, t.muted),
        (CopyShow::Block(_), _) => (t.quiet, t.ink_dim),
    };
    let glyph_icon = if copied { IconName::Check } else { IconName::Copy };
    let glyph = div()
        .text_color(if lifted { raised } else { rest })
        .when_some(group.clone(), |glyph, group| glyph.group_hover(group, move |style| style.text_color(raised)))
        .child(div().when(!copied, |glyph| glyph.group_hover(COPY_TILE, move |style| style.text_color(t.ink_soft))).child(ink_icon(glyph_icon, 14.0)));
    let button = IconButton::new(SharedString::from(id), glyph_icon, if copied { SharedString::from("Copied") } else { label })
        .icon_size(14.0)
        .hover_look(clear, clear, t.ink_soft)
        .tip_again()
        .track_focus(&focus)
        .on_click(move |_, window, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
            state.update(cx, |copied, cx| {
                copied.0 = Some(std::time::Instant::now());
                cx.notify();
            });
            let state = state.clone();
            window
                .spawn(cx, async move |cx| {
                    cx.background_executor().timer(COPIED_FOR).await;
                    let _ = cx.update(|_, cx| state.update(cx, |_, cx| cx.notify()));
                })
                .detach();
        })
        .glyph(glyph);
    // The edge at rest, once its row or block is under the pointer; under the pointer itself it is bright.
    let edges = match (&show, copied) {
        (_, true) => (t.success_line, t.success_line),
        (CopyShow::Always, _) => (t.line_strong, t.line_strong),
        (CopyShow::Row(..), _) => (clear, t.line_glass),
        (CopyShow::Block(_), _) => (clear, t.line_strong),
        (CopyShow::Hover, _) => (clear, t.line_bright),
    };
    let (group, edges) = if lifted { (None, (edges.1, edges.1)) } else { (group, edges) };
    glass_tile(button, side, ground, group, edges, !copied, &t)
}

/// A tile of glass around `button` (`.icon-btn[data-variant=glass]`): with a `group`, hidden with
/// its edge `rest` until the group is under the pointer, then edged `raised`; bright under the
/// pointer itself when `lights`.
fn glass_tile(button: IconButton, side: f32, ground: Option<gpui::Rgba>, group: Option<SharedString>, (rest_edge, raised_edge): (gpui::Rgba, gpui::Rgba), lights: bool, t: &act_ui::theme::Tokens) -> Div {
    let outline = act_ui::theme::ShadowLayer { x: 0.0, y: 0.0, blur: 0.0, spread: 0.5, color: t.glass_float_outline }.box_shadow();
    let layer = || div().absolute().inset_0().rounded(px(8.0));
    let pane = layer()
        .shadow(vec![outline])
        .when_some(ground, |pane, ground| pane.child(layer().bg(ground)))
        // The sheen fills the padding box and repeats into the border, as CSS lays it out.
        .child(layer().border_1().border_color(gpui::transparent_black()).bg(t.glass_clear.background().tiled_from_padding_box()))
        // CSS draws the inset rim inside the 1px border.
        .child(div().absolute().inset(px(1.0)).rounded(px(7.0)).float_rim(t, Corners::Radius(7.0)));
    let pane = match &group {
        Some(group) => pane.opacity(0.0).group_hover(group.clone(), |style| style.opacity(1.0)),
        None => pane,
    };
    let bright = t.line_bright;
    let edge = layer()
        .border_1()
        .border_color(rest_edge)
        .when_some(group, |edge, group| edge.group_hover(group, move |style| style.border_color(raised_edge)))
        .when(lights, |edge| edge.hover(move |style| style.border_color(bright)));
    let inset = (side - 28.0) / 2.0;
    div().relative().flex_none().size(px(side)).group(COPY_TILE).child(pane).child(div().absolute().top(px(inset)).left(px(inset)).child(button)).child(edge)
}

/// A block's corner control beside its copy button, drawn as that button is over `ground`: its
/// glyph dim, and soft with a bright edge under the pointer.
pub fn block_tile(button: IconButton, icon: IconName, ground: gpui::Rgba, cx: &App) -> Div {
    let t = cx.theme().tokens.clone();
    let soft = t.ink_soft;
    let clear = gpui::Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };
    let glyph = div().text_color(t.ink_dim).child(div().group_hover(COPY_TILE, move |style| style.text_color(soft)).child(ink_icon(icon, 14.0)));
    let button = button.icon_size(14.0).hover_look(clear, clear, soft).glyph(glyph);
    glass_tile(button, 28.0, Some(ground), None, (t.line_strong, t.line_strong), true, &t)
}

/// A block's copy button: hidden until the block is under the pointer, or holding focus or a
/// copy. On the newest answer it is a bare glyph till then.
fn corner_copy(tile: CopyTile, group: SharedString, emphasize: bool, window: &mut Window, cx: &mut App) -> Div {
    let shown = emphasize || copy_focus(&tile.id, window, cx).is_focused(window) || is_copied(&copy_state(&tile.id, window, cx), cx);
    let show = if emphasize { CopyShow::Block(group.clone()) } else { CopyShow::Always };
    div().when(!shown, |corner| corner.opacity(0.0).group_hover(group, |style| style.opacity(1.0))).child(copy_tile(tile, show, window, cx))
}

/// The copy button a Markdown block asks for (`.copyable > .copy-affordance`).
pub fn block_copy_button(request: act_markdown::CopyButtonRequest, window: &mut Window, cx: &mut App) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let tile = CopyTile {
        id: format!("{}-copy", request.group),
        text: request.text.to_string(),
        label: request.label,
        side: if request.kind == act_markdown::CopyKind::CodeBlock { 26.0 } else { 28.0 },
        ground: Some(if request.kind == act_markdown::CopyKind::Fence { t.sunken } else { t.surface }),
    };
    corner_copy(tile, request.group, request.emphasize, window, cx).on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation()).into_any_element()
}

/// A block with a copy button in its corner, shown while the block is under the pointer.
fn copyable(id: &str, text: &str, label: &'static str, body: impl IntoElement, ground: gpui::Rgba, window: &mut Window, cx: &mut App) -> Div {
    let group: SharedString = format!("copyable-{id}").into();
    let tile = CopyTile { id: id.to_string(), text: text.to_string(), label: label.into(), side: 28.0, ground: Some(ground) };
    let button = corner_copy(tile, group.clone(), false, window, cx);
    div().relative().group(group).child(body).child(div().absolute().top(px(6.0)).right(px(6.0)).child(button))
}

/// The thread a message came from, when it names one on this computer.
fn sender_thread(origin: Option<&MessageOrigin>) -> Option<String> {
    match origin? {
        MessageOrigin::Thread { thread_id: Some(thread_id), computer: None, .. } => Some(thread_id.clone()),
        _ => None,
    }
}

/// A link chip naming a thread, with the arrow that says it opens it.
/// `room` caps its width in pixels, as a percentage of the shrink-to-fit stack is not resolved
/// when its text is measured, so a wrapped label would be short.
fn open_thread_chip(id: SharedString, label: String, thread_id: String, room: Option<f32>, cx: &App) -> impl IntoElement {
    let t = cx.theme().tokens.clone();
    let (fill, color) = (t.glass_control_hover, t.accent_soft);
    // A column holding the label, so a wrapped label is measured at the chip's width; the arrow
    // hangs in the right padding, centred.
    div()
        .id(id)
        .relative()
        .flex()
        .flex_col()
        .justify_center()
        .max_w_full()
        .when_some(room, |chip, room| chip.max_w(px(room)))
        .min_h(px(28.0))
        .pl(px(7.0))
        .pr(px(22.0))
        .py(px(2.0))
        .rounded(px(7.0))
        .line_height(gpui::line_height_normal())
        .pane(t.glass_control)
        .rim(t.glass_rim, Corners::Radius(7.0))
        .text_color(t.accent_text)
        .text_size(px(cx.theme().metrics.text_meta))
        .cursor_pointer()
        .transition(gpui::Transition::ease(120))
        .hover(move |style| style.bg(fill.background()).text_color(color))
        .child(div().text_center().child(label))
        .child(div().absolute().top_0().bottom_0().right(px(7.0)).flex().items_center().opacity(0.8).child(ink_icon(IconName::ArrowUpRight, 12.0)))
        .tip("Open thread")
        .on_click(move |_, _, cx| dispatch(AppCommand::TaskSelect { task_id: thread_id.clone() }, cx))
}

fn user_message(model: &RowModel, message: &ConversationMessage, newest: bool, row_group: &SharedString, window: &mut Window, cx: &mut App) -> AnyElement {
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let m = &theme.metrics;
    let mut stack = div().flex().flex_col().items_end().gap(px(8.0)).min_w_0().max_w_full();
    if let Some(annotations) = message.annotations.as_ref().filter(|items| !items.is_empty()) {
        stack = stack.child(super::attached_rows::annotation_row(&message.id, annotations, window, cx));
    }
    if let Some(pastes) = message.pastes.as_ref().filter(|items| !items.is_empty()) {
        stack = stack.child(super::attached_rows::paste_row(&message.id, pastes, cx));
    }
    if let Some(files) = message.files.as_ref().filter(|files| !files.is_empty()) {
        stack = stack.child(super::attached_rows::file_row(&message.id, &model.task_id, files, cx));
    }
    if let Some(attachments) = message.attachments.as_ref().filter(|items| !items.is_empty()) {
        let gallery: Vec<String> = attachments.clone();
        stack = stack.child(div().flex().flex_wrap().justify_end().gap(px(8.0)).max_w_full().children(attachments.iter().enumerate().map(|(index, file)| {
            let path = crate::effects::window_desktop::image_file(file, cx);
            let name = act_host::attachments::attachment_name(file).to_string();
            let source = file.clone();
            let gallery = gallery.clone();
            div()
                .id(SharedString::from(format!("attachment-{}-{index}", message.id)))
                .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, format!("View {name}")))
                .h(px(92.0))
                .max_w(px(280.0))
                .overflow_hidden()
                .border_1()
                .border_color(t.line_soft)
                .rounded(px(8.0))
                .bg(t.canvas)
                .cursor(gpui::CursorStyle::PointingHand)
                .hover(|style| style.border_color(t.line_bright))
                // A definite height lets the width follow the picture's aspect, as `height: 100%; width: auto`.
                .child(gpui::img(path).h(px(90.0)).max_w(px(280.0)).object_fit(gpui::ObjectFit::Cover).rounded(px(7.0)))
                .tip(name)
                .on_click(move |_, _, cx| dispatch(AppCommand::ImageOpen { source: source.clone(), gallery: Some(gallery.clone()) }, cx))
        })));
    }
    if let Some(label) = message_label(message.origin.as_ref(), message.detail.as_deref()) {
        match sender_thread(message.origin.as_ref()) {
            Some(sender) => stack = stack.child(open_thread_chip(SharedString::from(format!("origin-{}", message.id)), label, sender, stack_width(model), cx)),
            None => stack = stack.child(div().text_color(t.quiet_strong).text_size(px(m.text_meta)).child(label)),
        }
    }
    if !message.text.is_empty() {
        // A window up to 1100 CSS pixels wide narrows the bubble to 85% of its column (responsive.css).
        let narrow = f32::from(window.viewport_size().width) <= super::session_panel::NARROW_WINDOW;
        let cap = narrow.then(|| bubble_cap(model, message, window, cx)).flatten();
        stack = stack.child(
            div()
                .max_w_full()
                .when(narrow, |pane| match cap {
                    Some(cap) => pane.max_w(px(cap)),
                    None => pane.max_w(relative(0.85)),
                })
                .relative()
                .rounded(px(18.0))
                .px(px(if model.docked { 13.0 } else { 16.0 }))
                .py(px(if model.docked { 9.0 } else { 11.0 }))
                .accessible(gpui::AccessInfo::new(gpui::AccessRole::Group, ""))
                .pane(t.glass_control)
                .rim(t.glass_rim, Corners::Radius(18.0))
                .text_color(t.ink_soft)
                .text_size(px(m.text_content))
                .line_height(relative(1.5))
                .child(user_text(model, message, window, cx)),
        );
    }
    div()
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Article, ""))
        .flex()
        .flex_col()
        .items_end()
        .child(div().map(|wrap| if model.docked { wrap.w(relative(0.92)) } else { wrap.w(relative(0.72)).max_w(px(680.0)) }).flex().flex_col().items_end().child(stack))
        .child(div().w_full().mt(px(4.0)).mb(px(if model.docked { 0.0 } else { -28.0 })).child(actions(message, newest, "Copy the message", row_group, window, cx)))
        .into_any_element()
}

/// The width of a sent message's column: 72% of the conversation's (capped at 680), or 92% in a dock.
fn stack_width(model: &RowModel) -> Option<f32> {
    let column = (model.width - model.inset * 2.0 - model.side).min(860.0);
    (model.width > 0.0).then(|| if model.docked { column * 0.92 } else { (column * 0.72).min(680.0) })
}

/// The bubble's width limit in a narrow window: 85% of its stack, which is as wide as its natural
/// width or the column's 72% (capped at 680), whichever is less. Given in pixels because a percentage of
/// the shrink-to-fit stack is not resolved when the text is measured, so its height would be short.
fn bubble_cap(model: &RowModel, message: &ConversationMessage, window: &mut Window, cx: &App) -> Option<f32> {
    if model.docked || model.width <= 0.0 {
        return None;
    }
    let stack = stack_width(model)?;
    // Its natural width is its widest line, as `pre-wrap` text sizes to fit; a line as wide as the
    // stack settles it.
    let theme = cx.theme();
    let font = gpui::font(theme.typography.ui_family.clone());
    let mut natural: f32 = 32.0;
    for line in message.text.split('\n') {
        if line.is_empty() {
            continue;
        }
        let run = gpui::TextRun { len: line.len(), font: font.clone(), color: gpui::black(), background_color: None, underline: None, strikethrough: None };
        let width = f32::from(window.text_system().shape_line(SharedString::from(line.to_string()), px(theme.metrics.text_content), &[run], None).width) + 32.0;
        natural = natural.max(width);
        if natural >= stack {
            break;
        }
    }
    Some(0.85 * natural.min(stack))
}

/// A state change the workspace made on its own; a failure tints the pane and offers its detail to copy.
fn notice(model: &RowModel, message: &ConversationMessage, window: &mut Window, cx: &mut App) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    let error = message.tone == Some(MessageTone::Error);
    let copy = error && message.detail.is_some();
    // A failure mixes its hue into the pane (`.glass-chip[data-tone]`).
    let fill = if error { act_ui::theme::Pane { fill: act_ui::color::mix(t.danger, 0.14, t.tone_glass_ground), sheen: t.glass_blur } } else { t.glass_control };
    let mut pane = rimmed(div(), &t, 14.0).min_w_0().max_w_full().pl(px(10.0)).pr(px(if copy { 40.0 } else { 13.0 })).py(px(6.0)).pane(fill).text_color(if error { t.danger } else { t.ink_dim });
    pane = pane.child(
        // The icon sits in the text's first line, so a wrapped line starts under it.
        div()
            .relative()
            .min_w_0()
            .text_size(px(m.text_menu))
            .line_height(px(18.0))
            .text_indent(px(21.0))
            .child(div().absolute().top(px(2.0)).left_0().child(icon(if error { IconName::CircleAlert } else { IconName::Info }, 14.0, if error { t.danger } else { t.info })))
            .child(lit(model, &message.id, FindField::Text, &message.text, cx)),
    );
    if let Some(detail) = &message.detail {
        pane = pane.child(div().mt(px(3.0)).text_color(if error { t.muted } else { t.quiet_strong }).text_size(px(m.text_meta)).child(lit(model, &message.id, FindField::Detail, detail, cx)));
    }
    if copy {
        let text = format!("{}\n{}", message.text, message.detail.clone().unwrap_or_default());
        let tile = CopyTile { id: format!("copy-{}", message.id), text, label: "Copy the error".into(), side: 28.0, ground: None };
        pane = pane.child(div().absolute().top(px(2.0)).right(px(4.0)).child(copy_tile(tile, CopyShow::Hover, window, cx)));
    }
    // Glass over the transcript frosts a toned pane, as Chromium's does.
    if error {
        pane = pane.child(super::common::seen_by_backdrops());
    }
    div().flex().child(pane).into_any_element()
}

/// The chevron closing a fold's summary: right while closed, down while open.
/// The summary's chevron (tool-calls.css `summary::after`): the right and bottom edges of a 6px box
/// turned to point right, in a 10px slot; open, it points down. The 17px glyph's strokes match the box's.
/// Muted, it lights with its summary's hover.
fn fold_mark(open: bool, t: &act_ui::theme::Tokens) -> impl IntoElement {
    let lit = t.ink_dim;
    let turn = if open { gpui::ElementTransform::translate(px(-2.125), px(0.0)).rotated(90.0) } else { gpui::ElementTransform::default() };
    div()
        .flex_none()
        .relative()
        .w(px(10.0))
        .h(px(17.0))
        .text_color(t.muted)
        .group_hover(FOLD_SUMMARY, move |style| style.text_color(lit))
        .child(
            div()
                .id("fold-mark")
                .absolute()
                .top_0()
                .left(px(-2.375))
                .transition(gpui::Transition::ease(140).only(&[gpui::TransitionProperty::Transform]))
                .transform(turn)
                .child(ink_icon(IconName::ChevronRight, 17.0)),
        )
}

const FOLD_SUMMARY: &str = "fold-summary";

/// A summary row that opens and closes the fold it heads. `holds` are the messages inside it;
/// `text` is its words, which select with the transcript.
#[allow(clippy::too_many_arguments)]
fn summary(model: &RowModel, fold: &str, holds: Vec<String>, index: usize, open: bool, content: Div, text: &Entity<SelectableText>, cx: &App) -> Stateful<Div> {
    summary_with(model, fold, holds, index, open, content, cx.theme().tokens.surface_hover, text, cx)
}

#[allow(clippy::too_many_arguments)]
fn summary_with(model: &RowModel, fold: &str, holds: Vec<String>, index: usize, open: bool, content: Div, hover: gpui::Rgba, text: &Entity<SelectableText>, cx: &App) -> Stateful<Div> {
    let t = cx.theme().tokens.clone();
    let root = model.root.clone();
    let folds = model.folds.clone();
    let revealed = model.revealed().map(String::from);
    let fold_id = fold.to_string();
    let (copy_key, copy_menu) = (text.downgrade(), text.downgrade());
    content
        .id(SharedString::from(format!("fold-{fold}")))
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Summary, "").expanded(open))
        .track_focus(&gpui::Focusable::focus_handle(text.read(cx), cx).tab_stop(true))
        .focus_ring(true)
        .key_context(act_markdown::KEY_CONTEXT)
        .on_action(move |_: &act_markdown::Copy, _, cx| copy_text(&copy_key, cx))
        .on_action(move |_: &act_ui::input::Copy, _, cx| copy_text(&copy_menu, cx))
        .relative()
        .flex()
        .items_center()
        .gap(px(9.0))
        .rounded(px(8.0))
        .text_color(t.quiet_strong)
        .text_size(px(cx.theme().metrics.text_meta))
        .cursor_pointer()
        .group(FOLD_SUMMARY)
        .hover(move |style| style.bg(hover))
        .child(fold_mark(open, &t))
        .child(selectable_area(text))
        .on_click(move |_, _, cx| {
            folds.borrow_mut().toggle(revealed.as_deref(), &fold_id, &holds);
            if let Some(root) = root.upgrade() {
                root.update(cx, |root, cx| {
                    root.conversation.remeasure(index);
                    cx.notify();
                });
            }
        })
}

fn copy_text(text: &WeakEntity<SelectableText>, cx: &mut App) {
    if !text.update(cx, |text, cx| text.copy_selection(cx)).unwrap_or(false) {
        cx.propagate();
    }
}

const fn family_icon(family: ToolFamily) -> IconName {
    match family {
        ToolFamily::Shell => IconName::Terminal,
        ToolFamily::Read => IconName::FileText,
        ToolFamily::Write => IconName::PenLine,
        ToolFamily::Search => IconName::Search,
        ToolFamily::Web => IconName::Globe,
        ToolFamily::Agent => IconName::Bot,
        ToolFamily::Thread => IconName::MessagesSquare,
        ToolFamily::Automation => IconName::Bell,
        ToolFamily::Other => IconName::Wrench,
    }
}

fn glyph(family: ToolFamily, cx: &App) -> impl IntoElement {
    let t = cx.theme().tokens.clone();
    rimmed(div(), &t, 6.0).flex_none().size(px(20.0)).flex().items_center().justify_center().bg(t.surface_raised).child(icon(family_icon(family), 12.0, t.muted_soft).stroke(1.75))
}

type CallKey = (AgentEngine, String, usize, usize);

thread_local! {
    /// A message never changes once written, so its call is read from it once per engine.
    static CALLS: RefCell<HashMap<CallKey, Rc<ToolCall>>> = RefCell::default();
}

fn tool_call(engine: AgentEngine, message: &ConversationMessage) -> Rc<ToolCall> {
    let key = (engine, message.id.clone(), message.text.len(), message.detail.as_ref().map(String::len).unwrap_or(0));
    CALLS.with(|calls| {
        let mut calls = calls.borrow_mut();
        if calls.len() > 20_000 {
            calls.clear();
        }
        calls.entry(key).or_insert_with(|| Rc::new(describe_tool_call(Some(engine), &message.text, message.detail.as_deref()))).clone()
    })
}

/// The thread a call named: by its id, an unambiguous prefix of one, or its exact title.
fn resolve_thread(names: &HashMap<String, String>, reference: &str) -> Option<(String, String)> {
    if let Some(title) = names.get(reference) {
        return Some((reference.to_string(), title.clone()));
    }
    let mut found = None;
    for (id, title) in names {
        if title != reference && !id.starts_with(reference) {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some((id.clone(), title.clone()));
    }
    found
}

/// What the call acted on. A call that names nothing falls back to its label, unless the label is already shown.
#[allow(clippy::too_many_arguments)]
fn subject(model: &RowModel, message_id: &str, detail: Option<&str>, call: &ToolCall, label_shown: bool, words: &Entity<SelectableText>, window: &Window, cx: &mut App) -> impl IntoElement {
    let t = cx.theme().tokens.clone();
    let mono = cx.theme().typography.mono_family.clone();
    let ui = cx.theme().typography.ui_family.clone();
    let fallback = if label_shown || call.thread.is_some() { String::new() } else { call.label.clone() };
    let mut arg = div().flex_1().min_w_0().flex().items_center().overflow_hidden().text_color(t.muted_strong).font_family(mono).text_size(px(12.0)).line_height(relative(1.4));
    if let Some(sigil) = &call.sigil {
        arg = arg.child(div().flex_none().mr(px(6.0)).text_color(t.quiet_soft).child(piece(words, sigil.clone(), Vec::new(), window, cx)));
    }
    if let Some(reference) = &call.thread {
        let thread = div().flex_none().mr(px(8.0)).font_family(ui).font_weight(FontWeight(550.0)).text_color(t.ink);
        match resolve_thread(&model.names, reference) {
            Some((id, title)) => {
                arg = arg.child(
                    thread
                        .id(SharedString::from(format!("thread-link-{message_id}")))
                        .cursor_pointer()
                        .hover(|style| style.text_color(t.accent_text).underline())
                        .child(piece(words, title, Vec::new(), window, cx))
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            dispatch(AppCommand::TaskSelect { task_id: id.clone() }, cx);
                        }),
                );
            }
            None => arg = arg.child(thread.child(piece(words, reference.clone(), Vec::new(), window, cx))),
        }
    }
    let text = if call.argument.is_empty() { fallback } else { call.argument.clone() };
    // The summary is read from the call's input; where it appears there verbatim, the current match lights in it.
    let (field, base) = match detail {
        Some(detail) if !text.is_empty() => (FindField::Detail, detail.find(text.as_str())),
        _ => (FindField::Text, None),
    };
    if text.is_empty() {
        return arg;
    }
    let lit = lit_ranges(model, message_id, field, &text, base, cx);
    arg.child(div().min_w_0().flex_1().ellipsis().child(piece(words, text, lit, window, cx)))
}

fn tip_of(call: &ToolCall) -> SharedString {
    [Some(call.label.clone()), call.thread.clone(), Some(call.argument.clone()).filter(|argument| !argument.is_empty())].into_iter().flatten().collect::<Vec<_>>().join(" · ").into()
}

fn elapsed(started: i64, ends: Option<i64>, now: i64, words: &Entity<SelectableText>, window: &Window, cx: &mut App) -> impl IntoElement {
    let t = cx.theme().tokens.clone();
    div().flex_none().text_color(t.quiet_strong).tabular_nums().child(piece(words, format_elapsed(ends.unwrap_or(now) - started), Vec::new(), window, cx))
}

/// One call, which opens to its input. `named` draws the tool it was, which only a run of mixed tools needs.
fn tool_step(model: &RowModel, index: usize, step: &TimedStep, named: bool, nested: bool, window: &mut Window, cx: &mut App) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    let call = tool_call(model.engine, &step.message);
    let label_shown = named || call.verb;
    let id = &step.message.id;
    let holds = vec![id.clone()];
    let fold = step_fold(id);
    let open = model.shown(&fold, &holds);
    let words = selectable(model, id, id, STEP_HEAD, window, cx);
    let mut head = div().min_h(px(33.0)).px(px(8.0));
    if named {
        head = head.child(glyph(call.family, cx));
    }
    if label_shown {
        head = head.child(div().flex_none().text_color(t.muted_soft).child(piece(&words, call.label.clone(), Vec::new(), window, cx)));
    }
    head = head.child(subject(model, id, step.message.detail.as_deref(), &call, label_shown, &words, window, cx));
    head = head.child(div().flex_none().flex().items_center().gap(px(9.0)).ml_auto().child(elapsed(step.message.at, step.ends_at, model.now, &words, window, cx)));
    let head = summary(model, &fold, holds, index, open, head, &words, cx).tip(tip_of(&call));
    let mut row = div().flex().flex_col();
    if !nested {
        row = rimmed(row, &t, 10.0).pane(t.glass_control);
    }
    row = row.child(head);
    if open {
        let body = match &step.message.detail {
            Some(detail) => {
                let ground = if nested { t.surface } else { t.sunken };
                let pre = div()
                    .border_1()
                    .border_color(t.line)
                    .rounded(px(8.0))
                    .p(px(11.0))
                    .bg(ground)
                    .text_color(t.muted_strong)
                    .font_family(cx.theme().typography.mono_family.clone())
                    .text_size(px(m.text_meta))
                    .line_height(relative(1.55))
                    .child(input_text(model, &step.message, detail, window, cx));
                div().mx(px(8.0)).mt(px(1.0)).mb(px(9.0)).child(copyable(&format!("input-{id}"), detail, "Copy the input", pre, ground, window, cx))
            }
            None => div().mx(px(8.0)).mb(px(9.0)).py(px(4.0)).text_color(t.quiet).text_size(px(m.text_meta)).child("No input"),
        };
        row = row.child(body);
    }
    row.into_any_element()
}

/// A run of tool calls: the newest stays visible, the rest hide behind a +N counter.
fn tool_run(model: &RowModel, index: usize, id: &str, steps: &[TimedStep], in_settled: bool, window: &mut Window, cx: &mut App) -> AnyElement {
    if steps.len() == 1 {
        return tool_step(model, index, &steps[0], true, in_settled, window, cx);
    }
    let t = cx.theme().tokens.clone();
    let newest = steps.last().unwrap();
    let call = tool_call(model.engine, &newest.message);
    let uniform = steps.iter().all(|step| step.message.text == steps[0].message.text);
    let hidden = steps.len() - 1;
    let holds: Vec<String> = steps.iter().map(|step| step.message.id.clone()).collect();
    let fold = run_fold(id);
    let open = model.shown(&fold, &holds);
    let words = selectable(model, id, &steps[0].message.id, RUN_HEAD, window, cx);
    let mut head = div().min_h(px(if in_settled { 33.0 } else { 38.0 })).px(px(if in_settled { 8.0 } else { 11.0 })).child(glyph(call.family, cx));
    if call.verb {
        head = head.child(div().flex_none().text_color(t.muted_soft).child(piece(&words, call.label.clone(), Vec::new(), window, cx)));
    }
    head = head.child(subject(model, &newest.message.id, newest.message.detail.as_deref(), &call, call.verb, &words, window, cx));
    let count = count_chip(piece(&words, format!("+{hidden}"), Vec::new(), window, cx), &t);
    let count = div().accessible(gpui::AccessInfo::new(gpui::AccessRole::Image, format!("{hidden} earlier tool {}", if hidden == 1 { "call" } else { "calls" }))).child(count);
    head = head.child(div().flex_none().flex().items_center().gap(px(9.0)).ml_auto().child(count).child(elapsed(steps[0].message.at, newest.ends_at, model.now, &words, window, cx)));
    let head = summary(model, &fold, holds, index, open, head, &words, cx).tip(tip_of(&call));
    // In a settled fold a run keeps its rim but not its fill.
    let mut run = rimmed(div().flex().flex_col(), &t, 10.0);
    if !in_settled {
        run = run.pane(t.glass_control);
    }
    run = run.child(head);
    if open {
        let mut rows = div().flex().flex_col().pt(px(1.0)).px(px(5.0)).pb(px(7.0));
        for step in steps {
            rows = rows.child(tool_step(model, index, step, !uniform, true, window, cx));
        }
        run = run.child(rows);
    }
    run.into_any_element()
}

/// A turn's steps: notes in Markdown, runs of tool calls folded. A live turn streams its newest text.
fn segments(model: &RowModel, index: usize, segments: &[Segment], live: bool, window: &mut Window, cx: &mut App) -> Vec<AnyElement> {
    let streaming_id = if live { model.tail.as_ref().map(|tail| tail.message_id.clone()).or_else(|| segments.last().and_then(|segment| if let Segment::Note { message, .. } = segment { Some(message.id.clone()) } else { None })) } else { None };
    let mut nodes: Vec<AnyElement> = Vec::new();
    for segment in segments {
        match segment {
            Segment::Tools { id, steps } => nodes.push(tool_run(model, index, id, steps, !live, window, cx)),
            Segment::Note { message, .. } => {
                let streaming = streaming_id.as_deref() == Some(message.id.as_str());
                let text = if streaming { format!("{}{}", message.text, model.tail.as_ref().filter(|tail| tail.message_id == message.id).map(|tail| tail.text.as_str()).unwrap_or("")) } else { message.text.clone() };
                let view = markdown(model, message, &text, streaming, Variant::WorkNote, window, cx);
                let shown = if streaming { None } else { previews(model, message, cx) };
                nodes.push(div().py(px(9.0)).child(view).children(shown).into_any_element());
            }
        }
    }
    if let Some(streaming) = &streaming_id {
        if !segments.iter().any(|segment| matches!(segment, Segment::Note { message, .. } if &message.id == streaming)) {
            let text = model.tail.as_ref().map(|tail| tail.text.clone()).unwrap_or_default();
            let view = message_view(SharedString::from(format!("md-{streaming}")), &text, true, Variant::WorkNote, Some(model.task_id.clone()), Some(image_scope(model, streaming)), window, cx);
            model.join_selection(&view, streaming, cx);
            nodes.push(div().py(px(9.0)).child(view).into_any_element());
        }
    }
    nodes
}

fn counted(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// What a settled turn changed and ran, so its folded row says whether it is worth opening.
fn digest(engine: AgentEngine, steps: &[Shared<ConversationMessage>]) -> String {
    let mut edits = 0;
    let mut commands = 0;
    for step in steps.iter().filter(|step| step.kind == MessageKind::Tool) {
        match tool_call(engine, step).family {
            ToolFamily::Write => edits += 1,
            ToolFamily::Shell => commands += 1,
            _ => {}
        }
    }
    let mut parts = Vec::new();
    if edits > 0 {
        parts.push(counted(edits, "edit", "edits"));
    }
    if commands > 0 {
        parts.push(counted(commands, "command", "commands"));
    }
    parts.join(" · ")
}

/// A settled turn: every step folds behind one framed row.
fn settled(model: &RowModel, index: usize, id: &str, steps: &[Shared<ConversationMessage>], ends_at: Option<i64>, window: &mut Window, cx: &mut App) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let holds: Vec<String> = steps.iter().map(|step| step.id.clone()).collect();
    let open = model.shown(id, &holds);
    let summary_words = [counted(steps.len(), "step", "steps"), digest(model.engine, steps)].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" · ");
    let words = selectable(model, id, &steps[0].id, SETTLED_HEAD, window, cx);
    let head = rimmed(div(), &t, 8.0)
        .min_h(px(30.0))
        .pl(px(11.0))
        .pr(px(9.0))
        .bg(t.control)
        .child(div().text_color(t.muted).child(piece(&words, "Worked", Vec::new(), window, cx)))
        .child(elapsed(steps[0].at, ends_at, model.now, &words, window, cx))
        .child(div().text_color(t.quiet_strong).child(piece(&words, summary_words, Vec::new(), window, cx)));
    let head = summary(model, id, holds, index, open, head, &words, cx);
    let mut fold = div().accessible(gpui::AccessInfo::new(gpui::AccessRole::Details, "")).flex().flex_col().items_start().child(head);
    if open {
        let parts = segments(model, index, &to_segments(time_steps(steps, ends_at)), false, window, cx);
        let steps = div().id(SharedString::from(format!("fold-steps-{id}"))).pt(px(2.0)).pb(px(6.0)).pl(px(20.0)).border_l_2().border_color(t.line).flex().flex_col().children(parts);
        fold = fold.child(div().w_full().mt(px(14.0)).pl(px(9.0)).child(if t.reduced_motion { steps } else { act_ui::style::fold_open(steps) }));
    }
    fold.into_any_element()
}

/// A message a coordinator's thread sent it, behind one row naming its sender until opened.
fn thread_message(model: &RowModel, index: usize, message: &ConversationMessage, from: &str, window: &mut Window, cx: &mut App) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    let holds = vec![message.id.clone()];
    let open = model.shown(&message.id, &holds);
    let words = selectable(model, &format!("head-{}", message.id), &message.id, SETTLED_HEAD, window, cx);
    let sender = piece(&words, from.to_string(), Vec::new(), window, cx);
    let mut head = div()
        .items_start()
        .px(px(11.0))
        .py(px(8.0))
        .child({
            let mut mark = div().flex_none();
            mark.style().align_self = Some(gpui::AlignSelf::Center);
            mark.child(icon(IconName::MessagesSquare, 14.0, t.accent_text))
        })
        .child(div().flex_none().text_color(t.ink_dim).font_weight(FontWeight(560.0)).child(sender));
    if open {
        head = head.child(div().flex_1()).border_b_1().border_color(t.line_soft);
    } else {
        // The preview flows as one paragraph, as white space does in a clamped box.
        let preview = message.text.split_whitespace().collect::<Vec<_>>().join(" ");
        let base = (preview == message.text).then_some(0);
        let lit = lit_ranges(model, &message.id, FindField::Text, &preview, base, cx);
        head = head.child(div().flex_1().min_w_0().line_clamp(2).text_ellipsis().overflow_hidden().text_color(t.muted).line_height(relative(1.5)).child(piece(&words, preview, lit, window, cx)));
    }
    let head = summary_with(model, &message.id, holds, index, open, head, t.wash, &words, cx).items_start().rounded(px(0.0));
    let mut card = rimmed(div(), &t, 12.0).overflow_hidden().flex().flex_col().pane(t.glass_control).child(head);
    if open {
        let mut body = div().id(SharedString::from(format!("thread-message-open-{}", message.id))).flex().flex_col().items_start().gap(px(10.0)).pt(px(12.0)).pr(px(14.0)).pb(px(12.0)).pl(px(33.0));
        if !t.reduced_motion {
            body = act_ui::style::fold_open(body);
        }
        let text = selectable(model, &format!("body-{}", message.id), &message.id, BODY, window, cx);
        text.update(cx, |text, _| text.set_separator("\n\n"));
        let mut offset = 0;
        let mut paragraphs: Vec<Div> = Vec::new();
        for line in message.text.split('\n') {
            if !line.trim().is_empty() {
                let lit = lit_ranges(model, &message.id, FindField::Text, line, Some(offset), cx);
                paragraphs.push(div().child(piece(&text, line.to_string(), lit, window, cx)));
            }
            offset += line.len() + 1;
        }
        let (copy_key, copy_menu) = (text.downgrade(), text.downgrade());
        let paragraphs = div()
            .id(SharedString::from(format!("thread-message-body-{}", message.id)))
            .track_focus(&gpui::Focusable::focus_handle(text.read(cx), cx))
            .key_context(act_markdown::KEY_CONTEXT)
            .on_action(move |_: &act_markdown::Copy, _, cx| copy_text(&copy_key, cx))
            .on_action(move |_: &act_ui::input::Copy, _, cx| copy_text(&copy_menu, cx))
            .relative()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .text_color(t.ink_dim)
            .text_size(px(m.text_meta))
            .line_height(relative(1.55))
            .children(paragraphs)
            .child(selectable_area(&text));
        body = body.child(paragraphs);
        if let Some(sender) = sender_thread(message.origin.as_ref()) {
            body = body.child(open_thread_chip(SharedString::from(format!("open-sender-{}", message.id)), "Open thread".into(), sender, None, cx));
        }
        card = card.child(body);
    }
    card.into_any_element()
}

/// Earlier thread updates and the coordinator's answers to them, behind one row until opened.
#[allow(clippy::too_many_arguments)]
fn updates(model: &Rc<RowModel>, index: usize, id: &str, group: &Group, entries: &[Entry], count: usize, row_group: &SharedString, window: &mut Window, cx: &mut App) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let holds = group_message_ids(group);
    let open = model.shown(id, &holds);
    let words = selectable(model, id, holds.first().map_or(id, String::as_str), SETTLED_HEAD, window, cx);
    let head = rimmed(div(), &t, 8.0)
        .min_h(px(30.0))
        .pl(px(11.0))
        .pr(px(9.0))
        .bg(t.control)
        .child(div().text_color(t.muted).child(piece(&words, "Earlier thread updates", Vec::new(), window, cx)))
        .child(count_chip(piece(&words, format!("{count} {}", if count == 1 { "update" } else { "updates" }), Vec::new(), window, cx), &t));
    let mut fold = div().flex().flex_col().items_start().child(summary(model, id, holds, index, open, head, &words, cx));
    if open {
        let mut list = div().w_full().flex().flex_col().gap(px(26.0)).pt(px(16.0)).pb(px(4.0));
        for item in entries {
            list = list.child(self::entry(model, index, item, false, row_group, window, cx));
        }
        fold = fold.child(list);
    }
    fold.into_any_element()
}
