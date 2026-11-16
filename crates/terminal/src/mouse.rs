//! Mouse tracking reports, following xterm.js's `CoreMouseService`.

use alacritty_terminal::term::TermMode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportButton {
    Left = 0,
    Middle = 1,
    Right = 2,
    None = 3,
    Wheel = 4,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportAction {
    /// A release, or a wheel turned up.
    Up = 0,
    /// A press, or a wheel turned down.
    Down = 1,
    Move = 32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseReport {
    /// Zero-based cell.
    pub col: usize,
    pub row: usize,
    pub button: ReportButton,
    pub action: ReportAction,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// Whether the program asked for mouse events at all.
pub fn tracking(mode: TermMode) -> bool {
    mode.intersects(TermMode::MOUSE_MODE)
}

/// Whether the active protocol reports motion with no button held.
pub fn reports_hover(mode: TermMode) -> bool {
    mode.contains(TermMode::MOUSE_MOTION)
}

fn event_code(report: &MouseReport, sgr: bool) -> u32 {
    let mut code = (report.ctrl as u32) << 4 | (report.shift as u32) << 2 | (report.alt as u32) << 3;
    if report.button == ReportButton::Wheel {
        code |= 64 | report.action as u32;
    } else {
        code |= report.button as u32 & 3;
        if report.action == ReportAction::Move {
            code |= 32;
        } else if report.action == ReportAction::Up && !sgr {
            code |= ReportButton::None as u32;
        }
    }
    code
}

/// The bytes for a report under the program's protocol, or `None` when the protocol leaves it out.
pub fn encode(mut report: MouseReport, mode: TermMode, cols: usize, rows: usize) -> Option<String> {
    if report.col >= cols || report.row >= rows {
        return None;
    }
    if report.button == ReportButton::Wheel && report.action == ReportAction::Move {
        return None;
    }
    if report.button == ReportButton::None && report.action != ReportAction::Move {
        return None;
    }
    let allowed = if mode.contains(TermMode::MOUSE_MOTION) {
        true
    } else if mode.contains(TermMode::MOUSE_DRAG) {
        !(report.action == ReportAction::Move && report.button == ReportButton::None)
    } else if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
        report.action != ReportAction::Move
    } else {
        false
    };
    if !allowed {
        return None;
    }
    report.col += 1;
    report.row += 1;
    if mode.contains(TermMode::SGR_MOUSE) {
        let last = if report.action == ReportAction::Up && report.button != ReportButton::Wheel { 'm' } else { 'M' };
        return Some(format!("\x1b[<{};{};{}{last}", event_code(&report, true), report.col, report.row));
    }
    let params = [event_code(&report, false) + 32, report.col as u32 + 32, report.row as u32 + 32];
    // Past what one byte can address, the report is suppressed.
    if params.iter().any(|value| *value > 255) {
        return None;
    }
    Some(params.iter().map(|value| char::from_u32(*value).unwrap_or(' ')).fold(String::from("\x1b[M"), |mut out, ch| {
        out.push(ch);
        out
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(button: ReportButton, action: ReportAction) -> MouseReport {
        MouseReport { col: 4, row: 2, button, action, ctrl: false, alt: false, shift: false }
    }

    #[test]
    fn sgr_reports_carry_the_button_on_release() {
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(encode(report(ReportButton::Left, ReportAction::Down), mode, 80, 24).as_deref(), Some("\x1b[<0;5;3M"));
        assert_eq!(encode(report(ReportButton::Left, ReportAction::Up), mode, 80, 24).as_deref(), Some("\x1b[<0;5;3m"));
        assert_eq!(encode(report(ReportButton::Wheel, ReportAction::Up), mode, 80, 24).as_deref(), Some("\x1b[<64;5;3M"));
        assert_eq!(encode(report(ReportButton::Left, ReportAction::Move), mode, 80, 24), None);
    }

    #[test]
    fn default_encoding_reports_release_as_no_button() {
        let mode = TermMode::MOUSE_DRAG;
        assert_eq!(encode(report(ReportButton::Left, ReportAction::Up), mode, 80, 24).as_deref(), Some("\x1b[M#%#"));
        assert_eq!(encode(report(ReportButton::Left, ReportAction::Move), mode, 80, 24).as_deref(), Some("\x1b[M@%#"));
        assert_eq!(encode(report(ReportButton::None, ReportAction::Move), mode, 80, 24), None);
    }
}
