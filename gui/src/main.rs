//! Slint desktop entry point for SpaceServal.
// On Windows, mark this as a GUI (windows) subsystem binary so launching it
// does not spawn a console window. Only in release builds, so `cargo run` /
// debug builds still show logs in a terminal.
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod app;
mod license;
mod license_mgr;
#[cfg(target_os = "linux")]
mod linux_theme;
#[cfg(target_os = "macos")]
mod macos_theme;
#[cfg(target_os = "macos")]
mod menu_macos;
mod mounts;
mod scan;
mod sys_colors;
#[cfg(target_os = "windows")]
mod win_theme;

fn main() -> Result<(), slint::PlatformError> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    app::run()
}
