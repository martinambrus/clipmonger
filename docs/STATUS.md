# Docs status

## 0.3.0 (2026-11-08)

macOS release: one universal app (Intel and Apple Silicon), floor macOS 10.15
Catalina, at parity with the Windows and Linux v1 ([macOS plan](macos-v1-plan.md),
[release notes](release-notes-0.3.0.md)). Tested on Intel VMs 10.15, 13, 15 and 26
and on GitHub Apple Silicon runners; Linux and Windows packages re-tested. The
history below is kept for reference.

## Current implementation (2026-11-04)

Start at [v1 continuation](v1-continuation.md) for exact candidate identity,
software evidence, outstanding task IDs, test packet and the next-agent procedure.
The recovered batch has 65 tasks: 53 completed and 12 partial. Phase 1 is
implemented; Phase 7 packaging/QA and native acceptance remain open.

- The single CPU-rendered egui runner (ADR-005) replaces eframe/glow and the
  separate Win32 fallback. Settings has 29 rows; GUI builds require Rust 1.95
  or newer for the locked dependencies.
- Popup Alt+Tab landing on the most recent window instead of the one before it
  is a documented Windows limitation. A tray-opened popup now takes keyboard
  focus. The guided Windows pass on that build passed except the skipped Scroll
  Lock case. The popup/Settings UI batch from its findings (f99f45e and earlier)
  passed its guided Windows retest on 2026-11-04.
- Windows clipboard paste now defaults to keeping the selected item on the
  clipboard; restoration is opt-in and existing explicit preferences are preserved.
- The combined draft includes image/file-list storage, backup/restore safety,
  capture/storage hardening, helper-registration and failed-paste fixes. Source
  inclusion is not native acceptance or delivery to main.
- Normal automated checks, GNOME tests, release builds and package payload checks
  are recorded in the continuation. The additional performance results retain
  their actual measurements and initial target; no timing miss is waived.
- The user takes over Windows UI/clipboard/installer/UAC testing. Current CPU-runner
  T-905 through T-908 and capture fault paths need actual results. T-409 legs 1-10
  and T-410 passed on 2026-11-04; T-409 leg 5 (CLI-opened popups) passed on 527ad07
  the same day. T-409 legs 11-17 remain open. Historical dated passes remain scoped to their old builds.
- Windows-only 0.1.0 pre-release: artifacts (NSIS setup, portable ZIP,
  SHA256SUMS) are built from 5a5f4e1, see
  [the release notes](release-notes-0.1.0.md). The published 0.1.0 release
  (tag at b46698f) also carries an Ubuntu `.deb`.
  T-1204 (install guide on Windows 10 and Windows 11) passed on 2026-11-05.
