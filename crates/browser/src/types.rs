//! What the panel's pages report, and what a caller may ask of them.

use act_core::domain::find::FindResults;
use act_core::events::BrowserPageEvent;

/// What happened in the panel's pages, for the workspace to record.
#[derive(Clone, Debug, PartialEq)]
pub enum BrowserEvent {
    /// A page changed. The reducer keeps the tab record, so this is all it learns.
    Page(BrowserPageEvent),
    /// How a search of a page went. WebKit holds the text, so the matches are counted there.
    Find { tab_id: String, results: FindResults },
    /// The user clicked into a page, which now holds the keyboard.
    PageFocused { tab_id: String },
    /// A page the app draws itself has a new frame, or a new pointer over it.
    PageFrame { tab_id: String },
    /// A page the app draws itself opened a drop-down list, for the app to show at `anchor`.
    OptionMenu { tab_id: String, choices: Vec<OptionChoice>, anchor: PageRect },
    /// The page closed the list it had opened.
    OptionMenuClosed { tab_id: String },
}

/// A row of a page's drop-down list.
#[derive(Clone, Debug, PartialEq)]
pub struct OptionChoice {
    pub label: String,
    pub enabled: bool,
    pub selected: bool,
    /// A group's heading rather than a choice.
    pub heading: bool,
}

/// A rectangle in a page's device pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Input for a page the app draws itself. Positions are in device pixels from the page's top left.
#[derive(Clone, Debug)]
pub enum PageInput {
    Pointer { kind: PointerKind, x: f64, y: f64, button: u32, modifiers: gpui::Modifiers },
    Scroll { x: f64, y: f64, delta: gpui::ScrollDelta, modifiers: gpui::Modifiers },
    Leave,
    Key { down: bool, keystroke: gpui::Keystroke },
    Focus(bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerKind {
    Down,
    Up,
    Move,
}

/// Where the panel is, in the window's own points from its top left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Extra page state an agent may inspect without changing the page or workspace.
#[derive(Clone, Debug, PartialEq)]
pub enum Inspection {
    Console { since: Option<u64>, limit: Option<u64>, minimum_level: Option<String> },
    Network { since: Option<u64>, limit: Option<u64>, failures_only: bool },
    Wait { condition: String, value: Option<String>, timeout_ms: u64 },
}

pub fn page_event(tab_id: &str) -> BrowserPageEvent {
    BrowserPageEvent { navigation_request: None, tab_id: tab_id.to_string(), url: None, title: None, loading: None, can_go_back: None, can_go_forward: None, error: None }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|elapsed| elapsed.as_millis() as i64).unwrap_or(0)
}
