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

## Other release gates: do not silently waive

- Linux desktop acceptance: GNOME 46 on Xorg and Wayland passed on 2026-11-05
  (T-508 GNOME rows, T-607 GNOME rows, T-702, T-703, T-705; see
  [Linux acceptance](#linux-acceptance-on-gnome-46-2026-11-05)). Still open:
  [T-508](../tests/manual/T-508-x11-real-de.md) on KDE and Xfce,
  [T-606](../tests/manual/T-606-wayland-compositor-matrix.md),
  [T-607](../tests/manual/T-607-paste-spike-acceptance.md) on KDE/wlroots,
  T-704 for GNOME 47-49 and 51 (46 and 50 passed, see
  [GNOME 50](#linux-acceptance-on-gnome-50-2026-11-06)) and the portal rebind
  (ADR-004 item 2). X11 permanent
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
