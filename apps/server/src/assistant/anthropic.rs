//! The Anthropic Messages API (raw HTTP; there is no official Rust SDK): one streamed turn
//! (`stream: true`), with text deltas, `tool_use` blocks built from `input_json_delta`,
//! thinking blocks kept with their signature so they can be sent back unchanged, the stop
//! reason and the token usage. Also the model list, which the settings page's Test button uses
//! because it checks a key without spending tokens.
//!
//! The API key is only ever put in the `x-api-key` header. It is never logged, and no error
//! message built here contains it.

use std::time::Duration;

use reqwest::StatusCode;
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::model::{
    Block, Delta, Message, ModelProvider, ModelRequest, ModelTurn, ProviderError, Role, StopReason,
    Usage,
};

pub(crate) const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2026-10-21";
/// Server-side refusal fallbacks (`fallbacks: "default"`): when the safety system declines a
/// request on Claude Opus 5.5 or Sonnet 5.5, the API retries it on a fallback model in the same
/// call. Claude Haiku 5.5 has none.
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// How long the stream may stay silent before the turn fails (the API sends pings meanwhile).
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);
const RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(4)];
const PROVIDER: &str = "anthropic";

#[derive(Clone)]
pub(crate) struct AnthropicProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    /// Set when the requests go to an Anthropic-compatible gateway instead of Anthropic itself.
    gateway: Option<GatewayRequest>,
}

/// Where Ask AI's model requests go when a gateway (for example LiteLLM) sits in front of Anthropic:
/// `ORBIT_AI_ANTHROPIC_BASE_URL`, plus the gateway's own headers. Built once at startup.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Gateway {
    /// `ORBIT_AI_GATEWAY_HEADERS`: fixed headers sent with every request to the gateway.
    pub(crate) headers: Vec<(String, String)>,
    /// `ORBIT_AI_GATEWAY_USER_HEADER`: a header that carries the SHA-256 (hex) of the member's id,
    /// so the gateway can tell members apart without learning who they are.
    pub(crate) user_header: Option<String>,
}

/// The gateway headers of one member's requests.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct GatewayRequest {
    headers: Vec<(String, String)>,
}

impl Gateway {
    /// The headers for `user_id`'s requests (`None`: the models list of the Test button, which has no member hash).
    pub(crate) fn for_user(&self, user_id: Option<&str>) -> GatewayRequest {
        let mut headers = self.headers.clone();
        if let (Some(name), Some(user_id)) = (&self.user_header, user_id) {
            headers.push((name.clone(), user_hash(user_id)));
        }
        GatewayRequest { headers }
    }
}

/// The SHA-256 of a member's id, as 64 lower-case hex digits.
pub(crate) fn user_hash(user_id: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(user_id.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

impl std::fmt::Debug for AnthropicProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnthropicProvider")
            .field("base_url", &self.base_url)
            .field("api_key", &"[REDACTED]")
            .field("gateway", &self.gateway.is_some())
            .finish()
    }
}

