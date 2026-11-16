//! The captured window's accessibility text, read from the computer-use driver's window state
//! (screenshot-context-snapshot.ts). A bounded read: no screenshot, input action, or focus change.

use std::collections::HashMap;
use std::sync::LazyLock;

use act_core::domain::conversation::{AccessibilitySnapshot, ScreenshotPlatform};
use regex::Regex;
use serde_json::{json, Map, Value};

use super::tree::{is_document_role, is_protected_role, screenshot_tree};

pub const SCREENSHOT_CONTEXT_MAX_ELEMENTS: usize = 2_000;
pub const SCREENSHOT_CONTEXT_MAX_DEPTH: usize = 50;
pub const MAX_SCREENSHOT_CONTEXT_TEXT: usize = 12_000;
pub const MAX_SCREENSHOT_CONTEXT_NAME: usize = 512;
const MAX_RESULT_LENGTH: usize = 1_000_000;
const MAX_LINE_LENGTH: usize = 1_200;

/// The window a screenshot was taken of.
#[derive(Clone, Debug, PartialEq)]
pub struct ScreenshotTarget {
    pub platform: ScreenshotPlatform,
    pub pid: i64,
    pub window_id: i64,
    pub app: String,
    pub title: String,
}

/// What a read of the driver answers: whether it was an error, and its structured JSON.
pub struct ToolAnswer {
    pub is_error: bool,
    pub structured_json: Option<String>,
}

/// The driver's `get_window_state`, however it is reached.
pub trait ContextDriver {
    fn call_tool(&mut self, name: &str, arguments: &Value) -> Result<ToolAnswer, String>;
}

fn unavailable(reason: &str) -> AccessibilitySnapshot {
    AccessibilitySnapshot::Unavailable { reason: reason.into() }
}

fn js_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Hyprland's stableId names a grim toplevel; the driver uses the full compositor address.
pub fn hyprland_accessibility_window_id(address: Option<&str>) -> Option<i64> {
    let address = address?;
    let digits = address.strip_prefix("0x").or_else(|| address.strip_prefix("0X"))?;
    if digits.is_empty() || !digits.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    let id = u64::from_str_radix(digits, 16).ok()?;
    (id > 0 && id < (1u64 << 53)).then_some(id as i64)
}

static SPARSE_MARKERS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*- (?:AXStaticText|AXTextArea|AXHeading|AXOutline|AXTable|label|static|paragraph|heading|text|table)(?:\s|$)").unwrap());

/// A read, retried once when a newly enabled Chromium tree shows only its title bar. `partial` is
/// handed the first usable read in case the follow-up stalls.
pub fn snapshot_window_accessibility(driver: &mut dyn ContextDriver, target: &ScreenshotTarget, partial: &mut dyn FnMut(&AccessibilitySnapshot)) -> AccessibilitySnapshot {
    let input = json!({
        "pid": target.pid,
        "window_id": target.window_id,
        "include_screenshot": false,
        "max_elements": SCREENSHOT_CONTEXT_MAX_ELEMENTS,
        "max_depth": SCREENSHOT_CONTEXT_MAX_DEPTH,
    });
    let read = |driver: &mut dyn ContextDriver| -> AccessibilitySnapshot {
        let answer = match driver.call_tool("get_window_state", &input) {
            Ok(answer) => answer,
            Err(_) => return unavailable("failed"),
        };
        let Some(json) = answer.structured_json.filter(|json| !answer.is_error && json.len() <= MAX_RESULT_LENGTH) else { return unavailable("failed") };
        match serde_json::from_str::<Value>(&json) {
            Ok(snapshot) => accessibility_from_snapshot(&snapshot, target),
            Err(_) => unavailable("failed"),
        }
    };
    let first = read(driver);
    let sparse = matches!(&first, AccessibilitySnapshot::Captured { text, .. } if !SPARSE_MARKERS.is_match(text));
    let retry_unavailable = matches!(&first, AccessibilitySnapshot::Unavailable { reason } if reason == "window" || reason == "empty");
    if !sparse && !retry_unavailable {
        return first;
    }
    if matches!(first, AccessibilitySnapshot::Captured { .. }) {
        partial(&first);
    }
    std::thread::sleep(std::time::Duration::from_millis(100));
    let second = read(driver);
    if matches!(second, AccessibilitySnapshot::Captured { .. }) {
        second
    } else {
        first
    }
}

static ROW_INDEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\s*-?\s*)\[(?:element_index )?\d+\]\s*").unwrap());
static ROW_TAIL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s*\[(?:id=|help=|actions=).*\]\s*$").unwrap());
static CONTROL: LazyLock<Regex> = LazyLock::new(|| Regex::new("[\u{0000}-\u{0008}\u{000b}\u{000c}\u{000e}-\u{001f}\u{007f}]").unwrap());
static CHECKABLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)check.?box|radio.?button").unwrap());
static TEXT_ROLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:AXStaticText|AXHeading|label|static|heading|paragraph)$").unwrap());
static LINE_BREAK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\r?\n\s*").unwrap());
static LINUX_ROOT_PREFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^- (?:\[\d+\] )?(?:frame|window|dialog|alert|file chooser)(?: =)? ").unwrap());

/// A value as JavaScript's `String()` writes it.
fn js_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "null".into(),
        Value::Number(number) => js_number(number.as_f64().unwrap_or(f64::NAN)),
        Value::Bool(flag) => flag.to_string(),
        Value::Array(_) | Value::Object(_) => String::new(),
    }
}

fn js_number(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e21 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

fn record(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

fn js_slice(text: &str, length: usize) -> String {
    let units: Vec<u16> = text.encode_utf16().take(length).collect();
    String::from_utf16_lossy(&units)
}

/// The structured element list omits passive labels, so the Markdown tree is kept for those and its
/// controls are supplemented with their typed states. Actionable snapshot tokens are never sent.
pub fn accessibility_from_snapshot(snapshot: &Value, target: &ScreenshotTarget) -> AccessibilitySnapshot {
    let Some(snapshot) = record(snapshot) else { return unavailable("window") };
    let title_differs = snapshot.get("window_title").and_then(Value::as_str).is_some_and(|title| title != target.title);
    if snapshot.get("pid").and_then(Value::as_i64) != Some(target.pid) || snapshot.get("window_id").and_then(Value::as_i64) != Some(target.window_id) || title_differs {
        return unavailable("window");
    }
    // The driver marks ambiguous matches, such as Hyprland AT-SPI ones, as application-scoped.
    if snapshot.get("degraded") == Some(&Value::Bool(true)) {
        return unavailable("window");
    }
    let Some(markdown) = snapshot.get("tree_markdown").and_then(Value::as_str).filter(|markdown| !markdown.trim().is_empty()) else { return unavailable("empty") };
    let (all_lines, tree_truncated) = screenshot_tree(markdown, target.platform);
    // macOS also supplies the application's menu bar, so exactly the captured window's subtree is kept.
    let is_root = |role: &str| match target.platform {
        ScreenshotPlatform::Macos => role == "AXWindow",
        _ => matches!(role, "frame" | "window" | "dialog" | "alert" | "file chooser"),
    };
    let candidates: Vec<(usize, usize)> = all_lines.iter().enumerate().filter(|(_, line)| is_root(&line.role)).map(|(index, line)| (index, line.depth)).collect();
    let Some(root_depth) = candidates.iter().map(|(_, depth)| *depth).min() else { return unavailable("window") };
    let roots: Vec<&(usize, usize)> = candidates.iter().filter(|(_, depth)| *depth == root_depth).collect();
    if roots.len() != 1 {
        return unavailable("window");
    }
    let (root_index, root_depth) = *roots[0];
    let end = all_lines.iter().enumerate().position(|(index, line)| index > root_index && line.depth <= root_depth);
    let lines = &all_lines[root_index..end.unwrap_or(all_lines.len())];
    if target.platform != ScreenshotPlatform::Macos {
        // An application-wide tree can come back when geometry did not resolve the top-level, so only
        // one named window root belonging to the captured window is accepted.
        let first = lines[0].text.trim_start();
        let Some(prefix) = LINUX_ROOT_PREFIX.find(first) else { return unavailable("window") };
        let title = &first[prefix.end()..];
        let expected = format!("\"{}\"", target.title);
        if target.title.contains(['\r', '\n']) || (title != expected && !title.starts_with(&format!("{expected} [actions="))) {
            return unavailable("window");
        }
    }
    let mut elements: HashMap<i64, &Map<String, Value>> = HashMap::new();
    if let Some(list) = snapshot.get("elements").and_then(Value::as_array) {
        for element in list.iter().take(SCREENSHOT_CONTEXT_MAX_ELEMENTS).filter_map(record) {
            if let Some(index) = element.get("element_index").and_then(Value::as_f64) {
                elements.insert(index as i64, element);
            }
        }
    }
    let mut formatted: Vec<(String, bool)> = Vec::new();
    let mut protected_depth: Option<usize> = None;
    let mut document_depth: Option<usize> = None;
    // The driver counts actionable elements apart from its cap on visited nodes, so its walk warning decides.
    let mut truncated = tree_truncated;
    for row in lines.iter().take(SCREENSHOT_CONTEXT_MAX_ELEMENTS * 2) {
        if document_depth.is_some_and(|depth| row.depth <= depth) {
            document_depth = None;
        }
        if document_depth.is_none() && is_document_role(&row.role) {
            document_depth = Some(row.depth);
        }
        if protected_depth.is_some_and(|depth| row.depth > depth) {
            continue;
        }
        protected_depth = None;
        if is_protected_role(&row.role) {
            protected_depth = Some(row.depth);
            continue;
        }
        let element = row.index.and_then(|index| elements.get(&(index as i64)).copied());
        let mut states: Vec<String> = Vec::new();
        if let Some(element) = element {
            for key in ["enabled", "selected", "checked"] {
                if let Some(Value::Bool(flag)) = element.get(key) {
                    states.push(format!("{key}={flag}"));
                }
            }
            let role = element.get("role").map(js_string).unwrap_or_else(|| "undefined".into());
            if !matches!(element.get("checked"), Some(Value::Bool(_))) && CHECKABLE.is_match(&role) {
                let value = element.get("value").map(js_string).unwrap_or_else(|| "undefined".into());
                if ["1", "true", "on"].contains(&value.as_str()) {
                    states.push("checked=true".into());
                }
                if ["0", "false", "off"].contains(&value.as_str()) {
                    states.push("checked=false".into());
                }
            }
            if let Some(frame) = element.get("frame").and_then(record).filter(|_| !TEXT_ROLE.is_match(&row.role)) {
                let parts: Vec<Option<f64>> = ["x", "y", "w", "h"].iter().map(|key| frame.get(*key).and_then(Value::as_f64).filter(|value| value.is_finite())).collect();
                if let [Some(x), Some(y), Some(w), Some(h)] = parts[..] {
                    if w > 0.0 && h > 0.0 {
                        states.push(format!("desktop bounds=[{},{},{},{}]", js_number(x), js_number(y), js_number(w), js_number(h)));
                    }
                }
            }
            // Numeric values, such as a checkbox's or a slider's, may only be on the structured side.
            if let Some(value) = element.get("value").and_then(Value::as_str).filter(|value| !value.is_empty()) {
                let rendered = LINE_BREAK.replace_all(value.trim(), "\\n").into_owned();
                if !row.text.contains(&format!("= \"{rendered}\"")) && !row.text.contains(&format!("value=\"{rendered}\"")) {
                    states.push(format!("value={}", serde_json::to_string(&js_slice(value, MAX_LINE_LENGTH)).unwrap_or_default()));
                }
            }
        }
        let line = ROW_INDEX.replace(&row.text, "$1").into_owned();
        let line = ROW_TAIL.replace(&line, "").into_owned();
        let mut line = CONTROL.replace_all(&line, "").into_owned();
        if !states.is_empty() {
            line = format!("{line} ({})", states.join(", "));
        }
        if js_len(&line) > MAX_LINE_LENGTH {
            line = format!("{}…", js_slice(&line, MAX_LINE_LENGTH - 1));
            truncated = true;
        }
        let in_web = element.and_then(|element| element.get("in_web_content")) == Some(&Value::Bool(true));
        formatted.push((line, document_depth.is_some() || in_web));
    }
    if lines.len() > SCREENSHOT_CONTEXT_MAX_ELEMENTS * 2 {
        truncated = true;
    }
    let mut fit = |rows: Vec<&String>, budget: usize| -> (String, usize) {
        let mut kept: Vec<&str> = Vec::new();
        let mut used = 0;
        for row in rows {
            let length = js_len(row);
            if used + length + 1 > budget {
                truncated = true;
                continue;
            }
            kept.push(row);
            used += length + 1;
        }
        (kept.join("\n"), used)
    };
    let documents: Vec<&String> = formatted.iter().filter(|(_, document)| *document).map(|(text, _)| text).collect();
    let has_documents = !documents.is_empty();
    // Room is kept for a page's own content even when bookmarks, tabs or app controls run long.
    let chrome_rows: Vec<&String> = formatted.iter().filter(|(_, document)| !has_documents || !*document).map(|(text, _)| text).collect();
    let (chrome, used) = fit(chrome_rows, if has_documents { 2_000 } else { MAX_SCREENSHOT_CONTEXT_TEXT });
    let (page, _) = fit(documents, MAX_SCREENSHOT_CONTEXT_TEXT.saturating_sub(used));
    let text = [chrome, page].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join("\n").trim().to_string();
    if text.is_empty() {
        unavailable("empty")
    } else {
        AccessibilitySnapshot::Captured { text, truncated }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> ScreenshotTarget {
        ScreenshotTarget { platform: ScreenshotPlatform::Macos, pid: 7, window_id: 9, app: "Safari".into(), title: "Docs".into() }
    }

    #[test]
    fn keeps_the_window_subtree_without_tokens_and_hides_passwords() {
        let snapshot = json!({
            "pid": 7, "window_id": 9, "window_title": "Docs",
            "tree_markdown": "- AXMenuBar\n  - AXMenuItem \"File\"\n- [1] AXWindow \"Docs\" [actions=[raise]]\n  - [2] AXCheckBox \"Remember\" [id=r]\n  - [3] AXSecureTextField \"Password\"\n    - AXStaticText = \"hunter2\"\n  - AXStaticText = \"Welcome\"",
            "elements": [{ "element_index": 2, "role": "AXCheckBox", "value": 1, "enabled": true, "frame": { "x": 10, "y": 20, "w": 30, "h": 4.5 } }],
        });
        let AccessibilitySnapshot::Captured { text, truncated } = accessibility_from_snapshot(&snapshot, &target()) else { panic!("not captured") };
        assert!(!truncated);
        assert_eq!(
            text,
            "- AXWindow \"Docs\"\n  - AXCheckBox \"Remember\" (enabled=true, checked=true, desktop bounds=[10,20,30,4.5])\n  - AXStaticText = \"Welcome\""
        );
    }

    #[test]
    fn another_window_or_an_ambiguous_tree_is_refused() {
        let other = json!({ "pid": 7, "window_id": 10, "tree_markdown": "- AXWindow" });
        assert_eq!(accessibility_from_snapshot(&other, &target()), unavailable("window"));
        let two = json!({ "pid": 7, "window_id": 9, "tree_markdown": "- AXWindow \"a\"\n- AXWindow \"b\"" });
        assert_eq!(accessibility_from_snapshot(&two, &target()), unavailable("window"));
        let empty = json!({ "pid": 7, "window_id": 9, "tree_markdown": "  " });
        assert_eq!(accessibility_from_snapshot(&empty, &target()), unavailable("empty"));
        assert_eq!(hyprland_accessibility_window_id(Some("0x55d1c0a3e2f0")), Some(0x55d1c0a3e2f0));
        assert_eq!(hyprland_accessibility_window_id(Some("abc")), None);
    }
}
