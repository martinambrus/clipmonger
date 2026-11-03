# T-905 — Popup window matrix [manual-qa]

> Current runner: ADR-005, owned winit event loop and CPU rendering.
> Current-runner native verdict: NOT RUN. Preserve earlier dated results
> separately. Record RustDesk/physical display, OS/compositor, DPI, monitors
> and source revision for each run.


- **ID / goal:** T-905. Verify frameless/on-top/focus/dismiss/placement of the popup per OS.
- **Setup (box, build, profile fixture):** Per-OS sub-table: Win11, X11, Wayland (Sway/KDE), GNOME+ext. Release build for that OS; synthetic clips seeded. Use a fresh test profile; SYNTHETIC clipboard fixtures only.
- **Session timebox:** 90 min, split per OS (M-02: cap per session; split overruns into a new session).

## Steps

1. Trigger the popup with the configured hotkey on each OS.
   - Expected: Popup is frameless as designed, legible, on top and focused where activation is granted.
2. Trigger again / press Esc / click outside on each OS.
   - Expected: Popup dismisses; focus returns to the previous app.
3. Trigger near each screen edge and on multi-monitor setups.
   - Expected: Popup stays fully on-screen on the active monitor.
4. Open the popup while a fullscreen app is foreground.
   - Expected: Popup appears above or honestly defers; never pastes into the wrong window.
5. Resize/scale (100%/150%/200%) and re-trigger.
   - Expected: Popup renders legibly at every scale.

6. Start minimized, open Settings, close it and reopen the popup.
   - Expected: minimized startup stays hidden; popup and Settings have separate
     native lifecycles with no duplicate UI or tray instance.
7. Repeat show/hide, scale and monitor changes on RustDesk and a physical display.
   - Expected: the same CPU UI renders both; record focus/placement failures.
8. On Windows, switch from ordinary app A to app B (for example ChatGPT to
   Notepad), then open the popup by hotkey. Type in search, hold Alt and press
   Tab, then release Alt. Repeat after Esc, outside-click dismissal, several
   reopen cycles, and a DPI/monitor change.
   - Expected: the popup has no taskbar button or Alt-Tab tile; search accepts
     typing. Alt-Tab dismisses it and switches to A, preserving the ordinary-app
     cycle instead of consuming a switch to return to B. Record membership
     and actual destination separately; native style flags alone prove neither
     the Explorer cycle nor the observed destination. Settings remains a normal
     taskbar/Alt-Tab window through close/reopen.

## Evidence to attach

Per-OS screenshots, caps dump, OS/build identification for each row. Redact paths/window titles/tokens (finding 52).

## Verdict

- Verdict: PASS / FAIL / BLOCKED (circle one)
- Notes:
- Signature:
- Date:
