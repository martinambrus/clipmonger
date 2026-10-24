# Spec index — cross-platform ArsClip successor (working title: `arsclip-x`)

Working title only; the user MAY rename the project at any time before v1
without affecting this spec (only identifiers/paths change).

## 1. How to use this spec

- **Implementers (human or AI agent)**: read `seg-00` (this file),
  `seg-01-product-scope.md`, then `seg-02-architecture.md`, then ONLY the
  segment(s) for your work unit. Do not invent scope, interfaces, or
  verification: if your segment does not define something you need, stop and
  surface it as a spec gap instead of guessing.
- **QA (human lead)**: `seg-11-testing-qa.md` is the entry point; every
  feature carries a stable ID (`F-xxx`) and every test a stable ID (`T-xxx`)
  so findings can reference exact items.
- **Review order**: segments are written and reviewed step by step in the
  order listed below. A segment is final only after user review.

## 2. Segment map

Status = acceptance state (`Final` = user-accepted 2026-10-16 per
plan gate 0.8; post-freeze corrections use short ADRs per §5, never
silent edits — review L-04).

| File | Content | Status |
|---|---|---|
| `seg-00-index.md` | This index: decisions, reuse map, conventions, glossary | Final |
| `seg-01-product-scope.md` | Full ArsClip-parity inventory, v1/v2/v3 milestones, non-goals | Final |
| `seg-02-architecture.md` | Crate layout, core/backend traits, processes, IPC, extension | Final |
| `seg-03-clip-model-storage.md` | Clip model, formats, SQLite schema, config, macros/JS engine | Final |
| `seg-04-windows-backend.md` | Monitor/paste/hotkey/tray/autostart on Windows + tests | Final |
| `seg-05-linux-x11-backend.md` | X11 monitor/paste/hotkey/tray/autostart + tests | Final |
| `seg-06-linux-wayland-backend.md` | Data-control probing, portals, per-compositor matrix, paste spike | Final |
| `seg-07-gnome-extension.md` | Shell extension design, version matrix, IPC, packaging | Final |
| `seg-08-macos-backend.md` | Monitor/paste/hotkey/tray/permissions on macOS + tests | Final |
| `seg-09-ui-ux.md` | Popup, config window, clipboard bar, tray menu, keyboard map | Final |
| `seg-10-sync-sharing.md` | LAN sync, cloud/shared-folder, import/export | Final |
| `seg-11-testing-qa.md` | Unit/integration/e2e strategy, per-area tests, QA checklists, CI | Final |
| `seg-12-build-release.md` | Cargo workspace, cross-compile, packaging, versioning | Final |
| `plan.md` | Canonical plan (goal, approach, steps, validation, risks, sources) | Final |

## 3. Decision log

| ID | Decision | Status | Evidence |
|---|---|---|---|
| D-001 | Full-spec scope = ArsClip parity; build in v1/v2/v3 slices | DECIDED (user) | seg-01 |
| D-002 | Linux scope = full everywhere incl. a GNOME Shell-extension component | DECIDED (user, Q1=b) | Mutter NEWS has no data-control entry 48–51; St.Clipboard source inspected |
| D-003 | Language = Rust; Go is reserve; Zig is out for now | DECIDED (delegated to planner; user veto at final review) | Inspected: tray-icon, global-hotkey, egui/eframe, wl-clipboard-rs, golang-design/clipboard, systray, Fyne; Zig pieces exist but no maintained story |
| D-004 | UI = egui/eframe primary; Slint documented alternative; Tauri and iced out | DECIDED (delegated to planner; user veto at final review) | egui: pure Rust, eframe native Win/Mac/Linux; Slint permitted but would GPL-encumber MIT distribution; smaller agent corpus anyway; Tauri too heavy for a resident daemon; iced self-describes as experimental |
| D-005 | Architecture = shared core + per-OS backends (CopyQ pattern) | DECIDED (delegated to planner; user veto at final review) | CopyQ `PlatformClipboard` + per-OS backends inspected |
| D-006 | Reuse-first: verified-license dependency > port > reinvent; GPL projects are behavior references only, no GPL code ingestion | DECIDED (user policy) | License table below |
| D-007 | Project license = MIT | DECIDED (user) | Compatible with every reuse-map dependency (all MIT/Apache-2.0); no GPL ingestion per N-003 |
| D-008 | ArsClip source code is NEVER reused (freeware, non-profit-terms source); behavior reference only | DECIDED | ArsClip license terms as published |
| D-009 | Roles: user = architecture oversight + QA lead; agents = implementation | DECIDED (user) | — |
| D-011 | Windows: full paste coverage via ON-DEMAND elevated helper (Total Commander-style). Daemon always runs standard; helper spawns (UAC) only when the paste/macro target is an elevated window, lives for the app run, re-prompts after restart/helper death. | DECIDED (user, Q1) | seg-04 |
| D-012 | Back-compat (Q3a, AMENDED by review finding 24). Rust floors: Win10+/Server 2016+; macOS 11.0+ ARM64 / 10.12+ Intel; Linux kernel 3.2+ / glibc 2.17+. Retro out (would need abandoning Rust std). APP floor (what we ship+test): Win/macOS per above; Linux = oldest TESTED LTS distro (its glibc, asserted in CI per seg-12 §2) — NOT a 2.17 promise, since LTS images ship newer glibc (24.04 verified 2.39). glibc-2.17 sysroot = future track on user demand. GUI/input crates get floor-verification tasks; plan takes the highest verified line. | DECIDED (user, Q3) | Rust platform-support table (inspected); floors matrix in seg-12 |
| D-013 | GNOME Shell extension IN v1 (Q4b). v1 = full function on GNOME incl. the user's GNOME 46 box; extension is a v1 product surface (own language/version matrix/packaging). seg-01/seg-02 milestones updated. | DECIDED (user, Q4) | seg-06, seg-07 |
| D-014 | Config UI = ArsClip-maximalist (Q6b). EVERY setting in the config window from the start (tab per area); no file-only settings except env overrides. Raises seg-09 scope + UI QA burden (carried in seg-11). CLARIFIED (review finding 39): every setting for an IMPLEMENTED feature has UI; future-feature controls appear only when that feature lands (no dead UI). | DECIDED (user, Q6) | seg-09 |
| D-015 | Sync = (b): v3a LAN (code pairing, E2E, no accounts/infra) then v3b user-provided cloud (encrypted blobs in user's own storage). Hosted cloud out. | DECIDED (user, Q7) | seg-10 |
| D-016 | Mobile = DROPPED altogether (user conditional 2026-10-16, condition met). No background-monitoring path: Android 10+ restricts background reads to default-IME-or-focused-app (doc-verified) — monitoring only by shipping a keyboard = a different product; iOS gates reads per access (UIPasteControl evidence) — no background path at all. seg-10 §6 mobile target removed (pure-sync-core kept as good design). N-001 now reads DROPPED, not deferred. | DECIDED | Android privacy-changes + expo-clipboard docs (inspected); seg-10 §6 |
| D-017 | QA division (Q8): user validates Linux + Windows hands-on; colleague validates macOS (from v2 — v1 has no macOS per finding 2); Ubuntu-version matrix via local VMs (VirtualBox/VMware/other); agents own all automation + CI/VM evidence + suites. | DECIDED (user, Q8) | seg-11 |
| D-018 | Release = $0 pipeline (Q9a). GitHub Releases hosting, unsigned binaries + verified-accurate bypass docs, cargo-packager artifacts, update-check without auto-install, Flathub track v2+. Paid signing revisited on tagged user demand. | DECIDED (user, Q9) | seg-12 |
| D-019 | Codex review fold (2026-10-16): all 52 findings verified against primary sources where upstream-factual, all ADOPTED into seg-00…seg-12 + plan.md (see `codex-clipboard-manager-review-findings.md` + `codex-plan-revised.md` at repo root). Judgment calls CONFIRMED by user (no veto): (a) Linux app floor = oldest tested LTS; (b) local data plaintext-by-default; (c) GNOME v1 matrix 46–51. | ACCEPTED (user 2026-10-16; fold is final) | this row + per-segment edits |
| D-020 | Claude review fold (2026-10-16): 67 findings triaged against the Codex-folded spec; ~40 newly ADOPTED (arboard dropped for direct OS transfer, IPC method table completed, threat model + logging added, GNOME-50 Wayland-only noted, packaging/floor matrices corrected, config key table, hotkey grammar, v1 slice definitions), ~20 already covered (verified point-by-point), 0 rejected, 0 blocking questions. Judgment calls CONFIRMED by user at acceptance 2026-10-16: (a) update-check default ON with first-run notice; (b) Win ARM64 OUT until v3+; (c) Wayland popup = centered window + activation token (no extension-drawn UI). | ACCEPTED (user 2026-10-16; fold is final) | `claude_REVIEW-FINDINGS.md` at repo root + per-segment edits |

### Past decision sessions (Q1–Q9; review L-04)

- Q1 → D-011: Windows elevated helper = on-demand (Total Commander-style).
- Q2 → X11 backend approved as proposed (seg-05).
- Q3 → D-012: back-compat = Rust floors (retro out).
- Q4 → D-013: GNOME extension in v1.
- Q5 → macOS backend scope approved (seg-08, v2).
- Q6 → D-014: maximalist config UI.
- Q7 → D-015: sync = LAN + user-cloud.
- Q8 → D-017: QA division (user Linux/Win, colleague macOS).
- Q9 → D-018: $0 release pipeline.

## 4. Reuse map (all licenses verified from inspected sources)

| Need | Source | License | Mode |
|---|---|---|---|
| Windows clipboard transfer (formats, atomic multi-write) | windows-sys 0.61 + clipboard-win 5.4 | MIT OR Apache-2.0 / BSL-1.0 | DEPENDENCIES (seg-04; direct OS APIs — arboard lacks RTF/custom/atomic, review C-01) |
| macOS clipboard transfer (NSPasteboard) | objc2 0.6 + objc2-app-kit + objc2-foundation 0.3 | MIT / Zlib OR Apache-2.0 OR MIT / MIT | DEPENDENCIES (seg-08; direct AppKit — arboard insufficient, review C-01) |
| Evaluated transfer/monitor alternatives | clipboard-rs 0.3.5 (MIT, maintained) / clipboard-watcher 0.2.1 (Apache-2.0, immature ~210 dl) | — | EVALUATED, NOT adopted (review C-01): atomic multi-format writes unverified; revisit only with proof |
| Wayland copy/paste/watch (ext + wlr data-control) | wl-clipboard-rs 0.9 | MIT OR Apache-2.0 | DEPENDENCY for seg-06 |
| Watch semantics, Sensitive flag, auto backend selection | golang-design/clipboard | MIT | PORT design to Rust (no Go toolchain in build) |
| Tray icon + menu (Win/Mac/Linux incl. pure-D-Bus KSNI) | tray-icon (Tauri) | MIT OR Apache-2.0 | DEPENDENCY |
| Global hotkeys (Win/Mac/X11) | global-hotkey (Tauri) | MIT OR Apache-2.0 | DEPENDENCY; Wayland needs portal/extension/compositor fallbacks (seg-06/07) |
| UI framework | egui/eframe | MIT OR Apache-2.0 | DEPENDENCY |
| Backend-split pattern, monitor/quirks knowledge | CopyQ | GPL-3.0-or-later | REFERENCE ONLY (D-006) |
| Minimal Wayland-history pattern, store/CLI split, caps/dedupe | cliphist | GPL (LICENSE file present) | REFERENCE ONLY (D-006) |
| `wl-paste --watch` capture pattern | wl-clipboard | GPL-3.0-or-later | REFERENCE ONLY (D-006) |
| GNOME-extension approach + maintenance cost | Pano (unmaintained) | GPL | REFERENCE ONLY; cautionary tale for seg-07 scoping |
| Full feature checklist + macro language behavior | ArsClip 5.x | Freeware, non-profit-terms source | BEHAVIOR REFERENCE ONLY (D-008) |
| SQLite store (bundled + backup API) | rusqlite 0.40 | MIT | DEPENDENCY (seg-03; sqlx rejected: async pool overkill) |
| JS macro engine (QuickJS-NG, ES2020) | rquickjs | MIT | DEPENDENCY for v2 macrolib (seg-03; Boa rejected: experimental) |
| Serialization / IPC / config / CLI / IDs / errors / hashing / images / dirs / runtime | serde, serde_json, tokio, uuid, thiserror, clap, toml, directories, sha2, image (all latest stable) | ALL MIT OR Apache-2.0 (uuid: Apache-2.0 OR MIT; tokio: MIT) | DEPENDENCIES (seg-02 §3; licenses via crates.io API) |
| Input simulation (Win/macOS/X11; Wayland experimental = NOT used) | enigo 0.6 | MIT | DEPENDENCY (seg-04/05/08 paste engines; adopted under D-006) |
| E2E session crypto (Noise XX + ChaChaPoly) | snow 0.10 | Apache-2.0 OR MIT | DEPENDENCY for v3a (seg-10; license via crates.io API) |
| LAN discovery (mDNS, no runtime dep) | mdns-sd 0.21 | Apache-2.0 OR MIT | DEPENDENCY for v3a (seg-10; license via crates.io API) |
| File-blob encryption for user-cloud | age 0.12 | MIT OR Apache-2.0 | DEPENDENCY for v3b (seg-10; self-labeled BETA — re-evaluate at v3, fallback spec'd) |
| Folder watching (user-cloud dir) | notify 8.x | CC0-1.0 | DEPENDENCY for v3b (seg-10; license via crates.io API) |
| Manifest signing (Ed25519) | ed25519-dalek 3.0 | BSD-3-Clause | DEPENDENCY for v3b (seg-10; license via crates.io API) |

Versions above are "verified ≥" minimums (seen-good at writing, review
finding 43): exact pins live in `Cargo.lock`/toolchain files at
implementation, and the seg-11 §5 release gate re-checks licenses.

## 5. Conventions

- RFC 2119 keywords: MUST / MUST NOT / SHOULD / MAY.
- IDs: `F-xxx` features (seg-01), `T-xxx` tests (seg-11 + per-segment
  lists), `D-xxx` decisions (this file), `R-xxx` risks (plan.md).
- Test tags: `[unit]` runs headless in CI; `[integration]` needs a real
  OS service or display server; `[e2e]` drives the built app;
  `[manual-qa]` is a human checklist step for the QA lead.
- Every segment ends with: its test list, its verification commands, and
  gaps/conditionals (never silently guessed).
- Post-freeze architecture changes need a short ADR (decision + reason +
  affected segments + migration/test impact) instead of silent edits
  (review finding 4; gate defined in plan.md).
- Identity (review M-17): all wire/persistence/bundle names (IPC methods,
  mDNS service, bundle IDs, profile dirs, extension UUID) derive from ONE
  `APP_IDENTITY` constant; renaming = changing the constant + the
  documented migration (profile copy, no re-pair). The working title may
  change pre-v1; post-v1 renames need an ADR. Affiliation note: the final
  name MUST NOT imply ArsClip/JoeJoe endorsement (living product).

## 6. Glossary

- **Clip**: one captured clipboard entry (any format set).
- **History / Popup Clips**: rolling store of recent clips shown in the popup.
- **Permanent Clip**: user-pinned reusable item (paste text, run macro).
- **Macro**: template/automation attached to a Permanent Clip
  (`[VARIABLE]` inserts, keystrokes, `[RUN]` commands, JavaScript).
- **Permanent Group**: named folder of Permanent Clips; `_System` group is
  hotkey-only, never shown on the popup.
- **Popup**: the triggered paste window (ArsClip's main UI).
- **Clipboard Bar**: optional always-on viewer window.
- **Removed Clips**: soft-deleted items (recoverable).
- **Global Clipboard**: ArsClip's sync-across-devices feature; ours is
  re-designed in seg-10.
- **Backend**: one per-OS/per-display-server implementation of the monitor,
  paste, hotkey, tray, and autostart traits.
- **Daemon**: the always-on background process owning monitoring + store.
- **v1/v2/v3**: milestone slices defined in seg-01. The architecture MUST
  cover the full scope from day one; only implementation is sliced.

## 7. Open items (do not block drafting)

### Resolved (review L-04: kept for history, not action)

- R-1 (2026-10-16, probed): Ubuntu 24.04.5 LTS x86_64, GNOME Shell 46.0
  on Wayland, XWayland present. This box CANNOT validate the
  GlobalShortcuts portal (needs GNOME 48+) nor data-control capture on
  GNOME; it validates the X11 backend (via XWayland), degraded
  native-Wayland modes, and the GNOME extension (v1). Second target
  (GNOME 48+, KDE, Sway VM) required — carried into seg-11.
- R-2 (user decision): STAY on 24.04; newer GNOME via agent-driven VMs;
  Docker rejected for compositor e2e; nested wlroots compositors for
  data-control smoke tests.
- R-3: D-007 = MIT, decided (stale "OR Apache" wording removed).
- R-4: D-010 ("zyq") removed as moot — Zig eliminated by D-003.

### Open

1. Working title `arsclip-x` — user MAY rename pre-v1 (rename-safe via
   `APP_IDENTITY`, seg-00 §5).
2. Wayland "paste into active window" mechanism — seg-06 spike + honest
   fallback. Highest-risk validation item.
3. KDE/wlroots GlobalShortcuts-portal support levels — verified during
   seg-06 implementation; fallback chain covers all cases.
4. Full ArsClip macro-variable list — seg-03 verified core + completion
   task against the ArsClip binary's built-in help.
