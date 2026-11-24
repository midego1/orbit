//! Reading an uploaded Outline export. Nothing is extracted to disk: the zip is opened from its
//! file, entries are listed with the same safety checks as an Obsidian vault (no `..`, absolute
//! paths, symbolic links, encrypted entries; entry, size and compression-ratio caps), and each
//! file is read on its own when it is needed, so a large export never sits in memory whole.
//!
//! Two formats are recognized from the contents (Outline → Settings → Export):
//!
//! - **JSON** (`ExportJSONTask.ts`): `metadata.json` plus one `<collection>.json` per collection
//!   at the root (`{ collection, documents, attachments }`; `collection.documentStructure` is the
//!   tree in sidebar order, `documents` holds ProseMirror `data`), and every attachment at its
//!   storage `key` (`uploads/…`).
//! - **Markdown** (`ExportMarkdownZipTask.ts` / `ExportDocumentTreeTask.ts`): a folder per
//!   collection, `<Title>.md` per document with its children in a `<Title>/` folder next to it,
//!   entries written in sidebar order, attachments at `<document folder>/<key>` and linked
//!   relatively, internal links rewritten to relative `.md` paths, and each document's
//!   `createdAt`/`updatedAt` in its zip entry comment.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;
use zip::ZipArchive;

use super::model::{
    CollectionFileSummary, ExportDocumentSummary, NavigationNode, PmNode, emoji_icon,
};
use crate::collab::markdown_import::obsidian_links;
use crate::obsidian::vault::{RATIO_FLOOR, VaultError, normalize, read_error};

/// Limits for one archive.
#[derive(Clone, Copy, Debug)]
pub struct ArchiveLimits {
    pub max_entries: usize,
    pub max_total_bytes: u64,
    /// Larger attachments and Markdown files are skipped (and counted).
    pub max_file_bytes: u64,
    /// The largest `<collection>.json` that is read (it holds all of a collection's documents).
    pub max_json_bytes: u64,
    pub max_ratio: u64,
}

impl ArchiveLimits {
    #[must_use]
    pub const fn new(max_file_bytes: u64) -> Self {
        Self {
            max_entries: 100_000,
            max_total_bytes: 8 * 1024 * 1024 * 1024,
            max_file_bytes,
            max_json_bytes: 512 * 1024 * 1024,
            max_ratio: 200,
        }
    }
}

/// Why an archive cannot be imported. The message is shown to the member as is.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum ArchiveError {
    #[error("{0}")]
    Unsafe(String),
    #[error(
        "This is not an Outline export. In Outline, open Settings → Export and export the workspace as JSON or Markdown."
    )]
    NotOutline,
    #[error(
        "This is an HTML export. In Outline, export the workspace as JSON (recommended) or Markdown instead."
    )]
    Html,
    #[error("The export has no collections or documents.")]
    Empty,
    #[error("{name} could not be read: {reason}")]
    BadFile { name: String, reason: String },
    #[error("{name} is larger than {limit_mb} MB.")]
    FileTooLarge { name: String, limit_mb: u64 },
    #[error("The archive could not be read: {0}")]
    Read(String),
}

impl From<VaultError> for ArchiveError {
    fn from(error: VaultError) -> Self {
        match error {
            VaultError::Read(message) => Self::Read(message),
            other => Self::Unsafe(other.to_string()),
        }
    }
}

/// Which export the archive holds.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutlineImportFormat {
    /// The JSON export: keeps the tree, the order and the metadata.
    Json,
    /// The Markdown export.
    Markdown,
    /// Read from Outline's API.
    Api,
}

/// A file kept from the archive. `path` is normalized (`/`-separated).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArchiveFile {
    pub path: String,
    pub index: usize,
    pub size: u64,
    /// The zip entry comment (the Markdown export stores the document's dates there).
    pub comment: String,
}

/// The files of an export archive.
#[derive(Clone, Debug)]
pub struct OutlineArchive {
    pub format: OutlineImportFormat,
    files: Vec<ArchiveFile>,
    /// Lowercase path → index into `files`.
    by_path: HashMap<String, usize>,
    /// Files over the per-file limit (not imported).
    pub too_large: u32,
}

impl OutlineArchive {
    /// Lists an archive's files, checks every entry and detects the format.
    pub fn open<R: Read + Seek>(
        archive: &mut ZipArchive<R>,
        limits: ArchiveLimits,
    ) -> Result<Self, ArchiveError> {
        if archive.len() > limits.max_entries {
            return Err(VaultError::TooManyEntries(limits.max_entries).into());
        }
        let mut entries: Vec<(Vec<String>, ArchiveFile)> = Vec::new();
        let mut total = 0u64;
        for index in 0..archive.len() {
            let file = archive.by_index_raw(index).map_err(read_error)?;
            let name = file
                .name()
                .map_err(|_| VaultError::UnsafePath("(invalid name)".to_owned()))?
                .into_owned();
            let parts = normalize(&name)?;
            if file.is_symlink() {
                return Err(VaultError::Symlink(name.chars().take(200).collect()).into());
            }
            if file.encrypted() {
                return Err(VaultError::Encrypted.into());
            }
            if file.is_dir() || parts.is_empty() {
                continue;
            }
            // Folders and files macOS adds, and dotfiles, are not part of an export.
            if parts
                .iter()
                .any(|part| part.starts_with('.') || *part == "__MACOSX")
            {
                continue;
            }
            let size = file.size();
            total = total.saturating_add(size);
            if total > limits.max_total_bytes {
                return Err(VaultError::TooLarge(limits.max_total_bytes / 1_000_000).into());
            }
            if size > RATIO_FLOOR && size / file.compressed_size().max(1) > limits.max_ratio {
                return Err(VaultError::Bomb(name.chars().take(200).collect()).into());
            }
            let comment = String::from_utf8_lossy(file.comment().as_bytes())
                .chars()
                .take(1_000)
                .collect();
            let parts: Vec<String> = parts.into_iter().map(str::to_owned).collect();
            entries.push((
                parts.clone(),
                ArchiveFile {
                    path: parts.join("/"),
                    index,
                    size,
                    comment,
                },
            ));
        }
        let format = detect(&entries)?;
        let strip = wrapper(&entries, format);
        let mut archive = Self {
            format,
            files: Vec::new(),
            by_path: HashMap::new(),
            too_large: 0,
        };
        for (parts, mut file) in entries {
            if strip.as_deref() == Some(parts[0].as_str()) {
                file.path = parts[1..].join("/");
            }
            let key = file.path.to_lowercase();
            if archive.by_path.contains_key(&key) {
                continue;
            }
            let is_collection_json = format == OutlineImportFormat::Json && is_root_json(&file.path);
            let limit = if is_collection_json {
                limits.max_json_bytes
            } else {
                limits.max_file_bytes
            };
            if file.size > limit {
                if is_collection_json {
                    return Err(ArchiveError::FileTooLarge {
                        name: file.path,
                        limit_mb: limit / 1_000_000,
                    });
                }
                archive.too_large = archive.too_large.saturating_add(1);
                continue;
            }
            archive.by_path.insert(key, archive.files.len());
            archive.files.push(file);
        }
        Ok(archive)
    }

    /// A file by its path (letter case ignored).
    #[must_use]
    pub fn get(&self, path: &str) -> Option<&ArchiveFile> {
        self.by_path
            .get(&path.to_lowercase())
            .map(|&index| &self.files[index])
    }

    /// The files, in archive order.
    #[must_use]
    pub fn files(&self) -> &[ArchiveFile] {
        &self.files
    }

    /// Reads one file, refusing more bytes than its header declared.
    pub fn read<R: Read + Seek>(
        archive: &mut ZipArchive<R>,
        file: &ArchiveFile,
    ) -> Result<Vec<u8>, ArchiveError> {
        let entry = archive.by_index(file.index).map_err(read_error)?;
        let mut bytes = Vec::with_capacity(usize::try_from(file.size).unwrap_or(0).min(1 << 20));
        entry
            .take(file.size.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| ArchiveError::Read(error.to_string()))?;
        if bytes.len() as u64 > file.size {
            return Err(VaultError::Bomb(file.path.chars().take(200).collect()).into());
        }
        Ok(bytes)
    }

    /// Parses one JSON file straight from the archive (no copy of its bytes is kept).
    pub fn read_json<R: Read + Seek, T: for<'de> Deserialize<'de>>(
        archive: &mut ZipArchive<R>,
        file: &ArchiveFile,
    ) -> Result<T, ArchiveError> {
        let entry = archive.by_index(file.index).map_err(read_error)?;
        let reader = std::io::BufReader::new(entry.take(file.size));
        serde_json::from_reader(reader).map_err(|error| ArchiveError::BadFile {
            name: file.path.clone(),
            reason: error.to_string(),
        })
    }
}

fn is_root_json(path: &str) -> bool {
    !path.contains('/') && path.to_lowercase().ends_with(".json")
}

fn is_markdown(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.ends_with(".md") || lower.ends_with(".markdown")
}

/// JSON when collection files sit at the root (or one folder down), Markdown when there are
/// `.md` files.
fn detect(entries: &[(Vec<String>, ArchiveFile)]) -> Result<OutlineImportFormat, ArchiveError> {
    let json_at = |depth: usize| {
        entries.iter().any(|(parts, _)| {
            parts.len() == depth && parts[depth - 1].to_lowercase().ends_with(".json")
        })
    };
    if json_at(1) || (json_at(2) && single_top_folder(entries).is_some()) {
        return Ok(OutlineImportFormat::Json);
    }
    if entries.iter().any(|(_, file)| is_markdown(&file.path)) {
        return Ok(OutlineImportFormat::Markdown);
    }
    if entries
        .iter()
        .any(|(_, file)| file.path.to_lowercase().ends_with(".html"))
    {
        return Err(ArchiveError::Html);
    }
    if entries.is_empty() {
        return Err(ArchiveError::Empty);
    }
    Err(ArchiveError::NotOutline)
}

fn single_top_folder(entries: &[(Vec<String>, ArchiveFile)]) -> Option<&str> {
    let mut top: Option<&str> = None;
    for (parts, _) in entries {
        if parts.len() < 2 {
            return None;
        }
        match top {
            None => top = Some(parts[0].as_str()),
            Some(existing) if existing == parts[0] => {}
            Some(_) => return None,
        }
    }
    top
}

/// A folder around the whole export (an unzipped and zipped-again export) that is not part of
/// it. A Markdown export's own top folders are collections, so one is only stripped when no
/// document sits directly in it.
fn wrapper(entries: &[(Vec<String>, ArchiveFile)], format: OutlineImportFormat) -> Option<String> {
    let top = single_top_folder(entries)?;
    let strip = match format {
        OutlineImportFormat::Json => !entries.iter().any(|(parts, _)| parts.len() == 1),
        _ => entries
            .iter()
            .filter(|(_, file)| is_markdown(&file.path))
            .all(|(parts, _)| parts.len() >= 3),
    };
    strip.then(|| top.to_owned())
}

/// A collection, document, template group or template in the scan tree.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutlineNodeKind {
    Collection,
    Document,
    /// The "Templates" page of a collection (API imports with templates).
    Templates,
    Template,
}

impl OutlineNodeKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Collection => "collection",
            Self::Document => "document",
            Self::Templates => "templates",
            Self::Template => "template",
        }
    }
}

/// Whether a document is published, archived or in Outline's trash.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentState {
    #[default]
    Published,
    Archived,
    Deleted,
}

/// One node of the scan tree (stored with the import; not part of the API answer).
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ScanNode {
    /// Unique within the import: `c:<id>`, `d:<id>`, `t:<collection id>`, `tpl:<id>`, or for the
    /// Markdown export `c:<folder>` and `d:<path>`.
    pub key: String,
    pub kind: Option<OutlineNodeKind>,
    pub title: String,
    pub parent_key: Option<String>,
    pub collection_key: String,
    /// `page_external_refs.external_id`: `collection:<id>`, `document:<id>`, `template:<id>`,
    /// `templates:<collection id>`, or `markdown:<path>` for the Markdown export.
    pub external_id: String,
    /// The Outline id (UUID); `None` for the Markdown export.
    pub id: Option<String>,
    pub url_id: Option<String>,
    pub icon: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub author_email: Option<String>,
    pub state: DocumentState,
    /// Attachments the node's content refers to (for the progress total).
    pub attachments: u32,
    /// Where the content is: the collection's JSON file (JSON export) or the `.md` file.
    pub source: Option<String>,
    /// The collection's description (Markdown, API imports of older servers).
    pub description: Option<String>,
    /// The collection's description as ProseMirror (JSON export and API).
    pub description_data: Option<PmNode>,
}

impl ScanNode {
    #[must_use]
    pub fn kind(&self) -> OutlineNodeKind {
        self.kind.unwrap_or(OutlineNodeKind::Document)
    }
}

/// The scan result stored with an import: the tree (parents before children, siblings in
/// Outline's order) and the Outline users seen, by id, for mentions and authors.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ScanResult {
    pub format: Option<OutlineImportFormat>,
    pub nodes: Vec<ScanNode>,
    /// Outline user id → email.
    pub users: HashMap<String, String>,
    /// Files over the upload limit, not imported.
    pub too_large_files: u32,
}

/// Lists and checks the archive and builds the tree.
pub fn scan_archive<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    limits: ArchiveLimits,
) -> Result<ScanResult, ArchiveError> {
    let files = OutlineArchive::open(archive, limits)?;
    let mut result = match files.format {
        OutlineImportFormat::Markdown => scan_markdown(archive, &files)?,
        _ => scan_json(archive, &files)?,
    };
    result.too_large_files = files.too_large;
    if result.nodes.is_empty() {
        return Err(ArchiveError::Empty);
    }
    Ok(result)
}

/// The collection files of a JSON export, in archive order.
#[must_use]
pub fn collection_files(files: &OutlineArchive) -> Vec<&ArchiveFile> {
    files
        .files()
        .iter()
        .filter(|file| is_root_json(&file.path) && !file.path.eq_ignore_ascii_case("metadata.json"))
        .collect()
}

fn scan_json<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    files: &OutlineArchive,
) -> Result<ScanResult, ArchiveError> {
    let mut result = ScanResult {
        format: Some(OutlineImportFormat::Json),
        ..ScanResult::default()
    };
    let mut seen_collections: HashSet<String> = HashSet::new();
    for file in collection_files(files) {
        let summary: CollectionFileSummary = OutlineArchive::read_json(archive, file)?;
        let collection = summary.collection;
        if collection.id.is_empty() || !seen_collections.insert(collection.id.clone()) {
            continue;
        }
        let collection_key = format!("c:{}", collection.id);
        let collection_attachments = summary
            .attachments
            .values()
            .filter(|attachment| attachment.document_id.is_none())
            .filter(|attachment| files.get(&attachment.key).is_some())
            .count();
        result.nodes.push(ScanNode {
            key: collection_key.clone(),
            kind: Some(OutlineNodeKind::Collection),
            title: collection.name.clone(),
            parent_key: None,
            collection_key: collection_key.clone(),
            external_id: format!("collection:{}", collection.id),
            id: Some(collection.id.clone()),
            url_id: Some(collection.url_id.clone()).filter(|id| !id.is_empty()),
            icon: emoji_icon(collection.icon.as_deref()),
            created_at: collection.created_at.clone(),
            updated_at: collection.updated_at.clone(),
            attachments: u32::try_from(collection_attachments).unwrap_or(u32::MAX),
            source: Some(file.path.clone()),
            description: collection.description.clone(),
            description_data: collection.data.clone(),
            ..ScanNode::default()
        });
        let mut attachments_per_document: HashMap<&str, u32> = HashMap::new();
        for attachment in summary.attachments.values() {
            if let Some(document) = attachment.document_id.as_deref()
                && files.get(&attachment.key).is_some()
            {
                *attachments_per_document.entry(document).or_default() += 1;
            }
        }
        for document in summary.documents.values() {
            if let Some(email) = document.created_by_email.as_deref()
                && let Some(id) = document.created_by_id.as_deref()
            {
                result.users.insert(id.to_owned(), email.to_owned());
            }
        }
        let mut placed: HashSet<String> = HashSet::new();
        let context = JsonTree {
            documents: &summary.documents,
            attachments: &attachments_per_document,
            collection_key: &collection_key,
            source: &file.path,
        };
        context.walk(
            &collection.document_structure,
            &collection_key,
            &mut placed,
            &mut result.nodes,
            0,
        );
        // Documents the structure does not list (it should list every published one): keep them
        // at the collection's top level, by title.
        let mut rest: Vec<&ExportDocumentSummary> = summary
            .documents
            .values()
            .filter(|document| !placed.contains(&document.id))
            .filter(|document| document.published_at.is_some() || document.parent_document_id.is_none())
            .collect();
        rest.sort_by_cached_key(|document| (document.title.to_lowercase(), document.id.clone()));
        for document in rest {
            placed.insert(document.id.clone());
            result
                .nodes
                .push(context.node(document, &collection_key));
        }
    }
    Ok(result)
}

struct JsonTree<'a> {
    documents: &'a HashMap<String, ExportDocumentSummary>,
    attachments: &'a HashMap<&'a str, u32>,
    collection_key: &'a str,
    source: &'a str,
}

impl JsonTree<'_> {
    fn node(&self, document: &ExportDocumentSummary, parent_key: &str) -> ScanNode {
        ScanNode {
            key: format!("d:{}", document.id),
            kind: Some(OutlineNodeKind::Document),
            title: document.title.clone(),
            parent_key: Some(parent_key.to_owned()),
            collection_key: self.collection_key.to_owned(),
            external_id: format!("document:{}", document.id),
            id: Some(document.id.clone()),
            url_id: Some(document.url_id.clone()).filter(|id| !id.is_empty()),
            icon: emoji_icon(document.icon.as_deref().or(document.emoji.as_deref())),
            created_at: document.created_at.clone(),
            updated_at: document.updated_at.clone(),
            author_email: document.created_by_email.clone(),
            attachments: self
                .attachments
                .get(document.id.as_str())
                .copied()
                .unwrap_or(0),
            source: Some(self.source.to_owned()),
            ..ScanNode::default()
        }
    }

    fn walk(
        &self,
        nodes: &[NavigationNode],
        parent_key: &str,
        placed: &mut HashSet<String>,
        out: &mut Vec<ScanNode>,
        depth: usize,
    ) {
        if depth > 64 {
            return;
        }
        for node in nodes {
            let Some(document) = self.documents.get(&node.id) else {
                continue;
            };
            if !placed.insert(document.id.clone()) {
                continue;
            }
            let scan = self.node(document, parent_key);
            let key = scan.key.clone();
            out.push(scan);
            self.walk(&node.children, &key, placed, out, depth + 1);
        }
    }
}

/// `{"createdAt": "…", "updatedAt": "…"}` from a Markdown export's entry comment.
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EntryDates {
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
}

/// The title of a Markdown export file: its name without `.md`.
#[must_use]
pub fn markdown_title(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    let lower = name.to_lowercase();
    for extension in [".markdown", ".md"] {
        if lower.ends_with(extension) {
            return &name[..name.len() - extension.len()];
        }
    }
    name
}

/// `folder` + `target` (`./` and `..` resolved), or `None` when it leaves the archive.
#[must_use]
pub fn join_path(folder: &str, target: &str) -> Option<String> {
    let mut parts: Vec<&str> = if folder.is_empty() {
        Vec::new()
    } else {
        folder.split('/').collect()
    };
    for part in target.split(['#', '?']).next().unwrap_or(target).split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            _ => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// The folder of a path (`""` at the root).
#[must_use]
pub fn folder_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(folder, _)| folder)
}

fn scan_markdown<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    files: &OutlineArchive,
) -> Result<ScanResult, ArchiveError> {
    let mut result = ScanResult {
        format: Some(OutlineImportFormat::Markdown),
        ..ScanResult::default()
    };
    let markdown: Vec<&ArchiveFile> = files
        .files()
        .iter()
        .filter(|file| is_markdown(&file.path))
        .collect();
    if markdown.is_empty() {
        return Err(ArchiveError::Empty);
    }
    let paths: HashSet<String> = markdown
        .iter()
        .map(|file| file.path.to_lowercase())
        .collect();
    let mut collections: Vec<String> = Vec::new();
    let mut documents: Vec<ScanNode> = Vec::new();
    for file in &markdown {
        let collection = match file.path.split_once('/') {
            Some((collection, _)) => collection.to_owned(),
            // A document outside any folder (a single-document export).
            None => String::new(),
        };
        if !collections.contains(&collection) {
            collections.push(collection.clone());
        }
        let collection_key = format!("c:{collection}");
        // `A/B/Child.md` is a child of `A/B.md` when that file exists (the collection folder
        // itself has no file).
        let folder = folder_of(&file.path);
        let parent_key = if folder.contains('/') && paths.contains(&format!("{}.md", folder.to_lowercase())) {
            format!("d:{folder}.md")
        } else {
            collection_key.clone()
        };
        let dates: EntryDates = serde_json::from_str(&file.comment).unwrap_or_default();
        let bytes = OutlineArchive::read(archive, file)?;
        let text = String::from_utf8_lossy(&bytes);
        let mut attachments: HashSet<String> = HashSet::new();
        for (target, _) in obsidian_links(&text) {
            if let Some(path) = join_path(folder, &target)
                && !is_markdown(&path)
                && files.get(&path).is_some()
            {
                attachments.insert(path.to_lowercase());
            }
        }
        documents.push(ScanNode {
            key: format!("d:{}", file.path),
            kind: Some(OutlineNodeKind::Document),
            title: markdown_title(&file.path).to_owned(),
            parent_key: Some(parent_key),
            collection_key,
            external_id: format!("markdown:{}", file.path),
            created_at: dates.created_at,
            updated_at: dates.updated_at,
            attachments: u32::try_from(attachments.len()).unwrap_or(u32::MAX),
            source: Some(file.path.clone()),
            ..ScanNode::default()
        });
    }
    for collection in collections {
        let key = format!("c:{collection}");
        let title = if collection.is_empty() {
            "Documents".to_owned()
        } else {
            collection.clone()
        };
        result.nodes.push(ScanNode {
            key: key.clone(),
            kind: Some(OutlineNodeKind::Collection),
            title,
            parent_key: None,
            collection_key: key.clone(),
            external_id: format!("markdown:{collection}/"),
            ..ScanNode::default()
        });
        // The export writes a document before its sub-documents, in sidebar order; order the
        // tree explicitly anyway, in case an archive tool reordered the entries.
        let members: Vec<ScanNode> = documents
            .iter()
            .filter(|document| document.collection_key == key)
            .cloned()
            .collect();
        result.nodes.extend(depth_first(members));
    }
    Ok(result)
}

/// Orders nodes so that each comes right after its parent's earlier descendants (depth first),
/// keeping siblings in their given order.
pub(crate) fn depth_first(nodes: Vec<ScanNode>) -> Vec<ScanNode> {
    let keys: HashSet<String> = nodes.iter().map(|node| node.key.clone()).collect();
    let mut children: HashMap<Option<String>, Vec<usize>> = HashMap::new();
    for (index, node) in nodes.iter().enumerate() {
        let parent = node.parent_key.clone().filter(|parent| keys.contains(parent));
        children.entry(parent).or_default().push(index);
    }
    let mut order = Vec::with_capacity(nodes.len());
    let mut stack: Vec<usize> = children
        .get(&None)
        .map(|roots| roots.iter().rev().copied().collect())
        .unwrap_or_default();
    while let Some(index) = stack.pop() {
        order.push(index);
        if let Some(list) = children.get(&Some(nodes[index].key.clone())) {
            stack.extend(list.iter().rev().copied());
        }
    }
    let mut slots: Vec<Option<ScanNode>> = nodes.into_iter().map(Some).collect();
    order
        .into_iter()
        .filter_map(|index| slots[index].take())
        .collect()
}

/// Markdown written by Outline → what the shared Markdown reader understands: `:::style`
/// notices become `> [!style]` callouts, `@[Name](mention://…)` mentions become `@Name` text,
/// a first line repeating the title is dropped.
#[must_use]
pub fn prepare_markdown(markdown: &str, title: &str) -> String {
    let mut out = String::with_capacity(markdown.len());
    let mut in_notice = false;
    let mut notice_first = false;
    let mut in_fence = false;
    let mut first_line = true;
    for line in markdown.lines() {
        let trimmed = line.trim();
        if first_line {
            first_line = false;
            if trimmed == format!("# {}", title.trim()) {
                continue;
            }
        }
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
        }
        if !in_fence {
            if !in_notice
                && let Some(style) = trimmed.strip_prefix(":::")
                && !style.is_empty()
                && style.chars().all(|c| c.is_ascii_alphabetic())
            {
                in_notice = true;
                notice_first = true;
                out.push_str(&format!("> [!{}]", style.to_ascii_lowercase()));
                continue;
            }
            if in_notice && trimmed == ":::" {
                in_notice = false;
                if notice_first {
                    out.push('\n');
                }
                out.push('\n');
                continue;
            }
        }
        let line = if in_fence {
            line.to_owned()
        } else {
            strip_mentions(line)
        };
        if in_notice {
            if notice_first {
                notice_first = false;
                if line.trim().is_empty() {
                    // The notice's first line is the callout's title; skip leading blank lines.
                    notice_first = true;
                    continue;
                }
                out.push(' ');
                out.push_str(line.trim());
                out.push('\n');
            } else {
                out.push_str("> ");
                out.push_str(&line);
                out.push('\n');
            }
            continue;
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// `@[Name](mention://…)` → `@Name`.
fn strip_mentions(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find("@[") {
        let after = &rest[start + 2..];
        let Some(close) = after.find("](mention://") else {
            out.push_str(&rest[..start + 2]);
            rest = after;
            continue;
        };
        let target = &after[close + 2..];
        let Some(end) = target.find(')') else {
            out.push_str(&rest[..start + 2]);
            rest = after;
            continue;
        };
        out.push_str(&rest[..start]);
        out.push('@');
        out.push_str(&after[..close]);
        rest = &target[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepares_outline_markdown() {
        let markdown = "# Title\nIntro @[Ada](mention://1/user/2) here.\n\n:::warning\nCareful now\nmore\n:::\n\n```\n:::info\n```\n";
        let prepared = prepare_markdown(markdown, "Title");
        assert!(prepared.starts_with("Intro @Ada here."), "{prepared}");
        assert!(prepared.contains("> [!warning] Careful now\n> more\n"), "{prepared}");
        assert!(prepared.contains("```\n:::info\n```"), "{prepared}");
    }

    #[test]
    fn joins_relative_paths() {
        assert_eq!(join_path("A/B", "./C.md").as_deref(), Some("A/B/C.md"));
        assert_eq!(join_path("A/B", "../C.md#x").as_deref(), Some("A/C.md"));
        assert_eq!(join_path("", "../x"), None);
        assert_eq!(markdown_title("A/My Doc.md"), "My Doc");
    }
}
