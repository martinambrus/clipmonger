# T-607 — Wayland paste spike acceptance [manual-qa]

- **ID / goal:** T-607. Per-avenue paste verdicts per compositor; v1 fallback is write+toast (W-F7) — assert the honest path, including injection-success rows where available.
- **Setup (box, build, profile fixture):** Ubuntu GNOME/KDE (plus Sway/Hyprland rows if available). Synthetic target apps. Use a fresh test profile; SYNTHETIC clipboard fixtures only.
- **Session timebox:** 45 min per compositor (M-02: cap per session; split overruns into a new session).

## Steps

1. Paste into a standard app on this compositor; record the avenue (injection vs write+toast).
   - Expected: Avenue matches caps; content lands or toast explains the manual step.
2. If write+toast: follow the toast instruction and paste manually.
   - Expected: Clipboard holds the clip; manual paste succeeds.
3. Paste a large/unicode clip.
   - Expected: Limits enforced honestly; no truncation without notice.
4. Paste after switching focus mid-operation.
   - Expected: Stale-target error, never a mis-paste (pairs T-908).


## Evidence to attach

Per-compositor avenue table, caps dumps, toast screenshots. Redact paths/window titles/tokens (finding 52).

## Verdict

- Verdict: PASS / FAIL / BLOCKED (circle one)
- Notes:
- Signature:
- Date:
