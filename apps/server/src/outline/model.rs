//! Typed shapes of what Outline writes: ProseMirror documents, its JSON workspace export and the
//! API answers the importer reads. Checked against Outline's source (outline/outline at
//! `333cac83d4508fabad276f0ac1afa9c87d149b95`): `server/types.ts` (`CollectionJSONExport`,
//! `DocumentJSONExport`, `AttachmentJSONExport`, `JSONExportMetadata`),
//! `server/queues/tasks/ExportJSONTask.ts`, `server/presenters/{document,collection,template}.ts`
//! and `shared/editor/{nodes,marks}`. Every field Outline may leave out is optional, so an export
//! from an older or newer Outline still reads; unknown fields are ignored.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A ProseMirror node (`doc`, `paragraph`, `text`, …). `attrs` stays a map: each node type reads
/// the attributes it knows through the typed accessors below.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct PmNode {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default, deserialize_with = "map_or_null")]
    pub attrs: Map<String, Value>,
    #[serde(default, deserialize_with = "vec_or_null")]
    pub content: Vec<PmNode>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default, deserialize_with = "vec_or_null")]
    pub marks: Vec<PmMark>,
}

/// A ProseMirror mark (`strong`, `em`, `link`, …).
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct PmMark {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default, deserialize_with = "map_or_null")]
    pub attrs: Map<String, Value>,
}

fn map_or_null<'de, D>(deserializer: D) -> Result<Map<String, Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<Map<String, Value>>::deserialize(deserializer)?.unwrap_or_default())
}

fn vec_or_null<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

/// Typed reads of `attrs`.
pub trait Attrs {
    fn attrs(&self) -> &Map<String, Value>;

    /// A non-empty string attribute.
    fn attr_str(&self, name: &str) -> Option<&str> {
        self.attrs()
            .get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
    }

    /// A number attribute (Outline sometimes writes numbers as strings).
    fn attr_u64(&self, name: &str) -> Option<u64> {
        match self.attrs().get(name)? {
            Value::Number(number) => number.as_u64(),
            Value::String(text) => text.trim().parse().ok(),
            _ => None,
        }
    }

    fn attr_bool(&self, name: &str) -> bool {
        match self.attrs().get(name) {
            Some(Value::Bool(value)) => *value,
            Some(Value::String(text)) => text == "true",
            _ => false,
        }
    }
}

impl Attrs for PmNode {
    fn attrs(&self) -> &Map<String, Value> {
        &self.attrs
    }
}

impl Attrs for PmMark {
    fn attrs(&self) -> &Map<String, Value> {
        &self.attrs
    }
}

impl PmNode {
    /// The node's text, concatenated from its descendants.
    #[must_use]
    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        self.collect_text(&mut out);
        out
    }

    fn collect_text(&self, out: &mut String) {
        if let Some(text) = &self.text {
            out.push_str(text);
        }
        for child in &self.content {
            child.collect_text(out);
        }
    }

    /// Every attachment id (`/api/attachments.redirect?id=…`) the node refers to, in order.
    #[must_use]
    pub fn attachment_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        self.collect_attachments(&mut ids);
        ids
    }

    fn collect_attachments(&self, ids: &mut Vec<String>) {
        for name in ["src", "href"] {
            if matches!(self.kind.as_str(), "image" | "attachment" | "video")
                && let Some(id) = self.attr_str(name).and_then(attachment_id)
                && !ids.contains(&id)
            {
                ids.push(id);
            }
        }
        for child in &self.content {
            child.collect_attachments(ids);
        }
    }
}

/// The attachment id of an Outline attachment URL (`/api/attachments.redirect?id=<uuid>`,
/// relative or absolute), or `None` for any other URL.
#[must_use]
pub fn attachment_id(url: &str) -> Option<String> {
    let (path, query) = url.split_once('?')?;
    if !path.ends_with("/api/attachments.redirect") && path != "api/attachments.redirect" {
        return None;
    }
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(name, _)| *name == "id")
        .map(|(_, value)| value.split('#').next().unwrap_or(value).to_owned())
        .filter(|id| is_uuid(id))
}

/// Whether `value` looks like a UUID (Outline's ids).
#[must_use]
pub fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.char_indices().all(|(index, c)| match index {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// What an internal Outline link points to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OutlineLink {
    /// `/doc/<slug>-<urlId>` or `/doc/<id>`: the `urlId` (10 or more alphanumerics) or the id.
    Document(String),
    /// `/collection/<slug>-<urlId>` or `/collection/<id>`.
    Collection(String),
}

/// The target of an Outline-internal link (`/doc/…`, `/collection/…`, relative or on any host),
/// or `None` for anything else. Outline's own pattern: `/doc/(?:[0-9a-zA-Z-_~]*-)?([a-zA-Z0-9]{10,15})`
/// (`ExportDocumentTreeTask.ts`), plus `/doc/<uuid>` for imported documents.
#[must_use]
pub fn outline_link(href: &str) -> Option<OutlineLink> {
    let without_fragment = href.split(['#', '?']).next().unwrap_or(href);
    let path = match without_fragment.split_once("://") {
        Some((_, rest)) => rest.find('/').map_or("", |index| &rest[index..]),
        None => without_fragment,
    };
    let path = path.trim_end_matches('/');
    let (kind, rest) = if let Some(rest) = path.strip_prefix("/doc/") {
        (0, rest)
    } else if let Some(rest) = path.strip_prefix("/collection/") {
        (1, rest)
    } else {
        return None;
    };
    // `/doc/<slug>-<urlId>/edit` and similar suffixes still point to the document.
    let segment = rest.split('/').next().unwrap_or_default();
    let key = if is_uuid(segment) {
        segment.to_owned()
    } else {
        let candidate = segment.rsplit('-').next().unwrap_or(segment);
        if !(10..=15).contains(&candidate.len())
            || !candidate.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return None;
        }
        candidate.to_owned()
    };
    Some(if kind == 0 {
        OutlineLink::Document(key)
    } else {
        OutlineLink::Collection(key)
    })
}

/// `metadata.json` of a JSON export (`JSONExportMetadata`).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportMetadata {
    #[serde(default)]
    pub export_version: Option<u64>,
    #[serde(default)]
    pub version: Option<String>,
}

/// A node of a collection's `documentStructure` (Outline's `NavigationNode`).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct NavigationNode {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: String,
    #[serde(default, deserialize_with = "vec_or_null")]
    pub children: Vec<NavigationNode>,
}

/// One `<collection>.json` of a JSON export, without the documents' content (the scan reads
/// this; the documents' `data` is skipped while parsing).
#[derive(Clone, Debug, Deserialize)]
pub struct CollectionFileSummary {
    pub collection: ExportCollection,
    #[serde(default)]
    pub documents: HashMap<String, ExportDocumentSummary>,
    #[serde(default)]
    pub attachments: HashMap<String, ExportAttachment>,
}

/// One `<collection>.json` with every document's content as unparsed JSON, so each document is
/// parsed only when it is imported.
#[derive(Debug, Deserialize)]
pub struct CollectionFileContent {
    pub collection: ExportCollection,
    #[serde(default)]
    pub documents: HashMap<String, Box<serde_json::value::RawValue>>,
    #[serde(default)]
    pub attachments: HashMap<String, ExportAttachment>,
}

/// `CollectionJSONExport.collection`. The export leaves out the Markdown `description`; `data`
/// is the description as ProseMirror.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportCollection {
    pub id: String,
    #[serde(default)]
    pub url_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub data: Option<PmNode>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub archived_at: Option<String>,
    #[serde(default, deserialize_with = "vec_or_null")]
    pub document_structure: Vec<NavigationNode>,
}

/// `DocumentJSONExport` without `data`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportDocumentSummary {
    pub id: String,
    #[serde(default)]
    pub url_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub emoji: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub created_by_id: Option<String>,
    #[serde(default)]
    pub created_by_email: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub parent_document_id: Option<String>,
}

/// `DocumentJSONExport.data`, read for one document at a time.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ExportDocumentContent {
    #[serde(default)]
    pub data: Option<PmNode>,
}

/// `AttachmentJSONExport`: the file is in the archive at `key`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportAttachment {
    pub id: String,
    #[serde(default)]
    pub document_id: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub key: String,
}

/// Outline's pagination block (`{ offset, limit, nextPath }`).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pagination {
    #[serde(default)]
    pub next_path: Option<String>,
}

/// A paginated `POST /api/<list>` answer.
#[derive(Clone, Debug, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
pub struct ListResponse<T> {
    #[serde(default = "Vec::new", deserialize_with = "vec_or_null")]
    pub data: Vec<T>,
    #[serde(default)]
    pub pagination: Option<Pagination>,
}

/// A single-object answer (`documents.info`, `auth.info`).
#[derive(Clone, Debug, Deserialize)]
pub struct InfoResponse<T> {
    pub data: T,
}

/// `presentCollection` (the fields the importer reads).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiCollection {
    pub id: String,
    #[serde(default)]
    pub url_id: String,
    #[serde(default)]
    pub name: String,
    /// ProseMirror description (API version 3).
    #[serde(default)]
    pub data: Option<PmNode>,
    /// Markdown description (older servers).
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub index: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub archived_at: Option<String>,
}

/// `presentUser` (the fields the importer reads; `email` only reaches admins).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ApiUser {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub email: Option<String>,
}

/// `presentDocument` / `presentTemplate` (the fields the importer reads).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiDocument {
    pub id: String,
    #[serde(default)]
    pub url_id: String,
    #[serde(default)]
    pub title: String,
    /// ProseMirror content (API version 3; only `documents.info` is read for it).
    #[serde(default)]
    pub data: Option<PmNode>,
    /// Markdown content (older servers that ignore `x-api-version`).
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub emoji: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub collection_id: Option<String>,
    #[serde(default)]
    pub parent_document_id: Option<String>,
    #[serde(default)]
    pub created_by: Option<ApiUser>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub archived_at: Option<String>,
    #[serde(default)]
    pub deleted_at: Option<String>,
}

/// `auth.info` (the parts the importer reads).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ApiAuthInfo {
    #[serde(default)]
    pub user: Option<ApiUser>,
    #[serde(default)]
    pub team: Option<ApiTeam>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ApiTeam {
    #[serde(default)]
    pub name: String,
}

/// An Outline icon (documents and collections) that is an emoji, or `None` for Outline's named
/// icons (`"beaker"`, `"collection"`) and empty values.
#[must_use]
pub fn emoji_icon(icon: Option<&str>) -> Option<String> {
    let icon = icon?.trim();
    (!icon.is_empty()
        && icon.chars().count() <= 16
        && !icon.chars().any(|c| c.is_ascii_alphanumeric()))
    .then(|| icon.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_attachment_ids_and_internal_links() {
        let id = "8d4b9f3e-3c35-4a6e-9a58-0b1c2d3e4f50";
        assert_eq!(
            attachment_id(&format!("/api/attachments.redirect?id={id}")),
            Some(id.to_owned())
        );
        assert_eq!(
            attachment_id(&format!(
                "https://wiki.example.com/api/attachments.redirect?id={id}&x=1"
            )),
            Some(id.to_owned())
        );
        assert_eq!(attachment_id("https://example.com/a.png"), None);
        assert_eq!(
            outline_link("/doc/my-page-AbCdEf1234#heading"),
            Some(OutlineLink::Document("AbCdEf1234".to_owned()))
        );
        assert_eq!(
            outline_link("https://team.getoutline.com/doc/AbCdEf1234/edit"),
            Some(OutlineLink::Document("AbCdEf1234".to_owned()))
        );
        assert_eq!(
            outline_link(&format!("/doc/{id}")),
            Some(OutlineLink::Document(id.to_owned()))
        );
        assert_eq!(
            outline_link("/collection/engineering-Xy12345678"),
            Some(OutlineLink::Collection("Xy12345678".to_owned()))
        );
        assert_eq!(outline_link("/doc/short-abc"), None);
        assert_eq!(outline_link("https://example.com/docs/x"), None);
    }

    #[test]
    fn keeps_only_emoji_icons() {
        assert_eq!(emoji_icon(Some("📚")), Some("📚".to_owned()));
        assert_eq!(emoji_icon(Some("beaker")), None);
        assert_eq!(emoji_icon(Some("")), None);
        assert_eq!(emoji_icon(None), None);
    }
}
