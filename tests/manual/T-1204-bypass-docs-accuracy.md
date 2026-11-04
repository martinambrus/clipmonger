# T-1204 — Unsigned-bypass docs accuracy [manual-qa, QA-LEAD signoff]

- **ID / goal:** T-1204. Verify the SmartScreen (and Gatekeeper, v2) bypass docs are accurate on a clean current Windows VM; this script is also the authoring checklist for that doc.
- **Setup (box, build, profile fixture):** Clean Windows VM (current build), no prior Clipmonger install. macOS rows skipped in v1. Use a fresh test profile; SYNTHETIC clipboard fixtures only.
- **Session timebox:** 45 min per OS (M-02: cap per session; split overruns into a new session).

## Steps

1. Download the Windows zip, the setup exe and `SHA256SUMS` to the clean VM with a browser (so they carry the downloaded-file mark); verify them with the command in `docs/install-windows.md`.
   - Expected: Checksums verify before any bypass step.
2. Follow only `docs/install-windows.md` to first run, once for the installer and once for the portable ZIP.
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

- Verdict: PASS
- Notes: User-verified on Windows 10 and Windows 11; no doc/UI mismatch reported.
- Signature:
- Date: 2026-11-05
