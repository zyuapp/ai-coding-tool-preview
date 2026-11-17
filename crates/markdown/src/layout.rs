//! Inline layout: line breaking over runs of mixed fonts and sizes, with inline boxes for code
//! padding and link glyphs, the way a browser lays out a paragraph under `white-space: normal` and
//! `overflow-wrap: break-word`, or a `pre` block under `white-space: pre`.

use std::cell::RefCell;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    point, px, size, App, Bounds, Corners, Element, ElementId, Font, GlobalElementId, Hitbox,
    HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId, Pixels, Point, SharedString,
    Size, Style, TextRun, Window, WindowTextSystem,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::parse::{BoxKind, InlineText};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

/// The font a span is drawn in.
#[derive(Clone, Debug, PartialEq)]
pub struct SpanFont {
    pub font: Font,
    pub size: Pixels,
    /// How far the span's baseline sits above the line's.
    pub raise: Pixels,
    /// CSS `letter-spacing`: space added after each glyph.
    pub tracking: Pixels,
}

/// Everything about a text block that decides where its lines break.
#[derive(Clone, Debug, PartialEq)]
pub struct TextSpec {
    /// One per span of the text.
    pub fonts: Vec<SpanFont>,
    /// One per inline box of the text.
    pub box_widths: Vec<Pixels>,
    /// The block's own font, whose metrics place the baseline in each line.
    pub strut: Font,
    pub size: Pixels,
    pub line_height: Pixels,
    /// `white-space: normal` rather than `pre`.
    pub wrap: bool,
    pub align: TextAlign,
}

impl TextSpec {
    fn key(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for font in &self.fonts {
            font.font.hash(&mut hasher);
            f32::from(font.size).to_bits().hash(&mut hasher);
            f32::from(font.raise).to_bits().hash(&mut hasher);
            f32::from(font.tracking).to_bits().hash(&mut hasher);
        }
        for width in &self.box_widths {
            f32::from(*width).to_bits().hash(&mut hasher);
        }
        self.strut.hash(&mut hasher);
        f32::from(self.size).to_bits().hash(&mut hasher);
        f32::from(self.line_height).to_bits().hash(&mut hasher);
        self.wrap.hash(&mut hasher);
        hasher.finish()
    }
}

/// A stretch of one span with no newline in it, shaped as one line.
#[derive(Debug)]
struct Piece {
    range: Range<usize>,
    span: usize,
    /// Each glyph's text index paired with its x, then the piece's end paired with its width.
    xs: Vec<(usize, Pixels)>,
    ascent: Pixels,
    descent: Pixels,
}

impl Piece {
    fn x_at(&self, index: usize) -> Pixels {
        let at = self.xs.partition_point(|(glyph, _)| *glyph < index);
        self.xs
            .get(at)
            .or(self.xs.last())
            .map_or(px(0.), |(_, x)| *x)
    }

    /// The character boundary in `range` nearest to `x`, measured from the piece's start.
    fn closest_index(&self, range: &Range<usize>, x: Pixels) -> usize {
        let mut best = range.start;
        let mut best_distance = f32::MAX;
        let from = self.xs.partition_point(|(glyph, _)| *glyph < range.start);
        for (glyph, glyph_x) in &self.xs[from..] {
            if *glyph > range.end {
                break;
            }
            let distance = (f32::from(*glyph_x) - f32::from(x)).abs();
            if distance < best_distance {
                best_distance = distance;
                best = *glyph;
            }
        }
        best
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ItemKind {
    Text { piece: usize },
    Box { index: usize },
}

/// Something placed on a line: text from one piece, or an inline box.
#[derive(Clone, Debug)]
pub struct Item {
    pub kind: ItemKind,
    pub range: Range<usize>,
    pub x: Pixels,
    pub width: Pixels,
}

#[derive(Clone, Debug)]
pub struct Line {
    pub range: Range<usize>,
    /// The end of the line's content, before trailing spaces and the newline.
    pub content_end: usize,
    pub width: Pixels,
    pub items: Vec<Item>,
}

#[derive(Debug, Default)]
pub struct Lines {
    pub lines: Vec<Line>,
    pub width: Pixels,
    pub line_height: Pixels,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct WrapMode {
    width: Option<Pixels>,
    min_content: bool,
}

/// Where a text block was last drawn, which hit testing reads.
#[derive(Clone, Debug)]
pub struct Painted {
    pub bounds: Bounds<Pixels>,
    pub lines: Rc<Lines>,
    offsets: Vec<Pixels>,
}

/// Shaping and line breaks kept with the text across frames.
#[derive(Debug, Default)]
pub struct LayoutCache {
    key: Option<u64>,
    pieces: Vec<Piece>,
    breaks: Vec<(usize, bool)>,
    /// Per box, whether it ends the line a break at its offset closes rather than starting the next.
    glued: Vec<bool>,
    results: Vec<(WrapMode, Rc<Lines>)>,
    painted: Option<Painted>,
}

const FIT_EPSILON: f32 = 0.5;

fn black() -> Hsla {
    Hsla {
        h: 0.,
        s: 0.,
        l: 0.,
        a: 1.,
    }
}

impl LayoutCache {
    fn prepare(&mut self, text: &InlineText, spec: &TextSpec, system: &WindowTextSystem) {
        let key = spec.key();
        if self.key == Some(key) {
            return;
        }
        self.key = Some(key);
        self.results.clear();
        self.pieces.clear();
        for (span_index, span) in text.spans.iter().enumerate() {
            let Some(font) = spec.fonts.get(span_index) else {
                continue;
            };
            let mut start = span.range.start;
            for part in text.text[span.range.clone()].split('\n') {
                let end = start + part.len();
                if end > start {
                    let run = TextRun {
                        len: end - start,
                        font: font.font.clone(),
                        color: black(),
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    };
                    let layout =
                        system.layout_line(&text.text[start..end], font.size, &[run], None);
                    let tracking = font.tracking;
                    let mut xs: Vec<(usize, Pixels)> = layout
                        .runs
                        .iter()
                        .flat_map(|run| run.glyphs.iter())
                        .enumerate()
                        .map(|(ordinal, glyph)| {
                            (
                                start + glyph.index,
                                glyph.position.x + tracking * ordinal as f32,
                            )
                        })
                        .collect();
                    let glyphs = xs.len();
                    xs.sort_by_key(|(index, _)| *index);
                    xs.dedup_by_key(|(index, _)| *index);
                    xs.push((end, layout.width + tracking * glyphs as f32));
                    self.pieces.push(Piece {
                        range: start..end,
                        span: span_index,
                        xs,
                        ascent: layout.ascent,
                        descent: layout.descent,
                    });
                }
                start = end + 1;
            }
        }
        self.breaks = if spec.wrap {
            line_breaks(&text.text)
        } else {
            let mut breaks: Vec<(usize, bool)> = text
                .text
                .match_indices('\n')
                .map(|(at, _)| (at + 1, true))
                .collect();
            if breaks.last().is_none_or(|(at, _)| *at != text.text.len()) {
                breaks.push((text.text.len(), true));
            }
            breaks
        };
        self.glued = text.boxes.iter().map(|item| item.glued_before()).collect();
        if spec.wrap {
            glyph_breaks(text, spec, &mut self.breaks, &mut self.glued);
        }
    }

    fn piece_at(&self, index: usize) -> Option<usize> {
        let at = self
            .pieces
            .partition_point(|piece| piece.range.end <= index);
        self.pieces
            .get(at)
            .filter(|piece| piece.range.start <= index)
            .map(|_| at)
    }

    fn text_width(&self, range: Range<usize>) -> Pixels {
        let mut width = px(0.);
        let mut at = self
            .pieces
            .partition_point(|piece| piece.range.end <= range.start);
        while let Some(piece) = self.pieces.get(at) {
            if piece.range.start >= range.end {
                break;
            }
            let start = range.start.max(piece.range.start);
            let end = range.end.min(piece.range.end);
            if end > start {
                width += piece.x_at(end) - piece.x_at(start);
            }
            at += 1;
        }
        width
    }

    fn box_in(item_at: usize, glued_before: bool, range: &Range<usize>, text_len: usize) -> bool {
        if glued_before {
            (range.start < item_at || item_at == 0 && range.start == 0) && item_at <= range.end
        } else {
            range.start <= item_at
                && (item_at < range.end || item_at == text_len && range.end == text_len)
        }
    }

    fn width(&self, text: &InlineText, spec: &TextSpec, range: Range<usize>) -> Pixels {
        let boxes: Pixels = text
            .boxes
            .iter()
            .enumerate()
            .filter(|(index, item)| Self::box_in(item.at, self.glued.get(*index).copied().unwrap_or_else(|| item.glued_before()), &range, text.text.len()))
            .map(|(index, _)| spec.box_widths.get(index).copied().unwrap_or_default())
            .fold(px(0.), |sum, width| sum + width);
        self.text_width(range) + boxes
    }

    fn compute(&self, text: &InlineText, spec: &TextSpec, mode: WrapMode) -> Lines {
        let source = text.text.as_str();
        if source.is_empty() && text.boxes.is_empty() {
            // An empty element makes no line box.
            return Lines {
                lines: Vec::new(),
                width: px(0.),
                line_height: spec.line_height,
            };
        }
        let max = mode.width.map(|width| f32::from(width) + FIT_EPSILON);
        let trailing = |range: &Range<usize>| {
            range.start + source[range.clone()].trim_end_matches([' ', '\n']).len()
        };
        let mut ranges: Vec<Range<usize>> = Vec::new();
        let mut line_start = 0;
        let mut line_width = 0.0f32;
        let mut has_content = false;
        let mut previous = 0;
        for &(at, mandatory) in &self.breaks {
            let segment = previous..at;
            previous = at;
            if segment.is_empty() {
                continue;
            }
            let full = f32::from(self.width(text, spec, segment.clone()));
            let fit = f32::from(self.width(
                text,
                spec,
                segment.start..trailing(&segment).max(segment.start),
            ));
            if let Some(max) = max {
                if has_content && line_width + fit > max {
                    ranges.push(line_start..segment.start);
                    line_start = segment.start;
                    line_width = 0.;
                    has_content = false;
                }
                if !has_content && fit > max && !mode.min_content {
                    // overflow-wrap: break-word splits a word that cannot fit a line of its own.
                    let mut cursor = segment.start;
                    let content_end = trailing(&segment);
                    for (offset, grapheme) in
                        source[segment.start..content_end].grapheme_indices(true)
                    {
                        let index = segment.start + offset;
                        let end = index + grapheme.len();
                        let width = f32::from(self.width(text, spec, cursor..end));
                        if width > max && index > cursor {
                            ranges.push(line_start..index);
                            line_start = index;
                            cursor = index;
                        }
                    }
                    line_width = f32::from(self.width(text, spec, cursor..segment.end));
                    has_content = true;
                    if mandatory && source[..segment.end].ends_with('\n') {
                        ranges.push(line_start..segment.end);
                        line_start = segment.end;
                        line_width = 0.;
                        has_content = false;
                    }
                    continue;
                }
            }
            line_width += full;
            has_content = true;
            if mandatory && source[..segment.end].ends_with('\n') {
                ranges.push(line_start..segment.end);
                line_start = segment.end;
                line_width = 0.;
                has_content = false;
            }
        }
        if line_start < source.len() || ranges.is_empty() || source.ends_with('\n') {
            ranges.push(line_start..source.len());
        }
        let mut lines = Lines {
            lines: Vec::with_capacity(ranges.len()),
            width: px(0.),
            line_height: spec.line_height,
        };
        for range in ranges {
            let line = self.build_line(text, spec, range);
            lines.width = lines.width.max(line.width);
            lines.lines.push(line);
        }
        lines.width = px(f32::from(lines.width).ceil());
        lines
    }

    fn build_line(&self, text: &InlineText, spec: &TextSpec, range: Range<usize>) -> Line {
        let source = text.text.as_str();
        let boxes: Vec<usize> = (0..text.boxes.len())
            .filter(|index| {
                let item = &text.boxes[*index];
                Self::box_in(item.at, self.glued.get(*index).copied().unwrap_or_else(|| item.glued_before()), &range, source.len())
            })
            .collect();
        let mut items = Vec::new();
        let mut x = px(0.);
        let mut position = range.start;
        let mut next_box = 0;
        loop {
            while next_box < boxes.len() && text.boxes[boxes[next_box]].at <= position {
                let index = boxes[next_box];
                let width = spec.box_widths.get(index).copied().unwrap_or_default();
                let at = text.boxes[index].at;
                items.push(Item {
                    kind: ItemKind::Box { index },
                    range: at..at,
                    x,
                    width,
                });
                x += width;
                next_box += 1;
            }
            if position >= range.end {
                break;
            }
            let Some(piece_index) = self.piece_at(position) else {
                position += source[position..].chars().next().map_or(1, char::len_utf8);
                continue;
            };
            let piece = &self.pieces[piece_index];
            let mut end = range.end.min(piece.range.end);
            if let Some(&index) = boxes.get(next_box) {
                end = end.min(text.boxes[index].at);
            }
            let width = piece.x_at(end) - piece.x_at(position);
            items.push(Item {
                kind: ItemKind::Text { piece: piece_index },
                range: position..end,
                x,
                width,
            });
            x += width;
            position = end;
        }
        let content_end = range.start + source[range.clone()].trim_end_matches([' ', '\n']).len();
        let mut line = Line {
            range,
            content_end,
            width: px(0.),
            items,
        };
        let content_width = self.x_in_line(&line, content_end);
        let box_tail = line
            .items
            .iter()
            .filter(|item| {
                matches!(item.kind, ItemKind::Box { .. }) && item.range.start >= content_end
            })
            .fold(px(0.), |sum, item| sum.max(item.x + item.width));
        line.width = content_width.max(box_tail);
        line
    }

    fn x_in_line(&self, line: &Line, index: usize) -> Pixels {
        for item in &line.items {
            if let ItemKind::Text { piece } = item.kind {
                if item.range.start <= index && index <= item.range.end {
                    let piece = &self.pieces[piece];
                    return item.x + piece.x_at(index) - piece.x_at(item.range.start);
                }
            }
        }
        line.items
            .iter()
            .find(|item| item.range.start >= index)
            .map(|item| item.x)
            .or_else(|| line.items.last().map(|item| item.x + item.width))
            .unwrap_or_default()
    }

    fn lines(
        &mut self,
        text: &InlineText,
        spec: &TextSpec,
        mode: WrapMode,
        system: &WindowTextSystem,
    ) -> Rc<Lines> {
        self.prepare(text, spec, system);
        let mode = if spec.wrap {
            mode
        } else {
            WrapMode {
                width: None,
                min_content: false,
            }
        };
        if let Some((_, lines)) = self.results.iter().find(|(cached, _)| *cached == mode) {
            return lines.clone();
        }
        let lines = Rc::new(self.compute(text, spec, mode));
        // A frame's layout asks a block for several widths; keeping fewer than it asks for would
        // line-break every block again on every frame.
        if self.results.len() >= 12 {
            self.results.remove(0);
        }
        self.results.push((mode, lines.clone()));
        lines
    }

    /// The last drawn layout, for hit testing.
    pub fn painted(&self) -> Option<&Painted> {
        self.painted.as_ref()
    }

    pub(crate) fn forget_painted(&mut self) {
        self.painted = None;
    }
}

impl Painted {
    fn line_at(&self, y: Pixels) -> usize {
        let line_height = f32::from(self.lines.line_height).max(1.);
        let index = (f32::from(y - self.bounds.origin.y) / line_height).floor();
        (index.max(0.) as usize).min(self.lines.lines.len().saturating_sub(1))
    }
}

/// Hit testing against a block as it was last drawn.
pub fn index_for_point(text: &InlineText, point: Point<Pixels>) -> Option<usize> {
    let cache = text.cache.borrow();
    let painted = cache.painted.as_ref()?;
    let line_index = painted.line_at(point.y);
    let line = painted.lines.lines.get(line_index)?;
    let x = point.x
        - painted.bounds.origin.x
        - painted.offsets.get(line_index).copied().unwrap_or_default();
    for item in &line.items {
        if x < item.x + item.width {
            return Some(match item.kind {
                ItemKind::Text { piece } => {
                    let piece = &cache.pieces[piece];
                    let local = x - item.x + piece.x_at(item.range.start);
                    piece
                        .closest_index(&item.range, local)
                        .max(item.range.start)
                        .min(item.range.end)
                }
                ItemKind::Box { .. } => item.range.start,
            });
        }
    }
    Some(if x < px(0.) {
        line.range.start
    } else {
        line.content_end
    })
}

/// The text index a point is over when it sits on a glyph or box, for links and cursors.
pub fn index_under_point(text: &InlineText, point: Point<Pixels>) -> Option<usize> {
    let cache = text.cache.borrow();
    let painted = cache.painted.as_ref()?;
    if !painted.bounds.contains(&point) {
        return None;
    }
    let line_index = painted.line_at(point.y);
    let line = painted.lines.lines.get(line_index)?;
    let x = point.x
        - painted.bounds.origin.x
        - painted.offsets.get(line_index).copied().unwrap_or_default();
    let item = line
        .items
        .iter()
        .find(|item| x >= item.x && x < item.x + item.width)?;
    Some(match item.kind {
        ItemKind::Text { piece } => {
            let piece = &cache.pieces[piece];
            let local = x - item.x + piece.x_at(item.range.start);
            let at = piece.xs.partition_point(|(_, glyph_x)| *glyph_x <= local);
            piece
                .xs
                .get(at.saturating_sub(1))
                .map_or(item.range.start, |(index, _)| *index)
                .clamp(
                    item.range.start,
                    item.range.end.saturating_sub(1).max(item.range.start),
                )
        }
        ItemKind::Box { index } => text.boxes[index].at,
    })
}

/// Colours and decorations for one span.
#[derive(Clone, Debug, PartialEq)]
pub struct SpanPaint {
    pub color: Hsla,
    pub underline: Option<Hsla>,
    pub strike: bool,
}

/// Colours by text range, sorted and not overlapping.
pub type ColorRuns = Rc<Vec<(Range<usize>, Hsla)>>;

/// How a block is drawn, apart from where its lines break.
#[derive(Clone, Debug, Default)]
pub struct TextPaint {
    /// One per span.
    pub spans: Vec<SpanPaint>,
    /// Colours that override a span's, by text range, sorted. Code blocks colour their tokens here.
    pub colors: Option<ColorRuns>,
    /// Ranges drawn at a fraction of their opacity, sorted: words still fading in.
    pub fades: Vec<(Range<usize>, f32)>,
    pub selection: Option<(Range<usize>, Hsla)>,
    /// Painted ranges with their background and text colour, under the selection.
    pub highlights: Vec<(Range<usize>, Hsla, Option<Hsla>)>,
    pub code_background: Option<Hsla>,
    /// Glyphs by box index: the SVG asset and its colour.
    pub glyphs: Vec<Option<(SharedString, Hsla)>>,
    /// Text ranges whose hitboxes take the pointing-hand cursor.
    pub links: Vec<Range<usize>>,
    /// What the block is to assistive technology, read around its text: a paragraph or heading.
    pub access: Option<gpui::AccessRole>,
    /// By link index: how assistive technology reads each link, or `None` for plain text.
    pub link_access: Vec<Option<LinkAccess>>,
    /// Makes each live link a stop the keyboard reaches and presses.
    pub link_keys: Option<(LinkKeys, usize)>,
    /// By link index: the hint shown while the pointer rests on it.
    pub link_tips: Vec<Option<SharedString>>,
}

/// Where a message keeps its links' keyboard focus, and what pressing one does.
#[derive(Clone)]
pub struct LinkKeys {
    /// By text block and link index.
    pub focus: Rc<RefCell<std::collections::HashMap<(usize, usize), gpui::FocusHandle>>>,
    pub activate: Rc<dyn Fn(crate::links::LinkTarget, gpui::Modifiers, &mut App)>,
    /// Draws the host's tooltip over a link, given an id unique in the window and its hint.
    pub tip: Option<crate::render::LinkTipSlot>,
    /// Starts the ids the tip zones take, so they differ between messages.
    pub tip_prefix: SharedString,
}

impl std::fmt::Debug for LinkKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LinkKeys")
    }
}

/// A link as assistive technology reads it.
#[derive(Clone, Debug)]
pub struct LinkAccess {
    pub role: gpui::AccessRole,
    /// A name read instead of the link's words.
    pub label: Option<SharedString>,
    pub url: Option<SharedString>,
}

/// A text block drawn by the inline layout above.
pub struct RichText {
    text: Rc<InlineText>,
    spec: Rc<TextSpec>,
    paint: TextPaint,
}

impl RichText {
    pub fn new(text: Rc<InlineText>, spec: Rc<TextSpec>, paint: TextPaint) -> Self {
        Self { text, spec, paint }
    }
}

pub struct RichPrepaint {
    hitbox: Hitbox,
    links: Vec<Hitbox>,
    stops: Vec<gpui::AnyElement>,
    /// Where layout placed the block before its box was rounded to device pixels.
    unrounded_top: Pixels,
}

fn align_offset(align: TextAlign, available: Pixels, width: Pixels) -> Pixels {
    match align {
        TextAlign::Left => px(0.),
        TextAlign::Center => ((available - width) / 2.).max(px(0.)),
        TextAlign::Right => (available - width).max(px(0.)),
    }
}

impl IntoElement for RichText {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for RichText {
    type RequestLayoutState = LayoutId;
    type PrepaintState = RichPrepaint;

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
        let text = self.text.clone();
        let spec = self.spec.clone();
        let layout_id =
            window.request_measured_layout(Style::default(), move |known, available, window, _| {
                let mode = match known.width {
                    Some(width) => WrapMode {
                        width: Some(width),
                        min_content: false,
                    },
                    None => match available.width {
                        gpui::AvailableSpace::Definite(width) => WrapMode {
                            width: Some(width),
                            min_content: false,
                        },
                        gpui::AvailableSpace::MinContent => WrapMode {
                            width: Some(px(0.)),
                            min_content: true,
                        },
                        gpui::AvailableSpace::MaxContent => WrapMode {
                            width: None,
                            min_content: false,
                        },
                    },
                };
                let mut cache = text.cache.borrow_mut();
                let lines = cache.lines(&text, &spec, mode, window.text_system());
                // As CSS shrink-to-fit does, text that had to wrap takes the whole width it wrapped in.
                let width = match mode.width {
                    Some(width) if !mode.min_content && spec.wrap => {
                        let unwrapped = cache.lines(&text, &spec, WrapMode { width: None, min_content: false }, window.text_system());
                        if unwrapped.width > width { lines.width.max(width) } else { lines.width }
                    }
                    _ => lines.width,
                };
                size(width, spec.line_height * lines.lines.len() as f32)
            });
        (layout_id, layout_id)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout_id: &mut LayoutId,
        window: &mut Window,
        cx: &mut App,
    ) -> RichPrepaint {
        // Lines wrap at the width layout measured them in: snapping the box to device pixels can
        // take a fraction off, which must not push a word that fitted onto a line of its own.
        let measured = window.unrounded_layout_bounds(*layout_id).size.width;
        let mode = WrapMode {
            width: Some(bounds.size.width.max(measured)),
            min_content: false,
        };
        let mut cache = self.text.cache.borrow_mut();
        let lines = cache.lines(&self.text, &self.spec, mode, window.text_system());
        let offsets: Vec<Pixels> = lines
            .lines
            .iter()
            .map(|line| align_offset(self.spec.align, bounds.size.width, line.width))
            .collect();
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let mut links = Vec::new();
        if !self.paint.links.is_empty() {
            for (line_index, line) in lines.lines.iter().enumerate() {
                let top = bounds.origin.y + self.spec.line_height * line_index as f32;
                for item in &line.items {
                    let index = match item.kind {
                        ItemKind::Text { .. } => item.range.start,
                        ItemKind::Box { index } => self.text.boxes[index].at,
                    };
                    let inside = self.paint.links.iter().any(|link| {
                        link.contains(&index)
                            || (link.start == index && matches!(item.kind, ItemKind::Box { .. }))
                    });
                    if inside && item.width > px(0.) {
                        let origin = point(bounds.origin.x + offsets[line_index] + item.x, top);
                        links.push(window.insert_hitbox(
                            Bounds::new(origin, size(item.width, self.spec.line_height)),
                            HitboxBehavior::Normal,
                        ));
                    }
                }
            }
        }
        cache.painted = Some(Painted {
            bounds,
            lines,
            offsets,
        });
        drop(cache);
        let unrounded = window.unrounded_layout_origin(*layout_id);
        let stops = self.link_stops(unrounded - bounds.origin, window, cx);
        let unrounded_top = unrounded.y;
        RichPrepaint { hitbox, links, stops, unrounded_top }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut LayoutId,
        prepaint: &mut RichPrepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.set_cursor_style(gpui::CursorStyle::IBeam, &prepaint.hitbox);
        for hitbox in &prepaint.links {
            window.set_cursor_style(gpui::CursorStyle::PointingHand, hitbox);
        }
        if window.is_accessibility_active() {
            self.paint_access(bounds, window);
        }
        self.paint_lines(bounds, prepaint.unrounded_top, window, cx);
        // A focus ring is drawn over the text, as a browser draws an outline.
        for stop in &mut prepaint.stops {
            stop.paint(window, cx);
        }
    }
}

impl RichText {
    fn paint_lines(&self, bounds: Bounds<Pixels>, unrounded_top: Pixels, window: &mut Window, cx: &mut App) {
        let cache = self.text.cache.borrow();
        let Some(painted) = cache.painted.clone() else {
            return;
        };
        let system = window.text_system().clone();
        let strut_id = system.resolve_font(&self.spec.strut);
        let ascent = system.ascent(strut_id, self.spec.size);
        let descent = system.descent(strut_id, self.spec.size).abs();
        let line_height = self.spec.line_height;
        let scale = window.scale_factor();
        let baseline_offset = gpui::line_baseline(line_height, ascent, descent, scale);
        // Lines stack from where layout placed the block, before its box was rounded to device pixels.
        let mask = window.layout_content_mask();
        let first_visible = painted.line_at(mask.origin.y);
        let last_visible = painted.line_at(mask.origin.y + mask.size.height);
        let code_pairs = code_pairs(&self.text);
        for (line_index, line) in painted.lines.lines.iter().enumerate() {
            if line_index < first_visible || line_index > last_visible {
                continue;
            }
            let top = bounds.origin.y + line_height * line_index as f32;
            let left = bounds.origin.x + painted.offsets[line_index];
            let baseline = px((f32::from(unrounded_top + line_height * line_index as f32 + baseline_offset) * scale).round() / scale);

            if let Some(background) = self.paint.code_background {
                for (start_box, end_box) in &code_pairs {
                    let start_at = self.text.boxes[*start_box].at;
                    let end_at = self.text.boxes[*end_box].at;
                    let in_line: Vec<&Item> = line
                        .items
                        .iter()
                        .filter(|item| match item.kind {
                            ItemKind::Box { index } => index == *start_box || index == *end_box,
                            ItemKind::Text { .. } => {
                                item.range.start >= start_at
                                    && item.range.end <= end_at
                                    && !item.range.is_empty()
                            }
                        })
                        .collect();
                    let (Some(first), Some(last)) = (in_line.first(), in_line.last()) else {
                        continue;
                    };
                    let metrics = in_line.iter().find_map(|item| match item.kind {
                        ItemKind::Text { piece } => {
                            Some((cache.pieces[piece].ascent, cache.pieces[piece].descent))
                        }
                        ItemKind::Box { .. } => None,
                    });
                    let (code_ascent, code_descent) =
                        metrics.unwrap_or((ascent * 0.85, descent * 0.85));
                    let rect = Bounds::from_corners(
                        point(left + first.x, baseline - code_ascent - px(2.)),
                        point(left + last.x + last.width, baseline + code_descent + px(2.)),
                    );
                    window.paint_quad(
                        gpui::fill(rect, background).corner_radii(Corners::all(px(5.))),
                    );
                }
            }

            for (range, background, _) in &self.paint.highlights {
                let start = range.start.max(line.range.start);
                let end = range.end.min(line.content_end);
                if start < end {
                    let x0 = cache.x_in_line(line, start);
                    let x1 = cache.x_in_line(line, end);
                    window.paint_quad(gpui::fill(
                        Bounds::from_corners(
                            point(left + x0, top),
                            point(left + x1, top + line_height),
                        ),
                        *background,
                    ));
                }
            }

            if let Some((selected, color)) = &self.paint.selection {
                let start = selected.start.max(line.range.start);
                let end = selected.end.min(line.range.end);
                let covers_break = selected.end > line.content_end
                    && selected.start <= line.content_end
                    && line_index + 1 < painted.lines.lines.len();
                if start < end || covers_break {
                    let x0 = cache.x_in_line(line, start.min(line.content_end));
                    let mut x1 = cache.x_in_line(line, end.min(line.content_end));
                    if covers_break {
                        x1 = x1.max(cache.x_in_line(line, line.content_end)) + self.spec.size * 0.3;
                    }
                    if x1 > x0 {
                        let rect = Bounds::from_corners(
                            point(left + x0, top),
                            point(left + x1, top + line_height),
                        );
                        window.paint_quad(gpui::fill(rect, *color));
                    }
                }
            }

            for item in &line.items {
                match item.kind {
                    ItemKind::Text { piece } => {
                        let piece_ref = &cache.pieces[piece];
                        let font = &self.spec.fonts[piece_ref.span];
                        let paint =
                            self.paint
                                .spans
                                .get(piece_ref.span)
                                .cloned()
                                .unwrap_or(SpanPaint {
                                    color: black(),
                                    underline: None,
                                    strike: false,
                                });
                        let runs = self.runs(item.range.clone(), &font.font, paint.color);
                        let content = &self.text.text[item.range.clone()];
                        if content.trim().is_empty() && paint.underline.is_none() {
                            continue;
                        }
                        let item_baseline = baseline - font.raise;
                        paint_glyphs(
                            window,
                            &system,
                            content,
                            font,
                            &runs,
                            point(left + item.x, item_baseline),
                            mask,
                        );
                        let visible_width = if item.range.end > line.content_end {
                            let end = line.content_end.max(item.range.start);
                            piece_ref.x_at(end) - piece_ref.x_at(item.range.start)
                        } else {
                            item.width
                        };
                        if let Some(underline) = paint.underline.filter(|_| visible_width > px(0.))
                        {
                            let rect = Bounds::new(
                                point(left + item.x, item_baseline + px(3.)),
                                size(visible_width, px(1.)),
                            );
                            window.paint_quad(gpui::fill(rect, underline));
                        }
                        if paint.strike && visible_width > px(0.) {
                            let x_height =
                                system.x_height(system.resolve_font(&font.font), font.size);
                            let rect = Bounds::new(
                                point(left + item.x, item_baseline - x_height / 2. - px(0.5)),
                                size(visible_width, px(1.)),
                            );
                            window.paint_quad(gpui::fill(
                                rect,
                                self.fade_color(paint.color, item.range.start),
                            ));
                        }
                    }
                    ItemKind::Box { index } => {
                        if let Some(Some((path, color))) = self.paint.glyphs.get(index) {
                            let glyph = self.spec.size * 0.85;
                            let bottom = baseline + self.spec.size * 0.08;
                            let rect = Bounds::new(
                                point(left + item.x, bottom - glyph),
                                size(glyph, glyph),
                            );
                            let color = self.fade_color(*color, self.text.boxes[index].at);
                            let _ = window.paint_svg(
                                rect,
                                path.clone(),
                                None,
                                gpui::TransformationMatrix::unit(),
                                color,
                                cx,
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Draws shaped text glyph by glyph from a baseline origin, so `letter-spacing` can space them.
fn paint_glyphs(
    window: &mut Window,
    system: &WindowTextSystem,
    content: &str,
    font: &SpanFont,
    runs: &[TextRun],
    origin: Point<Pixels>,
    mask: Bounds<Pixels>,
) {
    let layout = system.layout_line(content, font.size, runs, None);
    let mut ordinal = 0;
    let mut run_index = 0;
    let mut run_end = runs.first().map_or(usize::MAX, |run| run.len);
    for run in &layout.runs {
        for glyph in &run.glyphs {
            while glyph.index >= run_end && run_index + 1 < runs.len() {
                run_index += 1;
                run_end += runs[run_index].len;
            }
            let x = origin.x + glyph.position.x + font.tracking * ordinal as f32;
            ordinal += 1;
            if x > mask.right() || x + font.size * 2. < mask.left() {
                continue;
            }
            let at = point(x, origin.y);
            let _ = if glyph.is_emoji {
                window.paint_emoji(at, run.font_id, glyph.id, font.size)
            } else {
                window.paint_glyph(
                    at,
                    run.font_id,
                    glyph.id,
                    font.size,
                    runs.get(run_index).map_or(black(), |run| run.color),
                )
            };
        }
    }
}

/// Each code span's opening and closing box.
fn code_pairs(text: &InlineText) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    let mut open = None;
    for (index, item) in text.boxes.iter().enumerate() {
        match item.kind {
            BoxKind::CodeStart => open = Some(index),
            BoxKind::CodeEnd => {
                if let Some(start) = open.take() {
                    pairs.push((start, index));
                }
            }
            BoxKind::Glyph(_) => {}
        }
    }
    pairs
}

impl RichText {
    fn link_focus(&self, link: usize) -> Option<gpui::FocusHandle> {
        let (keys, entry) = self.paint.link_keys.as_ref()?;
        keys.focus.borrow().get(&(*entry, link)).cloned()
    }

    /// Each live link as a keyboard stop laid over its first line: Tab reaches it, Enter presses
    /// it with the modifiers held, as a browser's link. A link with a hint also takes the host's
    /// tooltip over each of its lines, placed against the whole link where layout put it before
    /// rounding (`shift` from the drawn box).
    fn link_stops(&self, shift: Point<Pixels>, window: &mut Window, cx: &mut App) -> Vec<gpui::AnyElement> {
        use gpui::{InteractiveElement, ParentElement, Styled};
        let Some((keys, entry)) = self.paint.link_keys.clone() else { return Vec::new() };
        let mut stops = Vec::new();
        for (link, span) in self.text.links.iter().enumerate() {
            let Some(Some(access)) = self.paint.link_access.get(link) else { continue };
            // The link's own boxes from its glyph on, as the anchor measures them: a commit is a
            // button as tall as its line, any other link its text's boxes.
            let rects = if access.role == gpui::AccessRole::Button { range_rects(&self.text, span.range.clone()) } else { range_text_rects(&self.text, span.range.clone()) };
            let mut rects: Vec<Bounds<Pixels>> = rects.unwrap_or_default().into_iter().filter(|rect| rect.size.width > px(0.)).collect();
            // Its glyph and a code span's padding are the link's too.
            if let Some(height) = rects.first().map(|rect| rect.size.height) {
                for index in (0..self.text.boxes.len()).filter(|index| self.text.boxes[*index].link == Some(link)) {
                    let Some(item) = box_rect(&self.text, index) else { continue };
                    let item = Bounds::new(point(item.origin.x, item.origin.y + (item.size.height - height) / 2.), size(item.size.width, height));
                    match rects.iter_mut().find(|rect| (rect.origin.y - item.origin.y).abs() < px(1.)) {
                        Some(rect) => *rect = rect.union(&item),
                        None => {
                            let at = rects.iter().position(|rect| rect.origin.y > item.origin.y).unwrap_or(rects.len());
                            rects.insert(at, item);
                        }
                    }
                }
            }
            let Some(first) = rects.first().copied() else { continue };
            let focus = keys.focus.borrow_mut().entry((entry, link)).or_insert_with(|| cx.focus_handle().tab_stop(true)).clone();
            let target = span.target.clone();
            let activate = keys.activate.clone();
            if let (Some(slot), Some(Some(tip))) = (&keys.tip, self.paint.link_tips.get(link)) {
                let whole = rects.iter().fold(first, |all, rect| all.union(rect));
                let place = Bounds::new(whole.origin + shift, whole.size);
                for (index, rect) in rects.into_iter().enumerate() {
                    let id = ElementId::Name(format!("{}-{entry}-{link}-{index}", keys.tip_prefix).into());
                    let mut zone = gpui::div().w(rect.size.width).h(rect.size.height).child(slot(id, tip.clone(), place, window, cx)).into_any_element();
                    zone.layout_as_root(rect.size.into(), window, cx);
                    zone.prepaint_at(rect.origin, window, cx);
                    stops.push(zone);
                }
            }
            let mut stop = gpui::div()
                .track_focus(&focus)
                .focus_ring(true)
                .w(first.size.width)
                .h(first.size.height)
                .on_key_down(move |event, _, cx| {
                    if event.keystroke.key == "enter" {
                        cx.stop_propagation();
                        activate(target.clone(), event.keystroke.modifiers, cx);
                    }
                })
                .into_any_element();
            stop.layout_as_root(first.size.into(), window, cx);
            stop.prepaint_at(first.origin, window, cx);
            stops.push(stop);
        }
        stops
    }

    /// Reports the block's text to assistive technology as a browser reads it: runs of plain text,
    /// with code, strong and emphasised runs and links each a node of their own.
    fn paint_access(&self, bounds: Bounds<Pixels>, window: &mut Window) {
        let text = &self.text;
        let access = self.paint.access;
        let paint_runs = |window: &mut Window| {
            let mut runs: Vec<(Range<usize>, crate::parse::SpanStyle, Option<usize>)> = Vec::new();
            for span in &text.spans {
                let link = span.link.filter(|link| self.paint.link_access.get(*link).is_some_and(Option::is_some));
                let style = crate::parse::SpanStyle { strike: false, superscript: false, ..span.style };
                match runs.last_mut() {
                    Some((range, last_style, last_link)) if *last_style == style && *last_link == link && range.end == span.range.start => range.end = span.range.end,
                    _ => runs.push((span.range.clone(), style, link)),
                }
            }
            let mut index = 0;
            while index < runs.len() {
                let link = runs[index].2;
                let mut end = index + 1;
                while link.is_some() && end < runs.len() && runs[end].2 == link {
                    end += 1;
                }
                let group = &runs[index..end];
                match link.and_then(|link| self.paint.link_access[link].as_ref()) {
                    Some(target) => {
                        let range = group[0].0.start..group[group.len() - 1].0.end;
                        let words = text.text.get(range.clone()).unwrap_or_default().trim().to_string();
                        let mut info = gpui::AccessInfo::new(target.role, target.label.clone().unwrap_or_else(|| words.into()));
                        if let Some(url) = &target.url {
                            info = info.url(url.clone());
                        }
                        let focused = link.and_then(|link| self.link_focus(link)).is_some_and(|focus| focus.is_focused(window));
                        let contains = target.role.contains();
                        window.with_access_node(span_bounds(text, range, bounds), info, focused, |window| {
                            // A button reads only its name, as a browser reads a button's contents.
                            for (range, style, _) in group.iter().filter(|_| contains) {
                                access_run(text, range.clone(), *style, bounds, window);
                            }
                        });
                    }
                    None => {
                        for (range, style, _) in group {
                            access_run(text, range.clone(), *style, bounds, window);
                        }
                    }
                }
                index = end;
            }
        };
        match access {
            Some(gpui::AccessRole::ListMarker) => {
                window.push_access_node(None, bounds, gpui::AccessInfo::new(gpui::AccessRole::ListMarker, format!("{} ", text.text.trim())), false);
            }
            Some(gpui::AccessRole::Heading) => {
                window.with_access_node(bounds, gpui::AccessInfo::new(gpui::AccessRole::Heading, text.text.trim().to_string()), false, paint_runs)
            }
            Some(role) => window.with_access_node(bounds, gpui::AccessInfo::new(role, ""), false, paint_runs),
            None => paint_runs(window),
        }
    }

    fn fade_color(&self, color: Hsla, at: usize) -> Hsla {
        match self
            .paint
            .fades
            .iter()
            .find(|(range, _)| range.contains(&at))
        {
            Some((_, alpha)) => Hsla {
                a: color.a * alpha,
                ..color
            },
            None => color,
        }
    }

    /// Runs over `range`, split where an override colour or a fade begins or ends.
    fn runs(&self, range: Range<usize>, font: &Font, base: Hsla) -> Vec<TextRun> {
        let mut cuts = vec![range.start, range.end];
        let overrides: &[(Range<usize>, Hsla)] = self
            .paint
            .colors
            .as_deref()
            .map_or(&[], |colors| colors.as_slice());
        let first = overrides.partition_point(|(color_range, _)| color_range.end <= range.start);
        for (color_range, _) in overrides[first..]
            .iter()
            .take_while(|(color_range, _)| color_range.start < range.end)
        {
            cuts.push(color_range.start.clamp(range.start, range.end));
            cuts.push(color_range.end.clamp(range.start, range.end));
        }
        for (highlight, _, color) in &self.paint.highlights {
            if color.is_some() && highlight.start < range.end && highlight.end > range.start {
                cuts.push(highlight.start.clamp(range.start, range.end));
                cuts.push(highlight.end.clamp(range.start, range.end));
            }
        }
        for (fade, _) in &self.paint.fades {
            if fade.start < range.end && fade.end > range.start {
                cuts.push(fade.start.clamp(range.start, range.end));
                cuts.push(fade.end.clamp(range.start, range.end));
            }
        }
        cuts.sort_unstable();
        cuts.dedup();
        let mut runs = Vec::with_capacity(cuts.len());
        let mut override_at = first;
        for window in cuts.windows(2) {
            let (start, end) = (window[0], window[1]);
            if end <= start {
                continue;
            }
            while override_at < overrides.len() && overrides[override_at].0.end <= start {
                override_at += 1;
            }
            let color = match overrides.get(override_at) {
                Some((color_range, color)) if color_range.start <= start => *color,
                _ => base,
            };
            let color = self
                .paint
                .highlights
                .iter()
                .find_map(|(range, _, color)| {
                    color.filter(|_| range.start <= start && start < range.end)
                })
                .unwrap_or(color);
            let color = self.fade_color(color, start);
            match runs.last_mut() {
                Some(TextRun {
                    len, color: last, ..
                }) if *last == color => *len += end - start,
                _ => runs.push(TextRun {
                    len: end - start,
                    font: font.clone(),
                    color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }),
            }
        }
        runs
    }
}

/// Text layout helpers the renderer needs outside an element.
pub fn measure(
    text: &InlineText,
    spec: &TextSpec,
    width: Option<Pixels>,
    min_content: bool,
    system: &WindowTextSystem,
) -> Size<Pixels> {
    let lines = text
        .cache
        .borrow_mut()
        .lines(text, spec, WrapMode { width, min_content }, system);
    size(lines.width, spec.line_height * lines.lines.len() as f32)
}

/// The widest line, in Chromium's 1/64px layout units rather than whole pixels, as a table's
/// columns are sized.
pub fn measure_width(
    text: &InlineText,
    spec: &TextSpec,
    width: Option<Pixels>,
    min_content: bool,
    system: &WindowTextSystem,
) -> Pixels {
    let lines = text
        .cache
        .borrow_mut()
        .lines(text, spec, WrapMode { width, min_content }, system);
    let widest = lines.lines.iter().map(|line| f32::from(line.width)).fold(0., f32::max);
    px((widest * 64.).ceil() / 64.)
}

/// The rectangles a range covers as last drawn, one per line, in window coordinates.
pub fn range_rects(text: &InlineText, range: Range<usize>) -> Option<Vec<Bounds<Pixels>>> {
    let cache = text.cache.borrow();
    let painted = cache.painted.as_ref()?;
    let line_height = painted.lines.line_height;
    let mut rects = Vec::new();
    for (index, line) in painted.lines.lines.iter().enumerate() {
        let start = range.start.max(line.range.start);
        let end = range.end.min(line.content_end);
        if start >= end && !(range.is_empty() && line.range.contains(&range.start)) {
            continue;
        }
        let left = painted.bounds.origin.x + painted.offsets[index];
        let top = painted.bounds.origin.y + line_height * index as f32;
        let x0 = cache.x_in_line(line, start);
        let x1 = cache.x_in_line(line, end.max(start));
        rects.push(Bounds::from_corners(
            point(left + x0, top),
            point(left + x1, top + line_height),
        ));
    }
    Some(rects)
}

/// Where an inline box was last drawn, as tall as its line.
fn box_rect(text: &InlineText, index: usize) -> Option<Bounds<Pixels>> {
    let cache = text.cache.borrow();
    let painted = cache.painted.as_ref()?;
    let line_height = painted.lines.line_height;
    painted.lines.lines.iter().enumerate().find_map(|(line_index, line)| {
        let item = line.items.iter().find(|item| matches!(item.kind, ItemKind::Box { index: at } if at == index))?;
        let left = painted.bounds.origin.x + painted.offsets[line_index] + item.x;
        let top = painted.bounds.origin.y + line_height * line_index as f32;
        Some(Bounds::new(point(left, top), size(item.width, line_height)))
    })
}

/// `range_rects` cut down to the text's own boxes, as a DOM range measures them: each line keeps
/// the height of its tallest font rather than the line's.
pub fn range_text_rects(text: &InlineText, range: Range<usize>) -> Option<Vec<Bounds<Pixels>>> {
    let rects = range_rects(text, range)?;
    let cache = text.cache.borrow();
    let content = cache.pieces.iter().map(|piece| piece.ascent + piece.descent.abs()).fold(Pixels::ZERO, Pixels::max);
    if content <= Pixels::ZERO {
        return Some(rects);
    }
    Some(
        rects
            .into_iter()
            .map(|rect| {
                let inset = ((rect.size.height - content) / 2.).max(Pixels::ZERO);
                Bounds::from_corners(point(rect.origin.x, rect.origin.y + inset), point(rect.origin.x + rect.size.width, rect.origin.y + rect.size.height - inset))
            })
            .collect(),
    )
}

/// The line and x of an index as last drawn, for callers that draw over a block.
pub fn caret_x(text: &InlineText, index: usize) -> Option<(usize, Pixels)> {
    let cache = text.cache.borrow();
    let painted = cache.painted.as_ref()?;
    let line_index = painted
        .lines
        .lines
        .iter()
        .position(|line| index < line.range.end)
        .unwrap_or(painted.lines.lines.len().saturating_sub(1));
    let line = painted.lines.lines.get(line_index)?;
    Some((
        line_index,
        cache.x_in_line(line, index) + painted.offsets[line_index],
    ))
}

/// Shared handle to a block's layout cache, which `RichText` fills.
pub type SharedCache = Rc<RefCell<LayoutCache>>;

/// Where a range of the block was last drawn, as one rectangle around its lines.
fn span_bounds(text: &InlineText, range: Range<usize>, fallback: Bounds<Pixels>) -> Bounds<Pixels> {
    range_rects(text, range)
        .and_then(|rects| rects.into_iter().reduce(|all, rect| all.union(&rect)))
        .unwrap_or(fallback)
}

/// One styled run of text: static text, inside a code, strong or emphasis node when styled so.
fn access_run(text: &InlineText, range: Range<usize>, style: crate::parse::SpanStyle, block: Bounds<Pixels>, window: &mut Window) {
    let words = text.text.get(range.clone()).unwrap_or_default();
    if words.trim().is_empty() {
        return;
    }
    let bounds = span_bounds(text, range, block);
    let label = gpui::AccessInfo::new(gpui::AccessRole::Label, words.to_string());
    let wrapper = if style.code {
        Some(gpui::AccessRole::Code)
    } else if style.strong {
        Some(gpui::AccessRole::Strong)
    } else if style.emphasis {
        Some(gpui::AccessRole::Emphasis)
    } else {
        None
    };
    match wrapper {
        Some(role) => window.with_access_node(bounds, gpui::AccessInfo::new(role, ""), false, |window| window.push_access_node(None, bounds, label, false)),
        None => window.push_access_node(None, bounds, label, false),
    }
}

/// Where a line may wrap, as Chromium's `LazyLineBreakIterator` finds it: after a run of spaces;
/// between two printable ASCII characters only where its fixed table allows; elsewhere by UAX #14.
/// Each break is its offset and whether it is mandatory.
pub(crate) fn line_breaks(text: &str) -> Vec<(usize, bool)> {
    let unicode: Vec<(usize, bool)> = unicode_linebreak::linebreaks(text)
        .map(|(index, kind)| (index, kind == unicode_linebreak::BreakOpportunity::Mandatory))
        .collect();
    let mut found = Vec::with_capacity(unicode.len());
    let mut chars = text.char_indices().peekable();
    let (mut before, mut last) = ('\0', match chars.next() {
        Some((_, first)) => first,
        None => return unicode,
    });
    for (at, current) in chars {
        let rule = unicode.binary_search_by_key(&at, |(index, _)| *index).ok().map(|found| unicode[found].1);
        let breaks = if rule == Some(true) {
            true
        } else if breakable_space(last) {
            !breakable_space(current)
        } else if breakable_space(current) {
            false
        } else if is_fast_ascii(last) && is_fast_ascii(current) {
            ascii_break(before, last, current)
        } else {
            rule.is_some()
        };
        if breaks {
            found.push((at, rule == Some(true)));
        }
        (before, last) = (last, current);
    }
    found.push((text.len(), true));
    found
}

/// A link's glyph is an atomic inline, so a line may also wrap after it, keeping it with what
/// comes before (`(📄` then `name)`), unless the text there already wraps before it or its next
/// character may not start a line.
fn glyph_breaks(text: &InlineText, spec: &TextSpec, breaks: &mut Vec<(usize, bool)>, glued: &mut [bool]) {
    for (index, item) in text.boxes.iter().enumerate() {
        let drawn = spec.box_widths.get(index).is_some_and(|width| *width > px(0.));
        if !matches!(item.kind, BoxKind::Glyph(_)) || !drawn || item.at == 0 || item.at >= text.text.len() {
            continue;
        }
        let Err(slot) = breaks.binary_search_by_key(&item.at, |(at, _)| *at) else { continue };
        let next = text.text[item.at..].chars().next().unwrap_or(' ');
        let before = text.text[..item.at].chars().next_back().unwrap_or(' ');
        if breakable_space(before) || breakable_space(next) || matches!(next, ')' | ',' | '.' | ':' | ';' | '!' | '?' | ']' | '}') {
            continue;
        }
        breaks.insert(slot, (item.at, false));
        glued[index] = true;
    }
}

fn breakable_space(ch: char) -> bool {
    matches!(ch, ' ' | '\n' | '\t')
}

fn is_fast_ascii(ch: char) -> bool {
    ('!'..='\u{7f}').contains(&ch)
}

/// Chromium's table for two printable ASCII characters (`LineBreakData::FillAscii`), with its
/// hyphen-before-digit rule.
fn ascii_break(before: char, last: char, current: char) -> bool {
    if last == '-' && current.is_ascii_digit() {
        return before.is_ascii_alphanumeric();
    }
    let mut breaks = matches!(current, '(' | '<' | '[' | '{') || matches!(last, '-' | '?');
    if (last == '-' && current == '$') || current == '!' || (last == '?' && matches!(current, '"' | '\'')) {
        breaks = false;
    }
    if matches!(current, ')' | ',' | '.' | '/' | ':' | ';' | '?' | ']' | '}') {
        breaks = false;
    }
    if matches!(last, '$' | '\'' | '(' | '/' | '0'..='9' | '<' | '@' | 'A'..='Z' | '[' | '^' | '_' | '`' | 'a'..='z' | '{' | '\u{7f}') {
        breaks = false;
    }
    breaks
}

#[cfg(test)]
mod tests {
    use super::{glyph_breaks, line_breaks, TextSpec};
    use crate::parse::{BoxKind, GlyphKind, InlineBox, InlineText};
    use gpui::px;

    fn soft(text: &str) -> Vec<&str> {
        let mut start = 0;
        let mut pieces = Vec::new();
        for (at, _) in line_breaks(text) {
            pieces.push(&text[start..at]);
            start = at;
        }
        pieces
    }

    #[test]
    fn breaks_where_chromium_does() {
        assert_eq!(soft("per the ./Scripts/run.sh flow"), ["per ", "the ", "./Scripts/run.sh ", "flow"]);
        assert_eq!(soft(r"real weight.\n\nRollback: the"), ["real ", r"weight.\n\nRollback: ", "the"]);
        assert_eq!(soft("v0.18.0-beta.2 n=-1 ab-12"), ["v0.18.0-", "beta.2 ", "n=-1 ", "ab-", "12"]);
        assert_eq!(soft("call(x) what?yes"), ["call(x) ", "what?", "yes"]);
    }

    #[test]
    fn wraps_after_a_link_glyph_that_follows_a_bracket() {
        let source = "at runtime (catalog.mts) or (,x)";
        let glyph = |at| InlineBox { at, kind: BoxKind::Glyph(GlyphKind::File), link: Some(0) };
        let text = InlineText { text: source.into(), boxes: vec![glyph(3), glyph(12), glyph(29)], ..Default::default() };
        let spec = TextSpec { fonts: Vec::new(), box_widths: vec![px(14.); 3], strut: gpui::font("x"), size: px(17.), line_height: px(29.), wrap: true, align: Default::default() };
        let mut breaks = line_breaks(source);
        let mut glued = vec![false; 3];
        glyph_breaks(&text, &spec, &mut breaks, &mut glued);
        assert_eq!(glued, [false, true, false]);
        assert!(breaks.contains(&(12, false)));
    }
}
