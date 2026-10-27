# T-1117 — Evidence-bundle audit [manual-qa, release-time]

- **ID / goal:** T-1117. At release time: every release claim maps to a finite, reviewable evidence bundle; each manual script ran in its timeboxed window with evidence attached.
- **Setup (box, build, profile fixture):** Any box. Executed at release time, not before. Release record draft + all signed manual scripts. Use a fresh test profile; SYNTHETIC clipboard fixtures only.
- **Session timebox:** 60 min, desk task (M-02: cap per session; split overruns into a new session).

## Steps

1. List every claim in the draft release notes.
   - Expected: Each claim has a linked evidence item or is removed.
2. Check each manual script's verdict block.
   - Expected: Signed, dated, inside its timebox (or split into sessions per M-02).
3. Check evidence redaction (finding 52).
   - Expected: No real clipboard payloads, paths, titles or tokens in any attachment.
4. File gaps as issues (user-approved) or record them as known-issues.
   - Expected: No silent gaps (gate 7).


## Evidence to attach

The audited bundle index itself, known-issues list. Redact paths/window titles/tokens (finding 52).

## Verdict

- Verdict: PASS / FAIL / BLOCKED (circle one)
- Notes:
- Signature:
- Date:
