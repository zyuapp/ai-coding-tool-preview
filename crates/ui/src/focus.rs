//! Whether focus was reached by the keyboard (focus-appearance.ts): a control draws its focus ring
//! only then, as `:focus-visible` does, so a click never leaves a ring behind.

use gpui::{App, FocusHandle, Global, Keystroke, Window};

#[derive(Default)]
pub struct FocusAppearance {
    keyboard: bool,
    /// What held focus after the last keystroke, so a movement key shows rings only when it moved focus.
    last_focus: Option<FocusHandle>,
}

impl Global for FocusAppearance {}

const ACTIVATION_KEYS: &[&str] = &["tab", "enter", "space", "escape"];
const MOVEMENT_KEYS: &[&str] = &["up", "down", "left", "right", "home", "end", "pageup", "pagedown"];

/// Starts watching keystrokes. Tab, Escape, and Enter or Space outside a text field turn rings on;
/// an arrow does only when it moved focus, since arrows also edit text and scroll.
pub fn init(cx: &mut App) {
    cx.set_global(FocusAppearance::default());
    cx.observe_keystrokes(|event, window, cx| note_keystroke(&event.keystroke, window, cx)).detach();
}

fn note_keystroke(keystroke: &Keystroke, window: &mut Window, cx: &mut App) {
    let focus = window.focused(cx);
    let previous = std::mem::replace(&mut cx.global_mut::<FocusAppearance>().last_focus, focus.clone());
    let modifiers = &keystroke.modifiers;
    if modifiers.control || modifiers.alt || modifiers.platform {
        return;
    }
    let key = keystroke.key.as_str();
    let editing = window.context_stack().iter().any(|context| context.contains(crate::input::KEY_CONTEXT));
    let turns_on = if MOVEMENT_KEYS.contains(&key) {
        focus != previous
    } else {
        ACTIVATION_KEYS.contains(&key) && !(editing && matches!(key, "enter" | "space"))
    };
    if turns_on && !cx.global::<FocusAppearance>().keyboard {
        cx.global_mut::<FocusAppearance>().keyboard = true;
        sync(cx);
        cx.refresh_windows();
    }
}

/// Hands GPUI the ring a focused control draws, in the theme's colour while focus came from the keyboard.
pub fn sync(cx: &mut App) {
    use crate::theme::ActiveTheme;
    let ring = keyboard_focus_visible(cx).then(|| cx.theme().tokens.focus_ring.into());
    if cx.focus_ring() != ring {
        cx.set_focus_ring(ring);
    }
}

/// A press of the pointer anywhere turns rings off. The app's root calls this from a capture-phase
/// mouse-down listener.
pub fn note_pointer(cx: &mut App) {
    quiet(cx);
}

/// The window taking or losing the keyboard turns rings off, so a control focus returns to is
/// not drawn as a new keyboard destination.
pub fn note_activation(cx: &mut App) {
    quiet(cx);
}

fn quiet(cx: &mut App) {
    if cx.try_global::<FocusAppearance>().is_some_and(|appearance| appearance.keyboard) {
        cx.global_mut::<FocusAppearance>().keyboard = false;
        sync(cx);
        cx.refresh_windows();
    }
}

/// Whether a focused control should show its ring.
pub fn keyboard_focus_visible(cx: &App) -> bool {
    cx.try_global::<FocusAppearance>().is_some_and(|appearance| appearance.keyboard)
}
