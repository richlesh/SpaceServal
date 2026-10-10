//! Slint controller for SpaceServal.
//!
//! Builds the main window and wires the shared Glowing Cat dialogs — About,
//! Splash (donation nag), License Key, and Settings — to the persisted
//! [`Settings`](crate::license_mgr::Settings). The main-window application
//! logic is intentionally omitted; this is the surrounding chrome only.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use slint::{ModelRc, SharedPixelBuffer, SharedString, VecModel};

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

/// Open the Settings dialog, prefilling the saved token + theme + font size.
fn open_settings_dialog(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) {
    let (token, dark) = {
        let c = ctrl.borrow();
        (c.settings.api_token.clone(), c.settings.theme_is_dark())
    };
    w.set_settings_token(SharedString::from(token));
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

/// Save the token + theme + font size + network-shares toggle from the
/// Settings dialog.
fn settings_save(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) {
    let token = w.get_settings_token().trim().to_string();
    let dark = w.get_settings_theme_index() != 0;
    let font_idx = w.get_settings_font_index();
    let show_network = w.get_settings_show_network();
    let (result, network_changed) = {
        let mut c = ctrl.borrow_mut();
        c.settings.api_token = token;
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
