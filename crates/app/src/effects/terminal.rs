//! The terminal panel's shells. Their output goes straight to the view; only what they did lands here.

use act_terminal::TerminalHost;

use super::Sink;

pub fn start(terminals: &TerminalHost, terminal_id: &str, cwd: &str) {
    terminals.start(terminal_id, cwd);
}

pub fn write(terminals: &TerminalHost, sink: &Sink, terminal_id: &str, data: &str) {
    if let Err(error) = terminals.write(terminal_id, data) {
        sink.failed(error.to_string());
    }
}

pub fn resize(terminals: &TerminalHost, terminal_id: &str, cols: i64, rows: i64) {
    let (Ok(cols), Ok(rows)) = (u32::try_from(cols), u32::try_from(rows)) else { return };
    let _ = terminals.resize(terminal_id, cols, rows);
}
