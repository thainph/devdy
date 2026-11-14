//! Focus AI commands (docs/focus-board-spec.md §3.6, §6.1, phase P3) — only
//! ever run when the user presses a button:
//!
//! - `focus_ai_suggest` — a todo proposal for a source ("✨ Suggest" of the form)
//! - `focus_ai_cancel` — abort a request (it then fails with `cancelled`)
//! - `focus_start_with_ai` — a new session working on the todo
//!
//! The prompts and validation are pure (`focus::ai`). BR-09: the AI never
//! writes a todo; the form's fields change when the user applies a suggestion.
//! One request per source at a time; a failure changes nothing.

use crate::commands::todos::get_todo;
use crate::commands::translate::{submit_prompt_as, TranslateState, TurnEvent};
use crate::db::Db;
use crate::focus::ai::{self, SourceCtx};
use crate::focus::ai_util::AiEngine;
use crate::focus::github::{GithubFetch, OctoGithub};
use crate::focus::settings::{self as focus_settings, FocusSettings};
use crate::focus::source_content::{github_content, slack_content, SlackMsg};
use crate::runs::RunRegistry;
use async_trait::async_trait;
use chrono::{DateTime, Local};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, State};

/// A prompt that has not answered by then is abandoned.
const TURN_TIMEOUT: Duration = Duration::from_secs(180);

/// Running requests (key → request id + abort handle) and the warm engine
/// they share, its own instance so Focus prompts never queue behind
/// translations. A fresh session per prompt (one source never leaks into
/// another's answer).
#[derive(Clone)]
pub struct FocusAiState {
    running: Arc<Mutex<HashMap<String, Running>>>,
    translate: TranslateState,
}

/// The engine turn a request is waiting on (so a cancel can free the warm
/// session, like the timeout does).
type TurnSlot = Arc<Mutex<Option<u64>>>;

struct Running {
    request_id: String,
    abort: tokio::task::AbortHandle,
    turn: TurnSlot,
}

impl FocusAiState {
    /// Stop the warm Focus AI session (app exit).
    pub async fn shutdown(&self) {
        self.translate.shutdown().await;
    }

    /// Abort request `request_id` and abandon its engine turn; false when it
    /// is not running (any more).
    pub async fn cancel(&self, request_id: &str) -> bool {
        let turn = {
            let Ok(running) = self.running.lock() else { return false };
            let Some(r) = running.values().find(|r| r.request_id == request_id) else {
                return false;
            };
            r.abort.abort();
            r.turn.lock().ok().and_then(|t| *t)
        };
        if let Some(turn) = turn {
            self.translate.abandon_turn(turn).await;
        }
        true
    }
}

impl Default for FocusAiState {
    fn default() -> Self {
        FocusAiState {
            running: Arc::default(),
            translate: TranslateState::default()
                .with_recycle_after(1)
                .silent()
                .tracked_as(crate::procs::ProcKind::FocusAi),
        }
    }
}

/// The translate sidecar with the Focus engine / model (empty = translate's).
struct FocusEngine {
    app: AppHandle,
    db: Db,
    state: TranslateState,
    /// The turn in flight (read by `FocusAiState::cancel`).
    turn: TurnSlot,
    engine: String,
    model: String,
}

#[async_trait]
impl AiEngine for FocusEngine {
    async fn complete(&self, prompt: String) -> Result<String, String> {
        let (turn, rx) = submit_prompt_as(
            &self.app,
            &self.db,
            &self.state,
            prompt,
            &self.engine,
            &self.model,
        )
        .await?;
        if let Ok(mut slot) = self.turn.lock() {
            *slot = Some(turn);
        }
        let mut rx = rx.ok_or("no result channel")?;
        let wait = async {
            loop {
                match rx.recv().await {
                    Some(TurnEvent::Chunk(_)) => continue,
                    Some(TurnEvent::Done(text)) => return Ok(text),
                    Some(TurnEvent::Error(e)) => return Err(e),
                    None => return Err("the AI engine stopped unexpectedly".to_string()),
                }
            }
        };
        let out = match tokio::time::timeout(TURN_TIMEOUT, wait).await {
            Ok(r) => r,
            Err(_) => {
                self.state.abandon_turn(turn).await;
                Err("timeout".into())
            }
        };
        if let Ok(mut slot) = self.turn.lock() {
            *slot = None;
        }
        out
    }
}

fn engine_for(app: &AppHandle, db: &Db, state: &FocusAiState, s: &FocusSettings) -> FocusEngine {
    FocusEngine {
        app: app.clone(),
        db: db.clone(),
        state: state.translate.clone(),
        turn: TurnSlot::default(),
        engine: s.ai_engine.clone(),
        model: s.ai_model.clone(),
    }
}

/// One attempt + one retry carrying the validation error (like the Pulse digest).
async fn ask<T>(
    engine: &dyn AiEngine,
    prompt: String,
    parse: impl Fn(&str) -> Result<T, String>,
) -> Result<T, String> {
    let first = engine.complete(prompt.clone()).await?;
    let err = match parse(&first) {
        Ok(v) => return Ok(v),
        Err(e) => e,
    };
    tracing::info!(event = "focus_ai_retry", error = %err);
    let second = engine.complete(ai::retry_prompt(&prompt, &err)).await?;
    parse(&second).map_err(|e| format!("ai_invalid_answer: {e}"))
}

/// Run `work` as request `request_id` under `key` (one at a time per key),
/// abortable by `focus_ai_cancel` (the caller then gets `cancelled`).
async fn run_request<T, F>(
    state: &FocusAiState,
    key: String,
    request_id: Option<String>,
    turn: TurnSlot,
    work: F,
) -> Result<T, String>
where
    T: Send + 'static,
    F: std::future::Future<Output = Result<T, String>> + Send + 'static,
{
    let request_id = request_id
        .filter(|r| !r.trim().is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let task = {
        let mut running = state.running.lock().map_err(|e| e.to_string())?;
        if running.contains_key(&key) {
            return Err("busy: an AI request for this is already running".into());
        }
        let task = tokio::spawn(work);
        running.insert(
            key.clone(),
            Running {
                request_id: request_id.clone(),
                abort: task.abort_handle(),
                turn,
            },
        );
        task
    };
    let out = task.await;
    if let Ok(mut running) = state.running.lock() {
        running.remove(&key);
    }
    match out {
        Ok(r) => r,
        Err(e) if e.is_cancelled() => Err("cancelled".into()),
        Err(e) => Err(e.to_string()),
    }
}

// ── Context loading ─────────────────────────────────────────────────────────

fn local_now() -> DateTime<Local> {
    Local::now()
}

/// UI language (`en` | `vi`) for the AI's text; Vietnamese by default.
async fn app_language(db: &Db) -> String {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = 'language'")
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .filter(|l| l.trim().eq_ignore_ascii_case("en"))
        .unwrap_or_else(|| "vi".into())
}

async fn project_name(db: &Db, id: Option<&str>) -> Option<String> {
    sqlx::query_scalar("SELECT name FROM projects WHERE id = ?")
        .bind(id?)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
}

/// A captured Slack thread's messages (translations included), or None.
async fn slack_text(
    db: &Db,
    source_ref: &str,
) -> Option<(String, crate::focus::source_content::Content)> {
    // `slack:<team>/<channel>/<ts>`.
    let rest = source_ref.strip_prefix("slack:")?;
    let mut parts = rest.splitn(3, '/');
    let (_team, channel, ts) = (parts.next()?, parts.next()?, parts.next()?);
    let (id, title, team_id): (String, String, Option<String>) = sqlx::query_as(
        "SELECT id, title, team_id FROM slack_threads WHERE channel_id = ? AND thread_ts = ?",
    )
    .bind(channel)
    .bind(ts)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()?;
    let mut all = crate::slack_threads::store::load_names(db).await.ok()?;
    let names = team_id
        .as_deref()
        .and_then(|t| all.remove(t))
        .unwrap_or_default();
    let me: Option<String> = match team_id.as_deref() {
        Some(t) => sqlx::query_scalar("SELECT user_id FROM slack_identities WHERE team_id = ?")
            .bind(t)
            .fetch_optional(db)
            .await
            .ok()
            .flatten(),
        None => None,
    };
    type Row = (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
        Option<String>,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT ts, user_id, bot_id, author_name, subtype, text_raw, text_vi \
         FROM slack_messages WHERE thread_id = ? ORDER BY seq ASC, ts ASC",
    )
    .bind(&id)
    .fetch_all(db)
    .await
    .ok()?;
    if rows.is_empty() {
        return None;
    }
    let msgs: Vec<SlackMsg> = rows
        .into_iter()
        .map(
            |(ts, user_id, bot_id, author_name, subtype, text_raw, text_vi)| {
                let author = user_id
                    .as_deref()
                    .and_then(|u| names.user(u))
                    .map(str::to_string)
                    .or(author_name)
                    .or_else(|| user_id.clone())
                    .or_else(|| bot_id.clone())
                    .unwrap_or_else(|| "unknown".to_string());
                SlackMsg {
                    ts,
                    user_id,
                    bot_id,
                    subtype,
                    author,
                    text_raw,
                    text_vi,
                }
            },
        )
        .collect();
    Some((title, slack_content(&msgs, me.as_deref(), &names)))
}

/// What the AI may read about `url` (spec §3.6): a captured Slack thread's
/// messages, a GitHub issue / PR with comments (the project's account), else
/// only the title and link. Fetch failures fall back to title + link.
async fn source_ctx(
    db: &Db,
    url: &str,
    title: &str,
    project_id: Option<&str>,
) -> Result<SourceCtx, String> {
    let src = crate::focus::source::normalize(url)?.ok_or("invalid source: empty link")?;
    let mut ctx = SourceCtx {
        source_type: src.source_type.to_string(),
        title: title.to_string(),
        url: src.source_url.clone(),
        ..Default::default()
    };
    let r = src.source_ref.as_deref().unwrap_or("");
    match src.source_type {
        "slack" => {
            if let Some((t, c)) = slack_text(db, r).await {
                if ctx.title.trim().is_empty() {
                    ctx.title = t;
                }
                ctx.content = c.text;
                ctx.truncated = c.truncated;
            }
        }
        "github" => {
            let parsed = r.strip_prefix("github:").and_then(|x| {
                let (repo_part, n) = x.rsplit_once('#')?;
                let (o, rp) = repo_part.split_once('/')?;
                Some((o.to_string(), rp.to_string(), n.parse::<u64>().ok()?))
            });
            if let (Some((o, rp, n)), Some(pid)) = (parsed, project_id) {
                match (OctoGithub { db: db.clone() })
                    .fetch_issue(pid, &o, &rp, n)
                    .await
                {
                    Ok(issue) => {
                        if ctx.title.trim().is_empty() {
                            ctx.title = issue.title.clone();
                        }
                        let c = github_content(&issue);
                        ctx.content = c.text;
                        ctx.truncated = c.truncated;
                    }
                    Err(e) => {
                        tracing::info!(event = "focus_ai_github_fetch_failed", code = %e.code)
                    }
                }
            }
        }
        // GitLab / runs / links: title + link only.
        _ => {}
    }
    Ok(ctx)
}

fn first_line(text: &str) -> String {
    text.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

// ── a. Suggest ──────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn focus_ai_suggest(
    app: AppHandle,
    db: State<'_, Db>,
    state: State<'_, FocusAiState>,
    source_url: String,
    project_id: Option<String>,
    title: Option<String>,
    request_id: Option<String>,
) -> Result<ai::Suggestion, String> {
    suggest_inner(&app, db.inner(), state.inner(), source_url, project_id, title, request_id).await
}

/// `focus_ai_suggest`.
pub async fn suggest_inner(
    app: &AppHandle,
    db: &Db,
    state: &FocusAiState,
    source_url: String,
    project_id: Option<String>,
    title: Option<String>,
    request_id: Option<String>,
) -> Result<ai::Suggestion, String> {
    let db = db.clone();
    let settings = focus_settings::load(&db).await?;
    let engine = engine_for(app, &db, state, &settings);
    let key = format!("suggest:{}", source_url.trim());
    let turn = engine.turn.clone();
    run_request(state, key, request_id, turn, async move {
        let source = source_ctx(
            &db,
            &source_url,
            title.as_deref().unwrap_or(""),
            project_id.as_deref(),
        )
        .await?;
        let lang = app_language(&db).await;
        let project = project_name(&db, project_id.as_deref()).await;
        let prompt = ai::suggest_prompt(&ai::SuggestCtx {
            now: local_now().naive_local(),
            lang: &lang,
            project: project.as_deref(),
            source: &source,
        });
        let content = source.content.clone();
        ask(&engine, prompt, |raw| ai::parse_suggestion(raw, &content)).await
    })
    .await
}

// ── Cancel ──────────────────────────────────────────────────────────────────

/// Abort a running request; false when it is not running (any more).
#[tauri::command]
pub async fn focus_ai_cancel(
    state: State<'_, FocusAiState>,
    request_id: String,
) -> Result<bool, String> {
    Ok(state.cancel(&request_id).await)
}

// ── d. Start with AI ────────────────────────────────────────────────────────

/// Open a session in the todo's project working on it; returns the run id.
/// Errors `project_required` when the todo has no project. With `worktree`, the
/// session works in its own fresh worktree of the project (see `crate::worktrees`).
#[tauri::command]
pub async fn focus_start_with_ai(
    app: AppHandle,
    db: State<'_, Db>,
    registry: State<'_, RunRegistry>,
    todo_id: String,
    worktree: Option<bool>,
) -> Result<String, String> {
    start_with_ai_inner(&app, db.inner(), registry.inner(), todo_id, worktree.unwrap_or(false)).await
}

/// `focus_start_with_ai`.
pub async fn start_with_ai_inner(
    app: &AppHandle,
    db: &Db,
    registry: &RunRegistry,
    todo_id: String,
    worktree: bool,
) -> Result<String, String> {
    let pool = db.clone();
    let todo = get_todo(&pool, &todo_id).await?;
    let project_id = todo
        .project_id
        .clone()
        .ok_or("project_required: pick a project for this todo first")?;
    // Prepared before the session row exists, so a failed fetch/setup leaves nothing behind.
    let wt = if worktree {
        Some(crate::worktrees::create(app, &pool, &project_id, &first_line(&todo.text), Some(&todo.id)).await?)
    } else {
        None
    };
    let run = match crate::commands::runs::create_session_run_inner(&pool, project_id, None).await {
        Ok(run) => run,
        Err(e) => {
            if let Some(wt) = &wt {
                let _ = crate::worktrees::remove(&pool, &wt.id, true).await;
            }
            return Err(e);
        }
    };
    let prompt = ai::start_prompt(
        &todo.id,
        &todo.text,
        todo.source_url.as_deref(),
        todo.ai_plan.as_deref(),
    );
    // Linked before the start: a turn that ends at once must find the link.
    crate::focus::start_todo_for_run(app, &pool, &todo_id, &run.id).await;
    let started = async {
        let title = crate::focus::ai_util::truncate(&first_line(&todo.text), 80);
        sqlx::query("UPDATE runs SET title = ?, worktree_path = ? WHERE id = ?")
            .bind(&title)
            .bind(wt.as_ref().map(|w| w.path.as_str()))
            .bind(&run.id)
            .execute(&pool)
            .await
            .map_err(|e| e.to_string())?;
        let payload = crate::commands::runs::StartRunPayload {
            run_id: run.id.clone(),
            engine_override: None,
            permission_mode_override: None,
            prompt_override: Some(prompt),
            model_override: None,
            images: Vec::new(),
            override_budget: false,
            append_system_prompt: None,
            conductor_token: None,
        };
        crate::commands::runs::start_run_inner(
            app.clone(),
            pool.clone(),
            registry.clone(),
            payload,
        )
        .await
    }
    .await;
    if let Err(e) = started {
        // Nothing ran: don't leave an empty session (or its checkout) behind.
        crate::focus::undo_todo_start(app, &pool, &todo_id, &run.id).await;
        discard_run(&pool, &run.id).await;
        if let Some(wt) = &wt {
            let _ = crate::worktrees::remove(&pool, &wt.id, true).await;
        }
        return Err(e);
    }
    Ok(run.id)
}

/// Remove a session row that never started (and its conductor session row).
async fn discard_run(db: &Db, run_id: &str) {
    for sql in [
        "DELETE FROM conductor_sessions WHERE id = ?",
        "DELETE FROM runs WHERE id = ?",
    ] {
        if let Err(e) = sqlx::query(sql).bind(run_id).execute(db).await {
            tracing::warn!(event = "focus_start_discard_failed", run_id = %run_id, error = %e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancel_aborts_the_request_and_frees_its_turn() {
        let state = FocusAiState::default();
        let task = tokio::spawn(async { tokio::time::sleep(Duration::from_secs(60)).await });
        let turn: TurnSlot = Arc::new(Mutex::new(Some(7)));
        state.running.lock().unwrap().insert(
            "todo:t1".into(),
            Running {
                request_id: "r1".into(),
                abort: task.abort_handle(),
                turn,
            },
        );
        assert!(!state.cancel("other").await);
        // No warm session: abandoning turn 7 is a no-op, but must not hang.
        assert!(state.cancel("r1").await);
        assert!(task.await.unwrap_err().is_cancelled());
        state.shutdown().await;
    }

    #[tokio::test]
    async fn a_failed_start_leaves_no_session_row() {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&db).await.unwrap();
        sqlx::query("INSERT INTO projects (id, name, path, created_at) VALUES ('p', 'p', '/tmp/p', 'c')")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO runs (id, project_id, type, created_at) VALUES ('r', 'p', 'session', 'c')")
            .execute(&db)
            .await
            .unwrap();
        discard_run(&db, "r").await;
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs").fetch_one(&db).await.unwrap();
        assert_eq!(n, 0);
    }
}
