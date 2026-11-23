//! Cards: what the panel draws for a tool call, built here from the tool's own result (never from the model's text).
//!
//! - A task, a list of tasks (with exact totals per project and per status), docs and projects get a card on their
//!   step; a waiting change gets a preview on its approval card (the current values, read by the server when the
//!   approval is made, next to the new ones); a message that made changes ends with a summary of them.
//! - Names (project, status, assignees) are resolved here, as the signed-in user, through the same in-process tools
//!   (`ToolRunner::lookup`), and kept for the run.
//! - Every text in a card (titles, names) is plain text: the panel never renders it as Markdown or HTML. Every link is
//!   a same-origin path built here from an id (`/tasks/<id>?workspace=<id>`); ids that are not ids give no link.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;

use orbit_platform::Id;
use serde_json::{Map, Value, json};

use super::engine::ToolRunner;

/// Status categories of a finished task (`task_filter::DONE_CATEGORIES`).
const DONE: [&str; 3] = ["completed", "cancelled", "duplicate"];
/// The most tasks a list card carries (a list call returns at most 100); its totals still count every task.
const MAX_LIST_ITEMS: usize = 50;
const MAX_TITLE: usize = 300;
/// The step that carries the summary card at the end of a message with changes (not a tool the model can call).
pub(crate) const SUMMARY_TOOL: &str = "orbit_summary";
/// The server's own count of the tasks a `list_tasks` call matches (`TaskTools::count_tasks_in_process`); a lookup
/// only, never a tool of the model.
pub(crate) const COUNT_TASKS: &str = "count_tasks";

#[derive(Clone, Debug, Default)]
struct Project {
    name: String,
    key: String,
}

#[derive(Clone, Debug, Default)]
struct Status {
    name: String,
    category: String,
    color: String,
}

#[derive(Default)]
struct Directory {
    projects: Option<HashMap<String, Project>>,
    statuses: HashMap<String, Status>,
    status_projects: HashSet<String>,
    members: Option<HashMap<String, String>>,
}

/// Names of projects, statuses and members, looked up once per run as the signed-in user.
#[derive(Default)]
pub(crate) struct Names {
    inner: Mutex<Directory>,
}

impl Names {
    fn with<R>(&self, f: impl FnOnce(&mut Directory) -> R) -> R {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut guard)
    }

    async fn load_projects<T: ToolRunner>(&self, tools: &T) {
        if self.with(|dir| dir.projects.is_some()) {
            return;
        }
        let mut projects = HashMap::new();
        if let Ok(value) = tools.lookup("list_projects", json!({})).await {
            for item in items(&value) {
                if let Some(id) = item["id"].as_str() {
                    projects.insert(
                        id.to_owned(),
                        Project {
                            name: text(&item["name"]),
                            key: text(&item["key"]),
                        },
                    );
                }
            }
        }
        self.with(|dir| dir.projects = Some(projects));
    }

    async fn load_statuses<T: ToolRunner>(&self, tools: &T, project_id: &str) {
        if project_id.is_empty() || self.with(|dir| dir.status_projects.contains(project_id)) {
            return;
        }
        let mut statuses = Vec::new();
        if let Ok(value) = tools
            .lookup(
                "list_task_statuses",
                json!({"project_id": project_id, "limit": 100}),
            )
            .await
        {
            for item in items(&value) {
                if let Some(id) = item["id"].as_str() {
                    statuses.push((
                        id.to_owned(),
                        Status {
                            name: text(&item["name"]),
                            category: text(&item["category"]),
                            color: text(&item["color"]),
                        },
                    ));
                }
            }
        }
        self.with(|dir| {
            dir.status_projects.insert(project_id.to_owned());
            dir.statuses.extend(statuses);
        });
    }

    async fn load_members<T: ToolRunner>(&self, tools: &T) {
        if self.with(|dir| dir.members.is_some()) {
            return;
        }
        let mut members = HashMap::new();
        let mut cursor: Option<String> = None;
        // a few pages at most (500 people): anyone beyond them shows as "Unknown member"
        for _ in 0..5 {
            let mut input = json!({"limit": 100});
            if let Some(cursor) = &cursor {
                input["cursor"] = json!(cursor);
            }
            let Ok(value) = tools.lookup("list_members", input).await else {
                break;
            };
            for item in items(&value) {
                if let Some(id) = item["user_id"].as_str() {
                    members.insert(id.to_owned(), text(&item["display_name"]));
                }
            }
            cursor = value["next_cursor"].as_str().map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        self.with(|dir| dir.members = Some(members));
    }

    /// Looks up every name the given tasks (or a change's arguments) need.
    async fn prepare<T: ToolRunner>(&self, tools: &T, tasks: &[&Value]) {
        let projects: HashSet<String> = tasks
            .iter()
            .filter_map(|task| task["project_id"].as_str().map(str::to_owned))
            .collect();
        if !projects.is_empty() {
            self.load_projects(tools).await;
        }
        for project in &projects {
            self.load_statuses(tools, project).await;
        }
        if tasks.iter().any(|task| {
            task["assignee_ids"]
                .as_array()
                .is_some_and(|ids| !ids.is_empty())
        }) {
            self.load_members(tools).await;
        }
    }

    fn project(&self, id: &str) -> Option<Project> {
        self.with(|dir| dir.projects.as_ref().and_then(|all| all.get(id).cloned()))
    }

    fn status(&self, id: &str) -> Option<Status> {
        self.with(|dir| dir.statuses.get(id).cloned())
    }

    fn member(&self, id: &str) -> Option<String> {
        self.with(|dir| dir.members.as_ref().and_then(|all| all.get(id).cloned()))
    }
}

fn items(value: &Value) -> Vec<&Value> {
    match value {
        Value::Array(list) => list.iter().collect(),
        Value::Object(_) => value["items"]
            .as_array()
            .map(|list| list.iter().collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// A string field as plain text, cut to a sane length. Never Markdown: the panel shows it as text.
fn text(value: &Value) -> String {
    value
        .as_str()
        .unwrap_or_default()
        .chars()
        // no control characters, and no bidi overrides or isolates that could reorder what the user reads
        .filter(|c| {
            !c.is_control() && !matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
        })
        .take(MAX_TITLE)
        .collect()
}

/// An id that may go in a path: letters, digits, `-` and `_` only.
fn path_id(value: &Value) -> Option<&str> {
    value.as_str().filter(|id| {
        !id.is_empty()
            && id.len() <= 64
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    })
}

pub(crate) fn task_href(id: &Value, workspace_id: Id) -> Option<String> {
    path_id(id).map(|id| format!("/tasks/{id}?workspace={workspace_id}"))
}

fn doc_href(id: &Value, workspace_id: Id) -> Option<String> {
    path_id(id).map(|id| format!("/docs/{id}?workspace={workspace_id}"))
}

fn project_href(id: &Value, workspace_id: Id) -> Option<String> {
    path_id(id).map(|id| format!("/tasks?project={id}&workspace={workspace_id}"))
}

/// `2026-11-30T00:00:00.000Z` → `2026-11-30`; anything else → `null`.
fn day(value: &Value) -> Value {
    value
        .as_str()
        .filter(|raw| raw.len() >= 10 && raw.is_char_boundary(10))
        .map(|raw| &raw[..10])
        .filter(|date| {
            date.chars().enumerate().all(|(at, c)| {
                if at == 4 || at == 7 {
                    c == '-'
                } else {
                    c.is_ascii_digit()
                }
            })
        })
        .map_or(Value::Null, |date| json!(date))
}

fn priority(value: &Value) -> Value {
    match value.as_str() {
        Some(p @ ("none" | "low" | "medium" | "high" | "urgent")) => json!(p),
        _ => json!("none"),
    }
}

fn color(value: &str) -> Value {
    let hex = value.strip_prefix('#').unwrap_or("");
    if matches!(hex.len(), 3 | 6 | 8) && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        json!(value)
    } else {
        Value::Null
    }
}

fn status_view(status: Option<Status>) -> Value {
    status.map_or(Value::Null, |status| {
        json!({"name": status.name, "category": status.category, "color": color(&status.color)})
    })
}

fn assignees(names: &Names, ids: &Value) -> Value {
    let list: Vec<Value> = ids
        .as_array()
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .take(10)
                .map(|id| json!({"name": names.member(id).unwrap_or_else(|| "Unknown member".into())}))
                .collect()
        })
        .unwrap_or_default();
    json!(list)
}

/// The card of one task (a tool's task record). `None` when the record has no usable id.
fn task_card(names: &Names, task: &Value, workspace_id: Id) -> Option<Value> {
    let href = task_href(&task["id"], workspace_id)?;
    let project_id = task["project_id"].as_str().unwrap_or_default();
    let project = names.project(project_id);
    let key = task["identifier"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| {
            let project = project.as_ref()?;
            let number = task["number"].as_i64()?;
            Some(format!("{}-{number}", project.key))
        })
        .filter(|key| is_task_key(key));
    Some(json!({
        "kind": "task",
        "id": task["id"],
        "key": key,
        "title": text(&task["title"]),
        "href": href,
        "status": status_view(task["status_id"].as_str().and_then(|id| names.status(id))),
        "priority": priority(&task["priority"]),
        "assignees": assignees(names, &task["assignee_ids"]),
        "due": day(&task["due_at"]),
        "project": project.map(|project| json!({"name": project.name, "key": project.key})),
    }))
}

/// `GEN-12`: a project key (letters, digits, `_`, at most 20) and a number.
pub(crate) fn is_task_key(key: &str) -> bool {
    let Some((project, number)) = key.split_once('-') else {
        return false;
    };
    (1..=20).contains(&project.len())
        && project
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !number.is_empty()
        && number.len() <= 15
        && number.chars().all(|c| c.is_ascii_digit())
        && !number.starts_with('0')
}

fn is_done(category: &str) -> bool {
    DONE.contains(&category)
}

/// Exact counts from `(project, status, number of tasks)` rows: all, open and done, per project and per status (in
/// first-seen order). `complete`: the counts cover every task that matches, not only the ones listed (`shown`). A
/// task whose status could not be read leaves `open` and `done` unknown (`null`) rather than counting it as open.
fn tally(
    rows: impl IntoIterator<Item = (Option<Project>, Option<Status>, usize)>,
    complete: bool,
    shown: usize,
) -> Value {
    let mut by_project: Vec<(String, String, usize)> = Vec::new();
    let mut by_status: Vec<(String, String, usize)> = Vec::new();
    let (mut all, mut done, mut unknown) = (0, 0, false);
    for (project, status, count) in rows {
        all += count;
        let (name, key) = project.map_or_else(
            || ("No project".to_owned(), String::new()),
            |project| (project.name, project.key),
        );
        match by_project
            .iter_mut()
            .find(|(n, k, _)| *n == name && *k == key)
        {
            Some(entry) => entry.2 += count,
            None => by_project.push((name, key, count)),
        }
        let (name, category) = match status {
            Some(status) => {
                if is_done(&status.category) {
                    done += count;
                }
                (status.name, status.category)
            }
            None => {
                unknown = true;
                ("Unknown status".to_owned(), String::new())
            }
        };
        match by_status
            .iter_mut()
            .find(|(n, c, _)| *n == name && *c == category)
        {
            Some(entry) => entry.2 += count,
            None => by_status.push((name, category, count)),
        }
    }
    let known = |n: usize| if unknown { Value::Null } else { json!(n) };
    json!({
        "tasks": all,
        "open": known(all - done),
        "done": known(done),
        "byProject": by_project.into_iter().map(|(name, key, count)| json!({"name": name, "key": key, "count": count})).collect::<Vec<_>>(),
        "byStatus": by_status.into_iter().map(|(name, category, count)| json!({"name": name, "category": category, "count": count})).collect::<Vec<_>>(),
        "complete": complete,
        "shown": shown,
    })
}

/// Totals of the listed tasks only (when the server's own count failed). Complete when no more pages follow.
pub(crate) fn page_totals(names: &Names, tasks: &[&Value], more: bool) -> Value {
    let rows = tasks.iter().map(|task| {
        (
            names.project(task["project_id"].as_str().unwrap_or_default()),
            names.status(task["status_id"].as_str().unwrap_or_default()),
            1,
        )
    });
    tally(rows.collect::<Vec<_>>(), !more, tasks.len())
}

/// Totals of every task a `list_tasks` call matches, from the server's grouped count (`COUNT_TASKS`) with the same
/// filter. `None` when the count failed.
async fn query_totals<T: ToolRunner>(
    names: &Names,
    tools: &T,
    input: &Value,
    shown: usize,
) -> Option<Value> {
    let mut filter = input.as_object().cloned().unwrap_or_default();
    filter.remove("cursor");
    filter.remove("limit");
    let counted = tools
        .lookup(COUNT_TASKS, Value::Object(filter))
        .await
        .ok()?;
    let rows: Vec<(String, String, usize)> = items(&counted)
        .into_iter()
        .filter_map(|row| {
            Some((
                row["project_id"].as_str()?.to_owned(),
                row["status_id"].as_str()?.to_owned(),
                usize::try_from(row["count"].as_u64()?).ok()?,
            ))
        })
        .collect();
    let projects: HashSet<&str> = rows
        .iter()
        .map(|(project, _, _)| project.as_str())
        .collect();
    if !projects.is_empty() {
        names.load_projects(tools).await;
    }
    for project in projects {
        names.load_statuses(tools, project).await;
    }
    Some(tally(
        rows.iter()
            .map(|(project, status, count)| (names.project(project), names.status(status), *count))
            .collect::<Vec<_>>(),
        true,
        shown,
    ))
}

/// What the model is told with a list of tasks: the exact counts, so it never has to count.
pub(crate) fn totals_for_model(totals: &Value) -> Value {
    let mut by_project = Map::new();
    for entry in totals["byProject"].as_array().into_iter().flatten() {
        let mut name = text(&entry["name"]);
        let key = text(&entry["key"]);
        if !key.is_empty() {
            name = format!("{name} ({key})");
        }
        by_project.insert(name, entry["count"].clone());
    }
    // statuses of different projects can share a name: their counts add up
    let mut by_status: BTreeMap<String, u64> = BTreeMap::new();
    for entry in totals["byStatus"].as_array().into_iter().flatten() {
        *by_status.entry(text(&entry["name"])).or_default() += entry["count"].as_u64().unwrap_or(0);
    }
    json!({
        "tasks": totals["tasks"],
        "open": totals["open"],
        "done": totals["done"],
        "by_project": by_project,
        "by_status": by_status,
        "complete": totals["complete"],
        "shown": totals["shown"],
    })
}

/// The text the model gets of a `list_tasks` result: `totals` and `next_cursor` first, then as many whole tasks as fit
/// in `limit` bytes (with `items_omitted` when some did not), so the counts are never cut off.
pub(crate) fn list_model_text(value: &Value, limit: usize) -> String {
    let all = items(value);
    let mut text = format!(
        "{{\"totals\":{},\"next_cursor\":{}",
        value["totals"], value["next_cursor"]
    );
    let mut list = String::new();
    let mut shown = 0;
    for item in &all {
        let item = item.to_string();
        // room for the closing text and the omitted count
        if text.len() + list.len() + item.len() + 64 > limit {
            break;
        }
        if shown > 0 {
            list.push(',');
        }
        list.push_str(&item);
        shown += 1;
    }
    if shown < all.len() {
        text.push_str(&format!(",\"items_omitted\":{}", all.len() - shown));
    }
    text.push_str(&format!(",\"items\":[{list}]}}"));
    text
}

/// The card of a read tool's result, if it has one. For `list_tasks` the result the model sees also gets `totals`.
pub(crate) async fn read_card<T: ToolRunner>(
    names: &Names,
    tools: &T,
    tool: &str,
    input: &Value,
    value: &mut Value,
    workspace_id: Id,
) -> Option<Value> {
    match tool {
        "get_task" => {
            names.prepare(tools, &[&*value]).await;
            task_card(names, value, workspace_id)
        }
        "list_tasks" => {
            let tasks: Vec<&Value> = items(value);
            names.prepare(tools, &tasks).await;
            let more = value["next_cursor"].as_str().is_some_and(|c| !c.is_empty());
            let totals = match query_totals(names, tools, input, tasks.len()).await {
                Some(totals) => totals,
                None => page_totals(names, &tasks, more),
            };
            // the card keeps the first tasks only (the display log stays small); its totals count them all
            let cards: Vec<Value> = tasks
                .iter()
                .take(MAX_LIST_ITEMS)
                .filter_map(|task| task_card(names, task, workspace_id))
                .collect();
            if let Some(object) = value.as_object_mut() {
                object.insert("totals".into(), totals_for_model(&totals));
            }
            Some(json!({"kind": "tasks", "totals": totals, "items": cards}))
        }
        "list_projects" => {
            let list: Vec<Value> = items(value)
                .into_iter()
                .take(MAX_LIST_ITEMS)
                .filter_map(|project| {
                    Some(json!({
                        "name": text(&project["name"]),
                        "key": text(&project["key"]),
                        "href": project_href(&project["id"], workspace_id)?,
                    }))
                })
                .collect();
            (!list.is_empty()).then(|| json!({"kind": "projects", "items": list}))
        }
        "list_docs" | "search_docs" | "get_doc" => {
            let docs: Vec<&Value> = if tool == "get_doc" {
                vec![&*value]
            } else {
                items(value)
            };
            let list: Vec<Value> = docs
                .into_iter()
                .take(MAX_LIST_ITEMS)
                .filter_map(|doc| {
                    Some(json!({
                        "title": text(&doc["title"]),
                        "updatedAt": doc["updated_at"].as_str().map_or(Value::Null, |at| json!(at)),
                        "href": doc_href(&doc["id"], workspace_id)?,
                    }))
                })
                .collect();
            (!list.is_empty()).then(|| json!({"kind": "docs", "items": list}))
        }
        _ => None,
    }
}

/// The card of a change's result (the task it created or updated).
pub(crate) async fn change_card<T: ToolRunner>(
    names: &Names,
    tools: &T,
    tool: &str,
    value: &Value,
    workspace_id: Id,
) -> Option<Value> {
    match tool {
        "create_task" | "update_task" => {
            names.prepare(tools, &[value]).await;
            task_card(names, value, workspace_id)
        }
        _ => None,
    }
}

/// One line of the summary of a message's changes: what was done (past tense) to what, and its page. The name comes
/// from the change's result, else from its (approved) arguments.
pub(crate) fn change_entry(
    tool: &str,
    input: &Value,
    value: &Value,
    card: Option<&Value>,
    href: Option<String>,
) -> Value {
    let (verb, object) = tool.split_once('_').unwrap_or((tool, ""));
    let past = match verb {
        "create" => "Created".to_owned(),
        "update" => "Updated".to_owned(),
        "delete" => "Deleted".to_owned(),
        "restore" => "Restored".to_owned(),
        "move" => "Moved".to_owned(),
        "duplicate" => "Duplicated".to_owned(),
        "trash" => "Trashed".to_owned(),
        "add" => "Added".to_owned(),
        "remove" => "Removed".to_owned(),
        "mark" => "Marked".to_owned(),
        "set" => "Set".to_owned(),
        other => {
            let mut chars = other.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        }
    };
    let (verb, key, title) = match card {
        // a task card names the task by its key
        Some(card) => (past, card["key"].clone(), text(&card["title"])),
        None => (
            format!("{past} {}", object.replace('_', " "))
                .trim()
                .to_owned(),
            Value::Null,
            [
                &value["title"],
                &value["name"],
                &input["title"],
                &input["name"],
            ]
            .into_iter()
            .map(text)
            .find(|title| !title.is_empty())
            .unwrap_or_default(),
        ),
    };
    let href = card
        .and_then(|card| card["href"].as_str().map(str::to_owned))
        .or(href);
    json!({"verb": verb, "key": key, "title": title, "href": href})
}

/// The panel's view of a result without a card: the view as it is, but never a `card` field of the tool's own
/// (only the server's cards are cards).
pub(crate) fn strip_card(client: &str) -> String {
    match serde_json::from_str::<Value>(client) {
        Ok(Value::Object(mut object)) if object.contains_key("card") => {
            object.remove("card");
            Value::Object(object).to_string()
        }
        _ => client.to_owned(),
    }
}

pub(crate) fn summary_card(changes: &[Value]) -> Value {
    json!({"kind": "summary", "changes": changes})
}

/// The preview on an approval card, from the server's own reading of the item (never the client's or the model's):
/// for `update_task` the current task and each changed field before → after; for `create_task` the new task as a
/// card. Other changes show their arguments only.
pub(crate) async fn approval_preview<T: ToolRunner>(
    names: &Names,
    tools: &T,
    tool: &str,
    input: &Value,
    workspace_id: Id,
) -> Option<Value> {
    match tool {
        "create_task" => {
            names.prepare(tools, &[input]).await;
            let project = names.project(input["project_id"].as_str().unwrap_or_default());
            Some(json!({
                "kind": "create",
                "task": {
                    "kind": "task",
                    "id": Value::Null,
                    "key": Value::Null,
                    "title": text(&input["title"]),
                    "href": Value::Null,
                    "status": status_view(input["status_id"].as_str().and_then(|id| names.status(id))),
                    "priority": priority(&input["priority"]),
                    "assignees": assignees(names, &input["assignee_ids"]),
                    "due": day(&input["due_at"]),
                    "project": project.map(|project| json!({"name": project.name, "key": project.key})),
                },
            }))
        }
        "update_task" => {
            let task_ref = input["task_id"].as_str()?;
            let current = tools
                .lookup("get_task", json!({"task_id": task_ref}))
                .await
                .ok()?;
            // `assignee_ids` of an update replaces the list
            let mut wanted = current.clone();
            if let Some(ids) = input.get("assignee_ids") {
                wanted["assignee_ids"] = ids.clone();
            }
            names.prepare(tools, &[&current, &wanted]).await;
            let card = task_card(names, &current, workspace_id)?;
            let mut changes = Vec::new();
            let mut push = |field: &str, before: String, after: String| {
                if before != after {
                    changes.push(json!({"field": field, "before": before, "after": after}));
                }
            };
            if let Some(status_id) = input["status_id"].as_str() {
                let name = |id: &str| {
                    names
                        .status(id)
                        .map_or_else(|| "Unknown status".to_owned(), |status| status.name)
                };
                push(
                    "Status",
                    name(current["status_id"].as_str().unwrap_or_default()),
                    name(status_id),
                );
            }
            if input["title"].is_string() {
                push("Title", text(&current["title"]), text(&input["title"]));
            }
            if input["priority"].is_string() {
                let label =
                    |value: &Value| priority_label(priority(value).as_str().unwrap_or("none"));
                push(
                    "Priority",
                    label(&current["priority"]),
                    label(&input["priority"]),
                );
            }
            if input.get("assignee_ids").is_some() {
                let people = |ids: &Value| {
                    let list: Vec<String> = assignees(names, ids)
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|person| text(&person["name"]))
                        .collect();
                    if list.is_empty() {
                        "Nobody".to_owned()
                    } else {
                        list.join(", ")
                    }
                };
                push(
                    "Assignees",
                    people(&current["assignee_ids"]),
                    people(&wanted["assignee_ids"]),
                );
            }
            if let Some(due) = input.get("due_at") {
                let label = |value: &Value| {
                    day(value)
                        .as_str()
                        .map_or_else(|| "No due date".to_owned(), str::to_owned)
                };
                push("Due", label(&current["due_at"]), label(due));
            }
            if input["description"].is_string() {
                let clip = |value: &Value| {
                    let full = value.as_str().unwrap_or_default();
                    let cut: String = full.chars().take(200).collect();
                    if cut.len() < full.len() {
                        format!("{cut}…")
                    } else {
                        cut
                    }
                };
                push(
                    "Description",
                    clip(&current["description"]),
                    clip(&input["description"]),
                );
            }
            Some(json!({"kind": "change", "task": card, "changes": changes}))
        }
        _ => None,
    }
}

fn priority_label(priority: &str) -> String {
    match priority {
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        "urgent" => "Urgent",
        _ => "No priority",
    }
    .to_owned()
}

/// The panel's view of a result with a card: the view as an object, with `card` set (replacing any field of that
/// name the result had).
pub(crate) fn attach(client: &str, card: Value) -> String {
    let mut view = match serde_json::from_str::<Value>(client) {
        Ok(Value::Object(object)) => Value::Object(object),
        Ok(Value::Array(list)) => json!({"summary": format!("{} results", list.len())}),
        _ => json!({}),
    };
    view["card"] = card;
    view.to_string()
}
