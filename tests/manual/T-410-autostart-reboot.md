# T-410 — Autostart + reboot persistence [manual-qa]

- **ID / goal:** T-410. Verify autostart enable/disable persists across reboot and portable mode leaves no Run value.
- **Setup (box, build, profile fixture):** Windows host; installed and portable layouts, one at a time; fresh profile. Use a fresh test profile; SYNTHETIC clipboard fixtures only.
- **Session timebox:** 45 min plus reboot (M-02: cap per session; split overruns into a new session).

## Steps

1. Enable autostart in config; inspect the Run registry value.
   - Expected: Run value present pointing at the installed daemon/UI entry.
2. Reboot and sign in.
   - Expected: Clipmonger starts; history from before reboot is intact.
3. Disable autostart; inspect the Run value.
   - Expected: Run value removed.
4. Reboot again.
   - Expected: Clipmonger does not start automatically; manual start works.
5. Repeat enable/reboot with the portable layout.
   - Expected: No Run value is written; portable mode leaves the host untouched.
6. Uninstall/remove the portable folder; inspect Run + profile leftovers.
   - Expected: No autostart leftovers outside the profile.


## Evidence to attach

Registry screenshots/exports per leg, caps dump after each reboot. Redact paths/window titles/tokens (finding 52).

## Verdict

- Verdict: **PASS** (steps 1-6; installed 2623046, portable 2dabd38)
- Notes: Windows 10.0.26300. Settings "Start on login" wrote the HKCU Run value with the installed UI, `--minimized` and the quoted profile, and removed it when cleared. After a reboot Clipmonger started without a window and history was intact; with autostart off it did not start and a manual start worked. The helper logon task runs at every logon independently of app autostart (ADR-003). Portable: applying autostart was refused and wrote nothing; the checkbox and message do not explain this yet. Uninstall with the profile kept left no files, task, Run, Startup, Start menu, Uninstall or Software keys and no process; nothing was written outside the portable folder.
- Signature: user (interactive session)
- Date: 2026-11-04
