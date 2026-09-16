//! Native application menu (File / Edit / View / Go / Window / Help).
//!
//! The menu's *contents* are described by the frontend and sent down here as a
//! spec, rather than hardcoded in Rust. Two reasons:
//!
//!  - i18n. Labels have to follow the app language, which lives in vue-i18n.
//!    The frontend re-sends the spec whenever the language changes.
//!  - The actions themselves are frontend concerns (route pushes, layout
//!    toggles, the quick-capture overlay). Rust only reports *which* item was
//!    clicked; `src/lib/appMenu.ts` owns what that means.
//!
//! Clicking an item emits `menu://action` to the main window with the item id
//! as payload (see `MENU_ACTION_EVENT`).

use serde::Deserialize;
use tauri::menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder};
use tauri::{AppHandle, Emitter, Manager, Runtime};

/// Event carrying a clicked item's id to the main window's webview.
pub const MENU_ACTION_EVENT: &str = "menu://action";

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ItemSpec {
    /// A normal, clickable item that reports back to the frontend.
    #[serde(rename_all = "camelCase")]
    Item {
        id: String,
        label: String,
        /// e.g. "CmdOrCtrl+B". The OS handles it; no webview keydown involved.
        accelerator: Option<String>,
    },
    /// An OS-provided item (copy/paste/quit/…). Handled entirely by the system,
    /// which is exactly why Edit must keep them: without a Copy item in the
    /// menu, macOS never delivers ⌘C to the webview at all.
    Predefined {
        role: String,
        label: Option<String>,
    },
    Separator,
}

#[derive(Debug, Deserialize)]
pub struct SubmenuSpec {
    label: String,
    items: Vec<ItemSpec>,
}

#[derive(Debug, Deserialize)]
pub struct MenuSpec {
    submenus: Vec<SubmenuSpec>,
}

/// Apply one predefined role onto the submenu being built. Unknown roles are
/// skipped rather than failing the whole menu: the frontend only sends the
/// macOS-only roles (services/hideOthers/showAll) on macOS, and a typo should
/// cost one item, not the entire menu bar.
fn apply_role<'m, R: Runtime, M: Manager<R>>(
    builder: SubmenuBuilder<'m, R, M>,
    role: &str,
    label: Option<&str>,
) -> SubmenuBuilder<'m, R, M> {
    match role {
        "undo" => builder.undo(),
        "redo" => builder.redo(),
        "cut" => builder.cut(),
        "copy" => builder.copy(),
        "paste" => builder.paste(),
        "selectAll" => builder.select_all(),
        "minimize" => builder.minimize(),
        "maximize" => builder.maximize(),
        "fullscreen" => builder.fullscreen(),
        "closeWindow" => builder.close_window(),
        "quit" => builder.quit(),
        "about" => builder.about(None),
        #[cfg(target_os = "macos")]
        "services" => builder.services(),
        #[cfg(target_os = "macos")]
        "hide" => builder.hide(),
        #[cfg(target_os = "macos")]
        "hideOthers" => builder.hide_others(),
        #[cfg(target_os = "macos")]
        "showAll" => builder.show_all(),
        _ => {
            let _ = label;
            builder
        }
    }
}

/// Build the described menu and install it app-wide, replacing Tauri's default
/// (which is where the empty File/Edit/View menus came from).
#[tauri::command]
pub fn set_app_menu<R: Runtime>(app: AppHandle<R>, spec: MenuSpec) -> Result<(), String> {
    let mut menu = MenuBuilder::new(&app);

    for sub in &spec.submenus {
        let mut submenu = SubmenuBuilder::new(&app, &sub.label);
        for item in &sub.items {
            submenu = match item {
                ItemSpec::Separator => submenu.separator(),
                ItemSpec::Predefined { role, label } => {
                    apply_role(submenu, role, label.as_deref())
                }
                ItemSpec::Item {
                    id,
                    label,
                    accelerator,
                } => {
                    let mut builder = MenuItemBuilder::with_id(id.clone(), label);
                    if let Some(acc) = accelerator {
                        builder = builder.accelerator(acc);
                    }
                    let built = builder.build(&app).map_err(|e| e.to_string())?;
                    submenu.item(&built)
                }
            };
        }
        let built = submenu.build().map_err(|e| e.to_string())?;
        menu = menu.item(&built);
    }

    let menu = menu.build().map_err(|e| e.to_string())?;
    app.set_menu(menu).map_err(|e| e.to_string())?;
    Ok(())
}

/// Forward a menu click to the main window. Registered once on the builder.
///
/// On macOS the menu bar is app-global, so an item can be clicked while a
/// pop-out (mascot / permission / gantt) holds focus — hence focusing main
/// first: a "Go → Todos" that navigated an invisible window would look broken.
pub fn on_menu_event<R: Runtime>(app: &AppHandle<R>, event: tauri::menu::MenuEvent) {
    let id = event.id().0.clone();
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.set_focus();
    }
    let _ = app.emit_to("main", MENU_ACTION_EVENT, id);
}