- Resolved 2026-11-04: dropped clipboard pastes into elevated windows were
  the daemon's capture reading its own paste write while the target handled
  Ctrl+V; fixed and verified on the Windows 10 VM, see
  [the continuation](v1-continuation.md#resolved-dropped-clipboard-paste-into-elevated-windows-2026-11-04).
- Linux on GNOME 46 (Xorg and Wayland) passed its user-run acceptance on
  2026-11-05: hotkeys, paste, permanent clips, capture, privacy, Shell restart,
  tray and autostart (see
  [the continuation](v1-continuation.md#linux-acceptance-on-gnome-46-2026-11-05)).
  Other desktops, the GNOME version matrix and the portal rebind remain open.
  The blur-protocol licensing was settled on 2026-11-05 (LGPL-2.1-or-later for
  those generated bindings, notice in `licenses/wayland/`).
- [Feature-to-test index](v1-qa-traceability.md) maps the v1 slices and explicit
  evidence gaps. The wider release program still needs the 24-hour soak,
  million-case fuzzing, coverage calibration, per-box caps goldens, applicable
  integration matrix, signed manuals and the final release audit. Seven passing
  software checks are not all seven release gates.
- Publishing the Windows 0.1.0 pre-release does not close the gates above or
  any remaining task.

## Historical implementation records

The dated records below describe their original builds and environments.
They do not establish acceptance of the current runner or pending drafts.

## Implementation state (refreshed 2026-10-30, daemon-owned popup hotkey)

Source of truth for step detail: `plan.md` Steps.

- Steps 1-4: DONE (foundation, UI pilot, Windows/X11/Wayland backends).
- Step 5: CODE DONE (GNOME extension); manual legs pending.
- Step 6: DONE (UI v1 + permanents follow-up); Ubuntu automated suites green on this box 2026-10-27 (app-ui 80/80); `D-024` Win32 CPU fallback now mirrors Ubuntu on this Windows box via RustDesk virtual display (no GPU/OpenGL, `crates/app-ui/src/fallback.rs`); `T-905..T-908` Win32 legs verified 2026-10-28 on RustDesk (popup, config 27-row, keyboard, safety) and re-verified 2026-10-29 (popup.restore_clipboard live reload, WSL ingest, bump_clip).
- Step 7: release prep done on this box (release gate script, caps golden, `deny.toml`, changelog/notes drafts, `tests/manual/` scripts, dist-release zips + SHA256SUMS). Draft only: no tag/GitHub release.
- Windows admin-window paste 2026-10-30 (ADR-003, D-025): elevated helper reworked (single-thread pipe protocol, liveness from the pipe instead of heartbeats, per-job target window + 2 s deadline, popup UI owns the hotkey, windowless daemon/helper/UI, `portable.marker` portable profile). User-verified on this Windows box: portable (`C:\Temp\clipmonger-portable`) consent → one UAC → admin and standard pastes (helper round trip 1-2 ms), 17 min idle, tray Exit stops UI + daemon + helper, no scheduled task; installed (`C:\Program Files\Clipmonger`) consent → one UAC → `ClipmongerHelper` logon task, logoff/logon starts the helper with no visible window, app start reconnects with no prompt. Settings matrix is 28 rows (`windows.helper_auto_elevate` removed, `popup.restore_clipboard` counted); app-ui suite green.
- Daemon-owned popup hotkey 2026-10-30 (ADR-004, D-026): the daemon owns the popup hotkey on every OS and pushes `ui.show_popup` to one UI per profile (`ui.lock`; a second launch hands over); a press captures the paste target and mints the only gesture that allows an admin-window paste; Windows applies hotkey changes without a restart. Verified on this Windows box with synthesized presses: popup opens in front and a second press hides it (5/5), `clipmonger-cli show-popup`, UI restart by the next press, daemon restart by the UI, second-launch handover. Hands-on checklist passed 2026-10-31 (user, over RustDesk; it found the Settings edit-field fix in CHANGELOG); Linux items in ADR-004 "Open items".
- Windows test infra 2026-10-30: installed build + marker-tagged `ClipmongerHelper` logon task active for the QA user; portable copy kept in `C:\Temp\clipmonger-portable`; earlier test folders, binaries and the legacy task removed.
- Manual pending (user/native legs): ADR-004 Linux open items (X11 live hotkey change, push path on a Linux desktop), `T-409` legs 11-17, `T-508`, `T-606`, `T-607`, `T-702..T-705`, `T-1117`, `T-1204` (Windows `T-905..T-908` done via CPU fallback; egui path still needs real GPU display).
- Known gaps: update-check (`T-1202`, out of `0.1.0`), per-box caps goldens beyond headless, coverage-floor enforcement, cargo-packager installers (native runners), CI matrix; explicit DACL for the helper pipe (default DACL + peer binary checks today); a popup-hotkey change needs a daemon restart on Linux (X11, portal, GNOME extension; ADR-004 open items 1-3); `store::tests::self_concurrent_burst` is load-sensitive (failed once under a parallel rebuild, passes alone).

## Document inventory
- `README.md` — Plan (source of truth)
- `adr-001-plugins.md` — ADR-001 — Out-of-process IPC plugin system (Notepad++-inspired UX)
- `adr-002-i18n.md` — ADR-002 — Internationalization: string ids + community catalogs
- `adr-003-elevated-helper.md` — ADR-003 — Elevated helper: consent per install mode, pipe liveness, paste deadlines
- `adr-004-daemon-popup-hotkey.md` — ADR-004 — Daemon-owned popup hotkey, popup push, press-bound gestures
- `plan.md` — Plan — Clipmonger (cross-platform ArsClip successor)
- `seg-00-index.md` — Spec index — Clipmonger (cross-platform ArsClip successor)
- `seg-01-product-scope.md` — Product scope — full ArsClip parity + milestone slices
- `seg-02-architecture.md` — Architecture — processes, crates, traits, flows
- `seg-03-clip-model-storage.md` — Clip model, formats, storage, config, macros
- `seg-04-windows-backend.md` — Windows backend (crate `backend-windows`)
- `seg-05-linux-x11-backend.md` — Linux X11 backend (crate `backend-x11`)
- `seg-06-linux-wayland-backend.md` — Linux Wayland backend (crate `backend-wayland`, v1)
- `seg-07-gnome-extension.md` — GNOME Shell extension (dir `gnome-ext/`, v1 per D-013)
- `seg-08-macos-backend.md` — macOS backend (crate `backend-macos`, v2)
- `seg-09-ui-ux.md` — UI/UX — popup, config (maximalist), bar, tray, keys (crate `app-ui`)
- `seg-10-sync-sharing.md` — Sync, sharing, portability (v3; F-701/702/703)
- `seg-11-testing-qa.md` — Testing + QA (process owner: the human QA lead, D-017)
- `seg-12-build-release.md` — Build + release ($0 pipeline per D-018)
