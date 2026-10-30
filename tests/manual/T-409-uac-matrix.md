# T-409 — UAC elevated-paste matrix [manual-qa]

- **ID / goal:** T-409. Verify the elevated helper per install mode (ADR-003): consent question, one UAC prompt, instant pastes into elevated and standard windows, no late or misdirected paste, helper lifetime, logon start, refusals, and audit entries.
- **Setup (box, build, profile fixture):** Windows host; an elevated target app (for example Notepad run as administrator) and a standard one. Portable legs: build in a user folder with `portable.marker`. Installed legs: build in `C:\Program Files\Clipmonger`. SYNTHETIC clipboard fixtures only.
- **Session timebox:** 60 min (M-02: cap per session; split overruns into a new session).

## Steps

Portable (user folder + `portable.marker`):

1. Start `clipmonger.exe`; check processes and `clipmonger-cli status`.
   - Expected: UI + daemon only, no console windows, profile beside the binary; `helper.route` unavailable.
2. Focus the elevated app.
   - Expected: within about 1 s one question "Enable Clipmonger in admin windows?" (default button No); Yes → one UAC prompt → `audit.log` shows `connected`.
3. Paste via the popup hotkey into the elevated app, the standard app, and the elevated app again.
   - Expected: every paste instant and into the window the hotkey was pressed in; `audit.log` `job` entries with single-digit `ms`.
4. Leave everything idle for more than 8 minutes, then paste into the elevated app.
   - Expected: instant paste, no prompt.
5. Open the popup from the tray icon and pick a clip.
   - Expected: pastes into the previously active window, or a "Paste failed" box with the reason; never silent.
6. On a fresh app start answer No; on another, cancel the UAC prompt.
   - Expected: no further question until the next app start; after a cancel caps paste is Degraded; elevated pastes fail visibly, standard pastes work.
7. Tray → Exit.
   - Expected: UI, daemon and helper exit; no scheduled task exists.

Installed (`C:\Program Files\Clipmonger`):

8. Start the app, focus the elevated app, answer Yes and approve UAC.
   - Expected: `ClipmongerHelper` task with the `(clipmonger-task ...)` marker, highest privileges, no battery stop, `PT0S` time limit, logon trigger for the current user; helper connected; pastes instant.
9. Exit and start the app again.
   - Expected: helper reconnects; no question, no UAC.
10. Log off and on (or reboot), then start the app.
   - Expected: helper started at logon with no visible window; admin paste works with no prompt.

Both:

11. With an unmarked `ClipmongerHelper` task present (left by an older build), start any elevated helper.
   - Expected: the unmarked task is deleted.
12. Inspect `<profile>/audit.log`.
   - Expected: metadata only (`connected`, `job` with `ms` and result, `disconnected`, refusals); no payload bytes.


## Evidence to attach

Caps dumps before/after each leg, audit.log excerpt, screenshots of the UAC prompt and Degraded caps state. Redact paths/window titles/tokens (finding 52).

## Verdict (2026-10-30, ADR-003 design)

- Verdict: **PASS** for legs 1-5, 7-10 and 12; legs 6 (No / UAC cancel) and 11 (legacy task cleanup) were not run in this session.
- Notes: portable helper round trips 1-2 ms; admin paste after 17 min idle 2 ms; tray Exit left no process and no task. Installed task verified (marker, highest privileges, no battery stop, `PT0S`, logon trigger for the current user). Legs 9 and 10 ran together: after logoff/logon the helper ran with no console window and the app reconnected at start with no question and no UAC.
- Signature: user (interactive session)
- Date: 2026-10-30

## Verdict (2026-10-27, previous design)

- Verdict: **PASS** (11/11 legs, Windows host, 2026-10-27)
- Notes: Executed interactively with the user (agent drove, user
  consented/cancelled UAC prompts and confirmed visually). Findings
  fixed en route: daemon shipped without backends in release builds
  (feature flags), daemon hotkey could never fire (global-hotkey
  message pump + on-thread registration), helper supervisor panicked
  (Tokio pipe created outside the runtime), heartbeats could be
  mistaken for job results and supervisor died after refusals,
  helper target validation raced the UAC desktop switch (3 s
  foreground-settle wait added). Cancellation leg: win32 1223,
  caps Degraded, audit metadata-only (25 entries, no payloads).
  Portable leg: elevated route unavailable from unprotected path,
  standard paste worked. Follow-ups recorded: first retry after a
  refusal hits a stale helper channel once; `elevation_refused`
  stayed set after a later success until daemon restart; helper
  died twice on missed heartbeats while idle. Paste delivery was
  then switched to clipboard-restore by default (popup.delivery)
  and re-verified live on both routes with a 2,026-byte clip
  (instant paste, previous clipboard restored).
- Signature: user (interactive session)
- Date: 2026-10-27
