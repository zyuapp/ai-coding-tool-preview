//! Cutting a growing Markdown stream into settled blocks and a repaired live tail, ported from
//! `domain/markdown-stream.ts` and `components/StreamingText.tsx`.

/// An open code fence: its marker character and run length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fence {
    pub marker: char,
    pub length: usize,
}

/// How far a Markdown stream can be cut without splitting a block, plus the reader's position.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockScan {
    /// Byte index just past the last complete block. Never moves backwards.
    pub safe_end: usize,
    /// How much of the text the scan has already read.
    pub scanned: usize,
    pub fence: Option<Fence>,
}

/// `^(?:\s*>\s*)*\s*(?:(?:[-+*]|\d+[.)])\s+)?(`{3,}|~{3,})([\s\S]*)$`: the fence run and what follows it.
fn block_marker(line: &str) -> Option<(char, usize, &str)> {
    let rest = line.trim_start_matches(|ch: char| ch.is_whitespace() || ch == '>');
    let rest = list_marker(rest).unwrap_or(rest);
    let marker = rest.chars().next().filter(|ch| *ch == '`' || *ch == '~')?;
    let length = rest.len() - rest.trim_start_matches(marker).len();
    (length >= 3).then(|| (marker, length, &rest[length..]))
}

/// A list marker and the whitespace after it, consumed.
fn list_marker(text: &str) -> Option<&str> {
    let after = if text.starts_with(['-', '+', '*']) {
        &text[1..]
    } else {
        let digits = text.len()
            - text
                .trim_start_matches(|ch: char| ch.is_ascii_digit())
                .len();
        if digits == 0 || !text[digits..].starts_with(['.', ')']) {
            return None;
        }
        &text[digits + 1..]
    };
    let trimmed = after.trim_start_matches(char::is_whitespace);
    (trimmed.len() < after.len()).then_some(trimmed)
}

/// Reads the lines added since the last scan. A block ends at a blank line outside a fence or at the
/// line that closes one, so a cut at `safe_end` never lands inside a fence, list, or emphasis run.
pub fn scan_blocks(text: &str, previous: BlockScan) -> BlockScan {
    let BlockScan {
        mut safe_end,
        scanned,
        mut fence,
    } = previous;
    let mut line_start = scanned.min(text.len());
    while line_start < text.len() {
        let Some(offset) = text[line_start..].find('\n') else {
            break;
        };
        let newline = line_start + offset;
        let line = &text[line_start..newline];
        let marker = block_marker(line);
        match (fence, marker) {
            (None, Some((marker, length, _))) => fence = Some(Fence { marker, length }),
            (Some(open), Some((marker, length, rest)))
                if marker == open.marker && length >= open.length && rest.trim().is_empty() =>
            {
                fence = None;
                safe_end = newline + 1;
            }
            (None, None) if line.trim().is_empty() => safe_end = newline + 1,
            _ => {}
        }
        line_start = newline + 1;
    }
    BlockScan {
        safe_end,
        scanned: line_start,
        fence,
    }
}

/// CommonMark's punctuation class, which decides whether a delimiter run can open or close.
fn is_punctuation(ch: char) -> bool {
    ch.is_ascii_punctuation()
}

fn closing_run(text: &str, from: usize, ch: u8, length: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut index = from;
    while index < bytes.len() {
        let found = index + bytes[index..].iter().position(|byte| *byte == ch)?;
        let mut run = 0;
        while bytes.get(found + run) == Some(&ch) {
            run += 1;
        }
        if run == length {
            return Some(found);
        }
        index = found + run;
    }
    None
}

/// Whether a delimiter run leans against the text before or after it, which is what lets it pair.
fn flanks(text: &str, start: usize, end: usize, ch: char) -> (bool, bool) {
    let before = text[..start].chars().next_back().unwrap_or(' ');
    let after = text[end..].chars().next().unwrap_or(' ');
    let left = !after.is_whitespace()
        && (!is_punctuation(after) || before.is_whitespace() || is_punctuation(before));
    let right = !before.is_whitespace()
        && (!is_punctuation(before) || after.is_whitespace() || is_punctuation(after));
    if ch != '_' {
        return (left, right);
    }
    (
        left && (!right || is_punctuation(before)),
        right && (!left || is_punctuation(after)),
    )
}

/// How much of `text` renders as finished Markdown. A stream can stop anywhere, and a cut inside a
/// code span, link, or emphasis run leaves its markers on screen as literal text, so the cut moves
/// back to before whatever is still being written.
pub fn inline_safe_end(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut open: Vec<(u8, usize)> = Vec::new();
    let earliest = |open: &Vec<(u8, usize)>| open.first().map_or(text.len(), |(_, at)| *at);
    let mut index = 0;
    while index < bytes.len() {
        let Some(offset) = bytes[index..].iter().position(|byte| {
            matches!(byte, b'\\' | b'`' | b'<' | b'[' | b'!' | b'*' | b'_' | b'~')
        }) else {
            break;
        };
        index += offset;
        let ch = bytes[index];
        match ch {
            b'\\' => index += 2,
            b'`' => {
                let start = index;
                while bytes.get(index) == Some(&b'`') {
                    index += 1;
                }
                match closing_run(text, index, b'`', index - start) {
                    Some(close) => index = close + (index - start),
                    None => return start.min(earliest(&open)),
                }
            }
            b'<' => match text[index..].find('>') {
                Some(close) => index += close + 1,
                None => return index.min(earliest(&open)),
            },
            b'[' | b'!' if ch == b'[' || bytes.get(index + 1) == Some(&b'[') => {
                let start = index;
                let Some(label) = text[start + 1..].find(']').map(|at| start + 1 + at) else {
                    return start.min(earliest(&open));
                };
                if bytes.get(label + 1) == Some(&b'(') && !text[label + 1..].contains(')') {
                    return start.min(earliest(&open));
                }
                index = label + 1;
            }
            b'*' | b'_' | b'~' => {
                let start = index;
                while bytes.get(index) == Some(&ch) {
                    index += 1;
                }
                // A run at the cut has nothing after it yet, so it closes what is open and otherwise opens.
                let (can_open, can_close) = if index >= bytes.len() {
                    (
                        true,
                        text[..start]
                            .chars()
                            .next_back()
                            .is_some_and(|before| !before.is_whitespace()),
                    )
                } else {
                    flanks(text, start, index, ch as char)
                };
                if can_close && open.last().is_some_and(|(open_ch, _)| *open_ch == ch) {
                    open.pop();
                } else if can_open {
                    open.push((ch, start));
                }
            }
            _ => index += 1,
        }
    }
    earliest(&open).min(text.len())
}

/// A line that carries markers but no content yet: `##`, `-`, `>`, `|`, a fence, a rule in progress.
fn markers_only(text: &str) -> bool {
    text.chars()
        .all(|ch| ch.is_whitespace() || ">#*+-_=~`|.:".contains(ch) || ch.is_ascii_digit())
}

fn is_table_row(line: &str) -> bool {
    line.trim_start().starts_with('|')
}

fn is_table_delimiter(line: &str) -> bool {
    line.contains('-')
        && line
            .chars()
            .all(|ch| ch == '|' || ch == ':' || ch == '-' || ch.is_whitespace())
}

/// Makes a cut in a Markdown stream render as finished text. An open fence is closed, a table with no
/// delimiter row yet and a line that is still only markers are held back, and the cut moves back off
/// any half-written inline markup. Text is only ever withheld, never altered.
pub fn repair_cut(text: &str) -> String {
    let cut = text.rfind('\n').map_or(0, |at| at + 1);
    let complete = &text[..cut];
    let scan = scan_blocks(complete, BlockScan::default());
    if let Some(fence) = scan.fence {
        let close: String = std::iter::repeat_n(fence.marker, fence.length).collect();
        let newline = if text.ends_with('\n') { "" } else { "\n" };
        return format!("{text}{newline}{close}\n");
    }
    let partial = &text[cut..];
    let held = if markers_only(partial) || block_marker(partial).is_some() {
        ""
    } else {
        partial
    };
    let mut lines: Vec<&str> = complete.split('\n').collect();
    lines.pop();
    let mut rows = lines.len();
    while rows > 0 && is_table_row(lines[rows - 1]) {
        rows -= 1;
    }
    let established = lines[rows..].iter().any(|line| is_table_delimiter(line));
    let mut body = if established {
        complete.to_string()
    } else {
        lines[..rows]
            .iter()
            .map(|line| format!("{line}\n"))
            .collect()
    };
    if !is_table_row(held) || established {
        body.push_str(held);
    }
    let end = inline_safe_end(&body);
    body.truncate(end);
    body
}

/// How much settled text one chunk gathers before it is frozen and never parsed again.
pub const CHUNK_CHARS: usize = 4_000;

/// A line that may still belong to the block above: indented, a list item, a quote, or a link definition.
fn continues(line: &str) -> bool {
    let bytes = line.as_bytes();
    match bytes.first() {
        Some(b' ' | b'\t' | b'>') => true,
        Some(b'-' | b'*' | b'+') => matches!(bytes.get(1), Some(b' ' | b'\t')),
        Some(b'[') => line[1..]
            .find(']')
            .is_some_and(|at| !line[1..1 + at].contains(']') && line[2 + at..].starts_with(':')),
        Some(byte) if byte.is_ascii_digit() => {
            let digits = bytes
                .iter()
                .take_while(|byte| byte.is_ascii_digit())
                .count();
            matches!(bytes.get(digits), Some(b'.' | b')'))
                && matches!(bytes.get(digits + 1), Some(b' ' | b'\t'))
        }
        _ => false,
    }
}

/// A cut holds only where the next line has arrived and starts a block of its own.
fn clean_cut(text: &str, at: usize) -> bool {
    let Some(newline) = text[at..].find('\n') else {
        return false;
    };
    let next = &text[at..at + newline];
    !next.trim().is_empty() && !continues(next)
}

/// Where a streamed answer is cut. Streamed text only ever grows, so the block scan resumes rather
/// than re-reading from the start. Settled text is cut at block ends into chunks, so a new block
/// re-parses only the chunk it joins.
#[derive(Clone, Debug, Default)]
pub struct StreamCuts {
    scan: BlockScan,
    seen: usize,
    seen_hash: u64,
    cuts: Vec<usize>,
}

/// A fingerprint of a prefix: its length and its last bytes, which change when a text is replaced
/// rather than extended without reading all of it.
fn prefix_hash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.len().hash(&mut hasher);
    text.as_bytes()[text.len().saturating_sub(256)..].hash(&mut hasher);
    hasher.finish()
}

impl StreamCuts {
    /// Reads `text` as the stream now stands. Returns the end of the settled blocks and the chunk cuts
    /// before it. A text that no longer extends the last one starts the scan over.
    pub fn update(&mut self, text: &str, active: bool) -> (usize, &[usize]) {
        if !active {
            *self = Self::default();
            return (0, &self.cuts);
        }
        let extends = self.seen <= text.len()
            && text.is_char_boundary(self.seen)
            && prefix_hash(&text[..self.seen]) == self.seen_hash;
        if !extends {
            *self = Self::default();
        }
        let before = self.scan.safe_end;
        self.scan = scan_blocks(text, self.scan);
        self.seen = text.len();
        self.seen_hash = prefix_hash(text);
        let end = self.scan.safe_end;
        let last = self.cuts.last().copied().unwrap_or(0);
        // The newest block end waits for the line after it, and a blank line arriving with the end
        // of that line moves it on; the block end it moved past then holds.
        let cut = [end, before]
            .into_iter()
            .find(|at| *at >= last + CHUNK_CHARS && clean_cut(text, *at));
        if let Some(at) = cut {
            self.cuts.push(at);
        }
        (end, &self.cuts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cut_inside_inline_markup_moves_back_to_before_the_markup() {
        assert_eq!(repair_cut("Then a **partly"), "Then a ");
        assert_eq!(repair_cut("Use `npm ru"), "Use ");
        assert_eq!(repair_cut("See [the docs](http"), "See ");
        assert_eq!(repair_cut("Some ~~struck"), "Some ");
        assert_eq!(repair_cut("An <http"), "An ");
    }

    #[test]
    fn markup_the_stream_has_already_closed_is_kept() {
        assert_eq!(repair_cut("Then a **partly**"), "Then a **partly**");
        assert_eq!(repair_cut("a `code` span"), "a `code` span");
        assert_eq!(repair_cut("one *two* three"), "one *two* three");
        assert_eq!(
            repair_cut("[the docs](https://x) and on"),
            "[the docs](https://x) and on"
        );
    }

    #[test]
    fn a_lone_underscore_or_asterisk_in_prose_is_not_an_opener() {
        assert_eq!(
            repair_cut("The task_id field is fine"),
            "The task_id field is fine"
        );
        assert_eq!(repair_cut("2 * 3 is six"), "2 * 3 is six");
    }

    #[test]
    fn emphasis_left_open_across_lines_holds_back_the_rest() {
        assert_eq!(repair_cut("Multi **line\nemphasis still open"), "Multi ");
    }

    #[test]
    fn an_open_fence_is_closed() {
        assert_eq!(
            repair_cut("```ts\nconst a = 1;\nconst b"),
            "```ts\nconst a = 1;\nconst b\n```\n"
        );
        assert_eq!(repair_cut("```ts"), "");
        assert_eq!(repair_cut("- ~~~~\nx\n"), "- ~~~~\nx\n~~~~\n");
    }

    #[test]
    fn a_table_waits_for_its_delimiter_row() {
        assert_eq!(repair_cut("| a | b |\n| c | d |"), "");
        assert_eq!(
            repair_cut("| a | b |\n| --- | --- |\n| c | d"),
            "| a | b |\n| --- | --- |\n| c | d"
        );
        assert_eq!(repair_cut("Intro line\n| a | b |"), "Intro line\n");
    }

    #[test]
    fn a_line_of_only_markers_waits() {
        assert_eq!(repair_cut("## "), "");
        assert_eq!(repair_cut("## Head"), "## Head");
        assert_eq!(repair_cut("- "), "");
        assert_eq!(repair_cut(""), "");
    }

    #[test]
    fn inline_scanning_finds_late_markers() {
        let plain = "ordinary prose ".repeat(1_000);
        let before_open = format!("{plain}! still plain ");
        assert_eq!(
            inline_safe_end(&format!("{before_open}**open")),
            before_open.len()
        );
        let escaped = format!("{plain}\\* escaped");
        assert_eq!(inline_safe_end(&escaped), escaped.len());
        let closed = format!("{plain}`closed` tail");
        assert_eq!(inline_safe_end(&closed), closed.len());
        assert_eq!(inline_safe_end("é *open"), "é ".len());
    }

    #[test]
    fn the_block_scan_cuts_only_at_whole_blocks() {
        assert_eq!(
            scan_blocks("One block.\n\nStill writing", BlockScan::default()).safe_end,
            "One block.\n\n".len()
        );
        assert_eq!(
            scan_blocks("```ts\ncode\n", BlockScan::default()).safe_end,
            0
        );
        let first = scan_blocks("```ts\ncode\n", BlockScan::default());
        assert_eq!(
            scan_blocks("```ts\ncode\n```\nmore", first).safe_end,
            "```ts\ncode\n```\n".len()
        );
    }

    #[test]
    fn chunks_cut_only_before_a_block_of_its_own() {
        let mut cuts = StreamCuts::default();
        let long = format!("{}\n\n", "word ".repeat(900));
        let (_, at) = cuts.update(&format!("{long}- item\n"), true);
        assert!(at.is_empty());
        let (end, at) = cuts.update(&format!("{long}Next paragraph\n"), true);
        assert_eq!(end, long.len());
        assert_eq!(at, &[long.len()]);
        let (_, at) = cuts.update("unrelated", true);
        assert!(at.is_empty());
    }

    #[test]
    fn a_block_end_passed_with_the_next_line_still_cuts() {
        let mut cuts = StreamCuts::default();
        let long = format!("{}\n\n", "word ".repeat(900));
        let (_, at) = cuts.update(&format!("{long}Next para"), true);
        assert!(at.is_empty());
        let (end, at) = cuts.update(&format!("{long}Next paragraph\n\nThird"), true);
        assert_eq!(end, format!("{long}Next paragraph\n\n").len());
        assert_eq!(at, &[long.len()]);
    }
}
