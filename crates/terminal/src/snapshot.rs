//! Rebuilds a screen as escape sequences, so a viewer that missed output can start from the
//! resolved cells rather than from a replay.

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::Column;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::{Color, NamedColor};
use alacritty_terminal::Term;

use crate::contract::utf16_len;
use crate::screen::{row_line, row_wraps};

const MAX_SNAPSHOT_SIZE: usize = 2 * 1024 * 1024;

fn colour_codes(codes: &mut Vec<String>, colour: Color, foreground: bool) {
    let extended: usize = if foreground { 38 } else { 48 };
    match colour {
        Color::Spec(rgb) => codes.extend([extended.to_string(), "2".into(), rgb.r.to_string(), rgb.g.to_string(), rgb.b.to_string()]),
        Color::Indexed(index) => codes.extend([extended.to_string(), "5".into(), index.to_string()]),
        // Named colours keep their 16-colour form, which bold brightens and 256-colour slots do not.
        Color::Named(named) if (named as usize) < 8 => codes.push((extended - 8 + named as usize).to_string()),
        Color::Named(named) if (named as usize) < 16 => codes.push((extended + 52 + named as usize - 8).to_string()),
        Color::Named(_) => {}
    }
}

fn style(cell: &Cell) -> String {
    let mut codes = vec!["0".to_string()];
    let flags = cell.flags;
    if flags.contains(Flags::BOLD) {
        codes.push("1".into());
    }
    if flags.contains(Flags::DIM) {
        codes.push("2".into());
    }
    if flags.contains(Flags::ITALIC) {
        codes.push("3".into());
    }
    if flags.intersects(Flags::ALL_UNDERLINES) {
        codes.push("4".into());
    }
    if flags.contains(Flags::INVERSE) {
        codes.push("7".into());
    }
    if flags.contains(Flags::HIDDEN) {
        codes.push("8".into());
    }
    if flags.contains(Flags::STRIKEOUT) {
        codes.push("9".into());
    }
    colour_codes(&mut codes, cell.fg, true);
    colour_codes(&mut codes, cell.bg, false);
    format!("\x1b[{}m", codes.join(";"))
}

const STYLE_FLAGS: Flags = Flags::BOLD
    .union(Flags::DIM)
    .union(Flags::ITALIC)
    .union(Flags::ALL_UNDERLINES)
    .union(Flags::INVERSE)
    .union(Flags::HIDDEN)
    .union(Flags::STRIKEOUT);

fn is_blank(cell: &Cell) -> bool {
    (cell.c == ' ' || cell.c == '\0')
        && cell.fg == Color::Named(NamedColor::Foreground)
        && cell.bg == Color::Named(NamedColor::Background)
        && !cell.flags.intersects(STYLE_FLAGS | Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER)
        && cell.zerowidth().is_none()
}

fn cell_text(cell: &Cell, out: &mut String) {
    if cell.c == '\t' || cell.c == '\0' || cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
        out.push(' ');
        return;
    }
    out.push(cell.c);
    if let Some(extra) = cell.zerowidth() {
        out.extend(extra.iter());
    }
}

fn cursor<T>(term: &Term<T>) -> String {
    let grid = term.grid();
    let point = grid.cursor.point;
    let cols = grid.columns();
    if !grid.cursor.input_needs_wrap {
        return format!("\x1b[{};{}H", point.line.0 + 1, point.column.0 + 1);
    }
    // A pending wrap is reproduced by printing the last cell again from the column before it.
    let line = &grid[point.line];
    let mut col = cols - 1;
    if col > 0 && line[Column(col)].flags.contains(Flags::WIDE_CHAR_SPACER) {
        col -= 1;
    }
    let cell = &line[Column(col)];
    let mut text = String::new();
    cell_text(cell, &mut text);
    format!("\x1b[{};{}H{}{}\x1b[0m", point.line.0 + 1, col + 1, style(cell), text)
}

/// The active buffer's cells and scrollback, keeping the newest rows within `budget`.
fn serialize_buffer<T>(term: &Term<T>, budget: usize) -> String {
    let grid = term.grid();
    let cols = grid.columns();
    let total = grid.total_lines();
    let mut lines: Vec<(String, bool)> = Vec::new();
    let mut size = 0;
    for row in (0..total).rev() {
        let line = &grid[row_line(term, row)];
        let mut width = cols;
        if !row_wraps(term, row) {
            while width > 0 && is_blank(&line[Column(width - 1)]) {
                width -= 1;
            }
        }
        let mut data = String::new();
        let mut previous: Option<(Color, Color, Flags)> = None;
        let mut previous_style = String::new();
        for col in 0..width {
            let cell = &line[Column(col)];
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let key = (cell.fg, cell.bg, cell.flags & STYLE_FLAGS);
            if previous != Some(key) {
                let attributes = style(cell);
                if attributes != previous_style {
                    data.push_str(&attributes);
                }
                previous_style = attributes;
                previous = Some(key);
            }
            cell_text(cell, &mut data);
        }
        let length = utf16_len(&data);
        if size + length > budget {
            break;
        }
        let wrapped = row > 0 && row_wraps(term, row - 1);
        lines.push((data, wrapped));
        size += length + 2;
    }
    lines.reverse();
    let mut result = String::new();
    for (index, (data, wrapped)) in lines.iter().enumerate() {
        if index > 0 && !wrapped {
            result.push_str("\r\n");
        }
        result.push_str(data);
    }
    result.push_str("\x1b[0m");
    result.push_str(&cursor(term));
    result
}

/// A reload restores the screen and its modes before live output resumes at its sequence watermark.
///
/// alacritty keeps the inactive buffer private, so while a program holds the alternate screen only
/// that screen is restored; the primary buffer comes back empty.
pub fn serialize_terminal<T>(term: &Term<T>) -> String {
    let mode = *term.mode();
    let alternate = mode.contains(TermMode::ALT_SCREEN);
    let mut data = String::from("\x1bc");
    if alternate {
        data.push_str("\x1b[0m\x1b[1;1H\x1b[?1049h\x1b[H");
        data.push_str(&serialize_buffer(term, MAX_SNAPSHOT_SIZE / 2));
    } else {
        data.push_str(&serialize_buffer(term, MAX_SNAPSHOT_SIZE));
    }
    let modes = [
        (1, mode.contains(TermMode::APP_CURSOR)),
        (6, mode.contains(TermMode::ORIGIN)),
        (7, mode.contains(TermMode::LINE_WRAP)),
        (45, false),
        (66, mode.contains(TermMode::APP_KEYPAD)),
        (1004, mode.contains(TermMode::FOCUS_IN_OUT)),
        (2004, mode.contains(TermMode::BRACKETED_PASTE)),
    ];
    for (code, enabled) in modes {
        data.push_str(&format!("\x1b[?{code}{}", if enabled { 'h' } else { 'l' }));
    }
    data.push_str(if mode.contains(TermMode::INSERT) { "\x1b[4h" } else { "\x1b[4l" });
    let mouse = if mode.contains(TermMode::MOUSE_MOTION) {
        Some(1003)
    } else if mode.contains(TermMode::MOUSE_DRAG) {
        Some(1002)
    } else if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
        Some(1000)
    } else {
        None
    };
    if let Some(mouse) = mouse {
        data.push_str(&format!("\x1b[?{mouse}h"));
        if mode.contains(TermMode::SGR_MOUSE) {
            data.push_str("\x1b[?1006h");
        }
    }
    data.push_str(&cursor(term));
    data
}

#[cfg(test)]
mod tests {
    use crate::screen::Screen;

    use super::*;

    #[test]
    fn a_snapshot_restores_the_same_screen() {
        let source = Screen::new(10, 4);
        source.feed(b"\x1b[1;31mred\x1b[0m plain\r\n0123456789wrapped\r\n\x1b[?2004h> ");
        let data = serialize_terminal(&*source.lock());
        let copy = Screen::new(10, 4);
        copy.write(&data);
        assert_eq!(copy.lines(), source.lines());
        let (a, b) = (source.lock(), copy.lock());
        assert_eq!(a.grid().cursor.point, b.grid().cursor.point);
        assert!(b.mode().contains(TermMode::BRACKETED_PASTE));
        let red = |term: &Term<_>| term.grid()[row_line(term, 0)][Column(0)].fg;
        assert_eq!(red(&a), red(&b));
    }
}
