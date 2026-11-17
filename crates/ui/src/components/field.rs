//! Fields (Field.tsx and field.css): the search box, the text field, the label-hint-error group,
//! the switch and the checkbox. Text is edited by a `TextInput` the caller owns.

use std::rc::Rc;

use gpui::{
    div, prelude::*, px, AnyElement, App, ClickEvent, CursorStyle, ElementId, Entity, FocusHandle,
    FontWeight, Hsla, Pixels, SharedString, Window,
};

use super::button::{IconButton, IconButtonSize};
use crate::focus::keyboard_focus_visible;
use crate::icon::{Icon, IconName};
use crate::input::TextInput;
use crate::style::{Corners, Raised};
use crate::theme::{ActiveTheme, ThemeVariant, Tokens};
use crate::tooltip::Tippable;

/// The halo as a ring outside the edge, as CSS draws a box shadow: never under a translucent fill.
pub(crate) fn halo_ring(radius: f32, t: &Tokens) -> gpui::Div {
    div().absolute().top(px(-3.0)).left(px(-3.0)).right(px(-3.0)).bottom(px(-3.0)).rounded(px(radius + 3.0)).border(px(3.0)).border_color(t.focus_halo())
}

fn input_focused(input: &Entity<TextInput>, window: &Window, cx: &App) -> bool {
    input.read(cx).is_focused(window) && keyboard_focus_visible(cx)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SearchVariant {
    /// Sits on a page.
    #[default]
    Field,
    /// Heads a menu or picker.
    Menu,
    /// The whole head of a command palette.
    Palette,
    /// The head of the model library: a palette's head at a menu's size, room kept for its close button.
    Library,
}

/// A magnifier, the input, and an optional clear button. Clicking anywhere but the clear button
/// focuses the input.
#[derive(IntoElement)]
pub struct SearchField {
    id: ElementId,
    input: Entity<TextInput>,
    variant: SearchVariant,
    on_clear: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>>,
    clear_tip: Option<SharedString>,
    /// A plain text input, with no native cancel glyph while it holds text.
    plain: bool,
}

impl SearchField {
    pub fn new(id: impl Into<ElementId>, input: &Entity<TextInput>) -> Self {
        Self { id: id.into(), input: input.clone(), variant: SearchVariant::Field, on_clear: None, clear_tip: None, plain: false }
    }

    pub fn variant(mut self, variant: SearchVariant) -> Self {
        self.variant = variant;
        self
    }

    /// Draws a clear button while the field holds text.
    pub fn on_clear(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_clear = Some(Rc::new(handler));
        self
    }

    pub fn clear_tip(mut self, tip: impl Into<SharedString>) -> Self {
        self.clear_tip = Some(tip.into());
        self
    }

    /// An input of `type="text"`: no native cancel glyph.
    pub fn plain(mut self) -> Self {
        self.plain = true;
        self
    }
}

/// The native search cancel glyph (`::-webkit-search-cancel-button`): a bold cross at 40% opacity,
/// 75% under the pointer, that empties the field. Chromium sizes it with the field's font and draws
/// it white on a dark colour scheme, and on a light one in a blue that deepens towards its foot.
fn native_cancel(input: Entity<TextInput>, font: f32, dark: bool) -> impl IntoElement {
    let (top, foot) = if dark { (gpui::rgb(0xffffff), gpui::rgb(0xffffff)) } else { (gpui::rgb(0x2c64aa), gpui::rgb(0x1f4475)) };
    let target = input.clone();
    let scale = font / 14.0;
    div()
        .id("cancel")
        .flex_none()
        .w(px(16.0 * scale))
        .h(px(16.0 * scale))
        .cursor(CursorStyle::PointingHand)
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, window, cx| {
            target.update(cx, |input, cx| {
                input.set_text("", cx);
                input.focus(window);
            });
        })
        .child(
            gpui::canvas(
                |bounds, window, _| window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal),
                move |bounds, hitbox, window, _| {
                    let alpha = if hitbox.is_hovered(window) { 0.75 } else { 0.4 };
                    // One outline for both strokes, so where they cross is no darker than either.
                    let (arm, thick) = (4.0 * std::f32::consts::SQRT_2 * scale, 1.1 * scale);
                    let plus = [(thick, thick), (thick, arm), (-thick, arm), (-thick, thick), (-arm, thick), (-arm, -thick), (-thick, -thick), (-thick, -arm), (thick, -arm), (thick, -thick), (arm, -thick), (arm, thick)];
                    let centre = bounds.center();
                    let corner = |(u, v): (f32, f32)| centre + gpui::point(px((u - v) / std::f32::consts::SQRT_2), px((u + v) / std::f32::consts::SQRT_2));
                    let mut path = gpui::PathBuilder::fill();
                    path.move_to(corner(plus[0]));
                    for point in &plus[1..] {
                        path.line_to(corner(*point));
                    }
                    path.close();
                    if let Ok(path) = path.build() {
                        let stop = |color: gpui::Rgba, at| gpui::linear_color_stop(gpui::Rgba { a: alpha, ..color }, at);
                        window.paint_path(path, gpui::linear_gradient(180.0, stop(top, 0.0), stop(foot, 1.0)));
                    }
                },
            )
            .size_full(),
        )
}

impl RenderOnce for SearchField {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let t = &theme.tokens;
        let m = &theme.metrics;
        let filled = !self.input.read(cx).is_empty();
        let headed = matches!(self.variant, SearchVariant::Palette | SearchVariant::Library);
        let cancel = filled && !headed && !self.plain && self.on_clear.is_none();
        let focused = !headed && input_focused(&self.input, window, cx);
        let (gap, pad, font, icon) = match self.variant {
            SearchVariant::Field => (8.0, 10.0, m.text_control, 14.0),
            SearchVariant::Menu => (6.0, 8.0, m.text_menu, 14.0),
            SearchVariant::Palette => (10.0, 15.0, m.text_content, 16.0),
            SearchVariant::Library => (9.0, 16.0, m.text_menu, 14.0),
        };
        let icon_color = if self.variant == SearchVariant::Field { t.quiet_strong } else { t.quiet };
        let focus_target = self.input.clone();
        let input_box = div()
            .id("box")
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(gap))
            .px(px(pad))
            .when(self.variant == SearchVariant::Library, |this| this.pr(px(44.0)))
            .cursor(CursorStyle::IBeam)
            .skip_tab_stop()
            .accessible(gpui::AccessInfo::new(gpui::AccessRole::Group, ""))
            .on_click(move |_, window, cx| focus_target.read(cx).focus(window))
            .child(Icon::new(IconName::Search).size(px(icon)).color(icon_color))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(t.ink)
                    .text_size(px(font))
                    .when(self.variant == SearchVariant::Field, |this| this.py(px(8.0)))
                    .when(cancel, |this| this.flex().items_center().child(div().flex_1().min_w_0().child(self.input.clone())).child(native_cancel(self.input.clone(), font, theme.variant() == ThemeVariant::Dark)))
                    .when(!cancel, |this| this.child(self.input.clone())),
            );
        let mut field = div().id(self.id).flex().items_center().min_w_0().text_color(t.quiet);
        field = match self.variant {
            SearchVariant::Field => field.border_1().border_color(t.line_strong).rounded(px(8.0)).bg(t.control_soft),
            SearchVariant::Menu => field.h(px(28.0)).border_1().border_color(t.line_glass).rounded(px(7.0)).bg(t.frame_field),
            SearchVariant::Palette => field.h(px(49.0)).border_b_1().border_color(t.line_soft),
            SearchVariant::Library => field.h(px(37.0)).border_b_1().border_color(t.line_soft),
        };
        if focused {
            let radius = if self.variant == SearchVariant::Menu { 7.0 } else { 8.0 };
            field = field.relative().border_color(t.focus_ring).child(halo_ring(radius, t));
        }
        field = field.child(input_box);
        if let (true, Some(handler)) = (filled, self.on_clear) {
            let mut clear = IconButton::new("clear", IconName::X, "Clear search")
                .size(IconButtonSize::Xs)
                .icon_size(14.0)
                .on_click(move |event, window, cx| handler(event, window, cx));
            clear = match self.clear_tip {
                Some(tip) => clear.tip(tip),
                None => clear.no_tip(),
            };
            field = field.child(div().flex_none().mr(px(2.0)).child(clear));
        }
        field
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FieldSize {
    Sm,
    #[default]
    Md,
}

/// A one-line input, or a multi-line one when its `TextInput` is. With a label, hint, or error it
/// comes wrapped with them.
#[derive(IntoElement)]
pub struct TextField {
    id: ElementId,
    input: Entity<TextInput>,
    label: Option<SharedString>,
    hint: Option<SharedString>,
    error: Option<SharedString>,
    mono: bool,
    size: FieldSize,
    invalid: bool,
    gutter: Pixels,
}

impl TextField {
    pub fn new(id: impl Into<ElementId>, input: &Entity<TextInput>) -> Self {
        Self { id: id.into(), input: input.clone(), label: None, hint: None, error: None, mono: false, size: FieldSize::Md, invalid: false, gutter: px(0.0) }
    }

    /// Room kept clear at the right of the text, as for a scrollbar laid over the field.
    pub fn gutter(mut self, gutter: Pixels) -> Self {
        self.gutter = gutter;
        self
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn hint(mut self, hint: impl Into<SharedString>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Marks the field invalid and says why under it.
    pub fn error(mut self, error: Option<SharedString>) -> Self {
        self.error = error;
        self
    }

    pub fn invalid(mut self, invalid: bool) -> Self {
        self.invalid = invalid;
        self
    }

    pub fn mono(mut self, mono: bool) -> Self {
        self.mono = mono;
        self
    }

    pub fn size(mut self, size: FieldSize) -> Self {
        self.size = size;
        self
    }
}

impl RenderOnce for TextField {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        if let Some(label) = self.label.clone() {
            self.input.update(cx, |input, _| input.set_label(label));
        }
        let theme = cx.theme();
        let t = &theme.tokens;
        let m = &theme.metrics;
        let input = self.input.read(cx);
        let multiline = input.is_multiline();
        let muted = input.is_read_only() || input.is_disabled();
        let focused = input_focused(&self.input, window, cx);
        let invalid = self.invalid || self.error.is_some();
        let (height, radius, pad, font) = match self.size {
            FieldSize::Sm => (26.0, 7.0, 8.0, m.text_meta),
            FieldSize::Md => (30.0, 8.0, 9.0, m.text_control),
        };
        let focus_target = self.input.clone();
        let mut field = div()
            .id(self.id.clone())
            .w_full()
            .min_w_0()
            .flex()
            .border_1()
            .border_color(if invalid { t.danger_line } else { t.line_input })
            .rounded(px(radius))
            .pl(px(pad))
            .pr(px(pad) + self.gutter)
            .bg(t.surface_input)
            .text_color(if muted { t.muted } else { t.ink })
            .text_size(px(font))
            .cursor(CursorStyle::IBeam)
            .skip_tab_stop()
            .accessible(gpui::AccessInfo::new(gpui::AccessRole::Group, ""))
            .on_click(move |_, window, cx| focus_target.read(cx).focus(window));
        field = if multiline {
            field.min_h(px(60.0)).py(px(6.0)).line_height(gpui::relative(1.45)).items_start()
        } else {
            field.h(px(height)).items_center()
        };
        if self.mono {
            field = field.font_family(theme.typography.mono_family.clone());
        }
        if focused {
            field = field.relative().border_color(t.focus_ring).child(halo_ring(radius, t));
        }
        let field = field.child(self.input.clone());
        if self.label.is_none() && self.hint.is_none() && self.error.is_none() {
            return field.into_any_element();
        }
        FieldGroup::new().label_opt(self.label).hint_opt(self.hint).error(self.error).child(field).into_any_element()
    }
}

/// A label over a control, or over several, with a hint or error under them.
#[derive(IntoElement, Default)]
pub struct FieldGroup {
    label: Option<SharedString>,
    hint: Option<SharedString>,
    error: Option<SharedString>,
    children: Vec<AnyElement>,
}

impl FieldGroup {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn label(self, label: impl Into<SharedString>) -> Self {
        self.label_opt(Some(label.into()))
    }

    fn label_opt(mut self, label: Option<SharedString>) -> Self {
        self.label = label;
        self
    }

    pub fn hint(self, hint: impl Into<SharedString>) -> Self {
        self.hint_opt(Some(hint.into()))
    }

    fn hint_opt(mut self, hint: Option<SharedString>) -> Self {
        self.hint = hint;
        self
    }

    pub fn error(mut self, error: Option<SharedString>) -> Self {
        self.error = error;
        self
    }
}

impl ParentElement for FieldGroup {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for FieldGroup {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let t = &theme.tokens;
        let m = &theme.metrics;
        div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .min_w_0()
            .children(self.label.map(|label| {
                div().text_color(t.muted).text_size(px(m.text_menu)).font_weight(FontWeight::MEDIUM).child(label)
            }))
            .children(self.children)
            .children(self.hint.map(|hint| div().text_color(t.muted).text_size(px(m.text_meta)).child(hint)))
            .children(self.error.map(|error| div().text_color(t.danger_soft).text_size(px(m.text_meta)).child(error)))
    }
}

type ToggleHandler = Rc<dyn Fn(&bool, &mut Window, &mut App)>;

/// An on/off control. Each press asks for the other state; the caller decides whether `on` follows.
#[derive(IntoElement)]
pub struct Switch {
    id: ElementId,
    on: bool,
    label: SharedString,
    disabled: bool,
    focus: Option<FocusHandle>,
    on_change: Option<ToggleHandler>,
}

impl Switch {
    pub fn new(id: impl Into<ElementId>, on: bool, label: impl Into<SharedString>) -> Self {
        Self { id: id.into(), on, label: label.into(), disabled: false, focus: None, on_change: None }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn track_focus(mut self, focus: &FocusHandle) -> Self {
        self.focus = Some(focus.clone());
        self
    }

    pub fn on_change(mut self, handler: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }

    pub fn label(&self) -> &SharedString {
        &self.label
    }
}

impl RenderOnce for Switch {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = &cx.theme().tokens;
        let focused = self.focus.as_ref().is_some_and(|focus| focus.is_focused(window)) && keyboard_focus_visible(cx);
        let (border, fill, knob): (Hsla, Hsla, Hsla) = if self.on {
            (t.accent.into(), t.accent.into(), t.accent_ink.into())
        } else {
            (t.line_input.into(), t.sunken.into(), t.muted.into())
        };
        let on = self.on;
        let knob_element = div()
            .id(ElementId::NamedChild(Box::new(self.id.clone()), "knob".into()))
            .absolute()
            .top(px(2.0))
            .left(px(2.0))
            .size(px(18.0))
            .rounded_full()
            .when(!t.reduced_motion, |knob| knob.transition(gpui::Transition::ease(140).only(&[gpui::TransitionProperty::Transform, gpui::TransitionProperty::Background])))
            .when(on, |knob| knob.transform(gpui::ElementTransform::translate(px(18.0), px(0.0))))
            .bg(knob);
        let mut switch = div()
            .id(self.id)
            .accessible(gpui::AccessInfo::new(gpui::AccessRole::Switch, self.label.clone()).toggled(on).disabled(self.disabled))
            .relative()
            .flex_none()
            .w(px(42.0))
            .h(px(24.0))
            .border_1()
            .border_color(border)
            .rounded_full()
            .bg(fill)
            .glint(t, Corners::Full)
            .child(knob_element);
        if focused {
            switch = switch.border_color(t.focus_ring).child(halo_ring(12.0, t));
        }
        if let Some(focus) = &self.focus {
            switch = switch.track_focus(focus);
        }
        if self.disabled {
            switch = switch.opacity(t.disabled_opacity);
        } else {
            if !self.on {
                let bright = t.line_bright;
                switch = switch.hover(move |style| style.border_color(bright));
            }
            if let Some(handler) = self.on_change {
                switch = switch.cursor(CursorStyle::PointingHand).on_click(move |_, window, cx| handler(&!on, window, cx));
            }
        }
        switch
    }
}

/// A ticked box, with an optional label beside it.
#[derive(IntoElement)]
pub struct Checkbox {
    id: ElementId,
    checked: bool,
    label: Option<SharedString>,
    tip: Option<SharedString>,
    name: Option<SharedString>,
    disabled: bool,
    focus: Option<FocusHandle>,
    on_change: Option<ToggleHandler>,
    accent: bool,
    mixed: bool,
}

impl Checkbox {
    pub fn new(id: impl Into<ElementId>, checked: bool) -> Self {
        Self { id: id.into(), checked, label: None, tip: None, name: None, disabled: false, focus: None, on_change: None, accent: false, mixed: false }
    }

    /// What assistive technology calls the box, as `aria-label`, when neither label nor tip says it.
    pub fn name(mut self, name: impl Into<SharedString>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Half ticked, as a native box with `indeterminate` set: some of what it stands for is ticked.
    pub fn mixed(mut self, mixed: bool) -> Self {
        self.mixed = mixed;
        self
    }

    /// Ticked in the accent, as a native box with `accent-color: var(--accent)` is.
    pub fn accent(mut self, accent: bool) -> Self {
        self.accent = accent;
        self
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn tip(mut self, tip: impl Into<SharedString>) -> Self {
        self.tip = Some(tip.into());
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn track_focus(mut self, focus: &FocusHandle) -> Self {
        self.focus = Some(focus.clone());
        self
    }

    pub fn on_change(mut self, handler: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Checkbox {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let t = &theme.tokens;
        let focused = self.focus.as_ref().is_some_and(|focus| focus.is_focused(window)) && keyboard_focus_visible(cx);
        let checked = self.checked;
        let filled = checked || self.mixed;
        let mut tick = div()
            .flex_none()
            .size(px(16.0))
            .p(px(1.0))
            .border(px(1.5))
            .rounded(px(4.0))
            .flex()
            .items_center()
            .justify_center();
        tick = if filled {
            let fill = if self.accent { t.accent } else { t.success };
            tick.border_color(fill).bg(fill).text_color(if self.accent { t.accent_ink } else { t.canvas })
        } else {
            tick.border_color(t.line_input).text_color(gpui::transparent_black())
        };
        if focused {
            tick = tick.relative().border_color(t.focus_ring).child(halo_ring(4.0, t));
        }
        let group: SharedString = format!("checkbox-{:?}", self.id).into();
        if !filled && !self.disabled && !focused {
            let hover = t.quiet_strong;
            tick = tick.group_hover(group.clone(), move |style| style.border_color(hover).text_color(hover));
        }
        let tick = tick.child(Icon::new(if self.mixed && !checked { IconName::Minus } else { IconName::Check }).size(px(12.0)));
        let name = self.name.clone().or_else(|| self.label.clone()).or_else(|| self.tip.clone()).unwrap_or_default();
        let mut checkbox = div()
            .id(self.id)
            .accessible(gpui::AccessInfo::new(gpui::AccessRole::CheckBox, name).toggled(checked).disabled(self.disabled))
            .group(group.clone())
            .flex()
            .items_center()
            .justify_center()
            .gap(px(8.0))
            .h(px(28.0))
            .rounded(px(6.0))
            .px(px(6.0))
            .text_color(t.ink_soft)
            .text_size(px(theme.metrics.text_control));
        checkbox = checkbox.child(tick);
        if let Some(focus) = &self.focus {
            checkbox = checkbox.track_focus(focus);
        }
        if let Some(label) = self.label {
            checkbox = checkbox.child(label);
        }
        if self.disabled {
            checkbox = checkbox.opacity(t.disabled_opacity);
        } else if let Some(handler) = self.on_change {
            checkbox = checkbox.cursor(CursorStyle::PointingHand).on_click(move |_, window, cx| handler(&!checked, window, cx));
        }
        match self.tip {
            Some(tip) => checkbox.tip(tip),
            None => checkbox,
        }
    }
}
