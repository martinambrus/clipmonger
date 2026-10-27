# T-905 — Popup window matrix [manual-qa]

- **ID / goal:** T-905. Verify frameless/on-top/focus/dismiss/placement of the popup per OS.
- **Setup (box, build, profile fixture):** Per-OS sub-table: Win11, X11, Wayland (Sway/KDE), GNOME+ext. Release build for that OS; synthetic clips seeded. Use a fresh test profile; SYNTHETIC clipboard fixtures only.
- **Session timebox:** 90 min, split per OS (M-02: cap per session; split overruns into a new session).

## Steps

1. Trigger the popup with the configured hotkey on each OS.
   - Expected: Popup appears framed as designed, on top, focused.
2. Trigger again / press Esc / click outside on each OS.
   - Expected: Popup dismisses; focus returns to the previous app.
3. Trigger near each screen edge and on multi-monitor setups.
   - Expected: Popup stays fully on-screen on the active monitor.
4. Open the popup while a fullscreen app is foreground.
   - Expected: Popup appears above or honestly defers; never pastes into the wrong window.
5. Resize/scale (100%/150%/200%) and re-trigger.
   - Expected: Popup renders legibly at every scale.


## Evidence to attach

Per-OS screenshots, caps dump, OS/build identification for each row. Redact paths/window titles/tokens (finding 52).

## Verdict

- Verdict: PASS / FAIL / BLOCKED (circle one)
- Notes:
- Signature:
- Date:
