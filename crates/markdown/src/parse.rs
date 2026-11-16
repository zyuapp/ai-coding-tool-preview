//! Markdown source to the block tree the renderer draws, matching react-markdown with remark-gfm,
//! `skipHtml`, and the app's URL transform.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, LinkType, Options, Parser, Tag, TagEnd};

use crate::autolink::find_literals;
use crate::layout::LayoutCache;
use crate::links::{
    classify_href, is_commit_hash, message_image_path, normalize_uri, url_transform,
    LinkCapabilities, LinkTarget,
};

/// What decides how links parse: a link the host cannot open is plain text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ParseOptions {
    pub capabilities: LinkCapabilities,
    /// Whether the message has an id, which image links need to open.
    pub has_message_id: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SpanStyle {
    pub strong: bool,
    pub emphasis: bool,
    pub strike: bool,
    pub code: bool,
    pub superscript: bool,
}

/// A run of text with one style. Spans cover an inline's text in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub range: Range<usize>,
    pub style: SpanStyle,
    pub link: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlyphKind {
    File,
    Thread,
}

/// Something drawn between characters that is not text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoxKind {
    /// The left padding of an inline code span, glued to the text after it.
    CodeStart,
    /// The right padding of an inline code span, glued to the text before it.
    CodeEnd,
    /// The glyph before a link into the app or the checkout, glued to the text after it.
    Glyph(GlyphKind),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InlineBox {
    pub at: usize,
    pub kind: BoxKind,
    pub link: Option<usize>,
}

impl InlineBox {
    /// Whether the box sits after the character before `at` rather than before the one at it.
    pub fn glued_before(&self) -> bool {
        matches!(self.kind, BoxKind::CodeEnd)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkSpan {
    pub range: Range<usize>,
    pub target: LinkTarget,
}

/// Rendered text with its styles, links and boxes. `text` is what a selection copies.
#[derive(Debug, Default)]
pub struct InlineText {
    pub text: String,
    pub spans: Vec<Span>,
    pub links: Vec<LinkSpan>,
    pub boxes: Vec<InlineBox>,
    /// Text offsets paired with the source offsets they were written at, in order.
    pub source_map: Vec<(usize, usize)>,
    pub(crate) cache: RefCell<LayoutCache>,
}

impl InlineText {
    /// Where in the source the character at `offset` was written.
    pub fn source_offset(&self, offset: usize) -> usize {
        let index = self.source_map.partition_point(|(text, _)| *text <= offset);
        match index
            .checked_sub(1)
            .and_then(|index| self.source_map.get(index))
        {
            Some((text, source)) => source + (offset - text),
            None => offset,
        }
    }

    pub fn link_at(&self, offset: usize) -> Option<usize> {
        self.links
            .iter()
            .position(|link| link.range.contains(&offset))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    None,
    Left,
    Center,
    Right,
}

#[derive(Debug)]
pub enum ParaPart {
    Text(Rc<InlineText>),
    /// A web image, drawn on its own line inside the paragraph.
    Image {
        src: String,
        alt: String,
    },
}

#[derive(Debug)]
pub struct ListItem {
    pub checked: Option<bool>,
    pub blocks: Vec<Block>,
}

#[derive(Debug)]
pub struct CodeBlock {
    /// The first word of the fence's info string.
    pub lang: Option<String>,
    /// What was written between the fences, without the final newline.
    pub code: Rc<InlineText>,
    pub(crate) highlight: RefCell<Option<Option<Vec<crate::highlight::Token>>>>,
}

#[derive(Debug)]
pub struct Table {
    pub aligns: Vec<Align>,
    pub head: Vec<Rc<InlineText>>,
    pub rows: Vec<Vec<Rc<InlineText>>>,
    /// The Markdown the table was written as, which is what its copy button writes.
    pub source: String,
    pub(crate) columns: RefCell<Option<crate::render::ColumnCache>>,
}

#[derive(Debug)]
pub enum Block {
    Paragraph(Vec<ParaPart>),
    /// Inline content straight inside a tight list item, with no paragraph margins.
    Plain(Vec<ParaPart>),
    Heading {
        level: u8,
        content: Rc<InlineText>,
    },
    Quote(Vec<Block>),
    List {
        start: Option<u64>,
        task_list: bool,
        items: Vec<ListItem>,
    },
    Code(CodeBlock),
    Table(Table),
    Rule,
}

/// A parsed Markdown document.
#[derive(Debug, Default)]
pub struct Document {
    pub blocks: Vec<Block>,
}

const COLLAPSIBLE: [char; 5] = [' ', '\t', '\n', '\r', '\u{c}'];

#[derive(Default)]
struct InlineBuilder {
    out: InlineText,
    preformatted: bool,
    at_line_start: bool,
}

impl InlineBuilder {
    fn new() -> Self {
        Self {
            at_line_start: true,
            ..Self::default()
        }
    }

    fn last_is_space(&self) -> bool {
        self.at_line_start || self.out.text.ends_with(' ')
    }

    fn push_text(&mut self, text: &str, style: SpanStyle, link: Option<usize>, source: usize) {
        if text.is_empty() {
            return;
        }
        let start = self.out.text.len();
        self.out.source_map.push((start, source));
        if self.preformatted {
            self.out.text.push_str(text);
        } else {
            for ch in text.chars() {
                if COLLAPSIBLE.contains(&ch) {
                    if !self.last_is_space() {
                        self.out.text.push(' ');
                    }
                } else {
                    self.out.text.push(ch);
                    self.at_line_start = false;
                }
            }
        }
        let end = self.out.text.len();
        if end == start {
            self.out.source_map.pop();
            return;
        }
        match self.out.spans.last_mut() {
            Some(last) if last.style == style && last.link == link && last.range.end == start => {
                last.range.end = end
            }
            _ => self.out.spans.push(Span {
                range: start..end,
                style,
                link,
            }),
        }
    }

    fn hard_break(&mut self, style: SpanStyle, link: Option<usize>, source: usize) {
        self.trim_trailing_space();
        let saved = self.preformatted;
        self.preformatted = true;
        self.push_text("\n", style, link, source);
        self.preformatted = saved;
        self.at_line_start = true;
    }

    fn trim_trailing_space(&mut self) {
        while self.out.text.ends_with(' ') && !self.preformatted {
            self.out.text.pop();
            let end = self.out.text.len();
            if let Some(last) = self.out.spans.last_mut() {
                last.range.end = last.range.end.min(end);
                if last.range.is_empty() {
                    self.out.spans.pop();
                }
            }
            for item in &mut self.out.boxes {
                item.at = item.at.min(end);
            }
        }
    }

    fn push_box(&mut self, kind: BoxKind, link: Option<usize>) {
        self.out.boxes.push(InlineBox {
            at: self.out.text.len(),
            kind,
            link,
        });
    }

    fn finish(mut self) -> InlineText {
        self.trim_trailing_space();
        let end = self.out.text.len();
        for link in &mut self.out.links {
            link.range.end = link.range.end.min(end);
        }
        self.out
    }
}

struct ParaBuilder {
    parts: Vec<ParaPart>,
    inline: InlineBuilder,
}

impl ParaBuilder {
    fn new() -> Self {
        Self {
            parts: Vec::new(),
            inline: InlineBuilder::new(),
        }
    }

    fn flush(&mut self) {
        let inline = std::mem::replace(&mut self.inline, InlineBuilder::new());
        let text = inline.finish();
        if !text.text.is_empty() || !text.boxes.is_empty() {
            self.parts.push(ParaPart::Text(Rc::new(text)));
        }
    }

    fn finish(mut self) -> Vec<ParaPart> {
        self.flush();
        self.parts
    }
}

enum Frame {
    Root(Vec<Block>),
    Quote(Vec<Block>),
    List {
        start: Option<u64>,
        task_list: bool,
        items: Vec<ListItem>,
    },
    Item {
        checked: Option<bool>,
        blocks: Vec<Block>,
    },
    Footnote {
        label: String,
        blocks: Vec<Block>,
    },
    Paragraph(ParaBuilder),
    Plain(ParaBuilder),
    Heading {
        level: u8,
        inline: InlineBuilder,
    },
    Code {
        lang: Option<String>,
        code: String,
    },
    Table {
        aligns: Vec<Align>,
        head: Vec<Rc<InlineText>>,
        rows: Vec<Vec<Rc<InlineText>>>,
        in_head: bool,
        source: Range<usize>,
    },
    Cell(InlineBuilder),
    Html,
}

struct OpenLink {
    index: Option<usize>,
}

struct OpenImage {
    src: String,
    alt: String,
}

struct Builder<'a> {
    source: &'a str,
    options: ParseOptions,
    stack: Vec<Frame>,
    style: SpanStyle,
    link: Option<OpenLink>,
    image: Option<OpenImage>,
    footnote_numbers: HashMap<String, usize>,
    footnotes: HashMap<String, Vec<Block>>,
}

impl<'a> Builder<'a> {
    fn push_block(&mut self, block: Block) {
        if let Some(
            Frame::Root(blocks)
            | Frame::Quote(blocks)
            | Frame::Item { blocks, .. }
            | Frame::Footnote { blocks, .. },
        ) = self.stack.last_mut()
        {
            blocks.push(block);
        }
    }

    /// The inline builder text goes into, opening a bare one inside a tight list item.
    fn inline(&mut self) -> Option<&mut InlineBuilder> {
        if matches!(
            self.stack.last(),
            Some(Frame::Item { .. } | Frame::Footnote { .. })
        ) {
            self.stack.push(Frame::Plain(ParaBuilder::new()));
        }
        match self.stack.last_mut()? {
            Frame::Paragraph(para) | Frame::Plain(para) => Some(&mut para.inline),
            Frame::Heading { inline, .. } | Frame::Cell(inline) => Some(inline),
            _ => None,
        }
    }

    fn close_plain(&mut self) {
        if !matches!(self.stack.last(), Some(Frame::Plain(_))) {
            return;
        }
        if let Some(Frame::Plain(para)) = self.stack.pop() {
            let parts = para.finish();
            if !parts.is_empty() {
                self.push_block(Block::Plain(parts));
            }
        }
    }

    fn current_link(&self) -> Option<usize> {
        self.link.as_ref().and_then(|link| link.index)
    }

    fn text(&mut self, text: &str, at: usize) {
        if let Some(image) = &mut self.image {
            image.alt.push_str(text);
            return;
        }
        let style = self.style;
        let in_link = self.link.is_some();
        let options = self.options;
        let link = self.current_link();
        let Some(inline) = self.inline() else { return };
        if in_link || inline.preformatted {
            inline.push_text(text, style, link, at);
            return;
        }
        let mut cursor = 0;
        for literal in find_literals(text) {
            inline.push_text(&text[cursor..literal.range.start], style, None, at + cursor);
            let href = url_transform(&normalize_uri(&literal.href), false);
            let index =
                classify_href(&href, options.capabilities, options.has_message_id).map(|target| {
                    let start = inline.out.text.len();
                    inline.out.links.push(LinkSpan {
                        range: start..start,
                        target,
                    });
                    inline.out.links.len() - 1
                });
            if let Some(index) = index {
                push_glyph(inline, index);
            }
            inline.push_text(
                &text[literal.range.clone()],
                style,
                index,
                at + literal.range.start,
            );
            if let Some(index) = index {
                inline.out.links[index].range.end = inline.out.text.len();
            }
            cursor = literal.range.end;
        }
        inline.push_text(&text[cursor..], style, None, at + cursor);
    }

    fn code(&mut self, code: &str, at: usize) {
        if let Some(image) = &mut self.image {
            image.alt.push_str(code);
            return;
        }
        let style = SpanStyle {
            code: true,
            ..self.style
        };
        let mut link = self.current_link();
        let commit =
            self.link.is_none() && self.options.capabilities.open_commit && is_commit_hash(code);
        let Some(inline) = self.inline() else { return };
        if commit {
            let start = inline.out.text.len();
            inline.out.links.push(LinkSpan {
                range: start..start,
                target: LinkTarget::Commit(code.to_string()),
            });
            link = Some(inline.out.links.len() - 1);
        }
        inline.push_box(BoxKind::CodeStart, link);
        inline.push_text(code, style, link, at + 1);
        inline.push_box(BoxKind::CodeEnd, link);
        if let Some(index) = link.filter(|_| commit) {
            inline.out.links[index].range.end = inline.out.text.len();
        }
    }

    fn start_link(&mut self, link_type: LinkType, dest: &str) {
        if self.image.is_some() {
            return;
        }
        let raw = if link_type == LinkType::Email {
            format!("mailto:{dest}")
        } else {
            dest.to_string()
        };
        let href = url_transform(&normalize_uri(&raw), false);
        let target = classify_href(
            &href,
            self.options.capabilities,
            self.options.has_message_id,
        );
        let index = target.and_then(|target| {
            let inline = self.inline()?;
            let start = inline.out.text.len();
            inline.out.links.push(LinkSpan {
                range: start..start,
                target,
            });
            let index = inline.out.links.len() - 1;
            push_glyph(inline, index);
            Some(index)
        });
        self.link = Some(OpenLink { index });
    }

    fn end_link(&mut self) {
        if self.image.is_some() {
            return;
        }
        if let Some(OpenLink { index: Some(index) }) = self.link.take() {
            if let Some(inline) = self.inline() {
                inline.out.links[index].range.end = inline.out.text.len();
            }
        }
        self.link = None;
    }

    fn end_image(&mut self, at: usize) {
        let Some(image) = self.image.take() else {
            return;
        };
        let local = message_image_path(&image.src).is_some() || image.src.is_empty();
        let style = self.style;
        let link = self.current_link();
        if local {
            if let Some(inline) = self.inline() {
                inline.push_text(&image.alt, style, link, at);
            }
            return;
        }
        if let Some(Frame::Paragraph(para) | Frame::Plain(para)) = self.stack.last_mut() {
            para.flush();
            para.parts.push(ParaPart::Image {
                src: image.src,
                alt: image.alt,
            });
        } else if let Some(inline) = self.inline() {
            inline.push_text(&image.alt, style, link, at);
        }
    }

    fn footnote_reference(&mut self, label: &str, at: usize) {
        let next = self.footnote_numbers.len() + 1;
        let number = *self
            .footnote_numbers
            .entry(label.to_string())
            .or_insert(next);
        let style = SpanStyle {
            superscript: true,
            ..self.style
        };
        let Some(inline) = self.inline() else { return };
        let start = inline.out.text.len();
        inline.out.links.push(LinkSpan {
            range: start..start,
            target: LinkTarget::Other(format!("#user-content-fn-{label}")),
        });
        let index = inline.out.links.len() - 1;
        inline.push_text(&number.to_string(), style, Some(index), at);
        inline.out.links[index].range.end = inline.out.text.len();
    }

    fn start(&mut self, tag: Tag<'_>, range: Range<usize>) {
        let is_inline = matches!(
            tag,
            Tag::Emphasis
                | Tag::Strong
                | Tag::Strikethrough
                | Tag::Superscript
                | Tag::Subscript
                | Tag::Link { .. }
                | Tag::Image { .. }
        );
        if !is_inline {
            self.close_plain();
        }
        match tag {
            Tag::Paragraph => self.stack.push(Frame::Paragraph(ParaBuilder::new())),
            Tag::Heading { level, .. } => self.stack.push(Frame::Heading {
                level: level as u8,
                inline: InlineBuilder::new(),
            }),
            Tag::BlockQuote(_) => self.stack.push(Frame::Quote(Vec::new())),
            Tag::CodeBlock(kind) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => {
                        info.split_whitespace().next().map(str::to_string)
                    }
                    CodeBlockKind::Indented => None,
                };
                self.stack.push(Frame::Code {
                    lang,
                    code: String::new(),
                });
            }
            Tag::HtmlBlock => self.stack.push(Frame::Html),
            Tag::List(start) => self.stack.push(Frame::List {
                start,
                task_list: false,
                items: Vec::new(),
            }),
            Tag::Item => self.stack.push(Frame::Item {
                checked: None,
                blocks: Vec::new(),
            }),
            Tag::FootnoteDefinition(label) => self.stack.push(Frame::Footnote {
                label: label.to_string(),
                blocks: Vec::new(),
            }),
            Tag::Table(aligns) => self.stack.push(Frame::Table {
                aligns: aligns
                    .iter()
                    .map(|align| match align {
                        Alignment::None => Align::None,
                        Alignment::Left => Align::Left,
                        Alignment::Center => Align::Center,
                        Alignment::Right => Align::Right,
                    })
                    .collect(),
                head: Vec::new(),
                rows: Vec::new(),
                in_head: false,
                source: range,
            }),
            Tag::TableHead => {
                if let Some(Frame::Table { in_head, .. }) = self.stack.last_mut() {
                    *in_head = true;
                }
            }
            Tag::TableRow => {
                if let Some(Frame::Table { rows, .. }) = self.stack.last_mut() {
                    rows.push(Vec::new());
                }
            }
            Tag::TableCell => self.stack.push(Frame::Cell(InlineBuilder::new())),
            Tag::Emphasis => self.style.emphasis = true,
            Tag::Strong => self.style.strong = true,
            Tag::Strikethrough => self.style.strike = true,
            Tag::Superscript | Tag::Subscript => {}
            Tag::Link {
                link_type,
                dest_url,
                ..
            } => self.start_link(link_type, &dest_url),
            Tag::Image { dest_url, .. } => {
                if self.image.is_none() {
                    let src = url_transform(&normalize_uri(&dest_url), true);
                    self.image = Some(OpenImage {
                        src,
                        alt: String::new(),
                    });
                }
            }
            Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::MetadataBlock(_) => {}
        }
    }

    fn end(&mut self, tag: TagEnd, range: Range<usize>) {
        match tag {
            TagEnd::Emphasis => self.style.emphasis = false,
            TagEnd::Strong => self.style.strong = false,
            TagEnd::Strikethrough => self.style.strike = false,
            TagEnd::Link => self.end_link(),
            TagEnd::Image => self.end_image(range.start),
            TagEnd::Superscript | TagEnd::Subscript => {}
            TagEnd::TableHead => {
                if let Some(Frame::Table { in_head, .. }) = self.stack.last_mut() {
                    *in_head = false;
                }
            }
            TagEnd::TableRow => {}
            TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::MetadataBlock(_) => {}
            _ => {
                self.close_plain();
                self.close_block();
            }
        }
    }

    fn close_block(&mut self) {
        let Some(frame) = self.stack.pop() else {
            return;
        };
        match frame {
            Frame::Paragraph(para) => {
                let parts = para.finish();
                if !parts.is_empty() {
                    self.push_block(Block::Paragraph(parts));
                }
            }
            Frame::Plain(para) => {
                let parts = para.finish();
                if !parts.is_empty() {
                    self.push_block(Block::Plain(parts));
                }
            }
            Frame::Heading { level, inline } => self.push_block(Block::Heading {
                level,
                content: Rc::new(inline.finish()),
            }),
            Frame::Quote(blocks) => self.push_block(Block::Quote(blocks)),
            Frame::List {
                start,
                task_list,
                items,
            } => self.push_block(Block::List {
                start,
                task_list,
                items,
            }),
            Frame::Item { checked, blocks } => {
                if let Some(Frame::List {
                    items, task_list, ..
                }) = self.stack.last_mut()
                {
                    *task_list |= checked.is_some();
                    items.push(ListItem { checked, blocks });
                }
            }
            Frame::Footnote { label, blocks } => {
                self.footnotes.entry(label).or_insert(blocks);
            }
            Frame::Code { lang, mut code } => {
                if code.ends_with('\n') {
                    code.pop();
                }
                let mut inline = InlineBuilder {
                    preformatted: true,
                    ..InlineBuilder::new()
                };
                inline.push_text(
                    &code,
                    SpanStyle {
                        code: true,
                        ..SpanStyle::default()
                    },
                    None,
                    0,
                );
                self.push_block(Block::Code(CodeBlock {
                    lang,
                    code: Rc::new(inline.out),
                    highlight: RefCell::new(None),
                }));
            }
            Frame::Table {
                aligns,
                head,
                rows,
                source,
                ..
            } => {
                let source = self.source[source].trim_end_matches('\n').to_string();
                self.push_block(Block::Table(Table {
                    aligns,
                    head,
                    rows,
                    source,
                    columns: RefCell::new(None),
                }));
            }
            Frame::Cell(inline) => {
                let cell = Rc::new(inline.finish());
                if let Some(Frame::Table {
                    head,
                    rows,
                    in_head,
                    ..
                }) = self.stack.last_mut()
                {
                    if *in_head {
                        head.push(cell);
                    } else if let Some(row) = rows.last_mut() {
                        row.push(cell);
                    }
                }
            }
            Frame::Html => {}
            Frame::Root(blocks) => self.stack.push(Frame::Root(blocks)),
        }
    }

    fn event(&mut self, event: Event<'_>, range: Range<usize>) {
        if matches!(self.stack.last(), Some(Frame::Html))
            && !matches!(event, Event::End(TagEnd::HtmlBlock))
        {
            return;
        }
        match event {
            Event::Start(tag) => self.start(tag, range),
            Event::End(tag) => self.end(tag, range),
            Event::Text(text) => {
                if let Some(Frame::Code { code, .. }) = self.stack.last_mut() {
                    code.push_str(&text);
                } else {
                    self.text(&text, range.start);
                }
            }
            Event::Code(code) => self.code(&code, range.start),
            Event::InlineMath(text) | Event::DisplayMath(text) => self.text(&text, range.start),
            Event::Html(_) | Event::InlineHtml(_) => {}
            Event::FootnoteReference(label) => self.footnote_reference(&label, range.start),
            Event::SoftBreak => {
                if self.image.is_some() {
                    self.text(" ", range.start);
                } else {
                    let (style, link) = (self.style, self.current_link());
                    if let Some(inline) = self.inline() {
                        inline.push_text(" ", style, link, range.start);
                    }
                }
            }
            Event::HardBreak => {
                let (style, link) = (self.style, self.current_link());
                if let Some(inline) = self.inline() {
                    inline.hard_break(style, link, range.start);
                }
            }
            Event::Rule => {
                self.close_plain();
                self.push_block(Block::Rule);
            }
            Event::TaskListMarker(checked) => {
                for frame in self.stack.iter_mut().rev() {
                    if let Frame::Item { checked: slot, .. } = frame {
                        *slot = Some(checked);
                        break;
                    }
                }
            }
        }
    }

    fn finish(mut self) -> Document {
        while self.stack.len() > 1 {
            self.close_plain();
            self.close_block();
        }
        let mut blocks = match self.stack.pop() {
            Some(Frame::Root(blocks)) => blocks,
            _ => Vec::new(),
        };
        if !self.footnote_numbers.is_empty() {
            let mut numbered: Vec<(usize, String)> = self
                .footnote_numbers
                .iter()
                .map(|(label, number)| (*number, label.clone()))
                .collect();
            numbered.sort();
            let notes: Vec<Vec<Block>> = numbered
                .into_iter()
                .filter_map(|(_, label)| self.footnotes.remove(&label))
                .collect();
            if !notes.is_empty() {
                // remark-gfm's section: a heading the app leaves visible, then the notes in reference order.
                let mut heading = InlineBuilder::new();
                heading.push_text("Footnotes", SpanStyle::default(), None, self.source.len());
                blocks.push(Block::Heading {
                    level: 2,
                    content: Rc::new(heading.finish()),
                });
                let items = notes
                    .into_iter()
                    .map(|blocks| ListItem {
                        checked: None,
                        blocks,
                    })
                    .collect();
                blocks.push(Block::List {
                    start: Some(1),
                    task_list: false,
                    items,
                });
            }
        }
        Document { blocks }
    }
}

fn push_glyph(inline: &mut InlineBuilder, index: usize) {
    let glyph = match inline.out.links[index].target {
        LinkTarget::File(_) => GlyphKind::File,
        LinkTarget::Thread(_) => GlyphKind::Thread,
        _ => return,
    };
    inline.push_box(BoxKind::Glyph(glyph), Some(index));
}

pub fn markdown_options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
}

/// Parses `source` into the tree the renderer draws.
/// Text shown as written, as `white-space: pre-wrap` shows it: one block with every line break kept.
pub fn plain(source: &str) -> Document {
    let spans = if source.is_empty() { Vec::new() } else { vec![Span { range: 0..source.len(), style: SpanStyle::default(), link: None }] };
    let text = InlineText { text: source.to_string(), spans, source_map: vec![(0, 0)], ..InlineText::default() };
    Document { blocks: vec![Block::Plain(vec![ParaPart::Text(Rc::new(text))])] }
}

pub fn parse(source: &str, options: ParseOptions) -> Document {
    let mut builder = Builder {
        source,
        options,
        stack: vec![Frame::Root(Vec::new())],
        style: SpanStyle::default(),
        link: None,
        image: None,
        footnote_numbers: HashMap::new(),
        footnotes: HashMap::new(),
    };
    for (event, range) in Parser::new_ext(source, markdown_options()).into_offset_iter() {
        builder.event(event, range);
    }
    builder.finish()
}

impl Document {
    /// Every inline code hash in the document, which the host checks against the checkout.
    pub fn commit_hashes(&self) -> Vec<String> {
        let mut hashes = Vec::new();
        self.visit_inlines(&mut |inline| {
            for link in &inline.links {
                if let LinkTarget::Commit(hash) = &link.target {
                    if !hashes.contains(hash) {
                        hashes.push(hash.clone());
                    }
                }
            }
        });
        hashes
    }

    pub fn visit_inlines(&self, visit: &mut impl FnMut(&InlineText)) {
        fn walk(blocks: &[Block], visit: &mut impl FnMut(&InlineText)) {
            for block in blocks {
                match block {
                    Block::Paragraph(parts) | Block::Plain(parts) => {
                        for part in parts {
                            if let ParaPart::Text(text) = part {
                                visit(text);
                            }
                        }
                    }
                    Block::Heading { content, .. } => visit(content),
                    Block::Quote(blocks) => walk(blocks, visit),
                    Block::List { items, .. } => {
                        for item in items {
                            walk(&item.blocks, visit);
                        }
                    }
                    Block::Code(code) => visit(&code.code),
                    Block::Table(table) => {
                        for cell in table.head.iter().chain(table.rows.iter().flatten()) {
                            visit(cell);
                        }
                    }
                    Block::Rule => {}
                }
            }
        }
        walk(&self.blocks, visit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all() -> ParseOptions {
        ParseOptions {
            capabilities: LinkCapabilities {
                select_thread: true,
                open_file: true,
                open_url_in_app: true,
                open_image: true,
                open_commit: true,
            },
            has_message_id: true,
        }
    }

    fn paragraph(doc: &Document, index: usize) -> &InlineText {
        match &doc.blocks[index] {
            Block::Paragraph(parts) | Block::Plain(parts) => match &parts[0] {
                ParaPart::Text(text) => text,
                ParaPart::Image { .. } => panic!("image"),
            },
            other => panic!("not a paragraph: {other:?}"),
        }
    }

    #[test]
    fn whitespace_collapses_and_html_is_skipped() {
        let doc = parse(
            "a   *b*\nc <b>bold</b>  \nd\n\n<script>bad()</script>",
            all(),
        );
        assert_eq!(doc.blocks.len(), 1);
        let text = paragraph(&doc, 0);
        assert_eq!(text.text, "a b c bold\nd");
        assert!(text
            .spans
            .iter()
            .any(|span| span.style.emphasis && &text.text[span.range.clone()] == "b"));
    }

    #[test]
    fn unknown_app_links_are_plain_and_threads_are_links() {
        let doc = parse(
            "See [work](aicodingtool://thread/t9), not [x](aicodingtool://archive/1).",
            all(),
        );
        let text = paragraph(&doc, 0);
        assert_eq!(text.text, "See work, not x.");
        assert_eq!(text.links.len(), 1);
        assert_eq!(text.links[0].target, LinkTarget::Thread("t9".into()));
        assert_eq!(&text.text[text.links[0].range.clone()], "work");
        assert!(text
            .boxes
            .iter()
            .any(|item| item.kind == BoxKind::Glyph(GlyphKind::Thread) && item.at == 4));
    }

    #[test]
    fn literal_urls_link_outside_code_only() {
        let doc = parse("Read https://example.com/docs. `https://x.com`", all());
        let text = paragraph(&doc, 0);
        assert_eq!(text.links.len(), 1);
        assert_eq!(
            text.links[0].target,
            LinkTarget::Web("https://example.com/docs".into())
        );
    }

    #[test]
    fn commit_hashes_link_only_in_inline_code() {
        let doc = parse("pushed `60cceb8` and\n\n```\n35995b7\n```", all());
        assert_eq!(doc.commit_hashes(), vec!["60cceb8".to_string()]);
        match &doc.blocks[1] {
            Block::Code(code) => assert_eq!(code.code.text, "35995b7"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn task_lists_tables_and_footnotes() {
        let doc = parse(
            "- [x] Done\n- Todo\n\n| A | B |\n|---|--:|\n| 1 | 2 |\n\nNote[^n].\n\n[^n]: The note.",
            all(),
        );
        match &doc.blocks[0] {
            Block::List {
                task_list, items, ..
            } => {
                assert!(task_list);
                assert_eq!(items[0].checked, Some(true));
                assert_eq!(items[1].checked, None);
            }
            other => panic!("{other:?}"),
        }
        match &doc.blocks[1] {
            Block::Table(table) => {
                assert_eq!(table.aligns, vec![Align::None, Align::Right]);
                assert_eq!(table.rows[0][1].text, "2");
                assert!(
                    table.source.starts_with("| A | B |") && table.source.ends_with("| 1 | 2 |")
                );
            }
            other => panic!("{other:?}"),
        }
        assert!(
            matches!(&doc.blocks[doc.blocks.len() - 2], Block::Heading { level: 2, content } if content.text == "Footnotes")
        );
        assert!(matches!(doc.blocks.last(), Some(Block::List { items, .. }) if items.len() == 1));
    }

    #[test]
    fn local_images_render_their_alt_text() {
        let doc = parse(
            "Shot: ![the shot](/tmp/a.png) and ![web](https://x.dev/a.png)",
            all(),
        );
        match &doc.blocks[0] {
            Block::Paragraph(parts) => {
                assert!(
                    matches!(&parts[0], ParaPart::Text(text) if text.text == "Shot: the shot and")
                );
                assert!(
                    matches!(&parts[1], ParaPart::Image { src, .. } if src == "https://x.dev/a.png")
                );
            }
            other => panic!("{other:?}"),
        }
    }
}
