# T-409 — UAC elevated-paste matrix [manual-qa]

- **ID / goal:** T-409. Verify the on-demand elevated helper route: first prompt, no re-prompt while alive, cancel/refusal degradation, re-prompt after daemon restart, and audit entries.
- **Setup (box, build, profile fixture):** Windows host; build installed under Program Files (protected path); elevated target app available. If no helper route ships, run the negative leg (step 3) and mark BLOCKED-HELPER. Use a fresh test profile; SYNTHETIC clipboard fixtures only.
- **Session timebox:** 60 min (M-02: cap per session; split overruns into a new session).

## Steps

1. Install the Windows zip under Program Files and start the daemon; check `clipmonger-cli status` caps.
   - Expected: Daemon up; paste level reported honestly (Unavailable/Degraded until helper runs).
2. Copy synthetic text, paste into a standard-integrity app.
   - Expected: Paste succeeds without any UAC prompt.
3. If no separate helper executable/route exists, record caps paste state and stop the positive legs.
   - Expected: Caps report Unavailable/Degraded with a reason; no crash, no silent success. Mark BLOCKED-HELPER.
4. Paste into an elevated (high-integrity) app.
   - Expected: One UAC prompt appears; consent allows the paste.
5. Paste again into the same elevated app while the helper lives.
   - Expected: Paste succeeds with no second prompt.
6. Restart the elevated app and paste again.
   - Expected: Paste succeeds or one fresh prompt; never a mis-paste.
7. Trigger an elevated paste and cancel the UAC prompt.
   - Expected: Paste refused; caps paste becomes Degraded; a retry is offered.
8. Retry the elevated paste after the refusal and consent.
   - Expected: Paste succeeds or degrades honestly; no prompt loop.
9. Restart the daemon, then paste into the elevated app.
   - Expected: A fresh UAC prompt appears (helper does not survive daemon restart silently).
10. Inspect `<profile>/audit.log`.
   - Expected: Helper spawn/refusal/job entries exist with metadata only; no payload bytes.
11. Run from a portable (non-Program Files) layout and attempt an elevated paste.
   - Expected: Elevated route Unavailable with reason; standard paste still works.


## Evidence to attach

Caps dumps before/after each leg, audit.log excerpt, screenshots of the UAC prompt and Degraded caps state. Redact paths/window titles/tokens (finding 52).

## Verdict

- Verdict: PASS / FAIL / BLOCKED (circle one)
- Notes:
- Signature:
- Date:
