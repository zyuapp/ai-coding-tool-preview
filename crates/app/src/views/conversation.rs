//! The transcript: a virtual list of rows, the run's status at its foot, the approval waiting on the
//! reader, the way back to the latest message, and where the reader is kept reading
//! (ConversationTimeline.tsx, WorkspaceConversation.tsx, ApprovalCard.tsx, use-reading-view.ts).

use std::cell::{Cell, RefCell};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::time::{Duration, Instant};

use act_core::domain::coordination::is_coordinator;
use act_core::domain::engine::engine_label;
use act_core::domain::find::{FindHit, FindTarget};
use act_core::domain::session_resources::{ResourceCleanup, ResourceStop};
use act_core::domain::usage_limit::{lift_time, line_positions, resumes_on_its_own, LimitPause};
use act_core::state::{ApprovalView, ReadingPoint, RunPhase, WorkspaceState};
use act_core::workspace::activity::{run_status, waiting_on, ThreadWait};
use act_core::AppCommand;
use act_ui::components::button::{Button, ButtonVariant, IconButton, IconButtonSize, IconButtonVariant};
use act_ui::overlay::{bounds_cell, bounds_probe, BoundsCell};
use act_ui::style::{glass, GlassKind, Raised};
use act_ui::theme::ActiveTheme;
use act_ui::IconName;
use gpui::{div, list, prelude::*, px, relative, AnyElement, App, Context, FontWeight, ListAlignment, ListOffset, ListState, SharedString, Window};
use serde_json::Value;

use super::common::{format_elapsed, icon, with_alpha};
use super::letter_spacing::LetterSpacing;
use super::grouping::{group_timeline, message_rows, Entry, Group, Options};
use super::root::Root;
use super::spinner::bead;
use super::sweep::sweep;
use super::timeline_row::{self, FindPaint, Folds, RowModel};
use crate::workspace::dispatch;

/// Where the view sits. `Row` holds a fresh answer's top in view; `Kept` the place a reader left;
/// `Rest` is wherever the reader or a found match put it, which nothing takes back.
#[derive(Clone, Debug, PartialEq)]
enum View {
    Foot,
    Row { id: String, depth: f32 },
    Kept { id: String, depth: f32 },
    Rest,
}

/// How far into a row an answer's first line sits, so it is not flush against the top of the view.
const ANSWER_DEPTH: f32 = -16.0;
/// How long a view waits for its reader to stop moving before telling the workspace where they are.
const READING_SETTLE: Duration = Duration::from_millis(150);
/// A view with less than this below it is at its foot.
const FOOT_SLACK: f32 = 120.0;
/// How long a scroll after the reader's last input still belongs to that input, which carries a
/// trackpad's momentum past the gaps between its events.
const READER_GRIP: Duration = Duration::from_millis(400);
/// Input this soon after the last is the same gesture still going.
const STREAK: Duration = Duration::from_millis(100);

struct Grouped {
    key: u64,
    groups: Rc<Vec<Group>>,
    rows: Rc<HashMap<String, usize>>,
}

pub struct ConversationState {
    pub list: ListState,
    /// Whether the last press landed in the transcript, which the page's scrolling keys then move.
    pub keys_scroll: Rc<Cell<bool>>,
    /// The scrollbar's room the rows were last measured beside.
    gutter: f32,
    /// Set by the reader's own scroll, which takes the view: to its foot, or to rest where it stopped.
    scrolled: Rc<Cell<bool>>,
    view: Rc<RefCell<View>>,
    /// Whether the scroll-to-end button is hidden: the view is at, or on its way to, its foot.
    at_foot: Rc<Cell<bool>>,
    /// Set when the reader moved the view, so where they settled is reported.
    moved_at: Rc<Cell<Option<Instant>>>,
    /// When the reader last scrolled any thread, which outlives a switch.
    input_at: Rc<Cell<Option<Instant>>>,
    /// A gesture still going when this thread opened was meant for the one before; it is put back
    /// rather than taken as this reader's until it pauses.
    carried: Rc<Cell<bool>>,
    /// Set when a carried gesture moved the view, which is placed again.
    put_back: Rc<Cell<bool>>,
    /// Whether the transcript is tucked away behind an expanded dock.
    tucked: bool,
    thread: Option<String>,
    signatures: Vec<u64>,
    /// The view still needs placing, once its rows exist.
    unplaced: bool,
    /// The rows changed since the view was last placed, which a view at its foot follows.
    moved: bool,
    extent: Option<(gpui::Pixels, gpui::Size<gpui::Pixels>)>,
    grouped: Option<Grouped>,
    folds: Rc<RefCell<Folds>>,
    /// Ticks once a second while something on screen counts time.
    ticker: Option<gpui::Task<()>>,
    settle: Option<gpui::Task<()>>,
    /// When the status row first showed, so a wait counts from its own start.
    shown_since: Option<(String, i64)>,
    /// The newest answer, tool call and prompt seen, which move a following view.
    newest: (Option<String>, Option<String>, Option<String>),
    /// The match last jumped to, and whether it still waits to be centred.
    found: Option<(FindHit, u8)>,
    reported: Option<ReadingPoint>,
    approval_probe: BoundsCell,
    /// Whether the approval card floats over the transcript's foot in this draw.
    approval_shown: bool,
    scroll_button: BoundsCell,
    scroll_button_shown: bool,
    brief_open: bool,
    annotated_at: Option<Instant>,
    pub annotate: super::annotate::AnnotateState,
    /// The transcript's messages, selected across as one page.
    selection: act_markdown::SelectionGroup,
    /// Each message's place in reading order, kept while the thread's messages stay the same.
    order: Option<(String, usize, Rc<HashMap<String, u64>>)>,
    /// Each thread's title by id, kept until the workspace's outline changes.
    names: Option<((u64, usize), Rc<HashMap<String, String>>)>,
}

impl ConversationState {
    /// Whether a drag is selecting across the transcript.
    pub fn selecting(&self) -> bool {
        self.selection.dragging().is_some()
    }

    /// The scroll-to-end button's bounds while it shows.
    pub fn scroll_button_bounds(&self) -> Option<gpui::Bounds<gpui::Pixels>> {
        self.scroll_button_shown.then(|| self.scroll_button.get()).flatten()
    }

    /// The approval card's bounds while it floats over the transcript.
    pub fn approval_bounds(&self) -> Option<gpui::Bounds<gpui::Pixels>> {
        self.approval_shown.then(|| self.approval_probe.get()).flatten()
    }

    pub fn new() -> ConversationState {
        let list = ListState::new(0, ListAlignment::Top, px(1200.0));
        let view = Rc::new(RefCell::new(View::Foot));
        let at_foot = Rc::new(Cell::new(true));
        let moved_at = Rc::new(Cell::new(None));
        let scrolled = Rc::new(Cell::new(false));
        let input_at: Rc<Cell<Option<Instant>>> = Rc::new(Cell::new(None));
        let carried = Rc::new(Cell::new(false));
        let put_back = Rc::new(Cell::new(false));
        {
            let (scrolled, moved_at, input_at, carried, put_back) = (scrolled.clone(), moved_at.clone(), input_at.clone(), carried.clone(), put_back.clone());
            list.set_scroll_handler(move |event, _, _| {
                if !event.is_scrolled {
                    return;
                }
                let now = Instant::now();
                let last = input_at.replace(Some(now));
                let streak = last.is_some_and(|at| now.duration_since(at) < STREAK);
                if carried.get() && streak {
                    put_back.set(true);
                    return;
                }
                carried.set(false);
                scrolled.set(true);
                moved_at.set(Some(now));
            });
        }
        ConversationState {
            list,
            keys_scroll: Rc::new(Cell::new(false)),
            gutter: 0.0,
            scrolled,
            view,
            at_foot,
            moved_at,
            input_at,
            carried,
            put_back,
            tucked: false,
            thread: None,
            signatures: Vec::new(),
            unplaced: true,
            moved: true,
            extent: None,
            grouped: None,
            folds: Rc::default(),
            ticker: None,
            settle: None,
            shown_since: None,
            newest: (None, None, None),
            found: None,
            reported: None,
            approval_probe: bounds_cell(),
            approval_shown: false,
            selection: act_markdown::SelectionGroup::new(),
            order: None,
            names: None,
            scroll_button: bounds_cell(),
            scroll_button_shown: false,
            brief_open: false,
            annotated_at: None,
            annotate: Default::default(),
        }
    }

    /// Hands the workspace where this thread's reader settled, unless that is what it holds already.
    fn report(&mut self, cx: &mut App) {
        self.moved_at.set(None);
        let (Some(thread), Some(grouped)) = (self.thread.clone(), self.grouped.as_ref()) else { return };
        let point = self.observe(&grouped.groups.clone());
        if point != self.reported {
            self.reported = point.clone();
            dispatch(AppCommand::ReadingPoint { task_id: thread, point }, cx);
        }
    }

    /// Opens a thread: its own folds, and the place its reader left, or its foot.
    fn open(&mut self, thread: Option<&str>, point: Option<&ReadingPoint>, rows: &HashMap<String, usize>, count: usize) {
        self.thread = thread.map(String::from);
        self.list.reset(count);
        self.signatures.clear();
        self.folds = Rc::default();
        self.found = None;
        self.newest = (None, None, None);
        self.moved_at.set(None);
        self.reported = point.cloned();
        self.annotate = Default::default();
        let kept = point.filter(|point| rows.contains_key(&point.anchor));
        *self.view.borrow_mut() = match kept {
            Some(point) => View::Kept { id: point.anchor.clone(), depth: point.depth as f32 },
            None => View::Foot,
        };
        self.at_foot.set(kept.is_none());
        self.unplaced = true;
        self.carried.set(self.input_at.get().is_some_and(|at| at.elapsed() < READER_GRIP));
        self.put_back.set(false);
        self.scrolled.set(false);
    }

    /// A transcript hidden behind an expanded dock is left where its reader last saw it, rather
    /// than wherever new output would take it, and that place is reported.
    pub fn tuck_away(&mut self, cx: &mut App) {
        if self.tucked || self.thread.is_none() {
            return;
        }
        self.tucked = true;
        let Some(groups) = self.grouped.as_ref().map(|grouped| grouped.groups.clone()) else { return };
        if !matches!(*self.view.borrow(), View::Kept { .. }) {
            if let Some(point) = self.observe(&groups) {
                *self.view.borrow_mut() = View::Kept { id: point.anchor, depth: point.depth as f32 };
            }
        }
        self.report(cx);
    }

    /// Brings the list's rows into step with `signatures`, re-measuring from the first that changed.
    fn sync(&mut self, signatures: Vec<u64>, estimates: &[f32]) {
        if self.list.item_count() != self.signatures.len() {
            self.list.reset_estimated(estimates.iter().map(|height| px(*height)));
            self.moved = true;
        } else if sync_rows(&self.list, &self.signatures, &signatures, estimates) {
            self.moved = true;
        }
        self.signatures = signatures;
        // What is drawn grew or the view changed size, as a diagram that finished drawing does.
        let extent = (self.list.max_offset_for_scrollbar().height, self.list.viewport_bounds().size);
        if self.extent != Some(extent) {
            self.extent = Some(extent);
            self.moved = true;
        }
    }

    /// Puts the view where it is held, again whenever the rows change under it until the reader
    /// scrolls: the foot follows what arrives; a row or a kept place keeps its row where it was put.
    fn place(&mut self, rows: &HashMap<String, usize>) {
        let view = self.view.borrow().clone();
        match view {
            View::Foot => {
                if self.moved || self.unplaced {
                    self.list.scroll_to(ListOffset { item_ix: self.signatures.len(), offset_in_item: px(0.0) });
                }
            }
            View::Row { id, depth } | View::Kept { id, depth } => {
                if !self.unplaced && !self.moved {
                    return;
                }
                if let Some(row) = rows.get(&id) {
                    self.list.scroll_to(ListOffset { item_ix: *row, offset_in_item: px(depth.max(0.0)) });
                    if depth < 0.0 {
                        self.list.scroll_by(px(depth));
                    }
                }
            }
            View::Rest => {}
        }
        self.unplaced = false;
        self.moved = false;
    }

    /// Scrolls as a browser's page keys scroll the scroller last pressed in: arrows by a line,
    /// Page Up/Down and Space by most of the view, Home and End to the ends. The place is the reader's.
    pub fn scroll_by_key(&mut self, key: &str, shift: bool) -> bool {
        let height = f32::from(self.list.viewport_bounds().size.height);
        let page = (height * 0.875).max(height - 40.0 * 2.0).max(40.0);
        let distance = match (key, shift) {
            ("down", false) => 40.0,
            ("up", false) => -40.0,
            ("pagedown", _) | ("space", false) => page,
            ("pageup", _) | ("space", true) => -page,
            ("home", false) => {
                self.list.scroll_to(ListOffset { item_ix: 0, offset_in_item: px(0.0) });
                self.scrolled.set(true);
                self.moved_at.set(Some(Instant::now()));
                return true;
            }
            ("end", false) => {
                self.scroll_to_foot();
                self.scrolled.set(true);
                return true;
            }
            _ => return false,
        };
        self.list.scroll_by(px(distance));
        self.scrolled.set(true);
        self.moved_at.set(Some(Instant::now()));
        true
    }

    /// Takes the reader back to the latest message.
    pub fn scroll_to_foot(&mut self) {
        *self.view.borrow_mut() = View::Foot;
        self.at_foot.set(true);
        self.list.scroll_to(ListOffset { item_ix: self.signatures.len(), offset_in_item: px(0.0) });
        self.moved_at.set(Some(Instant::now()));
    }

    /// Brings a message's row into view, as revealing an annotation asks.
    pub fn reveal(&mut self, message_id: &str, window: &Window) {
        let Some(row) = self.grouped.as_ref().and_then(|grouped| grouped.rows.get(message_id)).copied() else { return };
        *self.view.borrow_mut() = View::Rest;
        self.at_foot.set(false);
        self.list.scroll_to(ListOffset { item_ix: row, offset_in_item: px(0.0) });
        self.list.scroll_by(px(-f32::from(window.viewport_size().height) * 0.3));
    }

    /// Whether the reader scrolled since the annotation popovers were placed, which puts them away.
    pub fn scrolled_since_annotating(&mut self) -> bool {
        let moved = self.moved_at.get();
        let fresh = moved.is_some() && moved != self.annotated_at;
        self.annotated_at = moved;
        fresh
    }

    /// Re-measures one row, as a fold opening in it changes its height.
    pub fn remeasure(&mut self, index: usize) {
        if index < self.signatures.len() {
            self.list.remeasure_items(index..index + 1);
        }
    }

    /// Where the reader is: the row at the top of the view, and how far into it.
    fn observe(&self, groups: &[Group]) -> Option<ReadingPoint> {
        let top = self.list.logical_scroll_top();
        groups.get(top.item_ix).map(|group| ReadingPoint { anchor: group.id().to_string(), depth: f64::from(f32::from(top.offset_in_item)) })
    }
}

/// The height a status row under the transcript is counted at until it is drawn.
pub const STATUS_ESTIMATE: f32 = 40.0;

/// Brings a transcript list's rows from `old` to `new` signatures: changed rows are measured again
/// (counted at their last height meanwhile), added rows at `estimates`. Whether anything changed.
pub fn sync_rows(list: &ListState, old: &[u64], new: &[u64], estimates: &[f32]) -> bool {
    let common = old.len().min(new.len());
    let first_change = old.iter().zip(new).position(|(old, new)| old != new).unwrap_or(common);
    if first_change == common && old.len() == new.len() {
        return false;
    }
    if first_change < common {
        list.remeasure_items(first_change..common);
    }
    if new.len() > old.len() {
        list.splice_estimated(old.len()..old.len(), estimates[old.len()..].iter().map(|height| px(*height)));
    } else if new.len() < old.len() {
        list.splice(new.len()..old.len(), 0);
    }
    true
}

/// How far the list is from its foot, as last laid out.
fn remaining(list: &ListState) -> f32 {
    let max = f32::from(list.scroll_max());
    let at = -f32::from(list.scroll_px_offset_for_scrollbar().y);
    (max - at).max(0.0)
}

fn signature(group: &Group, live_tail: Option<&str>, holds_hit: bool) -> u64 {
    let mut hasher = DefaultHasher::new();
    group.id().hash(&mut hasher);
    match group {
        Group::Entry(Entry::Message { message, from, .. }) => {
            message.text.len().hash(&mut hasher);
            from.hash(&mut hasher);
        }
        Group::Entry(Entry::Turn { steps, final_message, ends_at, live, .. }) => {
            steps.len().hash(&mut hasher);
            steps.last().map(|step| (step.id.as_str(), step.text.len())).hash(&mut hasher);
            final_message.as_ref().map(|message| (message.id.as_str(), message.text.len())).hash(&mut hasher);
            ends_at.hash(&mut hasher);
            live.hash(&mut hasher);
            if *live {
                live_tail.hash(&mut hasher);
            }
        }
        Group::Entry(Entry::Threads { thread_ids, .. }) => thread_ids.hash(&mut hasher),
        Group::Updates { count, .. } => count.hash(&mut hasher),
    }
    holds_hit.hash(&mut hasher);
    hasher.finish()
}

/// What each wait is called, in the thread the wait is happening in.
fn wait_label(wait: ThreadWait) -> &'static str {
    match wait {
        ThreadWait::Worktree => "Creating worktree…",
        ThreadWait::WorktreeRelease => "Removing worktree…",
        ThreadWait::Run => "Starting…",
    }
}

pub(super) fn retry_label(retry: &act_core::domain::run::RetryNotice) -> String {
    let count = match (retry.attempt, retry.max_retries) {
        (Some(attempt), Some(max)) => format!(" {attempt} of {max}"),
        (Some(attempt), None) => format!(" {attempt}"),
        _ => String::new(),
    };
    format!("{} Retrying{count}…", retry.message)
}

fn limit_label(pause: &LimitPause, position: Option<usize>, now: i64) -> String {
    if !resumes_on_its_own(pause.window) {
        return if now < pause.resets_at { format!("Weekly usage limit reached. Resets {}.", lift_time(pause.resets_at, now)) } else { "Weekly usage limit reset.".into() };
    }
    let behind = match position {
        Some(position) if position > 1 => format!(" after {} other {}", position - 1, if position == 2 { "thread" } else { "threads" }),
        _ => String::new(),
    };
    if now < pause.resets_at {
        format!("Usage limit reached. Resumes at {}{behind}.", lift_time(pause.resets_at, now))
    } else {
        format!("Usage limit reset. Resuming{behind}.")
    }
}

/// The newest message the user sent themselves, rather than one another thread or a schedule sent for them.
fn newest_prompt(messages: &[act_core::shared::Shared<act_core::domain::conversation::ConversationMessage>]) -> Option<String> {
    messages.iter().rev().find(|message| message.kind == act_core::domain::conversation::MessageKind::User && message.origin.is_none()).map(|message| message.id.clone())
}

/// What the run is doing, set where the reply will begin.
pub(super) enum Status {
    ResourceStop { stop: ResourceStop, task_id: String },
    Waiting { wait: ThreadWait, since: i64 },
    Limit { pause: LimitPause, position: Option<usize>, task_id: String },
    Compacting,
    Retrying(String),
    Thinking { since: i64, blocked: bool },
}

impl Status {
    pub(super) fn key(&self) -> String {
        match self {
            Status::ResourceStop { stop, .. } => format!("memory-{}-{:?}-{}", stop.at, stop.cleanup, MEMORY_DETAILS.with(|open| open.borrow().contains(&stop.at))),
            Status::Waiting { wait, .. } => format!("wait-{wait:?}"),
            Status::Limit { pause, position, .. } => format!("limit-{}-{position:?}", pause.resets_at),
            Status::Compacting => "compacting".into(),
            Status::Retrying(label) => format!("retry-{label}"),
            Status::Thinking { blocked, .. } => format!("thinking-{blocked}"),
        }
    }

    pub(super) fn render(&self, model: &RowModel, cx: &mut App) -> AnyElement {
        let theme = cx.theme().clone();
        let t = &theme.tokens;
        let m = &theme.metrics;
        let row = div().flex().items_center().gap(px(8.0)).max_w_full().min_h(px(28.0)).mb(px(28.0)).text_color(t.muted).text_size(px(m.text_meta)).line_height(relative(1.0));
        let elapsed = |since: i64| div().text_color(t.quiet_strong).tabular_nums().child(format_elapsed(model.now - since));
        let content = match self {
            Status::ResourceStop { stop, task_id } => return timeline_row::column(model.inset, model.side, memory_stop_notice(stop, task_id, cx)).into_any_element(),
            Status::Waiting { wait, since } => row.child(icon(IconName::FolderSymlink, 14.0, t.muted_strong).stroke(1.8)).child(sweep("status-wait", wait_label(*wait), cx)).child(elapsed(*since)),
            Status::Limit { pause, position, task_id } => {
                let task_id = task_id.clone();
                let (label, command) = if resumes_on_its_own(pause.window) { ("Cancel auto-resume", AppCommand::LimitCancel { task_id }) } else { ("Resume", AppCommand::LimitResume { task_id }) };
                let tone = t.tone(act_ui::theme::Tone::Warning);
                return timeline_row::column(
                    model.inset,
                    model.side,
                    div()
                        .mb(px(28.0))
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .px(px(12.0))
                        .py(px(8.0))
                        .rounded(px(10.0))
                        .border_1()
                        .border_color(tone.line)
                        .bg(tone.surface)
                        .text_color(t.ink_soft)
                        .text_size(px(m.text_menu))
                        .child(icon(IconName::Hourglass, 16.0, tone.text).stroke(1.8))
                        .child(div().flex_1().child(limit_label(pause, *position, model.now)))
                        .child(Button::new("limit-action", label).variant(ButtonVariant::Secondary).pill(true).on_click(move |_, _, cx| dispatch(command.clone(), cx))),
                )
                .into_any_element();
            }
            Status::Compacting => row.child(bead("compacting", 14.0, t, false)).child(sweep("status-compacting", "Compacting messages…", cx)),
            Status::Retrying(label) => row.child(bead("retrying", 14.0, t, false)).child(sweep("status-retrying", label.clone(), cx)),
            Status::Thinking { blocked: true, .. } => row.child(bead("held", 14.0, t, true)).child("Waiting for your approval"),
            Status::Thinking { since, .. } => row.child(bead("thinking", 14.0, t, false)).child(sweep("status-working", "Working", cx)).child(elapsed(*since)),
        };
        timeline_row::column(model.inset, model.side, content).into_any_element()
    }
}

std::thread_local! {
    /// Memory stops, by when they happened, whose details are open.
    static MEMORY_DETAILS: RefCell<std::collections::HashSet<i64>> = RefCell::new(std::collections::HashSet::new());
}

/// A thread stopped for its engine's memory, resumed once its processes have exited (ConversationTimeline.tsx).
fn memory_stop_notice(stop: &ResourceStop, task_id: &str, cx: &mut App) -> AnyElement {
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let m = &theme.metrics;
    let tone = t.tone(act_ui::theme::Tone::Warning);
    let open = MEMORY_DETAILS.with(|details| details.borrow().contains(&stop.at));
    let at = stop.at;
    let command = AppCommand::ResourceResume { task_id: task_id.to_string() };
    let cleanup = match stop.cleanup {
        ResourceCleanup::Stopping => "Stopping owned processes…",
        ResourceCleanup::Incomplete => "Some owned processes have not exited. Resume will be available once they stop.",
        ResourceCleanup::Complete => "Owned processes have exited.",
    };
    let paragraph = |text: SharedString| div().mt(px(4.0)).text_color(t.ink_dim).child(text);
    let details = div()
        .id("memory-stop-details")
        .mt(px(4.0))
        .flex()
        .items_center()
        .gap(px(4.0))
        .cursor_pointer()
        .text_color(t.muted)
        .child(act_ui::Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).size(px(12.0)))
        .child("Details")
        .on_click(move |_, window, _| {
            MEMORY_DETAILS.with(|details| {
                let mut details = details.borrow_mut();
                if !details.remove(&at) {
                    details.insert(at);
                }
            });
            window.refresh();
        });
    let text = div()
        .flex_1()
        .min_w_0()
        .child(div().text_color(t.ink).font_weight(FontWeight(600.0)).child("Stopped due to high memory usage"))
        .child(paragraph("This thread and its background agents were stopped to keep the app responsive. Work already saved is preserved.".into()))
        .child(paragraph("An interrupted action may have partially completed.".into()))
        .child(details)
        .when(open, |text| text.child(paragraph(format!("Memory at stop: {:.1} GB. {cleanup}", stop.bytes as f64 / (1024.0 * 1024.0 * 1024.0)).into())));
    div()
        .mb(px(28.0))
        .flex()
        .items_start()
        .gap(px(10.0))
        .px(px(12.0))
        .py(px(10.0))
        .rounded(px(10.0))
        .border_1()
        .border_color(tone.line)
        .bg(tone.surface)
        .text_color(t.ink_soft)
        .text_size(px(m.text_menu))
        .child(div().pt(px(1.0)).child(icon(IconName::TriangleAlert, 16.0, tone.text)))
        .child(text)
        .child(
            Button::new("memory-resume", "Resume")
                .variant(ButtonVariant::Secondary)
                .pill(true)
                .disabled(stop.cleanup != ResourceCleanup::Complete)
                .on_click(move |_, _, cx| dispatch(command.clone(), cx)),
        )
        .into_any_element()
}

pub fn render(root: &mut Root, state: &WorkspaceState, window: &mut Window, cx: &mut Context<Root>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let m = &theme.metrics;
    let viewport = window.viewport_size();
    let (inset, side) = super::session_panel::conversation_insets(state, f32::from(viewport.width), m.page_inset(f32::from(viewport.width)));
    let Some(thread) = state.current_thread() else { return div().into_any_element() };
    if thread.history_summary.is_some() {
        return div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .pb(px(f32::from(viewport.height) * 0.11))
            .text_size(px(m.text_control))
            .child(sweep("loading-conversation", "Loading conversation…", cx))
            .into_any_element();
    }
    let status = run_status(state, Some(&thread.id));
    let running = status == act_core::state::ThreadRunStatus::Running;
    let tail = state.streaming_tails.get(&thread.id).cloned();
    if thread.messages.is_empty() && tail.is_none() {
        root.conversation.thread = None;
        return div()
            .size_full()
            .flex()
            .flex_col()
            .justify_end()
            .items_center()
            .pb(px(root.composer.height() + 10.0))
            .child(div().text_color(t.ink).text_size(px(m.text_hero)).font_weight(FontWeight(600.0)).line_height(relative(1.2)).letter_spacing(-0.025).child(format!("What should {} work on?", engine_label(thread.engine))))
            .into_any_element();
    }
    let coordinator = is_coordinator(Some(thread));
    let coordination = super::coordination::view(state, Some(thread));
    let cards = coordinator.then(|| Rc::new(super::coordination::cards_of(state, &coordination)));
    let starts: Vec<(String, i64)> = if coordinator { coordination.starts.iter().map(|start| (start.id.clone(), start.at)).collect() } else { Vec::new() };

    // The thread being left keeps where its view was, wherever that came from, so it reopens there;
    // a kept place it never left is what it holds already.
    if root.conversation.thread.as_deref() != Some(thread.id.as_str()) {
        let placed = !root.conversation.unplaced && !matches!(*root.conversation.view.borrow(), View::Kept { .. });
        if placed || root.conversation.moved_at.get().is_some() {
            root.conversation.report(cx);
        }
    }
    // Groups are rebuilt only when what they are cut from changed.
    let key = {
        let mut hasher = DefaultHasher::new();
        thread.id.hash(&mut hasher);
        thread.messages.len().hash(&mut hasher);
        for message in thread.messages.iter().rev().take(2) {
            (&message.id, message.text.len(), message.detail.as_ref().map(String::len), message.answer).hash(&mut hasher);
        }
        thread.messages.first().map(|message| &message.id).hash(&mut hasher);
        (running, tail.as_ref().map(|tail| &tail.message_id), thread.run_ended_at, coordinator, &starts).hash(&mut hasher);
        hasher.finish()
    };
    if root.conversation.grouped.as_ref().is_none_or(|grouped| grouped.key != key) {
        let groups = group_timeline(&thread.messages, &Options { running, tail_message_id: tail.as_ref().map(|tail| tail.message_id.as_str()), run_ended_at: thread.run_ended_at, coordinator, starts: &starts });
        let rows = message_rows(&groups);
        root.conversation.grouped = Some(Grouped { key, groups: Rc::new(groups), rows: Rc::new(rows) });
    }
    let (groups, rows) = {
        let grouped = root.conversation.grouped.as_ref().unwrap();
        (grouped.groups.clone(), grouped.rows.clone())
    };
    if root.conversation.thread.as_deref() != Some(thread.id.as_str()) {
        let point = state.reading_points.get(&thread.id).and_then(Option::as_ref);
        root.conversation.open(Some(&thread.id), point, &rows, groups.len());
    }

    // The find bar, when this transcript is the one it searches.
    let find = act_core::view::find::find_view(state).filter(|view| matches!(&view.find.target, FindTarget::Thread { task_id } if task_id.as_deref() == Some(thread.id.as_str()))).map(|view| FindPaint { needle: view.find.query.trim().to_lowercase(), hit: view.hit });
    let folds = root.conversation.folds.clone();
    let hit_row = find.as_ref().and_then(|find| find.hit.as_ref()).and_then(|hit| rows.get(&hit.message_id)).copied();
    let mut signatures: Vec<u64> = groups.iter().enumerate().map(|(index, group)| signature(group, tail.as_ref().map(|tail| tail.text.as_str()), hit_row == Some(index))).collect();

    let run = state.active_runs.get(&thread.id);
    let compacting = run.is_some_and(|run| matches!(run.phase, RunPhase::Compacting));
    let retrying = run.and_then(|run| match &run.phase {
        RunPhase::Retrying { retry } => Some(retry_label(retry)),
        _ => None,
    });
    let approval: Option<ApprovalView> = run.filter(|run| matches!(run.phase, RunPhase::AwaitingApproval)).and_then(|run| state.approvals.get(&run.run_id).cloned());
    let coordinated_approval = coordinator && approval.is_some();
    let wait = waiting_on(state, Some(&thread.id), &act_core::reducer::sending::prompt_key(state));
    let live_since = match groups.last() {
        Some(Group::Entry(Entry::Turn { live: true, steps, .. })) => steps.first().map(|step| step.at),
        _ => None,
    };
    let now = act_core::util::now();
    let mut statuses: Vec<Status> = Vec::new();
    if let Some(stop) = &thread.resource_stop {
        statuses.push(Status::ResourceStop { stop: stop.clone(), task_id: thread.id.clone() });
    }
    if let Some(wait) = wait {
        statuses.push(Status::Waiting { wait, since: now });
    }
    if !running {
        if let Some(pause) = &thread.limit_pause {
            let position = line_positions(&state.threads).get(&thread.id).copied();
            statuses.push(Status::Limit { pause: pause.clone(), position, task_id: thread.id.clone() });
        }
    }
    if running {
        if compacting {
            statuses.push(Status::Compacting);
        } else if let Some(retry) = retrying {
            statuses.push(Status::Retrying(retry));
        } else {
            statuses.push(Status::Thinking { since: now, blocked: approval.is_some() });
        }
    }
    let status_key = statuses.iter().map(Status::key).collect::<Vec<_>>().join("|");
    let since = match &root.conversation.shown_since {
        Some((key, at)) if *key == status_key => *at,
        _ => {
            root.conversation.shown_since = Some((status_key.clone(), now));
            now
        }
    };
    for status in &mut statuses {
        match status {
            Status::Waiting { since: at, .. } => *at = since,
            Status::Thinking { since: at, .. } => *at = live_since.unwrap_or(since).min(since),
            _ => {}
        }
    }
    for status in &statuses {
        let mut hasher = DefaultHasher::new();
        status.key().hash(&mut hasher);
        signatures.push(hasher.finish());
    }
    let estimates: Vec<f32> = groups.iter().map(Group::estimate).chain(statuses.iter().map(|_| STATUS_ESTIMATE)).collect();
    root.conversation.sync(signatures, &estimates);

    if root.conversation.scrolled.replace(false) {
        let foot = remaining(&root.conversation.list) < FOOT_SLACK;
        root.conversation.at_foot.set(foot);
        *root.conversation.view.borrow_mut() = if foot { View::Foot } else { View::Rest };
    }
    // What arrives while this thread is open moves a view that follows it; a prompt the user sent
    // takes the view to the foot wherever it was.
    let answer_id = tail.as_ref().map(|tail| tail.message_id.clone()).or_else(|| thread.messages.last().filter(|message| message.kind == act_core::domain::conversation::MessageKind::Assistant).map(|message| message.id.clone()));
    let tool_id = thread.messages.last().filter(|message| message.kind == act_core::domain::conversation::MessageKind::Tool).map(|message| message.id.clone());
    let prompt_id = newest_prompt(&thread.messages);
    let first_sight = root.conversation.newest == (None, None, None);
    let (seen_answer, seen_tool, seen_prompt) = root.conversation.newest.clone();
    if !first_sight {
        let follows = {
            let view = root.conversation.view.borrow();
            matches!(*view, View::Foot | View::Row { .. }) || (matches!(*view, View::Kept { .. }) && root.conversation.at_foot.get())
        };
        if prompt_id.is_some() && prompt_id != seen_prompt {
            *root.conversation.view.borrow_mut() = View::Foot;
            root.conversation.at_foot.set(true);
            root.conversation.unplaced = true;
        } else if answer_id.is_some() && answer_id != seen_answer && follows {
            let id = answer_id.clone().unwrap();
            let row = rows.get(&id).and_then(|row| groups.get(*row)).map(|group| group.id().to_string()).unwrap_or(id);
            *root.conversation.view.borrow_mut() = View::Row { id: row, depth: ANSWER_DEPTH };
            root.conversation.unplaced = true;
        } else if tool_id.is_some() && tool_id != seen_tool && follows {
            *root.conversation.view.borrow_mut() = View::Foot;
        }
    }
    root.conversation.newest = (answer_id.or(Some(String::new())), tool_id, prompt_id);

    root.conversation.annotate.next_frame();
    if std::mem::take(&mut root.conversation.tucked) {
        root.conversation.unplaced = true;
    }
    if root.conversation.put_back.replace(false) {
        root.conversation.unplaced = true;
        root.conversation.moved = true;
    }
    // Reading a match takes the view over, the way scrolling by hand does.
    if let Some(hit) = find.as_ref().and_then(|find| find.hit.clone()) {
        if root.conversation.found.as_ref().is_none_or(|(found, _)| found != &hit) {
            if let Some(row) = rows.get(&hit.message_id) {
                *root.conversation.view.borrow_mut() = View::Rest;
                root.conversation.at_foot.set(false);
                root.conversation.list.scroll_to(ListOffset { item_ix: *row, offset_in_item: px(0.0) });
                root.conversation.found = Some((hit, 3));
            }
        } else if let Some((hit, tries)) = root.conversation.found.clone().filter(|(_, tries)| *tries > 0) {
            // Once the match's row is drawn, the match itself is brought to the middle of the view.
            root.conversation.found = Some((hit.clone(), tries - 1));
            let needle = find.as_ref().map(|find| find.needle.clone()).unwrap_or_default();
            let view_box = root.conversation.list.viewport_bounds();
            let found_at = root
                .conversation
                .annotate
                .shown_view(&hit.message_id)
                .filter(|_| hit.field == act_core::domain::find::FindField::Text)
                .and_then(|view| {
                    let read = view.read(cx);
                    let (start, end) = read.find(&needle).into_iter().nth(hit.occurrence)?;
                    read.range_bounds(start, end).map(|bounds| bounds.center().y)
                })
                .or_else(|| rows.get(&hit.message_id).and_then(|row| root.conversation.list.bounds_for_item(*row)).map(|bounds| bounds.origin.y + bounds.size.height.min(view_box.size.height) / 2.0));
            if let Some(y) = found_at {
                let delta = y - view_box.center().y;
                if f32::from(delta).abs() > 2.0 {
                    root.conversation.list.scroll_by(delta);
                }
            }
            cx.notify();
        }
    } else {
        root.conversation.found = None;
    }
    root.conversation.place(&rows);
    if !matches!(*root.conversation.view.borrow(), View::Foot) && root.conversation.unplaced {
        root.conversation.unplaced = false;
    }

    // Where the reader settles is kept for the trip back.
    if let Some(moved) = root.conversation.moved_at.get() {
        if moved.elapsed() >= READING_SETTLE {
            root.conversation.settle = None;
            root.conversation.report(cx);
        } else if root.conversation.settle.is_none() {
            let wait = READING_SETTLE.saturating_sub(moved.elapsed());
            root.conversation.settle = Some(cx.spawn(async move |root, cx| {
                cx.background_executor().timer(wait).await;
                let _ = root.update(cx, |root, cx| {
                    root.conversation.settle = None;
                    root.redraw_transcript(cx);
                });
            }));
        }
    }

    if running || wait.is_some() {
        if root.conversation.ticker.is_none() {
            root.conversation.ticker = Some(cx.spawn(async |root, cx| loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if root.update(cx, |root, cx| root.redraw_transcript(cx)).is_err() {
                    break;
                }
            }));
        }
    } else {
        root.conversation.ticker = None;
    }

    let names_key = (root.workspace.read(cx).outline_revision, state.threads.len());
    let names = match &root.conversation.names {
        Some((key, names)) if *key == names_key => names.clone(),
        _ => {
            let names: Rc<HashMap<String, String>> = Rc::new(state.threads.iter().map(|thread| (thread.id.clone(), thread.title.clone())).collect());
            root.conversation.names = Some((names_key, names.clone()));
            names
        }
    };
    let prompt_key = act_core::reducer::sending::prompt_key(state);
    let annotations = state.annotations.get(&prompt_key).cloned().unwrap_or_default();
    let drawn = root.conversation.annotate.drawn.clone();
    // Rows sit beside the transcript's scrollbar, which takes room once it overflows.
    let gutter = f32::from(act_ui::components::scrollbar::list_gutter(&root.conversation.list));
    if gutter != root.conversation.gutter {
        root.conversation.gutter = gutter;
        root.conversation.list.remeasure();
    }
    let order = match &root.conversation.order {
        Some((id, len, order)) if *id == thread.id && *len == thread.messages.len() => order.clone(),
        _ => {
            let order = Rc::new(super::timeline_row::reading_order(&thread.messages));
            root.conversation.order = Some((thread.id.clone(), thread.messages.len(), order.clone()));
            order
        }
    };
    root.conversation.selection.begin_frame();
    let model = Rc::new(RowModel {
        engine: thread.engine,
        task_id: thread.id.clone(),
        groups,
        tail,
        folds,
        root: cx.entity().downgrade(),
        inset,
        side: side + gutter,
        now,
        find,
        annotations: annotations.clone(),
        cards,
        names,
        drawn: drawn.clone(),
        docked: false,
        selection: Some((root.conversation.selection.clone(), order)),
        folder: act_core::workspace::location::thread_workspace_root(state, Some(thread)),
        width: f32::from(root.conversation.list.viewport_bounds().size.width),
    });
    let statuses = Rc::new(statuses);
    let count = model.groups.len();
    let composer_space = root.composer.height();
    // The composer is measured as it draws, so a change in what stands over it lays this out again.
    let approval_height = root.conversation.approval_probe.get().map(|bounds| bounds.size.height);
    // Room made at the foot for what stands over it moves the foot, which the view then follows.
    let extent = root.conversation.list.max_offset_for_scrollbar().height;
    // The approval card follows the last row, whose place is known only once the list has drawn it.
    let last_row_bottom = |root: &Root| {
        let list = &root.conversation.list;
        list.item_count().checked_sub(1).and_then(|last| list.bounds_for_item(last)).map(|row| row.bottom() - list.viewport_bounds().top())
    };
    let row_bottom = last_row_bottom(root);
    let follows_row = approval.is_some();
    cx.on_next_frame(window, move |root, _, cx| {
        if root.composer.height() != composer_space
            || root.conversation.approval_probe.get().map(|bounds| bounds.size.height) != approval_height
            || (follows_row && last_row_bottom(root) != row_bottom)
        {
            cx.notify();
        } else if root.conversation.list.max_offset_for_scrollbar().height != extent {
            root.redraw_transcript(cx);
        }
    });
    root.conversation.approval_shown = approval.is_some() && !coordinated_approval;
    let approval_space = if approval.is_some() && !coordinated_approval { root.conversation.approval_probe.get().map(|bounds| f32::from(bounds.size.height) + 12.0).unwrap_or(222.0) } else { 0.0 };
    let bar_room = super::coordination::bar_room(&coordination);
    let item_model = model.clone();
    let item_statuses = statuses.clone();
    let transcript = list(root.conversation.list.clone(), move |index, window, cx| {
        if index < count {
            timeline_row::render(&item_model, index, index + 1 == count, window, cx)
        } else if let Some(status) = item_statuses.get(index - count) {
            status.render(&item_model, cx)
        } else {
            div().into_any_element()
        }
    })
    .size_full()
    .pt(px(54.0 + bar_room))
    .pb(px(24.0 + composer_space + approval_space));

    let pressed = root.conversation.keys_scroll.clone();
    let left = root.conversation.keys_scroll.clone();
    let mut area = div()
        .id("conversation")
        .relative()
        .size_full()
        .capture_any_mouse_down(move |_, _, _| pressed.set(true))
        .on_mouse_down_out(move |_, _, _| left.set(false))
        .on_key_down(cx.listener(|root, event: &gpui::KeyDownEvent, window, cx| {
            let keystroke = &event.keystroke;
            let bare = !keystroke.modifiers.platform && !keystroke.modifiers.control && !keystroke.modifiers.alt && !keystroke.modifiers.shift;
            let mod_only = (if cfg!(target_os = "macos") { keystroke.modifiers.platform && !keystroke.modifiers.control } else { keystroke.modifiers.control && !keystroke.modifiers.platform }) && !keystroke.modifiers.alt && !keystroke.modifiers.shift;
            if keystroke.key == "enter" && ((bare && super::annotate::enter(root, window, cx)) || (mod_only && super::annotate::enter_side(root, cx))) {
                cx.stop_propagation();
            }
        }))
        .child(transcript)
        .child(drag_scroller(&root.conversation))
        .child(act_ui::components::scrollbar::Scrollbar::list("conversation-scrollbar", &root.conversation.list));
    // On the glass sheet nothing under the composer hides the transcript's foot, so it fades out
    // there. Chromium's backdrop filters cannot see into the masked transcript: glass over it frosts
    // only the frame.
    let frame = cx.theme().tokens.frame;
    if cx.theme().tokens.page.a == 0.0 {
        area = area.edge_fade(gpui::EdgeFade::vertical().end(px(6.0), px(26.0)));
    }
    if cx.theme().tokens.page.a == 0.0 && cx.theme().tokens.glass_blur {
        area = area.child(gpui::canvas(|_, _, _| {}, move |bounds, _, window, _| window.hide_from_backdrops(bounds, frame)).absolute().top_0().left_0().size_full());
    }
    area = area.children(super::annotate::layer(root, &thread.id, &annotations, &model, window, cx));
    let brief_root = cx.entity().downgrade();
    if let Some(bar) = super::coordination::coordination_bar(&coordination, root.conversation.brief_open, move |cx| {
        if let Some(root) = brief_root.upgrade() {
            root.update(cx, |root, cx| {
                root.conversation.brief_open = !root.conversation.brief_open;
                cx.notify();
            });
        }
    }, cx) {
        area = area.child(div().absolute().top_0().left_0().right_0().pt(px(10.0)).pb(px(12.0)).pl(px(inset)).pr(px(inset + side)).child(bar));
    }
    if let Some(approval) = approval.filter(|_| !coordinated_approval) {
        let probe = root.conversation.approval_probe.clone();
        // In the transcript's flow under its last row, whose bottom margin takes in the card's 10px
        // one; held over the composer while that row is further down (`position: sticky`).
        let pinned = composer_space + 36.0;
        let list = &root.conversation.list;
        let viewport = list.viewport_bounds();
        let flow_top = list.item_count().checked_sub(1).and_then(|last| list.bounds_for_item(last)).map(|row| f32::from(row.bottom() - viewport.top()));
        let pinned_top = probe.get().map(|card| f32::from(viewport.size.height) - pinned - f32::from(card.size.height));
        let holder = match (flow_top, pinned_top) {
            (Some(flow), Some(held)) if flow < held => div().absolute().top(px(flow)),
            _ => div().absolute().bottom(px(pinned)),
        };
        area = area.child(holder.left_0().right_0().pl(px(inset)).pr(px(inset + side)).flex().justify_center().child(approval_card(&approval, &probe, state, cx)));
    }
    // A view the app placed on a row (a fresh answer, a kept place) is off the foot once there is more below it.
    let placed_on_row = matches!(*root.conversation.view.borrow(), View::Row { .. } | View::Kept { .. });
    let at_foot = (root.conversation.at_foot.get() && !placed_on_row) || remaining(&root.conversation.list) < FOOT_SLACK;
    root.conversation.scroll_button_shown = !at_foot;
    if !at_foot {
        let probe = root.conversation.scroll_button.clone();
        let weak = cx.entity().downgrade();
        area = area.child(
            div().absolute().left_0().right(px(side)).bottom(px(composer_space + 34.0 + approval_space + if approval_space > 0.0 { 10.0 } else { 0.0 })).flex().justify_center().child(div().relative().child(bounds_probe(&probe)).child(
                IconButton::new("scroll-to-end", IconName::ChevronDown, "Scroll to the latest message")
                    .variant(IconButtonVariant::Glass)
                    // The transcript stays sharp under this button.
                    .clear_backdrop()
                    .size(IconButtonSize::Lg)
                    .round(true)
                    .icon_size(18.0)
                    .tip("Latest message")
                    .on_click(move |_, _, cx| {
                        if let Some(root) = weak.upgrade() {
                            root.update(cx, |root, cx| {
                                root.conversation.scroll_to_foot();
                                cx.notify();
                            });
                        }
                    }),
            )),
        );
    }
    area.into_any_element()
}

