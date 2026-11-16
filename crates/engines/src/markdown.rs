//! Streams assistant text one complete Markdown block at a time, so a half-written fence never ships.

use std::collections::HashMap;

use crate::provider::ProviderEvent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fence {
    pub marker: char,
    pub length: usize,
}

/// How far a Markdown stream can be cut without splitting a block, plus the reader's position.
/// Offsets are byte indices into the text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockScan {
    pub safe_end: usize,
    pub scanned: usize,
    pub fence: Option<Fence>,
}

/// `^(?:\s*>\s*)*\s*(?:(?:[-+*]|\d+[.)])\s+)?(`{3,}|~{3,})(.*)$`: the fence run and what follows it.
fn block_marker(line: &str) -> Option<(char, usize, &str)> {
    let mut rest = line;
    loop {
        let trimmed = rest.trim_start();
        if let Some(after) = trimmed.strip_prefix('>') {
            rest = after;
        } else {
            rest = trimmed;
            break;
        }
    }
    let list = {
        let bytes = rest.as_bytes();
        let marker_end = if matches!(bytes.first(), Some(b'-' | b'+' | b'*')) {
            Some(1)
        } else {
            let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
            if digits > 0 && matches!(bytes.get(digits), Some(b'.' | b')')) {
                Some(digits + 1)
            } else {
                None
            }
        };
        marker_end.and_then(|end| {
            let after = &rest[end..];
            let spaces = after.len() - after.trim_start().len();
            if spaces > 0 {
                Some(&after[spaces..])
            } else {
                None
            }
        })
    };
    let candidates = match list {
        Some(after_list) => vec![after_list, rest],
        None => vec![rest],
    };
    for candidate in candidates {
        let marker = candidate.chars().next()?;
        if marker != '`' && marker != '~' {
            continue;
        }
        let length = candidate.chars().take_while(|c| *c == marker).count();
        if length >= 3 {
            return Some((marker, length, &candidate[length..]));
        }
    }
    None
}

/// Reads the lines added since the last scan. A block ends at a blank line outside a fence or at the
/// line that closes one.
pub fn scan_blocks(text: &str, previous: &BlockScan) -> BlockScan {
    let mut safe_end = previous.safe_end;
    let mut fence = previous.fence.clone();
    let mut line_start = previous.scanned;
    while line_start < text.len() {
        let Some(offset) = text[line_start..].find('\n') else {
            break;
        };
        let newline = line_start + offset;
        let line = &text[line_start..newline];
        let marker = block_marker(line);
        match (&fence, marker) {
            (None, Some((char, length, _))) => {
                fence = Some(Fence {
                    marker: char,
                    length,
                })
            }
            (Some(open), Some((char, length, rest)))
                if char == open.marker && length >= open.length && rest.trim().is_empty() =>
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

/// Text a stream has produced but not released yet.
#[derive(Debug, Clone, Default)]
pub struct MarkdownBuffer {
    pub text: String,
    pub scan: BlockScan,
}

/// Releases whole Markdown blocks and keeps the rest buffered.
pub fn append_complete_markdown(buffer: &mut MarkdownBuffer, text: &str) -> String {
    buffer.text.push_str(text);
    let scan = scan_blocks(&buffer.text, &buffer.scan);
    if scan.safe_end == 0 {
        buffer.scan = scan;
        return String::new();
    }
    let complete = buffer.text[..scan.safe_end].to_string();
    buffer.text.drain(..scan.safe_end);
    buffer.scan = BlockScan {
        safe_end: 0,
        scanned: scan.scanned - scan.safe_end,
        fence: scan.fence,
    };
    complete
}

/// Streams a delta of one message: its whole blocks as they complete, and the rest as its tail.
pub fn stream_markdown(
    buffers: &mut HashMap<String, MarkdownBuffer>,
    emit: &dyn Fn(ProviderEvent),
    message_id: &str,
    delta: &str,
) {
    let buffer = buffers.entry(message_id.to_string()).or_default();
    let complete = append_complete_markdown(buffer, delta);
    if !complete.is_empty() {
        emit(ProviderEvent::Assistant {
            message_id: message_id.to_string(),
            text: complete,
            append: true,
            artifact: false,
        });
    }
    emit(ProviderEvent::AssistantTail {
        message_id: message_id.to_string(),
        text: buffer.text.clone(),
    });
}

/// Ends one message. One that streamed is on screen but for its tail; one that did not arrives whole.
pub fn end_markdown(
    buffers: &mut HashMap<String, MarkdownBuffer>,
    emit: &dyn Fn(ProviderEvent),
    message_id: &str,
    whole: &str,
) {
    if let Some(buffer) = buffers.remove(message_id) {
        if !buffer.text.is_empty() {
            emit(ProviderEvent::Assistant {
                message_id: message_id.to_string(),
                text: buffer.text,
                append: true,
                artifact: false,
            });
        }
    } else if !whole.trim().is_empty() {
        emit(ProviderEvent::Assistant {
            message_id: message_id.to_string(),
            text: whole.to_string(),
            append: false,
            artifact: false,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holds_an_open_fence_until_it_closes() {
        let mut buffer = MarkdownBuffer::default();
        assert_eq!(
            append_complete_markdown(&mut buffer, "Intro\n\n```rust\nfn a() {}\n\n"),
            "Intro\n\n"
        );
        assert_eq!(append_complete_markdown(&mut buffer, "more\n"), "");
        assert_eq!(
            append_complete_markdown(&mut buffer, "```\ntail"),
            "```rust\nfn a() {}\n\nmore\n```\n"
        );
        assert_eq!(buffer.text, "tail");
    }

    #[test]
    fn a_fence_inside_a_list_or_quote_counts() {
        let scan = scan_blocks("> - ~~~\ncode\n\n> ~~~\n", &BlockScan::default());
        assert_eq!(scan.fence, None);
        assert_eq!(scan.safe_end, 20);
        let open = scan_blocks("1. ```\nx\n\n", &BlockScan::default());
        assert!(open.fence.is_some());
        assert_eq!(open.safe_end, 0);
    }
}
