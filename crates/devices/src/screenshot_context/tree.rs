//! The accessibility tree as the computer-use driver renders it, read row by row (screenshot-context-tree.ts).

use std::sync::LazyLock;

use act_core::domain::conversation::ScreenshotPlatform;
use regex::Regex;

#[derive(Clone, Debug, PartialEq)]
pub struct ScreenshotTreeRow {
    pub text: String,
    pub depth: usize,
    pub role: String,
    pub index: Option<u64>,
}

static TRUNCATED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^⚠.*\b(?:AX tree|accessibility tree) truncated at \d+ nodes\b").unwrap());
static MAC_ROW: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\s*)- (?:\[(?:element_index )?(\d+)\] )?(AX[A-Za-z0-9]+)(?:\s|$)").unwrap());
static LINUX_ROW: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"^(\s*)- (?:\[(\d+)\] ([a-z][a-z\d -]*?) "|([a-z][a-z\d -]*?) = ")"#).unwrap());
static DOCUMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:AXWebArea|AXDocument|AXDocumentWeb|document(?: web| frame| text)?)$").unwrap());
static PROTECTED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:AXSecureTextField|password(?: text| field)?)$").unwrap());

/// Only native role prefixes start rows; paragraphs and list items can span several physical lines.
pub fn screenshot_tree(markdown: &str, platform: ScreenshotPlatform) -> (Vec<ScreenshotTreeRow>, bool) {
    let mut rows: Vec<ScreenshotTreeRow> = Vec::new();
    let mut truncated = false;
    for line in markdown.split('\n') {
        if TRUNCATED.is_match(line.trim_start()) {
            truncated = true;
            break;
        }
        let pattern = if platform == ScreenshotPlatform::Macos { &*MAC_ROW } else { &*LINUX_ROW };
        if let Some(found) = pattern.captures(line) {
            let role = found.get(3).or_else(|| found.get(4)).map(|role| role.as_str().to_string()).unwrap_or_default();
            rows.push(ScreenshotTreeRow {
                text: line.to_string(),
                depth: found.get(1).map(|indent| indent.as_str().encode_utf16().count()).unwrap_or(0),
                role,
                index: found.get(2).and_then(|index| index.as_str().parse().ok()),
            });
        } else if let Some(last) = rows.last_mut().filter(|_| !line.trim().is_empty()) {
            last.text.push_str("\\n");
            last.text.push_str(line.trim());
        }
    }
    (rows, truncated)
}

pub fn is_document_role(role: &str) -> bool {
    DOCUMENT.is_match(role)
}

pub fn is_protected_role(role: &str) -> bool {
    PROTECTED.is_match(role)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_keep_their_depth_index_and_continuation_lines() {
        let markdown = "- [0] AXWindow \"Docs\"\n  - [3] AXStaticText = \"one\n  two\"\n  - AXGroup\n⚠️ AX tree truncated at 2000 nodes\n  - AXButton";
        let (rows, truncated) = screenshot_tree(markdown, ScreenshotPlatform::Macos);
        assert!(truncated);
        assert_eq!(rows.len(), 3);
        assert_eq!((rows[1].depth, rows[1].role.as_str(), rows[1].index), (2, "AXStaticText", Some(3)));
        assert!(rows[1].text.ends_with("\\ntwo\""));
    }
}
