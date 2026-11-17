//! A rendered message: parsing as the text streams in, selection, copying, and link events.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ops::Range;
use std::rc::Rc;
use std::time::Instant;

use gpui::{
    actions, div, prelude::*, App, Bounds, ClipboardItem, Context, DispatchPhase, Element,
    ElementId, EventEmitter, FocusHandle, Focusable, GlobalElementId, Hitbox, HitboxBehavior,
    InspectorElementId, KeyBinding, LayoutId, Modifiers, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, SharedString, Subscription, WeakEntity, Window,
};

use crate::chunk::{partition, Chunk, Group, Settled, GROUPED_FROM};
use crate::group::{MemberView, SelectionGroup, Span};
use crate::links::{LinkCapabilities, LinkTarget};
use crate::parse::{parse, plain, Document, ParseOptions};
use crate::render::{
    stack, Built, CopyButtonSlot, CopyFeedback, CopyHandler, FenceSlot, Highlight, Inputs,
    TextEntry, TextPos, COPY_CONFIRM,
};
use crate::selection::{
    block_range, clamp, link_at_point, position_for_point, selected_text, word_range, Granularity,
};
use crate::stream::{repair_cut, StreamCuts};
use crate::style::MarkdownStyle;

actions!(markdown, [Copy, SelectAll]);

type KnownCommit = Rc<dyn Fn(&str) -> bool>;
type CopyHook = fn(gpui::Div, WeakEntity<MarkdownView>) -> gpui::Div;

fn copy_on<A: gpui::Action>(root: gpui::Div, view: WeakEntity<MarkdownView>) -> gpui::Div {
    root.on_action(move |_: &A, _, cx| {
        let copied = view.update(cx, |view, cx| view.copy_selection(cx)).unwrap_or(false);
        if !copied {
            cx.propagate();
        }
    })
}

/// The key context a focused message dispatches in.
pub const KEY_CONTEXT: &str = "Markdown";

/// Binds `cmd-c` and `cmd-a` (`ctrl-` off macOS) for focused messages.
pub fn bind_keys(cx: &mut App) {
    let (copy, select_all) = if cfg!(target_os = "macos") {
        ("cmd-c", "cmd-a")
    } else {
        ("ctrl-c", "ctrl-a")
    };
    cx.bind_keys([
        KeyBinding::new(copy, Copy, Some(KEY_CONTEXT)),
        KeyBinding::new(select_all, SelectAll, Some(KEY_CONTEXT)),
    ]);
}

/// How a link was acted on.
#[derive(Clone, Debug, PartialEq)]
pub enum LinkAction {
    /// A primary click. A web link opens in the app's browser panel when the platform's command
    /// modifier is held and the host can open it there, and in the default browser otherwise.
    Click { modifiers: Modifiers },
    /// A secondary click: the host shows the link's menu at `position` (window coordinates).
    ContextMenu { position: Point<Pixels> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct LinkEvent {
    pub target: LinkTarget,
    pub action: LinkAction,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MarkdownEvent {
    Link(LinkEvent),
    /// The hovered link changed; carries its hint, such as a file link's `path:line`.
    LinkHover(Option<LinkTarget>),
    SelectionChanged,
    /// The built-in copy button wrote this text.
    Copied(String),
}

#[derive(Clone, Copy, Debug)]
struct Selection {
    anchor: TextPos,
    head: TextPos,
}

impl Selection {
    fn ordered(&self) -> (TextPos, TextPos) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }
}

struct Drag {
    origin: Point<Pixels>,
    moved: bool,
    link: Option<(usize, usize)>,
    granularity: Granularity,
    /// The unit the drag started on, which stays selected whichever way it goes.
    unit: (TextPos, TextPos),
    /// A Shift press that selects on from the group's anchor in another member.
    from_anchor: bool,
}

/// One parsed piece of the message and where it starts in the source.
struct Piece {
    start: usize,
    end: usize,
    doc: Rc<Document>,
    live: bool,
    /// Its blocks in groups, once the message is long enough to draw them that way.
    groups: Vec<Rc<Group>>,
}

/// A message rendered from Markdown. Settled text is parsed once; while a stream is still writing,
/// only the chunk that grew and the repaired live tail are parsed again.
pub struct MarkdownView {
    source: String,
    streaming: bool,
    options: ParseOptions,
    style: Rc<MarkdownStyle>,
    pieces: Vec<Piece>,
    cuts: StreamCuts,
    /// When each word of the live tail first showed, by its offset in the source.
    reveal: BTreeMap<usize, Instant>,
    focus: FocusHandle,
    selection: Option<Selection>,
    drag: Option<Drag>,
    hovered: Option<(usize, usize)>,
    entries: Rc<RefCell<Vec<TextEntry>>>,
    known_commit: Option<KnownCommit>,
    copy_slot: Option<CopyButtonSlot>,
    tip_slot: Option<crate::render::LinkTipSlot>,
    fence_slot: Option<FenceSlot>,
    scrollbar_slot: Option<crate::render::ScrollbarSlot>,
    feedback: Option<CopyFeedback>,
    emphasize_copy: bool,
    animate: bool,
    fade_scheduled: bool,
    highlights: Rc<Vec<Highlight>>,
    /// The messages this one selects across, and its place among them.
    group: Option<(SelectionGroup, u64)>,
    /// Text shown as written rather than as Markdown.
    plain: bool,
    copy_hook: Option<CopyHook>,
    /// Keyboard focus of each link, by text block and link index.
    link_focus: Rc<RefCell<std::collections::HashMap<(usize, usize), FocusHandle>>>,
    /// The width the message was last drawn at.
    width: Rc<Cell<Option<Pixels>>>,
    /// The text entries each group holds, for placing ranges in groups not drawn.
    placed_groups: Vec<(Range<usize>, Rc<Group>)>,
    _blur: Subscription,
}

impl EventEmitter<MarkdownEvent> for MarkdownView {}

impl Focusable for MarkdownView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl MarkdownView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        let blur = cx.on_blur(&focus, window, |this, window, cx| {
            // The window going to the background keeps the selection, drawn inactive.
            if !window.is_window_active() {
                return;
            }
            let group = this.group.clone();
            if group.as_ref().is_some_and(|(group, order)| group.focus_moved_within(*order, window, cx)) {
                return;
            }
            if this.selection.take().is_some() {
                cx.emit(MarkdownEvent::SelectionChanged);
                cx.notify();
            }
            // A selection run on into other messages goes too.
            if let Some((group, order)) = group {
                group.release_others(order, cx);
            }
        });
        Self {
            source: String::new(),
            streaming: false,
            options: ParseOptions::default(),
            style: Rc::new(MarkdownStyle::default()),
            pieces: Vec::new(),
            cuts: StreamCuts::default(),
            reveal: BTreeMap::new(),
            focus,
            selection: None,
            drag: None,
            hovered: None,
            entries: Rc::default(),
            known_commit: None,
            copy_slot: None,
            tip_slot: None,
            fence_slot: None,
            scrollbar_slot: None,
            feedback: None,
            emphasize_copy: false,
            animate: true,
            fade_scheduled: false,
            highlights: Rc::default(),
            group: None,
            plain: false,
            copy_hook: None,
            link_focus: Rc::default(),
            width: Rc::default(),
            placed_groups: Vec::new(),
            _blur: blur,
        }
    }

    /// The Markdown the message was rendered from.
    pub fn text(&self) -> &str {
        &self.source
    }

    pub fn is_streaming(&self) -> bool {
        self.streaming
    }

    /// Sets the message text. `streaming` says more text may still arrive, which renders the
    /// half-written tail repaired and fading in, and leaves settled chunks alone as the text grows.
    pub fn set_text(&mut self, text: impl Into<String>, streaming: bool, cx: &mut Context<Self>) {
        let text = text.into();
        if text == self.source && streaming == self.streaming {
            return;
        }
        let extends = text.starts_with(&self.source);
        self.source = text;
        self.streaming = streaming;
        if !extends {
            self.pieces.clear();
            self.reveal.clear();
            self.cuts = StreamCuts::default();
        }
        self.reparse();
        cx.notify();
    }

    /// Appends streamed text.
    pub fn push_str(&mut self, delta: &str, cx: &mut Context<Self>) {
        if delta.is_empty() {
            return;
        }
        let text = format!("{}{delta}", self.source);
        self.set_text(text, true, cx);
    }

    /// Marks the stream finished, rendering the whole text as one document.
    pub fn finish(&mut self, cx: &mut Context<Self>) {
        if self.streaming {
            self.streaming = false;
            self.reparse();
            cx.notify();
        }
    }

    pub fn set_style(&mut self, style: MarkdownStyle, cx: &mut Context<Self>) {
        if *self.style != style {
            self.style = Rc::new(style);
            // Groups measured in the old fonts are measured again.
            for piece in &mut self.pieces {
                piece.groups.clear();
            }
            cx.notify();
        }
    }

    pub fn style(&self) -> &MarkdownStyle {
        &self.style
    }

    /// What the host can open; a link it cannot open renders as text.
    pub fn set_link_capabilities(
        &mut self,
        capabilities: LinkCapabilities,
        cx: &mut Context<Self>,
    ) {
        self.set_options(
            ParseOptions {
                capabilities,
                ..self.options
            },
            cx,
        );
    }

    /// Whether the message has an id, which image links need to open in the viewer.
    pub fn set_has_message_id(&mut self, has_message_id: bool, cx: &mut Context<Self>) {
        self.set_options(
            ParseOptions {
                has_message_id,
                ..self.options
            },
            cx,
        );
    }

    fn set_options(&mut self, options: ParseOptions, cx: &mut Context<Self>) {
        if options != self.options {
            self.options = options;
            self.pieces.clear();
            self.reparse();
            cx.notify();
        }
    }

    /// Which inline code hashes open a commit: those the thread's checkout holds. Call `cx.notify`
    /// on the view when the answer changes.
    pub fn set_known_commit(&mut self, known: impl Fn(&str) -> bool + 'static) {
        self.known_commit = Some(Rc::new(known));
    }

    /// Draws copy buttons with the host's own button instead of the built-in one.
    pub fn set_copy_button(
        &mut self,
        slot: impl Fn(crate::render::CopyButtonRequest, &mut Window, &mut App) -> gpui::AnyElement
            + 'static,
    ) {
        self.copy_slot = Some(Rc::new(slot));
    }

    /// Shows a link's hint (a file's path, "Open commit") with the host's tooltip: `slot` draws a
    /// zone filling its parent that shows the hint while the pointer rests on it, placed against
    /// the whole link's box.
    pub fn set_link_tip(&mut self, slot: impl Fn(ElementId, SharedString, gpui::Bounds<gpui::Pixels>, &mut Window, &mut App) -> gpui::AnyElement + 'static) {
        self.tip_slot = Some(Rc::new(slot));
    }

    /// Lets the host draw fenced blocks of its own languages, such as `mermaid` and `visual`.
    pub fn set_fence_renderer(
        &mut self,
        slot: impl Fn(
                crate::render::FenceRequest,
                &mut Window,
                &mut App,
            ) -> Option<(gpui::AnyElement, SharedString)>
            + 'static,
    ) {
        self.fence_slot = Some(Rc::new(slot));
    }

    /// Lets the host draw the bar over code blocks and tables that scroll sideways.
    pub fn set_scrollbar_renderer(&mut self, slot: impl Fn(ElementId, &gpui::ScrollHandle, &mut Window, &mut App) -> gpui::AnyElement + 'static) {
        self.scrollbar_slot = Some(Rc::new(slot));
    }

    /// The newest row shows its copy buttons as bare glyphs without a hover.
    pub fn set_emphasize_copy(&mut self, emphasize: bool, cx: &mut Context<Self>) {
        if self.emphasize_copy != emphasize {
            self.emphasize_copy = emphasize;
            cx.notify();
        }
    }

    /// Whether streamed words fade in. Off for reduced motion.
    pub fn set_animate(&mut self, animate: bool) {
        self.animate = animate;
    }

    /// Every inline code hash in the message, which the host checks against the checkout.
    pub fn commit_hashes(&self) -> Vec<String> {
        let mut hashes = Vec::new();
        for piece in &self.pieces {
            for hash in piece.doc.commit_hashes() {
                if !hashes.contains(&hash) {
                    hashes.push(hash);
                }
            }
        }
        hashes
    }

    /// Joins the messages a drag can select across; `order` is this message's place in reading order.
    pub fn set_selection_group(&mut self, group: SelectionGroup, order: u64) {
        self.group = Some((group, order));
    }

    /// Shows the text as written, every line break kept, instead of as Markdown.
    pub fn set_plain(&mut self, plain: bool, cx: &mut Context<Self>) {
        if self.plain != plain {
            self.plain = plain;
            self.pieces.clear();
            self.reparse();
            cx.notify();
        }
    }

