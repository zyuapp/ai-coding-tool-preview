//! Text entry: one editor for single-line fields and the multi-line composer. It owns the text,
//! selection, IME composition, clipboard and undo; whoever embeds it supplies the type through the
//! inherited text style and draws the field around it.

mod buffer;
mod element;

use std::ops::Range;
use std::time::Duration;

use crate::components::scrollbar::ScrollModel;

use gpui::{
    actions, App, Bounds, ClipboardItem, Context, EntityInputHandler, EventEmitter, FocusHandle, Focusable, KeyBinding,
    prelude::FluentBuilder as _, KeyContext, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollWheelEvent,
    SharedString, Task, UTF16Selection, Window,
};

pub use buffer::{EditBuffer, EditKind};
pub use element::InputLayout;

actions!(
    text_input,
    [
        Backspace,
        Delete,
        DeleteWordLeft,
        DeleteWordRight,
        DeleteToLineStart,
        DeleteToLineEnd,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        LineStart,
        LineEnd,
        SelectToLineStart,
        SelectToLineEnd,
        DocStart,
        DocEnd,
        SelectToDocStart,
        SelectToDocEnd,
        SelectAll,
        Copy,
        Cut,
        Paste,
        Undo,
        Redo,
        /// Inserts a line break in a multi-line field.
        Newline,
        /// Submits a single-line field, or a multi-line one set to submit on Enter.
        Enter,
        ShowCharacterPalette,
    ]
);

pub const KEY_CONTEXT: &str = "TextInput";

/// The editing keys. Up and Down are only bound in a multi-line field, so a list around a
/// single-line search field keeps its arrows.
pub fn bind_keys(cx: &mut App) {
    let single = Some(KEY_CONTEXT);
    let multi = Some("TextInput && multiline");
    let mut bindings = vec![
        KeyBinding::new("backspace", Backspace, single),
        KeyBinding::new("shift-backspace", Backspace, single),
        KeyBinding::new("delete", Delete, single),
        KeyBinding::new("left", Left, single),
        KeyBinding::new("right", Right, single),
        KeyBinding::new("shift-left", SelectLeft, single),
        KeyBinding::new("shift-right", SelectRight, single),
        KeyBinding::new("home", LineStart, single),
        KeyBinding::new("end", LineEnd, single),
        KeyBinding::new("shift-home", SelectToLineStart, single),
        KeyBinding::new("shift-end", SelectToLineEnd, single),
        KeyBinding::new("enter", Enter, single),
        KeyBinding::new("up", Up, multi),
        KeyBinding::new("down", Down, multi),
        KeyBinding::new("shift-up", SelectUp, multi),
        KeyBinding::new("shift-down", SelectDown, multi),
        KeyBinding::new("shift-enter", Newline, multi),
        KeyBinding::new("alt-enter", Newline, multi),
    ];
    if cfg!(target_os = "macos") {
        bindings.extend([
            KeyBinding::new("alt-backspace", DeleteWordLeft, single),
            KeyBinding::new("alt-delete", DeleteWordRight, single),
            KeyBinding::new("cmd-backspace", DeleteToLineStart, single),
            KeyBinding::new("cmd-delete", DeleteToLineEnd, single),
            KeyBinding::new("ctrl-k", DeleteToLineEnd, single),
            KeyBinding::new("ctrl-h", Backspace, single),
            KeyBinding::new("ctrl-d", Delete, single),
            KeyBinding::new("alt-left", WordLeft, single),
            KeyBinding::new("alt-right", WordRight, single),
            KeyBinding::new("alt-shift-left", SelectWordLeft, single),
            KeyBinding::new("alt-shift-right", SelectWordRight, single),
            KeyBinding::new("cmd-left", LineStart, single),
            KeyBinding::new("cmd-right", LineEnd, single),
            KeyBinding::new("ctrl-a", LineStart, single),
            KeyBinding::new("ctrl-e", LineEnd, single),
            KeyBinding::new("cmd-shift-left", SelectToLineStart, single),
            KeyBinding::new("cmd-shift-right", SelectToLineEnd, single),
            KeyBinding::new("cmd-up", DocStart, single),
            KeyBinding::new("cmd-down", DocEnd, single),
            KeyBinding::new("cmd-shift-up", SelectToDocStart, single),
            KeyBinding::new("cmd-shift-down", SelectToDocEnd, single),
            KeyBinding::new("cmd-a", SelectAll, single),
            KeyBinding::new("cmd-c", Copy, single),
            KeyBinding::new("cmd-x", Cut, single),
            KeyBinding::new("cmd-v", Paste, single),
            KeyBinding::new("cmd-z", Undo, single),
            KeyBinding::new("cmd-shift-z", Redo, single),
            KeyBinding::new("ctrl-cmd-space", ShowCharacterPalette, single),
            KeyBinding::new("ctrl-p", Up, multi),
            KeyBinding::new("ctrl-n", Down, multi),
        ]);
    } else {
        bindings.extend([
            KeyBinding::new("ctrl-backspace", DeleteWordLeft, single),
            KeyBinding::new("ctrl-delete", DeleteWordRight, single),
            KeyBinding::new("ctrl-left", WordLeft, single),
            KeyBinding::new("ctrl-right", WordRight, single),
            KeyBinding::new("ctrl-shift-left", SelectWordLeft, single),
            KeyBinding::new("ctrl-shift-right", SelectWordRight, single),
            KeyBinding::new("ctrl-home", DocStart, single),
            KeyBinding::new("ctrl-end", DocEnd, single),
            KeyBinding::new("ctrl-shift-home", SelectToDocStart, single),
            KeyBinding::new("ctrl-shift-end", SelectToDocEnd, single),
            KeyBinding::new("ctrl-a", SelectAll, single),
            KeyBinding::new("ctrl-c", Copy, single),
            KeyBinding::new("ctrl-x", Cut, single),
            KeyBinding::new("ctrl-v", Paste, single),
            KeyBinding::new("ctrl-insert", Copy, single),
            KeyBinding::new("shift-delete", Cut, single),
            KeyBinding::new("shift-insert", Paste, single),
            KeyBinding::new("ctrl-z", Undo, single),
            KeyBinding::new("ctrl-shift-z", Redo, single),
            KeyBinding::new("ctrl-y", Redo, single),
        ]);
    }
    cx.bind_keys(bindings);
}

