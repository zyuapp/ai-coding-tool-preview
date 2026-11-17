//! One modal over the window (Dialog.tsx, dialog.css): the window out of focus behind a veil, a
//! pane of glass with a title, body and answers. Escape answers the topmost layer only; a press on
//! the backdrop cancels. And the viewer: a block over the whole window on a deeper veil. The root
//! draws either as its last child, before the tooltip layer.

use std::rc::Rc;
use std::time::Duration;

use gpui::{
    actions, div, point, prelude::*, px, Animation, AnimationExt as _, AnyElement, App, ElementId, FocusHandle,
    FontWeight, KeyBinding, MouseButton, Pixels, SharedString, Window,
};

use super::button::{IconButton, IconButtonSize, IconButtonVariant};
use crate::icon::IconName;
use crate::overlay::{placed, Placement};
use crate::style::{glass, GlassKind};
use crate::style::Backdrop;
use crate::theme::ActiveTheme;

actions!(dialog, [DialogCancel]);

pub const KEY_CONTEXT: &str = "Dialog";

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", DialogCancel, Some(KEY_CONTEXT))]);
}

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;


struct DialogState {
    focus: FocusHandle,
    focused_once: bool,
}

/// The layer's state, made on its first draw. When the layer leaves the tree, the keyboard goes
/// back to what held it before, unless something else took it meanwhile.
fn modal_state(id: ElementId, window: &mut Window, cx: &mut App) -> gpui::Entity<DialogState> {
    window.use_keyed_state(id, cx, |window, cx| {
        let opener = window.focused(cx);
        let handle = window.window_handle();
        cx.on_release(move |_, cx| {
            let Some(opener) = opener else { return };
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, cx| {
                    if window.focus_stranded(cx) {
                        window.focus(&opener);
                    }
                });
            });
        })
        .detach();
        DialogState { focus: cx.focus_handle(), focused_once: false }
    })
}

/// Covers the window. A modal is drawn in the normal tree, not deferred, so the menus it opens
/// (which are deferred) can draw over it; the root draws it last, after everything it covers.
fn layer(window: &Window, child: impl IntoElement) -> impl IntoElement {
    let viewport = window.viewport_size();
    placed(Placement::At { position: point(px(0.0), px(0.0)) }, div().w(viewport.width).h(viewport.height).child(child))
}

#[derive(IntoElement)]
pub struct Dialog {
    id: ElementId,
    title: Option<SharedString>,
    width: Pixels,
    media: Option<AnyElement>,
    children: Vec<AnyElement>,
    actions: Vec<AnyElement>,
    on_close: Handler,
    on_escape: Option<Handler>,
    initial_focus: Option<FocusHandle>,
    label: Option<SharedString>,
}

impl Dialog {
    /// `on_close` is cancel: a press on the backdrop, and Escape unless `on_escape` is given.
    pub fn new(id: impl Into<ElementId>, on_close: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        Self {
            id: id.into(),
            title: None,
            width: px(460.0),
            media: None,
            children: Vec::new(),
            actions: Vec::new(),
            on_close: Rc::new(on_close),
            on_escape: None,
            initial_focus: None,
            label: None,
        }
    }

    /// The name read for the dialog when it has no title.
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// `--dialog-width`, for a wider or narrower question.
    pub fn width(mut self, width: Pixels) -> Self {
        self.width = width;
        self
    }

    /// Drawn edge to edge above the title.
    pub fn media(mut self, media: impl IntoElement) -> Self {
        self.media = Some(media.into_any_element());
        self
    }

    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.actions.push(action.into_any_element());
        self
    }

    /// Escape, where it means more than closing, such as first folding an open list.
    pub fn on_escape(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_escape = Some(Rc::new(handler));
        self
    }

    /// Takes the keyboard on entry instead of the dialog itself.
    pub fn initial_focus(mut self, focus: &FocusHandle) -> Self {
        self.initial_focus = Some(focus.clone());
        self
    }
}

impl ParentElement for Dialog {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

/// Body copy in a dialog: muted, at control size, with room between its lines.
pub fn dialog_paragraph(text: impl Into<SharedString>, cx: &App) -> gpui::Div {
    let theme = cx.theme();
    div()
        .text_color(theme.tokens.muted)
        .text_size(px(theme.metrics.text_control))
        .line_height(gpui::relative(1.55))
        .child(text.into())
}

impl RenderOnce for Dialog {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = modal_state(self.id.clone(), window, cx);
        let focus = state.read(cx).focus.clone();
        if !state.read(cx).focused_once {
            match &self.initial_focus {
                Some(target) => window.focus(target),
                None => {
                    // The first control takes the keyboard once the dialog is drawn.
                    window.focus(&focus);
                    window.on_next_frame(|window, _| window.focus_next());
                }
            }
            state.update(cx, |state, _| state.focused_once = true);
        }
        let name = self.label.clone().or_else(|| self.title.clone()).unwrap_or_default();
        let theme = cx.theme();
        let t = &theme.tokens;
        let m = &theme.metrics;
        let reduced = t.reduced_motion;
        let viewport = window.viewport_size();
        let width = self.width.min(viewport.width - px(56.0));
        let max_height = viewport.height - px(84.0);

        let mut panel = glass(div(), t, GlassKind::Overlay, None, None)
            .id("dialog-panel")
            .w(width)
            .max_h(max_height)
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .p(px(24.0))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        if let Some(media) = self.media {
            panel = panel.child(div().mx(px(-24.0)).mt(px(-24.0)).child(media));
        }
        if let Some(title) = self.title {
            panel = panel.child(
                div()
                    .accessible(gpui::AccessInfo::new(gpui::AccessRole::Heading, title.clone()))
                    .text_color(t.ink)
                    .text_size(px(m.text_heading))
                    .font_weight(FontWeight::SEMIBOLD)
                    .map(|title| crate::style::letter_spacing(title, -0.01))
                    .child(title),
            );
        }
        panel = panel.children(self.children);
        if !self.actions.is_empty() {
            panel = panel.child(div().mt(px(6.0)).flex().flex_wrap().justify_end().gap(px(8.0)).children(self.actions));
        }
        let panel = crate::style::under_veil(panel, t.veil, 16.0, t);
        let panel_id = ElementId::NamedChild(Box::new(self.id.clone()), "panel".into());
        let panel = if reduced {
            panel.into_any_element()
        } else {
            div()
                .relative()
                .child(panel)
                .with_animation(
                    panel_id,
                    Animation::new(Duration::from_millis(180)).with_easing(|t| crate::theme::tokens::cubic_bezier([0.22, 1.0, 0.36, 1.0], t)),
                    |panel, delta| panel.opacity(delta).transform(gpui::ElementTransform::translate(px(0.0), px(8.0 * (1.0 - delta))).scaled(0.985 + 0.015 * delta)),
                )
                .into_any_element()
        };

        let on_close = self.on_close.clone();
        let on_escape = self.on_escape.clone().unwrap_or_else(|| self.on_close.clone());
        let scrim = div()
            .id(self.id.clone())
            .key_context(KEY_CONTEXT)
            .track_focus(&focus)
            .focus_trap()
            .accessible(gpui::AccessInfo::new(gpui::AccessRole::Dialog, name).modal())
            .occlude()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .pt(px(56.0))
            .px(px(28.0))
            .pb(px(28.0))
            .bg(t.veil)
            .veil_backdrop(t)
            .on_action(move |_: &DialogCancel, window, cx| on_escape(window, cx))
            .on_mouse_down(MouseButton::Left, move |_, window, cx| on_close(window, cx))
            .child(panel);
        let scrim_id = ElementId::NamedChild(Box::new(self.id), "scrim".into());
        let scrim = if reduced {
            scrim.into_any_element()
        } else {
            scrim
                .with_animation(scrim_id, Animation::new(Duration::from_millis(140)).with_easing(gpui::ease_out_quint()), |scrim, delta| {
                    scrim.opacity(delta)
                })
                .into_any_element()
        };
        layer(window, scrim)
    }
}

/// A block over the whole window, closed by Escape, its close button, or a press on the backdrop.
#[derive(IntoElement)]
pub struct Viewer {
    id: ElementId,
    label: SharedString,
    controls: Vec<AnyElement>,
    children: Vec<AnyElement>,
    on_close: Handler,
}

impl Viewer {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>, on_close: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        Self { id: id.into(), label: label.into(), controls: Vec::new(), children: Vec::new(), on_close: Rc::new(on_close) }
    }

    /// Controls drawn over the view beside the close button, such as zoom.
    pub fn control(mut self, control: impl IntoElement) -> Self {
        self.controls.push(control.into_any_element());
        self
    }
}

impl ParentElement for Viewer {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Viewer {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = modal_state(self.id.clone(), window, cx);
        let focus = state.read(cx).focus.clone();
        if !state.read(cx).focused_once {
            window.focus(&focus);
            window.on_next_frame(|window, _| window.focus_next());
            state.update(cx, |state, _| state.focused_once = true);
        }
        let t = &cx.theme().tokens;
        let close = self.on_close.clone();
        let escape = self.on_close.clone();
        let backdrop = self.on_close.clone();
        let label = format!("Close {}", self.label.to_lowercase());
        let viewer = div()
            .id(self.id)
            .key_context(KEY_CONTEXT)
            .track_focus(&focus)
            .focus_trap()
            .accessible(gpui::AccessInfo::new(gpui::AccessRole::Dialog, self.label.clone()).modal())
            .occlude()
            .relative()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .pt(px(56.0))
            .px(px(32.0))
            .pb(px(32.0))
            .bg(t.veil_deep)
            .veil_backdrop(t)
            .cursor(gpui::CursorStyle::Arrow)
            .on_action(move |_: &DialogCancel, window, cx| escape(window, cx))
            .on_click(move |_, window, cx| backdrop(window, cx))
            .children(self.controls)
            .child(
                div().absolute().top(px(58.0)).right(px(20.0)).child(
                    IconButton::new("viewer-close", IconName::X, label)
                        .variant(IconButtonVariant::Soft)
                        .size(IconButtonSize::Md)
                        .icon_size(16.0)
                        .tip(crate::tooltip::shortcut_tip("Close", if cfg!(target_os = "macos") { "Esc" } else { "Escape" }))
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            close(window, cx)
                        }),
                ),
            )
            .child(div().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).children(self.children));
        layer(window, viewer)
    }
}
