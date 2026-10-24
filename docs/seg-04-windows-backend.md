# Windows backend (crate `backend-windows`)

Target floor: Windows 10+ / Server 2016+ x64 per D-012 (all APIs below
exist since Vista/2000, so the floor only gates CI/QA).
Daemon and UI ALWAYS run as standard user (medium integrity). v1 scope:
everything in this segment except §10 (v3).

## 1. Monitor — `ClipboardMonitor`

- Hidden message-only window (`HWND_MESSAGE`) registered via
  `AddClipboardFormatListener(hwnd)` (MS Learn: Vista+, User32).
  `WM_CLIPBOARDUPDATE` → capture event.
- Dedup with `GetClipboardSequenceNumber()` (CopyQ-proven: ignore repeats).
- Privacy opt-outs (CopyQ-verified, F-104): skip capture when the data
  offers `ExcludeClipboardContentFromMonitorProcessing`,
  `CanIncludeInClipboardHistory=0`, `CanUploadToCloudClipboard=0`, or
  `Clipboard Viewer Ignore`. Count skip in `privacy_skips`, store nothing.
- `poll()` adapts the window-message pump: run pump on the backend thread,
  translate messages to `CaptureEvent`s. `Clipboard` origin only (no
  primary selection on Windows).

## 2. Transfer — `ClipboardTransfer`

- Backend: DIRECT OS APIs via windows-sys 0.61 + clipboard-win 5.4
  (review C-01: arboard audited — no RTF, no arbitrary formats, no
  atomic multi-format writes — so it cannot satisfy this contract).
  The MIME map below is ours, implemented directly:

| Canonical (seg-03) | Windows read | Windows write |
|---|---|---|
| text/plain;charset=utf-8 | CF_UNICODETEXT | CF_UNICODETEXT (rely on OS CF_TEXT synthesis — verify at implementation, M-18) |
| text/html | `HTML Format` (parse StartHTML/EndHTML fragment offsets!) | `HTML Format` with valid header + fragment comments |
| text/rtf | `Rich Text Format` | `Rich Text Format` |
| image/png | CF_DIB/CF_DIBV5 → decode → PNG normalize | CF_DIB (from PNG) + PNG |
| text/uri-list | CF_HDROP → file URLs | CF_HDROP |
| anything else | registered format name → `x-os/<name>` passthrough | same back |

- `HTML Format` header parsing (Version/StartHTML/EndHTML/StartFragment/
  EndFragment byte offsets) MUST be a pure function with `[unit]` vectors
  (T-401): this format is a classic bug farm.
- Multi-format `write` MUST be one `OpenClipboard`/`EmptyClipboard`/
  `SetClipboardData×n`/`CloseClipboard` sequence (atomic). Retry open up
  to 5×20 ms (clipboard is a shared lock; failure → `Timeout` error).

## 3. Hotkeys — `HotkeyManager`

- Via `global-hotkey` crate (`RegisterHotKey`, `WM_HOTKEY`, MOD_WIN/
  MOD_CONTROL/MOD_ALT/MOD_SHIFT/MOD_NOREPEAT per MS Learn).
- Chord syntax (shared, see seg-09): e.g. `win+v`, `ctrl+alt+1`.
- Conflict (`ERROR_HOTKEY_ALREADY_REGISTERED`): keep running, mark that
  hotkey `Degraded` in caps with the conflicting owner unknown, surface
  in UI + tray tooltip (ArsClip-parity behavior: never crash on conflict).
- Scroll-Lock monitoring toggle (F-105): poll `GetKeyState(VK_SCROLL)`
  on the backend thread; document that some keyboards lack the key.
  VERIFY at implementation (review M-18): `GetKeyState` is thread-local —
  confirm correct thread/API (`GetAsyncKeyState` or hook alternative).

## 4. Paste engine — `PasteEngine`

Strategy order per paste job (target = foreground window at job start):

All simulation via the `enigo` crate (MIT, v0.6; adopted under D-006):
`text()` for Unicode injection, key chords for Ctrl+V (verify chord API,
task §12.5), mouse where macros need it. enigo documents the same UIPI
rule this design routes around (§5). ORDERING (review M-07): target
integrity is detected FIRST (seg-02 §6 flow); elevated targets take
strategy 3 ONLY — strategies 1–2 are non-elevated-only (SendInput fails
SILENTLY under UIPI, so blind attempts are forbidden). All strategies
run AFTER the target re-focus step (seg-02 §6; review C-05). Unicode
limit = `popup.unicode_limit` (default 64 KiB, review M-18).
1. **UnicodeInject** (text ≤64 KiB, non-elevated target): enigo text.
   No clipboard clobber. Preferred.
2. **ClipboardRestore** (rich formats, or target ignores Unicode inject):
   save → write clip → enigo Ctrl+V chord → best-effort restore 500 ms
   later. Document the flicker tradeoff.
3. **ElevatedRoute** (target integrity HIGH, §5): forward job to helper
   (helper runs the SAME enigo calls, elevated).
4. Report `Unavailable` only if all applicable strategies fail.
- Restore is GUARDED (seg-02 §6): after ClipboardRestore, restore the
  saved value ONLY if the clipboard still equals our temp write
  (sequence number unchanged AND content-hash match). The user's newer
  copy always wins (review finding 9). Self-write suppression registry
  (seg-02 §6) prevents re-capture of temp/restore writes (finding 8).

- `SendInput` is UIPI-gated (MS Learn: inject only into equal/lesser
  integrity; failure is SILENT — no error distinguishes UIPI block).
  Therefore: ALWAYS detect target integrity FIRST (§5), never SendInput
  blindly and hope.
- Respect pressed-modifier state (`GetAsyncKeyState` guidance in
  SendInput Remarks): release-safe sequencing (mods down → keys → mods up).

## 5. Elevated helper — on-demand paste into admin windows (D-011)

Total Commander-style: NOTHING runs elevated until the user actually
pastes/macros into a high-integrity window.

- **Detection**: `GetForegroundWindow` → `GetWindowThreadProcessId` →
  `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` →
  `GetTokenInformation(TokenElevation)` (+ integrity SID on need).
  Medium target → normal path, NO prompt, NO helper. High target →
  helper path.
- **Spawn**: same binary, hidden subcommand
  `arsclipx-daemon __elevated-worker <random-pipe-name>`, launched via
  `ShellExecuteEx` with the **`runas` verb** (MS Learn: "Launches an
  application as Administrator. UAC will prompt for consent or admin
  credentials"). Daemon shows its own "admin paste needed" notice FIRST
  so the UAC prompt is never a surprise. NO secret travels in argv
  (review finding 28): the pipe name is a random 128-bit value
  (unguessable rendezvous, not a credential). Protected-path rule
  (review C-03): the helper spawns ONLY from a non-user-writable install
  path (Program Files install); portable mode → elevated route is
  `Unavailable` (documented, F-504) — no elevation from swappable binaries.
- **Lifetime** (per user decision): helper lives until daemon exit or
  helper crash; UAC consent is per process launch (MS Learn UAC: "each
  application that requires the administrator access token must prompt
  for consent" — no system timeout on a running elevated process), so
  re-prompt happens ONLY on respawn. Daemon↔helper heartbeat every 15 s;
  missed ×3 → helper assumed dead → next elevated job re-spawns (+prompt).
  Self-exit (review C-03/O-01): the helper terminates itself on pipe
  disconnect/daemon stop — never orphaned across upgrades; the installer
  stops the daemon first and the helper follows.
- **Channel (reverse-connect, no DACL surgery)**: daemon creates a
  same-user named pipe (explicit current-user-SID DACL); helper (high)
  connects DOWN to it (MIC permits write-down). Mutual authentication
  WITHOUT argv secrets (review finding 28): random pipe name + BOTH sides
  verify the peer (pipe peer PID → image path MUST equal our own exe;
  daemon additionally checks the helper token is high-integrity).
  Mismatch → drop + audit alert.
- **Narrow job protocol**: `{job_id, target_pid, op, payload}` where op ∈
  `send_input_batch` ONLY — input-only by design (clipboard writes need
  no elevation; the daemon does them itself). Helper pins the target by
  open PROCESS HANDLE (no PID-reuse race), re-validating foreground +
  high-integrity per job, else refuses. Helper CANNOT
  read history, config, or DB — it receives one job's bytes only.
  Gesture rule (review C-03): elevated jobs require a fresh single-use
  gesture token minted at hotkey/popup-trigger time; generic IPC calls,
  CLI invocations, and macros CANNOT mint it (a same-user attacker
  driving `paste.execute` gets standard-user paste at most).
- **UIAccess alternative (review C-03):** NOT chosen — reportedly
  requires signing + secure location (VERIFY if D-011 is ever reworked),
  which conflicts with the D-018 $0 pipeline; revisit together with paid
  signing.
- **Refusals**: user cancels UAC → job fails `Denied("elevation refused")`
  → caps mark `paste: Degraded (elevation refused)` + UI hint with
  "try again" (re-prompts). Never retry-spawn in a loop.
- **Audit**: daemon logs every helper spawn/job/refusal (op hash, target
  exe name, ts, result) to the audit log (seg-03 §7 policy family).
- Helper NEVER autostarts, NEVER monitors clipboard, NEVER touches the DB.

## 6. Tray — `TrayController`

`tray-icon` crate default Windows backend (`Shell_NotifyIcon`, NIM_ADD/
MODIFY/DELETE). Menu = seg-09 §7 (sole definition — review M-06). Icon variants: normal / monitoring-off / attention
(elevation needed, sync error). Tooltip shows caps one-liner.

## 7. Autostart — `AutostartManager`

`HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value
(`REG_SZ`, quoted exe path + `--minimized`). Query/enable/disable per
trait; query MUST also read the OS disabled/override state
(StartupApproved-style override — verify exact key at implementation,
review M-18). Portable mode (seg-03 §6) MUST skip autostart entirely. Helper
is NEVER in autostart (D-011).

## 8. Foreground app (v2, AutoSwitch F-304)

`GetForegroundWindow` + `GetWindowText` + process exe basename via
`QueryFullProcessImageName`. Return `AppId { exe, title_hash }`. Works
standard-user for medium targets; high targets' titles still readable
(UIPI blocks input, not `GetWindowText`).

## 9. Caps mapping (seg-02 §7)

Normal: all `Full`, `primary_selection=false`.
Degraded cases: hotkey conflict (§3), clipboard-open contention storm,
elevation refused (§5), UAC-disabled-box with admin targets (prompt
impossible → paste-to-admin `Unavailable` with explanation).

## 10. v3: Jumplist (F-505)

`ICustomDestinationList` recent/frequent clips + tasks (Show popup, Toggle
monitoring). Design placeholder: implement against documented COM API at
v3 time; MUST NOT affect v1 architecture (separate module).

## 11. Segment tests (T-4xx)

- T-401 `[unit]` HTML Format header parse/serialize vectors (fragments,
  missing offsets, UTF-8/UTF-16 CuttingEdge cases) + malformed/overlong-header fuzz vectors.
- T-402 `[unit]` opt-out format filter matrix (all §1 formats ± values).
- T-403 `[unit]` helper job protocol roundtrip + refusal cases (bad token,
  wrong target_pid, non-foreground).
- T-404 `[unit]` chord parser + conflict-error mapping.
- T-405 `[integration]` clipboard roundtrip all §2 formats (needs session).
- T-406 `[integration]` hotkey register/fire/unregister + conflict inject.
- T-407 `[integration]` UnicodeInject + ClipboardRestore into a scratch
  test window (own window, no UAC).
- T-408 `[integration]` target-integrity detection (medium self-check;
  high-target case via a fixture IF harness permits, else manual-qa).
- T-409 `[manual-qa]` FULL UAC matrix: first elevated paste prompts;
  helper survives app run without re-prompt; cancel → Degraded + retry;
  daemon restart → re-prompt; audit log entries present. QA-LEAD signoff.
- T-410 `[manual-qa]` autostart enable/disable/reboot-persist; portable
  mode leaves no Run value.
- T-411 `[integration]` guarded-restore race (user copy lands mid-paste →
  user value preserved, no restore).
- T-412 `[integration]` rapid clipboard churn + open-contention storm
  (retry path, no deadlock, contention counter capped).
- T-413 `[integration]` gesture-token enforcement (tokenless/generic-IPC
  elevated request → refused; fresh-trigger request → served).
- T-414 `[integration]` protected-path rule (portable-layout daemon →
  elevated route `Unavailable`, no spawn attempt).

## 12. Verification tasks

1. RESOLVED (see seg-02 §13.1): arboard DROPPED (C-01); direct
   Win32 transfer (§2) + custom monitor (§1).
2. Confirm reverse-connect pipe medium↔high flow on Win10 + Win11.
3. Confirm `runas` verb behavior + consent UI strings for T-409 script.
4. `windows-sys` feature list to pin (Win32_Foundation,
   UI_WindowsAndMessaging, System_DataExchange, Security, Shell…).
5. enigo key/chord API shape (Ctrl+V etc.) + per-monitor DPI behavior
   + physical-position preference (review M-08).
6. Review M-18 set: correct Scroll-Lock polling API, StartupApproved
   query key, CF_TEXT synthesis behavior.
