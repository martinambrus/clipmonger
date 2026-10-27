# T-702 — GNOME install + capture [integration, Ubuntu box]

- **ID / goal:** T-702. Install/enable the extension, capture text+image+files, fire the hotkey, disable/re-enable with caps flips.
- **Setup (box, build, profile fixture):** Ubuntu 24.04, GNOME 46. BLOCKED-UBUNTU until that box exists. Extension zip from the Linux artifact; fresh profile. Use a fresh test profile; SYNTHETIC clipboard fixtures only.
- **Session timebox:** 60 min (M-02: cap per session; split overruns into a new session).

## Steps

1. Install the extension zip and enable it (gnome-extensions / Extension Manager).
   - Expected: Extension ACTIVE; no Shell log errors.
2. Start the daemon; run `clipmonger-cli status`.
   - Expected: caps extension_connected=true.
3. Copy synthetic text in a GNOME app.
   - Expected: Clip appears in history (`clipmonger-cli list`).
4. Copy a synthetic image, then synthetic files in Files.
   - Expected: Image and file clips appear with correct formats.
5. Press the popup hotkey chord.
   - Expected: Hotkey fires (daemon event/popup path).
6. Disable the extension; copy text again.
   - Expected: caps flips to disconnected; capture pauses honestly, daemon stays up.
7. Re-enable the extension and copy again.
   - Expected: Capture resumes; caps flips back.


## Evidence to attach

caps dumps per leg, Shell log excerpt, history JSON. Redact paths/window titles/tokens (finding 52).

## Verdict

- Verdict: PASS / FAIL / BLOCKED (circle one)
- Notes:
- Signature:
- Date:
