# GNOME Shell extension (dir `gnome-ext/`, v1 per D-013)

Role: in-Shell RELAY for GNOME sessions where out-of-process capture/
hotkeys are refused (notably GNOME ≤48 incl. the user's GNOME 46 box).
It NEVER owns history (seg-02 §1): capture events + hotkey triggers flow
daemon-ward; paste-assist + config flow Shell-ward. On disconnect the
daemon flips GNOME caps without crashing.

## 1. Stack

- TypeScript source (Pano precedent: typed, agent-friendly), compiled JS
  shipped; GJS runtime in-Shell. Build: standard TS toolchain (verify
  ts-for-gir flavor at implementation).
- Imports (Shell APIs) MUST be verified per supported Shell version at
  implementation; record exact module paths in this segment then.

## 2. Capture relay

- Text: `St.Clipboard` (source inspected: `get_default()`, async
  get/set text over `MetaSelection`; mimetypes text/plain/UTF8_STRING…).
- Rich formats: `MetaSelection` transfers directly (Pano proves
  images/files feasible; VERIFY exact calls at implementation).
- Change detection: inspected `StClipboard` head shows NO change signal →
  default = 500 ms interval poll + diff; IF `MetaSelection` exposes an
  owner-change signal on our targets (VERIFY), use it and keep polling
  as fallback.
- Privacy: no OS signal → config denylist by app-id (same list as X11,
  seg-05 §1); sensitive items never leave the Shell process. ALSO check
  the `x-kde-passwordManagerHint` MIME through the transfer API where
  exposed (review H-03); denylist remains the guaranteed supplement.
- **Phase-0 PoC BLOCKER (review finding 22):** before v1 scope freeze,
  prove on GNOME 46 AND current stable (51): change detection for
  text-only AND image/file-only changes, enumerate/read text+image+file
  offers, privacy-denylist enforcement, and Shell overhead measurement.
  If any format cannot be proven, v1 GNOME format scope is revised
  EXPLICITLY before acceptance (no silent gaps).

## 3. Hotkeys

- Registered through the Shell keybinding API (VERIFY imports per
  version): popup trigger + per-clip chords mirror daemon config (pushed
  on `config.changed`). Shell-level conflicts → report to daemon → caps
  note + UI hint.

## 4. Paste-assist (with seg-06 §7 spike on GNOME)

- In-Shell injection path for popup-paste/macros (VERIFY API at
  implementation — part of the spike). Until proven: same honest
  fallback as seg-06 §7 (clipboard-write + toast).

## 5. Foreground info (AutoSwitch, v2 feed)

- Full in-Shell access to active window app-id/title → push to daemon
  on change (throttled). This is the ONLY Wayland/GNOME AutoSwitch feed.

## 6. IPC (seg-02 §9 wire format, GJS side)

- JSONL over Gio Unix socket to the daemon socket path; `hello`+token
  identical to Rust clients; heartbeat 15 s; on Shell restart (`enable()`
  re-runs) re-register + resync config. Relay chunking (review M-14):
extension→daemon payloads >256 KiB (images!) travel as `blob.put`
chunks per seg-02 §9 — never one giant frame. Socket-path discovery via the seg-02 §1 rendezvous rule (review
  finding 40): read `<config>/clipmonger/rendezvous.json` (default profile
  socket + protocol version; token from the 0600 token file), fall back
  to `--profile` registry for named profiles, re-resolve on every
  connect failure. NEVER bake a mutable absolute socket path into the
  extension without this update path (portable/multi-profile safe).

## 7. Version matrix

- v1 matrix: GNOME 46 through current stable 51, EVERY major
  (46,47,48,49,50,51 — review finding 23; 51 "A Coruña" verified current
  2026-09-16). Our box (46) anchors the old end. After v1 ships, define
  an EOL policy (drop oldest only with 1-release notice + migration note).
  If the 6-major matrix proves too expensive during Phase 0, the ONLY
  permitted reduction is an explicit "46 + latest three" restatement —
  never a silent shrink. Shell <46 explicitly OUT (documented, review
H-13): pre-46 boxes (e.g. Ubuntu 22.04) get X11/XWayland capture where
available and explicitly degraded Wayland-native capture with a
documented note — never a silent gap.
- CI (seg-11): one VM per supported major for install/enable/smoke.

## 8. Packaging (v1: local install; EGO track from v2)

- v1: `install-extension.sh` (copy tree to
  `~/.local/share/gnome-shell/extensions/<uuid>/`, enable via
  `gnome-extensions enable`, restart hint) + docs. No review gate → fast
  iteration while the protocol churns.
- EGO (extensions.gnome.org) submission starts v2 (review lead time;
  submission/packaging rules = VERIFY task). Extension UUID shape `name@domain`
  under the final project name Clipmonger (D-021; stem `clipmonger@…`,
  domain part confirmed against current GNOME extension docs at
  implementation — review M-14).
- First-run UX: daemon detects GNOME + missing protocols → offers
  one-click install attempt with logout/restart notice; prefs page
  (standard extension prefs; GTK4/libadwaita flavor VERIFY).

## 9. Failure modes (all MUST be tested, §10)

- User disables extension → daemon caps `Degraded` + re-enable hint.
- `metadata.json` shell-version mismatch → clear error naming versions.
- Shell crash-loop guard (GNOME auto-disables extensions after crashes):
  document recovery; daemon MUST survive the silence.
- Daemon unreachable (not started): extension idles + retries with
  backoff; shows status in its indicator (if any).
- Shell stalls / session blast radius (review M-14): relay payloads are
  size-capped, all IPC async, polling backs off under load; a wedged
  relay MUST degrade to capture-paused + error (plan R-011), never hang
  the session.

## 10. Segment tests (T-7xx)

- T-701 `[unit]` (TS): message codec roundtrips, relay state machine,
  poll-diff logic (harness: standard TS runner, VERIFY at impl).
- T-702 `[integration]` GNOME 46 box (ours): install → enable → capture
  text+image+files → hotkey fires → disable → caps flip → re-enable.
- T-703 `[integration]` Shell restart (`--replace` in nested/Xephyr-safe
  manner where possible, else documented manual): re-register + resync.
- T-704 `[manual-qa]` VM matrix per supported major: install, prefs,
  version-mismatch message, crash-guard recovery doc check. QA-LEAD signoff.
- T-705 `[integration]` image-only + file-only change detection (no text
  change present) + privacy-denylist enforcement + password-hint check
  (H-03) + Shell overhead sample.

## 11. Verification tasks (close during v1 implementation)

1. Exact Shell imports per version (St, Meta, keybindings, Gio sockets).
2. `MetaSelection` multi-format transfer calls.
3. Owner-change signal availability (else poll-only, documented).
4. Paste-assist API (with seg-06 spike).
5. TS toolchain flavor + test harness.
6. EGO submission rules (v2 track).
7. Prefs stack (GTK4/libadwaita current).
