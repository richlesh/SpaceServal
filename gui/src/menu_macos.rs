//! Native macOS menu bar (built with [`muda`]).
//!
//! On macOS the menu bar lives in the system bar at the top of the screen, not
//! inside the window. Slint's declarative `MenuBar` *does* render natively via
//! muda, but in Slint 1.18 it always synthesises a fixed Application menu
//! (About / Services / Hide / Quit) and gives no way to inject our own
//! *About…* or *Settings…* items into it ("menu roles" are not implemented
//! yet). So on macOS we bypass the Slint `MenuBar` and build the whole menu
//! ourselves with muda, which lets us put *About SpaceServal…* and
//! *Settings…* in the Application menu exactly where macOS users expect them.
//!
//! The muda version is pinned in `Cargo.toml` to the same one Slint's winit
//! backend uses, so there is a single `muda` in the dependency tree and its
//! process-global `MenuEvent` channel is shared. Because our UI contains no
//! Slint `MenuBar`/`ContextMenuArea` on macOS, Slint never installs its own
//! muda event handler, leaving `MenuEvent::receiver()` free for us to poll.

use muda::{
    accelerator::{Accelerator, Code, Modifiers},
    Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
};
use slint::ComponentHandle;

use crate::app::MainWindow;

// Stable ids for our custom items so we can route activation back to the UI.
const ID_ABOUT: &str = "app.about";
const ID_SETTINGS: &str = "app.settings";
const ID_LICENSE: &str = "help.license";
// Edit items. macOS predefined Cut/Copy/Paste use the AppKit responder chain,
// which never reaches Slint's `TextInput` (it isn't an NSText responder), so
// paste etc. silently do nothing in our dialog fields. Use custom items that
// dispatch to the window's edit callbacks (the same broadcast the Windows/Linux
// menu uses) and keep the standard Cmd-X/C/V/A accelerators.
const ID_CUT: &str = "edit.cut";
const ID_COPY: &str = "edit.copy";
const ID_PASTE: &str = "edit.paste";
const ID_SELECT_ALL: &str = "edit.select_all";

/// Build the native macOS menu, install it as the application menu, and start
/// polling for menu events (routing them to the window's callbacks).
///
/// The returned [`Menu`] must be kept alive for the lifetime of the app, so the
/// caller should hold on to it (dropping it would tear the menu down).
pub fn install(window: &MainWindow) -> Menu {
    let menu = Menu::new();

    // ---- Application menu (the bold, app-named menu) --------------------
    // Order follows the macOS HIG: About, Preferences/Settings, then the
    // standard Services/Hide/Quit block built from predefined items.
    let app_menu = Submenu::new("SpaceServal", true);
    let about_item = MenuItem::with_id(ID_ABOUT, "About SpaceServal", true, None);
    let settings_item = MenuItem::with_id(
        ID_SETTINGS,
        "Settings…",
        true,
        Some(Accelerator::new(Some(Modifiers::META), Code::Comma)),
    );
    menu.append(&app_menu).unwrap();
    app_menu
        .append_items(&[
            &about_item,
            &PredefinedMenuItem::separator(),
            &settings_item,
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::services(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::hide(None),
            &PredefinedMenuItem::hide_others(None),
            &PredefinedMenuItem::show_all(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::quit(None),
        ])
        .unwrap();

    // ---- File -----------------------------------------------------------
    // No document model yet; a Close Window keeps the menu non-empty and
    // behaves natively (Cmd-W).
    let file_menu = Submenu::new("File", true);
    menu.append(&file_menu).unwrap();
    file_menu
        .append_items(&[&PredefinedMenuItem::close_window(None)])
        .unwrap();

    // ---- Edit -----------------------------------------------------------
    // Custom items (with the usual accelerators) that route to the window so
    // the ops reach our Slint TextInputs. Undo/Redo stay predefined.
    let edit_menu = Submenu::new("Edit", true);
    let cut_item = MenuItem::with_id(
        ID_CUT,
        "Cut",
        true,
        Some(Accelerator::new(Some(Modifiers::META), Code::KeyX)),
    );
    let copy_item = MenuItem::with_id(
        ID_COPY,
        "Copy",
        true,
        Some(Accelerator::new(Some(Modifiers::META), Code::KeyC)),
    );
    let paste_item = MenuItem::with_id(
        ID_PASTE,
        "Paste",
        true,
        Some(Accelerator::new(Some(Modifiers::META), Code::KeyV)),
    );
    let select_all_item = MenuItem::with_id(
        ID_SELECT_ALL,
        "Select All",
        true,
        Some(Accelerator::new(Some(Modifiers::META), Code::KeyA)),
    );
    menu.append(&edit_menu).unwrap();
    edit_menu
        .append_items(&[
            &PredefinedMenuItem::undo(None),
            &PredefinedMenuItem::redo(None),
            &PredefinedMenuItem::separator(),
            &cut_item,
            &copy_item,
            &paste_item,
            &select_all_item,
        ])
        .unwrap();

    // ---- Help -----------------------------------------------------------
    let help_menu = Submenu::new("Help", true);
    let license_item = MenuItem::with_id(ID_LICENSE, "License Key…", true, None);
    menu.append(&help_menu).unwrap();
    help_menu.append_items(&[&license_item]).unwrap();

    // Install into the shared NSApp. Must run on the main thread (we are).
    menu.init_for_nsapp();

    install_event_handler(window);

    menu
}

/// Receive muda menu events and dispatch them to the window on the UI thread.
///
/// We register our own handler via [`MenuEvent::set_event_handler`] (the
/// pattern muda recommends for event-loop integrations). The handler runs on
/// the main thread; we hop onto the Slint event loop with
/// [`slint::invoke_from_event_loop`] before touching the window. Because we
/// disable Slint's default menu bar (see `app::run`), Slint never installs its
/// own handler, so ours is the one muda uses.
fn install_event_handler(window: &MainWindow) {
    let weak = window.as_weak();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let weak = weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(window) = weak.upgrade() {
                dispatch(&window, &event.id);
            }
        });
    }));
}

/// Route a menu id to the corresponding window callback. Unknown ids (e.g. the
/// predefined Quit/Edit items, which macOS handles itself) are ignored.
fn dispatch(window: &MainWindow, id: &MenuId) {
    match id.as_ref() {
        ID_ABOUT => window.invoke_open_about(),
        ID_SETTINGS => window.invoke_open_settings(),
        ID_LICENSE => window.invoke_open_license(),
        ID_CUT => window.invoke_edit_cut(),
        ID_COPY => window.invoke_edit_copy(),
        ID_PASTE => window.invoke_edit_paste(),
        ID_SELECT_ALL => window.invoke_edit_select_all(),
        _ => {}
    }
}
