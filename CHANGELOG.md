# Changelog

All notable changes to Clipmonger. Semver per seg-12 §6; `1.0.0` is
reserved for the release that passes all seg-11 §5 gates.

## [Unreleased]

### Added

- Clipmonger has its own icon on Linux, Windows and macOS: launcher, programs,
  installer, windows, notifications, and a tray icon with normal, paused and
  degraded variants. Windows and macOS now show the state in the tray too.

### Changed

- Popup search now searches the whole history and every permanent clip and
  shows its own paginated results view, with the total match count.

### Fixed

- Wayland: the popup grows to fit its clips reliably. At the pointer it
  reaches the bottom of the screen; the GNOME extension must be reloaded
  (log out and back in) for this.

## [0.3.0] — 2026-11-08

macOS release: one universal app for Intel and Apple Silicon Macs on macOS
10.15 Catalina and newer, at feature parity with Windows and Linux.

### Added

- macOS backend: capture of text, HTML, RTF, images and file lists (with
  the nspasteboard.org concealed and transient markers honoured), global
  popup and permanent-clip hotkeys, paste into the app you were in (Cmd+V
  with guarded restore, or typed text), menu-bar icon, start on login
  (LaunchAgent), popup at the pointer within the screen's visible area.
- macOS packages: `Clipmonger.app` as a `.dmg` and a `.zip` (universal,
  signed ad hoc), with the licence texts and `RUST-NOTICES.txt`.
- `clipmonger-cli uninstall [--delete-history]` on macOS: stops Clipmonger,
  removes its login item and optionally the history.
- Accessibility status on macOS: without the permission, paste reports
  "Degraded" with the System Settings path, and Clipmonger notices a grant
  without a restart.
- GitHub Actions builds and tests on Intel and Apple Silicon (macOS 15 and
  26) and builds the universal app.

### Fixed

- Unix socket paths that would exceed the system limit fall back to a short
  path (long profile folders, macOS temporary folders).
- Store tests: a deadlock and timing flakes under load.

### Changed

- Settings on macOS hide the Scroll Lock and primary selection options,
  which do not exist there.

## [0.2.0] — 2026-11-07

Linux release: packages for every major distro and fixes from acceptance
runs on GNOME 46 to 51, KDE Plasma 5.27 and 6, Xfce, Cinnamon, Sway,
Hyprland and COSMIC.

### Added

- Linux packages: `.deb` and `.rpm` (programs in `/opt/clipmonger`, menu
  entries, the GNOME Shell extension system-wide) and a per-user installer
  for any distro (`install.sh`, curl or wget one-liner, checks
  `SHA256SUMS`, installs into `~/.local`, `--uninstall`).
- Release binaries are built in a Debian 12 container: they run on glibc
  2.35 and newer instead of needing the build host's glibc.
- KDE Plasma Wayland: auto-paste through KWin fake input, popup and
  permanent-clip hotkeys through the GlobalShortcuts portal (also on
  Plasma 5.27).
- Sway and Hyprland: auto-paste through the virtual keyboard; Settings
  shows the commands to bind the popup, each permanent clip and the login
  start.
- COSMIC: auto-paste with toplevel focus tracking (COSMIC Terminal gets
  Ctrl+Shift+V).
- `clipmonger-cli permanent-delete`.
- Release regression runbook (`tests/manual/release-regression.md`) and the
  VM harness that drives it (`tests/vm`).

### Fixed

- Terminals now get Ctrl+Shift+V (they ignored Ctrl+V), including GNOME
  Terminal on GNOME 46 Xorg.
- GNOME Wayland: permanent hotkeys, duplicate image and file rows, Shell
  freeze on large images, large-clip paste and format choice, popup
  placement on GNOME 50, a UI crash on GNOME 50, auto-paste on GNOME 51
  (removed Clutter API), and the extension being refused on GNOME 47 to 51.
- GNOME 48+: the popup now opens with keyboard focus, and the extension
  reconnects within seconds of a daemon start, so GNOME no longer shows
  its own "Set shortcut" dialog for the popup hotkey.
- KDE Plasma Wayland: the Settings window froze (and the hotkey stopped
  working) when opened from the popup.
- Wayland: a hotkey press brings an open popup to the front with focus.
- X11: INCR transfers truncating or stalling, Qt images not captured,
  Klipper refills stored as new rows, charset-twin duplicates.
- GNOME Xorg: Clipmonger's own paste and permanent-clip writes were stored
  as new rows; tray paused state; denylist.
- Cinnamon: every second popup opened without keyboard focus.
- Tray icon missing when the panel or bar starts after Clipmonger.
- A stale "Connection lost" line right after the daemon started.
- Password-manager text re-offered without its hint is no longer stored.
- Windows: per-clip hotkey paste into an elevated window with modifiers
  held.
- Windows: the uninstaller left the `licenses` folder (and itself) behind
  and reported an error; the installer build now checks that every
  licence file is uninstalled.

## [0.1.0] — 2026-11-04

First public pre-release, for Windows and Ubuntu (`.deb`).

### Added

- One CPU-rendered egui UI across Windows, X11 and Wayland, with independent
  popup and Settings windows, hidden logic and persistent geometry (ADR-005).
- Image/file-list previews, bounded thumbnail work and lazy file metadata.
- Default-off Windows Scroll Lock monitoring toggle and decoded UTF-8
  minimum-length filtering.
- Self-write suppression diagnostics and capture filtering for daemon paste
  and restore writes before history ingestion.

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
- `popup.preview_lines` (default 3, Popup tab): popup previews keep up to
  that many lines; `popup.preview_width` caps their total characters.
- The popup sizes itself to its clips at every show: it grows with the
  clip list up to 90% of the monitor's usable height (on Windows without
  the taskbar), is at most 30% of its usable width, and is sized again
  after a DPI or zoom change. While open it only grows.

### Changed

- Popup list: the separate "Current item" block is gone (the newest row is
  the current clip), "First page" is disabled on the first page, "End of
  history" follows the last row, and text rows no longer get an empty
  thumbnail box.
- Popup rows are highlighted and picked anywhere across their full width,
  a shortened preview shows its full text in a tooltip, and image rows
  show only the thumbnail.
- Settings filter searches every tab: matches are grouped under tab
  headings, and picking a tab clears the filter.
- Settings validation errors show in red under the setting: unparsable text
  unstages the older value, a value the daemon refuses on Apply keeps its
  text and focus, and the field keeps keyboard focus while errors appear.
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
- Settings: placement, paste delivery and log level are dropdowns; the
  Permanent Clips note says a clip's hotkey works once the clip is saved.

### Fixed

- Error text in the popup and in Settings is readable: a lighter red on
  dark and a darker red on light themes replace pure red, and the popup
  error is plain text without a tinted box.
- Windows clipboard pastes no longer randomly insert nothing: the capture
  monitor leaves the clipboard alone briefly after the daemon's own paste
  write, so the target's Ctrl+V can read it.
- Portable mode: Start on login is disabled with the reason shown, and the
  daemon refuses the change as unsupported instead of reporting a failed
  apply. `status` reports `autostart_portable`.

- A long popup history no longer runs under the footer, and a paste error is
  shown in a red box above the status line.
- A popup opened by `clipmonger-cli show-popup` pastes into the window that
  was active, instead of the next window Windows activates after the popup
  hides.
- Popup context, keyboard ownership, placement and offline/shutdown handling
  in the unified runner; Settings and remote displays use the same UI.
- Windows popup activation uses asynchronous showing and bounded confirmation;
  helper jobs recheck expiry before dispatch.
- Linux popup events carry X11 timestamps and available Wayland activation
  tokens; registered X11 popup presses toggle without creating a paste target.
- Capture loops recover, oversized captures become visible stubs, and Windows
  marker/fingerprint reads and image decoding stay bounded.
- Blob reuse verifies stored content, ingestion failures are reported, and
  backup/restore preserves raw config with checksum and rollback validation.
- X11 text aliases are served consistently; self-write matching retains
  privacy/size checks and complete backend projections.

Current native acceptance for the CPU runner and clipboard paths remains
pending; dated verification notes below retain their original scope.

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
- GNOME extension: until the daemon's config arrived it registered the old
  default shortcut `Ctrl+Alt+V`, and its settings schema still defaulted to
  it; it now uses `Ctrl+Alt+H` like the daemon.
- Windows popup: a popup opened by `clipmonger-cli show-popup` or by
  starting Clipmonger again pasted into the window of an earlier hotkey
  press instead of the window in front, because it kept that press's
  target after hiding. It now forgets the target and the admin-paste
  gesture whenever it hides.
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

## Unpublished 0.1.0 draft — 2026-10-27

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
