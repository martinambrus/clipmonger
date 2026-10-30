# ADR-003 — Elevated helper: consent per install mode, pipe liveness, paste deadlines

Status: Final (user-accepted 2026-10-30 after hands-on QA on the Windows
host: portable and Program Files installs, 17 min idle, logoff/logon).
Decision: D-025 (amends D-011; supersedes the seg-04 §5 spawn, lifetime,
channel and heartbeat rules, the §3 popup-hotkey owner and the §7 helper
autostart ban).

## Decision

The elevation mode follows where the daemon binary lives:

- Installed (binary under the Program Files known folder, resolved with
  `SHGetKnownFolderPath`, never from `%ProgramFiles%`): on the first focus
  of an elevated window the daemon asks once ("Enable Clipmonger in admin
  windows?", default button No). Yes = one UAC prompt that registers the
  `ClipmongerHelper` logon task (current user, highest privileges, no
  battery stop, no time limit) and starts it. Later logons start the
  helper silently; it waits for the daemon and reconnects across app
  restarts.
- Portable (anything else): the same question, then one UAC prompt
  (`runas`) that starts a helper for this app run only; it exits with the
  daemon. Portable builds never create a scheduled task: a task running a
  user-writable binary with highest privileges would let any same-user
  process run code elevated without UAC (the task owner may run it with
  plain `schtasks /run`).
- No / UAC cancel: no further question until the next app start.

Helper and channel:

- The helper is the daemon binary in a hidden mode
  (`__elevated-persistent` for the task, `__elevated-worker` for portable),
  windowless (Windows GUI subsystem) and input-only: no keyboard hook, no
  store, config or clipboard access.
- One named pipe per profile, named from the profile path; the daemon
  listens for its whole run. Both sides verify the peer binary equals
  their own (pipe client/server process id + image path); the daemon also
  requires an elevated helper token; the helper opens the pipe with
  identification-only security so the server can never impersonate it.
- Strict request/response on one helper thread; no heartbeat. Liveness is
  the pipe itself (process exit closes it). While no verified helper is
  connected, jobs fail at once ("admin helper is not running").
- Every job carries the target window (captured when the hotkey opened the
  popup) and an absolute deadline 2 s after the clip was picked. The helper
  refuses to inject after the deadline; if the target is not foreground it
  activates exactly that window within the deadline, never another one.
  The daemon drops queued jobs that already expired.
- Standard-integrity targets use the same target window and deadline in
  the daemon: a paste never lands in a different window, and failures are
  shown to the user instead of being dropped.
- The popup UI owns the popup hotkey on Windows (`RegisterHotKey` keeps
  firing while an elevated window is focused); the daemon registers no
  popup chord. Picking a clip mints the gesture token (`hotkey.fired`).
- Every task created here carries an ASCII identity marker (hash of binary
  and arguments) in its description, so matching never depends on the
  console code page of `schtasks` output. A task without the marker is
  treated as legacy and deleted by the elevated helper when it starts.

## Reason

The previous helper wrote heartbeats and read jobs on one synchronous pipe
handle from two threads. Windows serializes synchronous I/O per file
object, so no heartbeat was ever sent while idle (the T-409 run of
2026-10-27 already saw the helper "die on missed heartbeats while idle").
The daemon spent 30 s of every 45 s waiting for that heartbeat, so admin
pastes were delayed up to 30 s, the link was dropped after ~7.5 min idle
(pastes silently ignored), and queued jobs later ran against whatever
window had focus. The user asked for: a question on first admin-window
focus, one UAC per run (portable) or once (installed), a helper invisible
at logon, and instant, reliable pastes into admin windows.

## Affected segments

seg-00 (D-011 amended by D-025), seg-04 §3 (popup-hotkey owner), §5
(spawn, lifetime, channel, protocol, refusals), §7 (helper autostart in
installed mode), §11 (meaning of T-403, T-409, T-413, T-414).

## Migration / test impact

- T-403: jobs gain optional `target_hwnd` and `expires_ms`; old lines
  still decode.
- T-413: the popup mints the token when a clip is picked. `hotkey.fired`
  is an ordinary IPC method, so the gesture rule only holds against
  callers without the profile token (known limitation).
- T-414: the protected-path rule now selects the mode (logon task vs
  per-run helper) instead of making portable elevated paste Unavailable.
- T-409: matrix rewritten for both modes (`tests/manual/T-409-uac-matrix.md`).
- Migration: legacy `ClipmongerHelper` tasks (no marker) are deleted by the
  elevated helper; installed users answer the question once more.
