//! One line of text cut to a width ahead of layout, for rows whose width GPUI's own truncation
//! measures before the row has settled.

use gpui::{px, SharedString, TextRun, Window};

/// How wide a line of text draws in the window's current text style.
pub fn line_width(text: &str, size: f32, window: &Window) -> f32 {
    if text.is_empty() {
        return 0.0;
    }
    let style = window.text_style();
    let run = TextRun { len: text.len(), font: style.font(), color: style.color, background_color: None, underline: None, strikethrough: None };
    f32::from(window.text_system().shape_line(SharedString::from(text.to_string()), px(size), &[run], None).width)
}

/// The first line of `text`, ended with an ellipsis where it runs past `width`.
pub fn fit_line(text: &str, width: f32, size: f32, window: &Window) -> String {
    let line = text.lines().next().unwrap_or("");
    if width <= 0.0 || line_width(line, size, window) <= width {
        return line.to_string();
    }
    let chars: Vec<char> = line.chars().collect();
    let (mut low, mut high) = (0usize, chars.len());
    while low < high {
        let mid = (low + high).div_ceil(2);
        let candidate: String = chars[..mid].iter().collect::<String>().trim_end().to_string() + "…";
        if line_width(&candidate, size, window) <= width {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    chars[..low].iter().collect::<String>().trim_end().to_string() + "…"
}
