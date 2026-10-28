# Plan — Clipmonger (cross-platform ArsClip successor)

## Goal

Ship an MIT-licensed, Rust-built, ArsClip-parity clipboard manager for
Windows, Linux (X11 + Wayland + GNOME Shell extension), and macOS —
specified across the planning documents (seg-00…seg-12 + ADRs) detailed
enough for independent agents to implement, in v1/v2/v3 slices, with
unit/integration/e2e/manual QA at every step.

## Success Criteria

1. All spec documents internally consistent (milestones, IDs, refs,
   matrices pass lint + human review) and user-accepted (Draft → Final).
2. All architecture-changing uncertainties resolved before freeze (Phase
   0 gates); remaining VERIFY items cannot change interfaces, formats,
   security boundaries, or milestone feasibility.
3. v1 releases ONLY Windows + Linux/X11 + Linux/Wayland + GNOME ext
   (no macOS release/signoff in v1); user signs Linux/Win.
4. v1 vertical journeys pass per environment (capture→store→popup→
   paste→guarded restore→history correct), incl. degraded paths.
5. Store migrations, blobs, backup/restore, IPC versioning, and process
   lifecycle specified so independent agents cannot diverge.
6. Artifacts built from pinned toolchains, tested on declared floors,
   published with checksums + documented trust limits.
7. v2 and v3 each get a design-reopen gate before coding (extension
   points stable; deferred behavior honestly revisited).

## Approach

Review-then-build with Phase 0 closure gates (review finding 4):
doc-level closure (consistency, lifecycle, paste rules, storage, IPC)
lands in the spec NOW; runtime proofs (GNOME capture PoC, Wayland truth
table, portal behavior, X11 coexistence, build-floor proof) MUST complete
before v1 scope freeze. Only then does architecture FREEZE — and after
freeze, corrections use short ADRs (seg-00 §5), never silent edits and
never a freeze that forbids evidence. Highest-risk item (Wayland paste
injection) is time-boxed as a spike with a specified honest fallback,
and CANNOT block v1. Reuse-first throughout (seg-00 §4):
verified-license dependency > port > reinvent; zero GPL ingestion.

## Steps

- **0.1 Consistency repair (DONE in Codex fold, D-019).** GNOME=v1,
  macOS=v2, mobile dropped, refs/tests/macros fixed, GNOME 46–51 matrix,
  D-014 clarified. Gate: cross-ref/milestone lint clean.
- **0.1b Claude-review fold (D-020, DONE).** 67-item second review
  triaged: ~40 adopted (direct OS transfer, IPC method catalog, threat
  model, privacy signals, config catalog), ~20 already-covered, rest to
  VERIFY tasks. Gate: same lint clean.
- **0.2 Process/lifecycle finality (SPEC DONE in seg-02 §1/§8).** Tray
  owner, launcher/attach/quit, autostart target, headless mode,
  main-thread affinity, rendezvous. Gate: reviewed sequence coverage
  (launch, autostart, crash/restart, quit, headless).
- **0.3 Paste-correctness rules (SPEC DONE in seg-02 §4/§6).** Target
  sessions, suppression registry, guarded restore. Gate: T-206…208 +
  T-908 green + one Win + one Linux real-session PoC before backend
  paste lands.
- **0.4 Storage closure (SPEC DONE in seg-03).** Blobs/GC, no-sensitive
  rows, id-vs-hash split, search text, soft-delete, config flatten,
  transactional restore, per-OS perms. Gate: T-302/303/310…314 green.
- **0.5 IPC closure (SPEC DONE in seg-02 §9).** Envelope, versioning,
  chunking, backpressure, auth. Gate: T-201/205/210 conformance green.
- **0.6 Linux/Wayland/GNOME spikes (FUTURE, gates v1 scope freeze).**
  GNOME 46+51 capture PoC (seg-07), Wayland truth table + portal
  behavior (seg-06), X11 coexistence + modmap (seg-05). Gate: results
  written back; no blocking VERIFY remains for promised v1 function.
  Includes per-avenue paste-spike outcomes (H-01), wtype portal-level
  probes (M-09), xdg-terminal-exec check (L-04).
