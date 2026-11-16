//! Find in the terminal, following xterm's search addon: case-insensitive, across wrapped rows,
//! every match highlighted (up to a limit) and the current one selected and scrolled to.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::Term;

use crate::screen::{row_line, row_wraps};

/// The most matches highlighted at once.
const HIGHLIGHT_LIMIT: usize = 1_000;

/// A match in buffer rows (0 is the oldest line of scrollback), `size` cells long.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchMatch {
    pub row: usize,
    pub col: usize,
    pub size: usize,
}

impl SearchMatch {
    fn start(&self, cols: usize) -> usize {
        self.row * cols + self.col
    }

    fn end(&self, cols: usize) -> usize {
        self.start(cols) + self.size
    }
}

/// What the find bar shows for a search the terminal counted itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FindResults {
    pub matches: usize,
    /// Where the selected match sits among the highlighted ones; 0 when it is not among them.
    pub index: usize,
}

struct LogicalLine {
    last_row: usize,
    chars: Vec<char>,
    /// The cell each char came from: row, column, and how many columns it covers.
    cells: Vec<(usize, usize, usize)>,
}

fn fold(ch: char) -> char {
    ch.to_lowercase().next().unwrap_or(ch)
}

fn logical_line<T>(term: &Term<T>, first_row: usize) -> LogicalLine {
    let cols = term.columns();
    let total = term.grid().total_lines();
    let mut line = LogicalLine { last_row: first_row, chars: Vec::new(), cells: Vec::new() };
    let mut row = first_row;
    loop {
        let cells = &term.grid()[row_line(term, row)];
        for col in 0..cols {
            let cell = &cells[Column(col)];
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let width = if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
            let ch = if cell.c == '\t' || cell.c == '\0' { ' ' } else { cell.c };
            line.chars.push(fold(ch));
            line.cells.push((row, col, width));
            for extra in cell.zerowidth().into_iter().flatten() {
                line.chars.push(fold(*extra));
                line.cells.push((row, col, width));
            }
        }
        line.last_row = row;
        if !row_wraps(term, row) || row + 1 >= total {
            break;
        }
        row += 1;
    }
    while line.chars.last() == Some(&' ') && line.cells.last().is_some_and(|cell| cell.0 == line.last_row) {
        line.chars.pop();
        line.cells.pop();
    }
    line
}

/// The first row of the logical line `row` belongs to.
fn line_start<T>(term: &Term<T>, mut row: usize) -> usize {
    while row > 0 && row_wraps(term, row - 1) {
        row -= 1;
    }
    row
}

/// Every match in reading order, overlapping ones included.
fn all_matches<T>(term: &Term<T>, query: &[char]) -> Vec<SearchMatch> {
    let cols = term.columns();
    let total = term.grid().total_lines();
    let mut matches = Vec::new();
    let mut row = 0;
    while row < total {
        let line = logical_line(term, row);
        if line.chars.len() >= query.len() {
            for start in 0..=line.chars.len() - query.len() {
                if line.chars[start..start + query.len()] != *query {
                    continue;
                }
                let (first_row, first_col, _) = line.cells[start];
                let (last_row, last_col, width) = line.cells[start + query.len() - 1];
                let size = (last_row - first_row) * cols + last_col + width - first_col;
                matches.push(SearchMatch { row: first_row, col: first_col, size });
            }
        }
        row = line.last_row + 1;
    }
    matches
}

#[derive(Default)]
pub struct Search {
    /// The term the highlights were built for; cleared so the next search rebuilds them.
    cached: Option<String>,
    highlights: Vec<SearchMatch>,
    active: Option<SearchMatch>,
}

impl Search {
    pub fn highlights(&self) -> &[SearchMatch] {
        &self.highlights
    }

    pub fn active(&self) -> Option<SearchMatch> {
        self.active
    }

    pub fn is_active(&self) -> bool {
        self.cached.is_some()
    }

    /// Clears every highlight. The selected match stays selected.
    pub fn clear(&mut self) {
        self.cached = None;
        self.highlights.clear();
        self.active = None;
    }

    /// Re-runs the last search after output changed the screen, without scrolling.
    pub fn refresh<T: EventListener>(&mut self, term: &mut Term<T>) -> Option<FindResults> {
        let query = self.cached.take()?;
        Some(self.find(term, &query, false, true))
    }

    /// Steps to the next match, or the one before it, selecting it and scrolling it into view.
    pub fn find<T: EventListener>(&mut self, term: &mut Term<T>, query: &str, forward: bool, no_scroll: bool) -> FindResults {
        let folded: Vec<char> = query.chars().map(fold).collect();
        let changed = self.cached.as_deref() != Some(query);
        let previous = selection_bounds(term);
        self.active = None;
        if folded.is_empty() {
            term.selection = None;
            self.highlights.clear();
            self.cached = Some(String::new());
            return FindResults { matches: 0, index: 0 };
        }
        let cols = term.columns();
        let matches = all_matches(term, &folded);
        if changed {
            self.highlights = matches.iter().take(HIGHLIGHT_LIMIT).copied().collect();
        }
        let found = if forward {
            let start = match previous {
                Some((start, end)) => if changed { start } else { end },
                None => 0,
            };
            matches.iter().find(|m| m.start(cols) >= start).or(matches.first()).copied()
        } else {
            let mut found = None;
            let mut limit = term.grid().total_lines() * cols;
            if let Some((start, end)) = previous {
                limit = start;
                if changed {
                    // A new term first tries to grow the selection to the right.
                    let line = line_start(term, start / cols);
                    let last = logical_line(term, line).last_row;
                    found = matches.iter().find(|m| m.start(cols) >= start && m.row <= last).copied();
                    limit = end;
                }
            }
            found.or_else(|| matches.iter().rev().find(|m| m.end(cols) <= limit).or(matches.last()).copied())
        };
        match found {
            Some(found) => {
                select(term, found);
                if !no_scroll {
                    scroll_to(term, found.row);
                }
                self.active = Some(found);
            }
            None => term.selection = None,
        }
        self.cached = Some(query.to_owned());
        let index = self.active.and_then(|active| self.highlights.iter().position(|m| *m == active)).unwrap_or(0);
        FindResults { matches: self.highlights.len(), index }
    }
}

/// The current selection as linear cell positions, its end exclusive.
fn selection_bounds<T>(term: &Term<T>) -> Option<(usize, usize)> {
    let range = term.selection.as_ref()?.to_range(term)?;
    let cols = term.columns();
    let history = term.grid().history_size() as i32;
    let start = (range.start.line.0 + history) as usize * cols + range.start.column.0;
    let end_cell = &term.grid()[range.end.line][range.end.column];
    let width = if end_cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
    let end = (range.end.line.0 + history) as usize * cols + range.end.column.0 + width;
    Some((start, end))
}

fn point_at<T>(term: &Term<T>, position: usize) -> Point {
    let cols = term.columns();
    Point::new(row_line(term, position / cols), Column(position % cols))
}

fn select<T>(term: &mut Term<T>, found: SearchMatch) {
    let cols = term.columns();
    let start = found.start(cols);
    let mut selection = Selection::new(SelectionType::Simple, point_at(term, start), Side::Left);
    selection.update(point_at(term, start + found.size.max(1) - 1), Side::Right);
    term.selection = Some(selection);
}

fn scroll_to<T: EventListener>(term: &mut Term<T>, row: usize) {
    let rows = term.screen_lines();
    let viewport_top = term.grid().history_size() - term.grid().display_offset();
    if row >= viewport_top + rows || row < viewport_top {
        let lines = row as i32 - viewport_top as i32 - (rows / 2) as i32;
        term.scroll_display(Scroll::Delta(-lines));
    }
}

#[cfg(test)]
mod tests {
    use crate::screen::Screen;

    use super::*;

    #[test]
    fn finds_across_wraps_and_cycles_both_ways() {
        let screen = Screen::new(10, 5);
        screen.feed(b"one Match\r\nxxxxxxxmatch\r\nlast match");
        let mut term = screen.lock();
        let mut search = Search::default();
        let first = search.find(&mut term, "match", true, false);
        assert_eq!(first, FindResults { matches: 3, index: 0 });
        assert_eq!(search.active(), Some(SearchMatch { row: 0, col: 4, size: 5 }));
        search.find(&mut term, "match", true, false);
        assert_eq!(search.active(), Some(SearchMatch { row: 1, col: 7, size: 5 }));
        assert_eq!(search.find(&mut term, "match", true, false).index, 2);
        assert_eq!(search.find(&mut term, "match", true, false).index, 0);
        assert_eq!(search.find(&mut term, "match", false, false).index, 2);
        assert_eq!(term.selection_to_string().as_deref(), Some("match"));
    }
}