/// What a field tells whoever holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputEvent {
    /// The text changed, by typing, pasting, undo, or `set_text`.
    Changed,
    /// Enter in a single-line field, or in a multi-line one set to submit on it.
    Submit,
    Focus,
    Blur,
}

/// How tall a multi-line field grows: from `min_rows` up to `max_height`, then it scrolls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Autosize {
    pub min_rows: usize,
    pub max_height: Option<Pixels>,
}

pub struct TextInput {
    focus_handle: FocusHandle,
    buffer: EditBuffer,
    placeholder: SharedString,
    /// The name read for the field, when it is not its placeholder.
    label: Option<SharedString>,
    multiline: bool,
    autosize: Autosize,
    submit_on_enter: bool,
    disabled: bool,
    read_only: bool,
    /// The layout the last paint drew, for hit testing, IME rectangles and vertical motion.
    layout: Option<InputLayout>,
    /// How far the text has scrolled within the field, shared with a scrollbar laid over it.
    scroll: ScrollModel,
    /// How far above and below its box the text still shows as it scrolls: the padding around a
    /// field, which its text scrolls through.
    bleed: (Pixels, Pixels),
    scroll_to_cursor: bool,
    /// The caret got there by moving rather than by an edit, so a caret out of sight is centred.
    caret_moved: bool,
    selecting: bool,
    /// Whether a drag extends by word or by line, after a double or triple click.
    drag_unit: DragUnit,
    drag_origin: Range<usize>,
    /// The x a run of Up and Down presses keeps returning to.
    goal_x: Option<Pixels>,
    caret: gpui::Entity<element::Caret>,
    blink_epoch: usize,
    _blink: Option<Task<()>>,
    was_focused: bool,
    /// Focused in the active window: only then does the caret blink.
    blinking: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DragUnit {
    Char,
    Word,
    Line,
}

impl EventEmitter<InputEvent> for TextInput {}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

const BLINK: Duration = Duration::from_millis(500);

impl TextInput {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle().tab_stop(true),
            buffer: EditBuffer::default(),
            placeholder: SharedString::default(),
            label: None,
            multiline: false,
            autosize: Autosize { min_rows: 1, max_height: None },
            submit_on_enter: false,
            disabled: false,
            read_only: false,
            layout: None,
            scroll: ScrollModel::default(),
            bleed: (Pixels::ZERO, Pixels::ZERO),
            scroll_to_cursor: false,
            caret_moved: false,
            selecting: false,
            drag_unit: DragUnit::Char,
            drag_origin: 0..0,
            goal_x: None,
            caret: gpui::AppContext::new(cx, |_| element::Caret::new()),
            blink_epoch: 0,
            _blink: None,
            was_focused: false,
            blinking: false,
        }
    }

    /// A multi-line editor that grows with its text, as the composer does.
    pub fn multiline(mut self, autosize: Autosize) -> Self {
        self.multiline = true;
        self.autosize = autosize;
        self
    }

    /// Enter submits and Shift-Enter breaks the line, in a multi-line field.
    pub fn submit_on_enter(mut self, submit: bool) -> Self {
        self.submit_on_enter = submit;
        self
    }

    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// The name assistive technology reads for the field; its placeholder otherwise.
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn set_label(&mut self, label: impl Into<SharedString>) {
        let label = label.into();
        if self.label.as_ref() != Some(&label) {
            self.label = Some(label);
        }
    }

    pub fn access_label(&self) -> &SharedString {
        self.label.as_ref().unwrap_or(&self.placeholder)
    }

    pub fn with_text(mut self, text: &str) -> Self {
        self.buffer = EditBuffer::new(text);
        self
    }

    pub fn text(&self) -> &str {
        self.buffer.text()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.text().is_empty()
    }

    pub fn is_multiline(&self) -> bool {
        self.multiline
    }

    pub fn placeholder_text(&self) -> &SharedString {
        &self.placeholder
    }

    pub fn set_placeholder(&mut self, placeholder: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.placeholder = placeholder.into();
        cx.notify();
    }

    pub fn set_autosize(&mut self, autosize: Autosize, cx: &mut Context<Self>) {
        self.autosize = autosize;
        cx.notify();
    }

    pub fn autosize(&self) -> Autosize {
        self.autosize
    }

    pub fn set_submit_on_enter(&mut self, submit: bool) {
        self.submit_on_enter = submit;
    }

    pub fn set_disabled(&mut self, disabled: bool, cx: &mut Context<Self>) {
        self.disabled = disabled;
        cx.notify();
    }

    pub fn is_disabled(&self) -> bool {
        self.disabled
    }

    /// Selectable and copyable, but not editable.
    pub fn set_read_only(&mut self, read_only: bool, cx: &mut Context<Self>) {
        self.read_only = read_only;
        cx.notify();
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Replaces the text as code; undo can step back over it.
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.buffer.text() == text {
            return;
        }
        self.buffer.set_text(text);
        self.edited(cx);
    }

    /// Replaces the text and forgets its history, as after the draft was sent.
    pub fn reset(&mut self, text: &str, cx: &mut Context<Self>) {
        self.buffer.reset(text);
        self.scroll.set_scrolled(Point::default());
        self.edited(cx);
    }

    /// Inserts at the selection, as typing a token or dropping a path does.
    pub fn insert(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        let text = self.sanitize(text);
        self.buffer.replace(None, &text, EditKind::Other);
        self.edited(cx);
    }

    /// Replaces a byte range, leaving the caret after the new text.
    pub fn replace_range(&mut self, range: Range<usize>, text: &str, cx: &mut Context<Self>) {
        let text = self.sanitize(text);
        self.buffer.replace(Some(range), &text, EditKind::Other);
        self.edited(cx);
    }

    pub fn selection(&self) -> Range<usize> {
        self.buffer.selection()
    }

    pub fn cursor(&self) -> usize {
        self.buffer.cursor()
    }

    pub fn select_range(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        self.buffer.select_range(range, false);
        self.moved(cx);
    }

    pub fn move_to_end(&mut self, cx: &mut Context<Self>) {
        let end = self.buffer.text().len();
        self.buffer.move_to(end);
        self.moved(cx);
    }

    pub fn select_all_text(&mut self, cx: &mut Context<Self>) {
        self.buffer.select_all();
        self.moved(cx);
    }

    /// Whether Up would leave the first row, which a composer uses to recall the last message.
    pub fn cursor_on_first_row(&self) -> bool {
        match &self.layout {
            Some(layout) => layout.row_for_offset(self.buffer.cursor()) == 0,
            None => !self.buffer.text()[..self.buffer.cursor()].contains('\n'),
        }
    }

    pub fn cursor_on_last_row(&self) -> bool {
        match &self.layout {
            Some(layout) => layout.row_for_offset(self.buffer.cursor()) + 1 >= layout.row_count(),
            None => !self.buffer.text()[self.buffer.cursor()..].contains('\n'),
        }
    }

    pub fn buffer(&self) -> &EditBuffer {
        &self.buffer
    }

    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus_handle);
    }

    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus_handle.is_focused(window)
    }

    fn sanitize(&self, text: &str) -> String {
        if self.multiline {
            text.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            text.chars().filter(|ch| *ch != '\n' && *ch != '\r').collect()
        }
    }

    fn edited(&mut self, cx: &mut Context<Self>) {
        self.goal_x = None;
        self.scroll_to_cursor = true;
        self.caret_moved = false;
        self.show_cursor(cx);
        cx.emit(InputEvent::Changed);
        cx.notify();
    }

    fn moved(&mut self, cx: &mut Context<Self>) {
        self.scroll_to_cursor = true;
        self.caret_moved = true;
        self.show_cursor(cx);
        cx.notify();
    }

    fn editable(&self) -> bool {
        !self.disabled && !self.read_only
    }

    /// Restarts the blink with the caret showing, as any edit or move does.
    fn show_cursor(&mut self, cx: &mut Context<Self>) {
        self.caret.update(cx, |caret, cx| caret.show(true, cx));
        self.blink_epoch += 1;
        if !self.blinking {
            self._blink = None;
            return;
        }
        let epoch = self.blink_epoch;
        self._blink = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(BLINK).await;
            let keep = this
                .update(cx, |input, cx| {
                    if input.blink_epoch != epoch {
                        return false;
                    }
                    input.caret.update(cx, |caret, cx| caret.show(!caret.visible(), cx));
                    true
                })
                .unwrap_or(false);
            if !keep {
                break;
            }
        }));
    }

    pub(crate) fn caret(&self) -> gpui::Entity<element::Caret> {
        self.caret.clone()
    }

    pub(crate) fn key_context(&self) -> KeyContext {
        let mut context = KeyContext::new_with_defaults();
        context.add(KEY_CONTEXT);
        if self.multiline {
            context.add("multiline");
        }
        context
    }

    /// Called by the element each frame with the focus state, to start and stop the blink and
    /// tell the holder when focus arrives or leaves. The caret rests while the window is inactive.
    pub(crate) fn sync_focus(&mut self, focused: bool, window_active: bool, cx: &mut Context<Self>) {
        let blinking = focused && window_active;
        if blinking != self.blinking {
            self.blinking = blinking;
            self.show_cursor(cx);
        }
        if focused == self.was_focused {
            return;
        }
        self.was_focused = focused;
        if focused {
            if !self.multiline && crate::focus::take_tabbed_to(&self.focus_handle, cx) {
                self.select_all_text(cx);
            }
            cx.emit(InputEvent::Focus);
        } else {
            self.selecting = false;
            cx.emit(InputEvent::Blur);
        }
    }

    pub(crate) fn set_layout(&mut self, layout: InputLayout) {
        self.layout = Some(layout);
    }

    /// Keeps the caret inside the visible area after it moved, given the area's size and the
    /// caret's height.
    pub(crate) fn settle_scroll(&mut self, layout: &InputLayout, visible: gpui::Size<Pixels>, caret_height: Pixels) -> Point<Pixels> {
        let content = layout.content_size();
        let mut scroll = self.scroll.scrolled();
        if self.scroll_to_cursor {
            self.scroll_to_cursor = false;
            let caret = layout.position_for_offset(self.buffer.cursor());
            if self.multiline {
                let caret_top = caret.y + (layout.line_height() - caret_height) / 2.0;
                scroll.y = reveal_caret(scroll.y, caret_top, caret_height, visible.height, self.bleed, self.caret_moved);
            } else {
                let margin = gpui::px(1.0);
                if caret.x < scroll.x {
                    scroll.x = caret.x;
                } else if caret.x + margin > scroll.x + visible.width {
                    scroll.x = caret.x + margin - visible.width;
                }
            }
        }
        let max_x = (content.width + gpui::px(1.0) - visible.width).max(gpui::px(0.0));
        let max_y = (content.height - visible.height).max(gpui::px(0.0));
        scroll.x = scroll.x.clamp(gpui::px(0.0), max_x);
        scroll.y = scroll.y.clamp(gpui::px(0.0), max_y);
        if !self.multiline {
            scroll.y = gpui::px(0.0);
        }
        self.scroll.set_scrolled(scroll);
        scroll
    }

    /// Where a scrollbar over the field reads and moves its scroll.
    pub fn scroll_model(&self) -> ScrollModel {
        self.scroll.clone()
    }

    /// The field's box with the bleed above and below it: where its text shows.
    pub(crate) fn shown_bounds(&self, bounds: Bounds<Pixels>) -> Bounds<Pixels> {
        let (top, bottom) = self.bleed;
        Bounds::from_corners(gpui::point(bounds.left(), bounds.top() - top), gpui::point(bounds.right(), bounds.bottom() + bottom))
    }

    /// Lets the text show `top` above and `bottom` below the field as it scrolls.
    pub fn set_bleed(&mut self, top: Pixels, bottom: Pixels, cx: &mut Context<Self>) {
        self.bleed = (top, bottom);
        cx.notify();
    }

    fn offset_for_point(&self, position: Point<Pixels>) -> usize {
        match &self.layout {
            Some(layout) => layout.offset_for_point(position),
            None => 0,
        }
    }

    // Movement

    fn collapse_or(&mut self, forward: bool, step: impl FnOnce(&EditBuffer, usize) -> usize, cx: &mut Context<Self>) {
        let selection = self.buffer.selection();
        let target = if selection.is_empty() {
            step(&self.buffer, self.buffer.cursor())
        } else if forward {
            selection.end
        } else {
            selection.start
        };
        self.buffer.move_to(target);
        self.goal_x = None;
        self.moved(cx);
    }

    fn extend(&mut self, step: impl FnOnce(&EditBuffer, usize) -> usize, cx: &mut Context<Self>) {
        let target = step(&self.buffer, self.buffer.cursor());
        self.buffer.select_to(target);
        self.goal_x = None;
        self.moved(cx);
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        self.collapse_or(false, |buffer, at| buffer.previous_grapheme(at), cx);
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        self.collapse_or(true, |buffer, at| buffer.next_grapheme(at), cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.extend(|buffer, at| buffer.previous_grapheme(at), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.extend(|buffer, at| buffer.next_grapheme(at), cx);
    }

    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        let at = self.buffer.cursor();
        let target = self.buffer.previous_word(at);
        self.buffer.move_to(target);
        self.goal_x = None;
        self.moved(cx);
    }

    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        let at = self.buffer.cursor();
        let target = self.buffer.next_word(at);
        self.buffer.move_to(target);
        self.goal_x = None;
        self.moved(cx);
    }

    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.extend(|buffer, at| buffer.previous_word(at), cx);
    }

    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.extend(|buffer, at| buffer.next_word(at), cx);
    }

    /// The start of the row the caret is on: the visual row in a wrapped field.
    fn row_start(&self, at: usize) -> usize {
        match &self.layout {
            Some(layout) if self.multiline => layout.row_range(layout.row_for_offset(at)).start,
            _ if self.multiline => self.buffer.line_start(at),
            _ => 0,
        }
    }

    fn row_end(&self, at: usize) -> usize {
        match &self.layout {
            Some(layout) if self.multiline => layout.row_range(layout.row_for_offset(at)).end,
            _ if self.multiline => self.buffer.line_end(at),
            _ => self.buffer.text().len(),
        }
    }

    fn line_start(&mut self, _: &LineStart, _: &mut Window, cx: &mut Context<Self>) {
        let target = self.row_start(self.buffer.cursor());
        self.buffer.move_to(target);
        self.goal_x = None;
        self.moved(cx);
    }

    fn line_end(&mut self, _: &LineEnd, _: &mut Window, cx: &mut Context<Self>) {
        let target = self.row_end(self.buffer.cursor());
        self.buffer.move_to(target);
        self.goal_x = None;
        self.moved(cx);
    }

    fn select_to_line_start(&mut self, _: &SelectToLineStart, _: &mut Window, cx: &mut Context<Self>) {
        let target = self.row_start(self.buffer.cursor());
        self.buffer.select_to(target);
        self.moved(cx);
    }

    fn select_to_line_end(&mut self, _: &SelectToLineEnd, _: &mut Window, cx: &mut Context<Self>) {
        let target = self.row_end(self.buffer.cursor());
        self.buffer.select_to(target);
        self.moved(cx);
    }

    fn doc_start(&mut self, _: &DocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_to(0);
        self.moved(cx);
    }

    fn doc_end(&mut self, _: &DocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_to(self.buffer.text().len());
        self.moved(cx);
    }

    fn select_to_doc_start(&mut self, _: &SelectToDocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.select_to(0);
        self.moved(cx);
    }

    fn select_to_doc_end(&mut self, _: &SelectToDocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.select_to(self.buffer.text().len());
        self.moved(cx);
    }

    /// The offset one visual row up or down from the caret, keeping the goal column.
    fn vertical(&mut self, down: bool) -> usize {
        let at = self.buffer.cursor();
        let Some(layout) = &self.layout else {
            return if down { self.buffer.text().len() } else { 0 };
        };
        let row = layout.row_for_offset(at);
        let goal = *self.goal_x.get_or_insert_with(|| layout.position_for_offset(at).x);
        if !down && row == 0 {
            return 0;
        }
        if down && row + 1 >= layout.row_count() {
            return self.buffer.text().len();
        }
        let target = if down { row + 1 } else { row - 1 };
        layout.offset_for_row_x(target, goal)
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        let selection = self.buffer.selection();
        if !selection.is_empty() {
            self.buffer.move_to(selection.start);
            self.moved(cx);
            return;
        }
        let target = self.vertical(false);
        self.buffer.move_to(target);
        self.moved(cx);
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        let selection = self.buffer.selection();
        if !selection.is_empty() {
            self.buffer.move_to(selection.end);
            self.moved(cx);
            return;
        }
        let target = self.vertical(true);
        self.buffer.move_to(target);
        self.moved(cx);
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        let target = self.vertical(false);
        let goal = self.goal_x;
        self.buffer.select_to(target);
        self.goal_x = goal;
        self.moved(cx);
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        let target = self.vertical(true);
        let goal = self.goal_x;
        self.buffer.select_to(target);
        self.goal_x = goal;
        self.moved(cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.select_all();
        self.moved(cx);
    }

    // Editing

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        let to = self.buffer.previous_grapheme(self.buffer.cursor());
        self.buffer.delete_to(to);
        self.edited(cx);
    }

    fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        let to = self.buffer.next_grapheme(self.buffer.cursor());
        self.buffer.delete_to(to);
        self.edited(cx);
    }

    fn delete_word_left(&mut self, _: &DeleteWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        let to = self.buffer.previous_word(self.buffer.cursor());
        self.buffer.delete_to(to);
        self.edited(cx);
    }

    fn delete_word_right(&mut self, _: &DeleteWordRight, _: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        let to = self.buffer.next_word(self.buffer.cursor());
        self.buffer.delete_to(to);
        self.edited(cx);
    }

    fn delete_to_line_start(&mut self, _: &DeleteToLineStart, _: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        let at = self.buffer.cursor();
        let start = self.row_start(at);
        let to = if start == at { self.buffer.previous_grapheme(at) } else { start };
        self.buffer.delete_to(to);
        self.edited(cx);
    }

    fn delete_to_line_end(&mut self, _: &DeleteToLineEnd, _: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        let at = self.buffer.cursor();
        let end = self.row_end(at);
        let to = if end == at { self.buffer.next_grapheme(at) } else { end };
        self.buffer.delete_to(to);
        self.edited(cx);
    }

    fn newline(&mut self, _: &Newline, _: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() || !self.multiline {
            return;
        }
        self.buffer.replace(None, "\n", EditKind::Other);
        self.edited(cx);
    }

    fn enter(&mut self, _: &Enter, window: &mut Window, cx: &mut Context<Self>) {
        if self.multiline && !self.submit_on_enter {
            self.newline(&Newline, window, cx);
            return;
        }
        cx.emit(InputEvent::Submit);
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let selected = self.buffer.selected_text();
        if !selected.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(selected.to_string()));
        }
    }

    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        let selected = self.buffer.selected_text();
        if selected.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(selected.to_string()));
        if self.editable() {
            self.buffer.replace(None, "", EditKind::Other);
            self.edited(cx);
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let text = self.sanitize(&text);
            self.buffer.replace(None, &text, EditKind::Other);
            self.edited(cx);
        }
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        if self.editable() && self.buffer.undo() {
            self.edited(cx);
        }
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if self.editable() && self.buffer.redo() {
            self.edited(cx);
        }
    }

    fn show_character_palette(&mut self, _: &ShowCharacterPalette, window: &mut Window, _: &mut Context<Self>) {
        window.show_character_palette();
    }

    // Pointer

    /// A press at `event.position`, which may lie outside the field: the caret goes to the nearest place.
    pub fn press(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.on_mouse_down(event, window, cx);
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        window.focus(&self.focus_handle);
        let offset = self.offset_for_point(event.position);
        self.selecting = true;
        self.goal_x = None;
        match event.click_count {
            2 => {
                let word = self.buffer.word_range(offset);
                self.drag_unit = DragUnit::Word;
                self.drag_origin = word.clone();
                self.buffer.select_range(word, false);
            }
            count if count >= 3 => {
                let line = if self.multiline { self.buffer.line_range(offset) } else { 0..self.buffer.text().len() };
                self.drag_unit = DragUnit::Line;
                self.drag_origin = line.clone();
                self.buffer.select_range(line, false);
            }
            _ => {
                self.drag_unit = DragUnit::Char;
                if event.modifiers.shift {
                    self.buffer.select_to(offset);
                } else {
                    self.buffer.move_to(offset);
                }
            }
        }
        self.moved(cx);
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let offset = self.offset_for_point(event.position);
        match self.drag_unit {
            DragUnit::Char => self.buffer.select_to(offset),
            unit => {
                let around = if unit == DragUnit::Word {
                    self.buffer.word_range(offset)
                } else {
                    self.buffer.line_range(offset)
                };
                let origin = self.drag_origin.clone();
                if around.start < origin.start {
                    self.buffer.select_range(around.start..origin.end, true);
                } else {
                    self.buffer.select_range(origin.start..around.end.max(origin.end), false);
                }
            }
        }
        self.moved(cx);
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }

    /// Scrolls by a wheel turned over the field, or over the padding around it.
    pub fn scroll_wheel(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.on_scroll(event, window, cx);
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(layout) = &self.layout else { return };
        let delta = event.delta.pixel_delta(layout.line_height());
        let visible = layout.visible_size();
        let content = layout.content_size();
        if self.multiline {
            if content.height <= visible.height {
                return;
            }
            let max = content.height - visible.height;
            let mut scroll = self.scroll.scrolled();
            let next = (scroll.y - delta.y).clamp(gpui::px(0.0), max);
            if next != scroll.y {
                scroll.y = next;
                self.scroll.set_scrolled(scroll);
                cx.stop_propagation();
                cx.notify();
            }
        } else {
            let horizontal = if delta.x.abs() > delta.y.abs() { delta.x } else { gpui::px(0.0) };
            let max = (content.width - visible.width).max(gpui::px(0.0));
            let mut scroll = self.scroll.scrolled();
            let next = (scroll.x - horizontal).clamp(gpui::px(0.0), max);
            if next != scroll.x {
                scroll.x = next;
                self.scroll.set_scrolled(scroll);
                cx.stop_propagation();
                cx.notify();
            }
        }
        let _ = window;
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.buffer.range_from_utf16(&range_utf16);
        actual_range.replace(self.buffer.range_to_utf16(&range));
        Some(self.buffer.text()[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.buffer.range_to_utf16(&self.buffer.selection()),
            reversed: self.buffer.reversed(),
        })
    }

    fn marked_text_range(&self, _window: &mut Window, _cx: &mut Context<Self>) -> Option<Range<usize>> {
        self.buffer.marked().map(|range| self.buffer.range_to_utf16(&range))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.unmark();
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.editable() {
            return;
        }
        let range = range_utf16.map(|range| self.buffer.range_from_utf16(&range));
        let text = self.sanitize(new_text);
        self.buffer.replace(range, &text, EditKind::Typing);
        self.edited(cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.editable() {
            return;
        }
        let range = range_utf16.map(|range| self.buffer.range_from_utf16(&range));
        let selected = new_selected_range_utf16.map(|range| {
            let utf16: Vec<u16> = new_text.encode_utf16().collect();
            let to_bytes = |units: usize| String::from_utf16_lossy(&utf16[..units.min(utf16.len())]).len();
            to_bytes(range.start)..to_bytes(range.end)
        });
        self.buffer.replace_and_mark(range, new_text, selected);
        self.edited(cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.layout.as_ref()?;
        let range = self.buffer.range_from_utf16(&range_utf16);
        Some(layout.bounds_for_range(range))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let layout = self.layout.as_ref()?;
        let offset = layout.offset_for_point(point);
        Some(self.buffer.offset_to_utf16(offset))
    }
}

impl gpui::Render for TextInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        use gpui::{div, CursorStyle, InteractiveElement, ParentElement, Styled};
        div()
            .key_context(self.key_context())
            .track_focus(&self.focus_handle)
            .w_full()
            .min_w_0()
            .when(!self.disabled, |this| this.cursor(CursorStyle::IBeam))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_word_right))
            .on_action(cx.listener(Self::delete_to_line_start))
            .on_action(cx.listener(Self::delete_to_line_end))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::line_start))
            .on_action(cx.listener(Self::line_end))
            .on_action(cx.listener(Self::select_to_line_start))
            .on_action(cx.listener(Self::select_to_line_end))
            .on_action(cx.listener(Self::doc_start))
            .on_action(cx.listener(Self::doc_end))
            .on_action(cx.listener(Self::select_to_doc_start))
            .on_action(cx.listener(Self::select_to_doc_end))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::enter))
            .on_action(cx.listener(Self::show_character_palette))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .child(element::TextInputElement::new(cx.entity()))
    }
}

/// The scroll that shows a multiline field's caret box, as Chromium reveals it: within the bleed,
/// to the nearest edge, except that a move leaving the caret wholly out of sight centres it.
fn reveal_caret(scroll: Pixels, caret_top: Pixels, caret_height: Pixels, visible: Pixels, (above, below): (Pixels, Pixels), centre_hidden: bool) -> Pixels {
    let caret_bottom = caret_top + caret_height;
    let shown_top = scroll - above;
    let shown_bottom = scroll + visible + below;
    let revealed = if centre_hidden && (caret_bottom <= shown_top || caret_top >= shown_bottom) {
        caret_top + caret_height / 2.0 + above - (above + visible + below) / 2.0
    } else if caret_top < shown_top {
        caret_top + above
    } else if caret_bottom > shown_bottom {
        caret_bottom - visible - below
    } else {
        scroll
    };
    revealed.floor()
}

#[cfg(test)]
mod tests {
    use super::reveal_caret;
    use gpui::px;

    /// A 17px prompt on 25.5px lines, 380px tall, with 15px and 5px of bleed, as in the composer.
    fn reveal(scroll: f32, row: f32, moved: bool) -> f32 {
        let caret_height = 17.0 * 1.2;
        f32::from(reveal_caret(px(scroll), px(row * 25.5 + (25.5 - caret_height) / 2.0), px(caret_height), px(380.0), (px(15.0), px(5.0)), moved))
    }

    #[test]
    fn caret_reveal_matches_chromium() {
        // Typing onto a new last line scrolls only far enough to show the caret.
        assert_eq!(reveal(249.0, 25.0, false), 275.0);
        // A caret already in sight stays put.
        assert_eq!(reveal(275.0, 24.0, true), 275.0);
        // Moving up onto a half-hidden line takes it to the top edge; past it, the line is centred.
        assert_eq!(reveal(275.0, 10.0, true), 272.0);
        assert_eq!(reveal(272.0, 9.0, true), 57.0);
    }
}
