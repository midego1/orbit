//! Outline ProseMirror → Orbit blocks: one fixture per Outline node type (names and attributes
//! as in Outline's `shared/editor/nodes` and `shared/editor/marks`).

use std::cell::Cell;
use std::collections::HashMap;

use orbit_server::outline::convert::{
    Converter, OutlineResolver, ResolvedFile, ResolvedMember, Warning,
};
use orbit_server::outline::model::PmNode;
use serde_json::{Value, json};

const ATTACHMENT: &str = "8d4b9f3e-3c35-4a6e-9a58-0b1c2d3e4f50";
const IMAGE: &str = "11111111-2222-4333-8444-555555555555";
const MISSING: &str = "99999999-2222-4333-8444-555555555555";
const DOC_ID: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

struct Fixture {
    files: HashMap<String, ResolvedFile>,
    documents: HashMap<String, String>,
    collections: HashMap<String, String>,
    members: HashMap<String, ResolvedMember>,
}

impl Fixture {
    fn new() -> Self {
        let mut files = HashMap::new();
        files.insert(
            ATTACHMENT.to_owned(),
            ResolvedFile {
                url: "/files/report.pdf".to_owned(),
                name: "report.pdf".to_owned(),
                is_image: false,
            },
        );
        files.insert(
            IMAGE.to_owned(),
            ResolvedFile {
                url: "/files/diagram.png".to_owned(),
                name: "diagram.png".to_owned(),
                is_image: true,
            },
        );
        let mut documents = HashMap::new();
        documents.insert("AbCdEf1234".to_owned(), "/docs/page-1".to_owned());
        documents.insert(DOC_ID.to_owned(), "/docs/page-1".to_owned());
        let mut collections = HashMap::new();
        collections.insert("Coll123456".to_owned(), "/docs/collection-1".to_owned());
        let mut members = HashMap::new();
        members.insert(
            "user-1".to_owned(),
            ResolvedMember {
                user_id: "member-1".to_owned(),
                name: "Ada".to_owned(),
            },
        );
        Self {
            files,
            documents,
            collections,
            members,
        }
    }
}

impl OutlineResolver for Fixture {
    fn attachment(&self, attachment_id: &str) -> Option<ResolvedFile> {
        self.files.get(attachment_id).cloned()
    }
    fn document(&self, key: &str) -> Option<String> {
        self.documents.get(key).cloned()
    }
    fn collection(&self, key: &str) -> Option<String> {
        self.collections.get(key).cloned()
    }
    fn member(&self, outline_user_id: &str) -> Option<ResolvedMember> {
        self.members.get(outline_user_id).cloned()
    }
}

/// Converts a `doc` with `content` and returns the blocks (ids `b1`, `b2`, …) and warnings.
fn convert(content: Value) -> (Vec<Value>, Vec<Warning>) {
    let doc: PmNode = serde_json::from_value(json!({"type": "doc", "content": content})).unwrap();
    let fixture = Fixture::new();
    let counter = Cell::new(0);
    let mut converter = Converter::new(&fixture).with_ids(|| {
        counter.set(counter.get() + 1);
        format!("b{}", counter.get())
    });
    let blocks = converter.convert(&doc);
    (blocks, converter.into_warnings())
}

fn text(value: &str) -> Value {
    json!({"type": "text", "text": value})
}

fn paragraph(content: Value) -> Value {
    json!({"type": "paragraph", "content": content})
}

#[test]
fn paragraph_with_marks() {
    let (blocks, warnings) = convert(json!([paragraph(json!([
        {"type": "text", "text": "bold", "marks": [{"type": "strong"}]},
        {"type": "text", "text": " italic", "marks": [{"type": "em"}, {"type": "underline"}]},
        {"type": "text", "text": " gone", "marks": [{"type": "strikethrough"}]},
        {"type": "text", "text": " code", "marks": [{"type": "code_inline"}]},
        {"type": "text", "text": " hi", "marks": [{"type": "highlight", "attrs": {"color": "#FDEA9B"}}]},
        {"type": "text", "text": " note", "marks": [{"type": "comment", "attrs": {"id": "x"}}]},
        {"type": "br"},
        text("next line")
    ]))]));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0]["type"], "paragraph");
    let content = blocks[0]["content"].as_array().unwrap();
    assert_eq!(content[0], json!({"type": "text", "text": "bold", "styles": {"bold": true}}));
    assert_eq!(content[1]["styles"], json!({"italic": true, "underline": true}));
    assert_eq!(content[2]["styles"], json!({"strike": true}));
    assert_eq!(content[3]["styles"], json!({"code": true}));
    assert_eq!(content[4]["styles"], json!({"backgroundColor": "yellow"}));
    // The comment mark is dropped; the text and the line break join.
    assert_eq!(content[5], json!({"type": "text", "text": " note\nnext line", "styles": {}}));
}

#[test]
fn headings_keep_their_level() {
    let (blocks, _) = convert(json!([
        {"type": "heading", "attrs": {"level": 1}, "content": [text("One")]},
        {"type": "heading", "attrs": {"level": 3}, "content": [text("Three")]},
        {"type": "heading", "attrs": {"level": 9}, "content": [text("Clamped")]}
    ]));
    assert_eq!(blocks[0]["type"], "heading");
    assert_eq!(blocks[0]["props"]["level"], 1);
    assert_eq!(blocks[1]["props"]["level"], 3);
    assert_eq!(blocks[2]["props"]["level"], 6);
    assert_eq!(blocks[1]["content"][0]["text"], "Three");
}

#[test]
fn bullet_lists_nest() {
    let (blocks, _) = convert(json!([{"type": "bullet_list", "content": [
        {"type": "list_item", "content": [
            paragraph(json!([text("Parent")])),
            {"type": "bullet_list", "content": [
                {"type": "list_item", "content": [paragraph(json!([text("Child")]))]}
            ]}
        ]},
        {"type": "list_item", "content": [paragraph(json!([text("Sibling")]))]}
    ]}]));
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0]["type"], "bulletListItem");
    assert_eq!(blocks[0]["content"][0]["text"], "Parent");
    assert_eq!(blocks[0]["children"][0]["type"], "bulletListItem");
    assert_eq!(blocks[0]["children"][0]["content"][0]["text"], "Child");
    assert_eq!(blocks[1]["content"][0]["text"], "Sibling");
}

#[test]
fn ordered_lists_keep_their_start() {
    let (blocks, _) = convert(json!([{"type": "ordered_list", "attrs": {"order": 3}, "content": [
        {"type": "list_item", "content": [paragraph(json!([text("Third")]))]},
        {"type": "list_item", "content": [paragraph(json!([text("Fourth")]))]}
    ]}]));
    assert_eq!(blocks[0]["type"], "numberedListItem");
    assert_eq!(blocks[0]["props"]["start"], 3);
    assert!(blocks[1]["props"].get("start").is_none());
}

#[test]
fn checklists_keep_checked_state() {
    let (blocks, _) = convert(json!([{"type": "checkbox_list", "content": [
        {"type": "checkbox_item", "attrs": {"checked": true}, "content": [paragraph(json!([text("Done")]))]},
        {"type": "checkbox_item", "attrs": {"checked": false}, "content": [paragraph(json!([text("Open")]))]}
    ]}]));
    assert_eq!(blocks[0]["type"], "checkListItem");
    assert_eq!(blocks[0]["props"]["checked"], true);
    assert_eq!(blocks[1]["props"]["checked"], false);
    assert_eq!(blocks[1]["content"][0]["text"], "Open");
}

#[test]
fn tables_keep_header_rows_alignment_and_spans() {
    let cell = |kind: &str, value: &str, attrs: Value| {
        json!({"type": kind, "attrs": attrs, "content": [paragraph(json!([text(value)]))]})
    };
    let (blocks, warnings) = convert(json!([{"type": "table", "content": [
        {"type": "tr", "content": [
            cell("th", "Name", json!({"colwidth": [120]})),
            cell("th", "Score", json!({"alignment": "right"}))
        ]},
        {"type": "tr", "content": [
            cell("td", "Ada", json!({})),
            {"type": "td", "attrs": {"alignment": "center"}, "content": [
                paragraph(json!([text("1")])),
                paragraph(json!([text("2")]))
            ]}
        ]},
        {"type": "tr", "content": [cell("td", "Wide", json!({"colspan": 2}))]}
    ]}]));
    assert!(warnings.is_empty(), "{warnings:?}");
    let table = &blocks[0];
    assert_eq!(table["type"], "table");
    let content = &table["content"];
    assert_eq!(content["type"], "tableContent");
    assert_eq!(content["headerRows"], 1);
    assert_eq!(content["columnWidths"], json!([120, null]));
    let rows = content["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["cells"][1]["props"]["textAlignment"], "right");
    assert_eq!(rows[1]["cells"][1]["content"][0]["text"], "1\n2");
    assert_eq!(rows[1]["cells"][1]["props"]["textAlignment"], "center");
    assert_eq!(rows[2]["cells"][0]["props"]["colspan"], 2);
}

#[test]
fn code_blocks_keep_their_language() {
    let (blocks, _) = convert(json!([
        {"type": "code_fence", "attrs": {"language": "javascript"}, "content": [text("let a = 1;\nlet b = 2;")]},
        {"type": "code_block", "attrs": {"language": "mermaidjs"}, "content": [text("graph TD;")]},
        {"type": "code_fence", "attrs": {"language": "none"}, "content": []}
    ]));
    assert_eq!(blocks[0]["type"], "codeBlock");
    assert_eq!(blocks[0]["props"]["language"], "javascript");
    assert_eq!(blocks[0]["content"][0]["text"], "let a = 1;\nlet b = 2;");
    assert_eq!(blocks[1]["props"]["language"], "mermaid");
    assert_eq!(blocks[2]["props"]["language"], "text");
    assert_eq!(blocks[2]["content"], json!([]));
}

#[test]
fn notices_become_callouts() {
    let (blocks, _) = convert(json!([
        {"type": "container_notice", "attrs": {"style": "warning"}, "content": [
            paragraph(json!([text("Careful")])),
            paragraph(json!([text("More detail")]))
        ]},
        {"type": "container_notice", "attrs": {"style": "tip"}, "content": [paragraph(json!([text("Tip")]))]},
        {"type": "container_notice", "attrs": {"style": "success"}, "content": [paragraph(json!([text("Yay")]))]},
        {"type": "container_notice", "attrs": {"style": "info"}, "content": [paragraph(json!([text("FYI")]))]}
    ]));
    assert_eq!(blocks[0]["type"], "callout");
    assert_eq!(blocks[0]["props"]["emoji"], "⚠️");
    assert_eq!(blocks[0]["props"]["backgroundColor"], "yellow");
    assert_eq!(blocks[0]["content"][0]["text"], "Careful");
    assert_eq!(blocks[0]["children"][0]["content"][0]["text"], "More detail");
    assert_eq!(blocks[1]["props"]["emoji"], "⭐");
    assert_eq!(blocks[2]["props"]["backgroundColor"], "green");
    assert_eq!(blocks[3]["props"]["emoji"], "ℹ️");
}

#[test]
fn blockquotes_and_toggles_nest_their_rest() {
    let (blocks, _) = convert(json!([
        {"type": "blockquote", "content": [
            paragraph(json!([text("Quoted")])),
            paragraph(json!([text("Second")]))
        ]},
        {"type": "container_toggle", "content": [
            {"type": "heading", "attrs": {"level": 2}, "content": [text("Summary")]},
            paragraph(json!([text("Hidden")]))
        ]}
    ]));
    assert_eq!(blocks[0]["type"], "quote");
    assert_eq!(blocks[0]["content"][0]["text"], "Quoted");
    assert_eq!(blocks[0]["children"][0]["content"][0]["text"], "Second");
    assert_eq!(blocks[1]["type"], "toggleListItem");
    assert_eq!(blocks[1]["content"][0]["text"], "Summary");
    assert_eq!(blocks[1]["children"][0]["content"][0]["text"], "Hidden");
}

#[test]
fn embeds_become_links() {
    let (blocks, _) = convert(json!([
        {"type": "embed", "attrs": {"href": "https://www.youtube.com/watch?v=abc"}}
    ]));
    assert_eq!(blocks[0]["type"], "paragraph");
    assert_eq!(
        blocks[0]["content"][0],
        json!({"type": "link", "href": "https://www.youtube.com/watch?v=abc",
               "content": [{"type": "text", "text": "https://www.youtube.com/watch?v=abc", "styles": {}}]})
    );
}

#[test]
fn math_becomes_formulas() {
    let (blocks, _) = convert(json!([
        paragraph(json!([text("Area "), {"type": "math_inline", "content": [text("\\pi r^2")]}])),
        {"type": "math_block", "content": [text("E = mc^2")]}
    ]));
    assert_eq!(
        blocks[0]["content"][1],
        json!({"type": "math", "props": {"latex": "\\pi r^2"}})
    );
    assert_eq!(
        blocks[1]["content"],
        json!([{"type": "math", "props": {"latex": "E = mc^2"}}])
    );
}

#[test]
fn images_are_uploaded_files_or_links() {
    let (blocks, warnings) = convert(json!([
        paragraph(json!([
            text("Before"),
            {"type": "image", "attrs": {"src": format!("/api/attachments.redirect?id={IMAGE}"), "alt": "Diagram"}, "content": [text("A caption")]}
        ])),
        paragraph(json!([{"type": "image", "attrs": {"src": "https://example.com/cat.png", "alt": "Cat"}}])),
        {"type": "image", "attrs": {"src": format!("https://wiki.example.com/api/attachments.redirect?id={MISSING}")}}
    ]));
    assert_eq!(blocks[0]["type"], "paragraph");
    assert_eq!(blocks[0]["content"][0]["text"], "Before");
    assert_eq!(blocks[1]["type"], "image");
    assert_eq!(blocks[1]["props"]["url"], "/files/diagram.png");
    assert_eq!(blocks[1]["props"]["name"], "Diagram");
    assert_eq!(blocks[1]["props"]["caption"], "A caption");
    // An outside image stays a link; the paragraph that only held it is replaced.
    assert_eq!(blocks[2]["type"], "paragraph");
    assert_eq!(blocks[2]["content"][0]["href"], "https://example.com/cat.png");
    assert_eq!(blocks.len(), 3);
    assert!(warnings
        .iter()
        .any(|warning| matches!(warning, Warning::AttachmentFailed(detail) if detail.contains(MISSING))));
}

#[test]
fn attachments_become_file_blocks() {
    let (blocks, warnings) = convert(json!([
        {"type": "attachment", "attrs": {"href": format!("/api/attachments.redirect?id={ATTACHMENT}"), "title": "Q3 report.pdf", "size": 1234}},
        {"type": "attachment", "attrs": {"href": format!("/api/attachments.redirect?id={IMAGE}"), "title": "shot.png"}},
        {"type": "attachment", "attrs": {"href": format!("/api/attachments.redirect?id={MISSING}"), "title": "lost.zip"}},
        {"type": "video", "attrs": {"src": format!("/api/attachments.redirect?id={ATTACHMENT}"), "title": "demo"}}
    ]));
    assert_eq!(blocks[0]["type"], "file");
    assert_eq!(blocks[0]["props"]["url"], "/files/report.pdf");
    assert_eq!(blocks[0]["props"]["name"], "Q3 report.pdf");
    assert_eq!(blocks[1]["type"], "image");
    // A failed attachment keeps its name as text.
    assert_eq!(blocks[2]["type"], "paragraph");
    assert_eq!(blocks[2]["content"][0]["text"], "lost.zip");
    assert_eq!(blocks[3]["type"], "file");
    let kinds: Vec<&str> = warnings.iter().map(Warning::kind).collect();
    assert!(kinds.contains(&"attachment_failed"), "{kinds:?}");
    assert!(kinds.contains(&"lossy_conversion"), "{kinds:?}");
}

#[test]
fn mentions_of_members_documents_and_collections() {
    let (blocks, warnings) = convert(json!([paragraph(json!([
        {"type": "mention", "attrs": {"type": "user", "modelId": "user-1", "label": "Ada Lovelace"}},
        text(" "),
        {"type": "mention", "attrs": {"type": "user", "modelId": "user-2", "label": "Grace"}},
        text(" "),
        {"type": "mention", "attrs": {"type": "document", "modelId": DOC_ID, "label": "Roadmap"}},
        text(" "),
        {"type": "mention", "attrs": {"type": "document", "modelId": "missing", "label": "Gone"}},
        text(" "),
        {"type": "mention", "attrs": {"type": "pull_request", "modelId": "pr", "label": "#42", "href": "https://github.com/x/y/pull/42"}}
    ]))]));
    let content = blocks[0]["content"].as_array().unwrap();
    assert_eq!(
        content[0],
        json!({"type": "mention", "props": {"userId": "member-1", "name": "Ada"}})
    );
    assert_eq!(content[1]["text"], " @Grace ");
    assert_eq!(content[2]["type"], "link");
    assert_eq!(content[2]["href"], "/docs/page-1");
    assert_eq!(content[2]["content"][0]["text"], "Roadmap");
    assert_eq!(content[3]["text"], " Gone ");
    assert_eq!(content[4]["href"], "https://github.com/x/y/pull/42");
    let unresolved = warnings
        .iter()
        .filter(|warning| warning.kind() == "unresolved_mention")
        .count();
    assert_eq!(unresolved, 2, "{warnings:?}");
}

#[test]
fn internal_links_point_to_imported_pages() {
    let link = |href: &str, value: &str| {
        json!({"type": "text", "text": value, "marks": [{"type": "link", "attrs": {"href": href}}]})
    };
    let (blocks, warnings) = convert(json!([paragraph(json!([
        link("/doc/roadmap-AbCdEf1234", "relative"),
        text(" "),
        link("https://team.getoutline.com/doc/roadmap-AbCdEf1234#h-goals", "absolute"),
        text(" "),
        link("/collection/eng-Coll123456", "collection"),
        text(" "),
        link("/doc/unknown-ZzZzZz9999", "missing"),
        text(" "),
        link("https://example.com", "outside")
    ]))]));
    let content = blocks[0]["content"].as_array().unwrap();
    assert_eq!(content[0]["href"], "/docs/page-1");
    assert_eq!(content[2]["href"], "/docs/page-1");
    assert_eq!(content[4]["href"], "/docs/collection-1");
    assert_eq!(content[6]["href"], "/doc/unknown-ZzZzZz9999");
    assert_eq!(content[8]["href"], "https://example.com");
    assert_eq!(
        warnings,
        vec![Warning::UnresolvedLink("/doc/unknown-ZzZzZz9999".to_owned())]
    );
}

#[test]
fn horizontal_rules_become_dividers() {
    let (blocks, warnings) = convert(json!([
        {"type": "hr", "attrs": {"markup": "---"}},
        {"type": "hr", "attrs": {"markup": "***"}}
    ]));
    assert_eq!(blocks[0]["type"], "divider");
    assert_eq!(blocks[1]["type"], "divider");
    assert_eq!(warnings.len(), 1, "a page break is noted");
}

#[test]
fn emoji_nodes_keep_their_character() {
    let (blocks, warnings) = convert(json!([paragraph(json!([
        {"type": "emoji", "attrs": {"data-name": "smile"}, "content": [text("😄")]},
        {"type": "emoji", "attrs": {"data-name": "unknown_thing"}}
    ]))]));
    assert_eq!(blocks[0]["content"][0]["text"], "😄:unknown_thing:");
    assert_eq!(warnings.len(), 1);
}

#[test]
fn unknown_nodes_keep_their_content_and_warn() {
    let (blocks, warnings) = convert(json!([
        {"type": "fancy_widget", "content": [paragraph(json!([text("Inside")]))]},
        {"type": "mystery", "content": [text("Loose text")]}
    ]));
    assert_eq!(blocks[0]["content"][0]["text"], "Inside");
    assert_eq!(blocks[1]["content"][0]["text"], "Loose text");
    assert_eq!(
        warnings,
        vec![
            Warning::UnsupportedBlock("fancy_widget".to_owned()),
            Warning::UnsupportedBlock("mystery".to_owned())
        ]
    );
}

#[test]
fn deep_nesting_is_flattened_not_dropped() {
    let mut node = json!({"type": "list_item", "content": [paragraph(json!([text("Deepest")]))]});
    for _ in 0..40 {
        node = json!({"type": "list_item", "content": [
            paragraph(json!([text("Level")])),
            {"type": "bullet_list", "content": [node]}
        ]});
    }
    let (blocks, warnings) = convert(json!([{"type": "bullet_list", "content": [node]}]));
    assert_eq!(blocks.len(), 1);
    assert!(serde_json::to_string(&blocks).unwrap().contains("Deepest"));
    assert!(warnings.iter().any(|warning| warning.kind() == "lossy_conversion"));
}

#[test]
fn malformed_input_does_not_panic() {
    let doc: PmNode = serde_json::from_value(json!({
        "type": "doc",
        "content": [
            {"type": "heading", "attrs": null},
            {"type": "table", "content": []},
            {"type": "ordered_list", "attrs": {"order": "seven"}, "content": [{"type": "paragraph"}]},
            {"content": null},
            {"type": "image", "attrs": {}},
            {"type": "text", "text": "stray"}
        ]
    }))
    .unwrap();
    let fixture = Fixture::new();
    let blocks = Converter::new(&fixture).convert(&doc);
    assert!(!blocks.is_empty());
}
