//! Long messages drawn in groups of top-level blocks. A group keeps its text entries and its height
//! at each width it was drawn at, and builds and lays out its blocks only while it is on screen;
//! elsewhere it is a box of its known height, or of an estimate until a few groups a frame are
//! measured off screen. The list around the message keeps what is on screen in place while
//! estimates give way to heights (`Window::list_item_resized`, `Window::offer_scroll_anchor`).

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{prelude::*,
    px, relative, size, AnyElement, App, AvailableSpace, Bounds, Element, ElementId,
    GlobalElementId, InspectorElementId, LayoutId, Pixels, Style, Window,
};

use crate::parse::{Block, Document, ParaPart};
use crate::render::{stack, Inputs, TextEntry};
use crate::style::MarkdownStyle;

/// A message with less text than this draws every block every frame.
pub(crate) const GROUPED_FROM: usize = 12_000;
/// About how much text one group holds.
const GROUP_CHARS: usize = 1_500;
/// How many widths a group remembers its height at.
const WIDTHS: usize = 4;
/// How long one drawing of a message may spend laying out groups off screen.
const MEASURE_BUDGET: Duration = Duration::from_millis(4);

/// What a group passes to the message around it, known once it has been built.
pub(crate) struct Settled {
    pub entries: Vec<TextEntry>,
    /// How many element ids its blocks take.
    pub ids: usize,
    /// The margins of its first and last blocks, as `Built` holds them.
    pub edges: [f32; 4],
}

/// What a group's text amounts to, for a height before it was ever laid out.
#[derive(Clone, Copy, Default)]
struct Bulk {
    prose: usize,
    lines: usize,
    blocks: usize,
    code_lines: usize,
    code_blocks: usize,
}

pub(crate) struct Group {
    pub doc: Rc<Document>,
    pub blocks: Range<usize>,
    pub settled: RefCell<Option<Settled>>,
    heights: RefCell<Vec<(Pixels, Pixels)>>,
    bulk: Bulk,
    /// Whether its text was drawn last frame, so hit testing may read where.
    painted: Cell<bool>,
    /// Where it was laid out last frame, drawn or not.
    pub placed: Cell<Option<Bounds<Pixels>>>,
}

fn weight(block: &Block) -> usize {
    match block {
        Block::Paragraph(parts) | Block::Plain(parts) => parts
            .iter()
            .map(|part| match part {
                ParaPart::Text(text) => text.text.len(),
                ParaPart::Image { .. } => GROUP_CHARS / 4,
            })
            .sum(),
        Block::Heading { content, .. } => content.text.len(),
        Block::Quote(blocks) => blocks.iter().map(weight).sum(),
        Block::List { items, .. } => items
            .iter()
            .map(|item| 16 + item.blocks.iter().map(weight).sum::<usize>())
            .sum(),
        Block::Code(code) => code.code.text.len(),
        Block::Table(table) => table.source.len(),
        Block::Rule => 1,
    }
}

fn add_bulk(bulk: &mut Bulk, block: &Block) {
    match block {
        Block::Code(code) => {
            bulk.code_blocks += 1;
            bulk.code_lines += code.code.text.lines().count().max(1);
        }
        Block::Table(table) => {
            bulk.blocks += 1;
            bulk.lines += table.source.lines().count();
        }
        Block::List { items, .. } => {
            for item in items {
                bulk.lines += 1;
                for inner in &item.blocks {
                    add_bulk(bulk, inner);
                }
            }
        }
        Block::Quote(blocks) => blocks.iter().for_each(|inner| add_bulk(bulk, inner)),
        block => {
            bulk.blocks += 1;
            bulk.prose += weight(block);
        }
    }
}

/// Cuts a document's top-level blocks into groups of about `GROUP_CHARS` of text.
pub(crate) fn partition(doc: &Rc<Document>) -> Vec<Rc<Group>> {
    let mut groups = Vec::new();
    let mut start = 0;
    let mut held = 0;
    let mut bulk = Bulk::default();
    for (index, block) in doc.blocks.iter().enumerate() {
        held += weight(block);
        add_bulk(&mut bulk, block);
        if held >= GROUP_CHARS || index + 1 == doc.blocks.len() {
            groups.push(Rc::new(Group {
                doc: doc.clone(),
                blocks: start..index + 1,
                settled: RefCell::new(None),
                heights: RefCell::new(Vec::new()),
                bulk: std::mem::take(&mut bulk),
                painted: Cell::new(false),
                placed: Cell::new(None),
            }));
            start = index + 1;
            held = 0;
        }
    }
    groups
}

fn same_width(a: Pixels, b: Pixels) -> bool {
    (f32::from(a) - f32::from(b)).abs() < 0.01
}

impl Group {
    fn height(&self, width: Pixels) -> Option<Pixels> {
        self.heights
            .borrow()
            .iter()
            .find(|(at, _)| same_width(*at, width))
            .map(|(_, height)| *height)
    }

    /// A guess for a width it was never drawn at: the last height reflowed to the new width, or
    /// before any, what its text would fill at an average glyph width.
    fn estimate(&self, width: Option<Pixels>, style: &MarkdownStyle) -> Size {
        let heights = self.heights.borrow();
        match (heights.last().copied(), width) {
            (Some((at, height)), Some(width)) if width > px(0.) => (width, height * (f32::from(at) / f32::from(width))),
            (Some((at, height)), _) => (at, height),
            (None, width) => {
                let width = width.unwrap_or(px(720.));
                let font = f32::from(style.font_size);
                let line = font * style.line_height;
                let per_line = (f32::from(width) / (font * 0.5)).max(1.);
                let bulk = self.bulk;
                let prose = (bulk.prose as f32 / per_line).ceil() + bulk.blocks as f32 * 0.5;
                let height = (prose + bulk.lines as f32) * line + bulk.blocks as f32 * font + bulk.code_lines as f32 * font * 1.5 + bulk.code_blocks as f32 * (font * 3.);
                (width, px(height))
            }
        }
    }

    fn remember(&self, width: Pixels, height: Pixels) {
        let mut heights = self.heights.borrow_mut();
        heights.retain(|(at, _)| !same_width(*at, width));
        if heights.len() >= WIDTHS {
            heights.remove(0);
        }
        heights.push((width, height));
    }

    /// Takes its text out of hit testing once it is no longer drawn.
    fn forget_painted(&self) {
        if self.painted.replace(false) {
            if let Some(settled) = self.settled.borrow().as_ref() {
                for entry in &settled.entries {
                    entry.text.cache.borrow_mut().forget_painted();
                }
            }
        }
    }

    fn key(self: &Rc<Self>) -> u64 {
        Rc::as_ptr(self) as usize as u64
    }
}

type Size = (Pixels, Pixels);

/// One group in a message: its blocks while they are drawn, a box of its known height otherwise.
pub(crate) struct Chunk {
    pub group: Rc<Group>,
    pub inputs: Rc<Inputs>,
    /// The index of its first text entry and its first element id in the message.
    pub entry_base: usize,
    pub id_base: usize,
    /// Its blocks, when this frame built them already.
    pub fresh: Option<AnyElement>,
    /// The width the message was drawn at last frame.
    pub width_hint: Option<Pixels>,
}

impl Chunk {
    fn build(&mut self, window: &mut Window, cx: &mut App) -> AnyElement {
        if let Some(fresh) = self.fresh.take() {
            return fresh;
        }
        let mut ctx = self.inputs.ctx(window);
        ctx.entry_base = self.entry_base;
        ctx.blocks = self.id_base;
        let blocks = &self.group.doc.blocks[self.group.blocks.clone()];
        let color = self.inputs.style.text;
        let built = ctx.blocks(blocks, color, false, 0, window, cx);
        let (column, _, _) = stack(built, true, true);
        column.w_full().into_any_element()
    }

    /// Its blocks laid out on their own at `width`, their height kept.
    fn lay_out(&mut self, width: Pixels, window: &mut Window, cx: &mut App) -> (AnyElement, Pixels) {
        let mut element = self.build(window, cx);
        let laid = element.layout_as_root(size(AvailableSpace::Definite(width), AvailableSpace::MinContent), window, cx);
        self.group.remember(width, laid.height);
        (element, laid.height)
    }

    /// Whether this drawing may still lay out a group off screen.
    fn may_measure(&self) -> bool {
        let now = Instant::now();
        let until = self.inputs.measure_until.get().unwrap_or_else(|| {
            let until = now + MEASURE_BUDGET;
            self.inputs.measure_until.set(Some(until));
            until
        });
        now < until
    }

    /// Draws the message again next frame, once for the whole message.
    fn ask_frame(&self, window: &mut Window) {
        if !self.inputs.asked_frame.replace(true) {
            let view = self.inputs.view;
            window.on_next_frame(move |_, cx| cx.notify(view));
        }
    }
}

impl IntoElement for Chunk {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Chunk {
    type RequestLayoutState = LayoutId;
    type PrepaintState = Option<AnyElement>;

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
        _: &mut App,
    ) -> (LayoutId, LayoutId) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let group = self.group.clone();
        let hint = self.width_hint;
        let text_style = self.inputs.style.clone();
        let id = window.request_measured_layout(style, move |known, available, _, _| {
            let width = known.width.or(match available.width {
                AvailableSpace::Definite(width) => Some(width),
                _ => hint,
            });
            // Layout asks at its unrounded width, while heights are kept at the drawn width, which
            // the message's width was drawn at too.
            let near_hint = hint.filter(|hint| width.is_none_or(|width| (f32::from(width) - f32::from(*hint)).abs() < 1.));
            let found = width
                .and_then(|width| group.height(width).map(|height| (width, height)))
                .or_else(|| near_hint.and_then(|hint| group.height(hint).map(|height| (width.unwrap_or(hint), height))));
            let (width, height) = found.unwrap_or_else(|| group.estimate(width, &text_style));
            size(width, known.height.unwrap_or(height))
        });
        (id, id)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        id: &mut LayoutId,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let visible = window.layout_content_mask().intersects(&bounds);
        self.group.placed.set(Some(bounds));
        let width = bounds.size.width;
        let origin = window.unrounded_layout_origin(*id);
        let mut placed = Bounds { origin, size: gpui::size(width, window.unrounded_layout_size(*id).height) };
        let key = self.group.key();
        if !visible {
            self.group.forget_painted();
            if self.group.height(width).is_none() {
                if self.may_measure() {
                    let (_, height) = self.lay_out(width, window, cx);
                    if (f32::from(height) - f32::from(placed.size.height)).abs() > 0.01 && !window.list_item_resized(placed, height) {
                        self.ask_frame(window);
                    }
                    placed.size.height = height;
                } else {
                    self.ask_frame(window);
                }
            }
            window.offer_scroll_anchor(key, placed);
            return None;
        }
        let (mut element, height) = self.lay_out(width, window, cx);
        if (f32::from(height) - f32::from(placed.size.height)).abs() > 0.01 && !window.list_item_resized(placed, height) {
            // Its box was drawn at a height it no longer has: lay the message out again.
            self.ask_frame(window);
        }
        placed.size.height = height;
        window.offer_scroll_anchor(key, placed);
        element.prepaint_at(origin, window, cx);
        self.group.painted.set(true);
        Some(element)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut LayoutId,
        element: &mut Option<AnyElement>,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(element) = element {
            element.paint(window, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{parse, ParseOptions};

    #[test]
    fn groups_cover_every_block_in_order() {
        let text = (0..400)
            .map(|index| format!("Paragraph {index} {}", "word ".repeat(index % 50)))
            .collect::<Vec<_>>()
            .join("\n\n");
        let doc = Rc::new(parse(&text, ParseOptions::default()));
        let groups = partition(&doc);
        assert!(groups.len() > 1);
        let mut next = 0;
        for group in &groups {
            assert_eq!(group.blocks.start, next);
            assert!(group.blocks.end > group.blocks.start);
            next = group.blocks.end;
        }
        assert_eq!(next, doc.blocks.len());
    }
}
