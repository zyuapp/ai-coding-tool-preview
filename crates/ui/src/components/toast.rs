//! The cards in the window's top-right corner (Toasts.tsx, banners-toasts.css), newest on top.
//! Each leaves on its own or at its X, but not while the pointer or the keyboard is on them; the
//! holder decides, told through `on_hold`.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    deferred, div, point, prelude::*, px, Animation, AnimationExt as _, App, ElementId, FontWeight, SharedString,
    StyledText, TextRun, Window,
};

use super::button::{Button, ButtonVariant, IconButton};
use super::spinner::Spinning;
use crate::icon::{Icon, IconName};
use crate::overlay::{placed, Placement};
use crate::style::{glass, GlassKind};
use crate::theme::ActiveTheme;

/// How a toast reads at a glance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastTone {
    /// Something on offer.
    Update,
    /// Something under way.
    Progress,
    Success,
    Error,
}

#[derive(Clone)]
pub struct ToastCard {
    pub id: u64,
    pub tone: ToastTone,
    pub title: SharedString,
    /// Commands in backticks read as code.
    pub message: Option<SharedString>,
    /// The one button a toast can carry.
    pub action: Option<SharedString>,
}

type IdHandler = Rc<dyn Fn(&u64, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct Toasts {
    toasts: Vec<ToastCard>,
    on_dismiss: Option<IdHandler>,
    on_action: Option<IdHandler>,
    on_hold: Option<HoldHandler>,
}

impl Toasts {
    /// `toasts` oldest first, as the workspace keeps them.
    pub fn new(toasts: Vec<ToastCard>) -> Self {
        Self { toasts, on_dismiss: None, on_action: None, on_hold: None }
    }

    pub fn on_dismiss(mut self, handler: impl Fn(&u64, &mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Rc::new(handler));
        self
    }

    pub fn on_action(mut self, handler: impl Fn(&u64, &mut Window, &mut App) + 'static) -> Self {
        self.on_action = Some(Rc::new(handler));
        self
    }

    /// Told when the pointer or keyboard focus arrives on the cards and when both have left.
    pub fn on_hold(mut self, handler: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_hold = Some(Rc::new(handler));
        self
    }
}

/// Splits a message on backticks: the odd parts are commands.
pub fn message_parts(text: &str) -> Vec<(String, bool)> {
    text.split('`').enumerate().filter(|(_, part)| !part.is_empty()).map(|(index, part)| (part.to_string(), index % 2 == 1)).collect()
}

fn message(text: &str, window: &Window, cx: &App) -> StyledText {
    let theme = cx.theme();
    let t = &theme.tokens;
    let style = window.text_style();
    let mut font = style.font();
    font.family = theme.typography.ui_family.clone();
    let mut mono = font.clone();
    mono.family = theme.typography.mono_family.clone();
    let mut joined = String::new();
    let mut runs = Vec::new();
    for (part, code) in message_parts(text) {
        joined.push_str(&part);
        runs.push(TextRun {
            len: part.len(),
            font: if code { mono.clone() } else { font.clone() },
            color: if code { t.code_ink.into() } else { t.muted_strong.into() },
            background_color: code.then(|| t.code_surface.into()),
            underline: None,
            strikethrough: None,
        });
    }
    StyledText::new(joined).with_runs(runs)
}

/// What holds the cards: the pointer over them, keyboard focus inside them.
#[derive(Clone, Copy, Default)]
struct Hold {
    pointer: bool,
    focus: bool,
}

impl Hold {
    fn held(self) -> bool {
        self.pointer || self.focus
    }
}

type HoldHandler = Rc<dyn Fn(&bool, &mut Window, &mut App)>;

/// Records a change to the hold and tells the holder when the cards become held or free.
fn change_hold(hold: &Cell<Hold>, next: Hold, on_hold: &Option<HoldHandler>, window: &mut Window, cx: &mut App) {
    let before = hold.replace(next).held();
    if before != next.held() {
        if let Some(handler) = on_hold.clone() {
            let held = next.held();
            window.defer(cx, move |window, cx| handler(&held, window, cx));
        }
    }
}

impl RenderOnce for Toasts {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let hold = window.use_keyed_state("toasts-hold", cx, |_, _| Rc::new(Cell::new(Hold::default()))).read(cx).clone();
        // A card closed under the pointer or the caret takes the hold with it, since nothing then says it left.
        if self.toasts.is_empty() {
            change_hold(&hold, Hold::default(), &self.on_hold, window, cx);
            return div().into_any_element();
        }
        let theme = cx.theme();
        let t = theme.tokens.clone();
        let m = theme.metrics;
        let viewport = window.viewport_size();
        let width = px(340.0).min(viewport.width - px(32.0));
        let stops: Vec<gpui::FocusHandle> = self
            .toasts
            .iter()
            .rev()
            .map(|toast| window.use_keyed_state(ElementId::NamedInteger("toast-focus".into(), toast.id), cx, |_, cx| cx.focus_handle().tab_stop(true)).read(cx).clone())
            .collect();
        let action_stops: Vec<gpui::FocusHandle> = self
            .toasts
            .iter()
            .rev()
            .map(|toast| window.use_keyed_state(ElementId::NamedInteger("toast-action-focus".into(), toast.id), cx, |_, cx| cx.focus_handle().tab_stop(true)).read(cx).clone())
            .collect();
        let focus = window.is_window_active() && stops.iter().chain(&action_stops).any(|stop| stop.is_focused(window));
        if focus != hold.get().focus {
            change_hold(&hold, Hold { focus, ..hold.get() }, &self.on_hold, window, cx);
        }
        let cards = self.toasts.iter().rev().enumerate().map(|(index, toast)| {
            let (icon, color) = match toast.tone {
                ToastTone::Update => (IconName::Download, t.accent),
                ToastTone::Progress => (IconName::LoaderCircle, t.info),
                ToastTone::Success => (IconName::CircleCheck, t.success),
                ToastTone::Error => (IconName::CircleAlert, t.danger),
            };
            let glyph = if toast.tone == ToastTone::Progress {
                Spinning::new(ElementId::NamedInteger("toast-spin".into(), toast.id), icon).size(px(16.0)).color(color).into_any_element()
            } else {
                Icon::new(icon).size(px(16.0)).color(color).into_any_element()
            };
            let mut text = div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .text_size(px(m.text_menu))
                        .font_weight(FontWeight::SEMIBOLD)
                        .line_height(gpui::relative(1.35))
                        .child(toast.title.clone()),
                );
            if let Some(body) = &toast.message {
                text = text.child(
                    div()
                        .text_size(px(m.text_meta))
                        .line_height(gpui::relative(1.45))
                        .text_color(t.muted_strong)
                        .child(message(body, window, cx)),
                );
            }
            if let (Some(label), Some(handler)) = (&toast.action, self.on_action.clone()) {
                let id = toast.id;
                text = text.child(
                    div().mt(px(7.0)).flex().child(
                        Button::new(ElementId::NamedInteger("toast-action".into(), id), label.clone())
                            .variant(ButtonVariant::Inverse)
                            .track_focus(&action_stops[index])
                            .on_click(move |_, window, cx| handler(&id, window, cx)),
                    ),
                );
            }
            let mut card = glass(div(), &t, GlassKind::Float, None, None)
                .flex()
                .items_start()
                .gap(px(10.0))
                .pl(px(12.0))
                .pr(px(8.0))
                .py(px(11.0))
                .text_color(t.ink)
                .child(div().flex_none().mt(px(1.0)).child(glyph))
                .child(text);
            if let Some(handler) = self.on_dismiss.clone() {
                let id = toast.id;
                let close = stops[index].clone();
                // Closing the card that holds the keyboard hands it to the card taking its place.
                let heir = stops.get(index + 1).or(index.checked_sub(1).and_then(|before| stops.get(before))).cloned();
                let on_escape = handler.clone();
                card = card
                    .on_key_down(move |event, window, cx| {
                        if event.keystroke.key != "escape" {
                            return;
                        }
                        cx.stop_propagation();
                        if close.contains_focused(window, cx) {
                            if let Some(heir) = &heir {
                                window.focus(heir, cx);
                            }
                        }
                        on_escape(&id, window, cx);
                    })
                    .child(
                        div().flex_none().mt(px(-6.0)).mr(px(-2.0)).child(
                            IconButton::new(ElementId::NamedInteger("toast-close".into(), id), IconName::X, "Dismiss")
                                .icon_size(14.0)
                                .track_focus(&stops[index])
                                .on_click(move |_, window, cx| handler(&id, window, cx)),
                        ),
                    );
            }
            let card = card.id(ElementId::NamedInteger("toast".into(), toast.id)).occlude();
            if t.reduced_motion {
                card.into_any_element()
            } else {
                card.with_animation(
                    ElementId::NamedInteger("toast-in".into(), toast.id),
                    Animation::new(Duration::from_millis(220)),
                    |card, delta| {
                        // The curve overshoots, so it is applied here rather than as the animation's easing.
                        let eased = crate::theme::tokens::cubic_bezier([0.2, 0.9, 0.3, 1.15], delta);
                        card.opacity(eased.min(1.0)).transform(gpui::ElementTransform::translate(px(14.0 * (1.0 - eased)), px(0.0)).scaled(0.98 + 0.02 * eased))
                    },
                )
                .into_any_element()
            }
        });
        let on_hold = self.on_hold.clone();
        let stack = div()
            .id("toasts")
            .accessible(gpui::AccessInfo::new(gpui::AccessRole::Region, "Notifications"))
            .w(width)
            .flex()
            .flex_col()
            .gap(px(8.0))
            .children(cards)
            .on_hover(move |hovered, window, cx| change_hold(&hold, Hold { pointer: *hovered, ..hold.get() }, &on_hold, window, cx));
        let origin = point(viewport.width - width - px(16.0), px(m.chrome_head + 10.0));
        deferred(placed(Placement::At { position: origin }, stack)).with_priority(80).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::message_parts;

    #[test]
    fn backticks_mark_commands() {
        assert_eq!(
            message_parts("Run `npm i -g x` then retry"),
            vec![("Run ".into(), false), ("npm i -g x".into(), true), (" then retry".into(), false)]
        );
    }
}
