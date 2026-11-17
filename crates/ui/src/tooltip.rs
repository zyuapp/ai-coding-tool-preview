//! One tooltip for the whole window (TooltipLayer.tsx), moved to whatever the pointer rests on.
//! A control opts in with `.tip(text)`; the app's root draws `tooltip_layer()` last.
//!
//! The first tip waits 400ms; once one is open, the next shows after 60ms so reading along a
//! toolbar does not wait each time. A disabled control's reason shows at once and lingers 300ms
//! so the pointer can cross onto it. A press, a key that is not a modifier, or a scroll hides it.

use std::hash::{Hash, Hasher};
use std::time::Duration;

use gpui::{
    deferred, div, prelude::FluentBuilder as _, px, AnyElement, AnyWindowHandle, App, Bounds, DispatchPhase, Edges, Element,
    ElementId, FontWeight, Global, GlobalElementId, Hitbox, HitboxBehavior, HitboxId, InspectorElementId, InteractiveElement,
    IntoElement, KeyDownEvent, LayoutId, MouseDownEvent, MouseMoveEvent, ParentElement, Pixels, ScrollWheelEvent,
    SharedString, Stateful, StatefulInteractiveElement, Style, Styled, Task, WeakFocusHandle, Window,
};

use crate::components::kbd::Kbd;
use crate::overlay::{placed, Placement};
use crate::style::{glass, GlassKind};
use crate::theme::ActiveTheme;

const DELAY: Duration = Duration::from_millis(400);
const NEXT: Duration = Duration::from_millis(60);
const LINGER: Duration = Duration::from_millis(300);

#[derive(Clone, Debug)]
struct Tip {
    key: u64,
    text: SharedString,
    anchor: Bounds<Pixels>,
    hoverable: bool,
    /// The control's hitbox in the latest frame, which says whether it is still drawn.
    hitbox: HitboxId,
    /// The focus that showed the tip, which it lasts only as long as.
    focus: Option<WeakFocusHandle>,
}

#[derive(Default)]
pub struct Tooltips {
    window: Option<AnyWindowHandle>,
    hovered: Option<u64>,
    shown: Option<Tip>,
    pending: Option<Task<()>>,
    linger: Option<Task<()>>,
    on_tip: bool,
    /// The control keyboard focus reached, which explains itself at once.
    focused: Option<u64>,
    /// The window's size and whether it was active when the tip showed: a resize moves what it
    /// points at, and the window going to the background takes it away.
    shown_in: Option<(gpui::Size<Pixels>, bool)>,
    /// The text the hovered control's tip last showed, or was asked to.
    entered: Option<SharedString>,
}

impl Global for Tooltips {}

impl Tooltips {
    fn ensure(cx: &mut App) {
        if cx.try_global::<Tooltips>().is_none() {
            cx.set_global(Tooltips::default());
        }
    }

    /// Hides the tooltip, as anything that moves what it points at should.
    pub fn hide(cx: &mut App) {
        Self::ensure(cx);
        let state = cx.global_mut::<Tooltips>();
        state.pending = None;
        state.linger = None;
        state.on_tip = false;
        let window = state.window;
        if state.shown.take().is_some() {
            if let Some(window) = window {
                let _ = window.update(cx, |_, window, _| window.refresh());
            }
        }
    }

    fn enter(tip: Tip, window: &mut Window, cx: &mut App) {
        Self::ensure(cx);
        let handle = window.window_handle();
        let state = cx.global_mut::<Tooltips>();
        state.hovered = Some(tip.key);
        state.entered = Some(tip.text.clone());
        state.linger = None;
        if state.shown.as_ref().is_some_and(|shown| shown.key == tip.key) {
            return;
        }
        let open = state.shown.is_some() && state.window == Some(handle);
        let delay = if open || tip.hoverable { NEXT } else { DELAY };
        state.window = Some(handle);
        state.shown_in = Some((window.viewport_size(), window.is_window_active()));
        let key = tip.key;
        let pending = window.spawn(cx, async move |cx| {
            cx.background_executor().timer(delay).await;
            let _ = cx.update(|window, cx| {
                let state = cx.global_mut::<Tooltips>();
                if state.hovered != Some(key) {
                    return;
                }
                state.shown = Some(tip);
                state.pending = None;
                window.refresh();
            });
        });
        cx.global_mut::<Tooltips>().pending = Some(pending);
    }

    fn leave(key: u64, window: &mut Window, cx: &mut App) {
        Self::ensure(cx);
        let state = cx.global_mut::<Tooltips>();
        if state.hovered != Some(key) {
            return;
        }
        state.hovered = None;
        let hoverable = state.shown.as_ref().is_some_and(|shown| shown.key == key && shown.hoverable);
        if hoverable {
            let linger = window.spawn(cx, async move |cx| {
                cx.background_executor().timer(LINGER).await;
                let _ = cx.update(|_, cx| {
                    let state = cx.global::<Tooltips>();
                    if state.hovered.is_none() && !state.on_tip {
                        Tooltips::hide(cx);
                    }
                });
            });
            cx.global_mut::<Tooltips>().linger = Some(linger);
            return;
        }
        // Another tipped control may take over within this same pointer move; only hide if none did.
        cx.defer(move |cx| {
            let state = cx.global::<Tooltips>();
            if state.hovered.is_none() {
                Tooltips::hide(cx);
            }
        });
    }
}

/// Draws the window again once this frame is done, as the layer may already have been drawn in it.
fn redraw_after(window: AnyWindowHandle, cx: &mut App) {
    cx.defer(move |cx| {
        let _ = window.update(cx, |_, window, _| window.refresh());
    });
}

fn key_for(id: &GlobalElementId) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut hasher);
    hasher.finish()
}

/// Covers its parent and reports the pointer resting on it.
struct TipZone {
    text: SharedString,
    hoverable: bool,
    /// The tip repeats the control's own text, so it shows only while that text is cut short.
    overflow: bool,
    /// A press that changes the tip's text under the pointer shows the tip again, as a control
    /// whose content is replaced under the pointer does.
    again: bool,
    /// The control's border, which the zone (laid out inside it) reaches out over: a tip is placed
    /// against the control's border box.
    border: Edges<Pixels>,
}

impl IntoElement for TipZone {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TipZone {
    /// The control's text style, which layout runs inside.
    type RequestLayoutState = Option<gpui::TextStyle>;
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(ElementId::Name("tip-zone".into()))
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
        let mut style = Style { position: gpui::Position::Absolute, ..Style::default() };
        style.inset.top = px(0.0).into();
        style.inset.left = px(0.0).into();
        style.size.width = gpui::relative(1.0).into();
        style.size.height = gpui::relative(1.0).into();
        (window.request_layout(style, [], cx), self.overflow.then(|| window.text_style()))
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        text_style: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let outer = bounds.extend(self.border);
        let hitbox = window.insert_hitbox(outer, HitboxBehavior::Normal);
        if let Some(id) = id {
            Tooltips::ensure(cx);
            let key = key_for(id);
            // Focus reached by keyboard explains itself; a click already said what the control does.
            let focused = window.prepainting_in_focused()
                && crate::focus::keyboard_focus_visible(cx)
                && text_style.as_ref().is_none_or(|style| cut_short(&self.text, style, bounds.size.width, window));
            let focus = window.focused(cx).map(|focus| focus.downgrade());
            let handle = window.window_handle();
            let shown_in = (window.viewport_size(), window.is_window_active());
            let state = cx.global_mut::<Tooltips>();
            if focused && state.focused != Some(key) {
                state.focused = Some(key);
                state.pending = None;
                state.linger = None;
                state.window = Some(handle);
                state.shown_in = Some(shown_in);
                state.shown = Some(Tip { key, text: self.text.clone(), anchor: outer, hoverable: self.hoverable, hitbox: hitbox.id, focus });
                redraw_after(handle, cx);
            } else if !focused && state.focused == Some(key) {
                state.focused = None;
                if state.hovered != Some(key) && state.shown.as_ref().is_some_and(|shown| shown.key == key) {
                    state.shown = None;
                    redraw_after(handle, cx);
                }
            }
            let state = cx.global_mut::<Tooltips>();
            if let Some(shown) = state.shown.as_mut().filter(|shown| shown.key == key) {
                shown.anchor = outer;
                shown.text = self.text.clone();
                shown.hitbox = hitbox.id;
                state.entered = Some(self.text.clone());
            }
            let retell = self.again
                && state.hovered == Some(key)
                && state.shown.is_none()
                && state.pending.is_none()
                && state.entered.as_ref() != Some(&self.text);
            if retell {
                let tip = Tip { key, text: self.text.clone(), anchor: outer, hoverable: self.hoverable, hitbox: hitbox.id, focus: None };
                Tooltips::enter(tip, window, cx);
            }
        }
        hitbox
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        text_style: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        _cx: &mut App,
    ) {
        let Some(id) = id else { return };
        let key = key_for(id);
        let hitbox = hitbox.clone();
        let hitbox_id = hitbox.id;
        let text = self.text.clone();
        let hoverable = self.hoverable;
        let style = text_style.take();
        let outer = bounds.extend(self.border);
        window.on_mouse_event(move |_: &MouseMoveEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            let hovered = hitbox.is_hovered(window);
            let current = cx.try_global::<Tooltips>().is_some_and(|state| state.hovered == Some(key));
            if hovered && !current && style.as_ref().is_some_and(|style| !cut_short(&text, style, bounds.size.width, window)) {
                return;
            }
            if hovered && !current {
                Tooltips::enter(Tip { key, text: text.clone(), anchor: outer, hoverable, hitbox: hitbox.id, focus: None }, window, cx);
            } else if !hovered && current {
                Tooltips::leave(key, window, cx);
            }
        });
        if hoverable {
            let text = self.text.clone();
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble && outer.contains(&event.position) {
                    Tooltips::ensure(cx);
                    cx.global_mut::<Tooltips>().hovered = Some(key);
                    cx.global_mut::<Tooltips>().shown_in = Some((window.viewport_size(), window.is_window_active()));
                    let tip = Tip { key, text: text.clone(), anchor: outer, hoverable: true, hitbox: hitbox_id, focus: None };
                    cx.global_mut::<Tooltips>().shown = Some(tip);
                    window.refresh();
                }
            });
        }
    }
}

/// Whether `text`, set in `style`, is wider than `width`.
fn cut_short(text: &SharedString, style: &gpui::TextStyle, width: Pixels, window: &mut Window) -> bool {
    let font_size = style.font_size.to_pixels(window.rem_size());
    let run = style.to_run(text.len());
    let line = window.text_system().shape_line(text.clone(), font_size, &[run], None);
    line.width > width + px(0.5)
}

/// Opting a control into the window's tooltip.
pub trait Tippable: ParentElement + InteractiveElement + Styled + Sized {
    /// Shows `text` while the pointer rests on the control. A trailing `(keys)` draws as key caps.
    fn tip(mut self, text: impl Into<SharedString>) -> Self {
        let text = text.into();
        let border = border_of(&mut self);
        self.accessible_name(spoken(&text)).child(TipZone { text, hoverable: false, overflow: false, again: false, border })
    }

    /// Like `tip`, and shown again when a press changes `text` while the pointer stays.
    fn tip_again(mut self, text: impl Into<SharedString>) -> Self {
        let text = text.into();
        let border = border_of(&mut self);
        self.accessible_name(spoken(&text)).child(TipZone { text, hoverable: false, overflow: false, again: true, border })
    }

    /// Shows `text`, the control's own text, only while it is cut short.
    fn tip_overflow(mut self, text: impl Into<SharedString>) -> Self {
        let border = border_of(&mut self);
        self.child(TipZone { text: text.into(), hoverable: false, overflow: true, again: false, border })
    }

    /// The reason a disabled control cannot act: shown at once, and on a press, and readable.
    fn tip_reason(mut self, text: impl Into<SharedString>) -> Self {
        let border = border_of(&mut self);
        self.child(TipZone { text: text.into(), hoverable: true, overflow: false, again: false, border })
    }
}

/// The border a control has been given so far, in pixels.
fn border_of(control: &mut impl Styled) -> Edges<Pixels> {
    let widths = &control.style().border_widths;
    let side = |width: Option<gpui::AbsoluteLength>| match width {
        Some(gpui::AbsoluteLength::Pixels(width)) => width,
        _ => px(0.0),
    };
    Edges { top: side(widths.top), right: side(widths.right), bottom: side(widths.bottom), left: side(widths.left) }
}

impl Tippable for Stateful<gpui::Div> {}

/// A tip as read aloud: its words without the key caps.
fn spoken(text: &SharedString) -> SharedString {
    match text.strip_suffix(')').and_then(|rest| rest.rsplit_once(" (")) {
        Some((label, keys)) if names_keys(keys) => label.to_string().into(),
        _ => text.clone(),
    }
}

/// A control's tip: its name with the keystroke that does the same.
pub fn shortcut_tip(label: &str, keys: &str) -> SharedString {
    format!("{label} ({keys})").into()
}

/// Whether a trailing parenthetical names keys rather than words.
fn names_keys(text: &str) -> bool {
    const NAMED: &[&str] = &["Esc", "Escape", "Enter", "Tab", "Space", "Backspace", "Delete"];
    const GLYPHS: &[&str] = &["↩", "⇥", "⌫", "⌦", "←", "→", "↑", "↓", "⇞", "⇟", "↖", "↘"];
    if text.starts_with(['⌃', '⌥', '⇧', '⌘']) || NAMED.contains(&text) || GLYPHS.contains(&text) {
        return true;
    }
    for modifier in ["Ctrl+", "Meta+", "Alt+", "Shift+"] {
        if text.starts_with(modifier) && text.len() > modifier.len() {
            return true;
        }
    }
    if let Some(number) = text.strip_prefix('F') {
        return (1..=2).contains(&number.len()) && number.chars().all(|ch| ch.is_ascii_digit());
    }
    false
}

/// Splits a tip into its parts, each a label and the keys it names.
pub fn tip_parts(text: &str) -> Vec<(String, Option<String>)> {
    text.split(" · ")
        .map(|part| {
            if part.ends_with(')') {
                if let Some(open) = part.rfind(" (") {
                    let keys = &part[open + 2..part.len() - 1];
                    if !keys.contains(['(', ')']) && names_keys(keys) {
                        return (part[..open].to_string(), Some(keys.to_string()));
                    }
                }
            }
            (part.to_string(), None)
        })
        .collect()
}

/// The tip's words, with keys as caps. Words run together in one text that may wrap; a flex row
/// sizes text to its full line, so each run of words must be free to shrink.
fn tip_content(text: &str) -> Vec<AnyElement> {
    let mut children = Vec::new();
    let mut words = String::new();
    for (index, (label, keys)) in tip_parts(text).into_iter().enumerate() {
        if index > 0 {
            words.push_str(" · ");
        }
        words.push_str(&label);
        if let Some(keys) = keys {
            words.push(' ');
            children.push(div().min_w_0().child(std::mem::take(&mut words)).into_any_element());
            children.push(div().flex_none().child(Kbd::inline([keys])).into_any_element());
        }
    }
    if !words.is_empty() {
        children.push(div().min_w_0().child(words).into_any_element());
    }
    children
}

/// Draws the tip, if one is showing in this window. The app's root adds it as its last child.
pub fn tooltip_layer(window: &mut Window, cx: &mut App) -> impl IntoElement {
    Tooltips::ensure(cx);
    let handle = window.window_handle();
    let state = cx.global::<Tooltips>();
    // A pointer that left the window, or the control the tip names, takes a hover tip with it, even
    // when no move inside the window said so. A tip keyboard focus showed stays with the focus.
    let pointer = window.mouse_position();
    let left_window = pointer.x < px(-10_000.0);
    let shown = state.shown.clone().filter(|tip| state.window == Some(handle) && (tip.focus.is_some() || (!left_window && (tip.hoverable || tip.anchor.contains(&pointer)))));
    let theme = cx.theme();
    let tokens = &theme.tokens;
    let metrics = &theme.metrics;
    let tip = shown.map(|tip| {
        let body = glass(div(), tokens, GlassKind::Popover, Some(8.0), None)
            .when(crate::style::frame_glass(&theme.environment, false, true), |body| body.bg(tokens.surface_glass))
            .id("tooltip")
            .map(crate::style::glass_in)
            .max_w(px(280.0).min(window.viewport_size().width - px(32.0)))
            .px(px(9.0))
            .py(px(5.0))
            .text_color(tokens.ink_soft)
            .text_size(px(metrics.text_meta))
            .line_height(gpui::relative(1.35))
            .font_weight(FontWeight::NORMAL)
            .text_center()
            // `width: max-content` up to the cap: the label and its keys share a line until the label
            // itself has to wrap. A wrapping flex row would size to its widest item and break them apart.
            .child(div().flex().justify_center().items_center().children(tip_content(&tip.text)))
            .when(tip.hoverable, |this| {
                this.on_hover(|hovered, _, cx| {
                    Tooltips::ensure(cx);
                    cx.global_mut::<Tooltips>().on_tip = *hovered;
                    if !*hovered && cx.global::<Tooltips>().hovered.is_none() {
                        Tooltips::hide(cx);
                    }
                })
            });
        deferred(placed(Placement::Tooltip { anchor: tip.anchor }, body)).with_priority(100)
    });
    TooltipWatch { tip: tip.map(IntoElement::into_any_element) }
}

/// Whether the shown tip no longer belongs on screen once this frame is drawn: its control is not
/// drawn, is covered, or lost the focus that showed it, or the window was resized or went to the
/// background, as a page's tip hides when what is under the pointer or focus changes.
fn gone(window: &Window, cx: &App) -> bool {
    let Some(state) = cx.try_global::<Tooltips>() else { return false };
    let Some(tip) = state.shown.as_ref().filter(|_| state.window == Some(window.window_handle())) else { return false };
    if !window.frame_has_hitbox(tip.hitbox) {
        return true;
    }
    if let Some((size, active)) = state.shown_in {
        if size != window.viewport_size() || (active && !window.is_window_active()) {
            return true;
        }
    }
    match &tip.focus {
        Some(focus) => window.focused(cx).is_none_or(|focused| *focus != focused),
        None => !tip.hoverable && !tip.hitbox.is_hovered(window),
    }
}

/// Hides the tip on a press, a key, a scroll, or a move off what it explains.
struct TooltipWatch {
    tip: Option<AnyElement>,
}

impl IntoElement for TooltipWatch {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TooltipWatch {
    type RequestLayoutState = ();
    type PrepaintState = ();

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
        let children: Vec<LayoutId> = self.tip.iter_mut().map(|tip| tip.request_layout(window, cx)).collect();
        let style = Style { position: gpui::Position::Absolute, ..Style::default() };
        (window.request_layout(style, children, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(tip) = self.tip.as_mut() {
            tip.prepaint(window, cx);
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(tip) = self.tip.as_mut() {
            tip.paint(window, cx);
        }
        if gone(window, cx) {
            let focus = window.focused(cx);
            let state = cx.global_mut::<Tooltips>();
            if let Some(tip) = state.shown.take() {
                if state.hovered == Some(tip.key) {
                    state.hovered = None;
                }
                // Focus that left lets the control explain itself again when it returns.
                if tip.focus.is_some_and(|shown| focus.is_none_or(|focus| shown != focus)) && state.focused == Some(tip.key) {
                    state.focused = None;
                }
            }
            redraw_after(window.window_handle(), cx);
        }
        // A control that went away while explained sends no leave of its own, so any move off the
        // place the tip names hides it.
        window.on_mouse_event(|event: &MouseMoveEvent, phase, _window, cx| {
            if phase != DispatchPhase::Capture {
                return;
            }
            let Some(state) = cx.try_global::<Tooltips>() else { return };
            let stale = state.shown.as_ref().is_some_and(|tip| !tip.hoverable && !tip.anchor.contains(&event.position));
            if stale {
                // Cleared after this move is handled, so a control it entered keeps its pending tip.
                cx.defer(|cx| {
                    let state = cx.global_mut::<Tooltips>();
                    let key = state.shown.as_ref().map(|tip| tip.key);
                    if key.is_some() {
                        // Still "hovered" after the pointer left means its control was not drawn to say so.
                        if state.hovered == key {
                            state.hovered = None;
                        }
                        state.shown = None;
                        if let Some(window) = state.window {
                            let _ = window.update(cx, |_, window, _| window.refresh());
                        }
                    }
                });
            }
        });
        window.on_mouse_event(|event: &MouseDownEvent, phase, _window, cx| {
            if phase != DispatchPhase::Capture {
                return;
            }
            let on_tip = cx.try_global::<Tooltips>().is_some_and(|state| state.on_tip);
            let _ = event;
            if !on_tip {
                Tooltips::hide(cx);
            }
        });
        window.on_mouse_event(|_: &ScrollWheelEvent, phase, _window, cx| {
            if phase == DispatchPhase::Capture && cx.try_global::<Tooltips>().is_some_and(|state| state.shown.is_some()) {
                Tooltips::hide(cx);
            }
        });
        window.on_key_event(|event: &KeyDownEvent, phase, _window, cx| {
            if phase == DispatchPhase::Capture && cx.try_global::<Tooltips>().is_some_and(|state| state.shown.is_some()) {
                let modifier = matches!(event.keystroke.key.as_str(), "shift" | "control" | "alt" | "platform" | "function" | "capslock");
                if !modifier {
                    Tooltips::hide(cx);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_keys_become_caps_and_words_stay_words() {
        assert_eq!(tip_parts("Close (Esc)"), vec![("Close".into(), Some("Esc".into()))]);
        assert_eq!(tip_parts("New thread (⌘N) · Search (⌘K)"), vec![
            ("New thread".into(), Some("⌘N".into())),
            ("Search".into(), Some("⌘K".into())),
        ]);
        assert_eq!(tip_parts("Model (beta)"), vec![("Model (beta)".into(), None)]);
        assert_eq!(tip_parts("Toggle (F12)"), vec![("Toggle".into(), Some("F12".into()))]);
    }
}
