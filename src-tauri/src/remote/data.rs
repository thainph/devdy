//! Remote Control data actions: todos (with the Focus board), notes and
//! per-project switches
//! (`docs/remote-mobile-companion-spec.md` §2.B / §3.2).
//!
//! Only reached for a Controller that advertised
//! [`FEATURE_DATA`](crate::remote::protocol::FEATURE_DATA) — the gate lives in
//! [`crate::remote::handler::handle`], together with the allow-list and the rate
//! limit. Every write goes through the same `*_inner` function the desktop
//! command uses, so both surfaces validate and emit identically.
//!
//! Wire input is validated here first (pure functions, unit-tested below): ids,
//! sizes, and the exact set of patch keys a phone may touch. Every outcome is
//! audited exactly once; writes are answered with a `command_ack`.

use crate::commands::project_git_accounts::{self as git_accounts, Provider};
use crate::commands::{claude_accounts, mcp, notes, projects, rules, settings, todos, vps_servers};
use crate::db::Db;
use crate::item_links::{self, ItemKind};
use crate::remote::command::RejectReason;
use crate::remote::handler::{accept, command_ack, command_ack_item, reject, send_payload, HandlerCtx};
use crate::remote::protocol::{
    BuiltinToggle, ClaudeAccountToggle, CmdPayload, NoteInfo, NoteSummary, StreamPayload, TodoInfo, ToggleItem,
    NOTE_PREVIEW_CHARS,
};
use serde_json::{Map, Value};
use sqlx::Row;

/// Longest id accepted (uuids are 36; leaves room for imported ids).
pub const MAX_ID_LEN: usize = 128;
/// Longest todo text (title + note).
pub const MAX_TODO_TEXT: usize = 20_000;
/// Longest note title.
pub const MAX_NOTE_TITLE: usize = 1_000;
/// Longest note body (Markdown).
pub const MAX_NOTE_CONTENT: usize = 1_000_000;
/// Longest search query.
pub const MAX_QUERY: usize = 200;

/// Keys a `todo_update` patch may carry — the fields the phone edits: the
/// plain todo ones plus the Focus ones a person sets by hand. AI output,
/// actual time and `done` (→ `todo_toggle`) stay desktop-only.
pub const TODO_PATCH_KEYS: [&str; 9] = [
    "text",
    "due_date",
    "project_id",
    "due_time",
    "priority",
    "estimate_min",
    "status",
    "snoozed_until",
    "remind_at",
];
/// Keys a `note_update` patch may carry.
pub const NOTE_PATCH_KEYS: [&str; 3] = ["title", "content", "project_id"];

/// The settings key behind the built-in `devdy` MCP switch (app-wide).
const BUILTIN_DEVDY_KEY: &str = "mcp_builtin_devdy_enabled";

// ---- Pure validation ---------------------------------------------------------

/// A required id: trimmed, non-empty, bounded, printable.
pub fn valid_id(raw: Option<&str>) -> Result<String, RejectReason> {
    let id = raw.map(str::trim).unwrap_or("");
    if id.is_empty() || id.len() > MAX_ID_LEN || id.chars().any(char::is_control) {
        return Err(RejectReason::Malformed);
    }
    Ok(id.to_string())
}

/// An optional project id: blank means "none".
pub fn optional_id(raw: Option<&str>) -> Result<Option<String>, RejectReason> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(id) => valid_id(Some(id)).map(Some),
    }
}

fn bounded_text(s: &str, max: usize) -> Result<(), RejectReason> {
    if s.chars().count() > max {
        Err(RejectReason::Malformed)
    } else {
        Ok(())
    }
}

/// A `YYYY-MM-DD` date, or none for blank.
fn valid_due_date(raw: Option<&str>) -> Result<Option<String>, RejectReason> {
    crate::focus::validate::due_date(raw).map_err(|_| RejectReason::Malformed)
}

/// A string-or-null patch value; anything else is malformed.
fn opt_string(v: &Value) -> Result<Option<&str>, RejectReason> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s)),
        _ => Err(RejectReason::Malformed),
    }
}

/// An integer patch value (`null` allowed when `nullable`).
fn opt_int(v: &Value, nullable: bool) -> Result<Option<i64>, RejectReason> {
    match v {
        Value::Null if nullable => Ok(None),
        v => v.as_i64().map(Some).ok_or(RejectReason::Malformed),
    }
}

/// Validate a `todo_update` patch into the map `update_todo_fields_inner`
/// takes. Only [`TODO_PATCH_KEYS`]; at least one of them; `text` and
/// `priority` / `status` may not be cleared; a blank / null optional field
/// clears it. Cross-field rules (a `due_time` needs a `due_date`) are left to
/// `update_todo_fields_inner`.
pub fn todo_patch(wire: Option<&Value>) -> Result<Map<String, Value>, RejectReason> {
    use crate::focus::validate as v;
    let obj = wire.and_then(Value::as_object).ok_or(RejectReason::Malformed)?;
    if obj.is_empty() || obj.keys().any(|k| !TODO_PATCH_KEYS.contains(&k.as_str())) {
        return Err(RejectReason::Malformed);
    }
    let bad = |_| RejectReason::Malformed;
    let mut out = Map::new();
    for (key, val) in obj {
        let v = match key.as_str() {
            "text" => {
                let s = opt_string(val)?;
                let t = s.map(str::trim).filter(|t| !t.is_empty()).ok_or(RejectReason::Malformed)?;
                bounded_text(t, MAX_TODO_TEXT)?;
                Value::String(t.to_string())
            }
            "due_date" => valid_due_date(opt_string(val)?)?.map_or(Value::Null, Value::String),
            "project_id" => optional_id(opt_string(val)?)?.map_or(Value::Null, Value::String),
            "due_time" => v::due_time(opt_string(val)?).map_err(bad)?.map_or(Value::Null, Value::String),
            "priority" => {
                let p = opt_int(val, false)?.ok_or(RejectReason::Malformed)?;
                Value::from(v::priority(p).map_err(bad)?)
            }
            "estimate_min" => match opt_int(val, true)? {
                None => Value::Null,
                Some(m) => Value::from(v::estimate(m).map_err(bad)?),
            },
            "status" => {
                let s = opt_string(val)?.ok_or(RejectReason::Malformed)?;
                Value::String(v::status(s).map_err(bad)?)
            }
            "snoozed_until" | "remind_at" => {
                v::instant(key, opt_string(val)?).map_err(bad)?.map_or(Value::Null, Value::String)
            }
            _ => unreachable!("filtered above"),
        };
        out.insert(key.clone(), v);
    }
    Ok(out)
}

/// A validated `note_update` patch. `project_id: Some(None)` clears the link.
#[derive(Debug, Default, PartialEq)]
pub struct NotePatch {
    pub title: Option<String>,
    pub content: Option<String>,
    pub project_id: Option<Option<String>>,
}

/// Validate a `note_update` patch: only [`NOTE_PATCH_KEYS`], at least one.
/// `title` / `content` take a string (`null` = empty); whether the note keeps a
/// title or content is checked against the merged result by the caller.
pub fn note_patch(wire: Option<&Value>) -> Result<NotePatch, RejectReason> {
    let obj = wire.and_then(Value::as_object).ok_or(RejectReason::Malformed)?;
    if obj.is_empty() || obj.keys().any(|k| !NOTE_PATCH_KEYS.contains(&k.as_str())) {
        return Err(RejectReason::Malformed);
    }
    let mut out = NotePatch::default();
    for (key, val) in obj {
        let s = opt_string(val)?;
        match key.as_str() {
            "title" => {
                let t = s.unwrap_or("");
                bounded_text(t, MAX_NOTE_TITLE)?;
                out.title = Some(t.to_string());
            }
            "content" => {
                let c = s.unwrap_or("");
                bounded_text(c, MAX_NOTE_CONTENT)?;
                out.content = Some(c.to_string());
            }
            "project_id" => out.project_id = Some(optional_id(s)?),
            _ => unreachable!("filtered above"),
        }
    }
    Ok(out)
}

/// A search query: trimmed, bounded; blank means "no search".
pub fn search_query(raw: Option<&str>) -> Result<Option<String>, RejectReason> {
    let q = raw.map(str::trim).unwrap_or("");
    if q.is_empty() {
        return Ok(None);
    }
    bounded_text(q, MAX_QUERY)?;
    Ok(Some(q.to_string()))
}

/// What a `project_toggle_set` flips.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToggleKind {
    Skill,
    Rule,
    Mcp,
    /// The built-in `devdy` MCP server (an app-wide setting today).
    BuiltinMcp,
    /// A VPS server.
    Server,
    GithubAccount,
    GitlabAccount,
    AwsAccount,
    AzureAccount,
    /// The project's Claude account: on links the project to it, off returns
    /// the project to the default account.
    ClaudeAccount,
}

impl ToggleKind {
    pub fn parse(raw: Option<&str>) -> Result<Self, RejectReason> {
        match raw {
            Some("skill") => Ok(ToggleKind::Skill),
            Some("rule") => Ok(ToggleKind::Rule),
            Some("mcp") => Ok(ToggleKind::Mcp),
            Some("builtin_mcp") => Ok(ToggleKind::BuiltinMcp),
            Some("server") => Ok(ToggleKind::Server),
            Some("github_account") => Ok(ToggleKind::GithubAccount),
            Some("gitlab_account") => Ok(ToggleKind::GitlabAccount),
            Some("aws_account") => Ok(ToggleKind::AwsAccount),
            Some("azure_account") => Ok(ToggleKind::AzureAccount),
            Some("claude_account") => Ok(ToggleKind::ClaudeAccount),
            _ => Err(RejectReason::Malformed),
        }
    }
}

// ---- Wire mapping ------------------------------------------------------------

fn todo_info(t: todos::Todo) -> TodoInfo {
    TodoInfo {
        id: t.id,
        text: t.text,
        done: t.done,
        project_id: t.project_id,
        due_date: t.due_date,
        due_time: t.due_time,
        created_at: t.created_at,
        completed_at: t.completed_at,
    }
}

/// A list row: the body cut to a preview (on a char boundary).
pub fn note_summary(n: notes::Note) -> NoteSummary {
    NoteSummary {
        preview: n.content.chars().take(NOTE_PREVIEW_CHARS).collect(),
        id: n.id,
        title: n.title,
        project_id: n.project_id,
        updated_at: n.updated_at,
    }
}

fn note_info(n: notes::Note) -> NoteInfo {
    NoteInfo {
        id: n.id,
        title: n.title,
        content: n.content,
        project_id: n.project_id,
        updated_at: n.updated_at,
    }
}

/// `SELECT id, name, enabled` rows for one switch table. `sql` binds the
/// project id once.
async fn toggle_rows(db: &Db, sql: &str, project_id: &str) -> Result<Vec<ToggleItem>, String> {
    let rows = sqlx::query(sql)
        .bind(project_id)
        .fetch_all(db)
        .await
        .map_err(|e| e.to_string())?;
    Ok(rows
        .iter()
        .map(|r| ToggleItem {
            id: r.get("id"),
            name: r.get("name"),
            enabled: r.get::<i64, _>("enabled") != 0,
        })
        .collect())
}

/// `account_table` rows (id + label) with whether `link_table` enables them for
/// the project. Both names are our literals, never wire input.
async fn account_rows(
    db: &Db,
    account_table: &str,
    link_table: &str,
    project_id: &str,
) -> Result<Vec<ToggleItem>, String> {
    let sql = format!(
        "SELECT a.id, a.label AS name, EXISTS(SELECT 1 FROM {link_table} p \
         WHERE p.project_id = ? AND p.account_id = a.id) AS enabled \
         FROM {account_table} a ORDER BY a.label COLLATE NOCASE"
    );
    toggle_rows(db, &sql, project_id).await
}

/// Every managed Claude account, marking the one `project_id` is linked to.
async fn claude_account_rows(db: &Db, project_id: &str) -> Result<Vec<ClaudeAccountToggle>, String> {
    let rows = sqlx::query(
        "SELECT a.id, a.label, a.is_default, \
         EXISTS(SELECT 1 FROM projects p WHERE p.id = ? AND p.claude_account_id = a.id) AS enabled \
         FROM claude_accounts a ORDER BY a.is_default DESC, a.label COLLATE NOCASE",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    .map_err(|e| e.to_string())?;
    Ok(rows
        .iter()
        .map(|r| ClaudeAccountToggle {
            id: r.get("id"),
            name: r.get("label"),
            enabled: r.get::<i64, _>("enabled") != 0,
            is_default: r.get::<i64, _>("is_default") != 0,
        })
        .collect())
}

/// Every skill / rule / MCP server / VPS server / account with whether it is on
/// for `project_id`, plus the built-in `devdy` server. Selects ids and names
/// only — nothing a server or account is configured with (command, URL, env /
/// header keys, host, login, email) is read.
pub async fn project_toggles(db: &Db, project_id: &str) -> Result<StreamPayload, String> {
    let mcp_servers = toggle_rows(
        db,
        "SELECT m.id, m.name, EXISTS(SELECT 1 FROM project_mcp_servers p \
         WHERE p.project_id = ? AND p.server_id = m.id) AS enabled \
         FROM mcp_servers m ORDER BY m.name COLLATE NOCASE",
        project_id,
    )
    .await?;
    let skills = toggle_rows(
        db,
        "SELECT s.id, s.name, EXISTS(SELECT 1 FROM project_skills p \
         WHERE p.project_id = ? AND p.skill_id = s.id) AS enabled \
         FROM skills s ORDER BY s.name COLLATE NOCASE",
        project_id,
    )
    .await?;
    let rules = toggle_rows(
        db,
        "SELECT r.id, r.name, EXISTS(SELECT 1 FROM project_rules p \
         WHERE p.project_id = ? AND p.rule_id = r.id) AS enabled \
         FROM rules r ORDER BY r.name COLLATE NOCASE",
        project_id,
    )
    .await?;
    let servers = toggle_rows(
        db,
        "SELECT s.id, s.label AS name, EXISTS(SELECT 1 FROM project_servers p \
         WHERE p.project_id = ? AND p.server_id = s.id) AS enabled \
         FROM servers s ORDER BY s.label COLLATE NOCASE",
        project_id,
    )
    .await?;
    Ok(StreamPayload::ProjectToggles {
        project_id: project_id.to_string(),
        mcp_servers,
        skills,
        rules,
        builtin_devdy: BuiltinToggle {
            enabled: mcp::builtin_devdy_enabled(db).await,
            global: true,
        },
        servers,
        github_accounts: account_rows(db, "github_accounts", "project_github_accounts", project_id).await?,
        gitlab_accounts: account_rows(db, "gitlab_accounts", "project_gitlab_accounts", project_id).await?,
        aws_accounts: account_rows(db, "aws_accounts", "project_aws_accounts", project_id).await?,
        azure_accounts: account_rows(db, "azure_accounts", "project_azure_accounts", project_id).await?,
        claude_accounts: claude_account_rows(db, project_id).await?,
    })
}

async fn exists(db: &Db, table: &str, id: &str) -> bool {
    // `table` is always one of our literals, never wire input.
    sqlx::query(&format!("SELECT 1 FROM {table} WHERE id = ? LIMIT 1"))
        .bind(id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .is_some()
}

// ---- Dispatch ----------------------------------------------------------------

/// Run one data action (already allow-listed, rate-limited and flag-gated).
pub async fn handle(ctx: &HandlerCtx, cmd: &CmdPayload) {
    let action = cmd.action.as_str();
    let result = match action {
        "todo_list" => todo_list(ctx).await,
        "todo_add" => todo_add(ctx, cmd).await,
        "todo_update" => todo_update(ctx, cmd).await,
        "todo_toggle" => todo_toggle(ctx, cmd).await,
        "todo_delete" => todo_delete(ctx, cmd).await,
        "focus_board" => focus_board(ctx).await,
        "note_list" | "note_search" => note_list(ctx, cmd).await,
        "note_get" => note_get(ctx, cmd).await,
        "note_add" => note_add(ctx, cmd).await,
        "note_update" => note_update(ctx, cmd).await,
        "note_delete" => note_delete(ctx, cmd).await,
        "project_toggles_get" => toggles_get(ctx, cmd).await,
        "project_toggle_set" => toggle_set(ctx, cmd).await,
        _ => Err(Failure::Rejected(RejectReason::NotAllowed)),
    };
    match result {
        Ok(created) => {
            accept(ctx, action, None, None).await;
            command_ack_item(ctx, cmd, created).await;
        }
        Err(Failure::Rejected(reason)) => {
            reject(ctx, action, None, reason).await;
            command_ack(ctx, cmd, false, Some(reason.as_str())).await;
        }
        Err(Failure::Failed(e)) => {
            // The error text may quote user data; keep it out of the audit and
            // off the wire.
            tracing::debug!(event = "remote_data_action_failed", action = %action, error = %e);
            reject(ctx, action, None, RejectReason::Malformed).await;
            command_ack(ctx, cmd, false, Some("failed")).await;
        }
    }
}

enum Failure {
    /// Refused before touching anything (bad input, unknown item).
    Rejected(RejectReason),
    /// The shared logic refused or the DB failed.
    Failed(String),
}

impl From<RejectReason> for Failure {
    fn from(r: RejectReason) -> Self {
        Failure::Rejected(r)
    }
}

/// `Ok(Some(id))` names the todo / note an add created (echoed in the ack).
type DataResult = Result<Option<String>, Failure>;

/// A project id the action names must exist (a typo must not file a todo
/// under a phantom project).
async fn require_project(ctx: &HandlerCtx, project_id: Option<&str>) -> DataResult {
    match project_id {
        Some(pid) if !exists(&ctx.db, "projects", pid).await => Err(RejectReason::NotFound.into()),
        _ => Ok(None),
    }
}

/// The fresh list after a todo write; a Focus Controller also gets the
/// re-ranked board, so the write shows up in its place without another round
/// trip. Best effort: the write itself already landed, so a board failure must
/// not fail its ack.
async fn send_todos_after_write(ctx: &HandlerCtx) -> DataResult {
    send_todo_list(ctx).await?;
    if ctx.focus {
        if let Err(Failure::Failed(e)) = send_focus_board(ctx).await {
            tracing::debug!(event = "remote_focus_board_failed", error = %e);
        }
    }
    Ok(None)
}

async fn send_todo_list(ctx: &HandlerCtx) -> DataResult {
    let list = todos::load_todos(&ctx.db).await.map_err(Failure::Failed)?;
    send_payload(
        ctx,
        StreamPayload::TodoList {
            todos: list.into_iter().map(todo_info).collect(),
        },
    )
    .await;
    Ok(None)
}

async fn send_focus_board(ctx: &HandlerCtx) -> DataResult {
    let board = crate::commands::focus::load_board(ctx.app.clone(), &ctx.db)
        .await
        .map_err(Failure::Failed)?;
    let board = serde_json::to_value(board).map_err(|e| Failure::Failed(e.to_string()))?;
    send_payload(ctx, StreamPayload::FocusBoard { board }).await;
    Ok(None)
}

async fn todo_list(ctx: &HandlerCtx) -> DataResult {
    send_todo_list(ctx).await
}

async fn focus_board(ctx: &HandlerCtx) -> DataResult {
    send_focus_board(ctx).await
}

async fn todo_add(ctx: &HandlerCtx, cmd: &CmdPayload) -> DataResult {
    let text = cmd.text.as_deref().map(str::trim).unwrap_or("");
    if text.is_empty() {
        return Err(RejectReason::Malformed.into());
    }
    bounded_text(text, MAX_TODO_TEXT)?;
    let project_id = optional_id(cmd.project_id.as_deref())?;
    let due_date = valid_due_date(cmd.due_date.as_deref())?;
    let (due_time, priority, estimate_min) = new_todo_focus(cmd, due_date.is_some())?;
    require_project(ctx, project_id.as_deref()).await?;
    let input = todos::NewTodo {
        text: text.to_string(),
        project_id,
        due_date,
        due_time,
        priority,
        estimate_min,
        ..Default::default()
    };
    let todo = todos::add_todo_inner(&ctx.app, &ctx.db, input)
        .await
        .map_err(Failure::Failed)?;
    // Push the fresh list so the phone shows the write without a round trip.
    send_todos_after_write(ctx).await?;
    Ok(Some(todo.id))
}

/// The Focus fields of a `todo_add`: `due_time` (needs a date), `priority`,
/// `estimate_min`; each optional.
pub fn new_todo_focus(
    cmd: &CmdPayload,
    has_date: bool,
) -> Result<(Option<String>, Option<i64>, Option<i64>), RejectReason> {
    use crate::focus::validate as v;
    let bad = |_| RejectReason::Malformed;
    let due_time = v::due_time(cmd.due_time.as_deref()).map_err(bad)?;
    if due_time.is_some() && !has_date {
        return Err(RejectReason::Malformed);
    }
    let priority = cmd.priority.map(v::priority).transpose().map_err(bad)?;
    let estimate_min = cmd.estimate_min.map(v::estimate).transpose().map_err(bad)?;
    Ok((due_time, priority, estimate_min))
}

async fn require_todo(ctx: &HandlerCtx, cmd: &CmdPayload) -> Result<String, Failure> {
    let id = valid_id(cmd.item_id.as_deref())?;
    if !exists(&ctx.db, "todos", &id).await {
        return Err(RejectReason::NotFound.into());
    }
    Ok(id)
}

async fn todo_update(ctx: &HandlerCtx, cmd: &CmdPayload) -> DataResult {
    let patch = todo_patch(cmd.patch.as_ref())?;
    let id = require_todo(ctx, cmd).await?;
    require_project(ctx, patch.get("project_id").and_then(Value::as_str)).await?;
    todos::update_todo_fields_inner(&ctx.app, &ctx.db, &id, &patch)
        .await
        .map_err(Failure::Failed)?;
    send_todos_after_write(ctx).await
}

async fn todo_toggle(ctx: &HandlerCtx, cmd: &CmdPayload) -> DataResult {
    let id = require_todo(ctx, cmd).await?;
    todos::toggle_todo_inner(&ctx.app, &ctx.db, &id)
        .await
        .map_err(Failure::Failed)?;
    send_todos_after_write(ctx).await
}

async fn todo_delete(ctx: &HandlerCtx, cmd: &CmdPayload) -> DataResult {
    let id = require_todo(ctx, cmd).await?;
    todos::delete_todo_inner(&ctx.app, &ctx.db, &id)
        .await
        .map_err(Failure::Failed)?;
    send_todos_after_write(ctx).await
}

/// `note_list` / `note_search`: the reply echoes the filter. Writes are not
/// followed by a push — the Controller re-runs its own list / search.
async fn note_list(ctx: &HandlerCtx, cmd: &CmdPayload) -> DataResult {
    let project_id = optional_id(cmd.project_id.as_deref())?;
    let query = if cmd.action == "note_search" {
        search_query(cmd.query.as_deref())?
    } else {
        None
    };
    let list = notes::list_notes_inner(&ctx.db, project_id.as_deref(), query.as_deref())
        .await
        .map_err(Failure::Failed)?;
    send_payload(
        ctx,
        StreamPayload::NoteList {
            notes: list.into_iter().map(note_summary).collect(),
            project_id,
            query,
        },
    )
    .await;
    Ok(None)
}

/// `note_get`: one full note for the editor.
async fn note_get(ctx: &HandlerCtx, cmd: &CmdPayload) -> DataResult {
    let id = valid_id(cmd.item_id.as_deref())?;
    let note = notes::get_note(&ctx.db, &id)
        .await
        .map_err(|_| Failure::Rejected(RejectReason::NotFound))?;
    send_payload(ctx, StreamPayload::Note { note: note_info(note) }).await;
    Ok(None)
}

/// Desktop note views refetch on `item:changed` (the desktop UI broadcasts it
/// itself after its own writes; the note commands do not).
fn notes_changed(ctx: &HandlerCtx) {
    item_links::emit_changed(&ctx.app, Some(ItemKind::Note));
}

async fn note_add(ctx: &HandlerCtx, cmd: &CmdPayload) -> DataResult {
    let title = cmd.title.as_deref().unwrap_or("");
    let content = cmd.content.as_deref().unwrap_or("");
    bounded_text(title, MAX_NOTE_TITLE)?;
    bounded_text(content, MAX_NOTE_CONTENT)?;
    if title.trim().is_empty() && content.trim().is_empty() {
        return Err(RejectReason::Malformed.into());
    }
    let project_id = optional_id(cmd.project_id.as_deref())?;
    require_project(ctx, project_id.as_deref()).await?;
    let note = notes::add_note_inner(&ctx.db, title, content, project_id, None)
        .await
        .map_err(Failure::Failed)?;
    notes_changed(ctx);
    Ok(Some(note.id))
}

async fn note_update(ctx: &HandlerCtx, cmd: &CmdPayload) -> DataResult {
    let patch = note_patch(cmd.patch.as_ref())?;
    let id = valid_id(cmd.item_id.as_deref())?;
    let current = notes::get_note(&ctx.db, &id)
        .await
        .map_err(|_| Failure::Rejected(RejectReason::NotFound))?;
    if let Some(Some(pid)) = &patch.project_id {
        require_project(ctx, Some(pid)).await?;
    }
    if patch.title.is_some() || patch.content.is_some() {
        let title = patch.title.as_deref().unwrap_or(&current.title);
        let content = patch.content.as_deref().unwrap_or(&current.content);
        if title.trim().is_empty() && content.trim().is_empty() {
            return Err(RejectReason::Malformed.into());
        }
        notes::update_note_inner(&ctx.db, &id, title, content)
            .await
            .map_err(Failure::Failed)?;
    }
    if let Some(project_id) = &patch.project_id {
        notes::set_note_project_inner(&ctx.db, &id, project_id.as_deref())
            .await
            .map_err(Failure::Failed)?;
    }
    notes_changed(ctx);
    Ok(None)
}

async fn note_delete(ctx: &HandlerCtx, cmd: &CmdPayload) -> DataResult {
    let id = valid_id(cmd.item_id.as_deref())?;
    if !exists(&ctx.db, "notes", &id).await {
        return Err(RejectReason::NotFound.into());
    }
    notes::delete_note_inner(&ctx.db, &id)
        .await
        .map_err(Failure::Failed)?;
    notes_changed(ctx);
    Ok(None)
}

async fn send_toggles(ctx: &HandlerCtx, project_id: &str) -> DataResult {
    let payload = project_toggles(&ctx.db, project_id)
        .await
        .map_err(Failure::Failed)?;
    send_payload(ctx, payload).await;
    Ok(None)
}

async fn require_project_id(ctx: &HandlerCtx, cmd: &CmdPayload) -> Result<String, Failure> {
    let pid = valid_id(cmd.project_id.as_deref())?;
    require_project(ctx, Some(&pid)).await?;
    Ok(pid)
}

async fn toggles_get(ctx: &HandlerCtx, cmd: &CmdPayload) -> DataResult {
    let pid = require_project_id(ctx, cmd).await?;
    send_toggles(ctx, &pid).await
}

/// Flip one switch through the desktop's own apply / unlink logic, then push
/// the authoritative state: turning off a skill or rule that an enabled group
/// still brings in leaves it on, exactly as on the desktop.
async fn toggle_set(ctx: &HandlerCtx, cmd: &CmdPayload) -> DataResult {
    let kind = ToggleKind::parse(cmd.toggle_kind.as_deref())?;
    let enabled = cmd.enabled.ok_or(RejectReason::Malformed)?;
    let pid = require_project_id(ctx, cmd).await?;
    let db = &ctx.db;
    let outcome = match kind {
        ToggleKind::BuiltinMcp => {
            settings::set_setting(db, BUILTIN_DEVDY_KEY, if enabled { "true" } else { "false" }).await
        }
        ToggleKind::Skill | ToggleKind::Rule | ToggleKind::Mcp => {
            let id = valid_id(cmd.item_id.as_deref())?;
            let table = match kind {
                ToggleKind::Skill => "skills",
                ToggleKind::Rule => "rules",
                _ => "mcp_servers",
            };
            if !exists(db, table, &id).await {
                return Err(RejectReason::NotFound.into());
            }
            match (kind, enabled) {
                (ToggleKind::Skill, true) => projects::apply_skill_to_project(db, &pid, &id, true).await,
                (ToggleKind::Skill, false) => projects::unlink_skill_from_project(db, &pid, &id).await,
                (ToggleKind::Rule, true) => rules::apply_rule_to_project(db, &pid, &id, true).await,
                (ToggleKind::Rule, false) => rules::unlink_rule_from_project(db, &pid, &id).await,
                _ => mcp::set_project_mcp_server_enabled(db, &pid, &id, enabled).await,
            }
        }
        ToggleKind::Server => {
            let id = valid_id(cmd.item_id.as_deref())?;
            if !exists(db, "servers", &id).await {
                return Err(RejectReason::NotFound.into());
            }
            vps_servers::set_project_server_enabled(db, &pid, &id, enabled).await
        }
        ToggleKind::GithubAccount
        | ToggleKind::GitlabAccount
        | ToggleKind::AwsAccount
        | ToggleKind::AzureAccount => {
            let id = valid_id(cmd.item_id.as_deref())?;
            let (table, provider): (&str, Provider) = match kind {
                ToggleKind::GithubAccount => ("github_accounts", git_accounts::GITHUB),
                ToggleKind::GitlabAccount => ("gitlab_accounts", git_accounts::GITLAB),
                ToggleKind::AwsAccount => ("aws_accounts", git_accounts::AWS),
                _ => ("azure_accounts", git_accounts::AZURE),
            };
            if !exists(db, table, &id).await {
                return Err(RejectReason::NotFound.into());
            }
            match kind {
                ToggleKind::GithubAccount | ToggleKind::GitlabAccount => {
                    git_accounts::set_enabled(db, provider, &pid, &id, enabled).await
                }
                _ => git_accounts::set_cloud_enabled(&ctx.app, db, provider, &pid, &id, enabled).await,
            }
        }
        ToggleKind::ClaudeAccount => {
            let id = valid_id(cmd.item_id.as_deref())?;
            if !exists(db, "claude_accounts", &id).await {
                return Err(RejectReason::NotFound.into());
            }
            if enabled {
                claude_accounts::set_project_account(db, &pid, Some(&id)).await
            } else if project_claude_account(db, &pid).await.as_deref() == Some(id.as_str()) {
                // Only the linked account can be switched off; a stale tap on
                // another one must not unlink the project.
                claude_accounts::set_project_account(db, &pid, None).await
            } else {
                Ok(())
            }
        }
    };
    if let Err(e) = outcome {
        // Still push the real state so the phone's switch snaps back.
        let _ = send_toggles(ctx, &pid).await;
        return Err(Failure::Failed(e));
    }
    send_toggles(ctx, &pid).await
}

async fn project_claude_account(db: &Db, project_id: &str) -> Option<String> {
    sqlx::query_scalar::<_, Option<String>>("SELECT claude_account_id FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ids_are_required_bounded_and_printable() {
        assert_eq!(valid_id(Some(" t1 ")), Ok("t1".into()));
        for bad in [None, Some(""), Some("   "), Some("a\nb"), Some("a\u{0}b")] {
            assert_eq!(valid_id(bad), Err(RejectReason::Malformed), "{bad:?}");
        }
        let long = "x".repeat(MAX_ID_LEN + 1);
        assert_eq!(valid_id(Some(&long)), Err(RejectReason::Malformed));
        assert_eq!(optional_id(Some("  ")), Ok(None));
        assert_eq!(optional_id(None), Ok(None));
    }

    #[test]
    fn todo_patch_takes_only_the_phone_fields() {
        let p = todo_patch(Some(&json!({ "text": " Ship ", "due_date": "2026-11-01", "project_id": "p1" })))
            .unwrap();
        assert_eq!(p["text"], "Ship");
        assert_eq!(p["due_date"], "2026-11-01");
        assert_eq!(p["project_id"], "p1");
        // Blank / null clears the optional ones.
        let p = todo_patch(Some(&json!({ "due_date": "", "project_id": null }))).unwrap();
        assert_eq!(p["due_date"], Value::Null);
        assert_eq!(p["project_id"], Value::Null);

        let bad = |v: Value| todo_patch(Some(&v)).unwrap_err();
        assert_eq!(bad(json!({})), RejectReason::Malformed);
        for k in ["ai_plan", "actual_min", "estimate_source"] {
            assert_eq!(bad(json!({ k: null })), RejectReason::Malformed, "{k} stays desktop-only");
        }
        assert_eq!(bad(json!({ "done": true })), RejectReason::Malformed, "done goes through todo_toggle");
        assert_eq!(bad(json!({ "text": "  " })), RejectReason::Malformed);
        assert_eq!(bad(json!({ "text": null })), RejectReason::Malformed);
        assert_eq!(bad(json!({ "text": 5 })), RejectReason::Malformed);
        assert_eq!(bad(json!({ "due_date": "31/10/2026" })), RejectReason::Malformed);
        assert_eq!(bad(json!({ "text": "x".repeat(MAX_TODO_TEXT + 1) })), RejectReason::Malformed);
        assert_eq!(todo_patch(None).unwrap_err(), RejectReason::Malformed);
        assert_eq!(todo_patch(Some(&json!("text"))).unwrap_err(), RejectReason::Malformed);
    }

    #[test]
    fn todo_patch_takes_the_focus_fields() {
        let p = todo_patch(Some(&json!({
            "due_time": "15:30",
            "priority": 1,
            "estimate_min": 90,
            "status": "doing",
            "snoozed_until": "2026-11-04T09:00:00+07:00",
            "remind_at": null,
        })))
        .unwrap();
        assert_eq!(p["due_time"], "15:30");
        assert_eq!(p["priority"], 1);
        assert_eq!(p["estimate_min"], 90);
        assert_eq!(p["status"], "doing");
        assert_eq!(p["snoozed_until"], "2026-11-04T09:00:00+07:00");
        assert_eq!(p["remind_at"], Value::Null);
        let p = todo_patch(Some(&json!({ "estimate_min": null, "due_time": "", "snoozed_until": "" }))).unwrap();
        assert_eq!(p["estimate_min"], Value::Null);
        assert_eq!(p["due_time"], Value::Null);
        assert_eq!(p["snoozed_until"], Value::Null);

        let bad = |v: Value| todo_patch(Some(&v)).unwrap_err();
        for v in [
            json!({ "priority": 4 }),
            json!({ "priority": null }),
            json!({ "priority": "1" }),
            json!({ "estimate_min": 1 }),
            json!({ "estimate_min": 1.5 }),
            json!({ "status": "done" }),
            json!({ "status": null }),
            json!({ "due_time": "25:00" }),
            json!({ "snoozed_until": "tomorrow" }),
        ] {
            assert_eq!(bad(v.clone()), RejectReason::Malformed, "{v}");
        }
    }

    #[test]
    fn new_todo_focus_fields() {
        let cmd = |v: Value| serde_json::from_value::<CmdPayload>(v).unwrap();
        let base = json!({ "kind": "cmd", "action": "todo_add", "text": "x" });
        assert_eq!(new_todo_focus(&cmd(base.clone()), false), Ok((None, None, None)));
        let mut full = base.clone();
        full["due_time"] = json!("09:15");
        full["priority"] = json!(3);
        full["estimate_min"] = json!(30);
        assert_eq!(new_todo_focus(&cmd(full.clone()), true), Ok((Some("09:15".into()), Some(3), Some(30))));
        assert_eq!(new_todo_focus(&cmd(full), false), Err(RejectReason::Malformed), "a time needs a date");
        let mut bad = base;
        bad["priority"] = json!(0);
        assert_eq!(new_todo_focus(&cmd(bad), false), Err(RejectReason::Malformed));
    }

    #[test]
    fn note_patch_validation() {
        let p = note_patch(Some(&json!({ "title": "T", "content": "# body", "project_id": "" }))).unwrap();
        assert_eq!(
            p,
            NotePatch {
                title: Some("T".into()),
                content: Some("# body".into()),
                project_id: Some(None),
            }
        );
        let p = note_patch(Some(&json!({ "project_id": "p2" }))).unwrap();
        assert_eq!(p.project_id, Some(Some("p2".into())));
        assert_eq!(p.title, None);

        let bad = |v: Value| note_patch(Some(&v)).unwrap_err();
        assert_eq!(bad(json!({})), RejectReason::Malformed);
        assert_eq!(bad(json!({ "position": 0 })), RejectReason::Malformed);
        assert_eq!(bad(json!({ "content": ["x"] })), RejectReason::Malformed);
        assert_eq!(bad(json!({ "title": "x".repeat(MAX_NOTE_TITLE + 1) })), RejectReason::Malformed);
    }

    #[test]
    fn search_query_is_trimmed_and_bounded() {
        assert_eq!(search_query(Some("  csv ")), Ok(Some("csv".into())));
        assert_eq!(search_query(Some("   ")), Ok(None));
        assert_eq!(search_query(None), Ok(None));
        assert_eq!(search_query(Some(&"q".repeat(MAX_QUERY + 1))), Err(RejectReason::Malformed));
    }

    #[test]
    fn create_ack_names_the_new_item() {
        let ack = StreamPayload::CommandAck {
            command_id: "c1".into(),
            action: "note_add".into(),
            accepted: true,
            reason: None,
            run_id: None,
            item_id: Some("n1".into()),
        };
        let v = serde_json::to_value(&ack).unwrap();
        assert_eq!(v["item_id"], "n1");
        assert!(v.get("run_id").is_none());
        // Other acks keep their old shape.
        let plain = StreamPayload::CommandAck {
            command_id: "c2".into(),
            action: "todo_toggle".into(),
            accepted: true,
            reason: None,
            run_id: None,
            item_id: None,
        };
        assert!(serde_json::to_value(&plain).unwrap().get("item_id").is_none());
    }

    #[test]
    fn note_list_rows_carry_a_bounded_preview_only() {
        let long = "é".repeat(NOTE_PREVIEW_CHARS + 50);
        let row = note_summary(notes::Note {
            id: "n1".into(),
            title: "T".into(),
            content: long,
            project_id: None,
            position: 0,
            created_at: "c".into(),
            updated_at: "u".into(),
            linked_run_count: 0,
        });
        assert_eq!(row.preview.chars().count(), NOTE_PREVIEW_CHARS);
        let v = serde_json::to_value(&row).unwrap();
        assert!(v.get("content").is_none(), "the list never carries the full body");
    }

    #[test]
    fn toggle_kinds() {
        assert_eq!(ToggleKind::parse(Some("skill")), Ok(ToggleKind::Skill));
        assert_eq!(ToggleKind::parse(Some("rule")), Ok(ToggleKind::Rule));
        assert_eq!(ToggleKind::parse(Some("mcp")), Ok(ToggleKind::Mcp));
        assert_eq!(ToggleKind::parse(Some("builtin_mcp")), Ok(ToggleKind::BuiltinMcp));
        assert_eq!(ToggleKind::parse(Some("server")), Ok(ToggleKind::Server));
        assert_eq!(ToggleKind::parse(Some("github_account")), Ok(ToggleKind::GithubAccount));
        assert_eq!(ToggleKind::parse(Some("gitlab_account")), Ok(ToggleKind::GitlabAccount));
        assert_eq!(ToggleKind::parse(Some("aws_account")), Ok(ToggleKind::AwsAccount));
        assert_eq!(ToggleKind::parse(Some("azure_account")), Ok(ToggleKind::AzureAccount));
        assert_eq!(ToggleKind::parse(Some("claude_account")), Ok(ToggleKind::ClaudeAccount));
        for bad in [None, Some(""), Some("account"), Some("MCP")] {
            assert_eq!(ToggleKind::parse(bad), Err(RejectReason::Malformed));
        }
    }

    /// Collect every object key in a JSON tree.
    fn keys(v: &Value, out: &mut std::collections::BTreeSet<String>) {
        match v {
            Value::Object(m) => {
                for (k, v) in m {
                    out.insert(k.clone());
                    keys(v, out);
                }
            }
            Value::Array(a) => a.iter().for_each(|v| keys(v, out)),
            _ => {}
        }
    }

    #[tokio::test]
    async fn project_toggles_never_carry_server_config_or_secrets() {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&db).await.unwrap();
        for sql in [
            "INSERT INTO projects (id, name, path, created_at) VALUES ('p1', 'devdy', '/tmp/devdy', 'c')",
            "INSERT INTO mcp_servers (id, name, description, transport, command, args, url, env_keys, \
             header_keys, enabled, created_at) VALUES ('m1', 'pms', 'desc-SECRET', 'http', \
             'cmd-SECRET', '[\"arg-SECRET\"]', 'https://url-SECRET', '[\"ENV_KEY_SECRET\"]', \
             '[\"HEADER_KEY_SECRET\"]', 1, 'c')",
            "INSERT INTO mcp_servers (id, name, transport, created_at) VALUES ('m2', 'google', 'stdio', 'c')",
            "INSERT INTO project_mcp_servers (project_id, server_id, enabled_at) VALUES ('p1', 'm1', 'c')",
            "INSERT INTO skills (id, name, description, source_path, updated_at) \
             VALUES ('s1', 'git-flow', 'desc-SECRET', '/path-SECRET', 'c')",
            "INSERT INTO rules (id, name, description, source_path, updated_at) \
             VALUES ('r1', 'git-rule', 'desc-SECRET', '/path-SECRET', 'c')",
            "INSERT INTO settings (key, value) VALUES ('mcp_builtin_devdy_enabled', 'false')",
            "INSERT INTO servers (id, label, host, port, username, auth_method, private_key_path, created_at) \
             VALUES ('v1', 'prod', 'host-SECRET', 22, 'user-SECRET', 'key', '/key-SECRET', 'c')",
            "INSERT INTO project_servers (project_id, server_id, role, created_at) VALUES ('p1', 'v1', 'staging', 'c')",
            "INSERT INTO github_accounts (id, label, username, created_at) VALUES ('g1', 'work', 'login-SECRET', 'c')",
            "INSERT INTO project_github_accounts (project_id, account_id, created_at) VALUES ('p1', 'g1', 'c')",
            "INSERT INTO aws_accounts (id, label, auth_method, profile_name, region, created_at) \
             VALUES ('aw1', 'aws-dev', 'profile', 'profile-SECRET', 'region-SECRET', 'c')",
            "INSERT INTO claude_accounts (id, label, config_dir, email, is_default, created_at) \
             VALUES ('c1', 'main', '/dir-SECRET', 'mail-SECRET', 1, 'c'), \
                    ('c2', 'alt', '/dir2-SECRET', NULL, 0, 'c')",
            "UPDATE projects SET claude_account_id = 'c2' WHERE id = 'p1'",
        ] {
            sqlx::query(sql).execute(&db).await.unwrap();
        }

        let payload = project_toggles(&db, "p1").await.unwrap();
        let v = serde_json::to_value(&payload).unwrap();
        assert_eq!(v["kind"], "project_toggles");
        assert_eq!(v["mcp_servers"], json!([
            { "id": "m2", "name": "google", "enabled": false },
            { "id": "m1", "name": "pms", "enabled": true },
        ]));
        assert_eq!(v["skills"], json!([{ "id": "s1", "name": "git-flow", "enabled": false }]));
        assert_eq!(v["rules"], json!([{ "id": "r1", "name": "git-rule", "enabled": false }]));
        assert_eq!(v["builtin_devdy"], json!({ "enabled": false, "global": true }));
        assert_eq!(v["servers"], json!([{ "id": "v1", "name": "prod", "enabled": true }]));
        assert_eq!(v["github_accounts"], json!([{ "id": "g1", "name": "work", "enabled": true }]));
        assert_eq!(v["gitlab_accounts"], json!([]));
        assert_eq!(v["aws_accounts"], json!([{ "id": "aw1", "name": "aws-dev", "enabled": false }]));
        assert_eq!(v["azure_accounts"], json!([]));
        assert_eq!(v["claude_accounts"], json!([
            { "id": "c1", "name": "main", "enabled": false, "is_default": true },
            { "id": "c2", "name": "alt", "enabled": true, "is_default": false },
        ]));

        let mut seen = std::collections::BTreeSet::new();
        keys(&v, &mut seen);
        let allowed = [
            "kind", "project_id", "mcp_servers", "skills", "rules", "builtin_devdy", "id", "name",
            "enabled", "global", "servers", "github_accounts", "gitlab_accounts", "aws_accounts",
            "azure_accounts", "claude_accounts", "is_default",
        ];
        for k in &seen {
            assert!(allowed.contains(&k.as_str()), "unexpected field `{k}` in project_toggles");
        }
        let text = v.to_string();
        assert!(!text.contains("SECRET"), "server config leaked: {text}");
    }
}
