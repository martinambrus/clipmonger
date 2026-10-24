# Testing + QA (process owner: the human QA lead, D-017)

## 1. Roles (D-017)

- **User**: hands-on Linux (own GNOME 46 box) + Windows; Ubuntu-version
  matrix via local VMs (VirtualBox/VMware/other); per-release acceptance
  signoff. Time goes to judgment (feel, edge cases), not grids.
- **Colleague**: hands-on macOS validation + macOS signoff.
- **Agents**: write every automated suite; run CI + agent-driven VMs
  (QEMU/KVM on this box); attach full evidence (logs, screenshots,
  caps snapshots) to every release record.

## 2. Pyramid + gates

`[unit]` (cargo test, in-process, ms-fast) → `[integration]` (real OS
services: sockets, Xvfb, nested compositors, localhost daemons) →
`[e2e]` (built binaries on real/VM OSs, scripted scenarios) →
`[manual-qa]` (human scripts, §6). PR gate: fmt + clippy + unit + fast
integration. Nightly: full non-24h integration + e2e. Soak (T-1103) runs as a
rolling/weekly/RC job, never an overlapping ordinary nightly (finding 48). Release gate (§5):
everything green + signed checklists.

## 3. Test index

Per-segment suites own their area: T-2xx (arch/IPC) in seg-02 §12,
T-3xx (model/store/macros) seg-03 §9, T-4xx Windows seg-04 §11, T-5xx
X11 seg-05 §9, T-6xx Wayland seg-06 §10, T-7xx GNOME-ext seg-07 §10,
T-8xx macOS seg-08 §9, T-9xx UI seg-09 §13, T-10xx sync seg-10 §7.
This segment owns the CROSS-CUTTING T-11xx:

- T-1101 `[e2e]` install → run → uninstall per OS (no leftovers outside
  profile; portable mode leaves the host untouched).
- T-1102 `[e2e]` upgrade: vN profile (DB+config+blobs) opens cleanly on
  vN+1; corrupt-DB recovery path fires correctly (seg-03 §3);
  downgrade (older binary, newer schema) refuses cleanly (pairs T-314).
- T-1103 `[e2e]` 24 h soak: daemon + monitors + hourly retention; assert
  perf budgets (§8) + zero log-errors + DB integrity at end (rolling/
weekly/RC cadence per §2, finding 48).
- T-1104 `[e2e]` full user journeys (capture→popup→paste→permanent→macro
  (v2)→sync (v3)) on each OS, scripted via CLI + simulated input.
- T-1105 `[integration]` fuzz macro Stage-1 + IPC parser (toolchain
  fuzzing; VERIFY harness at implementation) — 1 M cases, zero panics.
- T-1106 `[integration]` backup-ZIP cross-version restore matrix.
- T-1117 `[manual-qa]` evidence-bundle audit: every release claims a
  FINITE, reviewable bundle — each manual script ran inside its
  timeboxed window with evidence attached (review M-02).

## 4. Harnesses (all chosen, versions verified at implementation)

- `cargo test` workspace (unit + in-process integration).
- Xvfb headless X11 (`Xvfb :99 -screen 0 1600x1200x24`, seg-05).
- Nested headless Wayland compositor for T-604/605 (seg-06 §11.6).
- Agent-driven QEMU/KVM VMs on this box (VERIFY KVM availability at
  implementation) for scripted Linux-matrix evidence.
- User-driven VirtualBox/VMware Ubuntu VMs for hands-on matrix.
- Hosted CI runners for Windows + Linux in v1 (macOS from v2;
  macOS compile-only optional in v1 — finding 2). Provider = VERIFY at
  implementation against requirements: required OSs, x64 (+arm64 in v3),
  artifact uploads, nightly schedules. No self-hosted runners in v1.
  Also required: a VM-in-CI / nested-virt story for T-6xx/T-7xx if
  hosted runners can't nest (review M-02).

## 5. Release gates (per milestone AND per patch)

1. Full pyramid green on the matrix (§4) with attached evidence.
2. `caps.get` golden files refreshed + diffed per validation box —
   incl. NEGATIVE-case goldens: blocked/Degraded/Unavailable paths are
   first-class assertions, not absence-of-crash (review M-12 §11).
3. Every `[manual-qa]` script for the release scope executed + signed
   (user for Linux/Win, colleague for macOS).
4. Perf budgets (§8) met; coverage floor (§9) met.
5. Changelog + upgrade notes written; signoff record filed (§7).
6. Dependency-review: license re-check vs seg-00 reuse map + audit tool
   clean (finding 43; tool per §10.6).
7. Traceability sweep (review M-08): every F-xxx in release scope maps
   to ≥1 T-xxx; gaps filed as issues, never silent.

## 6. Manual-QA script format (agents write, humans execute)

ID / goal / setup (box, build, profile fixture) / numbered steps /
expected per step / evidence to attach (screenshot/log/caps dump — SYNTHETIC clipboard
fixtures only; redact paths/window titles/tokens, finding 52) /
verdict + notes + signature + date + session timebox (review M-02:
cap manual QA per session; overruns split into a new session).
Every `[manual-qa]` item in seg-02…
seg-10 gets a script instance per release; scripts live in
`tests/manual/<id>.md` and are versioned with the code.

## 7. Signoff + bug process

- Signoff record per release: scope, matrix results links, checklist
  verdicts, two human signatures (user: Linux/Win; colleague: macOS),
  known-issues list. No signoff = no release.
- Bug format: title, F-xxx/T-xxx refs, `app-cli caps` snapshot, logs,
  minimal repro, severity (blocker/major/minor). Regression rule: every
  fix ships its automated test (or a manual script + reason why not).

## 8. Perf budgets (INITIAL — calibrate during v1, then freeze)

Daemon idle <1% CPU and <120 MB RAM; popup trigger→rendered <100 ms p95;
capture→stored <50 ms p95; cold start <1 s (excluding integrity check on
huge DBs); 10k-clip history search <50 ms. T-1103 enforces; regressions
fail the release gate. Budgets calibrate per validation box in v1;
noisy-neighbor CI gets generous + documented margins (review M-02).

## 9. Coverage + lints + flake policy

- Toolchain source-based coverage (VERIFY flags at implementation);
  floor 80% on core/platform/ipc in v1 (+ macrolib from v2 — f49),
enforced after v1 calibration.
- `cargo fmt --check` + `cargo clippy --workspace --all-targets --all-features -- -D warnings` (matrix-adjusted per target) gate every PR
  (toolchain components, no extra deps).
- Flakes: quarantine label + tracking issue + fix-or-delete within one
  release. No blind retries; UI-timing waits need explicit caps + logs.
  Same discipline for perf budgets: a miss files an issue + a new
  test/probe, never a shrug (review M-12 §11).

## 10. Verification tasks

1. CI provider choice vs §4 requirements.
2. Coverage flags + floor calibration.
3. Fuzz harness for T-1105.
4. KVM availability on this box for agent-driven VMs.
5. Screenshot/evidence conventions for VM runs.
6. Dependency-audit tool choice (feeds release gate 6, finding 43).
