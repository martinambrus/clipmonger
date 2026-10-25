# ADR-001 — Out-of-process IPC plugin system (Notepad++-inspired UX)

Status: Final (user-accepted 2026-10-25; v1-spec reopen authorized by user).
Decision: D-022.

## Decision

Clipmonger gains a user-facing plugin system with Notepad++-style ease of
use but different mechanics: plugins are OUT-OF-PROCESS programs speaking a
capability-gated subset of the seg-02 §9 IPC protocol — never in-process
native code. Milestones: v1 freezes reservations only (directory, manifest
schema, method/permission names); v2 ships the host + management UI +
sample plugin + docs; v3+ evaluates a curated plugin list.

- Discovery: `<profile>/plugins/<id>/plugin.toml` + executable, scanned at
  daemon start (mirrors Notepad++ `plugins/<name>/<name>.dll` drop-to-install).
- Manifest (TOML): `id`, `version`, `ipc_range`, `entrypoint`, `capabilities[]`,
  `locales[]` (ADR-002 namespaced strings). Invalid manifest = plugin ignored
  with a logged error, never a daemon failure.
- Capabilities (all default-deny, granted at install/enable with a UI prompt):
  `clips.read`, `clips.write` (via daemon, subject to normal suppression +
  dedupe), `paste.execute` (foreground target only, explicit user trigger),
  `events.capture` (post-policy-filter events ONLY — F-104 drops and denied
  content are never delivered), `ui.contribute` (popup/tray items),
  `config.read` (own keys only). No network, no store/backend access, no
  token/secret access — ever.
- Lifecycle: install (folder drop or config-UI import) → permission prompt →
  enable; disable keeps files (Notepad++ Deactivate); remove deletes the
  folder. Per-plugin crash/slow isolation: a dead plugin flips its own status,
  never the daemon. Version skew via the existing `hello` negotiation.
- Distribution: v1/v2 manual only (folder + import). The curated list
  (signed index, Notepad++ Plugin List-style) is a v3+ evaluation item under
  the v3 design-reopen gate; it MUST fit the D-018 $0 pipeline or stay out.

## Reason

Notepad++ proves the UX (one-click install/update/remove, broad community
capabilities), but its DLL mechanics are wrong here: Windows-only, in-process
crashes take down the host, native ABI versioning is brittle, and any store
access would violate seg-02 §1 (daemon-only writer) and §14 (threat model).
Out-of-process IPC reuses frozen, tested machinery (§9 auth/versioning/
backpressure), works on all OSs, isolates crashes, and admits non-Rust
plugins. The GNOME relay (seg-07) already proves the pattern.

## Non-goals

No in-process/native plugins; no plugin access to the clip store, backends,
tokens, or audit internals; no auto-run of untrusted code without explicit
install + enable; no curated list before the v3 design-reopen gate.

## Affected segments

seg-00 (D-022, glossary), seg-01 (F-801/802, §8, milestones), seg-02
(§1/§2/§9/§14/§15, T-212/213), seg-03 (config keys, audit), seg-09
(Plugins tab, contributions), seg-12 (packaging notes), plan.md (steps 8/9, R-012).

## Migration / test impact

None pre-v1 (nothing shipped). Tests: T-212 `[unit]` manifest +
permission vectors; T-213 `[integration]` hostile-plugin isolation
(crash, stall, over-privilege attempts); v2 T-906 inventory walk covers the
Plugins tab. Post-v1, capability/manifest changes need a new ADR.
