//! AG-UI 1.0 events (the wire format between the Rust run loop and the panel, over SSE) and the
//! AG-UI messages of a conversation, which a reload hands back to the panel.

use serde_json::{Value, json};
use tokio::sync::mpsc;

/// Sends AG-UI events to the open SSE response. A send after the browser went away is dropped;
/// the run notices through its cancellation token.
#[derive(Clone)]
pub(crate) struct Emitter {
    out: mpsc::Sender<Value>,
}

impl Emitter {
    pub(crate) fn new(out: mpsc::Sender<Value>) -> Self {
        Self { out }
    }

    pub(crate) async fn emit(&self, event: Value) {
        let _ = self.out.send(event).await;
    }

    pub(crate) async fn run_started(&self, thread_id: &str, run_id: &str) {
        self.emit(json!({"type": "RUN_STARTED", "threadId": thread_id, "runId": run_id}))
            .await;
    }

    pub(crate) async fn run_finished(&self, thread_id: &str, run_id: &str, outcome: Option<Value>) {
        let mut event = json!({"type": "RUN_FINISHED", "threadId": thread_id, "runId": run_id});
        if let Some(outcome) = outcome {
            event["outcome"] = outcome;
        }
        self.emit(event).await;
    }

    pub(crate) async fn run_error(&self, message: &str, code: &str) {
        self.emit(json!({"type": "RUN_ERROR", "message": message, "code": code}))
            .await;
    }

    pub(crate) async fn text_start(&self, message_id: &str) {
        self.emit(
            json!({"type": "TEXT_MESSAGE_START", "messageId": message_id, "role": "assistant"}),
        )
        .await;
    }

    pub(crate) async fn text_content(&self, message_id: &str, delta: &str) {
        if delta.is_empty() {
            return;
        }
        self.emit(json!({"type": "TEXT_MESSAGE_CONTENT", "messageId": message_id, "delta": delta}))
            .await;
    }

    pub(crate) async fn text_end(&self, message_id: &str) {
        self.emit(json!({"type": "TEXT_MESSAGE_END", "messageId": message_id}))
            .await;
    }

    /// A whole tool call: start, its arguments, end.
    pub(crate) async fn tool_call(
        &self,
        message_id: &str,
        call_id: &str,
        name: &str,
        args: &Value,
    ) {
        self.emit(json!({
            "type": "TOOL_CALL_START", "toolCallId": call_id, "toolCallName": name,
            "parentMessageId": message_id,
        }))
        .await;
        self.emit(
            json!({"type": "TOOL_CALL_ARGS", "toolCallId": call_id, "delta": args.to_string()}),
        )
        .await;
        self.emit(json!({"type": "TOOL_CALL_END", "toolCallId": call_id}))
            .await;
    }

    pub(crate) async fn tool_result(&self, call_id: &str, content: &str) {
        self.emit(json!({
            "type": "TOOL_CALL_RESULT", "messageId": format!("tool-{call_id}"),
            "toolCallId": call_id, "content": content, "role": "tool",
        }))
        .await;
    }
}

/// The interrupt that pauses a run for the user's approval of one tool call.
pub(crate) fn approval_interrupt(
    interrupt_id: &str,
    call_id: &str,
    message: &str,
    expires_at_iso: &str,
    metadata: Value,
) -> Value {
    json!({
        "id": interrupt_id,
        "reason": "tool_call",
        "toolCallId": call_id,
        "message": message,
        "expiresAt": expires_at_iso,
        "responseSchema": {
            "type": "object",
            "properties": {"approved": {"type": "boolean"}, "reason": {"type": "string"}},
            "required": ["approved"],
        },
        "metadata": metadata,
    })
}

/// AG-UI transcript messages, as the panel's runtime rebuilds them after a reload.
pub(crate) fn user_message(id: &str, text: &str) -> Value {
    json!({"id": id, "role": "user", "content": text})
}

pub(crate) fn assistant_message(id: &str) -> Value {
    json!({"id": id, "role": "assistant", "content": "", "toolCalls": []})
}

pub(crate) fn tool_message(call_id: &str, content: &str) -> Value {
    json!({"id": format!("tool-{call_id}"), "role": "tool", "toolCallId": call_id, "content": content})
}

pub(crate) fn tool_call_entry(call_id: &str, name: &str, args: &Value) -> Value {
    json!({"id": call_id, "type": "function", "function": {"name": name, "arguments": args.to_string()}})
}
