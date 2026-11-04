# V1 continuation — acceptance candidate

Updated 2026-11-03. Start here when continuing without the previous conversation.

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

- Delivered main before this documentation update: 6a515555b4a956e934eed58975d99e04215d311a.
- Combined application candidate: 9635310125daede2d842bd67cb790492552bbba9 on packaging-qa-20261103,
  [draft PR #41](https://github.com/martinambrus/clipmonger/pull/41).
- It includes the helper-registration work from [draft PR #37](https://github.com/martinambrus/clipmonger/pull/37)
  and failed-paste work from [draft PR #39](https://github.com/martinambrus/clipmonger/pull/39).
  Those drafts are not separately delivered or native-accepted.
- Windows test packet: C:\Temp\clipmonger-review-9635310-b879a2ee. Use the files named in its SHA256SUMS
  and the build-specific WINDOWS-TESTING.md. This is a local draft, not a release.
- The packet contains Windows and Linux portable ZIPs and an unsigned Windows
  NSIS setup. Portable ZIPs carry a portable marker and full license notices;
  Linux also carries the GNOME extension and compiled settings schema.
- Rebuilds or source corrections require a new source identity and package hashes.
  Never silently substitute an older dist-release ZIP or a test-only daemon.

## Completed software QA and limits

For the combined candidate above, the complete Linux workspace/all-targets/all-features
run recorded **1,456 passes, zero failures and 11 ignored tests**. Formatting,
Linux and Windows GNU workspace checks and strict Clippy, and dependency policy
also passed. The final candidate has a clean independent review, a clean
exact-head automated PR review, and all five CI checks succeeded.

The GNOME extension build and 34 tests passed on unchanged extension/dependency
inputs; a later core-only search correction did not rerun that unchanged build.
Ignored tests and platform guards are not native acceptance. The successful full
suite verified owned-process cleanup, private-namespace unmounting, backing
removal and unchanged inherited outer state. An earlier launcher interruption
remains recorded as NOT PASS; it is not included as a successful run.

The six normal release binaries were built for Linux x86_64 and Windows GNU
x86_64. Actual package payloads, license notices, compiled GNOME schema, embedded
uninstall helpers and combined checksums were checked. Linux CLI help passed;
its rebuilt binary is byte-identical to that checked copy. Windows CLI help from
a physical C-drive directory passed on the preceding a401911 package; the rebuilt
Windows CLI needs the operator load check. These are not GUI, capture, paste, UAC,
installer or compositor acceptance.

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
| #19 | Unified UI: tray, focus, popup lifecycle and real display behavior. |
| #22 | Installed autostart enable/disable, canonical executable path and logon/reboot; portable writes no Run value. |
| #26 | Installer/uninstaller, default-preserving profile UI, recovery, interoperability, manual matrix and release record. |
| #41 | Real X11/Wayland/GNOME: live hotkey changes, portal rebind, extension/push and activation behavior. |
| #49 | CPU runner: native lifecycle, minimized startup, tray, DPI and monitor changes. |
| #59 | Real GUI/daemon log output and visible failure diagnostics. |
| #62 | Windows opt-out markers, capture-pump recovery and wide-image capture through to stored PNG. |
| #67 | Failed-paste rollback, clipboard owner/sequence checks and Control release through real native paths. |
| #70 | Actual capture/hotkey startup failure and recovery on Windows and Linux. |
| #71 | Oversized captures produce the correct user-visible stub on the actual backend/UI. |
| #72 | Real privacy-marker/fingerprint behavior and bounded wide-image handling. |
| #74 | Same-user UAC registration, cancellation/alternate-account refusal, ACL/pinning/task readback, cleanup and logon/helper lifecycle. |

## Windows operator handoff

The user takes over real Windows testing. Start with the packet's WINDOWS-TESTING.md:
portable fresh profile and disposable text windows; then Settings/keyboard/focus;
then installed/UAC cases, and logon/display changes in separate sessions.
The exact candidate and evidence must accompany every result.

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

## Other release gates: do not silently waive

- Linux desktop acceptance needs actual X11/Wayland/GNOME environments:
  [T-508](../tests/manual/T-508-x11-real-de.md),
  [T-606](../tests/manual/T-606-wayland-compositor-matrix.md),
  [T-607](../tests/manual/T-607-paste-spike-acceptance.md), and T-702 through T-705.
  Pre-existing X11 permanent hotkeys restore the previous clipboard regardless
  of `popup.restore_clipboard`; this needs a separate fix and Linux acceptance
  before claiming the Windows preference behavior on X11.
- Linux release licensing is unresolved for scanner-generated blur protocol output
  used by winit through wayland-protocols-plasma. The crate's MIT manifest and the
  successful dependency scan do not determine the file-specific output boundary.
  Obtain a supported licensing determination consistent with the existing policy;
  do not assume a notice alone settles it, waive policy or rewrite Wayland by inference.
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
