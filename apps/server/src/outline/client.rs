//! HTTP client for Outline's API (`POST {base}/api/<method>`, cloud or self-hosted).
//!
//! - Every call carries `Authorization: Bearer <token>` and `x-api-version: 3`, so documents
//!   and collections come with their ProseMirror `data` (`presentDocument`); an older server
//!   that ignores the header answers with Markdown `text`, which the importer reads instead.
//! - Lists are read with `offset`/`limit` (100, Outline's maximum) until a page comes back short.
//! - `429` waits for `Retry-After` (seconds; else an exponential backoff) and tries again, as do
//!   `5xx` answers and dropped connections, a few times.
//! - The base URL must be `https` to a public host (`http`/loopback only under the development
//!   flag); host names resolve through [`PublicResolver`]. API calls follow no redirects.
//! - Attachments are fetched from `/api/attachments.redirect?id=…` with the token; its redirect
//!   to the file storage is followed without the token (unless it stays on the same origin).
//! - The token never appears in errors, logs or `Debug` output.

use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use orbit_platform::{PublicResolver, is_public_ip};
use reqwest::header::{self, HeaderMap, HeaderValue};
use reqwest::{StatusCode, Url};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use thiserror::Error;

use super::model::{
    ApiAuthInfo, ApiCollection, ApiDocument, ApiUser, InfoResponse, ListResponse, NavigationNode,
};

const MAX_URL_BYTES: usize = 2_048;
const MAX_TOKEN_CHARS: usize = 1_024;
/// Outline's largest page (`Pagination.maxLimit`).
pub const PAGE_LIMIT: u64 = 100;
/// Most pages one list reads (100,000 items).
const MAX_PAGES: u64 = 1_000;
/// Largest JSON answer read into memory.
const MAX_JSON_BYTES: usize = 64 * 1024 * 1024;
const MAX_REDIRECTS: usize = 3;

#[derive(Debug, Error)]
pub enum OutlineError {
    #[error("the Outline URL is invalid")]
    UrlInvalid,
    #[error("the API token is invalid")]
    TokenInvalid,
    /// `401`/`403`: the token is wrong, expired, revoked or lacks a scope.
    #[error("Outline refused the API token")]
    Unauthorized,
    #[error("not found on Outline")]
    NotFound,
    /// Still rate limited after the allowed attempts.
    #[error("Outline's rate limit was still exceeded after several retries")]
    RateLimited,
    #[error("the file is larger than {limit} bytes")]
    TooLarge { limit: u64 },
    /// Connection, timeout or a `5xx` after retries.
    #[error("Outline could not be reached: {0}")]
    Unreachable(String),
    #[error("Outline answered {status}: {message}")]
    Api { status: u16, message: String },
    #[error("Outline's answer could not be read: {0}")]
    Decode(String),
}

/// Retry and backoff settings.
#[derive(Clone, Copy, Debug)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_delay: Duration,
    /// A `Retry-After` longer than this is capped to it.
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 6,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
        }
    }
}

/// The base URL of an Outline instance, checked as [`OutlineClient`] requires. Path, query and
/// fragment are dropped.
pub fn parse_base_url(raw: &str, allow_local: bool) -> Result<Url, OutlineError> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > MAX_URL_BYTES {
        return Err(OutlineError::UrlInvalid);
    }
    let url = Url::parse(raw).map_err(|_| OutlineError::UrlInvalid)?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(OutlineError::UrlInvalid);
    }
    let host = url.host().ok_or(OutlineError::UrlInvalid)?;
    let loopback = match host {
        url::Host::Ipv4(ip) => ip.is_loopback(),
        url::Host::Ipv6(ip) => ip.is_loopback(),
        url::Host::Domain(name) => name.eq_ignore_ascii_case("localhost"),
    };
    let public = match host {
        url::Host::Ipv4(ip) => is_public_ip(IpAddr::V4(ip)),
        url::Host::Ipv6(ip) => is_public_ip(IpAddr::V6(ip)),
        url::Host::Domain(name) => !name.eq_ignore_ascii_case("localhost"),
    };
    let allowed = match url.scheme() {
        "https" => public || (allow_local && loopback),
        "http" => allow_local && loopback,
        _ => false,
    };
    if !allowed {
        return Err(OutlineError::UrlInvalid);
    }
    let mut base = url;
    base.set_path("/");
    base.set_query(None);
    base.set_fragment(None);
    Ok(base)
}

/// Checks a token's characters (Outline API keys are `ol_api_…`; any printable ASCII is let
/// through to Outline to judge).
pub fn check_token(token: &str) -> Result<&str, OutlineError> {
    let token = token.trim();
    if token.is_empty()
        || token.chars().count() > MAX_TOKEN_CHARS
        || !token.chars().all(|c| c.is_ascii_graphic())
    {
        return Err(OutlineError::TokenInvalid);
    }
    Ok(token)
}

/// Client for one base URL and one token. Cheap to clone.
#[derive(Clone)]
pub struct OutlineClient {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    base: Url,
    authorization: HeaderValue,
    retry: RetryPolicy,
    allow_local: bool,
}

impl fmt::Debug for OutlineClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutlineClient")
            .field("base", &self.inner.base.as_str())
            .finish_non_exhaustive()
    }
}

/// A downloaded attachment.
#[derive(Clone, Debug)]
pub struct Download {
    pub bytes: Vec<u8>,
    pub content_type: Option<String>,
    /// The file name from `Content-Disposition`, if any.
    pub file_name: Option<String>,
}

#[derive(Deserialize)]
struct ErrorBody {
    #[serde(default)]
    message: String,
    #[serde(default)]
    error: String,
}

impl OutlineClient {
    /// `base` comes from [`parse_base_url`].
    pub fn new(
        base: Url,
        token: &str,
        allow_local: bool,
        retry: RetryPolicy,
    ) -> Result<Self, OutlineError> {
        let token = check_token(token)?;
        let mut authorization = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| OutlineError::TokenInvalid)?;
        authorization.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert("x-api-version", HeaderValue::from_static("3"));
        let mut builder = reqwest::Client::builder()
            .user_agent(concat!("Orbit/", env!("CARGO_PKG_VERSION"), " (Outline import)"))
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            // A proxy from the environment would resolve the host itself, past the checks here.
            .no_proxy()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120));
        if !allow_local {
            builder = builder.dns_resolver(Arc::new(PublicResolver));
        }
        let http = builder
            .build()
            .map_err(|error| OutlineError::Unreachable(error.to_string()))?;
        Ok(Self {
            inner: Arc::new(Inner {
                http,
                base,
                authorization,
                retry,
                allow_local,
            }),
        })
    }

    #[must_use]
    pub fn base(&self) -> &Url {
        &self.inner.base
    }

    fn endpoint(&self, method: &str) -> Url {
        let mut url = self.inner.base.clone();
        url.set_path(&format!("/api/{method}"));
        url
    }

    /// `POST /api/<method>` with a JSON body, retried on `429`, `5xx` and transport errors.
    async fn post_raw(&self, method: &str, body: &Value) -> Result<Vec<u8>, OutlineError> {
        let retry = self.inner.retry;
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            let sent = self
                .inner
                .http
                .post(self.endpoint(method))
                .header(header::AUTHORIZATION, self.inner.authorization.clone())
                .json(body)
                .send()
                .await;
            let response = match sent {
                Ok(response) => response,
                Err(error) => {
                    if attempt >= retry.max_attempts.min(3) {
                        return Err(transport(&error));
                    }
                    tokio::time::sleep(backoff(retry, attempt)).await;
                    continue;
                }
            };
            let status = response.status();
            if status == StatusCode::TOO_MANY_REQUESTS {
                if attempt >= retry.max_attempts {
                    return Err(OutlineError::RateLimited);
                }
                let wait = retry_after(response.headers())
                    .unwrap_or_else(|| backoff(retry, attempt))
                    .min(retry.max_delay);
                tracing::debug!(method, wait_ms = wait.as_millis() as u64, "Outline rate limit, waiting");
                tokio::time::sleep(wait).await;
                continue;
            }
            if status.is_server_error() {
                if attempt >= retry.max_attempts.min(3) {
                    return Err(OutlineError::Unreachable(format!("HTTP {}", status.as_u16())));
                }
                tokio::time::sleep(backoff(retry, attempt)).await;
                continue;
            }
            let bytes = limited_body(response, MAX_JSON_BYTES as u64).await?;
            return match status.as_u16() {
                200..=299 => Ok(bytes),
                401 | 403 => Err(OutlineError::Unauthorized),
                404 => Err(OutlineError::NotFound),
                code => {
                    let message = serde_json::from_slice::<ErrorBody>(&bytes)
                        .map(|body| {
                            if body.message.is_empty() {
                                body.error
                            } else {
                                body.message
                            }
                        })
                        .unwrap_or_default();
                    Err(OutlineError::Api {
                        status: code,
                        message: message.chars().take(300).collect(),
                    })
                }
            };
        }
    }

    async fn post<T: DeserializeOwned>(&self, method: &str, body: &Value) -> Result<T, OutlineError> {
        let bytes = self.post_raw(method, body).await?;
        serde_json::from_slice(&bytes).map_err(|error| OutlineError::Decode(error.to_string()))
    }

    /// Every item of a paginated list (`offset`/`limit`), in order.
    pub async fn list_all<T: DeserializeOwned>(
        &self,
        method: &str,
        body: Value,
    ) -> Result<Vec<T>, OutlineError> {
        let mut items = Vec::new();
        let mut offset = 0u64;
        for _ in 0..MAX_PAGES {
            let mut page_body = body.clone();
            if let Some(object) = page_body.as_object_mut() {
                object.insert("offset".into(), json!(offset));
                object.insert("limit".into(), json!(PAGE_LIMIT));
            }
            let page: ListResponse<T> = self.post(method, &page_body).await?;
            let count = page.data.len() as u64;
            items.extend(page.data);
            let more = page
                .pagination
                .as_ref()
                .is_none_or(|pagination| pagination.next_path.is_some());
            if count < PAGE_LIMIT || !more {
                return Ok(items);
            }
            offset += count;
        }
        Ok(items)
    }

    /// `auth.info`: validates the token.
    pub async fn auth_info(&self) -> Result<ApiAuthInfo, OutlineError> {
        let info: InfoResponse<ApiAuthInfo> = self.post("auth.info", &json!({})).await?;
        Ok(info.data)
    }

    /// `collections.list` (active collections).
    pub async fn collections(&self) -> Result<Vec<ApiCollection>, OutlineError> {
        self.list_all("collections.list", json!({})).await
    }

    /// `collections.documents`: the collection's tree in sidebar order.
    pub async fn collection_tree(&self, collection_id: &str) -> Result<Vec<NavigationNode>, OutlineError> {
        let tree: InfoResponse<Vec<NavigationNode>> = self
            .post("collections.documents", &json!({"id": collection_id}))
            .await?;
        Ok(tree.data)
    }

    /// `documents.list` of a collection with one status (`published` or `archived`).
    pub async fn documents(
        &self,
        collection_id: &str,
        status: &str,
    ) -> Result<Vec<ApiDocument>, OutlineError> {
        self.list_all(
            "documents.list",
            json!({"collectionId": collection_id, "statusFilter": [status]}),
        )
        .await
    }

    /// `documents.deleted`: the trash.
    pub async fn deleted_documents(&self) -> Result<Vec<ApiDocument>, OutlineError> {
        self.list_all("documents.deleted", json!({})).await
    }

    /// `templates.list`; servers without it (before templates became their own model) list none.
    pub async fn templates(&self) -> Result<Vec<ApiDocument>, OutlineError> {
        match self.list_all("templates.list", json!({})).await {
            Err(OutlineError::NotFound | OutlineError::Api { .. }) => Ok(Vec::new()),
            other => other,
        }
    }

    /// `users.list` (emails reach admins only); an answer without access lists none.
    pub async fn users(&self) -> Result<Vec<ApiUser>, OutlineError> {
        match self.list_all("users.list", json!({})).await {
            Err(OutlineError::Unauthorized | OutlineError::Api { .. }) => Ok(Vec::new()),
            other => other,
        }
    }

    /// `documents.info`: one document with its content.
    pub async fn document(&self, id: &str) -> Result<ApiDocument, OutlineError> {
        let info: InfoResponse<ApiDocument> = self.post("documents.info", &json!({"id": id})).await?;
        Ok(info.data)
    }

    /// `templates.info`: one template with its content.
    pub async fn template(&self, id: &str) -> Result<ApiDocument, OutlineError> {
        let info: InfoResponse<ApiDocument> = self.post("templates.info", &json!({"id": id})).await?;
        Ok(info.data)
    }

    /// Downloads an attachment through `/api/attachments.redirect?id=…`, at most `max_bytes`.
    pub async fn attachment(&self, id: &str, max_bytes: u64) -> Result<Download, OutlineError> {
        let mut url = self.endpoint("attachments.redirect");
        url.query_pairs_mut().append_pair("id", id);
        let retry = self.inner.retry;
        let mut attempt = 0u32;
        let mut hops = 0usize;
        loop {
            attempt += 1;
            let same_origin = url.origin() == self.inner.base.origin();
            let mut request = self.inner.http.get(url.clone());
            if same_origin {
                request = request.header(header::AUTHORIZATION, self.inner.authorization.clone());
            }
            let response = match request.send().await {
                Ok(response) => response,
                Err(error) => {
                    if attempt >= 3 {
                        return Err(transport(&error));
                    }
                    tokio::time::sleep(backoff(retry, attempt)).await;
                    continue;
                }
            };
            let status = response.status();
            if status.is_redirection() {
                hops += 1;
                if hops > MAX_REDIRECTS {
                    return Err(OutlineError::Unreachable("too many redirects".to_owned()));
                }
                let location = response
                    .headers()
                    .get(header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| OutlineError::Decode("redirect without a location".to_owned()))?;
                let next = url
                    .join(location)
                    .map_err(|_| OutlineError::Decode("invalid redirect".to_owned()))?;
                if !matches!(next.scheme(), "https" | "http") {
                    return Err(OutlineError::Decode("invalid redirect".to_owned()));
                }
                // Plain http only to the base origin, or to loopback in development.
                let loopback = matches!(next.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
                if next.scheme() == "http"
                    && next.origin() != self.inner.base.origin()
                    && !(self.inner.allow_local && loopback)
                {
                    return Err(OutlineError::Decode("insecure redirect".to_owned()));
                }
                url = next;
                attempt = 0;
                continue;
            }
            if status == StatusCode::TOO_MANY_REQUESTS && attempt < retry.max_attempts {
                let wait = retry_after(response.headers())
                    .unwrap_or_else(|| backoff(retry, attempt))
                    .min(retry.max_delay);
                tokio::time::sleep(wait).await;
                continue;
            }
            if status.is_server_error() && attempt < 3 {
                tokio::time::sleep(backoff(retry, attempt)).await;
                continue;
            }
            return match status.as_u16() {
                200..=299 => {
                    let content_type = response
                        .headers()
                        .get(header::CONTENT_TYPE)
                        .and_then(|value| value.to_str().ok())
                        .map(|value| value.split(';').next().unwrap_or(value).trim().to_owned());
                    let file_name = response
                        .headers()
                        .get(header::CONTENT_DISPOSITION)
                        .and_then(|value| value.to_str().ok())
                        .and_then(disposition_name);
                    if response.content_length().is_some_and(|length| length > max_bytes) {
                        return Err(OutlineError::TooLarge { limit: max_bytes });
                    }
                    let bytes = limited_body(response, max_bytes).await?;
                    Ok(Download {
                        bytes,
                        content_type,
                        file_name,
                    })
                }
                401 | 403 => Err(OutlineError::Unauthorized),
                404 => Err(OutlineError::NotFound),
                429 => Err(OutlineError::RateLimited),
                code => Err(OutlineError::Api {
                    status: code,
                    message: String::new(),
                }),
            };
        }
    }
}

/// `filename="x.png"` from a `Content-Disposition` header.
fn disposition_name(value: &str) -> Option<String> {
    value.split(';').find_map(|part| {
        let part = part.trim();
        let name = part.strip_prefix("filename=")?;
        let name = name.trim_matches('"').trim();
        (!name.is_empty()).then(|| name.to_owned())
    })
}

/// Reads at most `limit` bytes of a body.
async fn limited_body(mut response: reqwest::Response, limit: u64) -> Result<Vec<u8>, OutlineError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| transport(&error))? {
        if bytes.len() as u64 + chunk.len() as u64 > limit {
            return Err(OutlineError::TooLarge { limit });
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// The error without the URL (it carries no token, but keeps messages short).
fn transport(error: &reqwest::Error) -> OutlineError {
    let kind = if error.is_timeout() {
        "timed out"
    } else if error.is_connect() {
        "connection failed"
    } else {
        "request failed"
    };
    OutlineError::Unreachable(kind.to_owned())
}

/// `Retry-After` in seconds (fractions allowed; an HTTP date is ignored).
fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get(header::RETRY_AFTER)?.to_str().ok()?.trim();
    let seconds: f64 = value.parse().ok()?;
    (seconds.is_finite() && seconds >= 0.0).then(|| Duration::from_secs_f64(seconds.min(3_600.0)))
}

/// `base · 2^(attempt-1)`, capped.
fn backoff(retry: RetryPolicy, attempt: u32) -> Duration {
    let factor = 1u32 << attempt.saturating_sub(1).min(10);
    retry.base_delay.saturating_mul(factor).min(retry.max_delay)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_public_https_base_urls() {
        assert!(parse_base_url("https://app.getoutline.com", false).is_ok());
        assert_eq!(
            parse_base_url("https://wiki.example.com/some/path?x=1", false)
                .unwrap()
                .as_str(),
            "https://wiki.example.com/"
        );
        assert!(parse_base_url("http://wiki.example.com", false).is_err());
        assert!(parse_base_url("https://127.0.0.1", false).is_err());
        assert!(parse_base_url("https://10.0.0.5", false).is_err());
        assert!(parse_base_url("https://user:pw@wiki.example.com", false).is_err());
        assert!(parse_base_url("ftp://wiki.example.com", false).is_err());
        assert!(parse_base_url("http://127.0.0.1:3000", true).is_ok());
    }

    #[test]
    fn parses_retry_after_and_backoff() {
        let mut headers = HeaderMap::new();
        headers.insert(header::RETRY_AFTER, HeaderValue::from_static("2"));
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(2)));
        headers.insert(header::RETRY_AFTER, HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT"));
        assert_eq!(retry_after(&headers), None);
        let retry = RetryPolicy::default();
        assert_eq!(backoff(retry, 1), Duration::from_secs(1));
        assert_eq!(backoff(retry, 3), Duration::from_secs(4));
        assert_eq!(backoff(retry, 20), Duration::from_secs(60));
    }

    #[test]
    fn debug_output_hides_the_token() {
        let client = OutlineClient::new(
            parse_base_url("https://wiki.example.com", false).unwrap(),
            "ol_api_secret_value",
            false,
            RetryPolicy::default(),
        )
        .unwrap();
        assert!(!format!("{client:?}").contains("secret"));
        assert!(check_token("bad\ntoken").is_err());
        assert_eq!(disposition_name("attachment; filename=\"a b.png\""), Some("a b.png".to_owned()));
    }
}
