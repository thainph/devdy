//! Background process tracker.
//!
//! Every long-lived process Devdy spawns (run sidecars, warm translate / Focus AI
//! sessions, mascot, probes, MCP tests, per-run ssh-agents) is registered here.
//! A periodic `ps` sample follows each one's whole tree — its process group plus
//! every descendant by parent link, so children that `setsid()` out of the group
//! are still found — and the list is persisted, so processes that outlive their
//! owner (`leftover`) or the app itself (`orphan`, found at the next launch) stay
//! visible and can be stopped.

pub mod commands;
pub mod ps;
pub mod quit;

use ps::PsRow;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

/// How long a released entry stays hidden while its processes exit normally,
/// before it is reported as a leftover.
const RELEASE_GRACE: Duration = Duration::from_secs(3);
/// Background sampling period.
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);
/// Event carrying [`Counts`] whenever they change.
pub const CHANGED_EVENT: &str = "procs:changed";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcKind {
    Run,
    Translate,
    SlackTranslate,
    /// One-shot Focus AI engine (P3); was the Pulse digest engine.
    #[serde(alias = "pulse_digest")]
    FocusAi,
    Mascot,
    Probe,
    McpTest,
    SshAgent,
}

impl ProcKind {
    /// App plumbing (not user work): stopped silently on quit, auto-killed when
    /// orphaned.
    pub fn is_infra(self) -> bool {
        self != ProcKind::Run
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcState {
    /// Owned by something live in this app session.
    Active,
    /// Its owner ended in this session, but processes it started live on.
    Leftover,
    /// Left over from a previous app session.
    Orphan,
}

#[derive(Debug, Clone)]
pub struct ProcSpec {
    pub kind: ProcKind,
    pub label: String,
    pub run_id: Option<String>,
    pub project_id: Option<String>,
    /// The process leads its own group (spawned with `detach_process_group`).
    pub own_group: bool,
}

impl ProcSpec {
    pub fn new(kind: ProcKind, label: impl Into<String>) -> Self {
        Self {
            kind,
            label: label.into(),
            run_id: None,
            project_id: None,
            own_group: true,
        }
    }

    pub fn run(mut self, run_id: &str, project_id: &str) -> Self {
        self.run_id = Some(run_id.to_string());
        self.project_id = Some(project_id.to_string());
        self
    }

    pub fn no_group(mut self) -> Self {
        self.own_group = false;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    id: String,
    kind: ProcKind,
    label: String,
    run_id: Option<String>,
    project_id: Option<String>,
    /// App session that spawned it.
    session: String,
    root_pid: u32,
    pgid: Option<u32>,
    /// Known live processes: pid -> `ps` start stamp ("" until first sampled).
    members: BTreeMap<u32, String>,
    created_at: String,
    released_at: Option<String>,
    #[serde(skip)]
    released: Option<Instant>,
}

impl Entry {
    fn state(&self, session: &str, now: Instant) -> Option<ProcState> {
        if self.session != session {
            return Some(ProcState::Orphan);
        }
        match self.released {
            None => Some(ProcState::Active),
            Some(t) if now.duration_since(t) >= RELEASE_GRACE => Some(ProcState::Leftover),
            // Just released: give its processes a moment to exit on their own.
            Some(_) => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    /// Active user-work entries (runs) with live processes.
    pub active: usize,
    pub leftover: usize,
    pub orphan: usize,
    pub total_processes: usize,
}

/// One tracked entry as seen in the latest sample.
#[derive(Debug, Clone)]
pub struct EntryView {
    pub id: String,
    pub kind: ProcKind,
    pub state: ProcState,
    pub label: String,
    pub run_id: Option<String>,
    pub project_id: Option<String>,
    pub root_pid: u32,
    pub created_at: String,
    pub released_at: Option<String>,
    pub processes: Vec<PsRow>,
}

/// What to signal to stop one entry.
#[derive(Debug, Clone)]
pub struct StopTarget {
    pub id: String,
    pub kind: ProcKind,
    pub state: ProcState,
    pub run_id: Option<String>,
    /// `(pid, start stamp)` of every live process.
    pub pids: Vec<(u32, String)>,
    /// Process groups that still hold one of those processes.
    pub groups: Vec<u32>,
}

struct Inner {
    /// Wakes the sampler early (a process was registered or released).
    kick: Arc<tokio::sync::Notify>,
    session: String,
    path: Option<PathBuf>,
    entries: HashMap<String, Entry>,
    /// Live rows per entry from the latest sample.
    live: HashMap<String, Vec<PsRow>>,
    last_counts: Counts,
}

#[derive(Clone)]
pub struct ProcTracker {
    inner: Arc<Mutex<Inner>>,
}

impl Default for ProcTracker {
    fn default() -> Self {
        Self::new(None)
    }
}

impl ProcTracker {
    /// A tracker persisted at `path` (None: in memory only). Entries already in
    /// the file belong to previous app sessions.
    pub fn new(path: Option<PathBuf>) -> Self {
        let entries = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<Vec<Entry>>(&b).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|e| (e.id.clone(), e))
            .collect();
        Self {
            inner: Arc::new(Mutex::new(Inner {
                kick: Arc::new(tokio::sync::Notify::new()),
                session: uuid::Uuid::new_v4().to_string(),
                path,
                entries,
                live: HashMap::new(),
                last_counts: Counts::default(),
            })),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Start tracking a just-spawned process. Dropping the guard marks it ended
    /// (its processes are then watched until they are gone).
    pub fn register(&self, spec: ProcSpec, pid: u32) -> ProcGuard {
        let id = uuid::Uuid::new_v4().to_string();
        let mut inner = self.lock();
        let entry = Entry {
            id: id.clone(),
            kind: spec.kind,
            label: spec.label,
            run_id: spec.run_id,
            project_id: spec.project_id,
            session: inner.session.clone(),
            root_pid: pid,
            pgid: spec.own_group.then_some(pid),
            members: BTreeMap::from([(pid, String::new())]),
            created_at: chrono::Utc::now().to_rfc3339(),
            released_at: None,
            released: None,
        };
        inner.entries.insert(id.clone(), entry);
        persist(&inner);
        // Stamp it soon, so a crash right after leaves a verifiable record.
        inner.kick.notify_one();
        ProcGuard {
            tracker: self.clone(),
            id,
        }
    }

    /// The owner of `id` is done with it.
    pub fn release(&self, id: &str) {
        let mut inner = self.lock();
        if let Some(e) = inner.entries.get_mut(id) {
            if e.released.is_none() {
                e.released = Some(Instant::now());
                e.released_at = Some(chrono::Utc::now().to_rfc3339());
                persist(&inner);
                // Catch what the owner started before it gets re-parented.
                inner.kick.notify_one();
            }
        }
    }

    /// Fold a fresh `ps` snapshot in: refresh each entry's live tree, drop the
    /// ones that are gone. Returns the new counts and whether they changed.
    pub fn apply_sample(&self, rows: &[PsRow]) -> (Counts, bool) {
        let me = std::process::id();
        let by_pid: HashMap<u32, &PsRow> = rows
            .iter()
            .filter(|r| !r.is_zombie())
            .map(|r| (r.pid, r))
            .collect();
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut by_pgid: HashMap<u32, Vec<u32>> = HashMap::new();
        for r in by_pid.values() {
            children.entry(r.ppid).or_default().push(r.pid);
            by_pgid.entry(r.pgid).or_default().push(r.pid);
        }

        let mut inner = self.lock();
        let mut dirty = false;
        let mut live = HashMap::new();
        let session = inner.session.clone();
        inner.entries.retain(|id, e| {
            let mut alive: BTreeMap<u32, String> = BTreeMap::new();
            let current = e.session == session;
            for (pid, start) in &e.members {
                if *pid == me {
                    continue;
                }
                if let Some(r) = by_pid.get(pid) {
                    // An unstamped pid (registered, not sampled yet) is only
                    // trusted within the session that registered it: across a
                    // restart the pid may belong to anything by now.
                    if (start.is_empty() && current) || *start == r.start {
                        alive.insert(*pid, r.start.clone());
                    }
                }
            }
            // The rest of its process group. The group id can only be reused
            // once the group is empty, so it is still ours while a known member
            // is in it.
            if let Some(g) = e.pgid {
                let ours = alive
                    .keys()
                    .any(|p| by_pid.get(p).is_some_and(|r| r.pgid == g));
                if ours {
                    for p in by_pgid.get(&g).into_iter().flatten() {
                        if *p != me {
                            alive.entry(*p).or_insert_with(|| by_pid[p].start.clone());
                        }
                    }
                }
            }
            // Every descendant, including ones that left the group.
            let mut stack: Vec<u32> = alive.keys().copied().collect();
            while let Some(p) = stack.pop() {
                for c in children.get(&p).into_iter().flatten() {
                    if *c != me && !alive.contains_key(c) {
                        alive.insert(*c, by_pid[c].start.clone());
                        stack.push(*c);
                    }
                }
            }
            if alive != e.members {
                e.members = alive;
                dirty = true;
            }
            let ended = e.released.is_some() || e.session != session;
            if e.members.is_empty() && ended {
                dirty = true;
                return false;
            }
            let rows: Vec<PsRow> = e
                .members
                .keys()
                .filter_map(|p| by_pid.get(p).map(|r| (*r).clone()))
                .collect();
            live.insert(id.clone(), rows);
            true
        });
        inner.live = live;
        if dirty {
            persist(&inner);
        }
        let counts = counts_of(&inner, Instant::now());
        let changed = counts != inner.last_counts;
        inner.last_counts = counts.clone();
        (counts, changed)
    }

    pub fn counts(&self) -> Counts {
        let inner = self.lock();
        counts_of(&inner, Instant::now())
    }

    /// Visible entries (with live processes) from the latest sample.
    pub fn view(&self) -> Vec<EntryView> {
        let inner = self.lock();
        let now = Instant::now();
        let mut out: Vec<EntryView> = inner
            .entries
            .values()
            .filter_map(|e| {
                let state = e.state(&inner.session, now)?;
                let processes = inner.live.get(&e.id).cloned().unwrap_or_default();
                if processes.is_empty() {
                    return None;
                }
                Some(EntryView {
                    id: e.id.clone(),
                    kind: e.kind,
                    state,
                    label: e.label.clone(),
                    run_id: e.run_id.clone(),
                    project_id: e.project_id.clone(),
                    root_pid: e.root_pid,
                    created_at: e.created_at.clone(),
                    released_at: e.released_at.clone(),
                    processes,
                })
            })
            .collect();
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        out
    }

    /// Stop targets for `ids` (all visible entries when `None`), from the
    /// latest sample.
    pub fn targets(&self, ids: Option<&[String]>) -> Vec<StopTarget> {
        let inner = self.lock();
        let now = Instant::now();
        inner
            .entries
            .values()
            .filter(|e| ids.is_none_or(|ids| ids.contains(&e.id)))
            .filter_map(|e| {
                let state = e.state(&inner.session, now).unwrap_or(ProcState::Leftover);
                let rows = inner.live.get(&e.id)?;
                if rows.is_empty() {
                    return None;
                }
                let mut groups: Vec<u32> = rows
                    .iter()
                    .filter(|r| Some(r.pgid) == e.pgid)
                    .map(|r| r.pgid)
                    .collect();
                groups.dedup();
                Some(StopTarget {
                    id: e.id.clone(),
                    kind: e.kind,
                    state,
                    run_id: e.run_id.clone(),
                    pids: rows.iter().map(|r| (r.pid, r.start.clone())).collect(),
                    groups,
                })
            })
            .collect()
    }

    /// The entry owning `pid` and the process's start stamp (latest sample).
    pub fn owner_of(&self, pid: u32) -> Option<(String, String)> {
        let inner = self.lock();
        inner.live.iter().find_map(|(id, rows)| {
            rows.iter()
                .find(|r| r.pid == pid)
                .map(|r| (id.clone(), r.start.clone()))
        })
    }

    /// Forget `pid` in an entry (after it was signalled at startup).
    fn forget_pid(&self, id: &str, pid: u32) {
        let mut inner = self.lock();
        let mut drop_entry = false;
        if let Some(e) = inner.entries.get_mut(id) {
            e.members.remove(&pid);
            drop_entry = e.members.is_empty();
        }
        if let Some(rows) = inner.live.get_mut(id) {
            rows.retain(|r| r.pid != pid);
        }
        if drop_entry {
            inner.entries.remove(id);
            inner.live.remove(id);
        }
        persist(&inner);
    }

    fn forget(&self, id: &str) {
        let mut inner = self.lock();
        inner.entries.remove(id);
        inner.live.remove(id);
        persist(&inner);
    }

    pub fn persist_now(&self) {
        persist(&self.lock());
    }
}

fn counts_of(inner: &Inner, now: Instant) -> Counts {
    let mut c = Counts::default();
    for e in inner.entries.values() {
        let Some(state) = e.state(&inner.session, now) else {
            continue;
        };
        let n = inner.live.get(&e.id).map_or(0, |r| r.len());
        if n == 0 {
            continue;
        }
        match state {
            ProcState::Active if !e.kind.is_infra() => c.active += 1,
            ProcState::Active => {}
            ProcState::Leftover => c.leftover += 1,
            ProcState::Orphan => c.orphan += 1,
        }
        c.total_processes += n;
    }
    c
}

/// Write the entry list atomically (tmp + rename). Best effort.
fn persist(inner: &Inner) {
    let Some(path) = &inner.path else { return };
    let list: Vec<&Entry> = inner.entries.values().collect();
    let Ok(bytes) = serde_json::to_vec_pretty(&list) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// Lifetime handle of one registration: dropping it marks the entry ended.
pub struct ProcGuard {
    tracker: ProcTracker,
    id: String,
}

impl Drop for ProcGuard {
    fn drop(&mut self) {
        self.tracker.release(&self.id);
    }
}

impl std::fmt::Debug for ProcGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcGuard").field("id", &self.id).finish()
    }
}

static GLOBAL: std::sync::OnceLock<ProcTracker> = std::sync::OnceLock::new();

/// Make `tracker` the one [`track`] registers with (once, at startup).
pub fn set_global(tracker: ProcTracker) {
    let _ = GLOBAL.set(tracker);
}

/// Register `pid` with the app's tracker, if there is one (unit tests have none).
pub fn track(spec: ProcSpec, pid: Option<u32>) -> Option<ProcGuard> {
    Some(GLOBAL.get()?.register(spec, pid?))
}

// ---- signalling -------------------------------------------------------------

#[cfg(unix)]
fn signal_pid(pid: u32, sig: libc::c_int) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, sig) == 0 }
}

#[cfg(unix)]
fn signal_group(pgid: u32, sig: libc::c_int) -> bool {
    unsafe { libc::killpg(pgid as libc::pid_t, sig) == 0 }
}

/// Stop processes: SIGTERM (SIGKILL when `force`), wait up to `wait` for them
/// to exit, then SIGKILL whatever is left. Identity is re-checked against a
/// fresh `ps` snapshot before every signal, so a recycled pid is never hit.
/// Returns how many processes were signalled.
pub async fn terminate(
    pids: Vec<(u32, String)>,
    groups: Vec<u32>,
    force: bool,
    wait: Duration,
) -> usize {
    tokio::task::spawn_blocking(move || terminate_blocking(&pids, &groups, force, wait))
        .await
        .unwrap_or(0)
}

/// Synchronous [`terminate`], for the quit path.
pub fn terminate_blocking(
    pids: &[(u32, String)],
    groups: &[u32],
    force: bool,
    wait: Duration,
) -> usize {
    #[cfg(unix)]
    {
        let still_ours = |rows: &[PsRow]| -> (Vec<u32>, Vec<u32>) {
            let live: Vec<&PsRow> = rows
                .iter()
                .filter(|r| !r.is_zombie())
                .filter(|r| {
                    pids.iter()
                        .any(|(p, s)| *p == r.pid && (s.is_empty() || *s == r.start))
                })
                .collect();
            let g: Vec<u32> = groups
                .iter()
                .copied()
                .filter(|g| live.iter().any(|r| r.pgid == *g))
                .collect();
            (live.iter().map(|r| r.pid).collect(), g)
        };
        let Some(rows) = ps::snapshot() else { return 0 };
        let (live, live_groups) = still_ours(&rows);
        if live.is_empty() {
            return 0;
        }
        let first = if force { libc::SIGKILL } else { libc::SIGTERM };
        for g in &live_groups {
            signal_group(*g, first);
        }
        for p in &live {
            signal_pid(*p, first);
        }
        if force {
            return live.len();
        }
        let deadline = Instant::now() + wait;
        loop {
            std::thread::sleep(Duration::from_millis(150));
            let Some(rows) = ps::snapshot() else { break };
            let (left, left_groups) = still_ours(&rows);
            if left.is_empty() {
                break;
            }
            if Instant::now() >= deadline {
                for g in &left_groups {
                    signal_group(*g, libc::SIGKILL);
                }
                for p in &left {
                    signal_pid(*p, libc::SIGKILL);
                }
                break;
            }
        }
        live.len()
    }
    #[cfg(not(unix))]
    {
        let _ = (pids, groups, force, wait);
        0
    }
}

// ---- lifecycle --------------------------------------------------------------

/// Startup sweep over entries left by previous app sessions:
/// - app plumbing (translate, probes, ssh-agent…) is stopped outright — it is
///   useless without the app, and an ssh-agent holds decrypted keys;
/// - an orphaned run's sidecar root is stopped (it would keep a turn running
///   with nobody listening); whatever else the run started stays listed for the
///   user to decide.
pub fn startup_sweep(tracker: &ProcTracker) {
    let Some(rows) = ps::snapshot() else { return };
    tracker.apply_sample(&rows);
    let targets = tracker.targets(None);
    let mut doomed: Vec<(u32, String)> = Vec::new();
    let mut doomed_groups: Vec<u32> = Vec::new();
    for t in &targets {
        if t.state != ProcState::Orphan {
            continue;
        }
        if t.kind.is_infra() {
            doomed.extend(t.pids.iter().cloned());
            doomed_groups.extend(t.groups.iter().copied());
            tracker.forget(&t.id);
            continue;
        }
        let root = tracker.lock().entries.get(&t.id).map(|e| e.root_pid);
        if let Some(p) = root.and_then(|root| t.pids.iter().find(|(p, _)| *p == root)) {
            doomed.push(p.clone());
            tracker.forget_pid(&t.id, p.0);
        }
    }
    if !doomed.is_empty() {
        tracing::info!(event = "procs_startup_sweep", processes = doomed.len());
        std::thread::spawn(move || {
            terminate_blocking(&doomed, &doomed_groups, false, Duration::from_secs(3));
        });
    }
    // Re-sample so the root removal is reflected (its children stay listed).
    if let Some(rows) = ps::snapshot() {
        tracker.apply_sample(&rows);
    }
}

/// Periodic sampler: keeps the live view fresh and emits [`CHANGED_EVENT`].
pub fn start_sampler<R: tauri::Runtime>(app: AppHandle<R>, tracker: ProcTracker) {
    tauri::async_runtime::spawn(async move {
        let kick = tracker.lock().kick.clone();
        let mut tick = tokio::time::interval(SAMPLE_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = kick.notified() => {
                    // Coalesce bursts (a run registers its sidecar and agent).
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
            }
            let rows = tokio::task::spawn_blocking(ps::snapshot)
                .await
                .ok()
                .flatten();
            if let Some(rows) = rows {
                let (counts, changed) = tracker.apply_sample(&rows);
                if changed {
                    let _ = app.emit(CHANGED_EVENT, counts);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: u32, ppid: u32, pgid: u32) -> PsRow {
        PsRow {
            pid,
            ppid,
            pgid,
            start: format!("start-{pid}"),
            rss_kb: 100,
            cpu: 0.0,
            stat: "S".into(),
            command: format!("cmd{pid}"),
        }
    }

    #[test]
    fn follows_group_and_escaped_descendants() {
        let t = ProcTracker::default();
        let g = t.register(ProcSpec::new(ProcKind::Run, "r"), 100);
        // 101: same group; 102: setsid'd child of 101; 200: unrelated.
        let rows = vec![
            row(100, 1, 100),
            row(101, 100, 100),
            row(102, 101, 102),
            row(200, 1, 200),
        ];
        let (c, _) = t.apply_sample(&rows);
        assert_eq!(c.active, 1);
        assert_eq!(c.total_processes, 3);
        let v = t.view();
        let pids: Vec<u32> = v[0].processes.iter().map(|r| r.pid).collect();
        assert_eq!(pids, vec![100, 101, 102]);

        // Run ends; the sidecar exits but the escaped child lives on.
        drop(g);
        let rows = vec![row(102, 1, 102), row(200, 1, 200)];
        t.apply_sample(&rows);
        // Within the grace window it is not reported yet…
        assert_eq!(t.counts().leftover, 0);
        // …afterwards it is a leftover.
        {
            let mut inner = t.lock();
            for e in inner.entries.values_mut() {
                e.released = Some(Instant::now() - RELEASE_GRACE);
            }
        }
        let (c, _) = t.apply_sample(&rows);
        assert_eq!(c.leftover, 1);
        assert_eq!(t.view()[0].state, ProcState::Leftover);

        // Gone → entry dropped.
        t.apply_sample(&[row(200, 1, 200)]);
        assert!(t.view().is_empty());
        assert!(t.lock().entries.is_empty());
    }

    #[test]
    fn recycled_pid_is_not_ours() {
        let t = ProcTracker::default();
        let g = t.register(ProcSpec::new(ProcKind::Mascot, "m"), 300);
        t.apply_sample(&[row(300, 1, 300)]);
        drop(g);
        let mut other = row(300, 1, 300);
        other.start = "later".into();
        t.apply_sample(&[other]);
        assert!(t.lock().entries.is_empty());
    }

    #[test]
    fn active_entry_with_no_process_is_hidden_but_kept() {
        let t = ProcTracker::default();
        let _g = t.register(ProcSpec::new(ProcKind::Translate, "t"), 400);
        t.apply_sample(&[]);
        assert!(t.view().is_empty());
        assert_eq!(t.lock().entries.len(), 1);
    }

    #[test]
    fn persisted_entries_become_orphans() {
        let dir = std::env::temp_dir().join(format!("devdy-procs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("procs.json");
        {
            let t = ProcTracker::new(Some(path.clone()));
            let g = t.register(ProcSpec::new(ProcKind::Run, "r").run("run1", "p1"), 500);
            t.apply_sample(&[row(500, 1, 500), row(501, 500, 500)]);
            std::mem::forget(g); // app died without releasing
        }
        let t = ProcTracker::new(Some(path.clone()));
        let (c, _) = t.apply_sample(&[row(500, 1, 500), row(501, 500, 500)]);
        assert_eq!(c.orphan, 1);
        let v = t.view();
        assert_eq!(v[0].state, ProcState::Orphan);
        assert_eq!(v[0].run_id.as_deref(), Some("run1"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unstamped_pid_from_a_previous_session_is_not_trusted() {
        let dir = std::env::temp_dir().join(format!("devdy-procs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("procs.json");
        {
            let t = ProcTracker::new(Some(path.clone()));
            // Registered, never sampled, app died.
            std::mem::forget(t.register(ProcSpec::new(ProcKind::Probe, "p"), 600));
        }
        let t = ProcTracker::new(Some(path));
        // pid 600 now belongs to some unrelated group leader with a child.
        t.apply_sample(&[row(600, 1, 600), row(601, 600, 600)]);
        assert!(t.view().is_empty());
        assert!(t.targets(None).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn never_tracks_the_app_itself() {
        let t = ProcTracker::default();
        let me = std::process::id();
        let _g = t.register(ProcSpec::new(ProcKind::Probe, "p"), me);
        t.apply_sample(&[row(me, 1, me)]);
        assert!(t.view().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminate_stops_a_real_process_tree() {
        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sleep 60 & sleep 60; wait"])
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = child.id();
        std::thread::sleep(Duration::from_millis(200));
        let t = ProcTracker::default();
        let g = t.register(ProcSpec::new(ProcKind::Probe, "p"), pid);
        t.apply_sample(&ps::snapshot().unwrap());
        let target = t.targets(None).pop().unwrap();
        assert!(target.pids.len() >= 3, "sh + 2 sleeps: {:?}", target.pids);
        let n = terminate(
            target.pids.clone(),
            target.groups.clone(),
            false,
            Duration::from_secs(2),
        )
        .await;
        assert!(n >= 3);
        let _ = child.wait();
        drop(g);
        let rows = ps::snapshot().unwrap();
        assert!(target.pids.iter().all(|(p, s)| !rows
            .iter()
            .any(|r| r.pid == *p && r.start == *s && !r.is_zombie())));
    }
}
