# Docs status

## Implementation state (refreshed 2026-10-28, D-024 Win32 CPU fallback)

Source of truth for step detail: `plan.md` Steps.

- Steps 1-4: DONE (foundation, UI pilot, Windows/X11/Wayland backends).
- Step 5: CODE DONE (GNOME extension); manual legs pending.
- Step 6: DONE (UI v1 + permanents follow-up); Ubuntu automated suites green on this box 2026-10-27 (app-ui 80/80); `D-024` Win32 CPU fallback now mirrors Ubuntu on this Windows box via RustDesk virtual display (no GPU/OpenGL, `crates/app-ui/src/fallback.rs`); `T-905..T-908` Win32 legs verified 2026-10-28 on RustDesk (popup, config 27-row, keyboard, safety) and re-verified 2026-10-29 (popup.restore_clipboard live reload, WSL ingest, bump_clip).
- Step 7: release prep done on this box (release gate script, caps golden, `deny.toml`, changelog/notes drafts, `tests/manual/` scripts, dist-release zips + SHA256SUMS). Draft only: no tag/GitHub release.
- Manual pending (user/native legs): `T-410`, `T-508`, `T-606`, `T-607`, `T-702..T-705`, `T-1117`, `T-1204` (Windows `T-905..T-908` done via CPU fallback; egui path still needs real GPU display).
- Known gaps: update-check (`T-1202`, out of `0.1.0`), per-box caps goldens beyond headless, coverage-floor enforcement, cargo-packager installers (native runners), CI matrix.

## Document inventory
- `README.md` — Plan (source of truth)
- `adr-001-plugins.md` — ADR-001 — Out-of-process IPC plugin system (Notepad++-inspired UX)
- `adr-002-i18n.md` — ADR-002 — Internationalization: string ids + community catalogs
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