/// While a drag selects past the transcript's top or bottom, scrolls toward the pointer each frame
/// by as far as it is past the edge, and selects on to what scrolls under it, as Chromium does.
fn drag_scroller(conversation: &ConversationState) -> impl IntoElement {
    let (group, list, scrolled, moved_at) = (conversation.selection.clone(), conversation.list.clone(), conversation.scrolled.clone(), conversation.moved_at.clone());
    gpui::canvas(
        |_, _, _| (),
        move |bounds, _, window, cx| {
            let ending = group.clone();
            // The annotate popover waits for the release, so the transcript draws again then.
            window.on_mouse_event(move |_: &gpui::MouseUpEvent, _, window, _| {
                if ending.dragging().is_some() {
                    ending.end_drag();
                    window.refresh();
                }
            });
            let Some(point) = group.dragging() else { return };
            let past = if point.y < bounds.top() {
                point.y - bounds.top()
            } else if point.y > bounds.bottom() {
                point.y - bounds.bottom()
            } else {
                return;
            };
            list.scroll_by(past);
            scrolled.set(true);
            moved_at.set(Some(Instant::now()));
            group.follow_drag(cx);
            window.request_animation_frame();
        },
    )
    .absolute()
    .inset_0()
}

/// A `<summary>`'s marker as Chromium draws it: a filled triangle, pointing down once open, in a
/// box as wide as the marker and the space after it.
fn disclosure_marker(open: bool, color: gpui::Rgba) -> impl IntoElement {
    gpui::canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let (side, height) = (5.6, 6.5);
            let left = bounds.origin.x;
            let middle = bounds.origin.y + bounds.size.height / 2.0;
            let at = |x: f32, y: f32| gpui::point(left + px(x), middle + px(y));
            let mut triangle = gpui::PathBuilder::fill();
            if open {
                triangle.move_to(at(0.0, -side / 2.0));
                triangle.line_to(at(height, -side / 2.0));
                triangle.line_to(at(height / 2.0, side / 2.0));
            } else {
                triangle.move_to(at(0.0, -height / 2.0));
                triangle.line_to(at(side, 0.0));
                triangle.line_to(at(0.0, height / 2.0));
            }
            triangle.close();
            if let Ok(path) = triangle.build() {
                window.paint_path(path, color);
            }
        },
    )
    .flex_none()
    .w(px(12.2))
    .h(px(8.0))
}

/// The action an approval asks for, drawn as what it is (approval-preview.ts, ApprovalAction).
pub fn approval_action(id: &str, input: &serde_json::Map<String, Value>, tool_name: &str, cx: &App) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    let mono = cx.theme().typography.mono_family.clone();
    let text = |keys: &[&str]| keys.iter().find_map(|key| input.get(*key).and_then(Value::as_str).filter(|value| !value.is_empty()).map(String::from));
    let lines = |value: &str| {
        let mut split: Vec<String> = value.split('\n').map(String::from).collect();
        if split.len() > 1 && split.last().is_some_and(String::is_empty) {
            split.pop();
        }
        split
    };
    let frame = || div().mt(px(11.0)).border_1().border_color(t.line_soft).rounded(px(10.0)).overflow_hidden().bg(t.frame_field).text_color(t.code_ink).font_family(mono.clone()).text_size(px(m.text_meta)).line_height(relative(1.55));
    let heading = |path: String| div().border_b_1().border_color(t.line_soft).px(px(10.0)).py(px(7.0)).text_color(t.muted_strong).line_height(relative(1.4)).child(path);
    let path = text(&["file_path", "filePath", "notebook_path", "path"]);
    let removed = text(&["old_string", "oldString"]);
    let added = input.get("new_string").filter(|value| !value.is_null()).or_else(|| input.get("newString")).and_then(Value::as_str);
    if let (Some(path), Some(removed), Some(added)) = (&path, removed.as_deref(), added) {
        let mut body = div().id(SharedString::from(format!("{id}-edit"))).flex().flex_col().py(px(8.0)).max_h(px(200.0)).overflow_y_scroll();
        let mark = |sign: &'static str, color, surface, line: String| div().relative().min_h(px(m.text_meta * 1.55)).pl(px(26.0)).pr(px(10.0)).bg(surface).child(div().absolute().left(px(10.0)).text_color(color).child(sign)).child(line);
        for line in lines(removed) {
            body = body.child(mark("−", t.danger, t.diff_del_surface, line));
        }
        if !added.is_empty() {
            for line in lines(added) {
                body = body.child(mark("+", t.success, t.diff_add_surface, line));
            }
        }
        return frame().child(heading(path.clone())).child(body).into_any_element();
    }
    if let (Some(path), Some(content)) = (&path, text(&["content"])) {
        return frame().child(heading(path.clone())).child(div().id(SharedString::from(format!("{id}-file"))).px(px(10.0)).py(px(8.0)).max_h(px(200.0)).overflow_y_scroll().child(pre_text(content))).into_any_element();
    }
    if let Some(command) = text(&["command"]).or(path).or_else(|| text(&["url"])) {
        return frame().id(SharedString::from(format!("{id}-command"))).px(px(12.0)).py(px(9.0)).max_h(px(168.0)).overflow_y_scroll().child(pre_text(command)).into_any_element();
    }
    let open_id: SharedString = format!("{id}-raw").into();
    let raw = serde_json::to_string_pretty(input).unwrap_or_default();
    let open = RAW_OPEN.with(|open| open.borrow().contains(open_id.as_ref()));
    let toggle_id = open_id.clone();
    let mut frame = frame().child(
        div()
            .id(open_id)
            .flex()
            .items_center()
            .px(px(10.0))
            .py(px(8.0))
            .text_color(t.muted_strong)
            .line_height(relative(1.3))
            .cursor_pointer()
            .child(disclosure_marker(open, t.muted_strong))
            .child(act_core::domain::tool_call::tool_label(tool_name))
            .on_click(move |_, _, cx| {
                RAW_OPEN.with(|open| {
                    let mut open = open.borrow_mut();
                    if !open.remove(toggle_id.as_ref()) {
                        open.insert(toggle_id.to_string());
                    }
                });
                cx.refresh_windows();
            }),
    );
    if open {
        frame = frame.child(div().id(SharedString::from(format!("{id}-raw-body"))).border_t_1().border_color(t.line_soft).px(px(10.0)).py(px(8.0)).max_h(px(200.0)).overflow_y_scroll().child(raw));
    }
    frame.into_any_element()
}

