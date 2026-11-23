//! Provider-neutral model types: the transcript Ask AI keeps, the request it sends, and the
//! answer a provider streams back. A provider (`anthropic.rs`; an OpenAI-compatible one can
//! follow) maps these to its wire format, so the run loop never sees a provider's JSON.

use std::future::Future;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Role {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Block {
    Text {
        text: String,
    },
    /// `input` is `None` when the model sent arguments that are not a JSON object (cut off or
    /// malformed); `raw` keeps what arrived so the model can be told.
    ToolUse {
        id: String,
        name: String,
        input: Option<Value>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        raw: String,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
    /// A provider's own block, sent back unchanged on the next request (a thinking block with
    /// its signature, a fallback marker). Never shown to the user.
    Opaque {
        provider: String,
        value: Value,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Message {
    pub role: Role,
    pub content: Vec<Block>,
}

impl Message {
    pub(crate) fn user(content: Vec<Block>) -> Self {
        Self {
            role: Role::User,
            content,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Clone, Debug)]
pub(crate) struct ModelRequest {
    pub model: String,
    /// Stable for the whole conversation, so the provider can cache it.
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub max_tokens: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    /// The provider's safety system declined; the category when it gave one.
    Refusal(Option<String>),
    /// Anything else (`pause_turn`, `stop_sequence`, a value added later).
    Other(String),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

/// One finished model turn.
#[derive(Clone, Debug)]
pub(crate) struct ModelTurn {
    pub content: Vec<Block>,
    pub stop: StopReason,
    pub usage: Usage,
    /// The model that answered (a fallback model when the provider switched).
    pub model: String,
}

/// What the run loop shows while a turn streams.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Delta {
    Text(String),
    /// The token usage so far (from the first event on): what a stopped or broken turn still cost.
    Usage(Usage),
}

impl ModelRequest {
    /// A rough input size in tokens (four bytes a token), reserved against the daily limit before the request.
    pub(crate) fn estimated_input_tokens(&self) -> u64 {
        let messages = serde_json::to_string(&self.messages).map_or(0, |text| text.len());
        let tools: usize = self
            .tools
            .iter()
            .map(|tool| {
                tool.name.len() + tool.description.len() + tool.input_schema.to_string().len()
            })
            .sum();
        u64::try_from((messages + tools + self.system.len()) / 4).unwrap_or(u64::MAX)
    }
}

impl ProviderError {
    /// Whether the provider may have billed the request although no usage arrived (it was cut off after it was
    /// sent). A refusal by status (bad key, rate limit, bad request) is not billed.
    pub(crate) fn may_be_billed(&self) -> bool {
        matches!(self, Self::Cancelled | Self::Network(_) | Self::Protocol(_))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum ProviderError {
    #[error("the API key was refused")]
    InvalidKey,
    #[error("the API key may not use this model or feature")]
    Forbidden(String),
    #[error("the provider is rate limiting this key")]
    RateLimited,
    #[error("the provider is overloaded or failing")]
    Unavailable(String),
    #[error("the provider refused the request: {0}")]
    BadRequest(String),
    #[error("the provider could not be reached")]
    Network(String),
    #[error("the provider sent an answer that could not be read: {0}")]
    Protocol(String),
    #[error("stopped")]
    Cancelled,
}

/// A model provider. `turn` streams one model turn: text deltas go to `deltas` while it runs,
/// and the finished turn comes back. It stops promptly when `cancel` fires.
pub(crate) trait ModelProvider: Send + Sync {
    fn turn(
        &self,
        request: &ModelRequest,
        deltas: mpsc::UnboundedSender<Delta>,
        cancel: CancellationToken,
    ) -> impl Future<Output = Result<ModelTurn, ProviderError>> + Send;
}
