# Candle Tray

Rust + GPUI rewrite of Twinkle Tray (monitor brightness from the Windows tray). Windows 10/11 only.

- `src/` is the Rust app. `legacy/` is the old Electron app, kept only as a reference for hardware behavior (`legacy/src/Monitors.js`, `legacy/src/modules/node-ddcci/ddcci.cc`, `legacy/src/modules/windows-hdr/`, `legacy/src/modules/wmi-bridge/`, `legacy/src/monitor-rules.json`, `legacy/src/electron.js`). Never build or modify it.
- The popup, tray, theme, backdrop and settings-window kit are ported from `C:\dev\codex-minibar` (same author). `vendor/gpui` is minibar's patched GPUI 0.2.2.
- MVP scope only: brightness (DDC/CI, WMI), SDR white level for HDR displays, contrast (DDC/CI VCP 0x12, opt-in per display), link levels, turn off displays, restore levels, settings window (General + Monitors), Twinkle Tray settings import, en + ru. Do not add hotkeys, OSD, schedules, idle dimming, profiles, other VCP features (beyond contrast and power), CLI/IPC, updater or telemetry.

## Building

- On this machine Cargo needs `CARGO_HTTP_CHECK_REVOKE=false` (schannel revocation checks fail).
- In git worktrees use `.\cargo-worktree.ps1 check --locked` so the main checkout's warm `target/` is reused. Quote `'--'` when forwarding flags: `.\cargo-worktree.ps1 clippy --all-targets --locked '--' -D warnings`.
- Always run `cargo check`, `cargo fmt` and `cargo clippy --all-targets -- -D warnings` before committing. Never launch the app.

## Conventions

- Read the vendored `gpui` sources and examples before UI work; LLM knowledge of GPUI is weak.
- Every setting applies live; no restart is ever required.
- Never use the pointing-hand cursor (`cursor_pointer`); clickable controls keep the arrow, as in Windows.
- Hardware I/O never runs on the UI thread (see `src/monitors`).
- Levels crossing `monitors::` are hardware percent; slider ↔ hardware mapping lives in `src/levels.rs`.
- Code, comments and commit messages in English.
