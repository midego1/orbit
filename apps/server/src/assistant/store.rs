//! Ask AI conversations, kept server-side for a few hours: one current conversation per user and
//! workspace, its transcript (what the model sees), its AG-UI messages (what a reload shows), the
//! approval waiting for the user (with the exact arguments, which are never taken back from the
//! client) and the tools the user allowed for the conversation. A lease makes sure a
//! conversation has one run at a time, over every server of the database.

use std::collections::BTreeSet;

use orbit_platform::{Database, Id, Row, TimestampMillis};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::model::{Block, Message};
use super::registry::Risk;

/// How long a conversation lives after its last activity.
pub(crate) const CONVERSATION_TTL_MS: i64 = 6 * 60 * 60 * 1000;
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
}

#[derive(Clone)]
pub(crate) struct ConversationStore {
    database: Database,
}

fn from_row(row: &sqlx::any::AnyRow) -> Result<Conversation, sqlx::Error> {
    let parse = |column: &str| -> Result<Id, sqlx::Error> {
        row.try_get::<String, _>(column)?
            .parse()
            .map_err(|error| sqlx::Error::Decode(Box::new(error)))
    };
    let state: String = row.try_get("state_json")?;
    Ok(Conversation {
        id: parse("id")?,
        workspace_id: parse("workspace_id")?,
        user_id: parse("user_id")?,
        state: serde_json::from_str(&state)
            .map_err(|error| sqlx::Error::Decode(Box::new(error)))?,
        expires_at: row.try_get("expires_at")?,
        busy_until: row.try_get("run_until")?,
    })
}

const COLUMNS: &str = "id, workspace_id, user_id, state_json, expires_at, run_until";

impl ConversationStore {
    pub(crate) const fn new(database: Database) -> Self {
        Self { database }
    }

    /// The user's current conversation in the workspace, if one is still alive.
    pub(crate) async fn current(
        &self,
        workspace_id: Id,
        user_id: Id,
        now: TimestampMillis,
    ) -> Result<Option<Conversation>, sqlx::Error> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM assistant_conversations WHERE workspace_id = $1 AND user_id = $2 \
             AND expires_at > $3 ORDER BY updated_at DESC, id DESC LIMIT 1"
        )))
        .bind(workspace_id.to_string())
        .bind(user_id.to_string())
        .bind(now.as_millis())
        .fetch_optional(self.database.pool())
        .await?
        .as_ref()
        .map(from_row)
        .transpose()
    }

    /// The conversation, only for its own user and workspace, while it lives.
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

    /// Starts a new conversation and drops the user's earlier ones in this workspace (and any
    /// conversation of the workspace that expired). Refused while a run holds the current one.
    pub(crate) async fn create(
        &self,
        workspace_id: Id,
        user_id: Id,
        nonce: String,
        now: TimestampMillis,
    ) -> Result<Option<Conversation>, sqlx::Error> {
        let id = Id::new_v7();
        let state = ConversationState {
            nonce,
            ..ConversationState::default()
        };
        let json =
            serde_json::to_string(&state).map_err(|error| sqlx::Error::Encode(Box::new(error)))?;
        let expires_at = now.as_millis() + CONVERSATION_TTL_MS;
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
            return Ok(None);
        }
        sqlx::query(
            "DELETE FROM assistant_conversations WHERE workspace_id = $1 AND (user_id = $2 OR expires_at <= $3)",
        )
        .bind(workspace_id.to_string())
        .bind(user_id.to_string())
        .bind(now.as_millis())
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "INSERT INTO assistant_conversations (id, workspace_id, user_id, state_json, created_at, updated_at, \
             expires_at) VALUES ($1, $2, $3, $4, $5, $5, $6)",
        )
        .bind(id.to_string())
        .bind(workspace_id.to_string())
        .bind(user_id.to_string())
        .bind(json)
        .bind(now.as_millis())
        .bind(expires_at)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(Some(Conversation {
            id,
            workspace_id,
            user_id,
            state,
            expires_at,
            busy_until: None,
        }))
    }

    /// Takes the conversation for one run until `until`; `false` when another run holds it.
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
            "UPDATE assistant_conversations SET run_id = $1, run_until = $2 WHERE id = $3 \
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

    /// Saves the state (it slides the expiry); with `release`, also ends the run's lease. `false` when this run no
    /// longer holds the conversation, so nothing was saved.
    pub(crate) async fn save(
        &self,
        conversation: &Conversation,
        state: &ConversationState,
        run_id: &str,
        release: bool,
        now: TimestampMillis,
    ) -> Result<bool, sqlx::Error> {
        let json =
            serde_json::to_string(state).map_err(|error| sqlx::Error::Encode(Box::new(error)))?;
        let mut transaction = self
            .database
            .workspace_transaction(conversation.workspace_id)
            .await?;
        let saved = sqlx::query(
            "UPDATE assistant_conversations SET state_json = $1, updated_at = $2, expires_at = $3, \
             run_id = CASE WHEN CAST($4 AS BIGINT) = 1 THEN NULL ELSE run_id END, \
             run_until = CASE WHEN CAST($4 AS BIGINT) = 1 THEN NULL ELSE run_until END \
             WHERE id = $5 AND run_id = $6",
        )
        .bind(json)
        .bind(now.as_millis())
        .bind(now.as_millis() + CONVERSATION_TTL_MS)
        .bind(i64::from(release))
        .bind(conversation.id.to_string())
        .bind(run_id)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        transaction.commit().await?;
        Ok(saved == 1)
    }
}
