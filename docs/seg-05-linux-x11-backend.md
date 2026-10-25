# Linux X11 backend (crate `backend-x11`)

Approved scope (Q2): full desktops first (GNOME/KDE/Xfce); bare window
managers work with tray/autostart gracefully degraded + documented manual
setup. Same code runs on real X and XWayland (v1 validates on XWayland).
Pure Rust: x11rb 0.14 (MIT OR Apache-2.0; rustix-based, NO libX11/libxcb
system deps). Features: `xfixes`, `xtest`.

## 1. Monitor — `ClipboardMonitor`

- Connect per X11rb docs (`Connection` trait); run on the backend thread.
- `xfixes::SelectSelectionInput` on atoms `CLIPBOARD` and `PRIMARY` →
  owner-change events → `CaptureEvent` (origins Clipboard/Primary).
Primary honoring `capture_primary=false` default + debounce + min length
(seg-03 §8, review H-11); also check the password-hint MIME where
managers offer it (verify behavior at implementation, review H-03).
- ALSO implement the freedesktop manager role (fd.o ClipboardManager
  spec, inspected): own selection `CLIPBOARD_MANAGER`, serve
  `SAVE_TARGETS` (take over + persist clips when owners exit),
  honor `TARGET_SIZES` (refuse/ask on huge payloads), answer `TARGETS`.
  Critical spec subtlety: do NOT send the `SelectionNotify` completing
  `SAVE_TARGETS` until ALL transfers (incl. INCR) finish — the owner
  exits on receipt and may abort slow transfers.
- Privacy (F-104): X11 has no standard opt-out signal; honor a config
  denylist of owner window classes (e.g. password managers) as the
  documented substitute + support the `TARGET_SIZES` refusal path.
- Manager coexistence (review finding 27): check for an existing
  `CLIPBOARD_MANAGER` owner FIRST. If present → capture-only mode (record
  history, do NOT serve SAVE_TARGETS, never steal/fight the selection).
  If absent → claim it and serve SAVE_TARGETS. On selection loss → fall
  back to capture-only + caps note. History capture and persistence are
  SEPARABLE roles; manager ownership is never required for history.

## 2. Transfer — `ClipboardTransfer`

Requestor (read):
- `XConvertSelection` → await `SelectionNotify` → `GetProperty` (delete
  after read). INCR: on `INCR` type, delete property to ACK, then loop
  `PropertyNotify` chunks until zero-length. Timeout 5 s per transfer;
  stuck owners → `Timeout` error, backend thread NEVER blocks the daemon.

Owner (write):
- Serve `SelectionRequest` for: `TARGETS`, `TIMESTAMP`, `UTF8_STRING`,
  `STRING`, `TEXT`, `image/png`, `text/uri-list`, plus `x-os/*`
  passthrough atoms. Refuse unknown targets properly (empty property).
  INCR-send payloads above max-request-size (query server limit).

MIME map = seg-03 §2 X11 column. Timestamps: use server time from events
for `TIMESTAMP` (ICCCM-correct subdivision, prevents races).

## 3. Hotkeys — `HotkeyManager`

- Via `global-hotkey` crate (X11 backend = `XGrabKey`, verified README).
- Grab each chord under all combinations of the DYNAMICALLY discovered
  ignored lock modifiers (review finding 26): inspect the server modifier
  map for NumLock/ScrollLock/CapsLock masks (NumLock is NOT always Mod2),
  grab every lock combo, recompute after keymap changes.
- `AlreadyGrabbed` → that hotkey `Degraded` + UI warning (never crash).

## 4. Paste engine — `PasteEngine`

- All simulation via `enigo` (default `x11rb` feature — pure Rust, no
  libxdo needed; chord API per seg-04 §12.5).
- Strategy: **ClipboardRestore only** (X11 has no Unicode injection):
  save → write clip → enigo Ctrl+V chord → best-effort restore.
- PRIMARY paste: copy primary content to CLIPBOARD first, then same flow
  (avoids risky middle-click simulation at arbitrary pointer positions).
- Restore is GUARDED (seg-02 §6): only when our temp value still current
  (owner still us + content-hash match); user copies always win (finding 9).
- Re-focus precedes chords (seg-02 §6): restore focus to the captured
  target via the standard call (verify exact X call + EWMH interplay at
  implementation — review C-05).
- XTest missing at runtime (rare) → `paste: Degraded` with reason.

## 5. Tray — `TrayController`

- `tray-icon` with `ksni` (pure D-Bus StatusNotifier, NO GTK) first;
  single Linux tray build preferring KSNI (review M-01: KSNI/AppIndicator
  are compile-time features, not a runtime fallback — verify exact
  feature behavior at implementation);
  no host at all → `tray: Unavailable`, daemon fully operable via CLI +
  hotkeys. Tray is hosted by app-ui (seg-02 §1); this section defines Linux
behavior only. Menu = seg-09 §7 (sole definition, review M-06).

## 6. Autostart — `AutostartManager`

- Write `~/.config/autostart/clipmonger.desktop`:
  `[Desktop Entry] / Type=Application / Name=... / Exec=<quoted exe>
  --minimized / Hidden=<bool> / Terminal=false`.
  (Format verified against this machine's `/etc/xdg/autostart` + CopyQ's
  `Hidden`/`Exec` management pattern; fd.o spec page itself is a stub.)
- Validate by re-parsing with our own reader (roundtrip `[unit]` test);
  never shell out. Portable mode skips autostart.

## 7. Foreground app (v2, AutoSwitch F-304)

- `_NET_ACTIVE_WINDOW` → active window → `_NET_WM_PID` → `/proc/<pid>/exe`
  (`std::fs`, no crate). Fallback: `WM_CLASS`. Missing EWMH (bare WM) →
  `ForegroundApp: Unavailable` → AutoSwitch off with note.

## 8. Caps mapping

Full DE: all `Full`, `primary_selection=true`.
Bare WM: tray/autostart/foreground per-probe (`Degraded`/`Unavailable`
with setup hints). XTest-less server: paste `Degraded`.

## 9. Segment tests (T-5xx)

- T-501 `[unit]` atom/MIME map incl. `x-os/*` passthrough both ways.
- T-502 `[unit]` INCR chunk encode/decode + timeout state machine.
- T-503 `[unit]` .desktop write/parse roundtrip + `Hidden` toggle.
- T-504 `[unit]` lock-mod chord expansion (4 combos).
- T-505 `[integration]` (Xvfb, X.Org man verified: `Xvfb :99 -screen 0
  1600x1200x24`, `DISPLAY=:99`): fake x11rb owner → capture event on
  change; read-back all §2 targets; SAVE_TARGETS handoff with a DYING
  owner (clip survives); 5 MiB INCR transfer; owner-timeout kill case.
- T-506 `[integration]` (Xvfb): hotkey grab/fire + conflict injection
  (second grabber → Degraded, no crash).
- T-507 `[integration]` (Xvfb+XTest): paste flow into a scratch window.
- T-508 `[manual-qa]` real-DE pass (GNOME+Xorg, KDE, Xfce): tray visible,
  autostart reboot-persist, password-manager denylist honored.
- T-509 `[integration]` (Xvfb) pre-existing fake CLIPBOARD_MANAGER owner →
  capture-only mode, no steal/fight; owner exit → clean claim.
- T-510 `[unit]` dynamic lock-modifier expansion incl. non-Mod2 NumLock map.
- T-511 `[integration]` (Xvfb) primary policy: default OFF (no capture),
  ON with debounce (churn coalesced) + min-length respected.

## 10. Verification tasks

1. x11rb 0.14 API shape at implementation (events, xfixes, xtest calls).
2. Runtime XTest-presence probe + Degraded path.
3. KSNI-host detection order vs AppIndicator on target DEs.
