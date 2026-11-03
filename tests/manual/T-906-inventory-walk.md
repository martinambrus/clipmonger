# T-906 — Config inventory walk [manual-qa]

> Current runner: ADR-005, owned winit event loop and CPU rendering.
> Current-runner native verdict: NOT RUN. Preserve earlier dated results
> separately. Record RustDesk/physical display, OS/compositor, DPI, monitors
> and source revision for each run.


- **ID / goal:** T-906. Touch EVERY control in the maximalist config window; verify staged Apply/Revert for each implemented config row (currently 29) (incl. `popup.delivery`).
- **Setup (box, build, profile fixture):** Any real display (Win first). Config matrix generated from seg-03 §6 key table x seg-09 §4 matrix; fresh profile plus a backed-up profile. Use a fresh test profile; SYNTHETIC clipboard fixtures only.
- **Session timebox:** 2 sessions of 60 min (M-02 split expected) (M-02: cap per session; split overruns into a new session).

## Steps

1. Open config; walk every tab/section top to bottom, screenshotting each.
   - Expected: Every documented control is present and reachable.
2. For every implemented row in this build: change the value, Apply, restart the daemon/UI.
   - Expected: Change persists and takes effect as documented.
3. For each row: change the value, then Revert before Apply.
   - Expected: Original value restored; no partial writes.
4. Set an invalid value in each validated field.
   - Expected: Field rejected with an inline error; config file unchanged.
5. Filter settings via the settings search and edit one result.
   - Expected: Filter finds it; edit applies to the correct key.
6. Close config with unsaved staged changes and reopen.
   - Expected: Prompt/discard behaviour matches spec; no silent loss or save.

7. On Windows check the default-off Scroll Lock control, physical press
   behavior when enabled and visible paused status; record missed short taps.
   Check minimum length using multibyte text and mixed plain/HTML or RTF.
   - Expected: persisted settings and behavior match seg-03, including privacy
     exclusions and separate genuine non-text handling.
8. On Windows, inspect **Restore clipboard after paste** on a fresh profile or
   a `[popup]` section with that key omitted; select an older synthetic clip
   from the popup and then paste with Ctrl+V into a second target.
   - Expected: the setting defaults unchecked and the selected clip stays on
     the clipboard. Enable it, Apply, and repeat with distinct original/selected
     clips: the original clipboard returns after the dwell if unchanged.
     Restart with explicit true and then false saved; both choices persist
     without changing unrelated settings.

## Evidence to attach

Build-specific row checklist (currently 29) with PASS/FAIL per row, screenshots, config file diff per Apply. Redact paths/window titles/tokens (finding 52).

## Verdict

- Verdict: PASS / FAIL / BLOCKED (circle one)
- Notes:
- Signature:
- Date:
