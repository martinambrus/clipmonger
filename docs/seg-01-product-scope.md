# Product scope — full ArsClip parity + milestone slices

Source inventory: ArsClip official features page, in-published help pages
(Getting Started, Automation/Macros, Permanent Clips Reference, Keystrokes,
Databases, Exporting/Portability), and the 5.x changelogs. Anything below
marked `(gap)` was seen referenced but not fully specified; the owning
segment carries a completion task — implementers MUST NOT invent it.

## 1. Monitoring (capture)

- F-101 Monitor the OS clipboard continuously and store text entries
  (rolling history, configurable depth). Milestone: v1.
- F-102 Monitor non-text formats: pictures, RichText, HTML, Unicode text,
  file copy/cut lists. Milestone: v1 (pictures + Unicode + files; RTF/HTML
  kept as formats with paste fidelity, viewers improve in v2).
- F-103 Per-format filters: which formats are captured, max clip size,
  min length, ignore rules. Milestone: v1 basics, v2 full parity.
- F-104 Honor OS privacy/opt-out signals (Windows exclusion formats,
  macOS concealed-type, Wayland sensitive/clear states). MUST in v1 wherever
  the OS exposes them. Cross-ref: seg-04/05/06/08.
- F-105 Monitoring toggle (ArsClip: Scroll Lock toggles; tray icon shows
  state). Milestone: v1 (hotkey + tray; Scroll Lock where the OS permits).
- F-106 Removed Clips: soft-delete STORAGE from v1 (stable deletion
  records benefit sync/retention), recovery UI in v2, explicit purge
  always (review finding 37).

## 2. Popup (paste window)

- F-201 Triggered popup listing recent clips for one-keypress paste.
  Milestone: v1.
- F-202 Triggers: global hotkey; hold-right-click; trigger window/edge;
  tray click. Milestone: v1 hotkey + tray; v2 all trigger types per OS
  feasibility (seg-04–09 define per-OS support; Wayland restrictions apply).
- F-203 Full keyboard navigation + user-definable keystrokes/clicks
  (navigation, selection, in-popup clip editing). Milestone: v1 core keys,
  v2 full custom map.
- F-204 Search/filter popup items (ArsClip: find in popup). Milestone: v1.
- F-205 Image/file previews in popup; "Current Clipboard" item; pinned
  items with shortcut keys; clip headers. Milestone: v1 previews + current
  item; v2 pins/headers parity.
- F-206 Paste modes: paste as-is; "Paste Using > Mimic" (match surrounding
  formatting); "Paste And Select"; paste-into-field variants. Milestone:
  v2 (needs per-OS paste-simulation research, esp. Wayland — see open
  item 4 in seg-00).
- F-207 Popup placement options incl. cursor, caret, program-center
  follow modes. Milestone: v2 (v1: cursor + center on Win/X11;
  compositor-decided centered window on Wayland/GNOME — review C-04).
- F-208 No-Focus mode (popup usable without stealing focus). Milestone: v2.

## 3. Permanent Clips, groups, macros

- F-301 Permanent Clips always shown on the popup; multiple named groups;
  shared permanent groups. Milestone: v1 basic groups; v2 sharing.
- F-302 Per-clip global hotkey (run without opening popup). Milestone: v1.
- F-303 `_System` group: hotkey-only command clips, never shown on popup.
  Milestone: v2.
- F-304 AutoSwitch: remember last-used group per program, switch on popup
  open. Milestone: v2 (needs active-window detection per OS;
  timing/activation semantics per seg-02 §6, review L-01).
- F-305 Macro inserts: `[POPUPITEM=n]`, `[CLIPBRDCURRENT]`/clipboard,
  date/time, `(gap)` full variable list incl. `CLIPBOARDFIND`. Milestone:
  v2 (verified core in seg-03 + completion task).
- F-306 Keystroke simulation in macros (`[TAB]`, nav/edit keys, custom
  combinations). Milestone: v2 (per-OS feasibility gates; Wayland hardest).
- F-307 `[RUN]...[/ENDRUN]` execute shell commands/URLs/programs;
  `[COPYWAIT=ms]` copy-and-wait. Milestone: v2.
- F-308 JavaScript macros (ArsClip: `deletePopupClip(int)` et al.).
  Milestone: v2 (engine choice in seg-03).
- F-309 Macro editor window with online/living code examples. Milestone: v2.

## 4. Formats, storage, databases

- F-401 Clip store in SQLite (ArsClip v5: `clips[Machine].dat`,
  portable `clips.dat` on removable drives). MUST keep SQLite. v1.
- F-402 Integrity check + repair path; version check at startup with
  clear error (never silent corruption). Milestone: v1.
- F-403 Databases tab: multiple/attached LOCAL databases, one-shot
  import from other installs. Shared folders carry sync/export OBJECTS,
  never a live shared DB (SQLite forbids network/live-shared use —
  review finding 15). Milestone: v3 (v1: single local DB + diagnostics
  per finding 38).
- F-404 Dedupe (configurable window), retention caps (count/size/age),
  DB compact. Milestone: v1 (from cliphist-proven semantics).

## 5. Hotkeys, tray, platform integration

- F-501 Global hotkeys for popup + per-clip activation (user-definable).
  Milestone: v1 on Win/X11 + full Wayland chain (portal→compositor→
  extension, D-013); macOS hotkeys arrive with the v2 macOS backend
  (review finding 2: v1 has no macOS release).
- F-502 Tray icon + context menu; monitoring-state indication.
  Milestone: v1 Win/Linux; macOS v2 (review C-08).
- F-503 Autostart on login per OS. Milestone: v1 Win/Linux; macOS v2.
- F-504 Portable mode (run from USB, config+DB alongside binary).
  Milestone: v1 Win/Linux (portable+elevated combined = paste-to-admin
  Unavailable, review C-03); macOS n/a (bundle install).
- F-505 Windows Jumplist support (or documented equivalent). v3.
- F-506 Native notifications where useful (capture errors, sync events).
  Milestone: v2.

## 6. Configuration

- F-601 Tabbed config: History, Permanents, Databases, General,
  About/Help (5 tabs — General holds tray/startup/updates/advanced;
  documented deviation from ArsClip's 4, review M-04). Milestone: v1
  (Databases tab = useful local diagnostics:
  path/size/schema/health, compact, backup/restore, open-folder;
  attach/shared controls in v3 — review finding 38).
- F-602 ArsClip-level configurability: every behavior above has a setting;
  import/export settings; backup-to-ZIP. Milestone: v2 parity (v1 covers
  v1 features + backup/restore).
- F-603 Optional Clipboard Bar viewer window. Milestone: v2.

## 7. Sync, sharing, portability

- F-701 LAN sync between this app's instances ("Global Clipboard"
  re-designed, desktop-to-desktop). Mobile DROPPED per D-016.
  Milestone: v3 (v3a LAN, v3b user-cloud).
- F-702 Sync THROUGH user cloud/shared folders (encrypted immutable
  objects; each device keeps its own local DB — never a live shared DB,
  review finding 15). Milestone: v3 (v3b).
- F-703 Export/import clips, groups, settings; move group folders.
  Milestone: v2 basic, v3 full.
- F-704 Import from the user's own ArsClip data (SQLite clips +
  settings, best-effort; DATA interop, not code reuse — D-008
  unaffected). Milestone: v2 (review O-06).

## 8. Plugins and localization

- F-801 Out-of-process plugin host: manifest (`plugins/<id>/plugin.toml`),
  capability-gated IPC subset, permission prompt, per-plugin isolation
  (ADR-001, D-022). Milestone: v1 freezes reservations (directory, manifest
  schema, method/permission names — no host); v2 ships the host + sample
  plugin + docs.
- F-802 Plugin install/manage UX (Notepad++-inspired): folder drop +
  config-UI import, enable/disable/remove, permission display. Curated
  plugin list = v3+ evaluation under the v3 design-reopen gate (must fit
  the D-018 $0 pipeline). Milestone: v2 (list in v3+).
- F-803 Locale selection + community translation catalogs (ADR-002, D-023):
  stable string ids, versioned per-locale files, in-app selector, PR-based
  community flow. Milestone: v1 English-only (ids frozen, T-910 enforced);
  v2 infrastructure + first community catalogs (no committed language;
  Slovak expected first, user-supplied, non-blocking).

## 9. Non-goals (explicit)

- N-001 Mobile clients: DROPPED per D-016 (no background-monitoring path
  on modern mobile OSs; not deferred, removed).
- N-002 Reusing ArsClip source code (D-008).
- N-003 Ingesting any GPL-licensed code (D-006); clean dep/port/reference
  separation per the seg-00 reuse map.
- N-004 Browser/IDE plugins; the app is a desktop daemon + windows + CLI.
  (This non-goal is about building plugins FOR browsers/IDEs; Clipmonger's
  OWN plugin system is F-801/802.)

## 10. Milestone exit criteria

- **v1 (prove the foundation cross-platform)**: F-101/102(part)/103(basic)/
  104/105, F-201–205(part), F-301(basic)/302, F-401/402/404, F-501(full Wayland
  chain incl. GNOME extension, D-013)/502/503/504, F-601(v1 scope) — on Windows,
  X11, Wayland-with-data-control, and GNOME 46–51; full `[unit]` + `[integration]`
  suites green; `[e2e]` smoke on all four; QA checklist signed off.
v1 slice definitions (review L-05): F-102(part) = pictures + Unicode +
files monitored; RTF/HTML captured with paste fidelity, viewers improve
in v2. F-103(basic) = formats + max bytes + min length + denylist (no
per-app rules until v2). F-201–205(part) = popup + search + previews +
current-item (pins/headers/paste-modes in v2). F-301(basic) = local
named groups + per-clip hotkeys (group sharing in v2).
F-801/802/803 in v1 = reservations ONLY (§8: names/schemas/ids frozen, no
host, no translations, no Plugins tab, no locale selector).
- **v2 (parity depth)**: macOS backend, macros/JS,
  AutoSwitch, clipboard bar, paste modes, full key customisation, export,
  plugin host + install/manage UX (F-801/802), i18n infrastructure + first
  community catalogs (F-803; Slovak expected, user-supplied, non-blocking).
- **v3 (sharing)**: sync (LAN + user-cloud objects), Jumplist-class
  extras, import/export full, curated plugin-list evaluation (F-802).

Implementation order inside v1: core store+config → UI shell pilot
(popup/tray/lifecycle against fakes+real IPC, so paste-target behavior is
validated before backend paste lands — review) → Windows backend →
X11 backend → Wayland backend + GNOME extension → hardening.
Architecture (seg-02) MUST cover v1–v3 on day one; only code lands sliced.
