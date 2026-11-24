//! Outline import: a workspace admin uploads an Outline export (`.zip`, JSON or Markdown) or
//! connects Outline's API (base URL + API token); an `outline.scan` job lists the collections
//! and documents; the admin picks collections, options and a destination; an `outline.import`
//! job creates the Docs pages.
//!
//! State machine (`outline_imports.status`), as for the Obsidian import:
//! `scanning` → `ready` → `queued` → `importing` → `completed`; any active state can become
//! `failed` or `cancelled`, and a `ready` import not started within 24 hours becomes `expired`.
//! The archive is deleted, and the API token wiped, in every final state.
//!
//! Jobs carry only the import id; progress lives in `outline_import_items`, so a restarted job
//! continues where it stopped. Pass 1 creates every page (parents first, collections first; each
//! page id is recorded before the page exists) or finds the page an earlier import made
//! (`page_external_refs`, `external_source = 'outline'`). Pass 2 fetches each page's content,
//! uploads its attachments as page files, converts it with every Outline link pointing to the
//! new pages (all pages exist by then) and saves it; a page an earlier import made is updated,
//! or skipped when Outline's `updatedAt` did not change. A page that fails is recorded and the
//! import continues. A failed or cancelled import trashes the pages it created but never filled.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use orbit_domain::Permission;
use orbit_platform::Row;
use orbit_platform::{
    Backend, Database, Flag, Id, Job, JobError, JobKind, JobKindRegistrationError, JobQueue,
    JobStore, ObjectStorage, TimestampMillis, Worker,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use utoipa::ToSchema;
use zip::ZipArchive;

use super::archive::{
    ArchiveError, ArchiveLimits, DocumentState, OutlineArchive, OutlineImportFormat,
    OutlineNodeKind, ScanNode, ScanResult, depth_first, folder_of, join_path,
    prepare_markdown, scan_archive,
};
use super::client::{OutlineClient, OutlineError, RetryPolicy, check_token, parse_base_url};
use super::convert::{Converter, OutlineResolver, ResolvedFile, ResolvedMember, Warning};
use super::model::{
    ApiDocument, CollectionFileContent, ExportAttachment, ExportDocumentContent, NavigationNode,
    PmNode, emoji_icon,
};
use crate::billing::WorkspaceAccess;
use crate::collab::markdown_import::{
    LinkKind, LinkResolver, LinkTarget, obsidian_links, obsidian_to_blocks,
};
use crate::notion::convert::{TRUNCATION_NOTE, cut_to_size};
use crate::notion::files::sanitize_file_name;
use crate::notion::import::clamp;
use crate::obsidian::archive::{ArchiveStore, LocalArchive, Location};
pub use crate::obsidian::import::JobRun;
use crate::obsidian::vault::open_archive;
use crate::repositories::billing;
use crate::repositories::membership;
use crate::repositories::page_files::{PageFileRepository, page_file_url};
use crate::repositories::page_versions::SaveOrigin;
use crate::repositories::pages::{
    CreatePage, PageChanges, PageError, PageKind, PageRepository, PageSpace, SpaceRequest,
};
use crate::secret_box::{decrypt_secret, encrypt_secret};
use crate::task_import::parse_instant_millis;

/// Job kind names.
pub const SCAN_JOB: &str = "outline.scan";
pub const IMPORT_JOB: &str = "outline.import";
/// The largest export upload (like an Orbit workspace archive).
pub const MAX_ARCHIVE_BYTES: u64 = 5 * 1024 * 1024 * 1024;
/// Most pages one import creates.
pub const MAX_PAGES: usize = 10_000;
/// Most warnings one import keeps for the warning list.
pub const MAX_WARNINGS: i64 = 5_000;
/// A scanned import waits this long for "start" before its archive or token is deleted.
pub const READY_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const STALLED_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
/// The Notion import's page size cap: below the 1 MiB page body limit.
const MAX_CONTENT_BYTES: usize = 900 * 1024;
const MAX_TITLE_CHARS: usize = 500;
const MAX_TITLE_BYTES: usize = 2_000;
const MAX_FILE_NAME_CHARS: usize = 255;
const MAX_FAILURES_LISTED: i64 = 200;
const MAX_LISTED_IMPORTS: i64 = 20;
const ACTIVE: &str = "('scanning', 'queued', 'importing')";
const OPEN: &str = "('scanning', 'ready', 'queued', 'importing')";
const FINAL: &str = "('completed', 'failed', 'cancelled', 'expired')";
const SOURCE: &str = "outline";

/// Both job kinds: two at a time on all servers and one per workspace (defaults; Admin can change
/// them), a few retries for storage and Outline outages.
#[must_use]
pub fn scan_job_kind() -> JobKind {
    outline_job_kind(SCAN_JOB)
}

#[must_use]
pub fn import_job_kind() -> JobKind {
    outline_job_kind(IMPORT_JOB)
}

fn outline_job_kind(name: &str) -> JobKind {
    JobKind::new(name)
        .with_concurrency_limit(2)
        .with_workspace_limit(1)
        .with_retry_policy(
            5,
            vec![
                Duration::from_secs(30),
                Duration::from_secs(2 * 60),
                Duration::from_secs(10 * 60),
                Duration::from_secs(30 * 60),
            ],
        )
}

/// Registers the `outline.scan` and `outline.import` handlers.
pub fn register_jobs(
    worker: Worker,
    service: OutlineImportService,
) -> Result<Worker, JobKindRegistrationError> {
    let scan = service.clone();
    worker
        .with_handler(scan_job_kind(), move |context| {
            let service = scan.clone();
            async move {
                let Some(import_id) = payload_import_id(&context.job.payload) else {
                    return Err(JobError::Permanent("invalid outline.scan payload".to_owned()));
                };
                let run = JobRun {
                    cancel: context.cancellation_token(),
                    last_attempt: context.claim.attempt >= context.job.max_attempts,
                };
                let result = service.run_scan(import_id, &run).await;
                service.after_attempt(import_id, &run, result).await
            }
        })?
        .with_handler(import_job_kind(), move |context| {
            let service = service.clone();
            async move {
                let Some(import_id) = payload_import_id(&context.job.payload) else {
                    return Err(JobError::Permanent(
                        "invalid outline.import payload".to_owned(),
                    ));
                };
                let run = JobRun {
                    cancel: context.cancellation_token(),
                    last_attempt: context.claim.attempt >= context.job.max_attempts,
                };
                let result = service.run_import(import_id, &run).await;
                service.after_attempt(import_id, &run, result).await
            }
        })
}

fn payload_import_id(payload: &Value) -> Option<Id> {
    payload.get("import_id")?.as_str()?.parse().ok()
}

/// Service settings.
#[derive(Clone, Copy, Debug)]
pub struct OutlineImportSettings {
    /// Largest export upload.
    pub max_archive_bytes: u64,
    /// Largest attachment (and Markdown file) that is imported (the upload limit for one file).
    pub max_file_bytes: u64,
    /// The server app key; without it no API import can store its token.
    pub app_key: Option<[u8; 32]>,
    /// Development only: allow `http://127.0.0.1:<port>` as the Outline base URL.
    pub allow_local: bool,
    pub retry: RetryPolicy,
}

#[derive(Clone)]
pub struct OutlineImportService {
    inner: Arc<Inner>,
}

struct Inner {
    database: Database,
    pages: PageRepository,
    files: PageFileRepository,
    jobs: JobQueue,
    archives: ArchiveStore,
    settings: OutlineImportSettings,
}

/// Errors of the import API.
#[derive(Debug, Error)]
pub enum OutlineImportError {
    #[error("import not found")]
    NotFound,
    /// Only workspace admins import from Outline.
    #[error("only workspace admins can import from Outline")]
    Forbidden,
    #[error("another import is in progress")]
    InProgress,
    #[error("the import is not in a state that allows this")]
    StateConflict,
    #[error("the selection has more than {MAX_PAGES} pages")]
    TooLarge,
    #[error("the archive is larger than {0} bytes")]
    ArchiveTooLarge(u64),
    #[error("the upload is not a zip archive")]
    ArchiveInvalid,
    #[error("the Outline URL is invalid")]
    UrlInvalid,
    #[error("the Outline API token is invalid")]
    TokenInvalid,
    #[error("Outline could not be reached: {0}")]
    OutlineUnavailable(String),
    #[error("the server has no app key")]
    AppKeyMissing,
    #[error("invalid input: {field}")]
    Invalid { field: &'static str },
    #[error("destination page not found")]
    PageNotFound,
    #[error("destination teamspace not found")]
    TeamspaceNotFound,
    #[error("import storage is unavailable")]
    Unavailable(String),
}

impl From<sqlx::Error> for OutlineImportError {
    fn from(error: sqlx::Error) -> Self {
        Self::Unavailable(error.to_string())
    }
}

/// `outline_imports.status`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutlineImportStatus {
    /// The scan job is reading the export or Outline's API.
    Scanning,
    /// The preview is ready; waiting for `start` (the archive or token is deleted after 24 hours).
    Ready,
    Queued,
    Importing,
    Completed,
    Failed,
    Cancelled,
    Expired,
}

impl OutlineImportStatus {
    fn parse(value: &str) -> Result<Self, OutlineImportError> {
        Ok(match value {
            "scanning" => Self::Scanning,
            "ready" => Self::Ready,
            "queued" => Self::Queued,
            "importing" => Self::Importing,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "expired" => Self::Expired,
            _ => {
                return Err(OutlineImportError::Unavailable(
                    "unknown import status".to_owned(),
                ));
            }
        })
    }
}

/// Where the import reads from.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutlineImportSource {
    /// An uploaded export (`.zip`).
    Archive,
    /// Outline's API.
    Api,
}

#[derive(Clone, Copy, Debug, Serialize, ToSchema)]
pub struct OutlineImportProgress {
    /// Pages to import (collections, documents, templates).
    pub total: u64,
    pub done: u64,
    pub failed: u64,
    /// Pages an earlier import made that did not change in Outline.
    pub skipped: u64,
    /// Attachments to import (known from the scan; grows when a page has more).
    pub attachments_total: u64,
    /// Attachments handled (imported or failed).
    pub attachments_done: u64,
}

/// Where the import puts its collections.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct OutlineImportDestination {
    /// `null` for the importer's private space.
    #[schema(value_type = Option<String>, required = true)]
    pub teamspace_id: Option<Id>,
    pub private: bool,
    #[schema(value_type = Option<String>, required = true)]
    pub parent_page_id: Option<Id>,
}

/// What the import includes besides published documents.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, ToSchema)]
pub struct OutlineImportOptions {
    /// Archived documents and documents in Outline's trash (API imports; exports hold neither).
    #[serde(default)]
    pub include_archived: bool,
    /// Templates, under a "Templates" page per collection (API imports).
    #[serde(default)]
    pub include_templates: bool,
}

/// A page the import could not create or fill.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct OutlineImportFailure {
    pub key: String,
    pub title: String,
    pub error: String,
}

/// What the import carried over.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct OutlineImportReport {
    /// New pages.
    pub pages_created: u32,
    /// Pages an earlier import made, updated with Outline's newer content.
    pub pages_updated: u32,
    /// Pages an earlier import made whose Outline document did not change.
    pub pages_unchanged: u32,
    /// Attachments uploaded as page files.
    pub files_imported: u32,
    /// Attachments that could not be fetched or stored.
    pub files_failed: u32,
    /// Files of the export over the upload limit (not imported).
    pub too_large_files: u32,
    /// Top-level blocks cut from documents larger than the page size limit.
    pub truncated_blocks: u32,
    /// Warnings recorded (unsupported blocks, failed attachments, unresolved links and
    /// mentions); the first 5,000 are in `GET …/warnings`.
    pub warnings: u32,
    /// Pages the import created but never filled because it failed or was cancelled; they
    /// were moved to the trash.
    pub unfilled_pages_trashed: u32,
    /// The first 200 failed pages.
    pub failures: Vec<OutlineImportFailure>,
}

/// One collection in the preview.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct OutlineCollectionPreview {
    /// Use it in `selection.collections`.
    pub key: String,
    pub title: String,
    #[schema(required = true)]
    pub icon: Option<String>,
    /// Published documents.
    pub documents: u32,
    /// Archived documents and documents in the trash (imported with `include_archived`).
    pub archived: u32,
    /// Templates (imported with `include_templates`).
    pub templates: u32,
    pub attachments: u32,
}

/// What the scan found.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct OutlineImportPreview {
    pub format: OutlineImportFormat,
    pub collections: Vec<OutlineCollectionPreview>,
    /// Files of the export over the upload limit (they will not be imported).
    pub too_large_files: u32,
}

/// One warning of an import.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct OutlineImportWarning {
    /// The page's title.
    pub title: String,
    /// `unsupported_block`, `lossy_conversion`, `attachment_failed`, `unresolved_link` or
    /// `unresolved_mention`.
    pub kind: String,
    pub detail: String,
}

/// An Outline import. Never contains the API token.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct OutlineImport {
    #[schema(value_type = String)]
    pub id: Id,
    #[schema(value_type = String)]
    pub workspace_id: Id,
    pub source: OutlineImportSource,
    pub status: OutlineImportStatus,
    /// What the scan found (`json`, `markdown` or `api`); `null` while scanning an upload.
    #[schema(required = true)]
    pub format: Option<OutlineImportFormat>,
    /// The uploaded file's name, or the Outline workspace's name for API imports.
    pub file_name: String,
    pub archive_bytes: u64,
    /// The Outline base URL of an API import.
    #[schema(required = true)]
    pub base_url: Option<String>,
    pub progress: OutlineImportProgress,
    #[schema(required = true)]
    pub error: Option<String>,
    #[schema(required = true)]
    pub destination: Option<OutlineImportDestination>,
    /// The options chosen at start.
    #[schema(required = true)]
    pub options: Option<OutlineImportOptions>,
    /// The scan's preview, once ready. Only in `GET …/imports/outline/{id}?include=preview`.
    #[schema(required = true)]
    pub preview: Option<OutlineImportPreview>,
    #[schema(required = true)]
    pub report: Option<OutlineImportReport>,
    /// The imported top-level pages (collections) that still exist, in order.
    #[schema(value_type = Vec<String>)]
    pub root_page_ids: Vec<Id>,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: TimestampMillis,
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: TimestampMillis,
}

/// Which collections to import.
#[derive(Clone, Debug)]
pub enum ImportSelection {
    All,
    Collections(Vec<String>),
}

/// The requested destination; with a parent page the collections join the parent's space.
#[derive(Clone, Debug, Default)]
pub struct ImportDestination {
    pub teamspace_id: Option<Id>,
    pub private: Option<bool>,
    pub parent_page_id: Option<Id>,
}

/// An upload being received: the route writes the file to `path`, then calls
/// [`OutlineImportService::create_upload`].
pub struct Upload {
    pub id: Id,
    pub path: PathBuf,
}

#[derive(Default, Deserialize, Serialize)]
struct StoredReport {
    #[serde(default)]
    pages_created: u32,
    #[serde(default)]
    pages_updated: u32,
    #[serde(default)]
    pages_unchanged: u32,
    #[serde(default)]
    files_imported: u32,
    #[serde(default)]
    files_failed: u32,
    #[serde(default)]
    too_large_files: u32,
    #[serde(default)]
    truncated_blocks: u32,
    #[serde(default)]
    warnings: u32,
}

#[derive(Deserialize, Serialize)]
struct StoredSelection {
    all: bool,
    #[serde(default)]
    collections: Vec<String>,
    #[serde(default)]
    options: OutlineImportOptions,
    #[serde(default)]
    parent_page_id: Option<String>,
}

#[derive(Clone, Debug)]
struct Item {
    key: String,
    parent_key: Option<String>,
    page_id: Option<Id>,
    reused: bool,
    status: String,
}

/// Why one page could not be imported, and what that means for the import.
enum Failure {
    /// Server shutdown: the job is requeued and resumes; never fails the import.
    Interrupted,
    /// Record on the page and continue.
    Page(String),
    /// Stop the import (archive gone, revoked token, missing destination, read-only workspace).
    Fatal(String),
    /// Database, storage or Outline trouble: retry the job later.
    Transient(String),
}

impl From<sqlx::Error> for Failure {
    fn from(error: sqlx::Error) -> Self {
        Self::Transient(format!("database error: {error}"))
    }
}

impl From<ArchiveError> for Failure {
    fn from(error: ArchiveError) -> Self {
        Self::Fatal(error.to_string())
    }
}

impl From<OutlineError> for Failure {
    fn from(error: OutlineError) -> Self {
        match error {
            OutlineError::Unauthorized | OutlineError::TokenInvalid => Self::Fatal(
                "Outline refused the API token: it was revoked, expired or lacks access."
                    .to_owned(),
            ),
            OutlineError::UrlInvalid => Self::Fatal("The Outline URL is invalid.".to_owned()),
            OutlineError::NotFound => Self::Page("Outline no longer has this document.".to_owned()),
            OutlineError::RateLimited | OutlineError::Unreachable(_) => {
                Self::Transient(error.to_string())
            }
            OutlineError::TooLarge { .. } | OutlineError::Api { .. } | OutlineError::Decode(_) => {
                Self::Page(format!("Outline's answer could not be used: {error}."))
            }
        }
    }
}

/// The opened source of one job attempt.
enum Source {
    Archive {
        files: Arc<OutlineArchive>,
        archive: Arc<Mutex<ZipArchive<File>>>,
        /// Keeps a downloaded copy alive (and deletes it at the end).
        _file: LocalArchive,
    },
    Api(OutlineClient),
}

impl Source {
    async fn read(&self, path: &str) -> Result<Vec<u8>, Failure> {
        let Self::Archive { files, archive, .. } = self else {
            return Err(Failure::Page("The file is not in the export.".to_owned()));
        };
        let files = Arc::clone(files);
        let archive = Arc::clone(archive);
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || {
            let file = files
                .get(&path)
                .ok_or_else(|| Failure::Page("The file is missing from the export.".to_owned()))?;
            let mut archive = archive
                .lock()
                .map_err(|_| Failure::Transient("archive lock poisoned".to_owned()))?;
            OutlineArchive::read(&mut archive, file).map_err(Failure::from)
        })
        .await
        .map_err(|error| Failure::Transient(error.to_string()))?
    }

    async fn collection_file(&self, path: &str) -> Result<CollectionFileContent, Failure> {
        let Self::Archive { files, archive, .. } = self else {
            return Err(Failure::Fatal("The export is not available.".to_owned()));
        };
        let files = Arc::clone(files);
        let archive = Arc::clone(archive);
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || {
            let file = files
                .get(&path)
                .ok_or_else(|| Failure::Fatal(format!("{path} is missing from the export.")))?;
            let mut archive = archive
                .lock()
                .map_err(|_| Failure::Transient("archive lock poisoned".to_owned()))?;
            OutlineArchive::read_json(&mut archive, file).map_err(Failure::from)
        })
        .await
        .map_err(|error| Failure::Transient(error.to_string()))?
    }
}

/// The content of one page, as Outline gave it.
enum Content {
    ProseMirror(PmNode),
    Markdown(String),
    /// A collection or template group without a description: it lists its pages.
    ChildList,
}

/// Looks up Outline references for one page (owned, so conversion can run off the runtime).
#[derive(Clone, Default)]
struct Lookup {
    /// Outline document id or `urlId` → page href.
    documents: Arc<HashMap<String, String>>,
    /// Outline collection id or `urlId` → page href.
    collections: Arc<HashMap<String, String>>,
    /// Outline user id → member.
    members: Arc<HashMap<String, ResolvedMember>>,
    /// Lowercase Markdown export path → page href.
    markdown_pages: Arc<HashMap<String, String>>,
    /// This page's uploaded attachments: Outline attachment id (or lowercase export path) → file.
    files: HashMap<String, ResolvedFile>,
}

impl OutlineResolver for Lookup {
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

/// The Markdown export's relative links, resolved from one document's folder.
struct MarkdownLinks {
    lookup: Lookup,
    folder: String,
    unresolved: std::cell::RefCell<Vec<String>>,
}

impl LinkResolver for MarkdownLinks {
    fn resolve(&self, target: &str, _kind: LinkKind) -> Option<LinkTarget> {
        let found = join_path(&self.folder, target).and_then(|path| {
            let key = path.to_lowercase();
            if let Some(href) = self.lookup.markdown_pages.get(&key) {
                return Some(LinkTarget::Page { href: href.clone() });
            }
            self.lookup.files.get(&key).map(|file| {
                if file.is_image {
                    LinkTarget::Image {
                        url: file.url.clone(),
                    }
                } else {
                    LinkTarget::File {
                        url: file.url.clone(),
                        name: file.name.clone(),
                    }
                }
            })
        });
        if found.is_none() {
            self.unresolved.borrow_mut().push(target.to_owned());
        }
        found
    }
}

fn is_image(name: &str, content_type: Option<&str>) -> bool {
    if content_type.is_some_and(|kind| kind.starts_with("image/")) {
        return true;
    }
    let lower = name.to_lowercase();
    [
        ".png", ".jpg", ".jpeg", ".gif", ".webp", ".svg", ".bmp", ".avif",
    ]
    .iter()
    .any(|extension| lower.ends_with(extension))
}

fn page_href(page_id: Id) -> String {
    format!("/docs/{page_id}")
}

/// Everything pass 2 needs about the import.
struct Run {
    import_id: Id,
    workspace_id: Id,
    user_id: Id,
    request_id: String,
    format: OutlineImportFormat,
    nodes: HashMap<String, ScanNode>,
    users: HashMap<String, String>,
    report: StoredReport,
}

impl OutlineImportService {
    #[must_use]
    pub fn new(
        database: Database,
        files: PageFileRepository,
        storage: ObjectStorage,
        local_root: impl Into<PathBuf>,
        settings: OutlineImportSettings,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                pages: PageRepository::new(database.clone()),
                jobs: JobQueue::new(JobStore::new(database.clone())),
                archives: ArchiveStore::new(storage, local_root),
                database,
                files,
                settings,
            }),
        }
    }

    fn database(&self) -> &Database {
        &self.inner.database
    }

    /// The largest upload the route accepts.
    #[must_use]
    pub fn max_archive_bytes(&self) -> u64 {
        self.inner.settings.max_archive_bytes
    }

    /// Checks that the admin may upload now and returns where to write the upload. A member has
    /// at most one active import.
    pub async fn prepare(&self, workspace_id: Id, user_id: Id) -> Result<Upload, OutlineImportError> {
        self.require_admin(workspace_id, user_id).await?;
        self.expire_stale(TimestampMillis::now()).await?;
        if self.has_active_import(user_id).await? {
            return Err(OutlineImportError::InProgress);
        }
        let id = Id::new_v7();
        let path = self
            .inner
            .archives
            .partial_path(workspace_id, id)
            .await
            .map_err(|error| OutlineImportError::Unavailable(error.to_string()))?;
        Ok(Upload { id, path })
    }

    /// Stores a received upload and queues the scan. The upload file is gone afterwards.
    pub async fn create_upload(
        &self,
        workspace_id: Id,
        user_id: Id,
        upload: Upload,
        file_name: &str,
        size: u64,
    ) -> Result<OutlineImport, OutlineImportError> {
        let result = self
            .store_upload(workspace_id, user_id, &upload, file_name, size)
            .await;
        let _ = tokio::fs::remove_file(&upload.path).await;
        let id = result?;
        self.after_create(workspace_id, user_id, id).await
    }

    async fn store_upload(
        &self,
        workspace_id: Id,
        user_id: Id,
        upload: &Upload,
        file_name: &str,
        size: u64,
    ) -> Result<Id, OutlineImportError> {
        if !is_zip(&upload.path).await {
            return Err(OutlineImportError::ArchiveInvalid);
        }
        let location = self
            .inner
            .archives
            .store(workspace_id, upload.id, &upload.path)
            .await
            .map_err(OutlineImportError::Unavailable)?;
        let inserted = self
            .insert(
                workspace_id,
                user_id,
                upload.id,
                NewImport::Archive {
                    file_name,
                    size,
                    location,
                },
            )
            .await;
        if inserted.is_err() {
            let _ = self
                .inner
                .archives
                .delete(workspace_id, upload.id, location)
                .await;
        }
        inserted.map(|()| upload.id)
    }

    /// Checks the base URL and the token with Outline (`auth.info`), stores the token encrypted
    /// and queues the scan.
    pub async fn connect(
        &self,
        workspace_id: Id,
        user_id: Id,
        base_url: &str,
        token: &str,
    ) -> Result<OutlineImport, OutlineImportError> {
        self.require_admin(workspace_id, user_id).await?;
        self.expire_stale(TimestampMillis::now()).await?;
        let key = self
            .inner
            .settings
            .app_key
            .ok_or(OutlineImportError::AppKeyMissing)?;
        let token = check_token(token).map_err(|_| OutlineImportError::TokenInvalid)?;
        let base = parse_base_url(base_url, self.inner.settings.allow_local)
            .map_err(|_| OutlineImportError::UrlInvalid)?;
        if self.has_active_import(user_id).await? {
            return Err(OutlineImportError::InProgress);
        }
        let client = OutlineClient::new(
            base.clone(),
            token,
            self.inner.settings.allow_local,
            self.inner.settings.retry,
        )
        .map_err(|_| OutlineImportError::TokenInvalid)?;
        let info = client.auth_info().await.map_err(|error| match error {
            OutlineError::Unauthorized | OutlineError::TokenInvalid => {
                OutlineImportError::TokenInvalid
            }
            OutlineError::NotFound => OutlineImportError::OutlineUnavailable(
                "this URL does not answer like Outline".to_owned(),
            ),
            other => OutlineImportError::OutlineUnavailable(other.to_string()),
        })?;
        let name = info
            .team
            .map(|team| team.name)
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| base.host_str().unwrap_or("Outline").to_owned());
        let ciphertext = encrypt_secret(&key, token)
            .map_err(|()| OutlineImportError::Unavailable("token encryption failed".to_owned()))?;
        let id = Id::new_v7();
        self.insert(
            workspace_id,
            user_id,
            id,
            NewImport::Api {
                name: &name,
                base_url: base.as_str().trim_end_matches('/'),
                ciphertext: &ciphertext,
            },
        )
        .await?;
        self.after_create(workspace_id, user_id, id).await
    }

    async fn after_create(
        &self,
        workspace_id: Id,
        user_id: Id,
        id: Id,
    ) -> Result<OutlineImport, OutlineImportError> {
        self.release_final_archives().await?;
        if let Err(error) = self.enqueue(scan_job_kind(), id, workspace_id).await {
            self.finish(id, "failed", Some("The scan could not be queued."))
                .await?;
            return Err(error);
        }
        self.get(workspace_id, user_id, id, false).await
    }

    async fn insert(
        &self,
        workspace_id: Id,
        user_id: Id,
        id: Id,
        new: NewImport<'_>,
    ) -> Result<(), OutlineImportError> {
        let now = TimestampMillis::now();
        let mut tx = self.database().immediate_transaction().await?;
        let active: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT COUNT(*) FROM outline_imports WHERE user_id = $1 AND status IN {ACTIVE}"
        )))
        .bind(user_id.to_string())
        .fetch_one(&mut *tx)
        .await?;
        if active > 0 {
            return Err(OutlineImportError::InProgress);
        }
        // An unstarted import of this member in this workspace gives way to the new one.
        sqlx::query(
            "UPDATE outline_imports SET status = 'cancelled', token_ciphertext = NULL, updated_at = $1 \
             WHERE user_id = $2 AND workspace_id = $3 AND status = 'ready'",
        )
        .bind(now.as_millis())
        .bind(user_id.to_string())
        .bind(workspace_id.to_string())
        .execute(&mut *tx)
        .await?;
        let (source, file_name, size, location, base_url, ciphertext) = match new {
            NewImport::Archive {
                file_name,
                size,
                location,
            } => (
                "archive",
                clamp(file_name.trim(), MAX_FILE_NAME_CHARS, 1_000),
                i64::try_from(size).unwrap_or(i64::MAX),
                Some(location.as_str()),
                None,
                None,
            ),
            NewImport::Api {
                name,
                base_url,
                ciphertext,
            } => (
                "api",
                clamp(name.trim(), MAX_FILE_NAME_CHARS, 1_000),
                0,
                None,
                Some(base_url.to_owned()),
                Some(ciphertext.to_vec()),
            ),
        };
        sqlx::query(
            "INSERT INTO outline_imports (id, workspace_id, user_id, source, status, format, file_name, \
             archive_bytes, archive_location, base_url, token_ciphertext, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, 'scanning', $5, $6, $7, $8, $9, $10, $11, $12)",
        )
        .bind(id.to_string())
        .bind(workspace_id.to_string())
        .bind(user_id.to_string())
        .bind(source)
        .bind((source == "api").then_some("api"))
        .bind(file_name)
        .bind(size)
        .bind(location)
        .bind(base_url)
        .bind(ciphertext)
        .bind(now.as_millis())
        .bind(now.as_millis())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// The admin's recent imports in this workspace, newest first (no previews).
    pub async fn list(
        &self,
        workspace_id: Id,
        user_id: Id,
    ) -> Result<Vec<OutlineImport>, OutlineImportError> {
        self.require_admin(workspace_id, user_id).await?;
        self.expire_stale(TimestampMillis::now()).await?;
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM outline_imports WHERE workspace_id = $1 AND user_id = $2 \
             ORDER BY created_at DESC, id DESC LIMIT $3",
        )
        .bind(workspace_id.to_string())
        .bind(user_id.to_string())
        .bind(MAX_LISTED_IMPORTS)
        .fetch_all(self.database().pool())
        .await?;
        let mut imports = Vec::with_capacity(ids.len());
        for id in ids {
            let id = id
                .parse()
                .map_err(|_| OutlineImportError::Unavailable("invalid import id".to_owned()))?;
            imports.push(self.load_view(workspace_id, user_id, id, false).await?);
        }
        Ok(imports)
    }

    /// One of the admin's imports; the preview only `with_preview`. Other members get
    /// `NotFound` (or `Forbidden` when they are not admins).
    pub async fn get(
        &self,
        workspace_id: Id,
        user_id: Id,
        import_id: Id,
        with_preview: bool,
    ) -> Result<OutlineImport, OutlineImportError> {
        self.require_admin(workspace_id, user_id).await?;
        self.expire_stale(TimestampMillis::now()).await?;
        self.load_view(workspace_id, user_id, import_id, with_preview)
            .await
    }

    /// The warnings of an import (the first 5,000), in the order they were found.
    pub async fn warnings(
        &self,
        workspace_id: Id,
        user_id: Id,
        import_id: Id,
    ) -> Result<Vec<OutlineImportWarning>, OutlineImportError> {
        self.require_admin(workspace_id, user_id).await?;
        self.load_view(workspace_id, user_id, import_id, false)
            .await?;
        let rows = sqlx::query(
            "SELECT title, kind, detail FROM outline_import_warnings WHERE import_id = $1 \
             ORDER BY position LIMIT $2",
        )
        .bind(import_id.to_string())
        .bind(MAX_WARNINGS)
        .fetch_all(self.database().pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| OutlineImportWarning {
                title: row.get("title"),
                kind: row.get("kind"),
                detail: row.get("detail"),
            })
            .collect())
    }

    /// Plans the import of `selection` with `options` into `destination` and queues the job.
    pub async fn start(
        &self,
        workspace_id: Id,
        user_id: Id,
        import_id: Id,
        selection: ImportSelection,
        options: OutlineImportOptions,
        destination: ImportDestination,
    ) -> Result<OutlineImport, OutlineImportError> {
        self.require_admin(workspace_id, user_id).await?;
        self.expire_stale(TimestampMillis::now()).await?;
        let row = sqlx::query(
            "SELECT status, tree_json FROM outline_imports WHERE id = $1 AND workspace_id = $2 AND user_id = $3",
        )
        .bind(import_id.to_string())
        .bind(workspace_id.to_string())
        .bind(user_id.to_string())
        .fetch_optional(self.database().pool())
        .await?
        .ok_or(OutlineImportError::NotFound)?;
        if row.get::<String, _>("status") != "ready" {
            return Err(OutlineImportError::StateConflict);
        }
        let tree: ScanResult = serde_json::from_str(
            &row.get::<Option<String>, _>("tree_json")
                .unwrap_or_default(),
        )
        .map_err(|_| OutlineImportError::Unavailable("invalid stored scan".to_owned()))?;

        let space = self
            .inner
            .pages
            .resolve_destination(
                workspace_id,
                user_id,
                destination.parent_page_id,
                SpaceRequest {
                    teamspace_id: destination.teamspace_id,
                    private: destination.private,
                },
            )
            .await
            .map_err(|error| match error {
                PageError::NotFound if destination.parent_page_id.is_some() => {
                    OutlineImportError::PageNotFound
                }
                PageError::NotFound => OutlineImportError::NotFound,
                PageError::TeamspaceNotFound => OutlineImportError::TeamspaceNotFound,
                PageError::Invalid { field } => OutlineImportError::Invalid { field },
                other => OutlineImportError::Unavailable(other.to_string()),
            })?;

        let plan = plan_items(&tree, &selection, options)?;
        if plan.len() > MAX_PAGES {
            return Err(OutlineImportError::TooLarge);
        }
        let attachments: u64 = plan.iter().map(|node| u64::from(node.attachments)).sum();
        let report = StoredReport {
            too_large_files: tree.too_large_files,
            ..StoredReport::default()
        };
        let stored_selection = StoredSelection {
            all: matches!(selection, ImportSelection::All),
            collections: match &selection {
                ImportSelection::All => Vec::new(),
                ImportSelection::Collections(keys) => keys.clone(),
            },
            options,
            parent_page_id: destination.parent_page_id.map(|id| id.to_string()),
        };

        let now = TimestampMillis::now();
        let mut tx = self.database().immediate_transaction().await?;
        let active: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT COUNT(*) FROM outline_imports WHERE user_id = $1 AND id <> $2 AND status IN {ACTIVE}"
        )))
        .bind(user_id.to_string())
        .bind(import_id.to_string())
        .fetch_one(&mut *tx)
        .await?;
        if active > 0 {
            return Err(OutlineImportError::InProgress);
        }
        let updated = sqlx::query(
            "UPDATE outline_imports SET status = 'queued', selection_json = $1, target_teamspace_id = $2, \
             target_private = $3, target_parent_page_id = $4, total = $5, done = 0, failed = 0, \
             skipped = 0, attachments_total = $6, attachments_done = 0, report_json = $7, \
             updated_at = $8, started_at = $8 \
             WHERE id = $9 AND status = 'ready' \
             AND (archive_location IS NOT NULL OR token_ciphertext IS NOT NULL)",
        )
        .bind(serde_json::to_string(&stored_selection).unwrap_or_default())
        .bind(space.teamspace_id().map(|id| id.to_string()))
        .bind(i64::from(matches!(space, PageSpace::Private(_))))
        .bind(destination.parent_page_id.map(|id| id.to_string()))
        .bind(plan.len() as i64)
        .bind(i64::try_from(attachments).unwrap_or(i64::MAX))
        .bind(serde_json::to_string(&report).unwrap_or_default())
        .bind(now.as_millis())
        .bind(import_id.to_string())
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if updated != 1 {
            return Err(OutlineImportError::StateConflict);
        }
        for (position, node) in plan.iter().enumerate() {
            sqlx::query(
                "INSERT INTO outline_import_items (import_id, item_key, kind, parent_key, position, title) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(import_id.to_string())
            .bind(&node.key)
            .bind(node.kind().as_str())
            .bind(&node.parent_key)
            .bind(position as i64)
            .bind(clamp(&node.title, MAX_TITLE_CHARS, MAX_TITLE_BYTES))
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        if let Err(error) = self
            .enqueue(import_job_kind(), import_id, workspace_id)
            .await
        {
            self.finish(import_id, "failed", Some("The import could not be queued."))
                .await?;
            return Err(error);
        }
        self.get(workspace_id, user_id, import_id, false).await
    }

    /// Cancels an open import, deletes its archive and wipes its token.
    pub async fn cancel(
        &self,
        workspace_id: Id,
        user_id: Id,
        import_id: Id,
    ) -> Result<OutlineImport, OutlineImportError> {
        self.require_admin(workspace_id, user_id).await?;
        let updated = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE outline_imports SET status = 'cancelled', token_ciphertext = NULL, updated_at = $1 \
             WHERE id = $2 AND workspace_id = $3 AND user_id = $4 AND status IN {OPEN}"
        )))
        .bind(TimestampMillis::now().as_millis())
        .bind(import_id.to_string())
        .bind(workspace_id.to_string())
        .bind(user_id.to_string())
        .execute(self.database().pool())
        .await?
        .rows_affected();
        if updated == 0 {
            self.load_view(workspace_id, user_id, import_id, false)
                .await?;
            return Err(OutlineImportError::StateConflict);
        }
        if !self.is_running(import_id).await? {
            self.trash_unfilled_pages(import_id).await?;
        }
        self.release_final_archives().await?;
        self.load_view(workspace_id, user_id, import_id, false)
            .await
    }

    /// Expires unstarted imports after 24 hours, fails active imports without progress for 24
    /// hours, and deletes the archives of finished imports. Returns the imports changed.
    pub async fn expire_stale(&self, now: TimestampMillis) -> Result<u64, OutlineImportError> {
        let ready_cutoff = now.as_millis() - READY_TTL.as_millis() as i64;
        let stalled_cutoff = now.as_millis() - STALLED_AFTER.as_millis() as i64;
        let expired = sqlx::query(
            "UPDATE outline_imports SET status = 'expired', token_ciphertext = NULL, \
             error = 'The import was not started within 24 hours.', updated_at = $1 \
             WHERE status = 'ready' AND updated_at < $2",
        )
        .bind(now.as_millis())
        .bind(ready_cutoff)
        .execute(self.database().pool())
        .await?
        .rows_affected();
        let stalled = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE outline_imports SET status = 'failed', token_ciphertext = NULL, \
             error = 'The import stopped making progress.', updated_at = $1 \
             WHERE status IN {ACTIVE} AND updated_at < $2"
        )))
        .bind(now.as_millis())
        .bind(stalled_cutoff)
        .execute(self.database().pool())
        .await?
        .rows_affected();
        let dead = self.fail_imports_with_dead_jobs().await?;
        self.release_final_archives().await?;
        Ok(expired + stalled + dead)
    }

    async fn release_final_archives(&self) -> Result<(), OutlineImportError> {
        let rows: Vec<(String, String, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT id, workspace_id, archive_location FROM outline_imports \
             WHERE status IN {FINAL} AND archive_location IS NOT NULL LIMIT 100"
        )))
        .fetch_all(self.database().pool())
        .await?;
        for (id, workspace, location) in rows {
            let (Ok(import_id), Ok(workspace_id), Some(location)) = (
                id.parse::<Id>(),
                workspace.parse::<Id>(),
                Location::parse(&location),
            ) else {
                continue;
            };
            match self
                .inner
                .archives
                .delete(workspace_id, import_id, location)
                .await
            {
                Ok(()) => {
                    sqlx::query("UPDATE outline_imports SET archive_location = NULL WHERE id = $1")
                        .bind(&id)
                        .execute(self.database().pool())
                        .await?;
                }
                Err(error) => {
                    tracing::warn!(import_id = %import_id, error = %error, "Outline import archive could not be deleted");
                }
            }
        }
        Ok(())
    }

    async fn after_attempt(
        &self,
        import_id: Id,
        run: &JobRun,
        result: Result<(), JobError>,
    ) -> Result<(), JobError> {
        if let Err(error) = &result
            && run.last_attempt
            && !run.cancel.is_cancelled()
            && let Err(cleanup) = self
                .fail_open_import(import_id, "The import stopped after repeated errors.")
                .await
        {
            tracing::warn!(import_id = %import_id, error = %error, cleanup = %cleanup, "Outline import could not be finalized");
        }
        result
    }

    fn import_id_of_job(&self) -> &'static str {
        match self.database().backend() {
            Backend::Sqlite => "json_extract(jobs.payload_json, '$.import_id')",
            Backend::Postgres => "(jobs.payload_json::jsonb ->> 'import_id')",
        }
    }

    async fn fail_imports_with_dead_jobs(&self) -> Result<u64, OutlineImportError> {
        let import_id_of_job = self.import_id_of_job();
        let ids: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT id FROM outline_imports WHERE status IN {ACTIVE} \
             AND EXISTS (SELECT 1 FROM jobs WHERE jobs.kind IN ('{SCAN_JOB}', '{IMPORT_JOB}') \
                 AND jobs.state = 'dead' \
                 AND {import_id_of_job} = outline_imports.id) \
             AND NOT EXISTS (SELECT 1 FROM jobs WHERE jobs.kind IN ('{SCAN_JOB}', '{IMPORT_JOB}') \
                 AND jobs.state IN ('queued', 'running') \
                 AND {import_id_of_job} = outline_imports.id)"
        )))
        .fetch_all(self.database().pool())
        .await?;
        let mut finished = 0;
        for id in ids {
            let Ok(import_id) = id.parse::<Id>() else {
                continue;
            };
            self.fail_open_import(import_id, "The import stopped after repeated errors.")
                .await?;
            finished += 1;
        }
        Ok(finished)
    }

    async fn fail_open_import(&self, import_id: Id, message: &str) -> Result<(), sqlx::Error> {
        let status: Option<String> =
            sqlx::query_scalar("SELECT status FROM outline_imports WHERE id = $1")
                .bind(import_id.to_string())
                .fetch_optional(self.database().pool())
                .await?;
        if !matches!(
            status.as_deref(),
            Some("scanning" | "ready" | "queued" | "importing")
        ) {
            return Ok(());
        }
        self.trash_unfilled_pages(import_id).await?;
        self.finish(import_id, "failed", Some(message)).await
    }

    async fn open_archive(&self, import_id: Id) -> Result<LocalArchive, Failure> {
        let row = sqlx::query("SELECT workspace_id, archive_location FROM outline_imports WHERE id = $1")
            .bind(import_id.to_string())
            .fetch_optional(self.database().pool())
            .await?
            .ok_or_else(|| Failure::Fatal("The import was deleted.".to_owned()))?;
        let workspace_id = parse_stored_id(row.get("workspace_id"))?;
        let location = row
            .get::<Option<String>, _>("archive_location")
            .as_deref()
            .and_then(Location::parse)
            .ok_or_else(|| Failure::Fatal("The uploaded export was deleted.".to_owned()))?;
        self.inner
            .archives
            .open(workspace_id, import_id, location)
            .await
            .map_err(Failure::Transient)?
            .ok_or_else(|| Failure::Fatal("The uploaded export is missing.".to_owned()))
    }

    /// The API client of an API import (the token decrypted for this job attempt only).
    async fn api_client(&self, import_id: Id) -> Result<OutlineClient, Failure> {
        let row = sqlx::query("SELECT base_url, token_ciphertext FROM outline_imports WHERE id = $1")
            .bind(import_id.to_string())
            .fetch_optional(self.database().pool())
            .await?
            .ok_or_else(|| Failure::Fatal("The import was deleted.".to_owned()))?;
        let key = self
            .inner
            .settings
            .app_key
            .ok_or_else(|| Failure::Fatal("The server's app key is missing.".to_owned()))?;
        let ciphertext: Option<Vec<u8>> = row.get("token_ciphertext");
        let ciphertext =
            ciphertext.ok_or_else(|| Failure::Fatal("The Outline API token was deleted.".to_owned()))?;
        let token = decrypt_secret(&key, &ciphertext).map_err(|()| {
            Failure::Fatal("The Outline API token can no longer be read (the app key changed).".to_owned())
        })?;
        let base_url: Option<String> = row.get("base_url");
        let base = parse_base_url(
            base_url.as_deref().unwrap_or_default(),
            self.inner.settings.allow_local,
        )
        .map_err(Failure::from)?;
        OutlineClient::new(
            base,
            &token,
            self.inner.settings.allow_local,
            self.inner.settings.retry,
        )
        .map_err(Failure::from)
    }

    /// The `outline.scan` job: reads the export or the API and stores the scan.
    pub async fn run_scan(&self, import_id: Id, run: &JobRun) -> Result<(), JobError> {
        let row = sqlx::query("SELECT status, source FROM outline_imports WHERE id = $1")
            .bind(import_id.to_string())
            .fetch_optional(self.database().pool())
            .await
            .map_err(retryable)?;
        let Some(row) = row else {
            return Ok(());
        };
        if row.get::<String, _>("status") != "scanning" {
            return Ok(());
        }
        let result = if row.get::<String, _>("source") == "api" {
            match self.api_client(import_id).await {
                Ok(client) => scan_api(&client).await.map_err(Failure::from),
                Err(failure) => Err(failure),
            }
        } else {
            match self.open_archive(import_id).await {
                Ok(file) => {
                    let limits = ArchiveLimits::new(self.inner.settings.max_file_bytes);
                    tokio::task::spawn_blocking(move || {
                        let reader = File::open(&file.path)
                            .map_err(|error| ArchiveError::Read(error.to_string()))?;
                        let mut archive = open_archive(reader).map_err(ArchiveError::from)?;
                        scan_archive(&mut archive, limits)
                    })
                    .await
                    .map_err(|error| Failure::Transient(error.to_string()))
                    .and_then(|scanned| scanned.map_err(Failure::from))
                }
                Err(failure) => Err(failure),
            }
        };
        match result {
            Ok(tree) => {
                let format = match tree.format {
                    Some(OutlineImportFormat::Json) => "json",
                    Some(OutlineImportFormat::Markdown) => "markdown",
                    _ => "api",
                };
                sqlx::query(
                    "UPDATE outline_imports SET status = 'ready', format = $1, tree_json = $2, updated_at = $3 \
                     WHERE id = $4 AND status = 'scanning'",
                )
                .bind(format)
                .bind(serde_json::to_string(&tree).map_err(retryable)?)
                .bind(TimestampMillis::now().as_millis())
                .bind(import_id.to_string())
                .execute(self.database().pool())
                .await
                .map_err(retryable)?;
                Ok(())
            }
            Err(failure) => self.job_failure(import_id, failure, run).await,
        }
    }

    /// The `outline.import` job.
    pub async fn run_import(&self, import_id: Id, run: &JobRun) -> Result<(), JobError> {
        match self.import(import_id, run).await {
            Ok(()) => {}
            Err(failure) => self.job_failure(import_id, failure, run).await?,
        }
        let status: Option<String> =
            sqlx::query_scalar("SELECT status FROM outline_imports WHERE id = $1")
                .bind(import_id.to_string())
                .fetch_optional(self.database().pool())
                .await
                .map_err(retryable)?;
        if matches!(status.as_deref(), Some("failed" | "cancelled")) {
            self.trash_unfilled_pages(import_id)
                .await
                .map_err(retryable)?;
        }
        self.release_final_archives().await.map_err(retryable)?;
        Ok(())
    }

    async fn is_running(&self, import_id: Id) -> Result<bool, OutlineImportError> {
        let import_id_of_job = self.import_id_of_job();
        let Flag(running) = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT EXISTS (SELECT 1 FROM jobs WHERE jobs.kind = '{IMPORT_JOB}' \
             AND jobs.state = 'running' AND {import_id_of_job} = $1)"
        )))
        .bind(import_id.to_string())
        .fetch_one(self.database().pool())
        .await?;
        Ok(running)
    }

    /// Moves the pages that pass 1 created and pass 2 never filled to the trash (never a page
    /// an earlier import made), as for the Obsidian import. Idempotent.
    async fn trash_unfilled_pages(&self, import_id: Id) -> Result<u64, sqlx::Error> {
        let Some(row) =
            sqlx::query("SELECT workspace_id, user_id FROM outline_imports WHERE id = $1")
                .bind(import_id.to_string())
                .fetch_optional(self.database().pool())
                .await?
        else {
            return Ok(0);
        };
        let (Ok(workspace_id), Ok(user_id)) = (
            row.get::<String, _>("workspace_id").parse::<Id>(),
            row.get::<String, _>("user_id").parse::<Id>(),
        ) else {
            return Ok(0);
        };
        let unfilled: Vec<(String, Option<String>, String)> = sqlx::query_as(
            "SELECT item_key, parent_key, page_id FROM outline_import_items \
             WHERE import_id = $1 AND status = 'created' AND reused = 0 AND page_id IS NOT NULL \
             ORDER BY position",
        )
        .bind(import_id.to_string())
        .fetch_all(self.database().pool())
        .await?;
        let unfilled_keys: HashSet<&str> = unfilled.iter().map(|(key, _, _)| key.as_str()).collect();
        let request_id = format!("outline-import-{import_id}");
        let mut trashed = 0;
        for (_, parent, page_id) in &unfilled {
            if parent
                .as_deref()
                .is_some_and(|parent| unfilled_keys.contains(parent))
            {
                continue;
            }
            let Ok(page) = page_id.parse::<Id>() else {
                continue;
            };
            let (version, size, kept): (Option<i64>, i64, i64) = sqlx::query_as(
                "WITH RECURSIVE subtree(id) AS ( \
                     SELECT id FROM pages WHERE id = $1 AND deleted_at IS NULL \
                     UNION ALL \
                     SELECT pages.id FROM pages JOIN subtree ON pages.parent_id = subtree.id \
                     WHERE pages.deleted_at IS NULL \
                 ) \
                 SELECT (SELECT version FROM pages WHERE id = $2), COUNT(*), \
                        CAST(COALESCE(SUM(CASE WHEN pages.content_json <> '[]' OR pages.id NOT IN ( \
                            SELECT page_id FROM outline_import_items \
                            WHERE import_id = $3 AND status = 'created' AND reused = 0 \
                            AND page_id IS NOT NULL) \
                            THEN 1 ELSE 0 END), 0) AS BIGINT) \
                 FROM pages WHERE pages.id IN (SELECT id FROM subtree)",
            )
            .bind(page_id)
            .bind(page_id)
            .bind(import_id.to_string())
            .fetch_one(self.database().pool())
            .await?;
            let Some(version) = version.and_then(|version| u64::try_from(version).ok()) else {
                continue;
            };
            if size == 0 || kept > 0 {
                continue;
            }
            match self
                .inner
                .pages
                .delete_page(
                    workspace_id,
                    page,
                    user_id,
                    version,
                    &request_id,
                    TimestampMillis::now(),
                )
                .await
            {
                Ok(()) => trashed += u64::try_from(size).unwrap_or_default(),
                Err(PageError::Unavailable(error)) => return Err(error),
                Err(_) => {}
            }
        }
        if trashed > 0 {
            tracing::info!(import_id = %import_id, pages = trashed, "Outline import trashed unfilled pages");
        }
        Ok(trashed)
    }

    async fn job_failure(
        &self,
        import_id: Id,
        failure: Failure,
        run: &JobRun,
    ) -> Result<(), JobError> {
        match failure {
            Failure::Interrupted => Err(JobError::Retryable("interrupted by shutdown".to_owned())),
            Failure::Transient(message) if !run.last_attempt && !run.cancel.is_cancelled() => {
                tracing::warn!(import_id = %import_id, error = %message, "Outline import will retry");
                Err(JobError::Retryable(message))
            }
            Failure::Transient(message) if run.cancel.is_cancelled() => {
                Err(JobError::Retryable(message))
            }
            Failure::Transient(message) | Failure::Fatal(message) | Failure::Page(message) => {
                self.trash_unfilled_pages(import_id)
                    .await
                    .map_err(retryable)?;
                self.finish(import_id, "failed", Some(&message))
                    .await
                    .map_err(retryable)?;
                self.release_final_archives().await.map_err(retryable)?;
                Ok(())
            }
        }
    }

    async fn import(&self, import_id: Id, run: &JobRun) -> Result<(), Failure> {
        let Some(row) = sqlx::query(
            "SELECT workspace_id, user_id, status, source, format, selection_json, tree_json, \
             target_teamspace_id, target_private, report_json FROM outline_imports WHERE id = $1",
        )
        .bind(import_id.to_string())
        .fetch_optional(self.database().pool())
        .await?
        else {
            return Ok(());
        };
        let status: String = row.get("status");
        if status != "queued" && status != "importing" {
            return Ok(());
        }
        let workspace_id = parse_stored_id(row.get("workspace_id"))?;
        let user_id = parse_stored_id(row.get("user_id"))?;
        self.ensure_writable(workspace_id).await?;
        let selection: Option<StoredSelection> = row
            .get::<Option<String>, _>("selection_json")
            .and_then(|json| serde_json::from_str(&json).ok());
        let parent_page_id = selection
            .as_ref()
            .and_then(|selection| selection.parent_page_id.as_deref())
            .map(|id| id.parse::<Id>())
            .transpose()
            .map_err(|_| Failure::Fatal("The import destination is invalid.".to_owned()))?;
        let teamspace_id = row
            .get::<Option<String>, _>("target_teamspace_id")
            .map(|id| id.parse::<Id>())
            .transpose()
            .map_err(|_| Failure::Fatal("The import destination is invalid.".to_owned()))?;
        let private = row.get::<i64, _>("target_private") != 0;
        let report: StoredReport = row
            .get::<Option<String>, _>("report_json")
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        let tree: ScanResult = row
            .get::<Option<String>, _>("tree_json")
            .and_then(|json| serde_json::from_str(&json).ok())
            .ok_or_else(|| Failure::Fatal("Stored import data is invalid.".to_owned()))?;
        let format = match row.get::<Option<String>, _>("format").as_deref() {
            Some("json") => OutlineImportFormat::Json,
            Some("markdown") => OutlineImportFormat::Markdown,
            _ => OutlineImportFormat::Api,
        };
        sqlx::query(
            "UPDATE outline_imports SET status = 'importing', updated_at = $1 WHERE id = $2 AND status = 'queued'",
        )
        .bind(TimestampMillis::now().as_millis())
        .bind(import_id.to_string())
        .execute(self.database().pool())
        .await?;

        if !private && teamspace_id.is_none() && parent_page_id.is_none() {
            return Err(Failure::Fatal(
                "The destination teamspace was deleted.".to_owned(),
            ));
        }
        let destination_space = SpaceRequest {
            teamspace_id: if parent_page_id.is_some() {
                None
            } else {
                teamspace_id
            },
            private: if parent_page_id.is_some() {
                None
            } else {
                Some(private)
            },
        };
        self.inner
            .pages
            .resolve_destination(workspace_id, user_id, parent_page_id, destination_space)
            .await
            .map_err(|error| match error {
                PageError::Unavailable(error) => Failure::from(error),
                _ => Failure::Fatal("The import destination is no longer available.".to_owned()),
            })?;

        let source = if row.get::<String, _>("source") == "api" {
            Source::Api(self.api_client(import_id).await?)
        } else {
            let file = self.open_archive(import_id).await?;
            let limits = ArchiveLimits::new(self.inner.settings.max_file_bytes);
            let path = file.path.clone();
            let (files, archive) = tokio::task::spawn_blocking(move || {
                let reader =
                    File::open(&path).map_err(|error| ArchiveError::Read(error.to_string()))?;
                let mut archive = open_archive(reader).map_err(ArchiveError::from)?;
                let files = OutlineArchive::open(&mut archive, limits)?;
                Ok::<_, ArchiveError>((files, archive))
            })
            .await
            .map_err(|error| Failure::Transient(error.to_string()))??;
            Source::Archive {
                files: Arc::new(files),
                archive: Arc::new(Mutex::new(archive)),
                _file: file,
            }
        };

        let mut state = Run {
            import_id,
            workspace_id,
            user_id,
            request_id: format!("outline-import-{import_id}"),
            format,
            nodes: tree
                .nodes
                .iter()
                .map(|node| (node.key.clone(), node.clone()))
                .collect(),
            users: tree.users.clone(),
            report,
        };
        let members = self.members(workspace_id).await?;
        let mut items = self.load_items(import_id).await?;

        // Pass 1: create (or find) every page, parents first.
        for index in 0..items.len() {
            if items[index].status != "pending" {
                continue;
            }
            if self.should_stop(import_id, workspace_id, run).await? {
                return stopped(run);
            }
            let parent = items[index]
                .parent_key
                .as_ref()
                .and_then(|parent| {
                    items.iter().find(|item| {
                        &item.key == parent
                            && matches!(item.status.as_str(), "created" | "done" | "skipped")
                    })
                })
                .and_then(|item| item.page_id);
            let result = self
                .create_item_page(
                    &state,
                    &members,
                    &mut items[index],
                    parent,
                    parent_page_id,
                    destination_space,
                )
                .await;
            match result {
                Ok(()) => items[index].status = "created".to_owned(),
                Err(Failure::Page(message)) => {
                    self.fail_item(import_id, &items[index].key, &message)
                        .await?;
                    items[index].status = "failed".to_owned();
                }
                Err(other) => return Err(other),
            }
        }

        // Pass 2: every page exists, so links can point to them.
        let lookup = self.lookup(&state, &items, &members).await?;
        let mut collection_cache: Option<(String, CollectionFileContent)> = None;
        for index in 0..items.len() {
            if items[index].status != "created" {
                continue;
            }
            if self.should_stop(import_id, workspace_id, run).await? {
                return stopped(run);
            }
            let item = items[index].clone();
            let result = self
                .fill_item_page(
                    &mut state,
                    &item,
                    &items,
                    &source,
                    &lookup,
                    &mut collection_cache,
                )
                .await;
            match result {
                Ok(outcome) => {
                    let status = if outcome == Filled::Unchanged {
                        "skipped"
                    } else {
                        "done"
                    };
                    sqlx::query(
                        "UPDATE outline_import_items SET status = $1, error = NULL \
                         WHERE import_id = $2 AND item_key = $3",
                    )
                    .bind(status)
                    .bind(import_id.to_string())
                    .bind(&item.key)
                    .execute(self.database().pool())
                    .await?;
                    let (done, skipped) = if outcome == Filled::Unchanged {
                        (0, 1)
                    } else {
                        (1, 0)
                    };
                    self.save_progress(import_id, &state.report, done, skipped)
                        .await?;
                    items[index].status = status.to_owned();
                }
                Err(Failure::Page(message)) => {
                    self.fail_item(import_id, &item.key, &message).await?;
                    self.save_progress(import_id, &state.report, 0, 0).await?;
                    items[index].status = "failed".to_owned();
                }
                Err(other) => {
                    self.save_progress(import_id, &state.report, 0, 0).await?;
                    return Err(other);
                }
            }
        }

        sqlx::query(
            "UPDATE outline_imports SET status = 'completed', token_ciphertext = NULL, updated_at = $1 \
             WHERE id = $2 AND status = 'importing'",
        )
        .bind(TimestampMillis::now().as_millis())
        .bind(import_id.to_string())
        .execute(self.database().pool())
        .await?;
        Ok(())
    }

    /// Active, non-guest members by lowercase email.
    async fn members(&self, workspace_id: Id) -> Result<HashMap<String, ResolvedMember>, Failure> {
        let rows = sqlx::query(
            "SELECT users.id, users.display_name, users.normalized_email FROM memberships \
             JOIN users ON users.id = memberships.user_id \
             WHERE memberships.workspace_id = $1 AND memberships.role <> 'guest' \
             AND users.suspended_at IS NULL",
        )
        .bind(workspace_id.to_string())
        .fetch_all(self.database().pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| {
                (
                    row.get::<String, _>("normalized_email").to_lowercase(),
                    ResolvedMember {
                        user_id: row.get("id"),
                        name: row.get("display_name"),
                    },
                )
            })
            .collect())
    }

    /// The live page an earlier import made for `external_id`.
    async fn existing_page(&self, workspace_id: Id, external_id: &str) -> Result<Option<Id>, Failure> {
        let page: Option<String> = sqlx::query_scalar(
            "SELECT refs.page_id FROM page_external_refs AS refs \
             JOIN pages ON pages.id = refs.page_id \
             WHERE refs.workspace_id = $1 AND refs.external_source = $2 AND refs.external_id = $3 \
             AND pages.deleted_at IS NULL",
        )
        .bind(workspace_id.to_string())
        .bind(SOURCE)
        .bind(external_id)
        .fetch_optional(self.database().pool())
        .await?;
        page.map(|id| parse_stored_id(id)).transpose()
    }

    async fn record_ref(
        &self,
        workspace_id: Id,
        external_id: &str,
        page_id: Id,
        source_updated_at: Option<&str>,
    ) -> Result<(), Failure> {
        let now = TimestampMillis::now().as_millis();
        sqlx::query(
            "INSERT INTO page_external_refs (workspace_id, external_source, external_id, page_id, \
             source_updated_at, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (workspace_id, external_source, external_id) DO UPDATE SET \
             page_id = excluded.page_id, source_updated_at = excluded.source_updated_at, \
             updated_at = excluded.updated_at",
        )
        .bind(workspace_id.to_string())
        .bind(SOURCE)
        .bind(clamp(external_id, 512, 2_000))
        .bind(page_id.to_string())
        .bind(source_updated_at)
        .bind(now)
        .bind(now)
        .execute(self.database().pool())
        .await?;
        Ok(())
    }

    async fn create_item_page(
        &self,
        state: &Run,
        members: &HashMap<String, ResolvedMember>,
        item: &mut Item,
        parent: Option<Id>,
        destination_parent: Option<Id>,
        destination_space: SpaceRequest,
    ) -> Result<(), Failure> {
        let node = state
            .nodes
            .get(&item.key)
            .ok_or_else(|| Failure::Page("The page is missing from the scan.".to_owned()))?;
        let is_root = item.parent_key.is_none();
        if item.page_id.is_none()
            && let Some(existing) = self.existing_page(state.workspace_id, &node.external_id).await?
        {
            sqlx::query(
                "UPDATE outline_import_items SET page_id = $1, reused = 1, status = 'created' \
                 WHERE import_id = $2 AND item_key = $3",
            )
            .bind(existing.to_string())
            .bind(state.import_id.to_string())
            .bind(&item.key)
            .execute(self.database().pool())
            .await?;
            item.page_id = Some(existing);
            item.reused = true;
            return Ok(());
        }
        if item.reused {
            sqlx::query(
                "UPDATE outline_import_items SET status = 'created' WHERE import_id = $1 AND item_key = $2",
            )
            .bind(state.import_id.to_string())
            .bind(&item.key)
            .execute(self.database().pool())
            .await?;
            return Ok(());
        }
        let page_id = match item.page_id {
            Some(page_id) => page_id,
            None => {
                let page_id = Id::new_v7();
                sqlx::query(
                    "UPDATE outline_import_items SET page_id = $1 WHERE import_id = $2 AND item_key = $3",
                )
                .bind(page_id.to_string())
                .bind(state.import_id.to_string())
                .bind(&item.key)
                .execute(self.database().pool())
                .await?;
                item.page_id = Some(page_id);
                page_id
            }
        };
        let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pages WHERE id = $1")
            .bind(page_id.to_string())
            .fetch_one(self.database().pool())
            .await?;
        if exists == 0 {
            let (parent_id, space) = match parent {
                Some(parent) => (Some(parent), SpaceRequest::default()),
                None => (destination_parent, destination_space),
            };
            let title = clamp(node.title.trim(), MAX_TITLE_CHARS, MAX_TITLE_BYTES);
            let input = CreatePage {
                kind: PageKind::Page,
                parent_id,
                space,
                title: if title.is_empty() {
                    "Untitled".to_owned()
                } else {
                    title
                },
                icon: node.icon.clone().filter(|icon| icon.chars().count() <= 64),
                position: None,
            };
            let created = node
                .created_at
                .as_deref()
                .and_then(parse_instant_millis)
                .map_or_else(TimestampMillis::now, TimestampMillis::from_millis);
            match self
                .inner
                .pages
                .create_page_with_id(
                    page_id,
                    state.workspace_id,
                    state.user_id,
                    input,
                    &state.request_id,
                    created,
                )
                .await
            {
                Ok(_) => {}
                Err(PageError::Unavailable(error)) => return Err(Failure::from(error)),
                Err(PageError::TrialLimit(reached)) => {
                    return Err(Failure::Fatal(format!(
                        "The import stopped at the trial's limit. {}",
                        reached.detail()
                    )));
                }
                Err(_) if is_root => {
                    return Err(Failure::Fatal(
                        "The import destination is no longer available.".to_owned(),
                    ));
                }
                Err(_) => {
                    return Err(Failure::Page(
                        "The parent page was deleted during the import.".to_owned(),
                    ));
                }
            }
            // The Outline author becomes the page's creator when they are a member here.
            if let Some(author) = node
                .author_email
                .as_deref()
                .and_then(|email| members.get(&email.trim().to_lowercase()))
                && author.user_id != state.user_id.to_string()
            {
                sqlx::query("UPDATE pages SET creator_id = $1 WHERE id = $2 AND workspace_id = $3")
                    .bind(&author.user_id)
                    .bind(page_id.to_string())
                    .bind(state.workspace_id.to_string())
                    .execute(self.database().pool())
                    .await?;
            }
            self.record_ref(state.workspace_id, &node.external_id, page_id, None)
                .await?;
        }
        sqlx::query(
            "UPDATE outline_import_items SET status = 'created' WHERE import_id = $1 AND item_key = $2",
        )
        .bind(state.import_id.to_string())
        .bind(&item.key)
        .execute(self.database().pool())
        .await?;
        sqlx::query("UPDATE outline_imports SET updated_at = $1 WHERE id = $2")
            .bind(TimestampMillis::now().as_millis())
            .bind(state.import_id.to_string())
            .execute(self.database().pool())
            .await?;
        Ok(())
    }

    /// Every Outline id and `urlId` of the scan (and Markdown path) → its page: this import's
    /// pages, else the live pages earlier imports made.
    async fn lookup(
        &self,
        state: &Run,
        items: &[Item],
        members: &HashMap<String, ResolvedMember>,
    ) -> Result<Lookup, Failure> {
        let earlier: HashMap<String, String> = sqlx::query_as::<_, (String, String)>(
            "SELECT refs.external_id, refs.page_id FROM page_external_refs AS refs \
             JOIN pages ON pages.id = refs.page_id \
             WHERE refs.workspace_id = $1 AND refs.external_source = $2 AND pages.deleted_at IS NULL",
        )
        .bind(state.workspace_id.to_string())
        .bind(SOURCE)
        .fetch_all(self.database().pool())
        .await?
        .into_iter()
        .collect();
        let ours: HashMap<&str, Id> = items
            .iter()
            .filter(|item| item.status != "failed")
            .filter_map(|item| Some((item.key.as_str(), item.page_id?)))
            .collect();
        let mut documents = HashMap::new();
        let mut collections = HashMap::new();
        let mut markdown_pages = HashMap::new();
        for node in state.nodes.values() {
            let page = ours
                .get(node.key.as_str())
                .map(|page| page_href(*page))
                .or_else(|| earlier.get(&node.external_id).map(|page| format!("/docs/{page}")));
            let Some(href) = page else {
                continue;
            };
            let target = match node.kind() {
                OutlineNodeKind::Collection => &mut collections,
                OutlineNodeKind::Document | OutlineNodeKind::Template => &mut documents,
                OutlineNodeKind::Templates => continue,
            };
            if let Some(id) = &node.id {
                target.insert(id.clone(), href.clone());
            }
            if let Some(url_id) = &node.url_id {
                target.insert(url_id.clone(), href.clone());
            }
            if state.format == OutlineImportFormat::Markdown
                && let Some(path) = &node.source
            {
                markdown_pages.insert(path.to_lowercase(), href.clone());
            }
        }
        let people = state
            .users
            .iter()
            .filter_map(|(outline_id, email)| {
                members
                    .get(&email.trim().to_lowercase())
                    .map(|member| (outline_id.clone(), member.clone()))
            })
            .collect();
        Ok(Lookup {
            documents: Arc::new(documents),
            collections: Arc::new(collections),
            members: Arc::new(people),
            markdown_pages: Arc::new(markdown_pages),
            files: HashMap::new(),
        })
    }

    async fn fill_item_page(
        &self,
        state: &mut Run,
        item: &Item,
        items: &[Item],
        source: &Source,
        lookup: &Lookup,
        collection_cache: &mut Option<(String, CollectionFileContent)>,
    ) -> Result<Filled, Failure> {
        let page_id = item
            .page_id
            .ok_or_else(|| Failure::Page("The page was not created.".to_owned()))?;
        let node = state
            .nodes
            .get(&item.key)
            .cloned()
            .ok_or_else(|| Failure::Page("The page is missing from the scan.".to_owned()))?;
        let request_id = format!("outline-import-{page_id}");
        if item.reused {
            let stored: Option<Option<String>> = sqlx::query_scalar(
                "SELECT source_updated_at FROM page_external_refs \
                 WHERE workspace_id = $1 AND external_source = $2 AND external_id = $3",
            )
            .bind(state.workspace_id.to_string())
            .bind(SOURCE)
            .bind(&node.external_id)
            .fetch_optional(self.database().pool())
            .await?;
            if let (Some(Some(stored)), Some(current)) = (stored, fingerprint(&node))
                && stored == current
            {
                state.report.pages_unchanged += 1;
                self.add_attachments_done(state.import_id, u64::from(node.attachments))
                    .await?;
                return Ok(Filled::Unchanged);
            }
        }

        // The content, as Outline gave it.
        let mut attachments: HashMap<String, ExportAttachment> = HashMap::new();
        let content = match (&node.kind(), source) {
            (OutlineNodeKind::Collection, _) => match (&node.description_data, &node.description) {
                (Some(data), _) if !data.plain_text().trim().is_empty() || has_media(data) => {
                    if let Some(path) = &node.source
                        && state.format == OutlineImportFormat::Json
                    {
                        let file = cached_collection(source, path, collection_cache).await?;
                        attachments.clone_from(&file.attachments);
                    }
                    Content::ProseMirror(data.clone())
                }
                (_, Some(markdown)) if !markdown.trim().is_empty() => {
                    Content::Markdown(markdown.clone())
                }
                _ => Content::ChildList,
            },
            (OutlineNodeKind::Templates, _) => Content::ChildList,
            (_, Source::Api(client)) => {
                let id = node
                    .id
                    .as_deref()
                    .ok_or_else(|| Failure::Page("The document has no Outline id.".to_owned()))?;
                let document = if node.kind() == OutlineNodeKind::Template {
                    client.template(id).await?
                } else {
                    client.document(id).await?
                };
                match (document.data, document.text) {
                    (Some(data), _) => Content::ProseMirror(data),
                    (None, Some(text)) => Content::Markdown(text),
                    (None, None) => Content::ProseMirror(PmNode::default()),
                }
            }
            (_, Source::Archive { .. }) if state.format == OutlineImportFormat::Markdown => {
                let path = node
                    .source
                    .as_deref()
                    .ok_or_else(|| Failure::Page("The document file is unknown.".to_owned()))?;
                let bytes = source.read(path).await?;
                Content::Markdown(String::from_utf8_lossy(&bytes).into_owned())
            }
            (_, Source::Archive { .. }) => {
                let path = node
                    .source
                    .clone()
                    .ok_or_else(|| Failure::Page("The collection file is unknown.".to_owned()))?;
                let id = node.id.clone().unwrap_or_default();
                let file = cached_collection(source, &path, collection_cache).await?;
                attachments.clone_from(&file.attachments);
                let raw = file
                    .documents
                    .get(&id)
                    .ok_or_else(|| Failure::Page("The document is missing from the export.".to_owned()))?;
                let document: ExportDocumentContent = serde_json::from_str(raw.get())
                    .map_err(|error| Failure::Page(format!("The document could not be read: {error}.")))?;
                Content::ProseMirror(document.data.unwrap_or_default())
            }
        };

        // Attachments become page files; links and mentions point to the imported pages.
        let mut page_lookup = lookup.clone();
        let mut warnings: Vec<Warning> = Vec::new();
        let blocks = match content {
            Content::ProseMirror(doc) => {
                let ids = doc.attachment_ids();
                let expected = u64::from(node.attachments);
                if ids.len() as u64 > expected {
                    self.add_attachments_total(state.import_id, ids.len() as u64 - expected)
                        .await?;
                }
                for id in &ids {
                    let fetched = self
                        .fetch_attachment(source, id, &attachments, &doc)
                        .await;
                    match fetched {
                        Ok(Some((name, content_type, bytes))) => {
                            match self
                                .store_file(state, page_id, &name, content_type.as_deref(), &bytes, &request_id)
                                .await?
                            {
                                Some(file) => {
                                    page_lookup.files.insert(id.clone(), file);
                                }
                                None => state.report.files_failed += 1,
                            }
                        }
                        Ok(None) => state.report.files_failed += 1,
                        Err(Failure::Page(_)) => state.report.files_failed += 1,
                        Err(other) => return Err(other),
                    }
                    self.add_attachments_done(state.import_id, 1).await?;
                }
                let (blocks, found) = tokio::task::spawn_blocking(move || {
                    let mut converter = Converter::new(&page_lookup);
                    let blocks = converter.convert(&doc);
                    (blocks, converter.into_warnings())
                })
                .await
                .map_err(|error| Failure::Transient(error.to_string()))?;
                warnings.extend(found);
                blocks
            }
            Content::Markdown(markdown) => {
                let markdown = prepare_markdown(&markdown, &node.title);
                let folder = node
                    .source
                    .as_deref()
                    .map(folder_of)
                    .unwrap_or_default()
                    .to_owned();
                if state.format == OutlineImportFormat::Markdown {
                    let mut seen: HashSet<String> = HashSet::new();
                    for (target, _) in obsidian_links(&markdown) {
                        let Some(path) = join_path(&folder, &target) else {
                            continue;
                        };
                        let key = path.to_lowercase();
                        if key.ends_with(".md") || !seen.insert(key.clone()) {
                            continue;
                        }
                        let available = matches!(source, Source::Archive { files, .. } if files.get(&path).is_some());
                        if !available {
                            continue;
                        }
                        match source.read(&path).await {
                            Ok(bytes) => {
                                let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
                                match self
                                    .store_file(state, page_id, &name, None, &bytes, &request_id)
                                    .await?
                                {
                                    Some(file) => {
                                        page_lookup.files.insert(key, file);
                                    }
                                    None => {
                                        state.report.files_failed += 1;
                                        warnings.push(Warning::AttachmentFailed(name));
                                    }
                                }
                            }
                            Err(Failure::Page(_)) => {
                                state.report.files_failed += 1;
                                warnings.push(Warning::AttachmentFailed(path.clone()));
                            }
                            Err(other) => return Err(other),
                        }
                        self.add_attachments_done(state.import_id, 1).await?;
                    }
                }
                let resolver = MarkdownLinks {
                    lookup: page_lookup,
                    folder,
                    unresolved: std::cell::RefCell::new(Vec::new()),
                };
                let (blocks, unresolved) = tokio::task::spawn_blocking(move || {
                    let blocks = obsidian_to_blocks(&markdown, &resolver);
                    (blocks, resolver.unresolved.into_inner())
                })
                .await
                .map_err(|error| Failure::Transient(error.to_string()))?;
                warnings.extend(unresolved.into_iter().map(Warning::UnresolvedLink));
                blocks
            }
            Content::ChildList => items
                .iter()
                .filter(|child| child.parent_key.as_deref() == Some(item.key.as_str()))
                .filter(|child| child.status != "failed")
                .filter_map(|child| child.page_id)
                .map(|child| {
                    json!({
                        "id": Id::new_v7().to_string(),
                        "type": "page",
                        "props": {"pageId": child.to_string()},
                        "children": []
                    })
                })
                .collect(),
        };
        let mut content = blocks;
        let cut = cut_to_size(&mut content, MAX_CONTENT_BYTES);
        if cut > 0 {
            state.report.truncated_blocks = state
                .report
                .truncated_blocks
                .saturating_add(u32::try_from(cut).unwrap_or(u32::MAX));
            content.push(json!({
                "id": Id::new_v7().to_string(),
                "type": "paragraph",
                "props": {},
                "content": [{"type": "text", "text": TRUNCATION_NOTE, "styles": {"italic": true}}],
                "children": [],
            }));
        }

        let current = self
            .inner
            .pages
            .get_page(state.workspace_id, page_id, state.user_id)
            .await
            .map_err(page_failure)?;
        let title = clamp(node.title.trim(), MAX_TITLE_CHARS, MAX_TITLE_BYTES);
        self.inner
            .pages
            .update_page(
                state.workspace_id,
                page_id,
                state.user_id,
                current.version,
                PageChanges {
                    title: item.reused.then(|| {
                        if title.is_empty() {
                            "Untitled".to_owned()
                        } else {
                            title
                        }
                    }),
                    icon: item
                        .reused
                        .then(|| node.icon.clone().filter(|icon| icon.chars().count() <= 64)),
                    content: Some(content),
                    origin: SaveOrigin::Import,
                    ..PageChanges::default()
                },
                &request_id,
                TimestampMillis::now(),
            )
            .await
            .map_err(page_failure)?;
        // Keep Outline's last edit time.
        if let Some(updated) = node.updated_at.as_deref().and_then(parse_instant_millis) {
            sqlx::query("UPDATE pages SET updated_at = $1 WHERE id = $2 AND workspace_id = $3")
                .bind(updated)
                .bind(page_id.to_string())
                .bind(state.workspace_id.to_string())
                .execute(self.database().pool())
                .await?;
        }
        self.record_ref(
            state.workspace_id,
            &node.external_id,
            page_id,
            fingerprint(&node).as_deref(),
        )
        .await?;
        self.save_warnings(state, &item.key, &node.title, &warnings)
            .await?;
        if item.reused {
            state.report.pages_updated += 1;
        } else {
            state.report.pages_created += 1;
        }
        Ok(Filled::Written)
    }

    /// The bytes of one attachment: `(file name, content type, bytes)`, or `None` when the
    /// export does not have it.
    async fn fetch_attachment(
        &self,
        source: &Source,
        id: &str,
        attachments: &HashMap<String, ExportAttachment>,
        doc: &PmNode,
    ) -> Result<Option<(String, Option<String>, Vec<u8>)>, Failure> {
        match source {
            Source::Archive { files, .. } => {
                let Some(attachment) = attachments.get(id) else {
                    return Ok(None);
                };
                let Some(path) = join_path("", &attachment.key) else {
                    return Ok(None);
                };
                if files.get(&path).is_none() {
                    return Ok(None);
                }
                let bytes = source.read(&path).await?;
                let name = if attachment.name.trim().is_empty() {
                    path.rsplit('/').next().unwrap_or("file").to_owned()
                } else {
                    attachment.name.clone()
                };
                Ok(Some((name, attachment.content_type.clone(), bytes)))
            }
            Source::Api(client) => {
                match client
                    .attachment(id, self.inner.settings.max_file_bytes)
                    .await
                {
                    Ok(download) => {
                        let name = download
                            .file_name
                            .clone()
                            .or_else(|| attachment_title(doc, id))
                            .unwrap_or_else(|| "file".to_owned());
                        let name = with_extension(&name, download.content_type.as_deref());
                        Ok(Some((name, download.content_type, download.bytes)))
                    }
                    Err(OutlineError::Unauthorized) => Err(Failure::from(OutlineError::Unauthorized)),
                    Err(OutlineError::RateLimited | OutlineError::Unreachable(_)) => {
                        // One attachment that cannot be fetched now does not stop the import.
                        Ok(None)
                    }
                    Err(_) => Ok(None),
                }
            }
        }
    }

    /// Stores a file on the page (once: a retried page reuses the file an earlier attempt
    /// stored). `None` when Orbit refuses it (type or size).
    async fn store_file(
        &self,
        state: &mut Run,
        page_id: Id,
        name: &str,
        content_type: Option<&str>,
        bytes: &[u8],
        request_id: &str,
    ) -> Result<Option<ResolvedFile>, Failure> {
        let name = sanitize_file_name(name);
        let sha256 = format!("{:x}", Sha256::digest(bytes));
        let attached = self.attached_files(state.workspace_id, page_id).await?;
        let url = match attached.iter().find(|(hash, size, _)| {
            *hash == sha256 && usize::try_from(*size).ok() == Some(bytes.len())
        }) {
            Some((_, _, url)) => url.clone(),
            None => match self
                .inner
                .files
                .create_from_bytes(state.workspace_id, page_id, state.user_id, &name, bytes, request_id)
                .await
            {
                Ok(stored) => {
                    state.report.files_imported += 1;
                    stored.url
                }
                Err(error) => {
                    tracing::debug!(error = %error, "Outline attachment could not be attached");
                    return Ok(None);
                }
            },
        };
        Ok(Some(ResolvedFile {
            url,
            is_image: is_image(&name, content_type),
            name,
        }))
    }

    async fn attached_files(
        &self,
        workspace_id: Id,
        page_id: Id,
    ) -> Result<Vec<(String, i64, String)>, Failure> {
        let rows: Vec<(String, String, i64)> = sqlx::query_as(
            "SELECT page_files.id, attachment_blobs.sha256, attachment_blobs.byte_size \
             FROM page_files JOIN attachment_blobs ON attachment_blobs.id = page_files.blob_id \
             WHERE page_files.workspace_id = $1 AND page_files.page_id = $2",
        )
        .bind(workspace_id.to_string())
        .bind(page_id.to_string())
        .fetch_all(self.database().pool())
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|(file_id, sha256, size)| {
                let file_id = file_id.parse().ok()?;
                Some((sha256, size, page_file_url(workspace_id, page_id, file_id)))
            })
            .collect())
    }

    /// Replaces the page's warnings (a page filled again after a restart) and counts them.
    async fn save_warnings(
        &self,
        state: &mut Run,
        key: &str,
        title: &str,
        warnings: &[Warning],
    ) -> Result<(), Failure> {
        let mut tx = self.database().immediate_transaction().await?;
        let removed = sqlx::query(
            "DELETE FROM outline_import_warnings WHERE import_id = $1 AND item_key = $2",
        )
        .bind(state.import_id.to_string())
        .bind(key)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        let next: Option<i64> = sqlx::query_scalar(
            "SELECT MAX(position) FROM outline_import_warnings WHERE import_id = $1",
        )
        .bind(state.import_id.to_string())
        .fetch_one(&mut *tx)
        .await?;
        let mut position = next.map_or(0, |last| last + 1);
        for warning in warnings {
            if position >= MAX_WARNINGS {
                break;
            }
            sqlx::query(
                "INSERT INTO outline_import_warnings (import_id, position, item_key, title, kind, detail) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(state.import_id.to_string())
            .bind(position)
            .bind(key)
            .bind(clamp(title, MAX_TITLE_CHARS, MAX_TITLE_BYTES))
            .bind(warning.kind())
            .bind(clamp(warning.detail(), 500, 2_000))
            .execute(&mut *tx)
            .await?;
            position += 1;
        }
        tx.commit().await?;
        let removed = u32::try_from(removed).unwrap_or(u32::MAX);
        state.report.warnings = state
            .report
            .warnings
            .saturating_sub(removed)
            .saturating_add(u32::try_from(warnings.len()).unwrap_or(u32::MAX));
        Ok(())
    }

    async fn add_attachments_done(&self, import_id: Id, count: u64) -> Result<(), Failure> {
        if count == 0 {
            return Ok(());
        }
        sqlx::query(
            "UPDATE outline_imports SET attachments_done = attachments_done + $1, \
             attachments_total = CASE WHEN attachments_total < attachments_done + $2 \
                 THEN attachments_done + $3 ELSE attachments_total END, updated_at = $4 WHERE id = $5",
        )
        .bind(i64::try_from(count).unwrap_or(i64::MAX))
        .bind(i64::try_from(count).unwrap_or(i64::MAX))
        .bind(i64::try_from(count).unwrap_or(i64::MAX))
        .bind(TimestampMillis::now().as_millis())
        .bind(import_id.to_string())
        .execute(self.database().pool())
        .await?;
        Ok(())
    }

    async fn add_attachments_total(&self, import_id: Id, count: u64) -> Result<(), Failure> {
        sqlx::query(
            "UPDATE outline_imports SET attachments_total = attachments_total + $1 WHERE id = $2",
        )
        .bind(i64::try_from(count).unwrap_or(i64::MAX))
        .bind(import_id.to_string())
        .execute(self.database().pool())
        .await?;
        Ok(())
    }

    async fn should_stop(&self, import_id: Id, workspace_id: Id, run: &JobRun) -> Result<bool, Failure> {
        if run.cancel.is_cancelled() {
            return Ok(true);
        }
        self.ensure_writable(workspace_id).await?;
        let status: Option<String> =
            sqlx::query_scalar("SELECT status FROM outline_imports WHERE id = $1")
                .bind(import_id.to_string())
                .fetch_optional(self.database().pool())
                .await?;
        Ok(status.as_deref() != Some("importing"))
    }

    async fn ensure_writable(&self, workspace_id: Id) -> Result<(), Failure> {
        match billing::write_access(self.database(), workspace_id).await? {
            WorkspaceAccess::Full | WorkspaceAccess::Trial => Ok(()),
            WorkspaceAccess::ReadOnly => Err(Failure::Fatal(
                "The import stopped because the workspace is read-only: it has no active \
                 subscription."
                    .to_owned(),
            )),
        }
    }

    async fn fail_item(&self, import_id: Id, key: &str, message: &str) -> Result<(), Failure> {
        let mut tx = self.database().immediate_transaction().await?;
        let changed = sqlx::query(
            "UPDATE outline_import_items SET status = 'failed', error = $1 \
             WHERE import_id = $2 AND item_key = $3 AND status <> 'failed'",
        )
        .bind(clamp(message, 500, 2_000))
        .bind(import_id.to_string())
        .bind(key)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        sqlx::query("UPDATE outline_imports SET failed = failed + $1, updated_at = $2 WHERE id = $3")
            .bind(changed as i64)
            .bind(TimestampMillis::now().as_millis())
            .bind(import_id.to_string())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn save_progress(
        &self,
        import_id: Id,
        report: &StoredReport,
        done: i64,
        skipped: i64,
    ) -> Result<(), Failure> {
        sqlx::query(
            "UPDATE outline_imports SET done = done + $1, skipped = skipped + $2, report_json = $3, \
             updated_at = $4 WHERE id = $5",
        )
        .bind(done)
        .bind(skipped)
        .bind(serde_json::to_string(report).unwrap_or_default())
        .bind(TimestampMillis::now().as_millis())
        .bind(import_id.to_string())
        .execute(self.database().pool())
        .await?;
        Ok(())
    }

    /// Moves an open import to a final state and wipes its token; its archive is deleted by
    /// [`Self::release_final_archives`].
    async fn finish(&self, import_id: Id, status: &str, error: Option<&str>) -> Result<(), sqlx::Error> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE outline_imports SET status = $1, error = $2, token_ciphertext = NULL, updated_at = $3 \
             WHERE id = $4 AND status IN {OPEN}"
        )))
        .bind(status)
        .bind(error.map(|error| clamp(error, 500, 2_000)))
        .bind(TimestampMillis::now().as_millis())
        .bind(import_id.to_string())
        .execute(self.database().pool())
        .await?;
        Ok(())
    }

    async fn enqueue(
        &self,
        kind: JobKind,
        import_id: Id,
        workspace_id: Id,
    ) -> Result<(), OutlineImportError> {
        let job = Job::new(
            kind,
            json!({"import_id": import_id.to_string()}),
            TimestampMillis::now(),
        )
        .with_workspace(workspace_id.to_string());
        self.inner
            .jobs
            .enqueue(job)
            .await
            .map(|_| ())
            .map_err(|error| OutlineImportError::Unavailable(error.to_string()))
    }

    /// Only workspace admins (who manage members) import from Outline: an import keeps
    /// other members as page authors.
    async fn require_admin(&self, workspace_id: Id, user_id: Id) -> Result<(), OutlineImportError> {
        let actor = membership::actor(self.database().pool(), workspace_id, user_id)
            .await?
            .ok_or(OutlineImportError::NotFound)?;
        if actor.can(Permission::MembersManage) {
            Ok(())
        } else {
            Err(OutlineImportError::Forbidden)
        }
    }

    async fn has_active_import(&self, user_id: Id) -> Result<bool, OutlineImportError> {
        let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT COUNT(*) FROM outline_imports WHERE user_id = $1 AND status IN {ACTIVE}"
        )))
        .bind(user_id.to_string())
        .fetch_one(self.database().pool())
        .await?;
        Ok(count > 0)
    }

    async fn load_items(&self, import_id: Id) -> Result<Vec<Item>, Failure> {
        let rows = sqlx::query(
            "SELECT item_key, parent_key, page_id, reused, status \
             FROM outline_import_items WHERE import_id = $1 ORDER BY position",
        )
        .bind(import_id.to_string())
        .fetch_all(self.database().pool())
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(Item {
                    key: row.get("item_key"),
                    parent_key: row.get("parent_key"),
                    page_id: row
                        .get::<Option<String>, _>("page_id")
                        .map(|id| id.parse())
                        .transpose()
                        .map_err(|_| Failure::Fatal("Stored import data is invalid.".to_owned()))?,
                    reused: row.get::<i64, _>("reused") != 0,
                    status: row.get("status"),
                })
            })
            .collect()
    }

    async fn load_view(
        &self,
        workspace_id: Id,
        user_id: Id,
        import_id: Id,
        with_preview: bool,
    ) -> Result<OutlineImport, OutlineImportError> {
        let row = sqlx::query(
            "SELECT id, workspace_id, source, status, format, file_name, archive_bytes, base_url, \
             CASE WHEN $1 = 1 THEN tree_json END AS tree_json, selection_json, target_teamspace_id, \
             target_private, target_parent_page_id, total, done, failed, skipped, attachments_total, \
             attachments_done, report_json, error, created_at, updated_at FROM outline_imports \
             WHERE id = $2 AND workspace_id = $3 AND user_id = $4",
        )
        .bind(i64::from(with_preview))
        .bind(import_id.to_string())
        .bind(workspace_id.to_string())
        .bind(user_id.to_string())
        .fetch_optional(self.database().pool())
        .await?
        .ok_or(OutlineImportError::NotFound)?;
        let invalid = |_| OutlineImportError::Unavailable("invalid stored import".to_owned());
        let status = OutlineImportStatus::parse(&row.get::<String, _>("status"))?;
        let selection: Option<StoredSelection> = row
            .get::<Option<String>, _>("selection_json")
            .and_then(|json| serde_json::from_str(&json).ok());
        let started = selection.is_some();
        let preview = row
            .get::<Option<String>, _>("tree_json")
            .map(|json| serde_json::from_str::<ScanResult>(&json))
            .transpose()
            .map_err(|_| OutlineImportError::Unavailable("invalid stored scan".to_owned()))?
            .map(|tree| preview(&tree));
        let destination = if started {
            Some(OutlineImportDestination {
                teamspace_id: row
                    .get::<Option<String>, _>("target_teamspace_id")
                    .map(|id| id.parse())
                    .transpose()
                    .map_err(invalid)?,
                private: row.get::<i64, _>("target_private") != 0,
                parent_page_id: row
                    .get::<Option<String>, _>("target_parent_page_id")
                    .map(|id| id.parse())
                    .transpose()
                    .map_err(invalid)?,
            })
        } else {
            None
        };
        let report = if started {
            let stored: StoredReport = row
                .get::<Option<String>, _>("report_json")
                .and_then(|json| serde_json::from_str(&json).ok())
                .unwrap_or_default();
            let failures = sqlx::query(
                "SELECT item_key, title, error FROM outline_import_items \
                 WHERE import_id = $1 AND status = 'failed' ORDER BY position LIMIT $2",
            )
            .bind(import_id.to_string())
            .bind(MAX_FAILURES_LISTED)
            .fetch_all(self.database().pool())
            .await?
            .into_iter()
            .map(|row| OutlineImportFailure {
                key: row.get("item_key"),
                title: row.get("title"),
                error: row.get::<Option<String>, _>("error").unwrap_or_default(),
            })
            .collect();
            let unfilled_pages_trashed: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM outline_import_items AS items \
                 JOIN pages ON pages.id = items.page_id \
                 WHERE items.import_id = $1 AND items.status = 'created' AND items.reused = 0 \
                 AND pages.deleted_at IS NOT NULL",
            )
            .bind(import_id.to_string())
            .fetch_one(self.database().pool())
            .await?;
            Some(OutlineImportReport {
                pages_created: stored.pages_created,
                pages_updated: stored.pages_updated,
                pages_unchanged: stored.pages_unchanged,
                files_imported: stored.files_imported,
                files_failed: stored.files_failed,
                too_large_files: stored.too_large_files,
                truncated_blocks: stored.truncated_blocks,
                warnings: stored.warnings,
                unfilled_pages_trashed: u32::try_from(unfilled_pages_trashed).unwrap_or_default(),
                failures,
            })
        } else {
            None
        };
        let root_page_ids = sqlx::query_scalar::<_, String>(
            "SELECT items.page_id FROM outline_import_items AS items \
             JOIN pages ON pages.id = items.page_id \
             WHERE items.import_id = $1 AND items.parent_key IS NULL \
             AND items.status IN ('created', 'done', 'skipped') AND pages.workspace_id = $2 \
             AND pages.deleted_at IS NULL AND (pages.teamspace_id IS NOT NULL OR pages.owner_id = $3) \
             ORDER BY items.position",
        )
        .bind(import_id.to_string())
        .bind(workspace_id.to_string())
        .bind(user_id.to_string())
        .fetch_all(self.database().pool())
        .await?
        .into_iter()
        .map(|id| id.parse())
        .collect::<Result<Vec<Id>, _>>()
        .map_err(invalid)?;
        let count = |column: &str| u64::try_from(row.get::<i64, _>(column)).unwrap_or_default();
        let source = if row.get::<String, _>("source") == "api" {
            OutlineImportSource::Api
        } else {
            OutlineImportSource::Archive
        };
        let format = match row.get::<Option<String>, _>("format").as_deref() {
            Some("json") => Some(OutlineImportFormat::Json),
            Some("markdown") => Some(OutlineImportFormat::Markdown),
            Some("api") => Some(OutlineImportFormat::Api),
            _ => None,
        };
        Ok(OutlineImport {
            id: row.get::<String, _>("id").parse().map_err(invalid)?,
            workspace_id: row
                .get::<String, _>("workspace_id")
                .parse()
                .map_err(invalid)?,
            source,
            status,
            format,
            file_name: row.get("file_name"),
            archive_bytes: count("archive_bytes"),
            base_url: row.get("base_url"),
            progress: OutlineImportProgress {
                total: count("total"),
                done: count("done"),
                failed: count("failed"),
                skipped: count("skipped"),
                attachments_total: count("attachments_total"),
                attachments_done: count("attachments_done"),
            },
            error: row.get("error"),
            destination,
            options: selection.map(|selection| selection.options),
            preview,
            report,
            root_page_ids,
            created_at: TimestampMillis::from_millis(row.get("created_at")),
            updated_at: TimestampMillis::from_millis(row.get("updated_at")),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Filled {
    Written,
    Unchanged,
}

enum NewImport<'a> {
    Archive {
        file_name: &'a str,
        size: u64,
        location: Location,
    },
    Api {
        name: &'a str,
        base_url: &'a str,
        ciphertext: &'a [u8],
    },
}

/// The parsed collection file of the JSON export, kept while its documents are imported (they
/// are contiguous in the plan).
async fn cached_collection<'a>(
    source: &Source,
    path: &str,
    cache: &'a mut Option<(String, CollectionFileContent)>,
) -> Result<&'a CollectionFileContent, Failure> {
    if cache.as_ref().is_none_or(|(cached, _)| cached != path) {
        *cache = None;
        let file = source.collection_file(path).await?;
        *cache = Some((path.to_owned(), file));
    }
    cache
        .as_ref()
        .map(|(_, file)| file)
        .ok_or_else(|| Failure::Transient("collection cache".to_owned()))
}

/// What tells whether a node changed since an earlier import: Outline's `updatedAt`, else (a
/// collection or template group, which may have none) a hash of its title and description.
fn fingerprint(node: &ScanNode) -> Option<String> {
    if let Some(updated) = &node.updated_at {
        return Some(updated.clone());
    }
    match node.kind() {
        OutlineNodeKind::Collection | OutlineNodeKind::Templates => {
            let mut hasher = Sha256::new();
            hasher.update(node.title.as_bytes());
            hasher.update([0]);
            hasher.update(node.description.as_deref().unwrap_or_default().as_bytes());
            hasher.update([0]);
            if let Some(data) = &node.description_data {
                hasher.update(format!("{data:?}").as_bytes());
            }
            Some(format!("sha256:{:x}", hasher.finalize()))
        }
        OutlineNodeKind::Document | OutlineNodeKind::Template => None,
    }
}

/// A file name with an extension: Outline's attachment redirect may give none, so one is taken
/// from the content type (Orbit checks a file's type by its name too).
fn with_extension(name: &str, content_type: Option<&str>) -> String {
    let base = name.rsplit('/').next().unwrap_or(name);
    if base.contains('.') {
        return name.to_owned();
    }
    let extension = match content_type.unwrap_or_default() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        "text/csv" => "csv",
        "application/zip" => "zip",
        "video/mp4" => "mp4",
        _ => return name.to_owned(),
    };
    format!("{name}.{extension}")
}

/// Whether a description has images or attachments (it is worth importing without text).
fn has_media(doc: &PmNode) -> bool {
    !doc.attachment_ids().is_empty()
}

/// The title an attachment node gives an attachment id.
fn attachment_title(doc: &PmNode, id: &str) -> Option<String> {
    use super::model::{Attrs, attachment_id};
    if ["href", "src"]
        .iter()
        .any(|name| doc.attr_str(name).and_then(attachment_id).as_deref() == Some(id))
    {
        return doc
            .attr_str("title")
            .or_else(|| doc.attr_str("alt"))
            .map(str::to_owned);
    }
    doc.content
        .iter()
        .find_map(|child| attachment_title(child, id))
}

/// The preview of a scan: collections with their counts.
fn preview(tree: &ScanResult) -> OutlineImportPreview {
    let mut collections: Vec<OutlineCollectionPreview> = Vec::new();
    let mut index: HashMap<&str, usize> = HashMap::new();
    for node in &tree.nodes {
        if node.kind() == OutlineNodeKind::Collection {
            index.insert(node.key.as_str(), collections.len());
            collections.push(OutlineCollectionPreview {
                key: node.key.clone(),
                title: node.title.clone(),
                icon: node.icon.clone(),
                documents: 0,
                archived: 0,
                templates: 0,
                attachments: node.attachments,
            });
        }
    }
    for node in &tree.nodes {
        let Some(&slot) = index.get(node.collection_key.as_str()) else {
            continue;
        };
        let collection = &mut collections[slot];
        match node.kind() {
            OutlineNodeKind::Document => {
                if node.state == DocumentState::Published {
                    collection.documents += 1;
                } else {
                    collection.archived += 1;
                }
                collection.attachments += node.attachments;
            }
            OutlineNodeKind::Template => {
                collection.templates += 1;
            }
            OutlineNodeKind::Collection | OutlineNodeKind::Templates => {}
        }
    }
    OutlineImportPreview {
        format: tree.format.unwrap_or(OutlineImportFormat::Json),
        collections,
        too_large_files: tree.too_large_files,
    }
}

/// The nodes to import, in tree order. A node whose parent is left out (an archived parent
/// without `include_archived`) moves up to its nearest included ancestor.
fn plan_items(
    tree: &ScanResult,
    selection: &ImportSelection,
    options: OutlineImportOptions,
) -> Result<Vec<ScanNode>, OutlineImportError> {
    let collections: HashSet<&str> = tree
        .nodes
        .iter()
        .filter(|node| node.kind() == OutlineNodeKind::Collection)
        .map(|node| node.key.as_str())
        .collect();
    let chosen: HashSet<&str> = match selection {
        ImportSelection::All => collections.clone(),
        ImportSelection::Collections(keys) => {
            if keys.is_empty() {
                return Err(OutlineImportError::Invalid { field: "selection" });
            }
            let mut chosen = HashSet::new();
            for key in keys {
                if !collections.contains(key.as_str()) {
                    return Err(OutlineImportError::Invalid { field: "selection" });
                }
                chosen.insert(key.as_str());
            }
            chosen
        }
    };
    let parents: HashMap<&str, Option<&str>> = tree
        .nodes
        .iter()
        .map(|node| (node.key.as_str(), node.parent_key.as_deref()))
        .collect();
    let mut included: HashSet<String> = HashSet::new();
    let mut plan = Vec::new();
    for node in &tree.nodes {
        if !chosen.contains(node.collection_key.as_str()) {
            continue;
        }
        let wanted = match node.kind() {
            OutlineNodeKind::Collection => true,
            OutlineNodeKind::Document => {
                node.state == DocumentState::Published || options.include_archived
            }
            OutlineNodeKind::Templates | OutlineNodeKind::Template => options.include_templates,
        };
        if !wanted {
            continue;
        }
        let mut planned = node.clone();
        let mut parent = node.parent_key.as_deref();
        while let Some(key) = parent {
            if included.contains(key) {
                break;
            }
            parent = parents.get(key).copied().flatten();
        }
        planned.parent_key = parent.map(str::to_owned);
        included.insert(planned.key.clone());
        plan.push(planned);
    }
    Ok(plan)
}

/// Reads collections, documents (published, archived, deleted), templates and users from the
/// API, in Outline's sidebar order.
async fn scan_api(client: &OutlineClient) -> Result<ScanResult, OutlineError> {
    let mut result = ScanResult {
        format: Some(OutlineImportFormat::Api),
        ..ScanResult::default()
    };
    for user in client.users().await? {
        if let Some(email) = user.email.filter(|email| !email.trim().is_empty()) {
            result.users.insert(user.id, email);
        }
    }
    let mut collections = client.collections().await?;
    collections.sort_by(|a, b| {
        a.index
            .as_deref()
            .unwrap_or_default()
            .cmp(b.index.as_deref().unwrap_or_default())
    });
    let deleted = client.deleted_documents().await.unwrap_or_default();
    let templates = client.templates().await?;
    for collection in collections {
        let collection_key = format!("c:{}", collection.id);
        let mut nodes = vec![ScanNode {
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
            attachments: collection
                .data
                .as_ref()
                .map_or(0, |data| u32::try_from(data.attachment_ids().len()).unwrap_or(0)),
            description: collection.description.clone(),
            description_data: collection.data.clone(),
            ..ScanNode::default()
        }];
        let tree = match client.collection_tree(&collection.id).await {
            Ok(tree) => tree,
            Err(OutlineError::NotFound) => Vec::new(),
            Err(error) => return Err(error),
        };
        let published = client.documents(&collection.id, "published").await?;
        let archived = client.documents(&collection.id, "archived").await?;
        let mut documents: HashMap<String, (ApiDocument, DocumentState)> = HashMap::new();
        let mut order: Vec<String> = Vec::new();
        for (list, state) in [
            (published, DocumentState::Published),
            (archived, DocumentState::Archived),
        ] {
            for document in list {
                // Drafts are never imported.
                if document.published_at.is_none() && document.archived_at.is_none() {
                    continue;
                }
                if !documents.contains_key(&document.id) {
                    order.push(document.id.clone());
                    documents.insert(document.id.clone(), (document, state));
                }
            }
        }
        for document in deleted
            .iter()
            .filter(|document| document.collection_id.as_deref() == Some(collection.id.as_str()))
            .filter(|document| document.published_at.is_some())
        {
            if !documents.contains_key(&document.id) {
                order.push(document.id.clone());
                documents.insert(
                    document.id.clone(),
                    (document.clone(), DocumentState::Deleted),
                );
            }
        }
        let api_node = |document: &ApiDocument, state: DocumentState, parent: &str| ScanNode {
            key: format!("d:{}", document.id),
            kind: Some(OutlineNodeKind::Document),
            title: document.title.clone(),
            parent_key: Some(parent.to_owned()),
            collection_key: collection_key.clone(),
            external_id: format!("document:{}", document.id),
            id: Some(document.id.clone()),
            url_id: Some(document.url_id.clone()).filter(|id| !id.is_empty()),
            icon: emoji_icon(document.icon.as_deref().or(document.emoji.as_deref())),
            created_at: document.created_at.clone(),
            updated_at: document.updated_at.clone(),
            author_email: document
                .created_by
                .as_ref()
                .and_then(|user| user.email.clone()),
            state,
            attachments: document
                .data
                .as_ref()
                .map_or(0, |data| u32::try_from(data.attachment_ids().len()).unwrap_or(0)),
            ..ScanNode::default()
        };
        // The sidebar tree first (published documents, in order), then the others under their
        // parent when it is known, else at the collection's top level.
        let mut placed: HashSet<String> = HashSet::new();
        walk_navigation(&tree, &collection_key, &documents, &mut placed, &mut nodes, &api_node, 0);
        let mut rest: Vec<&String> = order.iter().filter(|id| !placed.contains(*id)).collect();
        rest.sort_by_cached_key(|id| {
            documents
                .get(*id)
                .map(|(document, _)| document.title.to_lowercase())
                .unwrap_or_default()
        });
        for id in rest {
            let Some((document, state)) = documents.get(id) else {
                continue;
            };
            let parent = document
                .parent_document_id
                .as_deref()
                .filter(|parent| documents.contains_key(*parent))
                .map_or_else(|| collection_key.clone(), |parent| format!("d:{parent}"));
            let mut node = api_node(document, *state, &parent);
            if let Some(author) = document.created_by.as_ref()
                && node.author_email.is_none()
            {
                node.author_email = result.users.get(&author.id).cloned();
            }
            nodes.push(node);
        }
        for node in &mut nodes {
            if node.author_email.is_none()
                && let Some(id) = node.id.as_ref()
                && let Some((document, _)) = documents.get(id)
                && let Some(author) = &document.created_by
            {
                node.author_email = result.users.get(&author.id).cloned();
            }
        }
        let collection_templates: Vec<&ApiDocument> = templates
            .iter()
            .filter(|template| template.collection_id.as_deref() == Some(collection.id.as_str()))
            .collect();
        if !collection_templates.is_empty() {
            let group = format!("t:{}", collection.id);
            nodes.push(ScanNode {
                key: group.clone(),
                kind: Some(OutlineNodeKind::Templates),
                title: "Templates".to_owned(),
                parent_key: Some(collection_key.clone()),
                collection_key: collection_key.clone(),
                external_id: format!("templates:{}", collection.id),
                ..ScanNode::default()
            });
            for template in collection_templates {
                nodes.push(ScanNode {
                    key: format!("tpl:{}", template.id),
                    kind: Some(OutlineNodeKind::Template),
                    title: template.title.clone(),
                    parent_key: Some(group.clone()),
                    collection_key: collection_key.clone(),
                    external_id: format!("template:{}", template.id),
                    id: Some(template.id.clone()),
                    url_id: Some(template.url_id.clone()).filter(|id| !id.is_empty()),
                    icon: emoji_icon(template.icon.as_deref()),
                    created_at: template.created_at.clone(),
                    updated_at: template.updated_at.clone(),
                    author_email: template
                        .created_by
                        .as_ref()
                        .and_then(|user| user.email.clone()),
                    attachments: template
                        .data
                        .as_ref()
                        .map_or(0, |data| u32::try_from(data.attachment_ids().len()).unwrap_or(0)),
                    ..ScanNode::default()
                });
            }
        }
        result.nodes.extend(depth_first(nodes));
    }
    Ok(result)
}

fn walk_navigation(
    tree: &[NavigationNode],
    parent: &str,
    documents: &HashMap<String, (ApiDocument, DocumentState)>,
    placed: &mut HashSet<String>,
    out: &mut Vec<ScanNode>,
    node: &dyn Fn(&ApiDocument, DocumentState, &str) -> ScanNode,
    depth: usize,
) {
    if depth > 64 {
        return;
    }
    for entry in tree {
        let Some((document, state)) = documents.get(&entry.id) else {
            continue;
        };
        if !placed.insert(document.id.clone()) {
            continue;
        }
        let scan = node(document, *state, parent);
        let key = scan.key.clone();
        out.push(scan);
        walk_navigation(&entry.children, &key, documents, placed, out, node, depth + 1);
    }
}

/// Whether a file starts like a zip archive with entries (a local file header).
async fn is_zip(path: &Path) -> bool {
    use tokio::io::AsyncReadExt;
    let Ok(mut file) = tokio::fs::File::open(path).await else {
        return false;
    };
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic).await.is_ok() && magic == *b"PK\x03\x04"
}

fn stopped(run: &JobRun) -> Result<(), Failure> {
    if run.cancel.is_cancelled() {
        Err(Failure::Interrupted)
    } else {
        Ok(())
    }
}

fn retryable(error: impl std::fmt::Display) -> JobError {
    JobError::Retryable(error.to_string())
}

fn parse_stored_id(value: String) -> Result<Id, Failure> {
    value
        .parse()
        .map_err(|_| Failure::Fatal("Stored import data is invalid.".to_owned()))
}

fn page_failure(error: PageError) -> Failure {
    match error {
        PageError::Unavailable(error) => Failure::from(error),
        PageError::NotFound => Failure::Page("The page was deleted during the import.".to_owned()),
        PageError::VersionConflict { .. } => {
            Failure::Page("The page was edited during the import.".to_owned())
        }
        PageError::Locked => Failure::Page("The page is locked.".to_owned()),
        other => Failure::Page(format!("The page could not be saved: {other}.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(key: &str, kind: OutlineNodeKind, parent: Option<&str>, state: DocumentState) -> ScanNode {
        ScanNode {
            key: key.to_owned(),
            kind: Some(kind),
            title: key.to_owned(),
            parent_key: parent.map(str::to_owned),
            collection_key: "c:1".to_owned(),
            state,
            ..ScanNode::default()
        }
    }

    #[test]
    fn plans_options_and_moves_children_of_left_out_parents() {
        let tree = ScanResult {
            nodes: vec![
                node("c:1", OutlineNodeKind::Collection, None, DocumentState::Published),
                node("d:a", OutlineNodeKind::Document, Some("c:1"), DocumentState::Archived),
                node("d:b", OutlineNodeKind::Document, Some("d:a"), DocumentState::Published),
                node("t:1", OutlineNodeKind::Templates, Some("c:1"), DocumentState::Published),
                node("tpl:x", OutlineNodeKind::Template, Some("t:1"), DocumentState::Published),
            ],
            ..ScanResult::default()
        };
        let plan = plan_items(&tree, &ImportSelection::All, OutlineImportOptions::default()).unwrap();
        let keys: Vec<(&str, Option<&str>)> = plan
            .iter()
            .map(|node| (node.key.as_str(), node.parent_key.as_deref()))
            .collect();
        assert_eq!(keys, [("c:1", None), ("d:b", Some("c:1"))]);
        let all = plan_items(
            &tree,
            &ImportSelection::Collections(vec!["c:1".to_owned()]),
            OutlineImportOptions {
                include_archived: true,
                include_templates: true,
            },
        )
        .unwrap();
        assert_eq!(all.len(), 5);
        assert_eq!(all[2].parent_key.as_deref(), Some("d:a"));
        assert!(plan_items(
            &tree,
            &ImportSelection::Collections(vec!["c:nope".to_owned()]),
            OutlineImportOptions::default()
        )
        .is_err());
    }
}
