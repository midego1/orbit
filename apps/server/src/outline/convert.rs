//! Outline's ProseMirror documents → Orbit (BlockNote) blocks.
//!
//! Node and mark names follow Outline's schema (`shared/editor/nodes`, `shared/editor/marks`):
//! `paragraph`, `heading`, `bullet_list`, `ordered_list`, `checkbox_list` (`checkbox_item`),
//! `list_item`, `blockquote`, `code_fence`/`code_block`, `hr`, `table` (`tr`, `td`, `th`),
//! `container_notice`, `container_toggle`, `math_block`, `math_inline`, `image` (inline in current
//! Outline, a block in older documents), `attachment`, `video`, `embed`, `mention`, `emoji`, `br`;
//! marks `strong`, `em`, `underline`, `strikethrough`, `code_inline`, `highlight`, `link`
//! (`comment` and `placeholder` marks are dropped).
//!
//! Attachments, internal links and mentions go through an [`OutlineResolver`]. Whatever cannot
//! be carried over exactly is recorded as a [`Warning`]; nothing panics on unexpected input.

use std::fmt;

use serde_json::{Map, Value, json};

use super::model::{Attrs, OutlineLink, PmMark, PmNode, attachment_id, outline_link};

/// How deep blocks may nest (Orbit's editor limit is deeper; this keeps hostile input bounded).
const MAX_DEPTH: usize = 24;
/// The emoji of a callout per Outline notice style.
const NOTICE_STYLES: &[(&str, &str, &str)] = &[
    ("info", "ℹ️", "blue"),
    ("warning", "⚠️", "yellow"),
    ("tip", "⭐", "purple"),
    ("success", "✅", "green"),
];

/// An attachment as Orbit stores it after the upload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedFile {
    pub url: String,
    pub name: String,
    pub is_image: bool,
}

/// A workspace member a mention points to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedMember {
    pub user_id: String,
    pub name: String,
}

/// Looks up what Outline references point to in Orbit.
pub trait OutlineResolver {
    /// The uploaded file of an Outline attachment id; `None` when it could not be imported.
    fn attachment(&self, attachment_id: &str) -> Option<ResolvedFile>;
    /// The Orbit page href (`/docs/<id>`) of an Outline document (by id or `urlId`).
    fn document(&self, key: &str) -> Option<String>;
    /// The Orbit page href of an imported collection (by id or `urlId`).
    fn collection(&self, key: &str) -> Option<String>;
    /// The member an Outline user id stands for.
    fn member(&self, outline_user_id: &str) -> Option<ResolvedMember>;
}

/// What could not be carried over exactly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Warning {
    /// A node type Orbit has no block for; its text (if any) was kept.
    UnsupportedBlock(String),
    /// A block converted with a loss (for example a list inside a table cell).
    Lossy(String),
    /// An attachment or image that could not be imported; `detail` names it.
    AttachmentFailed(String),
    /// A link to an Outline document or collection that is not imported; it points to Outline.
    UnresolvedLink(String),
    /// A mention of a person without a member here, or of a document that is not imported;
    /// it became text.
    UnresolvedMention(String),
}

impl Warning {
    /// The `kind` stored and shown in the warning list.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::UnsupportedBlock(_) => "unsupported_block",
            Self::Lossy(_) => "lossy_conversion",
            Self::AttachmentFailed(_) => "attachment_failed",
            Self::UnresolvedLink(_) => "unresolved_link",
            Self::UnresolvedMention(_) => "unresolved_mention",
        }
    }

    #[must_use]
    pub fn detail(&self) -> &str {
        match self {
            Self::UnsupportedBlock(detail)
            | Self::Lossy(detail)
            | Self::AttachmentFailed(detail)
            | Self::UnresolvedLink(detail)
            | Self::UnresolvedMention(detail) => detail,
        }
    }
}

impl fmt::Display for Warning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.kind(), self.detail())
    }
}

/// One conversion: the resolver, the block id source and the warnings found.
pub struct Converter<'a> {
    resolver: &'a dyn OutlineResolver,
    next_id: Box<dyn FnMut() -> String + 'a>,
    warnings: Vec<Warning>,
}

impl<'a> Converter<'a> {
    #[must_use]
    pub fn new(resolver: &'a dyn OutlineResolver) -> Self {
        Self {
            resolver,
            next_id: Box::new(|| orbit_platform::Id::new_v7().to_string()),
            warnings: Vec::new(),
        }
    }

    /// Uses `next_id` for block ids (tests use a counter).
    #[must_use]
    pub fn with_ids(mut self, next_id: impl FnMut() -> String + 'a) -> Self {
        self.next_id = Box::new(next_id);
        self
    }

    /// Converts a `doc` node (or any node: its children are converted).
    pub fn convert(&mut self, doc: &PmNode) -> Vec<Value> {
        let mut out = Vec::new();
        self.blocks(&doc.content, 0, &mut out);
        out
    }

    #[must_use]
    pub fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    #[must_use]
    pub fn into_warnings(self) -> Vec<Warning> {
        self.warnings
    }

    fn warn(&mut self, warning: Warning) {
        self.warnings.push(warning);
    }

    fn block(
        &mut self,
        kind: &str,
        props: Map<String, Value>,
        content: Option<Vec<Value>>,
        children: Vec<Value>,
    ) -> Value {
        let mut block = Map::new();
        block.insert("id".into(), json!((self.next_id)()));
        block.insert("type".into(), json!(kind));
        block.insert("props".into(), Value::Object(props));
        if let Some(content) = content {
            block.insert("content".into(), Value::Array(content));
        }
        block.insert("children".into(), Value::Array(children));
        Value::Object(block)
    }

    fn blocks(&mut self, nodes: &[PmNode], depth: usize, out: &mut Vec<Value>) {
        for node in nodes {
            self.node(node, depth, out);
        }
    }

    fn children(&mut self, nodes: &[PmNode], depth: usize) -> Vec<Value> {
        let mut out = Vec::new();
        if depth >= MAX_DEPTH {
            // Too deep to nest: keep the content flat instead of dropping it.
            self.warn(Warning::Lossy("nesting too deep, flattened".to_owned()));
            for node in nodes {
                self.flat_text(node, &mut out);
            }
            return out;
        }
        self.blocks(nodes, depth + 1, &mut out);
        out
    }

    /// A paragraph with the node's text, for content too deep to nest.
    fn flat_text(&mut self, node: &PmNode, out: &mut Vec<Value>) {
        let text = node.plain_text();
        if !text.trim().is_empty() {
            let block = self.block("paragraph", text_props(), Some(vec![plain(text)]), Vec::new());
            out.push(block);
        }
    }

    fn node(&mut self, node: &PmNode, depth: usize, out: &mut Vec<Value>) {
        match node.kind.as_str() {
            "paragraph" => self.text_block("paragraph", text_props(), node, out),
            "heading" => {
                let level = node.attr_u64("level").unwrap_or(1).clamp(1, 6);
                let mut props = text_props();
                props.insert("level".into(), json!(level));
                self.text_block("heading", props, node, out);
            }
            "bullet_list" => self.list(node, "bulletListItem", depth, out),
            "ordered_list" => self.list(node, "numberedListItem", depth, out),
            "checkbox_list" => self.list(node, "checkListItem", depth, out),
            // A list item outside its list (malformed input): treat it as a bullet.
            "list_item" | "checkbox_item" => {
                let kind = if node.kind == "checkbox_item" {
                    "checkListItem"
                } else {
                    "bulletListItem"
                };
                self.list_item(node, kind, None, depth, out);
            }
            "blockquote" => {
                let mut props = Map::new();
                props.insert("textColor".into(), json!("default"));
                props.insert("backgroundColor".into(), json!("default"));
                self.container("quote", props, &node.content, depth, out);
            }
            "container_notice" | "notice" => {
                let style = node.attr_str("style").unwrap_or("info");
                let (emoji, color) = NOTICE_STYLES
                    .iter()
                    .find(|(name, _, _)| *name == style)
                    .map_or(("💡", "gray"), |(_, emoji, color)| (*emoji, *color));
                let mut props = Map::new();
                props.insert("emoji".into(), json!(emoji));
                props.insert("backgroundColor".into(), json!(color));
                props.insert("textColor".into(), json!("default"));
                self.container("callout", props, &node.content, depth, out);
            }
            "container_toggle" => {
                self.container("toggleListItem", text_props(), &node.content, depth, out);
            }
            "code_fence" | "code_block" => {
                let language = code_language(node.attr_str("language").unwrap_or(""));
                let source = node.plain_text();
                let mut props = Map::new();
                props.insert("language".into(), json!(language));
                let content = if source.is_empty() {
                    Vec::new()
                } else {
                    vec![plain(source)]
                };
                let block = self.block("codeBlock", props, Some(content), Vec::new());
                out.push(block);
            }
            "hr" | "horizontal_rule" => {
                if node.attr_str("markup") == Some("***") {
                    // A page break in Outline; Orbit has only the divider.
                    self.warn(Warning::Lossy("page break as divider".to_owned()));
                }
                let block = self.block("divider", Map::new(), None, Vec::new());
                out.push(block);
            }
            "table" => self.table(node, out),
            "math_block" => {
                let latex = node.plain_text();
                let block = self.block(
                    "paragraph",
                    text_props(),
                    Some(vec![math(latex.trim())]),
                    Vec::new(),
                );
                out.push(block);
            }
            "image" => {
                if let Some(block) = self.image(node) {
                    out.push(block);
                }
            }
            "attachment" | "video" => {
                if let Some(block) = self.attachment(node) {
                    out.push(block);
                }
            }
            "embed" => {
                let href = node.attr_str("href").unwrap_or_default().to_owned();
                if href.is_empty() {
                    self.warn(Warning::UnsupportedBlock("embed without a URL".to_owned()));
                    return;
                }
                let block = self.block(
                    "paragraph",
                    text_props(),
                    Some(vec![link(&href, vec![plain(href.clone())])]),
                    Vec::new(),
                );
                out.push(block);
            }
            // Inline nodes at block level (malformed input): wrap them in a paragraph.
            "text" | "mention" | "math_inline" | "emoji" | "br" => {
                let inline = PmNode {
                    kind: "paragraph".to_owned(),
                    content: vec![node.clone()],
                    ..PmNode::default()
                };
                self.text_block("paragraph", text_props(), &inline, out);
            }
            "doc" => self.blocks(&node.content, depth, out),
            other => {
                self.warn(Warning::UnsupportedBlock(if other.is_empty() {
                    "(untyped node)".to_owned()
                } else {
                    other.to_owned()
                }));
                // Keep what can be kept: nested blocks, or the node's text.
                if node.content.iter().any(is_block) {
                    self.blocks(&node.content, depth, out);
                } else {
                    self.flat_text(node, out);
                }
            }
        }
    }

    /// A paragraph or heading; images inside it become image blocks after it.
    fn text_block(
        &mut self,
        kind: &str,
        props: Map<String, Value>,
        node: &PmNode,
        out: &mut Vec<Value>,
    ) {
        let mut after = Vec::new();
        let content = self.inline(&node.content, &mut after);
        // A paragraph that only held images is replaced by them.
        if !(content.is_empty() && !after.is_empty() && kind == "paragraph") {
            let block = self.block(kind, props, Some(content), Vec::new());
            out.push(block);
        }
        out.extend(after);
    }

    /// A block whose first paragraph (or heading) is its own text and whose other blocks nest
    /// under it (quote, callout, toggle, list item).
    fn container(
        &mut self,
        kind: &str,
        props: Map<String, Value>,
        content: &[PmNode],
        depth: usize,
        out: &mut Vec<Value>,
    ) {
        let (first, rest) = match content.split_first() {
            Some((first, rest)) if matches!(first.kind.as_str(), "paragraph" | "heading") => {
                (Some(first), rest)
            }
            _ => (None, content),
        };
        let mut after = Vec::new();
        let inline = first
            .map(|first| self.inline(&first.content, &mut after))
            .unwrap_or_default();
        let mut children = after;
        children.extend(self.children(rest, depth));
        let block = self.block(kind, props, Some(inline), children);
        out.push(block);
    }

    fn list(&mut self, node: &PmNode, kind: &str, depth: usize, out: &mut Vec<Value>) {
        let start = (kind == "numberedListItem")
            .then(|| node.attr_u64("order"))
            .flatten()
            .filter(|start| *start != 1);
        for (index, item) in node.content.iter().enumerate() {
            match item.kind.as_str() {
                "list_item" | "checkbox_item" => {
                    self.list_item(item, kind, start.filter(|_| index == 0), depth, out);
                }
                // Anything else inside a list keeps its own conversion.
                _ => self.node(item, depth, out),
            }
        }
    }

    fn list_item(
        &mut self,
        item: &PmNode,
        kind: &str,
        start: Option<u64>,
        depth: usize,
        out: &mut Vec<Value>,
    ) {
        let mut props = text_props();
        if kind == "checkListItem" {
            props.insert("checked".into(), json!(item.attr_bool("checked")));
        }
        if let Some(start) = start {
            props.insert("start".into(), json!(start));
        }
        self.container(kind, props, &item.content, depth, out);
    }

    fn table(&mut self, node: &PmNode, out: &mut Vec<Value>) {
        let rows: Vec<&PmNode> = node.content.iter().filter(|row| row.kind == "tr").collect();
        if rows.is_empty() {
            self.warn(Warning::Lossy("empty table".to_owned()));
            return;
        }
        let width = rows
            .iter()
            .map(|row| {
                row.content
                    .iter()
                    .map(|cell| cell.attr_u64("colspan").unwrap_or(1).max(1))
                    .sum::<u64>()
            })
            .max()
            .unwrap_or(1)
            .max(1);
        let width = usize::try_from(width).unwrap_or(1).min(256);
        let header_row = rows[0].content.iter().all(|cell| cell.kind == "th");
        let header_col = rows
            .iter()
            .all(|row| row.content.first().is_some_and(|cell| cell.kind == "th"))
            && !header_row;
        let mut column_widths: Vec<Value> = vec![Value::Null; width];
        let mut table_rows = Vec::with_capacity(rows.len());
        for row in rows {
            let mut cells = Vec::new();
            for (index, cell) in row.content.iter().enumerate() {
                if let Some(widths) = cell.attrs.get("colwidth").and_then(Value::as_array)
                    && let Some(first) = widths.first().and_then(Value::as_u64)
                    && let Some(slot) = column_widths.get_mut(index)
                    && slot.is_null()
                {
                    *slot = json!(first);
                }
                cells.push(self.table_cell(cell));
            }
            table_rows.push(json!({"cells": cells}));
        }
        let mut content = Map::new();
        content.insert("type".into(), json!("tableContent"));
        content.insert("columnWidths".into(), Value::Array(column_widths));
        if header_row {
            content.insert("headerRows".into(), json!(1));
        }
        if header_col {
            content.insert("headerCols".into(), json!(1));
        }
        content.insert("rows".into(), Value::Array(table_rows));
        let mut props = Map::new();
        props.insert("textColor".into(), json!("default"));
        let mut block = self.block("table", props, None, Vec::new());
        if let Some(object) = block.as_object_mut() {
            object.insert("content".into(), Value::Object(content));
        }
        out.push(block);
    }

    /// A table cell holds inline content only: its paragraphs are joined by line breaks, other
    /// blocks keep their text (and count as a lossy conversion).
    fn table_cell(&mut self, cell: &PmNode) -> Value {
        let mut content: Vec<Value> = Vec::new();
        for (index, block) in cell.content.iter().enumerate() {
            if index > 0 {
                content.push(plain("\n"));
            }
            match block.kind.as_str() {
                "paragraph" | "heading" => {
                    let mut after = Vec::new();
                    content.extend(self.inline(&block.content, &mut after));
                    if !after.is_empty() {
                        self.warn(Warning::Lossy("image inside a table cell".to_owned()));
                    }
                }
                other => {
                    self.warn(Warning::Lossy(format!("{other} inside a table cell")));
                    content.push(plain(block.plain_text()));
                }
            }
        }
        let alignment = match cell.attr_str("alignment") {
            Some("center") => "center",
            Some("right") => "right",
            _ => "left",
        };
        json!({
            "type": "tableCell",
            "props": {
                "backgroundColor": "default",
                "textColor": "default",
                "textAlignment": alignment,
                "colspan": cell.attr_u64("colspan").unwrap_or(1).clamp(1, 256),
                "rowspan": cell.attr_u64("rowspan").unwrap_or(1).clamp(1, 1024),
            },
            "content": merge_text(content),
        })
    }

    /// An image node → an image block (uploaded file) or, for an outside URL, a link.
    fn image(&mut self, node: &PmNode) -> Option<Value> {
        let src = node.attr_str("src")?.to_owned();
        let alt = node
            .attr_str("alt")
            .map(str::to_owned)
            .unwrap_or_else(|| node.plain_text());
        let caption = node.plain_text();
        if let Some(id) = attachment_id(&src) {
            return match self.resolver.attachment(&id) {
                Some(file) => {
                    let mut props = Map::new();
                    props.insert("backgroundColor".into(), json!("default"));
                    props.insert("textAlignment".into(), json!("left"));
                    props.insert("name".into(), json!(if alt.is_empty() { file.name } else { alt }));
                    props.insert("url".into(), json!(file.url));
                    props.insert("caption".into(), json!(caption));
                    props.insert("showPreview".into(), json!(true));
                    Some(self.block("image", props, None, Vec::new()))
                }
                None => {
                    self.warn(Warning::AttachmentFailed(format!("image {id}")));
                    None
                }
            };
        }
        if src.starts_with("data:") {
            self.warn(Warning::Lossy("inline data image dropped".to_owned()));
            return None;
        }
        // An image on another site: Orbit does not fetch it; keep a link to it.
        self.warn(Warning::Lossy(format!("external image kept as a link: {src}")));
        let label = if alt.trim().is_empty() {
            src.clone()
        } else {
            alt
        };
        Some(self.block(
            "paragraph",
            text_props(),
            Some(vec![link(&src, vec![plain(label)])]),
            Vec::new(),
        ))
    }

    /// An attachment or video node → a file block (or an image block for an image file).
    fn attachment(&mut self, node: &PmNode) -> Option<Value> {
        let href = node
            .attr_str("href")
            .or_else(|| node.attr_str("src"))
            .unwrap_or_default()
            .to_owned();
        let title = node
            .attr_str("title")
            .map(str::to_owned)
            .unwrap_or_default();
        if node.kind == "video" {
            self.warn(Warning::Lossy("video as file".to_owned()));
        }
        match attachment_id(&href) {
            Some(id) => match self.resolver.attachment(&id) {
                Some(file) => {
                    let name = if title.is_empty() { file.name } else { title };
                    let mut props = Map::new();
                    props.insert("backgroundColor".into(), json!("default"));
                    props.insert("name".into(), json!(name));
                    props.insert("url".into(), json!(file.url));
                    props.insert("caption".into(), json!(""));
                    if file.is_image {
                        props.insert("textAlignment".into(), json!("left"));
                        props.insert("showPreview".into(), json!(true));
                        Some(self.block("image", props, None, Vec::new()))
                    } else {
                        Some(self.block("file", props, None, Vec::new()))
                    }
                }
                None => {
                    self.warn(Warning::AttachmentFailed(if title.is_empty() {
                        id
                    } else {
                        format!("{title} ({id})")
                    }));
                    (!title.is_empty()).then(|| {
                        self.block("paragraph", text_props(), Some(vec![plain(title)]), Vec::new())
                    })
                }
            },
            None if !href.is_empty() => {
                let label = if title.is_empty() { href.clone() } else { title };
                Some(self.block(
                    "paragraph",
                    text_props(),
                    Some(vec![link(&href, vec![plain(label)])]),
                    Vec::new(),
                ))
            }
            None => {
                self.warn(Warning::AttachmentFailed(if title.is_empty() {
                    "attachment without a URL".to_owned()
                } else {
                    title
                }));
                None
            }
        }
    }

    /// Inline nodes → BlockNote inline content. Images found inline are converted to blocks and
    /// pushed to `after`.
    fn inline(&mut self, nodes: &[PmNode], after: &mut Vec<Value>) -> Vec<Value> {
        let mut out: Vec<Value> = Vec::new();
        for node in nodes {
            match node.kind.as_str() {
                "text" => {
                    let text = node.text.clone().unwrap_or_default();
                    if text.is_empty() {
                        continue;
                    }
                    let (styles, href) = self.marks(&node.marks);
                    let item = json!({"type": "text", "text": text, "styles": Value::Object(styles)});
                    push_linked(&mut out, item, href);
                }
                "br" | "hard_break" => out.push(plain("\n")),
                "math_inline" => out.push(math(node.plain_text().trim())),
                "emoji" => {
                    let text = node.plain_text();
                    if text.is_empty() {
                        let name = node.attr_str("data-name").unwrap_or("emoji");
                        self.warn(Warning::Lossy(format!("emoji :{name}: kept as text")));
                        out.push(plain(format!(":{name}:")));
                    } else {
                        out.push(plain(text));
                    }
                }
                "mention" => out.push(self.mention(node)),
                "image" => {
                    if let Some(block) = self.image(node) {
                        after.push(block);
                    }
                }
                "attachment" | "video" => {
                    if let Some(block) = self.attachment(node) {
                        after.push(block);
                    }
                }
                other => {
                    self.warn(Warning::UnsupportedBlock(format!("inline {other}")));
                    let text = node.plain_text();
                    if !text.is_empty() {
                        out.push(plain(text));
                    }
                }
            }
        }
        merge_text(out)
    }

    /// Text styles and the link target of a text node's marks.
    fn marks(&mut self, marks: &[PmMark]) -> (Map<String, Value>, Option<String>) {
        let mut styles = Map::new();
        let mut href = None;
        for mark in marks {
            match mark.kind.as_str() {
                "strong" | "bold" => {
                    styles.insert("bold".into(), json!(true));
                }
                "em" | "italic" => {
                    styles.insert("italic".into(), json!(true));
                }
                "underline" => {
                    styles.insert("underline".into(), json!(true));
                }
                "strikethrough" | "strike" => {
                    styles.insert("strike".into(), json!(true));
                }
                "code_inline" | "code" => {
                    styles.insert("code".into(), json!(true));
                }
                "highlight" | "mark" => {
                    styles.insert("backgroundColor".into(), json!("yellow"));
                }
                "link" => {
                    if let Some(target) = mark.attr_str("href") {
                        href = Some(self.link_href(target));
                    }
                }
                // Comments and editor placeholders have no meaning outside Outline.
                "comment" | "placeholder" => {}
                other => self.warn(Warning::Lossy(format!("text style {other}"))),
            }
        }
        (styles, href)
    }

    /// An Outline-internal link → the imported page; anything else stays as written.
    fn link_href(&mut self, href: &str) -> String {
        if let Some(id) = attachment_id(href) {
            return match self.resolver.attachment(&id) {
                Some(file) => file.url,
                None => {
                    self.warn(Warning::AttachmentFailed(format!("linked file {id}")));
                    href.to_owned()
                }
            };
        }
        match outline_link(href) {
            // Outline heading anchors (`#…`) do not exist in Orbit: the link opens the page.
            Some(OutlineLink::Document(key)) => match self.resolver.document(&key) {
                Some(page) => page,
                None => {
                    self.warn(Warning::UnresolvedLink(href.to_owned()));
                    href.to_owned()
                }
            },
            Some(OutlineLink::Collection(key)) => match self.resolver.collection(&key) {
                Some(page) => page,
                None => {
                    self.warn(Warning::UnresolvedLink(href.to_owned()));
                    href.to_owned()
                }
            },
            None => href.to_owned(),
        }
    }

    /// A mention: a member mention, a link to an imported page, or text.
    fn mention(&mut self, node: &PmNode) -> Value {
        let label = node
            .attr_str("label")
            .map(str::to_owned)
            .unwrap_or_else(|| node.plain_text());
        let model = node.attr_str("modelId").unwrap_or_default().to_owned();
        match node.attr_str("type").unwrap_or("user") {
            "user" => match self.resolver.member(&model) {
                Some(member) => {
                    json!({"type": "mention", "props": {"userId": member.user_id, "name": member.name}})
                }
                None => {
                    self.warn(Warning::UnresolvedMention(format!("@{label}")));
                    plain(format!("@{label}"))
                }
            },
            "document" => match self.resolver.document(&model) {
                Some(href) => link(&href, vec![plain(label)]),
                None => {
                    self.warn(Warning::UnresolvedMention(format!("document {label}")));
                    plain(label)
                }
            },
            "collection" => match self.resolver.collection(&model) {
                Some(href) => link(&href, vec![plain(label)]),
                None => {
                    self.warn(Warning::UnresolvedMention(format!("collection {label}")));
                    plain(label)
                }
            },
            other => match node.attr_str("href") {
                // Issues, pull requests, URLs and other integrations carry their URL.
                Some(href) => link(href, vec![plain(label)]),
                None => {
                    self.warn(Warning::UnresolvedMention(format!("{other} {label}")));
                    plain(label)
                }
            },
        }
    }
}

fn is_block(node: &PmNode) -> bool {
    !matches!(
        node.kind.as_str(),
        "text" | "mention" | "math_inline" | "emoji" | "br" | "hard_break" | "image"
    )
}

fn text_props() -> Map<String, Value> {
    let mut props = Map::new();
    props.insert("textColor".into(), json!("default"));
    props.insert("backgroundColor".into(), json!("default"));
    props.insert("textAlignment".into(), json!("left"));
    props
}

fn plain(text: impl Into<String>) -> Value {
    json!({"type": "text", "text": text.into(), "styles": {}})
}

fn math(latex: &str) -> Value {
    json!({"type": "math", "props": {"latex": latex}})
}

fn link(href: &str, content: Vec<Value>) -> Value {
    json!({"type": "link", "href": href, "content": content})
}

/// Adds a text item, inside a link when `href` is set (joining a link to the same target).
fn push_linked(out: &mut Vec<Value>, item: Value, href: Option<String>) {
    let Some(href) = href else {
        out.push(item);
        return;
    };
    if let Some(last) = out.last_mut()
        && last.get("type").and_then(Value::as_str) == Some("link")
        && last.get("href").and_then(Value::as_str) == Some(href.as_str())
        && let Some(Value::Array(content)) = last.get_mut("content")
    {
        content.push(item);
        return;
    }
    out.push(link(&href, vec![item]));
}

/// Joins neighboring text items with the same styles.
fn merge_text(items: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::with_capacity(items.len());
    for item in items {
        if item.get("type").and_then(Value::as_str) == Some("text")
            && let Some(last) = out.last_mut()
            && last.get("type").and_then(Value::as_str) == Some("text")
            && last.get("styles") == item.get("styles")
            && let (Some(previous), Some(next)) = (
                last.get("text").and_then(Value::as_str).map(str::to_owned),
                item.get("text").and_then(Value::as_str),
            )
        {
            last["text"] = json!(format!("{previous}{next}"));
            continue;
        }
        out.push(item);
    }
    out
}

/// Outline code languages (Prism names) → Orbit's code block languages.
fn code_language(language: &str) -> String {
    let language = language.trim().to_ascii_lowercase();
    match language.as_str() {
        "" | "none" | "plaintext" | "plain" | "text" => "text".to_owned(),
        "mermaidjs" => "mermaid".to_owned(),
        "js" => "javascript".to_owned(),
        "ts" => "typescript".to_owned(),
        "sh" | "shell" => "bash".to_owned(),
        "yml" => "yaml".to_owned(),
        "md" => "markdown".to_owned(),
        "py" => "python".to_owned(),
        "c++" => "cpp".to_owned(),
        "c#" | "cs" => "csharp".to_owned(),
        other => other
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '+' | '#' | '_'))
            .take(40)
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_code_languages() {
        assert_eq!(code_language(""), "text");
        assert_eq!(code_language("JavaScript"), "javascript");
        assert_eq!(code_language("mermaidjs"), "mermaid");
        assert_eq!(code_language("weird lang!"), "weirdlang");
    }
}
