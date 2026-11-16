//! The emulated screen a shell draws on. One `Screen` is both what the panel paints and what a
//! caller reads, so a read always shows what the user sees.

use std::sync::Arc;
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Osc52};
use alacritty_terminal::vte::ansi::Processor;
use alacritty_terminal::Term;
use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use parking_lot::{Mutex, MutexGuard, RwLock};

use crate::contract::TerminalOutputRead;
use crate::theme::TerminalTheme;

/// How many lines of scrollback a shell keeps.
pub const SCROLLBACK_LINES: usize = 5_000;
pub const DEFAULT_COLS: u16 = 80;
pub const DEFAULT_ROWS: u16 = 24;

/// xterm's default `wordSeparator`, which a double click stops at.
const WORD_SEPARATORS: &str = " ()[]{}',\"`";

/// What parsing output asks of the session around the screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScreenEvent {
    /// OSC 0/2; `None` when the program reset it.
    Title(Option<String>),
    /// A reply the terminal owes the program, such as a cursor position or colour report.
    Reply(String),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct GridSize {
    pub cols: usize,
    pub rows: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

struct Shared {
    events: Mutex<Vec<ScreenEvent>>,
    theme: RwLock<TerminalTheme>,
}

/// Collects what the emulator reports while the term is locked; the screen hands it out after.
#[derive(Clone)]
pub struct Listener(Arc<Shared>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let reply = match event {
            Event::Title(title) => ScreenEvent::Title(Some(title)),
            Event::ResetTitle => ScreenEvent::Title(None),
            Event::PtyWrite(text) => ScreenEvent::Reply(text),
            Event::ColorRequest(index, format) => match self.0.theme.read().query_rgb(index) {
                Some(colour) => ScreenEvent::Reply(format(colour)),
                None => return,
            },
            _ => return,
        };
        self.0.events.lock().push(reply);
    }
}

pub type TermLock<'a> = MutexGuard<'a, Term<Listener>>;

pub struct Screen {
    term: FairMutex<Term<Listener>>,
    parser: Mutex<Processor>,
    shared: Arc<Shared>,
    subscribers: Mutex<Vec<UnboundedSender<()>>>,
}

impl Screen {
    pub fn new(cols: u16, rows: u16) -> Arc<Self> {
        let shared = Arc::new(Shared { events: Mutex::new(Vec::new()), theme: RwLock::new(TerminalTheme::default()) });
        let config = Config {
            scrolling_history: SCROLLBACK_LINES,
            semantic_escape_chars: WORD_SEPARATORS.to_owned(),
            kitty_keyboard: false,
            osc52: Osc52::Disabled,
            ..Config::default()
        };
        let size = GridSize { cols: cols.max(2) as usize, rows: rows.max(1) as usize };
        let term = Term::new(config, &size, Listener(shared.clone()));
        Arc::new(Self {
            term: FairMutex::new(term),
            parser: Mutex::new(Processor::new()),
            shared,
            subscribers: Mutex::new(Vec::new()),
        })
    }

    /// Parses output and returns what it asked for. Viewers are woken afterwards.
    pub fn feed(&self, bytes: &[u8]) -> Vec<ScreenEvent> {
        {
            let mut parser = self.parser.lock();
            let mut term = self.term.lock();
            parser.advance(&mut *term, bytes);
        }
        self.notify();
        std::mem::take(&mut *self.shared.events.lock())
    }

    /// Draws output from somewhere else, such as a paired computer's shell, whose own screen
    /// already answered anything it asked.
    pub fn write(&self, data: &str) {
        self.feed(data.as_bytes());
    }

    /// Restores a remote viewer from what its host read: a snapshot starts over, output continues.
    pub fn apply_read(&self, read: &TerminalOutputRead) {
        if read.kind == crate::contract::TerminalReadKind::Snapshot {
            self.feed(b"\x1bc");
        }
        self.resize(read.cols, read.rows);
        self.write(&read.data);
    }

    /// When a synchronized update (mode 2026) holds output back, the moment it must be let go.
    pub(crate) fn sync_deadline(&self) -> Option<Instant> {
        self.parser.lock().sync_timeout().sync_timeout()
    }

    /// Lets go of a synchronized update that never ended. Returns what parsing it asked for.
    pub(crate) fn finish_overdue_sync(&self) -> Vec<ScreenEvent> {
        {
            let mut parser = self.parser.lock();
            match parser.sync_timeout().sync_timeout() {
                Some(deadline) if deadline <= Instant::now() => {
                    let mut term = self.term.lock();
                    parser.stop_sync(&mut *term);
                }
                _ => return Vec::new(),
            }
        }
        self.notify();
        std::mem::take(&mut *self.shared.events.lock())
    }

    /// Returns whether the grid changed.
    pub fn resize(&self, cols: u16, rows: u16) -> bool {
        let size = GridSize { cols: cols.max(2) as usize, rows: rows.max(1) as usize };
        let changed = {
            let mut term = self.term.lock();
            if term.columns() == size.cols && term.screen_lines() == size.rows {
                false
            } else {
                // xterm clears the selection when the row count changes.
                let rows_changed = term.screen_lines() != size.rows;
                term.resize(size);
                if rows_changed {
                    term.selection = None;
                }
                true
            }
        };
        if changed {
            self.notify();
        }
        changed
    }

    pub fn size(&self) -> (u16, u16) {
        let term = self.term.lock();
        (term.columns() as u16, term.screen_lines() as u16)
    }

    /// The colours OSC 4/10/11/12 queries are answered with.
    pub fn set_theme(&self, theme: &TerminalTheme) {
        *self.shared.theme.write() = theme.clone();
    }

    /// Wakes whenever the screen changes. Bursts may be coalesced into one wake.
    pub fn subscribe(&self) -> UnboundedReceiver<()> {
        let (sender, receiver) = unbounded();
        self.subscribers.lock().push(sender);
        receiver
    }

    pub(crate) fn notify(&self) {
        self.subscribers.lock().retain(|subscriber| subscriber.unbounded_send(()).is_ok());
    }

    pub fn lock(&self) -> TermLock<'_> {
        self.term.lock()
    }

    /// Every line the active buffer holds, oldest first, with the trailing blank lines a screen
    /// always has removed.
    pub fn lines(&self) -> Vec<String> {
        let term = self.term.lock();
        let total = term.grid().total_lines();
        let mut lines: Vec<String> = (0..total).map(|row| row_text(&term, row)).collect();
        while lines.last().is_some_and(|line| line.trim().is_empty()) {
            lines.pop();
        }
        lines
    }

    /// The newest `limit` lines, and how many non-blank-tailed lines the buffer holds in all.
    pub fn tail(&self, limit: usize) -> (Vec<String>, usize) {
        let term = self.term.lock();
        let mut end = term.grid().total_lines();
        while end > 0 && row_text(&term, end - 1).trim().is_empty() {
            end -= 1;
        }
        let lines = (end.saturating_sub(limit)..end).map(|row| row_text(&term, row)).collect();
        (lines, end)
    }
}

/// A buffer row (0 is the oldest line of scrollback) as alacritty's line index.
pub(crate) fn row_line<T>(term: &Term<T>, row: usize) -> Line {
    Line(row as i32 - term.grid().history_size() as i32)
}

/// One row as plain text, trailing blanks trimmed, the way xterm's `translateToString(true)` reads.
pub(crate) fn row_text<T>(term: &Term<T>, row: usize) -> String {
    let grid = term.grid();
    let line = &grid[row_line(term, row)];
    let mut text = String::with_capacity(grid.columns());
    for col in 0..grid.columns() {
        let cell = &line[Column(col)];
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        if cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) || cell.c == '\t' || cell.c == '\0' {
            text.push(' ');
            continue;
        }
        text.push(cell.c);
        if let Some(extra) = cell.zerowidth() {
            text.extend(extra.iter());
        }
    }
    let trimmed = text.trim_end_matches(' ').len();
    text.truncate(trimmed);
    text
}

/// Whether a row continues onto the next one.
pub(crate) fn row_wraps<T>(term: &Term<T>, row: usize) -> bool {
    let grid = term.grid();
    if row + 1 >= grid.total_lines() {
        return false;
    }
    grid[row_line(term, row)][Column(grid.columns() - 1)].flags.contains(Flags::WRAPLINE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_resolve_overwrites_and_trim_blank_tail() {
        let screen = Screen::new(20, 4);
        screen.feed(b"hello\r\nworld\rW");
        assert_eq!(screen.lines(), vec!["hello", "World"]);
    }

    #[test]
    fn titles_and_replies_surface_after_parsing() {
        let screen = Screen::new(20, 4);
        let events = screen.feed(b"\x1b]0;build\x07\x1b[6n");
        assert!(events.contains(&ScreenEvent::Title(Some("build".into()))));
        assert!(events.contains(&ScreenEvent::Reply("\x1b[1;1R".into())));
    }
}
