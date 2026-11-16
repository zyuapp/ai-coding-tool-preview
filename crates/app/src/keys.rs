//! The keyboard: keystrokes the app answers, matched against the user's bindings
//! (src/domain/shortcuts.ts, keyboard-host.ts).

use act_core::domain::shortcuts::{resolve_shortcuts, shortcut_for, Keystroke, ShortcutSurface};
use act_core::state::WorkspaceState;
use act_core::AppCommand;
use gpui::App;

pub fn bind(_cx: &mut App) {}

/// The core's name for a GPUI keystroke.
pub fn keystroke(stroke: &gpui::Keystroke) -> Option<Keystroke> {
    let (key, shifted) = unshifted(&stroke.key);
    let key = match key {
        "left" => "ArrowLeft".to_string(),
        "right" => "ArrowRight".to_string(),
        "up" => "ArrowUp".to_string(),
        "down" => "ArrowDown".to_string(),
        "enter" => "Enter".to_string(),
        "space" => "Space".to_string(),
        "tab" => "Tab".to_string(),
        "escape" => "Escape".to_string(),
        "backspace" => "Backspace".to_string(),
        "delete" => "Delete".to_string(),
        "home" => "Home".to_string(),
        "end" => "End".to_string(),
        "pageup" => "PageUp".to_string(),
        "pagedown" => "PageDown".to_string(),
        key if key.len() == 1 => key.to_uppercase(),
        key if key.starts_with('f') && key[1..].parse::<u8>().is_ok() => key.to_uppercase(),
        _ => return None,
    };
    let modifiers = stroke.modifiers;
    let mac = cfg!(target_os = "macos");
    Some(Keystroke {
        key,
        r#mod: if mac { modifiers.platform } else { modifiers.control },
        ctrl: if mac { modifiers.control } else { false },
        alt: modifiers.alt,
        shift: modifiers.shift || shifted,
    })
}

/// GPUI names a shifted punctuation or digit key by the character it typed ("|", "!") and drops
/// shift; a binding names the physical key with shift held, as the browser's `code` does.
fn unshifted(key: &str) -> (&str, bool) {
    const SHIFTED: [(&str, &str); 21] = [
        ("|", "\\"), ("{", "["), ("}", "]"), ("<", ","), (">", "."), ("?", "/"), ("_", "-"), ("+", "="), (":", ";"), ("\"", "'"), ("~", "`"),
        ("!", "1"), ("@", "2"), ("#", "3"), ("$", "4"), ("%", "5"), ("^", "6"), ("&", "7"), ("*", "8"), ("(", "9"), (")", "0"),
    ];
    SHIFTED.iter().find(|(typed, _)| *typed == key).map_or((key, false), |(_, base)| (*base, true))
}

/// The command a keystroke stands for in the window, if the user bound one.
pub fn shortcut_command(state: &WorkspaceState, stroke: &gpui::Keystroke) -> Option<AppCommand> {
    let stroke = keystroke(stroke)?;
    if !(stroke.r#mod || stroke.ctrl || stroke.alt) && !stroke.key.starts_with('F') {
        return None;
    }
    let bindings = resolve_shortcuts(&state.shortcuts);
    let binding = shortcut_for(&bindings, &stroke, ShortcutSurface::Any)?;
    Some(AppCommand::Shortcut { action: binding.action.clone(), surface: ShortcutSurface::Any })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shifted_punctuation_names_the_physical_key() {
        let stroke = keystroke(&gpui::Keystroke::parse("secondary-|").unwrap()).unwrap();
        assert_eq!((stroke.key.as_str(), stroke.r#mod, stroke.shift), ("\\", true, true));
        let digit = keystroke(&gpui::Keystroke::parse("secondary-!").unwrap()).unwrap();
        assert_eq!((digit.key.as_str(), digit.shift), ("1", true));
    }
}
