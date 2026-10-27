# T-908 — Focus interplay — real-session leg (hybrid: automated + manual)

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


## Evidence to attach

Screen recording, daemon log excerpt, automated-leg test output attached. Redact paths/window titles/tokens (finding 52).

## Verdict

- Verdict: PASS / FAIL / BLOCKED (circle one)
- Notes:
- Signature:
- Date:
