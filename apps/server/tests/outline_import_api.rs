//! Outline import API and jobs: the JSON and Markdown exports (fixture zips in
//! `tests/fixtures/outline`, written by `make_fixtures.py`) and Outline's API (a mock server),
//! imported into Docs: tree, order, attachments, link rewriting, authors, re-imports.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use orbit_platform::{
    AttachmentMutationCoordinator, AuthenticatedUser, Id, LocalBlobStore, ObjectStorage,
    PasswordService, TestDatabase, TimestampMillis, UploadLimits, UploadService,
};
use orbit_server::auth_routes::CookieMode;
use orbit_server::outline::client::RetryPolicy;
use orbit_server::outline::import::{JobRun, OutlineImportService, OutlineImportSettings};
use orbit_server::outline_import_routes::{OutlineImportState, outline_import_router};
use orbit_server::page_routes::{PageState, page_router};
use orbit_server::repositories::identity::{IdentityRepository, SetupRequest};
use orbit_server::repositories::page_files::PageFileRepository;
use serde_json::{Value, json};
use tower::ServiceExt;

const BOUNDARY: &str = "orbit-test-boundary";
const JSON_EXPORT: &[u8] = include_bytes!("fixtures/outline/json-export.zip");
const MARKDOWN_EXPORT: &[u8] = include_bytes!("fixtures/outline/markdown-export.zip");
const PNG: &[u8] = b"\x89\x50\x4e\x47\x0d\x0a\x1a\x0a\x00\x00\x00\x0d\x49\x48\x44\x52\x00\x00\x00\x01\x00\x00\x00\x01\x08\x04\x00\x00\x00\xb5\x1c\x0c\x02\x00\x00\x00\x0b\x49\x44\x41\x54\x78\xda\x63\x64\xf8\x0f\x00\x01\x05\x01\x01\x27\x18\xe3\x66\x00\x00\x00\x00\x49\x45\x4e\x44\xae\x42\x60\x82";

fn multipart(name: &str, bytes: &[u8]) -> Vec<u8> {
    let mut body = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n\
         Content-Type: application/zip\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    body
}

struct Harness {
    _root: tempfile::TempDir,
    database: TestDatabase,
    identity: Arc<IdentityRepository>,
    service: OutlineImportService,
    app: Router,
    workspace_id: String,
    owner_id: String,
    owner_cookie: String,
}

impl Harness {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let database = TestDatabase::new().await.unwrap();
        let identity = Arc::new(IdentityRepository::new((*database).clone()));
        let now = TimestampMillis::now();
        identity
            .store_setup_token(
                "operator-secret",
                TimestampMillis::from_millis(now.as_millis() + 60_000),
            )
            .await
            .unwrap();
        let setup = identity
            .complete_setup(
                SetupRequest {
                    token: "operator-secret".to_owned(),
                    email: "owner@example.com".to_owned(),
                    display_name: "Owner".to_owned(),
                    password_hash: PasswordService::default()
                        .hash("correct horse battery")
                        .unwrap(),
                    workspace_name: "Orbit".to_owned(),
                    project_name: "General".to_owned(),
                },
                now,
            )
            .await
            .unwrap();
        let uploads = UploadService::new(
            (*database).clone(),
            Arc::new(LocalBlobStore::new(root.path().join("attachments"))),
            AttachmentMutationCoordinator::default(),
            UploadLimits::new(1024 * 1024, 4 * 1024 * 1024).unwrap(),
        );
        let service = OutlineImportService::new(
            (*database).clone(),
            PageFileRepository::new((*database).clone(), uploads),
            ObjectStorage::default(),
            root.path().join("backups"),
            OutlineImportSettings {
                max_archive_bytes: 1024 * 1024,
                max_file_bytes: 1024 * 1024,
                app_key: Some([7; 32]),
                allow_local: true,
                retry: RetryPolicy {
                    max_attempts: 4,
                    base_delay: Duration::from_millis(5),
                    max_delay: Duration::from_millis(50),
                },
            },
        );
        let app = outline_import_router(OutlineImportState::new(
            Arc::clone(&identity),
            service.clone(),
            CookieMode::secure(),
        ))
        .merge(page_router(PageState::new(
            Arc::clone(&identity),
            CookieMode::secure(),
        )));
        let owner_id: String = sqlx::query_scalar("SELECT id FROM users WHERE normalized_email = 'owner@example.com'")
            .fetch_one(database.pool())
            .await
            .unwrap();
        Self {
            _root: root,
            database,
            identity,
            service,
            app,
            workspace_id: setup.workspace_id.to_string(),
            owner_id,
            owner_cookie: format!("__Host-orbit_session={}", setup.session.token),
        }
    }

    fn imports_uri(&self) -> String {
        format!("/api/v1/workspaces/{}/imports/outline", self.workspace_id)
    }

    async fn send(&self, request: Request<Body>) -> (StatusCode, Value) {
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn call(&self, cookie: &str, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::COOKIE, cookie);
        let request = match body {
            Some(value) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(value.to_string())),
            None => builder
                .header(header::CONTENT_LENGTH, 0)
                .body(Body::empty()),
        }
        .unwrap();
        self.send(request).await
    }

    async fn upload(&self, cookie: &str, bytes: &[u8]) -> (StatusCode, Value) {
        let request = Request::builder()
            .method("POST")
            .uri(format!("{}/upload", self.imports_uri()))
            .header(header::COOKIE, cookie)
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={BOUNDARY}"),
            )
            .body(Body::from(multipart("outline-export.zip", bytes)))
            .unwrap();
        self.send(request).await
    }

    async fn scan(&self, id: &str) -> Value {
        self.service
            .run_scan(id.parse().unwrap(), &JobRun::default())
            .await
            .unwrap();
        self.detail(id, true).await
    }

    async fn detail(&self, id: &str, preview: bool) -> Value {
        let query = if preview { "?include=preview" } else { "" };
        let (status, import) = self
            .call(&self.owner_cookie, "GET", &format!("{}/{id}{query}", self.imports_uri()), None)
            .await;
        assert_eq!(status, StatusCode::OK, "{import}");
        import
    }

    async fn start(&self, id: &str, body: Value) -> Value {
        let (status, import) = self
            .call(&self.owner_cookie, "POST", &format!("{}/{id}/start", self.imports_uri()), Some(body))
            .await;
        assert_eq!(status, StatusCode::OK, "{import}");
        assert_eq!(import["status"], "queued");
        import
    }

    async fn run_import(&self, id: &str) -> Value {
        let set_state = |from: &'static str, to: &'static str| {
            sqlx::query(
                "UPDATE jobs SET state = $1 WHERE kind = 'outline.import' AND state = $2 \
                 AND payload_json LIKE $3",
            )
            .bind(to)
            .bind(from)
            .bind(format!("%{id}%"))
            .execute(self.database.pool())
        };
        set_state("queued", "running").await.unwrap();
        self.service
            .run_import(id.parse().unwrap(), &JobRun::default())
            .await
            .unwrap();
        set_state("running", "succeeded").await.unwrap();
        self.detail(id, false).await
    }

    /// Uploads an export, scans and imports all of it into the private space.
    async fn import_export(&self, bytes: &[u8]) -> (String, Value) {
        let (status, import) = self.upload(&self.owner_cookie, bytes).await;
        assert_eq!(status, StatusCode::CREATED, "{import}");
        assert_eq!(import["status"], "scanning");
        let id = import["id"].as_str().unwrap().to_owned();
        let ready = self.scan(&id).await;
        assert_eq!(ready["status"], "ready", "{ready}");
        self.start(&id, json!({"selection": {"all": true}, "destination": {"private": true}}))
            .await;
        let done = self.run_import(&id).await;
        (id, done)
    }

    async fn page_id(&self, import_id: &str, key: &str) -> String {
        sqlx::query_scalar("SELECT page_id FROM outline_import_items WHERE import_id = $1 AND item_key = $2")
            .bind(import_id)
            .bind(key)
            .fetch_one(self.database.pool())
            .await
            .unwrap()
    }

    async fn page(&self, page_id: &str) -> Value {
        let (status, page) = self
            .call(
                &self.owner_cookie,
                "GET",
                &format!("/api/v1/workspaces/{}/pages/{page_id}", self.workspace_id),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{page}");
        page
    }

    /// `(parent_id, position, creator_id, created_at, updated_at)` of a page.
    async fn page_row(&self, page_id: &str) -> (Option<String>, i64, String, i64, i64) {
        sqlx::query_as("SELECT parent_id, position, creator_id, created_at, updated_at FROM pages WHERE id = $1")
            .bind(page_id)
            .fetch_one(self.database.pool())
            .await
            .unwrap()
    }

    async fn add_member(&self, email: &str, role: &str) -> (String, String) {
        let id = Id::new_v7();
        let now = TimestampMillis::now();
        sqlx::query(
            "INSERT INTO users (id, email, normalized_email, display_name, password_hash, created_at, updated_at) \
             VALUES ($1, $2, $3, 'Member', 'unused', $4, $5)",
        )
        .bind(id.to_string())
        .bind(email)
        .bind(email)
        .bind(now.as_millis())
        .bind(now.as_millis())
        .execute(self.database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO memberships (id, workspace_id, user_id, role, version, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, 0, $5, $6)",
        )
        .bind(Id::new_v7().to_string())
        .bind(&self.workspace_id)
        .bind(id.to_string())
        .bind(role)
        .bind(now.as_millis())
        .bind(now.as_millis())
        .execute(self.database.pool())
        .await
        .unwrap();
        let session = self
            .identity
            .create_session(
                &AuthenticatedUser {
                    id,
                    email: email.to_owned(),
                    display_name: "Member".to_owned(),
                },
                now,
            )
            .await
            .unwrap();
        (id.to_string(), format!("__Host-orbit_session={}", session.token))
    }

    async fn page_count(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM pages WHERE workspace_id = $1 AND deleted_at IS NULL")
            .bind(&self.workspace_id)
            .fetch_one(self.database.pool())
            .await
            .unwrap()
    }
}

/// Every value under `key` anywhere in `value`.
fn find_all<'a>(value: &'a Value, key: &str, out: &mut Vec<&'a Value>) {
    match value {
        Value::Object(map) => {
            for (name, child) in map {
                if name == key {
                    out.push(child);
                }
                find_all(child, key, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| find_all(item, key, out)),
        _ => {}
    }
}

fn hrefs(page: &Value) -> Vec<String> {
    let mut found = Vec::new();
    find_all(&page["content"], "href", &mut found);
    found.iter().filter_map(|value| value.as_str().map(str::to_owned)).collect()
}

fn blocks_of_type<'a>(page: &'a Value, kind: &str) -> Vec<&'a Value> {
    let mut found = Vec::new();
    fn walk<'a>(blocks: &'a Value, kind: &str, out: &mut Vec<&'a Value>) {
        for block in blocks.as_array().into_iter().flatten() {
            if block["type"] == kind {
                out.push(block);
            }
            walk(&block["children"], kind, out);
        }
    }
    walk(&page["content"], kind, &mut found);
    found
}

const ARCH: &str = "d:d0000000-0000-4000-8000-0000000000a1";
const DB: &str = "d:d0000000-0000-4000-8000-0000000000a2";
const ONB: &str = "d:d0000000-0000-4000-8000-0000000000b1";
const ROAD: &str = "d:d0000000-0000-4000-8000-0000000000c1";
const ENG: &str = "c:c0000000-0000-4000-8000-000000000001";
const PRODUCT: &str = "c:c0000000-0000-4000-8000-000000000002";

#[tokio::test]
async fn imports_the_json_export_with_tree_order_files_links_and_authors() {
    let harness = Harness::new().await;
    let (member_id, _) = harness.add_member("member@example.com", "member").await;
    let (status, import) = harness.upload(&harness.owner_cookie, JSON_EXPORT).await;
    assert_eq!(status, StatusCode::CREATED, "{import}");
    assert_eq!(import["source"], "archive");
    let id = import["id"].as_str().unwrap().to_owned();
    let ready = harness.scan(&id).await;
    assert_eq!(ready["status"], "ready", "{ready}");
    assert_eq!(ready["format"], "json");
    let collections = ready["preview"]["collections"].as_array().unwrap();
    assert_eq!(collections.len(), 2);
    assert_eq!(collections[0]["title"], "Engineering");
    assert_eq!(collections[0]["icon"], "🛠️");
    assert_eq!(collections[0]["documents"], 3);
    assert_eq!(collections[0]["attachments"], 2);
    assert_eq!(collections[1]["title"], "Product");
    assert_eq!(collections[1]["icon"], Value::Null, "a named icon is not an emoji");
    assert_eq!(collections[1]["documents"], 1);

    let started = harness
        .start(&id, json!({"selection": {"all": true}, "destination": {"private": true}}))
        .await;
    assert_eq!(started["progress"]["total"], 6);
    assert_eq!(started["progress"]["attachments_total"], 2);
    let done = harness.run_import(&id).await;
    assert_eq!(done["status"], "completed", "{done}");
    assert_eq!(done["progress"]["done"], 6);
    assert_eq!(done["progress"]["failed"], 0);
    assert_eq!(done["progress"]["attachments_done"], 2);
    let report = &done["report"];
    assert_eq!(report["pages_created"], 6, "{report}");
    assert_eq!(report["files_imported"], 2, "{report}");
    assert_eq!(report["files_failed"], 0, "{report}");
    assert_eq!(done["root_page_ids"].as_array().unwrap().len(), 2);

    // Tree and sibling order.
    let eng = harness.page_id(&id, ENG).await;
    let arch = harness.page_id(&id, ARCH).await;
    let db = harness.page_id(&id, DB).await;
    let onb = harness.page_id(&id, ONB).await;
    let product = harness.page_id(&id, PRODUCT).await;
    let road = harness.page_id(&id, ROAD).await;
    let (eng_parent, _, _, _, _) = harness.page_row(&eng).await;
    assert_eq!(eng_parent, None);
    let (arch_parent, arch_position, arch_creator, arch_created, arch_updated) = harness.page_row(&arch).await;
    let (db_parent, _, db_creator, _, _) = harness.page_row(&db).await;
    let (onb_parent, onb_position, onb_creator, _, _) = harness.page_row(&onb).await;
    let (road_parent, _, _, _, _) = harness.page_row(&road).await;
    assert_eq!(arch_parent.as_deref(), Some(eng.as_str()));
    assert_eq!(db_parent.as_deref(), Some(arch.as_str()));
    assert_eq!(onb_parent.as_deref(), Some(eng.as_str()));
    assert_eq!(road_parent.as_deref(), Some(product.as_str()));
    assert!(arch_position < onb_position, "Outline's sidebar order is kept");

    // Authors (matched by member email, else the importer) and Outline's dates.
    assert_eq!(arch_creator, harness.owner_id);
    assert_eq!(db_creator, member_id);
    assert_eq!(onb_creator, harness.owner_id, "ghost@example.com is no member");
    assert_eq!(arch_created, 1_704_164_645_000);
    assert_eq!(arch_updated, 1_706_933_106_000);

    // Content: icon, image as a page file, internal links to the new pages.
    let arch_page = harness.page(&arch).await;
    assert_eq!(arch_page["icon"], "🏛️");
    assert!(hrefs(&arch_page).contains(&format!("/docs/{onb}")), "{arch_page}");
    let images = blocks_of_type(&arch_page, "image");
    assert_eq!(images.len(), 1, "{arch_page}");
    assert!(images[0]["props"]["url"].as_str().unwrap().contains(&arch), "{}", images[0]);
    let db_page = harness.page(&db).await;
    assert_eq!(blocks_of_type(&db_page, "codeBlock")[0]["props"]["language"], "sql");
    assert_eq!(blocks_of_type(&db_page, "callout")[0]["props"]["emoji"], "⚠️");
    assert_eq!(blocks_of_type(&db_page, "file").len(), 1, "{db_page}");
    assert!(hrefs(&db_page).contains(&format!("/docs/{arch}")));
    let mut mentions = Vec::new();
    find_all(&db_page["content"], "userId", &mut mentions);
    assert_eq!(mentions, vec![&json!(member_id)]);
    let onb_page = harness.page(&onb).await;
    assert_eq!(blocks_of_type(&onb_page, "checkListItem").len(), 2);
    assert_eq!(blocks_of_type(&onb_page, "table").len(), 1);
    let onb_links = hrefs(&onb_page);
    assert!(onb_links.contains(&format!("/docs/{db}")), "{onb_links:?}");
    assert!(onb_links.contains(&format!("/docs/{arch}")), "a document mention links: {onb_links:?}");
    let road_page = harness.page(&road).await;
    let road_links = hrefs(&road_page);
    assert!(road_links.contains(&format!("/docs/{arch}")), "absolute URL rewritten: {road_links:?}");
    assert!(road_links.contains(&"/doc/missing-MissDoc0001".to_owned()));
    // Collections: the description, or a list of the collection's pages.
    let eng_page = harness.page(&eng).await;
    assert_eq!(eng_page["icon"], "🛠️");
    assert!(eng_page["content"].to_string().contains("Everything engineering."));
    let product_page = harness.page(&product).await;
    assert_eq!(blocks_of_type(&product_page, "page")[0]["props"]["pageId"], json!(road));

    // Warnings: the link to a document that is not in the export.
    let (status, warnings) = harness
        .call(&harness.owner_cookie, "GET", &format!("{}/{id}/warnings", harness.imports_uri()), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let warnings = warnings["items"].as_array().unwrap();
    assert!(warnings.iter().any(|warning| warning["kind"] == "unresolved_link"
        && warning["detail"] == "/doc/missing-MissDoc0001"
        && warning["title"] == "Roadmap"), "{warnings:?}");
    assert_eq!(report["warnings"], warnings.len());

    // The archive is gone; each page remembers where it came from.
    let location: Option<String> = sqlx::query_scalar("SELECT archive_location FROM outline_imports WHERE id = $1")
        .bind(&id)
        .fetch_one(harness.database.pool())
        .await
        .unwrap();
    assert_eq!(location, None);
    let refs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM page_external_refs WHERE external_source = 'outline'")
        .fetch_one(harness.database.pool())
        .await
        .unwrap();
    assert_eq!(refs, 6);

    // Importing the same export again creates nothing new.
    let pages_before = harness.page_count().await;
    let (again_id, again) = harness.import_export(JSON_EXPORT).await;
    assert_eq!(again["status"], "completed", "{again}");
    assert_eq!(again["report"]["pages_created"], 0, "{again}");
    assert_eq!(again["report"]["pages_unchanged"], 6, "{again}");
    assert_eq!(again["progress"]["skipped"], 6);
    assert_eq!(harness.page_count().await, pages_before);
    assert_eq!(harness.page_id(&again_id, ARCH).await, arch);
}

#[tokio::test]
async fn imports_the_markdown_export_with_relative_links_and_files() {
    let harness = Harness::new().await;
    let (id, done) = harness.import_export(MARKDOWN_EXPORT).await;
    assert_eq!(done["status"], "completed", "{done}");
    assert_eq!(done["format"], "markdown");
    assert_eq!(done["progress"]["done"], 6, "{done}");
    assert_eq!(done["report"]["files_imported"], 2, "{done}");
    let eng = harness.page_id(&id, "c:Engineering").await;
    let arch = harness.page_id(&id, "d:Engineering/Architecture.md").await;
    let db = harness.page_id(&id, "d:Engineering/Architecture/Database.md").await;
    let onb = harness.page_id(&id, "d:Engineering/Onboarding.md").await;
    let road = harness.page_id(&id, "d:Product/Roadmap.md").await;
    let (arch_parent, arch_position, _, arch_created, _) = harness.page_row(&arch).await;
    let (db_parent, _, _, _, _) = harness.page_row(&db).await;
    let (onb_parent, onb_position, _, _, _) = harness.page_row(&onb).await;
    assert_eq!(arch_parent.as_deref(), Some(eng.as_str()));
    assert_eq!(db_parent.as_deref(), Some(arch.as_str()));
    assert_eq!(onb_parent.as_deref(), Some(eng.as_str()));
    assert!(arch_position < onb_position);
    assert_eq!(arch_created, 1_704_164_645_000, "dates from the entry comment");

    let arch_page = harness.page(&arch).await;
    assert_eq!(arch_page["title"], "Architecture");
    assert!(!arch_page["content"].to_string().contains("\"Architecture\""), "the title line is dropped");
    assert!(hrefs(&arch_page).contains(&format!("/docs/{onb}")), "{arch_page}");
    assert_eq!(blocks_of_type(&arch_page, "image").len(), 1, "{arch_page}");
    let db_page = harness.page(&db).await;
    assert_eq!(blocks_of_type(&db_page, "callout").len(), 1, "{db_page}");
    assert_eq!(blocks_of_type(&db_page, "file").len(), 1, "{db_page}");
    assert!(hrefs(&db_page).contains(&format!("/docs/{arch}")), "{db_page}");
    assert!(db_page["content"].to_string().contains("@Member"));
    let onb_page = harness.page(&onb).await;
    assert!(hrefs(&onb_page).contains(&format!("/docs/{db}")));
    assert_eq!(blocks_of_type(&onb_page, "checkListItem").len(), 2);
    let road_page = harness.page(&road).await;
    assert!(hrefs(&road_page).contains(&format!("/docs/{arch}")), "{road_page}");

    // Again: same pages, nothing new.
    let pages_before = harness.page_count().await;
    let (_, again) = harness.import_export(MARKDOWN_EXPORT).await;
    assert_eq!(again["report"]["pages_created"], 0, "{again}");
    assert_eq!(harness.page_count().await, pages_before);
}

#[tokio::test]
async fn only_admins_may_import_and_bad_uploads_are_refused() {
    let harness = Harness::new().await;
    let (_, member) = harness.add_member("member@example.com", "member").await;
    let (status, problem) = harness.upload(&member, JSON_EXPORT).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{problem}");
    assert_eq!(problem["code"], "forbidden");
    let (status, _) = harness.call(&member, "GET", &harness.imports_uri(), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, problem) = harness.upload(&harness.owner_cookie, b"not a zip").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert_eq!(problem["code"], "outline_archive_invalid");

    // A zip that is not an Outline export fails its scan with a sentence.
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    writer
        .start_file("photo.png", zip::write::SimpleFileOptions::default())
        .unwrap();
    std::io::Write::write_all(&mut writer, PNG).unwrap();
    let other = writer.finish().unwrap().into_inner();
    let (status, import) = harness.upload(&harness.owner_cookie, &other).await;
    assert_eq!(status, StatusCode::CREATED);
    let failed = harness.scan(import["id"].as_str().unwrap()).await;
    assert_eq!(failed["status"], "failed");
    assert!(failed["error"].as_str().unwrap().contains("not an Outline export"), "{failed}");
}

/// A recorded request to the mock Outline server.
#[derive(Clone, Debug)]
struct Logged {
    path: String,
    body: Value,
    authorization: Option<String>,
    api_version: Option<String>,
}

#[derive(Clone)]
struct Mock {
    log: Arc<Mutex<Vec<Logged>>>,
    rate_limited_once: Arc<AtomicBool>,
    port: Arc<Mutex<u16>>,
}

const C1: &str = "c1000000-0000-4000-8000-000000000001";
const D1: &str = "d1000000-0000-4000-8000-000000000001";
const D2: &str = "d1000000-0000-4000-8000-000000000002";
const D3: &str = "d1000000-0000-4000-8000-000000000003";
const D4: &str = "d1000000-0000-4000-8000-000000000004";
const T1: &str = "e1000000-0000-4000-8000-000000000001";
const ATT: &str = "a1000000-0000-4000-8000-000000000001";

fn api_doc(id: &str, url_id: &str, title: &str, parent: Option<&str>, data: Value) -> Value {
    json!({
        "id": id, "urlId": url_id, "title": title, "data": data, "icon": null,
        "collectionId": C1, "parentDocumentId": parent,
        "createdBy": {"id": "user-0", "name": "Owner"},
        "createdAt": "2024-05-01T00:00:00.000Z", "updatedAt": "2024-05-02T00:00:00.000Z",
        "publishedAt": "2024-05-01T00:00:00.000Z", "archivedAt": null, "deletedAt": null
    })
}

fn page(items: Vec<Value>, body: &Value) -> Value {
    let offset = body["offset"].as_u64().unwrap_or(0) as usize;
    let limit = body["limit"].as_u64().unwrap_or(25) as usize;
    let slice: Vec<Value> = items.into_iter().skip(offset).take(limit).collect();
    json!({"data": slice, "pagination": {"offset": offset, "limit": limit, "nextPath": "/api/next"}})
}

async fn outline(State(mock): State<Mock>, headers: HeaderMap, request: axum::extract::Request) -> Response {
    let path = request.uri().path().to_owned();
    let query = request.uri().query().unwrap_or_default().to_owned();
    let bytes = to_bytes(request.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let header = |name: &str| headers.get(name).and_then(|value| value.to_str().ok()).map(str::to_owned);
    mock.log.lock().unwrap().push(Logged {
        path: path.clone(),
        body: body.clone(),
        authorization: header("authorization"),
        api_version: header("x-api-version"),
    });
    if path.starts_with("/api/") && header("authorization").as_deref() != Some("Bearer ol_api_good") {
        return (StatusCode::UNAUTHORIZED, axum::Json(json!({"ok": false, "error": "authentication_required"}))).into_response();
    }
    let ok = |value: Value| axum::Json(value).into_response();
    let child_link = json!({"type": "text", "text": "child", "marks": [{"type": "link", "attrs": {"href": "/doc/child-ChildDoc01"}}]});
    match path.as_str() {
        "/api/auth.info" => ok(json!({"data": {"user": {"id": "user-0", "name": "Owner"}, "team": {"name": "Acme Wiki"}}})),
        "/api/users.list" => {
            let mut users: Vec<Value> = (1..150)
                .map(|index| json!({"id": format!("user-{index}"), "name": "x", "email": format!("person{index}@example.com")}))
                .collect();
            users.insert(0, json!({"id": "user-0", "name": "Owner", "email": "owner@example.com"}));
            ok(page(users, &body))
        }
        "/api/collections.list" => ok(page(
            vec![json!({"id": C1, "urlId": "WikiColl01", "name": "Wiki", "icon": "📘", "index": "a",
                        "data": {"type": "doc", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "About the wiki"}]}]}})],
            &body,
        )),
        "/api/collections.documents" => ok(json!({"data": [
            {"id": D1, "title": "Home", "url": "/doc/home-HomeDoc001", "children": [
                {"id": D2, "title": "Child", "url": "/doc/child-ChildDoc01", "children": []}
            ]}
        ]})),
        "/api/documents.list" => {
            if !mock.rate_limited_once.swap(true, Ordering::SeqCst) {
                return (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "0")], "slow down").into_response();
            }
            let status = body["statusFilter"][0].as_str().unwrap_or("published");
            let items = if status == "archived" {
                let mut archived = api_doc(D3, "OldDoc0001", "Old notes", Some(D1), Value::Null);
                archived["archivedAt"] = json!("2024-06-01T00:00:00.000Z");
                vec![archived]
            } else {
                vec![
                    api_doc(D2, "ChildDoc01", "Child", Some(D1), Value::Null),
                    api_doc(D1, "HomeDoc001", "Home", None, json!({"type": "doc", "content": [
                        {"type": "paragraph", "content": [{"type": "image", "attrs": {"src": format!("/api/attachments.redirect?id={ATT}")}}]}
                    ]})),
                ]
            };
            ok(page(items, &body))
        }
        "/api/documents.deleted" => {
            let mut deleted = api_doc(D4, "TrashDoc01", "Trashed", None, Value::Null);
            deleted["deletedAt"] = json!("2024-07-01T00:00:00.000Z");
            ok(page(vec![deleted], &body))
        }
        "/api/templates.list" => {
            let mut template = api_doc(T1, "TmplDoc001", "Meeting notes", None, Value::Null);
            template["collectionId"] = json!(C1);
            ok(page(vec![template], &body))
        }
        "/api/documents.info" | "/api/templates.info" => {
            let id = body["id"].as_str().unwrap_or_default();
            let data = if id == D1 {
                json!({"type": "doc", "content": [
                    {"type": "paragraph", "content": [{"type": "text", "text": "Welcome, see "}, child_link]},
                    {"type": "paragraph", "content": [{"type": "image", "attrs": {"src": format!("/api/attachments.redirect?id={ATT}"), "alt": "Logo"}}]}
                ]})
            } else {
                json!({"type": "doc", "content": [{"type": "paragraph", "content": [{"type": "text", "text": format!("Body of {id}")}]}]})
            };
            ok(json!({"data": api_doc(id, "Whatever01", "x", None, data)}))
        }
        "/api/attachments.redirect" => {
            assert!(query.contains(ATT), "{query}");
            let port = *mock.port.lock().unwrap();
            // Another origin (localhost, not 127.0.0.1): the token must not follow.
            (StatusCode::FOUND, [(header::LOCATION, format!("http://localhost:{port}/storage/logo.png"))]).into_response()
        }
        "/storage/logo.png" => ([(header::CONTENT_TYPE, "image/png")], PNG).into_response(),
        _ => (StatusCode::NOT_FOUND, axum::Json(json!({"ok": false, "error": "not_found"}))).into_response(),
    }
}

async fn mock_outline() -> (Mock, String) {
    let mock = Mock {
        log: Arc::new(Mutex::new(Vec::new())),
        rate_limited_once: Arc::new(AtomicBool::new(false)),
        port: Arc::new(Mutex::new(0)),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    *mock.port.lock().unwrap() = port;
    let app = Router::new().fallback(outline).with_state(mock.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (mock, format!("http://127.0.0.1:{port}"))
}

#[tokio::test]
async fn imports_through_the_api_with_pagination_rate_limits_and_attachments() {
    let harness = Harness::new().await;
    let (mock, base_url) = mock_outline().await;

    // A wrong token is refused at once.
    let (status, problem) = harness
        .call(&harness.owner_cookie, "POST", &harness.imports_uri(), Some(json!({"base_url": base_url, "token": "ol_api_bad"})))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert_eq!(problem["code"], "outline_token_invalid");
    let (status, problem) = harness
        .call(&harness.owner_cookie, "POST", &harness.imports_uri(), Some(json!({"base_url": "ftp://x", "token": "ol_api_good"})))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(problem["code"], "outline_url_invalid");

    let (status, import) = harness
        .call(&harness.owner_cookie, "POST", &harness.imports_uri(), Some(json!({"base_url": base_url, "token": "ol_api_good"})))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{import}");
    assert_eq!(import["source"], "api");
    assert_eq!(import["file_name"], "Acme Wiki");
    assert!(!import.to_string().contains("ol_api_good"), "the token never comes back");
    let id = import["id"].as_str().unwrap().to_owned();
    let ready = harness.scan(&id).await;
    assert_eq!(ready["status"], "ready", "{ready}");
    assert_eq!(ready["format"], "api");
    let preview = &ready["preview"]["collections"][0];
    assert_eq!(preview["title"], "Wiki");
    assert_eq!(preview["documents"], 2);
    assert_eq!(preview["archived"], 2, "one archived, one in the trash");
    assert_eq!(preview["templates"], 1);
    assert_eq!(preview["attachments"], 1);

    // Pagination: users.list read with offset/limit until a short page.
    let offsets: Vec<u64> = mock
        .log
        .lock()
        .unwrap()
        .iter()
        .filter(|logged| logged.path == "/api/users.list")
        .map(|logged| logged.body["offset"].as_u64().unwrap())
        .collect();
    assert_eq!(offsets, [0, 100]);
    // The 429 was retried.
    let list_calls = mock.log.lock().unwrap().iter().filter(|logged| logged.path == "/api/documents.list").count();
    assert_eq!(list_calls, 3, "one rate-limited call, then published and archived");

    harness
        .start(&id, json!({
            "selection": {"collections": [format!("c:{C1}")]},
            "options": {"include_archived": true, "include_templates": true},
            "destination": {"private": true}
        }))
        .await;
    let done = harness.run_import(&id).await;
    assert_eq!(done["status"], "completed", "{done}");
    assert_eq!(done["progress"]["done"], 7, "{done}");
    assert_eq!(done["report"]["files_imported"], 1, "{done}");
    assert_eq!(done["options"]["include_archived"], true);

    let home = harness.page_id(&id, &format!("d:{D1}")).await;
    let child = harness.page_id(&id, &format!("d:{D2}")).await;
    let old = harness.page_id(&id, &format!("d:{D3}")).await;
    let templates = harness.page_id(&id, &format!("t:{C1}")).await;
    let template = harness.page_id(&id, &format!("tpl:{T1}")).await;
    let collection = harness.page_id(&id, &format!("c:{C1}")).await;
    assert_eq!(harness.page_row(&child).await.0.as_deref(), Some(home.as_str()));
    assert_eq!(harness.page_row(&old).await.0.as_deref(), Some(home.as_str()), "archived under its parent");
    assert_eq!(harness.page_row(&templates).await.0.as_deref(), Some(collection.as_str()));
    assert_eq!(harness.page_row(&template).await.0.as_deref(), Some(templates.as_str()));
    let home_page = harness.page(&home).await;
    assert!(hrefs(&home_page).contains(&format!("/docs/{child}")), "{home_page}");
    assert_eq!(blocks_of_type(&home_page, "image").len(), 1, "{home_page}");
    let collection_page = harness.page(&collection).await;
    assert_eq!(collection_page["icon"], "📘");
    assert!(collection_page["content"].to_string().contains("About the wiki"));

    // Every API call carried the token and asked for ProseMirror; the file storage got no token.
    let log = mock.log.lock().unwrap().clone();
    for logged in log.iter().filter(|logged| logged.path.starts_with("/api/") && logged.authorization.as_deref() == Some("Bearer ol_api_good")) {
        assert_eq!(logged.api_version.as_deref(), Some("3"), "{logged:?}");
    }
    let storage: Vec<&Logged> = log.iter().filter(|logged| logged.path == "/storage/logo.png").collect();
    assert_eq!(storage.len(), 1);
    assert_eq!(storage[0].authorization, None);
    assert!(log.iter().any(|logged| logged.path == "/api/documents.info" && logged.body["id"] == D1));

    // The token is wiped once the import ends.
    let token: Option<Vec<u8>> = sqlx::query_scalar("SELECT token_ciphertext FROM outline_imports WHERE id = $1")
        .bind(&id)
        .fetch_one(harness.database.pool())
        .await
        .unwrap();
    assert_eq!(token, None);
}
