//! The block tree as GPUI elements, spaced as `markdown.css` spaces it, margins collapsing between
//! blocks the way they do in the browser.

use std::collections::BTreeMap;
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    canvas, div, img, point, prelude::*, px, relative, size, AnyElement, App, Bounds, ElementId,
    EntityId, FontStyle, FontWeight, Hsla, ImageSource, ObjectFit, PathBuilder, Pixels,
    SharedString, Window,
};

use crate::gutter::OverflowGutter;
use crate::highlight;
use crate::layout::{measure, RichText, SpanFont, SpanPaint, TextAlign, TextPaint, TextSpec};
use crate::links::LinkTarget;
use crate::metrics::*;
use crate::parse::{
    Align, Block, BoxKind, CodeBlock, GlyphKind, InlineText, ListItem, ParaPart, Span, SpanStyle,
    Table,
};
use crate::style::MarkdownStyle;

/// What kind of block a copy button sits on, which decides its size and inset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyKind {
    /// A fenced block that names its language: the button sits in the language strip.
    CodeBlock,
    /// A code block with no language.
    Code,
    Table,
    /// A block the host drew for a fence, such as a diagram.
    Fence,
}

/// What the host's copy button is asked to draw.
#[derive(Clone, Debug)]
pub struct CopyButtonRequest {
    pub id: ElementId,
    /// What the button writes to the clipboard.
    pub text: SharedString,
    /// Its accessible name and tooltip: "Copy the code", "Copy the table", ...
    pub label: SharedString,
    pub kind: CopyKind,
    /// The group of the block it sits on, for `group_hover` styles: hidden until the block is hovered.
    pub group: SharedString,
    /// The newest row shows its buttons as bare glyphs without a hover.
    pub emphasize: bool,
}

/// What the host is asked to draw in place of a fenced block, such as a Mermaid diagram.
#[derive(Clone, Debug)]
pub struct FenceRequest {
    pub id: ElementId,
    pub lang: SharedString,
    pub source: SharedString,
    /// The fence is still being written.
    pub pending: bool,
}

pub type CopyButtonSlot = Rc<dyn Fn(CopyButtonRequest, &mut Window, &mut App) -> AnyElement>;
/// Returns the element and the label of its copy button, or `None` to draw the fence as code.
pub type FenceSlot =
    Rc<dyn Fn(FenceRequest, &mut Window, &mut App) -> Option<(AnyElement, SharedString)>>;

/// Draws the host's scrollbar over a block that scrolls sideways, given the block's scroll handle.
pub type ScrollbarSlot = Rc<dyn Fn(ElementId, &gpui::ScrollHandle, &mut Window, &mut App) -> AnyElement>;

/// A painted range over a message's text, such as a find match or an annotation.
#[derive(Clone, Debug, PartialEq)]
pub struct Highlight {
    pub start: TextPos,
    pub end: TextPos,
    pub background: Hsla,
    /// The text colour inside the range, when it changes.
    pub color: Option<Hsla>,
}

/// A position in a message's text: which text block, and the byte offset in it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TextPos {
    pub block: usize,
    pub offset: usize,
}

/// A text block in render order, which hit testing and copying walk.
#[derive(Clone)]
pub(crate) struct TextEntry {
    pub text: Rc<InlineText>,
    /// What goes between this block and the one before when both are copied.
    pub separator: &'static str,
}

/// The state of a copy the default button made, shown for 1.4s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CopyFeedback {
    pub block: usize,
    pub copied: bool,
    pub at: Instant,
}

/// What the built-in copy button calls: the block's number and the text to write.
pub(crate) type CopyHandler = Rc<dyn Fn(usize, SharedString, &mut Window, &mut App)>;

pub(crate) const COPY_CONFIRM: Duration = Duration::from_millis(1400);

pub(crate) struct Ctx<'a> {
    pub style: &'a MarkdownStyle,
    pub text_system: std::sync::Arc<gpui::WindowTextSystem>,
    pub view: EntityId,
    pub selection: Option<(TextPos, TextPos)>,
    pub selection_color: Hsla,
    pub hovered: Option<(usize, usize)>,
    pub known_commit: Option<&'a dyn Fn(&str) -> bool>,
    pub copy_slot: Option<&'a CopyButtonSlot>,
    pub fence_slot: Option<&'a FenceSlot>,
    pub scrollbar_slot: Option<&'a ScrollbarSlot>,
    pub on_copy: CopyHandler,
    pub feedback: Option<CopyFeedback>,
    pub emphasize_copy: bool,
    pub reveal: &'a BTreeMap<usize, Instant>,
    pub now: Instant,
    pub animate: bool,
    pub entries: Vec<TextEntry>,
    /// How many text entries come before `entries` in the message.
    pub entry_base: usize,
    /// When the next word fade steps, which is when the message must draw again.
    pub next_frame: Option<Duration>,
    pub highlights: &'a [Highlight],
    pub blocks: usize,
    /// Whether the document being drawn is the live tail of a stream.
    pub live: bool,
    /// Where the document being drawn starts in the whole message's source.
    pub doc_offset: usize,
    pub separator: &'static str,
    /// What the next text block is to assistive technology.
    pub next_access: Option<gpui::AccessRole>,
    pub link_keys: Option<crate::layout::LinkKeys>,
}

/// What drawing a message reads besides its blocks, owned so a group drawn later can read it too.
pub(crate) struct Inputs {
    pub style: Rc<MarkdownStyle>,
    pub view: EntityId,
    pub selection: Option<(TextPos, TextPos)>,
    pub selection_color: Hsla,
    pub hovered: Option<(usize, usize)>,
    pub known_commit: Option<Rc<dyn Fn(&str) -> bool>>,
    pub copy_slot: Option<CopyButtonSlot>,
    pub fence_slot: Option<FenceSlot>,
    pub scrollbar_slot: Option<ScrollbarSlot>,
    pub on_copy: CopyHandler,
    pub feedback: Option<CopyFeedback>,
    pub emphasize_copy: bool,
    pub now: Instant,
    pub animate: bool,
    pub highlights: Rc<Vec<Highlight>>,
    pub link_keys: Option<crate::layout::LinkKeys>,
    /// Until when this drawing may lay out groups off screen whose heights are not known yet.
    pub measure_until: std::cell::Cell<Option<Instant>>,
    /// Whether a group asked for another frame already.
    pub asked_frame: std::cell::Cell<bool>,
}

static NO_REVEAL: BTreeMap<usize, Instant> = BTreeMap::new();

impl Inputs {
    /// A context drawing blocks whose words have all shown.
    pub fn ctx<'a>(&'a self, window: &Window) -> Ctx<'a> {
        self.ctx_revealing(window, &NO_REVEAL)
    }

    pub fn ctx_revealing<'a>(&'a self, window: &Window, reveal: &'a BTreeMap<usize, Instant>) -> Ctx<'a> {
        Ctx {
            style: &self.style,
            text_system: window.text_system().clone(),
            view: self.view,
            selection: self.selection,
            selection_color: self.selection_color,
            hovered: self.hovered,
            known_commit: self.known_commit.as_deref(),
            copy_slot: self.copy_slot.as_ref(),
            fence_slot: self.fence_slot.as_ref(),
            scrollbar_slot: self.scrollbar_slot.as_ref(),
            on_copy: self.on_copy.clone(),
            feedback: self.feedback,
            emphasize_copy: self.emphasize_copy,
            reveal,
            now: self.now,
            animate: self.animate,
            entries: Vec::new(),
            entry_base: 0,
            next_frame: None,
            highlights: &self.highlights,
            blocks: 0,
            live: false,
            doc_offset: 0,
            separator: "\n\n",
            next_access: None,
            link_keys: self.link_keys.clone(),
        }
    }
}

/// A block and its collapsible margins: its own, and what its first and last children pass up.
pub(crate) struct Built {
    pub element: AnyElement,
    pub top: f32,
    pub bottom: f32,
    pub inner_top: f32,
    pub inner_bottom: f32,
}

impl Built {
    fn new(element: impl IntoElement, top: f32, bottom: f32) -> Self {
        Self {
            element: element.into_any_element(),
            top,
            bottom,
            inner_top: 0.,
            inner_bottom: 0.,
        }
    }

    fn effective_top(&self) -> f32 {
        self.top.max(self.inner_top)
    }

    fn effective_bottom(&self) -> f32 {
        self.bottom.max(self.inner_bottom)
    }
}

fn spacer(height: f32) -> AnyElement {
    div().flex_none().h(px(height)).into_any_element()
}

/// Stacks blocks with the larger of each adjacent pair of margins between them. Margins at an edge
/// that collapses pass up to the parent; at one that does not, they become space inside it.
pub(crate) fn stack(
    items: Vec<Built>,
    collapse_top: bool,
    collapse_bottom: bool,
) -> (gpui::Div, f32, f32) {
    let mut column = div().flex().flex_col();
    let mut top = 0.;
    let mut bottom = 0.;
    let mut previous: Option<f32> = None;
    for item in items {
        let item_top = item.effective_top();
        match previous {
            None if collapse_top => top = item_top,
            None if item_top > 0. => column = column.child(spacer(item_top)),
            None => {}
            Some(previous) => {
                let gap = previous.max(item_top);
                if gap > 0. {
                    column = column.child(spacer(gap));
                }
            }
        }
        previous = Some(item.effective_bottom());
        column = column.child(item.element);
    }
    if let Some(last) = previous {
        if collapse_bottom {
            bottom = last;
        } else if last > 0. {
            column = column.child(spacer(last));
        }
    }
    (column, top, bottom)
}

/// The font a text block is set in.
#[derive(Clone)]
struct BaseFont {
    family: SharedString,
    size: Pixels,
    weight: f32,
    line_height: Pixels,
    color: Hsla,
    tracking: Pixels,
}

fn make_font(family: &SharedString, weight: f32, italic: bool) -> gpui::Font {
    let mut font = gpui::font(family.clone());
    font.weight = FontWeight(weight);
    font.style = if italic {
        FontStyle::Italic
    } else {
        FontStyle::Normal
    };
    font
}

/// A font's ascent and descent, both positive.
fn strut_metrics(window: &Window, font: &gpui::Font, size: Pixels) -> (Pixels, Pixels) {
    let system = window.text_system();
    let id = system.resolve_font(font);
    (system.ascent(id, size), system.descent(id, size).abs())
}

fn space_width(family: &SharedString, size: Pixels, system: &gpui::WindowTextSystem) -> Pixels {
    let id = system.resolve_font(&gpui::font(family.clone()));
    system
        .advance(id, size, ' ')
        .map(|advance| advance.width)
        .unwrap_or(size * 0.28)
}

/// Chromium's disc, circle and square list markers, drawn rather than set as glyphs.
#[derive(Clone, Copy, PartialEq)]
enum Bullet {
    Disc,
    Circle,
    Square,
}

fn bullet(kind: Bullet, color: Hsla, base: &BaseFont) -> AnyElement {
    let line_height = base.line_height;
    let font = gpui::font(base.family.clone());
    let font_size = base.size;
    let side = (font_size * (4.5 / 15.)).round();
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            window.push_access_node(None, bounds, gpui::AccessInfo::new(gpui::AccessRole::ListMarker, "• "), false);
            let (ascent, descent) = strut_metrics(window, &font, font_size);
            let baseline = bounds.origin.y + (line_height - ascent - descent) / 2. + ascent;
            let center = point(
                bounds.origin.x + side / 2.,
                baseline - font_size * BULLET_RAISE,
            );
            match kind {
                Bullet::Disc => {
                    let rect = Bounds::centered_at(center, size(side, side));
                    window.paint_quad(gpui::fill(rect, color).corner_radii(side / 2.));
                }
                Bullet::Circle => {
                    let outer = side + px(2.);
                    let rect = Bounds::centered_at(center, size(outer, outer));
                    window.paint_quad(
                        gpui::outline(rect, color, gpui::BorderStyle::Solid)
                            .corner_radii(outer / 2.),
                    );
                }
                Bullet::Square => {
                    let rect = Bounds::centered_at(center, size(side, side));
                    window.paint_quad(gpui::fill(rect, color));
                }
            }
        },
    )
    .absolute()
    .top_0()
    .left(-(font_size * BULLET_OFFSET) - side / 2. - px(ITEM_INDENT))
    .w(side)
    .h(line_height)
    .into_any_element()
}

fn fade_alpha(age: Duration) -> f32 {
    let progress = age.as_millis() as f32 / WORD_FADE_MS as f32;
    if progress >= 1. {
        1.
    } else {
        (progress * WORD_FADE_STEPS).floor() / WORD_FADE_STEPS
    }
}

/// Word starts as the stream splits them: wherever non-space follows space, and each text's start.
pub(crate) fn word_starts(text: &str) -> impl Iterator<Item = usize> + '_ {
    let mut previous_space = true;
    text.char_indices().filter_map(move |(index, ch)| {
        let space = ch.is_whitespace();
        let start = previous_space && !space;
        previous_space = space;
        start.then_some(index)
    })
}

impl<'a> Ctx<'a> {
    fn next_id(&mut self, name: &'static str) -> ElementId {
        self.blocks += 1;
        ElementId::NamedInteger(SharedString::new_static(name), self.blocks as u64)
    }

    /// The handle a sideways-scrolling block keeps across draws, and the host's bar over it.
    fn sideways(&self, id: &ElementId, window: &mut Window, cx: &mut App) -> Option<(gpui::ScrollHandle, AnyElement)> {
        let slot = self.scrollbar_slot?;
        let key = SharedString::from(format!("md-scroll-{}-{id}", self.view));
        let handle = window.use_keyed_state(key, cx, |_, _| gpui::ScrollHandle::new()).read(cx).clone();
        let bar = slot(ElementId::NamedChild(Box::new(id.clone()), "bar".into()), &handle, window, cx);
        Some((handle, bar))
    }

    fn group(&self, id: &ElementId) -> SharedString {
        SharedString::from(format!("md-{}-{id}", self.view))
    }

    fn body_font(&self, color: Hsla) -> BaseFont {
        BaseFont {
            family: self.style.ui_font.clone(),
            size: self.style.font_size,
            weight: 400.,
            line_height: self.style.font_size * self.style.line_height,
            color,
            tracking: px(0.),
        }
    }

    fn spec(
        &self,
        text: &InlineText,
        base: &BaseFont,
        wrap: bool,
        align: TextAlign,
        code_block: bool,
    ) -> TextSpec {
        let style = self.style;
        let fonts = text
            .spans
            .iter()
            .map(|span| {
                let s = span.style;
                if s.code {
                    let size = if code_block {
                        base.size
                    } else {
                        base.size * INLINE_CODE_SCALE
                    };
                    SpanFont {
                        font: make_font(&style.mono_font, 400., false),
                        size,
                        raise: px(0.),
                        tracking: px(0.),
                    }
                } else {
                    let weight = if s.strong { STRONG_WEIGHT } else { base.weight };
                    let (size, raise) = if s.superscript {
                        (base.size * 0.83, base.size * 0.33)
                    } else {
                        (base.size, px(0.))
                    };
                    SpanFont {
                        font: make_font(&base.family, weight, s.emphasis),
                        size,
                        raise,
                        tracking: base.tracking,
                    }
                }
            })
            .collect();
        let box_widths = text
            .boxes
            .iter()
            .map(|item| match item.kind {
                BoxKind::CodeStart | BoxKind::CodeEnd => px(INLINE_CODE_PADDING_X),
                BoxKind::Glyph(GlyphKind::File) if style.file_glyph.is_some() => {
                    base.size * (0.85 + 0.2)
                }
                BoxKind::Glyph(GlyphKind::Thread) if style.thread_glyph.is_some() => {
                    base.size * (0.85 + 0.28)
                }
                BoxKind::Glyph(_) => px(0.),
            })
            .collect();
        TextSpec {
            fonts,
            box_widths,
            strut: make_font(&base.family, base.weight, false),
            size: base.size,
            line_height: base.line_height,
            wrap,
            align,
        }
    }

    fn link_paint(
        &self,
        entry: usize,
        text: &InlineText,
        link: usize,
        base: &SpanPaint,
    ) -> (SpanPaint, Option<Hsla>) {
        let style = self.style;
        let hovered = self.hovered == Some((entry, link));
        let target = &text.links[link].target;
        match target {
            LinkTarget::Thread(_) => {
                let color = if hovered {
                    style.accent_soft
                } else {
                    style.ink
                };
                let line = if hovered {
                    color
                } else {
                    style.thread_underline()
                };
                (
                    SpanPaint {
                        color,
                        underline: Some(line),
                        strike: base.strike,
                    },
                    Some(style.accent_text),
                )
            }
            LinkTarget::Commit(hash) => {
                if !self.known_commit.is_some_and(|known| known(hash)) {
                    return (base.clone(), None);
                }
                let color = if hovered {
                    style.link_hover
                } else {
                    style.link
                };
                let line = if hovered { color } else { style.link_line };
                (
                    SpanPaint {
                        color,
                        underline: Some(line),
                        strike: base.strike,
                    },
                    None,
                )
            }
            _ => {
                let color = if hovered {
                    style.accent_soft
                } else {
                    style.accent_text
                };
                let line = if hovered {
                    color
                } else {
                    style.link_underline()
                };
                let text_color = if base.color == style.code_ink {
                    style.code_ink
                } else {
                    color
                };
                (
                    SpanPaint {
                        color: text_color,
                        underline: Some(line),
                        strike: base.strike,
                    },
                    Some(color),
                )
            }
        }
    }

    /// Whether a link renders as a link: a commit hash only once the checkout knows it.
    fn link_live(&self, text: &InlineText, link: usize) -> bool {
        match &text.links[link].target {
            LinkTarget::Commit(hash) => self.known_commit.is_some_and(|known| known(hash)),
            _ => true,
        }
    }

    /// How assistive technology reads a link, as the browser reads the element drawing it.
    fn link_access(&self, text: &InlineText, link: usize) -> Option<crate::layout::LinkAccess> {
        if !self.link_live(text, link) {
            return None;
        }
        let (role, label, url) = match &text.links[link].target {
            LinkTarget::Commit(hash) => (gpui::AccessRole::Button, Some(format!("View commit {hash}").into()), None),
            LinkTarget::Web(url) | LinkTarget::Other(url) => (gpui::AccessRole::Link, None, Some(SharedString::from(url.clone()))),
            LinkTarget::File(_) | LinkTarget::Thread(_) | LinkTarget::Image(_) => (gpui::AccessRole::Link, None, None),
        };
        Some(crate::layout::LinkAccess { role, label, url })
    }

    /// Registers a text block for selection and builds its element.
    fn text_block(
        &mut self,
        text: &Rc<InlineText>,
        base: BaseFont,
        wrap: bool,
        align: TextAlign,
        code: Option<&CodeBlock>,
    ) -> AnyElement {
        let entry = self.entry_base + self.entries.len();
        let separator = std::mem::replace(&mut self.separator, "\n\n");
        self.entries.push(TextEntry {
            text: text.clone(),
            separator,
        });
        let style = self.style;
        let spec = self.spec(text, &base, wrap, align, code.is_some());
        let mut glyphs = vec![None; text.boxes.len()];
        let mut spans = Vec::with_capacity(text.spans.len());
        for span in &text.spans {
            let s = span.style;
            let color = if s.code {
                style.code_ink
            } else if s.strong {
                style.ink
            } else {
                base.color
            };
            let plain = SpanPaint {
                color,
                underline: None,
                strike: s.strike,
            };
            let paint = match span.link {
                Some(link) if self.link_live(text, link) => {
                    let mut paint = self.link_paint(entry, text, link, &plain).0;
                    // A known commit hash inside a link is still drawn as a commit.
                    let code = &text.text[span.range.clone()];
                    if s.code
                        && !matches!(text.links[link].target, LinkTarget::Commit(_))
                        && crate::links::is_commit_hash(code)
                        && self.known_commit.is_some_and(|known| known(code))
                    {
                        paint.color = style.link;
                    }
                    paint
                }
                _ => plain,
            };
            spans.push(paint);
        }
        for (index, item) in text.boxes.iter().enumerate() {
            if let (BoxKind::Glyph(kind), Some(link)) = (item.kind, item.link) {
                let path = match kind {
                    GlyphKind::File => style.file_glyph.clone(),
                    GlyphKind::Thread => style.thread_glyph.clone(),
                };
                let base_paint = SpanPaint {
                    color: base.color,
                    underline: None,
                    strike: false,
                };
                if let (Some(path), (_, Some(color))) =
                    (path, self.link_paint(entry, text, link, &base_paint))
                {
                    glyphs[index] = Some((path, color));
                }
            }
        }
        let links = text
            .links
            .iter()
            .enumerate()
            .filter(|(index, _)| self.link_live(text, *index))
            .map(|(_, link)| link.range.clone())
            .collect();
        let selection = self.selection.and_then(|(start, end)| {
            if entry < start.block || entry > end.block {
                return None;
            }
            let from = if entry == start.block {
                start.offset
            } else {
                0
            };
            let to = if entry == end.block {
                end.offset
            } else {
                text.text.len()
            };
            let reaches_next = entry < end.block;
            (from < to || (reaches_next && from <= to)).then(|| {
                (
                    from.min(text.text.len())..to.min(text.text.len()) + usize::from(reaches_next),
                    self.selection_color,
                )
            })
        });
        let colors = code.and_then(|code| self.code_colors(code));
        let highlights = self
            .highlights
            .iter()
            .filter(|highlight| highlight.start.block <= entry && entry <= highlight.end.block)
            .map(|highlight| {
                let from = if highlight.start.block == entry {
                    highlight.start.offset
                } else {
                    0
                };
                let to = if highlight.end.block == entry {
                    highlight.end.offset
                } else {
                    text.text.len()
                };
                (
                    from.min(text.text.len())..to.min(text.text.len()),
                    highlight.background,
                    highlight.color,
                )
            })
            .filter(|(range, _, _)| !range.is_empty())
            .collect();
        let fades = if self.live && self.animate {
            self.fades(text)
        } else {
            Vec::new()
        };
        let paint = TextPaint {
            spans,
            colors,
            fades,
            selection,
            highlights,
            code_background: (code.is_none()
                && text
                    .boxes
                    .iter()
                    .any(|item| item.kind == BoxKind::CodeStart))
            .then_some(style.inline_code_background),
            glyphs,
            links,
            access: self.next_access.take(),
            link_access: (0..text.links.len()).map(|link| self.link_access(text, link)).collect(),
            link_keys: self.link_keys.clone().filter(|_| !text.links.is_empty()).map(|keys| (keys, entry)),
        };
        RichText::new(text.clone(), Rc::new(spec), paint).into_any_element()
    }

    fn code_colors(&self, code: &CodeBlock) -> Option<crate::layout::ColorRuns> {
        if !self.style.highlight_code_blocks {
            return None;
        }
        let lang = code.lang.as_deref()?;
        let mut cached = code.highlight.borrow_mut();
        let tokens = cached.get_or_insert_with(|| highlight::highlight(&code.code.text, lang));
        let tokens = tokens.as_ref()?;
        let plain = self.style.code_ink;
        Some(Rc::new(
            tokens
                .iter()
                .map(|(range, role)| (range.clone(), self.style.syntax.color(*role, plain)))
                .collect(),
        ))
    }

    fn fades(&mut self, text: &InlineText) -> Vec<(Range<usize>, f32)> {
        let mut fades = Vec::new();
        let starts: Vec<usize> = word_starts(&text.text).collect();
        for (index, start) in starts.iter().enumerate() {
            let source = self.doc_offset + text.source_offset(*start);
            let Some(seen) = self.reveal.get(&source) else {
                continue;
            };
            let age = self.now.saturating_duration_since(*seen);
            let alpha = fade_alpha(age);
            if alpha < 1. {
                let end = starts.get(index + 1).copied().unwrap_or(text.text.len());
                fades.push((*start..end, alpha));
                let step = Duration::from_millis(WORD_FADE_MS) / WORD_FADE_STEPS as u32;
                let next = step
                    .saturating_mul((age.as_nanos() / step.as_nanos()) as u32 + 1)
                    .saturating_sub(age);
                self.next_frame = Some(self.next_frame.map_or(next, |current| current.min(next)));
            }
        }
        fades
    }

    fn copy_button(
        &mut self,
        id: ElementId,
        text: String,
        label: &str,
        kind: CopyKind,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        if text.is_empty() {
            return None;
        }
        let group = self.group(&id);
        let (top, right) = match kind {
            CopyKind::CodeBlock => (COPY_CODE_TOP, COPY_CODE_RIGHT),
            CopyKind::Table => (COPY_INSET_TABLE_TOP, COPY_INSET),
            CopyKind::Code | CopyKind::Fence => (COPY_INSET, COPY_INSET),
        };
        let button = match self.copy_slot {
            Some(slot) => slot(
                CopyButtonRequest {
                    id: id.clone(),
                    text: text.into(),
                    label: SharedString::from(label.to_string()),
                    kind,
                    group,
                    emphasize: self.emphasize_copy,
                },
                window,
                cx,
            ),
            None => self.default_copy_button(id, text.into(), label, kind, group),
        };
        Some(
            div()
                .absolute()
                .top(px(top))
                .right(px(right))
                .child(button)
                .into_any_element(),
        )
    }

    fn default_copy_button(
        &mut self,
        id: ElementId,
        text: SharedString,
        label: &str,
        kind: CopyKind,
        group: SharedString,
    ) -> AnyElement {
        let colors = &self.style.copy_button;
        let block = self.blocks;
        let feedback = self.feedback.filter(|feedback| {
            feedback.block == block
                && self.now.saturating_duration_since(feedback.at) < COPY_CONFIRM
        });
        let size = if kind == CopyKind::CodeBlock {
            COPY_CODE_SIZE
        } else {
            COPY_SIZE
        };
        let on_copy = self.on_copy.clone();
        let (glyph_color, border) = match feedback {
            Some(CopyFeedback { copied: true, .. }) => (colors.copied, colors.copied_border),
            Some(_) => (colors.failed, colors.failed_border),
            None => (colors.glyph, colors.border),
        };
        let icon = match feedback {
            Some(CopyFeedback { copied: true, .. }) => {
                glyph(colors.check_glyph.clone(), Glyph::Check, glyph_color)
            }
            Some(_) => glyph(colors.x_glyph.clone(), Glyph::X, glyph_color),
            None => glyph(colors.copy_glyph.clone(), Glyph::Copy, glyph_color),
        };
        let mut button = div()
            .id(id)
            .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, label.to_string()))
            .size(px(size))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.))
            .border_1()
            .border_color(border)
            .bg(colors.fill)
            .text_color(glyph_color)
            .cursor_pointer()
            .child(icon)
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                on_copy(block, text.clone(), window, cx);
            });
        if feedback.is_none() {
            let hover_border = colors.border_hover;
            let hover_glyph = colors.glyph_hover;
            let block_glyph = colors.glyph_block_hover;
            let block_border = self.style.line_strong;
            let fill = colors.fill;
            button = if self.emphasize_copy {
                // The newest row shows a bare glyph at rest; the tile comes up with the pointer.
                button
                    .border_color(gpui::transparent_black())
                    .bg(gpui::transparent_black())
                    .text_color(self.style.quiet)
            } else {
                button.invisible()
            };
            button = button
                .group_hover(group, move |style| {
                    style
                        .visible()
                        .bg(fill)
                        .border_color(block_border)
                        .text_color(block_glyph)
                })
                .hover(move |style| style.border_color(hover_border).text_color(hover_glyph));
        }
        button.into_any_element()
    }

    /// Paragraph content: text, and web images on lines of their own.
    fn parts(&mut self, parts: &[ParaPart], base: &BaseFont, checkbox: Option<bool>, access: Option<gpui::AccessRole>) -> AnyElement {
        let mut column = div().flex().flex_col();
        if let Some(role) = access {
            column = column.accessible(gpui::AccessInfo::new(role, ""));
        }
        for (index, part) in parts.iter().enumerate() {
            let element = match part {
                ParaPart::Text(text) => {
                    self.text_block(text, base.clone(), true, TextAlign::Left, None)
                }
                ParaPart::Image { src, alt } => image(src, alt, base),
            };
            column = match (index, checkbox) {
                (0, Some(checked)) => column.child(
                    div()
                        .flex()
                        .flex_row()
                        .child(self.checkbox(checked, base))
                        .child(div().flex_1().min_w_0().child(element)),
                ),
                _ => column.child(element),
            };
        }
        if let Some(checked) = checkbox.filter(|_| parts.is_empty()) {
            column = column.child(div().flex().flex_row().child(self.checkbox(checked, base)));
        }
        column.into_any_element()
    }

    /// A task item's box, sitting on the first line's baseline, then the space after it.
    fn checkbox(&self, checked: bool, base: &BaseFont) -> AnyElement {
        let style = self.style;
        let (fill, edge, tick) = if checked {
            (
                style.checkbox_checked,
                style.checkbox_checked,
                style.checkbox_tick,
            )
        } else {
            (style.checkbox, style.checkbox_border, style.checkbox_tick)
        };
        let line_height = base.line_height;
        let font = gpui::font(base.family.clone());
        let font_size = base.size;
        canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let (ascent, descent) = strut_metrics(window, &font, font_size);
                let baseline = bounds.origin.y + (line_height - ascent - descent) / 2. + ascent;
                let side = px(CHECKBOX_SIZE);
                let rect = Bounds::new(point(bounds.origin.x, baseline - side), size(side, side));
                window.paint_quad(
                    gpui::fill(rect, fill)
                        .corner_radii(px(2.5))
                        .border_widths(px(1.))
                        .border_color(edge),
                );
                if checked {
                    let mut path = PathBuilder::stroke(px(1.6));
                    let at =
                        |x: f32, y: f32| point(rect.origin.x + side * x, rect.origin.y + side * y);
                    path.move_to(at(0.22, 0.5));
                    path.line_to(at(0.42, 0.7));
                    path.line_to(at(0.78, 0.3));
                    if let Ok(path) = path.build() {
                        window.paint_path(path, tick);
                    }
                }
            },
        )
        .flex_none()
        .w(px(CHECKBOX_SIZE + CHECKBOX_GAP)
            + space_width(&base.family, base.size, &self.text_system))
        .h(line_height)
        .into_any_element()
    }

    pub fn blocks(
        &mut self,
        blocks: &[Block],
        color: Hsla,
        in_quote: bool,
        depth: usize,
        window: &mut Window,
        cx: &mut App,
    ) -> Vec<Built> {
        blocks
            .iter()
            .map(|block| self.block(block, color, in_quote, depth, None, window, cx))
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn block(
        &mut self,
        block: &Block,
        color: Hsla,
        in_quote: bool,
        depth: usize,
        checkbox: Option<bool>,
        window: &mut Window,
        cx: &mut App,
    ) -> Built {
        let body = self.body_font(color);
        match block {
            Block::Paragraph(parts) => {
                let bottom = if in_quote { 0. } else { PARAGRAPH_BOTTOM };
                Built::new(self.parts(parts, &body, checkbox, Some(gpui::AccessRole::Paragraph)), 0., bottom)
            }
            Block::Plain(parts) => Built::new(self.parts(parts, &body, checkbox, None), 0., 0.),
            Block::Heading { level, content } => {
                let scale = HEADING_SCALE
                    .get(usize::from(*level).saturating_sub(1))
                    .copied()
                    .unwrap_or(1.);
                let size = self.style.font_size * scale;
                let base = BaseFont {
                    family: self.style.ui_font.clone(),
                    size,
                    weight: HEADING_WEIGHT,
                    line_height: size * HEADING_LINE_HEIGHT,
                    color: self.style.ink,
                    tracking: size * HEADING_TRACKING,
                };
                self.next_access = Some(gpui::AccessRole::Heading);
                Built::new(
                    self.text_block(content, base, true, TextAlign::Left, None),
                    HEADING_TOP,
                    HEADING_BOTTOM,
                )
            }
            Block::Quote(children) => {
                let inner = self.blocks(children, self.style.muted_strong, true, depth, window, cx);
                let (column, _, _) = stack(inner, false, false);
                let quote = div()
                    .accessible(gpui::AccessInfo::new(gpui::AccessRole::Group, ""))
                    .border_l(px(QUOTE_RULE))
                    .border_color(self.style.line_strong)
                    .pt(px(QUOTE_PADDING_Y))
                    .pb(px(QUOTE_PADDING_Y))
                    .pl(px(QUOTE_PADDING_LEFT))
                    .child(column);
                Built::new(quote, QUOTE_MARGIN, QUOTE_MARGIN)
            }
            Block::List {
                start,
                task_list,
                items,
            } => self.list(
                *start, *task_list, items, color, in_quote, depth, window, cx,
            ),
            Block::Code(code) => self.code(code, window, cx),
            Block::Table(table) => self.table(table, color, window, cx),
            Block::Rule => Built::new(
                div().h(px(1.)).bg(self.style.line),
                RULE_MARGIN,
                RULE_MARGIN,
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn list(
        &mut self,
        start: Option<u64>,
        task_list: bool,
        items: &[ListItem],
        color: Hsla,
        in_quote: bool,
        depth: usize,
        window: &mut Window,
        cx: &mut App,
    ) -> Built {
        let mut built_items = Vec::with_capacity(items.len());
        let body = self.body_font(color);
        let space = space_width(&body.family, body.size, &self.text_system);
        for (index, item) in items.iter().enumerate() {
            if index > 0 || depth > 0 {
                self.separator = "\n";
            }
            let mut children = Vec::with_capacity(item.blocks.len());
            for (child_index, block) in item.blocks.iter().enumerate() {
                let checkbox = if child_index == 0 { item.checked } else { None };
                let mut built = self.block(block, color, in_quote, depth + 1, checkbox, window, cx);
                if matches!(block, Block::List { .. }) {
                    built.top = NESTED_LIST_TOP;
                    built.bottom = 0.;
                }
                children.push(built);
            }
            if let Some(checked) = item.checked.filter(|_| item.blocks.is_empty()) {
                children.push(Built::new(self.parts(&[], &body, Some(checked), None), 0., 0.));
            }
            let (column, inner_top, inner_bottom) = stack(children, true, true);
            let mut li = div()
                .accessible(gpui::AccessInfo::new(gpui::AccessRole::ListEntry, ""))
                .relative()
                .pl(px(ITEM_INDENT));
            if item.checked.is_none() {
                match start {
                    Some(first) => {
                        let marker = format!("{}.", first + index as u64);
                        let text = Rc::new(InlineText {
                            spans: vec![Span {
                                range: 0..marker.len(),
                                style: SpanStyle::default(),
                                link: None,
                            }],
                            text: marker,
                            ..InlineText::default()
                        });
                        let spec = Rc::new(self.spec(&text, &body, false, TextAlign::Left, false));
                        let paint = TextPaint {
                            spans: vec![SpanPaint {
                                color,
                                underline: None,
                                strike: false,
                            }],
                            access: Some(gpui::AccessRole::ListMarker),
                            ..TextPaint::default()
                        };
                        li = li.child(
                            div()
                                .absolute()
                                .top_0()
                                .right(relative(1.))
                                .pr(space)
                                .child(RichText::new(text, spec, paint)),
                        );
                    }
                    None => {
                        let kind = match depth {
                            0 => Bullet::Disc,
                            1 => Bullet::Circle,
                            _ => Bullet::Square,
                        };
                        li = li.child(bullet(kind, color, &body));
                    }
                }
            }
            // The marker comes first, as assistive technology reads it; it is drawn out of the flow.
            li = li.child(column.min_h(body.line_height));
            built_items.push(Built {
                element: li.into_any_element(),
                top: ITEM_MARGIN,
                bottom: ITEM_MARGIN,
                inner_top,
                inner_bottom,
            });
        }
        self.separator = "\n\n";
        let (column, inner_top, inner_bottom) = stack(built_items, true, true);
        let indent = if task_list {
            TASK_LIST_INDENT
        } else {
            LIST_INDENT
        };
        Built {
            element: column.accessible(gpui::AccessInfo::new(gpui::AccessRole::List, "")).pl(px(indent)).into_any_element(),
            top: 0.,
            bottom: LIST_BOTTOM,
            inner_top,
            inner_bottom,
        }
    }

    fn code(&mut self, code: &CodeBlock, window: &mut Window, cx: &mut App) -> Built {
        let id = self.next_id("md-code");
        let source = &code.code.text;
        let fence = code
            .lang
            .as_deref()
            .zip(self.fence_slot)
            .filter(|(lang, _)| !(*lang == "visual" && source.len() > 1_000_000));
        if let Some((lang, slot)) = fence {
            let request = FenceRequest {
                id: id.clone(),
                lang: lang.to_string().into(),
                source: source.clone().into(),
                pending: self.live,
            };
            if let Some((element, label)) = slot(request, window, cx) {
                self.entries.push(TextEntry {
                    text: Rc::new(InlineText::default()),
                    separator: "\n\n",
                });
                let copied = if self.live {
                    String::new()
                } else {
                    source.clone()
                };
                let button =
                    self.copy_button(id.clone(), copied, &label, CopyKind::Fence, window, cx);
                let group = self.group(&id);
                let wrapper = div()
                    .relative()
                    .group(group)
                    .child(element)
                    .children(button);
                return Built::new(wrapper, BLOCK_MARGIN, BLOCK_MARGIN);
            }
        }
        let base = BaseFont {
            family: self.style.mono_font.clone(),
            size: px(CODE_FONT_SIZE),
            weight: 400.,
            line_height: px(CODE_FONT_SIZE * CODE_LINE_HEIGHT),
            color: self.style.code_ink,
            tracking: px(0.),
        };
        let measured = self.spec(&code.code, &base, false, TextAlign::Left, true);
        let content = measure(&code.code, &measured, None, false, &window.text_system().clone()).width + px(2. * CODE_PADDING_X);
        let text = self.text_block(&code.code, base, false, TextAlign::Left, Some(code));
        let mut pre = div()
            .id(ElementId::NamedChild(Box::new(id.clone()), "scroll".into()))
            .overflow_x_scroll()
            .px(px(CODE_PADDING_X))
            .py(px(CODE_PADDING_Y))
            .flex()
            // A line wider than the block keeps its length, so the block has something to scroll.
            .child(div().flex_none().min_w_full().flex().flex_col().items_start().child(text));
        pre.style().restrict_scroll_to_axis = Some(true);
        let sideways = self.sideways(&id, window, cx);
        if let Some((handle, _)) = &sideways {
            pre = pre.track_scroll(handle);
        }
        let pre = div()
            .relative()
            .child(pre)
            .child(OverflowGutter::new(content, px(SCROLLBAR_THICKNESS)))
            .children(sideways.map(|(_, bar)| bar));
        let group = self.group(&id);
        let copied = if self.live {
            String::new()
        } else {
            source.clone()
        };
        let wrapper = match code.lang.as_deref() {
            Some(lang) => {
                let head = div()
                    .flex()
                    .items_center()
                    .h(px(CODE_HEAD_HEIGHT))
                    .px(px(CODE_HEAD_PADDING_X))
                    .border_b_1()
                    .border_color(self.style.line_soft)
                    .text_color(self.style.quiet)
                    .font_family(self.style.mono_font.clone())
                    .text_size(px(CODE_HEAD_FONT_SIZE))
                    .line_height(px(CODE_HEAD_FONT_SIZE))
                    .child(SharedString::from(lang.to_string()));
                let button = self.copy_button(
                    id.clone(),
                    copied,
                    "Copy the code",
                    CopyKind::CodeBlock,
                    window,
                    cx,
                );
                div()
                    .relative()
                    .group(group)
                    .rounded(px(BLOCK_RADIUS))
                    .bg(self.style.surface)
                    .overflow_hidden()
                    .child(head)
                    .child(pre)
                    .children(button)
            }
            None => {
                let button = self.copy_button(
                    id.clone(),
                    copied,
                    "Copy the code",
                    CopyKind::Code,
                    window,
                    cx,
                );
                div()
                    .relative()
                    .group(group)
                    .child(pre.rounded(px(BLOCK_RADIUS)).overflow_hidden().bg(self.style.surface))
                    .children(button)
            }
        };
        Built::new(wrapper, BLOCK_MARGIN, BLOCK_MARGIN)
    }

    fn cell_font(&self, head: bool, color: Hsla) -> BaseFont {
        let size = if head {
            self.style.font_size * TABLE_HEAD_SCALE
        } else {
            self.style.font_size
        };
        BaseFont {
            family: self.style.ui_font.clone(),
            size,
            weight: if head { TABLE_HEAD_WEIGHT } else { 400. },
            line_height: size * self.style.line_height,
            color: if head { self.style.muted_strong } else { color },
            tracking: px(0.),
        }
    }

    /// Column widths the way a table's auto layout finds them: each column's widest and narrowest
    /// fit, border box included.
    fn columns(&self, table: &Table, color: Hsla, window: &mut Window) -> ColumnCache {
        let key = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            self.style.ui_font.hash(&mut hasher);
            self.style.mono_font.hash(&mut hasher);
            f32::from(self.style.font_size).to_bits().hash(&mut hasher);
            self.style.line_height.to_bits().hash(&mut hasher);
            self.style.file_glyph.is_some().hash(&mut hasher);
            self.style.thread_glyph.is_some().hash(&mut hasher);
            hasher.finish()
        };
        if let Some(cached) = table
            .columns
            .borrow()
            .as_ref()
            .filter(|cached| cached.key == key)
        {
            return cached.clone();
        }
        let count = table.aligns.len().max(table.head.len());
        let mut max = vec![0f32; count];
        let mut min = vec![0f32; count];
        let system = window.text_system().clone();
        let rows =
            std::iter::once((true, &table.head)).chain(table.rows.iter().map(|row| (false, row)));
        for (head, row) in rows {
            let base = self.cell_font(head, color);
            for (column, cell) in row.iter().enumerate().take(count) {
                let spec = self.spec(cell, &base, true, TextAlign::Left, false);
                let right = if head && column + 1 == count {
                    LAST_HEAD_CELL_PADDING_RIGHT
                } else {
                    CELL_PADDING_X
                };
                let padding = CELL_PADDING_X + right;
                let widest = f32::from(measure(cell, &spec, None, false, &system).width) + padding;
                let narrowest =
                    f32::from(measure(cell, &spec, Some(px(0.)), true, &system).width) + padding;
                max[column] = max[column].max(widest);
                min[column] = min[column].max(narrowest);
            }
        }
        let columns = ColumnCache { key, max, min };
        *table.columns.borrow_mut() = Some(columns.clone());
        columns
    }

    fn table(&mut self, table: &Table, color: Hsla, window: &mut Window, cx: &mut App) -> Built {
        let id = self.next_id("md-table");
        let columns = self.columns(table, color, window);
        let count = columns.max.len();
        let total_max: f32 = columns.max.iter().sum();
        let total_min: f32 = columns.min.iter().sum();
        let mut body = div().flex().flex_col().w_full().min_w(px(total_min));
        let row_count = table.rows.len();
        let align = |column: usize| match table.aligns.get(column) {
            Some(Align::Center) => TextAlign::Center,
            Some(Align::Right) => TextAlign::Right,
            _ => TextAlign::Left,
        };
        let rows =
            std::iter::once((true, &table.head)).chain(table.rows.iter().map(|row| (false, row)));
        for (row_index, (head, row)) in rows.enumerate() {
            let base = self.cell_font(head, color);
            let last_row = !head && row_index == row_count;
            let mut cells = div().flex().flex_row().w_full();
            for column in 0..count {
                let separator = if column == 0 {
                    if row_index == 0 {
                        "\n\n"
                    } else {
                        "\n"
                    }
                } else {
                    "\t"
                };
                self.separator = separator;
                let content = match row.get(column) {
                    Some(cell) => self.text_block(cell, base.clone(), true, align(column), None),
                    None => div().into_any_element(),
                };
                let right = if head && column + 1 == count {
                    LAST_HEAD_CELL_PADDING_RIGHT
                } else {
                    CELL_PADDING_X
                };
                let (widest, narrowest) = (columns.max[column], columns.min[column]);
                let mut cell = div()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .flex_basis(px(narrowest))
                    .max_w(px(widest))
                    .pl(px(CELL_PADDING_X))
                    .pr(px(right))
                    .py(px(CELL_PADDING_Y))
                    .child(content);
                // Room past every column's narrowest goes to each in proportion to how much wider it
                // could be, as a table's auto layout shares it.
                cell.style().flex_grow = Some(widest - narrowest);
                cell.style().flex_shrink = Some(0.);
                cells = cells.child(cell);
            }
            // Rules and the header fill go on the row, so no seam shows where cells meet.
            if !last_row {
                cells = cells.border_b_1().border_color(if head {
                    self.style.line
                } else {
                    self.style.line_soft
                });
            }
            if head {
                cells = cells.bg(self.style.surface).rounded_t(px(BLOCK_RADIUS - 1.));
            }
            body = body.child(cells);
        }
        self.separator = "\n\n";
        let mut frame = div()
            .id(ElementId::NamedChild(Box::new(id.clone()), "scroll".into()))
            .w_full()
            .border_1()
            .border_color(self.style.line)
            .rounded(px(BLOCK_RADIUS))
            .overflow_x_scroll()
            .child(body)
            .child(OverflowGutter::new(px(total_min), px(SCROLLBAR_THICKNESS)));
        frame.style().restrict_scroll_to_axis = Some(true);
        let sideways = self.sideways(&id, window, cx);
        if let Some((handle, _)) = &sideways {
            frame = frame.track_scroll(handle);
        }
        let frame = div().relative().child(frame).children(sideways.map(|(_, bar)| bar));
        let copied = if self.live {
            String::new()
        } else {
            table.source.clone()
        };
        let button = self.copy_button(
            id.clone(),
            copied,
            "Copy the table",
            CopyKind::Table,
            window,
            cx,
        );
        let group = self.group(&id);
        // The block hugs a narrow table so its button sits at the table's edge, not the column's.
        let hug = div()
            .relative()
            .group(group)
            .w(px(total_max + 2.))
            .max_w_full()
            .child(frame)
            .children(button);
        let wrapper = div().w_full().child(hug);
        Built::new(wrapper, BLOCK_MARGIN, BLOCK_MARGIN)
    }
}

/// Measured column widths of a table, kept with it.
#[derive(Clone, Debug, Default)]
pub struct ColumnCache {
    key: u64,
    max: Vec<f32>,
    min: Vec<f32>,
}

fn image(src: &str, alt: &str, base: &BaseFont) -> AnyElement {
    let alt: SharedString = alt.to_string().into();
    let color = base.color;
    let source: ImageSource = SharedString::from(src.to_string()).into();
    img(source)
        .max_w_full()
        .object_fit(ObjectFit::ScaleDown)
        .with_fallback(move || {
            div()
                .text_color(color)
                .child(alt.clone())
                .into_any_element()
        })
        .into_any_element()
}

#[derive(Clone, Copy)]
enum Glyph {
    Copy,
    Check,
    X,
}

/// A 14px Lucide glyph: the host's SVG when it gave one, otherwise drawn with paths.
fn glyph(asset: Option<SharedString>, kind: Glyph, color: Hsla) -> AnyElement {
    if let Some(path) = asset {
        return gpui::svg()
            .path(path)
            .size(px(14.))
            .text_color(color)
            .into_any_element();
    }
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let unit = bounds.size.width / 24.;
            let at = |x: f32, y: f32| point(bounds.origin.x + unit * x, bounds.origin.y + unit * y);
            let mut path = PathBuilder::stroke(unit * 2.);
            match kind {
                Glyph::Copy => {
                    let rect = Bounds::from_corners(at(7., 7.), at(23., 23.));
                    window.paint_quad(
                        gpui::outline(rect, color, gpui::BorderStyle::Solid)
                            .border_widths(unit * 2.)
                            .corner_radii(unit * 3.),
                    );
                    path.move_to(at(4., 16.));
                    path.curve_to(at(2., 14.), at(2., 16.));
                    path.line_to(at(2., 4.));
                    path.curve_to(at(4., 2.), at(2., 2.));
                    path.line_to(at(14., 2.));
                    path.curve_to(at(16., 4.), at(16., 2.));
                }
                Glyph::Check => {
                    path.move_to(at(20., 6.));
                    path.line_to(at(9., 17.));
                    path.line_to(at(4., 12.));
                }
                Glyph::X => {
                    path.move_to(at(18., 6.));
                    path.line_to(at(6., 18.));
                    path.move_to(at(6., 6.));
                    path.line_to(at(18., 18.));
                }
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
        },
    )
    .size(px(14.))
    .into_any_element()
}
