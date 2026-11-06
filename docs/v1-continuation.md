# V1 continuation — acceptance candidate

Updated 2026-11-05 (Windows 0.1.0 pre-release; GNOME 46 Linux acceptance). Start here when continuing without the previous conversation.

## Scope and current state

The active deliverable is the recovered Windows v1/unified-UI batch. Its board has
65 tasks: 53 completed and 12 partially completed. Phase 1 is implemented; current
work is Phase 7 packaging/QA and final native acceptance. Phase 8 documentation is
implemented and being kept current. V2/v3 are future milestones, not completion
criteria for this batch; see [product scope](seg-01-product-scope.md#10-milestone-exit-criteria).

The batch is not a signed-off cross-platform v1 release. The wider release program
in [seg-11](seg-11-testing-qa.md#5-release-gates-per-milestone-and-per-patch) includes
more than the ordinary automated suite. Do not describe native Windows testing
alone as sufficient to publish.

## Source and artifacts

- Delivered main before this documentation update: a8dc22c1737b6c7bb2ba9cab844171207c3a83d4.
- Combined application source checkpoint: 3757fcf13e1162b66cae2fadc1ab707217a3fe9a on packaging-qa-20261103,
  [draft PR #41](https://github.com/martinambrus/clipmonger/pull/41). Since 9d1e15a it adds 2dabd38
  (Windows tray-opened popup takes keyboard focus) and 3757fcf (Alt-Tab destination documented as a
  known limitation). These two commits are local until pushed.
- It includes the helper-registration work from [draft PR #37](https://github.com/martinambrus/clipmonger/pull/37)
  and failed-paste work from [draft PR #39](https://github.com/martinambrus/clipmonger/pull/39).
  Those drafts are not separately delivered or native-accepted.
- Windows test packet: C:\Temp\clipmonger-review-65b2b2a-9f0460cb. Use the files named in its SHA256SUMS
  and the build-specific WINDOWS-TESTING.md. This is a local draft, not a release. Its packages
  predate 2dabd38; the guided session runs unpackaged 2dabd38 binaries (see below).
- The packet contains Windows and Linux portable ZIPs and an unsigned Windows
  NSIS setup. Portable ZIPs carry a portable marker and full license notices;
  Linux also carries the GNOME extension and compiled settings schema.
- Rebuilds or source corrections require a new source identity and package hashes.
  Never silently substitute an older dist-release ZIP or a test-only daemon.

## Completed software QA and limits

For application build 65b2b2a9b8c78cbea179a8fcd9dbf4fba2b2c33f, the complete
Linux workspace/all-targets/all-features run recorded **1,461 passes, zero failures
and 11 ignored tests**. Formatting, Linux and Windows GNU workspace checks and
strict Clippy, and dependency policy passed, with fresh independent reviews.
The later 9d1e15a checkpoint changes only the generated Linux portable instructions
and release notes; Rust, dependencies and all six release binaries are unchanged.
That correction passed independent review, synthetic package controls and actual
regenerated Linux/bundle checks. Its exact-head automated review is clean; query
live CI before delivery. CodeQL neutral or an in-progress Rust analysis is not
an all-five-checks pass. The application PR remains draft.

For 2dabd38 (UI crate only): formatting, strict Clippy for Linux and Windows GNU,
and the clipmonger-app-ui tests (560 passed, 1 ignored) passed. The full workspace
suite was not rerun; daemon and CLI sources are unchanged since 65b2b2a.

The GNOME extension build and 34 tests passed on unchanged extension/dependency
inputs; a later core-only search correction did not rerun that unchanged build.
Ignored tests and platform guards are not native acceptance. The successful full
suite verified owned-process cleanup, private-namespace unmounting, backing
removal and unchanged inherited outer state. An earlier launcher interruption
remains recorded as NOT PASS; it is not included as a successful run.

The six normal release binaries were built for Linux x86_64 and Windows GNU
x86_64. Actual package payloads, license notices, compiled GNOME schema, embedded
uninstall helpers and combined checksums were checked. Linux CLI help passed;
its rebuilt bytes are unchanged. The rebuilt Windows CLI help and status checks
passed from the physical C-drive test folder. The GUI was restarted with the
dedicated test profile for guided acceptance; its visible behavior and remaining
native matrix require operator results. CLI load and capability self-reports
do not establish installer, UAC, compositor or full native acceptance.

Synthetic Windows profile-removal components passed default/silent preservation,
selected deletion, identity/target refusal and verified NTFS junction rejection.
They do not prove the real NSIS checkbox/confirmation/recovery path. Positive file
symbolic-link cases remain unverified after privilege refusal 1314.

The additional release-mode 10,000-clip search calibration found p95 192.49 ms
against the initial 50 ms target. Bounded selected-metadata reads reduced it to
**45.83 ms p95** (50 samples; mean 38.31 ms, maximum 47.75 ms) on this machine.
The CPU rasterizer measured **0.527 ms p90**, below its 2 ms budget. The final
test-only assertion corrections preserved the measured production code exactly.
These local measurements do not establish native popup latency, other validation
boxes, idle CPU/RAM budgets or the 24-hour soak.

## Remaining recovered tasks

All twelve remain unfinished until their actual acceptance conditions pass.
A delivered source change does not close a native gate.

| ID | Remaining acceptance |
| --- | --- |
| #19 | Alt+Tab destination is a documented known limitation (3757fcf); tray focus fixed (2dabd38) and guided tray/focus/lifecycle checks passed. Remaining: DPI/monitor changes and physical-display acceptance. |
| #22 | Installed autostart enable/disable, canonical executable path and logon/reboot; portable writes no Run value. |
| #26 | Installer/uninstaller, default-preserving profile UI, recovery, interoperability, manual matrix and release record. |
| #41 | GNOME 46 X11 and Wayland passed 2026-11-05 (see [Linux acceptance](#linux-acceptance-on-gnome-46-2026-11-05)). Remaining: portal rebind on a portal desktop (KDE Plasma 6, GNOME 48+) and the other desktops. |
| #49 | CPU runner: native lifecycle, minimized startup, tray, DPI and monitor changes. Linux (GNOME 46 Wayland, 2026-11-06): display scale 100% -> 200% -> 100% while running kept the popup and Settings sharp, sized and placed at the pointer, paste working, the UI process alive; tray, minimized startup and lifecycle passed in the GNOME acceptance. Remaining on Linux: monitor plug/unplug on a two-monitor desktop (user, planned). Windows remains. |
| #59 | Real GUI/daemon log output and visible failure diagnostics. Linux (GNOME 46 Xorg, 2026-11-06): a monitor or hotkey that cannot start now logs once per failure streak plus its recovery, instead of every 2 s (4,501 repeats had filled the bounded log); a paste whose target window closed shows a visible error. Windows remains. |
| #62 | Windows opt-out markers, capture-pump recovery and wide-image capture through to stored PNG. |
| #67 | Failed-paste rollback, clipboard owner/sequence checks and Control release through real native paths. Linux X11 (2026-11-06): a failed paste chord restores the saved clipboard while it is still ours, and every pressed key is released even when a later key event fails (unit-tested); a paste into a closed target fails visibly and leaves the clipboard untouched (user-verified). Windows remains. |
| #70 | Actual capture/hotkey startup failure and recovery on Windows and Linux. Linux: a crashed daemon is restarted by the UI (3a5aee1) and a refused popup chord recovers within 5 s (GNOME 46 Xorg). On a private Xvfb (2026-11-05), a daemon started before its display came up, and one whose X server died and restarted, recovered capture, the popup hotkey and paste caps (re-probed once the display answers) with no restart. Windows start-up failures remain. |
| #71 | Oversized captures produce the correct user-visible stub on the actual backend/UI. Linux (GNOME 46 Xorg, 2026-11-06): oversized text and images give one "Too large" stub; a stub reported by both the X11 watcher and the extension within 2 s is stored once. Known limit: when the copying app quits, GNOME's clipboard manager re-owns the selection and a second stub can follow. Windows remains. |
| #72 | Real privacy-marker/fingerprint behavior and bounded wide-image handling. Linux (2026-11-06): a 20000x40 image is stored intact and shown in the popup; large X11 images (browser copies) no longer store an empty or tail-only PNG (INCR end detection fixed, PNG signature checked), and a browser image plus its HTML is one row. GNOME Wayland (2026-11-06): a browser image copy is one row, images over 512 KB paste through the extension (pulled in 256 KB chunks), the image is pasted rather than its HTML, and a paste no longer adds a duplicate row (Photopea briefly shows its previous paste first, its own behaviour also without Clipmonger). Windows remains. |
| #74 | Same-user UAC registration, cancellation/alternate-account refusal, ACL/pinning/task readback, cleanup and logon/helper lifecycle. |

## Resume point and possible reboot

Alt+Tab from the focused popup lands on the most recent ordinary window (B), not
the one before it (A). This is accepted as a known limitation: Windows reserves
Alt+Tab (RegisterHotKey fails with 1409), low-level hooks did not help, and a
post-switch foreground redirect worked in a probe but not in the app. Do not
reopen it without a new approach. The popup still dismisses without pasting.

Guided Windows pass on 2dabd38 (2026-11-04, dedicated QA profile, Windows
10.0.26300): W01-W07, W09 and W10 PASS; W08 (Scroll Lock) SKIPPED because the
keyboard has no Scroll Lock key. Results with notes are in
`/home/zathrus/clipmonger-work/continuation/RESULTS-2dabd38.csv`.

T-409 and T-410 (2026-11-04): T-409 legs 1-10 and all T-410 steps PASS; the
`clipmonger-cli show-popup` half of T-409 leg 5 first FAILED (CLI-opened popups
had no target) and PASSED on 527ad07; T-409 legs 11-17 not run. The first
installed run on 65b2b2a exposed a helper-task readback bug (scheduler returns
account names, registrar expected the SID); fixed on `packaging-qa-20261103` in
2623046 and re-run from an installer built on 65b2b2a plus that fix. Verdicts are
in the T-409/T-410 files on that branch; rows in the same results CSV.

Done since: CLI-opened popup targeting (527ad07), the dropped elevated paste
(8c4e597, below) and the popup/Settings UI batch from the pass findings
(c9918d0 through f99f45e: prominent paste errors, footer off the list, simpler
list paging, multiline preview cap `popup.preview_lines`, enum dropdowns,
portable Start on login disabled, Settings filter across tabs, inline
validation errors, popup sized to its clips and the work area, readable error
colours, full-width rows with tooltips). The user's guided Windows retest of
the batch passed on 2026-11-04.

Next: the Windows-only 0.1.0 pre-release ([notes](release-notes-0.1.0.md)) is
built from 5a5f4e1. T-1204 (install guide on Windows 10 and Windows 11) passed,
user-verified on 2026-11-05. Then T-409 legs 11-17 and the remaining tasks in
the table above.

The PC may reboot before testing resumes. Treat all prior PIDs, running apps,
WSL sessions and temporary directories as historical. Recheck Git status and
actual process paths; never stop a reused PID. Verify packet files and binary
hashes, restore missing owned test files or rebuild if needed, and relaunch the
same dedicated profile. Do not assume C:\Temp survived cleanup. Preserve test
history and restored Notepad tabs; have the user copy fresh disposable text
before startup capture. The agent handles build/start/restart while the user
performs visible checks. No background diagnostic needs to remain running.

Local operational handoff: `/home/zathrus/clipmonger-work/continuation/REBOOT-HANDOFF.md`
and `CURRENT.md` (both predate 2026-11-04; this page wins). Earlier observations are in
`packaging-qa/windows-guided-acceptance-1/session-state.json`; source, packages,
reviews and diagnostics are under `packaging-qa/popup-alt-tab-1` within the same
persistent work directory. Recheck the 5 GB backing-drive and 4 GiB memory
reserves before new heavy checks; old measurements are not reboot evidence.

## Windows operator handoff

The user performs the visible Windows checks while the agent handles builds,
starts and restarts. Start with the packet's WINDOWS-TESTING.md:
portable fresh profile and disposable text windows; then Settings/keyboard/focus;
then installed/UAC cases, and logon/display changes in separate sessions.
The exact candidate and evidence must accompany every result.

The active guided session reuses its dedicated profile at
C:\Temp\clipmonger-review-9635310-b879a2ee\profile-portable-qa with 2dabd38
binaries in its portable-qa folder (previous 65b2b2a binaries in portable-qa\prev-65b2b2a).
Autostart is disabled and `popup.restore_clipboard = false` is now explicitly
saved after testing both choices. Preserve that test history when restarting;
do not substitute the working user profile.

Guided Windows checks on the preceding 9635310 build passed tray presence, hotkey
opening, ASCII and Unicode capture/paste (including Japanese and emoji), normal
restoration under its old default, and tray Exit with both owned processes gone.
These dated observations are preserved; repeat clipboard preference checks on
the current rebuilt candidate.

On a1dc017, the operator confirmed the new default in a standard Notepad window:
pasting an older selected entry and then Ctrl+V produced that selected entry
again. The initial checkbox was unchecked. The user then applied restore=true
and verified normal restoration, restarted, reset it to false and verified the
normal keep-selected behavior. The intermediate checked state after restart was
not separately quoted. These results cover normal text only; explicit restart
presentation, elevated helper, failure rollback and rich clipboard fidelity
remain separate checks.

Current Windows acceptance criterion (user decision, 2026-11-03): default
`clipboard_restore` delivery keeps the selected clip on the clipboard, replacing
the previous contents (`popup.restore_clipboard = false`). Restoring the previous
clipboard is opt-in, and saved explicit true/false choices must survive restart.
Verify both standard and elevated-helper delivery on the rebuilt candidate; older
dated restoration passes do not validate this new default.

Versioned acceptance scripts:
[T-905](../tests/manual/T-905-window-matrix.md),
[T-906](../tests/manual/T-906-inventory-walk.md),
[T-907](../tests/manual/T-907-keyboard-only.md),
[T-908](../tests/manual/T-908-focus-interplay.md),
[T-409](../tests/manual/T-409-uac-matrix.md),
[T-410](../tests/manual/T-410-autostart-reboot.md),
[T-1204](../tests/manual/T-1204-bypass-docs-accuracy.md) and
[T-1117](../tests/manual/T-1117-evidence-bundle-audit.md).
Historical dated passes apply only to their original build; the current CPU
runner and hardened registrar need their own results.

Keep existing apps, the working profile and original clipboard intact. Use only
synthetic content. An explicit custom QA profile does not make the default roaming
profile disposable. Uninstall profile deletion stays unchecked unless testing a
separately prepared disposable default profile/account. Do not automate UAC,
reboot/logoff, permissions, global policy changes or window-station repairs.

The old no-input UI coordinator is HOLD: UI startup/reconnect may spawn a sibling
daemon and daemon.stop does not itself quit the UI. Do not reuse that coordinator
or infer that --headless disables capture. Clipboard/window-station errors 5/183
are STOP: record and hand off; do not explore or switch stations.

## Resolved: dropped clipboard paste into elevated windows (2026-11-04)

State: fixed on this branch and verified on the Windows 10 VM. Branch
diag/paste-clipboard-20261104 was for diagnosis only; never merge it, and
delete it.

Symptom: a popup paste (Ctrl+Alt+H, pick, Enter) sometimes inserted nothing,
although the clip was on the clipboard afterwards (2 of 7, and 3 of 10, in
manual runs). Seen on a Windows 10 Pro 21H2 (19044.1806) VMware VM under the
built-in Administrator, where dcb5a96 lets the elevated daemon paste directly,
and once on the Windows 11 host in an elevated Notepad through the helper.

Cause: the daemon's own capture monitor. `clipboard_restore` writes the clip
and presses Ctrl+V about 2 ms later. The write's `WM_CLIPBOARDUPDATE` woke the
monitor, which opened the clipboard to enumerate and fingerprint it, and the
daemon then read the payloads, before `suppress_capture` dropped the write as
our own. When the target's `OpenClipboard` for Ctrl+V landed while the monitor
held the clipboard, it failed silently and nothing was pasted.

Evidence: round 1 (diag log) ruled out timing, focus, menu mode and stuck
modifiers. An external poller of `GetOpenClipboardWindow` and the sequence
number then showed every manual failure with the daemon holding the clipboard
right after the write and no Notepad read. A write-then-Ctrl+V harness into
Notepad, 30 trials per arm, failed 25 with the daemon and VMware Tools running,
29 with only the daemon, 0 with only VMware Tools, 0 with neither, and 0 with
the daemon when Ctrl+V came 50 ms after the write. VMware Tools clipboard
sharing is not involved.

Fix: the monitor gets a `SelfWriteProbe` that peeks, without consuming, at the
self-write registry (`SuppressionRegistry::expects_sequence`, wired through
`DaemonState::expects_self_write`). A sequence the daemon finalized as its own
write is not read for `SELF_WRITE_DEFER` (150 ms). The read then happens as
before and the capture boundary still suppresses it, so history behaviour is
unchanged; the poll deadline is capped at the deferral end so the read happens
well inside the 2 s suppression TTL. Other clipboard changes are read at once.
Unit tests cover the deferred wake, the idle fallback and the registry peek.

Verification: the release daemon built natively on the VM (Rust 1.99.1
x86_64-pc-windows-gnu, WinLibs GCC 16.2 for SQLite and dlltool) pasted 20 of
20 popup picks into elevated Notepad. The poller saw Notepad read the
clipboard 3-9 ms after each write and no daemon read in that window. On the
VM the backend-windows session_acceptance tests t402, t405 and
t_show_owned_window_restores_minimized, and the daemon perm_daemon_acceptance
tests d4 and d8, fail the same way with and without the fix.

Not addressed: the monitor's idle fallback still enumerates and fingerprints
the clipboard on each quiet poll (`monitor.poll_ms`, default 500 ms) to catch
WSLg/RDP copies that do not bump the sequence number. That can rarely collide
with any application's paste, and so can other clipboard managers reading our
write. Neither was observed here.

Building on Windows natively works: install rustup with the
x86_64-pc-windows-gnu host and a MinGW-w64 GCC (WinLibs UCRT) on PATH, then
`cargo build --release --locked -p clipmonger-daemon --bin clipmonger-daemon`
(and likewise `-p clipmonger-app-ui --bin clipmonger`, `-p clipmonger-app-cli
--bin clipmonger-cli`). To install, quit Clipmonger, make sure no clipmonger
process remains, copy the exes into C:\Program Files\Clipmonger and start it
again.

## Linux acceptance on GNOME 46 (2026-11-05)

User-run checks on Ubuntu 24.04, GNOME 46, development builds of the head named
per row; recorded results, not signatures.

| Test | Session | Result |
| --- | --- | --- |
| Popup hotkey change, refusal, toggle, focus, placement (ADR-004 items 1, 3, 4) | Xorg, Wayland | Pass (404f204, 4a08cda, 642b193, 5badd58) |
| Popup paste (T-607 GNOME rows, T-508 step 2) | Xorg: X11 engine, restore on/off; Wayland: extension injects Ctrl+V, else a "press Ctrl+V" notification | Pass after 8de631a |
| Permanent-clip hotkeys | Xorg (daemon grab), Wayland (extension, eabadf8) | Pass, live rebind |
| T-702 install, capture text/image/files, extension off/on | Wayland | Pass after 0452f30: one row per copy; capture pauses while the extension is off |
| T-703 Shell restart (Alt+F2 r x3) | Xorg | Pass: the extension reconnects, the daemon keeps running |
| T-705 image/file-only changes, denylist, password hint, overhead | Wayland, Xorg | Pass; idle 2.6% Shell CPU, 26 copies in 3 min 10.9% |
| T-508 tray, denylist, autostart enable/disable over reboots | Xorg, Wayland | Pass after 2a7a2e2 (tray Resume label) |

Found and fixed on the way: duplicate rows from the X11 watcher (charset twins,
trailing NUL, Latin-1 text, XWayland mirror), a denylist miss when the popup took
the focus before the copy was seen, and a Shell freeze from hashing a 3 MB image
in JavaScript on every poll (045fb7c: native hashing, once per clipboard change).
Terminals paste with Ctrl+Shift+V: both the X11 engine and the extension send it
for known terminal apps (verified in GNOME Terminal on Xorg and Wayland). On Xorg
the engine waits 200 ms after writing the clipboard, since GTK3 apps drop a paste
made while their owner-change TARGETS query is in flight. Known limit: GNOME
reports no keybinding conflicts (a duplicate of a Shell shortcut takes it over).

## Linux acceptance on GNOME 50 (2026-11-06)

Automated run in the `cm-ubuntu-2604-gnome` VM (Ubuntu 26.04.1, GNOME Shell 50.1,
Wayland): real key presses with `virsh send-key`, absolute pointer moves with QMP
`input-send-event`, checks by screenshot, clipboard read-back and the store; the
user started the root apps.

| Test | Result |
| --- | --- |
| Extension loads (metadata lists 50) | Pass |
| Popup hotkey, placement at the pointer, size fitted to the clips | Pass after the GNOME 50 session fix |
| Capture: text, images, a 20000x40 image, a 1 MB image, an oversized stub | Pass, one row each |
| Terminal paste (Ptyxis, Ctrl+Shift+V) | Pass |
| Paste of a 1 MB image (chunked pull), no echo row | Pass, clipboard bytes equal the stored PNG |
| Permanent-clip hotkey (Ctrl+Alt+2 into Ptyxis) | Pass after the GNOME 50 session fix |
| Password hint, then the copying app quits | Pass after the fix: one skip, nothing stored |
| Extension off/on: capture pauses, then resumes | Pass |
| Root apps: paste into and copy from a root Wayland app; paste into a root X11 (XWayland) app | Pass; no privilege barrier like Windows UIPI. A `--standalone` root app has no app id, so app-id denylist entries do not match it |
| UI under repeated clipboard changes and pastes | Pass after replacing smithay-clipboard |

Found and fixed: GNOME 50 removed `Meta.is_wayland_compositor`, which silently
disabled popup placement and the extension's permanent hotkeys; a copy with the
password hint was stored once GNOME's clipboard manager re-offered its text
without the hint after the copying app quit (now remembered by hash, in the
extension and the daemon); the UI's Wayland clipboard (smithay-clipboard) died
with a libwayland "not a valid new object id ... data_offer" protocol error after
clipboard changes, taking the UI down (egui-winit now uses arboard only, through
XWayland). GNOME 47-49 and 51 are not covered by this run.

## Linux acceptance on GNOME 51 (2026-11-06)

Run in the `cm-ubuntu2610-gnome` VM (Ubuntu 26.10, GNOME Shell 51.0, Wayland,
glibc 2.44) with the same harness; the user ran the root app.

| Test | Result |
| --- | --- |
| Extension loads (metadata lists 51) and connects | Pass |
| Capture: text, a 2.4 MB image (chunked through the extension) | Pass |
| Popup hotkey and placement at the pointer, thumbnails | Pass |
| Auto-paste into Ptyxis (Ctrl+Shift+V) | Pass after 7133b1a |
| Permanent-clip hotkey with Ctrl+Alt still held | Pass |
| Password hint, then GNOME's clipboard manager re-offers the text | Pass, nothing stored |
| Paste of the 2.4 MB image (chunked pull), clipboard size matches | Pass |
| Root app (GTK window via sudo): popup paste and permanent hotkey | Pass |
| Daemon stops at logout | Pass |

Found and fixed: GNOME 51 removed `Clutter.get_default_backend()`, so the
extension's virtual keyboard could not be created and every popup paste fell
back to the "press Ctrl+V" notification (7133b1a; the fallback still serves
GNOME 46, rechecked on the user's host).

## Linux acceptance on GNOME 48 (2026-11-06)

Run in the `cm-debian13-gnome` VM (Debian 13.7, GNOME Shell 48.7, Wayland,
glibc 2.41) with the same harness and the HEAD build of b8b647c; the terminal was
gnome-terminal (Debian 13 has no Ptyxis). The user is not in sudoers, so the root
app ran through `su`; the user started it.

| Test | Result |
| --- | --- |
| Extension loads (metadata lists 48), ACTIVE, `extension_connected` | Pass |
| Capture: two texts, a 2.4 MB image (chunked), a 20000x40 image, 1 MB, 6.8 MB and 11.4 MB images, an 18.8 MB image as one "Too large" stub | Pass, one row each (one miss, see below) |
| Popup hotkey, placement at the pointer, image thumbnail | Pass |
| Auto-paste into gnome-terminal (filter, Enter) | Pass, exact bytes |
| Permanent-clip hotkey with Ctrl+Alt still held | Pass, no stray bytes |
| Password hint, then GNOME's clipboard manager re-offers the text | Pass, nothing stored |
| Paste of the 2.4 MB image (chunked pull) | Pass, clipboard bytes equal the stored blob (SHA-256) |
| Extension off/on: capture pauses, then resumes | Pass; on re-enable the first poll stores the text still on the clipboard |
| 30 rapid copies, then 5 popup pastes | Pass, 30 rows, pastes in order, UI and daemon stay up |
| Root app (GTK window via `su`): popup paste, permanent hotkey, copy out of it | Pass |
| Daemon and UI stop at logout | Pass |

Open, not reproduced: in the first multi-image run, a 1 MB image copied 8 s
after the 20000x40 image produced no extension capture report and no row. The
same sequence repeated three times, and alone, captured every image; 6.8 MB and
11.4 MB images were read within the extension's 1.5 s read timeout, so that
timeout is not the obvious cause. The CLI has no command to delete a permanent
clip.

## Linux acceptance on KDE Plasma 6.8 Wayland (2026-11-06)

Run in the `cm-fedora45-kde` VM (Fedora 45, KDE Plasma 6.8.2, KWin Wayland;
Plasma 6.8 has no X11 session) with the same harness as GNOME 50; the user
answered the KDE dialogs, started the root app and checked each fix.

| Test | Result |
| --- | --- |
| Capture without an extension (ext-data-control): text from Wayland and X11 apps, images, a 3.2 MB image | Pass |
| Popup hotkey through the GlobalShortcuts portal | Pass after registering the app id `com.martinambrus.clipmonger` and sending XDG trigger names (6af0607) |
| Auto-paste from the popup (KWin fake input): KWrite, Konsole (Ctrl+Shift+V), an image and a 3.2 MB image into LibreOffice Writer | Pass (6af0607) |
| Permanent-clip hotkeys, including chord edits (KDE asks once per new chord) | Pass after binding them in the portal session and waiting for released modifiers (c20444c) |
| No duplicate row after a paste | Pass after writing exactly the registered formats (c20444c) |
| Password hint, then Klipper re-offers the text without it | Pass, nothing stored |
| Klipper refill after the copying app quits | Pass, ignored (6af0607) |
| Tray menu, applications-menu entry, autostart at login (UI starts, then a fresh daemon in the session) | Pass |
| Root app (GTK window run with `sudo -E`): popup paste and permanent hotkey | Pass; KWrite and Kate refuse to run as root |

Known limits: KWin places the popup itself (centre of the screen, not at the
pointer). KWin 6.8 did not ask before granting fake input; the daemon's hidden
`.desktop` entry declares it in case a later KWin does. The X11 hotkey thread is
off on KWin Wayland, because KWin forwards modifier chords to X11 clients and the
X11 route then hit KWin's "control input devices" prompt.

## Linux acceptance on KDE Plasma 5.27, X11 and Wayland (2026-11-06)

Run in the `cm-debian12-kde` VM (Debian 12, Plasma 5.27.5, glibc 2.36) in both
sessions; the user checked the fixes, the root app and autostart.

| Test | X11 | Wayland |
| --- | --- | --- |
| Binaries on glibc 2.36 (built against 2.39; the two newer symbols are weak) | Pass | Pass |
| Capture: text, 1.9 and 3.2 MB images, Spectacle screenshots (Qt offers ~35 targets) | Pass after eda251e | Pass |
| Popup hotkey and auto-paste (Konsole with Ctrl+Shift+V, KWrite, LibreOffice) | Pass | Pass after d7c10c0 |
| Paste of a ~300 KB image into LibreOffice (X11 INCR send) | Pass after eda251e | n/a |
| Permanent-clip hotkey with modifiers still held | Pass | Pass after d7c10c0 |
| Klipper refill (old text must not jump above a newer image; a refill is kept when the original could not be read) | Pass after eda251e | Pass |
| Root app: popup paste and permanent hotkey | Pass | Pass |
| Autostart at login, hotkeys without a dialog | Pass | Pass after d7c10c0 |
| Portal restart while the daemon runs | n/a | Pass after d7c10c0 |
| Switching sessions: the old daemon stops, the new one has the new session's environment | Pass after d7c10c0 | Pass after d7c10c0 |

Found and fixed: X11 INCR send never selected PropertyChange on the requestor,
so every paste above ~256 KB hung (all X11 desktops); capture refused Qt images
for advertising more than 32 targets; the Klipper marker has an `x-os/` prefix
on X11; Plasma 5.27's portal registers shortcuts only from `CreateSession`
and, being xdg-desktop-portal 1.16, takes the app id from the systemd unit
name (autostart file renamed); KWin 5 script paths and names; the daemon
outlived logouts when started from a terminal; stale Xvfb locks made every
X11 test skip silently.

## Windows acceptance in KVM VMs (2026-11-06)

Automated run on two VMs converted from the user's VMware machines, with real
input through QEMU (keys, absolute pointer, UAC clicks on the secure desktop),
screenshots, SSH, and helpers in `~/clipmonger-vm-tools/` (deploy, launch in
the desktop session, clipboard copy/read, test editor/console). Candidate:
HEAD with 1693674.

- `cm-win11`: Windows 11 build 21996, standard admin account `martin` (UAC
  split token), portable test folder `C:\clipmonger-test`.
- `cm-win10`: Windows 10 22H2 (19045), built-in Administrator only (full
  token, every window elevated), installed copy in `C:\Program Files\Clipmonger`
  upgraded to the candidate for the autostart leg.

| Test | Windows 11 | Windows 10 |
| --- | --- | --- |
| Capture and paste of Unicode text (Japanese, emoji, accents), bytes identical | Pass | Pass |
| Pasting an older entry keeps it on the clipboard (`restore_clipboard = false`) | Pass | Pass |
| `restore_clipboard = true`: older entry pasted, previous clipboard back after the dwell | Pass | Not run |
| Image capture (small, 7.7 MB re-encoded PNG) and paste into WordPad; PNG, CF_DIB and CF_DIBV5 checked byte for byte | Pass | Pass |
| Paste into a PowerShell console | Pass | Pass |
| Permanent-clip hotkey with Ctrl+Alt still held | Pass | Pass |
| Password-manager format (`ExcludeClipboardContentFromMonitorProcessing`) not stored | Pass | Pass |
| Elevated window: "enable in admin windows" prompt, UAC, helper; popup paste | Pass | n/a (all windows elevated, direct paste) |
| Permanent hotkey into an elevated window | Pass after 1693674 | n/a |
| Tray menu, settings window, permanent-clip delete with confirmation | Pass | Pass |
| Autostart: setting writes the Run entry; reboot; starts at login and pastes | Pass | Pass (installed copy) |
| Tray Quit ends both processes | Pass | Pass |

Found and fixed: a per-clip hotkey pressed in an elevated window pasted
nothing, because GetAsyncKeyState reads all keys as up across the integrity
boundary; the elevated helper now waits for the modifiers itself (1693674).

Notes: an existing Run entry pointing at an installed copy is kept when a
portable/test build runs with autostart enabled; WordPad draws a scaled-down
large noise image very dark (a direct paste without Clipmonger looks the same);
a permanent chord equal to the popup chord degrades the hotkey capability
until removed (seen on the Windows 10 profile, whose popup chord is
Ctrl+Alt+J); the settings text "not on Wayland" for permanent-clip hotkeys is
outdated since the GNOME extension and the KDE portal support them.

## Other release gates: do not silently waive

- Linux desktop acceptance: GNOME 46 on Xorg and Wayland passed on 2026-11-05
  (T-508 GNOME rows, T-607 GNOME rows, T-702, T-703, T-705; see
  [Linux acceptance](#linux-acceptance-on-gnome-46-2026-11-05)). Still open:
  [T-508](../tests/manual/T-508-x11-real-de.md) on Xfce (KDE X11 passed on
  Plasma 5.27, see
  [Plasma 5.27](#linux-acceptance-on-kde-plasma-527-x11-and-wayland-2026-11-06)),
  [T-606](../tests/manual/T-606-wayland-compositor-matrix.md),
  [T-607](../tests/manual/T-607-paste-spike-acceptance.md) on wlroots (KDE
  Plasma 6.8 Wayland passed, see
  [KDE](#linux-acceptance-on-kde-plasma-68-wayland-2026-11-06)),
  T-704 for GNOME 47 and 49 (46, 48, 50 and 51 passed, see
  [GNOME 48](#linux-acceptance-on-gnome-48-2026-11-06),
  [GNOME 50](#linux-acceptance-on-gnome-50-2026-11-06) and
  [GNOME 51](#linux-acceptance-on-gnome-51-2026-11-06)). The portal rebind
  (ADR-004 item 2) passed on KDE. X11 permanent
  hotkeys now honor `popup.restore_clipboard` (8de631a, verified on Xorg).
- Linux release licensing for the scanner-generated blur protocol bindings (winit
  through wayland-protocols-plasma 0.3.12, from KDE's LGPL-2.1-or-later
  `blur.xml`) was settled by the project owner on 2026-11-05: those bindings are
  accepted under LGPL-2.1-or-later, the rest stays MIT, and the public source meets
  the relink terms. The notice and license text are in
  [licenses/wayland/](../licenses/wayland/README.md); Linux packages must ship them.
  The published 0.1.0 `.deb` predates the notice, so the next Linux package must
  carry it. Re-check on winit or wayland-protocols-plasma updates.
- T-1103's 24-hour daemon/monitor/retention soak has no current completion receipt.
  A search microbenchmark is not that soak.
- T-1105's specified million-case toolchain fuzz run and source-based coverage
  calibration have no completion receipts. The 80% floor is specified after v1
  calibration; neither ordinary tests nor a deferred floor proves calibration.
  Macro-engine work remains v2; record the v1 IPC scope separately.
- The source-level feature-to-test audit is recorded in [the index](v1-qa-traceability.md).
  Close its listed evidence gaps and final release-record traceability review.
  Actual per-box positive/degraded caps goldens, declared-platform/toolchain floors,
  remaining performance calibration, signed manuals and final release evidence audit
  also remain release gates.
- No tag, publication, draft-to-ready transition or application merge follows
  merely from the passing software suite. Resolve findings and native gates,
  obtain fresh exact-head review/CI where required, then integrate deliberately.

## Evidence and next-agent procedure

Detailed local receipts are under /home/zathrus/clipmonger-work/packaging-qa;
continuation/task-board.json in the sibling clipmonger-work tree is the detailed
65-task ledger. These local paths supplement this checked-in handoff; the source,
open PRs, remaining cases and limits above are sufficient to restart elsewhere.

1. Read this page, [STATUS](STATUS.md), [plan](plan.md) and the relevant manual script.
2. Check actual Git/worktree/PR heads and preserve unknown edits. Keep the combined
   candidate and original security worktrees; do not reset an unfinished merge.
3. Reconcile exact-source evidence. Reuse valid results; rerun affected checks after
   actual source/environment changes. Never count ignored or blocked native cases as passes.
4. Run heavy/Cargo work serially with two jobs, at least 4 GiB MemAvailable and
   a monitored decimal 5 GB reserve on the backing Windows C drive. Current local
   resource runner and full-suite isolation instructions are in the external CURRENT.md;
   inspect them before using the shared target. The current namespace runner is
   namespace-pidfd-repair-3/namespace_runner.py (SHA-256
   be2f3ab0295b47cfdeca2485bdc23a60f9d7abac68449a8e0bb3e37c437c7f19);
   the backing-space coordinator remains run-check-v5.py. Keep both frozen and
   preserve failed receipts; an unknown ownership/resource failure is HOLD. Do not run unguarded duplicate suites.
5. Fix confirmed failures with focused regressions, review the correction, then run
   the complete feature suite and refresh binaries/packages/hashes when relevant.
6. Record per-case results, source, environment, duration, redacted evidence and
   cleanup. Update this handoff and the plan before marking whole tasks done.
