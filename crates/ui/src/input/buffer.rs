//! The text a field edits, apart from how it is drawn: content, selection, the IME's marked range,
//! movement by grapheme and word, and undo history.

use std::ops::Range;
use std::time::{Duration, Instant};

use unicode_segmentation::UnicodeSegmentation;

/// How an edit joins the undo history. Runs of typing or deleting in one place undo as one step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKind {
    Typing,
    Deleting,
    /// Paste, cut, drop, or a replacement made by code: always its own step.
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Snapshot {
    text: String,
    selection: Range<usize>,
    reversed: bool,
}

/// How long a pause still continues the same run of typing.
const COALESCE: Duration = Duration::from_millis(1000);
const HISTORY_LIMIT: usize = 400;

#[derive(Clone, Debug)]
pub struct EditBuffer {
    text: String,
    /// Byte offsets, always on char boundaries, `start <= end`.
    selection: Range<usize>,
    /// The caret sits at `selection.start` when reversed.
    reversed: bool,
    marked: Option<Range<usize>>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last_edit: Option<(EditKind, usize, Instant)>,
}

impl Default for EditBuffer {
    fn default() -> Self {
        Self::new("")
    }
}

impl EditBuffer {
    pub fn new(text: &str) -> Self {
        let end = text.len();
        Self {
            text: text.to_string(),
            selection: end..end,
            reversed: false,
            marked: None,
            undo: Vec::new(),
            redo: Vec::new(),
            last_edit: None,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn selection(&self) -> Range<usize> {
        self.selection.clone()
    }

    pub fn reversed(&self) -> bool {
        self.reversed
    }

    pub fn marked(&self) -> Option<Range<usize>> {
        self.marked.clone()
    }

    pub fn cursor(&self) -> usize {
        if self.reversed {
            self.selection.start
        } else {
            self.selection.end
        }
    }

    /// The end of the selection that stays put while it is extended.
    pub fn anchor(&self) -> usize {
        if self.reversed {
            self.selection.end
        } else {
            self.selection.start
        }
    }

    pub fn selected_text(&self) -> &str {
        &self.text[self.selection.clone()]
    }

    fn clamp(&self, offset: usize) -> usize {
        let mut offset = offset.min(self.text.len());
        while !self.text.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    }

    /// Replaces the whole text as code, not as an edit the user can undo into pieces.
    pub fn set_text(&mut self, text: &str) {
        if self.text == text {
            return;
        }
        self.push_undo();
        self.redo.clear();
        self.text = text.to_string();
        let end = self.text.len();
        self.selection = end..end;
        self.reversed = false;
        self.marked = None;
        self.last_edit = None;
    }

    /// Clears the text and its history, as a field does after its contents were sent.
    pub fn reset(&mut self, text: &str) {
        *self = Self::new(text);
    }

    pub fn move_to(&mut self, offset: usize) {
        let offset = self.clamp(offset);
        self.selection = offset..offset;
        self.reversed = false;
        self.last_edit = None;
    }

    /// Moves the caret end of the selection, keeping the anchor.
    pub fn select_to(&mut self, offset: usize) {
        let offset = self.clamp(offset);
        let anchor = self.anchor();
        if offset < anchor {
            self.selection = offset..anchor;
            self.reversed = true;
        } else {
            self.selection = anchor..offset;
            self.reversed = false;
        }
        self.last_edit = None;
    }

    pub fn select_range(&mut self, range: Range<usize>, reversed: bool) {
        let start = self.clamp(range.start.min(range.end));
        let end = self.clamp(range.end.max(range.start));
        self.selection = start..end;
        self.reversed = reversed && start != end;
        self.last_edit = None;
    }

    pub fn select_all(&mut self) {
        self.select_range(0..self.text.len(), false);
    }

    pub fn previous_grapheme(&self, offset: usize) -> usize {
        self.text[..offset.min(self.text.len())]
            .grapheme_indices(true)
            .next_back()
            .map(|(index, _)| index)
            .unwrap_or(0)
    }

    pub fn next_grapheme(&self, offset: usize) -> usize {
        let offset = offset.min(self.text.len());
        self.text[offset..]
            .graphemes(true)
            .next()
            .map(|grapheme| offset + grapheme.len())
            .unwrap_or(self.text.len())
    }

    /// The start of the word at or before `offset`, skipping any gap before it (Option-Left).
    pub fn previous_word(&self, offset: usize) -> usize {
        let mut result = 0;
        for (index, word) in self.text[..offset.min(self.text.len())].split_word_bound_indices().rev() {
            if is_word(word) {
                result = index;
                break;
            }
        }
        result
    }

    /// The end of the word at or after `offset`, skipping any gap before it (Option-Right).
    pub fn next_word(&self, offset: usize) -> usize {
        let offset = offset.min(self.text.len());
        for (index, word) in self.text[offset..].split_word_bound_indices() {
            if is_word(word) {
                return offset + index + word.len();
            }
        }
        self.text.len()
    }

    /// The word around `offset`, for a double click.
    pub fn word_range(&self, offset: usize) -> Range<usize> {
        let offset = offset.min(self.text.len());
        let mut start = 0;
        for (index, word) in self.text.split_word_bound_indices() {
            let end = index + word.len();
            if offset < end || (offset == end && end == self.text.len()) {
                if word.trim().is_empty() && offset > 0 && index == offset {
                    break;
                }
                return index..end;
            }
            start = end;
        }
        start..self.text.len()
    }

    pub fn line_start(&self, offset: usize) -> usize {
        self.text[..offset.min(self.text.len())].rfind('\n').map(|index| index + 1).unwrap_or(0)
    }

    pub fn line_end(&self, offset: usize) -> usize {
        let offset = offset.min(self.text.len());
        self.text[offset..].find('\n').map(|index| offset + index).unwrap_or(self.text.len())
    }

    /// The logical line around `offset`, including its newline, for a triple click.
    pub fn line_range(&self, offset: usize) -> Range<usize> {
        let start = self.line_start(offset);
        let end = self.line_end(offset);
        start..(end + 1).min(self.text.len()).max(end)
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot { text: self.text.clone(), selection: self.selection.clone(), reversed: self.reversed }
    }

    fn push_undo(&mut self) {
        self.undo.push(self.snapshot());
        if self.undo.len() > HISTORY_LIMIT {
            self.undo.remove(0);
        }
    }

    /// Replaces `range` (or the marked text, or the selection) with `text`, leaving the caret after it.
    pub fn replace(&mut self, range: Option<Range<usize>>, text: &str, kind: EditKind) {
        let range = range.or_else(|| self.marked.clone()).unwrap_or_else(|| self.selection.clone());
        let range = self.clamp(range.start)..self.clamp(range.end);
        if range.is_empty() && text.is_empty() {
            self.marked = None;
            return;
        }
        let now = Instant::now();
        let continues = matches!(self.last_edit, Some((last, at, time))
            if last == kind && kind != EditKind::Other && at == range.end.max(range.start) && now.duration_since(time) < COALESCE
            && (kind == EditKind::Deleting || range.is_empty()));
        if !continues || self.marked.is_some() {
            self.push_undo();
        }
        self.redo.clear();
        self.text.replace_range(range.clone(), text);
        let caret = range.start + text.len();
        self.selection = caret..caret;
        self.reversed = false;
        self.marked = None;
        self.last_edit = Some((kind, caret, now));
        if kind == EditKind::Deleting {
            self.last_edit = Some((kind, range.start, now));
        }
    }

    /// The IME's composition: replaces the range with `text`, marks it, and selects within it.
    pub fn replace_and_mark(&mut self, range: Option<Range<usize>>, text: &str, selected: Option<Range<usize>>) {
        let range = range.or_else(|| self.marked.clone()).unwrap_or_else(|| self.selection.clone());
        let range = self.clamp(range.start)..self.clamp(range.end);
        if self.marked.is_none() {
            self.push_undo();
            self.redo.clear();
        }
        self.text.replace_range(range.clone(), text);
        self.marked = (!text.is_empty()).then(|| range.start..range.start + text.len());
        let selected = selected
            .map(|inner| range.start + inner.start..range.start + inner.end)
            .unwrap_or(range.start + text.len()..range.start + text.len());
        self.selection = self.clamp(selected.start)..self.clamp(selected.end);
        self.reversed = false;
        self.last_edit = None;
    }

    pub fn unmark(&mut self) {
        self.marked = None;
    }

    /// Deletes the selection, or from the caret to `to` when nothing is selected.
    pub fn delete_to(&mut self, to: usize) {
        let range = if self.selection.is_empty() {
            let cursor = self.cursor();
            let to = self.clamp(to);
            to.min(cursor)..to.max(cursor)
        } else {
            self.selection.clone()
        };
        if range.is_empty() {
            return;
        }
        self.replace(Some(range), "", EditKind::Deleting);
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) -> bool {
        let Some(snapshot) = self.undo.pop() else { return false };
        self.redo.push(self.snapshot());
        self.restore(snapshot);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(snapshot) = self.redo.pop() else { return false };
        self.undo.push(self.snapshot());
        self.restore(snapshot);
        true
    }

    fn restore(&mut self, snapshot: Snapshot) {
        self.text = snapshot.text;
        self.selection = snapshot.selection;
        self.reversed = snapshot.reversed;
        self.marked = None;
        self.last_edit = None;
    }

    pub fn offset_to_utf16(&self, offset: usize) -> usize {
        self.text[..self.clamp(offset)].encode_utf16().count()
    }

    pub fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf16 = 0;
        for (index, ch) in self.text.char_indices() {
            if utf16 >= offset {
                return index;
            }
            utf16 += ch.len_utf16();
        }
        self.text.len()
    }

    pub fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    pub fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }
}

fn is_word(segment: &str) -> bool {
    segment.chars().any(|ch| ch.is_alphanumeric() || ch == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_in_one_place_undoes_as_one_step() {
        let mut buffer = EditBuffer::new("");
        for ch in ["h", "e", "y"] {
            buffer.replace(None, ch, EditKind::Typing);
        }
        buffer.replace(None, " there", EditKind::Other);
        assert_eq!(buffer.text(), "hey there");
        buffer.undo();
        assert_eq!(buffer.text(), "hey");
        buffer.undo();
        assert_eq!(buffer.text(), "");
        buffer.redo();
        assert_eq!(buffer.text(), "hey");
    }

    #[test]
    fn moving_the_caret_starts_a_new_undo_step() {
        let mut buffer = EditBuffer::new("");
        buffer.replace(None, "a", EditKind::Typing);
        buffer.move_to(0);
        buffer.replace(None, "b", EditKind::Typing);
        assert_eq!(buffer.text(), "ba");
        buffer.undo();
        assert_eq!(buffer.text(), "a");
    }

    #[test]
    fn word_motion_skips_the_gap_before_a_word() {
        let buffer = EditBuffer::new("let foo = bar_baz;");
        assert_eq!(buffer.previous_word(18), 10);
        assert_eq!(buffer.previous_word(10), 4);
        assert_eq!(buffer.next_word(3), 7);
        assert_eq!(buffer.next_word(7), 17);
    }

    #[test]
    fn graphemes_keep_clusters_whole() {
        let buffer = EditBuffer::new("a👍🏽b");
        let after_thumb = buffer.next_grapheme(1);
        assert_eq!(&buffer.text()[1..after_thumb], "👍🏽");
        assert_eq!(buffer.previous_grapheme(after_thumb), 1);
    }

    #[test]
    fn composition_replaces_its_own_marked_text() {
        let mut buffer = EditBuffer::new("x");
        buffer.replace_and_mark(None, "n", None);
        buffer.replace_and_mark(None, "に", None);
        assert_eq!(buffer.text(), "xに");
        buffer.replace(None, "日", EditKind::Typing);
        assert_eq!(buffer.text(), "x日");
        assert_eq!(buffer.marked(), None);
    }

    #[test]
    fn utf16_offsets_round_trip_through_astral_chars() {
        let buffer = EditBuffer::new("a😀b");
        assert_eq!(buffer.offset_to_utf16(5), 3);
        assert_eq!(buffer.offset_from_utf16(3), 5);
    }

    #[test]
    fn selection_extends_from_its_anchor_either_way() {
        let mut buffer = EditBuffer::new("hello");
        buffer.move_to(2);
        buffer.select_to(4);
        assert_eq!(buffer.selection(), 2..4);
        buffer.select_to(0);
        assert_eq!(buffer.selection(), 0..2);
        assert!(buffer.reversed());
        assert_eq!(buffer.cursor(), 0);
    }
}
