# Changelog

All notable changes to Clipmonger. Semver per seg-12 §6; `1.0.0` is
reserved for the release that passes all seg-11 §5 gates.

## [Unreleased]

## [0.1.0] — 2026-10-27

Pre-release hardening build (R-F1). Draft artifacts are unsigned local
zips; no tag or GitHub release is created by this step (R-F4).

### Added

- Foundation (Step 1): Cargo workspace (core model/store/config/policy,
  platform traits + fakes, IPC, daemon skeleton, CLI skeleton);
  T-201..T-210, T-301..T-308, T-310..T-314.
- UI shell pilot (Step 1b): popup/tray/lifecycle against fakes + real IPC.
- Windows backend (Step 2): clipboard listener/transfer, hotkeys, enigo
  paste, on-demand elevated helper protocol, tray, autostart.
- X11 backend (Step 3): XFixes monitor, transfer + INCR, hotkeys, paste,
  KSNI tray model, `.desktop` autostart; Xvfb harness.
- Wayland backend (Step 4): data-control capture, portal hotkeys,
  dual X11+Wayland merge, honest write-and-toast paste fallback.
- GNOME Shell extension (Step 5): capture/hotkey relay, chunked blob
  transfer, config sync; Node test suite (T-701).
- UI v1 (Step 6): popup, maximalist config window, tray, keymap;
  permanents (store CRUD + groups, `permanent.*` IPC + exec, editor and
  popup group tabs, T-315..T-318).
- Release prep (Step 7): `[profile.release]` thin LTO + strip,
  `scripts/release-gate.sh`, headless caps golden test, `deny.toml`,
  `tests/manual/` QA scripts, release/perf evidence.

### Changed

- Nothing (first release).

### Fixed

- Nothing release-scoped (first release). Pilot bug: `ipc_bridge` sent
  `{id}`, daemon expects `{clip}` (fixed in Step 6).

### Known manual-pending items

- T-409 UAC matrix and T-410 reboot leg (Windows host, user).
- T-508 real-DE pass, T-606/T-607 compositor matrix (Ubuntu box).
- T-702..T-705 GNOME legs (Ubuntu 24.04 GNOME box).
- T-905..T-908 UI matrix (real displays).
- Per-box caps goldens beyond headless, coverage-floor enforcement,
  update-check (T-1202, out of 0.1.0 scope), cargo-packager installers
  (deferred to native runners).
