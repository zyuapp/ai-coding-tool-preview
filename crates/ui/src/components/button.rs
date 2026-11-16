//! Buttons (Button.tsx and button.css): the text button in its variants, the icon button, the
//! toggle, and the segmented control.

use std::rc::Rc;

use gpui::{
    div, prelude::*, px, AnyElement, App, ClickEvent, CursorStyle, ElementId, FocusHandle, FontWeight, Rgba,
    SharedString, Window,
};

use crate::color::{hex, mix};
use crate::focus::keyboard_focus_visible;
use crate::icon::{Icon, IconName};
use crate::style::{glass, Corners, GlassKind, PaneFill, Raised};
use crate::theme::{ActiveTheme, Pane, Tokens};
use crate::tooltip::Tippable;

pub type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ButtonVariant {
    /// The accent fill.
    Primary,
    /// The one high-contrast fill.
    Inverse,
    #[default]
    Secondary,
    Soft,
    Danger,
    Ghost,
    Link,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ButtonSize {
    Sm,
    #[default]
    Md,
    Lg,
}

/// The fill a primary button takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FillTone {
    #[default]
    Accent,
    Info,
}

/// The fill a pressed toggle takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ToggleTone {
    Accent,
    #[default]
    Neutral,
}

/// What a control looks like at rest and under the pointer.
struct Look {
    fill: Option<Pane>,
    hover_fill: Option<Pane>,
    color: Rgba,
    hover_color: Rgba,
    weight: f32,
    rim: bool,
    glint: bool,
    opacity: f32,
}

#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    label: Option<SharedString>,
    icon: Option<IconName>,
    icon_size: f32,
    children: Vec<AnyElement>,
    variant: ButtonVariant,
    size: ButtonSize,
    pill: bool,
    busy: bool,
    spinning: bool,
    armed: bool,
    tone: FillTone,
    pressed: Option<bool>,
    toggle_tone: ToggleTone,
    disabled: bool,
    unsupported: bool,
    tip: Option<SharedString>,
    reason: Option<SharedString>,
    focus: Option<FocusHandle>,
    full_width: bool,
    keep_focus: bool,
    name: Option<SharedString>,
    group: Option<SharedString>,
    on_click: Option<ClickHandler>,
}

impl Button {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self::bare(id).label(label)
    }

    /// A button with no label yet: an icon-only square, or one built from children.
    pub fn bare(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            label: None,
            icon: None,
            icon_size: 14.0,
            children: Vec::new(),
            variant: ButtonVariant::Secondary,
            size: ButtonSize::Md,
            pill: false,
            busy: false,
            spinning: false,
            armed: false,
            tone: FillTone::Accent,
            pressed: None,
            toggle_tone: ToggleTone::Neutral,
            disabled: false,
            unsupported: false,
            tip: None,
            reason: None,
            focus: None,
            full_width: false,
            keep_focus: false,
            name: None,
            group: None,
            on_click: None,
        }
    }

    /// Names the button as a hover group, so its children can light with it.
    pub fn group(mut self, group: impl Into<SharedString>) -> Self {
        self.group = Some(group.into());
        self
    }

    /// What assistive technology calls it when its words alone do not say enough.
    pub fn name(mut self, name: impl Into<SharedString>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// A press leaves keyboard focus where it was.
    pub fn keep_focus(mut self) -> Self {
        self.keep_focus = true;
        self
    }

    /// An empty label leaves the button without one.
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into()).filter(|label| !label.is_empty());
        self
    }

    /// A leading icon. With no label or children, the button is a square around it.
    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn icon_size(mut self, size: f32) -> Self {
        self.icon_size = size;
        self
    }

    pub fn variant(mut self, variant: ButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    pub fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }

    pub fn pill(mut self, pill: bool) -> Self {
        self.pill = pill;
        self
    }

    /// Swaps the label for a spinner, keeping the button's width.
    pub fn busy(mut self, busy: bool) -> Self {
        self.busy = busy;
        self
    }

    /// The icon turns for as long as it is drawn.
    pub fn spinning(mut self, spinning: bool) -> Self {
        self.spinning = spinning;
        self
    }

    /// Danger's second ask: the destructive answer, filled.
    pub fn armed(mut self, armed: bool) -> Self {
        self.armed = armed;
        self
    }

    pub fn tone(mut self, tone: FillTone) -> Self {
        self.tone = tone;
        self
    }

    /// Makes it a toggle that stays down while what it turns on is on.
    pub fn pressed(mut self, pressed: bool) -> Self {
        self.pressed = Some(pressed);
        self
    }

    pub fn toggle_tone(mut self, tone: ToggleTone) -> Self {
        self.toggle_tone = tone;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// A command its computer cannot run: no fill, a quiet label, a dashed edge.
    pub fn unsupported(mut self, unsupported: bool) -> Self {
        self.unsupported = unsupported;
        self
    }

    pub fn tip(mut self, tip: impl Into<SharedString>) -> Self {
        self.tip = Some(tip.into());
        self
    }

    /// Why a disabled button cannot act, shown as soon as the pointer reaches it.
    pub fn reason(mut self, reason: impl Into<SharedString>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    pub fn track_focus(mut self, focus: &FocusHandle) -> Self {
        self.focus = Some(focus.clone());
        self
    }

    pub fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }

    fn look(&self, t: &Tokens) -> Look {
        let filled = matches!(self.variant, ButtonVariant::Primary | ButtonVariant::Inverse)
            || (self.variant == ButtonVariant::Danger && self.armed);
        let inactive = self.disabled && !self.busy;
        if filled {
            if inactive && !self.unsupported {
                let off = Pane::solid(t.surface_inverse_off);
                return Look {
                    fill: Some(off),
                    hover_fill: Some(off),
                    color: t.muted,
                    hover_color: t.muted,
                    weight: 600.0,
                    rim: false,
                    glint: false,
                    opacity: 1.0,
                };
            }
            let (fill, hover, color) = match self.variant {
                ButtonVariant::Primary => match self.tone {
                    FillTone::Accent => (t.accent, t.accent_hover, t.accent_ink),
                    FillTone::Info => (t.info_badge, mix(t.info_badge, 0.82, t.ink), t.badge_ink),
                },
                ButtonVariant::Inverse => (t.surface_inverse, t.surface_inverse_soft, t.ink_inverse),
                _ => (t.danger, mix(t.danger, 0.88, t.ink), hex(0xffffff)),
            };
            return Look {
                fill: Some(Pane::solid(fill)),
                hover_fill: Some(Pane::solid(if inactive { fill } else { hover })),
                color,
                hover_color: color,
                weight: 600.0,
                rim: false,
                glint: true,
                opacity: 1.0,
            };
        }
        let mut look = match self.variant {
            ButtonVariant::Secondary | ButtonVariant::Soft | ButtonVariant::Danger => {
                let danger = self.variant == ButtonVariant::Danger;
                Look {
                    fill: Some(Pane::solid(t.control)),
                    hover_fill: Some(Pane::solid(t.control_hover)),
                    color: if danger { t.danger } else { t.ink_dim },
                    hover_color: if danger { t.danger } else { t.ink },
                    weight: if self.variant == ButtonVariant::Soft { 520.0 } else { 560.0 },
                    rim: true,
                    glint: false,
                    opacity: 1.0,
                }
            }
            ButtonVariant::Ghost => Look {
                fill: None,
                hover_fill: Some(Pane::solid(t.surface_hover)),
                color: t.muted,
                hover_color: t.ink_dim,
                weight: 560.0,
                rim: false,
                glint: false,
                opacity: 1.0,
            },
            _ => Look {
                fill: None,
                hover_fill: None,
                color: t.link,
                hover_color: t.link,
                weight: 400.0,
                rim: false,
                glint: false,
                opacity: 1.0,
            },
        };
        if self.pressed == Some(true) {
            let accent = self.toggle_tone == ToggleTone::Accent;
            let fill = Pane::solid(if accent { t.accent_surface } else { t.surface_active });
            look.fill = Some(fill);
            look.hover_fill = Some(fill);
            look.rim = true;
            look.color = if accent { t.accent_soft } else { t.ink };
            look.hover_color = look.color;
        }
        if inactive {
            look.hover_fill = look.fill;
            look.hover_color = look.color;
            look.opacity = t.disabled_opacity;
        }
        if self.unsupported && self.disabled {
            look = Look {
                fill: None,
                hover_fill: None,
                color: t.quiet,
                hover_color: t.quiet,
                weight: look.weight,
                rim: false,
                glint: false,
                opacity: 1.0,
            };
        }
        look
    }
}

impl RenderOnce for Button {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let t = &theme.tokens;
        let m = &theme.metrics;
        let look = self.look(t);
        let link = self.variant == ButtonVariant::Link;
        let icon_only = self.icon.is_some() && self.label.is_none() && self.children.is_empty();
        let (height, radius, pad, font) = match self.size {
            ButtonSize::Sm => (24.0, 7.0, 8.0, m.text_meta),
            ButtonSize::Md => (28.0, 8.0, 10.0, m.text_meta),
            ButtonSize::Lg => (32.0, 9.0, 13.0, m.text_control),
        };
        let corners = if self.pill { Corners::Full } else { Corners::Radius(radius) };
        let interactive = !self.disabled && !self.busy;
        let focused = self.focus.as_ref().is_some_and(|focus| focus.is_focused(window)) && keyboard_focus_visible(cx);

        let name = self.name.clone().or_else(|| self.label.clone()).or_else(|| self.tip.clone()).unwrap_or_default();
        let role = if self.pressed.is_some() { gpui::AccessRole::ToggleButton } else { gpui::AccessRole::Button };
        let mut access = gpui::AccessInfo::new(role, name).disabled(!interactive);
        if let Some(pressed) = self.pressed {
            access = access.toggled(pressed);
        }
        let mut button = div()
            .id(self.id.clone())
            .accessible(access)
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap(px(6.0))
            .text_color(look.color)
            .whitespace_nowrap()
            .opacity(look.opacity);
        button = match corners {
            Corners::Full => button.rounded_full(),
            Corners::Radius(radius) => button.rounded(px(radius)),
        };
        if link {
            button = button.min_h(px(24.0)).px(px(2.0));
        } else {
            button = button
                .min_h(px(height))
                .border_1()
                .border_color(gpui::transparent_black())
                .text_size(px(font))
                .font_weight(FontWeight(look.weight))
                .line_height(gpui::relative(1.2));
            button = if icon_only { button.min_w(px(height)) } else { button.px(px(pad)) };
        }
        if self.full_width {
            button = button.w_full();
        }
        if let Some(group) = self.group.clone() {
            button = button.group(group);
        }
        if let Some(fill) = look.fill {
            button = button.pane(fill);
        }
        if look.rim {
            button = button.rim(t.glass_rim, corners);
        }
        if look.glint {
            button = button.glint(t, corners);
        }
        if self.unsupported && self.disabled {
            button = button.border_dashed().border_color(t.quiet);
        }
        if let Some(focus) = &self.focus {
            button = button.track_focus(focus);
        }
        if self.keep_focus {
            button = button.keep_focus_on_click();
        }
        if focused {
            let ring = if matches!(self.variant, ButtonVariant::Primary | ButtonVariant::Inverse) { look.color } else { t.focus_ring };
            button = button.child(ring_overlay(ring, corners));
        }
        if matches!(self.variant, ButtonVariant::Primary | ButtonVariant::Inverse) {
            button = button.focus_ring_color(look.color);
        }
        if interactive {
            let hover_fill = look.hover_fill;
            let hover_color = look.hover_color;
            if matches!(self.variant, ButtonVariant::Primary | ButtonVariant::Inverse) {
                button = button.active(|style| style.transform(gpui::ElementTransform::scale(0.97)));
            }
            button = button.cursor(CursorStyle::PointingHand).transition(crate::style::touch()).hover(move |style| {
                let style = style.text_color(hover_color);
                let style = if link { style.underline().text_underline_offset(px(3.0)) } else { style };
                match hover_fill {
                    Some(fill) => style.bg(fill.background()),
                    None => style,
                }
            });
            if let Some(handler) = self.on_click.clone() {
                button = button.on_click(move |event, window, cx| handler(event, window, cx));
            }
        } else if self.busy {
            button = button.cursor(CursorStyle::Arrow);
        }

        let mut content: Vec<AnyElement> = Vec::new();
        if let Some(icon) = self.icon {
            content.push(if self.spinning {
                crate::components::spinner::Spinning::new(ElementId::NamedChild(Box::new(self.id.clone()), "icon".into()), icon).size(px(self.icon_size)).into_any_element()
            } else {
                Icon::new(icon).size(px(self.icon_size)).into_any_element()
            });
        }
        if let Some(label) = self.label {
            content.push(label.into_any_element());
        }
        content.extend(self.children);

        if self.busy {
            let spinner_id = ElementId::NamedChild(Box::new(self.id.clone()), "spinner".into());
            button = button
                .child(div().flex().items_center().gap(px(6.0)).invisible().children(content))
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(crate::components::spinner::Spinner::new(spinner_id)),
                );
        } else {
            button = button.children(content);
        }

        if let Some(reason) = self.reason.filter(|_| self.disabled) {
            button = button.tip_reason(reason);
        } else if let Some(tip) = self.tip {
            button = button.tip(tip);
        }
        button
    }
}

impl ParentElement for Button {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

/// The 2px ring a focused control draws inside its edge.
pub(crate) fn ring_overlay(color: impl Into<gpui::Hsla>, corners: Corners) -> gpui::Div {
    let ring = div().absolute().top_0().left_0().size_full().border_2().border_color(color.into());
    match corners {
        Corners::Full => ring.rounded_full(),
        Corners::Radius(radius) => ring.rounded(px(radius)),
    }
}

/// A button that stays down while what it turns on is on.
pub fn toggle_button(id: impl Into<ElementId>, label: impl Into<SharedString>, pressed: bool) -> Button {
    Button::new(id, label).variant(ButtonVariant::Ghost).pressed(pressed)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum IconButtonVariant {
    #[default]
    Ghost,
    Soft,
    Glass,
    Inverse,
    Danger,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum IconButtonSize {
    Xs,
    #[default]
    Sm,
    Md,
    Lg,
}

/// A square, or a circle, around one glyph. Its label is its tooltip unless `tip` says otherwise.
#[derive(IntoElement)]
pub struct IconButton {
    id: ElementId,
    icon: IconName,
    icon_size: Option<f32>,
    label: SharedString,
    tip: Option<Option<SharedString>>,
    variant: IconButtonVariant,
    size: IconButtonSize,
    round: bool,
    pressed: bool,
    /// Whether it stays on or off, so it reads as a toggle even while off.
    toggle: bool,
    /// Left out of the Tab order, as `tabIndex={-1}` leaves it.
    skip_tab: bool,
    disabled: bool,
    reason: Option<SharedString>,
    focus: Option<FocusHandle>,
    on_click: Option<ClickHandler>,
    child: Option<AnyElement>,
    /// A glass button that tints what is under it without frosting it.
    clear: bool,
    /// Fill, border and colour under the pointer, in place of the variant's.
    hover_look: Option<(Rgba, Rgba, Rgba)>,
}

impl IconButton {
    pub fn new(id: impl Into<ElementId>, icon: IconName, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            icon,
            icon_size: None,
            label: label.into(),
            tip: None,
            variant: IconButtonVariant::Ghost,
            size: IconButtonSize::Sm,
            round: false,
            pressed: false,
            toggle: false,
            skip_tab: false,
            disabled: false,
            reason: None,
            focus: None,
            on_click: None,
            child: None,
            clear: false,
            hover_look: None,
        }
    }

    /// Takes this fill, border and colour under the pointer, as a container's `:hover` rule does.
    pub fn hover_look(mut self, fill: Rgba, border: Rgba, color: Rgba) -> Self {
        self.hover_look = Some((fill, border, color));
        self
    }

    /// Leaves what is under a glass button sharp.
    pub fn clear_backdrop(mut self) -> Self {
        self.clear = true;
        self
    }

    pub fn icon_size(mut self, size: f32) -> Self {
        self.icon_size = Some(size);
        self
    }

    pub fn tip(mut self, tip: impl Into<SharedString>) -> Self {
        self.tip = Some(Some(tip.into()));
        self
    }

    pub fn no_tip(mut self) -> Self {
        self.tip = Some(None);
        self
    }

    /// Leaves the button out of the Tab order; a click still reaches it.
    pub fn skip_tab_stop(mut self) -> Self {
        self.skip_tab = true;
        self
    }

    pub fn variant(mut self, variant: IconButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    pub fn size(mut self, size: IconButtonSize) -> Self {
        self.size = size;
        self
    }

    pub fn round(mut self, round: bool) -> Self {
        self.round = round;
        self
    }

    /// Down, or with its menu open.
    pub fn pressed(mut self, pressed: bool) -> Self {
        self.pressed = pressed;
        self.toggle = true;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn reason(mut self, reason: impl Into<SharedString>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    pub fn track_focus(mut self, focus: &FocusHandle) -> Self {
        self.focus = Some(focus.clone());
        self
    }

    /// Draws this in place of the icon, as a glyph of the caller's own.
    pub fn glyph(mut self, glyph: impl IntoElement) -> Self {
        self.child = Some(glyph.into_any_element());
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for IconButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = &cx.theme().tokens;
        let (side, radius) = match self.size {
            IconButtonSize::Xs => (24.0, 7.0),
            IconButtonSize::Sm => (28.0, 8.0),
            IconButtonSize::Md => (30.0, 8.0),
            IconButtonSize::Lg => (34.0, 9.0),
        };
        let corners = if self.round { Corners::Full } else { Corners::Radius(radius) };
        let icon_size = self.icon_size.unwrap_or(match self.size {
            IconButtonSize::Xs => 12.0,
            IconButtonSize::Sm => 14.0,
            IconButtonSize::Md => 16.0,
            IconButtonSize::Lg => 18.0,
        });
        let mut look = match self.variant {
            IconButtonVariant::Ghost => Look {
                fill: None,
                hover_fill: Some(Pane::solid(t.surface_hover)),
                color: t.quiet_strong,
                hover_color: t.ink_dim,
                weight: 400.0,
                rim: false,
                glint: false,
                opacity: 1.0,
            },
            IconButtonVariant::Soft => Look {
                fill: Some(Pane::solid(t.control)),
                hover_fill: Some(Pane::solid(t.control_hover)),
                color: t.ink_dim,
                hover_color: t.ink,
                weight: 400.0,
                rim: true,
                glint: false,
                opacity: 1.0,
            },
            IconButtonVariant::Glass => Look {
                fill: None,
                hover_fill: None,
                color: t.muted,
                hover_color: t.ink_soft,
                weight: 400.0,
                rim: false,
                glint: false,
                opacity: 1.0,
            },
            IconButtonVariant::Danger => Look {
                fill: None,
                hover_fill: Some(Pane::solid(t.danger_chip)),
                color: t.danger,
                hover_color: t.danger,
                weight: 400.0,
                rim: false,
                glint: false,
                opacity: 1.0,
            },
            IconButtonVariant::Inverse => Look {
                fill: Some(Pane::solid(t.surface_inverse)),
                hover_fill: Some(Pane::solid(t.surface_inverse_soft)),
                color: t.ink_inverse,
                hover_color: t.ink_inverse,
                weight: 600.0,
                rim: false,
                glint: true,
                opacity: 1.0,
            },
        };
        if self.pressed && self.variant != IconButtonVariant::Inverse {
            look.fill = Some(t.glass_control_hover);
            look.hover_fill = look.fill;
            look.rim = true;
            look.color = t.ink;
            look.hover_color = t.ink;
        }
        if self.disabled {
            if self.variant == IconButtonVariant::Inverse {
                look.fill = Some(Pane::solid(t.surface_inverse_off));
                look.glint = false;
                look.color = t.muted;
            } else {
                look.opacity = t.disabled_opacity;
            }
            look.hover_fill = look.fill;
            look.hover_color = look.color;
        }

        let base = div().id(self.id.clone());
        let mut button = if self.variant == IconButtonVariant::Glass {
            let mut surface = glass(div(), t, GlassKind::Capsule, None, None);
            if self.clear {
                surface.style().backdrop_filter = None;
            }
            let surface = match corners {
                Corners::Full => surface.rounded_full(),
                Corners::Radius(radius) => surface.rounded(px(radius)),
            };
            surface.border_color(t.line_glass).id(self.id.clone())
        } else {
            let mut stateful = base.relative().border_1().border_color(gpui::transparent_black());
            stateful = match corners {
                Corners::Full => stateful.rounded_full(),
                Corners::Radius(radius) => stateful.rounded(px(radius)),
            };
            if let Some(fill) = look.fill {
                stateful = stateful.pane(fill);
            }
            if look.rim {
                stateful = stateful.rim(t.glass_rim, corners);
            }
            if look.glint {
                stateful = stateful.glint(t, corners);
            }
            stateful
        };
        let mut access = gpui::AccessInfo::new(gpui::AccessRole::Button, self.label.clone()).disabled(self.disabled);
        if self.toggle {
            access = access.toggled(self.pressed);
        }
        if self.skip_tab {
            button = button.skip_tab_stop();
        }
        button = button
            .accessible(access)
            .size(px(side))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .text_color(look.color)
            .opacity(look.opacity);
        if let Some(focus) = &self.focus {
            button = button.track_focus(focus);
            if focus.is_focused(window) && keyboard_focus_visible(cx) {
                let ring = if self.variant == IconButtonVariant::Inverse { look.color } else { t.focus_ring };
                button = button.child(ring_overlay(ring, corners));
            }
        }
        if self.variant == IconButtonVariant::Inverse {
            button = button.focus_ring_color(look.color);
        }
        if !self.disabled {
            let mut hover_fill = look.hover_fill;
            let mut hover_color = look.hover_color;
            let mut hover_border = (self.variant == IconButtonVariant::Glass).then_some(t.line_bright);
            if let Some((fill, border, color)) = self.hover_look {
                hover_fill = Some(Pane::solid(fill));
                hover_border = Some(border);
                hover_color = color;
            }
            if self.variant == IconButtonVariant::Inverse {
                button = button.active(|style| style.transform(gpui::ElementTransform::scale(0.97)));
            }
            button = button.cursor(CursorStyle::PointingHand).transition(crate::style::icon_touch()).hover(move |style| {
                let style = style.text_color(hover_color);
                let style = match hover_border {
                    Some(border) => style.border_color(border),
                    None => style,
                };
                match hover_fill {
                    Some(fill) => style.bg(fill.background()),
                    None => style,
                }
            });
            if let Some(handler) = self.on_click {
                button = button.on_click(move |event, window, cx| handler(event, window, cx));
            }
        }
        button = match self.child {
            Some(child) => button.child(child),
            None => button.child(Icon::new(self.icon).size(px(icon_size))),
        };
        let tip = match self.tip {
            Some(tip) => tip,
            None => Some(self.label.clone()),
        };
        match (self.reason.filter(|_| self.disabled), tip) {
            (Some(reason), _) => button.tip_reason(reason),
            (None, Some(tip)) => button.tip(tip),
            (None, None) => button,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SegmentedVariant {
    /// A recessed track with a raised thumb.
    #[default]
    Track,
    /// A row of separate pills.
    Pills,
}

#[derive(Clone, Debug)]
pub struct SegmentedOption {
    pub value: SharedString,
    pub label: SharedString,
    pub icon: Option<IconName>,
    pub icon_size: f32,
    /// A quiet figure after the label, in the pills variant.
    pub count: Option<SharedString>,
    pub tip: Option<SharedString>,
    pub tone: Option<crate::theme::Tone>,
}

impl SegmentedOption {
    pub fn new(value: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self { value: value.into(), label: label.into(), icon: None, icon_size: 14.0, count: None, tip: None, tone: None }
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn icon_size(mut self, size: f32) -> Self {
        self.icon_size = size;
        self
    }

    pub fn count(mut self, count: impl Into<SharedString>) -> Self {
        self.count = Some(count.into());
        self
    }

    pub fn tip(mut self, tip: impl Into<SharedString>) -> Self {
        self.tip = Some(tip.into());
        self
    }

    pub fn tone(mut self, tone: crate::theme::Tone) -> Self {
        self.tone = Some(tone);
        self
    }
}

pub type ChangeHandler = Rc<dyn Fn(&SharedString, &mut Window, &mut App) + 'static>;

/// One choice of a few, as radios.
#[derive(IntoElement)]
pub struct Segmented {
    id: ElementId,
    options: Vec<SegmentedOption>,
    value: SharedString,
    variant: SegmentedVariant,
    on_change: Option<ChangeHandler>,
    focus: Option<FocusHandle>,
    fill: bool,
}

impl Segmented {
    pub fn new(id: impl Into<ElementId>, options: Vec<SegmentedOption>, value: impl Into<SharedString>) -> Self {
        Self { id: id.into(), options, value: value.into(), variant: SegmentedVariant::Track, on_change: None, focus: None, fill: false }
    }

    /// Each segment takes an equal share of the track, the chosen one filling its share to the edge.
    pub fn fill(mut self) -> Self {
        self.fill = true;
        self
    }

    /// The group's keyboard stop, which rings the chosen segment while the keyboard holds it.
    pub fn track_focus(mut self, focus: &FocusHandle) -> Self {
        self.focus = Some(focus.clone());
        self
    }

    pub fn variant(mut self, variant: SegmentedVariant) -> Self {
        self.variant = variant;
        self
    }

    pub fn on_change(mut self, handler: impl Fn(&SharedString, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }

    /// The value the arrow keys move to from `value`, wrapping.
    pub fn step(options: &[SegmentedOption], value: &str, forward: bool) -> Option<SharedString> {
        if options.is_empty() {
            return None;
        }
        let at = options.iter().position(|option| option.value.as_ref() == value).unwrap_or(0) as isize;
        let next = (at + if forward { 1 } else { -1 }).rem_euclid(options.len() as isize) as usize;
        Some(options[next].value.clone())
    }
}

impl RenderOnce for Segmented {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // The group is one stop, the arrows choose within it, as the page's segmented control is.
        let focus = self.focus.clone().unwrap_or_else(|| {
            let key = ElementId::NamedChild(Box::new(self.id.clone()), "focus".into());
            window.use_keyed_state(key, cx, |_, cx| cx.focus_handle().tab_stop(true)).read(cx).clone()
        });
        let held = focus.is_focused(window);
        let ringed = held && keyboard_focus_visible(cx);
        let theme = cx.theme();
        let t = &theme.tokens;
        let m = &theme.metrics;
        let pills = self.variant == SegmentedVariant::Pills;
        let options = Rc::new(self.options.clone());
        let mut group = div().id(self.id.clone()).flex().relative().track_focus(&focus);
        group = if pills {
            group.flex_wrap().gap(px(6.0))
        } else {
            group
                .gap(px(if self.fill { 0.0 } else { 2.0 }))
                .rounded(px(9.0))
                .p(px(if self.fill { 0.0 } else { 2.0 }))
                .pane(Pane::sheened(t.control_soft))
                .rim(t.glass_rim, Corners::Radius(9.0))
        };
        if let Some(handler) = self.on_change.clone() {
            let value = self.value.clone();
            let options = options.clone();
            group = group.on_key_down(move |event, window, cx| {
                let forward = match event.keystroke.key.as_str() {
                    "left" | "up" => false,
                    "right" | "down" => true,
                    _ => return,
                };
                if let Some(next) = Segmented::step(&options, &value, forward) {
                    cx.stop_propagation();
                    handler(&next, window, cx);
                }
            });
        }
        group.children(self.options.into_iter().enumerate().map(|(index, option)| {
            let chosen = option.value == self.value;
            let tone = option.tone.map(|tone| t.tone(tone));
            let mut segment = div()
                .id(ElementId::NamedInteger("segment".into(), index as u64))
                .skip_tab_stop()
                .accessible(gpui::AccessInfo::new(gpui::AccessRole::Tab, option.label.clone()).selected(chosen).active(chosen && held))
                .relative()
                .flex()
                .items_center()
                .text_size(px(m.text_meta))
                .cursor(CursorStyle::PointingHand)
                .transition(gpui::Transition::ease(120));
            if pills {
                let corners = Corners::Full;
                segment = segment
                    .min_h(px(26.0))
                    .gap(px(6.0))
                    .border_1()
                    .rounded_full()
                    .px(px(10.0))
                    .py(px(3.0));
                segment = if chosen {
                    let (border, fill, color) = match option.tone {
                        Some(crate::theme::Tone::Danger) => (t.danger, t.danger_surface, t.danger),
                        Some(crate::theme::Tone::Info) => (t.info, t.info_surface, t.info),
                        _ => (t.line_control, t.surface_active, t.ink),
                    };
                    segment
                        .border_color(border)
                        .bg(fill)
                        .text_color(color)
                        .font_weight(FontWeight::SEMIBOLD)
                        .rim(t.glass_rim, corners)
                } else {
                    segment
                        .border_color(t.line_strong)
                        .text_color(t.muted)
                        .font_weight(FontWeight::NORMAL)
                        .rim(t.glass_rim, corners)
                        .hover({
                            let (fill, color) = (t.surface_hover, t.ink_soft);
                            move |style| style.bg(fill).text_color(color)
                        })
                };
            } else {
                let radius = if self.fill { 9.0 } else { 7.0 };
                segment = segment.gap(px(7.0)).rounded(px(radius)).px(px(13.0)).py(px(if self.fill { 6.0 } else { 5.0 })).font_weight(FontWeight(560.0));
                if self.fill {
                    segment = segment.flex_1().justify_center();
                }
                segment = if chosen {
                    segment
                        .pane(Pane::sheened(t.surface_menu_hover))
                        .shadow(t.shadow_thumb.box_shadows())
                        .rim(t.glass_rim, Corners::Radius(radius))
                        .text_color(t.ink)
                } else {
                    let color = t.ink_dim;
                    segment.text_color(t.muted).hover(move |style| style.text_color(color))
                };
            }
            if let Some(icon) = option.icon {
                let glyph = Icon::new(icon).size(px(option.icon_size));
                segment = segment.child(if chosen && !pills { glyph.color(t.accent) } else { glyph });
            }
            segment = segment.child(option.label.clone());
            if chosen && ringed {
                segment = segment.child(ring_overlay(t.focus_ring, if pills { Corners::Full } else { Corners::Radius(7.0) }));
            }
            if let Some(count) = option.count.clone() {
                let color = if chosen { tone.map(|tone| tone.text).unwrap_or(t.ink) } else { t.quiet };
                segment = segment.child(crate::style::tabular_nums(div().text_color(color)).child(count));
            }
            if let Some(handler) = self.on_change.clone() {
                let value = option.value.clone();
                segment = segment.on_click(move |_, window, cx| handler(&value, window, cx));
            }
            match option.tip {
                Some(tip) => segment.tip(tip),
                None => segment,
            }
        }))
    }
}
