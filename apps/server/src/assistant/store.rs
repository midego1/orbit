//! Ask AI conversations, kept server-side for their member only: one active conversation per user
//! and workspace (the one the panel shows) and the archived ones of the history. Each holds its
//! transcript (what the model sees), its AG-UI messages (what a reload or the history shows), the
//! approval waiting for the user (with the exact arguments, which are never taken back from the
//! client) and the tools the user allowed for the conversation. A lease makes sure a conversation
//! has one run at a time, over every server of the database. Only the active conversation runs:
//! switching away retires its waiting approval (`engine::retire`), so an old approval can never be
//! applied later.

use std::collections::BTreeSet;

use orbit_platform::{Database, Id, Row, SqlBuilder, TimestampMillis};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::engine;
use super::model::{Block, Message};
use super::registry::Risk;

/// How long a conversation is kept after its last use (the history's retention).
pub(crate) const CONVERSATION_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// The panel opens the active conversation only while it was used within this time; after that a
/// new chat starts and the old one stays in the history.
pub(crate) const IDLE_MS: i64 = 6 * 60 * 60 * 1000;
/// The most conversations kept per member and workspace; a new chat prunes the oldest.
pub(crate) const MAX_CONVERSATIONS: i64 = 100;
/// The daily purge deletes expired conversations this many at a time, for at most `PURGE_ROUNDS` batches a run.
const PURGE_BATCH: i64 = 200;
const PURGE_ROUNDS: usize = 50;
/// A derived title is the first message cut to this many characters.
pub(crate) const TITLE_CHARS: usize = 80;
/// The longest title a member may give a conversation.
pub(crate) const MAX_TITLE_CHARS: usize = 120;
/// A conversation whose state grows past this takes no new messages ("start a new chat").
pub(crate) const MAX_STATE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct ConversationState {
    /// What the model sees, in its provider-neutral form.
    #[serde(default)]
    pub transcript: Vec<Message>,
    /// AG-UI messages, as the panel showed them.
    #[serde(default)]
    pub display: Vec<Value>,
    /// Write tools the user allowed for the rest of the conversation.
    #[serde(default)]
    pub allowed_tools: BTreeSet<String>,
    /// Tools the model loaded with `load_tools`.
    #[serde(default)]
    pub loaded_tools: BTreeSet<String>,
    #[serde(default)]
    pub pending: Option<Pending>,
    /// Counters of the current user message (they reset with each new message).
    #[serde(default)]
    pub turn: TurnState,
    /// The approved change being made right now (its `tool_use` id), saved before the effect.
    #[serde(default)]
    pub applying: Option<String>,
    /// Tags the untrusted content of this conversation (see `prompt.rs`).
    #[serde(default)]
    pub nonce: String,
    /// What the user can undo of the changes Ask AI made in this conversation (`undo.rs`), oldest first.
    #[serde(default)]
    pub undo: Vec<UndoRecord>,
    /// Notes from Orbit for the model, sent with the next user message (the changes the user undid).
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct TurnState {
    pub model_turns: u32,
    pub tool_calls: u32,
    /// The honesty correction was sent for this message.
    pub nudged: bool,
    /// A change was made for this message.
    pub wrote: bool,
    /// A change ran without an approval card in this message: no page opens until the next message.
    #[serde(default)]
    pub no_follow: bool,
    /// The changes made for this message (`cards::change_entry`), for the summary card at its end.
    #[serde(default)]
    pub changes: Vec<Value>,
}

/// A tool call waiting for the user's approval, and the rest of its model turn.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Pending {
    pub interrupt_id: String,
    pub tool_use_id: String,
    pub name: String,
    pub input: Value,
    pub risk: Risk,
    pub expires_at: i64,
    /// The AG-UI assistant message the call belongs to.
    pub message_id: String,
    /// The interrupt as sent, so a reload shows the same approval card.
    pub interrupt: Value,
    /// Tool calls of the same model turn after this one, not run yet.
    pub queue: Vec<Block>,
    /// Results already made for this model turn.
    pub results: Vec<Block>,
    /// A batch of changes (`propose_changes`) waiting as one approval: each item with the exact arguments the server
    /// stored when it was proposed. `None` for a single change.
    #[serde(default)]
    pub batch: Option<Vec<BatchItem>>,
}

/// One change of a waiting batch. Its id is made by the server; the client only names ids it approves.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct BatchItem {
    pub id: String,
    pub tool: String,
    pub input: Value,
    pub risk: Risk,
    /// The task it is about, from the server's preview (`{key, title}`), for the summary of a change that was not made.
    #[serde(default)]
    pub subject: Value,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UndoState {
    Available,
    /// Saved before the undo's effect: an undo cut off by a crash is never run twice.
    Undoing,
    Undone,
    /// The task changed after Ask AI's change (or is gone): it can no longer be undone.
    Conflict,
}

/// A change Ask AI made that the user may undo (`undo.rs`), recorded by the server when the change ran.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct UndoRecord {
    pub id: String,
    /// `create_task` or `update_task`.
    pub tool: String,
    pub task_id: String,
    /// The task's key (`GEN-12`) when known, for messages.
    #[serde(default)]
    pub key: Option<String>,
    /// The task's version before the change (`None` for a create).
    #[serde(default)]
    pub before_version: Option<u64>,
    /// The task's version right after the change: undo runs only while the task still has it.
    pub after_version: u64,
    /// For an update: the previous values of the fields it changed, as `update_task` arguments.
    #[serde(default)]
    pub restore: Value,
    pub created_at: i64,
    pub expires_at: i64,
    pub state: UndoState,
    /// The task's version right after this change was undone (an update's undo; `None` for a create, whose task went
    /// to the trash). An earlier change to the same task may be undone next, in this request or a later one, while
    /// the task still has it.
    #[serde(default)]
    pub undone_version: Option<u64>,
}

#[derive(Clone, Debug)]
pub(crate) struct Conversation {
    pub id: Id,
    pub workspace_id: Id,
    /// The owner; every query already filters on it.
    #[allow(dead_code)]
    pub user_id: Id,
    pub state: ConversationState,
    pub expires_at: i64,
    /// A run holds the conversation until this time.
    pub busy_until: Option<i64>,
    pub title: Option<String>,
    pub message_count: i64,
    /// The member's current conversation in the workspace (the others are history).
    pub active: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone)]
pub(crate) struct ConversationStore {
    database: Database,
}

/// A conversation as the history lists it (no transcript).
#[derive(Clone, Debug)]
pub(crate) struct Summary {
    pub id: Id,
    pub title: Option<String>,
    pub message_count: i64,
    pub active: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// What switching to a conversation (a new one, or one of the history) did.
#[derive(Debug)]
pub(crate) enum Switch {
    Done(Box<Conversation>),
    /// A run holds one of the member's conversations: stop it first.
    Busy,
    /// No such conversation for this member (or it expired).
    NotFound,
}

/// What a delete did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Deleted {
    Done,
    Busy,
    NotFound,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    New,
    Existing(Id),
}

/// The title a conversation gets from its first message: line breaks and tabs become spaces, the
/// text is trimmed and cut at `TITLE_CHARS` (the migration that made the history uses the same rule).
pub(crate) fn derive_title(text: &str) -> Option<String> {
    let flat: String = text
        .chars()
        .map(|c| {
            if matches!(c, '\r' | '\n' | '\t') {
                ' '
            } else {
                c
            }
        })
        .collect();
    let flat = flat.trim_matches(' ');
    if flat.is_empty() {
        return None;
    }
    if flat.chars().count() <= TITLE_CHARS {
        return Some(flat.to_owned());
    }
    let cut: String = flat.chars().take(TITLE_CHARS - 1).collect();
    Some(format!("{}…", cut.trim_end_matches(' ')))
}

/// A title the member typed: whitespace runs become one space; empty, too long or with control
/// characters is refused.
pub(crate) fn clean_title(text: &str) -> Option<String> {
    if text.chars().any(|c| c.is_control() && !c.is_whitespace()) {
        return None;
    }
    let title = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!title.is_empty() && title.chars().count() <= MAX_TITLE_CHARS).then_some(title)
}

/// The title of a conversation's first user message, as the panel shows it.
fn first_message_title(state: &ConversationState) -> Option<String> {
    state
        .display
        .iter()
        .find(|message| message["role"] == "user")
        .and_then(|message| message["content"].as_str())
        .and_then(derive_title)
}

/// The user and assistant messages the panel shows.
fn message_count(state: &ConversationState) -> i64 {
    let count = state
        .display
        .iter()
        .filter(|message| message["role"] == "user" || message["role"] == "assistant")
        .count();
    i64::try_from(count).unwrap_or(i64::MAX)
}

/// An `assistant.change` audit row (declined) for each change a retired chat's approval dropped unanswered.
pub(crate) async fn audit_dropped(
    transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
    conversation: &Conversation,
    tools: &[String],
    now: TimestampMillis,
) -> Result<(), sqlx::Error> {
    for tool in tools {
        crate::audit::record(
            transaction,
            conversation.workspace_id,
            Some(conversation.user_id),
            "assistant.change",
            crate::audit::AuditOutcome::Success,
            "assistant_conversation",
            Some(conversation.id),
            super::REQUEST_ID,
            serde_json::json!({"tool": tool, "outcome": "declined", "reason": "chat_retired", "via": "assistant"}),
            now,
        )
        .await?;
    }
    Ok(())
}

/// The state as `engine::retire` leaves it, as JSON; `None` when retiring changes nothing.
fn retired_json(state: &ConversationState) -> Result<Option<String>, sqlx::Error> {
    let mut state = state.clone();
    if !engine::retire(&mut state) {
        return Ok(None);
    }
    serde_json::to_string(&state)
        .map(Some)
        .map_err(|error| sqlx::Error::Encode(Box::new(error)))
}

fn parse_id(row: &sqlx::any::AnyRow, column: &str) -> Result<Id, sqlx::Error> {
    row.try_get::<String, _>(column)?
        .parse()
        .map_err(|error| sqlx::Error::Decode(Box::new(error)))
}

fn from_row(row: &sqlx::any::AnyRow) -> Result<Conversation, sqlx::Error> {
    let state: String = row.try_get("state_json")?;
    Ok(Conversation {
        id: parse_id(row, "id")?,
        workspace_id: parse_id(row, "workspace_id")?,
        user_id: parse_id(row, "user_id")?,
        state: serde_json::from_str(&state)
            .map_err(|error| sqlx::Error::Decode(Box::new(error)))?,
        expires_at: row.try_get("expires_at")?,
        busy_until: row.try_get("run_until")?,
        title: row.try_get("title")?,
        message_count: row.try_get("message_count")?,
        active: row.try_get::<i64, _>("active")? == 1,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn summary_from_row(row: &sqlx::any::AnyRow) -> Result<Summary, sqlx::Error> {
    Ok(Summary {
        id: parse_id(row, "id")?,
        title: row.try_get("title")?,
        message_count: row.try_get("message_count")?,
        active: row.try_get::<i64, _>("active")? == 1,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

const COLUMNS: &str = "id, workspace_id, user_id, state_json, expires_at, run_until, title, message_count, active, \
     created_at, updated_at";
const SUMMARY_COLUMNS: &str = "id, title, message_count, active, created_at, updated_at";

impl ConversationStore {
    pub(crate) const fn new(database: Database) -> Self {
        Self { database }
    }

    /// The conversation the panel opens: the user's active one in the workspace, while it was used
    /// within `IDLE_MS` (after that the panel starts a new chat; the old one stays in the history).
    pub(crate) async fn current(
        &self,
        workspace_id: Id,
        user_id: Id,
        now: TimestampMillis,
    ) -> Result<Option<Conversation>, sqlx::Error> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM assistant_conversations WHERE workspace_id = $1 AND user_id = $2 \
             AND active = 1 AND expires_at > $3 AND updated_at > $4"
        )))
        .bind(workspace_id.to_string())
        .bind(user_id.to_string())
        .bind(now.as_millis())
        .bind(now.as_millis() - IDLE_MS)
        .fetch_optional(self.database.pool())
        .await?
        .as_ref()
        .map(from_row)
        .transpose()
    }

    /// The conversation, only for its own user and workspace, while it is kept (active or not).
    pub(crate) async fn get(
        &self,
        id: Id,
        workspace_id: Id,
        user_id: Id,
        now: TimestampMillis,
    ) -> Result<Option<Conversation>, sqlx::Error> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM assistant_conversations WHERE id = $1 AND workspace_id = $2 AND user_id = $3 \
             AND expires_at > $4"
        )))
        .bind(id.to_string())
        .bind(workspace_id.to_string())
        .bind(user_id.to_string())
        .bind(now.as_millis())
        .fetch_optional(self.database.pool())
        .await?
        .as_ref()
        .map(from_row)
        .transpose()
    }

    /// The conversation only while it is the user's active one: the only one a run may use.
    pub(crate) async fn get_active(
        &self,
        id: Id,
        workspace_id: Id,
        user_id: Id,
        now: TimestampMillis,
    ) -> Result<Option<Conversation>, sqlx::Error> {
        Ok(self
            .get(id, workspace_id, user_id, now)
            .await?
            .filter(|conversation| conversation.active))
    }

    /// The user's own conversations in the workspace that have messages, newest use first, after
    /// the `before` cursor (`updated_at`, `id` of the last row of the previous page). Asks for one
    /// more than `limit`, so the caller knows whether another page follows.
    pub(crate) async fn list(
        &self,
        workspace_id: Id,
        user_id: Id,
        before: Option<(i64, Id)>,
        limit: i64,
        now: TimestampMillis,
    ) -> Result<Vec<Summary>, sqlx::Error> {
        let mut query = SqlBuilder::new(format!(
            "SELECT {SUMMARY_COLUMNS} FROM assistant_conversations WHERE workspace_id = "
        ));
        query
            .push_bind(workspace_id.to_string())
            .push(" AND user_id = ")
            .push_bind(user_id.to_string())
            .push(" AND message_count > 0 AND expires_at > ")
            .push_bind(now.as_millis());
        if let Some((updated_at, id)) = before {
            query
                .push(" AND (updated_at < ")
                .push_bind(updated_at)
                .push(" OR (updated_at = ")
                .push_bind(updated_at)
                .push(" AND id < ")
                .push_bind(id.to_string())
                .push("))");
        }
        query
            .push(" ORDER BY updated_at DESC, id DESC LIMIT ")
            .push_bind(limit + 1);
        query
            .build()
            .fetch_all(self.database.pool())
            .await?
            .iter()
            .map(summary_from_row)
            .collect()
    }

    /// Starts a new conversation. The user's active one is archived (deleted when it has no
    /// messages), its waiting approval expires; expired conversations of the workspace go, and the
    /// user's oldest ones beyond `MAX_CONVERSATIONS`. `None` while a run holds one of the user's
    /// conversations.
    pub(crate) async fn create(
        &self,
        workspace_id: Id,
        user_id: Id,
        nonce: String,
        now: TimestampMillis,
    ) -> Result<Option<Conversation>, sqlx::Error> {
        Ok(
            match self
                .switch(workspace_id, user_id, Target::New, nonce, now)
                .await?
            {
                Switch::Done(conversation) => Some(*conversation),
                Switch::Busy | Switch::NotFound => None,
            },
        )
    }

    /// Makes one of the user's earlier conversations the active one again (it moves to the top of
    /// the history). The conversation that was active is archived as by `create`.
    pub(crate) async fn resume(
        &self,
        id: Id,
        workspace_id: Id,
        user_id: Id,
        now: TimestampMillis,
    ) -> Result<Switch, sqlx::Error> {
        self.switch(
            workspace_id,
            user_id,
            Target::Existing(id),
            String::new(),
            now,
        )
        .await
    }

    async fn switch(
        &self,
        workspace_id: Id,
        user_id: Id,
        target: Target,
        nonce: String,
        now: TimestampMillis,
    ) -> Result<Switch, sqlx::Error> {
        // The states are read and retired before the write transaction (they can be large); inside
        // it, the versions read are checked again. A run that saved in between, or another switch
        // (another tab), makes the check fail: the switch starts over.
        for _ in 0..3 {
            let active: Option<Conversation> = sqlx::query(sqlx::AssertSqlSafe(format!(
                "SELECT {COLUMNS} FROM assistant_conversations WHERE workspace_id = $1 AND user_id = $2 \
                 AND active = 1"
            )))
            .bind(workspace_id.to_string())
            .bind(user_id.to_string())
            .fetch_optional(self.database.pool())
            .await?
            .as_ref()
            .map(from_row)
            .transpose()?;
            // what continuing starts from: the conversation retired as well (an approval that still
            // waits in it, e.g. of the chat that is open, expires; "Allow in this chat" ends)
            let resumed = match target {
                Target::New => None,
                Target::Existing(id) => {
                    let Some(conversation) = self.get(id, workspace_id, user_id, now).await? else {
                        return Ok(Switch::NotFound);
                    };
                    let json = retired_json(&conversation.state)?;
                    Some((conversation, json))
                }
            };
            let retiring = match active {
                Some(conversation) if target != Target::Existing(conversation.id) => {
                    let json = retired_json(&conversation.state)?;
                    Some((conversation, json))
                }
                _ => None,
            };
            let mut transaction = self.database.workspace_transaction(workspace_id).await?;
            let busy: Option<i64> = sqlx::query_scalar::<_, Option<i64>>(
                "SELECT run_until FROM assistant_conversations WHERE workspace_id = $1 AND user_id = $2 \
                 AND run_until IS NOT NULL AND run_until > $3 LIMIT 1",
            )
            .bind(workspace_id.to_string())
            .bind(user_id.to_string())
            .bind(now.as_millis())
            .fetch_optional(&mut *transaction)
            .await?
            .flatten();
            if busy.is_some() {
                return Ok(Switch::Busy);
            }
            // the active conversation must still be the one read (none, or the same version)
            let now_active: Option<(String, i64)> = sqlx::query_as(
                "SELECT id, updated_at FROM assistant_conversations WHERE workspace_id = $1 AND user_id = $2 \
                 AND active = 1",
            )
            .bind(workspace_id.to_string())
            .bind(user_id.to_string())
            .fetch_optional(&mut *transaction)
            .await?;
            let read_active = retiring
                .as_ref()
                .map(|(conversation, _)| conversation)
                .or_else(|| {
                    resumed
                        .as_ref()
                        .map(|(conversation, _)| conversation)
                        .filter(|conversation| conversation.active)
                })
                .map(|conversation| (conversation.id.to_string(), conversation.updated_at));
            if now_active != read_active {
                drop(transaction);
                continue;
            }
            // what the retired chats' approvals dropped unanswered counts as declined
            for (conversation, json) in retiring.iter().chain(resumed.iter()) {
                if json.is_some() {
                    audit_dropped(
                        &mut transaction,
                        conversation,
                        &engine::pending_changes(&conversation.state),
                        now,
                    )
                    .await?;
                }
            }
            if let Some((conversation, json)) = &retiring {
                // archived: no run holds it any more, and none may (only the active one runs)
                if conversation.message_count == 0 {
                    sqlx::query("DELETE FROM assistant_conversations WHERE id = $1")
                        .bind(conversation.id.to_string())
                        .execute(&mut *transaction)
                        .await?;
                } else {
                    sqlx::query(
                        "UPDATE assistant_conversations SET active = 0, run_id = NULL, run_until = NULL, \
                         state_json = COALESCE($1, state_json) WHERE id = $2",
                    )
                    .bind(json.clone())
                    .bind(conversation.id.to_string())
                    .execute(&mut *transaction)
                    .await?;
                }
            }
            let conversation = match resumed {
                None => {
                    let conversation = self
                        .insert(&mut transaction, workspace_id, user_id, nonce.clone(), now)
                        .await?;
                    transaction.commit().await?;
                    conversation
                }
                Some((resumed, json)) => {
                    let updated = sqlx::query(
                        "UPDATE assistant_conversations SET active = 1, updated_at = $1, expires_at = $2, \
                         run_id = NULL, run_until = NULL, state_json = COALESCE($3, state_json) \
                         WHERE id = $4 AND updated_at = $5 AND expires_at > $1",
                    )
                    .bind(now.as_millis())
                    .bind(now.as_millis() + CONVERSATION_TTL_MS)
                    .bind(json)
                    .bind(resumed.id.to_string())
                    .bind(resumed.updated_at)
                    .execute(&mut *transaction)
                    .await?
                    .rows_affected();
                    if updated != 1 {
                        // changed (or deleted) since it was read
                        drop(transaction);
                        continue;
                    }
                    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
                        "SELECT {COLUMNS} FROM assistant_conversations WHERE id = $1"
                    )))
                    .bind(resumed.id.to_string())
                    .fetch_one(&mut *transaction)
                    .await?;
                    transaction.commit().await?;
                    from_row(&row)?
                }
            };
            return Ok(Switch::Done(Box::new(conversation)));
        }
        Ok(Switch::Busy)
    }

    async fn insert(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
        workspace_id: Id,
        user_id: Id,
        nonce: String,
        now: TimestampMillis,
    ) -> Result<Conversation, sqlx::Error> {
        let id = Id::new_v7();
        let state = ConversationState {
            nonce,
            ..ConversationState::default()
        };
        let json =
            serde_json::to_string(&state).map_err(|error| sqlx::Error::Encode(Box::new(error)))?;
        let expires_at = now.as_millis() + CONVERSATION_TTL_MS;
        sqlx::query(
            "DELETE FROM assistant_conversations WHERE workspace_id = $1 AND expires_at <= $2",
        )
        .bind(workspace_id.to_string())
        .bind(now.as_millis())
        .execute(&mut **transaction)
        .await?;
        sqlx::query(
            "INSERT INTO assistant_conversations (id, workspace_id, user_id, state_json, created_at, updated_at, \
             expires_at, message_count, active) VALUES ($1, $2, $3, $4, $5, $5, $6, 0, 1)",
        )
        .bind(id.to_string())
        .bind(workspace_id.to_string())
        .bind(user_id.to_string())
        .bind(json)
        .bind(now.as_millis())
        .bind(expires_at)
        .execute(&mut **transaction)
        .await?;
        // the cap: the newest `MAX_CONVERSATIONS` stay (the new one among them)
        sqlx::query(
            "DELETE FROM assistant_conversations WHERE workspace_id = $1 AND user_id = $2 AND active = 0 \
             AND id NOT IN (SELECT id FROM assistant_conversations WHERE workspace_id = $1 AND user_id = $2 \
             ORDER BY updated_at DESC, id DESC LIMIT $3)",
        )
        .bind(workspace_id.to_string())
        .bind(user_id.to_string())
        .bind(MAX_CONVERSATIONS)
        .execute(&mut **transaction)
        .await?;
        Ok(Conversation {
            id,
            workspace_id,
            user_id,
            state,
            expires_at,
            busy_until: None,
            title: None,
            message_count: 0,
            active: true,
            created_at: now.as_millis(),
            updated_at: now.as_millis(),
        })
    }

    /// Deletes every conversation past its retention, of every workspace, in batches of
    /// `PURGE_BATCH` (one short write transaction per workspace and batch). Run by the daily
    /// `workspace.retention` job; safe to run again after a stop. Returns how many went.
    pub(crate) async fn purge_expired(&self, now: TimestampMillis) -> Result<u64, sqlx::Error> {
        let mut purged = 0;
        for _ in 0..PURGE_ROUNDS {
            let rows: Vec<(String, String)> = sqlx::query_as(
                "SELECT workspace_id, id FROM assistant_conversations WHERE expires_at <= $1 \
                 ORDER BY expires_at LIMIT $2",
            )
            .bind(now.as_millis())
            .bind(PURGE_BATCH)
            .fetch_all(self.database.pool())
            .await?;
            if rows.is_empty() {
                break;
            }
            let mut by_workspace: std::collections::BTreeMap<String, Vec<String>> =
                std::collections::BTreeMap::new();
            for (workspace, id) in rows {
                by_workspace.entry(workspace).or_default().push(id);
            }
            for (workspace, ids) in by_workspace {
                let workspace_id: Id = workspace
                    .parse()
                    .map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
                let mut transaction = self.database.workspace_transaction(workspace_id).await?;
                let mut query =
                    SqlBuilder::new("DELETE FROM assistant_conversations WHERE workspace_id = ");
                query
                    .push_bind(workspace)
                    .push(" AND expires_at <= ")
                    .push_bind(now.as_millis())
                    .push(" AND id IN (");
                {
                    let mut list = query.separated(", ");
                    for id in ids {
                        list.push_bind(id);
                    }
                }
                query.push(")");
                purged += query
                    .build()
                    .execute(&mut *transaction)
                    .await?
                    .rows_affected();
                transaction.commit().await?;
            }
        }
        Ok(purged)
    }

    /// Gives one of the user's conversations a title of their own. `None` when there is no such
    /// conversation for them.
    pub(crate) async fn rename(
        &self,
        id: Id,
        workspace_id: Id,
        user_id: Id,
        title: &str,
        now: TimestampMillis,
    ) -> Result<Option<Summary>, sqlx::Error> {
        let mut transaction = self.database.workspace_transaction(workspace_id).await?;
        let renamed = sqlx::query(
            "UPDATE assistant_conversations SET title = $1 WHERE id = $2 AND workspace_id = $3 AND user_id = $4 \
             AND expires_at > $5",
        )
        .bind(title)
        .bind(id.to_string())
        .bind(workspace_id.to_string())
        .bind(user_id.to_string())
        .bind(now.as_millis())
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        if renamed != 1 {
            return Ok(None);
        }
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {SUMMARY_COLUMNS} FROM assistant_conversations WHERE id = $1"
        )))
        .bind(id.to_string())
        .fetch_one(&mut *transaction)
        .await?;
        transaction.commit().await?;
        summary_from_row(&row).map(Some)
    }

    /// Deletes one of the user's conversations for good (the active one too), unless a run holds it.
    pub(crate) async fn delete(
        &self,
        id: Id,
        workspace_id: Id,
        user_id: Id,
        now: TimestampMillis,
    ) -> Result<Deleted, sqlx::Error> {
        let mut transaction = self.database.workspace_transaction(workspace_id).await?;
        let found: Option<Option<i64>> = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT run_until FROM assistant_conversations WHERE id = $1 AND workspace_id = $2 AND user_id = $3 \
             AND expires_at > $4",
        )
        .bind(id.to_string())
        .bind(workspace_id.to_string())
        .bind(user_id.to_string())
        .bind(now.as_millis())
        .fetch_optional(&mut *transaction)
        .await?;
        match found {
            None => return Ok(Deleted::NotFound),
            Some(Some(until)) if until > now.as_millis() => return Ok(Deleted::Busy),
            Some(_) => {}
        }
        sqlx::query("DELETE FROM assistant_conversations WHERE id = $1")
            .bind(id.to_string())
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(Deleted::Done)
    }

    /// Takes the conversation for one run until `until`; `false` when another run holds it or it is
    /// no longer the user's active conversation.
    pub(crate) async fn lease(
        &self,
        conversation: &Conversation,
        run_id: &str,
        until: i64,
        now: TimestampMillis,
    ) -> Result<bool, sqlx::Error> {
        let mut transaction = self
            .database
            .workspace_transaction(conversation.workspace_id)
            .await?;
        let taken = sqlx::query(
            "UPDATE assistant_conversations SET run_id = $1, run_until = $2 WHERE id = $3 AND active = 1 \
             AND (run_until IS NULL OR run_until <= $4)",
        )
        .bind(run_id)
        .bind(until)
        .bind(conversation.id.to_string())
        .bind(now.as_millis())
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        transaction.commit().await?;
        Ok(taken == 1)
    }

    /// Ends a lease without saving anything (the state stays as the last save left it).
    pub(crate) async fn release(
        &self,
        conversation: &Conversation,
        run_id: &str,
    ) -> Result<(), sqlx::Error> {
        let mut transaction = self
            .database
            .workspace_transaction(conversation.workspace_id)
            .await?;
        sqlx::query(
            "UPDATE assistant_conversations SET run_id = NULL, run_until = NULL WHERE id = $1 AND run_id = $2",
        )
        .bind(conversation.id.to_string())
        .bind(run_id)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await
    }

    /// Saves the state (it slides the expiry, sets the title from the first message unless there is
    /// one, and the message count); with `release`, also ends the run's lease. `false` when this run
    /// no longer holds the conversation, so nothing was saved.
    pub(crate) async fn save(
        &self,
        conversation: &Conversation,
        state: &ConversationState,
        run_id: &str,
        release: bool,
        now: TimestampMillis,
    ) -> Result<bool, sqlx::Error> {
        self.save_auditing(conversation, state, run_id, release, &[], now)
            .await
    }

    /// [`Self::save`], with an `assistant.change` row (declined) for each of `dropped` written in the same
    /// transaction, only when the state is saved: a retired approval is audited exactly once.
    pub(crate) async fn save_auditing(
        &self,
        conversation: &Conversation,
        state: &ConversationState,
        run_id: &str,
        release: bool,
        dropped: &[String],
        now: TimestampMillis,
    ) -> Result<bool, sqlx::Error> {
        let json =
            serde_json::to_string(state).map_err(|error| sqlx::Error::Encode(Box::new(error)))?;
        let title = first_message_title(state);
        let mut transaction = self
            .database
            .workspace_transaction(conversation.workspace_id)
            .await?;
        let saved = sqlx::query(
            "UPDATE assistant_conversations SET state_json = $1, updated_at = $2, expires_at = $3, \
             run_id = CASE WHEN CAST($4 AS BIGINT) = 1 THEN NULL ELSE run_id END, \
             run_until = CASE WHEN CAST($4 AS BIGINT) = 1 THEN NULL ELSE run_until END, \
             title = COALESCE(title, $7), message_count = $8 \
             WHERE id = $5 AND run_id = $6 AND active = 1",
        )
        .bind(json)
        .bind(now.as_millis())
        .bind(now.as_millis() + CONVERSATION_TTL_MS)
        .bind(i64::from(release))
        .bind(conversation.id.to_string())
        .bind(run_id)
        .bind(title)
        .bind(message_count(state))
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        if saved == 1 {
            audit_dropped(&mut transaction, conversation, dropped, now).await?;
        }
        transaction.commit().await?;
        Ok(saved == 1)
    }
}
