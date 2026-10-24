# macOS backend (crate `backend-macos`, v2)

Scope (Q5 approved): direct distribution (NO Mac App Store sandboxing),
ARM64 + Intel, floors per D-012 (11.0+ ARM64 / 10.12+ Intel, raised if a
crate requires it). Daemon runs in the USER session context (LaunchAgent):
The arboard audit documented that `generalPasteboard` MAY return NULL
under launchd (dep since dropped, C-01; behavior note kept)
daemon modes — a system-daemon install is therefore FORBIDDEN; detect and
refuse with a clear error.

## 1. Monitor — `ClipboardMonitor` (changeCount polling)

- CopyQ-verified pattern (`MacClipboard` source): snapshot
  `[NSPasteboard generalPasteboard] changeCount`, poll every 500 ms
  (tolerance 5 s), fire on change. Same defaults + `ARSCLIPX_` env
  overrides (names reserved in seg-03 §6).
- Privacy (F-104): skip when data offers
  `application/x-nspasteboard-concealed-type` (CopyQ-verified).
- Origin: Clipboard only (no primary selection on macOS).

## 2. Transfer — `ClipboardTransfer` via direct AppKit (objc2)

Direct AppKit via objc2 0.6 + app-kit/foundation 0.3 (crates.io-verified,
MIT — review C-01; replaces the former arboard transfer role), same
NSPasteboardType surface CopyQ uses: read STRING/RTF/HTML/TIFF/files,
write into 5-type slots (Slot Rule, seg-03 §1.1), order + polling-free
`changeCount` monitoring consumed by §1. Types without an enumerated
constant live in the passthrough registry (§3.1).

| Canonical (seg-03) | macOS (AppKit constants, CopyQ-verified) |
|---|---|
| text/plain;charset=utf-8 | `NSPasteboardTypeString` |
| text/html | `NSPasteboardTypeHTML` |
| image/png (normalized) | `NSPasteboardTypeTIFF` → decode → PNG (image crate `tiff`) |
| text/uri-list | file URLs (`NSPasteboardURLReading…`, `public.file-url` per CopyQ) |
| text/rtf | RTF type (VERIFY constant at implementation) |
| passthrough | `x-os/<uti>` (CopyQ-verified examples: `public.url`, `public.file-url`) |

- Write quirk (CopyQ): UTF-16 text MUST omit BOM
  (`public.utf16-plain-text` without BOM; some apps choke otherwise).
- UTI string literals behind the constants: VERIFY at implementation
  (task §10.1); code MUST use the AppKit constants, never hardcode UTIs.

## 3. Hotkeys — `HotkeyManager`

- Via `global-hotkey` crate (macOS support verified in README; manager on
  the main thread with a running event loop). Shared chord syntax,
  shared conflict→Degraded behavior (seg-04 §3).

## 4. Paste engine — `PasteEngine` via enigo

- enigo 0.6 (MIT; macOS mouse+text verified): Unicode `text()` preferred,
  Cmd+V chord for rich formats (chord API per seg-04 §12.5), same
  ClipboardRestore sequencing as Windows.
- **Accessibility permission** (REQUIRED for input simulation): enigo
  checks and prompts (Permissions.md, inspected). Our flow: pre-flight
  permission check at daemon start → if denied, UI banner + guided steps
  (System Settings → Privacy & Security → Accessibility → enable) →
  paste reports `Degraded (needs Accessibility)` until granted. Settings
  deep-link URL = VERIFY at implementation (else plain instructions).
- Re-focus precedes chords (seg-02 §6; review C-05): restore focus to
  the captured target via the platform API (verify exact call + AX
  interplay at implementation).
- TCC identity (review finding 41): the DAEMON requests/holds
  Accessibility (it simulates); it ships INSIDE the stable `.app`
  bundle identity (fixed `CFBundleIdentifier`). CI signs deterministically
  (ad-hoc; Developer ID deferred per D-018); permission persistence
  across upgrades is TESTED (T-808) and any re-prompt becomes a
  documented known-issue (never silent).
- Restore is GUARDED (seg-02 §6): only when our temp value still current
  (changeCount unchanged AND content-hash match); user copies win (finding 9).

## 5. Tray — menu-bar extra via tray-icon

- `tray-icon` macOS backend (inspected README rule: event loop + icon
  creation on the MAIN thread). Tray is hosted by app-ui (seg-02 §1); this section defines macOS
behavior only. Menu = seg-09 §7 (sole definition, review M-06).
- Ship inside a `.app` bundle with `LSUIElement=1` (no Dock icon) +
  `NSHighResolutionCapable` (systray-README-verified keys); document the
  bundle layout in seg-12 (unsigned-Gatekeeper consequences there too).

## 6. Autostart — `AutostartManager`

- PRIMARY: LaunchAgent plist
  `~/Library/LaunchAgents/<bundle-id>.plist` with `RunAtLoad` (file-based
  like our Linux `.desktop`: version-proof, no API deprecation risk).
  Plist keys + load/unload commands = VERIFY at implementation (§10.4).
- CONSIDERED + NOT chosen: `SMAppService` (macOS 13+ only — conflicts
  D-012 Intel floor); `LSSharedFileList` Login Items (CopyQ uses it, but
  Apple deprecated it — avoid new adoption).

## 7. Foreground app (v2, AutoSwitch F-304)

- Bundle-id + name of the frontmost app via running-application APIs
  (VERIFY exact API at implementation; no extra permission needed for
  id/name; AX-backed fallback acceptable since paste needs AX anyway).

## 8. Caps mapping

Normal: all `Full`, `primary_selection=false`.
Degraded: Accessibility denied (paste), hotkey conflict, system-daemon
context detected (refuse-to-run as monitor with explanation).

## 9. Segment tests (T-8xx, need macOS runners — see seg-11)

- T-801 `[unit]` MIME/UTI map incl. passthrough + TIFF→PNG normalize.
- T-802 `[unit]` plist write/parse roundtrip + RunAtLoad toggle.
- T-803 `[unit]` no-BOM UTF-16 writer vectors.
- T-804 `[integration]` roundtrip all §2 formats (needs session).
- T-805 `[integration]` hotkey register/fire + conflict inject.
- T-806 `[integration]` enigo paste into scratch window (AX pre-granted
  on the test user) + denied-permission simulation → Degraded.
- T-807 `[manual-qa]` first-run AX prompt flow; unsigned build Gatekeeper
  behavior (feeds seg-12); reboot autostart persist. QA-LEAD signoff.
- T-808 `[manual-qa]` Accessibility persistence across package upgrade
  (grant → upgrade → still granted?) + re-prompt known-issue check.

## 10. Verification tasks

1. RTF pasteboard constant + any missing UTI literals.
2. AppKit-constant-only rule respected in code review.
3. Confirm `generalPasteboard`-NULL guard + refusal message.
4. LaunchAgent plist keys + load/unload commands.
5. Settings deep-link URL (or drop the link, keep instructions).
6. Foreground-app API choice.
