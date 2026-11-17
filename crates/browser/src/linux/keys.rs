//! Which keystrokes in a focused page belong to the app: its shortcuts and the panel's own keys,
//! named as the core names them (shortcuts.ts).

use std::cell::RefCell;
use std::rc::Rc;

use act_core::domain::shortcuts::{resolve_shortcuts, shortcut_for, Keystroke, ShortcutBinding, ShortcutOverrides, ShortcutSurface};
use gtk::gdk;

/// The panel's own keys (`panel::bind_keys`), which no setting moves.
const PANEL_KEYS: [&str; 4] = ["Mod+[", "Mod+]", "Mod+R", "Mod+L"];

/// The bindings a page hands to the app, shared by every tab.
#[derive(Clone)]
pub struct AppShortcuts(Rc<RefCell<(ShortcutOverrides, Vec<ShortcutBinding>)>>);

impl Default for AppShortcuts {
    fn default() -> Self {
        let overrides = ShortcutOverrides::default();
        let resolved = bindings(&overrides);
        AppShortcuts(Rc::new(RefCell::new((overrides, resolved))))
    }
}

fn bindings(overrides: &ShortcutOverrides) -> Vec<ShortcutBinding> {
    let mut bindings = resolve_shortcuts(overrides);
    bindings.extend(PANEL_KEYS.iter().map(|binding| ShortcutBinding { action: "browser.panel".into(), binding: (*binding).into(), surface: ShortcutSurface::Browser }));
    bindings
}

impl AppShortcuts {
    pub fn set(&self, overrides: &ShortcutOverrides) {
        if self.0.borrow().0 != *overrides {
            *self.0.borrow_mut() = (overrides.clone(), bindings(overrides));
        }
    }

    /// Whether a key pressed in a page is one of the app's.
    pub fn wants(&self, event: &gdk::EventKey) -> bool {
        let Some(stroke) = keystroke(event) else { return false };
        if !(stroke.r#mod || stroke.alt) && !stroke.key.starts_with('F') {
            return false;
        }
        shortcut_for(&self.0.borrow().1, &stroke, ShortcutSurface::Browser).is_some()
    }
}

impl AppShortcuts {
    /// Whether a key GPUI received for a page is one of the app's.
    pub fn wants_keystroke(&self, keystroke: &gpui::Keystroke) -> bool {
        let modifiers = &keystroke.modifiers;
        let key = match keystroke.key.as_str() {
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
            key if key.len() > 1 && key.starts_with('f') && key[1..].parse::<u8>().is_ok() => key.to_uppercase(),
            key if key.chars().count() == 1 => key.to_uppercase(),
            _ => return false,
        };
        if !(modifiers.control || modifiers.alt) && !key.starts_with('F') {
            return false;
        }
        let stroke = Keystroke { key, r#mod: modifiers.control, ctrl: false, alt: modifiers.alt, shift: modifiers.shift };
        shortcut_for(&self.0.borrow().1, &stroke, ShortcutSurface::Browser).is_some()
    }
}

/// The core's name for a key, by the key itself rather than what Shift made of it.
fn key_name(keyval: gdk::keys::Key) -> Option<String> {
    let name = keyval.to_lower().name()?.to_string();
    let named = match name.as_str() {
        "Left" => "ArrowLeft",
        "Right" => "ArrowRight",
        "Up" => "ArrowUp",
        "Down" => "ArrowDown",
        "Return" | "KP_Enter" => "Enter",
        "space" => "Space",
        "Tab" | "ISO_Left_Tab" => "Tab",
        "Escape" => "Escape",
        "BackSpace" => "Backspace",
        "Delete" => "Delete",
        "Home" => "Home",
        "End" => "End",
        "Page_Up" => "PageUp",
        "Page_Down" => "PageDown",
        "comma" | "less" => ",",
        "period" | "greater" => ".",
        "slash" | "question" => "/",
        "backslash" | "bar" => "\\",
        "bracketleft" | "braceleft" => "[",
        "bracketright" | "braceright" => "]",
        "semicolon" | "colon" => ";",
        "apostrophe" | "quotedbl" => "'",
        "minus" | "underscore" => "-",
        "equal" | "plus" => "=",
        "grave" | "asciitilde" => "`",
        "exclam" => "1",
        "at" => "2",
        "numbersign" => "3",
        "dollar" => "4",
        "percent" => "5",
        "asciicircum" => "6",
        "ampersand" => "7",
        "asterisk" => "8",
        "parenleft" => "9",
        "parenright" => "0",
        other if other.chars().count() == 1 => return Some(other.to_uppercase()),
        other if other.starts_with('F') && other[1..].parse::<u8>().is_ok() => other,
        _ => return None,
    };
    Some(named.to_string())
}

fn keystroke(event: &gdk::EventKey) -> Option<Keystroke> {
    let state = event.state();
    Some(Keystroke {
        key: key_name(event.keyval())?,
        r#mod: state.contains(gdk::ModifierType::CONTROL_MASK),
        ctrl: false,
        alt: state.contains(gdk::ModifierType::MOD1_MASK),
        shift: state.contains(gdk::ModifierType::SHIFT_MASK),
    })
}

#[cfg(test)]
mod tests {
    use super::AppShortcuts;

    #[test]
    fn app_keeps_its_shortcuts_and_the_page_gets_the_rest() {
        let shortcuts = AppShortcuts::default();
        let wants = |stroke: &'static str| shortcuts.wants_keystroke(&gpui::Keystroke::parse(stroke).unwrap());
        assert!(wants("ctrl-b"));
        assert!(wants("ctrl-l"));
        assert!(!wants("ctrl-a"));
        assert!(!wants("b"));
        assert!(!wants("shift-left"));
    }
}
