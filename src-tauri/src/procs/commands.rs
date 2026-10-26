//! Tauri commands of the background-process manager.

use super::{ps, terminate, Counts, EntryView, ProcKind, ProcState, ProcTracker, CHANGED_EVENT};
use crate::db::Db;
use crate::runs::RunRegistry;
use serde::Serialize;
use sqlx::Row;
use std::collections::HashMap;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};

/// Grace given to a process between SIGTERM and SIGKILL on a user stop.
const STOP_GRACE: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Serialize)]
pub struct BgProcess {
    pub pid: u32,
    pub ppid: u32,
    pub pgid: u32,
    pub command: String,
    pub cpu: f32,
    pub rss_kb: u64,
    pub started_at: Option<String>,
    pub is_root: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct BgEntry {
    pub id: String,
    pub kind: ProcKind,
    pub state: ProcState,
    pub label: String,
    pub run_id: Option<String>,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub created_at: String,
    pub ended_at: Option<String>,
    pub infra: bool,
    pub processes: Vec<BgProcess>,
    pub cpu: f32,
    pub rss_kb: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BgSnapshot {
    pub entries: Vec<BgEntry>,
    pub counts: Counts,
}

/// Take a fresh sample and emit [`CHANGED_EVENT`] if the counts moved.
pub async fn refresh(app: &AppHandle, tracker: &ProcTracker) -> Counts {
    let rows = tokio::task::spawn_blocking(ps::snapshot)
        .await
        .ok()
        .flatten();
    match rows {
        Some(rows) => {
            let (counts, changed) = tracker.apply_sample(&rows);
            if changed {
                let _ = app.emit(CHANGED_EVENT, counts.clone());
            }
            counts
        }
        None => tracker.counts(),
    }
}

/// Run titles and project names for the entries' labels.
async fn enrich(db: &Db, views: Vec<EntryView>) -> Vec<BgEntry> {
    let mut run_info: HashMap<String, (String, String)> = HashMap::new();
    let run_ids: Vec<&String> = views.iter().filter_map(|v| v.run_id.as_ref()).collect();
    for run_id in run_ids {
        if run_info.contains_key(run_id) {
            continue;
        }
        let row = sqlx::query(
            "SELECT COALESCE(NULLIF(TRIM(title), ''), type || COALESCE(' #' || ref_number, '')) AS label, \
             project_id FROM runs WHERE id = ?",
        )
        .bind(run_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
        if let Some(row) = row {
            run_info.insert(
                run_id.clone(),
                (
                    row.get::<String, _>("label"),
                    row.get::<String, _>("project_id"),
                ),
            );
        }
    }
    let mut project_names: HashMap<String, String> = HashMap::new();
    if let Ok(rows) = sqlx::query("SELECT id, name FROM projects")
        .fetch_all(db)
        .await
    {
        for r in rows {
            project_names.insert(r.get("id"), r.get("name"));
        }
    }

    views
        .into_iter()
        .map(|v| {
            let info = v.run_id.as_ref().and_then(|r| run_info.get(r));
            let label = info
                .filter(|_| v.kind == ProcKind::Run)
                .map(|(l, _)| l.clone())
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| v.label.clone());
            let project_id = v
                .project_id
                .clone()
                .or_else(|| info.map(|(_, p)| p.clone()));
            let project_name = project_id
                .as_ref()
                .and_then(|p| project_names.get(p).cloned());
            let processes: Vec<BgProcess> = v
                .processes
                .iter()
                .map(|r| BgProcess {
                    pid: r.pid,
                    ppid: r.ppid,
                    pgid: r.pgid,
                    command: r.command.clone(),
                    cpu: r.cpu,
                    rss_kb: r.rss_kb,
                    started_at: ps::start_to_rfc3339(&r.start),
                    is_root: r.pid == v.root_pid,
                })
                .collect();
            BgEntry {
                id: v.id,
                kind: v.kind,
                state: v.state,
                label,
                run_id: v.run_id,
                project_id,
                project_name,
                created_at: v.created_at,
                ended_at: v.released_at,
                infra: v.kind.is_infra(),
                cpu: processes.iter().map(|p| p.cpu).sum(),
                rss_kb: processes.iter().map(|p| p.rss_kb).sum(),
                processes,
            }
        })
        .collect()
}

/// Current entries (after a fresh sample) with labels resolved.
pub async fn snapshot(app: &AppHandle, tracker: &ProcTracker, db: &Db) -> BgSnapshot {
    let counts = refresh(app, tracker).await;
    BgSnapshot {
        entries: enrich(db, tracker.view()).await,
        counts,
    }
}

#[tauri::command]
pub async fn list_background_processes(
    app: AppHandle,
    tracker: State<'_, ProcTracker>,
    db: State<'_, Db>,
) -> Result<BgSnapshot, String> {
    Ok(snapshot(&app, tracker.inner(), db.inner()).await)
}

/// Stop entries. An active run is cancelled first (DB status, registry), the
/// rest of its tree — including processes that left its group — is then
/// terminated. Returns the number of processes signalled.
#[tauri::command]
pub async fn stop_background_processes(
    app: AppHandle,
    tracker: State<'_, ProcTracker>,
    registry: State<'_, RunRegistry>,
    db: State<'_, Db>,
    ids: Vec<String>,
    force: Option<bool>,
) -> Result<usize, String> {
    let force = force.unwrap_or(false);
    refresh(&app, tracker.inner()).await;
    let targets = tracker.targets(Some(&ids));
    for t in &targets {
        if t.kind == ProcKind::Run && t.state == ProcState::Active {
            if let Some(run_id) = &t.run_id {
                if registry.lock().await.contains_key(run_id) {
                    crate::commands::runs::cancel_run_inner(
                        registry.inner(),
                        db.inner(),
                        run_id,
                        true,
                    )
                    .await?;
                }
            }
        }
    }
    let mut pids = Vec::new();
    let mut groups = Vec::new();
    for t in targets {
        pids.extend(t.pids);
        groups.extend(t.groups);
    }
    let n = terminate(pids, groups, force, STOP_GRACE).await;
    refresh(&app, tracker.inner()).await;
    Ok(n)
}

/// Stop one process of a tracked entry.
#[tauri::command]
pub async fn kill_background_process(
    app: AppHandle,
    tracker: State<'_, ProcTracker>,
    pid: u32,
    force: Option<bool>,
) -> Result<(), String> {
    refresh(&app, tracker.inner()).await;
    let (_, start) = tracker
        .owner_of(pid)
        .ok_or_else(|| format!("Process {pid} is not tracked by Devdy"))?;
    terminate(
        vec![(pid, start)],
        Vec::new(),
        force.unwrap_or(false),
        STOP_GRACE,
    )
    .await;
    refresh(&app, tracker.inner()).await;
    Ok(())
}

/// Make the managed tracker for the app: persisted under app data, swept for
/// leftovers of previous sessions, sampled in the background.
pub fn install(app: &tauri::App, app_data_dir: &std::path::Path) {
    let tracker = ProcTracker::new(Some(app_data_dir.join("background-processes.json")));
    super::startup_sweep(&tracker);
    // No run is alive yet, so every per-run ssh-agent (it holds decrypted
    // keys) and scratch dir (config + agent socket) is a leftover of a session
    // that did not shut down cleanly — including ones from before tracking.
    let ssh_runs = app_data_dir.join("ssh-runs");
    if let Some(rows) = super::ps::snapshot() {
        let marker = ssh_runs.to_string_lossy().to_string();
        let agents: Vec<(u32, String)> = rows
            .iter()
            .filter(|r| r.command.contains("ssh-agent") && r.command.contains(&marker))
            .map(|r| (r.pid, r.start.clone()))
            .collect();
        if !agents.is_empty() {
            std::thread::spawn(move || {
                super::terminate_blocking(&agents, &[], false, Duration::from_secs(2));
            });
        }
    }
    if let Ok(dirs) = std::fs::read_dir(&ssh_runs) {
        for d in dirs.flatten() {
            let _ = std::fs::remove_dir_all(d.path());
        }
    }
    super::set_global(tracker.clone());
    super::start_sampler(app.handle().clone(), tracker.clone());
    app.manage(tracker);
    app.manage(super::quit::QuitState::default());
}
