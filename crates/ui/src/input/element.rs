//! The element that lays out, paints and hit-tests a `TextInput`'s text.

use std::ops::Range;

use gpui::{
    div, fill, point, prelude::FluentBuilder as _, px, relative, size, AnyElement, AnyView, App, AvailableSpace, Bounds, ContentMask, Context, Element,
    ElementId, ElementInputHandler, Entity, GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, PaintQuad, Pixels,
    Point, Render, SharedString, Size, Style, StyleRefinement, Styled as _, TextRun, UnderlineStyle, Window, WrappedLine,
};

use super::TextInput;
use crate::theme::ActiveTheme;

#[derive(Clone, Debug)]
struct Row {
    line: usize,
    /// Byte offsets into the whole text.
    start: usize,
    end: usize,
    /// Where the row begins along its unwrapped line.
    start_x: Pixels,
    y: Pixels,
}

/// The text as last drawn: its shaped lines and visual rows, and where they sit in the window.
#[derive(Clone, Debug)]
pub struct InputLayout {
    lines: Vec<WrappedLine>,
    line_starts: Vec<usize>,
    rows: Vec<Row>,
    line_height: Pixels,
    /// The window position of the text's top-left corner, scroll included.
    origin: Point<Pixels>,
    visible: Bounds<Pixels>,
    content: Size<Pixels>,
    /// The placeholder is showing, so every position maps to offset 0.
    placeholder: bool,
}

impl InputLayout {
    fn build(lines: Vec<WrappedLine>, line_starts: Vec<usize>, line_height: Pixels, placeholder: bool) -> Self {
        let mut rows = Vec::new();
        let mut width = px(0.0);
        for (index, line) in lines.iter().enumerate() {
            let base = line_starts[index];
            let unwrapped = &line.unwrapped_layout;
            let mut starts: Vec<usize> = vec![0];
            for boundary in line.wrap_boundaries.iter() {
                let glyph = &unwrapped.runs[boundary.run_ix].glyphs[boundary.glyph_ix];
                starts.push(glyph.index);
            }
            for (row_index, local_start) in starts.iter().enumerate() {
                let local_end = starts.get(row_index + 1).copied().unwrap_or(line.len());
                let start_x = unwrapped.x_for_index(*local_start);
                let end_x = unwrapped.x_for_index(local_end);
                width = width.max(end_x - start_x);
                rows.push(Row {
                    line: index,
                    start: base + local_start,
                    end: base + local_end,
                    start_x,
                    y: line_height * rows.len() as f32,
                });
            }
        }
        if rows.is_empty() {
            rows.push(Row { line: 0, start: 0, end: 0, start_x: px(0.0), y: px(0.0) });
        }
        let content = size(width, line_height * rows.len() as f32);
        Self {
            lines,
            line_starts,
            rows,
            line_height,
            origin: Point::default(),
            visible: Bounds::default(),
            content,
            placeholder,
        }
    }

    pub fn line_height(&self) -> Pixels {
        self.line_height
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub fn content_size(&self) -> Size<Pixels> {
        self.content
    }

    pub fn visible_size(&self) -> Size<Pixels> {
        self.visible.size
    }

    pub fn row_range(&self, row: usize) -> Range<usize> {
        let index = row.min(self.rows.len() - 1);
        let row = &self.rows[index];
        if self.placeholder {
            return 0..0;
        }
        // A wrapped row's end is the next row's start; the caret for "end of row" sits before
        // the wrap's trailing space rather than on the next row.
        let last_of_line = self.rows.get(index + 1).is_none_or(|next| next.line != row.line);
        if last_of_line || row.end == row.start {
            row.start..row.end
        } else {
            let text = &self.lines[row.line].text;
            let local_end = row.end - self.line_starts[row.line];
            let previous = text[..local_end].char_indices().next_back().map(|(index, _)| index).unwrap_or(0);
            row.start..(self.line_starts[row.line] + previous).max(row.start)
        }
    }

    pub fn row_for_offset(&self, offset: usize) -> usize {
        if self.placeholder {
            return 0;
        }
        // Rows are in text order; an offset on a wrap belongs to the row it starts.
        self.rows.partition_point(|row| row.start <= offset).saturating_sub(1)
    }

    /// The caret position for an offset, relative to the text's top-left corner.
    pub fn position_for_offset(&self, offset: usize) -> Point<Pixels> {
        if self.placeholder {
            return Point::default();
        }
        let row_index = self.row_for_offset(offset);
        let row = &self.rows[row_index];
        let line = &self.lines[row.line];
        let local = offset.saturating_sub(self.line_starts[row.line]).min(line.len());
        point(line.unwrapped_layout.x_for_index(local) - row.start_x, row.y)
    }

    /// The offset nearest to `x` on a visual row.
    pub fn offset_for_row_x(&self, row_index: usize, x: Pixels) -> usize {
        if self.placeholder {
            return 0;
        }
        let row = &self.rows[row_index.min(self.rows.len() - 1)];
        let line = &self.lines[row.line];
        let base = self.line_starts[row.line];
        let local = line.unwrapped_layout.closest_index_for_x(x.max(px(0.0)) + row.start_x) + base;
        let range = self.row_range(row_index);
        local.clamp(range.start, range.end)
    }

    /// The offset under a window position.
    pub fn offset_for_point(&self, position: Point<Pixels>) -> usize {
        let local = position - self.origin;
        let row = if local.y < px(0.0) {
            0
        } else {
            ((local.y / self.line_height).floor() as usize).min(self.rows.len() - 1)
        };
        self.offset_for_row_x(row, local.x)
    }

    /// The window rectangle a range covers on its first row, for the IME's candidate window.
    pub fn bounds_for_range(&self, range: Range<usize>) -> Bounds<Pixels> {
        let start = self.position_for_offset(range.start);
        let end_row = self.row_for_offset(range.end);
        let end = if end_row == self.row_for_offset(range.start) {
            self.position_for_offset(range.end)
        } else {
            point(self.content.width, start.y)
        };
        Bounds::from_corners(self.origin + start, self.origin + point(end.x, start.y + self.line_height))
    }

    /// The text's rows as assistive technology reads them: each row's characters and where they
    /// sit, a line's break counted at the end of its last row.
    fn access_runs(&self, text: &str) -> Vec<gpui::AccessTextRun> {
        if self.placeholder || text.is_empty() {
            let bounds = Bounds::new(self.origin, size(px(1.0), self.line_height));
            return vec![gpui::AccessTextRun { range: 0..0, bounds, positions: Vec::new(), widths: Vec::new() }];
        }
        self.rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let line = &self.lines[row.line];
                let base = self.line_starts[row.line];
                let last_of_line = self.rows.get(index + 1).is_none_or(|next| next.line != row.line);
                let mut end = row.end;
                if last_of_line && text[end..].starts_with('\n') {
                    end += 1;
                }
                // Glyph lefts in text order, walked once: a character's left is that of the first
                // glyph at or after it, else the line's end.
                let layout = &line.unwrapped_layout;
                let glyphs: Vec<(usize, Pixels)> = layout
                    .runs
                    .iter()
                    .flat_map(|run| run.glyphs.iter().map(|glyph| (glyph.index, glyph.position.x)))
                    .collect();
                let mut cursor = glyphs.partition_point(|(index, _)| *index < row.start - base);
                let mut left_of = |local: usize| {
                    while cursor < glyphs.len() && glyphs[cursor].0 < local {
                        cursor += 1;
                    }
                    glyphs.get(cursor).map(|(_, x)| *x).unwrap_or(layout.width) - row.start_x
                };
                let mut positions = Vec::new();
                let mut widths = Vec::new();
                for (at, ch) in text[row.start..end].char_indices() {
                    let local = row.start + at - base;
                    let x = left_of(local);
                    let next = if ch == '\n' { x } else { left_of(local + ch.len_utf8()) };
                    positions.push(f32::from(x));
                    widths.push(f32::from((next - x).max(px(0.0))));
                }
                let width = line.unwrapped_layout.x_for_index(row.end - base) - row.start_x;
                gpui::AccessTextRun {
                    range: row.start..end,
                    bounds: Bounds::new(self.origin + point(px(0.0), row.y), size(width.max(px(1.0)), self.line_height)),
                    positions,
                    widths,
                }
            })
            .collect()
    }

    fn selection_rects(&self, range: Range<usize>) -> Vec<Bounds<Pixels>> {
        if range.is_empty() || self.placeholder {
            return Vec::new();
        }
        let first = self.row_for_offset(range.start);
        let last = self.row_for_offset(range.end);
        let mut rects = Vec::new();
        for index in first..=last {
            let row = &self.rows[index];
            let line = &self.lines[row.line];
            let base = self.line_starts[row.line];
            let from = range.start.max(row.start) - base;
            let to = range.end.min(row.end) - base;
            let x0 = line.unwrapped_layout.x_for_index(from) - row.start_x;
            let mut x1 = line.unwrapped_layout.x_for_index(to) - row.start_x;
            // A selected line break shows as a sliver past the row's end, as browsers draw it.
            let ends_line = index + 1 < self.rows.len() && self.rows[index + 1].line != row.line;
            if ends_line && range.end > row.end {
                x1 += self.line_height * 0.3;
            }
            if x1 > x0 {
                rects.push(Bounds::from_corners(
                    self.origin + point(x0, row.y),
                    self.origin + point(x1, row.y + self.line_height),
                ));
            }
        }
        rects
    }
}

/// The caret, drawn as a live view: a blink paints only the caret while the views around it replay
/// their last frame.
pub struct Caret {
    visible: bool,
}

impl Caret {
    pub(crate) fn new() -> Caret {
        Caret { visible: true }
    }

    pub(crate) fn visible(&self) -> bool {
        self.visible
    }

    pub(crate) fn show(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible != visible {
            self.visible = visible;
            cx.notify();
        }
    }
}

impl Render for Caret {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let color = window.text_style().color;
        div().size_full().when(self.visible, |caret| caret.bg(color))
    }
}

pub struct TextInputElement {
    input: Entity<TextInput>,
}

impl TextInputElement {
    pub fn new(input: Entity<TextInput>) -> Self {
        Self { input }
    }
}

pub struct Prepaint {
    layout: InputLayout,
    selections: Vec<PaintQuad>,
    caret: Option<AnyElement>,
}

impl IntoElement for TextInputElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

struct Shaping {
    text: SharedString,
    runs: Vec<TextRun>,
    font_size: Pixels,
    line_height: Pixels,
    placeholder: bool,
}

fn shaping(input: &TextInput, window: &Window, cx: &App) -> Shaping {
    let style = window.text_style();
    let font_size = style.font_size.to_pixels(window.rem_size());
    let line_height = window.line_height();
    let content = input.text();
    let placeholder = content.is_empty();
    let (text, color): (SharedString, Hsla) = if placeholder {
        (input.placeholder_text().clone(), cx.theme().tokens.quiet.into())
    } else {
        (SharedString::from(content.to_string()), style.color)
    };
    let base = TextRun {
        len: text.len(),
        font: style.font(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let runs = match input.buffer().marked().filter(|_| !placeholder) {
        Some(marked) => [
            TextRun { len: marked.start, ..base.clone() },
            TextRun {
                len: marked.end - marked.start,
                underline: Some(UnderlineStyle { color: Some(color), thickness: px(1.0), ..Default::default() }),
                ..base.clone()
            },
            TextRun { len: text.len() - marked.end, ..base },
        ]
        .into_iter()
        .filter(|run| run.len > 0)
        .collect(),
        None => vec![base],
    };
    Shaping { text, runs, font_size, line_height, placeholder }
}

fn split_lines(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (index, ch) in text.char_indices() {
        if ch == '\n' {
            starts.push(index + 1);
        }
    }
    starts
}

/// The field's clip, a pixel wider: the caret may sit on its right edge.
fn caret_clip(bounds: Bounds<Pixels>) -> Bounds<Pixels> {
    Bounds::new(bounds.origin, size(bounds.size.width + px(1.0), bounds.size.height))
}

impl Element for TextInputElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

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
        let input = self.input.read(cx);
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let line_height = window.line_height();
        if !input.is_multiline() {
            style.size.height = line_height.into();
            return (window.request_layout(style, [], cx), ());
        }
        let Shaping { text, runs, font_size, line_height, .. } = shaping(input, window, cx);
        let autosize = input.autosize();
        let layout_id = window.request_measured_layout(style, move |known, available, window, _cx| {
            let width = known.width.or(match available.width {
                AvailableSpace::Definite(width) => Some(width),
                _ => None,
            });
            let rows = window
                .text_system()
                .shape_text(text.clone(), font_size, &runs, width, None)
                .map(|lines| lines.iter().map(|line| line.wrap_boundaries.len() + 1).sum::<usize>())
                .unwrap_or(1);
            let mut height = line_height * rows.max(autosize.min_rows) as f32;
            if let Some(max) = autosize.max_height {
                height = height.min(max.max(line_height));
            }
            size(width.unwrap_or(px(0.0)), height)
        });
        (layout_id, ())
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
        let input = self.input.read(cx);
        let multiline = input.is_multiline();
        let Shaping { text, runs, font_size, line_height, placeholder } = shaping(input, window, cx);
        let wrap = multiline.then_some(bounds.size.width);
        let lines: Vec<WrappedLine> = window
            .text_system()
            .shape_text(text.clone(), font_size, &runs, wrap, None)
            .map(|lines| lines.into_iter().collect())
            .unwrap_or_default();
        let line_starts = split_lines(&text);
        let mut layout = InputLayout::build(lines, line_starts, line_height, placeholder);
        let selection = input.selection();
        let cursor = input.cursor();
        // Chromium lays a field's selection over it at 80%.
        let selection_color = cx.theme().tokens.text_selection;
        let selection_color = gpui::Rgba { a: selection_color.a * 0.8, ..selection_color };
        let shows_caret = selection.is_empty() && !input.is_disabled() && input.focus_handle.is_focused(window) && window.is_window_active();
        let caret_view = input.caret();

        let scroll = self.input.update(cx, |input, _| input.settle_scroll(&layout, bounds.size, (font_size * 1.2).min(line_height)));
        layout.origin = bounds.origin - scroll;
        layout.visible = bounds;
        let shown = self.input.read(cx).shown_bounds(bounds);
        let max = size(px(0.0), (layout.content_size().height - bounds.size.height).max(px(0.0)));
        self.input.read(cx).scroll_model().publish(shown, max);

        let selections = layout
            .selection_rects(selection.clone())
            .into_iter()
            .map(|rect| fill(rect, selection_color))
            .collect();
        let caret = shows_caret.then(|| {
            let position = layout.position_for_offset(cursor);
            let caret_height = (font_size * 1.2).min(line_height);
            let top = position.y + (line_height - caret_height) / 2.0;
            let mut style = StyleRefinement::default();
            style.size.width = Some(px(1.0).into());
            style.size.height = Some(caret_height.into());
            let mut caret = AnyView::from(caret_view).cached(style).live().into_any_element();
            window.with_content_mask(Some(ContentMask { bounds: caret_clip(shown) }), |window| {
                caret.prepaint_as_root(layout.origin + point(position.x, top), size(px(1.0), caret_height).map(AvailableSpace::Definite), window, cx);
            });
            caret
        });
        Prepaint { layout, selections, caret }
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        let focused = focus_handle.is_focused(window);
        let window_active = window.is_window_active();
        {
            let input = self.input.read(cx);
            let role = if input.is_multiline() { gpui::AccessRole::MultilineTextInput } else { gpui::AccessRole::TextInput };
            let text = input.text();
            let selection = input.selection();
            let focus = input.cursor();
            let anchor = if focus == selection.start { selection.end } else { selection.start };
            let access = gpui::AccessInfo::new(role, input.access_label().clone())
                .value(input.placeholder_text().clone())
                .disabled(input.is_disabled())
                ;
            // Laying out every character costs, so only while something reads the window.
            let access = if window.is_accessibility_active() {
                access.text(gpui::AccessText {
                    text: SharedString::from(text.to_string()),
                    runs: prepaint.layout.access_runs(text),
                    anchor,
                    focus,
                })
            } else {
                access.value(text.to_string())
            };
            window.push_access_node(id, bounds, access, focused);
        }
        window.handle_input(&focus_handle, ElementInputHandler::new(bounds, self.input.clone()), cx);
        let layout = &prepaint.layout;
        let shown = self.input.read(cx).shown_bounds(bounds);
        window.with_content_mask(Some(ContentMask { bounds: caret_clip(shown) }), |window| {
            for selection in prepaint.selections.drain(..) {
                window.paint_quad(selection);
            }
            let mut y = layout.origin.y;
            for line in &layout.lines {
                let rows = line.wrap_boundaries.len() + 1;
                let height = layout.line_height * rows as f32;
                if y + height >= shown.top() && y <= shown.bottom() {
                    let _ = line.paint(
                        point(layout.origin.x, y),
                        layout.line_height,
                        gpui::TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                }
                y += height;
            }
            if let Some(caret) = prepaint.caret.as_mut() {
                caret.paint(window, cx);
            }
        });
        let layout = prepaint.layout.clone();
        self.input.update(cx, |input, cx| {
            input.set_layout(layout);
            input.sync_focus(focused, window_active, cx);
        });
    }
}
