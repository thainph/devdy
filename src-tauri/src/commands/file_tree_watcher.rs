//! On-demand file-system watcher for the VSCode-style file-tree panel.
//!
//! Unlike [`crate::runs::session_watcher`] (which tails the transcript stores for
//! the whole app lifetime), this watcher is **scoped to what the user is actually
//! looking at**: the frontend starts it only while the Files tab is open and feeds
//! it the set of currently expanded+loaded directories. Each directory is watched
//! **non-recursively**, so heavy trees (`node_modules`, build output, …) are never
//! watched unless the user explicitly expands them.
//!
//! On a debounced change it emits `file_tree:changed` with `{ project_path, dir }`
//! (POSIX relative dir, `""` = root). The frontend reloads just that directory's
//! children if it is cached, so external edits (git checkout, another editor, a
//! terminal `mkdir`, …) show up without a manual reload.

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, State};

/// Coalesce the burst of events an editor/tool emits while writing files.
const DEBOUNCE: Duration = Duration::from_millis(400);

/// One project's live watcher: the OS watcher handle plus the absolute dirs it is
/// currently watching (so we can diff against the next desired set).
struct ProjectWatcher {
    watcher: RecommendedWatcher,
    watched: HashSet<PathBuf>,
}

/// Managed state: at most one [`ProjectWatcher`] per project path.
#[derive(Default)]
pub struct FileTreeWatchers(Mutex<HashMap<String, ProjectWatcher>>);

/// Start (or update) watching a project's file tree. `rel_dirs` is the full set of
/// directories the panel currently shows expanded (POSIX relative, `""` = root);
/// the watcher is synced to exactly that set — new dirs are watched, dirs no longer
/// expanded are unwatched. Idempotent: safe to call on every expand/collapse.
#[tauri::command]
pub fn set_file_tree_watch(
    project_path: String,
    rel_dirs: Vec<String>,
    app: AppHandle,
    state: State<'_, FileTreeWatchers>,
) -> Result<(), String> {
    let root = PathBuf::from(&project_path)
        .canonicalize()
        .map_err(|e| format!("invalid project path: {e}"))?;

    // Resolve the desired dirs, confined to the project root and existing on disk.
    let mut desired: HashSet<PathBuf> = HashSet::new();
    for rel in &rel_dirs {
        let abs = if rel.is_empty() {
            root.clone()
        } else {
            root.join(rel)
        };
        if let Ok(c) = abs.canonicalize() {
            if c.starts_with(&root) && c.is_dir() {
                desired.insert(c);
            }
        }
    }

    let mut map = state.0.lock().unwrap();

    // Lazily create the watcher + its debounce/emit consumer thread on first use.
    if !map.contains_key(&project_path) {
        let (tx, rx) = std::sync::mpsc::channel();
        let watcher = notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
        })
        .map_err(|e| format!("failed to create watcher: {e}"))?;
        spawn_consumer(rx, root.clone(), project_path.clone(), app);
        map.insert(
            project_path.clone(),
            ProjectWatcher {
                watcher,
                watched: HashSet::new(),
            },
        );
    }

    let pw = map.get_mut(&project_path).unwrap();
    // Add newly expanded dirs (non-recursive: only direct children fire events).
    for dir in desired.difference(&pw.watched.clone()) {
        let _ = pw.watcher.watch(dir, RecursiveMode::NonRecursive);
    }
    // Drop dirs the user collapsed / that are no longer visible.
    for dir in pw.watched.clone().difference(&desired) {
        let _ = pw.watcher.unwatch(dir);
    }
    pw.watched = desired;
    Ok(())
}

/// Stop watching a project's file tree entirely. Called when the Files tab is
/// hidden, the project changes, or the panel unmounts. Dropping the watcher lets
/// its consumer thread exit (the event channel sender is dropped with it).
#[tauri::command]
pub fn stop_file_tree_watch(project_path: String, state: State<'_, FileTreeWatchers>) {
    state.0.lock().unwrap().remove(&project_path);
}

/// Own the event channel: block for a change, drain a short debounce window, then
/// emit one `file_tree:changed` per affected directory. Exits when the watcher is
/// dropped (sender gone → `recv` errors).
fn spawn_consumer(
    rx: Receiver<notify::Result<notify::Event>>,
    root: PathBuf,
    project_path: String,
    app: AppHandle,
) {
    std::thread::spawn(move || loop {
        let first = match rx.recv() {
            Ok(ev) => ev,
            Err(_) => break, // watcher dropped — stop requested / app shutting down
        };
        let mut dirs: HashSet<String> = HashSet::new();
        collect(first, &root, &mut dirs);
        let deadline = Instant::now() + DEBOUNCE;
        while let Ok(ev) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            collect(ev, &root, &mut dirs);
        }
        for dir in dirs {
            let _ = app.emit(
                "file_tree:changed",
                serde_json::json!({ "project_path": project_path, "dir": dir }),
            );
        }
    });
}

/// Map a watcher event to the relative directories whose listing may have changed.
/// A non-recursive watch reports its children's paths, so the directory to reload
/// is each touched path's parent (relative to the project root, POSIX, `""` = root).
fn collect(res: notify::Result<notify::Event>, root: &Path, out: &mut HashSet<String>) {
    let Ok(event) = res else { return };
    for path in event.paths {
        let Some(parent) = path.parent() else { continue };
        let Ok(rel) = parent.strip_prefix(root) else {
            continue;
        };
        out.insert(rel.to_string_lossy().replace('\\', "/"));
    }
}
