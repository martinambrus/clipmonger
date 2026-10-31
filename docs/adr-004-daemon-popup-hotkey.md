# ADR-004 — Daemon-owned popup hotkey, popup push, press-bound gestures

Status: Final (design user-approved 2026-10-30 with the Windows v1
completion plan; focus gate passed on the Windows host; hands-on Windows
checklist passed 2026-10-31). Pending: the Linux open items below.
Decision: D-026 (supersedes the ADR-003 / D-025 lines on the popup-hotkey
owner and on gesture minting; amends seg-02 §1/§6/§9, seg-04 §3/§5/§11,
seg-05 §3, seg-06 §4, seg-07 §3, seg-09 §3).

## Decision

Popup hotkey and trigger path:

- The daemon owns the popup hotkey on every OS: Windows (`RegisterHotKey`
  on one daemon thread that owns all daemon hotkeys), X11 (key grab),
  Wayland (`GlobalShortcuts` portal) and GNOME (the extension reports
  presses with `hotkey.fired`). The UI registers no chord and installs no
  keyboard hook.
- Every press and every `popup.open` request takes one daemon path:
  - presses less than 300 ms apart count once (key repeat; extension and
    portal reporting the same chord);
  - a press captures the paste target (Windows foreground window, X11
    active window, none on Wayland); on Windows a press while the UI's
    popup is the foreground window hides it instead (`toggle`);
  - a press mints a single-use gesture token bound to that target (30 s);
    `popup.open` (CLI, second launch) captures nothing and mints nothing,
    and the tray opens the popup inside the UI without a target;
  - Windows: the daemon raises the UI's popup itself while handling the
    press. Only the process that received the hotkey may take the
    foreground, and only until the next input (the key release), so an
    activation by the UI after the push would race the release;
  - the daemon pushes `ui.show_popup{source, toggle, target?, gesture?,
    x11_time?, activation_token?}` (`source` = `hotkey` | `request`; the
    last two are reserved for Linux focus handling);
  - with no UI subscribed, the event is held 10 s for the next subscriber
    and the daemon starts `clipmonger --profile-dir <profile> --popup`, at
    most once per 10 s (`CLIPMONGER_NO_UI_SPAWN` disables this for tests).
- Windows hotkey changes: the daemon re-reads `popup.hotkey` on every
  poll (200 ms), registers a changed chord before dropping the old one,
  keeps the old chord when the new one is refused, retries the refused
  chord every 5 s, and reports it in caps (`hotkey: degraded` + note).
  Linux still binds the chord once per daemon run (open items).

Push channel and UI:

- The IPC server answers `events.subscribe{topics, ...}` itself with
  `{ok: true}` before any event; events are frames whose `method` is the
  topic. `ui.show_popup` is exclusive (a second subscriber gets
  `already_subscribed`) and guarded: the subscriber's process (peer
  credentials on Unix, `GetNamedPipeClientProcessId` on Windows) must run
  the UI binary next to the daemon, and a `popup_hwnd` param must belong
  to that process. A subscription ends with its connection and is never
  idle-evicted; overflow still yields `resync-required` (seg-02 §9).
- One UI per profile: exclusive lock on `<profile>/ui.lock`. A second
  launch sends `popup.open` and exits (on Windows it first passes on its
  foreground right); an `already_subscribed` answer also ends a UI.
- The UI keeps its daemon link: it reconnects at most every 2 s, restarts
  the daemon when it is gone, and never retries a failed call.

Gesture rule (replaces "picking a clip mints the gesture token"):

- A paste into an elevated window needs the gesture of the press that
  opened the popup, for the window captured with it; the UI sends it with
  `paste.execute{target_hwnd, picked_ms, gesture}`. There is no fallback
  to a recent trigger.
- `hotkey.fired` mints nothing on Windows (ignored there); on Linux it is
  the GNOME extension's press report and takes the trigger path.
- `paste.execute` accepts `target` (alias `target_hwnd`) as an integer
  only; a non-integer value or a `target`/`target_hwnd` mismatch is
  `invalid_params`, never a fallback to the foreground window.

## Reason

- No OS opened the popup through the daemon: X11, portal and GNOME
  presses only minted a token, only the Windows Win32 popup registered its
  own chord, and the seg-02 §9 push was not implemented.
- ADR-003 minted a gesture whenever a clip was picked, through
  `hotkey.fired`, which any holder of the profile token can call (its
  known limitation). Tokens now come only from real presses and go only
  to the verified UI binary.
- A second UI launch made a second tray icon and fought over the chord.
- Focus gate (2026-10-30, Windows host over RustDesk): the daemon owning
  the chord and raising a separate UI process's popup inside the press
  focused it 26/26 times (13 normal and 13 admin Notepad, taps and holds),
  the same as the UI owning the chord. For admin targets
  `AttachThreadInput` is refused (UIPI) but `SetForegroundWindow`
  succeeds on the hotkey's foreground right.

## Affected segments

seg-00 (D-025 hotkey line superseded by D-026), seg-02 §1 (one UI per
profile), §6 (popup paste flow), §9 (`events.subscribe`, `ui.show_popup`,
`popup.open`), seg-04 §3 (popup-hotkey owner, live change), §5 (gesture
rule), §11 (T-406, T-413), seg-05 §3, seg-06 §4, seg-07 §3, seg-09 §3,
ADR-003 (popup-hotkey owner and gesture lines).

## Migration / test impact

- Behavior: admin-window paste needs the hotkey. A popup opened from the
  tray, the CLI or a second launch carries no gesture; its admin-window
  paste is refused with a visible reason (T-409 step 5).
- IPC: live push tests in `crates/ipc/tests/push_acceptance.rs` (Unix
  socket) and `push_pipe_acceptance.rs` (Windows pipe).
- T-406: a refused chord change keeps the old chord
  (`t406_refused_change_keeps_chord`, Windows session).
- T-413: missing gestures and gestures for another window are refused;
  `popup.open` never mints; `hotkey.fired` mints nothing on Windows.
- UI: `crates/app-ui/tests/push_ui_acceptance.rs` (push mapping,
  `ui.lock`, shared event queue, live listener, second instance).
- No data migration; config keys unchanged.

## Open items (Linux pickup)

Need a Linux machine with an X11 session (Xvfb for the automated leg).

1. Live popup-hotkey change on X11 (Windows only today):
   - `crates/backend-x11/src/hotkeys.rs` `X11Hotkeys::register` ungrabs
     the old chord first. Grab the new chord first; the same keycode and
     modifier mask is a no-op (an ungrab after the new grab would drop
     it); a refused grab leaves the old grab and the id's state unchanged.
   - `crates/daemon/src/main.rs` `setup_x11_backend` registers the chord
     once at startup. Register it in the poll thread through
     `clipmonger_daemon::popup::PopupChord` (shared with Windows, see
     `popup_chord_loop`) and re-register the `x11` caps when the status
     changes.
   - `crates/app-ui/src/main.rs`: show the Settings "Applies on restart"
     note only where a change still needs a restart (today: every
     non-Windows build).
   - Tests: an Xvfb twin of `t406_refused_change_keeps_chord` in
     `crates/backend-x11/tests/xvfb_acceptance.rs` (another client holds
     chord B: a change to B is refused and A stays grabbed; a change to C
     releases A). Manual on Xorg: after a change in Settings the new chord
     opens the popup and the old one does nothing; with the new chord held
     by another client, `clipmonger-cli status` shows `hotkey: degraded`
     with a note and the old chord keeps working; after release the new
     chord works within 5 s.
2. Wayland portal: the chord is bound once per daemon run
   (`crates/daemon/src/portal.rs`); a rebind may show the portal dialog
   again, so decide the UX before making it live.
3. GNOME extension: its keybindings follow a changed chord only once the
   daemon pushes `config.changed` (seg-07 §3; not implemented).
4. Verify the push path on a Linux desktop (X11 grab, portal, extension,
   `clipmonger-cli show-popup`, second launch, UI and daemon restart);
   on Linux it is covered by unit and socket tests only. Known gaps there:
   a press while the popup is in front does not hide it (the daemon sets
   `toggle` on Windows only), and the popup does not yet use `x11_time` /
   `activation_token` to take focus.
