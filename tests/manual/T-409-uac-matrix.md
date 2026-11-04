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
   - With default `clipboard_restore` delivery and `restore_clipboard = false`,
     the selected clip stays on the clipboard on both routes. Repeat with
     **Restore clipboard after paste** enabled: the previous clipboard returns
     after the dwell if unchanged. These current criteria do not rewrite the
     dated verdicts below.
4. Leave everything idle for more than 8 minutes, then paste into the elevated app.
   - Expected: instant paste, no prompt.
5. Open the popup from the tray icon and pick a clip; repeat with `clipmonger-cli show-popup`.
   - Expected: pastes into the previously active standard window; for an elevated window a "Paste failed" box with the reason (popups not opened by the hotkey carry no gesture, ADR-004); never silent.
6. On a fresh app start answer No; on another, cancel the UAC prompt.
   - Expected: no further question until the next app start; after a cancel caps paste is Degraded; elevated pastes fail visibly, standard pastes work.
7. Tray → Exit.
   - Expected: UI, daemon and helper exit; no scheduled task exists.

Installed (`C:\Program Files\Clipmonger`):

8. Start the app, focus the elevated app, answer Yes and approve UAC.
   - Expected: `ClipmongerHelper` task registered via COM with verified SID principal/trigger, highest privileges, no battery stop, `PT0S` time limit, IgnoreNew, priority normal, exact protected command, quoted profile argument, empty working directory, restrictive SDDL; marker remains as optional description metadata only; helper connected; pastes instant.
9. Exit and start the app again.
   - Expected: helper reconnects; no question, no UAC.
10. Log off and on (or reboot), then start the app.
   - Expected: helper started at logon with no visible window; admin paste works with no prompt.

Both:

11. With an unprotected/invalid `ClipmongerHelper` task present (left by an older build or forged marker with wrong command/profile/action), start any elevated helper.
   - Expected: the unprotected/invalid task is deleted after inspecting actual action; valid protected tasks for other profiles are preserved; query failures are reported separately and not counted as safe.
12. Inspect `<profile>/audit.log`.
   - Expected: metadata only (`connected`, `job` with `ms` and result, `disconnected`, refusals); no payload bytes.


Registration hardening (pending interactive Windows acceptance):

13. Approve consent with the original account, cancel it on another fresh start,
    and try alternate-account credentials on another.
    - Expected: same-account registration succeeds; cancel/failure never starts
      the task and snoozes consent until restart; alternate credentials return a
      clear refusal and never register a task for the administrator account.
14. Inspect the actual task action, principal, single logon trigger, settings,
    owner and DACL. Try changing its action, owner or DACL as the medium user.
    - Expected: only System/Administrators can modify it; the original SID can
      read/run. A copied description marker cannot authorize a different action;
      removing the description from an otherwise correct definition is harmless.
15. Exercise synthetic installed fixtures with requester-owned files/directories,
    write/delete/WRITE_DAC rights, a replaceable parent, and reparse points.
    - Expected: persistent registration refuses every mutable or unresolved path
      before elevation; portable consent remains a per-run worker with no task.
    - On an owned synthetic file and directory, check that write/delete access
      succeeds before pinning, fails with sharing violation (32) while pinned,
      and succeeds after dropping the pin. Existing write/delete handles must
      also prevent the pin. Access-denied (5) alone does not prove sharing.
16. Repeat with Unicode, ampersands, spaces and a trailing backslash in the profile.
    - Expected: the exact arguments survive in-memory Unicode registration and
      structured readback; no pre-UAC XML or other task-definition file is created.
17. With isolated scheduler fixtures, exercise forged-marker wrong commands,
    appended arguments, extra/non-Exec actions, working directories, other SIDs,
    writable descriptors, and incorrect settings. Repeat cleanup with a correct
    protected task for another profile and with a query failure.
    - Expected: mismatches never run; proven unsafe legacy tasks are removed;
      valid other-profile tasks and unknown query/protection results are preserved,
      with unknown results reported separately.

These new legs are **NOT RUN**. The dated verdicts below retain their original
scope and do not validate the registration hardening or native ACL/UAC boundary.


## Evidence to attach

Caps dumps before/after each leg, audit.log excerpt, screenshots of the UAC prompt and Degraded caps state. Redact paths/window titles/tokens (finding 52).

## Verdict (2026-11-04, 527ad07 portable, leg 5 rerun)

- Verdict: **PASS** for leg 5 (`clipmonger-cli show-popup` and tray).
- Notes: Windows 10.0.26300. The CLI popup now captures the window that was active. Over a standard window Enter pasted into it. Over the admin window the popup appeared without focus (a non-elevated process cannot take the foreground from an elevated window without the input; daemon logged `popup.activation_failed`) and stayed open; after a click into it, Enter was refused with the elevation error in the status line. Regression: a tray-opened popup pasted into the previously active standard window, and a hotkey-opened popup pasted into the admin window through the helper.
- Signature: user (interactive session)
- Date: 2026-11-04

## Verdict (2026-11-04, 2dabd38 portable, 2623046 installed)

- Verdict: **PASS** for legs 1-4, 6-10 and the tray half of leg 5; **FAIL** for the `clipmonger-cli show-popup` half of leg 5; legs 11-17 were not run.
- Notes: Windows 10.0.26300. Portable: consent asked once, one UAC; helper pastes 1-17 ms, also after 10 min idle; tray-opened popup pasted into a standard window and refused the admin target with a visible reason; No and UAC cancel were not asked again and left admin paste visibly refused (caps Degraded after cancel); tray Exit left no process, task or Run value; `restore_clipboard` on restored the previous clipboard on both routes. CLI-opened popups have no target: over the admin window the popup hid itself (`popup.activation_failed`), over a standard window Enter focused the admin window and the paste was refused. Installed: the first leg 8 run on 65b2b2a failed because the registrar compared the read-back principal and trigger account names with the expected SID (exit 101, helper never ran); after 2623046 the task verified, the helper connected, reconnected after an app restart without a prompt, and started at logon after a reboot with no window. Admin and standard pastes worked throughout.
- Signature: user (interactive session)
- Date: 2026-11-04

## Verdict (2026-10-31, ADR-004 design)

- Verdict: **PASS** for portable legs 1-3 and 5; legs 4 and 6-12 were not re-run (ADR-004 changed only the leg 5 expectation).
- Notes: hotkey-opened admin pastes went through the helper (`job` 2 ms, twice); a tray-opened popup's admin paste was refused with a visible reason (`elevation_refused`, "missing or invalid gesture token"); a `clipmonger-cli show-popup` popup pasted into a standard terminal. After a daemon restart the app asked again and the helper reconnected. Same session: ADR-004 checklist (hold, toggle, live `popup.hotkey` change, second launch, UI and daemon restart) passed over RustDesk.
- Signature: user (interactive session)
- Date: 2026-10-31

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
