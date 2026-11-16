//! Messages whose selections join into one: a drag that starts in one message and moves over others
//! selects through them in reading order, as a browser's selection runs across a page.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use gpui::{App, Bounds, Pixels, Point, WeakEntity, Window};

use crate::parts::SelectableText;
use crate::view::MarkdownView;

/// Something a group selects across: a message, or plain text such as a fold's summary.
#[derive(Clone)]
pub(crate) enum MemberView {
    Markdown(WeakEntity<MarkdownView>),
    Text(WeakEntity<SelectableText>),
}

/// What a member selects while a drag runs through or ends in it.
#[derive(Clone, Copy)]
pub(crate) enum Span {
    Clear,
    Whole,
    /// From its start, or its end, to the point.
    ToPoint { from_start: bool, point: Point<Pixels> },
    /// From its own anchor to its end, or to its start.
    FromAnchor { to_end: bool },
    /// From its own anchor to the point.
    AnchorTo { point: Point<Pixels> },
}

impl MemberView {
    fn alive(&self) -> bool {
        match self {
            MemberView::Markdown(view) => view.upgrade().is_some(),
            MemberView::Text(text) => text.upgrade().is_some(),
        }
    }

    fn same(&self, other: &MemberView) -> bool {
        match (self, other) {
            (MemberView::Markdown(a), MemberView::Markdown(b)) => a == b,
            (MemberView::Text(a), MemberView::Text(b)) => a == b,
            _ => false,
        }
    }

    fn put(&self, span: Span, cx: &mut App) {
        match self {
            MemberView::Markdown(view) => {
                let _ = view.update(cx, |view, cx| view.put_span(span, cx));
            }
            MemberView::Text(text) => {
                let _ = text.update(cx, |text, cx| text.put_span(span, cx));
            }
        }
    }

    fn selected_text(&self, cx: &App) -> Option<String> {
        match self {
            MemberView::Markdown(view) => view.upgrade()?.read(cx).selected_text(),
            MemberView::Text(text) => text.upgrade()?.read(cx).selected_text(),
        }
    }

    /// Whether it holds a selection's anchor, a caret included.
    fn has_anchor(&self, cx: &App) -> bool {
        match self {
            MemberView::Markdown(view) => view.upgrade().is_some_and(|view| view.read(cx).has_anchor()),
            MemberView::Text(text) => text.upgrade().is_some_and(|text| text.read(cx).has_anchor()),
        }
    }

    /// What a copy puts after this member's text when more follows: a message's paragraphs end with
    /// a blank line, as Chromium adds one after a `<p>`; a summary's flex row ends with a line break.
    fn break_after(&self, cx: &App) -> &'static str {
        match self {
            MemberView::Markdown(_) => "\n\n",
            MemberView::Text(text) => text.upgrade().map_or("\n", |text| text.read(cx).separator()),
        }
    }

    fn focused(&self, window: &Window, cx: &App) -> bool {
        match self {
            MemberView::Markdown(view) => view.upgrade().is_some_and(|view| view.read(cx).is_focused(window)),
            MemberView::Text(text) => text.upgrade().is_some_and(|text| text.read(cx).is_focused(window)),
        }
    }

    fn drag_to(&self, point: Point<Pixels>, cx: &mut App) {
        match self {
            MemberView::Markdown(view) => {
                let _ = view.update(cx, |view, cx| view.drag_to(point, cx));
            }
            MemberView::Text(text) => {
                let _ = text.update(cx, |text, cx| text.drag_to(point, cx));
            }
        }
    }
}

struct Member {
    view: MemberView,
    bounds: Bounds<Pixels>,
    frame: u64,
}

#[derive(Default)]
struct Inner {
    frame: u64,
    /// By each member's place in reading order.
    members: BTreeMap<u64, Member>,
    /// The member a drag began in and where the pointer last was, while the button is held.
    drag: Option<(MemberView, Point<Pixels>)>,
    /// The member the last plain press landed in, which holds the selection's anchor.
    anchor: Option<u64>,
}

#[derive(Clone, Default)]
pub struct SelectionGroup(Rc<RefCell<Inner>>);

impl SelectionGroup {
    pub fn new() -> Self {
        Self::default()
    }

    /// Call once per frame the messages are drawn in, before they paint; only the messages painted
    /// since are hit by the pointer.
    pub fn begin_frame(&self) {
        self.0.borrow_mut().frame += 1;
    }

    pub(crate) fn drag_at(&self, view: MemberView, point: Point<Pixels>) {
        self.0.borrow_mut().drag = Some((view, point));
    }

    pub(crate) fn drag_ended(&self, view: &MemberView) {
        let mut inner = self.0.borrow_mut();
        if inner.drag.as_ref().is_some_and(|(held, _)| held.same(view)) {
            inner.drag = None;
        }
    }

    /// The button went up, wherever it did.
    pub fn end_drag(&self) {
        self.0.borrow_mut().drag = None;
    }

    /// Where the pointer is while a drag selects, so the host can scroll toward it past its edge.
    pub fn dragging(&self) -> Option<Point<Pixels>> {
        self.0.borrow().drag.as_ref().map(|(_, point)| *point)
    }

    /// Selects again to where the pointer is, after the messages under it moved.
    pub fn follow_drag(&self, cx: &mut App) {
        let Some((view, point)) = self.0.borrow().drag.clone() else { return };
        view.drag_to(point, cx);
    }

    pub(crate) fn record(&self, order: u64, view: MemberView, bounds: Bounds<Pixels>) {
        let mut inner = self.0.borrow_mut();
        let frame = inner.frame;
        inner.members.insert(order, Member { view, bounds, frame });
    }

    /// The drawn member under a point, or the one nearest it above or below.
    fn member_at(&self, point: Point<Pixels>) -> Option<u64> {
        let inner = self.0.borrow();
        let mut best: Option<(Pixels, u64)> = None;
        for (order, member) in inner.members.iter().filter(|(_, member)| member.frame == inner.frame) {
            let distance = if point.y < member.bounds.top() {
                member.bounds.top() - point.y
            } else if point.y > member.bounds.bottom() {
                point.y - member.bounds.bottom()
            } else {
                gpui::px(0.)
            };
            if best.is_none_or(|(nearest, _)| distance < nearest) {
                best = Some((distance, *order));
            }
        }
        best.map(|(_, order)| order)
    }

    /// Every member that still exists, in reading order.
    fn members(&self) -> Vec<(u64, MemberView)> {
        let mut inner = self.0.borrow_mut();
        inner.members.retain(|_, member| member.view.alive());
        inner.members.iter().map(|(order, member)| (*order, member.view.clone())).collect()
    }

    /// Selects from the drag's member `origin` across to the member under `point`: whole members
    /// between, part of the last. Returns that member, or `None` when the point is over `origin`,
    /// in which case the others let go.
    pub(crate) fn extend(&self, origin: u64, point: Point<Pixels>, cx: &mut App) -> Option<u64> {
        let head = self.member_at(point).filter(|head| *head != origin);
        for (other, view) in self.members() {
            if other == origin {
                continue;
            }
            let span = match head {
                Some(head) if other == head => Span::ToPoint { from_start: head > origin, point },
                Some(head) if (origin.min(head)..origin.max(head)).contains(&other) => Span::Whole,
                _ => Span::Clear,
            };
            view.put(span, cx);
        }
        head
    }

    /// A plain press in `order` puts the selection's anchor there.
    pub(crate) fn anchor_at(&self, order: u64) {
        self.0.borrow_mut().anchor = Some(order);
    }

    /// A Shift press or drag in `me` selects from the anchor, held in another member, to `point`:
    /// whole members between, the anchor's member from its anchor on. Puts every other member's part
    /// and returns `me`'s; `None` when the anchor is in `me` or no member holds it any more.
    pub(crate) fn from_anchor(&self, me: u64, point: Point<Pixels>, cx: &mut App) -> Option<Span> {
        let anchor = self.0.borrow().anchor.filter(|anchor| *anchor != me)?;
        let members = self.members();
        if !members.iter().any(|(order, view)| *order == anchor && view.has_anchor(cx)) {
            return None;
        }
        let head = self.member_at(point).unwrap_or(me);
        let forward = head > anchor;
        let span_of = |order: u64| {
            if order == anchor {
                if head == anchor { Span::AnchorTo { point } } else { Span::FromAnchor { to_end: forward } }
            } else if order == head {
                Span::ToPoint { from_start: forward, point }
            } else if (anchor.min(head)..anchor.max(head)).contains(&order) {
                Span::Whole
            } else {
                Span::Clear
            }
        };
        for (other, view) in members {
            if other != me {
                view.put(span_of(other), cx);
            }
        }
        Some(span_of(me))
    }

    /// Clears every member but `origin`.
    pub(crate) fn release_others(&self, origin: u64, cx: &mut App) {
        for (other, view) in self.members() {
            if other != origin {
                view.put(Span::Clear, cx);
            }
        }
    }

    /// Whether the keys moved from `origin` to another member, whose own press then chose what stays selected.
    pub(crate) fn focus_moved_within(&self, origin: u64, window: &Window, cx: &App) -> bool {
        self.members().iter().any(|(other, view)| *other != origin && view.focused(window, cx))
    }

    /// The selection across the group in reading order, when it reaches past `origin`'s own (or lies
    /// wholly in another member, as after a Shift press that came back to the anchor's).
    pub(crate) fn text(&self, origin: u64, own: Option<String>, cx: &App) -> Option<String> {
        let mut own = Some(own);
        let mut parts: Vec<(String, &'static str)> = Vec::new();
        let mut others = false;
        for (other, view) in self.members() {
            let text = if other == origin { own.take().flatten() } else { view.selected_text(cx) };
            others |= other != origin && text.is_some();
            parts.extend(text.map(|text| (text, view.break_after(cx))));
        }
        if !others {
            return None;
        }
        let mut out = String::new();
        let mut before = "";
        for (text, after) in parts {
            out.push_str(before);
            out.push_str(&text);
            before = after;
        }
        Some(out)
    }
}