/// Text as a `pre` shows it: a final line break ends the last line rather than opening another.
fn pre_text(mut text: String) -> String {
    if text.ends_with('\n') {
        text.pop();
    }
    text
}

thread_local! {
    static RAW_OPEN: RefCell<std::collections::HashSet<String>> = RefCell::default();
}

/// A keystroke from the settings, as its tip names it.
fn binding_tip(state: &WorkspaceState, label: &str, action: &str) -> SharedString {
    let binding = act_core::domain::shortcuts::shortcut_settings(&state.shortcuts).into_iter().find(|setting| setting.action.id == action).and_then(|setting| setting.binding);
    match binding {
        Some(binding) => act_ui::tooltip::shortcut_tip(label, &act_core::domain::shortcuts::display_shortcut(&binding, cfg!(target_os = "macos"))),
        None => label.to_string().into(),
    }
}

pub(super) fn approval_card(approval: &ApprovalView, probe: &BoundsCell, state: &WorkspaceState, cx: &mut Context<Root>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let m = &theme.metrics;
    let decide = |allow: bool| {
        let task_id = approval.task_id.clone();
        let run_id = approval.run_id.clone();
        let approval_id = approval.approval_id.clone();
        move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| dispatch(AppCommand::RunDecide { allow, task_id: task_id.clone(), run_id: run_id.clone(), approval_id: approval_id.clone() }, cx)
    };
    let mut text = div().flex().flex_col().child(div().pt(px(1.0)).text_color(t.ink).text_size(px(m.text_control)).font_weight(FontWeight(600.0)).child(approval.title.clone()));
    if !approval.description.is_empty() {
        text = text.child(div().mt(px(3.0)).text_color(t.muted).text_size(px(m.text_meta)).line_height(relative(1.45)).child(approval.description.clone()));
    }
    let body = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .child(text)
        .child(approval_action(&format!("approval-{}", approval.approval_id), &approval.input, &approval.tool_name, cx))
        .child(
            div()
                .flex()
                .justify_end()
                .gap(px(8.0))
                .mt(px(12.0))
                .child(Button::new("approval-deny", "Deny").variant(ButtonVariant::Secondary).tip(binding_tip(state, "Deny", "run.deny")).on_click(decide(false)))
                .child(Button::new("approval-allow", "Allow once").variant(ButtonVariant::Primary).tip(binding_tip(state, "Allow once", "run.allow")).on_click(decide(true))),
        );
    let tint = with_alpha(t.accent, 0.2);
    let lit = gpui::linear_gradient(180.0, gpui::linear_color_stop(act_ui::color::over(with_alpha(gpui::white().into(), 0.18), tint), 0.0), gpui::linear_color_stop(act_ui::color::over(with_alpha(gpui::white().into(), 0.02), tint), 1.0));
    let glow = |color: gpui::Rgba, blur: f32, spread: f32| gpui::BoxShadow { color: color.into(), offset: gpui::point(px(0.0), px(0.0)), blur_radius: px(blur), spread_radius: px(spread) };
    glass(div(), t, GlassKind::Float, Some(14.0), None)
        .relative()
        .w_full()
        .max_w(px(860.0))
        .flex()
        .gap(px(12.0))
        .pt(px(14.0))
        .pr(px(14.0))
        .pb(px(14.0))
        .pl(px(13.0))
        .shadow(std::iter::once(glow(with_alpha(t.accent, 0.32), 0.0, 1.0)).chain(t.shadow_float.box_shadows()).collect())
        // The accent pools faintly at the top left.
        .child(div().absolute().inset_0().rounded(px(14.0)).bg(gpui::radial_gradient(gpui::point(0.0, 0.0), gpui::size(1.2, 1.4), gpui::linear_color_stop(with_alpha(t.accent, 0.12), 0.0), gpui::linear_color_stop(with_alpha(t.accent, 0.0), 0.55))))
        .child(
            div()
                .relative()
                .flex_none()
                .size(px(30.0))
                .rounded(px(9.0))
                .flex()
                .items_center()
                .justify_center()
                .bg(lit)
                .shadow(vec![glow(with_alpha(t.accent, 0.25), 12.0, 0.0)])
                .rim(t.glass_rim, act_ui::style::Corners::Radius(9.0))
                .child(icon(IconName::ShieldAlert, 16.0, t.accent_soft)),
        )
        .child(body)
        .child(super::common::seen_by_backdrops())
        .child(bounds_probe(probe))
}
