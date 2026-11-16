//! The colours and face a terminal draws in. The app fills these from its theme tokens, the way
//! the Electron view read them off the stylesheet.

use alacritty_terminal::vte::ansi::Rgb;
use gpui::{px, rgb, Hsla, Pixels, Rgba, SharedString};

/// Colours read from the theme tokens. Cells with the default background stay clear, so
/// `background` (the viewport's fill) shows behind them.
#[derive(Clone, Debug, PartialEq)]
pub struct TerminalTheme {
    /// `--code-surface`: the viewport's fill.
    pub background: Hsla,
    /// `--code-ink`.
    pub foreground: Hsla,
    /// `--ink`.
    pub cursor: Hsla,
    /// `--code-surface`: the glyph under a block cursor.
    pub cursor_accent: Hsla,
    /// `--terminal-selection`.
    pub selection: Hsla,
    /// `--find-match`: every match find is showing.
    pub find_match: Hsla,
    /// `--find-active`: the match being read.
    pub find_active: Hsla,
    /// `--ansi-*`, black through bright white.
    pub ansi: [Hsla; 16],
}

impl Default for TerminalTheme {
    /// AI Coding Tool Dark, the default theme.
    fn default() -> Self {
        Self {
            background: rgb(0x1b2028).into(),
            foreground: rgb(0xc7d2dd).into(),
            cursor: rgb(0xe6e4df).into(),
            cursor_accent: rgb(0x1b2028).into(),
            // color-mix(info 25%, bg-5)
            selection: rgb(0x344255).into(),
            // color-mix(ansi-yellow 25%, bg-0)
            find_match: rgb(0x413729).into(),
            find_active: rgb(0xd97757).into(),
            ansi: [
                rgb(0x2a2f36).into(),
                rgb(0xe06c75).into(),
                rgb(0x72c66b).into(),
                rgb(0xd9a760).into(),
                rgb(0x6f9ed6).into(),
                rgb(0xc58fd6).into(),
                rgb(0x63bfba).into(),
                rgb(0xc7d2dd).into(),
                rgb(0x565c65).into(),
                rgb(0xef8b93).into(),
                rgb(0x97d891).into(),
                rgb(0xe8c088).into(),
                rgb(0x93bbe8).into(),
                rgb(0xd9aee7).into(),
                rgb(0x8ad6d1).into(),
                rgb(0xe6e4df).into(),
            ],
        }
    }
}

impl TerminalTheme {
    /// The colour of a 256-colour palette slot: the theme's sixteen, then xterm's cube and greys.
    pub fn indexed(&self, index: u8) -> Hsla {
        if index < 16 {
            return self.ansi[index as usize];
        }
        let Rgb { r, g, b } = xterm_256(index);
        Rgba { r: r as f32 / 255., g: g as f32 / 255., b: b as f32 / 255., a: 1. }.into()
    }

    /// The colour a program asking with OSC 4/10/11/12 is told, as opaque RGB.
    pub(crate) fn query_rgb(&self, index: usize) -> Option<Rgb> {
        let colour = match index {
            0..=255 => self.indexed(index as u8),
            256 => self.foreground,
            257 => self.background,
            258 => self.cursor,
            _ => return None,
        };
        Some(to_rgb(colour))
    }
}

pub(crate) fn to_rgb(colour: Hsla) -> Rgb {
    let rgba = colour.to_rgb();
    let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u8;
    Rgb { r: channel(rgba.r), g: channel(rgba.g), b: channel(rgba.b) }
}

pub(crate) fn from_rgb(colour: Rgb) -> Hsla {
    Rgba { r: colour.r as f32 / 255., g: colour.g as f32 / 255., b: colour.b as f32 / 255., a: 1. }.into()
}

/// xterm's 6x6x6 cube (16-231) and grey ramp (232-255).
pub(crate) fn xterm_256(index: u8) -> Rgb {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match index {
        16..=231 => {
            let i = index - 16;
            Rgb { r: LEVELS[(i / 36) as usize], g: LEVELS[(i / 6 % 6) as usize], b: LEVELS[(i % 6) as usize] }
        }
        232..=255 => {
            let v = 8 + (index - 232) * 10;
            Rgb { r: v, g: v, b: v }
        }
        _ => Rgb { r: 0, g: 0, b: 0 },
    }
}

/// The face and size the terminal draws in: `--mono` and `--terminal-text`.
#[derive(Clone, Debug, PartialEq)]
pub struct TerminalFont {
    pub family: SharedString,
    pub size: Pixels,
}

impl Default for TerminalFont {
    fn default() -> Self {
        Self { family: "Fira Code".into(), size: px(12.) }
    }
}
