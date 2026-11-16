//! Plain text drawn in pieces by the host, such as a fold's summary row, that selects like a
//! message: by dragging over it, and across the other members of its selection group.

use std::ops::Range;

use gpui::{
    canvas, prelude::*, App, ClipboardItem, Context, DispatchPhase, Entity, FocusHandle, Focusable, HighlightStyle, HitboxBehavior, Hsla,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, SharedString, StyledText, Subscription, TextLayout, Window,
};

use unicode_segmentation::UnicodeSegmentation;

use crate::group::{MemberView, SelectionGroup, Span};

/// A piece's index and a byte offset in its text.
type Pos = (usize, usize);

struct Part {
    text: SharedString,
    layout: TextLayout,
}

struct Drag {
    origin: Point<Pixels>,
    moved: bool,
    /// A Shift press that selects on from the group's anchor in another member.
    from_anchor: bool,
}

pub struct SelectableText {
    parts: Vec<Part>,
    /// Anchor, then head.
    selection: Option<(Pos, Pos)>,
    drag: Option<Drag>,
    group: Option<(SelectionGroup, u64)>,
    colors: (Hsla, Hsla),
    /// What a copy puts between pieces.
    separator: &'static str,
    focus: FocusHandle,
    _blur: Subscription,
}

impl Focusable for SelectableText {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl SelectableText {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        let blur = cx.on_blur(&focus, window, |this, window, cx| {
            if !window.is_window_active() {
                return;
            }
            let group = this.group.clone();
            if group.as_ref().is_some_and(|(group, order)| group.focus_moved_within(*order, window, cx)) {
                return;
            }
            if this.selection.take().is_some() {
                cx.notify();
            }
            if let Some((group, order)) = group {
                group.release_others(order, cx);
            }
        });
        Self { parts: Vec::new(), selection: None, drag: None, group: None, colors: (Hsla::default(), Hsla::default()), separator: "\n", focus, _blur: blur }
    }

    /// Joins the group a drag selects across; `order` is this text's place in reading order.
    pub fn set_selection_group(&mut self, group: SelectionGroup, order: u64) {
        self.group = Some((group, order));
    }

    /// The selection's colour while the window is active, and while it is in the background.
    pub fn set_selection_colors(&mut self, active: Hsla, inactive: Hsla) {
        self.colors = (active, inactive);
    }

    pub fn set_separator(&mut self, separator: &'static str) {
        self.separator = separator;
    }

    pub(crate) fn separator(&self) -> &'static str {
        self.separator
    }

    /// Starts a new drawing; the pieces follow in reading order.
    pub fn begin(&mut self) {
        self.parts.clear();
    }

    /// The next piece, drawn with `highlights` (sorted, apart) and the selection over them.
    pub fn part(&mut self, text: impl Into<SharedString>, highlights: Vec<(Range<usize>, HighlightStyle)>, window: &Window) -> StyledText {
        let text: SharedString = text.into();
        let index = self.parts.len();
        let selected = self.selection.and_then(|selection| {
            let ((from_part, from), (to_part, to)) = ordered(selection);
            if index < from_part || index > to_part {
                return None;
            }
            let start = if index == from_part { from.min(text.len()) } else { 0 };
            let end = if index == to_part { to.min(text.len()) } else { text.len() };
            Some(floor(&text, start)..floor(&text, end)).filter(|range| !range.is_empty())
        });
        let color = if window.is_window_active() { self.colors.0 } else { self.colors.1 };
        let styled = StyledText::new(text.clone()).with_highlights(merge(highlights, selected, color));
        self.parts.push(Part { text, layout: styled.layout().clone() });
        styled
    }

    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    /// Whether it holds a selection's anchor, a caret included.
    pub(crate) fn has_anchor(&self) -> bool {
        self.selection.is_some()
    }

    pub fn has_selection(&self) -> bool {
        self.selection.is_some_and(|(anchor, head)| anchor != head)
    }

    pub fn selected_text(&self) -> Option<String> {
        let (from, to) = ordered(self.selection?);
        if from == to || self.parts.is_empty() {
            return None;
        }
        let last = to.0.min(self.parts.len() - 1);
        let mut out = String::new();
        for index in from.0..=last {
            let text = &self.parts[index].text;
            let start = if index == from.0 { floor(text, from.1) } else { 0 };
            let end = if index == to.0 { floor(text, to.1) } else { text.len() };
            if index > from.0 {
                out.push_str(self.separator);
            }
            out.push_str(&text[start.min(end)..end]);
        }
        Some(out).filter(|text| !text.is_empty())
    }

    /// Writes the selection, across the group, to the clipboard; false when nothing is selected.
    pub fn copy_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let own = self.selected_text();
        let text = match &self.group {
            Some((group, order)) => group.text(*order, own.clone(), cx).or(own),
            None => own,
        };
        let Some(text) = text else { return false };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        true
    }

    fn end(&self) -> Pos {
        match self.parts.len().checked_sub(1) {
            Some(last) => (last, self.parts[last].text.len()),
            None => (0, 0),
        }
    }

    /// The place in the pieces nearest `point`: in the piece closest to it, the character boundary
    /// under it with its height clamped into the piece.
    fn pos_at(&self, point: Point<Pixels>) -> Pos {
        let mut best: Option<((Pixels, Pixels), usize, gpui::Bounds<Pixels>)> = None;
        for (index, part) in self.parts.iter().enumerate() {
            let Some(bounds) = part.layout.painted_bounds() else { continue };
            let gap = |at: Pixels, low: Pixels, high: Pixels| if at < low { low - at } else if at > high { at - high } else { gpui::px(0.) };
            let distance = (gap(point.y, bounds.top(), bounds.bottom()), gap(point.x, bounds.left(), bounds.right()));
            if best.is_none_or(|(nearest, _, _)| distance < nearest) {
                best = Some((distance, index, bounds));
            }
        }
        let Some((_, index, bounds)) = best else { return (0, 0) };
        let part = &self.parts[index];
        let len = part.text.len();
        let inside = gpui::point(point.x.clamp(bounds.left(), bounds.right()), point.y.clamp(bounds.top() + gpui::px(1.), (bounds.bottom() - gpui::px(1.)).max(bounds.top())));
        let at = match part.layout.index_for_position(inside) {
            Ok(at) | Err(at) => at,
        };
        // Past the start of an ellipsis is past the text it stands for.
        let laid = part.layout.len();
        let at = if laid != len && at + '…'.len_utf8() > laid { len } else { at.min(len) };
        (index, floor(&part.text, at))
    }

    pub(crate) fn put_span(&mut self, span: Span, cx: &mut Context<Self>) {
        let selection = match span {
            Span::Clear => None,
            Span::Whole => Some(((0, 0), self.end())),
            Span::ToPoint { from_start, point } => Some((if from_start { (0, 0) } else { self.end() }, self.pos_at(point))),
            Span::FromAnchor { to_end } => Some((self.selection.map_or((0, 0), |(anchor, _)| anchor), if to_end { self.end() } else { (0, 0) })),
            Span::AnchorTo { point } => Some((self.selection.map_or((0, 0), |(anchor, _)| anchor), self.pos_at(point))),
        };
        if selection != self.selection {
            self.selection = selection;
            cx.notify();
        }
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        let group = self.group.clone();
        if let Some(span) = group.as_ref().filter(|_| event.modifiers.shift).and_then(|(group, order)| group.from_anchor(*order, event.position, cx)) {
            self.put_span(span, cx);
            self.drag = Some(Drag { origin: event.position, moved: false, from_anchor: true });
            return;
        }
        let pos = self.pos_at(event.position);
        match self.selection.as_mut().filter(|_| event.modifiers.shift) {
            Some(selection) => selection.1 = pos,
            None => {
                self.selection = Some(match event.click_count {
                    0 | 1 => (pos, pos),
                    2 => self.word_at(pos),
                    _ => ((pos.0, 0), (pos.0, self.parts.get(pos.0).map_or(0, |part| part.text.len()))),
                });
                if let Some((group, order)) = &group {
                    group.anchor_at(*order);
                }
            }
        }
        if let Some((group, order)) = group {
            group.release_others(order, cx);
        }
        self.drag = Some(Drag { origin: event.position, moved: false, from_anchor: false });
        cx.notify();
    }

    /// The word around `pos` in its piece, as a double click picks it.
    fn word_at(&self, (part, at): Pos) -> (Pos, Pos) {
        let Some(text) = self.parts.get(part).map(|part| &part.text) else { return ((part, at), (part, at)) };
        for (start, word) in text.split_word_bound_indices() {
            let end = start + word.len();
            if at < end || end == text.len() {
                return ((part, start), (part, end));
            }
        }
        ((part, at), (part, at))
    }

    /// Selects from where the drag began to `point`, across other members when it lies over them.
    pub(crate) fn drag_to(&mut self, point: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = &self.drag else { return };
        if drag.from_anchor {
            if let Some(span) = self.group.clone().and_then(|(group, order)| group.from_anchor(order, point, cx)) {
                self.put_span(span, cx);
            }
            return;
        }
        let anchor = self.selection.map_or((0, 0), |(anchor, _)| anchor);
        let across = self.group.clone().and_then(|(group, order)| Some((group.extend(order, point, cx)?, order)));
        let head = match across {
            Some((head, order)) if head > order => self.end(),
            Some(_) => (0, 0),
            None => self.pos_at(point),
        };
        if self.selection != Some((anchor, head)) {
            self.selection = Some((anchor, head));
            cx.notify();
        }
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.as_mut() else { return };
        let me = MemberView::Text(cx.entity().downgrade());
        if event.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            if let Some((group, _)) = &self.group {
                group.drag_ended(&me);
            }
            return;
        }
        let delta = event.position - drag.origin;
        if !drag.moved && f32::from(delta.x).hypot(f32::from(delta.y)) < 3. {
            return;
        }
        drag.moved = true;
        if let Some((group, _)) = &self.group {
            group.drag_at(me, event.position);
        }
        self.drag_to(event.position, cx);
    }

    /// A plain click leaves a caret, which a later Shift press extends from.
    fn mouse_up(&mut self, cx: &mut Context<Self>) {
        if self.drag.take().is_none() {
            return;
        }
        if let Some((group, _)) = &self.group {
            group.drag_ended(&MemberView::Text(cx.entity().downgrade()));
        }
    }
}

/// Fills the element it sits in (positioned absolutely over it) and turns the pointer over that
/// element into selection of `text`'s pieces.
pub fn selectable_area(text: &Entity<SelectableText>) -> impl IntoElement {
    let (prepaint_text, paint_text) = (text.clone(), text.clone());
    canvas(
        move |bounds, window, cx| {
            if let Some((group, order)) = prepaint_text.read(cx).group.clone() {
                group.record(order, MemberView::Text(prepaint_text.downgrade()), bounds);
            }
            window.insert_hitbox(bounds, HitboxBehavior::Normal)
        },
        move |_, hitbox, window, _| {
            let text = paint_text.downgrade();
            let down = hitbox.clone();
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble && event.button == MouseButton::Left && down.is_hovered(window) {
                    let _ = text.update(cx, |text, cx| text.mouse_down(event, window, cx));
                }
            });
            let text = paint_text.downgrade();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble {
                    let _ = text.update(cx, |text, cx| text.mouse_move(event, cx));
                }
            });
            let text = paint_text.downgrade();
            window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                    let _ = text.update(cx, |text, cx| text.mouse_up(cx));
                }
            });
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

fn ordered((anchor, head): (Pos, Pos)) -> (Pos, Pos) {
    if head < anchor {
        (head, anchor)
    } else {
        (anchor, head)
    }
}

fn floor(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// `highlights` with the selection's background laid over the part of them it covers.
fn merge(highlights: Vec<(Range<usize>, HighlightStyle)>, selected: Option<Range<usize>>, color: Hsla) -> Vec<(Range<usize>, HighlightStyle)> {
    let Some(selected) = selected else { return highlights };
    let mut cuts = vec![selected.start, selected.end];
    for (range, _) in &highlights {
        cuts.extend([range.start, range.end]);
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut out = Vec::new();
    for pair in cuts.windows(2) {
        let range = pair[0]..pair[1];
        let lit = highlights.iter().find(|(lit, _)| lit.start <= range.start && range.end <= lit.end).map(|(_, style)| style.clone());
        let style = match (lit, selected.start <= range.start && range.end <= selected.end) {
            (None, false) => continue,
            (Some(style), false) => style,
            (lit, true) => HighlightStyle { background_color: Some(color), ..lit.unwrap_or_default() },
        };
        out.push((range, style));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_splits_the_matches_it_covers() {
        let lit = HighlightStyle { color: Some(Hsla::default()), ..Default::default() };
        let color = gpui::hsla(0.5, 0.5, 0.5, 1.);
        let merged = merge(vec![(2..6, lit.clone())], Some(4..9), color);
        let ranges: Vec<_> = merged.iter().map(|(range, style)| (range.clone(), style.background_color.is_some(), style.color.is_some())).collect();
        assert_eq!(ranges, vec![(2..4, false, true), (4..6, true, true), (6..9, true, false)]);
    }
}
