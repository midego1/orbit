//! Outline import API under `/api/v1/workspaces/{workspace_id}/imports/outline`. Only workspace
//! admins may use it; an import is visible only to the admin who created it.

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Extension, FromRequest, Multipart, Path, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use orbit_platform::{Id, RequestId};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use utoipa::{IntoParams, ToSchema};

use crate::attachment_routes::{has_length_limit, multipart_too_large};
use crate::auth_routes::CookieMode;
use crate::outline::import::{
    ImportDestination, ImportSelection, OutlineImport, OutlineImportError, OutlineImportOptions,
    OutlineImportService, OutlineImportWarning, Upload,
};
use crate::repositories::identity::IdentityRepository;
use crate::task_routes::{ApiError, ApiJson, ApiQuery, authenticate_session, validation};

/// The upload route's path ends with this; it may go above the instance's upload limit (see
/// `HttpLimits::large_upload`).
pub const UPLOAD_PATH_SUFFIX: &str = "/imports/outline/upload";

#[derive(Clone)]
pub struct OutlineImportState {
    identity: Arc<IdentityRepository>,
    service: OutlineImportService,
    cookie_mode: CookieMode,
}

impl OutlineImportState {
    #[must_use]
    pub fn new(
        identity: Arc<IdentityRepository>,
        service: OutlineImportService,
        cookie_mode: CookieMode,
    ) -> Self {
        Self {
            identity,
            service,
            cookie_mode,
        }
    }
}

pub fn outline_import_router(state: OutlineImportState) -> Router {
    // Room for the multipart boundaries and headers around the archive.
    let body_limit = usize::try_from(state.service.max_archive_bytes())
        .unwrap_or(usize::MAX)
        .saturating_add(64 * 1024);
    Router::new()
        .route(
            "/api/v1/workspaces/{workspace_id}/imports/outline",
            get(list_outline_imports).post(connect_outline_import),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/imports/outline/upload",
            post(upload_outline_import).layer(DefaultBodyLimit::max(body_limit)),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/imports/outline/{import_id}",
            get(get_outline_import),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/imports/outline/{import_id}/warnings",
            get(list_outline_import_warnings),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/imports/outline/{import_id}/start",
            post(start_outline_import),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/imports/outline/{import_id}/cancel",
            post(cancel_outline_import),
        )
        .with_state(state)
}

/// The export as one `.zip` file in the `file` field.
#[derive(ToSchema)]
#[allow(dead_code)]
struct OutlineImportUploadBody {
    #[schema(value_type = String, format = Binary)]
    file: Vec<u8>,
}

/// Connects Outline's API. The token is sent once, kept encrypted while the import runs, and
/// deleted when it ends.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct ConnectOutlineImportBody {
    /// `https://app.getoutline.com` or a self-hosted Outline's URL.
    base_url: String,
    /// An Outline API key (Settings → API & Apps).
    token: String,
}

/// `{ "all": true }` for every collection, or `{ "collections": [...] }` (preview keys).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct OutlineImportSelectionBody {
    all: Option<bool>,
    collections: Option<Vec<String>>,
}

/// Where the collections go: under `parent_page_id`, or at the root of `teamspace_id`, of the
/// caller's private space (`private: true`), or of the default teamspace.
#[derive(Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct OutlineImportDestinationBody {
    teamspace_id: Option<String>,
    private: Option<bool>,
    parent_page_id: Option<String>,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct StartOutlineImportBody {
    selection: OutlineImportSelectionBody,
    #[serde(default)]
    options: OutlineImportOptions,
    #[serde(default)]
    destination: OutlineImportDestinationBody,
}

/// Optional parts of an import detail response.
#[derive(Clone, Copy, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
enum OutlineImportInclude {
    /// The scan's preview. Fetch it once when the import is `ready`; polling leaves it out.
    Preview,
}

#[derive(Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
struct OutlineImportDetailQuery {
    /// `preview` adds the scan's preview; without it `preview` is `null`.
    #[param(inline)]
    include: Option<OutlineImportInclude>,
}

#[derive(Serialize, ToSchema)]
struct OutlineImportList {
    items: Vec<OutlineImport>,
}

#[derive(Serialize, ToSchema)]
struct OutlineImportWarningList {
    items: Vec<OutlineImportWarning>,
}

fn collection(workspace: &str) -> String {
    format!("/api/v1/workspaces/{workspace}/imports/outline")
}

async fn scope(
    state: &OutlineImportState,
    headers: &HeaderMap,
    workspace: &str,
    instance: &str,
    request_id: Option<&Extension<RequestId>>,
) -> Result<(Id, Id), ApiError> {
    let session = authenticate_session(
        &state.identity,
        state.cookie_mode,
        headers,
        instance,
        request_id,
    )
    .await?;
    let workspace_id = workspace
        .parse()
        .map_err(|_| import_not_found(instance, request_id))?;
    Ok((workspace_id, session.user.id))
}

enum Received {
    TooLarge,
    Invalid,
    Storage(String),
}

/// Streams the `file` field to the upload's path; returns the file name and size.
async fn receive(
    request: Request,
    upload: &Upload,
    max_bytes: u64,
) -> Result<(String, u64), Received> {
    let mut multipart = Multipart::from_request(request, &())
        .await
        .map_err(|error| {
            if has_length_limit(&error)
                || error.into_response().status() == StatusCode::PAYLOAD_TOO_LARGE
            {
                Received::TooLarge
            } else {
                Received::Invalid
            }
        })?;
    let multipart_error = |error: axum::extract::multipart::MultipartError| {
        if multipart_too_large(&error) {
            Received::TooLarge
        } else {
            Received::Invalid
        }
    };
    let storage = |error: std::io::Error| Received::Storage(error.to_string());
    while let Some(mut field) = multipart.next_field().await.map_err(multipart_error)? {
        if field.name() != Some("file") {
            continue;
        }
        let name = field.file_name().unwrap_or("outline-export.zip").to_owned();
        let mut file = tokio::fs::File::create(&upload.path)
            .await
            .map_err(storage)?;
        let mut size = 0u64;
        while let Some(chunk) = field.chunk().await.map_err(multipart_error)? {
            size = size.saturating_add(chunk.len() as u64);
            if size > max_bytes {
                return Err(Received::TooLarge);
            }
            file.write_all(&chunk).await.map_err(storage)?;
        }
        file.flush().await.map_err(storage)?;
        return Ok((name, size));
    }
    Err(Received::Invalid)
}

/// Uploads an Outline export (multipart, one `.zip` in `file`), stores it and starts the scan.
#[utoipa::path(post, path = "/api/v1/workspaces/{workspace_id}/imports/outline/upload", params(("workspace_id" = String, Path)), request_body(content = OutlineImportUploadBody, content_type = "multipart/form-data"), responses((status = 201, body = OutlineImport)))]
async fn upload_outline_import(
    State(state): State<OutlineImportState>,
    Path(workspace): Path<String>,
    request: Request,
) -> Result<Response, ApiError> {
    let instance = format!("{}/upload", collection(&workspace));
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .cloned()
        .map(Extension);
    let headers = request.headers().clone();
    let id = request_id.as_ref();
    // Authorize before the body is read.
    let (workspace_id, user_id) = scope(&state, &headers, &workspace, &instance, id).await?;
    let upload = state
        .service
        .prepare(workspace_id, user_id)
        .await
        .map_err(|error| import_problem(error, &instance, id))?;
    let max_bytes = state.service.max_archive_bytes();
    let (name, size) = match receive(request, &upload, max_bytes).await {
        Ok(received) => received,
        Err(failure) => {
            let _ = tokio::fs::remove_file(&upload.path).await;
            let error = match failure {
                Received::TooLarge => OutlineImportError::ArchiveTooLarge(max_bytes),
                Received::Invalid => OutlineImportError::ArchiveInvalid,
                Received::Storage(error) => OutlineImportError::Unavailable(error),
            };
            return Err(import_problem(error, &instance, id));
        }
    };
    state
        .service
        .create_upload(workspace_id, user_id, upload, &name, size)
        .await
        .map(|import| (StatusCode::CREATED, Json(import)).into_response())
        .map_err(|error| import_problem(error, &instance, id))
}

/// Connects Outline's API with a base URL and an API token and starts the scan.
#[utoipa::path(post, path = "/api/v1/workspaces/{workspace_id}/imports/outline", params(("workspace_id" = String, Path)), request_body = ConnectOutlineImportBody, responses((status = 201, body = OutlineImport)))]
async fn connect_outline_import(
    State(state): State<OutlineImportState>,
    Path(workspace): Path<String>,
    headers: HeaderMap,
    request_id: Option<Extension<RequestId>>,
    ApiJson(body): ApiJson<ConnectOutlineImportBody>,
) -> Result<Response, ApiError> {
    let instance = collection(&workspace);
    let request = request_id.as_ref();
    let (workspace_id, user_id) = scope(&state, &headers, &workspace, &instance, request).await?;
    state
        .service
        .connect(workspace_id, user_id, &body.base_url, &body.token)
        .await
        .map(|import| (StatusCode::CREATED, Json(import)).into_response())
        .map_err(|error| import_problem(error, &instance, request))
}

/// The caller's 20 most recent Outline imports in this workspace (without previews).
#[utoipa::path(get, path = "/api/v1/workspaces/{workspace_id}/imports/outline", params(("workspace_id" = String, Path)), responses((status = 200, body = OutlineImportList)))]
async fn list_outline_imports(
    State(state): State<OutlineImportState>,
    Path(workspace): Path<String>,
    headers: HeaderMap,
    request_id: Option<Extension<RequestId>>,
) -> Result<Json<OutlineImportList>, ApiError> {
    let instance = collection(&workspace);
    let request = request_id.as_ref();
    let (workspace_id, user_id) = scope(&state, &headers, &workspace, &instance, request).await?;
    state
        .service
        .list(workspace_id, user_id)
        .await
        .map(|items| Json(OutlineImportList { items }))
        .map_err(|error| import_problem(error, &instance, request))
}

/// One of the caller's imports: status, progress and report; the preview with
/// `?include=preview`.
#[utoipa::path(get, path = "/api/v1/workspaces/{workspace_id}/imports/outline/{import_id}", params(("workspace_id" = String, Path), ("import_id" = String, Path), OutlineImportDetailQuery), responses((status = 200, body = OutlineImport)))]
async fn get_outline_import(
    State(state): State<OutlineImportState>,
    Path((workspace, import)): Path<(String, String)>,
    ApiQuery(query): ApiQuery<OutlineImportDetailQuery>,
    headers: HeaderMap,
    request_id: Option<Extension<RequestId>>,
) -> Result<Json<OutlineImport>, ApiError> {
    let instance = format!("{}/{import}", collection(&workspace));
    let request = request_id.as_ref();
    let (workspace_id, user_id) = scope(&state, &headers, &workspace, &instance, request).await?;
    let import_id = import
        .parse()
        .map_err(|_| import_not_found(&instance, request))?;
    state
        .service
        .get(
            workspace_id,
            user_id,
            import_id,
            query.include == Some(OutlineImportInclude::Preview),
        )
        .await
        .map(Json)
        .map_err(|error| import_problem(error, &instance, request))
}

/// The import's warnings (unsupported blocks, failed attachments, unresolved links and
/// mentions), the first 5,000.
#[utoipa::path(get, path = "/api/v1/workspaces/{workspace_id}/imports/outline/{import_id}/warnings", params(("workspace_id" = String, Path), ("import_id" = String, Path)), responses((status = 200, body = OutlineImportWarningList)))]
async fn list_outline_import_warnings(
    State(state): State<OutlineImportState>,
    Path((workspace, import)): Path<(String, String)>,
    headers: HeaderMap,
    request_id: Option<Extension<RequestId>>,
) -> Result<Json<OutlineImportWarningList>, ApiError> {
    let instance = format!("{}/{import}/warnings", collection(&workspace));
    let request = request_id.as_ref();
    let (workspace_id, user_id) = scope(&state, &headers, &workspace, &instance, request).await?;
    let import_id = import
        .parse()
        .map_err(|_| import_not_found(&instance, request))?;
    state
        .service
        .warnings(workspace_id, user_id, import_id)
        .await
        .map(|items| Json(OutlineImportWarningList { items }))
        .map_err(|error| import_problem(error, &instance, request))
}

/// Starts a scanned import with a selection, options and a destination.
#[utoipa::path(post, path = "/api/v1/workspaces/{workspace_id}/imports/outline/{import_id}/start", params(("workspace_id" = String, Path), ("import_id" = String, Path)), request_body = StartOutlineImportBody, responses((status = 200, body = OutlineImport)))]
async fn start_outline_import(
    State(state): State<OutlineImportState>,
    Path((workspace, import)): Path<(String, String)>,
    headers: HeaderMap,
    request_id: Option<Extension<RequestId>>,
    ApiJson(body): ApiJson<StartOutlineImportBody>,
) -> Result<Json<OutlineImport>, ApiError> {
    let instance = format!("{}/{import}/start", collection(&workspace));
    let request = request_id.as_ref();
    let (workspace_id, user_id) = scope(&state, &headers, &workspace, &instance, request).await?;
    let import_id = import
        .parse()
        .map_err(|_| import_not_found(&instance, request))?;
    let selection = match (body.selection.all, body.selection.collections) {
        (Some(true), None) => ImportSelection::All,
        (None | Some(false), Some(keys)) if !keys.is_empty() && keys.len() <= 1_000 => {
            ImportSelection::Collections(keys)
        }
        _ => return Err(validation("selection", &instance, request)),
    };
    let destination = ImportDestination {
        teamspace_id: body
            .destination
            .teamspace_id
            .map(|id| {
                id.parse().map_err(|_| {
                    import_problem(OutlineImportError::TeamspaceNotFound, &instance, request)
                })
            })
            .transpose()?,
        private: body.destination.private,
        parent_page_id: body
            .destination
            .parent_page_id
            .map(|id| {
                id.parse().map_err(|_| {
                    import_problem(OutlineImportError::PageNotFound, &instance, request)
                })
            })
            .transpose()?,
    };
    state
        .service
        .start(
            workspace_id,
            user_id,
            import_id,
            selection,
            body.options,
            destination,
        )
        .await
        .map(Json)
        .map_err(|error| import_problem(error, &instance, request))
}

/// Cancels a scan or import, deletes the uploaded export and the API token; pages already
/// imported stay.
#[utoipa::path(post, path = "/api/v1/workspaces/{workspace_id}/imports/outline/{import_id}/cancel", params(("workspace_id" = String, Path), ("import_id" = String, Path)), responses((status = 200, body = OutlineImport)))]
async fn cancel_outline_import(
    State(state): State<OutlineImportState>,
    Path((workspace, import)): Path<(String, String)>,
    headers: HeaderMap,
    request_id: Option<Extension<RequestId>>,
) -> Result<Json<OutlineImport>, ApiError> {
    let instance = format!("{}/{import}/cancel", collection(&workspace));
    let request = request_id.as_ref();
    let (workspace_id, user_id) = scope(&state, &headers, &workspace, &instance, request).await?;
    let import_id = import
        .parse()
        .map_err(|_| import_not_found(&instance, request))?;
    state
        .service
        .cancel(workspace_id, user_id, import_id)
        .await
        .map(Json)
        .map_err(|error| import_problem(error, &instance, request))
}

fn import_not_found(instance: &str, request_id: Option<&Extension<RequestId>>) -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "outline_import_not_found",
        "Import not found",
        "The requested import was not found.",
        instance,
        request_id,
    )
}

fn import_problem(
    error: OutlineImportError,
    instance: &str,
    request_id: Option<&Extension<RequestId>>,
) -> ApiError {
    let (status, code, title, detail) = match error {
        OutlineImportError::NotFound => return import_not_found(instance, request_id),
        OutlineImportError::Invalid { field } => return validation(field, instance, request_id),
        OutlineImportError::Forbidden => (
            StatusCode::FORBIDDEN,
            "forbidden",
            "Forbidden",
            "Only workspace admins can import from Outline.",
        ),
        OutlineImportError::InProgress => (
            StatusCode::CONFLICT,
            "import_in_progress",
            "Import in progress",
            "You already have an import running. Wait for it to finish or cancel it.",
        ),
        OutlineImportError::StateConflict => (
            StatusCode::CONFLICT,
            "outline_import_state_conflict",
            "Import state conflict",
            "The import is not in a state that allows this action.",
        ),
        OutlineImportError::TooLarge => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "outline_import_too_large",
            "Import too large",
            "An import can contain at most 10,000 pages.",
        ),
        OutlineImportError::ArchiveTooLarge(_) => (
            StatusCode::PAYLOAD_TOO_LARGE,
            "outline_archive_too_large",
            "Archive too large",
            "The export is larger than the server's upload limit.",
        ),
        OutlineImportError::ArchiveInvalid => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "outline_archive_invalid",
            "Not a zip archive",
            "Upload the Outline export as one .zip file.",
        ),
        OutlineImportError::UrlInvalid => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "outline_url_invalid",
            "Outline URL invalid",
            "Enter the https:// address of your Outline workspace.",
        ),
        OutlineImportError::TokenInvalid => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "outline_token_invalid",
            "Outline token invalid",
            "Outline did not accept the API token.",
        ),
        OutlineImportError::OutlineUnavailable(_) => (
            StatusCode::BAD_GATEWAY,
            "outline_unavailable",
            "Outline unavailable",
            "Outline could not be reached. Check the URL and try again.",
        ),
        OutlineImportError::AppKeyMissing => (
            StatusCode::SERVICE_UNAVAILABLE,
            "app_key_missing",
            "App key missing",
            "The server has no app key, so it cannot store the API token.",
        ),
        OutlineImportError::PageNotFound => (
            StatusCode::NOT_FOUND,
            "page_not_found",
            "Page not found",
            "The destination page was not found.",
        ),
        OutlineImportError::TeamspaceNotFound => (
            StatusCode::NOT_FOUND,
            "teamspace_not_found",
            "Teamspace not found",
            "The destination teamspace was not found.",
        ),
        OutlineImportError::Unavailable(error) => {
            tracing::error!(error = %error, "Outline import storage failed");
            return ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "Internal server error",
                "An unexpected error occurred. Use the request ID when contacting support.",
                instance,
                request_id,
            )
            .cause(error);
        }
    };
    ApiError::new(status, code, title, detail, instance, request_id)
}
