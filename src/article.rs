//! Conversion of a Readeck article HTML fragment into readable text blocks.
//!
//! Readeck serves only the article body (not a complete page), but that body
//! still contains lots of structural markup. This module walks the DOM and
//! turns paragraphs, headings, lists, quotes and code into blocks that the
//! reader can wrap and style.

use std::collections::HashMap;

use ego_tree::NodeRef;
use image::DynamicImage;
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::layout::Size;
use scraper::{Html, Node};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    Heading1,
    Heading2,
    Heading3,
    ListItem,
    Quote,
    Code,
    Caption,
    Rule,
    Image,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub kind: BlockKind,
    pub text: String,
}

/// Where a block came from in the article HTML, for annotations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// Readeck annotation selector (`p[2]`, `section[1]/ul[1]/li[3]`).
    pub path: String,
    /// Length of the element's text in characters.
    pub len: usize,
    /// For each character of the block text, its offset in the element text.
    pub offsets: Vec<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct Article {
    pub blocks: Vec<Block>,
    /// Maps an image block's index to its source URL.
    pub images: HashMap<usize, String>,
    /// Annotation source for each block, when it can be computed.
    pub sources: Vec<Option<Source>>,
    /// Markdown source offsets for each block when parsed from Markdown.
    pub markdown_sources: Vec<Option<MarkdownSource>>,
}

/// Maps a rendered block's characters back to the Markdown source.
#[derive(Debug, Clone, Default)]
pub struct MarkdownSource {
    /// Byte offset where the block starts in the Markdown source.
    pub start: usize,
    /// Byte offset in the Markdown source for each character of the block text.
    pub offsets: Vec<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageCell {
    pub top: [u8; 4],
    pub bottom: [u8; 4],
}

/// One line after wrapping the article to a given width.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrappedLine {
    pub text: String,
    /// `None` for the blank separator lines inserted between blocks.
    pub kind: Option<BlockKind>,
    /// Pre-rendered image cells when this line is part of an image.
    pub image: Option<Vec<ImageCell>>,
    /// Index of the block this line belongs to.
    pub block: Option<usize>,
    /// Character range of the block text shown on this line.
    pub range: Option<std::ops::Range<usize>>,
    /// Leading columns added by the block style (quote bar, code indent).
    pub indent: usize,
}

impl Article {
    /// Wrap the article into lines that fit `width` terminal columns.
    pub fn layout(&self, width: usize) -> Vec<WrappedLine> {
        self.layout_with_images(width, &HashMap::new(), &HashMap::new())
    }

    /// Wrap the article, reserving blank rows for images drawn with a
    /// terminal graphics protocol. Images without a size get placeholders.
    pub fn layout_with_graphics(
        &self,
        width: usize,
        sizes: &HashMap<String, Size>,
        image_errors: &HashMap<String, String>,
    ) -> Vec<WrappedLine> {
        self.layout_inner(
            width,
            |url| sizes.get(url).map(|size| reserved_rows(*size)),
            image_errors,
        )
    }

    /// Wrap the article and replace image blocks with either rendered image
    /// rows or placeholders while images are loading or unavailable.
    pub fn layout_with_images(
        &self,
        width: usize,
        images: &HashMap<String, DynamicImage>,
        image_errors: &HashMap<String, String>,
    ) -> Vec<WrappedLine> {
        self.layout_inner(
            width,
            |url| images.get(url).map(|image| render_image(image, width)),
            image_errors,
        )
    }

    fn layout_inner(
        &self,
        width: usize,
        image_rows: impl Fn(&str) -> Option<Vec<WrappedLine>>,
        image_errors: &HashMap<String, String>,
    ) -> Vec<WrappedLine> {
        let width = width.max(12);
        let mut lines = Vec::new();
        let mut previous: Option<BlockKind> = None;

        for (index, block) in self.blocks.iter().enumerate() {
            let tight = matches!(
                (previous, block.kind),
                (Some(BlockKind::ListItem), BlockKind::ListItem)
                    | (Some(BlockKind::Code), BlockKind::Code)
            );
            if !lines.is_empty() && !tight {
                lines.push(WrappedLine {
                    text: String::new(),
                    kind: None,
                    image: None,
                    block: None,
                    range: None,
                    indent: 0,
                });
            }

            if block.kind == BlockKind::Image {
                let url = self.images.get(&index);
                if let Some(rows) = url
                    .and_then(|url| image_rows(url))
                    .filter(|rows| !rows.is_empty())
                {
                    for row in rows {
                        lines.push(WrappedLine {
                            block: Some(index),
                            ..row
                        });
                    }
                } else {
                    let alt = block.text.trim();
                    let label = if alt.is_empty() {
                        "image".to_string()
                    } else {
                        format!("image: {alt}")
                    };
                    let placeholder = if url
                        .is_some_and(|url| image_errors.contains_key(url))
                    {
                        format!("[{label} unavailable]")
                    } else {
                        format!("[{label}]")
                    };
                    for line in textwrap::wrap(&placeholder, width) {
                        lines.push(WrappedLine {
                            text: line.into_owned(),
                            kind: Some(BlockKind::Image),
                            image: None,
                            block: Some(index),
                            range: None,
                            indent: 0,
                        });
                    }
                }
                previous = Some(block.kind);
                continue;
            }

            if block.kind == BlockKind::Rule {
                lines.push(WrappedLine {
                    text: "─".repeat(width.min(80)),
                    kind: Some(BlockKind::Rule),
                    image: None,
                    block: Some(index),
                    range: None,
                    indent: 0,
                });
                previous = Some(block.kind);
                continue;
            }

            let (initial_indent, subsequent_indent) = match block.kind {
                BlockKind::ListItem => ("", "  "),
                BlockKind::Quote => ("│ ", "│ "),
                BlockKind::Code => ("  ", "  "),
                BlockKind::Caption => ("  ", "  "),
                _ => ("", ""),
            };
            for (start, end, text, indent) in wrap_block(
                &block.text,
                width,
                initial_indent,
                subsequent_indent,
            ) {
                lines.push(WrappedLine {
                    text,
                    kind: Some(block.kind),
                    image: None,
                    block: Some(index),
                    range: Some(start..end),
                    indent,
                });
            }
            previous = Some(block.kind);
        }

        while lines.last().is_some_and(|line| line.text.is_empty()) {
            lines.pop();
        }
        lines
    }

    /// Build a small article from arbitrary text, used for bookmarks without
    /// an extracted article (for example a note or description).
    pub fn from_text_blocks(blocks: Vec<(&str, String)>) -> Self {
        let blocks: Vec<Block> = blocks
            .into_iter()
            .filter(|(_, text)| !text.trim().is_empty())
            .map(|(heading, text)| Block {
                kind: heading_kind(heading),
                text: text.trim().to_string(),
            })
            .collect();
        let sources = vec![None; blocks.len()];
        Self {
            blocks,
            images: HashMap::new(),
            sources,
            markdown_sources: Vec::new(),
        }
    }
}

/// Greedy wrap that reports which block characters each line shows.
fn wrap_block(
    text: &str,
    width: usize,
    initial_indent: &str,
    subsequent_indent: &str,
) -> Vec<(usize, usize, String, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let mut lines = Vec::new();
    let mut start = 0;
    let mut first = true;
    while start < chars.len() {
        while start < chars.len() && chars[start].is_whitespace() {
            start += 1;
        }
        if start >= chars.len() {
            break;
        }
        let indent = if first {
            initial_indent
        } else {
            subsequent_indent
        };
        let indent_width = indent.width();
        let available = width.saturating_sub(indent_width).max(1);
        let mut used = 0;
        let mut end = start;
        let mut last_space = None;
        while end < chars.len() {
            let char_width = UnicodeWidthChar::width(chars[end]).unwrap_or(0);
            if used + char_width > available {
                break;
            }
            if chars[end].is_whitespace() {
                last_space = Some(end);
            }
            used += char_width;
            end += 1;
        }
        if end == start {
            end = start + 1;
        }
        let mut cut = end;
        if end < chars.len() {
            if let Some(space) = last_space {
                if space > start {
                    cut = space;
                }
            }
        }
        while cut > start && chars[cut - 1].is_whitespace() {
            cut -= 1;
        }
        if cut == start {
            cut = end;
        }
        let content: String = chars[start..cut].iter().collect();
        lines.push((start, cut, format!("{indent}{content}"), indent_width));
        start = if cut > start { cut } else { end };
        first = false;
    }
    lines
}

fn reserved_rows(size: Size) -> Vec<WrappedLine> {
    (0..size.height)
        .map(|_| WrappedLine {
            text: " ".repeat(size.width as usize),
            kind: Some(BlockKind::Image),
            image: None,
            block: None,
            range: None,
            indent: 0,
        })
        .collect()
}

fn render_image(image: &DynamicImage, width: usize) -> Vec<WrappedLine> {
    let rgba = image.to_rgba8();
    let (width_px, height_px) = rgba.dimensions();
    if width_px == 0 || height_px == 0 {
        return Vec::new();
    }

    let max_width = width.max(12) as f32;
    let max_height_px = 40.0_f32;
    let scale = (max_width / width_px as f32)
        .min(max_height_px / height_px as f32)
        .min(1.0);

    let target_width = ((width_px as f32 * scale).round() as u32).max(1);
    let mut target_height = ((height_px as f32 * scale).round() as u32).max(2);
    if target_height % 2 != 0 {
        target_height = (target_height - 1).max(2);
    }

    let resized = image::imageops::resize(
        &rgba,
        target_width,
        target_height,
        image::imageops::FilterType::Triangle,
    );

    let mut rows = Vec::with_capacity((target_height / 2) as usize);
    for y in (0..target_height).step_by(2) {
        let bottom_y = (y + 1).min(target_height - 1);
        let mut cells = Vec::with_capacity(target_width as usize);
        for x in 0..target_width {
            let top = resized.get_pixel(x, y).0;
            let bottom = resized.get_pixel(x, bottom_y).0;
            cells.push(ImageCell { top, bottom });
        }
        rows.push(WrappedLine {
            text: " ".repeat(target_width as usize),
            kind: Some(BlockKind::Image),
            image: Some(cells),
            block: None,
            range: None,
            indent: 0,
        });
    }
    rows
}

fn heading_kind(heading: &str) -> BlockKind {
    match heading {
        "" => BlockKind::Paragraph,
        _ => BlockKind::Heading2,
    }
}

pub fn parse_article(html: &str) -> Article {
    let document = Html::parse_document(html);
    let body = document
        .root_element()
        .children()
        .find(|node| {
            node.value()
                .as_element()
                .is_some_and(|element| element.name() == "body")
        })
        .unwrap_or_else(|| document.tree.root());
    let mut collector = Collector::new(body);
    collector.walk(body, Context::default());
    let sources = collector
        .block_sources
        .iter()
        .enumerate()
        .map(|(block, index)| {
            index.map(|index| {
                let source = &collector.sources[index];
                Source {
                    path: source.path.clone(),
                    len: source.len,
                    offsets: collector.block_offsets[block].clone(),
                }
            })
        })
        .collect();
    Article {
        blocks: collector.blocks,
        images: collector.images,
        sources,
        markdown_sources: Vec::new(),
    }
}

#[derive(Clone, Copy, Default)]
struct Context {
    quote: bool,
    list_depth: usize,
    prefix: Option<&'static str>,
    source: Option<usize>,
}

impl Context {
    fn in_quote(self) -> Self {
        Self {
            quote: true,
            ..self
        }
    }

    fn without_prefix(self) -> Self {
        Self {
            prefix: None,
            ..self
        }
    }
}

struct Collector<'a> {
    body: NodeRef<'a, Node>,
    blocks: Vec<Block>,
    block_sources: Vec<Option<usize>>,
    block_offsets: Vec<Vec<usize>>,
    sources: Vec<Source>,
    source_counters: HashMap<usize, usize>,
    buffer: String,
    buffer_offsets: Vec<usize>,
    images: HashMap<usize, String>,
}

impl<'a> Collector<'a> {
    fn new(body: NodeRef<'a, Node>) -> Self {
        Self {
            body,
            blocks: Vec::new(),
            block_sources: Vec::new(),
            block_offsets: Vec::new(),
            sources: Vec::new(),
            source_counters: HashMap::new(),
            buffer: String::new(),
            buffer_offsets: Vec::new(),
            images: HashMap::new(),
        }
    }

    fn push_block(
        &mut self,
        block: Block,
        source: Option<usize>,
        offsets: Vec<usize>,
    ) {
        self.blocks.push(block);
        self.block_sources.push(source);
        self.block_offsets.push(offsets);
    }

    fn push_text(&mut self, text: &str, source: Option<usize>) {
        let mut offset = source
            .and_then(|source| self.source_counters.get(&source).copied())
            .unwrap_or(0);
        for ch in text.chars() {
            self.buffer.push(ch);
            self.buffer_offsets.push(offset);
            if source.is_some() {
                offset += 1;
            }
        }
        if let Some(source) = source {
            self.source_counters.insert(source, offset);
        }
    }

    fn push_synthetic(&mut self, text: &str, source: Option<usize>) {
        let offset = source
            .and_then(|source| self.source_counters.get(&source).copied())
            .unwrap_or(0);
        for ch in text.chars() {
            self.buffer.push(ch);
            self.buffer_offsets.push(offset);
        }
    }

    fn skip_text(&mut self, node: NodeRef<'_, Node>, source: Option<usize>) {
        if let Some(source) = source {
            let len = text_content(node).chars().count();
            self.source_counters
                .entry(source)
                .and_modify(|offset| *offset += len)
                .or_insert(len);
        }
    }

    fn source_for(&mut self, node: NodeRef<'_, Node>) -> Option<usize> {
        let path = selector_for(self.body, node)?;
        let len = text_content(node).chars().count();
        if len == 0 {
            return None;
        }
        self.sources.push(Source {
            path,
            len,
            offsets: Vec::new(),
        });
        Some(self.sources.len() - 1)
    }

    fn walk(&mut self, node: NodeRef<'_, Node>, context: Context) {
        if let Some(text) = node.value().as_text() {
            self.push_text(&text.text, context.source);
            return;
        }

        let Some(element) = node.value().as_element() else {
            for child in node.children() {
                self.walk(child, context);
            }
            return;
        };

        let name = element.name();
        match name {
            "script" | "style" | "noscript" | "template" | "svg" | "head"
            | "title" | "meta" | "link" | "nav" | "footer" | "form"
            | "button" | "input" | "select" | "textarea" | "iframe"
            | "video" | "audio" | "canvas" | "object" | "embed" => {
                self.skip_text(node, context.source);
            }
            "br" => {
                self.push_synthetic(" ", context.source);
            }
            "img" => {
                let alt = element.attr("alt").unwrap_or("").trim().to_string();
                if let Some(src) = element
                    .attr("src")
                    .map(str::trim)
                    .filter(|src| !src.is_empty() && !src.starts_with("data:"))
                {
                    self.flush(context);
                    self.push_block(
                        Block {
                            kind: BlockKind::Image,
                            text: alt,
                        },
                        None,
                        Vec::new(),
                    );
                    let index = self.blocks.len() - 1;
                    self.images.insert(index, src.to_string());
                } else if !alt.is_empty() {
                    self.push_synthetic(&format!(" {alt} "), context.source);
                }
            }
            "hr" => {
                self.flush(context);
                self.push_block(
                    Block {
                        kind: BlockKind::Rule,
                        text: String::new(),
                    },
                    None,
                    Vec::new(),
                );
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.flush(context);
                let raw = text_content(node);
                if !normalize(&raw).is_empty() {
                    let kind = match name {
                        "h1" => BlockKind::Heading1,
                        "h2" => BlockKind::Heading2,
                        _ => BlockKind::Heading3,
                    };
                    let source = self.source_for(node);
                    let (text, offsets) = normalize_and_index(&raw);
                    self.push_block(Block { kind, text }, source, offsets);
                }
            }
            "pre" => {
                self.flush(context);
                let raw = text_content(node).replace("\r\n", "\n");
                let mut lines: Vec<&str> =
                    raw.lines().map(|line| line.trim_end()).collect();
                while lines.first().is_some_and(|line| line.trim().is_empty()) {
                    lines.remove(0);
                }
                while lines.last().is_some_and(|line| line.trim().is_empty()) {
                    lines.pop();
                }
                for line in lines {
                    if line.trim().is_empty() {
                        self.push_block(
                            Block {
                                kind: BlockKind::Code,
                                text: String::new(),
                            },
                            None,
                            Vec::new(),
                        );
                    } else {
                        self.push_block(
                            Block {
                                kind: BlockKind::Code,
                                text: line.to_string(),
                            },
                            None,
                            Vec::new(),
                        );
                    }
                }
            }
            "tr" => {
                self.flush(context);
                let mut cells = Vec::new();
                for child in node.children() {
                    if let Some(element) = child.value().as_element() {
                        if matches!(element.name(), "td" | "th") {
                            let text = normalize(&text_content(child));
                            if !text.is_empty() {
                                cells.push(text);
                            }
                        }
                    }
                }
                if !cells.is_empty() {
                    self.push_block(
                        Block {
                            kind: BlockKind::Paragraph,
                            text: cells.join("  ·  "),
                        },
                        None,
                        Vec::new(),
                    );
                }
            }
            "li" => {
                self.flush(context);
                let mut child_context = context.without_prefix();
                child_context.prefix = Some(match context.list_depth {
                    0 | 1 => "• ",
                    2 => "◦ ",
                    _ => "▪ ",
                });
                child_context.source = self.source_for(node).or(context.source);
                for child in node.children() {
                    self.walk(child, child_context);
                }
                self.flush(child_context);
            }
            "ul" | "ol" | "menu" => {
                self.flush(context);
                let child_context = Context {
                    list_depth: (context.list_depth + 1).min(3),
                    ..context
                };
                for child in node.children() {
                    self.walk(child, child_context);
                }
                self.flush(context);
            }
            "blockquote" => {
                self.flush(context);
                let mut child_context = context.in_quote().without_prefix();
                child_context.source = self.source_for(node).or(context.source);
                for child in node.children() {
                    self.walk(child, child_context);
                }
                self.flush(child_context);
            }
            "figcaption" => {
                self.flush(context);
                let raw = text_content(node);
                if !normalize(&raw).is_empty() {
                    let source = self.source_for(node);
                    let (text, offsets) = normalize_and_index(&raw);
                    self.push_block(
                        Block {
                            kind: BlockKind::Caption,
                            text,
                        },
                        source,
                        offsets,
                    );
                }
            }
            _ if is_block(name) => {
                self.flush(context);
                let mut child_context = context;
                child_context.source = self.source_for(node).or(context.source);
                for child in node.children() {
                    self.walk(child, child_context);
                }
                self.flush(child_context);
            }
            _ => {
                for child in node.children() {
                    self.walk(child, context);
                }
            }
        }
    }

    fn flush(&mut self, context: Context) {
        let raw = std::mem::take(&mut self.buffer);
        let raw_offsets = std::mem::take(&mut self.buffer_offsets);
        let (text, offsets) = normalize_with_offsets(&raw, &raw_offsets);
        if text.is_empty() {
            return;
        }
        let (offsets, text) = match context.prefix {
            Some(prefix) => {
                let first = offsets.first().copied().unwrap_or(0);
                let mut prefixed = String::with_capacity(text.len() + 2);
                prefixed.push_str(prefix);
                prefixed.push_str(&text);
                let mut prefixed_offsets = vec![first; prefix.chars().count()];
                prefixed_offsets.extend(offsets);
                (prefixed_offsets, prefixed)
            }
            None => (offsets, text),
        };
        let kind = if context.prefix.is_some() {
            BlockKind::ListItem
        } else if context.quote {
            BlockKind::Quote
        } else {
            BlockKind::Paragraph
        };
        self.push_block(Block { kind, text }, context.source, offsets);
    }
}

/// Remove the YAML frontmatter Readeck puts at the top of Markdown exports.
pub fn strip_markdown_frontmatter(markdown: &str) -> &str {
    let Some(first_end) = markdown.find('\n') else {
        return markdown;
    };
    if markdown[..first_end].trim_end() != "---" {
        return markdown;
    }
    let body_start = first_end + 1;
    let mut consumed = body_start;
    for line in markdown[body_start..].split_inclusive('\n') {
        consumed += line.len();
        if line.trim_end() == "---" {
            return &markdown[consumed..];
        }
    }
    markdown
}

/// Parse Readeck's Markdown export into article blocks.
pub fn parse_markdown(markdown: &str) -> Article {
    let markdown = strip_markdown_frontmatter(markdown);
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_FOOTNOTES);
    let parser = Parser::new_ext(markdown, options).into_offset_iter();
    let mut builder = MarkdownBuilder::default();
    for (event, range) in parser {
        builder.handle(event, range);
    }
    builder.flush();
    builder.flush_code();
    builder.article
}

#[derive(Default)]
struct MarkdownBuilder {
    article: Article,
    text: String,
    offsets: Vec<usize>,
    kind: Option<BlockKind>,
    start: usize,
    item_prefix: &'static str,
    in_item: bool,
    quote_depth: usize,
    list_depth: usize,
    code: Option<(String, Vec<usize>, usize)>,
    image: Option<(String, String, usize)>,
    table: bool,
    table_row: Vec<String>,
    table_cell: Option<String>,
}

impl MarkdownBuilder {
    fn begin(&mut self, kind: BlockKind, start: usize) {
        if self.kind.is_none() {
            self.kind = Some(kind);
            self.start = start;
        }
    }

    fn push_char(&mut self, ch: char, offset: usize) {
        self.text.push(ch);
        self.offsets.push(offset);
    }

    fn append(&mut self, text: &str, start: usize) {
        for (index, ch) in text.char_indices() {
            self.push_char(ch, start + index);
        }
    }

    fn on_text(&mut self, text: &str, start: usize) {
        if let Some((code, offsets, _)) = &mut self.code {
            for (index, ch) in text.char_indices() {
                code.push(ch);
                offsets.push(start + index);
            }
        } else if let Some((_, alt, _)) = &mut self.image {
            alt.push_str(text);
        } else if let Some(cell) = &mut self.table_cell {
            cell.push_str(text);
        } else if !self.table {
            self.append(text, start);
        }
    }

    fn flush(&mut self) {
        let Some(kind) = self.kind.take() else {
            return;
        };
        let (mut text, mut offsets) =
            normalize_with_offsets(&self.text, &self.offsets);
        self.text.clear();
        self.offsets.clear();
        if kind == BlockKind::ListItem {
            let prefix = self.item_prefix;
            let mut prefixed_offsets = Vec::with_capacity(
                prefix.chars().count() + offsets.len(),
            );
            prefixed_offsets
                .extend(std::iter::repeat_n(self.start, prefix.chars().count()));
            prefixed_offsets.append(&mut offsets);
            text = format!("{prefix}{text}");
            offsets = prefixed_offsets;
        } else if matches!(
            kind,
            BlockKind::Heading1 | BlockKind::Heading2 | BlockKind::Heading3
        ) && !offsets.is_empty()
        {
            // Include the leading `#` characters when copying from here.
            offsets[0] = self.start;
        }
        if text.trim().is_empty() {
            return;
        }
        self.article.blocks.push(Block { kind, text });
        self.article
            .markdown_sources
            .push(Some(MarkdownSource {
                start: self.start,
                offsets,
            }));
    }

    fn flush_code(&mut self) {
        let Some((text, offsets, start)) = self.code.take() else {
            return;
        };
        let chars: Vec<char> = text.chars().collect();
        let mut line = String::new();
        let mut line_offsets = Vec::new();
        let mut first_line = true;
        let push_line = |line: &mut String,
                             line_offsets: &mut Vec<usize>,
                             first_line: &mut bool,
                             article: &mut Article| {
            while line.ends_with(' ') || line.ends_with('\t') {
                line.pop();
                line_offsets.pop();
            }
            if line.is_empty() && line_offsets.is_empty() {
                return;
            }
            let block_start = if *first_line {
                start
            } else {
                line_offsets.first().copied().unwrap_or(start)
            };
            *first_line = false;
            article.blocks.push(Block {
                kind: BlockKind::Code,
                text: std::mem::take(line),
            });
            article.markdown_sources.push(Some(MarkdownSource {
                start: block_start,
                offsets: std::mem::take(line_offsets),
            }));
        };
        for (index, ch) in chars.into_iter().enumerate() {
            if ch == '\n' {
                push_line(
                    &mut line,
                    &mut line_offsets,
                    &mut first_line,
                    &mut self.article,
                );
            } else {
                line.push(ch);
                line_offsets.push(offsets.get(index).copied().unwrap_or(start));
            }
        }
        push_line(
            &mut line,
            &mut line_offsets,
            &mut first_line,
            &mut self.article,
        );
    }

    fn flush_image(&mut self) {
        let Some((dest, alt, start)) = self.image.take() else {
            return;
        };
        let text = normalize(&alt);
        let index = self.article.blocks.len();
        self.article.images.insert(index, dest);
        self.article.markdown_sources.push(Some(MarkdownSource {
            start,
            offsets: vec![start; text.chars().count()],
        }));
        self.article
            .blocks
            .push(Block { kind: BlockKind::Image, text });
    }

    fn flush_table_row(&mut self) {
        if self.table_row.is_empty() {
            return;
        }
        let text = self.table_row.join("  ·  ");
        self.table_row.clear();
        if text.trim().is_empty() {
            return;
        }
        self.article.blocks.push(Block {
            kind: BlockKind::Paragraph,
            text,
        });
        self.article.markdown_sources.push(None);
    }

    fn handle(&mut self, event: Event<'_>, range: std::ops::Range<usize>) {
        match event {
            Event::Start(Tag::Paragraph) => {
                if !self.in_item {
                    let kind = if self.quote_depth > 0 {
                        BlockKind::Quote
                    } else {
                        BlockKind::Paragraph
                    };
                    self.begin(kind, range.start);
                }
            }
            Event::End(TagEnd::Paragraph) => {
                if !self.in_item {
                    self.flush();
                }
            }
            Event::Start(Tag::Heading { level, .. }) => {
                self.begin(markdown_heading_kind(level), range.start);
            }
            Event::End(TagEnd::Heading(_)) => self.flush(),
            Event::Start(Tag::BlockQuote(_)) => self.quote_depth += 1,
            Event::End(TagEnd::BlockQuote(_)) => {
                self.quote_depth = self.quote_depth.saturating_sub(1);
            }
            Event::Start(Tag::CodeBlock(_)) => {
                self.code = Some((String::new(), Vec::new(), range.start));
            }
            Event::End(TagEnd::CodeBlock) => self.flush_code(),
            Event::Start(Tag::List(_)) => {
                if self.in_item {
                    self.flush();
                }
                self.list_depth += 1;
            }
            Event::End(TagEnd::List(_)) => {
                self.list_depth = self.list_depth.saturating_sub(1);
            }
            Event::Start(Tag::Item) => {
                self.in_item = true;
                self.item_prefix = markdown_list_bullet(self.list_depth);
                self.begin(BlockKind::ListItem, range.start);
            }
            Event::End(TagEnd::Item) => {
                self.flush();
                self.in_item = false;
            }
            Event::Rule => {
                self.article.blocks.push(Block {
                    kind: BlockKind::Rule,
                    text: String::new(),
                });
                self.article.markdown_sources.push(None);
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                self.image = Some((
                    dest_url.to_string(),
                    String::new(),
                    range.start,
                ));
            }
            Event::End(TagEnd::Image) => self.flush_image(),
            Event::Start(Tag::Table(_)) => self.table = true,
            Event::End(TagEnd::Table) => self.table = false,
            Event::Start(Tag::TableHead) | Event::Start(Tag::TableRow) => {
                self.table_row.clear();
            }
            Event::End(TagEnd::TableHead) | Event::End(TagEnd::TableRow) => {
                self.flush_table_row();
            }
            Event::Start(Tag::TableCell) => {
                self.table_cell = Some(String::new());
            }
            Event::End(TagEnd::TableCell) => {
                if let Some(cell) = self.table_cell.take() {
                    let cell = cell.trim();
                    if !cell.is_empty() {
                        self.table_row.push(cell.to_string());
                    }
                }
            }
            Event::Text(text) => self.on_text(&text, range.start),
            Event::Code(text) => self.on_text(&text, range.start),
            Event::SoftBreak | Event::HardBreak => {
                if self.code.is_some() {
                    self.push_char('\n', range.start);
                } else {
                    self.on_text(" ", range.start);
                }
            }
            Event::TaskListMarker(done) => {
                self.on_text(if done { "[x] " } else { "[ ] " }, range.start);
            }
            Event::FootnoteReference(name) => {
                self.on_text(&format!("[^{name}]"), range.start);
            }
            Event::Html(_) | Event::InlineHtml(_) => {}
            _ => {}
        }
    }
}

fn markdown_heading_kind(level: HeadingLevel) -> BlockKind {
    match level {
        HeadingLevel::H1 => BlockKind::Heading1,
        HeadingLevel::H2 => BlockKind::Heading2,
        _ => BlockKind::Heading3,
    }
}

fn markdown_list_bullet(depth: usize) -> &'static str {
    match depth {
        1 => "• ",
        2 => "◦ ",
        _ => "▪ ",
    }
}

fn selector_for(
    root: NodeRef<'_, Node>,
    node: NodeRef<'_, Node>,
) -> Option<String> {
    let mut parts = Vec::new();
    let mut current = node;
    while current.id() != root.id() {
        let element = current.value().as_element()?;
        let mut index = 1;
        let mut sibling = current.prev_sibling();
        while let Some(previous) = sibling {
            if let Some(previous_element) = previous.value().as_element() {
                if previous_element.name() == element.name() {
                    index += 1;
                }
            }
            sibling = previous.prev_sibling();
        }
        parts.push(format!("{}[{}]", element.name(), index));
        current = current.parent()?;
    }
    if parts.is_empty() {
        return None;
    }
    parts.reverse();
    Some(parts.join("/"))
}

fn text_content(node: NodeRef<'_, Node>) -> String {
    let mut out = String::new();
    for descendant in node.descendants() {
        if let Some(text) = descendant.value().as_text() {
            out.push_str(&text.text);
        }
    }
    out
}

fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Collapse whitespace like [`normalize`] and keep a source offset per
/// resulting character.
fn normalize_with_offsets(
    text: &str,
    offsets: &[usize],
) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(text.len());
    let mut out_offsets = Vec::with_capacity(text.len());
    let mut pending_space = None;
    for (index, ch) in text.chars().enumerate() {
        let offset = offsets.get(index).copied().unwrap_or(0);
        if ch.is_whitespace() {
            if !out.is_empty() && pending_space.is_none() {
                pending_space = Some(offset);
            }
        } else {
            if let Some(space) = pending_space.take() {
                out.push(' ');
                out_offsets.push(space);
            }
            out.push(ch);
            out_offsets.push(offset);
        }
    }
    (out, out_offsets)
}

fn normalize_and_index(text: &str) -> (String, Vec<usize>) {
    let offsets: Vec<usize> = (0..text.chars().count()).collect();
    normalize_with_offsets(text, &offsets)
}

fn is_block(name: &str) -> bool {
    matches!(
        name,
        "address"
            | "article"
            | "aside"
            | "body"
            | "dd"
            | "details"
            | "dialog"
            | "div"
            | "dl"
            | "dt"
            | "fieldset"
            | "figure"
            | "header"
            | "hgroup"
            | "main"
            | "p"
            | "section"
            | "summary"
            | "table"
            | "tbody"
            | "td"
            | "tfoot"
            | "th"
            | "thead"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_markdown_articles() {
        let markdown = r#"---
title: Test
description: Frontmatter
---

# Heading

A paragraph with **bold** text.

- First item
- Second item

> Quoted text

```rust
let x = 1;
```

| a | b |
|---|---|
| c | d |

![A picture](picture.jpg)
"#;
        let article = parse_markdown(markdown);
        let kinds: Vec<BlockKind> =
            article.blocks.iter().map(|block| block.kind).collect();
        assert!(kinds.contains(&BlockKind::Heading1));
        assert!(kinds.contains(&BlockKind::Paragraph));
        assert!(kinds.contains(&BlockKind::ListItem));
        assert!(kinds.contains(&BlockKind::Quote));
        assert!(kinds.contains(&BlockKind::Code));
        assert!(kinds.contains(&BlockKind::Image));
        assert!(!article
            .blocks
            .iter()
            .any(|block| block.text.contains("Frontmatter")));
        assert!(article
            .blocks
            .iter()
            .any(|block| block.text == "A paragraph with bold text."));
        assert!(article
            .blocks
            .iter()
            .any(|block| block.kind == BlockKind::ListItem
                && block.text == "• First item"));
        let image = article
            .blocks
            .iter()
            .position(|block| block.kind == BlockKind::Image)
            .unwrap();
        assert_eq!(article.images[&image], "picture.jpg");
        assert!(article
            .blocks
            .iter()
            .any(|block| block.text.contains("a  ·  b")));
        assert_eq!(
            article.markdown_sources.len(),
            article.blocks.len()
        );
    }

    #[test]
    fn parses_nested_markdown_lists() {
        let article = parse_markdown("- First\n  - Nested\n- Second\n");
        let texts: Vec<&str> = article
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect();
        assert_eq!(texts, vec!["• First", "◦ Nested", "• Second"]);
    }

    #[test]
    fn markdown_sources_include_block_syntax() {
        let markdown = "## Heading\n\n- item\n";
        let article = parse_markdown(markdown);
        let heading = &article.blocks[0];
        assert_eq!(heading.text, "Heading");
        let source = article.markdown_sources[0].as_ref().unwrap();
        assert_eq!(&markdown[source.start..source.start + 2], "##");
        let item = article
            .blocks
            .iter()
            .position(|block| block.kind == BlockKind::ListItem)
            .unwrap();
        let source = article.markdown_sources[item].as_ref().unwrap();
        assert_eq!(&markdown[source.start..source.start + 1], "-");
    }

    #[test]
    fn wraps_with_character_ranges() {
        let lines = wrap_block("one two three four", 9, "", "");
        let texts: Vec<&str> = lines.iter().map(|l| l.2.as_str()).collect();
        assert_eq!(texts, vec!["one two", "three", "four"]);
        assert_eq!(lines[0].0..lines[0].1, 0..7);
        assert_eq!(lines[1].0..lines[1].1, 8..13);
        assert_eq!(lines[2].0..lines[2].1, 14..18);
        // Every reported range matches the block text.
        let text = "one two three four";
        let chars: Vec<char> = text.chars().collect();
        for (start, end, _, _) in &lines {
            let slice: String = chars[*start..*end].iter().collect();
            assert!(!slice.trim().is_empty());
        }

        let indented = wrap_block("alpha beta gamma", 8, "│ ", "│ ");
        assert_eq!(indented[0].2, "│ alpha");
        assert_eq!(indented[0].3, 2);
        assert!(indented.iter().all(|line| line.2.width() <= 8));
    }

    #[test]
    fn maps_block_characters_to_source_offsets() {
        let article = parse_article(
            "<section><p>\n  Hello <strong>world</strong>!\n</p></section>",
        );
        let block = article
            .blocks
            .iter()
            .position(|block| block.text == "Hello world!")
            .unwrap();
        let source = article.sources[block].as_ref().unwrap();
        assert_eq!(source.path, "section[1]/p[1]");
        assert_eq!(source.offsets.len(), "Hello world!".len());
        // "world" starts after the leading whitespace and "Hello ".
        let start = source.offsets[6];
        let end = source.offsets[10] + 1;
        let raw = "\n  Hello world!\n";
        let chars: Vec<char> = raw.chars().collect();
        let selected: String = chars[start..end].iter().collect();
        assert_eq!(selected, "world");
    }

    #[test]
    fn records_annotation_sources() {
        let html = r#"<section>
            <h1>Title</h1>
            <p>Hello <strong>world</strong>!</p>
            <ul><li>a</li><li>b</li><li>c</li><li>target text</li></ul>
        </section>"#;
        let article = parse_article(html);

        let paragraph = article
            .blocks
            .iter()
            .position(|block| block.text.starts_with("Hello"))
            .unwrap();
        let source = article.sources[paragraph].as_ref().unwrap();
        assert_eq!(source.path, "section[1]/p[1]");
        assert_eq!(source.len, "Hello world!".chars().count());

        let item = article
            .blocks
            .iter()
            .position(|block| block.text.contains("target"))
            .unwrap();
        let source = article.sources[item].as_ref().unwrap();
        assert_eq!(source.path, "section[1]/ul[1]/li[4]");
        assert_eq!(source.len, "target text".chars().count());

        let heading = article
            .blocks
            .iter()
            .position(|block| block.kind == BlockKind::Heading1)
            .unwrap();
        assert_eq!(
            article.sources[heading].as_ref().unwrap().path,
            "section[1]/h1[1]"
        );
    }

    #[test]
    fn extracts_structure() {
        let html = r##"
          <div class="article">
            <h1>Main title</h1>
            <p>Hello <strong>world</strong>! This is a paragraph.</p>
            <h2>Section</h2>
            <ul><li>First item</li><li>Second <a href="#">item</a></li></ul>
            <blockquote><p>A quote.</p></blockquote>
            <pre><code>fn main() {
    println!("hi");
}</code></pre>
            <script>bad()</script>
          </div>
        "##;
        let article = parse_article(html);
        assert_eq!(
            article.blocks[0],
            Block {
                kind: BlockKind::Heading1,
                text: "Main title".into()
            }
        );
        assert_eq!(article.blocks[1].text, "Hello world! This is a paragraph.");
        assert_eq!(article.blocks[2].kind, BlockKind::Heading2);
        assert!(article.blocks.iter().any(|b| b.text == "• First item"));
        assert!(article.blocks.iter().any(|b| b.text == "• Second item"));
        assert!(
            article
                .blocks
                .iter()
                .any(|b| b.kind == BlockKind::Quote && b.text == "A quote.")
        );
        assert!(article.blocks.iter().any(|b| b.kind == BlockKind::Code));
        assert!(!article.blocks.iter().any(|b| b.text.contains("bad()")));
    }

    #[test]
    fn layout_wraps_and_separates() {
        let article = Article::from_text_blocks(vec![
            ("Section", "A heading".into()),
            ("", "One two three four five six seven eight nine ten eleven twelve.".into()),
        ]);
        let lines = article.layout(24);
        assert!(lines.iter().all(|line| line.text.chars().count() <= 26));
        let text = lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("A heading"));
        assert!(text.contains("One two three four"));

        for line in &lines {
            if line.kind.is_some() {
                assert!(!line.text.trim().is_empty());
            }
        }
    }

    #[test]
    fn empty_html_is_empty() {
        assert!(parse_article("").blocks.is_empty());
        assert!(parse_article("<p>   </p>").blocks.is_empty());
    }

    #[test]
    fn captures_and_renders_images() {
        let article = parse_article(
            r#"<p>Before</p><img src="/bm/aa/bb/img/pic.png" alt="A picture"><p>After</p>"#,
        );
        let index = article
            .blocks
            .iter()
            .position(|block| block.kind == BlockKind::Image)
            .unwrap();
        assert_eq!(article.blocks[index].text, "A picture");
        assert_eq!(article.images[&index], "/bm/aa/bb/img/pic.png");

        let placeholder = article.layout(40);
        assert!(
            placeholder
                .iter()
                .any(|line| line.text.contains("[image: A picture]"))
        );

        let image = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            4,
            4,
            image::Rgba([255, 0, 0, 255]),
        ));
        let mut images = HashMap::new();
        images.insert("/bm/aa/bb/img/pic.png".to_string(), image);
        let lines = article.layout_with_images(40, &images, &HashMap::new());
        let image_lines: Vec<_> =
            lines.iter().filter(|line| line.image.is_some()).collect();
        assert!(!image_lines.is_empty());
        assert!(
            image_lines[0]
                .image
                .as_ref()
                .unwrap()
                .iter()
                .all(|cell| cell.top == [255, 0, 0, 255])
        );
    }

    #[test]
    fn reserves_rows_for_graphics() {
        let article = parse_article(
            r#"<p>Before</p><img src="/pic.png" alt="A picture"><p>After</p>"#,
        );
        let index = article
            .blocks
            .iter()
            .position(|block| block.kind == BlockKind::Image)
            .unwrap();

        let lines =
            article.layout_with_graphics(40, &HashMap::new(), &HashMap::new());
        assert!(
            lines
                .iter()
                .any(|line| line.text.contains("[image: A picture]"))
        );

        let mut sizes = HashMap::new();
        sizes.insert("/pic.png".to_string(), Size::new(10, 3));
        let lines = article.layout_with_graphics(40, &sizes, &HashMap::new());
        let rows: Vec<_> = lines
            .iter()
            .filter(|line| line.block == Some(index))
            .collect();
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|line| {
            line.kind == Some(BlockKind::Image)
                && line.image.is_none()
                && line.text == " ".repeat(10)
        }));
        assert_eq!(lines.last().unwrap().text, "After");
    }

    #[test]
    fn table_rows_become_text() {
        let article = parse_article(
            "<table><tr><th>Name</th><th>Value</th></tr><tr><td>a</td><td>b</td></tr></table>",
        );
        assert!(article.blocks.iter().any(|b| b.text == "Name  ·  Value"));
        assert!(article.blocks.iter().any(|b| b.text == "a  ·  b"));
    }
}
