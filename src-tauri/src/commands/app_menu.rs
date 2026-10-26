//! Native application menu (File / Edit / View / Go / Window / Help).
//!
//! The menu's *contents* are described by the frontend and sent down here as a
//! spec, rather than hardcoded in Rust. Two reasons:
//!
//!  - i18n. Labels have to follow the app language, which lives in vue-i18n.
//!    The frontend re-sends the spec whenever the language changes. That covers
//!    the OS-provided items too (Copy, Quit, …): they get an explicit label, or
//!    macOS would title them in the *system* language and mix two languages in
//!    one menu.
//!  - The actions themselves are frontend concerns (route pushes, layout
//!    toggles, the quick-capture overlay). Rust only reports *which* item was
//!    clicked; `src/lib/appMenu.ts` owns what that means.
//!
//! Clicking an item emits `menu://action` to the main window with the item id
//! as payload (see `MENU_ACTION_EVENT`).
//!
//! The Window menu also lists every open window. That list is owned by the tray
//! (`tray.rs`), which any window can refresh, so the last spec is cached here and
//! the menu is rebuilt whenever the tray's window list changes.

use std::sync::Mutex;

use serde::Deserialize;
use tauri::menu::{
    IsMenuItem, MenuBuilder, MenuItemBuilder, PredefinedMenuItem, Submenu, SubmenuBuilder,
};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use super::tray::{self, TrayItem};