- **0.7 Floor/build proof (FUTURE, gates v1 scope freeze).** Oldest-LTS
  glibc assertion, TS toolchain pin, packaging host tools (seg-12).
  Gate: install/run smoke on oldest declared runtime per v1 target.
- **0.8 User acceptance (DONE 2026-10-16).** User accepted the full
  spec + Claude-fold edits; status flipped Draft → Final. Delivers:
  BUILD AUTHORIZATION — Phase 0 proofs + v1 implementation may proceed.
- **1. Foundation. (DONE 2026-10-26, WSL; commits b7c937d…332dc02)**
  Workspace + core (model/store/config/policy) +
  platform traits + fakes + IPC + daemon skeleton + CLI skeleton.
  Seg-02/03. Tests: T-201…210, T-301…308, T-310…314. Unlocks everything.
  Evidence: 124 tests green (core 48, platform 19, ipc 48, daemon/cli 8+1);
  fmt + clippy -D warnings clean; windows-gnu check + PE build green;
  CLI daemon-down path run on the Windows host via WSL interop.
  Deferred inside step: T-304 (v2 macrolib, D-F6), T-209 full lifecycle
  matrix + T-205/T-211 Windows legs (native runner, Step 2), image
  pixel-decode hashing (D-F4 note). Freeze decisions D-F1…F24 (D-024
  `Win32` CPU fallback unified — `fallback.rs` reuses `PopupVm`, no
  `GPU`/`OpenGL`, `RustDesk`/`RDP`/`headless` OK) in
  `.agents/tmp/foundation-v1/ledger.md`.
- **1b. UI shell pilot. (DONE 2026-10-26, WSL)** Popup/tray/lifecycle against fakes + real IPC
  (validates paste-target behavior before backend paste exists). Seg-09
  subset. Then backends (steps 2–5 as listed).
  Evidence: crates/app-ui (strings/search/keymap/placement/popup-VM/
  lifecycle/IPC-bridge/tray logic + glow eframe shell); 26 acceptance
  tests (T-901/902/903/904-logic/910) pass both feature modes with
  DISPLAY unset; workspace 150 tests green; windows-gnu check clean.
  Deferred: T-905 window matrix (needs real displays), `wgpu` renderer
  (`glow` pilot, U-F2; `D-024` `Win32` CPU fallback now ships on Windows
  via `fallback.rs` — `PopupVm` reused, no `GPU`/`OpenGL`, `RustDesk`/`RDP`
  OK; `egui` `glow` remains on real displays, `wgpu` still deferred),
  tray Paused variant (U-F5), daemon search wiring
  (U-F8) — all Step 6 / backend steps.
  Manual evidence 2026-10-26: Windows window-open test PASSED (user-run
  clipmonger.exe on Win host: opens, stays, renders empty popup; offline
  fix f58987f after first run exited pre-window).
- **2. Windows backend. (DONE 2026-10-27, WSL+Win host; T-409 PASS 11/11)**
  Seg-04 (listener, transfer, hotkeys, enigo
  paste, on-demand elevated helper, tray, autostart). Tests T-401…410
  incl. the UAC manual matrix.
  Evidence: named-pipe IPC 8/8 on Win host; pure logic T-401/402 (37
  tests); session harness 8/8 serial on Win host (monitor/transfer/
  hotkeys/integrity/guarded-restore/autostart/caps); helper protocol
  T-403/413-unit/414 (12 tests); daemon live-ingest + caps proven via
  CLI. Coexistence with ArsClip + RustDesk/ScreenConnect verified by
  design (passive listener; sync tools paused only for clean test
  runs). T-409 UAC matrix PASSED 11/11 interactively on the Windows
  host 2026-10-27 (verdict in tests/manual/T-409-uac-matrix.md);
  bugs found and fixed en route: release daemon lacked backends
  (feature flags), daemon hotkey could not fire, helper supervisor
  panicked outside the Tokio runtime, heartbeat/result stream race,
  UAC foreground-settle race. Paste delivery switched to
  clipboard-restore by default (popup.delivery, user decision) and
  re-verified live on standard + elevated routes. Open: T-410
  reboot leg, explicit pipe DACL, PNG/DIB conversion.
