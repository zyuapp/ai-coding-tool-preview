//! What opens over the whole window to be looked at: an attached picture with its gallery, or a
//! diagram at full size (AttachmentViewer.tsx, ZoomView.tsx, MermaidBlock's DiagramViewer, ui/Dialog's Viewer).

use std::rc::Rc;

use act_core::state::WorkspaceState;
use act_core::AppCommand;
use act_ui::components::button::{Button, ButtonSize, ButtonVariant, IconButton, IconButtonSize, IconButtonVariant};
use act_ui::style::{glass, GlassKind};
use act_ui::theme::ActiveTheme;
use act_ui::style::Backdrop;
use act_ui::IconName;
use gpui::{div, img, prelude::*, px, AnyElement, App, Context, FocusHandle, Global, KeyDownEvent, ObjectFit, ScrollHandle, Window};

use super::mermaid::{self, Diagram};
use super::root::Root;
use crate::effects::window_desktop;
use crate::workspace::dispatch;

/// How far past the fitted size a view can be pushed.
const MAX_SCALE: f32 = 4.0;
/// What one press of a zoom button does.
const SCALE_STEP: f32 = 1.4;
/// How far a pinch has to travel to double the size.
const PINCH_RATE: f32 = 100.0;

/// The diagram opened at full size, held above the transcript that may drop the row it came from.
#[derive(Default)]
struct OpenDiagram(Option<(Rc<Diagram>, String)>);

impl Global for OpenDiagram {}

pub fn open_diagram(diagram: Rc<Diagram>, source: String, cx: &mut App) {
    cx.set_global(OpenDiagram(Some((diagram, source))));
    cx.refresh_windows();
}

/// Pasted text read in full, with its label.
#[derive(Default)]
struct OpenPaste(Option<(String, String)>);

impl Global for OpenPaste {}

pub fn open_paste(text: String, label: String, cx: &mut App) {
    cx.set_global(OpenPaste(Some((text, label))));
    cx.refresh_windows();
}

fn close_paste(cx: &mut App) {
    cx.set_global(OpenPaste(None));
    cx.refresh_windows();
}

fn close_diagram(cx: &mut App) {
    cx.set_global(OpenDiagram(None));
    cx.refresh_windows();
}

/// One viewer's zoom and the scroller it lives in; reset whenever what it shows changes.
pub struct ViewerState {
    shown: Option<String>,
    chosen: Option<f32>,
    scroll: ScrollHandle,
    focus: Option<FocusHandle>,
    focused_for: Option<String>,
    /// What had the keys before the viewer opened, which decides where they go when it closes.
    previous: Option<FocusHandle>,
}

impl Default for ViewerState {
    fn default() -> Self {
        ViewerState { shown: None, chosen: None, scroll: ScrollHandle::new(), focus: None, focused_for: None, previous: None }
    }
}

thread_local! {
    static STATE: std::cell::RefCell<ViewerState> = std::cell::RefCell::default();
}

fn with_state<R>(work: impl FnOnce(&mut ViewerState) -> R) -> R {
    STATE.with(|state| work(&mut state.borrow_mut()))
}

/// Whether a diagram, pasted text or visual is open over the window.
pub fn open(cx: &App) -> bool {
    cx.try_global::<OpenDiagram>().is_some_and(|open| open.0.is_some()) || cx.try_global::<OpenPaste>().is_some_and(|open| open.0.is_some()) || super::visual::opened(cx).is_some()
}

pub fn render(root: &mut Root, state: &WorkspaceState, window: &mut Window, cx: &mut Context<Root>) -> Option<AnyElement> {
    if let Some(source) = super::visual::opened(cx) {
        return Some(visual_viewer(source, window, cx));
    }
    let diagram = cx.try_global::<OpenDiagram>().and_then(|open| open.0.clone());
    if let Some((diagram, source)) = diagram {
        return Some(diagram_viewer(diagram, source, window, cx));
    }
    let paste = cx.try_global::<OpenPaste>().and_then(|open| open.0.clone());
    if let Some((text, label)) = paste {
        return Some(paste_viewer(text, label, window, cx));
    }
    let Some(source) = state.viewing_image.clone() else {
        // Keys go back to the dock tab that held them, else to the thread's composer.
        let closed = with_state(|viewer| viewer.focused_for.take().map(|_| viewer.previous.take()));
        if let Some(previous) = closed {
            if !previous.is_some_and(|previous| root.dock.restore_focus(&previous, window, cx)) {
                cx.defer(|cx| dispatch(AppCommand::FocusComposer { recover: Some(true) }, cx));
            }
        }
        return None;
    };
    Some(image_viewer(&source, &state.viewing_gallery, window, cx))
}

/// The stage's room, which a view is fitted against.
fn stage_room(window: &Window, gallery: bool) -> (f32, f32) {
    let viewport = window.viewport_size();
    let side = if gallery { 68.0 } else { 32.0 };
    ((f32::from(viewport.width) - 2.0 * side).max(1.0), (f32::from(viewport.height) - 56.0 - 32.0).max(1.0))
}

fn fit_for(size: (f32, f32), room: (f32, f32)) -> f32 {
    (room.0 / size.0).min(room.1 / size.1).min(1.0)
}

fn clamp(scale: f32, fit: f32) -> f32 {
    scale.max(fit).min(MAX_SCALE)
}

/// Brings the state up to what is shown, and the keyboard into the viewer.
fn sync(key: &str, window: &mut Window, cx: &mut App) -> FocusHandle {
    with_state(|viewer| {
        if viewer.shown.as_deref() != Some(key) {
            viewer.shown = Some(key.to_string());
            viewer.chosen = None;
            viewer.scroll = ScrollHandle::new();
        }
        let focus = viewer.focus.get_or_insert_with(|| cx.focus_handle()).clone();
        if viewer.focused_for.as_deref() != Some(key) {
            if viewer.focused_for.is_none() {
                viewer.previous = window.focused(cx).filter(|previous| previous != &focus);
            }
            viewer.focused_for = Some(key.to_string());
            window.focus(&focus);
        }
        focus
    })
}

fn zoom(by: Option<f32>, fit: f32, cx: &mut App) {
    with_state(|viewer| {
        viewer.chosen = by.map(|factor| clamp(viewer.chosen.unwrap_or(fit) * factor, fit));
    });
    cx.refresh_windows();
}

/// The readout and the two presses that change the size.
fn zoom_controls(scale: f32, fit: f32, cx: &App) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let fitted = (scale - fit).abs() < 0.001;
    let level = format!("{}%", (scale * 100.0).round());
    let readout: AnyElement = if fitted {
        div().min_w(px(48.0)).h(px(26.0)).flex().items_center().justify_center().text_color(t.muted).text_size(px(12.0)).child(level).into_any_element()
    } else {
        Button::new("viewer-zoom-level", level.clone()).name(format!("Zoom {level}, fit to window")).variant(ButtonVariant::Soft).size(ButtonSize::Sm).icon(IconName::Minimize2).icon_size(12.0).tip("Fit to window (0)").on_click(move |_, _, cx| zoom(None, fit, cx)).into_any_element()
    };
    glass(div(), &t, GlassKind::Float, Some(8.0), None)
        .id("viewer-zoom")
        .absolute()
        .top(px(58.0))
        .left(px(20.0))
        .flex()
        .items_center()
        .gap(px(3.0))
        .p(px(3.0))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(IconButton::new("viewer-zoom-out", IconName::Minus, "Zoom out").size(IconButtonSize::Xs).icon_size(16.0).tip("Zoom out (−)").disabled(scale <= fit + 0.001).on_click(move |_, _, cx| zoom(Some(1.0 / SCALE_STEP), fit, cx)))
        .child(readout)
        .child(IconButton::new("viewer-zoom-in", IconName::Plus, "Zoom in").size(IconButtonSize::Xs).icon_size(16.0).tip("Zoom in (+)").disabled(scale >= MAX_SCALE - 0.001).on_click(move |_, _, cx| zoom(Some(SCALE_STEP), fit, cx)))
        .into_any_element()
}

/// The dimmed layer every viewer stands on; a click on it, rather than on what it shows, closes it.
fn shell(label: &str, focus: &FocusHandle, gallery: bool, close: impl Fn(&mut App) + Clone + 'static, keys: impl Fn(&KeyDownEvent, &mut App) -> bool + 'static, cx: &App) -> gpui::Stateful<gpui::Div> {
    let t = cx.theme().tokens.clone();
    let on_close = close.clone();
    div()
        .id("viewer")
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Dialog, label.to_string()).modal())
        .focus_trap()
        .track_focus(focus)
        .key_context("Viewer")
        .absolute()
        .inset_0()
        .occlude()
        .bg(t.veil_deep)
        .veil_backdrop(&t)
        .pt(px(56.0))
        .pb(px(32.0))
        .px(px(if gallery { 68.0 } else { 32.0 }))
        .on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| on_close(cx))
        .on_key_down(move |event, _, cx| {
            if event.keystroke.key == "escape" {
                close(cx);
                cx.stop_propagation();
            } else if keys(event, cx) {
                cx.stop_propagation();
            }
        })
}

/// + and − zoom and 0 fits, with no modifier held.
fn zoom_key(event: &KeyDownEvent, fit: f32, cx: &mut App) -> bool {
    let keystroke = &event.keystroke;
    if keystroke.modifiers.platform || keystroke.modifiers.control || keystroke.modifiers.alt {
        return false;
    }
    let typed = keystroke.key_char.as_deref().unwrap_or(keystroke.key.as_str());
    match typed {
        "+" | "=" => zoom(Some(SCALE_STEP), fit, cx),
        "-" => zoom(Some(1.0 / SCALE_STEP), fit, cx),
        "0" => zoom(None, fit, cx),
        _ => return false,
    }
    true
}

/// A pinch, which arrives as a wheel with Control held, scales around nothing but the view itself.
fn pinch(event: &gpui::ScrollWheelEvent, fit: f32, window: &Window, cx: &mut App) -> bool {
    if !event.modifiers.control {
        return false;
    }
    let delta = f32::from(event.delta.pixel_delta(window.line_height()).y);
    with_state(|viewer| viewer.chosen = Some(clamp(viewer.chosen.unwrap_or(fit) * (delta / PINCH_RATE).exp(), fit)));
    cx.refresh_windows();
    true
}

/// The scroller a zooming view lives in: it centres a view that fits and scrolls one that does not.
fn stage(scroll: &ScrollHandle, fit: f32, size: (f32, f32), room: (f32, f32), content: AnyElement) -> gpui::Stateful<gpui::Div> {
    div()
        .id("viewer-stage")
        .size_full()
        .flex()
        .overflow_scroll()
        .track_scroll(scroll)
        .on_scroll_wheel(move |event, window, cx| {
            if pinch(event, fit, window, cx) {
                cx.stop_propagation();
            }
        })
        .child(div().when(size.0 <= room.0, |view| view.mx_auto()).when(size.1 <= room.1, |view| view.my_auto()).flex_none().on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation()).cursor_default().child(content))
}

fn close_button(label: &'static str, close: impl Fn(&mut App) + 'static) -> impl IntoElement {
    div().absolute().top(px(58.0)).right(px(20.0)).on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(
        IconButton::new("viewer-close", IconName::X, label).variant(IconButtonVariant::Soft).size(IconButtonSize::Md).icon_size(16.0).tip("Close (Escape)").on_click(move |_, _, cx| close(cx)),
    )
}

fn image_viewer(source: &str, gallery: &[String], window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let index = if gallery.len() > 1 { gallery.iter().position(|item| item == source) } else { None };
    let stepping = index.is_some();
    let focus = sync(source, window, cx);
    let path = window_desktop::image_file(source, cx);
    let size = window_desktop::image_size(&path);
    let room = stage_room(window, stepping);
    let fit = size.map(|size| fit_for(size, room)).unwrap_or(1.0);
    let (scale, scroll) = with_state(|viewer| (clamp(viewer.chosen.unwrap_or(fit), fit), viewer.scroll.clone()));
    let close = |cx: &mut App| dispatch(AppCommand::ImageClose, cx);
    let mut layer = shell("Screenshot", &focus, stepping, close, move |event, cx| {
        let keystroke = &event.keystroke;
        let bare = !keystroke.modifiers.platform && !keystroke.modifiers.control && !keystroke.modifiers.alt && !keystroke.modifiers.shift;
        if stepping && bare && (keystroke.key == "left" || keystroke.key == "right") {
            dispatch(AppCommand::ImageStep { delta: if keystroke.key == "left" { -1 } else { 1 } }, cx);
            return true;
        }
        size.is_some() && zoom_key(event, fit, cx)
    }, cx);
    match size {
        Some((width, height)) => {
            let picture = img(path).w(px(width * scale)).h(px(height * scale)).rounded(px(8.0)).object_fit(ObjectFit::Fill).into_any_element();
            layer = layer.child(stage(&scroll, fit, (width * scale, height * scale), room, picture));
            layer = layer.child(zoom_controls(scale, fit, cx));
            layer = layer.child(
                div().absolute().top(px(58.0)).right(px(58.0)).on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(
                    IconButton::new("viewer-download", IconName::Download, "Download image").variant(IconButtonVariant::Soft).size(IconButtonSize::Md).icon_size(16.0).tip("Download").on_click(|_, _, cx| dispatch(AppCommand::ImageDownload, cx)),
                ),
            );
        }
        None => {
            layer = layer.child(div().size_full().flex().items_center().justify_center().text_color(t.muted).text_size(px(14.0)).child("This image is no longer available."));
        }
    }
    layer = layer.child(close_button("Close screenshot", close));
    if let Some(index) = index {
        let last = gallery.len() - 1;
        let step = |id: &'static str, glyph: IconName, label: &'static str, tip: &'static str, delta: i8, disabled: bool| {
            IconButton::new(id, glyph, label).variant(IconButtonVariant::Soft).size(IconButtonSize::Md).round(true).icon_size(20.0).tip(tip).disabled(disabled).on_click(move |_, _, cx| dispatch(AppCommand::ImageStep { delta }, cx))
        };
        let center = |element: gpui::Div| element.absolute().top_0().bottom_0().flex().items_center().on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation());
        layer = layer
            .child(center(div().left(px(20.0))).child(step("viewer-previous", IconName::ChevronLeft, "Previous image", "Previous (←)", -1, index == 0)))
            .child(center(div().right(px(20.0))).child(step("viewer-next", IconName::ChevronRight, "Next image", "Next (→)", 1, index == last)))
            .child(
                div().absolute().bottom(px(10.0)).left_0().right_0().flex().justify_center().child(
                    glass(div(), &t, GlassKind::Float, Some(6.0), None).px(px(8.0)).py(px(2.0)).text_color(t.ink_soft).text_size(px(12.0)).map(act_ui::style::tabular_nums).child(format!("{} / {}", index + 1, gallery.len())),
                ),
            );
    }
    layer.into_any_element()
}

fn diagram_viewer(diagram: Rc<Diagram>, source: String, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let focus = sync(&format!("diagram:{source}"), window, cx);
    let room = stage_room(window, false);
    let fit = if diagram.width > 0.0 { fit_for((diagram.width, diagram.height), room) } else { 1.0 };
    let (scale, scroll) = with_state(|viewer| (clamp(viewer.chosen.unwrap_or(fit), fit), viewer.scroll.clone()));
    let picture = mermaid::picture("viewer-diagram", &diagram, scale, window, cx);
    shell("Diagram", &focus, false, close_diagram, move |event, cx| zoom_key(event, fit, cx), cx)
        .child(stage(&scroll, fit, (diagram.width * scale, diagram.height * scale), room, picture))
        .child(zoom_controls(scale, fit, cx))
        .child(close_button("Close diagram", close_diagram))
        .into_any_element()
}

fn paste_viewer(text: String, label: String, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    let mono = cx.theme().typography.mono_family.clone();
    let focus = sync(&format!("paste:{label}"), window, cx);
    let copied = text.clone();
    let body = glass(div(), &t, GlassKind::Float, Some(10.0), None)
        .id("paste-full")
        .max_w(px(880.0))
        .max_h_full()
        .overflow_y_scroll()
        .px(px(18.0))
        .py(px(16.0))
        .text_color(t.ink_soft)
        .font_family(mono)
        .text_size(px(m.text_meta))
        .line_height(gpui::relative(1.55))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(text);
    shell(&label, &focus, false, close_paste, |_, _| false, cx)
        .flex()
        .items_center()
        .justify_center()
        .child(body)
        .child(
            div().absolute().top(px(58.0)).right(px(58.0)).on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(
                IconButton::new("viewer-copy", IconName::Copy, "Copy pasted text").variant(IconButtonVariant::Soft).size(IconButtonSize::Md).icon_size(16.0).tip("Copy").on_click(move |_, _, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(copied.clone()))),
            ),
        )
        .child(close_button("Close pasted text", close_paste))
        .into_any_element()
}

/// A visual at full size, drawn on the conversation's own canvas.
fn visual_viewer(source: String, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let focus = sync(&format!("visual:{source}"), window, cx);
    let close = |cx: &mut App| super::visual::close(cx);
    let block = super::visual::block("viewer-visual".into(), source.into(), false, true, window, cx);
    shell("Visual", &focus, false, close, |_, _| false, cx)
        .child(
            div().id("viewer-visual-stage").size_full().flex().overflow_y_scroll().child(
                div()
                    .flex_none()
                    .w_full()
                    .max_w(px(1100.0))
                    .m_auto()
                    .rounded(px(12.0))
                    .px(px(20.0))
                    .py(px(8.0))
                    .bg(t.canvas)
                    .cursor_default()
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(block),
            ),
        )
        .child(close_button("Close visual", close))
        .into_any_element()
}
