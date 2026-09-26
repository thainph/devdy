//! macOS menu-bar (system tray) icon with a quick window switcher.
//!
//! Clicking the tray icon drops a menu listing every open window so the user can
//! jump straight to a session pop-out (or back to the main window) without
//! hunting through Mission Control.
//!
//! Same split as the app menu (`app_menu.rs`): the menu's *contents* are described
//! by the frontend and sent down via `set_tray_menu`, so labels follow the app
//! language and each session's human title. Rust only owns the icon, routes a
//! click to the matching window, and handles Quit. The frontend re-sends the spec
//! whenever a window opens/closes or a run is renamed (see `src/lib/tray.ts`).

use serde::Deserialize;
use tauri::image::Image;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Runtime};

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

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TrayItem {
    /// A clickable row. `id` is a window label to focus, or `tray:quit`.
    Item { id: String, label: String },
    Separator,
}

#[derive(Debug, Deserialize)]
pub struct TraySpec {
    items: Vec<TrayItem>,
}

/// Build a Tauri menu from the frontend spec.
fn build_menu<R: Runtime>(
    app: &AppHandle<R>,
    spec: &TraySpec,
) -> Result<tauri::menu::Menu<R>, String> {
    let mut menu = MenuBuilder::new(app);
    for item in &spec.items {
        menu = match item {
            TrayItem::Separator => menu.separator(),
            TrayItem::Item { id, label } => {
                let built = MenuItemBuilder::with_id(id.clone(), label)
                    .build(app)
                    .map_err(|e| e.to_string())?;
                menu.item(&built)
            }
        };
    }
    menu.build().map_err(|e| e.to_string())
}

/// Route a tray-menu click. `tray:quit` closes the main window (which runs the
/// normal sidecar-cleanup + exit path, see `lib.rs`); any other id is a window
/// label to bring to the front.
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
    }
    // Anything else is an app-menu id; app_menu::on_menu_event owns it.
}

/// Create the tray icon once, at startup. The initial menu only carries Quit; the
/// frontend fills in the window list via `set_tray_menu` as soon as it mounts.
pub fn init<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let quit = MenuItemBuilder::with_id(QUIT_ID, "Quit Devdy")
        .build(app)
        .map_err(|e| e.to_string())?;
    let menu = MenuBuilder::new(app)
        .item(&quit)
        .build()
        .map_err(|e| e.to_string())?;

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

/// Replace the tray menu with the frontend-provided window list.
#[tauri::command]
pub fn set_tray_menu<R: Runtime>(app: AppHandle<R>, spec: TraySpec) -> Result<(), String> {
    let menu = build_menu(&app, &spec)?;
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        tray.set_menu(Some(menu)).map_err(|e| e.to_string())?;
    }
    Ok(())
}
