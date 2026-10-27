# T-606 — Wayland compositor matrix [manual-qa, signoff]

- **ID / goal:** T-606. Per compositor (KDE, Sway, Hyprland, GNOME 48+ VM): portal dialog UX, bind persistence across reboot, caps goldens. BLOCKED-UBUNTU/VM.
- **Setup (box, build, profile fixture):** One VM/session per compositor. Release Linux build; fresh profile per compositor. Use a fresh test profile; SYNTHETIC clipboard fixtures only.
- **Session timebox:** 60 min per compositor (M-02: cap per session; split overruns into a new session).

## Steps

1. Start the daemon; capture caps.
   - Expected: Caps honestly reflect this compositor's data-control support.
2. Copy synthetic text/image; verify history.
   - Expected: Capture works or degrades with a stated reason.
3. Bind the popup hotkey through the portal dialog.
   - Expected: Dialog UX is understandable; bind succeeds or fails honestly.
4. Reboot/re-login; check the bind.
   - Expected: Portal bind persists across reboot as documented.
5. Save this compositor's caps dump as its golden candidate.
   - Expected: Golden candidate attached for gate 2.
6. Attempt paste; record the avenue used.
   - Expected: Injection or write-and-toast fallback matches caps; never a silent no-op.


## Evidence to attach

Per-compositor table, caps golden candidates, portal screenshots. Redact paths/window titles/tokens (finding 52).

## Verdict

- Verdict: PASS / FAIL / BLOCKED (circle one)
- Notes:
- Signature:
- Date:
