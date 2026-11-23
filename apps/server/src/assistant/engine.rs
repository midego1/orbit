//! The run loop of one user message: model turns, tool calls, approvals, limits and stopping.
//!
//! - Reads run at once. Every write pauses the run with an approval card: the call (name and
//!   exact arguments) is stored server-side in the conversation, and the client only answers
//!   approve or decline for that interrupt id. A write tool the user allowed "for this
//!   conversation" runs without asking; dangerous tools always ask. Approvals expire.
//! - Per user message: at most `max_model_turns` model turns and `max_tool_calls` tool calls; per
//!   run a wall-clock limit. The run stops on Stop or when the browser goes away (`cancel`).
//! - Tool results reach the model wrapped as untrusted data (`prompt::wrap_untrusted`).
//! - When the model says it did something without a change having been made, it is asked once
//!   to correct itself.

use std::collections::VecDeque;
use std::time::Duration;

use futures_util::future::BoxFuture;
use orbit_platform::{Id, TimestampMillis};
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::agui::{self, Emitter};
use super::model::{
    Block, Delta, Message, ModelProvider, ModelRequest, ProviderError, Role, StopReason, Usage,
};
use super::prompt;
use super::registry::{self, LOAD_TOOLS, OPEN_PAGE, Registry, Risk};
pub(crate) use super::settings::Gate;
use super::store::{ConversationState, Pending, TurnState};

/// Runs a tool as the signed-in user (the MCP tools in-process, `crate::mcp::TaskTools`).
pub(crate) trait ToolRunner: Send + Sync {
    fn call<'a>(&'a self, name: &'a str, input: Value) -> BoxFuture<'a, Result<Value, String>>;
}

/// What a run needs from the database, behind a trait so the loop can be tested on its own.
pub(crate) trait RunHooks: Send + Sync {
    /// Checks the workspace's daily token limit and counts one model request against the user's rate limit.
    fn take_request(&self) -> BoxFuture<'_, Gate>;
    /// Reserves `estimate` against the workspace's daily limit before a model request (so requests in flight count),
    /// returning the reservation; `None` when it could not be stored (then no request goes out).
    fn reserve_usage<'a>(&'a self, model: &'a str, estimate: Usage) -> BoxFuture<'a, Option<Id>>;
    /// Replaces a reservation with what the request used.
    fn settle_usage<'a>(
        &'a self,
        reservation: Id,
        model: &'a str,
        usage: Usage,
    ) -> BoxFuture<'a, ()>;
    /// An audit row for a change the assistant made, declined or failed.
    fn audit_change<'a>(&'a self, tool: &'a str, outcome: &'a str) -> BoxFuture<'a, ()>;
    /// Saves the conversation in the middle of a run; `false` when it was not saved (then no change may run).
    fn checkpoint<'a>(&'a self, state: &'a ConversationState) -> BoxFuture<'a, bool>;
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub max_model_turns: u32,
    pub max_tool_calls: u32,
    pub run_time: Duration,
    pub approval_ttl_ms: i64,
    pub max_tokens: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_model_turns: 12,
            max_tool_calls: 20,
            run_time: Duration::from_secs(240),
            approval_ttl_ms: 15 * 60 * 1000,
            // Room for adaptive thinking plus an answer; a chat answer is far shorter.
            max_tokens: 16_000,
        }
    }
}

pub(crate) enum Input {
    Message {
        text: String,
        page: Option<String>,
    },
    Resume {
        interrupt_id: String,
        approved: bool,
        /// "Allow this action for this conversation" (write tools only).
        allow_for_conversation: bool,
    },
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Outcome {
    Finished,
    /// Waits for the user's approval.
    Paused,
    Cancelled,
    /// The request did not fit the conversation (a stale approval, nothing to resume).
    Rejected,
}

enum ToolsOutcome {
    Done(Vec<Block>),
    Paused,
    Cancelled(Vec<Block>),
}

/// What a tool call returns to the model and to the panel.
struct CallResult {
    model: String,
    is_error: bool,
    client: String,
}

impl CallResult {
    fn error(message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            client: json!({"error": message}).to_string(),
            model: message,
            is_error: true,
        }
    }
}

const DECLINED: &str = "The user declined this action, so it was NOT performed. Do not try it again and do not try \
another way to get the same result; ask the user what they want instead.";
const EXPIRED: &str = "The approval request expired before the user answered, so the action was NOT performed. Do not \
retry it unless the user asks again.";
const MOVED_ON: &str = "The user sent a new message instead of answering the approval, so this action was NOT \
performed.";
const NOT_RUN: &str =
    "Not run: an earlier action in the same step waited for the user, who moved on.";
const STOPPED: &str = "Not run: the user stopped the run.";
const UNCERTAIN: &str = "The server stopped while this approved change ran, so it may or may not have been made. Read \
the item to check before you do anything else; do not repeat the change without asking the user.";
const MODEL_RESULT_LIMIT: usize = 20_000;
const CLIENT_RESULT_LIMIT: usize = 4_000;

pub(crate) struct Engine<'a, P, T, H> {
    pub provider: &'a P,
    pub tools: &'a T,
    pub hooks: &'a H,
    pub registry: &'a Registry,
    pub model: &'a str,
    pub system: &'a str,
    pub workspace_id: Id,
    pub thread_id: &'a str,
    pub run_id: &'a str,
    pub out: &'a Emitter,
    pub cancel: CancellationToken,
    pub limits: Limits,
    /// The panel's Follow along switch for this run: whether the pages Ask AI opens are shown to the user.
    pub follow_along: bool,
}

impl<P: ModelProvider, T: ToolRunner, H: RunHooks> Engine<'_, P, T, H> {
    pub(crate) async fn run(&self, state: &mut ConversationState, input: Input) -> Outcome {
        self.out.run_started(self.thread_id, self.run_id).await;
        let deadline = Instant::now() + self.limits.run_time;
        match input {
            Input::Message { text, page } => self.start_message(state, &text, page.as_deref()),
            Input::Resume {
                interrupt_id,
                approved,
                allow_for_conversation,
            } => {
                match self
                    .resume(state, &interrupt_id, approved, allow_for_conversation)
                    .await
                {
                    Ok(ToolsOutcome::Done(results)) => {
                        state.transcript.push(Message::user(results));
                        self.hooks.checkpoint(state).await;
                    }
                    Ok(ToolsOutcome::Paused) => return Outcome::Paused,
                    Ok(ToolsOutcome::Cancelled(results)) => {
                        state.transcript.push(Message::user(results));
                        return Outcome::Cancelled;
                    }
                    Err(outcome) => return outcome,
                }
            }
        }
        self.model_loop(state, deadline).await
    }

    fn start_message(&self, state: &mut ConversationState, text: &str, page: Option<&str>) {
        let mut content = Vec::new();
        // A call still waiting for approval is answered as "moved on", with the rest of its turn.
        if let Some(pending) = state.pending.take() {
            content.extend(pending.results);
            content.push(Block::ToolResult {
                tool_use_id: pending.tool_use_id.clone(),
                content: MOVED_ON.into(),
                is_error: false,
            });
            state.display.push(agui::tool_message(
                &pending.tool_use_id,
                &json!({"skipped": true}).to_string(),
            ));
            for block in pending.queue {
                if let Block::ToolUse { id, .. } = block {
                    content.push(Block::ToolResult {
                        tool_use_id: id,
                        content: NOT_RUN.into(),
                        is_error: false,
                    });
                }
            }
        }
        if let Some(context) = page.and_then(prompt::page_context) {
            content.push(Block::Text {
                text: format!("<page-context>{context}</page-context>"),
            });
        }
        content.push(Block::Text {
            text: text.to_owned(),
        });
        state.transcript.push(Message::user(content));
        state
            .display
            .push(agui::user_message(&format!("user-{}", Id::new_v7()), text));
        state.turn = TurnState::default();
    }

    async fn resume(
        &self,
        state: &mut ConversationState,
        interrupt_id: &str,
        approved: bool,
        allow_for_conversation: bool,
    ) -> Result<ToolsOutcome, Outcome> {
        let Some(pending) = state.pending.take() else {
            self.out
                .run_error(
                    "Nothing is waiting for your approval anymore.",
                    "no_pending_approval",
                )
                .await;
            return Err(Outcome::Rejected);
        };
        if pending.interrupt_id != interrupt_id {
            state.pending = Some(pending);
            self.out
                .run_error(
                    "This approval is no longer current; reload the conversation.",
                    "stale_approval",
                )
                .await;
            return Err(Outcome::Rejected);
        }
        let Pending {
            tool_use_id,
            name,
            input,
            risk,
            expires_at,
            queue,
            mut results,
            ..
        } = pending;
        // What this run shows (the page it opens, the calls after the approved one) goes in a message of its
        // own: the panel does not add parts to a message of an earlier run.
        let message_id = format!("msg-{}", Id::new_v7());
        state.display.push(agui::assistant_message(&message_id));
        let nonce = state.nonce.clone();
        let result = if TimestampMillis::now().as_millis() > expires_at {
            CallResult {
                model: EXPIRED.into(),
                is_error: false,
                client: json!({"expired": true}).to_string(),
            }
        } else if !approved {
            self.hooks.audit_change(&name, "declined").await;
            CallResult {
                model: DECLINED.into(),
                is_error: false,
                client: json!({"declined": true}).to_string(),
            }
        } else {
            if allow_for_conversation && risk == Risk::Write && !self.free_text(&name) {
                state.allowed_tools.insert(name.clone());
            }
            self.run_change(state, &message_id, &tool_use_id, &name, input, &nonce, true)
                .await
        };
        results.push(self.deliver(state, &tool_use_id, result).await);
        let outcome = self
            .run_tools(state, &message_id, queue.into(), results, false)
            .await;
        // nothing was shown in it (a decline): no empty message on reload
        if let Some(position) = state.display.iter().position(|message| {
            message["id"] == message_id.as_str()
                && message["toolCalls"].as_array().is_none_or(Vec::is_empty)
        }) {
            state.display.remove(position);
        }
        Ok(outcome)
    }

    async fn model_loop(&self, state: &mut ConversationState, deadline: Instant) -> Outcome {
        loop {
            if self.cancel.is_cancelled() {
                return Outcome::Cancelled;
            }
            if state.turn.model_turns >= self.limits.max_model_turns {
                self.say(
                    state,
                    &format!(
                        "I stopped here: one message may take at most {} steps. Tell me to continue if you want me to go on.",
                        self.limits.max_model_turns
                    ),
                )
                .await;
                return self.finish().await;
            }
            if Instant::now() >= deadline {
                self.say(state, "I stopped here: this took longer than the time limit. Tell me to continue if you want me to go on.")
                    .await;
                return self.finish().await;
            }
            let refusal = match self.hooks.take_request().await {
                Gate::Open => None,
                Gate::RateLimited => Some(
                    "You are sending requests faster than this workspace allows. Wait a minute and try again.",
                ),
                Gate::DailyCap => Some(
                    "This workspace has used its Ask AI tokens for today. A workspace owner or admin can raise the daily limit in Settings → AI.",
                ),
                Gate::Unavailable => Some(
                    "Ask AI cannot check this workspace's limits right now, so it stops here. Try again shortly.",
                ),
            };
            if let Some(text) = refusal {
                self.say(state, text).await;
                return self.finish().await;
            }
            state.turn.model_turns += 1;
            let applying = state.applying.take();
            repair(&mut state.transcript, applying.as_deref());
            let request = ModelRequest {
                model: self.model.to_owned(),
                system: self.system.to_owned(),
                messages: state.transcript.clone(),
                tools: self.registry.specs(&state.loaded_tools),
                max_tokens: self.limits.max_tokens,
            };
            let estimated_input = request.estimated_input_tokens();
            let estimate = Usage {
                input_tokens: estimated_input,
                output_tokens: u64::from(request.max_tokens),
                ..Usage::default()
            };
            let Some(reservation) = self.hooks.reserve_usage(self.model, estimate).await else {
                self.say(state, "Ask AI cannot check this workspace's limits right now, so it stops here. Try again shortly.")
                    .await;
                return self.finish().await;
            };
            let message_id = format!("msg-{}", Id::new_v7());
            state.display.push(agui::assistant_message(&message_id));
            let (result, shown, seen) = self.stream_turn(&request, &message_id, deadline).await;
            // what the request cost
            let used = match &result {
                Ok(turn) => turn.usage,
                // Cut off after it was sent (stop, timeout, closed tab, broken stream): the input as the stream showed
                // it (else the estimate), and the whole output budget, because output and thinking streamed before
                // the stop are billed but not reported until the end.
                Err(error) if error.may_be_billed() => {
                    let shown = seen.unwrap_or(Usage {
                        input_tokens: estimated_input,
                        ..Usage::default()
                    });
                    Usage {
                        output_tokens: shown.output_tokens.max(u64::from(request.max_tokens)),
                        ..shown
                    }
                }
                // refused by status before any output (bad key, rate limit, bad request): not billed
                Err(_) => seen.unwrap_or_default(),
            };
            let used_model = result
                .as_ref()
                .map_or(self.model, |turn| turn.model.as_str());
            self.hooks.settle_usage(reservation, used_model, used).await;
            if let Some(message) = display_message(state, &message_id) {
                message["content"] = json!(shown);
            }
            let turn = match result {
                Err(ProviderError::Cancelled) if self.cancel.is_cancelled() => {
                    return Outcome::Cancelled;
                }
                Err(ProviderError::Cancelled) => {
                    self.say(state, "I stopped here: this took longer than the time limit. Tell me to continue if you want me to go on.")
                        .await;
                    return self.finish().await;
                }
                Err(error) => {
                    tracing::warn!(error = %error, "assistant model request failed");
                    self.say(state, &provider_error_text(&error, self.model))
                        .await;
                    return self.finish().await;
                }
                Ok(turn) => turn,
            };
            if let StopReason::Refusal(category) = &turn.stop {
                // The partial answer is dropped from the transcript, as the API advises.
                let category = category.as_deref().unwrap_or("unspecified");
                self.say(
                    state,
                    &format!("The model's safety system declined this request (category: {category}). Try rephrasing it."),
                )
                .await;
                return self.finish().await;
            }
            let tool_uses: Vec<Block> = turn
                .content
                .iter()
                .filter(|block| matches!(block, Block::ToolUse { .. }))
                .cloned()
                .collect();
            let text: String = turn
                .content
                .iter()
                .filter_map(|block| match block {
                    Block::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !turn.content.is_empty() {
                state.transcript.push(Message {
                    role: Role::Assistant,
                    content: turn.content.clone(),
                });
            }
            if tool_uses.is_empty() {
                if turn.stop == StopReason::MaxTokens {
                    self.say(state, "(The answer was cut off at the length limit.)")
                        .await;
                }
                if !state.turn.nudged && !state.turn.wrote && prompt::claims_action(&text) {
                    state.turn.nudged = true;
                    state.transcript.push(Message::user(vec![Block::Text {
                        text: prompt::HONESTY_NUDGE.into(),
                    }]));
                    continue;
                }
                return self.finish().await;
            }
            let truncated = turn.stop == StopReason::MaxTokens;
            match self
                .run_tools(state, &message_id, tool_uses.into(), Vec::new(), truncated)
                .await
            {
                ToolsOutcome::Done(results) => {
                    state.transcript.push(Message::user(results));
                    self.hooks.checkpoint(state).await;
                }
                ToolsOutcome::Paused => return Outcome::Paused,
                ToolsOutcome::Cancelled(results) => {
                    state.transcript.push(Message::user(results));
                    return Outcome::Cancelled;
                }
            }
        }
    }

    /// Streams one model turn and shows its text; returns the turn and the text shown.
    async fn stream_turn(
        &self,
        request: &ModelRequest,
        message_id: &str,
        deadline: Instant,
    ) -> (
        Result<super::model::ModelTurn, ProviderError>,
        String,
        Option<Usage>,
    ) {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let turn_cancel = self.cancel.child_token();
        let turn = self.provider.turn(request, sender, turn_cancel.clone());
        tokio::pin!(turn);
        let mut shown = String::new();
        let mut started = false;
        let mut seen = None;
        let timeout = tokio::time::sleep_until(deadline);
        tokio::pin!(timeout);
        let result = loop {
            tokio::select! {
                Some(delta) = receiver.recv() => {
                    self.show_delta(delta, message_id, &mut started, &mut shown, &mut seen).await;
                }
                result = &mut turn => break result,
                () = &mut timeout => turn_cancel.cancel(),
            }
        };
        while let Ok(delta) = receiver.try_recv() {
            self.show_delta(delta, message_id, &mut started, &mut shown, &mut seen)
                .await;
        }
        if started {
            self.out.text_end(message_id).await;
        }
        (result, shown, seen)
    }

    async fn show_delta(
        &self,
        delta: Delta,
        message_id: &str,
        started: &mut bool,
        shown: &mut String,
        seen: &mut Option<Usage>,
    ) {
        match delta {
            Delta::Text(piece) => {
                if !*started {
                    self.out.text_start(message_id).await;
                    *started = true;
                }
                self.out.text_content(message_id, &piece).await;
                shown.push_str(&piece);
            }
            Delta::Usage(usage) => *seen = Some(usage),
        }
    }

    /// Runs a turn's tool calls in order. A change that needs approval pauses the run: it and
    /// the calls after it are stored as the pending approval.
    async fn run_tools(
        &self,
        state: &mut ConversationState,
        message_id: &str,
        mut queue: VecDeque<Block>,
        mut results: Vec<Block>,
        truncated: bool,
    ) -> ToolsOutcome {
        while let Some(block) = queue.pop_front() {
            let Block::ToolUse {
                id,
                name,
                input,
                raw,
            } = block
            else {
                continue;
            };
            if self.cancel.is_cancelled() {
                results.push(Block::ToolResult {
                    tool_use_id: id,
                    content: STOPPED.into(),
                    is_error: false,
                });
                for rest in queue {
                    if let Block::ToolUse { id, .. } = rest {
                        results.push(Block::ToolResult {
                            tool_use_id: id,
                            content: STOPPED.into(),
                            is_error: false,
                        });
                    }
                }
                return ToolsOutcome::Cancelled(results);
            }
            let args = input.clone().unwrap_or_else(|| json!({}));
            self.announce(state, message_id, &id, &name, &args).await;
            state.turn.tool_calls += 1;
            let nonce = state.nonce.clone();
            let result = if state.turn.tool_calls > self.limits.max_tool_calls {
                CallResult::error(format!(
                    "Not run: one message may make at most {} tool calls. Answer the user with what you have.",
                    self.limits.max_tool_calls
                ))
            } else if truncated {
                CallResult::error(
                    "Not run: your answer was cut off (max_tokens) before this tool call was complete. Call it again with shorter input.",
                )
            } else if let Some(input) = input {
                match self.registry.risk(&name) {
                    None => CallResult::error(format!(
                        "Not run: there is no tool named {name} available to you."
                    )),
                    Some(Risk::Read) => {
                        self.run_read(state, message_id, &name, input, &nonce).await
                    }
                    // allowed for the chat, unless its text could load something from outside once shown
                    Some(Risk::Write)
                        if state.allowed_tools.contains(&name)
                            && !self.free_text(&name)
                            && !plants_outside_content(&input) =>
                    {
                        self.run_change(state, message_id, &id, &name, input, &nonce, false)
                            .await
                    }
                    Some(risk) => {
                        self.pause(state, message_id, id, name, input, risk, queue, results)
                            .await;
                        return ToolsOutcome::Paused;
                    }
                }
            } else {
                CallResult::error(json!({"INVALID_JSON": raw}).to_string())
            };
            results.push(self.deliver(state, &id, result).await);
        }
        ToolsOutcome::Done(results)
    }

    #[allow(clippy::too_many_arguments)]
    async fn pause(
        &self,
        state: &mut ConversationState,
        message_id: &str,
        id: String,
        name: String,
        input: Value,
        risk: Risk,
        queue: VecDeque<Block>,
        results: Vec<Block>,
    ) {
        let expires_at = TimestampMillis::now().as_millis() + self.limits.approval_ttl_ms;
        let interrupt_id = format!("approve:{id}");
        let interrupt = agui::approval_interrupt(
            &interrupt_id,
            &id,
            &approval_message(&name, &input),
            &iso_time(expires_at),
            json!({"tool": name, "risk": risk, "allowForConversation": risk == Risk::Write && !self.free_text(&name)}),
        );
        state.pending = Some(Pending {
            interrupt_id,
            tool_use_id: id,
            name,
            input,
            risk,
            expires_at,
            message_id: message_id.to_owned(),
            interrupt: interrupt.clone(),
            queue: queue.into(),
            results,
        });
        self.hooks.checkpoint(state).await;
        self.out
            .run_finished(
                self.thread_id,
                self.run_id,
                Some(json!({"type": "interrupt", "interrupts": [interrupt]})),
            )
            .await;
    }

    async fn run_read(
        &self,
        state: &mut ConversationState,
        message_id: &str,
        name: &str,
        input: Value,
        nonce: &str,
    ) -> CallResult {
        match name {
            LOAD_TOOLS => {
                let requested: Vec<String> = input["names"]
                    .as_array()
                    .map(|names| {
                        names
                            .iter()
                            .filter_map(|name| name.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                let (mut loaded, mut unknown) = (Vec::new(), Vec::new());
                for tool in requested.into_iter().take(20) {
                    if self
                        .registry
                        .loadable()
                        .any(|def| def.tool.name == tool.as_str())
                    {
                        state.loaded_tools.insert(tool.clone());
                        loaded.push(tool);
                    } else if !state.loaded_tools.contains(&tool) {
                        unknown.push(tool);
                    }
                }
                let mut text = format!(
                    "Loaded: {}. They can be called from your next step on.",
                    loaded.join(", ")
                );
                if !unknown.is_empty() {
                    text.push_str(&format!(" Not available: {}.", unknown.join(", ")));
                }
                CallResult {
                    client: json!({"summary": format!("{} tools", loaded.len())}).to_string(),
                    model: text,
                    is_error: false,
                }
            }
            OPEN_PAGE => {
                let path = input["path"].as_str().unwrap_or_default();
                match registry::open_page_path(path, self.workspace_id) {
                    Some(_) if state.turn.no_follow => CallResult {
                        model: format!(
                            "Not opened: after a change made without an approval card, pages are not opened in this message. Tell the user where to find {path}."
                        ),
                        is_error: false,
                        client: json!({"notOpened": path}).to_string(),
                    },
                    // the panel opens the page from this step's own result
                    Some(url) => CallResult {
                        model: page_note(self.follow_along, &url),
                        is_error: false,
                        client: json!({"url": url, "opened": self.follow_along}).to_string(),
                    },
                    None => {
                        CallResult::error(format!("Not opened: {path} is not a page of this app."))
                    }
                }
            }
            _ => match self.tools.call(name, input.clone()).await {
                Ok(value) => {
                    let page = self.registry.get(name).map(|def| def.label);
                    if let Some(label) = page
                        && label.shows
                        && let Some(url) =
                            registry::page_url(label.page, &input, &value, self.workspace_id)
                    {
                        let note = self.navigate(state, message_id, &url).await;
                        return with_note(success(nonce, name, &value), &note);
                    }
                    success(nonce, name, &value)
                }
                Err(error) => CallResult::error(prompt::wrap_untrusted(nonce, name, &error)),
            },
        }
    }

    /// Runs an approved (or allowed) change. After a change the user approved on a card the page follows to what it
    /// changed; after one that ran without a card ("Allow in this chat") it does not: the step's link opens it.
    #[allow(clippy::too_many_arguments)]
    async fn run_change(
        &self,
        state: &mut ConversationState,
        message_id: &str,
        id: &str,
        name: &str,
        input: Value,
        nonce: &str,
        carded: bool,
    ) -> CallResult {
        // Saved before the effect: the approval is used up, and if the server stops during the write the model is
        // told it may or may not have happened (never "approve it again").
        state.applying = Some(id.to_owned());
        if !self.hooks.checkpoint(state).await {
            state.applying = None;
            return CallResult::error(
                "Not run: the conversation could not be saved, so the change was not made. Try again.",
            );
        }
        let outcome = self.tools.call(name, input.clone()).await;
        state.applying = None;
        if !carded {
            // nothing moves the page for the rest of this message: a change made without a card could otherwise be
            // opened (and shown) by a later step the user did not see coming
            state.turn.no_follow = true;
        }
        match outcome {
            Ok(value) => {
                state.turn.wrote = true;
                self.hooks.audit_change(name, "applied").await;
                if carded
                    && let Some(label) = self.registry.get(name).map(|def| def.label)
                    && let Some(url) =
                        registry::page_url(label.page, &input, &value, self.workspace_id)
                {
                    let note = self.navigate(state, message_id, &url).await;
                    return with_note(success(nonce, name, &value), &note);
                }
                success(nonce, name, &value)
            }
            Err(error) => {
                self.hooks.audit_change(name, "failed").await;
                CallResult::error(prompt::wrap_untrusted(nonce, name, &error))
            }
        }
    }

    async fn announce(
        &self,
        state: &mut ConversationState,
        message_id: &str,
        id: &str,
        name: &str,
        args: &Value,
    ) {
        self.out.tool_call(message_id, id, name, args).await;
        if let Some(message) = display_message(state, message_id)
            && let Some(calls) = message["toolCalls"].as_array_mut()
        {
            calls.push(agui::tool_call_entry(id, name, args));
        }
    }

    async fn deliver(&self, state: &mut ConversationState, id: &str, result: CallResult) -> Block {
        self.out.tool_result(id, &result.client).await;
        state.display.push(agui::tool_message(id, &result.client));
        Block::ToolResult {
            tool_use_id: id.to_owned(),
            content: result.model,
            is_error: result.is_error,
        }
    }

    /// Opens a page in the panel's tab (the panel follows unless Follow along is off).
    /// Asks the panel to open a page, and returns what the model should know about it (the truth: whether it was
    /// opened or the user has a link).
    async fn navigate(&self, state: &mut ConversationState, message_id: &str, url: &str) -> String {
        if state.turn.no_follow {
            return "The app did not open this page: a change ran without an approval card in this message.".into();
        }
        let id = format!("nav-{}", Id::new_v7());
        let args = json!({"url": url});
        self.announce(state, message_id, &id, "navigate", &args)
            .await;
        // `opened: false` (Follow along off): the panel shows the step as a link, not as done
        let content = json!({"url": url, "opened": self.follow_along}).to_string();
        self.out.tool_result(&id, &content).await;
        state.display.push(agui::tool_message(&id, &content));
        page_note(self.follow_along, url)
    }

    /// A message from Orbit itself (a limit, an error): shown, not sent to the model.
    async fn say(&self, state: &mut ConversationState, text: &str) {
        let id = format!("msg-{}", Id::new_v7());
        self.out.text_start(&id).await;
        self.out.text_content(&id, text).await;
        self.out.text_end(&id).await;
        let mut message = agui::assistant_message(&id);
        message["content"] = json!(text);
        state.display.push(message);
    }

    async fn finish(&self) -> Outcome {
        self.out
            .run_finished(self.thread_id, self.run_id, None)
            .await;
        Outcome::Finished
    }
}

/// Whether a change's text holds something that could load from outside once shown in Orbit (a link, an image or a
/// diagram, which can carry data in its address): such a change always gets an approval card.
pub(crate) fn plants_outside_content(input: &Value) -> bool {
    const MARKERS: [&str; 9] = [
        "http://", "https://", "//", "www.", "<img", "![", "mermaid", "@{", "url(",
    ];
    match input {
        Value::String(text) => {
            let lower = text.to_lowercase();
            MARKERS.iter().any(|marker| lower.contains(marker))
        }
        Value::Array(items) => items.iter().any(plants_outside_content),
        Value::Object(fields) => fields.values().any(plants_outside_content),
        _ => false,
    }
}

impl<P, T, H> Engine<'_, P, T, H> {
    /// A change whose text Orbit renders as Markdown or HTML: always a card (see `Label::free_text`).
    fn free_text(&self, name: &str) -> bool {
        self.registry
            .get(name)
            .is_none_or(|def| def.label.free_text)
    }
}

/// What the model is told about a page the app was asked to open: opened only when Follow along is on.
fn page_note(follow_along: bool, url: &str) -> String {
    let path = url.split('?').next().unwrap_or(url);
    if follow_along {
        format!("The app opened {path} for the user (Follow along is on).")
    } else {
        format!(
            "Not opened: Follow along is off (the user's choice, not an error), so their page stays where it is. A 'go there' link to {path} is shown to them: say the page is ready behind that link. Do not say you opened it, and do not call it a failure."
        )
    }
}

/// A note from Orbit itself after the (untrusted) tool output.
fn with_note(mut result: CallResult, note: &str) -> CallResult {
    result.model.push_str(&format!("\n(Orbit: {note})"));
    result
}

fn display_message<'s>(state: &'s mut ConversationState, id: &str) -> Option<&'s mut Value> {
    state
        .display
        .iter_mut()
        .rev()
        .find(|message| message["id"] == id)
}

fn success(nonce: &str, name: &str, value: &Value) -> CallResult {
    let mut text = value.to_string();
    if text.len() > MODEL_RESULT_LIMIT {
        let cut = (0..=MODEL_RESULT_LIMIT)
            .rev()
            .find(|at| text.is_char_boundary(*at))
            .unwrap_or(0);
        text.truncate(cut);
        text.push_str(" …[truncated: ask for less, or page with a cursor]");
    }
    CallResult {
        model: prompt::wrap_untrusted(nonce, name, &text),
        is_error: false,
        client: client_view(value),
    }
}

/// What the panel gets of a result: the result itself when small, else its title, link and size.
pub(crate) fn client_view(value: &Value) -> String {
    let text = value.to_string();
    if text.len() <= CLIENT_RESULT_LIMIT {
        return text;
    }
    let mut compact = Map::new();
    if let Some(object) = value.as_object() {
        for key in ["id", "identifier", "title", "name", "url"] {
            if let Some(Value::String(field)) = object.get(key) {
                compact.insert(
                    key.into(),
                    json!(field.chars().take(200).collect::<String>()),
                );
            }
        }
        if let Some(count) = object.values().find_map(Value::as_array).map(Vec::len) {
            compact.insert("summary".into(), json!(format!("{count} results")));
        }
    } else if let Some(items) = value.as_array() {
        compact.insert("summary".into(), json!(format!("{} results", items.len())));
    }
    Value::Object(compact).to_string()
}

/// Every `tool_use` of the last assistant message gets a result, so a stopped run never leaves
/// the transcript in a shape the provider refuses.
pub(crate) fn repair(transcript: &mut Vec<Message>, applying: Option<&str>) {
    let Some(position) = transcript
        .iter()
        .rposition(|message| message.role == Role::Assistant)
    else {
        return;
    };
    let ids: Vec<String> = transcript[position]
        .content
        .iter()
        .filter_map(|block| match block {
            Block::ToolUse { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    if ids.is_empty() {
        return;
    }
    if position + 1 == transcript.len() {
        transcript.push(Message::user(Vec::new()));
    }
    let next = &mut transcript[position + 1];
    let answered: Vec<String> = next
        .content
        .iter()
        .filter_map(|block| match block {
            Block::ToolResult { tool_use_id, .. } => Some(tool_use_id.clone()),
            _ => None,
        })
        .collect();
    let missing: Vec<Block> = ids
        .into_iter()
        .filter(|id| !answered.contains(id))
        .map(|id| Block::ToolResult {
            content: if applying == Some(id.as_str()) {
                UNCERTAIN
            } else {
                STOPPED
            }
            .into(),
            tool_use_id: id,
            is_error: false,
        })
        .collect();
    // results go first in the message
    let at = next
        .content
        .iter()
        .take_while(|block| matches!(block, Block::ToolResult { .. }))
        .count();
    for (offset, block) in missing.into_iter().enumerate() {
        next.content.insert(at + offset, block);
    }
}

fn approval_message(name: &str, input: &Value) -> String {
    let title = ["title", "name"]
        .iter()
        .find_map(|key| input[*key].as_str())
        .map(|title| format!(" \"{}\"", title.chars().take(80).collect::<String>()))
        .unwrap_or_default();
    format!("{}{title}?", name.replace('_', " "))
}

fn iso_time(millis: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(millis)
        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}

pub(crate) fn provider_error_text(error: &ProviderError, model: &str) -> String {
    match error {
        ProviderError::InvalidKey => "Anthropic refused this workspace's API key. A workspace admin can replace it in Settings → AI.".into(),
        ProviderError::Forbidden(_) => format!("This workspace's API key may not use {model}. A workspace admin can pick another model in Settings → AI."),
        ProviderError::RateLimited => "Anthropic is rate limiting this workspace's API key. Try again in a minute.".into(),
        ProviderError::Unavailable(_) => "Anthropic is overloaded or unavailable right now. Try again shortly.".into(),
        ProviderError::BadRequest(detail) => format!("Anthropic refused the request: {detail}"),
        ProviderError::Network(_) => "Orbit could not reach Anthropic. Try again shortly.".into(),
        ProviderError::Protocol(_) => "Anthropic's answer could not be read. Try again.".into(),
        ProviderError::Cancelled => "Stopped.".into(),
    }
}
