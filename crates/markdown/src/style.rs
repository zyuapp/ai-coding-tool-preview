//! Colours and fonts a message is drawn with. Spacing is not here: it is `markdown.css`, fixed in
//! [`crate::metrics`].

use gpui::{px, rgb, rgba, Hsla, Pixels, SharedString};

use crate::highlight::SyntaxRole;

/// The `--syntax-*` colours.
#[derive(Clone, Debug, PartialEq)]
pub struct SyntaxColors {
    pub comment: Hsla,
    pub string: Hsla,
    pub number: Hsla,
    pub constant: Hsla,
    pub keyword: Hsla,
    pub operator: Hsla,
    pub function: Hsla,
    pub type_name: Hsla,
    pub variable: Hsla,
    pub tag: Hsla,
    pub attribute: Hsla,
    pub punctuation: Hsla,
}

impl SyntaxColors {
    pub fn color(&self, role: SyntaxRole, plain: Hsla) -> Hsla {
        match role {
            SyntaxRole::Plain => plain,
            SyntaxRole::Comment => self.comment,
            SyntaxRole::String => self.string,
            SyntaxRole::Number => self.number,
            SyntaxRole::Constant => self.constant,
            SyntaxRole::Keyword => self.keyword,
            SyntaxRole::Operator => self.operator,
            SyntaxRole::Function => self.function,
            SyntaxRole::Type => self.type_name,
            SyntaxRole::Variable => self.variable,
            SyntaxRole::Tag => self.tag,
            SyntaxRole::Attribute => self.attribute,
            SyntaxRole::Punctuation => self.punctuation,
        }
    }
}

/// The glass icon button a block's copy action sits in.
#[derive(Clone, Debug, PartialEq)]
pub struct CopyButtonColors {
    /// `--glass-clear`.
    pub fill: Hsla,
    /// `--line-glass`.
    pub border: Hsla,
    /// `--line-bright`: the edge under the pointer.
    pub border_hover: Hsla,
    /// `--muted`.
    pub glyph: Hsla,
    /// `--ink-dim`: the glyph while its block is under the pointer.
    pub glyph_block_hover: Hsla,
    /// `--ink-soft`: the glyph under the pointer.
    pub glyph_hover: Hsla,
    /// `--success-line` and `--success-soft`.
    pub copied_border: Hsla,
    pub copied: Hsla,
    /// `--danger` and `--danger-soft`.
    pub failed_border: Hsla,
    pub failed: Hsla,
    /// Asset paths of Lucide `copy`, `check` and `x`. `None` draws the glyphs with paths.
    pub copy_glyph: Option<SharedString>,
    pub check_glyph: Option<SharedString>,
    pub x_glyph: Option<SharedString>,
}

/// Everything theme- or setting-dependent about a rendered message. Field comments name the CSS
/// token each one stands for.
#[derive(Clone, Debug, PartialEq)]
pub struct MarkdownStyle {
    /// `--ui-font`.
    pub ui_font: SharedString,
    /// `--mono`.
    pub mono_font: SharedString,
    /// The message's font size: `--text-content` (15px) for answers.
    pub font_size: Pixels,
    /// Line height as a multiple of the font size: 1.7 for answers, 1.5 in a user bubble.
    pub line_height: f32,

    /// The message colour: `--ink-soft` for answers, `--muted-strong` for work notes.
    pub text: Hsla,
    /// `--ink`: headings and strong text.
    pub ink: Hsla,
    /// `--muted-strong`: quotes and table headings.
    pub muted_strong: Hsla,
    /// `--quiet`: a code block's language strip.
    pub quiet: Hsla,
    /// `--accent-text`: links and the thread glyph.
    pub accent_text: Hsla,
    /// `--accent-soft`: a link under the pointer.
    pub accent_soft: Hsla,
    /// `--link`: a commit hash that opens.
    pub link: Hsla,
    /// `--link-hover`.
    pub link_hover: Hsla,
    /// `--link-line`: a commit hash's underline.
    pub link_line: Hsla,
    /// `--code-ink`.
    pub code_ink: Hsla,
    /// `color-mix(in srgb, var(--code-surface) 70%, var(--canvas))`: inline code.
    pub inline_code_background: Hsla,
    /// `--surface`: code blocks and table headings.
    pub surface: Hsla,
    /// `--line`.
    pub line: Hsla,
    /// `--line-soft`.
    pub line_soft: Hsla,
    /// `--line-strong`: a quote's rule.
    pub line_strong: Hsla,
    /// A task box as Chromium draws a disabled checkbox: its fill and edge, and when checked, its
    /// fill and tick. The app's `accent-color` does not reach a disabled box.
    pub checkbox: Hsla,
    pub checkbox_border: Hsla,
    pub checkbox_checked: Hsla,
    pub checkbox_tick: Hsla,
    /// The selection highlight while the window is active.
    pub selection: Hsla,
    /// The selection highlight while the window is in the background.
    pub selection_inactive: Hsla,
    pub syntax: SyntaxColors,
    /// The built-in copy button, drawn when the host supplies none of its own.
    pub copy_button: CopyButtonColors,
    /// Colours fenced code by its language. The Electron app draws message code in `--code-ink` only.
    pub highlight_code_blocks: bool,
    /// Asset path of the glyph before a file link (Lucide `file-text`). `None` draws no glyph.
    pub file_glyph: Option<SharedString>,
    /// Asset path of the glyph before a thread link (Lucide `message-square-text`).
    pub thread_glyph: Option<SharedString>,
}

fn color(hex: u32) -> Hsla {
    rgb(hex).into()
}

fn with_alpha(color: Hsla, alpha: f32) -> Hsla {
    Hsla { a: alpha, ..color }
}

impl MarkdownStyle {
    /// `aicodingtool-dark`, the default theme.
    pub fn dark() -> Self {
        let accent = color(0xd97757);
        Self {
            ui_font: ".SystemUIFont".into(),
            mono_font: "Menlo".into(),
            font_size: px(15.),
            line_height: 1.7,
            text: color(0xd9d8d4),
            ink: color(0xe6e4df),
            muted_strong: color(0xb9bab9),
            quiet: color(0x888b8f),
            accent_text: accent,
            accent_soft: color(0xdd9880),
            link: color(0x8ab5e1),
            link_hover: color(0xa7c7e9),
            link_line: color(0x50657c),
            code_ink: color(0xc7d2dd),
            inline_code_background: color(0x171c23),
            surface: color(0x151920),
            line: color(0x272c34),
            line_soft: color(0x1d2229),
            line_strong: color(0x48525f),
            checkbox: color(0x3b3b3b),
            checkbox_border: color(0x626262),
            checkbox_checked: color(0x757575),
            checkbox_tick: color(0x3b3b3b),
            selection: rgba(0x3f638bff).into(),
            selection_inactive: rgba(0x464646ff).into(),
            syntax: SyntaxColors {
                comment: color(0x6b7580),
                string: color(0x9ec97f),
                number: color(0xe2b46f),
                constant: color(0xe2b46f),
                keyword: color(0xe3917a),
                operator: color(0x8ab5e1),
                function: color(0x8db6e6),
                type_name: color(0x7fc8c1),
                variable: color(0xc7d2dd),
                tag: color(0xe8877f),
                attribute: color(0xd8b27c),
                punctuation: color(0x8b939c),
            },
            copy_button: CopyButtonColors {
                fill: rgba(0x1b2028c0).into(),
                border: with_alpha(color(0xe6e4df), 0.11),
                border_hover: color(0x5f6872),
                glyph: color(0xb3b4b4),
                glyph_block_hover: color(0xcbcbc8),
                glyph_hover: color(0xd9d8d4),
                copied_border: color(0x3f6d3b),
                copied: color(0x9cd396),
                failed_border: color(0xe06c75),
                failed: color(0xeb98a0),
                copy_glyph: None,
                check_glyph: None,
                x_glyph: None,
            },
            highlight_code_blocks: false,
            file_glyph: None,
            thread_glyph: None,
        }
    }

    /// `aicodingtool-light`.
    pub fn light() -> Self {
        let accent = color(0xaa4824);
        Self {
            text: color(0x272a2e),
            ink: color(0x1d1f24),
            muted_strong: color(0x3d4045),
            quiet: color(0x61656c),
            accent_text: accent,
            accent_soft: color(0x7e3b23),
            link: color(0x1f5fa8),
            link_hover: color(0x6290c2),
            link_line: color(0x8ca7bf),
            code_ink: color(0x565961),
            inline_code_background: color(0xf1f0eb),
            surface: color(0xffffff),
            line: color(0xdedbd2),
            line_soft: color(0xeceae3),
            line_strong: color(0xa8a396),
            checkbox: color(0xf7f7f7),
            checkbox_border: color(0xc5c5c5),
            checkbox_checked: color(0xc5c5c5),
            checkbox_tick: color(0xffffff),
            selection: rgba(0xb3d7ffff).into(),
            selection_inactive: rgba(0xdcdcdcff).into(),
            syntax: SyntaxColors {
                comment: color(0x8a8f98),
                string: color(0x3f7a2e),
                number: color(0x84560b),
                constant: color(0x84560b),
                keyword: color(0x9e4526),
                operator: color(0x1f5fa8),
                function: color(0x2b5f9f),
                type_name: color(0x256a65),
                variable: color(0x565961),
                tag: color(0xa13f47),
                attribute: color(0x80580f),
                punctuation: color(0x6b6f78),
            },
            copy_button: CopyButtonColors {
                fill: rgba(0xeeece5c0).into(),
                border: with_alpha(color(0x1d1f24), 0.11),
                border_hover: color(0x8f8b80),
                glyph: color(0x41444a),
                glyph_block_hover: color(0x303338),
                glyph_hover: color(0x272a2e),
                copied_border: color(0x8db889),
                copied: color(0x2f742a),
                failed_border: color(0xb43540),
                failed: color(0x9c2e38),
                copy_glyph: None,
                check_glyph: None,
                x_glyph: None,
            },
            ..Self::dark()
        }
    }

    /// `color-mix(in srgb, var(--accent-text) 40%, transparent)`: a link's underline.
    pub fn link_underline(&self) -> Hsla {
        with_alpha(self.accent_text, 0.4)
    }

    /// The underline under a thread link: the accent at 45%.
    pub fn thread_underline(&self) -> Hsla {
        with_alpha(self.accent_text, 0.45)
    }
}

impl Default for MarkdownStyle {
    fn default() -> Self {
        Self::dark()
    }
}

/// Loads SF Mono, the face CSS's `ui-monospace` resolves to on macOS, which the system keeps out of
/// the font list. Returns its family name once loaded.
pub fn load_system_monospace(cx: &gpui::App) -> Option<SharedString> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let mut loaded = false;
    for file in [
        "/System/Library/Fonts/SFNSMono.ttf",
        "/System/Library/Fonts/SFNSMonoItalic.ttf",
    ] {
        if let Ok(bytes) = std::fs::read(file) {
            loaded |= cx
                .text_system()
                .add_fonts(vec![std::borrow::Cow::Owned(bytes)])
                .is_ok();
        }
    }
    loaded.then(|| SharedString::new_static(".SF NS Mono"))
}
