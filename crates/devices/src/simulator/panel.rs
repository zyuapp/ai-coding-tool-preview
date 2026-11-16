//! The Simulator dock panel (SimulatorPanel.tsx, SimulatorDeviceMenu.tsx, use-screen-input.ts): a
//! toolbar to pick, turn and press the device, and its screen, streamed live and driven by the pointer,
//! trackpad, keyboard and clipboard.

use std::collections::BTreeSet;
use std::rc::Rc;
use std::time::Duration;

use act_core::domain::platform::{
    screen_corner_radius, untilted, SimulatorAvailability, SimulatorButton, SimulatorDevice, SimulatorDeviceState, SimulatorInput, SimulatorOrientation, SimulatorState, TouchPhase,
    MAX_PASTE_TEXT,
};
use act_core::AppCommand;
use act_ui::components::button::{Button, ButtonSize, ButtonVariant, IconButton};
use act_ui::components::empty_state::EmptyState;
use act_ui::components::menu::{Menu, MenuEntry, MenuItem};
use act_ui::components::panel_header::toolbar;
use act_ui::overlay::{bounds_cell, bounds_probe, Align, BoundsCell, Placement};
use act_ui::theme::ActiveTheme;
use act_ui::{Icon, IconName};
use futures::StreamExt;
use gpui::{
    canvas, div, point, prelude::*, px, size, Animation, AnimationExt, App, Bounds, BoxShadow, Context, Corners, DismissEvent, DispatchPhase, Entity, FocusHandle, Focusable,
    FontWeight, Hitbox, HitboxBehavior, KeyUpEvent, Modifiers, ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    Rgba, ScrollWheelEvent, SharedString, Subscription, Task, WeakEntity, Window,
};

use super::devices::simulator_groups;
use super::gestures::{Idle, PinchFingers, Touch, WheelPinch, WheelSwipe, WHEEL_IDLE};
use super::host::SimulatorHost;
use super::keys::{hid_usage, key_down, key_name, KeyAction, ALT, CONTROL, FIRST_MODIFIER, SHIFT};
use super::stream::{ScreenStream, StreamStatus};

/// What the panel asks of the workspace: every press is a command.
pub type CommandHandler = Rc<dyn Fn(AppCommand, &mut Window, &mut App)>;

/// Wide enough for the longest iPad name beside its state.
const MENU_WIDTH: f32 = 320.0;
const STAGE_PADDING: f32 = 18.0;
const FINGER: f32 = 26.0;

/// How a drag with ⌥ held places its second finger: mirrored through the screen's middle, or, with ⇧
/// too, moving alongside the first.
#[derive(Clone, Copy, Debug, PartialEq)]
enum TwoFingers {
    Mirror,
    Alongside { dx: f64, dy: f64 },
}

fn mirrored(x: f64, y: f64) -> (f64, f64) {
    (1.0 - x, 1.0 - y)
}

#[derive(Default)]
struct ScreenInput {
    dragging: bool,
    two_fingers: Option<TwoFingers>,
    /// Where a pinch's two fingers are shown, as fractions of the screen as it is shown.
    marks: Option<PinchFingers>,
    /// Keys the device has down, by HID usage.
    pressed: BTreeSet<i64>,
    /// The usage each key held down was sent as, so its release lets go of the same one.
    held_keys: std::collections::HashMap<String, i64>,
    modifiers: Modifiers,
    swipe: WheelSwipe,
    pinch: WheelPinch,
    swipe_idle: Option<Task<()>>,
    pinch_idle: Option<Task<()>>,
}

/// The screen as it was last drawn: where, and the corner radius it was drawn with.
#[derive(Clone, Copy, Default)]
struct ScreenGeometry {
    bounds: Option<Bounds<Pixels>>,
}

pub struct SimulatorPanel {
    host: SimulatorHost,
    simulator: SimulatorState,
    /// The device this dock shows.
    udid: Option<String>,
    /// Whether the panel is the tab on screen, which is the only time the device's screen is streamed.
    active: bool,
    on_command: CommandHandler,
    stream: Option<ScreenStream>,
    attempt: usize,
    stream_task: Option<Task<()>>,
    menu: Option<(Entity<Menu>, Subscription)>,
    trigger: BoundsCell,
    focus: FocusHandle,
    input: ScreenInput,
    geometry: std::rc::Rc<std::cell::Cell<ScreenGeometry>>,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for SimulatorPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl SimulatorPanel {
    pub fn new(host: SimulatorHost, on_command: CommandHandler, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        let weak = cx.entity().downgrade();
        let intercept_focus = focus.clone();
        let subscriptions = vec![
            cx.on_focus(&focus, window, |this, window, cx| this.screen_focused(true, window, cx)),
            cx.on_blur(&focus, window, |this, window, cx| this.screen_focused(false, window, cx)),
            // Keys reach the device before the app's own bindings, as the page took them first in a browser.
            cx.intercept_keystrokes(move |event, window, cx| {
                if !intercept_focus.is_focused(window) {
                    return;
                }
                let keystroke = event.keystroke.clone();
                let taken = weak.update(cx, |this, cx| this.key_pressed(&keystroke, window, cx)).unwrap_or(false);
                if taken {
                    cx.stop_propagation();
                }
            }),
        ];
        #[cfg(target_os = "macos")]
        let subscriptions = {
            let mut subscriptions = subscriptions;
            subscriptions.extend(super::magnify::watch(window, cx));
            subscriptions
        };
        SimulatorPanel {
            host,
            simulator: SimulatorState::default(),
            udid: None,
            active: false,
            on_command,
            stream: None,
            attempt: 0,
            stream_task: None,
            menu: None,
            trigger: bounds_cell(),
            focus,
            input: ScreenInput::default(),
            geometry: Default::default(),
            _subscriptions: subscriptions,
        }
    }

    /// What the workspace says: the devices, which one this dock shows, and whether the tab is on screen.
    pub fn set(&mut self, simulator: SimulatorState, udid: Option<String>, active: bool, cx: &mut Context<Self>) {
        let device_changed = udid != self.udid;
        let orientation_before = self.orientation();
        self.simulator = simulator;
        self.udid = udid;
        self.active = active;
        // A finger the trackpad left down lifts on the device it landed on once the device or the way it is held changes.
        if device_changed || orientation_before != self.orientation() {
            self.lift_gestures_on(orientation_before, cx);
        }
        self.reconcile_stream(cx);
        cx.notify();
    }

    fn device(&self) -> Option<&SimulatorDevice> {
        let udid = self.udid.as_deref()?;
        self.simulator.devices.iter().find(|device| device.udid == udid)
    }

    fn orientation(&self) -> SimulatorOrientation {
        self.device().and_then(|device| self.simulator.orientations.get(&device.udid).copied()).unwrap_or(SimulatorOrientation::Portrait)
    }

    fn booted(&self) -> bool {
        self.device().is_some_and(|device| device.state == SimulatorDeviceState::Booted)
    }

    fn reconcile_stream(&mut self, cx: &mut Context<Self>) {
        let wanted = (self.active && self.booted()).then(|| self.udid.clone()).flatten();
        if self.stream.as_ref().map(|stream| stream.udid()) != wanted.as_deref() {
            self.stream = None;
            self.stream_task = None;
            if let Some(udid) = wanted {
                self.start_stream(&udid, cx);
            }
        }
        if let Some(stream) = &self.stream {
            stream.set_turns(self.orientation().turns());
        }
    }

    fn start_stream(&mut self, udid: &str, cx: &mut Context<Self>) {
        let (sender, mut receiver) = futures::channel::mpsc::unbounded::<()>();
        let stream = ScreenStream::start(&self.host, udid, move || {
            let _ = sender.unbounded_send(());
        });
        stream.set_turns(self.orientation().turns());
        self.stream = Some(stream);
        self.stream_task = Some(cx.spawn(async move |this, cx| {
            while receiver.next().await.is_some() {
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        }));
    }

    fn retry_stream(&mut self, cx: &mut Context<Self>) {
        self.attempt += 1;
        self.stream = None;
        self.stream_task = None;
        self.reconcile_stream(cx);
        cx.notify();
    }

    fn dispatch(&self, command: AppCommand, window: &mut Window, cx: &mut App) {
        (self.on_command)(command, window, cx);
    }

    fn send(&self, input: SimulatorInput, window: &mut Window, cx: &mut App) {
        if let Some(udid) = self.udid.clone() {
            self.dispatch(AppCommand::SimulatorInput { task_id: None, udid: Some(udid), input }, window, cx);
        }
    }

    /// A touch on one device held one way: it lands on the device's own portrait screen, wherever the turned picture puts it.
    fn touch(&self, phase: TouchPhase, shown: (f64, f64), second: Option<(f64, f64)>, orientation: SimulatorOrientation, window: &mut Window, cx: &mut App) {
        let (x, y) = untilted(shown.0, shown.1, orientation);
        let other = second.map(|(x2, y2)| untilted(x2, y2, orientation));
        self.send(SimulatorInput::Touch { phase, x, y, x2: other.map(|point| point.0), y2: other.map(|point| point.1) }, window, cx);
    }

    fn second_finger(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        match self.input.two_fingers? {
            TwoFingers::Mirror => Some(mirrored(x, y)),
            TwoFingers::Alongside { dx, dy } => Some(((x + dx).clamp(0.0, 1.0), (y + dy).clamp(0.0, 1.0))),
        }
    }

    fn show_fingers(&mut self, fingers: Option<PinchFingers>, cx: &mut Context<Self>) {
        if self.input.marks != fingers {
            self.input.marks = fingers;
            cx.notify();
        }
    }

    fn lift_gestures_on(&mut self, orientation: SimulatorOrientation, cx: &mut Context<Self>) {
        self.input.swipe_idle = None;
        self.input.pinch_idle = None;
        let swipe = self.input.swipe.lift();
        let pinch = self.input.pinch.lift();
        let had_pinch = pinch.is_some();
        let (udid, on_command) = (self.udid.clone(), self.on_command.clone());
        let Some(udid) = udid else { return };
        let mut inputs = Vec::new();
        if let Some(Touch { phase, x, y }) = swipe {
            let (x, y) = untilted(x, y, orientation);
            inputs.push(SimulatorInput::Touch { phase, x, y, x2: None, y2: None });
        }
        if let Some((phase, fingers)) = pinch {
            let (x, y) = untilted(fingers.x, fingers.y, orientation);
            let (x2, y2) = untilted(fingers.x2, fingers.y2, orientation);
            inputs.push(SimulatorInput::Touch { phase, x, y, x2: Some(x2), y2: Some(y2) });
        }
        if had_pinch {
            self.show_fingers(None, cx);
        }
        if inputs.is_empty() {
            return;
        }
        cx.defer(move |cx| {
            let Some(window) = cx.active_window() else { return };
            let _ = window.update(cx, |_, window, cx| {
                for input in inputs {
                    on_command(AppCommand::SimulatorInput { task_id: None, udid: Some(udid.clone()), input }, window, cx);
                }
            });
        });
    }

    fn pointer_down(&mut self, at: (f64, f64), modifiers: Modifiers, window: &mut Window, cx: &mut Context<Self>) {
        self.lift_gestures_on(self.orientation(), cx);
        window.focus(&self.focus);
        self.input.dragging = true;
        let landing = mirrored(at.0, at.1);
        self.input.two_fingers = if !modifiers.alt {
            None
        } else if modifiers.shift {
            Some(TwoFingers::Alongside { dx: landing.0 - at.0, dy: landing.1 - at.1 })
        } else {
            Some(TwoFingers::Mirror)
        };
        let second = self.second_finger(at.0, at.1);
        self.show_fingers(second.map(|(x2, y2)| PinchFingers { x: at.0, y: at.1, x2, y2 }), cx);
        self.touch(TouchPhase::Down, at, second, self.orientation(), window, cx);
    }

    fn pointer_move(&mut self, at: (f64, f64), modifiers: Modifiers, over: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.input.dragging {
            // Holding ⌥ over the screen shows where a pinch's two fingers would land.
            let marks = (over && modifiers.alt).then(|| {
                let (x2, y2) = mirrored(at.0, at.1);
                PinchFingers { x: at.0, y: at.1, x2, y2 }
            });
            if self.input.pinch.fingers().is_none() {
                self.show_fingers(marks, cx);
            }
            return;
        }
        let second = self.second_finger(at.0, at.1);
        if let Some((x2, y2)) = second {
            self.show_fingers(Some(PinchFingers { x: at.0, y: at.1, x2, y2 }), cx);
        }
        self.touch(TouchPhase::Move, at, second, self.orientation(), window, cx);
    }

    fn pointer_up(&mut self, at: (f64, f64), modifiers: Modifiers, window: &mut Window, cx: &mut Context<Self>) {
        if !self.input.dragging {
            return;
        }
        self.input.dragging = false;
        let second = self.second_finger(at.0, at.1);
        self.touch(TouchPhase::Up, at, second, self.orientation(), window, cx);
        self.input.two_fingers = None;
        let marks = modifiers.alt.then(|| {
            let (x2, y2) = mirrored(at.0, at.1);
            PinchFingers { x: at.0, y: at.1, x2, y2 }
        });
        self.show_fingers(marks, cx);
    }

    fn arm_idle(&mut self, pinch: bool, cx: &mut Context<Self>) {
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(WHEEL_IDLE).await;
            let _ = this.update(cx, |this, cx| this.idle(pinch, cx));
        });
        if pinch {
            self.input.pinch_idle = Some(task);
        } else {
            self.input.swipe_idle = Some(task);
        }
    }

    fn idle(&mut self, pinch: bool, cx: &mut Context<Self>) {
        let orientation = self.orientation();
        let Some(udid) = self.udid.clone() else { return };
        let input = if pinch {
            self.input.pinch_idle = None;
            let lifted = self.input.pinch.lift();
            self.show_fingers(None, cx);
            lifted.map(|(phase, fingers)| {
                let (x, y) = untilted(fingers.x, fingers.y, orientation);
                let (x2, y2) = untilted(fingers.x2, fingers.y2, orientation);
                SimulatorInput::Touch { phase, x, y, x2: Some(x2), y2: Some(y2) }
            })
        } else {
            self.input.swipe_idle = None;
            self.input.swipe.lift().map(|touch| {
                let (x, y) = untilted(touch.x, touch.y, orientation);
                SimulatorInput::Touch { phase: touch.phase, x, y, x2: None, y2: None }
            })
        };
        let Some(input) = input else { return };
        let on_command = self.on_command.clone();
        cx.defer(move |cx| {
            let Some(window) = cx.active_window() else { return };
            let _ = window.update(cx, |_, window, cx| on_command(AppCommand::SimulatorInput { task_id: None, udid: Some(udid), input }, window, cx));
        });
    }

    fn wheel(&mut self, at: (f64, f64), delta: Point<Pixels>, screen: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let orientation = self.orientation();
        if let Some(lifted) = self.input.pinch.lift() {
            self.input.pinch_idle = None;
            self.show_fingers(None, cx);
            self.pinch_touches(vec![lifted], orientation, window, cx);
        }
        // A browser's wheel runs opposite to the trackpad's own deltas.
        let dx = -f64::from(f32::from(delta.x)) / f64::from(f32::from(screen.size.width));
        let dy = -f64::from(f32::from(delta.y)) / f64::from(f32::from(screen.size.height));
        let (touches, idle) = self.input.swipe.wheel(at.0, at.1, dx, dy);
        for touch in touches {
            self.touch(touch.phase, (touch.x, touch.y), None, orientation, window, cx);
        }
        match idle {
            Idle::Restart => self.arm_idle(false, cx),
            Idle::Stop => self.input.swipe_idle = None,
        }
    }

    fn pinch_touches(&self, touches: Vec<(TouchPhase, PinchFingers)>, orientation: SimulatorOrientation, window: &mut Window, cx: &mut App) {
        for (phase, fingers) in touches {
            self.touch(phase, (fingers.x, fingers.y), Some((fingers.x2, fingers.y2)), orientation, window, cx);
        }
    }

    /// A trackpad pinch over the screen, as macOS reports its magnification.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn magnify(&mut self, position: Point<Pixels>, magnification: f64, ended: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(screen) = self.geometry.get().bounds else { return };
        if self.input.pinch.fingers().is_none() && !screen.contains(&position) {
            return;
        }
        if !self.booted() {
            return;
        }
        let orientation = self.orientation();
        if let Some(lifted) = self.input.swipe.lift() {
            self.input.swipe_idle = None;
            self.touch(lifted.phase, (lifted.x, lifted.y), None, orientation, window, cx);
        }
        let at = shown_at(screen, position);
        if ended {
            self.input.pinch_idle = None;
            if let Some(lifted) = self.input.pinch.lift() {
                self.pinch_touches(vec![lifted], orientation, window, cx);
            }
            self.show_fingers(None, cx);
            return;
        }
        let touches = self.input.pinch.magnify(at.0, at.1, magnification);
        self.pinch_touches(touches, orientation, window, cx);
        self.show_fingers(self.input.pinch.fingers(), cx);
        self.arm_idle(true, cx);
    }

    fn release_keys(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.held_keys.clear();
        for usage in std::mem::take(&mut self.input.pressed) {
            self.send(SimulatorInput::Key { usage, down: false }, window, cx);
        }
    }

    fn screen_focused(&mut self, focused: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.udid.is_none() {
            return;
        }
        if !focused {
            self.release_keys(window, cx);
            self.input.modifiers = Modifiers::default();
        }
        let phase = if focused { act_core::domain::platform::PasteboardPhase::Enter } else { act_core::domain::platform::PasteboardPhase::Leave };
        self.send(SimulatorInput::Pasteboard { phase }, window, cx);
        cx.notify();
    }

    /// A key going down while the screen has focus. Returns whether the device took it.
    fn key_pressed(&mut self, keystroke: &gpui::Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.udid.is_none() {
            return false;
        }
        let modifiers = keystroke.modifiers;
        let name = key_name(&keystroke.key, keystroke.key_char.as_deref(), modifiers.platform, modifiers.control);
        let held = hid_usage(&name).is_some_and(|usage| self.input.pressed.contains(&usage));
        match key_down(&name, modifiers.platform, modifiers.shift, held, self.orientation()) {
            KeyAction::Pass => false,
            KeyAction::Swallow => true,
            KeyAction::Paste => {
                let text = cx.read_from_clipboard().and_then(|item| item.text()).unwrap_or_default();
                let text: String = text.chars().take(MAX_PASTE_TEXT).collect();
                if !text.is_empty() {
                    self.send(SimulatorInput::Paste { text }, window, cx);
                }
                true
            }
            KeyAction::Send(input) => {
                if let SimulatorInput::Key { usage, .. } = input {
                    self.input.pressed.insert(usage);
                    self.input.held_keys.insert(keystroke.key.clone(), usage);
                }
                self.send(input, window, cx);
                true
            }
        }
    }

    fn key_released(&mut self, event: &KeyUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        let held = self.input.held_keys.remove(&event.keystroke.key);
        let Some(usage) = held.or_else(|| hid_usage(&event.keystroke.key)) else { return };
        if held.is_none() && event.keystroke.modifiers.platform && usage < FIRST_MODIFIER {
            return;
        }
        if self.input.pressed.remove(&usage) {
            self.send(SimulatorInput::Key { usage, down: false }, window, cx);
        }
        cx.stop_propagation();
    }

    /// ⇧, ⌃ and ⌥ reach the device as keys of their own, which a shortcut such as ⌘⇧Z needs.
    fn modifiers_changed(&mut self, event: &ModifiersChangedEvent, window: &mut Window, cx: &mut Context<Self>) {
        let before = self.input.modifiers;
        let now = event.modifiers;
        self.input.modifiers = now;
        for (usage, was, is) in [(SHIFT, before.shift, now.shift), (CONTROL, before.control, now.control), (ALT, before.alt, now.alt)] {
            if was == is {
                continue;
            }
            if is && self.input.pressed.insert(usage) {
                self.send(SimulatorInput::Key { usage, down: true }, window, cx);
            } else if !is && self.input.pressed.remove(&usage) {
                self.send(SimulatorInput::Key { usage, down: false }, window, cx);
            }
        }
        // ⌥ over the screen previews a pinch's two fingers.
        if !self.input.dragging && before.alt != now.alt {
            if let Some(bounds) = self.geometry.get().bounds {
                let position = window.mouse_position();
                let over = bounds.contains(&position);
                self.pointer_move(shown_at(bounds, position), now, over, window, cx);
            }
        }
    }

    /// Opens the device picker, as a press on the device's name does.
    pub fn open_device_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.is_none() {
            self.toggle_menu(window, cx);
        }
    }

    fn toggle_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
            return;
        }
        let Some(anchor) = self.trigger.get() else { return };
        let devices = self.simulator.devices.clone();
        let selected = self.udid.clone();
        let empty: SharedString = match &self.simulator.availability {
            SimulatorAvailability::Unsupported { reason } => reason.clone().into(),
            SimulatorAvailability::Unknown => "Looking for simulators…".into(),
            SimulatorAvailability::Available => "No simulators. Add one in Xcode.".into(),
        };
        let on_command = self.on_command.clone();
        let dots = (cx.theme().tokens.success, cx.theme().tokens.quiet_strong);
        let entries = move |query: &str| device_entries(&devices, query, selected.as_deref(), &empty, dots, &on_command);
        let initial = entries("");
        let trigger = self.trigger.clone();
        let menu = Menu::build(initial, Placement::Anchor { anchor, align: Align::Start }, window, cx, move |menu| {
            let menu = menu.named("Simulators").top_level();
            menu.checkable(true).min_width(px(MENU_WIDTH)).max_width(px(MENU_WIDTH)).exempt(&trigger).searchable("Search simulators", move |query, _| entries(query))
        });
        let subscription = cx.subscribe(&menu, |this, _, _: &DismissEvent, cx| {
            this.menu = None;
            cx.notify();
        });
        self.menu = Some((menu, subscription));
        cx.notify();
    }

    fn render_trigger(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let t = &theme.tokens;
        let m = &theme.metrics;
        let selected = self.device();
        let open = self.menu.is_some();
        let label: SharedString = selected.map(|device| device.name.clone()).unwrap_or_else(|| "Simulator".into()).into();
        let name = selected.map(|device| format!("Simulator: {}, {}", device.name, device.runtime)).unwrap_or_else(|| "Choose a simulator".into());
        div()
            .id("simulator-device-trigger")
            .accessible(gpui::AccessInfo::new(gpui::AccessRole::PopupButton, name).expanded(open))
            .relative()
            .min_w_0()
            .min_h(px(30.0))
            .flex()
            .items_center()
            .gap(px(7.0))
            .rounded(px(8.0))
            .px(px(8.0))
            .text_color(t.ink_soft)
            .text_size(px(m.text_control))
            .font_weight(FontWeight(560.0))
            .cursor_pointer()
            .when(open, |this| this.bg(t.surface_hover))
            .hover(|this| this.bg(t.surface_hover))
            .child(Icon::new(device_icon(selected.map(|device| device.name.as_str()).unwrap_or(""))).size(px(16.0)).color(t.quiet_strong))
            .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(label))
            .when_some(selected, |this, device| {
                this.child(div().flex_none().text_color(t.quiet_strong).font_weight(FontWeight(450.0)).text_size(px(m.text_meta)).child(device.runtime.clone()))
            })
            .child(Icon::new(IconName::ChevronDown).size(px(14.0)).color(t.quiet_strong))
            .child(bounds_probe(&self.trigger))
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .on_click(cx.listener(|this, _, window, cx| this.toggle_menu(window, cx)))
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let t = &theme.tokens;
        let m = &theme.metrics;
        let device = self.device().cloned();
        let booted = self.booted();
        let driving = device.as_ref().is_some_and(|device| self.simulator.driver.as_ref().is_some_and(|driver| driver.udid == device.udid));
        let orientation = self.orientation();
        let mut bar = toolbar(cx).gap(px(6.0)).child(self.render_trigger(cx));
        if driving {
            let reduced = t.reduced_motion;
            bar = bar.child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .rounded_full()
                    .pt(px(3.0))
                    .pb(px(3.0))
                    .pl(px(8.0))
                    .pr(px(9.0))
                    .bg(t.info_surface)
                    .text_color(t.info)
                    .text_size(px(m.text_meta))
                    .font_weight(FontWeight(550.0))
                    .whitespace_nowrap()
                    .child(pulse_dot("simulator-driving-dot", t.info, reduced))
                    .child("Agent is using this device"),
            );
        }
        bar = bar.child(div().flex_1());
        if let Some(device) = device {
            let buttons = [
                ("simulator-rotate-left", IconName::RotateCcw, "Rotate left", SimulatorInput::Rotate { orientation: orientation.turned(-1) }),
                ("simulator-rotate-right", IconName::RotateCw, "Rotate right", SimulatorInput::Rotate { orientation: orientation.turned(1) }),
                ("simulator-home", IconName::House, "Home", SimulatorInput::Button { button: SimulatorButton::Home }),
                ("simulator-lock", IconName::Lock, "Lock", SimulatorInput::Button { button: SimulatorButton::Lock }),
            ];
            for (id, icon, label, input) in buttons {
                let udid = device.udid.clone();
                let on_command = self.on_command.clone();
                bar = bar.child(IconButton::new(id, icon, label).icon_size(16.0).disabled(!booted).on_click(move |_, window, cx| {
                    on_command(AppCommand::SimulatorInput { task_id: None, udid: Some(udid.clone()), input: input.clone() }, window, cx);
                }));
            }
        }
        bar
    }

    fn render_notice(&self, cx: &mut Context<Self>) -> Option<EmptyState> {
        let availability = &self.simulator.availability;
        if let SimulatorAvailability::Unsupported { reason } = availability {
            return Some(EmptyState::new().icon(IconName::Smartphone).title("Simulator unavailable").detail(reason.clone()));
        }
        let Some(device) = self.device().cloned() else {
            let unknown = *availability == SimulatorAvailability::Unknown;
            let notice = EmptyState::new().icon(IconName::Smartphone).spinning(unknown).title(if unknown { "Looking for simulators" } else { "No simulators" });
            return Some(if unknown { notice } else { notice.detail("Add an iOS simulator in Xcode, then open this panel again.") });
        };
        let booting = self.simulator.booting.contains(&device.udid) || device.state == SimulatorDeviceState::Booting;
        let select = |label: &'static str, cx: &mut Context<Self>| {
            let on_command = self.on_command.clone();
            let udid = device.udid.clone();
            let _ = cx;
            Button::new(SharedString::from(format!("simulator-{label}")), label)
                .variant(ButtonVariant::Soft)
                .size(ButtonSize::Lg)
                .on_click(move |_, window, cx| on_command(AppCommand::SimulatorSelect { udid: udid.clone() }, window, cx))
        };
        if let Some(error) = self.simulator.errors.get(&device.udid).filter(|_| !booting) {
            return Some(
                EmptyState::new()
                    .danger()
                    .icon(IconName::TriangleAlert)
                    .title(format!("{} could not start", device.name))
                    .detail(error.clone())
                    .action(select("Try again", cx)),
            );
        }
        if device.state != SimulatorDeviceState::Booted {
            return Some(if booting {
                EmptyState::new().icon(IconName::Smartphone).spinning(true).title(format!("Starting {}", device.name)).detail("It runs in the background, so you can keep working.")
            } else {
                EmptyState::new().icon(IconName::Smartphone).title(format!("{} is off", device.name)).action(select("Start", cx))
            });
        }
        match self.stream.as_ref().map(ScreenStream::status) {
            Some(StreamStatus::Failed(error)) => Some(
                EmptyState::new().danger().icon(IconName::TriangleAlert).title(format!("Could not show {}", device.name)).detail(error).action(
                    Button::new("simulator-stream-retry", "Try again")
                        .variant(ButtonVariant::Soft)
                        .size(ButtonSize::Lg)
                        .on_click(cx.listener(|this, _, _, cx| this.retry_stream(cx))),
                ),
            ),
            Some(StreamStatus::Showing { .. }) => None,
            _ => Some(EmptyState::new().icon(IconName::Smartphone).spinning(true).title(format!("Connecting to {}", device.name))),
        }
    }

    fn render_screen(&mut self, window: &Window, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let device = self.device()?.clone();
        if device.state != SimulatorDeviceState::Booted {
            return None;
        }
        let stream = self.stream.as_ref()?;
        let StreamStatus::Showing { width, height } = stream.status() else {
            stream.mark_drawn();
            return None;
        };
        #[cfg(target_os = "macos")]
        let picture = stream.picture();
        let orientation = self.orientation();
        let sideways = orientation.turns() % 2 == 1;
        let shown = if sideways { (height as f32, width as f32) } else { (width as f32, height as f32) };
        let corner = screen_corner_radius(&device.name) as f32 * (width.min(height) as f32);
        let theme = cx.theme().clone();
        let t = theme.tokens.clone();
        let driven = self.simulator.driver.as_ref().is_some_and(|driver| driver.udid == device.udid);
        let focused_ring = self.focus.is_focused(window) && act_ui::focus::keyboard_focus_visible(cx);
        let marks = self.input.marks;
        let geometry = self.geometry.clone();
        let entity = cx.entity().downgrade();
        Some(
            div()
                .id("simulator-screen")
                .when_some(self.device(), |screen, device| screen.accessible(gpui::AccessInfo::new(gpui::AccessRole::Group, format!("{} screen", device.name))))
                .track_focus(&self.focus)
                .key_context("SimulatorScreen")
                .size_full()
                .on_key_up(cx.listener(|this, event: &KeyUpEvent, window, cx| this.key_released(event, window, cx)))
                .on_modifiers_changed(cx.listener(|this, event: &ModifiersChangedEvent, window, cx| this.modifiers_changed(event, window, cx)))
                .child(
                    canvas(
                        move |bounds, window, _| {
                            let screen = fit(bounds, shown);
                            let hitbox = window.insert_hitbox(screen, HitboxBehavior::Normal);
                            geometry.set(ScreenGeometry { bounds: Some(screen) });
                            (screen, hitbox)
                        },
                        move |_, (screen, hitbox): (Bounds<Pixels>, Hitbox), window, _cx| {
                            let radius = px(corner * f32::from(screen.size.width) / shown.0);
                            paint_screen_frame(screen, radius, driven, focused_ring, &t, window);
                            #[cfg(target_os = "macos")]
                            if let Some(picture) = picture {
                                window.paint_surface_rounded(screen, Corners::all(radius), picture.0);
                            }
                            paint_screen_rings(screen, radius, driven, focused_ring, &t, window);
                            if let Some(fingers) = marks {
                                paint_finger(screen, fingers.x, fingers.y, window);
                                paint_finger(screen, fingers.x2, fingers.y2, window);
                            }
                            register_pointer(entity, screen, hitbox, window);
                        },
                    )
                    .size_full(),
                ),
        )
    }
}

fn device_icon(name: &str) -> IconName {
    if name.starts_with("iPad") {
        IconName::Tablet
    } else {
        IconName::Smartphone
    }
}

fn device_entries(devices: &[SimulatorDevice], query: &str, selected: Option<&str>, empty: &SharedString, dots: (Rgba, Rgba), on_command: &CommandHandler) -> Vec<MenuEntry> {
    let groups = simulator_groups(devices, query);
    if groups.is_empty() {
        let text: SharedString = if devices.is_empty() { empty.clone() } else { "No simulator matches".into() };
        return vec![MenuEntry::Note { text, danger: false }];
    }
    groups
        .into_iter()
        .map(|group| MenuEntry::Group {
            label: group.label.clone().into(),
            items: group
                .devices
                .iter()
                .map(|device| {
                    let udid = device.udid.clone();
                    let on_command = on_command.clone();
                    let mut item = MenuItem::new(device.name.clone())
                        .icon(device_icon(&device.name))
                        .checked(Some(device.udid.as_str()) == selected)
                        .on_select(move |window, cx| on_command(AppCommand::SimulatorSelect { udid: udid.clone() }, window, cx));
                    if group.running {
                        let booted = device.state == SimulatorDeviceState::Booted;
                        let state = if booted { "Running" } else { "Starting" };
                        item = item.detail(format!("{state} · {}", device.runtime)).detail_dot(if booted { dots.0 } else { dots.1 });
                    }
                    item
                })
                .collect(),
        })
        .collect()
}

/// A dot that fades and shrinks while something is going on (`simulator-pulse`).
fn pulse_dot(id: &'static str, color: Rgba, reduced: bool) -> impl IntoElement {
    let dot = move |phase: f32| {
        let dim = 1.0 - (phase * std::f32::consts::PI).sin().abs();
        let scale = 0.8 + 0.2 * dim;
        let opacity = 0.35 + 0.65 * dim;
        div().flex_none().size(px(6.0 * scale)).m(px(3.0 * (1.0 - scale))).rounded_full().bg(color).opacity(opacity)
    };
    div().flex_none().size(px(6.0)).flex().items_center().justify_center().child(if reduced {
        dot(0.0).into_any_element()
    } else {
        dot(0.0).with_animation(id, Animation::new(Duration::from_millis(1400)).repeat(), move |_, delta| dot(delta)).into_any_element()
    })
}

/// The largest box of the screen's shape that fits, centred.
fn fit(bounds: Bounds<Pixels>, shown: (f32, f32)) -> Bounds<Pixels> {
    let aspect = shown.0 / shown.1;
    let available = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let width = available.0.min(available.1 * aspect).max(1.0);
    let height = width / aspect;
    let origin = point(bounds.origin.x + px((available.0 - width) / 2.0), bounds.origin.y + px((available.1 - height) / 2.0));
    Bounds { origin, size: size(px(width), px(height)) }.map(|value| px(f32::from(value).round()))
}

/// Where a pointer is on the screen as it is shown, as fractions of it from its top left.
fn shown_at(screen: Bounds<Pixels>, position: Point<Pixels>) -> (f64, f64) {
    let x = f32::from(position.x - screen.origin.x) / f32::from(screen.size.width);
    let y = f32::from(position.y - screen.origin.y) / f32::from(screen.size.height);
    (f64::from(x).clamp(0.0, 1.0), f64::from(y).clamp(0.0, 1.0))
}

fn grow(bounds: Bounds<Pixels>, by: f32) -> Bounds<Pixels> {
    Bounds { origin: point(bounds.origin.x - px(by), bounds.origin.y - px(by)), size: size(bounds.size.width + px(by * 2.0), bounds.size.height + px(by * 2.0)) }
}

/// The drop shadow and the black behind the picture.
fn paint_screen_frame(screen: Bounds<Pixels>, radius: Pixels, driven: bool, focused: bool, t: &act_ui::Tokens, window: &mut Window) {
    let _ = (driven, focused, t);
    window.paint_shadows(
        screen,
        Corners::all(radius),
        &[BoxShadow { color: gpui::hsla(0.0, 0.0, 0.0, 0.45), offset: point(px(0.0), px(18.0)), blur_radius: px(40.0), spread_radius: px(-18.0) }],
    );
    window.paint_quad(gpui::fill(screen, gpui::black()).corner_radii(radius));
}

/// The 1px line round the screen, or the info ring while a run drives it or the keyboard is on it.
fn paint_screen_rings(screen: Bounds<Pixels>, radius: Pixels, driven: bool, focused: bool, t: &act_ui::Tokens, window: &mut Window) {
    let ring = |bounds: Bounds<Pixels>, radius: Pixels, width: f32, color: Rgba| {
        gpui::quad(bounds, radius, gpui::transparent_black(), px(width), color, gpui::BorderStyle::Solid)
    };
    if driven {
        window.paint_quad(ring(grow(screen, 6.0), radius + px(6.0), 4.0, t.info_surface));
    }
    if driven || focused {
        window.paint_quad(ring(grow(screen, 2.0), radius + px(2.0), 2.0, t.info));
    } else {
        window.paint_quad(ring(grow(screen, 1.0), radius + px(1.0), 1.0, t.line_strong));
    }
}

fn paint_finger(screen: Bounds<Pixels>, x: f64, y: f64, window: &mut Window) {
    let centre = point(screen.origin.x + screen.size.width * x as f32, screen.origin.y + screen.size.height * y as f32);
    let bounds = Bounds { origin: point(centre.x - px(FINGER / 2.0), centre.y - px(FINGER / 2.0)), size: size(px(FINGER), px(FINGER)) };
    window.paint_shadows(bounds, Corners::all(px(FINGER / 2.0)), &[BoxShadow { color: gpui::hsla(0.0, 0.0, 0.0, 0.35), offset: point(px(0.0), px(2.0)), blur_radius: px(8.0), spread_radius: px(0.0) }]);
    window.paint_quad(gpui::quad(
        grow(bounds, 1.5),
        px(FINGER / 2.0 + 1.5),
        gpui::hsla(0.0, 0.0, 1.0, 0.32),
        px(1.5),
        gpui::hsla(0.0, 0.0, 1.0, 0.7),
        gpui::BorderStyle::Solid,
    ));
}

/// The pointer on the screen: a press lands a finger, a drag moves it anywhere in the window until it
/// lifts, and the trackpad's scrolling drags a finger of its own.
fn register_pointer(entity: WeakEntity<SimulatorPanel>, screen: Bounds<Pixels>, hitbox: Hitbox, window: &mut Window) {
    let down = entity.clone();
    let down_hitbox = hitbox.clone();
    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble || event.button != MouseButton::Left || !down_hitbox.is_hovered(window) {
            return;
        }
        cx.stop_propagation();
        let modifiers = event.modifiers;
        let _ = down.update(cx, |this, cx| this.pointer_down(shown_at(screen, event.position), modifiers, window, cx));
    });
    let moved = entity.clone();
    let move_hitbox = hitbox.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble {
            return;
        }
        let over = move_hitbox.is_hovered(window);
        let _ = moved.update(cx, |this, cx| {
            if this.input.dragging || over || this.input.marks.is_some() {
                this.pointer_move(shown_at(screen, event.position), event.modifiers, over, window, cx);
            }
        });
    });
    let up = entity.clone();
    window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
            return;
        }
        let _ = up.update(cx, |this, cx| this.pointer_up(shown_at(screen, event.position), event.modifiers, window, cx));
    });
    window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble || !hitbox.should_handle_scroll(window) {
            return;
        }
        cx.stop_propagation();
        let delta = event.delta.pixel_delta(px(16.0));
        let at = shown_at(screen, event.position);
        let _ = entity.update(cx, |this, cx| {
            if event.modifiers.control {
                let orientation = this.orientation();
                if let Some(lifted) = this.input.swipe.lift() {
                    this.input.swipe_idle = None;
                    this.touch(lifted.phase, (lifted.x, lifted.y), None, orientation, window, cx);
                }
                let touches = this.input.pinch.pinch(at.0, at.1, -f64::from(f32::from(delta.y)));
                this.pinch_touches(touches, orientation, window, cx);
                this.show_fingers(this.input.pinch.fingers(), cx);
                this.arm_idle(true, cx);
                return;
            }
            this.wheel(at, delta, screen, window, cx);
        });
    });
}

impl Render for SimulatorPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let t = &theme.tokens;
        let notice = self.render_notice(cx);
        let screen = self.render_screen(window, cx);
        let toolbar = self.render_toolbar(cx);
        div()
            .id("simulator-panel")
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(toolbar)
            .child(
                div()
                    .relative()
                    .min_h_0()
                    .flex_1()
                    .bg(t.dock_sunken)
                    .when_some(screen, |stage, screen| stage.child(div().absolute().inset(px(STAGE_PADDING)).child(screen)))
                    .when_some(notice, |stage, notice| stage.child(div().absolute().inset_0().flex().flex_col().child(notice))),
            )
            .when_some(self.menu.as_ref(), |panel, (menu, _)| panel.child(menu.clone()))
    }
}
