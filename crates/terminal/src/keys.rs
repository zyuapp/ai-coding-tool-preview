//! Keystrokes to the bytes a shell expects, following xterm.js's `evaluateKeyboardEvent`.
//!
//! Printable text is not produced here: it arrives through the platform's text input, so dead
//! keys, Option-composed characters, and input methods work the way they do in any text field.

use gpui::Keystroke;

const ESC: &str = "\x1b";

/// What a keystroke means to the terminal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyResult {
    /// Bytes for the shell.
    Send(String),
    /// Scroll the viewport a page up (negative) or down.
    ScrollPage(i32),
    /// Not the terminal's: text input or the app's own bindings take it.
    Ignore,
}

#[derive(Clone, Copy)]
struct Event<'a> {
    code: u32,
    key: &'a str,
    shift: bool,
    alt: bool,
    ctrl: bool,
    meta: bool,
}

/// A browser `keyCode` for a GPUI key name, and whether the name already is the shifted character.
fn key_code(key: &str) -> Option<(u32, bool)> {
    let code = match key {
        "backspace" => 8,
        "tab" => 9,
        "enter" => 13,
        "escape" => 27,
        "space" => 32,
        "pageup" => 33,
        "pagedown" => 34,
        "end" => 35,
        "home" => 36,
        "left" => 37,
        "up" => 38,
        "right" => 39,
        "down" => 40,
        "insert" => 45,
        "delete" => 46,
        _ => {
            if let Some(number) = key.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
                return (1..=12).contains(&number).then_some((111 + number, false));
            }
            let mut chars = key.chars();
            let (Some(ch), None) = (chars.next(), chars.next()) else { return None };
            return Some(match ch {
                'a'..='z' => (ch as u32 - 32, false),
                'A'..='Z' => (ch as u32, true),
                '0'..='9' => (ch as u32, false),
                ';' => (186, false),
                '=' => (187, false),
                ',' => (188, false),
                '-' => (189, false),
                '.' => (190, false),
                '/' => (191, false),
                '`' => (192, false),
                '[' => (219, false),
                '\\' => (220, false),
                ']' => (221, false),
                '\'' => (222, false),
                ')' => (48, true),
                '!' => (49, true),
                '@' => (50, true),
                '#' => (51, true),
                '$' => (52, true),
                '%' => (53, true),
                '^' => (54, true),
                '&' => (55, true),
                '*' => (56, true),
                '(' => (57, true),
                ':' => (186, true),
                '+' => (187, true),
                '<' => (188, true),
                '_' => (189, true),
                '>' => (190, true),
                '?' => (191, true),
                '~' => (192, true),
                '{' => (219, true),
                '|' => (220, true),
                '}' => (221, true),
                '"' => (222, true),
                _ => (0, false),
            });
        }
    };
    Some((code, false))
}

/// Digits and punctuation, plain and shifted, as xterm's `KEYCODE_KEY_MAPPINGS`.
fn mapped_key(code: u32, shift: bool) -> Option<&'static str> {
    let (plain, shifted) = match code {
        48 => ("0", ")"),
        49 => ("1", "!"),
        50 => ("2", "@"),
        51 => ("3", "#"),
        52 => ("4", "$"),
        53 => ("5", "%"),
        54 => ("6", "^"),
        55 => ("7", "&"),
        56 => ("8", "*"),
        57 => ("9", "("),
        186 => (";", ":"),
        187 => ("=", "+"),
        188 => (",", "<"),
        189 => ("-", "_"),
        190 => (".", ">"),
        191 => ("/", "?"),
        192 => ("`", "~"),
        219 => ("[", "{"),
        220 => ("\\", "|"),
        221 => ("]", "}"),
        222 => ("'", "\""),
        _ => return None,
    };
    Some(if shift { shifted } else { plain })
}

/// Maps a keystroke the way xterm.js does with `macOptionIsMeta` off, its default.
pub fn evaluate(keystroke: &Keystroke, application_cursor: bool, is_mac: bool, option_is_meta: bool) -> KeyResult {
    let modifiers = keystroke.modifiers;
    let Some((code, shifted_name)) = key_code(&keystroke.key) else { return KeyResult::Ignore };
    let key = if keystroke.key == "space" { " " } else { keystroke.key.as_str() };
    let event = Event {
        code,
        key,
        shift: modifiers.shift || shifted_name,
        alt: modifiers.alt,
        ctrl: modifiers.control,
        meta: modifiers.platform,
    };
    let third_level_shift = is_mac && !option_is_meta && event.alt && !event.ctrl && !event.meta && event.code > 47;
    match evaluate_event(event, application_cursor, is_mac, option_is_meta) {
        // Option on a Mac types the character the layout gives, through text input.
        KeyResult::Send(_) if third_level_shift => KeyResult::Ignore,
        result => result,
    }
}

fn evaluate_event(ev: Event<'_>, application_cursor: bool, is_mac: bool, option_is_meta: bool) -> KeyResult {
    let modifiers = (ev.shift as u32) | (ev.alt as u32) << 1 | (ev.ctrl as u32) << 2 | (ev.meta as u32) << 3;
    let send = |text: String| KeyResult::Send(text);
    let cursor = |letter: char| {
        if modifiers != 0 {
            format!("{ESC}[1;{}{letter}", modifiers + 1)
        } else if application_cursor {
            format!("{ESC}O{letter}")
        } else {
            format!("{ESC}[{letter}")
        }
    };
    let tilde = |number: u32| {
        if modifiers != 0 {
            format!("{ESC}[{number};{}~", modifiers + 1)
        } else {
            format!("{ESC}[{number}~")
        }
    };
    let function = |letter: char| {
        if modifiers != 0 {
            format!("{ESC}[1;{}{letter}", modifiers + 1)
        } else {
            format!("{ESC}O{letter}")
        }
    };
    match ev.code {
        8 => {
            let key = if ev.ctrl { "\x08" } else { "\x7f" };
            send(if ev.alt { format!("{ESC}{key}") } else { key.to_owned() })
        }
        9 => send(if ev.shift { format!("{ESC}[Z") } else { "\t".to_owned() }),
        13 => send(if ev.alt { format!("{ESC}\r") } else { "\r".to_owned() }),
        27 => send(if ev.alt { format!("{ESC}{ESC}") } else { ESC.to_owned() }),
        37 | 38 | 39 | 40 if ev.meta => KeyResult::Ignore,
        37 => send(cursor('D')),
        39 => send(cursor('C')),
        38 => send(cursor('A')),
        40 => send(cursor('B')),
        45 if !ev.shift && !ev.ctrl => send(format!("{ESC}[2~")),
        45 => KeyResult::Ignore,
        46 => send(tilde(3)),
        36 => send(cursor('H')),
        35 => send(cursor('F')),
        33 if ev.shift => KeyResult::ScrollPage(-1),
        33 if ev.ctrl => send(format!("{ESC}[5;{}~", modifiers + 1)),
        33 => send(format!("{ESC}[5~")),
        34 if ev.shift => KeyResult::ScrollPage(1),
        34 if ev.ctrl => send(format!("{ESC}[6;{}~", modifiers + 1)),
        34 => send(format!("{ESC}[6~")),
        112 => send(function('P')),
        113 => send(function('Q')),
        114 => send(function('R')),
        115 => send(function('S')),
        116 => send(tilde(15)),
        117 => send(tilde(17)),
        118 => send(tilde(18)),
        119 => send(tilde(19)),
        120 => send(tilde(20)),
        121 => send(tilde(21)),
        122 => send(tilde(23)),
        123 => send(tilde(24)),
        _ => evaluate_default(ev, is_mac, option_is_meta),
    }
}

fn evaluate_default(ev: Event<'_>, is_mac: bool, option_is_meta: bool) -> KeyResult {
    let char_from = |code: u32| char::from_u32(code).map(String::from);
    let key = if ev.ctrl && !ev.shift && !ev.alt && !ev.meta {
        match ev.code {
            65..=90 => char_from(ev.code - 64),
            32 => Some("\0".to_owned()),
            51..=55 => char_from(ev.code - 51 + 27),
            56 => Some("\x7f".to_owned()),
            219 => Some(ESC.to_owned()),
            220 => Some("\x1c".to_owned()),
            221 => Some("\x1d".to_owned()),
            _ => None,
        }
    } else if (!is_mac || option_is_meta) && ev.alt && !ev.meta {
        if let Some(mapped) = mapped_key(ev.code, ev.shift) {
            Some(format!("{ESC}{mapped}"))
        } else if (65..=90).contains(&ev.code) {
            let code = if ev.ctrl { ev.code - 64 } else { ev.code + 32 };
            char_from(code).map(|text| {
                let text = if ev.shift { text.to_uppercase() } else { text };
                format!("{ESC}{text}")
            })
        } else if ev.code == 32 {
            Some(format!("{ESC}{}", if ev.ctrl { "\0" } else { " " }))
        } else {
            None
        }
    } else if is_mac && !ev.alt && !ev.ctrl && !ev.shift && ev.meta {
        // cmd-a is the app's select-all binding.
        None
    } else if !ev.ctrl && !ev.alt && !ev.meta && ev.code >= 48 && ev.key.chars().count() == 1 {
        // Printable: text input delivers it.
        None
    } else if ev.ctrl {
        match ev.key {
            "_" => Some("\x1f".to_owned()),
            "@" => Some("\0".to_owned()),
            _ => None,
        }
    } else {
        None
    };
    key.map_or(KeyResult::Ignore, KeyResult::Send)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(text: &str, application_cursor: bool, is_mac: bool) -> KeyResult {
        evaluate(&Keystroke::parse(text).unwrap(), application_cursor, is_mac, false)
    }

    fn sent(text: &str) -> KeyResult {
        KeyResult::Send(text.to_owned())
    }

    #[test]
    fn control_and_cursor_keys() {
        assert_eq!(press("ctrl-c", false, true), sent("\x03"));
        assert_eq!(press("ctrl-[", false, true), sent("\x1b"));
        assert_eq!(press("ctrl-@", false, true), sent("\0"));
        assert_eq!(press("up", true, true), sent("\x1bOA"));
        assert_eq!(press("up", false, true), sent("\x1b[A"));
        assert_eq!(press("shift-tab", false, true), sent("\x1b[Z"));
        assert_eq!(press("alt-left", false, true), sent("\x1b[1;3D"));
        assert_eq!(press("alt-backspace", false, true), sent("\x1b\x7f"));
        assert_eq!(press("f5", false, true), sent("\x1b[15~"));
        assert_eq!(press("shift-pageup", false, true), KeyResult::ScrollPage(-1));
    }

    #[test]
    fn option_composes_on_mac_and_is_meta_elsewhere() {
        assert_eq!(press("alt-f", false, true), KeyResult::Ignore);
        assert_eq!(press("alt-f", false, false), sent("\x1bf"));
        assert_eq!(press("alt-shift-f", false, false), sent("\x1bF"));
        assert_eq!(press("a", false, true), KeyResult::Ignore);
        assert_eq!(press("cmd-left", false, true), KeyResult::Ignore);
    }
}
