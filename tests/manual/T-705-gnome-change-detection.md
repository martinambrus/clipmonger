# T-705 — GNOME change detection [integration, Ubuntu box]

- **ID / goal:** T-705. Verify image-only and file-only change detection, privacy denylist, password-hint handling, and Shell overhead sampling.
- **Setup (box, build, profile fixture):** Ubuntu GNOME box, T-702 passing; synthetic image/files; a denylisted app and a password field fixture. Use a fresh test profile; SYNTHETIC clipboard fixtures only.
- **Session timebox:** 45 min (M-02: cap per session; split overruns into a new session).

## Steps

1. Copy a synthetic image with no text change.
   - Expected: Image-only change is captured.
2. Copy synthetic files with no text change.
   - Expected: File-only change is captured.
3. Copy from a denylisted (password-manager) app.
   - Expected: Nothing stored; privacy counter/log reflects the skip.
4. Copy from a password field (hint path).
   - Expected: Treated as sensitive: not stored as a normal clip.
5. Sample Shell CPU/RSS during 5 min of copy activity.
   - Expected: Overhead recorded as calibration data; no session jank observed.


## Evidence to attach

History JSON per leg, privacy log excerpt, overhead sample numbers. Redact paths/window titles/tokens (finding 52).

## Verdict

- Verdict: PASS / FAIL / BLOCKED (circle one)
- Notes:
- Signature:
- Date:
