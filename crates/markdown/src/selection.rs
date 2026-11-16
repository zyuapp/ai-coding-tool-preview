//! Hit testing and text extraction across a message's text blocks.

use gpui::{Pixels, Point};
use unicode_segmentation::UnicodeSegmentation;

use crate::layout::{index_for_point, index_under_point};
use crate::render::{TextEntry, TextPos};

/// How a drag extends a selection: by character, by word after a double click, or by block after
/// a triple click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Granularity {
    Character,
    Word,
    Block,
}

fn bounds_of(entry: &TextEntry) -> Option<gpui::Bounds<Pixels>> {
    entry
        .text
        .cache
        .borrow()
        .painted()
        .map(|painted| painted.bounds)
}

/// The text position nearest a point: inside a block it is the nearest character boundary; in a gap
/// it is the end of the block above or the start of the block below.
pub(crate) fn position_for_point(entries: &[TextEntry], point: Point<Pixels>) -> Option<TextPos> {
    let mut best: Option<(f32, f32, usize)> = None;
    for (index, entry) in entries.iter().enumerate() {
        let Some(bounds) = bounds_of(entry) else {
            continue;
        };
        if entry.text.text.is_empty() {
            continue;
        }
        let dy = if point.y < bounds.top() {
            f32::from(bounds.top() - point.y)
        } else if point.y > bounds.bottom() {
            f32::from(point.y - bounds.bottom())
        } else {
            0.
        };
        let dx = if point.x < bounds.left() {
            f32::from(bounds.left() - point.x)
        } else if point.x > bounds.right() {
            f32::from(point.x - bounds.right())
        } else {
            0.
        };
        if best.is_none_or(|(best_dy, best_dx, _)| dy < best_dy || (dy == best_dy && dx < best_dx))
        {
            best = Some((dy, dx, index));
        }
    }
    let (_, _, block) = best?;
    let entry = &entries[block];
    let bounds = bounds_of(entry)?;
    let offset = if point.y < bounds.top() {
        0
    } else if point.y > bounds.bottom() {
        entry.text.text.len()
    } else {
        index_for_point(&entry.text, point)?
    };
    Some(TextPos { block, offset })
}

/// The link under a point, as (block, link index).
pub(crate) fn link_at_point(entries: &[TextEntry], point: Point<Pixels>) -> Option<(usize, usize)> {
    entries.iter().enumerate().find_map(|(block, entry)| {
        let index = index_under_point(&entry.text, point)?;
        let link = entry.text.links.iter().position(|link| {
            link.range.contains(&index) || (link.range.start == index && link.range.is_empty())
        })?;
        Some((block, link))
    })
}

/// The word around a position, as the browser's double click picks it.
pub(crate) fn word_range(entries: &[TextEntry], pos: TextPos) -> (TextPos, TextPos) {
    let Some(entry) = entries.get(pos.block) else {
        return (pos, pos);
    };
    let text = &entry.text.text;
    for (start, word) in text.split_word_bound_indices() {
        let end = start + word.len();
        if pos.offset < end || end == text.len() {
            return (
                TextPos {
                    block: pos.block,
                    offset: start,
                },
                TextPos {
                    block: pos.block,
                    offset: end,
                },
            );
        }
    }
    (pos, pos)
}

pub(crate) fn block_range(entries: &[TextEntry], pos: TextPos) -> (TextPos, TextPos) {
    let len = entries
        .get(pos.block)
        .map_or(0, |entry| entry.text.text.len());
    (
        TextPos {
            block: pos.block,
            offset: 0,
        },
        TextPos {
            block: pos.block,
            offset: len,
        },
    )
}

/// The text of a selection, with each block's separator between blocks.
pub(crate) fn selected_text(entries: &[TextEntry], start: TextPos, end: TextPos) -> String {
    let mut out = String::new();
    for block in start.block..=end.block.min(entries.len().saturating_sub(1)) {
        let Some(entry) = entries.get(block) else {
            break;
        };
        let text = &entry.text.text;
        let from = if block == start.block {
            start.offset.min(text.len())
        } else {
            0
        };
        let to = if block == end.block {
            end.offset.min(text.len())
        } else {
            text.len()
        };
        if from >= to || !text.is_char_boundary(from) || !text.is_char_boundary(to) {
            continue;
        }
        if !out.is_empty() {
            out.push_str(entry.separator);
        }
        out.push_str(&text[from..to]);
    }
    out
}

/// Clamps a stored position to the blocks as they now stand, which a growing stream can shift.
pub(crate) fn clamp(entries: &[TextEntry], pos: TextPos) -> TextPos {
    let Some(last) = entries.len().checked_sub(1) else {
        return TextPos::default();
    };
    if pos.block > last {
        return TextPos {
            block: last,
            offset: entries[last].text.text.len(),
        };
    }
    let text = &entries[pos.block].text.text;
    let mut offset = pos.offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    TextPos {
        block: pos.block,
        offset,
    }
}

/// The box around a range as last drawn, in window coordinates.
pub(crate) fn range_bounds(
    entries: &[TextEntry],
    start: TextPos,
    end: TextPos,
) -> Option<gpui::Bounds<Pixels>> {
    union_rects(entries, start, end, crate::layout::range_rects)
}

/// The box a DOM range reports: its text's own boxes, not the lines they sit on.
pub(crate) fn range_text_bounds(
    entries: &[TextEntry],
    start: TextPos,
    end: TextPos,
) -> Option<gpui::Bounds<Pixels>> {
    union_rects(entries, start, end, crate::layout::range_text_rects)
}

fn union_rects(
    entries: &[TextEntry],
    start: TextPos,
    end: TextPos,
    rects_of: fn(&crate::parse::InlineText, std::ops::Range<usize>) -> Option<Vec<gpui::Bounds<Pixels>>>,
) -> Option<gpui::Bounds<Pixels>> {
    let mut union: Option<gpui::Bounds<Pixels>> = None;
    let last = end.block.min(entries.len().saturating_sub(1));
    for (block, entry) in entries.iter().enumerate().take(last + 1).skip(start.block) {
        let from = if block == start.block {
            start.offset
        } else {
            0
        };
        let to = if block == end.block {
            end.offset
        } else {
            entry.text.text.len()
        };
        let Some(rects) = rects_of(&entry.text, from..to) else {
            continue;
        };
        for rect in rects {
            union = Some(union.map_or(rect, |current| current.union(&rect)));
        }
    }
    union
}

/// Case-insensitive matches of `query` inside single blocks.
pub(crate) fn find(entries: &[TextEntry], query: &str) -> Vec<(TextPos, TextPos)> {
    let needle: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    if needle.is_empty() {
        return Vec::new();
    }
    let mut found = Vec::new();
    for (block, entry) in entries.iter().enumerate() {
        let text = &entry.text.text;
        // Lowercased characters paired with the byte offset each came from.
        let folded: Vec<(usize, char)> = text
            .char_indices()
            .flat_map(|(at, ch)| ch.to_lowercase().map(move |lower| (at, lower)))
            .collect();
        let mut index = 0;
        while index + needle.len() <= folded.len() {
            if folded[index..index + needle.len()]
                .iter()
                .map(|(_, ch)| *ch)
                .eq(needle.iter().copied())
            {
                let start = folded[index].0;
                let last = folded[index + needle.len() - 1].0;
                let end = last + text[last..].chars().next().map_or(0, char::len_utf8);
                found.push((
                    TextPos {
                        block,
                        offset: start,
                    },
                    TextPos { block, offset: end },
                ));
                index += needle.len();
            } else {
                index += 1;
            }
        }
    }
    found
}

fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

pub(crate) fn rendered_offset(entries: &[TextEntry], pos: TextPos) -> usize {
    let before: usize = entries
        .iter()
        .take(pos.block)
        .map(|entry| utf16_len(&entry.text.text))
        .sum();
    let inside = entries.get(pos.block).map_or(0, |entry| {
        let text = &entry.text.text;
        let mut offset = pos.offset.min(text.len());
        while !text.is_char_boundary(offset) {
            offset -= 1;
        }
        utf16_len(&text[..offset])
    });
    before + inside
}

pub(crate) fn position_at(entries: &[TextEntry], offset: usize) -> Option<TextPos> {
    let mut remaining = offset;
    for (block, entry) in entries.iter().enumerate() {
        let text = &entry.text.text;
        let len = utf16_len(text);
        if remaining <= len {
            let mut units = 0;
            for (at, ch) in text.char_indices() {
                if units >= remaining {
                    return Some(TextPos { block, offset: at });
                }
                units += ch.len_utf16();
            }
            return Some(TextPos {
                block,
                offset: text.len(),
            });
        }
        remaining -= len;
    }
    None
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::*;
    use crate::parse::InlineText;

    fn entries(blocks: &[(&str, &'static str)]) -> Vec<TextEntry> {
        blocks
            .iter()
            .map(|(text, separator)| TextEntry {
                text: Rc::new(InlineText {
                    text: text.to_string(),
                    ..InlineText::default()
                }),
                separator,
            })
            .collect()
    }

    #[test]
    fn copied_text_joins_blocks_with_their_separators() {
        let entries = entries(&[
            ("Title", "\n\n"),
            ("one", "\n\n"),
            ("two", "\n"),
            ("A", "\n\n"),
            ("B", "\t"),
        ]);
        let text = selected_text(
            &entries,
            TextPos {
                block: 0,
                offset: 2,
            },
            TextPos {
                block: 4,
                offset: 1,
            },
        );
        assert_eq!(text, "tle\n\none\ntwo\n\nA\tB");
    }

    #[test]
    fn find_folds_case_and_offsets_count_utf16() {
        let entries = entries(&[("Émoji 😀 here", "\n\n"), ("HERE there", "\n\n")]);
        let found = find(&entries, "here");
        assert_eq!(found.len(), 3);
        assert_eq!(
            found[0].0,
            TextPos {
                block: 0,
                offset: "Émoji 😀 ".len()
            }
        );
        let second = found[1].0;
        assert_eq!(
            rendered_offset(&entries, second),
            "Émoji 😀 here".encode_utf16().count()
        );
        assert_eq!(
            position_at(&entries, rendered_offset(&entries, found[0].0)),
            Some(found[0].0)
        );
    }
}
