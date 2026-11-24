//! The tool registry: every MCP tool (`crate::mcp::all_tools`, the same definitions external
//! clients get) with the labels Ask AI needs: its risk, whether the assistant may use it, the
//! page that shows its result, and whether following along opens that page after a read.
//!
//! Ask AI acts as the signed-in member with a narrower set than MCP: no delete or trash tools,
//! and nothing that sends anything outside the workspace (MCP has no such tool today; mail
//! sending, invitations and webhooks stay out of this list when they arrive).

use std::collections::BTreeSet;

use orbit_platform::Id;
use rmcp::model::Tool;
use serde_json::{Value, json};

use super::model::ToolSpec;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Risk {
    /// Runs at once.
    Read,
    /// Waits for the user's approval; the user may allow it for the rest of the conversation.
    Write,
    /// Overwrites or moves content: always waits for approval.
    Dangerous,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Exposure {
    /// In every request.
    Core,
    /// Listed by `load_tools`; in the requests after the model loads it.
    Loadable,
    /// Not available to the assistant.
    Hidden,
}

/// The page that shows a tool's result. Ids come from the result (its `url`, then its `id`) or
/// the arguments.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Page {
    /// Explicitly none: the tool's result has no page of its own.
    None,
    Fixed(&'static str),
    Task,
    TrashedTask,
    Doc,
    Database,
    /// `/tasks/projects/:id/settings/<section>`.
    ProjectSettings(&'static str),
    Chat,
    /// `/views/:id`.
    View,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Label {
    pub risk: Risk,
    pub exposure: Exposure,
    pub page: Page,
    /// A read that shows one thing (a task, a doc): following along opens its page.
    pub shows: bool,
    /// A change with free text that Orbit shows as Markdown or HTML somewhere (a comment, a description, a doc, a
    /// database cell): it always gets an approval card, never "Allow in this chat". Derived from the schema.
    pub free_text: bool,
}

const fn label(risk: Risk, exposure: Exposure, page: Page, shows: bool) -> Label {
    Label {
        risk,
        exposure,
        page,
        shows,
        free_text: false,
    }
}

/// Arguments whose text Orbit renders as Markdown or HTML (comments, descriptions, docs, database cells, chat):
/// content the assistant writes there could load something from outside wherever it is shown later.
pub(crate) const RICH_TEXT_ARGS: [&str; 9] = [
    "description",
    "markdown",
    "body",
    "text",
    "content",
    "values",
    "message",
    "quote",
    // a database property's config: a button can hold an `open_url` action
    "config",
];

/// String arguments that are shown as plain text or are structured (ids, names, titles, enums, dates, colors,
/// filters). A string argument of a write tool must be in one of the two lists (a registry test checks it).
#[cfg(test)]
pub(crate) const PLAIN_ARGS: &[&str] = &[
    "workspace_id",
    "project_id",
    "status_id",
    "task_id",
    "other_task_id",
    "relation_id",
    "parent_task_id",
    "assignee_ids",
    "label_ids",
    "milestone_id",
    "cycle_id",
    "duplicate_of_id",
    "comment_id",
    "page_id",
    "parent_id",
    "teamspace_id",
    "database_id",
    "row_id",
    "row_ids",
    "view_id",
    "property",
    "property_id",
    "link_id",
    "template_id",
    "version_id",
    "title",
    "name",
    "key",
    "color",
    "icon",
    "priority",
    "category",
    "type",
    "kind",
    "layout",
    "property_type",
    "starts_at",
    "ends_at",
    "target_date",
    "due_at",
    "due_start_at",
    "frequency",
    "weekdays",
    "base",
    "position",
    "placement",
    "label_id",
    "from_view_id",
    "host_page_id",
    "thread_id",
    "target_at",
    "add_options",
    "columns",
    "filter",
    "group_by",
    "sorts",
    "tz",
    "cursor",
    "id",
    "after_row_id",
    "from_row_id",
    "notification_id",
    "view_ids",
    "state",
    "visibility",
];

/// The string-valued argument names of a schema (nested objects and `$defs` included). A free map or an
/// untyped value (any JSON) counts as a string argument too.
pub(crate) fn string_args(schema: &Value) -> Vec<String> {
    fn walk(node: &Value, defs: &Value, name: Option<&str>, out: &mut Vec<String>, depth: usize) {
        if depth > 8 {
            return;
        }
        if let Some(reference) = node["$ref"].as_str() {
            let key = reference.rsplit('/').next().unwrap_or_default();
            walk(&defs[key], defs, name, out, depth + 1);
            return;
        }
        for union in ["anyOf", "oneOf", "allOf"] {
            if let Some(options) = node[union].as_array() {
                for option in options {
                    walk(option, defs, name, out, depth + 1);
                }
            }
        }
        let types: Vec<&str> = match &node["type"] {
            Value::String(kind) => vec![kind.as_str()],
            Value::Array(kinds) => kinds.iter().filter_map(Value::as_str).collect(),
            _ => vec![],
        };
        let untyped = types.is_empty()
            && node.get("$ref").is_none()
            && !["anyOf", "oneOf", "allOf", "enum", "const"]
                .iter()
                .any(|key| node.get(*key).is_some());
        if let Some(name) = name
            && (types.contains(&"string")
                || untyped
                || (types.contains(&"object") && node.get("properties").is_none()))
        {
            out.push(name.to_owned());
        }
        if let Some(properties) = node["properties"].as_object() {
            for (child, schema) in properties {
                walk(schema, defs, Some(child), out, depth + 1);
            }
        }
        if let Some(items) = node.get("items") {
            walk(items, defs, name, out, depth + 1);
        }
    }
    let defs = schema
        .get("$defs")
        .or_else(|| schema.get("definitions"))
        .cloned()
        .unwrap_or(Value::Null);
    let mut out = Vec::new();
    walk(schema, &defs, None, &mut out, 0);
    out.sort();
    out.dedup();
    out
}

/// Whether a tool takes text that Orbit renders as Markdown or HTML (see [`RICH_TEXT_ARGS`]).
pub(crate) fn takes_free_text(tool: &Tool) -> bool {
    string_args(&Value::Object((*tool.input_schema).clone()))
        .iter()
        .any(|name| RICH_TEXT_ARGS.contains(&name.as_str()))
}

/// The labels of an MCP tool, or `None` for a tool this table does not know yet (a test keeps
/// the table complete). There is deliberately no catch-all arm.
pub(crate) fn label_of(name: &str) -> Option<Label> {
    use Exposure::{Core, Hidden, Loadable};
    use Page::{Chat, Database, Doc, Fixed, ProjectSettings, Task, TrashedTask, View};
    use Risk::{Dangerous, Read, Write};
    Some(match name {
        // reads
        "get_me" => label(Read, Core, Page::None, false),
        "list_projects" | "list_tasks" => label(Read, Core, Fixed("/tasks"), false),
        "get_task" => label(Read, Core, Task, true),
        "list_task_statuses" => label(Read, Core, ProjectSettings("statuses"), false),
        "list_members" => label(Read, Core, Fixed("/settings/members"), false),
        "list_docs" => label(Read, Core, Fixed("/docs"), false),
        "get_doc" => label(Read, Core, Doc, true),
        "search_messages" => label(Read, Core, Fixed("/chat"), false),
        "list_labels" => label(Read, Loadable, ProjectSettings("labels"), false),
        "list_milestones" => label(Read, Loadable, ProjectSettings("milestones"), false),
        "list_cycles" => label(Read, Loadable, ProjectSettings("cycles"), false),
        "list_comments" | "list_task_relations" | "list_task_activity" => {
            label(Read, Loadable, Task, false)
        }
        "list_task_trash" => label(Read, Loadable, Fixed("/tasks-trash"), false),
        "get_trashed_task" => label(Read, Loadable, TrashedTask, true),
        "list_teamspaces" => label(Read, Loadable, Fixed("/docs"), false),
        "list_doc_versions" | "get_doc_version" | "list_doc_comments" => {
            label(Read, Loadable, Doc, false)
        }
        "list_doc_trash" => label(Read, Loadable, Fixed("/docs/trash"), false),
        "get_database" => label(Read, Loadable, Database, true),
        "query_database" | "list_database_views" => label(Read, Loadable, Database, false),
        "list_conversations" => label(Read, Loadable, Fixed("/chat"), false),
        "list_messages" | "get_thread" => label(Read, Loadable, Chat, true),
        // writes
        "create_task" | "update_task" | "create_comment" => label(Write, Core, Task, false),
        "restore_task" | "update_comment" | "create_task_relation" => {
            label(Write, Loadable, Task, false)
        }
        "create_project" => label(Write, Loadable, Fixed("/tasks"), false),
        "update_project" => label(Write, Loadable, ProjectSettings("general"), false),
        "create_task_status" | "update_task_status" => {
            label(Write, Loadable, ProjectSettings("statuses"), false)
        }
        "create_label" | "update_label" => label(Write, Loadable, ProjectSettings("labels"), false),
        "create_milestone" | "update_milestone" => {
            label(Write, Loadable, ProjectSettings("milestones"), false)
        }
        "create_cycle" | "update_cycle" => label(Write, Loadable, ProjectSettings("cycles"), false),
        "create_doc" => label(Write, Core, Doc, false),
        "restore_doc" | "create_doc_comment" => label(Write, Loadable, Doc, false),
        "create_database"
        | "create_database_row"
        | "update_database_row"
        | "create_database_view"
        | "update_database_view"
        | "create_database_property"
        | "create_database_link" => label(Write, Loadable, Database, false),
        // overwrite or move: always asked
        "update_doc" | "move_doc" => label(Dangerous, Loadable, Doc, false),
        "update_database_property" => label(Dangerous, Loadable, Database, false),
        // deletes: not for the assistant in this step
        "delete_task" | "delete_comment" | "delete_task_relation" => {
            label(Dangerous, Hidden, Task, false)
        }
        "delete_task_status" => label(Dangerous, Hidden, ProjectSettings("statuses"), false),
        "delete_label" => label(Dangerous, Hidden, ProjectSettings("labels"), false),
        "delete_milestone" => label(Dangerous, Hidden, ProjectSettings("milestones"), false),
        "delete_cycle" => label(Dangerous, Hidden, ProjectSettings("cycles"), false),
        "delete_doc" => label(Dangerous, Hidden, Fixed("/docs/trash"), false),
        "delete_database_property"
        | "delete_database_view"
        | "delete_database_link"
        | "trash_database_rows" => label(Dangerous, Hidden, Database, false),
        // more reads
        "search_docs" | "list_recent_docs" | "list_favorite_docs" => {
            label(Read, Loadable, Fixed("/docs"), false)
        }
        "list_doc_descendants" => label(Read, Loadable, Doc, false),
        "list_task_attachments" | "list_task_github_links" => label(Read, Loadable, Task, false),
        "list_project_members" => label(Read, Loadable, ProjectSettings("access"), false),
        "list_project_trash" | "get_trashed_project" => {
            label(Read, Loadable, Fixed("/tasks-trash"), false)
        }
        "preview_database_formula" => label(Read, Loadable, Database, false),
        "list_notifications" => label(Read, Loadable, Fixed("/activity"), false),
        "list_views" => label(Read, Loadable, Fixed("/views"), false),
        "get_view" => label(Read, Loadable, View, true),
        // more changes
        "reorder_tasks" => label(Write, Loadable, Fixed("/tasks"), false),
        "reorder_task_statuses" => label(Write, Loadable, ProjectSettings("statuses"), false),
        "reorder_milestones" => label(Write, Loadable, ProjectSettings("milestones"), false),
        "restore_project" => label(Write, Loadable, Fixed("/tasks"), false),
        "add_favorite_doc"
        | "remove_favorite_doc"
        | "move_favorite_doc"
        | "duplicate_doc"
        | "set_doc_lock"
        | "update_doc_comment"
        | "resolve_doc_thread"
        | "reopen_doc_thread" => label(Write, Loadable, Doc, false),
        "create_teamspace" | "update_teamspace" | "move_teamspace" => {
            label(Write, Loadable, Fixed("/docs"), false)
        }
        "move_database_row" | "create_database_template" | "update_database_template" => {
            label(Write, Loadable, Database, false)
        }
        "mark_notification_read" => label(Write, Loadable, Fixed("/activity"), false),
        "create_view" | "update_view" | "set_view_favorite" | "reorder_view_favorites" => {
            label(Write, Loadable, View, false)
        }
        // many rows at once, or a doc's content replaced by an older version: always asked
        "bulk_update_tasks" => label(Dangerous, Loadable, Fixed("/tasks"), false),
        "move_unfinished_cycle_tasks" => {
            label(Dangerous, Loadable, ProjectSettings("cycles"), false)
        }
        "mark_all_notifications_read" => label(Dangerous, Loadable, Fixed("/activity"), false),
        "restore_doc_version" => label(Dangerous, Loadable, Doc, false),
        // not for the assistant: removing or emptying, project access, files as base64, and all of mail (mail
        // reaches people outside the workspace, and its threads are outside content)
        "delete_project" | "purge_project" | "empty_task_trash" | "delete_task_attachment" => {
            label(Dangerous, Hidden, Fixed("/tasks-trash"), false)
        }
        "purge_doc" | "empty_doc_trash" | "delete_teamspace" | "delete_doc_comment"
        | "delete_doc_thread" => label(Dangerous, Hidden, Fixed("/docs/trash"), false),
        "delete_database_template" => label(Dangerous, Hidden, Database, false),
        "delete_view" => label(Dangerous, Hidden, Fixed("/views"), false),
        "add_project_member" => label(Write, Hidden, ProjectSettings("access"), false),
        "remove_project_member" => label(Dangerous, Hidden, ProjectSettings("access"), false),
        "upload_task_attachment" => label(Write, Hidden, Task, false),
        "upload_doc_file" => label(Write, Hidden, Doc, false),
        "download_task_attachment" | "download_doc_file" => label(Read, Hidden, Page::None, false),
        name if name.contains("mail") => mail_label(name),
        _ => return None,
    })
}

/// Every mail tool is hidden from the assistant. The risk still follows MCP's hints, so the registry tests hold.
fn mail_label(name: &str) -> Label {
    let risk = if name.starts_with("delete_") || name.starts_with("send_") {
        Risk::Dangerous
    } else if name.starts_with("list_")
        || name.starts_with("get_")
        || name.starts_with("search_")
        || name.starts_with("check_")
    {
        Risk::Read
    } else {
        Risk::Write
    };
    label(risk, Exposure::Hidden, Page::Fixed("/mail"), false)
}

/// Ask AI's own tools, which MCP does not have.
pub(crate) const OPEN_PAGE: &str = "open_page";
pub(crate) const LOAD_TOOLS: &str = "load_tools";
/// Several changes proposed together: the user approves them as one group, item by item (`engine.rs`).
pub(crate) const PROPOSE_CHANGES: &str = "propose_changes";

/// Pages `open_page` may open: the app's own areas.
const OPEN_PAGE_PREFIXES: [&str; 10] = [
    "/home",
    "/tasks",
    "/tasks-trash",
    "/views",
    "/activity",
    "/docs",
    "/chat",
    "/mail",
    "/settings",
    "/profile",
];

fn is_destructive(tool: &Tool) -> bool {
    tool.annotations
        .as_ref()
        .and_then(|annotations| annotations.destructive_hint)
        .unwrap_or(false)
}

#[derive(Clone)]
pub(crate) struct ToolDef {
    pub tool: Tool,
    pub label: Label,
}

#[derive(Clone)]
pub(crate) struct Registry {
    tools: Vec<ToolDef>,
}

impl Registry {
    pub(crate) fn new() -> Self {
        let tools = crate::mcp::all_tools()
            .into_iter()
            .filter_map(|tool| {
                let mut label = label_of(&tool.name)?;
                // A tool MCP marks destructive (it overwrites, replaces or removes) always asks: the flag decides, so
                // the table cannot drift from it.
                if label.risk == Risk::Write && is_destructive(&tool) {
                    label.risk = Risk::Dangerous;
                }
                label.free_text = label.risk != Risk::Read && takes_free_text(&tool);
                Some(ToolDef { tool, label })
            })
            .collect();
        Self { tools }
    }

    pub(crate) fn get(&self, name: &str) -> Option<&ToolDef> {
        self.tools.iter().find(|tool| tool.tool.name == name)
    }

    /// A tool the assistant may call: an MCP tool that is not hidden, or one of its own.
    pub(crate) fn risk(&self, name: &str) -> Option<Risk> {
        if name == OPEN_PAGE || name == LOAD_TOOLS {
            return Some(Risk::Read);
        }
        self.get(name)
            .filter(|tool| tool.label.exposure != Exposure::Hidden)
            .map(|tool| tool.label.risk)
    }

    pub(crate) fn loadable(&self) -> impl Iterator<Item = &ToolDef> {
        self.tools
            .iter()
            .filter(|tool| tool.label.exposure == Exposure::Loadable)
    }

    /// The tools of a request: the core set, the loaded ones and Ask AI's own, in a fixed order
    /// (so the request prefix stays cacheable until the model loads more).
    pub(crate) fn specs(&self, loaded: &BTreeSet<String>) -> Vec<ToolSpec> {
        let mut specs: Vec<ToolSpec> = self
            .tools
            .iter()
            .filter(|tool| {
                tool.label.exposure == Exposure::Core
                    || (tool.label.exposure == Exposure::Loadable
                        && loaded.contains(tool.tool.name.as_ref()))
            })
            .map(|tool| ToolSpec {
                name: tool.tool.name.to_string(),
                description: description(&tool.tool, tool.label.risk),
                input_schema: Value::Object((*tool.tool.input_schema).clone()),
            })
            .collect();
        specs.push(ToolSpec {
            name: OPEN_PAGE.into(),
            description: format!(
                "Open a page of Orbit for the user, when they ask to see or go somewhere. path is an app path that starts with one of {}, e.g. /tasks/<task id> or /docs/<doc id>. /docs opens the Docs area: on a wide screen Orbit shows the first doc there, with every doc listed in the sidebar (there is no separate list page). Do not use it after a tool that already shows its result.",
                OPEN_PAGE_PREFIXES.join(", ")
            ),
            input_schema: json!({
                "type": "object",
                "properties": {"path": {"type": "string", "description": "The app path, without the workspace."}},
                "required": ["path"],
                "additionalProperties": false,
            }),
        });
        specs.push(ToolSpec {
            name: PROPOSE_CHANGES.into(),
            description: format!(
                "Propose several changes at once when one request needs more than one change (for example, create five tasks, or move three tasks to another status). The user sees one approval with a card per change, can untick any of them, and approves the rest together; Orbit then runs the approved ones in this order with the same checks as single calls, stops at the first failure, and tells you what happened to each. Each change is {{tool, input}}: a change tool you could call directly now (loaded ones included) and its exact arguments, at most {} changes. Use a single tool call for a single change. Reads do not go in a batch.",
                super::engine::MAX_BATCH
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "changes": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": super::engine::MAX_BATCH,
                        "items": {
                            "type": "object",
                            "properties": {
                                "tool": {"type": "string", "description": "The change tool, e.g. create_task or update_task."},
                                "input": {"type": "object", "description": "Its arguments, exactly as for a direct call."},
                            },
                            "required": ["tool", "input"],
                            "additionalProperties": false,
                        },
                    },
                },
                "required": ["changes"],
                "additionalProperties": false,
            }),
        });
        let catalog: Vec<String> = self
            .loadable()
            .map(|tool| {
                let summary = tool
                    .tool
                    .description
                    .as_deref()
                    .unwrap_or_default()
                    .split(". ")
                    .next()
                    .unwrap_or_default()
                    .trim_end_matches('.');
                format!(
                    "{} ({}): {summary}",
                    tool.tool.name,
                    risk_word(tool.label.risk)
                )
            })
            .collect();
        specs.push(ToolSpec {
            name: LOAD_TOOLS.into(),
            description: format!(
                "Load more tools by name; they can be called from your next step on. Load only what the task needs. Available:\n{}",
                catalog.join("\n")
            ),
            input_schema: json!({
                "type": "object",
                "properties": {"names": {"type": "array", "items": {"type": "string"}, "minItems": 1, "maxItems": 20}},
                "required": ["names"],
                "additionalProperties": false,
            }),
        });
        specs
    }
}

fn risk_word(risk: Risk) -> &'static str {
    match risk {
        Risk::Read => "read",
        Risk::Write => "change, asks the user",
        Risk::Dangerous => "overwrites, always asks the user",
    }
}

fn description(tool: &Tool, risk: Risk) -> String {
    let base = tool.description.as_deref().unwrap_or_default();
    match risk {
        Risk::Read => base.to_owned(),
        Risk::Write => {
            format!("{base} The user approves this change before it runs.")
        }
        Risk::Dangerous => format!(
            "{base} This overwrites or moves content; the user always approves it before it runs."
        ),
    }
}

/// The app path for `open_page`, or `None` when it is not one of the app's pages.
pub(crate) fn open_page_path(path: &str, workspace_id: Id) -> Option<String> {
    let path = path.trim();
    let (route, _) = path.split_once(['?', '#']).unwrap_or((path, ""));
    let allowed = route.starts_with('/')
        && !route.starts_with("//")
        && !route.contains("..")
        && !route.contains('\\')
        && route.len() <= 300
        && route
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_'))
        && OPEN_PAGE_PREFIXES
            .iter()
            .any(|prefix| route == *prefix || route.starts_with(&format!("{prefix}/")));
    allowed.then(|| with_workspace(route, workspace_id))
}

fn with_workspace(path: &str, workspace_id: Id) -> String {
    if path.contains("workspace=") {
        return path.to_owned();
    }
    let separator = if path.contains('?') { '&' } else { '?' };
    format!("{path}{separator}workspace={workspace_id}")
}

fn id_in(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value[*key]
            .as_str()
            .filter(|id| {
                !id.is_empty()
                    && id
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
            })
            .map(str::to_owned)
    })
}

/// The page that shows a tool call's result (same-origin path with the workspace), if any.
pub(crate) fn page_url(
    page: Page,
    args: &Value,
    result: &Value,
    workspace_id: Id,
) -> Option<String> {
    if page == Page::None {
        return None;
    }
    // The tool API's own link (tasks and databases carry one): its path.
    if let Some(url) = result["url"].as_str()
        && let Ok(parsed) = url::Url::parse(url)
    {
        let mut path = parsed.path().to_owned();
        if let Some(query) = parsed.query() {
            path.push('?');
            path.push_str(query);
        }
        if path.starts_with('/') && !path.starts_with("//") {
            return Some(with_workspace(&path, workspace_id));
        }
    }
    let path = match page {
        Page::None => return None,
        Page::Fixed(path) => path.to_owned(),
        Page::Task => format!(
            "/tasks/{}",
            id_in(result, &["id"]).or_else(|| id_in(args, &["task_id"]))?
        ),
        Page::TrashedTask => format!(
            "/tasks-trash/task/{}",
            id_in(result, &["id"]).or_else(|| id_in(args, &["task_id"]))?
        ),
        Page::Doc => format!(
            "/docs/{}",
            id_in(result, &["id", "page_id"]).or_else(|| id_in(args, &["page_id"]))?
        ),
        Page::Database => format!(
            "/docs/{}",
            id_in(args, &["database_id"]).or_else(|| id_in(result, &["database_id", "id"]))?
        ),
        Page::ProjectSettings(section) => format!(
            "/tasks/projects/{}/settings/{section}",
            id_in(args, &["project_id"]).or_else(|| id_in(result, &["project_id", "id"]))?
        ),
        Page::Chat => format!(
            "/chat/{}",
            id_in(args, &["conversation_id"]).or_else(|| id_in(result, &["conversation_id"]))?
        ),
        Page::View => format!(
            "/views/{}",
            id_in(args, &["view_id"]).or_else(|| id_in(result, &["id", "view_id"]))?
        ),
    };
    Some(with_workspace(&path, workspace_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mcp_tool_is_labelled_and_every_change_has_a_page() {
        for tool in crate::mcp::all_tools() {
            let label = label_of(&tool.name)
                .unwrap_or_else(|| panic!("{} has no assistant label", tool.name));
            let annotations = tool.annotations.clone().unwrap_or_default();
            let read_only = annotations.read_only_hint.unwrap_or(false);
            assert_eq!(
                read_only,
                label.risk == Risk::Read,
                "{}: risk {:?} does not match its MCP read-only hint",
                tool.name,
                label.risk
            );
            if label.risk != Risk::Read {
                // a write names its page, or says explicitly that it has none
                assert!(
                    !matches!(label.page, Page::Fixed("")),
                    "{} has an empty page",
                    tool.name
                );
                assert!(
                    !label.shows,
                    "{}: only reads open a page they show",
                    tool.name
                );
            }
        }
    }

    #[test]
    fn every_string_argument_of_a_change_is_classified() {
        let mut unknown = Vec::new();
        for tool in crate::mcp::all_tools() {
            let label = label_of(&tool.name).expect("labelled");
            // the free-text rule only matters for a change the assistant can call
            if label.risk == Risk::Read || label.exposure == Exposure::Hidden {
                continue;
            }
            for name in string_args(&Value::Object((*tool.input_schema).clone())) {
                if !RICH_TEXT_ARGS.contains(&name.as_str()) && !PLAIN_ARGS.contains(&name.as_str())
                {
                    unknown.push(format!("{}.{name}", tool.name));
                }
            }
        }
        assert!(
            unknown.is_empty(),
            "classify these string arguments as rich text (RICH_TEXT_ARGS) or plain (PLAIN_ARGS): {unknown:?}"
        );
    }

    #[test]
    fn changes_with_free_text_are_never_allowed_for_the_chat() {
        let registry = Registry::new();
        for name in [
            "create_comment",
            "update_comment",
            "create_doc",
            "create_doc_comment",
            "update_doc",
            "create_task",
            "update_task",
            "create_database_row",
            "update_database_row",
            "create_database_property",
            "create_database",
        ] {
            let def = registry.get(name).expect(name);
            assert!(def.label.free_text, "{name} takes free text");
        }
        for name in [
            "create_task_relation",
            "update_project",
            "create_database_view",
            "restore_task",
        ] {
            assert!(
                !registry.get(name).expect(name).label.free_text,
                "{name} is structured"
            );
        }
    }

    #[test]
    fn every_destructive_mcp_tool_always_asks() {
        let registry = Registry::new();
        let mut destructive = 0;
        for tool in crate::mcp::all_tools() {
            if !is_destructive(&tool) {
                continue;
            }
            destructive += 1;
            let def = registry.get(&tool.name).expect("labelled");
            assert_eq!(
                def.label.risk,
                Risk::Dangerous,
                "{} is destructive in MCP, so it must always ask",
                tool.name
            );
        }
        assert!(destructive >= 5, "the MCP flags were not read");
        for name in [
            "update_task",
            "create_task_relation",
            "update_database_row",
            "update_database_view",
            "update_comment",
            // many rows at once
            "bulk_update_tasks",
            "move_unfinished_cycle_tasks",
            "mark_all_notifications_read",
        ] {
            assert_eq!(registry.risk(name), Some(Risk::Dangerous), "{name}");
        }
        assert_eq!(registry.risk("create_task"), Some(Risk::Write));
    }

    #[test]
    fn the_assistant_gets_no_delete_or_trash_tool() {
        let registry = Registry::new();
        for tool in crate::mcp::all_tools() {
            let name = tool.name.as_ref();
            if name.starts_with("delete_") || name.starts_with("trash_") {
                assert_eq!(registry.risk(name), None, "{name} must stay hidden");
            }
        }
        let specs = registry.specs(
            &registry
                .loadable()
                .map(|t| t.tool.name.to_string())
                .collect(),
        );
        assert!(specs.iter().all(|spec| !spec.name.starts_with("delete_")));
        assert!(specs.iter().any(|spec| spec.name == "update_doc"));
        let core = registry.specs(&BTreeSet::new());
        assert!(core.iter().any(|spec| spec.name == "create_task"));
        assert!(!core.iter().any(|spec| spec.name == "update_doc"));
        assert_eq!(core.last().map(|spec| spec.name.as_str()), Some(LOAD_TOOLS));
    }

    #[test]
    fn pages_come_from_the_result_link_or_the_ids() {
        let workspace = Id::new_v7();
        let from_link = page_url(
            Page::Task,
            &json!({}),
            &json!({"url": format!("https://orbit.test/tasks/abc?workspace={workspace}")}),
            workspace,
        );
        assert_eq!(from_link, Some(format!("/tasks/abc?workspace={workspace}")));
        assert_eq!(
            page_url(Page::Doc, &json!({"page_id": "p1"}), &json!({}), workspace),
            Some(format!("/docs/p1?workspace={workspace}"))
        );
        assert_eq!(
            page_url(
                Page::ProjectSettings("labels"),
                &json!({"project_id": "x"}),
                &json!({}),
                workspace
            ),
            Some(format!(
                "/tasks/projects/x/settings/labels?workspace={workspace}"
            ))
        );
        assert_eq!(
            page_url(Page::None, &json!({}), &json!({}), workspace),
            None
        );
        // an id that is not an id is not put in a path
        assert_eq!(
            page_url(
                Page::Doc,
                &json!({"page_id": "../x"}),
                &json!({}),
                workspace
            ),
            None
        );
    }

    #[test]
    fn open_page_only_opens_app_pages() {
        let workspace = Id::new_v7();
        assert_eq!(
            open_page_path("/docs", workspace),
            Some(format!("/docs?workspace={workspace}"))
        );
        assert!(open_page_path("/tasks/abc-1", workspace).is_some());
        for bad in [
            "https://evil.test/docs",
            "//evil.test",
            "/admin",
            "/docs/../admin",
            "/docsx",
            "javascript:alert(1)",
        ] {
            assert_eq!(open_page_path(bad, workspace), None, "{bad}");
        }
    }
}
