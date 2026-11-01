//! Launch Devdy at login (macOS): a per-user LaunchAgent at
//! `~/Library/LaunchAgents/<bundle identifier>.plist` with `RunAtLoad`.
//!
//! The OS file IS the setting — nothing is stored in SQLite, so the toggle can't
//! drift from what macOS actually does. The identifier keeps the dev build
//! ("Devdy Dev", `vn.papay.devdy.dev`) and the release build apart.
//!
//! The agent may pass [`MINIMIZED_ARG`]: the app then starts with its main
//! window minimized to the Dock (closing it would quit the app, see `lib.rs`).

use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager, Runtime};

/// Launch argument: start with the main window minimized.
pub const MINIMIZED_ARG: &str = "--minimized";

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
pub struct AutostartStatus {
    pub enabled: bool,
    pub minimized: bool,
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn render_plist(label: &str, exe: &Path, minimized: bool) -> String {
    let mut args = format!(
        "        <string>{}</string>\n",
        xml_escape(&exe.to_string_lossy())
    );
    if minimized {
        args.push_str(&format!("        <string>{MINIMIZED_ARG}</string>\n"));
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{}</string>
    <key>ProgramArguments</key>
    <array>
{args}    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>ProcessType</key>
    <string>Interactive</string>
</dict>
</plist>
"#,
        xml_escape(label)
    )
}

/// Whether an existing agent file asks for a minimized start. Only Devdy writes
/// this file, so a plain substring check is enough.
fn plist_is_minimized(content: &str) -> bool {
    content.contains(&format!("<string>{MINIMIZED_ARG}</string>"))
}

fn plist_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    let home = app.path().home_dir().map_err(|e| e.to_string())?;
    let id = &app.config().identifier;
    Ok(home
        .join("Library/LaunchAgents")
        .join(format!("{id}.plist")))
}

fn read_status(path: &Path) -> AutostartStatus {
    match std::fs::read_to_string(path) {
        Ok(content) => AutostartStatus {
            enabled: true,
            minimized: plist_is_minimized(&content),
        },
        Err(_) => AutostartStatus {
            enabled: false,
            minimized: false,
        },
    }
}

fn write_agent<R: Runtime>(app: &AppHandle<R>, path: &Path, minimized: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let content = render_plist(&app.config().identifier, &exe, minimized);
    // Skip identical rewrites (startup refresh) so macOS doesn't re-announce
    // the login item.
    if std::fs::read_to_string(path).ok().as_deref() == Some(content.as_str()) {
        return Ok(());
    }
    std::fs::write(path, content).map_err(|e| e.to_string())
}

/// Startup: an enabled agent still points at the previous executable after the
/// app was moved or updated in place — rewrite it with the current path.
pub fn refresh<R: Runtime>(app: &AppHandle<R>) {
    if !cfg!(target_os = "macos") {
        return;
    }
    let Ok(path) = plist_path(app) else { return };
    let status = read_status(&path);
    if status.enabled {
        if let Err(e) = write_agent(app, &path, status.minimized) {
            tracing::warn!(event = "autostart_refresh_failed", error = %e);
        }
    }
}

/// Whether this process was started with [`MINIMIZED_ARG`].
pub fn launched_minimized() -> bool {
    std::env::args().any(|a| a == MINIMIZED_ARG)
}

#[tauri::command]
pub fn autostart_get<R: Runtime>(app: AppHandle<R>) -> Result<AutostartStatus, String> {
    Ok(read_status(&plist_path(&app)?))
}

#[tauri::command]
pub fn autostart_set<R: Runtime>(
    app: AppHandle<R>,
    enabled: bool,
    minimized: bool,
) -> Result<AutostartStatus, String> {
    if !cfg!(target_os = "macos") {
        return Err("Launch at login is only supported on macOS".into());
    }
    let path = plist_path(&app)?;
    if enabled {
        write_agent(&app, &path, minimized)?;
    } else if path.exists() {
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    Ok(read_status(&path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_has_label_exe_and_run_at_load() {
        let p = render_plist(
            "vn.papay.devdy",
            Path::new("/Applications/Devdy.app/Contents/MacOS/devdy"),
            false,
        );
        assert!(p.contains("<string>vn.papay.devdy</string>"));
        assert!(p.contains("<string>/Applications/Devdy.app/Contents/MacOS/devdy</string>"));
        assert!(p.contains("<key>RunAtLoad</key>\n    <true/>"));
        assert!(!plist_is_minimized(&p));
    }

    #[test]
    fn plist_minimized_round_trips() {
        let p = render_plist("x", Path::new("/a/b"), true);
        assert!(plist_is_minimized(&p));
    }

    #[test]
    fn plist_escapes_xml_in_paths() {
        let p = render_plist("x", Path::new("/Users/a&b/<Devdy>.app"), false);
        assert!(p.contains("/Users/a&amp;b/&lt;Devdy&gt;.app"));
    }
}
