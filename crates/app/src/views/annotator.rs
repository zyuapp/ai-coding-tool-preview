//! Marking up a screenshot before it is sent: boxes with notes and arrows, drawn over the image in
//! a dialog over the window (ImageAnnotator.tsx, annotate/marks.ts, annotate/use-annotator.ts).

use std::path::PathBuf;
use std::sync::Arc;

use act_core::domain::conversation::{ImageAnnotation, ImageAnnotationKind};
use act_ui::components::button::{toggle_button, Button, ButtonSize, ButtonVariant, IconButton, IconButtonSize, IconButtonVariant};
use act_ui::input::{InputEvent, TextInput};
use act_ui::overlay::{bounds_cell, bounds_probe, BoundsCell};
use act_ui::style::{glass, Backdrop, GlassKind};
use act_ui::theme::ActiveTheme;
use act_ui::tooltip::shortcut_tip;
use act_ui::IconName;
use gpui::{Focusable, canvas, div, img, point, prelude::*, px, AnyElement, App, Bounds, Context, Corners as QuadCorners, Entity, FocusHandle, FontWeight, Hsla, KeyDownEvent, ObjectFit, PathBuilder, Pixels, Point, RenderImage, Rgba, SharedString, Subscription, Window};

use super::root::Root;

const MARK_COLOR: u32 = 0xff453a;
const MIN_SIZE: f32 = 0.012;
const MIN_LENGTH: f32 = 0.03;

fn mark_color() -> Rgba {
    gpui::rgb(MARK_COLOR)
}

/// The marks a screenshot carries in the composer, and how it looks with them drawn in.
#[derive(Clone, Default)]
pub struct ImageMarks {
    pub annotations: Vec<ImageAnnotation>,
    pub preview: Option<Arc<RenderImage>>,
}

#[derive(Clone, Copy, PartialEq)]
struct Rect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

fn annotation(kind: ImageAnnotationKind, rect: Rect, text: String) -> ImageAnnotation {
    ImageAnnotation { kind, x: rect.x as f64, y: rect.y as f64, width: rect.width as f64, height: rect.height as f64, text }
}

fn rect_of(shape: &ImageAnnotation) -> Rect {
    Rect { x: shape.x as f32, y: shape.y as f32, width: shape.width as f32, height: shape.height as f32 }
}

fn normalize(start: (f32, f32), end: (f32, f32)) -> Rect {
    Rect { x: start.0.min(end.0), y: start.1.min(end.1), width: (end.0 - start.0).abs(), height: (end.1 - start.1).abs() }
}

fn arrow_between(start: (f32, f32), end: (f32, f32)) -> Rect {
    Rect { x: start.0, y: start.1, width: end.0 - start.0, height: end.1 - start.1 }
}

pub struct Annotator {
    pub image_id: String,
    path: PathBuf,
    natural: Option<(f32, f32)>,
    prefix: String,
    tool: ImageAnnotationKind,
    shapes: Vec<ImageAnnotation>,
    past: Vec<Vec<ImageAnnotation>>,
    hovered: Option<usize>,
    /// Set once the keyboard walks the marks, which rings the one it is on.
    walked: bool,
    discarding: bool,
    pending: Option<Rect>,
    editing: Option<usize>,
    label: Entity<TextInput>,
    _label: Subscription,
    origin: Option<(f32, f32)>,
    draft: Option<ImageAnnotation>,
    frame: BoundsCell,
    stage: BoundsCell,
    focus: FocusHandle,
    focused: bool,
}

impl Annotator {
    fn composing(&self) -> bool {
        self.pending.is_some() || self.editing.is_some()
    }

    fn picked(&self) -> Option<usize> {
        match self.hovered {
            Some(index) if index < self.shapes.len() => Some(index),
            _ => self.shapes.len().checked_sub(1),
        }
    }

    fn change(&mut self, next: Vec<ImageAnnotation>) {
        self.past.push(std::mem::replace(&mut self.shapes, next));
        self.hovered = None;
        self.discarding = false;
    }

    fn undo(&mut self) {
        if let Some(previous) = self.past.pop() {
            self.shapes = previous;
            self.hovered = None;
        }
    }

    fn written(&self, cx: &App) -> Option<Vec<ImageAnnotation>> {
        let text = self.label.read(cx).text().trim().to_string();
        if let Some(index) = self.editing {
            let shape = self.shapes.get(index)?;
            return (shape.text != text).then(|| self.shapes.iter().enumerate().map(|(at, shape)| if at == index { ImageAnnotation { text: text.clone(), ..shape.clone() } } else { shape.clone() }).collect());
        }
        self.pending.map(|pending| {
            let mut next = self.shapes.clone();
            next.push(annotation(ImageAnnotationKind::Box, pending, text));
            next
        })
    }

    fn close_note(&mut self, cx: &mut App) {
        self.pending = None;
        self.editing = None;
        self.label.update(cx, |label, cx| label.reset("", cx));
    }

    fn commit_note(&mut self, cx: &mut App) {
        if !self.composing() {
            return;
        }
        if let Some(next) = self.written(cx) {
            self.change(next);
        }
        self.close_note(cx);
    }

    /// A box just drawn is kept bare; an existing note stays as it was.
    fn cancel_note(&mut self, cx: &mut App) {
        if let Some(pending) = self.pending {
            let mut next = self.shapes.clone();
            next.push(annotation(ImageAnnotationKind::Box, pending, String::new()));
            self.change(next);
        }
        self.close_note(cx);
    }

    fn edit(&mut self, index: usize, window: &mut Window, cx: &mut App) {
        let text = self.shapes.get(index).map(|shape| shape.text.clone()).unwrap_or_default();
        self.label.update(cx, |label, cx| {
            label.reset(&text, cx);
            label.select_all_text(cx);
        });
        self.editing = Some(index);
        self.hovered = None;
        let focus = self.label.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
    }

    fn remove(&mut self, index: usize) {
        let next = self.shapes.iter().enumerate().filter(|(at, _)| *at != index).map(|(_, shape)| shape.clone()).collect();
        self.change(next);
    }

    /// Where a window position falls on the image, as a share of it.
    fn point_at(&self, position: Point<Pixels>) -> Option<(f32, f32)> {
        let bounds = self.frame.get()?;
        let width = f32::from(bounds.size.width).max(1.0);
        let height = f32::from(bounds.size.height).max(1.0);
        Some((((f32::from(position.x - bounds.origin.x)) / width).clamp(0.0, 1.0), ((f32::from(position.y - bounds.origin.y)) / height).clamp(0.0, 1.0)))
    }
}

fn mark_at(shapes: &[ImageAnnotation], point: (f32, f32), frame: (f32, f32)) -> Option<usize> {
    let at = (point.0 * frame.0, point.1 * frame.1);
    let tolerance = 9.0;
    for index in (0..shapes.len()).rev() {
        let rect = rect_of(&shapes[index]);
        let from = (rect.x * frame.0, rect.y * frame.1);
        let to = ((rect.x + rect.width) * frame.0, (rect.y + rect.height) * frame.1);
        if shapes[index].kind == ImageAnnotationKind::Arrow {
            let run = (to.0 - from.0, to.1 - from.1);
            let span = run.0 * run.0 + run.1 * run.1;
            let along = if span == 0.0 { 0.0 } else { (((at.0 - from.0) * run.0 + (at.1 - from.1) * run.1) / span).clamp(0.0, 1.0) };
            if (at.0 - (from.0 + run.0 * along)).hypot(at.1 - (from.1 + run.1 * along)) <= tolerance {
                return Some(index);
            }
            continue;
        }
        if at.0 >= from.0 - tolerance && at.0 <= to.0 + tolerance && at.1 >= from.1 - tolerance && at.1 <= to.1 + tolerance {
            return Some(index);
        }
    }
    None
}

fn badge_radius(width: f32, height: f32) -> f32 {
    (width.max(height) * 0.013).round().max(11.0)
}

/// Where each box's badge sits, in frame pixels: the first corner spot no earlier badge holds.
fn place_badges(boxes: &[Rect], width: f32, height: f32) -> Vec<(f32, f32)> {
    let radius = badge_radius(width, height);
    let mut placed: Vec<(f32, f32)> = Vec::new();
    for rect in boxes {
        let Rect { x, y, width: w, height: h } = *rect;
        let spots: Vec<(f32, f32)> = [
            (x - radius, y - radius),
            (x + w + radius, y - radius),
            (x - radius, y + h + radius),
            (x + w + radius, y + h + radius),
            (x + radius, y + radius),
            (x + w - radius, y + radius),
            (x + radius, y + h - radius),
            (x + w - radius, y + h - radius),
        ]
        .into_iter()
        .map(|(sx, sy)| (sx.max(radius).min((width - radius).max(radius)), sy.max(radius).min((height - radius).max(radius))))
        .collect();
        let free = spots.iter().find(|spot| placed.iter().all(|taken| (spot.0 - taken.0).hypot(spot.1 - taken.1) >= radius * 2.0)).copied();
        placed.push(free.unwrap_or(spots[0]));
    }
    placed
}

/// Box geometry in frame pixels, with the badge centre each one takes.
fn layout(shapes: &[ImageAnnotation], width: f32, height: f32) -> (f32, f32, Vec<Rect>, Vec<(f32, f32)>) {
    let stroke = (width.max(height) * 0.003).round().max(2.0);
    let radius = badge_radius(width, height);
    let boxes: Vec<Rect> = shapes
        .iter()
        .filter(|shape| shape.kind == ImageAnnotationKind::Box)
        .map(|shape| Rect { x: shape.x as f32 * width, y: shape.y as f32 * height, width: (shape.width as f32 * width).max(stroke), height: (shape.height as f32 * height).max(stroke) })
        .collect();
    let centers = place_badges(&boxes, width, height);
    (stroke, radius, boxes, centers)
}

/// The marks painted over the image at the size it is shown, as the canvas paints them.
fn marks_layer(shapes: Vec<ImageAnnotation>, frame: (f32, f32), prefix: &str) -> AnyElement {
    let (stroke, radius, boxes, centers) = layout(&shapes, frame.0, frame.1);
    let painted = shapes.clone();
    let boxes_paint = boxes.clone();
    let centers_paint = centers.clone();
    let layer = canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, _, window: &mut Window, _| {
            let origin = bounds.origin;
            let at = |x: f32, y: f32| point(origin.x + px(x), origin.y + px(y));
            let color = mark_color();
            let mut mark = 0;
            for shape in &painted {
                if shape.kind == ImageAnnotationKind::Arrow {
                    let tail = (shape.x as f32 * frame.0, shape.y as f32 * frame.1);
                    let tip = ((shape.x + shape.width) as f32 * frame.0, (shape.y + shape.height) as f32 * frame.1);
                    let angle = (tip.1 - tail.1).atan2(tip.0 - tail.0);
                    let length = (tip.0 - tail.0).hypot(tip.1 - tail.1);
                    let head = (length * 0.3).min(stroke * 9.0).max(stroke * 4.0);
                    let base = (tip.0 - angle.cos() * head, tip.1 - angle.sin() * head);
                    let spread = head * 0.5;
                    let mut line = PathBuilder::stroke(px(stroke));
                    line.move_to(at(tail.0, tail.1));
                    line.line_to(at(base.0, base.1));
                    if let Ok(path) = line.build() {
                        window.paint_path(path, color);
                    }
                    window.paint_quad(gpui::fill(Bounds::new(at(tail.0 - stroke / 2.0, tail.1 - stroke / 2.0), gpui::size(px(stroke), px(stroke))), color).corner_radii(px(stroke / 2.0)));
                    let mut tri = PathBuilder::fill();
                    tri.move_to(at(tip.0, tip.1));
                    tri.line_to(at(base.0 - angle.sin() * spread, base.1 + angle.cos() * spread));
                    tri.line_to(at(base.0 + angle.sin() * spread, base.1 - angle.cos() * spread));
                    tri.close();
                    if let Ok(path) = tri.build() {
                        window.paint_path(path, color);
                    }
                    continue;
                }
                let rect = boxes_paint[mark];
                let center = centers_paint[mark];
                mark += 1;
                window.paint_quad(gpui::outline(Bounds::new(at(rect.x, rect.y), gpui::size(px(rect.width.max(stroke)), px(rect.height.max(stroke)))), color, gpui::BorderStyle::Solid).border_widths(px(stroke)));
                let ring = (radius * 0.14).max(1.0);
                let disc = Bounds::new(at(center.0 - radius - ring / 2.0, center.1 - radius - ring / 2.0), gpui::size(px(2.0 * radius + ring), px(2.0 * radius + ring)));
                window.paint_quad(gpui::quad(disc, QuadCorners::all(px(radius + ring / 2.0)), color, px(ring), gpui::white(), gpui::BorderStyle::Solid));
            }
        },
    )
    .absolute()
    .inset_0();
    let mut stack = div().absolute().inset_0().child(layer);
    for (index, center) in centers.iter().enumerate() {
        let size = (radius * 1.1).round();
        stack = stack.child(
            div()
                .absolute()
                .left(px(center.0 - radius))
                .top(px(center.1 - radius))
                .size(px(radius * 2.0))
                .flex()
                .items_center()
                .justify_center()
                .text_color(gpui::white())
                .text_size(px(size))
                .line_height(px(size))
                .font_weight(FontWeight::BOLD)
                .child(format!("{prefix}{}", index + 1)),
        );
    }
    stack.into_any_element()
}

fn hint(count: usize, tool: ImageAnnotationKind) -> String {
    if count == 0 {
        match tool {
            ImageAnnotationKind::Box => "Drag a box over the area you mean".into(),
            ImageAnnotationKind::Arrow => "Drag from anywhere to point at the area you mean".into(),
        }
    } else {
        format!("{count} mark{}.", if count == 1 { "" } else { "s" })
    }
}

/// The number a box is drawn with, so what a control says matches what the screenshot shows.
fn mark_number(shapes: &[ImageAnnotation], index: usize) -> usize {
    shapes.iter().take(index + 1).filter(|shape| shape.kind == ImageAnnotationKind::Box).count()
}

impl Root {
    pub fn open_annotator(&mut self, image_id: String, path: String, prefix: String, window: &mut Window, cx: &mut Context<Root>) {
        super::mark_render::prepare(window, cx);
        let label = cx.new(|cx| TextInput::new(cx).label("Annotation note").placeholder("What's wrong here? (Enter to add)"));
        let subscription = cx.subscribe_in(&label, window, |root, _, event: &InputEvent, window, cx| {
            let Some(annotator) = root.composer.annotator.as_mut() else { return };
            match event {
                InputEvent::Submit => {
                    annotator.commit_note(cx);
                    window.focus(&annotator.focus, cx);
                }
                InputEvent::Changed => {}
                _ => return,
            }
            cx.notify();
        });
        let shapes = self.composer.marks.get(&image_id).map(|marks| marks.annotations.clone()).unwrap_or_default();
        let natural = crate::effects::window_desktop::image_size(std::path::Path::new(&path));
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        self.composer.annotator = Some(Annotator {
            image_id,
            path: PathBuf::from(path),
            natural,
            prefix,
            tool: ImageAnnotationKind::Box,
            shapes,
            past: Vec::new(),
            hovered: None,
            walked: false,
            discarding: false,
            pending: None,
            editing: None,
            label,
            _label: subscription,
            origin: None,
            draft: None,
            frame: bounds_cell(),
            stage: bounds_cell(),
            focus,
            focused: false,
        });
        cx.notify();
    }

    fn close_annotator(&mut self, window: &mut Window, cx: &mut Context<Root>) {
        self.composer.annotator = None;
        let focus = self.composer.input.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Closing throws the marks away, so changed marks or a note still being typed ask first.
    fn request_close_annotator(&mut self, window: &mut Window, cx: &mut Context<Root>) {
        let Some(annotator) = self.composer.annotator.as_mut() else { return };
        if annotator.past.is_empty() && annotator.written(cx).is_none() {
            self.close_annotator(window, cx);
        } else {
            annotator.discarding = true;
            cx.notify();
        }
    }

    fn apply_annotator(&mut self, window: &mut Window, cx: &mut Context<Root>) {
        let Some(annotator) = self.composer.annotator.as_ref() else { return };
        let annotations = annotator.written(cx).unwrap_or_else(|| annotator.shapes.clone());
        let image_id = annotator.image_id.clone();
        let path = annotator.path.to_string_lossy().into_owned();
        let prefix = annotator.prefix.clone();
        self.close_annotator(window, cx);
        if annotations.is_empty() {
            self.composer.marks.remove(&image_id);
            return;
        }
        self.composer.marks.insert(image_id.clone(), ImageMarks { annotations: annotations.clone(), preview: None });
        let drawn = super::mark_render::render(&path, &annotations, &prefix, cx);
        cx.spawn(async move |root, cx| {
            let Ok(Ok(png)) = drawn.await else { return };
            let png: String = png;
            let preview = cx.background_spawn(async move { decode(&png) }).await;
            let _ = root.update(cx, |root, cx| {
                if let Some(marks) = root.composer.marks.get_mut(&image_id).filter(|marks| marks.annotations == annotations) {
                    marks.preview = preview;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn annotator_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Root>) -> bool {
        let keys = &event.keystroke;
        let Some(annotator) = self.composer.annotator.as_mut() else { return false };
        let typing = annotator.label.read(cx).focus_handle(cx).is_focused(window);
        if keys.key == "escape" {
            if annotator.discarding {
                annotator.discarding = false;
            } else if annotator.composing() {
                annotator.cancel_note(cx);
                window.focus(&annotator.focus, cx);
            } else {
                self.request_close_annotator(window, cx);
            }
            cx.notify();
            return true;
        }
        if typing || annotator.composing() {
            return false;
        }
        if keys.key == "z" && (keys.modifiers.platform || keys.modifiers.control) && !keys.modifiers.shift && !keys.modifiers.alt {
            annotator.undo();
            cx.notify();
            return true;
        }
        if keys.modifiers.platform || keys.modifiers.control || keys.modifiers.alt {
            return false;
        }
        match keys.key.as_str() {
            "b" => annotator.tool = ImageAnnotationKind::Box,
            "a" => annotator.tool = ImageAnnotationKind::Arrow,
            "left" | "up" | "right" | "down" => {
                let count = annotator.shapes.len();
                if count == 0 {
                    return false;
                }
                let forward = matches!(keys.key.as_str(), "right" | "down");
                let picked = if annotator.walked { annotator.picked() } else { None };
                annotator.hovered = Some(match picked {
                    None => if forward { 0 } else { count - 1 },
                    Some(at) => if forward { (at + 1) % count } else { (at + count - 1) % count },
                });
                annotator.walked = true;
            }
            "backspace" | "delete" => {
                let Some(picked) = annotator.picked() else { return false };
                annotator.remove(picked);
            }
            "enter" => {
                let Some(picked) = annotator.picked().filter(|index| annotator.shapes[*index].kind == ImageAnnotationKind::Box) else { return false };
                annotator.edit(picked, window, cx);
            }
            _ => return false,
        }
        cx.notify();
        true
    }
}

/// A PNG as the BGRA frame GPUI draws.
fn decode(png: &str) -> Option<Arc<RenderImage>> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD.decode(png).ok()?;
    let mut frame = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png).ok()?.into_rgba8();
    for pixel in frame.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(frame)])))
}

/// The annotator over the whole window, while one is open.
pub fn render(root: &mut Root, window: &mut Window, cx: &mut Context<Root>) -> Option<AnyElement> {
    let annotator = root.composer.annotator.as_mut()?;
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let m = &theme.metrics;
    let viewport = window.viewport_size();
    let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
    let panel_w = 1080.0_f32.min(vw - 56.0).max(1.0);
    let panel_h = 820.0_f32.min(vh - 84.0).max(1.0);
    let stage = annotator.stage.get().map(|bounds| (f32::from(bounds.size.width) - 32.0, f32::from(bounds.size.height) - 32.0)).unwrap_or((panel_w - 34.0, panel_h - 150.0));
    let frame = annotator.natural.map(|(width, height)| {
        let ratio = (stage.0 / width).min(stage.1 / height).min(1.0).max(0.0);
        ((width * ratio).floor().max(1.0), (height * ratio).floor().max(1.0))
    });
    if !annotator.focused {
        annotator.focused = true;
        window.focus(&annotator.focus, cx);
    }
    let composing = annotator.composing();
    let tool = annotator.tool;
    let picked = annotator.picked();

    let tools = div()
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Group, "Annotation tool"))
        .flex()
        .gap(px(4.0))
        .child(toggle_button("annotator-box", "Box", tool == ImageAnnotationKind::Box).icon(IconName::Square).icon_size(16.0).tip(shortcut_tip("Box", "B")).on_click(cx.listener(|root, _, _, cx| {
            if let Some(annotator) = root.composer.annotator.as_mut() {
                annotator.commit_note(cx);
                annotator.tool = ImageAnnotationKind::Box;
                cx.notify();
            }
        })))
        .child(toggle_button("annotator-arrow", "Arrow", tool == ImageAnnotationKind::Arrow).icon(IconName::ArrowUpRight).icon_size(16.0).tip(shortcut_tip("Arrow", "A")).on_click(cx.listener(|root, _, _, cx| {
            if let Some(annotator) = root.composer.annotator.as_mut() {
                annotator.commit_note(cx);
                annotator.tool = ImageAnnotationKind::Arrow;
                cx.notify();
            }
        })));
    let head = div()
        .flex()
        .items_center()
        .gap(px(12.0))
        .pt(px(12.0))
        .pr(px(12.0))
        .pb(px(12.0))
        .pl(px(16.0))
        .border_b_1()
        .border_color(t.line_soft)
        .child(tools)
        .child(div().ml_auto().child(IconButton::new("annotator-close", IconName::X, "Close annotator").icon_size(16.0).tip(super::chrome::key_tip("Close", "Escape")).on_click(cx.listener(|root, _, window, cx| root.request_close_annotator(window, cx)))));

    let mut fit = div().size_full().flex().items_center().justify_center();
    if let Some(frame) = frame {
        let mut live = annotator.shapes.clone();
        if let Some(pending) = annotator.pending {
            live.push(annotation(ImageAnnotationKind::Box, pending, String::new()));
        }
        if let Some(draft) = annotator.draft.clone() {
            live.push(draft);
        }
        let mut stack = div()
            .id("annotator-frame")
            .accessible(gpui::AccessInfo::new(gpui::AccessRole::Image, "Screenshot canvas"))
            .relative()
            .flex_none()
            .w(px(frame.0))
            .h(px(frame.1))
            .cursor(gpui::CursorStyle::Crosshair)
            .child(bounds_probe(&annotator.frame))
            .child(img(annotator.path.clone()).size_full().rounded(px(4.0)).object_fit(ObjectFit::Fill))
            .child(marks_layer(live, frame, &annotator.prefix))
            .on_mouse_down(gpui::MouseButton::Left, cx.listener(move |root, event: &gpui::MouseDownEvent, window, cx| {
                let Some(annotator) = root.composer.annotator.as_mut() else { return };
                if annotator.composing() {
                    return;
                }
                window.focus(&annotator.focus, cx);
                annotator.walked = false;
                annotator.hovered = None;
                let Some(at) = annotator.point_at(event.position) else { return };
                annotator.origin = Some(at);
                annotator.draft = Some(annotation(annotator.tool, Rect { x: at.0, y: at.1, width: 0.0, height: 0.0 }, String::new()));
                cx.stop_propagation();
                cx.notify();
            }))
            .on_mouse_move(cx.listener(move |root, event: &gpui::MouseMoveEvent, _, cx| {
                let Some(annotator) = root.composer.annotator.as_mut() else { return };
                if annotator.origin.is_some() {
                    return;
                }
                let hovered = if annotator.composing() { None } else { annotator.point_at(event.position).and_then(|at| mark_at(&annotator.shapes, at, frame)) };
                if hovered.is_some() && hovered != annotator.hovered {
                    annotator.hovered = hovered;
                    annotator.walked = false;
                    cx.notify();
                }
            }));
        if !composing {
            if let Some(index) = picked.filter(|_| annotator.walked) {
                let rect = rect_of(&annotator.shapes[index]);
                let (x, y) = (rect.x.min(rect.x + rect.width) * frame.0, rect.y.min(rect.y + rect.height) * frame.1);
                stack = stack.child(div().absolute().left(px(x - 8.0)).top(px(y - 8.0)).w(px(rect.width.abs() * frame.0 + 16.0)).h(px(rect.height.abs() * frame.1 + 16.0)).border_2().border_color(t.focus_ring).rounded(px(8.0)));
            }
            for (index, shape) in annotator.shapes.iter().enumerate() {
                let is_picked = Some(index) == picked;
                let anchor = ((shape.x + shape.width) as f32 * frame.0, if shape.kind == ImageAnnotationKind::Arrow { (shape.y + shape.height) as f32 * frame.1 } else { shape.y as f32 * frame.1 });
                let mark = format!("{}{}", annotator.prefix, mark_number(&annotator.shapes, index));
                let boxed = shape.kind == ImageAnnotationKind::Box;
                let width = if boxed { 51.0 } else { 24.0 };
                let mut tools = div()
                    .id(SharedString::from(format!("annotator-marktools-{index}")))
                    .absolute()
                    .left(px(anchor.0 - width / 2.0))
                    .top(px(anchor.1 - 12.0))
                    .flex()
                    .gap(px(3.0))
                    .when(!is_picked, |tools| tools.opacity(0.8).transition(gpui::Transition::ease(120)).hover(|style| style.opacity(1.0)))
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_hover(cx.listener(move |root, hovered: &bool, _, cx| {
                        if let (true, Some(annotator)) = (*hovered, root.composer.annotator.as_mut()) {
                            annotator.hovered = Some(index);
                            cx.notify();
                        }
                    }));
                if boxed {
                    tools = tools.child(IconButton::new(SharedString::from(format!("annotator-edit-{index}")), IconName::Pencil, format!("Edit note on mark {mark}")).variant(IconButtonVariant::Glass).size(IconButtonSize::Xs).round(true).icon_size(14.0).tip(super::chrome::key_tip("Edit note", "Enter")).on_click(cx.listener(move |root, _, window, cx| {
                        if let Some(annotator) = root.composer.annotator.as_mut() {
                            annotator.edit(index, window, cx);
                            cx.notify();
                        }
                    })));
                }
                let remove_label = if boxed { format!("Delete mark {mark}") } else { "Delete arrow".into() };
                let alert = (t.alert, t.alert_line, t.alert_ink);
                tools = tools.child(
                    IconButton::new(SharedString::from(format!("annotator-remove-{index}")), IconName::X, remove_label).variant(IconButtonVariant::Glass).size(IconButtonSize::Xs).round(true).icon_size(14.0).hover_look(alert.0, alert.1, alert.2).tip(super::chrome::key_tip("Delete", "Backspace")).on_click(cx.listener(move |root, _, _, cx| {
                        if let Some(annotator) = root.composer.annotator.as_mut() {
                            annotator.remove(index);
                            cx.notify();
                        }
                    })),
                );
                stack = stack.child(tools);
            }
        }
        let at = annotator.pending.or_else(|| annotator.editing.and_then(|index| annotator.shapes.get(index)).map(rect_of));
        if let Some(rect) = at {
            stack = stack.child(
                glass(div(), t, GlassKind::Popover, Some(8.0), None)
                    .id("annotator-note")
                    .map(act_ui::style::glass_in)
                    .absolute()
                    .left(px(rect.x * frame.0))
                    .top(px((rect.y + rect.height) * frame.1 + 8.0))
                    .w(px(280.0_f32.min(vw * 0.6)))
                    .border_color(t.alert)
                    .px(px(9.0))
                    .py(px(7.0))
                    .text_color(t.ink)
                    .text_size(px(m.text_control))
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(annotator.label.clone()),
            );
        }
        fit = fit.child(stack);
    }
    let stage_el = div().flex_1().min_w_0().min_h_0().p(px(16.0)).relative().child(bounds_probe(&annotator.stage)).child(fit);

    let count = annotator.shapes.len();
    let bar = div().flex().items_center().gap(px(8.0)).px(px(14.0)).py(px(11.0)).border_t_1().border_color(t.line_soft);
    let bar = if annotator.discarding {
        bar.child(div().min_w_0().mr_auto().text_color(t.muted).text_size(px(m.text_meta)).child("Discard the changes to these marks?"))
            .child(Button::new("annotator-keep", "Keep editing").size(ButtonSize::Lg).on_click(cx.listener(|root, _, _, cx| {
                if let Some(annotator) = root.composer.annotator.as_mut() {
                    annotator.discarding = false;
                    cx.notify();
                }
            })))
            .child(Button::new("annotator-discard", "Discard").variant(ButtonVariant::Danger).armed(true).size(ButtonSize::Lg).on_click(cx.listener(|root, _, window, cx| root.close_annotator(window, cx))))
    } else {
        bar.child(IconButton::new("annotator-undo", IconName::Undo2, "Undo").variant(IconButtonVariant::Soft).size(IconButtonSize::Md).icon_size(16.0).tip(super::chrome::key_tip("Undo", "Mod+Z")).disabled(annotator.past.is_empty()).on_click(cx.listener(|root, _, _, cx| {
            if let Some(annotator) = root.composer.annotator.as_mut() {
                annotator.undo();
                cx.notify();
            }
        })))
        .child(div().min_w_0().mr_auto().text_color(t.muted).text_size(px(m.text_meta)).child(hint(count, tool)))
        .child(Button::new("annotator-cancel", "Cancel").size(ButtonSize::Lg).on_click(cx.listener(|root, _, window, cx| root.request_close_annotator(window, cx))))
        .child(Button::new("annotator-done", "Done").variant(ButtonVariant::Primary).size(ButtonSize::Lg).icon(IconName::Check).icon_size(16.0).disabled(frame.is_none()).on_click(cx.listener(|root, _, window, cx| root.apply_annotator(window, cx))))
    };

    let panel = glass(div(), t, GlassKind::Overlay, Some(16.0), None)
        .w(px(panel_w))
        .h(px(panel_h))
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(head)
        .child(stage_el)
        .child(bar);
    let focus = annotator.focus.clone();
    let fill: Hsla = t.veil_deep.into();
    Some(
        div()
            .id("annotator")
            .accessible(gpui::AccessInfo::new(gpui::AccessRole::Dialog, "Annotate screenshot").modal())
            .focus_trap()
            .track_focus(&focus)
            .key_context("Annotator")
            .absolute()
            .inset_0()
            .occlude()
            .bg(fill)
            .veil_backdrop(t)
            .pt(px(56.0))
            .px(px(28.0))
            .pb(px(28.0))
            .flex()
            .items_center()
            .justify_center()
            .on_key_down(cx.listener(|root, event: &KeyDownEvent, window, cx| {
                if root.annotator_key(event, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .on_mouse_move(cx.listener(|root, event: &gpui::MouseMoveEvent, _, cx| {
                let Some(annotator) = root.composer.annotator.as_mut() else { return };
                let (Some(origin), Some(at)) = (annotator.origin, annotator.point_at(event.position)) else { return };
                let rect = if annotator.tool == ImageAnnotationKind::Arrow { arrow_between(origin, at) } else { normalize(origin, at) };
                annotator.draft = Some(annotation(annotator.tool, rect, String::new()));
                cx.notify();
            }))
            .on_mouse_up(gpui::MouseButton::Left, cx.listener(|root, event: &gpui::MouseUpEvent, window, cx| {
                let Some(annotator) = root.composer.annotator.as_mut() else { return };
                let Some(origin) = annotator.origin.take() else { return };
                annotator.draft = None;
                let at = annotator.point_at(event.position).unwrap_or(origin);
                if annotator.tool == ImageAnnotationKind::Arrow {
                    let arrow = arrow_between(origin, at);
                    if arrow.width.hypot(arrow.height) >= MIN_LENGTH {
                        let mut next = annotator.shapes.clone();
                        next.push(annotation(ImageAnnotationKind::Arrow, arrow, String::new()));
                        annotator.change(next);
                    }
                } else {
                    let rect = normalize(origin, at);
                    if rect.width >= MIN_SIZE && rect.height >= MIN_SIZE {
                        annotator.pending = Some(rect);
                        annotator.label.update(cx, |label, cx| label.reset("", cx));
                        let focus = annotator.label.read(cx).focus_handle(cx);
                        window.focus(&focus, cx);
                    }
                }
                cx.notify();
            }))
            .child(act_ui::style::under_veil(panel, t.veil_deep, 16.0, t))
            .into_any_element(),
    )
}
