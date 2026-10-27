# T-1204 — Unsigned-bypass docs accuracy [manual-qa, QA-LEAD signoff]

- **ID / goal:** T-1204. Verify the SmartScreen (and Gatekeeper, v2) bypass docs are accurate on a clean current Windows VM; this script is also the authoring checklist for that doc.
- **Setup (box, build, profile fixture):** Clean Windows VM (current build), no prior Clipmonger install. macOS rows skipped in v1. Use a fresh test profile; SYNTHETIC clipboard fixtures only.
- **Session timebox:** 45 min per OS (M-02: cap per session; split overruns into a new session).

## Steps

1. Download/copy the Windows zip to the clean VM; verify `SHA256SUMS`.
   - Expected: Checksums verify before any bypass step.
2. Follow only the draft bypass doc to first run.
   - Expected: Every doc step matches the actual SmartScreen UI; no undocumented clicks.
3. Note any doc/UI mismatch with screenshots.
   - Expected: Doc corrected in the same session; mismatches listed.
4. Confirm the doc states checksums are integrity-only, not signatures.
   - Expected: Trust-scope sentence present and accurate (seg-12 §8).
5. QA-LEAD signoff on the corrected doc.
   - Expected: Signature below.


## Evidence to attach

Annotated screenshots per step, corrected doc diff, signoff. Redact paths/window titles/tokens (finding 52).

## Verdict

- Verdict: PASS / FAIL / BLOCKED (circle one)
- Notes:
- Signature:
- Date:
