//! The keyboard, as data: actions, the keystrokes bound to them, and how a keystroke is named.

use serde::{Deserialize, Serialize};

use crate::util::Map;

/// Where a binding is claimed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShortcutSurface {
    Any,
    Browser,
    Desktop,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShortcutAction {
    pub id: &'static str,
    pub group: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub surface: ShortcutSurface,
    pub default_binding: &'static str,
}

/// One keystroke, named so the same value survives a keyboard layout and a platform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keystroke {
    pub key: String,
    /// The platform's own command key.
    pub r#mod: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// A keystroke as the window reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyInput {
    pub key: String,
    pub code: String,
    pub meta: bool,
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShortcutBinding {
    pub action: String,
    pub binding: String,
    pub surface: ShortcutSurface,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShortcutSetting {
    pub action: ShortcutAction,
    pub binding: Option<String>,
    pub changed: bool,
}

/// The user's own bindings, by action. A `None` is an action they took the keystroke away from.
pub type ShortcutOverrides = Map<Option<String>>;

const PUNCTUATION: [(&str, &str); 11] = [
    ("Comma", ","),
    ("Period", "."),
    ("Slash", "/"),
    ("Backslash", "\\"),
    ("BracketLeft", "["),
    ("BracketRight", "]"),
    ("Minus", "-"),
    ("Equal", "="),
    ("Semicolon", ";"),
    ("Quote", "'"),
    ("Backquote", "`"),
];

const NAMED_KEYS: [&str; 14] = ["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Enter", "Space", "Tab", "Escape", "Backspace", "Delete", "Home", "End", "PageUp", "PageDown"];

fn is_function_key(key: &str) -> bool {
    key.strip_prefix('F').and_then(|number| number.parse::<u8>().ok()).is_some_and(|number| (1..=12).contains(&number) && !key[1..].starts_with('0'))
}

fn is_bindable_key(key: &str) -> bool {
    let single = key.len() == 1 && key.chars().all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit());
    single || PUNCTUATION.iter().any(|(_, symbol)| *symbol == key) || NAMED_KEYS.contains(&key) || is_function_key(key)
}

/// The key a binding names, taken from the physical key rather than the character it produced.
pub fn shortcut_key(code: &str, key: &str) -> Option<String> {
    if let Some(letter) = code.strip_prefix("Key") {
        if letter.len() == 1 && letter.chars().all(|ch| ch.is_ascii_uppercase()) {
            return Some(letter.into());
        }
    }
    if let Some(digit) = code.strip_prefix("Digit") {
        if digit.len() == 1 && digit.chars().all(|ch| ch.is_ascii_digit()) {
            return Some(digit.into());
        }
    }
    if let Some((_, symbol)) = PUNCTUATION.iter().find(|(name, _)| *name == code) {
        return Some((*symbol).into());
    }
    if NAMED_KEYS.contains(&code) || is_function_key(code) {
        return Some(code.into());
    }
    let produced = if key.chars().count() == 1 { key.to_uppercase() } else { key.to_string() };
    is_bindable_key(&produced).then_some(produced)
}

pub fn keystroke_of(input: &KeyInput, mac: bool) -> Option<Keystroke> {
    let key = shortcut_key(&input.code, &input.key)?;
    Some(Keystroke {
        key,
        r#mod: if mac { input.meta } else { input.control },
        ctrl: if mac { input.control } else { input.meta },
        alt: input.alt,
        shift: input.shift,
    })
}

pub fn format_shortcut(stroke: &Keystroke) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if stroke.r#mod {
        parts.push("Mod");
    }
    if stroke.ctrl {
        parts.push("Ctrl");
    }
    if stroke.alt {
        parts.push("Alt");
    }
    if stroke.shift {
        parts.push("Shift");
    }
    parts.push(&stroke.key);
    parts.join("+")
}

/// Anything unreadable is no binding at all.
pub fn parse_shortcut(text: &str) -> Option<Keystroke> {
    if text.is_empty() {
        return None;
    }
    let mut parts: Vec<&str> = text.split('+').collect();
    let key = parts.pop().unwrap_or("");
    if !is_bindable_key(key) {
        return None;
    }
    let mut stroke = Keystroke { key: key.into(), r#mod: false, ctrl: false, alt: false, shift: false };
    for part in parts {
        match part {
            "Mod" if !stroke.r#mod => stroke.r#mod = true,
            "Ctrl" if !stroke.ctrl => stroke.ctrl = true,
            "Alt" if !stroke.alt => stroke.alt = true,
            "Shift" if !stroke.shift => stroke.shift = true,
            _ => return None,
        }
    }
    Some(stroke)
}

fn key_symbol(key: &str) -> Option<&'static str> {
    Some(match key {
        "ArrowLeft" => "←",
        "ArrowRight" => "→",
        "ArrowUp" => "↑",
        "ArrowDown" => "↓",
        "Enter" => "↩",
        "Escape" => "Esc",
        "Backspace" => "⌫",
        "Delete" => "⌦",
        "Tab" => "⇥",
        "PageUp" => "⇞",
        "PageDown" => "⇟",
        "Home" => "↖",
        "End" => "↘",
        _ => return None,
    })
}

/// The keys of a binding, one to a token, so the screen can draw each as its own cap.
pub fn shortcut_keys(binding: &str, mac: bool) -> Vec<String> {
    let Some(stroke) = parse_shortcut(binding) else { return Vec::new() };
    let mut keys: Vec<String> = Vec::new();
    if mac {
        for (held, symbol) in [(stroke.ctrl, "⌃"), (stroke.alt, "⌥"), (stroke.shift, "⇧"), (stroke.r#mod, "⌘")] {
            if held {
                keys.push(symbol.into());
            }
        }
        keys.push(key_symbol(&stroke.key).map(String::from).unwrap_or(stroke.key.clone()));
    } else {
        for (held, name) in [(stroke.r#mod, "Ctrl"), (stroke.ctrl, "Meta"), (stroke.alt, "Alt"), (stroke.shift, "Shift")] {
            if held {
                keys.push(name.into());
            }
        }
        keys.push(stroke.key.clone());
    }
    keys
}

pub fn display_shortcut(binding: &str, mac: bool) -> String {
    shortcut_keys(binding, mac).join(if mac { "" } else { "+" })
}

/// The keystrokes the app owns outright.
pub fn fixed_shortcuts() -> Vec<ShortcutBinding> {
    let mut fixed: Vec<ShortcutBinding> = [
        ("thread.new", "Mod+N", ShortcutSurface::Any),
        ("thread.new-pick-project", "Mod+Shift+N", ShortcutSurface::Any),
        ("composer.focus", "Mod+L", ShortcutSurface::Any),
        ("find.open", "Mod+F", ShortcutSurface::Any),
        ("find.next", "Mod+G", ShortcutSurface::Any),
        ("find.previous", "Mod+Shift+G", ShortcutSurface::Any),
        ("nav.back", "Mod+[", ShortcutSurface::Any),
        ("nav.forward", "Mod+]", ShortcutSurface::Any),
        ("page.reload", "Mod+R", ShortcutSurface::Browser),
        ("tab.new", "Mod+T", ShortcutSurface::Any),
        ("terminal.focus", "Mod+J", ShortcutSurface::Any),
        ("tab.close", "Mod+W", ShortcutSurface::Any),
        ("dock.toggle", "Mod+\\", ShortcutSurface::Any),
        ("dock.toggle", "Mod+Alt+B", ShortcutSurface::Any),
        ("dock.expand", "Mod+Shift+\\", ShortcutSurface::Any),
        ("sidebar.toggle", "Mod+B", ShortcutSurface::Any),
        ("settings.toggle", "Mod+,", ShortcutSurface::Any),
    ]
    .into_iter()
    .map(|(action, binding, surface)| ShortcutBinding { action: action.into(), binding: binding.into(), surface })
    .collect();
    for index in 1..=9 {
        fixed.push(ShortcutBinding { action: format!("slot-{index}"), binding: format!("Mod+{index}"), surface: ShortcutSurface::Any });
    }
    fixed
}

pub const SHORTCUT_MODIFIER_REQUIRED: &str = "A shortcut needs ⌘, ⌥, or ⌃ so it cannot swallow what you type.";
pub const SHORTCUT_RESERVED: &str = "That keystroke belongs to the desktop.";
pub const SHORTCUT_TAKEN: &str = "That keystroke is one the app already answers.";

/// Why a keystroke cannot be bound, or None when it can.
pub fn shortcut_problem(binding: &str) -> Option<&'static str> {
    let Some(stroke) = parse_shortcut(binding) else { return Some(SHORTCUT_RESERVED) };
    if !stroke.r#mod && !stroke.ctrl && !stroke.alt {
        return Some(SHORTCUT_MODIFIER_REQUIRED);
    }
    if stroke.key == "Tab" || (stroke.key == "Q" && stroke.r#mod && !stroke.ctrl && !stroke.alt && !stroke.shift) {
        return Some(SHORTCUT_RESERVED);
    }
    let formatted = format_shortcut(&stroke);
    if fixed_shortcuts().iter().any(|fixed| fixed.binding == formatted) {
        return Some(SHORTCUT_TAKEN);
    }
    None
}

/// The actions worth a row in settings.
pub fn shortcut_actions() -> &'static [ShortcutAction] {
    const ACTIONS: [ShortcutAction; 9] = [
        ShortcutAction { id: "effort.increase", group: "Threads", label: "Increase effort", description: "Use the next higher effort supported by this model", surface: ShortcutSurface::Any, default_binding: "Mod+Shift+ArrowUp" },
        ShortcutAction { id: "effort.decrease", group: "Threads", label: "Decrease effort", description: "Use the next lower effort supported by this model", surface: ShortcutSurface::Any, default_binding: "Mod+Shift+ArrowDown" },
        ShortcutAction { id: "project.start", group: "Threads", label: "Start in a folder", description: "Type a folder's path and start a thread there", surface: ShortcutSurface::Any, default_binding: "Mod+O" },
        ShortcutAction { id: "thread.jump", group: "Threads", label: "Jump to a thread or a setting", description: "Search your threads and every setting, and open one", surface: ShortcutSurface::Any, default_binding: "Mod+K" },
        ShortcutAction { id: "run.allow", group: "Threads", label: "Allow", description: "Answer the approval this thread is waiting on", surface: ShortcutSurface::Any, default_binding: "Mod+Shift+A" },
        ShortcutAction { id: "run.deny", group: "Threads", label: "Deny", description: "Refuse the approval this thread is waiting on", surface: ShortcutSurface::Any, default_binding: "Mod+Shift+D" },
        ShortcutAction { id: "dictation.hold", group: "Threads", label: "Dictate", description: "Hold to talk, or tap for hands-free", surface: ShortcutSurface::Any, default_binding: "Alt+D" },
        ShortcutAction { id: "zen.toggle", group: "Threads", label: "Zen mode", description: "Hide everything but this thread and its composer", surface: ShortcutSurface::Any, default_binding: "Mod+Shift+F" },
        ShortcutAction { id: "window.capture", group: "Capture", label: "Grab the window you are in", description: "Attach the frontmost app's window to this thread, from anywhere on the desktop", surface: ShortcutSurface::Desktop, default_binding: "Alt+Shift+S" },
    ];
    &ACTIONS
}

pub fn shortcut_action(id: &str) -> Option<&'static ShortcutAction> {
    shortcut_actions().iter().find(|action| action.id == id)
}

/// Which position `slot-*` names, counting from zero.
pub fn slot_shortcut_index(action_id: &str) -> Option<usize> {
    let digit = action_id.strip_prefix("slot-")?;
    if digit.len() != 1 {
        return None;
    }
    let number: usize = digit.parse().ok()?;
    (1..=9).contains(&number).then(|| number - 1)
}

pub const SLOT_COUNT: usize = 9;

/// What each action is bound to now, for the settings list.
pub fn shortcut_settings(overrides: &ShortcutOverrides) -> Vec<ShortcutSetting> {
    shortcut_actions()
        .iter()
        .map(|action| {
            let binding = match overrides.get(action.id) {
                Some(override_binding) => override_binding.clone(),
                None => Some(action.default_binding.to_string()),
            };
            let changed = binding.as_deref() != Some(action.default_binding);
            ShortcutSetting { action: action.clone(), binding, changed }
        })
        .collect()
}

/// The bindings a keystroke is matched against. A keystroke belongs to one action.
pub fn resolve_shortcuts(overrides: &ShortcutOverrides) -> Vec<ShortcutBinding> {
    let mut bindings = fixed_shortcuts();
    let mut taken: std::collections::HashSet<String> = bindings.iter().map(|fixed| fixed.binding.clone()).collect();
    for setting in shortcut_settings(overrides) {
        let Some(binding) = setting.binding else { continue };
        if binding.is_empty() || taken.contains(&binding) || shortcut_problem(&binding).is_some() {
            continue;
        }
        taken.insert(binding.clone());
        bindings.push(ShortcutBinding { action: setting.action.id.into(), binding, surface: setting.action.surface });
    }
    bindings
}

/// A binding written the way the desktop registers one.
pub fn desktop_accelerator(binding: &str) -> Option<String> {
    let stroke = parse_shortcut(binding)?;
    if shortcut_problem(binding).is_some() {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    if stroke.r#mod {
        parts.push("CommandOrControl".into());
    }
    if stroke.ctrl {
        parts.push("Control".into());
    }
    if stroke.alt {
        parts.push("Alt".into());
    }
    if stroke.shift {
        parts.push("Shift".into());
    }
    let key = match stroke.key.as_str() {
        "ArrowLeft" => "Left",
        "ArrowRight" => "Right",
        "ArrowUp" => "Up",
        "ArrowDown" => "Down",
        "Enter" => "Return",
        "Escape" => "Esc",
        other => other,
    };
    parts.push(key.into());
    Some(parts.join("+"))
}

/// Which action a keystroke asks for on the surface it was pressed.
pub fn shortcut_for<'a>(bindings: &'a [ShortcutBinding], stroke: &Keystroke, surface: ShortcutSurface) -> Option<&'a ShortcutBinding> {
    let binding = format_shortcut(stroke);
    bindings.iter().find(|candidate| candidate.binding == binding && (candidate.surface == ShortcutSurface::Any || candidate.surface == surface))
}

/// Only the bindings that differ from the default are worth keeping.
pub fn shortcut_overrides(overrides: &ShortcutOverrides) -> ShortcutOverrides {
    let mut kept = ShortcutOverrides::new();
    for (id, binding) in overrides {
        let Some(action) = shortcut_action(id) else { continue };
        if binding.as_deref() == Some(action.default_binding) {
            continue;
        }
        kept.insert(id.clone(), binding.clone());
    }
    kept
}

/// The overrides after `action` takes `binding`; whoever held it loses it.
pub fn with_shortcut(overrides: &ShortcutOverrides, action: &str, binding: Option<&str>) -> ShortcutOverrides {
    let mut next = overrides.clone();
    if let Some(binding) = binding.filter(|binding| !binding.is_empty()) {
        for setting in shortcut_settings(overrides) {
            if setting.action.id != action && setting.binding.as_deref() == Some(binding) {
                next.insert(setting.action.id.into(), None);
            }
        }
    }
    next.insert(action.into(), binding.map(String::from));
    shortcut_overrides(&next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_steals_from_previous_holder() {
        let next = with_shortcut(&ShortcutOverrides::new(), "run.allow", Some("Mod+K"));
        assert_eq!(next.get("thread.jump"), Some(&None));
        assert_eq!(next.get("run.allow"), Some(&Some("Mod+K".to_string())));
    }

    #[test]
    fn problems_name_reserved_and_taken_keys() {
        assert_eq!(shortcut_problem("K"), Some(SHORTCUT_MODIFIER_REQUIRED));
        assert_eq!(shortcut_problem("Mod+Q"), Some(SHORTCUT_RESERVED));
        assert_eq!(shortcut_problem("Mod+N"), Some(SHORTCUT_TAKEN));
        assert_eq!(shortcut_problem("Mod+Shift+K"), None);
    }

    #[test]
    fn mac_keys_draw_as_symbols_in_mac_order() {
        assert_eq!(shortcut_keys("Mod+Shift+ArrowUp", true), vec!["⇧", "⌘", "↑"]);
        assert_eq!(display_shortcut("Mod+Shift+ArrowUp", false), "Ctrl+Shift+ArrowUp");
    }
}