impl AnthropicProvider {
    pub(crate) fn new(client: reqwest::Client, base_url: &str, api_key: String) -> Self {
        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key,
            gateway: None,
        }
    }

    /// Sends the requests through a gateway: with its headers, and without Anthropic's server-side
    /// refusal fallbacks (a gateway such as LiteLLM reads a `fallbacks` field as its own setting).
    pub(crate) fn with_gateway(mut self, gateway: Option<GatewayRequest>) -> Self {
        self.gateway = gateway;
        self
    }

    fn gateway_headers(&self, mut request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(gateway) = &self.gateway {
            for (name, value) in &gateway.headers {
                request = request.header(name.as_str(), value.as_str());
            }
        }
        request
    }

    /// The models the key may use (`GET /v1/models`): a free request, so it doubles as the key test.
    pub(crate) async fn models(&self) -> Result<Vec<String>, ProviderError> {
        let response = self
            .gateway_headers(
                self.client
                    .get(format!("{}/v1/models?limit=100", self.base_url)),
            )
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(|error| ProviderError::Network(network_detail(&error)))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| ProviderError::Network(network_detail(&error)))?;
        if !status.is_success() {
            return Err(status_error(status, &body));
        }
        let value: Value = serde_json::from_str(&body)
            .map_err(|_| ProviderError::Protocol("the model list is not JSON".into()))?;
        Ok(value["data"]
            .as_array()
            .map(|models| {
                models
                    .iter()
                    .filter_map(|model| model["id"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn send(
        &self,
        body: &Value,
        fallbacks: bool,
        cancel: &CancellationToken,
    ) -> Result<reqwest::Response, ProviderError> {
        let mut attempt = 0;
        loop {
            let mut request = self
                .gateway_headers(self.client.post(format!("{}/v1/messages", self.base_url)))
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", API_VERSION)
                .header("content-type", "application/json");
            let mut body = body.clone();
            if fallbacks {
                request = request.header("anthropic-beta", FALLBACK_BETA);
                body["fallbacks"] = json!("default");
            }
            let sent = tokio::select! {
                () = cancel.cancelled() => return Err(ProviderError::Cancelled),
                sent = request.json(&body).send() => sent,
            };
            let retry_after = match sent {
                Ok(response) if response.status().is_success() => return Ok(response),
                Ok(response) => {
                    let status = response.status();
                    let retry_after = response
                        .headers()
                        .get("retry-after")
                        .and_then(|value| value.to_str().ok())
                        .and_then(|value| value.parse::<u64>().ok());
                    let text = response.text().await.unwrap_or_default();
                    let error = status_error(status, &text);
                    if !retryable(status) || attempt >= RETRY_DELAYS.len() {
                        return Err(error);
                    }
                    retry_after
                }
                Err(error) => {
                    if attempt >= RETRY_DELAYS.len() {
                        return Err(ProviderError::Network(network_detail(&error)));
                    }
                    None
                }
            };
            let delay = retry_after
                .map(|seconds| Duration::from_secs(seconds.min(15)))
                .unwrap_or(RETRY_DELAYS[attempt]);
            attempt += 1;
            tokio::select! {
                () = cancel.cancelled() => return Err(ProviderError::Cancelled),
                () = tokio::time::sleep(delay) => {}
            }
        }
    }
}

impl ModelProvider for AnthropicProvider {
    async fn turn(
        &self,
        request: &ModelRequest,
        deltas: mpsc::UnboundedSender<Delta>,
        cancel: CancellationToken,
    ) -> Result<ModelTurn, ProviderError> {
        let body = request_body(request);
        let fallbacks = self.gateway.is_none() && supports_fallbacks(&request.model);
        let response = match self.send(&body, fallbacks, &cancel).await {
            // An account without the fallback beta: the same request without it.
            Err(ProviderError::BadRequest(detail))
                if fallbacks && detail.to_ascii_lowercase().contains("fallback") =>
            {
                self.send(&body, false, &cancel).await?
            }
            other => other?,
        };
        read_stream(response, &deltas, &cancel, &request.model).await
    }
}

fn supports_fallbacks(model: &str) -> bool {
    model.starts_with("claude-opus-5-5") || model.starts_with("claude-sonnet-5-5")
}

fn retryable(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() || status.as_u16() == 529
}

fn network_detail(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "timed out".into()
    } else if error.is_connect() {
        "could not connect".into()
    } else {
        "connection failed".into()
    }
}

/// The API's error message, shortened. It never contains the key.
fn error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| value["error"]["message"].as_str().map(str::to_owned))
        .map(|message| message.chars().take(300).collect())
        .unwrap_or_default()
}

fn status_error(status: StatusCode, body: &str) -> ProviderError {
    let message = error_message(body);
    match status.as_u16() {
        401 => ProviderError::InvalidKey,
        403 => ProviderError::Forbidden(message),
        429 => ProviderError::RateLimited,
        400 | 404 | 413 | 422 => ProviderError::BadRequest(if message.is_empty() {
            format!("HTTP {}", status.as_u16())
        } else {
            message
        }),
        _ => ProviderError::Unavailable(format!("HTTP {}", status.as_u16())),
    }
}

/// The request body. The system prompt carries a cache breakpoint (it covers the tools too), and
/// so does the last block of the conversation, so each step reads the earlier ones from cache.
pub(crate) fn request_body(request: &ModelRequest) -> Value {
    let mut messages: Vec<Value> = request.messages.iter().filter_map(wire_message).collect();
    if let Some(last) = messages
        .last_mut()
        .and_then(|message| message["content"].as_array_mut())
        .and_then(|content| content.last_mut())
        && matches!(last["type"].as_str(), Some("text" | "tool_result"))
    {
        last["cache_control"] = json!({"type": "ephemeral"});
    }
    let tools: Vec<Value> = request
        .tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": tool.input_schema,
                // Arguments stream as they are written; the run loop validates them.
                "eager_input_streaming": true,
            })
        })
        .collect();
    json!({
        "model": request.model,
        "max_tokens": request.max_tokens,
        "stream": true,
        "system": [{"type": "text", "text": request.system, "cache_control": {"type": "ephemeral"}}],
        "messages": messages,
        "tools": tools,
        // Adaptive thinking is on by default on the offered models; `medium` suits multistep tool use.
        "output_config": {"effort": "medium"},
    })
}

fn wire_message(message: &Message) -> Option<Value> {
    let content: Vec<Value> = message.content.iter().filter_map(wire_block).collect();
    if content.is_empty() {
        return None;
    }
    let role = match message.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    Some(json!({"role": role, "content": content}))
}

fn wire_block(block: &Block) -> Option<Value> {
    Some(match block {
        Block::Text { text } if text.is_empty() => return None,
        Block::Text { text } => json!({"type": "text", "text": text}),
        Block::ToolUse {
            id, name, input, ..
        } => json!({
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": input.clone().unwrap_or_else(|| json!({})),
        }),
        Block::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => json!({
            "type": "tool_result",
            "tool_use_id": tool_use_id,
            "content": content,
            "is_error": is_error,
        }),
        Block::Opaque { provider, value } if provider == PROVIDER => value.clone(),
        Block::Opaque { .. } => return None,
    })
}

enum Builder {
    Text(String),
    Tool {
        id: String,
        name: String,
        json: String,
    },
    Thinking {
        start: Value,
        thinking: String,
        signature: String,
    },
    /// A block passed back as it arrived: `redacted_thinking`, `fallback`, or a type added later.
    Raw(Value),
}

impl Builder {
    fn start(block: &Value) -> Self {
        match block["type"].as_str() {
            Some("text") => Self::Text(block["text"].as_str().unwrap_or_default().to_owned()),
            Some("tool_use") => Self::Tool {
                id: block["id"].as_str().unwrap_or_default().to_owned(),
                name: block["name"].as_str().unwrap_or_default().to_owned(),
                json: String::new(),
            },
            Some("thinking") => Self::Thinking {
                start: block.clone(),
                thinking: block["thinking"].as_str().unwrap_or_default().to_owned(),
                signature: block["signature"].as_str().unwrap_or_default().to_owned(),
            },
            _ => Self::Raw(block.clone()),
        }
    }

    fn finish(self) -> Block {
        match self {
            Self::Text(text) => Block::Text { text },
            Self::Tool { id, name, json } => {
                let input = if json.trim().is_empty() {
                    Some(Value::Object(Map::new()))
                } else {
                    serde_json::from_str::<Value>(&json)
                        .ok()
                        .filter(Value::is_object)
                };
                let raw = if input.is_none() {
                    json.chars().take(2000).collect()
                } else {
                    String::new()
                };
                Block::ToolUse {
                    id,
                    name,
                    input,
                    raw,
                }
            }
            Self::Thinking {
                mut start,
                thinking,
                signature,
            } => {
                start["thinking"] = json!(thinking);
                start["signature"] = json!(signature);
                Block::Opaque {
                    provider: PROVIDER.into(),
                    value: start,
                }
            }
            Self::Raw(value) => Block::Opaque {
                provider: PROVIDER.into(),
                value,
            },
        }
    }
}

/// Parses the event stream into a turn.
#[derive(Default)]
pub(crate) struct StreamState {
    blocks: Vec<(u64, Builder)>,
    stop: Option<StopReason>,
    usage: Usage,
    model: Option<String>,
    finished: bool,
}

impl StreamState {
    /// Handles one SSE `data` payload; returns the text to show, if any.
    pub(crate) fn handle(&mut self, data: &str) -> Result<Option<String>, ProviderError> {
        let event: Value = serde_json::from_str(data)
            .map_err(|_| ProviderError::Protocol("an event is not JSON".into()))?;
        match event["type"].as_str().unwrap_or_default() {
            "message_start" => {
                let message = &event["message"];
                if let Some(model) = message["model"].as_str() {
                    self.model = Some(model.to_owned());
                }
                self.add_usage(&message["usage"]);
            }
            "content_block_start" => {
                let index = event["index"].as_u64().unwrap_or_default();
                let builder = Builder::start(&event["content_block"]);
                let text = match &builder {
                    Builder::Text(text) if !text.is_empty() => Some(text.clone()),
                    _ => None,
                };
                self.blocks.push((index, builder));
                return Ok(text);
            }
            "content_block_delta" => {
                let index = event["index"].as_u64().unwrap_or_default();
                let delta = &event["delta"];
                let Some((_, builder)) = self.blocks.iter_mut().rev().find(|(i, _)| *i == index)
                else {
                    return Err(ProviderError::Protocol(
                        "a delta for an unknown block".into(),
                    ));
                };
                match (builder, delta["type"].as_str().unwrap_or_default()) {
                    (Builder::Text(text), "text_delta") => {
                        let piece = delta["text"].as_str().unwrap_or_default();
                        text.push_str(piece);
                        return Ok((!piece.is_empty()).then(|| piece.to_owned()));
                    }
                    (Builder::Tool { json, .. }, "input_json_delta") => {
                        json.push_str(delta["partial_json"].as_str().unwrap_or_default());
                    }
                    (Builder::Thinking { thinking, .. }, "thinking_delta") => {
                        thinking.push_str(delta["thinking"].as_str().unwrap_or_default());
                    }
                    (Builder::Thinking { signature, .. }, "signature_delta") => {
                        signature.push_str(delta["signature"].as_str().unwrap_or_default());
                    }
                    // citations and anything newer: not used here
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(reason) = event["delta"]["stop_reason"].as_str() {
                    self.stop = Some(match reason {
                        "end_turn" => StopReason::EndTurn,
                        "tool_use" => StopReason::ToolUse,
                        "max_tokens" => StopReason::MaxTokens,
                        "refusal" => StopReason::Refusal(
                            event["delta"]["stop_details"]["category"]
                                .as_str()
                                .map(str::to_owned),
                        ),
                        other => StopReason::Other(other.to_owned()),
                    });
                }
                self.add_usage(&event["usage"]);
            }
            "message_stop" => self.finished = true,
            "error" => {
                let error = &event["error"];
                let message: String = error["message"]
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .take(300)
                    .collect();
                return Err(match error["type"].as_str().unwrap_or_default() {
                    "overloaded_error" | "api_error" => ProviderError::Unavailable(message),
                    "rate_limit_error" => ProviderError::RateLimited,
                    "authentication_error" => ProviderError::InvalidKey,
                    "invalid_request_error" => ProviderError::BadRequest(message),
                    _ => ProviderError::Protocol(message),
                });
            }
            _ => {} // ping, and event types added later
        }
        Ok(None)
    }

    fn add_usage(&mut self, usage: &Value) {
        let read = |key: &str| usage[key].as_u64();
        if let Some(value) = read("input_tokens") {
            self.usage.input_tokens = value;
        }
        if let Some(value) = read("output_tokens") {
            self.usage.output_tokens = value;
        }
        if let Some(value) = read("cache_read_input_tokens") {
            self.usage.cache_read_tokens = value;
        }
        if let Some(value) = read("cache_creation_input_tokens") {
            self.usage.cache_write_tokens = value;
        }
    }

    pub(crate) fn finish(self, requested_model: &str) -> Result<ModelTurn, ProviderError> {
        if !self.finished {
            return Err(ProviderError::Protocol("the answer ended early".into()));
        }
        let mut blocks = self.blocks;
        blocks.sort_by_key(|(index, _)| *index);
        let mut content: Vec<Block> = blocks
            .into_iter()
            .map(|(_, builder)| builder.finish())
            .filter(|block| !matches!(block, Block::Text { text } if text.is_empty()))
            .collect();
        // After a fallback mid-answer, the declined model's thinking and tool calls before the
        // last `fallback` block are dropped (never run, never sent back); its text stays.
        if let Some(boundary) = content.iter().rposition(is_fallback) {
            let mut kept = Vec::with_capacity(content.len());
            for (position, block) in content.into_iter().enumerate() {
                if position >= boundary || matches!(block, Block::Text { .. }) {
                    kept.push(block);
                }
            }
            content = kept;
        }
        Ok(ModelTurn {
            content,
            stop: self.stop.unwrap_or(StopReason::Other("missing".into())),
            usage: self.usage,
            model: self.model.unwrap_or_else(|| requested_model.to_owned()),
        })
    }
}

fn is_fallback(block: &Block) -> bool {
    matches!(block, Block::Opaque { value, .. } if value["type"] == "fallback")
}

/// Splits an SSE byte buffer into complete events and returns their `data` payloads.
pub(crate) fn drain_events(buffer: &mut String) -> Vec<String> {
    let mut events = Vec::new();
    loop {
        let normalized = buffer.find("\r\n").is_some();
        if normalized {
            *buffer = buffer.replace("\r\n", "\n");
        }
        let Some(end) = buffer.find("\n\n") else {
            break;
        };
        let event: String = buffer.drain(..end + 2).collect();
        let data: Vec<&str> = event
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(|line| line.strip_prefix(' ').unwrap_or(line))
            .collect();
        if !data.is_empty() {
            events.push(data.join("\n"));
        }
    }
    events
}

async fn read_stream(
    mut response: reqwest::Response,
    deltas: &mpsc::UnboundedSender<Delta>,
    cancel: &CancellationToken,
    requested_model: &str,
) -> Result<ModelTurn, ProviderError> {
    let mut state = StreamState::default();
    let mut bytes: Vec<u8> = Vec::new();
    let mut text = String::new();
    loop {
        let chunk = tokio::select! {
            () = cancel.cancelled() => return Err(ProviderError::Cancelled),
            chunk = tokio::time::timeout(IDLE_TIMEOUT, response.chunk()) => chunk,
        };
        let chunk = match chunk {
            Err(_) => return Err(ProviderError::Network("timed out".into())),
            Ok(Err(error)) => return Err(ProviderError::Network(network_detail(&error))),
            Ok(Ok(None)) => break,
            Ok(Ok(Some(chunk))) => chunk,
        };
        bytes.extend_from_slice(&chunk);
        // Only whole UTF-8 characters move on to the text buffer.
        let valid = match std::str::from_utf8(&bytes) {
            Ok(valid) => valid.len(),
            Err(error) => error.valid_up_to(),
        };
        text.push_str(std::str::from_utf8(&bytes[..valid]).unwrap_or_default());
        bytes.drain(..valid);
        for data in drain_events(&mut text) {
            let before = state.usage;
            let piece = state.handle(&data)?;
            if state.usage != before {
                let _ = deltas.send(Delta::Usage(state.usage));
            }
            if let Some(piece) = piece {
                let _ = deltas.send(Delta::Text(piece));
            }
            if state.finished {
                return state.finish(requested_model);
            }
        }
    }
    state.finish(requested_model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_split_on_blank_lines_and_crlf() {
        let mut buffer = String::from(
            "event: ping\ndata: {\"type\":\"ping\"}\n\nevent: x\r\ndata: {\"a\":1}\r\n\r\ndata: partial",
        );
        let events = drain_events(&mut buffer);
        assert_eq!(events, vec!["{\"type\":\"ping\"}", "{\"a\":1}"]);
        assert_eq!(buffer, "data: partial");
    }

    #[test]
    fn a_stream_builds_text_tool_calls_and_thinking() {
        let mut state = StreamState::default();
        let events = [
            json!({"type":"message_start","message":{"model":"claude-sonnet-5-5","usage":{"input_tokens":10,"cache_read_input_tokens":4}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hi"}}),
            json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"t1","name":"list_projects","input":{}}}),
            json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"a\":"}}),
            json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"1}"}}),
            json!({"type":"content_block_start","index":3,"content_block":{"type":"tool_use","id":"t2","name":"get_task","input":{}}}),
            json!({"type":"content_block_delta","index":3,"delta":{"type":"input_json_delta","partial_json":"{\"task_id\": \"x"}}),
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}),
            json!({"type":"message_stop"}),
        ];
        let mut shown = String::new();
        for event in events {
            if let Some(text) = state.handle(&event.to_string()).unwrap() {
                shown.push_str(&text);
            }
        }
        assert_eq!(shown, "Hi");
        let turn = state.finish("claude-sonnet-5-5").unwrap();
        assert_eq!(turn.stop, StopReason::ToolUse);
        assert_eq!(turn.usage.input_tokens, 10);
        assert_eq!(turn.usage.cache_read_tokens, 4);
        assert_eq!(turn.usage.output_tokens, 7);
        assert_eq!(turn.content.len(), 4);
        assert!(
            matches!(&turn.content[0], Block::Opaque { value, .. } if value["signature"] == "sig")
        );
        assert!(
            matches!(&turn.content[2], Block::ToolUse { input: Some(input), .. } if input["a"] == 1)
        );
        // cut-off arguments: no input, the raw text kept for the model
        assert!(
            matches!(&turn.content[3], Block::ToolUse { input: None, raw, .. } if raw.starts_with("{\"task_id\""))
        );
    }

    #[test]
    fn a_fallback_drops_the_declined_thinking_and_tool_calls() {
        let mut state = StreamState::default();
        for event in [
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":"Part"}}),
            json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"t","name":"x","input":{}}}),
            json!({"type":"content_block_start","index":3,"content_block":{"type":"fallback","from":{"model":"a"},"to":{"model":"b"}}}),
            json!({"type":"content_block_start","index":4,"content_block":{"type":"text","text":"Rest"}}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{}}),
            json!({"type":"message_stop"}),
        ] {
            state.handle(&event.to_string()).unwrap();
        }
        let turn = state.finish("m").unwrap();
        assert_eq!(turn.content.len(), 3);
        assert!(matches!(&turn.content[0], Block::Text { text } if text == "Part"));
        assert!(is_fallback(&turn.content[1]));
    }

    #[test]
    fn an_unfinished_stream_and_an_error_event_fail() {
        let state = StreamState::default();
        assert!(matches!(state.finish("m"), Err(ProviderError::Protocol(_))));
        let mut state = StreamState::default();
        let error = state
            .handle(
                r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
            )
            .unwrap_err();
        assert_eq!(error, ProviderError::Unavailable("Overloaded".into()));
    }

    #[test]
    fn the_request_caches_the_system_prompt_and_the_last_block() {
        let request = ModelRequest {
            model: "claude-sonnet-5-5".into(),
            system: "sys".into(),
            messages: vec![
                Message::user(vec![Block::Text { text: "hi".into() }]),
                Message {
                    role: Role::Assistant,
                    content: vec![
                        Block::Opaque {
                            provider: "openai".into(),
                            value: json!({"x": 1}),
                        },
                        Block::Text {
                            text: String::new(),
                        },
                        Block::ToolUse {
                            id: "t".into(),
                            name: "get_task".into(),
                            input: None,
                            raw: "{".into(),
                        },
                    ],
                },
                Message::user(vec![Block::ToolResult {
                    tool_use_id: "t".into(),
                    content: "bad".into(),
                    is_error: true,
                }]),
            ],
            tools: vec![],
            max_tokens: 100,
        };
        let body = request_body(&request);
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        let assistant = &body["messages"][1]["content"];
        // another provider's block and an empty text are left out; broken input goes as {}
        assert_eq!(assistant.as_array().unwrap().len(), 1);
        assert_eq!(assistant[0]["input"], json!({}));
        assert_eq!(
            body["messages"][2]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        assert!(body.get("temperature").is_none());
    }
}
