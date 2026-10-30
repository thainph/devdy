//! Focus AI commands (docs/focus-board-spec.md §3.6, §6.1, phase P3) — only
//! ever run when the user presses a button:
//!
//! - `focus_ai_suggest` — a todo proposal for a source ("✨ Suggest" of the form)
//! - `focus_ai_analyze` — plan + estimate of a todo, stored as `ai_plan`
//! - `focus_ai_plan_today` / `focus_day_plan_get` — today's timeline (`focus_day_plans`)
//! - `focus_plan_to_calendar` — push plan blocks to Google Calendar
//! - `focus_ai_cancel` — abort a request (`focus:ai-progress` reports it)
//! - `focus_start_with_ai` — a new session working on the todo
//! - `focus_suggest_actual` — BR-11 "how long did it take"
//!
//! The prompts and validation are pure (`focus::ai`). BR-09: the AI only ever
//! writes `ai_plan` / `ai_analyzed_at` and day plans; the board fields change
//! when the user applies a suggestion (`update_todo_fields`). One request per
//! todo (and one day plan) at a time; a failure changes nothing.

use crate::commands::focus::{build_board, BoardItem};
use crate::commands::todos::{get_todo, load_todos, Todo};
use crate::commands::translate::{submit_prompt_as, TranslateState, TurnEvent};
use crate::db::Db;
use crate::focus::ai::{self, DayPlan, PlanTodo, Reference, SourceCtx};
use crate::focus::ai_util::AiEngine;
use crate::focus::github::{GithubFetch, OctoGithub};
use crate::focus::score::{self, Bucket};
use crate::focus::settings::{self as focus_settings, FocusSettings};
use crate::focus::source_content::{github_content, slack_content, SlackMsg};
use crate::item_links::{self, ItemKind};
use crate::runs::RunRegistry;
use async_trait::async_trait;
use chrono::{DateTime, Local, NaiveDateTime};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, State};

/// Progress of a request: `started`, then `done` | `error` | `cancelled`.
pub const PROGRESS_EVENT: &str = "focus:ai-progress";
/// A prompt that has not answered by then is abandoned.
const TURN_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Clone, Serialize)]
struct Progress<'a> {
    request_id: &'a str,
    /// `suggest | analyze | plan`.
    kind: &'a str,
    todo_id: Option<&'a str>,
    stage: &'a str,
    error: Option<&'a str>,
}

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

    async fn model(&self) -> Option<String> {
        None
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
/// abortable by `focus_ai_cancel`, with `focus:ai-progress` around it.
async fn run_request<T, F>(
    app: &AppHandle,
    state: &FocusAiState,
    key: String,
    request_id: Option<String>,
    kind: &str,
    todo_id: Option<&str>,
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
    let progress = |stage: &str, error: Option<&str>| {
        let _ = app.emit(
            PROGRESS_EVENT,
            Progress {
                request_id: &request_id,
                kind,
                todo_id,
                stage,
                error,
            },
        );
    };
    progress("started", None);
    let out = task.await;
    if let Ok(mut running) = state.running.lock() {
        running.remove(&key);
    }
    match out {
        Ok(Ok(v)) => {
            progress("done", None);
            Ok(v)
        }
        Ok(Err(e)) => {
            progress("error", Some(&e));
            Err(e)
        }
        Err(e) if e.is_cancelled() => {
            progress("cancelled", None);
            Err("cancelled".into())
        }
        Err(e) => {
            progress("error", Some(&e.to_string()));
            Err(e.to_string())
        }
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

/// `(actual_min, estimate_min)` samples → the factor "analyze" mentions.
async fn calibration_factor(db: &Db, enabled: bool) -> f64 {
    if !enabled {
        return 1.0;
    }
    let samples: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT actual_min, estimate_min FROM todos \
         WHERE done = 1 AND actual_min > 0 AND estimate_min > 0 \
         ORDER BY completed_at DESC LIMIT 20",
    )
    .fetch_all(db)
    .await
    .unwrap_or_default();
    score::calibration(&samples).factor
}

async fn references(db: &Db, project_id: Option<&str>, except: &str) -> Vec<Reference> {
    let Some(pid) = project_id else {
        return Vec::new();
    };
    let rows: Vec<(String, Option<i64>, i64)> = sqlx::query_as(
        "SELECT text, estimate_min, actual_min FROM todos \
         WHERE project_id = ? AND id != ? AND done = 1 AND actual_min > 0 \
         ORDER BY completed_at DESC LIMIT ?",
    )
    .bind(pid)
    .bind(except)
    .bind(ai::REFERENCE_MAX as i64)
    .fetch_all(db)
    .await
    .unwrap_or_default();
    rows.into_iter()
        .map(|(text, estimate_min, actual_min)| Reference {
            title: text.lines().next().unwrap_or("").to_string(),
            estimate_min,
            actual_min,
        })
        .collect()
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
    let db = db.inner().clone();
    let settings = focus_settings::load(&db).await?;
    let engine = engine_for(&app, &db, &state, &settings);
    let key = format!("suggest:{}", source_url.trim());
    let turn = engine.turn.clone();
    run_request(&app, &state, key, request_id, "suggest", None, turn, async move {
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

// ── b. Analyze ──────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct AnalyzeResult {
    pub analysis: ai::Analysis,
    /// The todo with its new `ai_plan` / `ai_analyzed_at`.
    pub todo: Todo,
}

#[tauri::command]
pub async fn focus_ai_analyze(
    app: AppHandle,
    db: State<'_, Db>,
    state: State<'_, FocusAiState>,
    todo_id: String,
    request_id: Option<String>,
) -> Result<AnalyzeResult, String> {
    let db = db.inner().clone();
    let settings = focus_settings::load(&db).await?;
    let engine = engine_for(&app, &db, &state, &settings);
    let key = format!("todo:{todo_id}");
    let app2 = app.clone();
    let id = todo_id.clone();
    let turn = engine.turn.clone();
    run_request(
        &app,
        &state,
        key,
        request_id,
        "analyze",
        Some(&todo_id),
        turn,
        async move {
            let todo = get_todo(&db, &id).await?;
            let source = match todo.source_url.as_deref() {
                Some(u) => Some(
                    source_ctx(&db, u, &first_line(&todo.text), todo.project_id.as_deref()).await?,
                ),
                None => None,
            };
            let lang = app_language(&db).await;
            let project = project_name(&db, todo.project_id.as_deref()).await;
            let refs = references(&db, todo.project_id.as_deref(), &id).await;
            let calibration = calibration_factor(&db, settings.calibration_enabled).await;
            let due = todo.due_date.as_ref().map(|d| {
                format!(
                    "{d}{}",
                    todo.due_time
                        .as_ref()
                        .map(|t| format!(" {t}"))
                        .unwrap_or_default()
                )
            });
            let prompt = ai::analyze_prompt(&ai::AnalyzeCtx {
                now: local_now().naive_local(),
                lang: &lang,
                todo_text: &todo.text,
                project: project.as_deref(),
                due,
                priority: todo.priority,
                estimate_min: todo.estimate_min,
                calibration,
                references: &refs,
                source: source.as_ref(),
            });
            let analysis = ask(&engine, prompt, ai::parse_analysis).await?;
            let plan = ai::render_markdown(&analysis, &lang);
            let now = chrono::Utc::now().to_rfc3339();
            // BR-09: only the plan; a todo deleted meanwhile is left alone.
            let done = sqlx::query(
                "UPDATE todos SET ai_plan = ?, ai_analyzed_at = ?, updated_at = ? WHERE id = ?",
            )
            .bind(&plan)
            .bind(&now)
            .bind(&now)
            .bind(&id)
            .execute(&db)
            .await
            .map_err(|e| e.to_string())?;
            if done.rows_affected() == 0 {
                return Err(format!("todo not found: {id}"));
            }
            crate::focus::emit_todos_changed(&app2);
            let todo = get_todo(&db, &id).await?;
            Ok(AnalyzeResult { analysis, todo })
        },
    )
    .await
}

// ── c. Plan today ───────────────────────────────────────────────────────────

/// Today's busy Calendar intervals (local), the same events `focus_board`'s
/// capacity uses (BR-08). No account / a failed fetch → working hours only.
async fn busy_today(db: &Db, settings: &FocusSettings) -> Vec<(NaiveDateTime, NaiveDateTime)> {
    let Ok(events) = crate::focus::calendar::today_events(db, settings, None).await else {
        return Vec::new();
    };
    let events = crate::focus::calendar::to_local(&events);
    crate::focus::capacity::busy_on(&events, Local::now().date_naive())
        .into_iter()
        .map(|b| (b.start, b.end))
        .collect()
}

fn plan_todos(items: &[BoardItem], limit: usize) -> Vec<PlanTodo> {
    items
        .iter()
        .filter(|i| !matches!(i.bucket, Bucket::Done | Bucket::Snoozed | Bucket::Waiting))
        .take(limit)
        .map(|i| PlanTodo {
            id: i.todo.id.clone(),
            title: first_line(&i.todo.text),
            project: None,
            due: i
                .effective_due
                .as_deref()
                .and_then(|d| DateTime::parse_from_rfc3339(d).ok())
                .map(|d| d.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string()),
            estimate_min: i.effective_estimate_min,
            estimate_missing: i.estimate_missing,
            priority: i.todo.priority,
            status: i.todo.status.clone(),
        })
        .collect()
}

async fn save_plan(db: &Db, plan: &DayPlan) -> Result<(), String> {
    let json = serde_json::to_string(plan).map_err(|e| e.to_string())?;
    sqlx::query(
        "INSERT INTO focus_day_plans (date, plan_json, created_at) VALUES (?, ?, ?) \
         ON CONFLICT(date) DO UPDATE SET plan_json = excluded.plan_json, created_at = excluded.created_at",
    )
    .bind(&plan.date)
    .bind(json)
    .bind(&plan.created_at)
    .execute(db)
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

async fn load_plan(db: &Db, date: &str) -> Result<Option<DayPlan>, String> {
    let json: Option<String> =
        sqlx::query_scalar("SELECT plan_json FROM focus_day_plans WHERE date = ?")
            .bind(date)
            .fetch_optional(db)
            .await
            .map_err(|e| e.to_string())?;
    Ok(json.and_then(|j| serde_json::from_str(&j).ok()))
}

#[tauri::command]
pub async fn focus_ai_plan_today(
    app: AppHandle,
    db: State<'_, Db>,
    state: State<'_, FocusAiState>,
    request_id: Option<String>,
) -> Result<DayPlan, String> {
    let db = db.inner().clone();
    let settings = focus_settings::load(&db).await?;
    let engine = engine_for(&app, &db, &state, &settings);
    let turn = engine.turn.clone();
    run_request(
        &app,
        &state,
        "plan".into(),
        request_id,
        "plan",
        None,
        turn,
        async move {
            let now = local_now();
            let wh = settings.work_hours();
            let todos = load_todos(&db).await?;
            let samples: Vec<(i64, i64)> = sqlx::query_as(
                "SELECT actual_min, estimate_min FROM todos \
             WHERE done = 1 AND actual_min > 0 AND estimate_min > 0 \
             ORDER BY completed_at DESC LIMIT 20",
            )
            .fetch_all(&db)
            .await
            .map_err(|e| e.to_string())?;
            let board = build_board(todos, now, &wh, settings.calibration_enabled, &samples);
            let mut todos = plan_todos(&board.items, ai::PLAN_MAX_TODOS);
            if todos.is_empty() {
                return Err("nothing_to_plan: no open todo".into());
            }
            for t in &mut todos {
                let pid = board
                    .items
                    .iter()
                    .find(|i| i.todo.id == t.id)
                    .and_then(|i| i.todo.project_id.clone());
                t.project = project_name(&db, pid.as_deref()).await;
            }
            let busy = busy_today(&db, &settings).await;
            let free = ai::free_slots(now.naive_local(), &wh, &busy);
            if free.is_empty() {
                return Err("no_free_time: no working time left today".into());
            }
            let lang = app_language(&db).await;
            let prompt = ai::plan_prompt(&ai::PlanCtx {
                now: now.naive_local(),
                lang: &lang,
                todos: &todos,
                free: &free,
            });
            let today = now.date_naive();
            let mut plan = ask(&engine, prompt, |raw| {
                ai::parse_plan(raw, today, &todos, &free)
            })
            .await?;
            plan.created_at = chrono::Utc::now().to_rfc3339();
            save_plan(&db, &plan).await?;
            Ok(plan)
        },
    )
    .await
}

/// The stored plan of `date` (today when omitted), if any.
#[tauri::command]
pub async fn focus_day_plan_get(
    db: State<'_, Db>,
    date: Option<String>,
) -> Result<Option<DayPlan>, String> {
    let date = date.unwrap_or_else(|| local_now().format("%Y-%m-%d").to_string());
    load_plan(db.inner(), &date).await
}

/// Create a Google Calendar event `[Devdy] <todo>` per block of `date` (all,
/// or `block_ids`) not pushed yet, on the primary calendar of the Focus
/// account (Settings → Focus) or else the default Google account.
#[tauri::command]
pub async fn focus_plan_to_calendar(
    db: State<'_, Db>,
    date: String,
    block_ids: Option<Vec<String>>,
) -> Result<DayPlan, String> {
    let pool = db.inner().clone();
    let mut plan = load_plan(&pool, &date)
        .await?
        .ok_or("no_plan: no plan for this day")?;
    let settings = focus_settings::load(&pool).await?;
    let account: Option<String> = match settings
        .calendar_account_id
        .filter(|a| !a.trim().is_empty())
    {
        Some(a) => Some(a),
        None => sqlx::query_scalar(
            "SELECT id FROM google_accounts ORDER BY is_default DESC, label LIMIT 1",
        )
        .fetch_optional(&pool)
        .await
        .map_err(|e| e.to_string())?,
    };
    let account =
        account.ok_or("no_google_account: connect a Google account in Settings → Google")?;
    let day = chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d").map_err(|e| e.to_string())?;
    let titles: HashMap<String, String> = load_todos(&pool)
        .await?
        .into_iter()
        .map(|t| (t.id.clone(), first_line(&t.text)))
        .collect();
    let rfc = |hm: &str| -> Result<String, String> {
        let t = chrono::NaiveTime::parse_from_str(hm, "%H:%M").map_err(|e| e.to_string())?;
        day.and_time(t)
            .and_local_timezone(Local)
            .earliest()
            .map(|d| d.to_rfc3339())
            .ok_or_else(|| "invalid local time".to_string())
    };
    let mut errors = Vec::new();
    for b in plan.blocks.iter_mut() {
        let wanted = block_ids.as_ref().is_none_or(|ids| ids.contains(&b.id));
        if !wanted || b.calendar_event_id.is_some() {
            continue;
        }
        let title = titles
            .get(&b.todo_id)
            .cloned()
            .unwrap_or_else(|| b.todo_id.clone());
        let payload = crate::commands::gcalendar::EventPayload {
            summary: Some(format!("[Devdy] {title}")),
            description: (!b.note.is_empty()).then(|| b.note.clone()),
            start: Some(crate::commands::gcalendar::EventDateTimePayload {
                date_time: Some(rfc(&b.start)?),
                ..Default::default()
            }),
            end: Some(crate::commands::gcalendar::EventDateTimePayload {
                date_time: Some(rfc(&b.end)?),
                ..Default::default()
            }),
            ..Default::default()
        };
        match crate::commands::gcalendar::create_google_calendar_event(
            db.clone(),
            account.clone(),
            "primary".into(),
            payload,
        )
        .await
        {
            Ok(ev) => b.calendar_event_id = Some(ev.id),
            Err(e) => errors.push(e),
        }
    }
    save_plan(&pool, &plan).await?;
    match errors.into_iter().next() {
        // Partial success is saved (no duplicates on retry); the first error is reported.
        Some(e) => Err(format!("calendar_failed: {e}")),
        None => Ok(plan),
    }
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
/// Errors `project_required` when the todo has no project.
#[tauri::command]
pub async fn focus_start_with_ai(
    app: AppHandle,
    db: State<'_, Db>,
    registry: State<'_, RunRegistry>,
    todo_id: String,
) -> Result<String, String> {
    let pool = db.inner().clone();
    let todo = get_todo(&pool, &todo_id).await?;
    let project_id = todo
        .project_id
        .clone()
        .ok_or("project_required: pick a project for this todo first")?;
    let run = crate::commands::runs::create_session_run(db.clone(), project_id, None).await?;
    let title = crate::focus::ai_util::truncate(&first_line(&todo.text), 80);
    sqlx::query("UPDATE runs SET title = ? WHERE id = ?")
        .bind(&title)
        .bind(&run.id)
        .execute(&pool)
        .await
        .map_err(|e| e.to_string())?;
    let payload = crate::commands::runs::StartRunPayload {
        run_id: run.id.clone(),
        engine_override: None,
        permission_mode_override: None,
        prompt_override: Some(ai::start_prompt(
            &todo.id,
            &todo.text,
            todo.source_url.as_deref(),
            todo.ai_plan.as_deref(),
        )),
        model_override: None,
        images: Vec::new(),
        override_budget: false,
        append_system_prompt: None,
        conductor_token: None,
    };
    crate::commands::runs::start_run_inner(
        app.clone(),
        pool.clone(),
        registry.inner().clone(),
        payload,
    )
    .await?;
    item_links::link(
        &pool,
        ItemKind::Todo,
        &todo_id,
        &run.id,
        "created_from_item",
    )
    .await
    .map_err(String::from)?;
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE todos SET status = 'doing', started_at = COALESCE(started_at, ?), updated_at = ? \
         WHERE id = ? AND done = 0",
    )
    .bind(&now)
    .bind(&now)
    .bind(&todo_id)
    .execute(&pool)
    .await
    .map_err(|e| e.to_string())?;
    crate::focus::emit_todos_changed(&app);
    Ok(run.id)
}

// ── BR-11 ───────────────────────────────────────────────────────────────────

fn to_local(s: Option<&str>) -> Option<NaiveDateTime> {
    DateTime::parse_from_rfc3339(s?.trim())
        .ok()
        .map(|d| d.with_timezone(&Local).naive_local())
}

/// Suggested `actual_min` of a todo (BR-11): its linked sessions' total time,
/// else working minutes from start to completion (now if not done yet); none
/// when above 3× the estimate.
#[tauri::command]
pub async fn focus_suggest_actual(
    db: State<'_, Db>,
    todo_id: String,
) -> Result<Option<i64>, String> {
    let db = db.inner();
    let todo = get_todo(db, &todo_id).await?;
    let runs: Vec<(Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT r.started_at, COALESCE(r.finished_at, r.last_activity_at) FROM todo_runs l \
         JOIN runs r ON r.id = l.run_id WHERE l.todo_id = ?",
    )
    .bind(&todo_id)
    .fetch_all(db)
    .await
    .map_err(|e| e.to_string())?;
    let run_minutes: i64 = runs
        .iter()
        .filter_map(|(s, e)| Some((to_local(s.as_deref())?, to_local(e.as_deref())?)))
        .map(|(s, e)| (e - s).num_minutes().max(0))
        .sum();
    let settings = focus_settings::load(db).await?;
    let completed =
        to_local(todo.completed_at.as_deref()).or_else(|| Some(local_now().naive_local()));
    Ok(ai::suggest_actual(
        Some(run_minutes),
        to_local(todo.started_at.as_deref()),
        completed,
        todo.estimate_min,
        &settings.work_hours(),
    ))
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
}
