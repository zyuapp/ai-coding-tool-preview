//! Chat message rendering: GFM Markdown parsed with pulldown-cmark and drawn as GPUI elements the
//! way `react-markdown` and `styles/markdown.css` draw it in the Electron app, with streaming,
//! selection, copy buttons and syntax colouring.

pub mod autolink;
mod chunk;
mod group;
mod gutter;
pub mod highlight;
pub mod images;
pub mod layout;
pub mod links;
pub mod metrics;
pub mod parse;
mod parts;
pub mod render;
mod selection;
pub mod stream;
pub mod style;
mod textmate;
pub mod view;

pub use group::SelectionGroup;
pub use gutter::OverflowGutter;
pub use parts::{selectable_area, SelectableText};
pub use links::{FileHref, LinkCapabilities, LinkTarget};
pub use parse::{parse, Document, ParseOptions};
pub use render::{
    CopyButtonRequest, CopyButtonSlot, CopyKind, FenceRequest, FenceSlot, Highlight, TextPos,
};
pub use style::{CopyButtonColors, MarkdownStyle, SyntaxColors};
pub use view::{
    bind_keys, Copy, LinkAction, LinkEvent, MarkdownEvent, MarkdownView, SelectAll, KEY_CONTEXT,
};
