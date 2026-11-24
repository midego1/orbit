//! Undo for the changes Ask AI made: a user action in the panel, never a tool of the model.
//!
//! - When a `create_task` or `update_task` of Ask AI runs, the server records what undoes it (`UndoRecord`, in the
//!   conversation): the task, its version right after the change and, for an update, the previous values of the
//!   fields it changed, read by the server right before the change.
//! - `POST …/conversations/{id}/undo` (the member's own conversation only) undoes records of that conversation with
//!   the member's own permissions: a created task goes to the trash (`delete_task`, restorable for 30 days), an
//!   update gets its previous values back (`update_task`). Only while the task still has the version Ask AI's change
//!   left (nobody changed it since), only within 24 hours, and only once.

use std::collections::HashMap;

use futures_util::future::BoxFuture;
use serde_json::{Map, Value, json};

use super::cards::is_task_key;
use super::engine::ToolRunner;
use super::store::{ConversationState, UndoRecord, UndoState};

/// How long a change can be undone (the conversation itself must also still live).
pub(crate) const UNDO_TTL_MS: i64 = 24 * 60 * 60 * 1000;
/// The most undo records a conversation keeps (the oldest go first).
pub(crate) const MAX_RECORDS: usize = 200;
/// The most ids one undo request may name.
pub(crate) const MAX_IDS: usize = 50;

/// The `update_task` fields an undo can set back. An update that touched anything else (the parent, the recurrence,
/// a duplicate mark) has side effects an undo could not reverse, so it gets no undo.
const RESTORABLE: [&str; 11] = [
    "title",
    "description",
    "status_id",
    "priority",
    "assignee_ids",
    "label_ids",
    "due_at",
    "due_start_at",
    "milestone_id",
    "cycle_id",
    "estimate",
];

fn key_of(card_key: Option<&str>) -> Option<String> {
    card_key.filter(|key| is_task_key(key)).map(str::to_owned)
}

fn id_of(value: &Value) -> Option<String> {
    value["id"]
        .as_str()
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 64
                && id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        })
        .map(str::to_owned)
}

/// The undo of a task Ask AI created: the new task and its version, from the create's own result.
pub(crate) fn record_create(created: &Value, key: Option<&str>, now: i64) -> Option<UndoRecord> {
    Some(UndoRecord {
        id: orbit_platform::Id::new_v7().to_string(),
        tool: "create_task".into(),
        task_id: id_of(created)?,
        key: key_of(key),
        before_version: None,
        after_version: created["version"].as_u64()?,
        restore: Value::Null,
        created_at: now,
        expires_at: now + UNDO_TTL_MS,
        state: UndoState::Available,
    })
}

/// The undo of an update: `before` is the task as the server read it right before the update. It counts only when its
/// version is the one the update was made against (`expected_version`), so it is exactly what the update replaced.
pub(crate) fn record_update(
    input: &Value,
    before: &Value,
    after: &Value,
    key: Option<&str>,
    now: i64,
) -> Option<UndoRecord> {
    let arguments = input.as_object()?;
    let before_version = before["version"].as_u64()?;
    if input["expected_version"].as_u64() != Some(before_version) {
        return None;
    }
    let task_id = id_of(after)?;
    if id_of(before).as_deref() != Some(task_id.as_str()) {
        return None;
    }
    // completing a recurring task makes its next occurrence, which an undo would leave behind
    if arguments.contains_key("status_id") && !before["recurrence"].is_null() {
        return None;
    }
    let mut restore = Map::new();
    for name in arguments.keys() {
        match name.as_str() {
            "task_id" | "expected_version" => {}
            field if RESTORABLE.contains(&field) => {
                restore.insert(field.to_owned(), before[field].clone());
            }
            _ => return None,
        }
    }
    if restore.is_empty() {
        return None;
    }
    Some(UndoRecord {
        id: orbit_platform::Id::new_v7().to_string(),
        tool: "update_task".into(),
        task_id,
        key: key_of(key),
        before_version: Some(before_version),
        after_version: after["version"].as_u64()?,
        restore: Value::Object(restore),
        created_at: now,
        expires_at: now + UNDO_TTL_MS,
        state: UndoState::Available,
    })
}

/// Adds a record and drops the oldest past [`MAX_RECORDS`].
pub(crate) fn remember(state: &mut ConversationState, record: UndoRecord) {
    state.undo.push(record);
    if state.undo.len() > MAX_RECORDS {
        let extra = state.undo.len() - MAX_RECORDS;
        state.undo.drain(..extra);
    }
}

/// What the panel shows of a record now: `available`, `undone`, `conflict`, `expired` or `undoing`.
pub(crate) fn state_word(record: &UndoRecord, now: i64) -> &'static str {
    match record.state {
        UndoState::Available if now > record.expires_at => "expired",
        UndoState::Available => "available",
        UndoState::Undoing => "undoing",
        UndoState::Undone => "undone",
        UndoState::Conflict => "conflict",
    }
}

/// Saves the conversation during an undo; `false` when it was not saved (then nothing more is undone).
pub(crate) trait UndoSaver: Send + Sync {
    fn save<'a>(&'a self, state: &'a ConversationState) -> BoxFuture<'a, bool>;
}

enum Failure {
    /// The task changed or is gone: final.
    Conflict(String),
    /// Something else went wrong (a database error): the user may try again.
    Retry(String),
}

fn name(record: &UndoRecord) -> String {
    record.key.clone().unwrap_or_else(|| "The task".to_owned())
}

fn changed_since(record: &UndoRecord) -> String {
    format!(
        "{} was changed after Ask AI's change (by you or someone else), so it was not undone.",
        name(record)
    )
}

fn clip(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(300).collect()
}

/// Whether a tool error says the item's version moved on.
fn is_version_conflict(error: &str) -> bool {
    error.contains("version changed") || error.contains("VersionConflict")
}

fn is_gone(error: &str) -> bool {
    let lower = error.to_lowercase();
    // MCP says "Task resource is not available" for a task that is gone or out of reach
    lower.contains("resource is not available") || lower.contains("not found")
}

/// Undoes one record as the member. `chain` holds, per task, the version an undo of a later change in this same
/// request left and the version that change was made against: then the earlier change may follow.
async fn undo_one<T: ToolRunner>(
    tools: &T,
    record: &UndoRecord,
    chain: &HashMap<String, (u64, u64)>,
) -> Result<Option<u64>, Failure> {
    let current = match tools
        .lookup("get_task", json!({"task_id": record.task_id}))
        .await
    {
        Ok(task) => task,
        Err(error) if is_gone(&error) => {
            return Err(Failure::Conflict(format!(
                "{} is no longer there (it was deleted or moved to the trash), so there is nothing to undo.",
                name(record)
            )));
        }
        Err(error) => {
            return Err(Failure::Retry(format!(
                "{} could not be read: {}",
                name(record),
                clip(&error)
            )));
        }
    };
    let Some(version) = current["version"].as_u64() else {
        return Err(Failure::Retry(format!(
            "{} could not be read.",
            name(record)
        )));
    };
    let unchanged = version == record.after_version
        || chain
            .get(&record.task_id)
            .is_some_and(|(made_against, left)| {
                *made_against == record.after_version && version == *left
            });
    if !unchanged {
        return Err(Failure::Conflict(changed_since(record)));
    }
    let outcome = if record.tool == "create_task" {
        // a sub-issue someone added since would go to the trash with it
        match tools
            .lookup(
                "list_tasks",
                json!({"parent_task_id": record.task_id, "limit": 1}),
            )
            .await
        {
            Ok(children)
                if children["items"]
                    .as_array()
                    .is_some_and(|items| !items.is_empty()) =>
            {
                return Err(Failure::Conflict(format!(
                    "{} has sub-issues now, so it was not moved to the trash. Delete it yourself if you want it gone.",
                    name(record)
                )));
            }
            Ok(_) => {}
            Err(error) => {
                return Err(Failure::Retry(format!(
                    "{} could not be checked: {}",
                    name(record),
                    clip(&error)
                )));
            }
        }
        tools
            .call(
                "delete_task",
                json!({"task_id": record.task_id, "expected_version": version, "permanent": false}),
            )
            .await
            .map(|_| None)
    } else {
        let mut arguments = record.restore.as_object().cloned().unwrap_or_default();
        arguments.insert("task_id".into(), json!(record.task_id));
        arguments.insert("expected_version".into(), json!(version));
        tools
            .call("update_task", Value::Object(arguments))
            .await
            .map(|task| task["version"].as_u64())
    };
    outcome.map_err(|error| {
        if is_version_conflict(&error) {
            Failure::Conflict(changed_since(record))
        } else if is_gone(&error) {
            Failure::Conflict(format!(
                "{} is no longer there, so there is nothing to undo.",
                name(record)
            ))
        } else {
            Failure::Retry(format!(
                "{} could not be undone: {}",
                name(record),
                clip(&error)
            ))
        }
    })
}

/// Undoes the records `ids` names (newest first, so "Undo all" can take back two changes to the same task), with the
/// member's tools. Each record is set to `undoing` and saved before its effect, so it never runs twice. Returns one
/// result per id: `{id, outcome, message}` (and `attempted` when the undo was tried on the task) with outcome `undone`, `conflict`, `expired`, `already_undone`,
/// `not_found`, `uncertain` or `failed`.
pub(crate) async fn undo<T: ToolRunner, S: UndoSaver>(
    state: &mut ConversationState,
    ids: &[String],
    tools: &T,
    saver: &S,
    now: i64,
) -> Vec<Value> {
    let mut results = Vec::new();
    let mut positions = Vec::new();
    for id in ids {
        if results
            .iter()
            .any(|result: &Value| result["id"] == id.as_str())
            || positions
                .iter()
                .any(|(_, known): &(usize, String)| known == id)
        {
            continue;
        }
        match state.undo.iter().position(|record| record.id == *id) {
            Some(position) => positions.push((position, id.clone())),
            None => results.push(json!({"id": id, "outcome": "not_found", "message": "This change cannot be undone here."})),
        }
    }
    positions.sort_by_key(|(position, _)| std::cmp::Reverse(*position));
    let mut chain: HashMap<String, (u64, u64)> = HashMap::new();
    let mut undone = Vec::new();
    let mut broken = false;
    for (position, id) in positions {
        let record = state.undo[position].clone();
        let refusal = match record.state {
            _ if broken => Some((
                "failed",
                "Not undone: the conversation could not be saved. Try again.".to_owned(),
            )),
            UndoState::Undone => Some((
                "already_undone",
                "This change was already undone.".to_owned(),
            )),
            UndoState::Undoing => Some((
                "uncertain",
                format!(
                    "An earlier undo of this change was cut off. Check {} before you try anything else.",
                    name(&record)
                ),
            )),
            UndoState::Conflict => Some(("conflict", changed_since(&record))),
            UndoState::Available if now > record.expires_at => Some((
                "expired",
                "Changes can be undone for 24 hours only; this one is older.".to_owned(),
            )),
            UndoState::Available => None,
        };
        if let Some((outcome, message)) = refusal {
            results.push(json!({"id": id, "outcome": outcome, "message": message}));
            continue;
        }
        // a later change by Ask AI to the same task that is still in place: that one goes first
        let later = state.undo[position + 1..]
            .iter()
            .any(|other| other.task_id == record.task_id && other.state != UndoState::Undone);
        if later {
            results.push(json!({"id": id, "outcome": "conflict", "message": format!(
                "{} was changed again by Ask AI after this change; undo that later change first.",
                name(&record)
            )}));
            continue;
        }
        state.undo[position].state = UndoState::Undoing;
        if !saver.save(state).await {
            state.undo[position].state = UndoState::Available;
            broken = true;
            results.push(json!({"id": id, "outcome": "failed", "message": "Not undone: the conversation could not be saved. Try again."}));
            continue;
        }
        match undo_one(tools, &record, &chain).await {
            Ok(left) => {
                state.undo[position].state = UndoState::Undone;
                if let (Some(made_against), Some(left)) = (record.before_version, left) {
                    chain.insert(record.task_id.clone(), (made_against, left));
                }
                undone.push(if record.tool == "create_task" {
                    format!("created task {} (now in the trash)", name(&record))
                } else {
                    format!("update of {} (its previous values are back)", name(&record))
                });
                results.push(json!({"id": id, "outcome": "undone", "attempted": true, "message": if record.tool == "create_task" {
                    format!("{} was moved to the trash.", name(&record))
                } else {
                    format!("{} has its previous values again.", name(&record))
                }}));
            }
            Err(Failure::Conflict(message)) => {
                state.undo[position].state = UndoState::Conflict;
                results.push(
                    json!({"id": id, "outcome": "conflict", "attempted": true, "message": message}),
                );
            }
            Err(Failure::Retry(message)) => {
                state.undo[position].state = UndoState::Available;
                results.push(
                    json!({"id": id, "outcome": "failed", "attempted": true, "message": message}),
                );
            }
        }
    }
    if !undone.is_empty() {
        state.notes.push(format!(
            "The user undid these changes you made earlier in this conversation: {}. Read the tasks again before you rely on them, and do not redo these changes unless the user asks.",
            undone.join("; ")
        ));
        // a long chat does not pile up notes
        if state.notes.len() > 20 {
            let extra = state.notes.len() - 20;
            state.notes.drain(..extra);
        }
    }
    // the answer lists the ids in the order they were asked
    let order = |result: &Value| {
        ids.iter()
            .position(|id| result["id"] == id.as_str())
            .unwrap_or(usize::MAX)
    };
    results.sort_by_key(order);
    results
}