    fn put_selection(&mut self, selection: Option<Selection>, cx: &mut Context<Self>) {
        let same = match (self.selection, selection) {
            (None, None) => true,
            (Some(a), Some(b)) => a.anchor == b.anchor && a.head == b.head,
            _ => false,
        };
        if !same {
            self.selection = selection;
            cx.emit(MarkdownEvent::SelectionChanged);
            cx.notify();
        }
    }

    fn release_others(&self, cx: &mut Context<Self>) {
        if let Some((group, order)) = self.group.clone() {
            group.release_others(order, cx);
        }
    }

    pub(crate) fn is_focused(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    /// Selects what a drag running through this message from another asks for.
    pub(crate) fn put_span(&mut self, span: Span, cx: &mut Context<Self>) {
        let selection = match span {
            Span::Clear => None,
            Span::Whole => Some(Selection { anchor: TextPos::default(), head: self.end_pos() }),
            Span::ToPoint { from_start, point } => {
                let pos = position_for_point(&self.entries.borrow(), point).unwrap_or_default();
                let from = if from_start { TextPos::default() } else { self.end_pos() };
                Some(Selection { anchor: from, head: pos })
            }
            Span::FromAnchor { to_end } => Some(Selection {
                anchor: self.selection.map(|selection| selection.anchor).unwrap_or_default(),
                head: if to_end { self.end_pos() } else { TextPos::default() },
            }),
            Span::AnchorTo { point } => Some(Selection {
                anchor: self.selection.map(|selection| selection.anchor).unwrap_or_default(),
                head: position_for_point(&self.entries.borrow(), point).unwrap_or_default(),
            }),
        };
        self.put_selection(selection, cx);
    }

    fn end_pos(&self) -> TextPos {
        let entries = self.entries.borrow();
        match entries.len().checked_sub(1) {
            Some(last) => TextPos { block: last, offset: entries[last].text.text.len() },
            None => TextPos::default(),
        }
    }

    /// Selects from where this message's drag began across to the message under `point`: whole
    /// messages between, part of the last. With no other message there, the others let go.
    fn extend_across(&mut self, point: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        let Some((group, order)) = self.group.clone() else { return false };
        let Some(head) = group.extend(order, point, cx) else { return false };
        let anchor = self.selection.map(|selection| selection.anchor).unwrap_or_default();
        let end = if head > order { self.end_pos() } else { TextPos::default() };
        self.put_selection(Some(Selection { anchor, head: end }), cx);
        true
    }

    /// The selection across every message of the group, in reading order.
    fn group_text(&self, cx: &App) -> Option<String> {
        let (group, order) = self.group.as_ref()?;
        group.text(*order, self.selected_text(), cx)
    }

    /// Whether it holds a selection's anchor, a caret included.
    pub(crate) fn has_anchor(&self) -> bool {
        self.selection.is_some()
    }

    pub fn has_selection(&self) -> bool {
        self.selection
            .is_some_and(|selection| selection.anchor != selection.head)
    }

    /// The selected text as it reads on screen.
    pub fn selected_text(&self) -> Option<String> {
        let selection = self.selection?;
        let entries = self.entries.borrow();
        let (start, end) = selection.ordered();
        let text = selected_text(&entries, clamp(&entries, start), clamp(&entries, end));
        (!text.is_empty()).then_some(text)
    }

    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        let entries = self.entries.borrow();
        let Some(last) = entries.len().checked_sub(1) else {
            return;
        };
        let end = TextPos {
            block: last,
            offset: entries[last].text.text.len(),
        };
        drop(entries);
        self.selection = Some(Selection {
            anchor: TextPos::default(),
            head: end,
        });
        cx.emit(MarkdownEvent::SelectionChanged);
        cx.notify();
    }

    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        if self.selection.take().is_some() {
            cx.emit(MarkdownEvent::SelectionChanged);
            cx.notify();
        }
    }

    /// Paints ranges over the text, such as find matches and annotations, under the selection.
    pub fn set_highlights(&mut self, highlights: Vec<Highlight>, cx: &mut Context<Self>) {
        if *self.highlights != highlights {
            self.highlights = Rc::new(highlights);
            cx.notify();
        }
    }

    /// The selection as it reads on screen, in order. `None` when nothing is selected.
    pub fn selection_range(&self) -> Option<(TextPos, TextPos)> {
        let selection = self
            .selection
            .filter(|selection| selection.anchor != selection.head)?;
        let entries = self.entries.borrow();
        let (start, end) = selection.ordered();
        Some((clamp(&entries, start), clamp(&entries, end)))
    }

    /// Where a range was last drawn, in window coordinates: the box around it, for placing a
    /// popover over a selection or a marker at a highlight's end.
    pub fn range_bounds(&self, start: TextPos, end: TextPos) -> Option<Bounds<Pixels>> {
        crate::selection::range_bounds(&self.entries.borrow(), start, end).or_else(|| {
            // A range in a part of a long message not drawn now is placed at that part.
            let (_, group) = self.placed_groups.iter().find(|(entries, _)| entries.contains(&start.block))?;
            group.placed.get()
        })
    }

    /// The box a DOM range reports for a selection: its text's own boxes, not the lines they sit on.
    pub fn range_text_bounds(&self, start: TextPos, end: TextPos) -> Option<Bounds<Pixels>> {
        crate::selection::range_text_bounds(&self.entries.borrow(), start, end).or_else(|| {
            let (_, group) = self.placed_groups.iter().find(|(entries, _)| entries.contains(&start.block))?;
            group.placed.get()
        })
    }

    /// Every case-insensitive occurrence of `query` within one text block, in order.
    pub fn find(&self, query: &str) -> Vec<(TextPos, TextPos)> {
        crate::selection::find(&self.entries.borrow(), query)
    }

    /// The offset of a position in the message's rendered text: every block's text in order with
    /// nothing between, counted in UTF-16 units as the DOM counts them.
    pub fn rendered_offset(&self, pos: TextPos) -> usize {
        crate::selection::rendered_offset(&self.entries.borrow(), pos)
    }

    /// The position a rendered-text offset names, the inverse of [`Self::rendered_offset`].
    pub fn position_at(&self, offset: usize) -> Option<TextPos> {
        crate::selection::position_at(&self.entries.borrow(), offset)
    }

    /// Copies with the host's own copy action too, such as the one its Edit menu sends.
    pub fn copy_on_action<A: gpui::Action>(&mut self) {
        self.copy_hook = Some(copy_on::<A>);
    }

    /// Writes the selection, across the group's messages, to the clipboard; false when nothing is selected.
    pub fn copy_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(text) = self.group_text(cx).or_else(|| self.selected_text()) else { return false };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        true
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.copy_selection(cx) {
            cx.propagate();
        }
    }

    fn select_all_action(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.select_all(cx);
    }

    fn reparse(&mut self) {
        let options = self.options;
        if self.plain {
            self.cuts.update("", false);
            self.reveal.clear();
            self.pieces = vec![Piece {
                start: 0,
                end: self.source.len(),
                doc: Rc::new(plain(&self.source)),
                live: false,
                groups: Vec::new(),
            }];
            return;
        }
        if !self.streaming {
            self.cuts.update("", false);
            self.reveal.clear();
            let whole = self.pieces.first().filter(|piece| {
                !piece.live
                    && piece.start == 0
                    && piece.end == self.source.len()
                    && self.pieces.len() == 1
            });
            if whole.is_none() {
                let doc = Rc::new(parse(&self.source, options));
                self.pieces = vec![Piece {
                    start: 0,
                    end: self.source.len(),
                    doc,
                    live: false,
                    groups: Vec::new(),
                }];
            }
            return;
        }
        let (end, cuts) = self.cuts.update(&self.source, true);
        let cuts = cuts.to_vec();
        let mut ranges: Vec<Range<usize>> = Vec::new();
        let mut start = 0;
        for cut in cuts.iter().copied().chain(std::iter::once(end)) {
            if cut > start {
                ranges.push(start..cut);
            }
            start = cut;
        }
        let mut previous = std::mem::take(&mut self.pieces);
        for range in ranges {
            let reused = previous.iter().position(|piece| {
                !piece.live && piece.start == range.start && piece.end == range.end
            });
            let (doc, groups) = match reused {
                Some(index) => {
                    let piece = previous.swap_remove(index);
                    (piece.doc, piece.groups)
                }
                None => (Rc::new(parse(&self.source[range.clone()], options)), Vec::new()),
            };
            self.pieces.push(Piece {
                start: range.start,
                end: range.end,
                doc,
                live: false,
                groups,
            });
        }
        let live = repair_cut(&self.source[end..]);
        if !live.is_empty() {
            let doc = Rc::new(parse(&live, options));
            let now = Instant::now();
            self.reveal = self.reveal.split_off(&end);
            doc.visit_inlines(&mut |text| {
                for start in crate::render::word_starts(&text.text) {
                    self.reveal
                        .entry(end + text.source_offset(start))
                        .or_insert(now);
                }
            });
            self.pieces.push(Piece {
                start: end,
                end: end + live.len(),
                doc,
                live: true,
                groups: Vec::new(),
            });
        } else {
            self.reveal = self.reveal.split_off(&end);
        }
    }

    fn on_copy_clicked(
        &mut self,
        block: usize,
        text: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
        self.feedback = Some(CopyFeedback {
            block,
            copied: true,
            at: Instant::now(),
        });
        cx.emit(MarkdownEvent::Copied(text.to_string()));
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(COPY_CONFIRM).await;
            let _ = this.update(cx, |this, cx| {
                if this
                    .feedback
                    .is_some_and(|feedback| feedback.at.elapsed() >= COPY_CONFIRM)
                {
                    this.feedback = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let entries = self.entries.clone();
        let entries = entries.borrow();
        let link = link_at_point(&entries, event.position);
        if event.button == MouseButton::Right {
            let target = link
                .and_then(|(block, index)| entries.get(block)?.text.links.get(index))
                .map(|link| link.target.clone());
            if let Some(target) = target {
                cx.emit(MarkdownEvent::Link(LinkEvent {
                    target,
                    action: LinkAction::ContextMenu {
                        position: event.position,
                    },
                }));
            }
            return;
        }
        if event.button != MouseButton::Left {
            return;
        }
        window.focus(&self.focus, cx);
        if let Some(span) = self.group.clone().filter(|_| event.modifiers.shift).and_then(|(group, order)| group.from_anchor(order, event.position, cx)) {
            drop(entries);
            self.put_span(span, cx);
            self.drag = Some(Drag { origin: event.position, moved: false, link: None, granularity: Granularity::Character, unit: Default::default(), from_anchor: true });
            return;
        }
        let Some(pos) = position_for_point(&entries, event.position) else {
            return;
        };
        let granularity = match event.click_count {
            0 | 1 => Granularity::Character,
            2 => Granularity::Word,
            _ => Granularity::Block,
        };
        let unit = match granularity {
            Granularity::Character => (pos, pos),
            Granularity::Word => word_range(&entries, pos),
            Granularity::Block => block_range(&entries, pos),
        };
        if let Some(selection) = self.selection.as_mut().filter(|_| event.modifiers.shift) {
            selection.head = pos;
        } else {
            self.selection = Some(Selection {
                anchor: unit.0,
                head: unit.1,
            });
            if let Some((group, order)) = &self.group {
                group.anchor_at(*order);
            }
        }
        self.release_others(cx);
        self.drag = Some(Drag {
            origin: event.position,
            moved: false,
            link,
            granularity,
            unit,
            from_anchor: false,
        });
        cx.emit(MarkdownEvent::SelectionChanged);
        cx.notify();
    }

    /// Selects from where the drag began to `position`, across other messages when it lies over them.
    pub(crate) fn drag_to(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = &self.drag else { return };
        if drag.from_anchor {
            if let Some(span) = self.group.clone().and_then(|(group, order)| group.from_anchor(order, position, cx)) {
                self.put_span(span, cx);
            }
            return;
        }
        if self.extend_across(position, cx) {
            return;
        }
        let entries = self.entries.clone();
        let entries = entries.borrow();
        let Some(drag) = self.drag.as_mut() else { return };
        let Some(pos) = position_for_point(&entries, position) else {
            return;
        };
        let (unit_start, unit_end) = drag.unit;
        let selection = match drag.granularity {
            Granularity::Character => Selection {
                anchor: unit_start,
                head: pos,
            },
            Granularity::Word | Granularity::Block => {
                let (start, end) = if drag.granularity == Granularity::Word {
                    word_range(&entries, pos)
                } else {
                    block_range(&entries, pos)
                };
                if pos < unit_start {
                    Selection {
                        anchor: unit_end,
                        head: start,
                    }
                } else {
                    Selection {
                        anchor: unit_start,
                        head: end,
                    }
                }
            }
        };
        let anchor = self.selection.map(|selection| selection.anchor);
        let selection = if drag.granularity == Granularity::Character && anchor.is_some() {
            Selection {
                anchor: anchor.unwrap_or(unit_start),
                head: pos,
            }
        } else {
            selection
        };
        self.selection = Some(selection);
        cx.emit(MarkdownEvent::SelectionChanged);
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, hovered: bool, cx: &mut Context<Self>) {
        if let Some(drag) = self.drag.as_mut() {
            if event.pressed_button != Some(MouseButton::Left) {
                self.drag = None;
                if let Some((group, _)) = &self.group {
                    group.drag_ended(&MemberView::Markdown(cx.entity().downgrade()));
                }
                return;
            }
            let delta = event.position - drag.origin;
            let distance = (f32::from(delta.x).powi(2) + f32::from(delta.y).powi(2)).sqrt();
            if !drag.moved && distance < 3. {
                return;
            }
            drag.moved = true;
            if let Some((group, _)) = &self.group {
                group.drag_at(MemberView::Markdown(cx.entity().downgrade()), event.position);
            }
            self.drag_to(event.position, cx);
            return;
        }
        let entries = self.entries.clone();
        let entries = entries.borrow();
        let link = if hovered {
            link_at_point(&entries, event.position)
        } else {
            None
        };
        if link != self.hovered {
            self.hovered = link;
            let target = link
                .and_then(|(block, index)| entries.get(block)?.text.links.get(index))
                .map(|link| link.target.clone());
            cx.emit(MarkdownEvent::LinkHover(target));
            cx.notify();
        }
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else { return };
        if let Some((group, _)) = &self.group {
            group.drag_ended(&MemberView::Markdown(cx.entity().downgrade()));
        }
        if drag.moved {
            return;
        }
        let entries = self.entries.clone();
        let entries = entries.borrow();
        let clicked = drag.link.filter(|link| {
            link_at_point(&entries, event.position) == Some(*link)
                && drag.granularity == Granularity::Character
        });
        if let Some(link) = clicked {
            let target = entries.get(link.0).and_then(|entry| {
                crate::render::link_target(&entry.text, link.1, self.known_commit.as_deref())
            });
            self.selection = None;
            if let Some(target) = target {
                cx.emit(MarkdownEvent::Link(LinkEvent {
                    target,
                    action: LinkAction::Click {
                        modifiers: event.modifiers,
                    },
                }));
            }
            cx.notify();
            return;
        }
        // A plain click leaves a caret, which a later Shift press extends from.
    }
}

impl Render for MarkdownView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let style = self.style.clone();
        let now = Instant::now();
        let view = cx.entity().downgrade();
        let on_copy: CopyHandler = Rc::new({
            let view = view.clone();
            move |block, text, window, cx| {
                let _ = view.update(cx, |view, cx| view.on_copy_clicked(block, text, window, cx));
            }
        });
        let selection = {
            let entries = self.entries.borrow();
            self.selection.map(|selection| {
                let (start, end) = selection.ordered();
                (clamp(&entries, start), clamp(&entries, end))
            })
        };
        let view_handle = cx.entity().downgrade();
        let link_activate: Rc<dyn Fn(LinkTarget, gpui::Modifiers, &mut App)> = Rc::new(move |target, modifiers, cx| {
            let _ = view_handle.update(cx, |_, cx| cx.emit(MarkdownEvent::Link(LinkEvent { target, action: LinkAction::Click { modifiers } })));
        });
        let inputs = Rc::new(Inputs {
            style: style.clone(),
            view: cx.entity_id(),
            selection: selection.filter(|(start, end)| start != end),
            selection_color: if window.is_window_active() {
                style.selection
            } else {
                style.selection_inactive
            },
            hovered: self.hovered,
            known_commit: self.known_commit.clone(),
            copy_slot: self.copy_slot.clone(),
            fence_slot: self.fence_slot.clone(),
            scrollbar_slot: self.scrollbar_slot.clone(),
            on_copy,
            feedback: self.feedback,
            emphasize_copy: self.emphasize_copy,
            now,
            animate: self.animate && window.is_window_active(),
            highlights: self.highlights.clone(),
            link_keys: Some(crate::layout::LinkKeys {
                focus: self.link_focus.clone(),
                activate: link_activate,
                tip: self.tip_slot.clone(),
                tip_prefix: SharedString::from(format!("md-tip-{}", cx.entity_id())),
            }),
            measure_until: Default::default(),
            asked_frame: Default::default(),
        });
        let mut ctx = inputs.ctx_revealing(window, &self.reveal);
        let grouped = !self.plain && self.source.len() >= GROUPED_FROM;
        let width_hint = self.width.get();
        let mut built = Vec::new();
        self.placed_groups.clear();
        for piece in &mut self.pieces {
            ctx.live = piece.live;
            ctx.doc_offset = piece.start;
            if !grouped || piece.live {
                built.extend(ctx.blocks(&piece.doc.blocks, style.text, false, 0, window, cx));
                continue;
            }
            if piece.groups.is_empty() {
                piece.groups = partition(&piece.doc);
            }
            for group in &piece.groups {
                let entry_base = ctx.entries.len();
                let id_base = ctx.blocks;
                let known = group.settled.borrow().as_ref().map(|settled| settled.edges);
                let (fresh, edges) = match known {
                    Some(edges) => {
                        let settled = group.settled.borrow();
                        let settled = settled.as_ref().expect("settled group");
                        ctx.entries.extend(settled.entries.iter().cloned());
                        ctx.blocks += settled.ids;
                        (None, edges)
                    }
                    None => {
                        let items = ctx.blocks(&group.doc.blocks[group.blocks.clone()], style.text, false, 0, window, cx);
                        let first = items.first().map_or((0., 0.), |item| (item.top, item.inner_top));
                        let last = items.last().map_or((0., 0.), |item| (item.bottom, item.inner_bottom));
                        let edges = [first.0, first.1, last.0, last.1];
                        let (column, _, _) = stack(items, true, true);
                        *group.settled.borrow_mut() = Some(Settled {
                            entries: ctx.entries[entry_base..].to_vec(),
                            ids: ctx.blocks - id_base,
                            edges,
                        });
                        (Some(column.w_full().into_any_element()), edges)
                    }
                };
                self.placed_groups.push((entry_base..ctx.entries.len(), group.clone()));
                let chunk = Chunk { group: group.clone(), inputs: inputs.clone(), entry_base, id_base, fresh, width_hint };
                let [top, inner_top, bottom, inner_bottom] = edges;
                built.push(Built { element: chunk.into_any_element(), top, bottom, inner_top, inner_bottom });
            }
        }
        // `.markdown-body > :first-child` and `:last-child` drop their own outer margins.
        if let Some(first) = built.first_mut() {
            first.top = 0.;
        }
        if let Some(last) = built.last_mut() {
            last.bottom = 0.;
        }
        let (column, _, _) = stack(built, false, false);
        let next_frame = ctx.next_frame;
        *self.entries.borrow_mut() = ctx.entries;
        if let Some(delay) = next_frame.filter(|_| !self.fade_scheduled) {
            // The fade moves in steps, so the message draws again only when the next one is due.
            self.fade_scheduled = true;
            cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(delay).await;
                let _ = this.update(cx, |this, cx| {
                    this.fade_scheduled = false;
                    cx.notify();
                });
            })
            .detach();
        }
        let root = match self.copy_hook {
            Some(hook) => hook(div(), cx.entity().downgrade()),
            None => div(),
        };
        root
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::select_all_action))
            .min_w_0()
            .text_color(style.text)
            .child(SelectionRoot {
                child: column.w_full().into_any_element(),
                view,
                group: self.group.clone(),
                width: self.width.clone(),
            })
    }
}

/// Wraps the rendered blocks and turns mouse input over them into selection and link events.
struct SelectionRoot {
    child: gpui::AnyElement,
    view: WeakEntity<MarkdownView>,
    group: Option<(SelectionGroup, u64)>,
    width: Rc<Cell<Option<Pixels>>>,
}

impl IntoElement for SelectionRoot {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for SelectionRoot {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let child = self.child.request_layout(window, cx);
        let mut style = gpui::Style::default();
        style.size.width = gpui::relative(1.).into();
        (window.request_layout(style, [child], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Hitbox {
        self.child.prepaint(window, cx);
        self.width.set(Some(bounds.size.width));
        if let Some((group, order)) = &self.group {
            group.record(*order, MemberView::Markdown(self.view.clone()), bounds);
        }
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        let view = self.view.clone();
        let down_hitbox = hitbox.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && down_hitbox.is_hovered(window) {
                let _ = view.update(cx, |view, cx| view.mouse_down(event, window, cx));
            }
        });
        let view = self.view.clone();
        let move_hitbox = hitbox.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                let hovered = move_hitbox.is_hovered(window);
                let _ = view.update(cx, |view, cx| view.mouse_move(event, hovered, cx));
            }
        });
        let view = self.view.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                let _ = view.update(cx, |view, cx| view.mouse_up(event, cx));
            }
        });
        self.child.paint(window, cx);
    }
}