/// Event carrying a clicked item's id to the main window's webview.
pub const MENU_ACTION_EVENT: &str = "menu://action";

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ItemSpec {
    /// A normal, clickable item that reports back to the frontend.
    #[serde(rename_all = "camelCase")]
    Item {
        id: String,
        label: String,
        /// e.g. "CmdOrCtrl+B". The OS handles it; no webview keydown involved.
        accelerator: Option<String>,
        /// Greyed out when false (e.g. "New Session" outside a project).
        #[serde(default = "default_true")]
        enabled: bool,
    },
    /// An OS-provided item (copy/paste/quit/…). Handled entirely by the system,
    /// which is exactly why Edit must keep them: without a Copy item in the
    /// menu, macOS never delivers ⌘C to the webview at all.
    Predefined {
        role: String,
        label: Option<String>,
    },
    /// A nested submenu.
    Submenu {
        label: String,
        items: Vec<ItemSpec>,
    },
    /// Placeholder for the list of open windows (from the tray's cache).
    WindowList,
    Separator,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct SubmenuSpec {
    label: String,
    items: Vec<ItemSpec>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MenuSpec {
    submenus: Vec<SubmenuSpec>,
}

/// The last spec the frontend sent, so the menu can be rebuilt when only the
/// window list changed.
#[derive(Default)]
pub struct AppMenuState {
    spec: Mutex<Option<MenuSpec>>,
}

/// Build one OS-provided item with the given label. Unknown roles yield `None`
/// rather than failing the whole menu: a typo should cost one item, not the
/// entire menu bar. Roles a platform doesn't support (services/hide on
/// Windows, …) are no-ops there, so the frontend only sends them on macOS.
fn predefined<R: Runtime>(
    app: &AppHandle<R>,
    role: &str,
    label: Option<&str>,
) -> tauri::Result<Option<PredefinedMenuItem<R>>> {
    let item = match role {
        "undo" => PredefinedMenuItem::undo(app, label)?,
        "redo" => PredefinedMenuItem::redo(app, label)?,
        "cut" => PredefinedMenuItem::cut(app, label)?,
        "copy" => PredefinedMenuItem::copy(app, label)?,
        "paste" => PredefinedMenuItem::paste(app, label)?,
        "selectAll" => PredefinedMenuItem::select_all(app, label)?,
        "minimize" => PredefinedMenuItem::minimize(app, label)?,
        "maximize" => PredefinedMenuItem::maximize(app, label)?,
        "fullscreen" => PredefinedMenuItem::fullscreen(app, label)?,
        "closeWindow" => PredefinedMenuItem::close_window(app, label)?,
        "quit" => PredefinedMenuItem::quit(app, label)?,
        "about" => PredefinedMenuItem::about(app, label, None)?,
        "services" => PredefinedMenuItem::services(app, label)?,
        "hide" => PredefinedMenuItem::hide(app, label)?,
        "hideOthers" => PredefinedMenuItem::hide_others(app, label)?,
        "showAll" => PredefinedMenuItem::show_all(app, label)?,
        "bringAllToFront" => PredefinedMenuItem::bring_all_to_front(app, label)?,
        _ => return Ok(None),
    };
    Ok(Some(item))
}

fn build_submenu<R: Runtime>(
    app: &AppHandle<R>,
    label: &str,
    items: &[ItemSpec],
    windows: &[TrayItem],
) -> Result<Submenu<R>, String> {
    let mut submenu = SubmenuBuilder::new(app, label);
    for item in items {
        submenu = match item {
            ItemSpec::Separator => submenu.separator(),
            ItemSpec::Predefined { role, label } => {
                match predefined(app, role, label.as_deref()).map_err(|e| e.to_string())? {
                    Some(p) => submenu.item(&p),
                    None => submenu,
                }
            }
            ItemSpec::Item {
                id,
                label,
                accelerator,
                enabled,
            } => {
                let mut builder = MenuItemBuilder::with_id(id.clone(), label).enabled(*enabled);
                if let Some(acc) = accelerator {
                    builder = builder.accelerator(acc);
                }
                let built = builder.build(app).map_err(|e| e.to_string())?;
                submenu.item(&built)
            }
            ItemSpec::Submenu { label, items } => {
                submenu.item(&build_submenu(app, label, items, windows)?)
            }
            ItemSpec::WindowList => {
                // Only the clickable rows; the tray's section header is redundant
                // inside a menu already titled "Window". The ids keep their
                // `tray:win:` prefix, so `tray::on_menu_event` focuses the window.
                let rows: Vec<_> = windows
                    .iter()
                    .filter_map(|w| match w {
                        TrayItem::Item { id, label } => Some(
                            MenuItemBuilder::with_id(id.clone(), label)
                                .build(app)
                                .map_err(|e| e.to_string()),
                        ),
                        _ => None,
                    })
                    .collect::<Result<_, _>>()?;
                let refs: Vec<&dyn IsMenuItem<R>> =
                    rows.iter().map(|r| r as &dyn IsMenuItem<R>).collect();
                submenu.items(&refs)
            }
        };
    }
    submenu.build().map_err(|e| e.to_string())
}

/// Build the cached spec (if any) and install it app-wide.
pub fn rebuild<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let Some(state) = app.try_state::<AppMenuState>() else {
        return Ok(());
    };
    let spec = state.spec.lock().map_err(|e| e.to_string())?.clone();
    let Some(spec) = spec else {
        return Ok(());
    };
    let windows = tray::cached_windows(app);

    let mut menu = MenuBuilder::new(app);
    for sub in &spec.submenus {
        menu = menu.item(&build_submenu(app, &sub.label, &sub.items, &windows)?);
    }
    let menu = menu.build().map_err(|e| e.to_string())?;
    app.set_menu(menu).map_err(|e| e.to_string())?;
    Ok(())
}

/// Build the described menu and install it app-wide, replacing Tauri's default
/// (which is where the empty File/Edit/View menus came from).
#[tauri::command]
pub fn set_app_menu<R: Runtime>(app: AppHandle<R>, spec: MenuSpec) -> Result<(), String> {
    if let Some(state) = app.try_state::<AppMenuState>() {
        *state.spec.lock().map_err(|e| e.to_string())? = Some(spec);
    }
    rebuild(&app)
}

/// Forward a menu click to the main window. Registered once on the builder.
///
/// On macOS the menu bar is app-global, so an item can be clicked while a
/// pop-out (mascot / permission / gantt) holds focus — hence focusing main
/// first: a "Go → Todos" that navigated an invisible window would look broken.
pub fn on_menu_event<R: Runtime>(app: &AppHandle<R>, event: tauri::menu::MenuEvent) {
    let id = event.id().0.clone();
    // Tauri routes tray-menu clicks through this same global listener. Tray ids are
    // namespaced `tray:` and handled in `tray::on_menu_event`; ignore them here so a
    // tray "focus session" click doesn't also yank focus back to the main window.
    // (The Window menu's window rows reuse those ids on purpose.)
    if id.starts_with("tray:") {
        return;
    }
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.set_focus();
    }
    let _ = app.emit_to("main", MENU_ACTION_EVENT, id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_frontend_spec() {
        let spec: MenuSpec = serde_json::from_str(
            r#"{"submenus":[{"label":"Window","items":[
                {"kind":"item","id":"file.newSession","label":"New Session","enabled":false},
                {"kind":"item","id":"go./settings","label":"Settings…","accelerator":"CmdOrCtrl+,"},
                {"kind":"predefined","role":"copy","label":"Copy"},
                {"kind":"submenu","label":"Developer","items":[{"kind":"item","id":"help.reload","label":"Reload"}]},
                {"kind":"windowList"},
                {"kind":"separator"}
            ]}]}"#,
        )
        .expect("spec should deserialize");
        let items = &spec.submenus[0].items;
        assert!(matches!(&items[0], ItemSpec::Item { enabled: false, .. }));
        assert!(matches!(
            &items[1],
            ItemSpec::Item {
                enabled: true,
                accelerator: Some(_),
                ..
            }
        ));
        assert!(matches!(&items[3], ItemSpec::Submenu { items, .. } if items.len() == 1));
        assert!(matches!(items[4], ItemSpec::WindowList));
    }
}
