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
/// The most tasks a list card carries (a list call returns at most 100).
const MAX_LIST_ITEMS: usize = 100;
const MAX_TITLE: usize = 300;
/// The step that carries the summary card at the end of a message with changes (not a tool the model can call).
pub(crate) const SUMMARY_TOOL: &str = "orbit_summary";

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
        // a few pages at most: names beyond them show as "1 more person"
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
        if projects.is_empty() && tasks.iter().all(|task| task["status_id"].is_null()) {
            return;
        }
        self.load_projects(tools).await;
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
        .filter(|c| !c.is_control() || *c == ' ')
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
            date.chars()
                .enumerate()
                .all(|(at, c)| if at == 4 || at == 7 { c == '-' } else { c.is_ascii_digit() })
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
        && project.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !number.is_empty()
        && number.len() <= 15
        && number.chars().all(|c| c.is_ascii_digit())
        && !number.starts_with('0')
}

fn is_done(category: &str) -> bool {
    DONE.contains(&category)
}

/// Exact counts of a list of task cards: all, open and done, per project and per status (in first-seen order).
pub(crate) fn totals(cards: &[Value], more: bool) -> Value {
    let mut by_project: Vec<(String, String, usize)> = Vec::new();
    let mut by_status: Vec<(String, String, usize)> = Vec::new();
    let mut done = 0;
    for card in cards {
        let project = &card["project"];
        let (name, key) = (text(&project["name"]), text(&project["key"]));
        let name = if name.is_empty() { "No project".into() } else { name };
        match by_project.iter_mut().find(|(n, k, _)| *n == name && *k == key) {
            Some(entry) => entry.2 += 1,
            None => by_project.push((name, key, 1)),
        }
        let status = &card["status"];
        let (name, category) = (text(&status["name"]), text(&status["category"]));
        let name = if name.is_empty() { "No status".into() } else { name };
        if is_done(&category) {
            done += 1;
        }
        match by_status.iter_mut().find(|(n, c, _)| *n == name && *c == category) {
            Some(entry) => entry.2 += 1,
            None => by_status.push((name, category, 1)),
        }
    }
    json!({
        "tasks": cards.len(),
        "open": cards.len() - done,
        "done": done,
        "byProject": by_project.into_iter().map(|(name, key, count)| json!({"name": name, "key": key, "count": count})).collect::<Vec<_>>(),
        "byStatus": by_status.into_iter().map(|(name, category, count)| json!({"name": name, "category": category, "count": count})).collect::<Vec<_>>(),
        "complete": !more,
    })
}

/// What the model is told with a list of tasks: the exact counts, so it never has to count.
pub(crate) fn totals_for_model(totals: &Value) -> Value {
    let named = |list: &Value| -> Map<String, Value> {
        let mut out = Map::new();
        for entry in list.as_array().into_iter().flatten() {
            let mut name = text(&entry["name"]);
            let key = text(&entry["key"]);
            if !key.is_empty() {
                name = format!("{name} ({key})");
            }
            out.insert(name, entry["count"].clone());
        }
        out
    };
    let mut by_status = BTreeMap::new();
    for entry in totals["byStatus"].as_array().into_iter().flatten() {
        by_status.insert(text(&entry["name"]), entry["count"].clone());
    }
    json!({
        "tasks": totals["tasks"],
        "open": totals["open"],
        "done": totals["done"],
        "by_project": named(&totals["byProject"]),
        "by_status": by_status,
        "complete": totals["complete"],
    })
}

/// The card of a read tool's result, if it has one. For `list_tasks` the result the model sees also gets `totals`.
pub(crate) async fn read_card<T: ToolRunner>(
    names: &Names,
    tools: &T,
    tool: &str,
    value: &mut Value,
    workspace_id: Id,
) -> Option<Value> {
    match tool {
        "get_task" => {
            names.prepare(tools, &[&*value]).await;
            task_card(names, value, workspace_id)
        }
        "list_tasks" => {
            let tasks: Vec<&Value> = items(value).into_iter().take(MAX_LIST_ITEMS).collect();
            names.prepare(tools, &tasks).await;
            let cards: Vec<Value> = tasks
                .iter()
                .filter_map(|task| task_card(names, task, workspace_id))
                .collect();
            let more = value["next_cursor"].as_str().is_some_and(|c| !c.is_empty());
            let totals = totals(&cards, more);
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

/// One line of the summary of a message's changes: what was done to what, and its page.
pub(crate) fn change_entry(tool: &str, input: &Value, card: Option<&Value>, href: Option<String>) -> Value {
    let verb = match tool {
        "create_task" => "Created".to_owned(),
        "update_task" => "Updated".to_owned(),
        _ => {
            let words = tool.replace('_', " ");
            let mut chars = words.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        }
    };
    let (key, title) = match card {
        Some(card) => (card["key"].clone(), text(&card["title"])),
        None => (
            Value::Null,
            ["title", "name"]
                .iter()
                .map(|field| text(&input[*field]))
                .find(|title| !title.is_empty())
                .unwrap_or_default(),
        ),
    };
    let href = card
        .and_then(|card| card["href"].as_str().map(str::to_owned))
        .or(href);
    json!({"verb": verb, "key": key, "title": title, "href": href})
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
                let label = |value: &Value| priority_label(priority(value).as_str().unwrap_or("none"));
                push("Priority", label(&current["priority"]), label(&input["priority"]));
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
                push("Assignees", people(&current["assignee_ids"]), people(&wanted["assignee_ids"]));
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
                    if cut.len() < full.len() { format!("{cut}…") } else { cut }
                };
                push("Description", clip(&current["description"]), clip(&input["description"]));
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
