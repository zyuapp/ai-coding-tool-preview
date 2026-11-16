//! A keyboard held over the screen, as the USB HID usages a phone's keyboard takes (use-screen-input.ts).

use act_core::domain::platform::{SimulatorButton, SimulatorInput, SimulatorOrientation};

/// The HID usage of the first modifier key; every key after it is a modifier too.
pub const FIRST_MODIFIER: i64 = 224;
pub const CONTROL: i64 = 224;
pub const SHIFT: i64 = 225;
pub const ALT: i64 = 226;

/// The HID usage of a key as GPUI names it, for the keys a phone's keyboard has. A shifted symbol
/// names the key it is on, since Shift reaches the device as a key of its own.
pub fn hid_usage(key: &str) -> Option<i64> {
    let mut chars = key.chars();
    if let (Some(ch), None) = (chars.next(), chars.next()) {
        let ch = ch.to_ascii_lowercase();
        if ch.is_ascii_lowercase() {
            return Some(4 + (ch as i64 - 'a' as i64));
        }
        if ('1'..='9').contains(&ch) {
            return Some(30 + (ch as i64 - '1' as i64));
        }
        let usage = match ch {
            '0' | ')' => 39,
            '!' => 30,
            '@' => 31,
            '#' => 32,
            '$' => 33,
            '%' => 34,
            '^' => 35,
            '&' => 36,
            '*' => 37,
            '(' => 38,
            '-' | '_' => 45,
            '=' | '+' => 46,
            '[' | '{' => 47,
            ']' | '}' => 48,
            '\\' | '|' => 49,
            ';' | ':' => 51,
            '\'' | '"' => 52,
            '`' | '~' => 53,
            ',' | '<' => 54,
            '.' | '>' => 55,
            '/' | '?' => 56,
            ' ' => 44,
            _ => return None,
        };
        return Some(usage);
    }
    Some(match key {
        "enter" => 40,
        "escape" => 41,
        "backspace" => 42,
        "tab" => 43,
        "space" => 44,
        "home" => 74,
        "pageup" => 75,
        "delete" => 76,
        "end" => 77,
        "pagedown" => 78,
        "right" => 79,
        "left" => 80,
        "down" => 81,
        "up" => 82,
        _ => return None,
    })
}

/// The key a keystroke names: the character it types where that is a key the device has, so a
/// keystroke made up from text still sends its own key; else the key itself.
pub fn key_name(key: &str, key_char: Option<&str>, command: bool, control: bool) -> String {
    match key_char.filter(|text| !command && !control && text.chars().count() == 1 && hid_usage(text).is_some()) {
        Some(text) => text.to_string(),
        None => key.to_string(),
    }
}

/// What a key press over the screen does.
#[derive(Clone, Debug, PartialEq)]
pub enum KeyAction {
    /// The app keeps the key for its own shortcuts.
    Pass,
    /// Taken, with nothing to send (a repeat of a key already down).
    Swallow,
    Send(SimulatorInput),
    /// ⌘V: the Mac's clipboard goes to the device.
    Paste,
}

/// ⌘ shortcuts the device takes, beside ⌘C, ⌘X and ⌘V.
fn device_shortcut(key: &str) -> bool {
    matches!(key, "a" | "z" | "up" | "down" | "backspace")
}

/// A key going down over the screen. `held` is whether the device already has it down.
pub fn key_down(key: &str, command: bool, shift: bool, held: bool, orientation: SimulatorOrientation) -> KeyAction {
    let key = key.to_ascii_lowercase();
    if command {
        // Simulator.app's own keys for the hardware: ⌘⇧H goes home, ⌘← and ⌘→ turn the device.
        let hardware = match (shift, key.as_str()) {
            (true, "h") => Some(SimulatorInput::Button { button: SimulatorButton::Home }),
            (false, "left") => Some(SimulatorInput::Rotate { orientation: orientation.turned(-1) }),
            (false, "right") => Some(SimulatorInput::Rotate { orientation: orientation.turned(1) }),
            _ => None,
        };
        if let Some(hardware) = hardware {
            return if held { KeyAction::Swallow } else { KeyAction::Send(hardware) };
        }
        match key.as_str() {
            "c" => return KeyAction::Send(SimulatorInput::Shortcut { usage: super::host::KEY_C }),
            "x" => return KeyAction::Send(SimulatorInput::Shortcut { usage: super::host::KEY_X }),
            "v" => return KeyAction::Paste,
            _ => {}
        }
        if device_shortcut(&key) {
            return match hid_usage(&key) {
                Some(usage) if !held => KeyAction::Send(SimulatorInput::Shortcut { usage }),
                _ => KeyAction::Swallow,
            };
        }
        // Every other ⌘ key keeps meaning the app's own shortcuts.
        return KeyAction::Pass;
    }
    match hid_usage(&key) {
        None => KeyAction::Pass,
        Some(_) if held => KeyAction::Swallow,
        Some(usage) => KeyAction::Send(SimulatorInput::Key { usage, down: true }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_map_to_the_usages_a_us_keyboard_sends() {
        assert_eq!(hid_usage("a"), Some(4));
        assert_eq!(hid_usage("Z"), Some(29));
        assert_eq!(hid_usage("1"), Some(30));
        assert_eq!(hid_usage("!"), Some(30));
        assert_eq!(hid_usage("0"), Some(39));
        assert_eq!(hid_usage("enter"), Some(40));
        assert_eq!(hid_usage("up"), Some(82));
        assert_eq!(hid_usage("f1"), None);
    }

    #[test]
    fn command_keys_stay_the_apps_unless_the_device_takes_them() {
        let portrait = SimulatorOrientation::Portrait;
        assert_eq!(key_down("k", true, false, false, portrait), KeyAction::Pass);
        assert_eq!(key_down("a", true, false, false, portrait), KeyAction::Send(SimulatorInput::Shortcut { usage: 4 }));
        assert_eq!(key_down("h", true, true, false, portrait), KeyAction::Send(SimulatorInput::Button { button: SimulatorButton::Home }));
        assert_eq!(
            key_down("right", true, false, false, portrait),
            KeyAction::Send(SimulatorInput::Rotate { orientation: SimulatorOrientation::LandscapeRight })
        );
        assert_eq!(key_down("v", true, false, false, portrait), KeyAction::Paste);
        assert_eq!(key_down("a", false, false, true, portrait), KeyAction::Swallow);
    }
}
