# UI/UX — popup, config (maximalist), bar, tray, keys (crate `app-ui`)

Stack: egui/eframe 0.37 (docs inspected) + Win32 CPU fallback (unified).
One process, multiple viewports on egui path: root (tray host) + popup +
config + bar (v2). Renderer: `glow` pilot (U-F2), `Win32 GDI` CPU fallback
on Windows when `glow` is unavailable (headless/RustDesk/virtual display —
D-024, `fallback.rs`); `wgpu` deferred (upstream `windows` skew). NO tokio
in `app-ui` (seg-02 §8): a dedicated thread owns IPC, UI polls a channel.
Fallback reuses the same `PopupVm`/`ConfigDoc`/`Placement`/`IPC bridge`
— `ArsClip`-style `Win32 GDI` (`CreateWindowExW`/`GDI`) for `popup` when
`OpenGL` is absent (no `GPU` required; `RDP`/`RustDesk`/`Safe Mode` OK).

## 1. Viewport recipes (verified `ViewportBuilder` surface)

- Popup: `with_decorations(false)`, `with_window_level(TOP)`,
  `with_position(p)`, `with_inner_size` + min/max, `with_resizable(false)`,
  `with_taskbar(false)`, `with_active(true)` (focus on open; F-208
  No-Focus mode inverts this in v2 — feasibility per OS = task §14.6).
  `with_window_level` builder VERIFIED; exact TOP enum path (winit
  re-export?) = VERIFY at implementation (task §14.1).
- Config: decorated, resizable, taskbar icon, remembered geometry
  (`persist_window`, verified field).
- Bar (v2): frameless micro-strip, on-top, edge-docked per config.
- Per-OS quirks (positioning, focus steal prevention, Wayland
  xdg-positioner limits) MUST be recorded in code + manual-QA per OS.

## 2. Popup (F-201–208)

Layout top→bottom: search field (F-204, focused by default, live filter)
→ "Current Clipboard" item → permanent-group tabs (F-301) → clip list
(newest first; F-205 previews: image thumbnails via `image`, file rows
with icons (VERIFY icon source per OS), pinned shortcuts, headers) →
status line (count, caps warnings, e.g. "monitoring paused").
v2: plugins holding `ui.contribute` (ADR-001) may add items in a dedicated
popup section (exact placement frozen at the v2 design-reopen gate).
Behavior: hover-select + click-paste; right-click item menu
(edit/duplicate/pin/delete); Enter pastes selected; Esc dismisses;
Delete key = soft-delete (v1 storage; recovery UI v2 — finding 37);
dismiss-on-focus-loss (configurable); placement per F-207
(cursor/caret/center/program-center; v1 = cursor + center).
Empty/error/offline states MUST all be designed (store unreachable =
offline banner, never a hang). Preview renders images/HTML/RTF +
truncation + item stats (review H-09).

## 3. Triggers (F-202)

v1: global hotkey + tray click. v2: hold-right-click, screen edge/corner,
floating trigger button. Per-OS feasibility gates (VERIFY): Windows low-
level mouse hook; X11 pointer grab/keyboard grab; Wayland = compositor
binding or GNOME-extension event ONLY (no global grabs exist). UI
forewarns the system approval dialog where binding is OS-mediated
(review C-04/H-07).

> Amended by ADR-004 (D-026, 2026-10-30): the daemon owns the popup hotkey
> on every OS and pushes `ui.show_popup`; the UI subscribes once per
> profile (`<profile>/ui.lock`). A press opens the popup for the window it
> was pressed in (on Windows a press while the popup is in front hides
> it); a `popup.open` request (CLI, second launch) while it is open only
> raises it and keeps the target; the tray opens it without a target.

## 4. Config window (F-601/602, D-014 MAXIMALIST)

Tabs: History | Permanent Clips/Macros | Databases | About/Help (+ Plugins
in v2 per ADR-001, + Sync in v3; a tab present-but-disabled until its
feature lands is FORBIDDEN — no dead UI).
v1 Databases tab shows local diagnostics (path/size/schema/health,
compact, backup/restore, open-folder — finding 38); attach/shared
controls arrive in v3. The v2 Plugins tab lists installed plugins with
install-from-folder/import, enable/disable/remove, and per-plugin
permission display (F-802). D-014 means implemented-features-only (finding 39).
RULE: every seg-03 §6 config key and every F-item behavior has a control
here (implemented features only — finding 39); the ONLY file-only
surface is env overrides. Inventory (v1 excerpt;
full matrix in code, one row per key):
History: depth, dedupe window, retention (count/bytes/age), formats,
max bytes, min length, ignore/denylist editor, monitoring toggle+hotkey.
Popup: trigger chords, placement, preview width, dismiss rules, key map
editor (§8), pins/headers toggles. Permanents: group manager, clip
editor (v1 basic; macro UI in v2), per-clip hotkeys. Tray: icon set,
menu toggles. Startup: autostart toggle (+ elevation note on Windows).
Privacy: strict mode, denylist, audit viewer. General (v2 addition):
locale selector (`ui.locale`, ADR-002; hidden while English-only v1).
Advanced: poll intervals,
timeouts, IPC/bit toggles, profile folder + config file openers,
backup-to-ZIP / restore, "reset to defaults" (with confirm).
Import/export covers implemented keys only (D-014).
Chrome: dirty-tracking Apply/Revert, inline validation errors, a
filter-as-you-type box over ALL settings (mandatory for sprawl),
caps matrix (About), log viewer (About). Diagnostics → Who-pasted
shows remote-control input evidence (review H-04; v3 with remote ctrl).

## 5. Permanent editor (F-309, v2)

List (drag-reorder where egui allows, else up/down) + editor: name,
group, hotkey capture, kind switch (Paste/Macro/System), body area with
`[VAR]` insert menu + keystroke palette, JS pane with API reference +
bundled examples, test-run button (dry-run expansion shown, no side
effects), `[RUN]` policy indicator.

## 6. Clipboard bar (F-603, v2)

Optional strip: last N clips as chips, click-to-paste, pause + open-popup
buttons, opacity slider. Dock edges per config; multi-monitor aware.

## 7. Tray menu (all OSs, same items — SOLE tray definition, review M-06)

Show popup / ─ / Pause monitoring (check) / Pause 1h / ─ / Permanent
group → (submenu, v2) / Open config / ─ / About / Quit.
Icon states: normal / paused / attention (elevation needed, sync error,
backend degraded). Tooltip = one-line caps summary.

## 8. Keyboard map (F-203)

Defaults (customizable, conflicts warned): Up/Down/Home/End/PgUp/PgDn
navigate; type = search; Enter paste; Esc dismiss; Ctrl+Enter paste into
(v2 modes); Delete remove; Ctrl+E edit; Ctrl+P pin; Alt+1..9 group tabs;
F5 refresh. Capture widget for rebinds; OS-reserved chords rejected with
reason (per-OS reserved lists = VERIFY at implementation).

## 9. Theming + fonts

egui `Visuals` dark/light + manual toggle + font scale slider. Follow-OS
theme: NO `NativeOptions` field found in inspected docs → VERIFY
detection path at implementation, else manual-only in v1 (acceptable).
Accent color picker. High-contrast = v3.

## 10. Accessibility

Hard requirement: 100% keyboard operable (every mouse action has a key).
Screen readers: eframe AccessKit status = VERIFY (upstream notes it
experimental); record the real level in About → Accessibility; do NOT
claim what isn't proven. Focus-visible styling everywhere.

## 11. i18n design (English-only v1; translations from v2 — ADR-002/D-023)

- String ids (FROZEN v1): ALL user strings via a `strings` module under
  STABLE ids; zero inline literals in views (enforced by T-910). Ids never
  change meaning (rename = new id + ADR note). ICU-style placeholders;
  no sentence concatenation.
- Catalogs (v2): one versioned file per locale (BCP-47), pinned to a
  string-id-set version; format decided at v2 implementation (VERIFY
  against M-11 criteria; requirements: CLDR plurals, RTL-safe layout,
  per-key fallback). English ships in-tree.
- Fallback chain: `ui.locale` → `ui.fallback_locale` → `en`. Missing key
  renders English + bumps a coverage counter (never blank, never crash).
  OS locale seeds `ui.locale` at first run; date/number formatting follows
  the OS locale (mechanism VERIFY at v2).
- Community flow: catalog PRs carry translator, id-set version, coverage %;
  CI checks id-set match + placeholder consistency. No catalog ever blocks
  a release. Expected first: Slovak (`sk`), user-supplied, best-effort.
- Plugin strings (ADR-001): namespaced `<plugin-id>.<key>`, shipped by the
  plugin, same fallback chain.

## 12. UI↔daemon contract

Everything over IPC (seg-02 §9): no direct store/backend access from
views. Destructive ops (delete/purge) require server-confirmed roundtrip
(no optimistic UI). Views handle loading/empty/error/offline; event
subscription drives refresh (no blind polling except 30 s caps check).

## 13. Segment tests (T-9xx)

- T-901 `[unit]` search-filter + settings-filter vectors.
- T-902 `[unit]` keymap parse/conflict/reserved-chord matrix.
- T-903 `[unit]` placement math (multi-monitor rects, cursor/caret/center).
- T-904 `[integration]` UI-behind-fakes: scripted IPC server → view
  states incl. offline/destructive-confirm flows (headless; VERIFY egui
  headless harness — else pure-logic split + rendering to manual-qa).
- T-905 `[manual-qa]` per-OS window matrix: frameless/ontop/focus/
  dismiss/placement on Win11, X11, Wayland×2 compositors, GNOME+ext.
  QA-LEAD signoff.
- T-906 `[manual-qa]` maximalist inventory walk: EVERY control touched
  per release (checklist generated from the §4 matrix). QA-LEAD signoff.
- T-907 `[manual-qa]` keyboard-only full pass (no mouse attached).
- T-908 `[integration]` popup focus interplay (target preserved across
  open/select/dismiss; focus-switch mid-popup → stale-target error, never
  a mis-paste; pairs with seg-02 T-208).
- T-909 `[unit]` settings-coverage: every config key in seg-03 §6
  renders as a control in §4 (review H-05).
- T-910 `[unit]` strings-module conformance: views contain zero inline
  literals; every string id has English text (v1 rule, ADR-002).
- T-911 `[unit]` (v2) locale fallback + plural vectors + catalog
  conformance (id-set match, placeholder consistency; Slovak catalog
  included when shipped).

## 14. Verification tasks

1. `WindowLevel`/TOP enum path + per-OS on-top reliability.
2. OS-theme-follow path (or manual-only v1, documented).
3. egui headless test harness (or logic/render split).
4. File-row icon sources per OS.
5. OS-reserved chord lists (Win/X11/Wayland/GNOME/macOS).
6. F-208 No-Focus feasibility per OS (v2 input).
7. Hold-right-click/edge feasibility per OS (v2 input).
