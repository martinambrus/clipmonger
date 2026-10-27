# T-508 — X11 real desktop environments [manual-qa]

- **ID / goal:** T-508. On real DEs (GNOME+Xorg, KDE, Xfce): tray visible, autostart reboot-persist, password-manager denylist, plus the t507 keystroke leg under a real WM.
- **Setup (box, build, profile fixture):** Real DE installs or VMs. BLOCKED-UBUNTU (partly VMs). Release Linux build; fresh profile per DE. Use a fresh test profile; SYNTHETIC clipboard fixtures only.
- **Session timebox:** 60 min per DE (M-02: cap per session; split overruns into a new session).

## Steps

1. Start under this DE; check the tray.
   - Expected: Tray icon visible with correct menu/state.
2. Copy synthetic text/image; open the popup and paste.
   - Expected: Full journey works under this DE's WM.
3. Perform the t507 keystroke leg: synthetic keystroke paste into a focused app under the real WM.
   - Expected: Keystrokes land in the focused app; guarded restore behaves.
4. Enable autostart, reboot, sign in.
   - Expected: Clipmonger autostarts; history intact.
5. Copy from a password manager (denylist).
   - Expected: Not stored; denylist honoured on this DE.
6. Disable autostart and reboot.
   - Expected: No autostart; no leftovers.


## Evidence to attach

Per-DE rows with screenshots, caps dumps, autostart file/registry evidence. Redact paths/window titles/tokens (finding 52).

## Verdict

- Verdict: PASS / FAIL / BLOCKED (circle one)
- Notes:
- Signature:
- Date:
