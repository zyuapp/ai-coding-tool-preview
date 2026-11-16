//! Drawing helpers for what CSS does and GPUI does not: inset rims and glints, layered glass fills,
//! and the frosted edge of a float.

use gpui::{div, prelude::FluentBuilder, px, BackdropFilter, Div, IntoElement, ParentElement, Pixels, Rgba, Styled};

use crate::theme::{Elevation, Pane, Rim, Tokens};

/// How a raised thing's corners turn, so its inner light turns with them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Corners {
    Radius(f32),
    Full,
}

impl Corners {
    fn apply(self, element: Div) -> Div {
        match self {
            Corners::Radius(radius) => element.rounded(px(radius)),
            Corners::Full => element.rounded_full(),
        }
    }
}

fn overlay(corners: Corners) -> Div {
    corners.apply(div().absolute().top_0().left_0().size_full())
}

/// `--glass-rim`: a hairline of light all round, caught along the top and faintly along the bottom.
/// Drawn over the fill and under whatever follows it. The CSS draws these as soft inset shadows less
/// than a pixel deep; each is spread over the rows it lights, at the share of light Chromium gives them.
pub fn rim_layers(rim: Rim, corners: Corners) -> [Div; 5] {
    let share = |color: Rgba, share: f32| Rgba { a: color.a * share, ..color };
    [
        overlay(corners).border(px(1.0)).border_color(share(rim.hairline, 0.6)),
        overlay(corners).border_t(px(1.0)).border_color(share(rim.top, 0.45)),
        overlay(corners).border_t(px(2.0)).border_color(share(rim.top, 0.15)),
        overlay(corners).border_b(px(1.0)).border_color(share(rim.bottom, 0.45)),
        overlay(corners).border_b(px(2.0)).border_color(share(rim.bottom, 0.15)),
    ]
}

/// `--glint`: the 1px of light along a raised thing's top edge.
pub fn glint_layer(color: Rgba, corners: Corners) -> Div {
    overlay(corners).border_t(px(1.0)).border_color(color)
}

/// Draws the rim and glint layers an element's CSS gives it. The element must be `relative`.
pub trait Raised: ParentElement + Sized {
    fn rim(self, rim: Rim, corners: Corners) -> Self {
        self.children(rim_layers(rim, corners))
    }

    fn glint(self, tokens: &Tokens, corners: Corners) -> Self {
        self.child(glint_layer(tokens.glint, corners))
    }

    /// A glass pane's 1px edge (`.glass-pane::before`), drawn inside the box without taking room from its content.
    fn edge(self, color: Rgba, corners: Corners) -> Self {
        self.child(overlay(corners).border_1().border_color(color))
    }

    /// An elevation's glint, if it has one.
    fn elevation_glint(self, elevation: &Elevation, tokens: &Tokens, corners: Corners) -> Self {
        if elevation.glint {
            self.glint(tokens, corners)
        } else {
            self
        }
    }
}

impl<T: ParentElement + Sized> Raised for T {}

/// Fills an element with a pane, sheen and all.
pub trait PaneFill: Styled + Sized {
    fn pane(self, pane: Pane) -> Self {
        self.bg(pane.background())
    }

    fn elevation(self, elevation: &Elevation) -> Self {
        self.shadow(elevation.box_shadows())
    }
}

impl<T: Styled + Sized> PaneFill for T {}

/// `--glass-blur`: what a glass pane does to what is behind it.
pub fn glass_backdrop(tokens: &Tokens) -> Option<BackdropFilter> {
    // Filters inside a pane see the window behind it too, which reads close to the pane alone.
    tokens.glass_blur.then(|| BackdropFilter::blur(px(7.0 + 16.0 * tokens.glass_tint)).saturate(1.7).root(false))
}

/// `--veil-blur`: what a veil does to the window behind it.
pub fn veil_backdrop(tokens: &Tokens) -> Option<BackdropFilter> {
    tokens.veil_blur.then(|| BackdropFilter::blur(px(10.0)).saturate(1.4))
}

/// Applies an optional backdrop filter.
pub trait Backdrop: Styled + Sized {
    fn backdrop(self, filter: Option<BackdropFilter>) -> Self {
        match filter {
            Some(filter) => self.backdrop_filter(filter),
            None => self,
        }
    }

    /// Frosts what is behind a glass pane.
    fn glass_backdrop(self, tokens: &Tokens) -> Self {
        self.backdrop(glass_backdrop(tokens))
    }

    /// Blurs the window behind a veil.
    fn veil_backdrop(self, tokens: &Tokens) -> Self {
        self.backdrop(veil_backdrop(tokens))
    }
}

impl<T: Styled + Sized> Backdrop for T {}

thread_local! {
    static FRAME_EXPANDED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Records whether the sidebar shows an expanded control (`[aria-expanded="true"]`, such as an open
/// section), which ui/glass.css matches to give every top-level popover the frame's near-solid glass.
pub fn set_frame_expanded(expanded: bool) {
    FRAME_EXPANDED.with(|cell| cell.set(expanded));
}

/// Whether a popover takes the frame's near-solid glass over a window material: one the frame
/// opened always, one drawn at the window's top level while the sidebar shows an expanded control.
pub fn frame_glass(environment: &crate::theme::Environment, from_frame: bool, top_level: bool) -> bool {
    (from_frame || (top_level && FRAME_EXPANDED.with(|cell| cell.get())))
        && environment.material != crate::theme::WindowMaterial::None
        && !environment.reduced_transparency
}

/// The kinds of glass in glass.css. Each owns its edge, corner, fill and shadow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlassKind {
    /// Menus, pickers, and cards that open from a control.
    Popover,
    /// A card or banner floating over the transcript.
    Float,
    /// A dialog or palette standing over the whole window.
    Overlay,
    /// One rounded capsule.
    Capsule,
    /// A chip set in the flow: a wash under the sheen, no shadow.
    Chip,
    /// A bar pinned over a scrolling list.
    Strip,
}

impl GlassKind {
    pub fn default_radius(self) -> Option<f32> {
        match self {
            GlassKind::Popover => Some(10.0),
            GlassKind::Float => Some(14.0),
            GlassKind::Overlay => Some(16.0),
            GlassKind::Capsule => None,
            GlassKind::Chip => Some(10.0),
            GlassKind::Strip => Some(0.0),
        }
    }
}

/// Paints a glass surface of `kind` on `element`, which becomes `relative`. `radius` overrides
/// `--glass-radius`. A tone mixes its hue into the pane and edges it in that hue.
pub fn glass(element: Div, tokens: &Tokens, kind: GlassKind, radius: Option<f32>, tone: Option<crate::theme::ToneColors>) -> Div {
    let corners = match radius.or(kind.default_radius()) {
        Some(radius) => Corners::Radius(radius),
        None => Corners::Full,
    };
    let element = corners.apply(element.relative());
    let floats = !matches!(kind, GlassKind::Chip | GlassKind::Strip);
    let pane = match (kind, tone) {
        (_, Some(tone)) => Pane {
            fill: crate::color::mix(tone.tone, 0.14, tokens.tone_glass_ground),
            sheen: tokens.glass_blur,
        },
        (GlassKind::Capsule, None) => tokens.glass_clear,
        (GlassKind::Chip, None) => tokens.glass_control,
        (GlassKind::Strip, None) => Pane::solid(tokens.sunken),
        _ => tokens.glass_menu,
    };
    // A float's sheen is sized to its padding box and repeats into its 1px border, so the bottom
    // edge catches the brightest of it.
    let mut element = if floats { element.bg(pane.background().tiled_from_padding_box()) } else { element.pane(pane) };
    if floats {
        element = element.glass_backdrop(tokens);
        // Chromium puts a dialog's or palette's frosted pane on a whole pixel and lays its contents out from there.
        if kind == GlassKind::Overlay && tokens.glass_blur {
            element = element.snap_origin();
        }
    }
    // On the glass sheet a strip has no ground to hide the list with, so it is frosted instead.
    if kind == GlassKind::Strip && tone.is_none() && tokens.page.a == 0.0 && tokens.glass_blur {
        element = element.bg(tokens.glass_strip).glass_backdrop(tokens).shadow(vec![gpui::BoxShadow {
            color: tokens.line_glass.into(),
            offset: gpui::point(px(0.0), px(1.0)),
            blur_radius: px(0.0),
            spread_radius: px(0.0),
        }]);
    }
    if floats {
        let edge = tone.map(|tone| tone.line).unwrap_or(tokens.glass_edge);
        element = element.border_1().border_color(edge);
        let mut shadows = match kind {
            GlassKind::Popover => tokens.shadow_menu.box_shadows(),
            GlassKind::Float => tokens.shadow_float.box_shadows(),
            GlassKind::Overlay => tokens.shadow_overlay.box_shadows(),
            _ => tokens.shadow_popover.box_shadows(),
        };
        shadows.insert(
            0,
            crate::theme::ShadowLayer { x: 0.0, y: 0.0, blur: 0.0, spread: 0.5, color: tokens.glass_float_outline }.box_shadow(),
        );
        element = element.shadow(shadows).glint(tokens, corners);
    } else if tone.is_some() {
        element = element.border_1().border_color(tone.map(|tone| tone.line).unwrap_or(tokens.line_glass));
    }
    if kind != GlassKind::Strip {
        element = element.rim(tokens.glass_rim, corners);
    }
    element
}

/// A pane of glass standing in a blurred veil. The veil's blur makes it the backdrop the pane's own
/// filter reads, which holds only the veil, so the pane draws `veil` once more under its glass.
pub fn under_veil(pane: impl IntoElement, veil: Rgba, radius: f32, tokens: &Tokens) -> Div {
    div()
        .relative()
        .when(tokens.veil_blur && tokens.glass_blur, |under| under.child(div().absolute().top_0().left_0().size_full().rounded(px(radius)).bg(veil)))
        .child(pane)
}

/// A thin rule across or down a container.
pub fn rule(color: Rgba) -> Div {
    div().flex_none().h(px(1.0)).w_full().bg(color)
}

/// An element that takes up `size` of room and draws nothing.
pub fn gap(size: Pixels) -> impl IntoElement {
    div().flex_none().size(size)
}

/// CSS `letter-spacing` in em, which the text system tracks letters by.
pub fn letter_spacing<E: gpui::Styled>(element: E, em: f32) -> E {
    with_feature(element, gpui::TRACKING_TAG, (em != 0.0).then(|| (em * 10_000.0).round() as i32 as u32))
}

/// CSS `font-variant-numeric: tabular-nums`: every digit as wide as the others.
pub fn tabular_nums<E: gpui::Styled>(element: E) -> E {
    with_feature(element, "tnum", Some(1))
}

/// Sets or clears one font feature, keeping the others this element sets.
fn with_feature<E: gpui::Styled>(mut element: E, tag: &str, value: Option<u32>) -> E {
    let style = element.text_style().get_or_insert_with(Default::default);
    let mut features: Vec<(String, u32)> = style.font_features.as_ref().map(|features| features.tag_value_list().to_vec()).unwrap_or_default();
    features.retain(|(existing, _)| existing != tag);
    if let Some(value) = value {
        features.push((tag.to_string(), value));
    }
    style.font_features = Some(gpui::FontFeatures(std::sync::Arc::new(features)));
    element
}

/// `--touch-time` along `--touch-ease`: how a control eases into hover and press.
pub fn touch() -> gpui::Transition {
    gpui::Transition::cubic(140, 0.2, 0.0, 0.0, 1.0)
}

/// `--panel-slide-ease` over `ms` milliseconds.
pub fn panel_slide(ms: u64) -> gpui::Transition {
    gpui::Transition::cubic(ms, 0.32, 0.72, 0.0, 1.0)
}

/// `fold-open`: what a fold shows drops in from 4px up as it fades in.
pub fn fold_open<E: gpui::InteractiveElement>(element: E) -> E {
    element.enter(panel_slide(180), |style| style.opacity(0.0).transform(gpui::ElementTransform::translate(px(0.0), px(-4.0))))
}

/// `glass-in`: a popover fades in where it stands along the touch curve. The element needs an id.
pub fn glass_in<E: gpui::InteractiveElement>(element: E) -> E {
    element.enter(touch(), |style| style.opacity(0.0))
}

/// `.icon-btn`'s transition: fill and transform along the touch curve, colour, edge and opacity
/// 110ms ease-out.
pub fn icon_touch() -> gpui::Transition {
    use gpui::TransitionProperty::*;
    gpui::Transition::ease_out(110).only(&[Color, Border, Opacity]).and(touch().only(&[Background, Transform]))
}
