//! Ask AI: the in-app assistant. A real model (Anthropic, with the workspace's own API key) acts
//! as the signed-in member through the MCP tools, in-process, while the user watches.
//!
//! - `registry`: the tools, their risk and the page that shows their result.
//! - `model`, `anthropic`: the provider trait and the Anthropic Messages streaming client.
//! - `engine`: the run loop (approvals, limits, stopping), `prompt`: what the model is told.
//! - `store`: conversations and their history (per member, 30 days); `settings`: the sealed key, usage, rate limit.
//! - `agui`: the AG-UI 1.0 events the panel reads over SSE.
//!
//! Routes (session cookie; members of the workspace only):
//! `GET  /api/v1/assistant/workspaces/{id}/conversation` the current conversation and setup state,
//! `GET  /api/v1/assistant/workspaces/{id}/conversations` the member's history (newest first, `?before=` cursor),
//! `POST /api/v1/assistant/workspaces/{id}/conversations` a new conversation (the current one is archived),
//! `GET|PATCH|DELETE /api/v1/assistant/workspaces/{id}/conversations/{conversation}` open, rename, delete one,
//! `POST /api/v1/assistant/workspaces/{id}/conversations/{conversation}/continue` make it the active one again,
//! `POST /api/v1/assistant/workspaces/{id}/run` one AG-UI run (SSE),
//! `POST /api/v1/assistant/workspaces/{id}/conversations/{conversation}/stop`,
//! `GET|PUT /api/v1/assistant/workspaces/{id}/settings`, `DELETE …/settings/key`, `POST …/settings/test`
//! (workspace owners and admins).

pub(crate) mod agui;
pub(crate) mod anthropic;
pub(crate) mod cards;
pub(crate) mod engine;
pub(crate) mod mentions;
pub(crate) mod model;
pub(crate) mod prompt;
pub(crate) mod registry;
pub(crate) mod settings;
pub(crate) mod store;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use futures_util::future::BoxFuture;
use orbit_platform::{Database, EnvironmentMode, Id, TimestampMillis};
use rmcp::model::CallToolRequestParams;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

use crate::audit::{self, AuditOutcome};
use crate::auth_routes::{CookieMode, request_session};
use crate::mcp::TaskTools;
use crate::repositories::api_tokens::{ApiTokenPrincipal, ApiTokenScope, live_projects};
use crate::repositories::identity::IdentityRepository;
use crate::repositories::membership;
use agui::Emitter;
use anthropic::AnthropicProvider;
use engine::{Engine, Input, Limits, Outcome, RunHooks, ToolRunner};
use model::Usage;
use settings::{
    AiSettings, DEFAULT_MODEL, OFFERED_MODELS, SettingsError, SettingsRepository, estimated_cost,
};
use store::{
    Conversation, ConversationState, ConversationStore, Deleted, MAX_STATE_BYTES, Summary, Switch,
};

/// The audit `request_id` of everything Ask AI does, and of its tool writes.
pub const REQUEST_ID: &str = "assistant";
const MAX_MESSAGE_CHARS: usize = 8_000;

#[derive(Clone)]
pub struct AssistantState {
    identity: Arc<IdentityRepository>,
    cookie_mode: CookieMode,
    tools: TaskTools,
    database: Database,
    settings: SettingsRepository,
    store: ConversationStore,
    registry: Arc<registry::Registry>,
    app_key: Option<[u8; 32]>,
    http: reqwest::Client,
    base_url: String,
    /// `ORBIT_AI_ANTHROPIC_KEY`: the key of workspaces that have none of their own.
    server_key: Option<String>,
    limits: Limits,
    /// Runs on this server, for Stop.
    runs: Arc<Mutex<HashMap<Id, CancellationToken>>>,
}

impl AssistantState {
    /// `ORBIT_AI_ANTHROPIC_KEY` (optional) gives workspaces without a key of their own the
    /// server's key. `ORBIT_DEV_AI_ANTHROPIC_BASE_URL` points at a stand-in for the Anthropic API,
    /// in development only (end-to-end tests with a fake model).
    pub(crate) fn new(
        identity: Arc<IdentityRepository>,
        cookie_mode: CookieMode,
        tools: TaskTools,
        app_key: Option<[u8; 32]>,
        environment: EnvironmentMode,
    ) -> Self {
        let database = identity.database().clone();
        let base_url = std::env::var("ORBIT_DEV_AI_ANTHROPIC_BASE_URL")
            .ok()
            .filter(|url| environment == EnvironmentMode::Development && is_loopback_url(url))
            .unwrap_or_else(|| anthropic::DEFAULT_BASE_URL.to_owned());
        let server_key = std::env::var("ORBIT_AI_ANTHROPIC_KEY")
            .ok()
            .map(|key| key.trim().to_owned())
            .filter(|key| !key.is_empty());
        Self {
            identity,
            cookie_mode,
            tools,
            settings: SettingsRepository::new(database.clone())
                .with_server_key_max(server_key_max_from_env()),
            store: ConversationStore::new(database.clone()),
            database,
            registry: Arc::new(registry::Registry::new()),
            app_key,
            // no redirects: the key goes in a header that a redirect would carry to another host
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap_or_else(|error| {
                    tracing::error!(%error, "Ask AI's HTTP client could not be built");
                    reqwest::Client::builder()
                        .redirect(reqwest::redirect::Policy::none())
                        .build()
                        .unwrap_or_default()
                }),
            base_url,
            server_key,
            limits: Limits::default(),
            runs: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

/// `ORBIT_AI_MAX_DAILY_TOKENS`, or the default; a value that is not a positive whole number is ignored with a
/// warning at startup.
fn server_key_max_from_env() -> i64 {
    let Ok(raw) = std::env::var("ORBIT_AI_MAX_DAILY_TOKENS") else {
        return settings::DEFAULT_DAILY_TOKEN_CAP;
    };
    match parse_token_count(&raw) {
        Some(value) => value.min(settings::MAX_DAILY_TOKEN_CAP),
        None => {
            tracing::warn!(
                default = settings::DEFAULT_DAILY_TOKEN_CAP,
                "ORBIT_AI_MAX_DAILY_TOKENS is not a positive whole number; using the default"
            );
            settings::DEFAULT_DAILY_TOKEN_CAP
        }
    }
}

fn parse_token_count(raw: &str) -> Option<i64> {
    raw.trim()
        .replace(['_', ','], "")
        .parse::<i64>()
        .ok()
        .filter(|value| *value > 0)
}

#[cfg(test)]
#[test]
fn the_server_maximum_parses_only_positive_whole_numbers() {
    assert_eq!(parse_token_count(" 5_000_000 "), Some(5_000_000));
    assert_eq!(parse_token_count("1,000"), Some(1_000));
    for bad in ["0", "-5", "lots", "", "1.5"] {
        assert_eq!(parse_token_count(bad), None, "{bad}");
    }
}

/// A stand-in for the Anthropic API on this machine: `http`, a loopback host, no user info.
fn is_loopback_url(raw: &str) -> bool {
    let Ok(url) = url::Url::parse(raw) else {
        return false;
    };
    let loopback = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        None => false,
    };
    url.scheme() == "http"
        && loopback
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && (url.path() == "/" || url.path().is_empty())
}

#[cfg(test)]
#[test]
fn only_a_loopback_base_url_is_honoured() {
    assert!(is_loopback_url("http://127.0.0.1:18960"));
    assert!(is_loopback_url("http://localhost:18960/"));
    assert!(is_loopback_url("http://[::1]:18960"));
    for bad in [
        "http://127.0.0.1:1@evil.example/",
        "http://evil.example/",
        "https://127.0.0.1:18960",
        "http://127.0.0.1.evil.example:80",
        "http://user:pw@127.0.0.1:18960",
        "http://127.0.0.1:18960/v1?x=1",
        "not a url",
    ] {
        assert!(!is_loopback_url(bad), "{bad}");
    }
}

pub(crate) fn router(state: AssistantState) -> Router {
    let base = "/api/v1/assistant/workspaces/{workspace_id}";
    Router::new()
        .route(&format!("{base}/conversation"), get(current_conversation))
        .route(
            &format!("{base}/conversations"),
            get(list_conversations).post(new_conversation),
        )
        .route(
            &format!("{base}/conversations/{{conversation_id}}"),
            get(open_conversation)
                .patch(rename_conversation)
                .delete(delete_conversation),
        )
        .route(
            &format!("{base}/conversations/{{conversation_id}}/continue"),
            post(continue_conversation),
        )
        .route(
            &format!("{base}/conversations/{{conversation_id}}/stop"),
            post(stop),
        )
        .route(&format!("{base}/run"), post(run))
        .route(
            &format!("{base}/settings"),
            get(get_settings).put(save_settings),
        )
        .route(&format!("{base}/settings/key"), delete(remove_key))
        .route(&format!("{base}/settings/test"), post(test_key))
        .with_state(state)
}

fn problem(status: StatusCode, code: &str, detail: &str) -> Response {
    (status, Json(json!({"code": code, "detail": detail}))).into_response()
}

struct Member {
    user_id: Id,
    name: String,
    email: String,
    workspace_id: Id,
}

/// The signed-in member of the workspace, or the response that refuses the request.
async fn member(
    state: &AssistantState,
    headers: &HeaderMap,
    workspace: &str,
) -> Result<Member, Response> {
    let Some(session) = request_session(&state.identity, state.cookie_mode, headers).await else {
        return Err(problem(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Sign in first.",
        ));
    };
    let Ok(workspace_id) = workspace.parse::<Id>() else {
        return Err(problem(
            StatusCode::NOT_FOUND,
            "not_found",
            "No such workspace.",
        ));
    };
    // Members only: guests and strangers get 404, like every workspace route.
    match membership::actor(state.database.pool(), workspace_id, session.user.id).await {
        Ok(Some(_)) => Ok(Member {
            user_id: session.user.id,
            name: session.user.display_name,
            email: session.user.email,
            workspace_id,
        }),
        Ok(None) => Err(problem(
            StatusCode::NOT_FOUND,
            "not_found",
            "No such workspace.",
        )),
        Err(_) => Err(problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "Try again.",
        )),
    }
}

enum KeyState {
    Ready(String),
    NoKey,
    NoAppKey,
}

/// The key a run uses: the workspace's own, else the server's.
fn resolve_key(
    state: &AssistantState,
    workspace_id: Id,
    settings: Option<&AiSettings>,
) -> KeyState {
    if let Some(sealed) = settings.and_then(|settings| settings.key.as_ref()) {
        let Some(app_key) = &state.app_key else {
            return KeyState::NoAppKey;
        };
        return match settings::open_key(app_key, workspace_id, sealed) {
            Ok(key) => KeyState::Ready(key),
            Err(_) => KeyState::NoAppKey,
        };
    }
    state
        .server_key
        .clone()
        .map_or(KeyState::NoKey, KeyState::Ready)
}

fn setup_word(key: &KeyState) -> &'static str {
    match key {
        KeyState::Ready(_) => "ready",
        KeyState::NoKey => "no_key",
        KeyState::NoAppKey => "no_app_key",
    }
}

/// The AG-UI messages of a conversation, with the approval card that waits, if any.
fn conversation_view(conversation: &Conversation, now: TimestampMillis) -> Value {
    let mut messages = conversation.state.display.clone();
    // only the active conversation can answer an approval: an archived one never shows its card
    if conversation.active
        && let Some(pending) = &conversation.state.pending
        && let Some(message) = messages
            .iter_mut()
            .rev()
            .find(|message| message["id"] == pending.message_id.as_str())
    {
        message["metadata"] = json!({"custom": {"agui": {"interrupts": [pending.interrupt]}}});
    }
    json!({
        "id": conversation.id.to_string(),
        "title": conversation.title,
        "messages": messages,
        "messageCount": conversation.message_count,
        "active": conversation.active,
        "createdAt": conversation.created_at,
        "updatedAt": conversation.updated_at,
        "expiresAt": conversation.expires_at,
        "busy": conversation.busy_until.is_some_and(|until| until > now.as_millis()),
    })
}

/// A conversation as the history lists it.
fn summary_view(summary: &Summary) -> Value {
    json!({
        "id": summary.id.to_string(),
        "title": summary.title,
        "messageCount": summary.message_count,
        "active": summary.active,
        "createdAt": summary.created_at,
        "updatedAt": summary.updated_at,
    })
}

const HISTORY_PAGE: i64 = 50;
const MAX_HISTORY_PAGE: i64 = 100;

#[derive(Deserialize)]
struct ListQuery {
    /// The `nextCursor` of the previous page.
    before: Option<String>,
    limit: Option<i64>,
}

fn parse_cursor(cursor: &str) -> Option<(i64, Id)> {
    let (updated_at, id) = cursor.split_once('.')?;
    Some((updated_at.parse().ok()?, id.parse().ok()?))
}

fn unavailable() -> Response {
    problem(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "Try again.")
}

fn no_conversation() -> Response {
    problem(StatusCode::NOT_FOUND, "not_found", "No such conversation.")
}

fn still_working() -> Response {
    problem(
        StatusCode::CONFLICT,
        "busy",
        "Ask AI is still working; stop it first.",
    )
}

/// The member's own conversations in this workspace, newest use first. Nobody else's: not even a
/// workspace owner or admin can list, open, rename or delete another member's conversations.
async fn list_conversations(
    State(state): State<AssistantState>,
    Path(workspace): Path<String>,
    Query(query): Query<ListQuery>,
    headers: HeaderMap,
) -> Response {
    let member = match member(&state, &headers, &workspace).await {
        Ok(member) => member,
        Err(response) => return response,
    };
    let before = match query.before.as_deref().filter(|cursor| !cursor.is_empty()) {
        None => None,
        Some(cursor) => match parse_cursor(cursor) {
            Some(cursor) => Some(cursor),
            None => return problem(StatusCode::BAD_REQUEST, "invalid_cursor", "Bad cursor."),
        },
    };
    let limit = query
        .limit
        .unwrap_or(HISTORY_PAGE)
        .clamp(1, MAX_HISTORY_PAGE);
    let mut rows = match state
        .store
        .list(
            member.workspace_id,
            member.user_id,
            before,
            limit,
            TimestampMillis::now(),
        )
        .await
    {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "assistant conversations could not be listed");
            return unavailable();
        }
    };
    let more = i64::try_from(rows.len()).unwrap_or(i64::MAX) > limit;
    rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    let next = more
        .then(|| rows.last())
        .flatten()
        .map(|last| format!("{}.{}", last.updated_at, last.id));
    Json(json!({
        "conversations": rows.iter().map(summary_view).collect::<Vec<_>>(),
        "nextCursor": next,
    }))
    .into_response()
}

fn conversation_id(raw: &str) -> Option<Id> {
    raw.parse::<Id>().ok()
}

/// One of the member's conversations with its messages, as the panel showed them.
async fn open_conversation(
    State(state): State<AssistantState>,
    Path((workspace, conversation)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let member = match member(&state, &headers, &workspace).await {
        Ok(member) => member,
        Err(response) => return response,
    };
    let Some(id) = conversation_id(&conversation) else {
        return no_conversation();
    };
    let now = TimestampMillis::now();
    match state
        .store
        .get(id, member.workspace_id, member.user_id, now)
        .await
    {
        Ok(Some(conversation)) => {
            Json(json!({"conversation": conversation_view(&conversation, now)})).into_response()
        }
        Ok(None) => no_conversation(),
        Err(_) => unavailable(),
    }
}

#[derive(Deserialize)]
struct RenameInput {
    title: String,
}

async fn rename_conversation(
    State(state): State<AssistantState>,
    Path((workspace, conversation)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<RenameInput>,
) -> Response {
    let member = match member(&state, &headers, &workspace).await {
        Ok(member) => member,
        Err(response) => return response,
    };
    let Some(id) = conversation_id(&conversation) else {
        return no_conversation();
    };
    let Some(title) = store::clean_title(&input.title) else {
        return problem(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_title",
            "Give the chat a name of at most 120 characters.",
        );
    };
    match state
        .store
        .rename(
            id,
            member.workspace_id,
            member.user_id,
            &title,
            TimestampMillis::now(),
        )
        .await
    {
        Ok(Some(summary)) => Json(json!({"conversation": summary_view(&summary)})).into_response(),
        Ok(None) => no_conversation(),
        Err(_) => unavailable(),
    }
}

async fn delete_conversation(
    State(state): State<AssistantState>,
    Path((workspace, conversation)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let member = match member(&state, &headers, &workspace).await {
        Ok(member) => member,
        Err(response) => return response,
    };
    let Some(id) = conversation_id(&conversation) else {
        return no_conversation();
    };
    match state
        .store
        .delete(
            id,
            member.workspace_id,
            member.user_id,
            TimestampMillis::now(),
        )
        .await
    {
        Ok(Deleted::Done) => StatusCode::NO_CONTENT.into_response(),
        Ok(Deleted::Busy) => still_working(),
        Ok(Deleted::NotFound) => no_conversation(),
        Err(_) => unavailable(),
    }
}

/// Makes an earlier conversation the active one again. The conversation that was active is
/// archived, and its waiting approval expires.
async fn continue_conversation(
    State(state): State<AssistantState>,
    Path((workspace, conversation)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let member = match member(&state, &headers, &workspace).await {
        Ok(member) => member,
        Err(response) => return response,
    };
    let Some(id) = conversation_id(&conversation) else {
        return no_conversation();
    };
    let now = TimestampMillis::now();
    match state
        .store
        .resume(id, member.workspace_id, member.user_id, now)
        .await
    {
        Ok(Switch::Done(conversation)) => {
            Json(json!({"conversation": conversation_view(&conversation, now)})).into_response()
        }
        Ok(Switch::Busy) => still_working(),
        Ok(Switch::NotFound) => no_conversation(),
        Err(error) => {
            tracing::warn!(%error, "assistant conversation could not be continued");
            unavailable()
        }
    }
}

async fn current_conversation(
    State(state): State<AssistantState>,
    Path(workspace): Path<String>,
    headers: HeaderMap,
) -> Response {
    let member = match member(&state, &headers, &workspace).await {
        Ok(member) => member,
        Err(response) => return response,
    };
    let now = TimestampMillis::now();
    let (settings, can_manage, conversation) = match tokio::try_join!(
        async { state.settings.get(member.workspace_id).await },
        async {
            state
                .settings
                .can_manage(member.workspace_id, member.user_id)
                .await
        },
        async {
            state
                .store
                .current(member.workspace_id, member.user_id, now)
                .await
                .map_err(SettingsError::from)
        },
    ) {
        Ok(found) => found,
        Err(_) => return problem(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "Try again."),
    };
    let key = resolve_key(&state, member.workspace_id, settings.as_ref());
    Json(json!({
        "conversation": conversation.as_ref().map(|conversation| conversation_view(conversation, now)),
        "setup": setup_word(&key),
        "canManage": can_manage,
        "model": settings.map_or_else(|| DEFAULT_MODEL.to_owned(), |settings| settings.model),
    }))
    .into_response()
}

async fn new_conversation(
    State(state): State<AssistantState>,
    Path(workspace): Path<String>,
    headers: HeaderMap,
) -> Response {
    let member = match member(&state, &headers, &workspace).await {
        Ok(member) => member,
        Err(response) => return response,
    };
    let now = TimestampMillis::now();
    match state
        .store
        .create(
            member.workspace_id,
            member.user_id,
            prompt::new_nonce(),
            now,
        )
        .await
    {
        Ok(Some(conversation)) => {
            Json(json!({"conversation": conversation_view(&conversation, now)})).into_response()
        }
        Ok(None) => still_working(),
        Err(error) => {
            tracing::warn!(%error, "assistant conversation could not be created");
            problem(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "Try again.")
        }
    }
}

async fn stop(
    State(state): State<AssistantState>,
    Path((workspace, conversation)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let member = match member(&state, &headers, &workspace).await {
        Ok(member) => member,
        Err(response) => return response,
    };
    let Ok(conversation_id) = conversation.parse::<Id>() else {
        return problem(StatusCode::NOT_FOUND, "not_found", "No such conversation.");
    };
    // Only the owner's own conversation: checked before the token is touched.
    match state
        .store
        .get(
            conversation_id,
            member.workspace_id,
            member.user_id,
            TimestampMillis::now(),
        )
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) => return problem(StatusCode::NOT_FOUND, "not_found", "No such conversation."),
        Err(_) => return problem(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "Try again."),
    }
    if let Some(cancel) = state
        .runs
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&conversation_id)
    {
        cancel.cancel();
    }
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunInput {
    thread_id: String,
    run_id: String,
    #[serde(default)]
    messages: Vec<Value>,
    #[serde(default)]
    resume: Vec<Value>,
    #[serde(default)]
    context: Vec<Value>,
}

fn message_text(message: &Value) -> String {
    match &message["content"] {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// What the run is asked to do. The client's transcript is never trusted: only its last user
/// message (a new question) or its answer to the pending approval is read.
/// The panel's Follow along switch, sent with every run (default on, as in the panel).
fn follow_along(input: &RunInput) -> bool {
    input
        .context
        .iter()
        .find(|entry| entry["description"] == "followAlong")
        .and_then(|entry| entry["value"].as_str())
        .is_none_or(|value| value != "off")
}

fn parse_input(input: &RunInput) -> Result<Input, &'static str> {
    if let Some(entry) = input.resume.first() {
        let interrupt_id = entry["interruptId"].as_str().unwrap_or_default().to_owned();
        let resolved = entry["status"] == "resolved";
        return Ok(Input::Resume {
            interrupt_id,
            approved: resolved && entry["payload"]["approved"] == true,
            allow_for_conversation: resolved
                && entry["payload"]["reason"] == "allow_for_conversation",
        });
    }
    let last = input.messages.last().ok_or("Send a message.")?;
    if last["role"] != "user" {
        return Err("Send a message.");
    }
    let text = message_text(last).trim().to_owned();
    if text.is_empty() {
        return Err("Send a message.");
    }
    if text.chars().count() > MAX_MESSAGE_CHARS {
        return Err("That message is too long; keep it under 8,000 characters.");
    }
    let page = input
        .context
        .iter()
        .find(|entry| entry["description"] == "page")
        .and_then(|entry| entry["value"].as_str())
        .map(str::to_owned);
    Ok(Input::Message {
        text,
        page,
        mentions: Vec::new(),
    })
}

/// An SSE response that only carries a short message (no run happens).
fn notice(
    thread_id: &str,
    run_id: &str,
    text: Option<&str>,
    error: Option<(&str, &str)>,
) -> Response {
    let mut events = vec![json!({"type": "RUN_STARTED", "threadId": thread_id, "runId": run_id})];
    if let Some(text) = text {
        let id = format!("msg-{}", Id::new_v7());
        events.push(json!({"type": "TEXT_MESSAGE_START", "messageId": id, "role": "assistant"}));
        events.push(json!({"type": "TEXT_MESSAGE_CONTENT", "messageId": id, "delta": text}));
        events.push(json!({"type": "TEXT_MESSAGE_END", "messageId": id}));
    }
    match error {
        Some((message, code)) => {
            events.push(json!({"type": "RUN_ERROR", "message": message, "code": code}))
        }
        None => {
            events.push(json!({"type": "RUN_FINISHED", "threadId": thread_id, "runId": run_id}))
        }
    }
    let stream = futures_util::stream::iter(
        events
            .into_iter()
            .map(|event| Ok::<_, Infallible>(Event::default().data(event.to_string()))),
    );
    Sse::new(stream).into_response()
}

async fn run(
    State(state): State<AssistantState>,
    Path(workspace): Path<String>,
    headers: HeaderMap,
    Json(input): Json<RunInput>,
) -> Response {
    let member = match member(&state, &headers, &workspace).await {
        Ok(member) => member,
        Err(response) => return response,
    };
    let (thread_id, run_id) = (input.thread_id.clone(), input.run_id.clone());
    let now = TimestampMillis::now();
    let conversation = match input.thread_id.parse::<Id>() {
        // only the member's active conversation runs (an archived one must be continued first)
        Ok(id) => {
            state
                .store
                .get_active(id, member.workspace_id, member.user_id, now)
                .await
        }
        Err(_) => Ok(None),
    };
    let mut conversation = match conversation {
        Ok(Some(conversation)) => conversation,
        Ok(None) => {
            return notice(
                &thread_id,
                &run_id,
                None,
                Some((
                    "This conversation has ended (another chat is open now). Start a new chat, or continue it from the history.",
                    "conversation_gone",
                )),
            );
        }
        Err(_) => return problem(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "Try again."),
    };
    let follow_along = follow_along(&input);
    let request = match parse_input(&input) {
        Ok(request) => request,
        Err(message) => return notice(&thread_id, &run_id, None, Some((message, "invalid_input"))),
    };
    let settings = match state.settings.get(member.workspace_id).await {
        Ok(settings) => settings,
        Err(_) => return problem(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "Try again."),
    };
    let model = settings.as_ref().map_or_else(
        || DEFAULT_MODEL.to_owned(),
        |settings| settings.model.clone(),
    );
    let api_key = match resolve_key(&state, member.workspace_id, settings.as_ref()) {
        KeyState::Ready(key) => key,
        KeyState::NoKey => {
            return notice(
                &thread_id,
                &run_id,
                Some(
                    "Ask AI needs an Anthropic API key first. A workspace owner or admin can add one in Settings → AI.",
                ),
                None,
            );
        }
        KeyState::NoAppKey => {
            return notice(
                &thread_id,
                &run_id,
                Some(
                    "The server cannot open this workspace's API key (ORBIT_APP_KEY is missing or changed). A workspace admin can enter the key again in Settings → AI.",
                ),
                None,
            );
        }
    };
    if matches!(request, Input::Message { .. })
        && serde_json::to_vec(&conversation.state).map_or(0, |bytes| bytes.len()) > MAX_STATE_BYTES
    {
        return notice(
            &thread_id,
            &run_id,
            Some("This conversation is full. Start a new chat."),
            None,
        );
    }
    let lease_until = now.as_millis()
        + i64::try_from(state.limits.run_time.as_millis()).unwrap_or(i64::MAX / 2)
        + 60_000;
    // The previous run saves right after its response ends: wait a moment for its lease before refusing.
    let mut leased = false;
    for _ in 0..10 {
        match state
            .store
            .lease(&conversation, &run_id, lease_until, TimestampMillis::now())
            .await
        {
            Ok(true) => {
                leased = true;
                break;
            }
            Ok(false) => tokio::time::sleep(Duration::from_millis(300)).await,
            Err(_) => return problem(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "Try again."),
        }
    }
    if !leased {
        return notice(
            &thread_id,
            &run_id,
            None,
            Some(("Ask AI is still working on this conversation.", "busy")),
        );
    }
    // the state as the previous run saved it
    conversation = match state
        .store
        .get_active(
            conversation.id,
            member.workspace_id,
            member.user_id,
            TimestampMillis::now(),
        )
        .await
    {
        Ok(Some(fresh)) => fresh,
        _ => return problem(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "Try again."),
    };
    if conversation.state.nonce.is_empty() {
        conversation.state.nonce = prompt::new_nonce();
    }
    // A chat left alone longer than the panel keeps it open (a tab left open) starts afresh, as a
    // continued one does: what waited for approval expires and "Allow in this chat" ends.
    if now.as_millis() - conversation.updated_at > store::IDLE_MS {
        engine::retire(&mut conversation.state);
    }
    let workspace_name: String = sqlx::query_scalar("SELECT name FROM workspaces WHERE id = $1")
        .bind(member.workspace_id.to_string())
        .fetch_optional(state.database.pool())
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let principal = match principal(
        &state.database,
        conversation.workspace_id,
        conversation.id,
        member.user_id,
    )
    .await
    {
        Ok(principal) => principal,
        Err(()) => {
            let _ = state
                .store
                .save(&conversation, &conversation.state, &run_id, true, now)
                .await;
            return problem(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "Try again.");
        }
    };
    let today = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(now.as_millis())
        .map(|time| time.format("%A %Y-%m-%d (UTC)").to_string())
        .unwrap_or_default();
    let system = prompt::system_prompt(
        &prompt::Who {
            user_name: &member.name,
            user_email: &member.email,
            user_id: member.user_id,
            workspace_name: &workspace_name,
            workspace_id: member.workspace_id,
            today: &today,
        },
        &conversation.state.nonce,
    );

    let (sender, receiver) = mpsc::channel::<Value>(256);
    let cancel = CancellationToken::new();
    state
        .runs
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(conversation.id, cancel.clone());
    // The browser went away (Stop, reload, closed tab): the run stops. The watcher lets go of its sender when the
    // run ends, so the response then ends too (the panel waits for that before it shows the approval as answerable).
    let finished = CancellationToken::new();
    {
        let sender = sender.clone();
        let cancel = cancel.clone();
        let finished = finished.clone();
        tokio::spawn(async move {
            tokio::select! {
                () = sender.closed() => cancel.cancel(),
                () = finished.cancelled() => {}
            }
        });
    }
    let context = input.context.clone();
    let provider = AnthropicProvider::new(state.http.clone(), &state.base_url, api_key);
    tokio::spawn(async move {
        let emitter = Emitter::new(sender);
        let tools = SessionTools {
            tools: state.tools.clone(),
            principal,
        };
        // the member's mentions, read again with their own access (before the run, outside any transaction)
        let mut request = request;
        mentions::attach(&mut request, &context, &tools, &cancel).await;
        let hooks = DbHooks {
            state: state.clone(),
            conversation: conversation.clone(),
            user_id: member.user_id,
            run_id: run_id.clone(),
        };
        let mut conversation_state = conversation.state.clone();
        let engine = Engine {
            provider: &provider,
            tools: &tools,
            hooks: &hooks,
            registry: &state.registry,
            model: &model,
            system: &system,
            workspace_id: member.workspace_id,
            thread_id: &thread_id,
            run_id: &run_id,
            out: &emitter,
            cancel: cancel.clone(),
            limits: state.limits,
            follow_along,
            names: cards::Names::default(),
        };
        let outcome = engine.run(&mut conversation_state, request).await;
        drop(engine);
        if outcome == Outcome::Cancelled {
            emitter.run_finished(&thread_id, &run_id, None).await;
        }
        // the response ends here; saving follows
        drop(emitter);
        finished.cancel();
        if let Err(error) = state
            .store
            .save(
                &conversation,
                &conversation_state,
                &run_id,
                true,
                TimestampMillis::now(),
            )
            .await
        {
            tracing::warn!(%error, "assistant conversation could not be saved");
        }
        state
            .runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&conversation.id);
    });
    let stream = futures_util::StreamExt::map(ReceiverStream::new(receiver), |event| {
        Ok::<_, Infallible>(Event::default().data(event.to_string()))
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// The signed-in member as a tool principal: their own access, every project they can see.
pub(crate) async fn principal(
    database: &Database,
    workspace_id: Id,
    conversation_id: Id,
    user_id: Id,
) -> Result<ApiTokenPrincipal, ()> {
    let project_ids = live_projects(database.pool(), workspace_id, Some(user_id))
        .await
        .map_err(|_| ())?;
    Ok(ApiTokenPrincipal {
        // Not a token: writes are attributed to the member with `via: "assistant"`.
        token_id: conversation_id,
        workspace_id,
        project_ids,
        all_projects: true,
        creator_id: user_id,
        actor_id: user_id,
        service_account: None,
        scopes: vec![
            ApiTokenScope::TasksWrite,
            ApiTokenScope::DocsWrite,
            ApiTokenScope::ChatRead,
        ],
    })
}

pub(crate) struct SessionTools {
    pub(crate) tools: TaskTools,
    pub(crate) principal: ApiTokenPrincipal,
}

impl ToolRunner for SessionTools {
    fn call<'a>(&'a self, name: &'a str, input: Value) -> BoxFuture<'a, Result<Value, String>> {
        Box::pin(async move {
            let Value::Object(arguments) = input else {
                return Err("The arguments must be an object.".to_owned());
            };
            let request = CallToolRequestParams::new(name.to_owned()).with_arguments(arguments);
            match self
                .tools
                .call_in_process(self.principal.clone(), request)
                .await
            {
                Ok(result) if result.is_error != Some(true) => {
                    Ok(result.structured_content.unwrap_or(Value::Null))
                }
                Ok(result) => {
                    let content = serde_json::to_value(&result.content).unwrap_or_default();
                    let text: Vec<String> = content
                        .as_array()
                        .map(|parts| {
                            parts
                                .iter()
                                .filter_map(|part| part["text"].as_str().map(str::to_owned))
                                .collect()
                        })
                        .unwrap_or_default();
                    Err(if text.is_empty() {
                        "The tool failed.".to_owned()
                    } else {
                        text.join("\n")
                    })
                }
                Err(error) => Err(error.message.to_string()),
            }
        })
    }

    fn lookup<'a>(&'a self, name: &'a str, input: Value) -> BoxFuture<'a, Result<Value, String>> {
        Box::pin(async move {
            match (name, input) {
                (cards::COUNT_TASKS, Value::Object(arguments)) => {
                    self.tools
                        .count_tasks_in_process(self.principal.clone(), arguments)
                        .await
                }
                (cards::COUNT_TASKS, _) => Err("The arguments must be an object.".to_owned()),
                (name, input) => self.call(name, input).await,
            }
        })
    }
}

struct DbHooks {
    state: AssistantState,
    conversation: Conversation,
    user_id: Id,
    run_id: String,
}

impl RunHooks for DbHooks {
    fn take_request(&self) -> BoxFuture<'_, engine::Gate> {
        Box::pin(async move {
            self.state
                .settings
                .gate(
                    self.conversation.workspace_id,
                    self.user_id,
                    TimestampMillis::now(),
                )
                .await
        })
    }

    fn reserve_usage<'a>(&'a self, model: &'a str, estimate: Usage) -> BoxFuture<'a, Option<Id>> {
        Box::pin(async move {
            match self
                .state
                .settings
                .record_usage(
                    self.conversation.workspace_id,
                    self.user_id,
                    self.conversation.id,
                    model,
                    estimate,
                    TimestampMillis::now(),
                )
                .await
            {
                Ok(id) => Some(id),
                Err(error) => {
                    tracing::warn!(%error, "assistant usage could not be reserved; the request is refused");
                    None
                }
            }
        })
    }

    fn settle_usage<'a>(
        &'a self,
        reservation: Id,
        model: &'a str,
        usage: Usage,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            // on failure the reservation (the estimate) stays: the limit errs on the safe side
            if let Err(error) = self
                .state
                .settings
                .settle_usage(self.conversation.workspace_id, reservation, model, usage)
                .await
            {
                tracing::warn!(%error, "assistant usage could not be settled");
            }
        })
    }

    fn audit_change<'a>(&'a self, tool: &'a str, outcome: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let workspace_id = self.conversation.workspace_id;
            let result = async {
                let mut transaction = self
                    .state
                    .database
                    .workspace_transaction(workspace_id)
                    .await?;
                audit::record(
                    &mut transaction,
                    workspace_id,
                    Some(self.user_id),
                    "assistant.change",
                    if outcome == "failed" {
                        AuditOutcome::Failure
                    } else {
                        AuditOutcome::Success
                    },
                    "assistant_conversation",
                    Some(self.conversation.id),
                    REQUEST_ID,
                    json!({"tool": tool, "outcome": outcome, "via": "assistant"}),
                    TimestampMillis::now(),
                )
                .await?;
                transaction.commit().await
            }
            .await;
            if let Err(error) = result {
                tracing::warn!(%error, "assistant audit row could not be written");
            }
        })
    }

    fn checkpoint<'a>(&'a self, state: &'a ConversationState) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            match self
                .state
                .store
                .save(
                    &self.conversation,
                    state,
                    &self.run_id,
                    false,
                    TimestampMillis::now(),
                )
                .await
            {
                Ok(saved) => saved,
                Err(error) => {
                    tracing::warn!(%error, "assistant conversation could not be saved");
                    false
                }
            }
        })
    }
}

// ---- settings ----

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsBody {
    model: String,
    #[serde(default)]
    api_key: Option<String>,
    /// Tokens per UTC day for the whole workspace (cache reads count a tenth).
    #[serde(default)]
    daily_token_cap: Option<i64>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct TestBody {
    #[serde(default)]
    api_key: Option<String>,
}

fn valid_key(key: &str) -> bool {
    key.starts_with("sk-ant-")
        && (20..=400).contains(&key.len())
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

fn settings_problem(error: &SettingsError) -> Response {
    match error {
        SettingsError::NotFound => {
            problem(StatusCode::NOT_FOUND, "not_found", "No such workspace.")
        }
        SettingsError::Forbidden => problem(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Only workspace owners and admins can change the AI settings.",
        ),
        SettingsError::NoAppKey => problem(
            StatusCode::CONFLICT,
            "app_key_missing",
            "The server has no ORBIT_APP_KEY, so it cannot store an API key safely.",
        ),
        SettingsError::Sealing => problem(
            StatusCode::CONFLICT,
            "key_unreadable",
            "The stored API key cannot be opened with this server's app key. Enter it again.",
        ),
        SettingsError::AboveServerMax(max) => problem(
            StatusCode::UNPROCESSABLE_ENTITY,
            "daily_token_cap_above_server_max",
            &format!(
                "This workspace uses the server's API key, so its daily token limit is at most {max}. Add the workspace's own key to set a higher one."
            ),
        ),
        SettingsError::InvalidCap => problem(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_daily_token_cap",
            "The daily token limit must be between 1 and 1,000,000,000.",
        ),
        SettingsError::UnknownModel => problem(
            StatusCode::UNPROCESSABLE_ENTITY,
            "unknown_model",
            "Pick one of the offered models.",
        ),
        SettingsError::Database(_) => {
            problem(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "Try again.")
        }
    }
}

async fn manager(
    state: &AssistantState,
    headers: &HeaderMap,
    workspace: &str,
) -> Result<Member, Response> {
    let member = member(state, headers, workspace).await?;
    match state
        .settings
        .can_manage(member.workspace_id, member.user_id)
        .await
    {
        Ok(true) => Ok(member),
        Ok(false) => Err(settings_problem(&SettingsError::Forbidden)),
        Err(error) => Err(settings_problem(&error)),
    }
}

fn usage_view(rows: &[settings::ModelUsage]) -> Value {
    let mut total = settings::UsageTotals::default();
    let mut cost = 0.0;
    let mut unpriced = 0;
    for row in rows {
        total.requests += row.totals.requests;
        total.input_tokens += row.totals.input_tokens;
        total.output_tokens += row.totals.output_tokens;
        total.cache_read_tokens += row.totals.cache_read_tokens;
        total.cache_write_tokens += row.totals.cache_write_tokens;
        match estimated_cost(&row.model, row.long_prompt, &row.totals) {
            Some(estimate) => cost += estimate,
            None => unpriced += row.totals.requests,
        }
    }
    json!({
        "models": rows,
        "total": total,
        // in dollars, to a hundredth of a cent; the page rounds it
        "estimatedCostUsd": (cost * 10_000.0).round() / 10_000.0,
        "unpricedRequests": unpriced,
    })
}

async fn settings_view(state: &AssistantState, workspace_id: Id) -> Result<Value, SettingsError> {
    let settings = state.settings.get(workspace_id).await?;
    let now = TimestampMillis::now().as_millis();
    let day_start = now - now.rem_euclid(86_400_000);
    let today = state
        .settings
        .usage_since(workspace_id, TimestampMillis::from_millis(day_start))
        .await?;
    let month = state
        .settings
        .usage_since(
            workspace_id,
            TimestampMillis::from_millis(now - 30 * 86_400_000),
        )
        .await?;
    let own = settings.as_ref().and_then(|settings| settings.key.as_ref());
    Ok(json!({
        "provider": settings::PROVIDER_ANTHROPIC,
        "model": settings.as_ref().map_or(DEFAULT_MODEL, |settings| settings.model.as_str()),
        "models": OFFERED_MODELS,
        "key": {
            "set": own.is_some() || state.server_key.is_some(),
            "last4": own.map(|key| key.last4.clone()),
            "source": if own.is_some() { Some("workspace") } else if state.server_key.is_some() { Some("server") } else { None },
        },
        "appKey": state.app_key.is_some(),
        "usage": {"today": usage_view(&today), "last30Days": usage_view(&month)},
        "dailyTokenCap": state.settings.effective_cap(settings.as_ref()),
        "maxDailyTokenCap": if own.is_some() {
            settings::MAX_DAILY_TOKEN_CAP
        } else {
            state.settings.server_key_max()
        },
        "tokensToday": state
            .settings
            .tokens_today(workspace_id, TimestampMillis::now())
            .await?,
    }))
}

async fn get_settings(
    State(state): State<AssistantState>,
    Path(workspace): Path<String>,
    headers: HeaderMap,
) -> Response {
    let member = match manager(&state, &headers, &workspace).await {
        Ok(member) => member,
        Err(response) => return response,
    };
    match settings_view(&state, member.workspace_id).await {
        Ok(view) => Json(view).into_response(),
        Err(error) => settings_problem(&error),
    }
}

async fn save_settings(
    State(state): State<AssistantState>,
    Path(workspace): Path<String>,
    headers: HeaderMap,
    Json(body): Json<SettingsBody>,
) -> Response {
    let member = match manager(&state, &headers, &workspace).await {
        Ok(member) => member,
        Err(response) => return response,
    };
    let key = match body
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
    {
        None => None,
        Some(key) if !valid_key(key) => {
            return problem(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_key",
                "That does not look like an Anthropic API key (it starts with sk-ant-).",
            );
        }
        Some(key) => {
            let Some(app_key) = &state.app_key else {
                return settings_problem(&SettingsError::NoAppKey);
            };
            match settings::seal_key(app_key, member.workspace_id, key) {
                Ok(sealed) => Some(Some(sealed)),
                Err(error) => return settings_problem(&error),
            }
        }
    };
    if let Err(error) = state
        .settings
        .save(
            member.workspace_id,
            member.user_id,
            &body.model,
            key,
            body.daily_token_cap,
            TimestampMillis::now(),
        )
        .await
    {
        return settings_problem(&error);
    }
    match settings_view(&state, member.workspace_id).await {
        Ok(view) => Json(view).into_response(),
        Err(error) => settings_problem(&error),
    }
}

async fn remove_key(
    State(state): State<AssistantState>,
    Path(workspace): Path<String>,
    headers: HeaderMap,
) -> Response {
    let member = match manager(&state, &headers, &workspace).await {
        Ok(member) => member,
        Err(response) => return response,
    };
    let model = match state.settings.get(member.workspace_id).await {
        Ok(settings) => {
            settings.map_or_else(|| DEFAULT_MODEL.to_owned(), |settings| settings.model)
        }
        Err(error) => return settings_problem(&error),
    };
    if let Err(error) = state
        .settings
        .save(
            member.workspace_id,
            member.user_id,
            &model,
            Some(None),
            None,
            TimestampMillis::now(),
        )
        .await
    {
        return settings_problem(&error);
    }
    match settings_view(&state, member.workspace_id).await {
        Ok(view) => Json(view).into_response(),
        Err(error) => settings_problem(&error),
    }
}

/// Checks a key (the one typed in, else the stored one) with the free model list request.
async fn test_key(
    State(state): State<AssistantState>,
    Path(workspace): Path<String>,
    headers: HeaderMap,
    body: Option<Json<TestBody>>,
) -> Response {
    let member = match manager(&state, &headers, &workspace).await {
        Ok(member) => member,
        Err(response) => return response,
    };
    let body = body.map(|Json(body)| body).unwrap_or_default();
    let key = match body
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
    {
        Some(key) if !valid_key(key) => {
            return Json(json!({"ok": false, "error": "That does not look like an Anthropic API key (it starts with sk-ant-)."})).into_response();
        }
        Some(key) => key.to_owned(),
        None => {
            let settings = match state.settings.get(member.workspace_id).await {
                Ok(settings) => settings,
                Err(error) => return settings_problem(&error),
            };
            match resolve_key(&state, member.workspace_id, settings.as_ref()) {
                KeyState::Ready(key) => key,
                KeyState::NoKey => {
                    return Json(json!({"ok": false, "error": "No API key is set."}))
                        .into_response();
                }
                KeyState::NoAppKey => return settings_problem(&SettingsError::Sealing),
            }
        }
    };
    // the free model list still counts against the user's per-minute limit; a database error refuses (fail closed)
    if !state
        .settings
        .take_request(member.user_id, TimestampMillis::now())
        .await
        .unwrap_or(false)
    {
        return Json(json!({"ok": false, "error": "Too many requests; wait a minute."}))
            .into_response();
    }
    let provider = AnthropicProvider::new(state.http.clone(), &state.base_url, key);
    match provider.models().await {
        Ok(models) => {
            let available: Vec<&str> = OFFERED_MODELS
                .iter()
                .copied()
                .filter(|offered| models.iter().any(|model| model == offered))
                .collect();
            Json(json!({"ok": true, "available": available, "models": models})).into_response()
        }
        Err(error) => {
            Json(json!({"ok": false, "error": engine::provider_error_text(&error, "this model")}))
                .into_response()
        }
    }
}
