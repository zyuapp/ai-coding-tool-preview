//! The spacing in `styles/markdown.css`, in pixels or ems of the message font.

pub const PARAGRAPH_BOTTOM: f32 = 15.;

pub const HEADING_TOP: f32 = 28.;
pub const HEADING_BOTTOM: f32 = 12.;
pub const HEADING_LINE_HEIGHT: f32 = 1.28;
pub const HEADING_WEIGHT: f32 = 620.;
/// `letter-spacing: -0.018em`.
pub const HEADING_TRACKING: f32 = -0.018;
/// h1 to h3; h4 to h6 keep the message size.
pub const HEADING_SCALE: [f32; 3] = [1.6, 1.33, 1.13];
pub const STRONG_WEIGHT: f32 = 650.;

pub const LIST_BOTTOM: f32 = 16.;
pub const LIST_INDENT: f32 = 25.;
pub const TASK_LIST_INDENT: f32 = 3.;
pub const ITEM_MARGIN: f32 = 5.;
pub const ITEM_INDENT: f32 = 2.;
/// `li > ul, li > ol { margin: 5px 0 0 }`.
pub const NESTED_LIST_TOP: f32 = 5.;
/// A symbol marker's centre: how far left of the item's text, and above its baseline, in ems.
pub const BULLET_OFFSET: f32 = 10.15 / 15.;
pub const BULLET_RAISE: f32 = 4.5 / 15.;
pub const CHECKBOX_SIZE: f32 = 13.;
pub const CHECKBOX_GAP: f32 = 8.;

pub const QUOTE_MARGIN: f32 = 17.;
pub const QUOTE_RULE: f32 = 3.;
pub const QUOTE_PADDING_Y: f32 = 2.;
pub const QUOTE_PADDING_LEFT: f32 = 15.;

pub const RULE_MARGIN: f32 = 26.;

/// `.markdown-body code`: 0.85em in the monospace face, with 2px 5px of padding and 5px corners.
pub const INLINE_CODE_SCALE: f32 = 0.85;
pub const INLINE_CODE_PADDING_X: f32 = 5.;

/// `.copyable`: pre, table and diagram blocks.
pub const BLOCK_MARGIN: f32 = 17.;
pub const BLOCK_RADIUS: f32 = 10.;
/// `pre`: `--text-meta` in the monospace face at 1.65, padded 13px 16px inside a 1px clear border.
pub const CODE_FONT_SIZE: f32 = 12.;
pub const CODE_LINE_HEIGHT: f32 = 1.65;
pub const CODE_PADDING_Y: f32 = 14.;
pub const CODE_PADDING_X: f32 = 17.;
/// `.code-block-head`.
pub const CODE_HEAD_HEIGHT: f32 = 36.;
pub const CODE_HEAD_PADDING_X: f32 = 16.;
pub const CODE_HEAD_FONT_SIZE: f32 = 11.;

pub const CELL_PADDING_Y: f32 = 8.;
pub const CELL_PADDING_X: f32 = 14.;
/// The heading's last cell keeps room for the copy button.
pub const LAST_HEAD_CELL_PADDING_RIGHT: f32 = 44.;
pub const TABLE_HEAD_SCALE: f32 = 0.87;
pub const TABLE_HEAD_WEIGHT: f32 = 600.;

/// `.copyable > .copy-affordance` insets, and the code block's own.
pub const COPY_INSET: f32 = 6.;
pub const COPY_INSET_TABLE_TOP: f32 = 5.;
pub const COPY_CODE_TOP: f32 = 5.;
pub const COPY_CODE_RIGHT: f32 = 7.;
pub const COPY_SIZE: f32 = 28.;
pub const COPY_CODE_SIZE: f32 = 26.;

/// `stream-word`: 180ms in three steps.
pub const WORD_FADE_MS: u64 = 180;
pub const WORD_FADE_STEPS: f32 = 3.;

/// `::-webkit-scrollbar { height: 12px }`: the room a horizontal scrollbar takes under its block.
pub const SCROLLBAR_THICKNESS: f32 = 12.;
