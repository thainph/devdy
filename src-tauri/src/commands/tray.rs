//! macOS menu-bar (system tray) icon with a quick window + session switcher.
//!
//! Clicking the tray icon drops a menu with two sections: every open window (jump
//! straight to a pop-out or back to the main window without hunting through
//! Mission Control) and every running session (open it where it lives — its own
//! pop-out if it has one, otherwise the main window).
//!
//! Same split as the app menu (`app_menu.rs`): the menu's *contents* are described
//! by the frontend and sent down via `set_tray_menu`, so labels follow the app
//! language and each session's human title. Rust only owns the icon, routes a
//! click to the matching window, and handles Quit. The frontend re-sends the spec
//! whenever a window opens/closes or a run is renamed (see `src/lib/tray.ts`).
//!
//! The two sections have different owners: ANY window may resend the window list,
//! but only the main window knows every running session. So each section is sent
//! independently and cached here, and the menu is rebuilt from the cached pair —
//! a pop-out refreshing the window list can't wipe the main window's sessions.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::image::Image;
use tauri::menu::{MenuBuilder, MenuItemBuilder, Submenu, SubmenuBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, Runtime};

/// Monochrome menu-bar glyph (`</>`), black + alpha so macOS can tint it as a
/// template image — it turns white on a dark menu bar, black on a light one.
const TRAY_ICON_PNG: &[u8] = include_bytes!("../../icons/tray-template.png");

/// Stable id so `set_tray_menu` can find the tray built at startup.
pub const TRAY_ID: &str = "devdy-tray";
/// Reserved menu id: quit the whole app (mirrors closing the main window).
const QUIT_ID: &str = "tray:quit";
/// Prefix for "focus this window" ids; the suffix is the window label.
///
/// Tauri delivers EVERY menu click — app menu AND tray — to one shared global
/// listener list, so both `app_menu::on_menu_event` and `on_menu_event` below run
/// for each click. The `tray:` namespace keeps them from acting on each other's
/// ids: this handler ignores anything without it, and the app-menu handler
/// early-returns on it (see `app_menu::on_menu_event`).
const WIN_PREFIX: &str = "tray:win:";
/// Prefix for "open this running session" ids; the suffix is the run id.
const RUN_PREFIX: &str = "tray:run:";
/// Event sent to the main window when a session row is clicked. The main window
/// decides where the run is shown (its pop-out, or itself) — see `src/lib/tray.ts`.
const OPEN_RUN_EVENT: &str = "tray://open-run";

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TrayItem {
    /// A clickable row. `id` is `tray:win:<label>`, `tray:run:<runId>` or `tray:quit`.
    Item { id: String, label: String },
    /// A greyed-out, non-clickable section title.
    Header { label: String },
    /// A nested menu (a conductor with its workers).
    Submenu { label: String, items: Vec<TrayItem> },
    Separator,
}

#[derive(Debug, Serialize, Clone)]
struct OpenRunPayload {
    #[serde(rename = "runId")]
    run_id: String,
}

/// Last section contents received from the frontend, merged into one menu.
pub struct TrayState {
    windows: Mutex<Vec<TrayItem>>,
    sessions: Mutex<Vec<TrayItem>>,
    quit_label: Mutex<String>,
}

impl Default for TrayState {
    fn default() -> Self {
        Self {
            windows: Mutex::default(),
            sessions: Mutex::default(),
            quit_label: Mutex::new("Quit Devdy".into()),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct TraySpec {
    /// The open-window section. Absent → keep the cached one.
    #[serde(default)]
    windows: Option<Vec<TrayItem>>,
    /// The running-session section. Only the main window sends it (pop-outs don't
    /// know every live run). Absent → keep the cached one.
    #[serde(default)]
    sessions: Option<Vec<TrayItem>>,
    /// Localized label for the Quit row. Absent → keep the cached one.
    #[serde(default, rename = "quitLabel")]
    quit_label: Option<String>,
    /// Text shown next to the menu-bar glyph (macOS) — the count of running
    /// sessions, including ones not popped out into their own window. Only the
    /// main window sends this; other windows omit it so they don't clobber the
    /// count. The double Option lets serde tell "key absent" (outer `None` →
    /// leave the title untouched) from "present but empty" (`Some(Some(""))` →
    /// clear back to icon-only).
    #[serde(default)]
    title: Option<Option<String>>,
}

fn build_submenu<R: Runtime>(
    app: &AppHandle<R>,
    label: &str,
    items: &[TrayItem],
) -> Result<Submenu<R>, String> {
    let mut sub = SubmenuBuilder::new(app, label);
    for item in items {
        sub = match item {
            TrayItem::Separator => sub.separator(),
            TrayItem::Item { id, label } => sub.item(
                &MenuItemBuilder::with_id(id.clone(), label)
                    .build(app)
                    .map_err(|e| e.to_string())?,
            ),
            TrayItem::Header { label } => sub.item(
                &MenuItemBuilder::new(label)
                    .enabled(false)
                    .build(app)
                    .map_err(|e| e.to_string())?,
            ),
            TrayItem::Submenu { label, items } => sub.item(&build_submenu(app, label, items)?),
        };
    }
    sub.build().map_err(|e| e.to_string())
}

/// Build the tray menu: windows, then sessions, then Quit — each non-empty
/// section separated from the next.
fn build_menu<R: Runtime>(
    app: &AppHandle<R>,
    windows: &[TrayItem],
    sessions: &[TrayItem],
    quit_label: &str,
) -> Result<tauri::menu::Menu<R>, String> {
    let mut menu = MenuBuilder::new(app);
    for section in [windows, sessions] {
        if section.is_empty() {
            continue;
        }
        for item in section {
            menu = match item {
                TrayItem::Separator => menu.separator(),
                TrayItem::Item { id, label } => menu.item(
                    &MenuItemBuilder::with_id(id.clone(), label)
                        .build(app)
                        .map_err(|e| e.to_string())?,
                ),
                TrayItem::Header { label } => menu.item(
                    &MenuItemBuilder::new(label)
                        .enabled(false)
                        .build(app)
                        .map_err(|e| e.to_string())?,
                ),
                TrayItem::Submenu { label, items } => {
                    menu.item(&build_submenu(app, label, items)?)
                }
            };
        }
        menu = menu.separator();
    }
    let quit = MenuItemBuilder::with_id(QUIT_ID, quit_label)
        .build(app)
        .map_err(|e| e.to_string())?;
    menu.item(&quit).build().map_err(|e| e.to_string())
}

/// Route a tray-menu click. `tray:quit` closes the main window (which runs the
/// normal sidecar-cleanup + exit path, see `lib.rs`); `tray:win:<label>` brings
/// that window to the front; `tray:run:<runId>` is handed to the main window,
/// which focuses the run's pop-out or navigates itself to the run.
fn on_menu_event<R: Runtime>(app: &AppHandle<R>, event: tauri::menu::MenuEvent) {
    let id = event.id().0.as_str();
    if id == QUIT_ID {
        if let Some(win) = app.get_webview_window("main") {
            let _ = win.close();
        } else {
            app.exit(0);
        }
        return;
    }
    if let Some(label) = id.strip_prefix(WIN_PREFIX) {
        if let Some(win) = app.get_webview_window(label) {
            let _ = win.unminimize();
            let _ = win.show();
            let _ = win.set_focus();
        }
        return;
    }
    if let Some(run_id) = id.strip_prefix(RUN_PREFIX) {
        let _ = app.emit_to(
            "main",
            OPEN_RUN_EVENT,
            OpenRunPayload {
                run_id: run_id.to_string(),
            },
        );
    }
    // Anything else is an app-menu id; app_menu::on_menu_event owns it.
}

/// Create the tray icon once, at startup. The initial menu only carries Quit; the
/// frontend fills in the window list via `set_tray_menu` as soon as it mounts.
pub fn init<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let state = TrayState::default();
    let menu = {
        let quit_label = state.quit_label.lock().map_err(|e| e.to_string())?;
        build_menu(app, &[], &[], &quit_label)?
    };
    app.manage(state);

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("Devdy")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(on_menu_event);

    // Prefer the monochrome template glyph; fall back to the colored app icon if
    // it somehow fails to decode, so the tray still appears.
    match Image::from_bytes(TRAY_ICON_PNG) {
        Ok(icon) => {
            builder = builder.icon(icon).icon_as_template(true);
        }
        Err(e) => {
            tracing::warn!(event = "tray_icon_decode_failed", error = %e);
            if let Some(icon) = app.default_window_icon() {
                builder = builder.icon(icon.clone());
            }
        }
    }

    builder.build(app).map_err(|e| e.to_string())?;
    Ok(())
}

/// Update the section(s) the frontend sent and rebuild the tray menu.
#[tauri::command]
pub fn set_tray_menu<R: Runtime>(app: AppHandle<R>, spec: TraySpec) -> Result<(), String> {
    let Some(state) = app.try_state::<TrayState>() else {
        return Ok(());
    };
    let menu = {
        let mut windows = state.windows.lock().map_err(|e| e.to_string())?;
        let mut sessions = state.sessions.lock().map_err(|e| e.to_string())?;
        let mut quit_label = state.quit_label.lock().map_err(|e| e.to_string())?;
        if let Some(q) = spec.quit_label.filter(|q| !q.is_empty()) {
            *quit_label = q;
        }
        if let Some(w) = spec.windows {
            *windows = w;
        }
        if let Some(s) = spec.sessions {
            *sessions = s;
        }
        build_menu(&app, &windows, &sessions, &quit_label)?
    };
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        tray.set_menu(Some(menu)).map_err(|e| e.to_string())?;
        // Only touch the title when the frontend actually sent one (outer Some);
        // an empty string collapses back to icon-only, a non-empty one shows the
        // running-session count beside the glyph.
        if let Some(title) = &spec.title {
            let title = title.as_deref().filter(|t| !t.is_empty());
            tray.set_title(title).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
