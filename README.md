# SpaceServal

A cross-platform desktop application built in Rust with [Slint](https://slint.dev/).

SpaceServal is modeled after the Glowing Cat Software app template: it ships the
shared chrome and dialogs (About, startup Splash / donation nag, License Key,
and Settings) along with the licensing, theming, and packaging machinery. The
main-window application logic is intentionally left as a shell to be filled in.

## Workspace layout

```
SpaceServal/
├── Cargo.toml            # workspace (members: gui, launch)
├── Cross.toml            # aarch64 Linux cross-build config
├── .cargo/config.toml    # static CRT on Windows
├── gui/                  # the Slint desktop app
│   ├── build.rs          # compiles ui/app.slint; Windows icon/version resource
│   ├── app_icon.rc       # Windows icon resource script
│   ├── ui/app.slint      # Theme, dialogs (About/Splash/License/Settings), window
│   └── src/
│       ├── main.rs
│       ├── app.rs         # controller: wires dialogs, theme, fonts, splash
│       ├── license.rs     # HMAC salt (git-ignored secret)
│       ├── license_mgr.rs # license validation + persisted Settings
│       ├── sys_colors.rs  # cross-platform OS accent/font-size reader
│       ├── macos_theme.rs / win_theme.rs / linux_theme.rs
│       └── resources/     # app_icon.png / .ico / .icns / _256.png
├── launch/               # macOS launcher shim (Open-With forwarding)
├── packaging/            # .desktop, entitlements, macOS bundle, Windows installer
└── .github/workflows/    # CI + multi-platform signed Release
```

## Building

```bash
cargo build --workspace
cargo run -p spaceserval-gui
```

### Linux build dependencies

```bash
sudo apt-get install -y build-essential libfontconfig-dev \
  libx11-dev libxcursor-dev libxrandr-dev libxi-dev \
  libgl1-mesa-dev libxkbcommon-dev
```

## Testing

```bash
cargo test --workspace
```

## Licensing

License keys are derived from the user's email with an HMAC:

```
key = HMAC-SHA256(LICENSE_SALT, email.lowercase().trim())
        -> hex, first 16 chars, uppercased, grouped XXXX-XXXX-XXXX-XXXX
```

The salt lives in `gui/src/license.rs`, which is **git-ignored** (a secret).
On a fresh clone / in CI, `gui/build.rs` regenerates it from the `LICENSE_SALT`
environment variable (or a placeholder, so the crate still compiles).

Generate a key locally with:

```bash
./generate_license_key.py user@example.com
```

Settings (including the saved license, theme, font size, and the
"Show Network Shares" toggle — removable drives are always listed) persist to
`~/.spaceserval-settings.json`.

## Packaging

- **macOS**: `packaging/macos_bundle.sh <target> <out-dir>` builds a signed
  `.app` + `.dmg`.
- **Windows**: `packaging/windows/spaceserval.iss` (Inno Setup) builds an
  installer.
- **Linux**: `cargo deb` / `cargo generate-rpm` using the metadata in
  `gui/Cargo.toml`.

The GitHub `Release` workflow builds, signs, and publishes all of the above.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
