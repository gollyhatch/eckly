//! Conversion of a Readeck article HTML fragment into readable text blocks.
//!
//! Readeck serves only the article body (not a complete page), but that body
//! still contains lots of structural markup. This module walks the DOM and
//! turns paragraphs, headings, lists, quotes and code into blocks that the
//! reader can wrap and style.

use std::collections::HashMap;

use ego_tree::NodeRef;
use image::DynamicImage;
use scraper::{Html, Node};

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
}

#[derive(Debug, Clone, Default)]
pub struct Article {
    pub blocks: Vec<Block>,
    /// Maps an image block's index to its source URL.
    pub images: HashMap<usize, String>,
    /// Annotation source for each block, when it can be computed.
    pub sources: Vec<Option<Source>>,
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
}

impl Article {
    /// Wrap the article into lines that fit `width` terminal columns.
    pub fn layout(&self, width: usize) -> Vec<WrappedLine> {
        self.layout_with_images(width, &HashMap::new(), &HashMap::new())
    }

    /// Wrap the article and replace image blocks with either rendered image
    /// rows or placeholders while images are loading or unavailable.
    pub fn layout_with_images(
        &self,
        width: usize,
        images: &HashMap<String, DynamicImage>,
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
                });
            }

            if block.kind == BlockKind::Image {
                let url = self.images.get(&index);
                if let Some(rows) = url
                    .and_then(|url| images.get(url))
                    .map(|image| render_image(image, width))
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
            let options = textwrap::Options::new(width)
                .initial_indent(initial_indent)
                .subsequent_indent(subsequent_indent)
                .break_words(true);

            for line in textwrap::wrap(&block.text, options) {
                lines.push(WrappedLine {
                    text: line.into_owned(),
                    kind: Some(block.kind),
                    image: None,
                    block: Some(index),
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
        }
    }
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
        .map(|index| index.map(|i| collector.sources[i].clone()))
        .collect();
    Article {
        blocks: collector.blocks,
        images: collector.images,
        sources,
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
    sources: Vec<Source>,
    buffer: String,
    images: HashMap<usize, String>,
}

impl<'a> Collector<'a> {
    fn new(body: NodeRef<'a, Node>) -> Self {
        Self {
            body,
            blocks: Vec::new(),
            block_sources: Vec::new(),
            sources: Vec::new(),
            buffer: String::new(),
            images: HashMap::new(),
        }
    }

    fn push_block(&mut self, block: Block, source: Option<usize>) {
        self.blocks.push(block);
        self.block_sources.push(source);
    }

    fn source_for(&mut self, node: NodeRef<'_, Node>) -> Option<usize> {
        let path = selector_for(self.body, node)?;
        let len = text_content(node).chars().count();
        if len == 0 {
            return None;
        }
        self.sources.push(Source { path, len });
        Some(self.sources.len() - 1)
    }

    fn walk(&mut self, node: NodeRef<'_, Node>, context: Context) {
        if let Some(text) = node.value().as_text() {
            self.buffer.push_str(&text.text);
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
            | "video" | "audio" | "canvas" | "object" | "embed" => (),
            "br" => {
                self.buffer.push(' ');
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
                    );
                    let index = self.blocks.len() - 1;
                    self.images.insert(index, src.to_string());
                } else if !alt.is_empty() {
                    self.buffer.push(' ');
                    self.buffer.push_str(&alt);
                    self.buffer.push(' ');
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
                );
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.flush(context);
                let text = normalize(&text_content(node));
                if !text.is_empty() {
                    let kind = match name {
                        "h1" => BlockKind::Heading1,
                        "h2" => BlockKind::Heading2,
                        _ => BlockKind::Heading3,
                    };
                    let source = self.source_for(node);
                    self.push_block(Block { kind, text }, source);
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
                        );
                    } else {
                        self.push_block(
                            Block {
                                kind: BlockKind::Code,
                                text: line.to_string(),
                            },
                            None,
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
                let text = normalize(&text_content(node));
                if !text.is_empty() {
                    let source = self.source_for(node);
                    self.push_block(
                        Block {
                            kind: BlockKind::Caption,
                            text,
                        },
                        source,
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
        let text = normalize(&self.buffer);
        self.buffer.clear();
        if text.is_empty() {
            return;
        }

        let (kind, text) = if let Some(prefix) = context.prefix {
            (BlockKind::ListItem, format!("{prefix}{text}"))
        } else if context.quote {
            (BlockKind::Quote, text)
        } else {
            (BlockKind::Paragraph, text)
        };
        self.push_block(Block { kind, text }, context.source);
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
    fn table_rows_become_text() {
        let article = parse_article(
            "<table><tr><th>Name</th><th>Value</th></tr><tr><td>a</td><td>b</td></tr></table>",
        );
        assert!(article.blocks.iter().any(|b| b.text == "Name  ·  Value"));
        assert!(article.blocks.iter().any(|b| b.text == "a  ·  b"));
    }
}