- **3. X11 backend. (DONE 2026-10-26, WSL Xvfb)** Seg-05 (XFixes + manager role, transfer+INCR,
  hotkeys, enigo paste, KSNI tray, .desktop autostart). Tests T-501…508
  (Xvfb headless).
  Evidence: pure logic 38 tests (T-501/502/503/504/510); Xvfb serial
  harness 10/10 (capture/readback, 5MiB INCR, owner timeout,
  SAVE_TARGETS dying-owner, manager coexistence capture-only,
  primary policy, hotkey conflict+fire, paste transfer/restore,
  autostart, caps); daemon smoke under Xvfb ingests with x11 caps.
  Open: T-508 real-DE manual pass (Ubuntu box), t507 keystroke leg
  under a real WM, reboot leg. Note: this box needed /tmp/.X11-unix
  remounted rw for Xvfb.
- **4. Wayland backend. (DONE 2026-10-26, WSL + nested sway)** Seg-06: paste SPIKE first (§7 exit criteria),
  then data-control backends, portal hotkeys + fallbacks, dual-backend
  merge. Tests T-601…607.
  Evidence: spike verdicts (WSLg no data-control = expected negative;
  sway zwlr v2 watcher ok); pure T-601/602/603/608 (21 tests); sway
  harness 8/8 (capture/readback, password-hint, clear, kill-owner,
  primary policy legs, fallback live); dual X11+Wayland smoke merges
  to one entry; paste = write+toast fallback (W-F7). Open: T-606/607
  per-compositor manual matrix + real injection (Ubuntu GNOME/KDE
  box), t609 primary positive legs (needs primary-capable peer),
  X11-on-local-Xvfb connect hang noted in smoke (WSLg :0 used).
- **5. GNOME extension. (CODE DONE 2026-10-26, WSL headless; T-702…705 manual pending)**
  Seg-07 (relay, keybindings, IPC, matrix, packaging). Tests T-701…704.
  Evidence: T-701 node:test 30/30 (codec incl. 3MiB→12 chunks + sha256
  vectors, relay fake-clock, polddiff, chord converter); tsc clean;
  committed dist/ + install-extension.sh (no npm at install). Daemon
  gaps closed: capture.report stores real payloads (was fake replay
  insert), blob.put assembled+sha-verified, hotkey.fired mints gesture
  token, hotkey.conflict caps note, config.get/set persisted, ipc
  lifecycle callback flips extension_connected. Freeze G-F1…G-F8 in
  working notes. Open: T-702…705 + Phase-0 MetaSelection rich-format
  PoC on Ubuntu 24.04 GNOME 46 box (VERIFY-marked APIs in
  gnome-ext/src), config.changed broadcast + hotkey popup push
  (Step 6).
- **6. UI v1. (CODE DONE 2026-10-27, WSL headless; T-905…908 manual pending)**
  Seg-09 (popup, tray, maximalist config for v1 scope,
  keymap). Tests T-901…907 subset.
  Evidence: ui_v1_acceptance 34/34 (T-901/902/903/909/910, tray model,
  target-session stale-never-pastes, ipc_bridge wire shapes) + pilot
  26/26; config window (26-row matrix, staged Apply/Revert, settings
  filter), Windows tray via tray-icon (Linux tray degraded, ksni
  follow-up), theme via ui.theme, honest AccessKit About sentence
  (not compiled in this build). Pilot bug fixed: ipc_bridge sent
  {id}, daemon expects {clip}. Daemon honesty: unknown methods
  (incl. history.*) return unknown_method, never purge. Paste
  delivery row added 2026-10-27 (popup.delivery, 27-row matrix,
  default clipboard_restore; user decision). Ubuntu closure
  2026-10-27 (this box, GNOME 46/Wayland): toolchain 1.99.1 installed,
  automated suites first observed here (app-ui 80/80 incl. new
  backup/logs, tray-mapping, viewport and font tests); backup/logs
  daemon arms wired (d232f9a, store reopened in-process on restore),
  seg-09 §1 popup viewport recipes implemented (952cf74), Linux ksni
  tray added (b81aa19, ksni 0.3.7), tofu-squares font regression fixed
  (c94d6d3, eframe default_fonts/system_fonts restored). Joint
  manual session 2026-10-27 (user hands, agent shell): charset-MIME
  blank clips (e09bc4c), search-focus arrows (fd677f3), settings
  deferred viewport + close (624078c, e324027) all user-verified;
  T-906 walk PASS with findings, T-907 popup leg PASS, T-908 safety
  leg PASS (delivery = extension/Xorg round), T-905 GNOME row
  frameless PASS / on-top+placement platform-limited on Wayland.
  Open: logout round (GNOME extension enable; Xorg session rows),
  T-905 fullscreen/scale rows, signoff filing. T-906 script
  corrected 26→27 rows.
  Permanents follow-up (DONE 2026-10-27): store CRUD + groups
  (perm_groups authoritative, no migration), permanent.* IPC +
  exec (Paste kind, shared paste path; X11 honest unavailable),
  config-window editor + popup group tabs. Rulers C/I/D/U 13/2/7/10
  green. Per-clip hotkeys stored, not yet registered live.
- **7. v1 hardening + release. (CODE DONE 2026-10-27; manual signoffs pending)**
  Seg-11 gates (matrix, goldens, signoffs,
  budgets) → seg-12 release (artifacts, notes, checksums, bypass docs).
  Evidence: scripts/release-gate.sh exit 0 (fmt/clippy/tests/xvfb
  serial/windows-gnu/gnome/caps-golden/cargo-deny PASS here);
  dist-release/clipmonger-0.1.0-{windows,linux}-x86_64.zip +
  SHA256SUMS verified (gitignored, cross-built Windows zip labelled);
  CHANGELOG 0.1.0 + draft release notes; 15 manual-QA scripts in
  tests/manual/. Perf calibration: idle RSS 6.3MB, 10k search p95
  76.8ms release (over initial 50ms budget — FNF for v1 freeze).
  Open: manual signoffs (T-905…908/GNOME/Ubuntu legs; T-409 PASSED
  2026-10-27 on the Windows host; T-908 Windows Notepad transient
  fix 2026-10-28 a1c36c9 — RustDesk virtual display: monitor empty→data
  drain + fallback poll, App A/B ingest + stale-target verified;
  T-907 Windows fallback Tab/Esc/Config verified 2026-10-28 f47bff2
  — RustDesk: degraded hotkey (daemon owns Ctrl+Alt+H), fallback
  minimizes to taskbar, Config placeholder),
  zip-crate
  advisory upgrade (documented ignores in deny.toml; extract()
  unused), GitHub release creation (user decision), cargo-packager
  installers on native runners.
- **8. v2 (after design-reopen gate).** Re-validate extension points
  vs current deps/platforms, then: macOS backend (seg-08), macrolib JS
  (seg-03 §7), bar/AutoSwitch/paste-modes (seg-09), export (seg-10 §4),
  plugin host + install/manage UX (F-801/802, ADR-001), i18n
  infrastructure + first community catalogs (F-803, ADR-002; Slovak
  expected, user-supplied, non-blocking).
  macOS release/signoff begins here. Same gates.
- **9. v3 (after design-reopen gate).** Re-validate trust/keys/merge/
  manifest/quota/file-list semantics, then: sync LAN (v3a) + user-cloud
  (v3b) (seg-10), extras (seg-01 §10), Flathub evaluation (seg-12 §7),
  curated plugin-list evaluation (F-802, ADR-001).
  Never open live SQLite from a synced folder. Same gates.

Spec file inventory (all Final — user-accepted 2026-10-16, gate 0.8;
ADR-001/002 + D-021/022/023 accepted 2026-10-25 under a user-authorized
reopen): seg-00-index, seg-01-scope,
seg-02-architecture, seg-03-model-storage, seg-04-windows, seg-05-x11,
seg-06-wayland, seg-07-gnome-ext, seg-08-macos, seg-09-ui, seg-10-sync,
seg-11-testing, seg-12-release, adr-001-plugins, adr-002-i18n,
plan.md (this file).

## Validation Plan

- Per unit: `cargo test -p <crate>` (unit), segment `[integration]`
  suites (Xvfb: `Xvfb :99 -screen 0 1600x1200x24` + `DISPLAY=:99`;
  nested Wayland per seg-06; localhost daemon pairs), `cargo fmt
  --check` + `cargo clippy --workspace --all-targets --all-features -- -D warnings` (matrix-adjusted) on every PR, `[e2e]` scripted
  journeys on real/VM OSs, `[manual-qa]` scripts signed per seg-11 §6.
- Release gates per seg-11 §5 (matrix green, caps goldens diffed,
  signoffs filed, budgets/coverage met). Coverage floor 80% on
  core/platform/ipc (calibrated in v1).
- **Highest-risk validation: the seg-06 §7 paste spike (T-607)** — if it
  fails, v1 ships the specified write-and-toast fallback, not a slip.
  Second: the Windows UAC matrix (T-409) and GNOME version matrix (T-704).

## Risks / Open Questions

- R-001 Wayland/GNOME capture/write/paste drift → Phase 0 proofs +
  truthful caps/fallbacks (seg-06/07), incl. per-avenue paste-spike
  outcomes gating v1 scope freeze (review H-01).
- R-002 Popup focus / clipboard restore races → PasteTarget sessions +
  suppression + guards (seg-02 §4/§6; T-206…208, T-908).
- R-003 Storage schema mistakes before migration 0001 → closure gate
  0.4 (blobs, identity, search, restore, perms).
- R-004 GNOME API drift 46→51 → explicit matrix + EOL-after-v1 +
  per-major fixtures/VMs.
- R-005 Linux floor mismatch → oldest-tested-LTS + CI assertion (D-012
  amended); 2.17-sysroot only on demand.
- R-006 Unsigned distribution → accurate trust UX; checksums = integrity
  only; signing/provenance revisited on demand (D-018).
- R-007 macOS AX/TCC + main-thread integration → v2 packaging PoC before
  macOS scope freeze (seg-08, T-808).
- R-008 Sync correctness/security → mandatory v3 design-reopen; id-based
  merge + LWW + signed manifests already spec'd (seg-10).
- R-009 Local privacy → explicit plaintext-at-rest boundary, user-only
  permissions, safe backups/logs/evidence (seg-03 §5/§6, seg-11).
- R-010 Windows elevated helper → on-demand spawn from protected paths
  only, reverse-connect pipe + mutual peer auth, input-only narrow
  protocol + gesture tokens, self-exit on disconnect, full audit
  (seg-04 §5; T-409/413/414).
- R-011 GNOME extension session impact → size-capped async relay with
  backoff, degrade-to-capture-paused on wedge (never hang the session),
  daemon survives silence/disconnect (seg-07 §9; T-702/703).
- R-012 Plugin supply chain (v2+) → untrusted plugins contained by
  capability-gated IPC + install-time permission prompt + crash isolation
  (ADR-001); residual: an installed plugin sees its permitted clips —
  documented in the permission prompt, never silent.
- Open: (1) "zyq" read as Zig (D-010) — trivial confirm. (2) RESOLVED
  (D-021): named Clipmonger, `APP_IDENTITY` = `clipmonger`. (3) Colleague macOS
  availability timing for v2 planning. (4) Claude-fold judgments
  CONFIRMED at acceptance 2026-10-16: update-check default ON +
  first-run notice (M-10), Win ARM64 OUT to v3+ (M-10), pre-46 =
  degraded-with-note (H-13), popup centering/activation per seg-09
  (M-16).

## Sources

ArsClip (behavior reference, no code reuse): http://www.joejoesoft.com/vcms/97 ,
http://www.joejoesoft.com/vcms/120/ , http://www.joejoesoft.com/vcms/151/ ,
http://www.joejoesoft.com/vcms/146/ , http://www.joejoesoft.com/vcms/156/ ,
http://www.joejoesoft.com/vcms/173/
Platforms/protocols: https://www.freedesktop.org/wiki/ClipboardManager/ ,
https://wayland.app/protocols/ext-data-control-v1 ,
https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html ,
https://release.gnome.org/48/ ,
https://gitlab.gnome.org/GNOME/xdg-desktop-portal-gnome/-/raw/main/NEWS ,
https://gitlab.gnome.org/GNOME/mutter/-/raw/main/NEWS ,
https://gitlab.gnome.org/GNOME/gnome-shell/-/raw/main/src/st/st-clipboard.c ,
https://xorg.freedesktop.org/archive/X11R7.7/doc/man/man1/Xvfb.1.xhtml ,
https://developer.android.com/about/versions/10/privacy/changes ,
https://docs.expo.dev/versions/latest/sdk/clipboard/ ,
https://doc.rust-lang.org/rustc/platform-support.html
Microsoft Learn (all under https://learn.microsoft.com/en-us/windows/):
/win32/api/winuser/nf-winuser-addclipboardformatlistener ,
/win32/api/winuser/nf-winuser-sendinput ,
/win32/api/winuser/nf-winuser-registerhotkey ,
/win32/api/shellapi/nf-shellapi-shell_notifyiconw ,
/win32/api/shellapi/nf-shellapi-shellexecuteexw ,
/win32/api/shellapi/nf-shellapi-shellexecutea ,
/win32/secauthz/mandatory-integrity-control ,
/security/application-security/application-control/user-account-control/how-it-works
Crates (licenses via https://crates.io/api/v1/crates/<name>): serde,
serde_json, tokio, uuid, thiserror, clap, toml, directories, sha2, image,
x11rb, wl-clipboard-rs, enigo, snow, mdns-sd, age, notify, cargo-packager.
READMEs/sources (raw.githubusercontent unless noted): hluk/CopyQ
src/platform/{platform.cmake,platformclipboard.h,win/winplatformclipboard.cpp,
x11/x11platform.h,mac/{macclipboard.h,macclipboard.mm,macplatform.mm}} ,
1Password/arboard/{README.md,Cargo.toml,src/lib.rs,src/platform/osx.rs} ,
tauri-apps/tray-icon/dev/README.md , tauri-apps/global-hotkey/dev/README.md ,
golang-design/clipboard/{README.md,LICENSE} (master) ,
bugaevc/wl-clipboard README , sentriz/cliphist cliphist.go ,
oae/gnome-shell-pano README , YaLTeR/wl-clipboard-rs README ,
emilk/egui{,/crates/eframe}/README.md , iced-rs/iced README ,
slint-ui/slint/{README.md,LICENSE.md} , tauri-apps/tauri/dev/README.md ,
fyne-io/fyne README , getlantern/systray README ,
DelSkayn/rquickjs README , boa-dev/boa README ,
rusqlite/rusqlite README , launchbadge/sqlx README ,
enigo-rs/enigo/{README.md,Permissions.md} ,
rust-windowing/winit README.
Docs: https://docs.rs/x11rb , https://docs.rs/eframe/latest/eframe/struct.NativeOptions.html ,
https://docs.rs/egui/latest/egui/viewport/struct.ViewportBuilder.html ,
https://docs.github.com/en/repositories/releasing-projects-on-github/about-releases ,
https://docs.flatpak.org/en/latest/introduction.html
Review-fold verification: https://www.sqlite.org/pragma.html ,
https://www.sqlite.org/useovernet.html ,
https://crates.io/api/v1/crates/ed25519-dalek ,
https://release.gnome.org/51/
