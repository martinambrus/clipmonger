# T-908 — Focus interplay — real-session leg (hybrid: automated + manual)

> Current runner: ADR-005, owned winit event loop and CPU rendering.
> Current-runner native verdict: NOT RUN. Preserve earlier dated results
> separately. Record RustDesk/physical display, OS/compositor, DPI, monitors
> and source revision for each run.


- **ID / goal:** T-908. Real-session leg of T-908 [integration]: focus-switch mid-popup yields a stale-target error, never a mis-paste. Automated leg lives in ui_v1_acceptance.rs.
- **Setup (box, build, profile fixture):** Win or Ubuntu desktop; two text apps (A and B) with distinct synthetic content; popup journey primed on A. Use a fresh test profile; SYNTHETIC clipboard fixtures only.
- **Session timebox:** 30 min (M-02: cap per session; split overruns into a new session).

## Steps

1. Copy synthetic text in app A; open the popup.
   - Expected: Popup targets A.
2. Alt-Tab to app B while the popup is open; select a clip to paste.
   - Expected: Stale-target error shown, OR paste is re-validated against B explicitly; the clip never lands silently in the wrong app without that check.
3. Return to A and paste from a fresh popup.
   - Expected: Paste lands in A correctly.
4. Close B mid-popup and attempt the paste.
   - Expected: Honest error; daemon and popup stay alive.

5. Open by hotkey, hide, then reopen through the tray or CLI.
   - Expected: the old target and gesture were cleared on hide. The new request
     cannot reuse that gesture for an elevated paste.
6. While a hotkey-opened popup stays open, issue a CLI show-popup request.
   - Expected: the popup is raised with its current open context. Hiding
     afterward still clears that context.
7. On Windows, establish the ordinary-app cycle A to B, open the popup from B,
   then use Alt-Tab once. Repeat with Esc/outside dismissal and reopening.
   - Expected: popup is transient, absent from taskbar/Alt-Tab, and does not
     paste: Alt-Tab dismisses it and reaches an ordinary window. Landing on B
     instead of A is a known limitation (seg-09), not a failure.
     Search still accepts keyboard input, context clears on hide, and Settings
     remains an ordinary taskbar/Alt-Tab window after close/reopen.

## Evidence to attach

Screen recording, daemon log excerpt, automated-leg test output attached. Redact paths/window titles/tokens (finding 52).

## Verdict

- Verdict: PASS / FAIL / BLOCKED (circle one)
- Notes:
- Signature:
- Date:
