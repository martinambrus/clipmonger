# T-907 — Keyboard-only operation [manual-qa]

- **ID / goal:** T-907. Complete the full journey with the mouse detached: open, search, select, paste, configure.
- **Setup (box, build, profile fixture):** Win or Ubuntu desktop; mouse detached/unplugged; synthetic clips seeded. Use a fresh test profile; SYNTHETIC clipboard fixtures only.
- **Session timebox:** 45 min (M-02: cap per session; split overruns into a new session).

## Steps

1. Open the popup via hotkey.
   - Expected: Popup opens focused with a visible focus indicator.
2. Type a search query; navigate results with arrow keys.
   - Expected: Selection moves visibly; search filters.
3. Press Enter to paste into the previous app.
   - Expected: Correct clip pastes; focus returns.
4. Open config via keyboard; navigate all tabs.
   - Expected: Every tab reachable; focus order is logical.
5. Change and Apply one setting via keyboard only.
   - Expected: Setting persists.
6. Esc out of popup and config.
   - Expected: No focus trap; previous app regains focus.


## Evidence to attach

Screen recording or step log, caps dump, list of any keyboard dead-ends. Redact paths/window titles/tokens (finding 52).

## Verdict

- Verdict: PASS / FAIL / BLOCKED (circle one)
- Notes:
- Signature:
- Date:
