//! Eases an element's colours, opacity and transform toward each new value its style takes, as a
//! CSS `transition` does: hover, press and focus styles and changes to the element's own style
//! alike, each property along its own timing.

use crate::{Background, ElementTransform, Fill, Hsla, Rgba, Style, color::BackgroundTag};
use std::time::{Duration, Instant};

/// A property a [`Transition`] eases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransitionProperty {
    /// `background`.
    Background,
    /// `border-color`.
    Border,
    /// `color`, which children inherit.
    Color,
    /// `opacity`.
    Opacity,
    /// `transform`.
    Transform,
    /// `text-decoration-color`.
    Decoration,
}

const PROPERTIES: usize = 6;

impl TransitionProperty {
    const ALL: [TransitionProperty; PROPERTIES] = [Self::Background, Self::Border, Self::Color, Self::Opacity, Self::Transform, Self::Decoration];

    fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Timing {
    duration: Duration,
    delay: Duration,
    curve: [f32; 4],
}

impl Timing {
    /// The share of the way along the curve at `progress` (0 to 1) of the time.
    fn eased(&self, progress: f32) -> f32 {
        let [x1, y1, x2, y2] = self.curve;
        if progress <= 0.0 {
            return 0.0;
        }
        if progress >= 1.0 {
            return 1.0;
        }
        let bezier = |t: f32, a: f32, b: f32| {
            let u = 1.0 - t;
            3.0 * u * u * t * a + 3.0 * u * t * t * b + t * t * t
        };
        let (mut low, mut high) = (0.0f32, 1.0f32);
        let mut t = progress;
        for _ in 0..24 {
            let x = bezier(t, x1, x2);
            if (x - progress).abs() < 1e-4 {
                break;
            }
            if x < progress {
                low = t;
            } else {
                high = t;
            }
            t = (low + high) / 2.0;
        }
        bezier(t, y1, y2)
    }

    /// How far along the eased curve a change made at `since` is at `now`, and whether it still runs.
    fn at(&self, since: Instant, now: Instant) -> (f32, bool) {
        let elapsed = now.saturating_duration_since(since);
        if elapsed < self.delay {
            return (0.0, true);
        }
        let duration = self.duration.as_secs_f32();
        let progress = if duration > 0.0 { (elapsed - self.delay).as_secs_f32() / duration } else { 1.0 };
        (self.eased(progress), progress < 1.0)
    }
}

/// How long an element's properties take to reach new values, and along which curves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transition {
    timings: [Option<Timing>; PROPERTIES],
}

impl Transition {
    /// A `cubic-bezier(x1, y1, x2, y2)` curve over `ms` milliseconds, for every property.
    pub fn cubic(ms: u64, x1: f32, y1: f32, x2: f32, y2: f32) -> Self {
        let timing = Timing { duration: Duration::from_millis(ms), delay: Duration::ZERO, curve: [x1, y1, x2, y2] };
        Self { timings: [Some(timing); PROPERTIES] }
    }

    /// CSS `ease`.
    pub fn ease(ms: u64) -> Self {
        Self::cubic(ms, 0.25, 0.1, 0.25, 1.0)
    }

    /// CSS `ease-out`.
    pub fn ease_out(ms: u64) -> Self {
        Self::cubic(ms, 0.0, 0.0, 0.58, 1.0)
    }

    /// CSS `linear`.
    pub fn linear(ms: u64) -> Self {
        Self::cubic(ms, 0.0, 0.0, 1.0, 1.0)
    }

    /// This timing for `properties` alone; the others change at once.
    pub fn only(mut self, properties: &[TransitionProperty]) -> Self {
        for property in TransitionProperty::ALL {
            if !properties.contains(&property) {
                self.timings[property.index()] = None;
            }
        }
        self
    }

    /// `other`'s timing for the properties it eases, and this one's for the rest.
    pub fn and(mut self, other: Transition) -> Self {
        for (mine, theirs) in self.timings.iter_mut().zip(other.timings) {
            if theirs.is_some() {
                *mine = theirs;
            }
        }
        self
    }

    /// Waits `ms` milliseconds before each change starts, as `transition-delay` does.
    pub fn delay(mut self, ms: u64) -> Self {
        for timing in self.timings.iter_mut().flatten() {
            timing.delay = Duration::from_millis(ms);
        }
        self
    }
}

/// One property's way from the value it showed to the one it is heading for, along `timing` when
/// it entered on its own.
#[derive(Clone, Debug)]
struct Track<T> {
    from: T,
    to: T,
    since: Instant,
    timing: Option<Timing>,
}

/// Where each of an element's eased properties stands.
#[derive(Clone, Debug, Default)]
pub(crate) struct TransitionState {
    background: Option<Track<Option<Fill>>>,
    border: Option<Track<Option<Hsla>>>,
    color: Option<Track<Option<Hsla>>>,
    opacity: Option<Track<Option<f32>>>,
    transform: Option<Track<Option<ElementTransform>>>,
    decoration: Option<Track<Option<Hsla>>>,
}

/// Moves `track` toward `target` and answers the value to draw now, and whether it still runs.
/// A property seen for the first time starts where it is, or at `enter` along its own timing.
fn step<T: Clone + PartialEq>(
    track: &mut Option<Track<T>>,
    target: T,
    timing: Option<&Timing>,
    enter: Option<(T, Option<&Timing>)>,
    now: Instant,
    lerp: impl Fn(&T, &T, f32) -> T,
) -> (T, bool) {
    if track.is_none()
        && let Some((from, Some(entering))) = enter
        && from != target
    {
        *track = Some(Track { from, to: target.clone(), since: now, timing: Some(*entering) });
    }
    let Some(timing) = track.as_ref().and_then(|track| track.timing).or(timing.copied()) else {
        *track = None;
        return (target, false);
    };
    let current = track.get_or_insert_with(|| Track { from: target.clone(), to: target.clone(), since: now, timing: None });
    if current.to != target {
        let (amount, running) = timing.at(current.since, now);
        let shown = if running { lerp(&current.from, &current.to, amount) } else { current.to.clone() };
        *current = Track { from: shown, to: target, since: now, timing: None };
    }
    let timing = current.timing.unwrap_or(timing);
    let (amount, running) = timing.at(current.since, now);
    if !running || current.from == current.to {
        current.timing = None;
        return (current.to.clone(), false);
    }
    (lerp(&current.from, &current.to, amount), true)
}

/// Draws `style`'s eased properties where their transitions stand at `now`; answers whether any
/// still runs. `inherited` is the text colour an element without its own takes. An element turned
/// hidden stays drawn while its opacity eases, so it fades out before it goes.
///
/// `enter` is the style an element seen for the first time sets out from, and the timing it eases
/// in along.
pub(crate) fn apply(
    state: &mut TransitionState,
    style: &mut Style,
    transition: Option<&Transition>,
    enter: Option<(&Style, &Transition)>,
    inherited: Hsla,
    now: Instant,
) -> bool {
    let timing = |property: TransitionProperty| transition.and_then(|transition| transition.timings[property.index()].as_ref());
    let entering = |property: TransitionProperty| enter.and_then(|(_, transition)| transition.timings[property.index()].as_ref());
    let start = enter.map(|(start, _)| start);
    let (background, a) = step(
        &mut state.background,
        style.background.clone(),
        timing(TransitionProperty::Background),
        start.map(|start| (start.background.clone(), entering(TransitionProperty::Background))),
        now,
        |from, to, amount| blend_fill(from.as_ref(), to.as_ref(), amount),
    );
    let (border, b) = step(
        &mut state.border,
        style.border_color,
        timing(TransitionProperty::Border),
        start.map(|start| (start.border_color, entering(TransitionProperty::Border))),
        now,
        |from, to, amount| Some(mix(from.unwrap_or_default(), to.unwrap_or_default(), amount)),
    );
    let (color, c) = step(
        &mut state.color,
        style.text.color,
        timing(TransitionProperty::Color),
        start.map(|start| (start.text.color, entering(TransitionProperty::Color))),
        now,
        |from, to, amount| Some(mix(from.unwrap_or(inherited), to.unwrap_or(inherited), amount)),
    );
    let (opacity, d) = step(
        &mut state.opacity,
        style.opacity,
        timing(TransitionProperty::Opacity),
        start.map(|start| (start.opacity, entering(TransitionProperty::Opacity))),
        now,
        |from, to, amount| {
            let (from, to) = (from.unwrap_or(1.0), to.unwrap_or(1.0));
            Some(from + (to - from) * amount)
        },
    );
    let (transform, e) = step(
        &mut state.transform,
        style.transform,
        timing(TransitionProperty::Transform),
        start.map(|start| (start.transform, entering(TransitionProperty::Transform))),
        now,
        |from, to, amount| Some(from.unwrap_or_default().lerp(&to.unwrap_or_default(), amount)),
    );
    let current = color.unwrap_or(inherited);
    let underline = style.text.underline.as_ref().and_then(|underline| underline.color);
    let (decoration, f) = step(&mut state.decoration, underline, timing(TransitionProperty::Decoration), None, now, |from, to, amount| {
        Some(mix(from.unwrap_or(current), to.unwrap_or(current), amount))
    });
    if let (Some(underline), Some(decoration)) = (style.text.underline.as_mut(), decoration) {
        underline.color = Some(decoration);
    }
    style.background = background;
    style.border_color = border;
    style.text.color = color;
    style.opacity = opacity;
    if d && style.visibility == crate::Visibility::Hidden {
        style.visibility = crate::Visibility::Visible;
    }
    style.transform = transform;
    a || b || c || d || e || f
}

/// Mixes premultiplied, as a browser interpolates colours, so a transparent end fades only alpha.
fn mix(from: Hsla, to: Hsla, amount: f32) -> Hsla {
    let (a, b) = (Rgba::from(from), Rgba::from(to));
    let alpha = a.a + (b.a - a.a) * amount;
    if alpha <= 0.0 {
        return Hsla::transparent_black();
    }
    let channel = |x: f32, y: f32| ((x * a.a) + ((y * b.a) - (x * a.a)) * amount) / alpha;
    Hsla::from(Rgba { r: channel(a.r, b.r), g: channel(a.g, b.g), b: channel(a.b, b.b), a: alpha })
}

fn blend_fill(from: Option<&Fill>, to: Option<&Fill>, amount: f32) -> Option<Fill> {
    let from = from.and_then(Fill::color).unwrap_or_default();
    let to = to.and_then(Fill::color).unwrap_or_default();
    let gradient = |tag: BackgroundTag| matches!(tag, BackgroundTag::LinearGradient | BackgroundTag::RadialGradient);
    let blendable = |background: &Background| background.tag == BackgroundTag::Solid || gradient(background.tag);
    if !blendable(&from) || !blendable(&to) || (gradient(from.tag) && gradient(to.tag) && from.tag != to.tag) {
        return Some(Fill::Color(if amount < 0.5 { from } else { to }));
    }
    if from.tag == BackgroundTag::Solid && to.tag == BackgroundTag::Solid {
        return Some(Fill::Color(mix(from.solid, to.solid, amount).into()));
    }
    let stops = |background: &Background, index: usize| {
        if background.tag == BackgroundTag::Solid { (background.solid, index as f32) } else { (background.colors[index].color, background.colors[index].percentage) }
    };
    let mut blended = if gradient(to.tag) { to } else { from };
    if blended.tag == BackgroundTag::LinearGradient {
        blended.solid = mix(from.base_color(), to.base_color(), amount);
    }
    for index in 0..2 {
        let ((from_color, from_at), (to_color, to_at)) = (stops(&from, index), stops(&to, index));
        blended.colors[index].color = mix(from_color, to_color, amount);
        blended.colors[index].percentage = from_at + (to_at - from_at) * amount;
    }
    Some(Fill::Color(blended))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reversing_midway_sets_out_from_where_it_is() {
        let timing = Timing { duration: Duration::from_millis(100), delay: Duration::ZERO, curve: [0.0, 0.0, 1.0, 1.0] };
        let lerp = |from: &f32, to: &f32, amount: f32| from + (to - from) * amount;
        let start = Instant::now();
        let mut track = None;
        assert_eq!(step(&mut track, 0.0, Some(&timing), None, start, lerp), (0.0, false));
        step(&mut track, 1.0, Some(&timing), None, start, lerp);
        let (halfway, running) = step(&mut track, 1.0, Some(&timing), None, start + Duration::from_millis(50), lerp);
        assert!((halfway - 0.5).abs() < 0.01 && running);
        let (turned, _) = step(&mut track, 0.0, Some(&timing), None, start + Duration::from_millis(50), lerp);
        assert!((turned - 0.5).abs() < 0.01);
        let (back, running) = step(&mut track, 0.0, Some(&timing), None, start + Duration::from_millis(100), lerp);
        assert!((back - 0.25).abs() < 0.01 && running);
        assert_eq!(step(&mut track, 0.0, Some(&timing), None, start + Duration::from_millis(200), lerp), (0.0, false));
    }

    #[test]
    fn each_property_keeps_its_own_timing() {
        let transition = Transition::ease_out(110)
            .only(&[TransitionProperty::Color])
            .and(Transition::linear(200).only(&[TransitionProperty::Opacity]));
        let start = Instant::now();
        let mut state = TransitionState::default();
        let red = Hsla::from(Rgba { r: 1.0, g: 0.0, b: 0.0, a: 1.0 });
        let mut resting = Style::default();
        resting.opacity = Some(0.0);
        resting.background = Some(Fill::Color(red.into()));
        apply(&mut state, &mut resting, Some(&transition), None, red, start);
        let mut lit = Style::default();
        lit.opacity = Some(1.0);
        lit.text.color = Some(Hsla::transparent_black());
        assert!(apply(&mut state, &mut lit.clone(), Some(&transition), None, red, start));
        let mut drawn = lit.clone();
        apply(&mut state, &mut drawn, Some(&transition), None, red, start + Duration::from_millis(120));
        assert!((drawn.opacity.unwrap() - 0.6).abs() < 0.01);
        assert_eq!(drawn.text.color, Some(Hsla::transparent_black()));
        assert_eq!(drawn.background, None);
    }

    #[test]
    fn an_entering_element_eases_in_from_its_start_once() {
        let start = Instant::now();
        let mut state = TransitionState::default();
        let mut from = Style::default();
        from.opacity = Some(0.0);
        let enter = Transition::linear(100);
        let mut drawn = Style::default();
        assert!(apply(&mut state, &mut drawn, None, Some((&from, &enter)), Hsla::default(), start));
        assert_eq!(drawn.opacity, Some(0.0));
        let mut drawn = Style::default();
        apply(&mut state, &mut drawn, None, None, Hsla::default(), start + Duration::from_millis(50));
        assert!((drawn.opacity.unwrap() - 0.5).abs() < 0.01);
        let mut drawn = Style::default();
        assert!(!apply(&mut state, &mut drawn, None, None, Hsla::default(), start + Duration::from_millis(150)));
        assert_eq!(drawn.opacity, None);
    }

    #[test]
    fn a_gradient_over_a_colour_fades_in_with_its_colour() {
        let red = Hsla::from(Rgba { r: 1.0, g: 0.0, b: 0.0, a: 1.0 });
        let sheened = crate::linear_gradient(180.0, crate::linear_color_stop(crate::white(), 0.0), crate::linear_color_stop(crate::white(), 1.0)).over_color(red);
        let Some(Fill::Color(halfway)) = blend_fill(None, Some(&Fill::Color(sheened)), 0.5) else { panic!("no fill") };
        assert!((halfway.solid.a - 0.5).abs() < 0.01);
    }

    #[test]
    fn fading_from_transparent_keeps_the_target_hue() {
        let red = Hsla::from(Rgba { r: 1.0, g: 0.0, b: 0.0, a: 1.0 });
        let halfway = Rgba::from(mix(Hsla::transparent_black(), red, 0.5));
        assert!((halfway.r - 1.0).abs() < 0.01 && (halfway.a - 0.5).abs() < 0.01);
    }
}
