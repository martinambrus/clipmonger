# Changelog

All notable changes to Clipmonger. Semver per seg-12 §6; `1.0.0` is
reserved for the release that passes all seg-11 §5 gates.

## [Unreleased]

### Added

- Paste delivery switch `popup.delivery` (config window, Popup tab):
  `clipboard_restore` (default — clipboard write + Ctrl+V with
  guarded restore, any size, standard and elevated routes) or
  `unicode_inject` (synthetic keystrokes).
- Portable mode: a `portable.marker` file next to `clipmonger.exe` keeps
  the profile beside the binary.
- One UI per profile (ADR-004): starting Clipmonger again brings up the
  running popup instead of a second copy with a second tray icon.
- Daemon-to-UI push (`events.subscribe`, `ui.show_popup`):
  `clipmonger-cli show-popup` opens the popup through the daemon, which
  starts the UI when none is running.

### Changed

- Windows admin-window paste (ADR-003): the app asks once when an
  elevated window is first focused. Program Files installs then use one
  UAC prompt to register a logon task that starts the elevated helper
  silently at every logon; portable copies use one UAC prompt per run and
  never create a scheduled task. Daemon, helper and popup run without
  console windows.
- The daemon owns the popup hotkey on every OS (ADR-004): presses on X11,
  through the Wayland portal and from the GNOME extension now open the
  popup. On Windows a changed popup hotkey applies without a restart; a
  refused chord keeps the previous one and shows in caps. Admin-window
  pastes need the hotkey press that opened the popup: from popups opened
  by the tray, the CLI or a second launch they are refused with a
  visible reason.
- Settings: the unused `windows.helper_auto_elevate` row was removed
  (28 rows).

### Fixed

- Any process holding the profile token could obtain an admin-paste
  gesture through `hotkey.fired` (ADR-003 known limitation); gestures now
  come only from hotkey presses and go only to the verified UI.
- Settings: the Windows settings window showed text values in JSON
  quotes, so an edited value (for example `popup.hotkey`) failed
  validation and was never applied; list values in both settings windows
  were shown as JSON but read back as comma-separated text. Both windows
  now show and read the same text form for every row kind.
- A refused admin-window paste from a popup not opened by the hotkey (no
  gesture) marked paste Degraded ("elevation refused") until the daemon
  restarted. It is now refused and audited only; a cancelled UAC prompt
  still degrades paste (seg-04 §5).
- Windows admin-window pastes were delayed by up to 30 s, silently
  dropped after ~7.5 min idle, or ran late into whatever window had
  focus: the helper heartbeat stalled on synchronous pipe I/O and jobs
  had neither a target window nor a deadline. Pastes now go only to the
  window the hotkey was pressed in, within 2 s of the pick, or fail with
  a visible reason.
- Tray Exit in the Windows popup now also stops the daemon (and the
  portable helper).
- The helper logon task is no longer stopped on battery power or after
  72 hours, and task creation works for install paths with spaces.
- UI test drift: the chord parser test and the settings-row tests match
  the configured default hotkey and rows again.
- The IPC acceptance suite did not compile for Windows (its socket tests
  use Unix-only APIs), so `cargo test -p clipmonger-ipc` failed there;
  those tests are now Unix-only and the protocol tests run on Windows.
- The default popup hotkey test still expected the old `ctrl+alt+v`
  default (changed to `ctrl+alt+h` in a1c36c9) and failed; it expects
  `ctrl+alt+h` now.
- UI text rendered as squares (tofu) on real displays: eframe's
  `default-features = false` had dropped its embedded/system fonts.
  Restored `default_fonts`/`system_fonts`; regression test pins it.
  Found by the user on Ubuntu 24.04 during Step 6 closure.
- Step 6 Ubuntu closure: `backup.create`/`backup.restore`/`logs.tail`
  daemon arms wired (were `unknown_method`; restore reopens the store
  in-process), seg-09 §1 popup viewport recipes (frameless/on-top)
  implemented, Linux tray added via ksni (was degraded-only).
- Joint manual QA fixes (Ubuntu, user-verified): copied text from X11
  (`text/plain;charset=utf-8`) no longer stores blank previews; arrow
  keys navigate while the popup search box has focus; Settings window
  is a deferred viewport (input/repaint independent of the popup) with
  a working native close button and status visible on every tab.
- T-409 UAC matrix (PASS 11/11, Windows host): daemon hotkey could
  never fire (global-hotkey message pump + registration thread);
  helper supervisor panicked creating the Tokio pipe outside the
  runtime; heartbeats could be mistaken for job results and the
  supervisor died after helper refusals; helper target validation
  raced the UAC desktop switch. Release builds now enable the
  platform backend features so the shipped daemon is not a skeleton.

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
