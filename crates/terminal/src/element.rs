//! Paints a screen's visible cells: backgrounds, find highlights, the selection, glyphs, the
//! cursor, text being composed, and xterm's overlay scrollbar.

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::Line;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};
use gpui::{
    fill, outline, point, px, relative, size, App, BorderStyle, Bounds, CursorStyle, DispatchPhase, Element,
    ElementId, ElementInputHandler, Entity, FocusHandle, Font, FontFeatures, FontStyle, FontWeight, GlobalElementId,
    Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PaintQuad, Pixels, Point, ScrollWheelEvent, ShapedLine, SharedString, StrikethroughStyle, Style,
    TextRun, UnderlineStyle, Window,
};

use crate::search::SearchMatch;
use crate::theme::{from_rgb, TerminalFont, TerminalTheme};
use crate::view::{GridLayout, ScrollbarLayout, TerminalView, SCROLLBAR_WIDTH};

/// VS Code's shortest scrollbar slider, which xterm's scrollbar is.
const MIN_SLIDER: f32 = 20.;

pub(crate) struct TerminalElement {
    view: Entity<TerminalView>,
    focus: FocusHandle,
    theme: TerminalTheme,
    font: TerminalFont,
}

impl TerminalElement {
    pub fn new(view: Entity<TerminalView>, focus: FocusHandle, theme: TerminalTheme, font: TerminalFont) -> Self {
        Self { view, focus, theme, font }
    }

    fn font(&self, bold: bool, italic: bool) -> Font {
        Font {
            family: self.font.family.clone(),
            // xterm draws every glyph in its own cell, so ligatures never join them.
            features: FontFeatures(std::sync::Arc::new(vec![("calt".into(), 0), ("liga".into(), 0)])),
            fallbacks: None,
            weight: if bold { FontWeight::BOLD } else { FontWeight::NORMAL },
            style: if italic { FontStyle::Italic } else { FontStyle::Normal },
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum UnderlineKind {
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

#[derive(Clone, PartialEq)]
struct GlyphStyle {
    color: Hsla,
    bold: bool,
    italic: bool,
    underline: Option<(UnderlineKind, Hsla)>,
    strike: bool,
}

struct Glyphs {
    origin: Point<Pixels>,
    line: ShapedLine,
}

/// A run of same-styled single-width cells, shaped as one line at the cell pitch.
struct Batch {
    row: usize,
    col: usize,
    text: String,
    style: GlyphStyle,
}

pub(crate) struct PrepaintState {
    hitbox: Hitbox,
    scrollbar_hitbox: Option<Hitbox>,
    layout: GridLayout,
    quads: Vec<PaintQuad>,
    glyphs: Vec<Glyphs>,
    decorations: Vec<PaintQuad>,
    cursor: Vec<PaintQuad>,
    cursor_glyph: Option<Glyphs>,
    marked: Option<(PaintQuad, Glyphs)>,
    scrollbar: Option<PaintQuad>,
    tracking: bool,
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

fn named(colors: &Colors, theme: &TerminalTheme, index: usize) -> Hsla {
    match colors[index] {
        Some(rgb) => from_rgb(rgb),
        None if index < 256 => theme.indexed(index as u8),
        None => theme.foreground,
    }
}

/// A colour, or `None` for the default background, which stays clear.
fn resolve(colour: Color, colors: &Colors, theme: &TerminalTheme) -> Option<Hsla> {
    Some(match colour {
        Color::Spec(rgb) => from_rgb(rgb),
        Color::Indexed(index) => named(colors, theme, index as usize),
        Color::Named(name) => match name {
            NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::DimForeground => {
                colors[NamedColor::Foreground].map(from_rgb).unwrap_or(theme.foreground)
            }
            NamedColor::Background => return colors[NamedColor::Background].map(from_rgb),
            NamedColor::Cursor => theme.cursor,
            NamedColor::DimBlack
            | NamedColor::DimRed
            | NamedColor::DimGreen
            | NamedColor::DimYellow
            | NamedColor::DimBlue
            | NamedColor::DimMagenta
            | NamedColor::DimCyan
            | NamedColor::DimWhite => named(colors, theme, name as usize - NamedColor::DimBlack as usize),
            _ => named(colors, theme, name as usize),
        },
    })
}

/// A cell's glyph colour and background after bold-brightening, inverse, and dim.
fn cell_colours(cell: &Cell, colors: &Colors, theme: &TerminalTheme) -> (Hsla, Option<Hsla>) {
    let mut fg_colour = cell.fg;
    // drawBoldTextInBrightColors: bold text in one of the first eight colours uses its bright twin.
    if cell.flags.contains(Flags::BOLD) {
        if let Color::Named(name) = fg_colour {
            if (name as usize) < 8 {
                fg_colour = Color::Indexed(name as u8 + 8);
            }
        }
    }
    let mut fg = resolve(fg_colour, colors, theme).unwrap_or(theme.foreground);
    let mut bg = resolve(cell.bg, colors, theme);
    if cell.flags.contains(Flags::INVERSE) {
        let swapped = bg.unwrap_or(Hsla { a: 1., ..theme.background });
        bg = Some(fg);
        fg = swapped;
    }
    if cell.flags.contains(Flags::DIM) {
        fg = fg.opacity(0.5);
    }
    (fg, bg)
}

fn underline(cell: &Cell, fg: Hsla, colors: &Colors, theme: &TerminalTheme) -> Option<(UnderlineKind, Hsla)> {
    let flags = cell.flags;
    let kind = if flags.contains(Flags::DOUBLE_UNDERLINE) {
        UnderlineKind::Double
    } else if flags.contains(Flags::UNDERCURL) {
        UnderlineKind::Curly
    } else if flags.contains(Flags::DOTTED_UNDERLINE) {
        UnderlineKind::Dotted
    } else if flags.contains(Flags::DASHED_UNDERLINE) {
        UnderlineKind::Dashed
    } else if flags.contains(Flags::UNDERLINE) {
        UnderlineKind::Single
    } else {
        return None;
    };
    let colour = cell.underline_color().and_then(|colour| resolve(colour, colors, theme)).unwrap_or(fg);
    Some((kind, colour))
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        style.flex_grow = 1.;
        style.min_size.width = px(0.).into();
        style.min_size.height = px(0.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let theme = &self.theme;
        let font_size = self.font.size;
        let regular = self.font(false, false);
        let text_system = window.text_system().clone();
        let font_id = text_system.resolve_font(&regular);
        let scale = window.scale_factor();
        // xterm measures 'W' and snaps the cell to device pixels: the width down, the height up.
        let advance = text_system.advance(font_id, font_size, 'W').map(|advance| advance.width).unwrap_or(font_size * 0.6);
        let cell_width = px(((f32::from(advance) * scale).floor() / scale).max(1.));
        let ascent = text_system.ascent(font_id, font_size);
        let descent = text_system.descent(font_id, font_size).abs();
        let line_height = px(((f32::from(ascent + descent) * scale).ceil() / scale).max(1.));
        let baseline = ascent;

        let view = self.view.read(cx);
        let focused = self.focus.is_focused(window) && window.is_window_active();
        let screen = view.screen().clone();
        let highlights: Vec<SearchMatch> = view.search.highlights().to_vec();
        let active_match = view.search.active();
        let marked_text = view.marked_text.clone();
        let (scrollbar_opacity, fading) = view.scrollbar_opacity();
        let scrollbar_hovered = view.scrollbar_hovered;
        let scrollbar_active = view.scrollbar_active();

        let term = screen.lock();
        let content = term.renderable_content();
        let cols = term.columns();
        let rows = term.screen_lines();
        let history = term.grid().history_size();
        let display_offset = content.display_offset;
        let colors = content.colors;
        let mode = content.mode;
        let selection = content.selection;
        let cursor = content.cursor;

        let cell_origin = |col: usize, row: usize| {
            point(bounds.origin.x + cell_width * col as f32, bounds.origin.y + line_height * row as f32)
        };
        let cell_bounds = |col: usize, row: usize, width: usize| {
            Bounds::new(cell_origin(col, row), size(cell_width * width as f32, line_height))
        };

        let mut quads = Vec::new();
        let mut batches: Vec<Batch> = Vec::new();
        let mut singles: Vec<(usize, usize, String, GlyphStyle)> = Vec::new();
        let mut decorations = Vec::new();
        let mut background_run: Option<(usize, usize, usize, Hsla)> = None;
        let flush_background = |run: &mut Option<(usize, usize, usize, Hsla)>, quads: &mut Vec<PaintQuad>| {
            if let Some((row, col, width, colour)) = run.take() {
                quads.push(fill(cell_bounds(col, row, width), colour));
            }
        };
        let mut cursor_cell: Option<(char, GlyphStyle, usize)> = None;

        for indexed in content.display_iter {
            let row = (indexed.point.line.0 + display_offset as i32) as usize;
            let col = indexed.point.column.0;
            let cell = indexed.cell;
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let width = if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
            let (fg, bg) = cell_colours(cell, colors, theme);

            match (bg, &mut background_run) {
                (Some(colour), Some((run_row, run_col, run_width, run_colour)))
                    if *run_row == row && *run_col + *run_width == col && *run_colour == colour =>
                {
                    *run_width += width;
                }
                (Some(colour), _) => {
                    flush_background(&mut background_run, &mut quads);
                    background_run = Some((row, col, width, colour));
                }
                (None, _) => flush_background(&mut background_run, &mut quads),
            }

            let style = GlyphStyle {
                color: fg,
                bold: cell.flags.contains(Flags::BOLD),
                italic: cell.flags.contains(Flags::ITALIC),
                underline: underline(cell, fg, colors, theme),
                strike: cell.flags.contains(Flags::STRIKEOUT),
            };
            if indexed.point == cursor.point {
                cursor_cell = Some((cell.c, style.clone(), width));
            }
            if let Some((kind, colour)) = style.underline {
                if matches!(kind, UnderlineKind::Double | UnderlineKind::Dotted | UnderlineKind::Dashed) {
                    let origin = cell_origin(col, row);
                    let y = origin.y + baseline + px(2.);
                    let span = cell_width * width as f32;
                    match kind {
                        UnderlineKind::Double => {
                            decorations.push(fill(Bounds::new(point(origin.x, y - px(1.)), size(span, px(1.))), colour));
                            decorations.push(fill(Bounds::new(point(origin.x, y + px(1.)), size(span, px(1.))), colour));
                        }
                        _ => {
                            let (on, off): (f32, f32) = if kind == UnderlineKind::Dotted { (1., 1.) } else { (3., 2.) };
                            let mut x = 0f32;
                            while x < f32::from(span) {
                                let dash = on.min(f32::from(span) - x);
                                decorations.push(fill(Bounds::new(point(origin.x + px(x), y), size(px(dash), px(1.))), colour));
                                x += on + off;
                            }
                        }
                    }
                }
            }
            let hidden = cell.flags.contains(Flags::HIDDEN);
            let blank = (cell.c == ' ' || cell.c == '\t' || cell.c == '\0') && cell.zerowidth().is_none();
            if hidden || (blank && style.underline.is_none() && !style.strike) {
                continue;
            }
            let ch = if blank { ' ' } else { cell.c };
            if width == 2 || cell.zerowidth().is_some() || !ch.is_ascii() {
                let mut text = String::from(ch);
                text.extend(cell.zerowidth().into_iter().flatten());
                singles.push((row, col, text, style));
                continue;
            }
            match batches.last_mut() {
                Some(batch) if batch.row == row && batch.col + batch.text.len() == col && batch.style == style => {
                    batch.text.push(ch);
                }
                _ => batches.push(Batch { row, col, text: String::from(ch), style }),
            }
        }
        flush_background(&mut background_run, &mut quads);

        // Find highlights, then the selection over them.
        let viewport_top = history - display_offset;
        let span_quads = |found: &SearchMatch, colour: Hsla, quads: &mut Vec<PaintQuad>| {
            let mut col = found.col;
            let mut remaining = found.size;
            let mut row = found.row;
            while remaining > 0 {
                let width = (cols - col).min(remaining);
                if row >= viewport_top && row < viewport_top + rows {
                    quads.push(fill(cell_bounds(col, row - viewport_top, width), colour));
                }
                remaining -= width;
                col = 0;
                row += 1;
            }
        };
        for found in &highlights {
            span_quads(found, theme.find_match, &mut quads);
        }
        if let Some(found) = active_match {
            span_quads(&found, theme.find_active, &mut quads);
        }
        if let Some(range) = selection {
            for row in 0..rows {
                let line = Line(row as i32 - display_offset as i32);
                if line < range.start.line || line > range.end.line {
                    continue;
                }
                let (start, end) = if range.is_block {
                    (range.start.column.0, range.end.column.0)
                } else {
                    let start = if line == range.start.line { range.start.column.0 } else { 0 };
                    let end = if line == range.end.line { range.end.column.0 } else { cols - 1 };
                    (start, end)
                };
                if end >= start {
                    quads.push(fill(cell_bounds(start, row, end - start + 1), theme.selection));
                }
            }
        }

        let shape = |text: String, style: &GlyphStyle, force: Option<Pixels>| {
            let run = TextRun {
                len: text.len(),
                font: self.font(style.bold, style.italic),
                color: style.color,
                background_color: None,
                underline: style.underline.and_then(|(kind, colour)| match kind {
                    UnderlineKind::Single | UnderlineKind::Curly => Some(UnderlineStyle {
                        thickness: px(1.),
                        color: Some(colour),
                        wavy: kind == UnderlineKind::Curly,
                        ..Default::default()
                    }),
                    _ => None,
                }),
                strikethrough: style.strike.then_some(StrikethroughStyle { thickness: px(1.), color: Some(style.color) }),
            };
            text_system.shape_line(SharedString::from(text), font_size, &[run], force)
        };
        let mut glyphs = Vec::with_capacity(batches.len() + singles.len());
        for batch in batches {
            let origin = cell_origin(batch.col, batch.row);
            glyphs.push(Glyphs { origin, line: shape(batch.text, &batch.style, Some(cell_width)) });
        }
        for (row, col, text, style) in singles {
            glyphs.push(Glyphs { origin: cell_origin(col, row), line: shape(text, &style, None) });
        }

        // The cursor: a block when focused, an outline when not, as xterm draws it.
        let mut cursor_quads = Vec::new();
        let mut cursor_glyph = None;
        let cursor_row = cursor.point.line.0 + display_offset as i32;
        let cursor_visible = cursor.shape != CursorShape::Hidden && (0..rows as i32).contains(&cursor_row);
        if cursor_visible {
            let row = cursor_row as usize;
            let col = cursor.point.column.0.min(cols - 1);
            let width = cursor_cell.as_ref().map_or(1, |cell| cell.2);
            let cell = cell_bounds(col, row, width);
            if !focused {
                cursor_quads.push(outline(cell, theme.cursor, BorderStyle::Solid));
            } else {
                match cursor.shape {
                    CursorShape::Beam => cursor_quads.push(fill(Bounds::new(cell.origin, size(px(1.), line_height)), theme.cursor)),
                    CursorShape::Underline => cursor_quads.push(fill(
                        Bounds::new(point(cell.origin.x, cell.bottom() - px(1.)), size(cell.size.width, px(1.))),
                        theme.cursor,
                    )),
                    CursorShape::HollowBlock => cursor_quads.push(outline(cell, theme.cursor, BorderStyle::Solid)),
                    _ => {
                        cursor_quads.push(fill(cell, theme.cursor));
                        if let Some((ch, style, _)) = cursor_cell.filter(|(ch, _, _)| *ch != ' ' && *ch != '\0' && *ch != '\t') {
                            let style = GlyphStyle { color: theme.cursor_accent, ..style };
                            cursor_glyph = Some(Glyphs { origin: cell.origin, line: shape(ch.to_string(), &style, None) });
                        }
                    }
                }
            }
        }

        // Text an input method is composing sits over the cursor.
        let marked = marked_text.filter(|text| !text.is_empty()).map(|text| {
            let row = cursor_row.clamp(0, rows as i32 - 1) as usize;
            let origin = cell_origin(cursor.point.column.0.min(cols - 1), row);
            let style = GlyphStyle {
                color: theme.foreground,
                bold: false,
                italic: false,
                underline: Some((UnderlineKind::Single, theme.foreground)),
                strike: false,
            };
            let line = shape(text, &style, None);
            let backdrop = fill(Bounds::new(origin, size(line.width, line_height)), Hsla { a: 1., ..theme.background });
            (backdrop, Glyphs { origin, line })
        });

        // The scrollbar xterm overlays at the right edge, shown on hover and after a scroll.
        let total = history + rows;
        let scrollbar_layout = (history > 0).then(|| {
            let track = Bounds::new(point(bounds.right() - SCROLLBAR_WIDTH, bounds.origin.y), size(SCROLLBAR_WIDTH, bounds.size.height));
            let representable = f32::from(track.size.height);
            let visible = f32::from(line_height) * rows as f32;
            let scroll = f32::from(line_height) * total as f32;
            let slider = (visible * representable / scroll).floor().max(MIN_SLIDER).min(representable);
            let ratio = if scroll > visible { (representable - slider) / (scroll - visible) } else { 0. };
            let position = (f32::from(line_height) * viewport_top as f32 * ratio).floor();
            let thumb = Bounds::new(point(track.origin.x, track.origin.y + px(position)), size(SCROLLBAR_WIDTH, px(slider)));
            ScrollbarLayout { track, thumb }
        });
        let scrollbar = scrollbar_layout.filter(|_| scrollbar_opacity > 0.).map(|bar| {
            let alpha = if scrollbar_active {
                0.5
            } else if scrollbar_hovered && bar.thumb.contains(&window.mouse_position()) {
                0.4
            } else {
                0.2
            };
            fill(bar.thumb, theme.foreground.opacity(alpha * scrollbar_opacity))
        });
        if fading {
            window.request_animation_frame();
        }

        let tracking = crate::mouse::tracking(mode);
        drop(term);

        let layout = GridLayout { bounds, cell_width, line_height, cols, rows, display_offset, history, scrollbar: scrollbar_layout };
        let available_width = f32::from(bounds.size.width - SCROLLBAR_WIDTH).max(0.);
        let fit = (
            ((available_width / f32::from(cell_width)).floor() as u16).clamp(2, crate::contract::MAX_TERMINAL_DIMENSION),
            ((f32::from(bounds.size.height) / f32::from(line_height)).floor() as u16).clamp(1, crate::contract::MAX_TERMINAL_DIMENSION),
        );
        if bounds.size.width >= px(1.) && bounds.size.height >= px(1.) {
            self.view.update(cx, |view, cx| view.laid_out(layout, fit, cx));
        }

        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let scrollbar_hitbox = scrollbar_layout.map(|bar| window.insert_hitbox(bar.track, HitboxBehavior::Normal));
        PrepaintState {
            hitbox,
            scrollbar_hitbox,
            layout,
            quads,
            glyphs,
            decorations,
            cursor: cursor_quads,
            cursor_glyph,
            marked,
            scrollbar,
            tracking,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let line_height = prepaint.layout.line_height;
        window.handle_input(&self.focus, ElementInputHandler::new(bounds, self.view.clone()), cx);
        let over_scrollbar = prepaint.scrollbar_hitbox.as_ref().is_some_and(|hitbox| hitbox.is_hovered(window));
        let cursor_style = if prepaint.tracking || over_scrollbar { CursorStyle::Arrow } else { CursorStyle::IBeam };
        window.set_cursor_style(cursor_style, &prepaint.hitbox);

        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            for quad in prepaint.quads.drain(..) {
                window.paint_quad(quad);
            }
            for glyphs in &prepaint.glyphs {
                let _ = glyphs.line.paint(glyphs.origin, line_height, window, cx);
            }
            for quad in prepaint.decorations.drain(..) {
                window.paint_quad(quad);
            }
            for quad in prepaint.cursor.drain(..) {
                window.paint_quad(quad);
            }
            if let Some(glyph) = &prepaint.cursor_glyph {
                let _ = glyph.line.paint(glyph.origin, line_height, window, cx);
            }
            if let Some((backdrop, glyphs)) = prepaint.marked.take() {
                window.paint_quad(backdrop);
                let _ = glyphs.line.paint(glyphs.origin, line_height, window, cx);
            }
            if let Some(scrollbar) = prepaint.scrollbar.take() {
                window.paint_quad(scrollbar);
            }
        });

        let hitbox = prepaint.hitbox.clone();
        let scrollbar_hitbox = prepaint.scrollbar_hitbox.clone();
        let view = self.view.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && hitbox.is_hovered(window) {
                view.update(cx, |view, cx| view.mouse_down(event, window, cx));
            }
        });
        let hitbox = prepaint.hitbox.clone();
        let view = self.view.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                let inside = hitbox.is_hovered(window);
                let over_scrollbar = scrollbar_hitbox.as_ref().is_some_and(|hitbox| hitbox.is_hovered(window));
                view.update(cx, |view, cx| view.mouse_move(event, inside, over_scrollbar, cx));
            }
        });
        let view = self.view.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble {
                view.update(cx, |view, cx| view.mouse_up(event, cx));
            }
        });
        let hitbox = prepaint.hitbox.clone();
        let view = self.view.clone();
        window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && hitbox.is_hovered(window) {
                view.update(cx, |view, cx| view.scroll_wheel(event, cx));
                cx.stop_propagation();
            }
        });
    }
}
