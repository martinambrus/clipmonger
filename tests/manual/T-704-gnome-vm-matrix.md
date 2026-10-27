# T-704 — GNOME version matrix 46-51 [manual-qa, signoff]

- **ID / goal:** T-704. Per GNOME major 46..51: install, prefs, version-mismatch message, crash-guard recovery. Phase-0 MetaSelection rich-format PoC rides this script.
- **Setup (box, build, profile fixture):** VMs per GNOME major 46, 47, 48, 49, 50, 51. BLOCKED-UBUNTU/VM until images exist. Use a fresh test profile; SYNTHETIC clipboard fixtures only.
- **Session timebox:** 60 min per major, table-driven (M-02: cap per session; split overruns into a new session).

## Steps

1. Install + enable the extension on this major.
   - Expected: ACTIVE, or an explicit version-mismatch message if unsupported.
2. Open extension prefs; toggle each setting.
   - Expected: Prefs persist; daemon config reflects changes.
3. Copy text/image/files; fire the hotkey.
   - Expected: Capture + hotkey work on this major.
4. Install a build declaring a mismatched shell-version.
   - Expected: Clear mismatch message; Shell does not crash.
5. Kill/ wedge the extension process if tooling allows; observe the daemon.
   - Expected: Daemon degrades to capture-paused, never hangs; recovery per docs works.
6. Run the MetaSelection rich-format PoC checklist for this major.
   - Expected: Rich-format result recorded (supported/degraded) per major.


## Evidence to attach

One filled table row per major (install/prefs/mismatch/crash-guard/PoC), caps dumps. Redact paths/window titles/tokens (finding 52).

## Verdict

- Verdict: PASS / FAIL / BLOCKED (circle one)
- Notes:
- Signature:
- Date:
