//! The terminal panel's shells. This is the only place a pseudo-terminal is created, written to, or
//! killed, and the only place that keeps what one has printed.
//!
//! Output is coalesced rather than parsed per chunk: a build printing thousands of small writes
//! would otherwise wake every viewer thousands of times. Each flush is parsed into the session's
//! [`Screen`], which the panel paints and reads come from, so both always agree.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::thread;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};

use crate::contract::{
    is_terminal_dimension, terminal_title, utf16_len, within_read_budget, TerminalError, TerminalOutputRead,
    TerminalReadKind, TerminalReadOptions, TerminalScreenSnapshot, TerminalStatus, TerminalText, TerminalUpdate,
    MAX_TERMINAL_INPUT, MAX_TERMINAL_LINES,
};
use crate::output_buffer::OutputBuffer;
use crate::screen::{Screen, ScreenEvent, DEFAULT_COLS, DEFAULT_ROWS};
use crate::snapshot::serialize_terminal;
use crate::theme::TerminalTheme;

const FLUSH_INTERVAL: Duration = Duration::from_millis(16);
/// The most one flush carries. Anything past it is dropped from the front, with a line saying so.
const MAX_FLUSH_BYTES: usize = 512 * 1024;
/// How long a reader with nothing new to read is held before it is answered anyway.
const OUTPUT_WAIT: Duration = Duration::from_secs(1);
/// How long an exited shell's last output is given to arrive before the exit is reported.
const EXIT_DRAIN: Duration = Duration::from_millis(250);

type UpdateHandler = dyn Fn(TerminalUpdate) + Send + Sync;

/// Owns every shell. Cheap to clone; clones share the same sessions.
///
/// Updates are reported on background threads.
#[derive(Clone)]
pub struct TerminalHost {
    inner: Arc<HostInner>,
}

struct HostInner {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    on_update: Box<UpdateHandler>,
    theme: Mutex<TerminalTheme>,
}

struct Io {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
}

#[derive(Default)]
struct State {
    pending: Vec<u8>,
    /// The start of a UTF-8 sequence the last read split, completed by the next one.
    carry: Vec<u8>,
    scheduled: bool,
    sequence: u64,
    output: Option<OutputBuffer>,
    eof: bool,
}

struct Session {
    id: String,
    cwd: String,
    screen: Arc<Screen>,
    io: Mutex<Option<Io>>,
    state: Mutex<State>,
    wake: Condvar,
    closed: AtomicBool,
    host: Weak<HostInner>,
}

impl TerminalHost {
    pub fn new(on_update: impl Fn(TerminalUpdate) + Send + Sync + 'static) -> Self {
        Self {
            inner: Arc::new(HostInner {
                sessions: Mutex::new(HashMap::new()),
                on_update: Box::new(on_update),
                theme: Mutex::new(TerminalTheme::default()),
            }),
        }
    }

    /// Idempotent: a terminal that already has a shell keeps it, so reopening the panel never
    /// restarts one.
    pub fn start(&self, terminal_id: &str, cwd: &str) {
        let session = {
            let mut sessions = self.inner.sessions.lock();
            if sessions.contains_key(terminal_id) {
                return;
            }
            let screen = Screen::new(DEFAULT_COLS, DEFAULT_ROWS);
            screen.set_theme(&self.inner.theme.lock());
            let session = Arc::new(Session {
                id: terminal_id.to_owned(),
                cwd: cwd.to_owned(),
                screen,
                io: Mutex::new(None),
                state: Mutex::new(State::default()),
                wake: Condvar::new(),
                closed: AtomicBool::new(false),
                host: Arc::downgrade(&self.inner),
            });
            sessions.insert(terminal_id.to_owned(), session.clone());
            session
        };
        // A shell cannot start in a folder that is gone: it exits 1, as a child failing its chdir does.
        if !std::path::Path::new(cwd).is_dir() {
            let mut update = TerminalUpdate::new(terminal_id);
            update.status = Some(TerminalStatus::Exited);
            update.exit_code = Some(1);
            (self.inner.on_update)(update);
            return;
        }
        if let Err(error) = spawn(&session) {
            let mut update = TerminalUpdate::new(terminal_id);
            update.status = Some(TerminalStatus::Exited);
            update.error = Some(format!("{error:#}"));
            (self.inner.on_update)(update);
        }
    }

    /// Keystrokes on their way to the shell.
    pub fn write(&self, terminal_id: &str, data: &str) -> Result<(), TerminalError> {
        if utf16_len(data) > MAX_TERMINAL_INPUT {
            return Err(TerminalError::InvalidInput);
        }
        if let Some(session) = self.session(terminal_id) {
            session.write(data.as_bytes());
        }
        Ok(())
    }

    pub fn resize(&self, terminal_id: &str, cols: u32, rows: u32) -> Result<(), TerminalError> {
        if !is_terminal_dimension(cols) || !is_terminal_dimension(rows) {
            return Err(TerminalError::InvalidSize);
        }
        let Some(session) = self.session(terminal_id) else { return Ok(()) };
        let (cols, rows) = (cols as u16, rows as u16);
        if session.screen.size() == (cols, rows) {
            return Ok(());
        }
        session.flush();
        let mut state = session.state.lock();
        session.screen.resize(cols, rows);
        if let Some(io) = session.io.lock().as_ref() {
            let _ = io.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        }
        state.sequence += 1;
        if let Some(output) = state.output.as_mut() {
            output.reset();
        }
        session.wake.notify_all();
        Ok(())
    }

    /// Kills the shell. Its screen stays with any view still holding it.
    pub fn close(&self, terminal_id: &str) {
        let Some(session) = self.inner.sessions.lock().remove(terminal_id) else { return };
        session.close();
    }

    /// Every shell goes with the window, so a closed window leaves no process running.
    pub fn stop_all(&self) {
        let sessions: Vec<_> = self.inner.sessions.lock().drain().map(|(_, session)| session).collect();
        for session in sessions {
            session.close();
        }
    }

    /// The screen a view paints and reads, shared with the shell writing to it.
    pub fn screen(&self, terminal_id: &str) -> Option<Arc<Screen>> {
        self.session(terminal_id).map(|session| session.screen.clone())
    }

    /// The colours every shell answers colour queries with.
    pub fn set_theme(&self, theme: &TerminalTheme) {
        *self.inner.theme.lock() = theme.clone();
        for session in self.inner.sessions.lock().values() {
            session.screen.set_theme(theme);
        }
    }

    /// Reads a terminal as plain text. Pending output is flushed first, so a read taken straight
    /// after a command shows what that command printed. `None` when that terminal is gone.
    pub fn read(&self, terminal_id: &str, options: &TerminalReadOptions) -> Option<TerminalText> {
        let session = self.session(terminal_id)?;
        session.flush();
        let limit = options.lines.clamp(1, MAX_TERMINAL_LINES);
        let filter = options.matching.as_deref().map(|text| text.trim().to_lowercase()).filter(|text| !text.is_empty());
        let Some(filter) = filter else {
            let (lines, total) = session.screen.tail(limit);
            let kept = within_read_budget(lines);
            return Some(TerminalText { omitted: total - kept.len(), lines: kept, matched: None });
        };
        let filtered: Vec<String> =
            session.screen.lines().into_iter().filter(|line| line.to_lowercase().contains(&filter)).collect();
        let matched = filtered.len();
        let kept = within_read_budget(filtered[matched.saturating_sub(limit)..].to_vec());
        Some(TerminalText { omitted: matched - kept.len(), lines: kept, matched: Some(matched) })
    }

    /// The resolved screen and the last flush it includes.
    pub fn snapshot(&self, terminal_id: &str) -> Option<TerminalScreenSnapshot> {
        let session = self.session(terminal_id)?;
        session.flush();
        Some(session.snapshot())
    }

    /// Only new output crosses a paired connection; missed output is recovered from the screen.
    /// A reader that is already current is held for up to a second, so this blocks: call it off
    /// the UI thread.
    pub fn read_output(&self, terminal_id: &str, after: Option<u64>) -> Result<Option<TerminalOutputRead>, TerminalError> {
        let Some(session) = self.session(terminal_id) else { return Ok(None) };
        let mut state = session.state.lock();
        state.output.get_or_insert_with(OutputBuffer::default);
        if after == Some(state.sequence) {
            let deadline = Instant::now() + OUTPUT_WAIT;
            while state.sequence == after.unwrap_or_default() && !session.closed.load(Ordering::Acquire) {
                if session.wake.wait_until(&mut state, deadline).timed_out() {
                    break;
                }
            }
        }
        if session.closed.load(Ordering::Acquire) {
            return Ok(None);
        }
        if let Some(after) = after {
            if let Some(data) = state.output.as_ref().and_then(|output| output.read(after, state.sequence)) {
                let (cols, rows) = session.screen.size();
                return Ok(Some(TerminalOutputRead { kind: TerminalReadKind::Output, data, sequence: state.sequence, cols, rows }));
            }
        }
        drop(state);
        session.flush();
        Ok(Some(TerminalOutputRead::snapshot(session.snapshot())))
    }

    pub fn terminal_ids(&self) -> Vec<String> {
        self.inner.sessions.lock().keys().cloned().collect()
    }

    fn session(&self, terminal_id: &str) -> Option<Arc<Session>> {
        self.inner.sessions.lock().get(terminal_id).cloned()
    }
}

/// The user's own login shell, so a terminal here has the environment their terminal app would.
fn shell_command(cwd: &str) -> CommandBuilder {
    #[cfg(windows)]
    let mut command = CommandBuilder::new(std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into()));
    #[cfg(not(windows))]
    let mut command = {
        let shell = std::env::var("SHELL").ok().filter(|shell| !shell.is_empty()).unwrap_or_else(|| "/bin/bash".into());
        let mut command = CommandBuilder::new(shell);
        command.arg("-l");
        command
    };
    command.cwd(cwd);
    command.env_remove("ELECTRON_RUN_AS_NODE");
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command
}

fn spawn(session: &Arc<Session>) -> anyhow::Result<()> {
    let pair = native_pty_system().openpty(PtySize { rows: DEFAULT_ROWS, cols: DEFAULT_COLS, pixel_width: 0, pixel_height: 0 })?;
    let mut child = pair.slave.spawn_command(shell_command(&session.cwd))?;
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;
    let killer = child.clone_killer();
    *session.io.lock() = Some(Io { master: pair.master, writer, killer });

    let reading = session.clone();
    thread::Builder::new().name(format!("terminal-read-{}", session.id)).spawn(move || {
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => reading.push(&buffer[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        reading.state.lock().eof = true;
        reading.wake.notify_all();
    })?;

    let flushing = session.clone();
    thread::Builder::new().name(format!("terminal-flush-{}", session.id)).spawn(move || flushing.run_flusher())?;

    let waiting = session.clone();
    thread::Builder::new().name(format!("terminal-wait-{}", session.id)).spawn(move || {
        let status = child.wait();
        {
            let mut state = waiting.state.lock();
            let deadline = Instant::now() + EXIT_DRAIN;
            while !state.eof && !waiting.closed.load(Ordering::Acquire) {
                if waiting.wake.wait_until(&mut state, deadline).timed_out() {
                    break;
                }
            }
        }
        if waiting.closed.load(Ordering::Acquire) {
            return;
        }
        waiting.flush();
        waiting.io.lock().take();
        // A shell ended by a signal reports no code of its own, which reads as 0.
        let exit_code = match &status {
            Ok(status) if status.signal().is_none() => status.exit_code() as i32,
            _ => 0,
        };
        let mut update = TerminalUpdate::new(&waiting.id);
        update.status = Some(TerminalStatus::Exited);
        update.exit_code = Some(exit_code);
        waiting.publish(update);
    })?;
    Ok(())
}

impl Session {
    fn push(&self, chunk: &[u8]) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let mut state = self.state.lock();
        state.pending.extend_from_slice(chunk);
        if !state.scheduled {
            state.scheduled = true;
            self.wake.notify_all();
        }
    }

    fn run_flusher(&self) {
        loop {
            {
                let mut state = self.state.lock();
                loop {
                    if self.closed.load(Ordering::Acquire) {
                        return;
                    }
                    if state.scheduled {
                        break;
                    }
                    match self.screen.sync_deadline() {
                        Some(deadline) => {
                            if self.wake.wait_until(&mut state, deadline).timed_out() {
                                drop(state);
                                let events = self.screen.finish_overdue_sync();
                                self.handle(events);
                                state = self.state.lock();
                            }
                        }
                        None => self.wake.wait(&mut state),
                    }
                }
            }
            thread::sleep(FLUSH_INTERVAL);
            self.flush();
        }
    }

    /// Parses everything pending into the screen as one write.
    fn flush(&self) {
        let events = {
            let mut state = self.state.lock();
            state.scheduled = false;
            if state.pending.is_empty() {
                return;
            }
            let mut bytes = std::mem::take(&mut state.carry);
            bytes.append(&mut state.pending);
            let pending = decode(bytes, &mut state.carry);
            if pending.is_empty() {
                return;
            }
            let data = trim_flush(pending);
            let events = self.screen.feed(data.as_bytes());
            state.sequence += 1;
            let sequence = state.sequence;
            if let Some(output) = state.output.as_mut() {
                output.push(sequence, data);
            }
            self.wake.notify_all();
            events
        };
        self.handle(events);
    }

    fn handle(&self, events: Vec<ScreenEvent>) {
        for event in events {
            match event {
                ScreenEvent::Reply(reply) => self.write(reply.as_bytes()),
                ScreenEvent::Title(title) => {
                    let mut update = TerminalUpdate::new(&self.id);
                    update.title = Some(title.filter(|title| !title.is_empty()).unwrap_or_else(|| terminal_title(&self.cwd)));
                    self.publish(update);
                }
            }
        }
    }

    fn write(&self, data: &[u8]) {
        if let Some(io) = self.io.lock().as_mut() {
            let _ = io.writer.write_all(data).and_then(|_| io.writer.flush());
        }
    }

    fn snapshot(&self) -> TerminalScreenSnapshot {
        let state = self.state.lock();
        let term = self.screen.lock();
        let data = serialize_terminal(&term);
        use alacritty_terminal::grid::Dimensions;
        TerminalScreenSnapshot { data, cols: term.columns() as u16, rows: term.screen_lines() as u16, sequence: state.sequence }
    }

    fn publish(&self, update: TerminalUpdate) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        if let Some(host) = self.host.upgrade() {
            (host.on_update)(update);
        }
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        {
            let _state = self.state.lock();
            self.wake.notify_all();
        }
        if let Some(mut io) = self.io.lock().take() {
            let _ = io.killer.kill();
        }
    }
}

/// Decodes complete UTF-8, keeping a split sequence at the end for the next read. Invalid bytes
/// become U+FFFD, as a streaming decoder would make them.
fn decode(bytes: Vec<u8>, carry: &mut Vec<u8>) -> String {
    let mut end = bytes.len();
    // A split sequence is at most three bytes long and starts with a lead byte.
    for back in 1..=3.min(bytes.len()) {
        let byte = bytes[bytes.len() - back];
        if byte & 0b1100_0000 == 0b1000_0000 {
            continue;
        }
        let width = match byte {
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            _ => 0,
        };
        if width > back {
            end = bytes.len() - back;
        }
        break;
    }
    carry.extend_from_slice(&bytes[end..]);
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Drops the front of a flush past the limit, saying how much went.
fn trim_flush(pending: String) -> String {
    let length = utf16_len(&pending);
    if length <= MAX_FLUSH_BYTES {
        return pending;
    }
    let dropped = length - MAX_FLUSH_BYTES;
    let mut units = 0;
    let mut cut = pending.len();
    for (index, ch) in pending.char_indices() {
        if units >= dropped {
            cut = index;
            break;
        }
        units += ch.len_utf16();
    }
    let kilobytes = (dropped as f64 / 1024.).round() as u64;
    format!("\r\n… {} KB of output dropped\r\n{}", group_thousands(kilobytes), &pending[cut..])
}

fn group_thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_utf8_waits_for_its_tail() {
        let mut carry = Vec::new();
        let bytes = "a\u{00e9}".as_bytes();
        assert_eq!(decode(bytes[..2].to_vec(), &mut carry), "a");
        let mut next = std::mem::take(&mut carry);
        next.extend_from_slice(&bytes[2..]);
        assert_eq!(decode(next, &mut carry), "\u{00e9}");
        assert!(carry.is_empty());
    }

    #[test]
    fn an_oversized_flush_keeps_its_tail() {
        let data = trim_flush("x".repeat(MAX_FLUSH_BYTES + 2048));
        assert!(data.starts_with("\r\n… 2 KB of output dropped\r\n"));
        assert!(data.ends_with(&"x".repeat(MAX_FLUSH_BYTES)));
        assert_eq!(group_thousands(1234567), "1,234,567");
    }

    #[test]
    fn a_missing_folder_exits_without_a_shell() {
        let updates = Arc::new(Mutex::new(Vec::new()));
        let seen = updates.clone();
        let host = TerminalHost::new(move |update| seen.lock().push(update));
        host.start("t1", "/nonexistent/act-terminal-missing");
        assert!(updates.lock().iter().any(|update| update.status == Some(TerminalStatus::Exited) && update.exit_code == Some(1)));
    }

    #[test]
    fn a_shell_runs_and_reads_back_what_it_printed() {
        let updates = Arc::new(Mutex::new(Vec::new()));
        let seen = updates.clone();
        let host = TerminalHost::new(move |update| seen.lock().push(update));
        host.start("t1", "/tmp");
        host.write("t1", "printf 'act-%s\\n' ready\r").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let options = TerminalReadOptions { lines: 50, matching: Some("ACT-READY".into()) };
        loop {
            let text = host.read("t1", &options).unwrap();
            if text.matched == Some(1) {
                assert_eq!(text.lines, vec!["act-ready"]);
                break;
            }
            assert!(Instant::now() < deadline, "shell never printed");
            thread::sleep(Duration::from_millis(50));
        }
        host.write("t1", "exit 3\r").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !updates.lock().iter().any(|update| update.status == Some(TerminalStatus::Exited)) {
            assert!(Instant::now() < deadline, "shell never exited");
            thread::sleep(Duration::from_millis(50));
        }
        assert!(updates.lock().iter().any(|update| update.exit_code == Some(3)));
        host.close("t1");
        assert!(host.read("t1", &options).is_none());
    }
}
