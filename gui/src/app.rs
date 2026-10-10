//! Slint controller for SpaceServal.
//!
//! Builds the main window and wires the shared Glowing Cat dialogs — About,
//! Splash (donation nag), License Key, and Settings — to the persisted
//! [`Settings`](crate::license_mgr::Settings). The main-window application
//! logic is intentionally omitted; this is the surrounding chrome only.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use slint::{Model, ModelRc, SharedPixelBuffer, SharedString, VecModel};

use crate::license_mgr::Settings;

slint::include_modules!();

/// Minimal controller state: just the persisted settings and the UI-open
/// snapshots used to revert live previews on Cancel.
struct Controller {
    settings: Settings,
    /// Theme (dark?) captured when the Settings dialog opened, so Cancel can
    /// revert a live preview.
    theme_before_settings: bool,
    /// Font-size index captured when the Settings dialog opened.
    font_index_before_settings: i32,
    /// Open disk-usage explorer windows, kept alive while shown. Each entry is
    /// a `VolumeWindow` and its navigation state.
    volume_windows: Vec<VolumeHandle>,
}

/// A live volume-explorer window plus the state its callbacks mutate.
struct VolumeHandle {
    /// Held so the window stays open; its callbacks own what they need.
    #[allow(dead_code)]
    window: VolumeWindow,
}

/// Navigation state for one volume-explorer window.
struct VolState {
    /// The volume root; the Up button is disabled when `current == root`.
    root: std::path::PathBuf,
    /// Directory currently displayed.
    current: std::path::PathBuf,
    /// Per-row selection flags, parallel to the window's `wedges` model.
    selected: Vec<bool>,
    /// Anchor row for Shift-range selection (the last plainly-clicked row).
    anchor: Option<usize>,
}

pub fn run() -> Result<(), slint::PlatformError> {
    // macOS: install our own winit backend with Slint's default menu bar
    // disabled. Otherwise Slint installs a default NSApp menu when the window
    // activates and hijacks muda's global menu-event handler, so clicks on our
    // native menu never reach us. Must happen before any window is created.
    #[cfg(target_os = "macos")]
    {
        let backend = i_slint_backend_winit::Backend::builder()
            .with_default_menu_bar(false)
            .build()?;
        slint::platform::set_platform(Box::new(backend))
            .expect("the Slint platform/backend must not be initialized yet");
    }

    let window = MainWindow::new()?;

    let settings = Settings::load();

    let ctrl = Rc::new(RefCell::new(Controller {
        settings,
        theme_before_settings: true,
        font_index_before_settings: 0,
        volume_windows: Vec::new(),
    }));

    // App version + icon for the window and dialogs.
    window.set_app_version(SharedString::from(env!("CARGO_PKG_VERSION")));

    // Windows/Linux use the Slint declarative menu bar; macOS uses a native
    // muda menu installed below, so the Slint one stays disabled there.
    #[cfg(not(target_os = "macos"))]
    window.set_use_slint_menubar(true);

    if let Some(icon) = load_app_icon() {
        window.set_app_icon(icon.clone());
        window.set_window_icon(icon);
    }

    // License state drives the About "thank you" and the startup splash.
    let licensed = ctrl.borrow().settings.is_licensed();
    window.set_about_licensed(licensed);

    // Theme: apply the persisted Light/Dark setting to the window.
    let dark = ctrl.borrow().settings.theme_is_dark();
    window.set_theme_dark(dark);
    window.set_settings_theme_index(if dark { 1 } else { 0 });

    // Font size: apply the persisted setting (System/Small/Medium/Large).
    {
        let idx = ctrl.borrow().settings.font_size_index();
        window.set_settings_font_index(idx);
        apply_font_index(&window, idx);
    }

    // Match the app's highlight/accent colors to the user's OS setting. Falls
    // back to Slint's Palette defaults when unavailable.
    apply_system_colors(&window);

    wire_callbacks(&window, &ctrl);

    // Populate the mounted drives / mount points list shown in the main window.
    refresh_mounts(&window, ctrl.borrow().settings.show_network_shares);
    if let Ok(dir) = std::env::var("SS_VERIFY_VOLUME") {
        open_volume_window(&ctrl, &window, &dir);
    }

    // macOS: install the native menu bar (File/Edit/Help plus About and
    // Settings in the Application menu, License Key under Help). The returned
    // Menu must outlive the event loop, so keep it bound until `run()` returns.
    #[cfg(target_os = "macos")]
    let _macos_menu = crate::menu_macos::install(&window);

    // Startup splash for unlicensed users (auto-dismisses after 20s via a
    // one-shot timer; also dismissable by clicking the scrim/OK).
    let splash_timer = slint::Timer::default();
    if !licensed {
        window.set_show_splash(true);
        let handle = window.as_weak();
        splash_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_secs(20),
            move || {
                if let Some(w) = handle.upgrade() {
                    w.set_show_splash(false);
                }
            },
        );
    }

    window.run()
}

/// Register every UI callback against the controller.
fn wire_callbacks(window: &MainWindow, ctrl: &Rc<RefCell<Controller>>) {
    window.on_open_settings({
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                open_settings_dialog(&ctrl, &w);
            }
        }
    });
    window.on_quit(|| {
        let _ = slint::quit_event_loop();
    });
    window.on_refresh_mounts({
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                refresh_mounts(&w, ctrl.borrow().settings.show_network_shares);
            }
        }
    });
    window.on_open_volume({
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        move |mount_point| {
            if let Some(w) = handle.upgrade() {
                open_volume_window(&ctrl, &w, mount_point.as_str());
            }
        }
    });
    window.on_open_license({
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                open_license_dialog(&ctrl, &w);
            }
        }
    });
    window.on_open_about({
        let handle = window.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                w.set_show_about(true);
            }
        }
    });

    // --- Dialog callbacks ---
    window.on_open_url(|url| {
        let _ = webbrowser_open(&url);
    });
    window.on_about_ok({
        let handle = window.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                w.set_show_about(false);
            }
        }
    });
    window.on_splash_dismiss({
        let handle = window.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                w.set_show_splash(false);
            }
        }
    });
    window.on_license_cancel({
        let handle = window.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                w.set_show_license(false);
            }
        }
    });
    window.on_license_key_edited({
        let handle = window.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                // Reformat the key and recompute validity live.
                let formatted = crate::license_mgr::format_key(&w.get_license_key());
                if formatted != w.get_license_key().as_str() {
                    w.set_license_key(SharedString::from(formatted.clone()));
                    // Setting `text` doesn't move the caret, so nudge the field
                    // to put it back at the end after the reformat.
                    w.set_license_key_cursor_serial(w.get_license_key_cursor_serial() + 1);
                }
                let valid = crate::license_mgr::is_valid(&formatted, &w.get_license_email());
                w.set_license_valid(valid);
            }
        }
    });
    window.on_license_save({
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                license_save(&ctrl, &w);
            }
        }
    });
    window.on_settings_cancel({
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                // Revert any live theme preview to what it was on open.
                let prev = ctrl.borrow().theme_before_settings;
                w.set_theme_dark(prev);
                w.set_settings_theme_index(if prev { 1 } else { 0 });
                // Revert any live font-size preview too.
                let font_prev = ctrl.borrow().font_index_before_settings;
                w.set_settings_font_index(font_prev);
                apply_font_index(&w, font_prev);
                w.set_show_settings(false);
            }
        }
    });
    window.on_settings_save({
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                settings_save(&ctrl, &w);
            }
        }
    });
    window.on_theme_changed({
        let handle = window.as_weak();
        move |idx| {
            if let Some(w) = handle.upgrade() {
                // 0 = Light, 1 = Dark. Apply immediately for a live preview.
                w.set_theme_dark(idx != 0);
            }
        }
    });
    window.on_font_size_changed({
        let handle = window.as_weak();
        move |idx| {
            if let Some(w) = handle.upgrade() {
                // Apply immediately for a live preview (persisted on Save).
                apply_font_index(&w, idx);
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Theme / fonts / system colors
// ---------------------------------------------------------------------------

/// Read the OS accent/selection colors and push them into the Theme global so
/// the whole UI matches the user's chosen highlight color.
fn apply_system_colors(w: &MainWindow) {
    if let Some(c) = crate::sys_colors::system_colors() {
        let theme = w.global::<Theme>();
        let col = |c: crate::sys_colors::Rgb| {
            slint::Brush::SolidColor(slint::Color::from_rgb_u8(c.r, c.g, c.b))
        };
        theme.set_sys_highlight(col(c.highlight));
        theme.set_sys_highlight_text(col(c.highlight_text));
        theme.set_sys_accent(col(c.accent));
        theme.set_has_sys_colors(true);
    }
}

/// Compute the pixel size for a Font Size combo index (0=System, 1=Small,
/// 2=Medium, 3=Large) and apply it to the window's global base font size.
fn apply_font_index(w: &MainWindow, index: i32) {
    let system_px = crate::sys_colors::system_font_size();
    let name = Settings::font_size_from_index(index);
    let px = Settings {
        font_size: name,
        ..Default::default()
    }
    .font_size_px(system_px);
    // Slint `length` properties are generated as `f32` (logical pixels).
    w.set_base_font_size(px);
}

// ---------------------------------------------------------------------------
// License dialog
// ---------------------------------------------------------------------------

/// Open the License dialog, prefilling the saved email + key.
fn open_license_dialog(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) {
    let (email, key) = {
        let c = ctrl.borrow();
        (
            c.settings.license_email.clone(),
            crate::license_mgr::format_key(&c.settings.license_key),
        )
    };
    let valid = crate::license_mgr::is_valid(&key, &email);
    w.set_license_email(SharedString::from(email));
    w.set_license_key(SharedString::from(key));
    w.set_license_valid(valid);
    w.set_license_msg(SharedString::from(""));
    w.set_show_license(true);
}

/// Save the license entered in the dialog (validated live in the UI).
fn license_save(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) {
    let email = w.get_license_email().trim().to_string();
    let key = crate::license_mgr::normalize_key(&w.get_license_key());
    let result = {
        let mut c = ctrl.borrow_mut();
        c.settings.license_email = email;
        c.settings.license_key = key;
        c.settings.save()
    };
    match result {
        Ok(()) => {
            w.set_show_license(false);
            w.set_status(SharedString::from("License saved. Thank you!"));
            // A valid license suppresses the splash and updates About.
            let licensed = ctrl.borrow().settings.is_licensed();
            w.set_about_licensed(licensed);
            if licensed {
                w.set_show_splash(false);
            }
        }
        Err(e) => {
            w.set_license_msg(SharedString::from(format!("Couldn't save settings: {e}")));
        }
    }
}

// ---------------------------------------------------------------------------
// Settings dialog
// ---------------------------------------------------------------------------

/// Open the Settings dialog, prefilling the saved theme + font size.
fn open_settings_dialog(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) {
    let dark = ctrl.borrow().settings.theme_is_dark();
    // Reflect the current theme in the selector; remember it so Cancel can
    // revert a live preview.
    w.set_settings_theme_index(if dark { 1 } else { 0 });
    w.set_theme_dark(dark);
    ctrl.borrow_mut().theme_before_settings = dark;
    // Reflect + remember the current Font Size so Cancel can revert a preview.
    let font_idx = ctrl.borrow().settings.font_size_index();
    w.set_settings_font_index(font_idx);
    apply_font_index(w, font_idx);
    ctrl.borrow_mut().font_index_before_settings = font_idx;
    w.set_settings_show_network(ctrl.borrow().settings.show_network_shares);
    w.set_show_settings(true);
}

/// Save the theme + font size + network-shares toggle from the Settings dialog.
fn settings_save(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) {
    let dark = w.get_settings_theme_index() != 0;
    let font_idx = w.get_settings_font_index();
    let show_network = w.get_settings_show_network();
    let (result, network_changed) = {
        let mut c = ctrl.borrow_mut();
        c.settings.theme = if dark { "Dark".into() } else { "Light".into() };
        c.settings.font_size = Settings::font_size_from_index(font_idx);
        let network_changed = c.settings.show_network_shares != show_network;
        c.settings.show_network_shares = show_network;
        (c.settings.save(), network_changed)
    };
    // Apply the chosen size (in case Save is clicked without a prior preview).
    apply_font_index(w, font_idx);
    // Show/hide network shares in the main list right away.
    if network_changed {
        refresh_mounts(w, show_network);
    }
    match result {
        Ok(()) => w.set_status(SharedString::from("Settings saved.")),
        Err(e) => w.set_status(SharedString::from(format!("Failed to save settings: {e}"))),
    }
    w.set_show_settings(false);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Load the embedded 256px app icon as a Slint image.
fn load_app_icon() -> Option<slint::Image> {
    let bytes = include_bytes!("resources/app_icon_256.png");
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    Some(rgba_to_image(w, h, &img.into_raw()))
}

/// Open a URL in the user's default browser.
fn webbrowser_open(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map(|_| ())
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map(|_| ())
    }
}

/// Open a file or directory with the OS default handler (the file's default
/// application, or the system file manager for a directory).
fn os_open_path(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(path)
            .spawn()
            .map(|_| ())
    }
    #[cfg(target_os = "windows")]
    {
        // `explorer <path>` opens files with their default app and directories
        // in a new Explorer window.
        std::process::Command::new("explorer")
            .arg(path)
            .spawn()
            .map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(path)
            .spawn()
            .map(|_| ())
    }
}

/// Build a Slint image from raw RGBA8 bytes.
fn rgba_to_image(width: u32, height: u32, rgba: &[u8]) -> slint::Image {
    let mut buf = SharedPixelBuffer::<slint::Rgba8Pixel>::new(width, height);
    buf.make_mut_bytes().copy_from_slice(rgba);
    slint::Image::from_rgba8(buf)
}

// ---------------------------------------------------------------------------
// Mounted drives / mount points
// ---------------------------------------------------------------------------

/// Re-enumerate mounts and push them into the window's `mounts` model,
/// including network shares only when `show_network` is set.
fn refresh_mounts(w: &MainWindow, show_network: bool) {
    let rows: Vec<MountInfo> = crate::mounts::list(show_network)
        .into_iter()
        .map(|m| MountInfo {
            name: SharedString::from(m.name.clone()),
            mount_point: SharedString::from(m.mount_point.clone()),
            file_system: SharedString::from(m.file_system.clone()),
            total: SharedString::from(crate::mounts::format_bytes(m.total_bytes)),
            available: SharedString::from(crate::mounts::format_bytes(m.available_bytes)),
            used_fraction: m.used_fraction(),
            removable: m.removable,
            network: m.network,
        })
        .collect();
    w.set_mounts(ModelRc::new(VecModel::from(rows)));
}

// ---------------------------------------------------------------------------
// Per-volume disk-usage explorer window
// ---------------------------------------------------------------------------

use std::collections::HashMap;

thread_local! {
    /// Completed directory scans, keyed by absolute path, so navigating Up and
    /// back down (or revisiting via the pie) redisplays instantly instead of
    /// re-walking the disk. UI-thread only: every access happens on the Slint
    /// event loop, so a `thread_local` needs no locking.
    static SCAN_CACHE: RefCell<HashMap<std::path::PathBuf, crate::scan::DirScan>> =
        RefCell::new(HashMap::new());
}

/// Soft cap on cached directories; beyond this the cache is cleared wholesale
/// (simple and good enough — the common case is a handful of visited dirs).
const SCAN_CACHE_MAX: usize = 512;

/// Return a cached scan for `path`, if one was stored.
fn scan_cache_get(path: &std::path::Path) -> Option<crate::scan::DirScan> {
    SCAN_CACHE.with(|c| c.borrow().get(path).cloned())
}

/// Store a completed scan for `path`, clearing the cache first if it grew past
/// the cap.
fn scan_cache_put(path: std::path::PathBuf, scan: crate::scan::DirScan) {
    SCAN_CACHE.with(|c| {
        let mut map = c.borrow_mut();
        if map.len() >= SCAN_CACHE_MAX {
            map.clear();
        }
        map.insert(path, scan);
    });
}

/// Drop every cached scan at or below `root`. Used to force a fresh scan (e.g.
/// when a volume window is (re)opened) so stale sizes aren't shown.
fn scan_cache_invalidate_under(root: &std::path::Path) {
    SCAN_CACHE.with(|c| c.borrow_mut().retain(|p, _| !p.starts_with(root)));
}

/// Open a new disk-usage explorer window rooted at `mount_point`, perform the
/// initial scan, wire its navigation callbacks, and keep it alive.
fn open_volume_window(ctrl: &Rc<RefCell<Controller>>, main: &MainWindow, mount_point: &str) {
    let Ok(win) = VolumeWindow::new() else {
        return;
    };
    // Inherit the main window's theme + font size + icon.
    win.set_theme_dark(main.get_theme_dark());
    win.set_base_font_size(main.get_base_font_size());
    win.set_window_icon(main.get_window_icon());
    // Inherit the OS highlight color (for the action buttons' pressed state).
    // Only when the real OS colors were resolved; otherwise the window keeps
    // its built-in selection-color fallback.
    {
        let theme = main.global::<Theme>();
        if theme.get_has_sys_colors() {
            win.set_highlight_color(theme.get_sys_highlight());
        }
    }
    // Windows/Linux use the Slint declarative menu bar; macOS gets a native
    // menu installed below.
    #[cfg(not(target_os = "macos"))]
    win.set_use_slint_menubar(true);

    let root = std::path::PathBuf::from(mount_point);
    let state = Rc::new(RefCell::new(VolState {
        root: root.clone(),
        current: root.clone(),
        selected: Vec::new(),
        anchor: None,
    }));

    // Drill into the entry at `index` in the current wedge model (legend row or
    // mapped pie click), if it is a directory.
    let drill_to = {
        let state = state.clone();
        move |w: &VolumeWindow, index: usize| {
            let wedges = w.get_wedges();
            let Some(entry) = wedges.row_data(index) else {
                return;
            };
            if !entry.is_dir {
                return;
            }
            state.borrow_mut().current = std::path::PathBuf::from(entry.path.as_str());
            rescan_volume(w, &state);
        }
    };
    let drill_to = Rc::new(drill_to);

    win.on_drill_in({
        let drill_to = drill_to.clone();
        let handle = win.as_weak();
        move |index| {
            if let Some(w) = handle.upgrade() {
                if index >= 0 {
                    drill_to(&w, index as usize);
                }
            }
        }
    });

    // Pie click: find the wedge whose span contains `deg`, then drill into it.
    win.on_drill_at_angle({
        let drill_to = drill_to.clone();
        let handle = win.as_weak();
        move |deg| {
            if let Some(w) = handle.upgrade() {
                let wedges = w.get_wedges();
                let hit = (0..wedges.row_count()).find(|&i| {
                    let e = wedges.row_data(i).unwrap();
                    deg >= e.start_deg && deg < e.start_deg + e.sweep_deg
                });
                if let Some(i) = hit {
                    drill_to(&w, i);
                }
            }
        }
    });

    // Pie hover: set `hovered-name` to the basename of the wedge under the
    // pointer so the window can show a tooltip.
    win.on_hover_at_angle({
        let handle = win.as_weak();
        move |deg| {
            if let Some(w) = handle.upgrade() {
                let wedges = w.get_wedges();
                let name = (0..wedges.row_count())
                    .find_map(|i| {
                        let e = wedges.row_data(i).unwrap();
                        (deg >= e.start_deg && deg < e.start_deg + e.sweep_deg).then_some(e.name)
                    })
                    .unwrap_or_default();
                w.set_hovered_name(name);
            }
        }
    });
    win.on_hover_clear({
        let handle = win.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                w.set_hovered_name(SharedString::new());
            }
        }
    });

    // Go up one level, but never above the volume root.
    win.on_go_up({
        let state = state.clone();
        let handle = win.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                let next = {
                    let s = state.borrow();
                    parent_within_root(&s.current, &s.root)
                };
                if let Some(p) = next {
                    state.borrow_mut().current = p;
                    rescan_volume(&w, &state);
                }
            }
        }
    });

    // Refresh: drop the cached scan for the current directory (and everything
    // cached beneath it, so re-drilling also re-reads) and scan it fresh from
    // disk. `rescan_volume` would otherwise hit the cache and redisplay the
    // stale result instantly.
    win.on_refresh({
        let state = state.clone();
        let handle = win.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                {
                    let s = state.borrow();
                    scan_cache_invalidate_under(&s.current);
                }
                rescan_volume(&w, &state);
            }
        }
    });

    // Selection: plain click selects one row; Shift extends a contiguous range
    // from the anchor; Ctrl/Cmd toggles the row (discontiguous). The resulting
    // mask is pushed back into the window.
    win.on_select({
        let state = state.clone();
        let handle = win.as_weak();
        move |index, shift, toggle| {
            if index < 0 {
                return;
            }
            let index = index as usize;
            if let Some(w) = handle.upgrade() {
                let count = w.get_wedges().row_count();
                let mut s = state.borrow_mut();
                if s.selected.len() != count {
                    s.selected = vec![false; count];
                }
                if index >= count {
                    return;
                }
                let anchor = s.anchor;
                let new_anchor = apply_selection(&mut s.selected, anchor, index, shift, toggle);
                s.anchor = new_anchor;
                push_selection(&w, &s.selected);
                drop(s);
                // Bring the just-selected row into view if it's off-screen.
                w.set_scroll_to_index(index as i32);
                w.set_scroll_generation(w.get_scroll_generation() + 1);
            }
        }
    });

    // Selection from the pie: map the clicked angle to a wedge and apply the
    // same selection logic as `on_select`. Wedges with an empty path (the
    // synthetic "Other" grouping) are not selectable.
    win.on_select_at_angle({
        let state = state.clone();
        let handle = win.as_weak();
        move |deg, shift, toggle| {
            if let Some(w) = handle.upgrade() {
                let Some(index) = wedge_at_angle(&w, deg) else {
                    return;
                };
                let count = w.get_wedges().row_count();
                let mut s = state.borrow_mut();
                if s.selected.len() != count {
                    s.selected = vec![false; count];
                }
                let anchor = s.anchor;
                let new_anchor = apply_selection(&mut s.selected, anchor, index, shift, toggle);
                s.anchor = new_anchor;
                push_selection(&w, &s.selected);
            }
        }
    });

    // Right-click on the pie: select the wedge if it isn't already selected,
    // so the context-menu action operates on it (mirrors `ensure-selected`).
    win.on_ensure_selected_at_angle({
        let state = state.clone();
        let handle = win.as_weak();
        move |deg| {
            if let Some(w) = handle.upgrade() {
                let Some(index) = wedge_at_angle(&w, deg) else {
                    return;
                };
                let mut s = state.borrow_mut();
                let count = w.get_wedges().row_count();
                if s.selected.len() != count {
                    s.selected = vec![false; count];
                }
                if s.selected.get(index).copied().unwrap_or(false) {
                    return; // already selected
                }
                let anchor = s.anchor;
                let new_anchor = apply_selection(&mut s.selected, anchor, index, false, false);
                s.anchor = new_anchor;
                push_selection(&w, &s.selected);
            }
        }
    });

    // "What is this File": populate and show the explanation dialog for every
    // selected entry.
    win.on_explain_selection({
        let state = state.clone();
        let handle = win.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                // Dismiss the confirmation dialog ourselves so the single
                // click that triggered this does the whole job (dismiss +
                // delete). Toggling the Slint-side flag inside the button's
                // own handler tore the dialog down mid-click and could drop
                // the first activation.
                w.set_show_confirm_delete(false);
                let paths = selected_paths(&w, &state.borrow().selected);
                if paths.is_empty() {
                    return;
                }
                let rows: Vec<ExplainEntry> = paths
                    .iter()
                    .map(|(p, is_dir)| {
                        let e = crate::file_info::describe(p, *is_dir);
                        ExplainEntry {
                            name: SharedString::from(e.name),
                            format: SharedString::from(e.format),
                            purpose: SharedString::from(e.purpose),
                            safe_to_delete: SharedString::from(e.safe_to_delete),
                            online_url: SharedString::from(e.online_url),
                        }
                    })
                    .collect();
                w.set_explain_entries(ModelRc::new(VecModel::from(rows)));
                w.set_show_explain(true);
            }
        }
    });

    // "Delete Immediately…": open the confirmation ("chicken") dialog listing
    // what will be permanently removed. The actual delete happens in
    // `on_confirm_delete`.
    win.on_delete_selection({
        let state = state.clone();
        let handle = win.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                let paths = selected_paths(&w, &state.borrow().selected);
                if paths.is_empty() {
                    return;
                }
                let names = paths
                    .iter()
                    .map(|(p, is_dir)| {
                        let n = p
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        if *is_dir {
                            format!("• {n}/  (folder — includes its contents)")
                        } else {
                            format!("• {n}")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                w.set_confirm_names(SharedString::from(names));
                w.set_show_confirm_delete(true);
            }
        }
    });

    // Confirmed: permanently delete each selected entry (no trash, no undo),
    // then invalidate the cache and re-scan so the chart updates. Finally show
    // a results dialog listing what was removed and what could not be (e.g.
    // permission errors).
    win.on_confirm_delete({
        let state = state.clone();
        let handle = win.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                // Dismiss the confirmation dialog here (not in the button's own
                // Slint handler): tearing the dialog subtree down mid-click
                // dropped the first activation and needed a second click.
                w.set_show_confirm_delete(false);

                let paths = selected_paths(&w, &state.borrow().selected);

                // Collect outcomes so we can report them to the user.
                let mut deleted: Vec<String> = Vec::new();
                let mut failed: Vec<String> = Vec::new();
                for (path, is_dir) in &paths {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.to_string_lossy().into_owned());
                    let label = if *is_dir { format!("{name}/") } else { name };
                    let result = if *is_dir {
                        std::fs::remove_dir_all(path)
                    } else {
                        std::fs::remove_file(path)
                    };
                    match result {
                        Ok(()) => deleted.push(format!("• {label}")),
                        Err(e) => {
                            tracing::warn!("delete failed for {}: {e}", path.display());
                            failed.push(format!("• {label}  —  {e}"));
                        }
                    }
                }

                // Populate and show the results dialog.
                w.set_deleted_names(SharedString::from(deleted.join("\n")));
                w.set_failed_names(SharedString::from(failed.join("\n")));
                w.set_show_delete_results(true);

                // Clear the selection and refresh from disk.
                {
                    let mut s = state.borrow_mut();
                    s.selected.clear();
                    s.anchor = None;
                    let current = s.current.clone();
                    scan_cache_invalidate_under(&current);
                }
                rescan_volume(&w, &state);
            }
        }
    });

    // Open a reference URL (the explain dialog's "Look up online" link).
    win.on_open_url(|url| {
        let _ = webbrowser_open(&url);
    });

    // "Open": hand each selected file/dir to the OS to open with its default
    // application / file manager.
    win.on_open_selection({
        let state = state.clone();
        let handle = win.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                for (path, _is_dir) in selected_paths(&w, &state.borrow().selected) {
                    let _ = os_open_path(&path);
                }
            }
        }
    });

    // The volume label: the mount's last path component, or the whole path for
    // the root "/".
    let label = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| mount_point.to_string());
    win.set_volume_name(SharedString::from(label));

    // Start each explorer session fresh: drop any scans cached under this root
    // from a previous session so on-disk changes are reflected.
    scan_cache_invalidate_under(&root);

    rescan_volume(&win, &state);

    // macOS: wire the native (muda) right-click menu. The Slint
    // `ContextMenuArea` in volume.slint is used on Windows/Linux only; on
    // macOS its muda events would be swallowed by our app menu's global muda
    // handler, so we show the menu ourselves instead (see `menu_macos`).
    #[cfg(target_os = "macos")]
    crate::menu_macos::attach_volume_window(&win);

    let _ = win.show();

    ctrl.borrow_mut()
        .volume_windows
        .push(VolumeHandle { window: win });
}

/// Scan `state.current`, recompute wedges, and push them (plus the path and
/// Up-button state) into the window model. The scan runs on a worker thread so
/// a large volume never freezes the UI; results are marshaled back to the event
/// loop. A generation counter guards against a stale scan landing after the
/// user has already navigated elsewhere.
fn rescan_volume(win: &VolumeWindow, state: &Rc<RefCell<VolState>>) {
    let (current, root) = {
        let s = state.borrow();
        (s.current.clone(), s.root.clone())
    };

    // Bump the generation so a slower earlier scan can't overwrite this one.
    let generation = win.get_scan_generation().wrapping_add(1);
    win.set_scan_generation(generation);

    // A new directory's rows don't correspond to the old selection; clear it.
    {
        let mut s = state.borrow_mut();
        s.selected.clear();
        s.anchor = None;
    }
    push_selection(win, &[]);

    // Update the path + Up button and show a "scanning" state immediately.
    win.set_current_path(SharedString::from(current.to_string_lossy().into_owned()));
    win.set_can_go_up(current != root);

    // Cache hit: this directory was fully scanned already (e.g. the user went
    // Up and back down, or revisited via the pie). Render instantly without
    // touching the disk again.
    if let Some(scan) = scan_cache_get(&current) {
        win.set_wedges(wedges_to_model(&crate::scan::wedges(&scan)));
        win.set_scanning(false);
        return;
    }

    win.set_scanning(true);
    // Clear stale wedges so the pie doesn't show the previous directory.
    win.set_wedges(ModelRc::new(VecModel::<WedgeInfo>::from(Vec::new())));

    let weak = win.as_weak();
    std::thread::spawn(move || {
        // Coalesce rapid progress updates: push at most every ~120ms while the
        // scan runs, then always push a final snapshot. This keeps a volume
        // with many directories from flooding the event loop.
        let mut last_push = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(1))
            .unwrap_or_else(std::time::Instant::now);
        let mut push = |scan: &crate::scan::DirScan, final_update: bool| {
            if !final_update && last_push.elapsed() < std::time::Duration::from_millis(120) {
                return;
            }
            last_push = std::time::Instant::now();
            let wedges = crate::scan::wedges(scan);
            let weak = weak.clone();
            // On the final update, carry the completed scan so the UI thread can
            // cache it for instant redisplay on revisit.
            let to_cache = final_update.then(|| (current.clone(), scan.clone()));
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(win) = weak.upgrade() {
                    // Ignore if the user navigated away while we were scanning.
                    if win.get_scan_generation() != generation {
                        return;
                    }
                    win.set_wedges(wedges_to_model(&wedges));
                    if final_update {
                        win.set_scanning(false);
                        // Runs on the UI thread, so the thread-local cache is
                        // safe to touch here.
                        if let Some((path, scan)) = to_cache {
                            scan_cache_put(path, scan);
                        }
                    }
                }
            });
        };

        // Keep the most recent snapshot so the final flush doesn't rescan.
        let mut latest = crate::scan::DirScan::default();
        crate::scan::scan_dir_streaming(&current, &mut |snapshot| {
            latest = snapshot.clone();
            push(snapshot, false);
        });
        // Final flush: guarantees the last update (and scanning=false) lands
        // even when the throttle skipped the final progress callback.
        push(&latest, true);
    });
}

/// Convert scan wedges into the Slint `WedgeInfo` model consumed by the window.
fn wedges_to_model(wedges: &[crate::scan::Wedge]) -> ModelRc<WedgeInfo> {
    let rows: Vec<WedgeInfo> = wedges
        .iter()
        .map(|w| WedgeInfo {
            name: SharedString::from(w.name.clone()),
            path: SharedString::from(w.path.to_string_lossy().into_owned()),
            is_dir: w.is_dir,
            size_text: SharedString::from(w.size_text.clone()),
            percent_text: SharedString::from(w.percent_text.clone()),
            commands: SharedString::from(w.commands.clone()),
            color: slint::Brush::SolidColor(parse_hex_color(&w.color)),
            start_deg: w.start_deg,
            sweep_deg: w.sweep_deg,
        })
        .collect();
    ModelRc::new(VecModel::from(rows))
}

/// The directory to show after pressing Up: `current`'s parent, but never
/// above `root`. Returns `None` when already at the root (nowhere to go).
fn parent_within_root(
    current: &std::path::Path,
    root: &std::path::Path,
) -> Option<std::path::PathBuf> {
    if current == root {
        return None;
    }
    match current.parent() {
        Some(p) if p.starts_with(root) => Some(p.to_path_buf()),
        // Parent escapes the volume root (shouldn't happen) — clamp to root.
        _ => Some(root.to_path_buf()),
    }
}

/// The index of the wedge whose angular span contains `deg` (clockwise from 12
/// o'clock), or `None` for a miss or the synthetic "Other" grouping wedge
/// (which has an empty path and isn't individually selectable).
fn wedge_at_angle(w: &VolumeWindow, deg: f32) -> Option<usize> {
    let wedges = w.get_wedges();
    (0..wedges.row_count()).find(|&i| {
        let e = wedges.row_data(i).unwrap();
        !e.path.is_empty() && deg >= e.start_deg && deg < e.start_deg + e.sweep_deg
    })
}

/// Update `mask` in place for a click on `index` and return the new anchor.
/// `toggle` (Ctrl/Cmd) flips just that row; `shift` selects the contiguous
/// range from the anchor to `index`; a plain click selects only `index`.
/// `index` is assumed in range for `mask`.
fn apply_selection(
    mask: &mut [bool],
    anchor: Option<usize>,
    index: usize,
    shift: bool,
    toggle: bool,
) -> Option<usize> {
    if toggle {
        mask[index] = !mask[index];
        Some(index)
    } else if shift {
        let a = anchor.unwrap_or(index).min(mask.len().saturating_sub(1));
        let (lo, hi) = (a.min(index), a.max(index));
        for b in mask.iter_mut() {
            *b = false;
        }
        for b in &mut mask[lo..=hi] {
            *b = true;
        }
        // Keep the original anchor so further Shift-clicks pivot around it.
        Some(a)
    } else {
        for b in mask.iter_mut() {
            *b = false;
        }
        mask[index] = true;
        Some(index)
    }
}

/// Push the selection mask into the window and update the selection count.
fn push_selection(w: &VolumeWindow, selected: &[bool]) {
    let count = selected.iter().filter(|b| **b).count() as i32;
    w.set_selected(ModelRc::new(VecModel::from(selected.to_vec())));
    w.set_selection_count(count);
}

/// The (absolute path, is-dir) of each currently-selected legend row, skipping
/// the synthetic "Other" grouping row (which has an empty path).
fn selected_paths(w: &VolumeWindow, selected: &[bool]) -> Vec<(std::path::PathBuf, bool)> {
    let wedges = w.get_wedges();
    let mut out = Vec::new();
    for (i, &sel) in selected.iter().enumerate() {
        if !sel {
            continue;
        }
        if let Some(row) = wedges.row_data(i) {
            if !row.path.is_empty() {
                out.push((std::path::PathBuf::from(row.path.as_str()), row.is_dir));
            }
        }
    }
    out
}

/// Parse a `#rrggbb` string into a Slint color (opaque; falls back to gray).
fn parse_hex_color(s: &str) -> slint::Color {
    let hex = s.trim_start_matches('#');
    if hex.len() == 6 {
        if let Ok(v) = u32::from_str_radix(hex, 16) {
            let r = ((v >> 16) & 0xff) as u8;
            let g = ((v >> 8) & 0xff) as u8;
            let b = (v & 0xff) as u8;
            return slint::Color::from_rgb_u8(r, g, b);
        }
    }
    slint::Color::from_rgb_u8(0x80, 0x80, 0x80)
}

#[cfg(test)]
mod tests {
    use super::{
        apply_selection, parent_within_root, parse_hex_color, scan_cache_get,
        scan_cache_invalidate_under, scan_cache_put,
    };
    use std::path::{Path, PathBuf};

    #[test]
    fn up_stops_at_root() {
        let root = Path::new("/Volumes/Disk");
        // At root: nowhere to go.
        assert_eq!(parent_within_root(root, root), None);
        // One level down: parent is the root.
        assert_eq!(
            parent_within_root(Path::new("/Volumes/Disk/sub"), root),
            Some(PathBuf::from("/Volumes/Disk"))
        );
        // Two levels down: parent is the intermediate dir.
        assert_eq!(
            parent_within_root(Path::new("/Volumes/Disk/a/b"), root),
            Some(PathBuf::from("/Volumes/Disk/a"))
        );
    }

    #[test]
    fn parse_hex_color_handles_valid_and_invalid() {
        let c = parse_hex_color("#ff8800");
        assert_eq!((c.red(), c.green(), c.blue()), (0xff, 0x88, 0x00));
        // No leading '#': still parsed.
        let c = parse_hex_color("00ff00");
        assert_eq!((c.red(), c.green(), c.blue()), (0x00, 0xff, 0x00));
        // Garbage falls back to gray, no panic.
        let c = parse_hex_color("nope");
        assert_eq!((c.red(), c.green(), c.blue()), (0x80, 0x80, 0x80));
    }

    #[test]
    fn scan_cache_stores_and_invalidates_by_path() {
        let mk = |total: u64| crate::scan::DirScan {
            entries: Vec::new(),
            total_bytes: total,
        };
        let a = PathBuf::from("/Volumes/CacheTest/a");
        let b = PathBuf::from("/Volumes/CacheTest/a/b");
        let other = PathBuf::from("/Volumes/Elsewhere/x");

        // Miss, then put, then hit.
        assert!(scan_cache_get(&a).is_none());
        scan_cache_put(a.clone(), mk(100));
        scan_cache_put(b.clone(), mk(40));
        scan_cache_put(other.clone(), mk(7));
        assert_eq!(scan_cache_get(&a).unwrap().total_bytes, 100);
        assert_eq!(scan_cache_get(&b).unwrap().total_bytes, 40);

        // Invalidate only the subtree under the test root.
        scan_cache_invalidate_under(Path::new("/Volumes/CacheTest"));
        assert!(scan_cache_get(&a).is_none());
        assert!(scan_cache_get(&b).is_none());
        // Unrelated entries survive.
        assert_eq!(scan_cache_get(&other).unwrap().total_bytes, 7);

        // Clean up so other tests on this thread start clean.
        scan_cache_invalidate_under(Path::new("/Volumes/Elsewhere"));
    }

    #[test]
    fn selection_plain_shift_toggle() {
        let mut m = vec![false; 6];

        // Plain click selects only that row and anchors there.
        let a = apply_selection(&mut m, None, 2, false, false);
        assert_eq!(a, Some(2));
        assert_eq!(m, vec![false, false, true, false, false, false]);

        // Shift-click extends a contiguous range from the anchor (2..=4).
        let a = apply_selection(&mut m, a, 4, true, false);
        assert_eq!(a, Some(2), "anchor stays put for shift");
        assert_eq!(m, vec![false, false, true, true, true, false]);

        // Shift the other direction from the same anchor (0..=2).
        let a = apply_selection(&mut m, a, 0, true, false);
        assert_eq!(m, vec![true, true, true, false, false, false]);

        // Ctrl/Cmd-click toggles a single row without clearing the rest.
        let a = apply_selection(&mut m, a, 5, false, true);
        assert_eq!(a, Some(5));
        assert_eq!(m, vec![true, true, true, false, false, true]);

        // Toggling an already-selected row clears just it.
        apply_selection(&mut m, a, 1, false, true);
        assert_eq!(m, vec![true, false, true, false, false, true]);

        // A plain click collapses back to a single selection.
        apply_selection(&mut m, Some(5), 3, false, false);
        assert_eq!(m, vec![false, false, false, true, false, false]);
    }
}
