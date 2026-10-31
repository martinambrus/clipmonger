# Linux Wayland backend (crate `backend-wayland`, v1)

Scope: NATIVE Wayland clients. X11 clients in the same session stay on
`backend-x11` (§10 runs both and merges). v1.

## 1. Capture — `ClipboardMonitor` via wl-clipboard-rs

- Dependency: wl-clipboard-rs 0.9.4 (MIT OR Apache-2.0, verified), built
  for window-less clipboard managers over `ext-data-control` OR
  `wlr-data-control` (upstream README, inspected).
- `watch::Watcher` (Regular + Primary): `Changed{mime_types, offer}` →
  capture event; `Cleared` → note; `receive(mime)` per wanted type.
  Primary needs ext-any-version or wlr-v2 (upstream); probe with
  `is_primary_selection_supported()` and set `primary_selection` cap.
- `CLIPBOARD_STATE=sensitive|clear` semantics (cliphist/wl-clipboard
  pattern): the PRIVACY signal is an OFFERED `x-kde-passwordManagerHint`
  MIME — check offered types in every watcher event (man-verified:
  wl-clipboard exposes it as `sensitive`); `CLIPBOARD_STATE` is
  wl-clipboard's env convention only, not a protocol signal. Hint
  present → store nothing (F-104). `nil`/`clear` (explicitly emptied) →
  update current-state to empty, NO history mutation (findings 20, H-03).
- Multi-seat: default `Seat::Unspecified`; per-seat watchers = task §11.5.

## 2. Transfer — `ClipboardTransfer` via wl-clipboard-rs

- `copy`/`get_contents` per seg-03 §2 Wayland column. Ownership lifetimes
  (gotcha found during the arboard audit; dep since dropped, C-01): the daemon is long-lived, so serving paste
  requests is natural; document the lifetime rule in code comments.

## 3. Protocol probing + compositor matrix

Probe order at backend start (and on `caps.refresh`): ext-data-control →
wlr-data-control → none. Our own `app-cli caps` output IS the probe
record (no external tooling needed).

| Compositor | Expected | Status |
|---|---|---|
| KDE Plasma (recent) | ext-data-control | CLAIMED (golang-design README) → VERIFY on target |
| Sway / Hyprland / wlroots family | wlr-data-control | VERIFY via nested-compositor smoke (T-604, §11.6) |
| GNOME 49+ | ext-data-control? (single ?) | UNVERIFIED (Mutter NEWS 48–51 silent) → probe decides; extension is the guarantee (seg-07) |
| GNOME ≤48 | none out-of-process | ASSUMED → extension path (seg-07); probe confirms |
| Unknown | probe decides | honest caps either way |

## 4. Hotkeys — portal-first fallback chain

1. **GlobalShortcuts portal** (docs v2 interface, extracted): app creates
   session (`CreateSession`), binds (`BindShortcuts` — NOTE: portal shows
   a system dialog letting the USER configure/confirm shortcuts; first
   bind is interactive by design; UI MUST forewarn the system dialog (seg-09 §3)), then receives `Activated`/`Deactivated`
   signals. D-Bus via a maintained Rust binding (verify crate at
   implementation — STANDALONE selection, seg-06 §11.2 (review L-01;
  seg-02 §13.3 is resolved to tokio built-ins).
   Backend support: GNOME 48+ CONFIRMED (release notes + portal NEWS);
   KDE/wlroots = VERIFY at implementation (task §11.3).
2. **Compositor-native bindings** (fallback): recipes invoking
   `app-cli show-popup` / `exec-permanent` (Sway `bindsym`, KDE custom
   shortcuts, Hyprland `bind` — document per compositor, QA-owned).
3. **GNOME extension keybindings** (seg-07) where neither applies.

> Amended by ADR-004 (D-026, 2026-10-30): portal `Activated` triggers the
> popup through the daemon (`ui.show_popup` push); presses less than
> 300 ms apart (portal and extension reporting one chord) count once. The
> chord is bound once per daemon run (ADR-004 open item 2).

## 5. Tray / autostart

Tray = seg-09 §7 menu on the single KSNI-preferring Linux build
(seg-05 §5, review M-01); autostart = seg-05 §6 `.desktop`.
GNOME note: stock GNOME has no tray host; document the AppIndicator
extension as the supported route (standard practice).

## 6. Foreground app (AutoSwitch, v2)

No standard Wayland API by design. Candidates: `ext-foreign-toplevel-
list-v1` (name seen on wayland.app staging; VERIFY API at implementation),
compositor-private protocols (verify each), GNOME extension feed (seg-07,
full in-Shell access). Else AutoSwitch reports `Unavailable` on Wayland.

## 7. Paste spike — HIGHEST RISK (seg-00 item 4)

v1 MUST NOT deadlock on this: the honest fallback is **clipboard-write +
toast "press Ctrl+V"** (works everywhere, zero privilege). The spike then
pursues real injection per compositor, time-boxed (~2–3 agent-days), exit
criteria = working PoC + permission-UX documented per target:

- (i) virtual-keyboard Wayland protocol (privileged; grant path VERIFY);
- (ii) RemoteDesktop portal (per-session user consent; UX cost; VERIFY);
- (iii) libei (enigo ships it EXPERIMENTAL incl. a tokio feature —
  re-evaluate maturity at spike time);
- (iv) GNOME extension-assisted injection (seg-07 §4; VERIFY API);
- (v) external helper in `wtype`/`ydotool` style (compositor permission
  setup; last resort).

Fallback truth table (review finding 21 — "works everywhere" retracted):
data-control available → daemon write + toast; GNOME extension available
→ extension-assisted write + toast; focused-UI regular data-device path
ONLY if explicitly implemented + proven; otherwise paste `Unavailable`
with a clear manual-copy action. Spike MUST complete before v1 ships
Wayland paste beyond the fallback.
Results land in this segment + seg-11 QA scripts.

## 8. XWayland coexistence (dual-backend merge; T-605)

When `DISPLAY` and `WAYLAND_DISPLAY` both work: run `backend-x11` AND
`backend-wayland` monitors concurrently; daemon merges by dedupe key
(seg-03 §8) with origin tags (`x11`/`wayland`). Rationale: each backend primarily sees its own clients — but compositors
MAY bridge X↔Wayland selections, so both monitors can observe bridged
copies; dedupe + suppression absorb the duplicates, ordering/fidelity
may differ (documented, review M-13). GNOME 50+ ships no X session
(Register-verified), so backend-x11 there covers XWayland apps only.
Tested by T-605.

## 9. Caps mapping

Full: monitor+transfer (`Full`), hotkey per §4 outcome, paste per §7
outcome (`Full` | `Degraded (write-only fallback)`), tray/autostart as
X11. Refusing compositors: monitor `Unavailable` + extension hint (GNOME)
or honest note (other).

## 10. Segment tests (T-6xx)

- T-601 `[unit]` probe state machine (ext/wlr/none × primary yes/no).
- T-602 `[unit]` portal Bind/Activated response parsing + session rebind.
- T-603 `[unit]` X11+Wayland merge-dedupe incl. origin tags.
- T-604 `[integration]` nested wlroots-based compositor (headless; verify
  package at implementation): capture→store→readback all MIMEs;
  password-hint skip + nil/clear-as-empty honored (H-03);
  kill-owner persistence.
- T-605 `[integration]` dual-BACKEND merge (Xvfb + nested Wayland;
  renamed from dual-monitor, review M-13).
- T-606 `[manual-qa]` per-compositor matrix (KDE, Sway, Hyprland, GNOME
  48+ VM): portal dialog UX, bind persistence across reboot, caps golden
  files. QA-LEAD signoff.
- T-607 `[manual-qa]` spike acceptance per §7 exit criteria.
- T-608 `[integration]` no-data-control truth table (write path present/
  absent × extension present/absent → write+toast vs Unavailable).
- T-609 `[integration]` primary-selection policy (default off; debounce +
  min-length when on — review H-11).

## 11. Verification tasks

1. wl-clipboard-rs 0.9 API shape at implementation (watch/copy/paste).
2. D-Bus crate selection (STANDALONE task, review L-01/C-07):
   maintained MIT/Apache binding for portal calls on all three sessions.
3. KDE + wlroots GlobalShortcuts support levels (fallback recipes).
4. ext-foreign-toplevel-list API (v2 AutoSwitch input).
5. Multi-seat watcher behavior.
6. Nested-compositor harness choice for T-604/605.
