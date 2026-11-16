//! Trackpad gestures as fingers on the screen (wheel-swipe.ts, wheel-pinch.ts). Positions are
//! fractions of the screen as it is shown. The view runs the idle timer each returns.

use act_core::domain::platform::TouchPhase;
use std::time::Duration;

/// How long the wheel has to rest before the finger it drives lifts.
pub const WHEEL_IDLE: Duration = Duration::from_millis(140);
/// How near an edge the finger may come before it lifts and lands again where it started.
const EDGE: f64 = 0.02;
/// How far a scroll has to go before the finger lands, so a nudge of the trackpad is never read as a tap.
const LANDING: f64 = 0.03;
/// How far apart a pinch's two fingers land, as a fraction of the screen's width.
const LANDING_SPREAD: f64 = 0.16;

/// What the idle timer should do after an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Idle {
    /// Start it again from now.
    Restart,
    /// The finger lifted; stop it.
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Touch {
    pub phase: TouchPhase,
    pub x: f64,
    pub y: f64,
}

/// A trackpad's scrolling as a finger dragging the screen: it lands where the pointer is, moves against
/// the scroll so the content follows the fingers, and lifts once the scrolling stops.
#[derive(Default)]
pub struct WheelSwipe {
    at: Option<(f64, f64)>,
    /// Where a scroll too short to land yet began, and how far it has gone.
    pending: Option<(f64, f64, f64, f64)>,
}

impl WheelSwipe {
    /// One wheel event over the screen: where the pointer is, and how far it scrolled, as fractions of the screen.
    pub fn wheel(&mut self, x: f64, y: f64, mut dx: f64, mut dy: f64) -> (Vec<Touch>, Idle) {
        let mut touches = Vec::new();
        let at = match self.at {
            Some(at) => at,
            None => {
                let (px, py, pdx, pdy) = self.pending.unwrap_or((x, y, 0.0, 0.0));
                let pending = (px, py, pdx + dx, pdy + dy);
                self.pending = Some(pending);
                if pending.2.hypot(pending.3) < LANDING {
                    return (touches, Idle::Restart);
                }
                touches.push(Touch { phase: TouchPhase::Down, x: px, y: py });
                (dx, dy) = (pending.2, pending.3);
                self.pending = None;
                (px, py)
            }
        };
        let next = (at.0 - dx, at.1 - dy);
        let clamped = (next.0.clamp(EDGE, 1.0 - EDGE), next.1.clamp(EDGE, 1.0 - EDGE));
        self.at = Some(clamped);
        touches.push(Touch { phase: TouchPhase::Move, x: clamped.0, y: clamped.1 });
        // A finger at the edge can drag no further, so it lifts, and the next scroll lands it under the pointer again.
        if clamped != next {
            touches.extend(self.lift());
            return (touches, Idle::Stop);
        }
        (touches, Idle::Restart)
    }

    /// Lifts the finger now, wherever it is.
    pub fn lift(&mut self) -> Option<Touch> {
        self.pending = None;
        self.at.take().map(|(x, y)| Touch { phase: TouchPhase::Up, x, y })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PinchFingers {
    pub x: f64,
    pub y: f64,
    pub x2: f64,
    pub y2: f64,
}

/// A trackpad's pinch as two fingers spreading or closing around the pointer. They lift once the pinch stops.
#[derive(Default)]
pub struct WheelPinch {
    center: Option<(f64, f64)>,
    spread: f64,
    fingers: Option<PinchFingers>,
}

impl WheelPinch {
    /// One pinch event over the screen: where the pointer is, and the wheel delta a browser gives a
    /// pinch, which is negative as fingers spread.
    pub fn pinch(&mut self, x: f64, y: f64, delta_y: f64) -> Vec<(TouchPhase, PinchFingers)> {
        let mut touches = Vec::new();
        if self.center.is_none() {
            self.center = Some((x, y));
            self.spread = LANDING_SPREAD;
            let fingers = self.place();
            touches.push((TouchPhase::Down, fingers));
        }
        self.spread *= (-delta_y / 100.0).exp();
        let fingers = self.place();
        touches.push((TouchPhase::Move, fingers));
        touches
    }

    /// A pinch as macOS measures it, a change in magnification, given as the wheel delta a browser would send.
    pub fn magnify(&mut self, x: f64, y: f64, magnification: f64) -> Vec<(TouchPhase, PinchFingers)> {
        self.pinch(x, y, -100.0 * (1.0 + magnification).max(0.01).ln())
    }

    pub fn lift(&mut self) -> Option<(TouchPhase, PinchFingers)> {
        self.center = None;
        self.fingers.take().map(|fingers| (TouchPhase::Up, fingers))
    }

    pub fn fingers(&self) -> Option<PinchFingers> {
        self.fingers
    }

    /// Both fingers stay on the screen: a pinch begun near an edge moves its middle inward.
    fn place(&mut self) -> PinchFingers {
        self.spread = self.spread.clamp(EDGE, 1.0 - 2.0 * EDGE);
        let half = self.spread / 2.0;
        let (cx, cy) = self.center.unwrap_or((0.5, 0.5));
        let middle = cx.clamp(EDGE + half, 1.0 - EDGE - half);
        let fingers = PinchFingers { x: middle - half, y: cy, x2: middle + half, y2: cy };
        self.fingers = Some(fingers);
        fingers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nudge_lands_nothing_and_a_scroll_drags_against_itself() {
        let mut swipe = WheelSwipe::default();
        assert!(swipe.wheel(0.5, 0.5, 0.0, 0.01).0.is_empty());
        let (touches, idle) = swipe.wheel(0.5, 0.5, 0.0, 0.03);
        assert_eq!(idle, Idle::Restart);
        assert_eq!(touches[0], Touch { phase: TouchPhase::Down, x: 0.5, y: 0.5 });
        assert_eq!(touches[1].phase, TouchPhase::Move);
        assert!((touches[1].y - 0.46).abs() < 1e-9);
    }

    #[test]
    fn a_finger_reaching_the_edge_lifts() {
        let mut swipe = WheelSwipe::default();
        swipe.wheel(0.5, 0.05, 0.0, 0.05);
        let (touches, idle) = swipe.wheel(0.5, 0.05, 0.0, 0.2);
        assert_eq!(idle, Idle::Stop);
        assert_eq!(touches.last().unwrap().phase, TouchPhase::Up);
        assert!(swipe.lift().is_none());
    }

    #[test]
    fn a_pinch_near_the_edge_keeps_both_fingers_on_screen() {
        let mut pinch = WheelPinch::default();
        let touches = pinch.pinch(0.0, 0.5, -300.0);
        let (_, fingers) = touches.last().unwrap();
        assert!(fingers.x >= 0.02 && fingers.x2 <= 0.98);
        assert!(fingers.x2 - fingers.x > LANDING_SPREAD);
        assert_eq!(pinch.lift().unwrap().0, TouchPhase::Up);
    }
}
