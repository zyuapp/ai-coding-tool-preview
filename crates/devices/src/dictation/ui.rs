//! The composer's dictation (ComposerDictation.tsx, dictation.css): the microphone button that is held
//! to talk or tapped to listen hands-free, the ring that swells with the voice, and the strip of level
//! bars that stands in the composer bar while it listens.

use std::rc::Rc;
use std::time::{Duration, Instant};

use act_core::domain::platform::{DictationModel, DictationPhase, DICTATION_TAP_MS};
use act_core::AppCommand;
use act_ui::color::alpha;
use act_ui::components::kbd::{Kbd, KbdSize};
use act_ui::components::spinner::Spinner;
use act_ui::theme::ActiveTheme;
use act_ui::tooltip::Tippable;
use act_ui::{Icon, IconName};
use gpui::{
    canvas, div, point, prelude::*, px, size, Animation, AnimationExt, App, Bounds, BoxShadow, Context, DispatchPhase, Entity, FontWeight, Hitbox, HitboxBehavior,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Rgba, SharedString, Subscription, Window,
};

use super::levels::DictationLevels;

/// How far past the button a held pointer may wander before letting go cancels instead of keeping the words.
pub const SLACK: f32 = 24.0;
const BUTTON: f32 = 32.0;

/// What a press on the microphone asks the workspace for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Talk {
    Start,
    HandsFree,
    Stop,
    Cancel,
}

impl Talk {
    pub fn command(self) -> AppCommand {
        match self {
            Talk::Start => AppCommand::DictationStart,
            Talk::HandsFree => AppCommand::DictationHandsFree,
            Talk::Stop => AppCommand::DictationStop { send: None },
            Talk::Cancel => AppCommand::DictationCancel,
        }
    }
}

/// The microphone's press: it listens from the moment it goes down. Letting go over the button after a
/// hold writes the words into the draft; letting go after a tap keeps listening hands-free, until the
/// next press writes them. Letting go away from it, or Escape, drops the recording.
#[derive(Default, Debug)]
pub struct TalkPress {
    holding: bool,
    pressed_at: Option<Instant>,
    away: bool,
}

impl TalkPress {
    pub fn pressed(&self) -> bool {
        self.holding
    }

    pub fn away(&self) -> bool {
        self.away
    }

    pub fn down(&mut self, hands_free: bool, now: Instant) -> Option<Talk> {
        if self.holding {
            return None;
        }
        if hands_free {
            return Some(Talk::Stop);
        }
        self.holding = true;
        self.pressed_at = Some(now);
        Some(Talk::Start)
    }

    /// Returns whether the pointer crossed the slack's edge.
    pub fn moved(&mut self, outside: bool) -> bool {
        if !self.holding || self.away == outside {
            return false;
        }
        self.away = outside;
        true
    }

    pub fn up(&mut self, keep: bool, now: Instant) -> Option<Talk> {
        self.away = false;
        if !self.holding {
            return None;
        }
        self.holding = false;
        let held = self.pressed_at.take().map(|at| now.duration_since(at)).unwrap_or_default();
        Some(if !keep {
            Talk::Cancel
        } else if held < Duration::from_millis(DICTATION_TAP_MS as u64) {
            Talk::HandsFree
        } else {
            Talk::Stop
        })
    }

    /// Escape: drops a hold, or a recording still listening. Returns None when Escape is not for it.
    pub fn escape(&mut self, phase: Option<DictationPhase>) -> Option<Talk> {
        if self.holding {
            return self.up(false, Instant::now());
        }
        match phase {
            None | Some(DictationPhase::Transcribing) => None,
            Some(_) => Some(Talk::Cancel),
        }
    }
}

/// How loud a reading is once room noise is taken off: silence is 0, and quiet speech still rises.
pub fn loudness(level: f64) -> f64 {
    ((level - 0.15) / 0.85).clamp(0.0, 1.0).powf(0.75)
}

/// A bar's height, as a share of the strip: a dot in silence, full height at the loudest.
pub fn bar_height(level: f64, quiet: bool) -> f64 {
    if quiet {
        0.12
    } else {
        0.12 + loudness(level) * 0.88
    }
}

/// The main composer's dictation as the workspace reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct ComposerDictation {
    pub model: DictationModel,
    pub phase: Option<DictationPhase>,
    /// Listening after the press that started it ended, until it is pressed again.
    pub hands_free: bool,
    /// The keystroke that dictates, as the tooltip shows it, or None when it has none.
    pub binding: Option<String>,
}

pub type TalkHandler = Rc<dyn Fn(Talk, &mut Window, &mut App)>;

/// Held to talk, or tapped to listen hands-free until it is pressed again. The words go into the draft.
pub struct MicButton {
    dictation: ComposerDictation,
    disabled: bool,
    press: TalkPress,
    levels: Entity<DictationLevels>,
    on_talk: TalkHandler,
    focus: gpui::FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl MicButton {
    pub fn new(levels: Entity<DictationLevels>, on_talk: TalkHandler, cx: &mut Context<Self>) -> Self {
        let weak = cx.entity().downgrade();
        let subscriptions = vec![
            cx.observe(&levels, |this, _, cx| {
                if this.listening() {
                    cx.notify();
                }
            }),
            // Escape drops the hold from anywhere, before the window's own Escape handling.
            cx.intercept_keystrokes(move |event, window, cx| {
                if event.keystroke.key != "escape" {
                    return;
                }
                let talk = weak.update(cx, |this, cx| {
                    let talk = this.press.escape(this.dictation.phase);
                    cx.notify();
                    talk.map(|talk| (talk, this.on_talk.clone()))
                });
                if let Ok(Some((talk, on_talk))) = talk {
                    cx.stop_propagation();
                    on_talk(talk, window, cx);
                }
            }),
        ];
        MicButton {
            dictation: ComposerDictation { model: DictationModel::Unsupported, phase: None, hands_free: false, binding: None },
            disabled: false,
            press: TalkPress::default(),
            levels,
            on_talk,
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    pub fn set(&mut self, dictation: ComposerDictation, disabled: bool, cx: &mut Context<Self>) {
        if self.dictation != dictation || self.disabled != disabled {
            let phase = dictation.phase;
            self.dictation = dictation;
            self.disabled = disabled;
            self.levels.update(cx, |levels, cx| levels.sync(phase, cx));
            cx.notify();
        }
    }

    /// Whether the pointer has wandered off a held button, which the strip says too.
    pub fn away(&self) -> bool {
        self.press.away()
    }

    fn listening(&self) -> bool {
        !self.dictation.hands_free && (self.press.pressed() || matches!(self.dictation.phase, Some(DictationPhase::Starting | DictationPhase::Recording)))
    }

    fn talk(&mut self, talk: Option<Talk>, window: &mut Window, cx: &mut Context<Self>) {
        cx.notify();
        if let Some(talk) = talk {
            (self.on_talk)(talk, window, cx);
        }
    }

    fn tip(&self) -> SharedString {
        let with_keys = |label: &str| match &self.dictation.binding {
            Some(binding) => act_ui::tooltip::shortcut_tip(label, binding),
            None => label.to_string().into(),
        };
        if self.dictation.hands_free {
            with_keys("Add the words")
        } else {
            format!("{} · Tap for hands-free", with_keys("Hold to talk")).into()
        }
    }
}

fn outside(bounds: Bounds<Pixels>, position: gpui::Point<Pixels>) -> bool {
    let slack = px(SLACK);
    position.x < bounds.left() - slack || position.x > bounds.right() + slack || position.y < bounds.top() - slack || position.y > bounds.bottom() + slack
}

/// A circle of `diameter` centred on the button.
fn ring(diameter: f32) -> gpui::Div {
    let offset = (BUTTON - diameter) / 2.0;
    div().absolute().left(px(offset)).top(px(offset)).size(px(diameter)).rounded_full()
}

impl Render for MicButton {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let t = &theme.tokens;
        let writing = self.dictation.phase == Some(DictationPhase::Transcribing);
        let hands_free = self.dictation.hands_free;
        let listening = self.listening();
        let away = listening && self.press.away();
        let disabled = self.disabled || writing;
        let level = loudness(self.levels.read(cx).current()) as f32;
        let entity = cx.entity().downgrade();
        let pressed = self.press.pressed();

        let (fill, color): (Option<Rgba>, Rgba) = if away {
            (Some(t.danger_surface), t.danger)
        } else if listening {
            (Some(t.accent), t.accent_ink)
        } else if hands_free {
            (Some(alpha(t.accent, 0.28)), t.accent_soft)
        } else {
            (None, t.muted)
        };
        let glyph = if writing {
            Spinner::new("mic-writing").size(px(16.0)).into_any_element()
        } else {
            let (icon, stroke) = if hands_free {
                (IconName::Check, 2.75)
            } else if away {
                (IconName::X, 2.5)
            } else {
                (IconName::Mic, 2.25)
            };
            Icon::new(icon).size(px(16.0)).stroke(stroke).into_any_element()
        };
        let mut button = div()
            .id("mic-button")
            .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, if hands_free { "Add the words" } else { "Dictate" }).disabled(disabled))
            .track_focus(&self.focus.clone().tab_stop(!disabled))
            .focus_ring(true)
            .relative()
            .flex_none()
            .size(px(BUTTON))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .transition(gpui::Transition::ease(160).only(&[gpui::TransitionProperty::Transform, gpui::TransitionProperty::Background, gpui::TransitionProperty::Color]))
            .when(listening && !away, |button| button.transform(gpui::ElementTransform::scale(1.08)))
            .text_color(color)
            .when_some(fill, |button, fill| button.bg(fill))
            .when(away, |button| button.border_1().border_color(t.danger_line))
            .when(hands_free && !listening, |button| {
                button.shadow(vec![BoxShadow { color: alpha(t.accent, 0.25).into(), offset: point(px(0.0), px(0.0)), blur_radius: px(14.0), spread_radius: px(0.0), inset: false }])
            })
            .when(disabled, |button| button.opacity(t.disabled_opacity))
            .when(!disabled && fill.is_none(), |button| button.cursor_pointer().hover(|button| button.bg(t.wash).text_color(t.ink)))
            .when(!disabled && hands_free && !listening, |button| {
                let lit = alpha(t.accent, 0.38);
                button.cursor_pointer().hover(move |button| button.bg(lit))
            });
        if listening && !away {
            let halo = 0.86 + level * 0.5;
            let rim = 0.9 + level * 0.35;
            button = button
                .child(ring(42.0 * halo).bg(alpha(t.accent, 0.22)))
                .child(ring(54.0 * halo * rim).border_1().border_color(alpha(t.accent, 0.15 + level * 0.5)));
        }
        button = button.child(div().relative().child(glyph));
        let button = button.child(
            canvas(
                |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
                move |bounds, hitbox: Hitbox, window, _| {
                    let down = entity.clone();
                    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                        if phase != DispatchPhase::Bubble || event.button != MouseButton::Left || !hitbox.is_hovered(window) || disabled {
                            return;
                        }
                        // The caret stays in the draft, so Enter still sends while it listens.
                        window.prevent_default();
                        cx.stop_propagation();
                        let _ = down.update(cx, |this, cx| {
                            let talk = this.press.down(this.dictation.hands_free, Instant::now());
                            this.talk(talk, window, cx);
                        });
                    });
                    if !pressed {
                        return;
                    }
                    let moved = entity.clone();
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                        if phase != DispatchPhase::Bubble {
                            return;
                        }
                        let _ = moved.update(cx, |this, cx| {
                            if this.press.moved(outside(bounds, event.position)) {
                                cx.notify();
                            }
                        });
                    });
                    let up = entity.clone();
                    window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                        if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
                            return;
                        }
                        let _ = up.update(cx, |this, cx| {
                            let talk = this.press.up(!outside(bounds, event.position), Instant::now());
                            this.talk(talk, window, cx);
                        });
                    });
                },
            )
            .absolute()
            .inset_0(),
        );
        if listening {
            button.into_any_element()
        } else {
            button.tip(self.tip()).into_any_element()
        }
    }
}

/// Stands in the composer bar while dictation is listening or turning speech into words.
#[derive(IntoElement)]
pub struct DictationStrip {
    phase: DictationPhase,
    away: bool,
    hands_free: bool,
    levels: Entity<DictationLevels>,
    /// Room for the Escape hint, which a narrow composer drops.
    roomy: bool,
}

impl DictationStrip {
    pub fn new(phase: DictationPhase, away: bool, hands_free: bool, levels: Entity<DictationLevels>) -> Self {
        DictationStrip { phase, away, hands_free, levels, roomy: true }
    }

    /// Whether the composer is wider than 520px, where the Escape hint fits.
    pub fn roomy(mut self, roomy: bool) -> Self {
        self.roomy = roomy;
        self
    }
}

impl RenderOnce for DictationStrip {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        let t = theme.tokens.clone();
        let m = &theme.metrics;
        let listening = self.phase != DictationPhase::Transcribing;
        let levels = self.levels.read(cx);
        let readings: Vec<f64> = levels.levels().collect();
        let elapsed = levels.elapsed();
        let away = listening && self.away;
        let reduced = t.reduced_motion;
        let (time_color, bar_color) = if !listening {
            (t.muted, t.muted)
        } else if away {
            (t.danger, t.danger)
        } else {
            (t.accent_text, t.accent_text)
        };
        let hands_free_dot = listening && !away && self.hands_free;

        let dot = move |phase: f32| div().flex_none().size(px(7.0)).mr(px(7.0)).rounded_full().bg(t.accent_text).opacity(1.0 - 0.7 * (phase * std::f32::consts::PI).sin().abs());
        let time = div()
            .flex_none()
            .flex()
            .items_center()
            .min_w(px(m.text_control * 3.2 * 0.6))
            .text_color(time_color)
            .text_size(px(m.text_control))
            .font_weight(FontWeight(560.0))
            .map(act_ui::style::tabular_nums)
            .when(hands_free_dot, |time| {
                time.child(if reduced { dot(0.0).into_any_element() } else { dot(0.0).with_animation("hands-free-pulse", Animation::new(Duration::from_millis(1200)).repeat(), move |_, delta| dot(delta)).into_any_element() })
            })
            .child(elapsed);

        let bars = move |opacity: f32| {
            let readings = readings.clone();
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _| {
                    const WIDTH: f32 = 3.0;
                    const STEP: f32 = 6.0;
                    let height = f32::from(bounds.size.height);
                    let width = f32::from(bounds.size.width);
                    for (index, level) in readings.iter().rev().enumerate() {
                        let right = width - index as f32 * STEP;
                        if right - WIDTH < 0.0 {
                            break;
                        }
                        // The oldest readings fade out towards the strip's left edge.
                        let fade = ((right - WIDTH / 2.0) / (width * 0.3)).clamp(0.0, 1.0);
                        let bar = (bar_height(*level, !listening) as f32 * height).max(3.0);
                        let origin = point(bounds.origin.x + px(right - WIDTH), bounds.origin.y + px((height - bar) / 2.0));
                        let color = alpha(bar_color, bar_color.a * fade * opacity);
                        window.paint_quad(gpui::fill(Bounds { origin, size: size(px(WIDTH), px(bar)) }, color).corner_radii(px(2.0)));
                    }
                },
            )
            .flex_1()
            .min_w(px(40.0))
            .h(px(22.0))
        };
        let bars = if !listening && !reduced {
            div()
                .flex_1()
                .min_w(px(40.0))
                .flex()
                .child(bars(1.0))
                .with_animation("talk-sweep", Animation::new(Duration::from_millis(1400)).repeat(), |strip, delta| strip.opacity(0.4 + 0.6 * (delta * std::f32::consts::PI).sin()))
                .into_any_element()
        } else {
            div().flex_1().min_w(px(40.0)).flex().child(bars(if listening { 1.0 } else { 0.7 })).into_any_element()
        };

        // Hands-free, the two buttons beside the strip say what they do, so it says nothing.
        let hint = (!(listening && !away && self.hands_free)).then(|| {
            let hint = div().flex_none().flex().items_center().gap(px(12.0)).text_color(if away { t.danger } else { t.ink_dim }).text_size(px(m.text_meta)).whitespace_nowrap();
            if !listening {
                hint.child("Writing it down…")
            } else if away {
                hint.child("Let go to cancel")
            } else {
                hint.child("Let go to add it").when(self.roomy, |hint| {
                    hint.child(div().flex().items_center().gap(px(5.0)).text_color(t.muted).child(Kbd::new(["Esc"]).size(KbdSize::Inline)).child("cancels"))
                })
            }
        });

        div().flex().flex_1().items_center().gap(px(14.0)).min_w_0().h(px(32.0)).pl(px(8.0)).child(time).child(bars).children(hint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tap_listens_hands_free_and_a_hold_writes_the_words() {
        let start = Instant::now();
        let mut press = TalkPress::default();
        assert_eq!(press.down(false, start), Some(Talk::Start));
        assert_eq!(press.up(true, start + Duration::from_millis(100)), Some(Talk::HandsFree));
        assert_eq!(press.down(true, start), Some(Talk::Stop));
        assert_eq!(press.down(false, start), Some(Talk::Start));
        assert_eq!(press.up(true, start + Duration::from_millis(600)), Some(Talk::Stop));
    }

    #[test]
    fn letting_go_away_or_escape_cancels() {
        let start = Instant::now();
        let mut press = TalkPress::default();
        press.down(false, start);
        assert!(press.moved(true));
        assert!(press.away());
        assert_eq!(press.up(false, start + Duration::from_secs(1)), Some(Talk::Cancel));
        assert_eq!(press.escape(Some(DictationPhase::Transcribing)), None);
        assert_eq!(press.escape(Some(DictationPhase::Recording)), Some(Talk::Cancel));
        press.down(false, start);
        assert_eq!(press.escape(None), Some(Talk::Cancel));
    }

    #[test]
    fn room_noise_reads_as_silence() {
        assert_eq!(loudness(0.1), 0.0);
        assert_eq!(loudness(1.0), 1.0);
        assert_eq!(bar_height(1.0, true), 0.12);
    }
}
