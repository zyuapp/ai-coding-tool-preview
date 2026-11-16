//! Things that turn while work runs: the ring a busy button shows, and any icon set spinning.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use gpui::{prelude::*, px, App, ElementId, EntityId, Hsla, Pixels, StyleRefinement, Window};

use crate::icon::{Icon, IconName};
use crate::theme::ActiveTheme;

/// One turn takes 0.9s in 18 steps; under reduced motion it turns slower, as the stylesheet does.
fn turn(reduced: bool) -> Duration {
    Duration::from_millis(if reduced { 2400 } else { 900 })
}

const TURN_STEPS: u32 = 18;

static EPOCH: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);

thread_local! {
    /// The views waiting to draw a motion's next step, by when that step begins. One timer serves
    /// each step, so every view due then draws in the same frame.
    static STEP_WAKES: RefCell<BTreeMap<Instant, Vec<EntityId>>> = RefCell::default();
}

/// How far through `period` a repeating motion of `steps` even steps is, held at the start of the
/// current step. The view drawing it is drawn again when the next step begins, not every frame.
pub fn stepped_progress(id: impl Into<ElementId>, period: Duration, steps: u32, window: &mut Window, cx: &mut App) -> f32 {
    let steps = steps.max(1);
    let step = (period.as_nanos() / steps as u128).max(1);
    // Steps begin on one clock, so every motion in the window wakes it on the same frame.
    let epoch = *EPOCH;
    let started = *window
        .use_keyed_state(id, cx, |_, _| {
            let since = Instant::now().saturating_duration_since(epoch).as_nanos();
            epoch + Duration::from_nanos((since - since % step).min(u64::MAX as u128) as u64)
        })
        .read(cx);
    let index = started.elapsed().as_nanos() / step;
    let progress = (index % steps as u128) as f32 / steps as f32;
    let next = started + Duration::from_nanos(((index + 1) * step).min(u64::MAX as u128) as u64);
    let view = window.current_view();
    let first = STEP_WAKES.with(|wakes| {
        let mut wakes = wakes.borrow_mut();
        let first = !wakes.contains_key(&next);
        let due = wakes.entry(next).or_default();
        if !due.contains(&view) {
            due.push(view);
        }
        first
    });
    if first {
        let wait = next.saturating_duration_since(Instant::now());
        cx.spawn(async move |cx| {
            cx.background_executor().timer(wait).await;
            let due = STEP_WAKES.with(|wakes| wakes.borrow_mut().remove(&next)).unwrap_or_default();
            let _ = cx.update(|cx| {
                for view in due {
                    cx.notify(view);
                }
            });
        })
        .detach();
    }
    progress
}

/// An icon that turns for as long as it is drawn (`.spinning`).
#[derive(IntoElement)]
pub struct Spinning {
    id: ElementId,
    icon: IconName,
    size: Pixels,
    color: Option<Hsla>,
}

impl Spinning {
    pub fn new(id: impl Into<ElementId>, icon: IconName) -> Self {
        Self { id: id.into(), icon, size: px(16.0), color: None }
    }

    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.size = size.into();
        self
    }

    pub fn color(mut self, color: impl Into<Hsla>) -> Self {
        self.color = Some(color.into());
        self
    }
}

impl RenderOnce for Spinning {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let reduced = cx.theme().tokens.reduced_motion;
        let (icon, size, color) = (self.icon, self.size, self.color);
        let build = move |angle: f32| {
            let glyph = Icon::new(icon).size(size).rotate(angle);
            let glyph = match color {
                Some(color) => glyph.color(color),
                None => glyph,
            };
            gpui::div().flex_none().size(size).child(glyph).into_any_element()
        };
        // A window not in use rests its indicators, so it is not redrawn every frame (motion.css).
        if !window.is_window_active() {
            return build(0.0);
        }
        let mut style = StyleRefinement::default();
        style.size.width = Some(size.into());
        style.size.height = Some(size.into());
        style.flex_shrink = Some(0.0);
        let id = self.id.clone();
        super::live::live(self.id, style, window, cx, move |window, cx| build(stepped_progress(id.clone(), turn(reduced), TURN_STEPS, window, cx) * 360.0)).into_any_element()
    }
}

/// The ring a busy button shows in place of its label (`.btn-spinner`).
#[derive(IntoElement)]
pub struct Spinner {
    id: ElementId,
    size: Pixels,
    color: Option<Hsla>,
}

impl Spinner {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self { id: id.into(), size: px(12.0), color: None }
    }

    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.size = size.into();
        self
    }

    pub fn color(mut self, color: impl Into<Hsla>) -> Self {
        self.color = Some(color.into());
        self
    }
}

impl RenderOnce for Spinner {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let mut spinning = Spinning::new(self.id, IconName::SpinnerRing).size(self.size);
        if let Some(color) = self.color {
            spinning = spinning.color(color);
        }
        spinning.render(window, cx)
    }
}

/// `child` dimming to `low` opacity and back over `period` in `steps` even steps, as a
/// `50% { opacity: low }` keyframe on `steps()` timing does, in a live view placed by `style`. It
/// rests at full opacity under reduced motion and while the window is not in use.
pub fn pulse(
    id: impl Into<ElementId>,
    style: StyleRefinement,
    period: Duration,
    steps: u32,
    low: f32,
    window: &mut Window,
    cx: &mut App,
    child: impl Fn() -> gpui::AnyElement + 'static,
) -> gpui::AnyElement {
    let build = move |progress: f32| gpui::div().size_full().opacity(1.0 - (1.0 - low) * (1.0 - (2.0 * progress - 1.0).abs())).child(child()).into_any_element();
    if cx.theme().tokens.reduced_motion || !window.is_window_active() {
        let mut still = gpui::div();
        *still.style() = style;
        return still.child(build(0.0)).into_any_element();
    }
    let id = id.into();
    let step_id = id.clone();
    super::live::live(id, style, window, cx, move |window, cx| build(stepped_progress(step_id.clone(), period, steps, window, cx))).into_any_element()
}
