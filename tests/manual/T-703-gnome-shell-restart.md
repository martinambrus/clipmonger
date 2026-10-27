# T-703 — GNOME Shell restart re-register [integration, Ubuntu box]

- **ID / goal:** T-703. Verify extension re-registration and resync after a Shell restart (documented-manual where --replace is unsafe).
- **Setup (box, build, profile fixture):** Ubuntu GNOME box with T-702 passing. Xorg session for `Alt+F2, r` restart (Wayland: log out/in, documented-manual). Use a fresh test profile; SYNTHETIC clipboard fixtures only.
- **Session timebox:** 45 min (M-02: cap per session; split overruns into a new session).

## Steps

1. With capture working, restart GNOME Shell (or log out/in on Wayland).
   - Expected: Shell restarts; session survives.
2. Check extension state and daemon caps.
   - Expected: Extension re-registers; extension_connected returns to true without daemon restart.
3. Copy synthetic text.
   - Expected: Capture resumes; no duplicate or lost daemon state.
4. Repeat the restart twice more.
   - Expected: Stable re-registration each time; no wedge or capture-paused latch.


## Evidence to attach

caps dumps before/after each restart, Shell log excerpt. Redact paths/window titles/tokens (finding 52).

## Verdict

- Verdict: PASS / FAIL / BLOCKED (circle one)
- Notes:
- Signature:
- Date:
