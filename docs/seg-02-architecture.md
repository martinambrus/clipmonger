# Architecture — processes, crates, traits, flows

## 1. Process topology

Four cooperating programs, one data owner:

```
+----------------+   JSONL IPC    +------------------+
|   app-ui       |<-------------->|     daemon       |
| (egui: popup,  |  query+events  | (owns store,     |
|  config, bar)  |                |  monitors, jobs) |
+-------+--------+                +---+----------+---+
        ^                             ^          ^
        | spawn/attach                | Unix sock| named pipe (win)
+-------+--------+                +---+----------+---+
|   app-cli      |                | gnome-ext (v1)   |
| (one-shots for |                | (GJS/TS in Shell:|
|  bindings,     |                |  capture+hotkey |
|  scripts, QA)  |                |  relay via IPC) |
+----------------+                +------------------+
```

- The **daemon** is the ONLY writer to the clip store. UI, CLI, and the
  GNOME extension are IPC clients. This rule MUST NOT be violated: it is
  what makes multi-backend capture + sync safe.
- **Single-daemon lock**: lockfile in the profile dir (`daemon.lock`,
  OS file locking). A second daemon MUST attach as a client or exit with
  a clear message, never double-open the DB for writing.
- **UI attach model**: `app-ui` windows (popup, config, clipboard bar) run
  in one egui/eframe process in v1 (simpler single-binary story); the IPC
  protocol MUST still be used between UI and daemon (no in-process
  shortcuts), so headless-daemon + remote-UI keeps working.
  Amended by ADR-004 (D-026): one UI per profile (`<profile>/ui.lock`); a
  second launch sends `popup.open` to the running UI and exits.
- **CLI**: one-shot commands (`show-popup`, `list`, `paste <id>`, `toggle-
  monitor`, `exec-permanent <id>`, `status`) used by compositor key
  bindings, scripts, and QA. Every command MUST also exist as an IPC call
  so UI and CLI share semantics. CLI commands: `show-popup`, `list`,
`paste <id>`, `toggle-monitor`, `exec-permanent <id>`, `purge-removed`,
`status`.
- **Elevated helper (Win, fifth role — review O-01):** spawned by the
  daemon on demand (seg-04 §5), input-only, exits with the daemon
  (self-terminates on pipe disconnect). Never autostarted, never
  shows UI, never touches the store.
  Amended by ADR-003 (D-025): Program Files builds start the helper from a
  consented logon task and it reconnects across daemon restarts; portable
  helpers still exit with the daemon.
- **GNOME extension (v1, D-013)**: in-Shell relay only (capture events +
  hotkey triggers in, paste/hotkey assists out). It MUST NOT own history;
  on disconnect the daemon marks GNOME capture `Unavailable` (see §7).
- **Tray ownership (review finding 5): `app-ui` OWNS the tray + window
  event loop.** `TrayController` is implemented ONCE in `app-ui` over
  `tray-icon`; backend segments document per-OS tray RULES (main thread,
  KSNI, bundle identity), not a second host. The daemon NEVER shows UI.
- **Launcher/attach/restart/quit (review finding 5):** the installed
  launcher binary `clipmonger` = UI process; on start it ensures the daemon
  runs (spawns `clipmonger-daemon` if the lock is free, else attaches over
  IPC). Autostart launches `clipmonger --minimized` (daemon + hidden UI
  with tray only). Tray Quit stops UI AND daemon (explicit `daemon.stop`
  + lock release). Daemon crash → UI shows reconnecting + offers restart;
  UI crash → daemon keeps running (monitoring/hotkeys continue), user
  relaunches UI which re-attaches. Headless/`minimal` = daemon + CLI,
  no tray (CLI covers control). Packaged as cooperating binaries in one
  install (seg-12), NOT one mega-binary. OS supervision (review O-01):
  systemd user unit `Restart=on-failure`, launchd `KeepAlive`, Windows
  none (UI offers relaunch; no service — session isolation). Version
  skew: UI/CLI check daemon version in `hello`; mismatch → refuse with
  "restart required" (covers upgrades over a running daemon).
  Amended by ADR-004 (D-026): the UI restarts a missing daemon itself;
  after a UI crash the next popup press or `popup.open` starts the UI.
- **Profile rendezvous (review finding 40):** clients discover the daemon
  via `<config>/clipmonger/rendezvous.json` (default profile socket path,
  protocol version; token stays in the 0600 token file, never here) or
  `--profile <name>` (registry file lists active profiles). Extension/
  CLI/UI/plugins MUST re-resolve on connect failure (never bake a stale path).
- **Plugins (v2 host per ADR-001/D-022; v1 reservations only):**
  out-of-process IPC clients like CLI/extension (one OS process per
  plugin, supervised by the daemon). Plugins speak a CAPABILITY-GATED IPC
  subset only (ADR-001); they MUST NOT touch the store, backends, tokens,
  or audit internals. A plugin crash/stall degrades that plugin alone,
  never the daemon. v1 freezes the directory (`<profile>/plugins/`),
  manifest schema, method names, and permission names — no host runs.

## 2. Cargo workspace layout

```
clipmonger/                       # D-021; APP_IDENTITY = `clipmonger`
  Cargo.toml                     # workspace + shared lints/versions
  crates/
    core/        # clip model, store, config, search, history policy,
                 # macro expander (no JS engine here), sync types (v3),
                 # plugin manifest + permission types (v2 host, frozen v1)
    platform/    # backend TRAITS + shared types + fakes (no OS code)
    ipc/         # proto (serde, no runtime) + client (std) + server (tokio)
    backend-windows/ backend-x11/ backend-wayland/ backend-macos/
    daemon/      # tokio runtime, monitor supervisor, jobs, single lock,
                 # plugin supervisor (v2; ADR-001)
    app-ui/      # eframe: popup + config + bar + tray (via tray-icon),
                 # plugin management UI (v2; ADR-001)
    app-cli/     # clap CLI over ipc client
    macrolib/    # (v2) [VAR] expansion + rquickjs engine + [RUN] runner
  gnome-ext/     # (v1, TypeScript/GJS, separate build; protocol in seg-07)
  tests/e2e/     # cross-crate black-box tests (seg-11)
```

v1 builds: core, platform, ipc, backend-windows, backend-x11,
backend-wayland, daemon, app-ui, app-cli, gnome-ext (D-013). v2 adds
backend-macos, macrolib, the plugin host (daemon+UI, ADR-001) and i18n
infrastructure (ADR-002). v3 extends core/ipc (sync) + daemon (jobs).

## 3. Foundation crates (licenses verified via crates.io API / README)

| Crate (seen v.) | License | Use | Features |
|---|---|---|---|
| serde 1.0 | MIT OR Apache-2.0 | Model + IPC + config derive | derive |
| serde_json 1.0 | MIT OR Apache-2.0 | IPC frames | std |
| tokio 1.53 | MIT | Daemon runtime, IPC server, jobs, `[RUN]` procs | rt-multi-thread, sync, time, signal, process, macros, net, io-util (fs stays v3 — review C-07; v1 needs net/io-util for socket+pipe IPC) |
| uuid 1.28 | Apache-2.0 OR MIT | Clip IDs (**v7**, time-ordered) | v7, serde |
| thiserror 2.0 | MIT OR Apache-2.0 | Typed errors per crate | — |
| clap 4.6 | MIT OR Apache-2.0 | CLI parsing (derive) | derive |
| toml 1.1 | MIT OR Apache-2.0 | Config file | parse, display, serde |
| directories 6.0 | MIT OR Apache-2.0 | OS config/cache/data dirs | — |
| sha2 0.11 | MIT OR Apache-2.0 | Dedupe keys (SHA-256) | — |
| image 0.25 | MIT OR Apache-2.0 | Decode/scale previews, PNG normalize | png, bmp, gif, jpeg, webp, tiff (NO default-formats: keep binary lean) |
| rusqlite 0.40 | MIT | Store (SQLite) | bundled, backup |
| rquickjs (v2, QuickJS-NG) | MIT | JS macros | (verify API at macrolib implementation) |
| windows-sys 0.61 + clipboard-win 5.4 | MIT OR Apache-2.0 / BSL-1.0 | Direct Win32 clipboard transfer (review C-01; arboard dropped: no RTF/custom/atomic) | Win32_Foundation, UI_WindowsAndMessaging, System_DataExchange, Security… |
| objc2 0.6 + objc2-app-kit + objc2-foundation 0.3 | MIT / Zlib OR Apache-2.0 OR MIT / MIT | Direct NSPasteboard transfer (review C-01) | NSPasteboard features as needed |
| wl-clipboard-rs 0.9 | MIT OR Apache-2.0 | Wayland copy/paste/**watch** | default |
| tray-icon (Tauri) | MIT OR Apache-2.0 | Tray + menu | ksni on Linux (no GTK), default elsewhere |
| global-hotkey (Tauri) | MIT OR Apache-2.0 | Hotkeys Win/macOS/X11 | default |
| egui + eframe | MIT OR Apache-2.0 | All windows | wgpu default; glow fallback where needed |

Pending selection (review M-11 — verify license+maintenance BEFORE use;
criteria: maintained, MIT/Apache/BSD/CC0, MSRV-compatible): ZIP read/write
crate, unicode-segmentation (grapheme previews), structured-logging facade +
rotation (§15), time/date formatting (v2 macros), hex encoding, file locking,
plist read/write (macOS), Windows registry access, D-Bus binding (seg-06).
Adopted CI tool: cargo-deny 0.20 (MIT OR Apache-2.0, verified) for the
license/advisory gate (seg-11 §9).

MSRV: latest stable at implementation start (uuid 1.28 already needs
rustc 1.89). Versions above are "seen-good"; implementation pins
compatible ranges and records them in `Cargo.lock` (committed for bins).

## 4. Platform traits (crate `platform`)

Sync where possible; async only at the daemon edge. All traits MUST have
a `Fake*` in `platform::fakes` for `[unit]` tests.

```rust
pub struct FormatOffer { pub mime: String, pub bytes_hint: Option<u64> }
pub struct CaptureEvent {
    pub offers: Vec<FormatOffer>, pub origin: CaptureOrigin,
    pub sensitive: bool,          // honor-and-drop when true (§5)
}
pub enum CaptureOrigin { Clipboard, Primary } // Primary: X11/Wayland only

pub trait ClipboardMonitor: Send {
    fn subscribe(&mut self, kinds: &[SubscribeKind]) -> Result<()>;
    fn poll(&mut self, timeout: Duration) -> Result<Vec<CaptureEvent>>;
    // poll-based (works for evented + polling backends uniformly)
}

pub trait ClipboardTransfer: Send {
    fn formats(&self) -> Result<Vec<String>>;
    fn read(&self, mime: &str, max_bytes: u64) -> Result<Vec<u8>>;
    fn write(&self, payload: &PayloadMap) -> Result<()>; // multi-mime atomic
}

pub trait HotkeyManager: Send {
    fn register(&mut self, id: HotkeyId, chord: &str) -> Result<()>;
    fn unregister(&mut self, id: HotkeyId) -> Result<()>;
    fn poll(&mut self, timeout: Duration) -> Result<Vec<HotkeyId>>;
}
// Chord grammar (review M-05; parsed ONCE in platform, shared by all
// backends + config UI): MOD[-MOD]* "+" KEY, case-insensitive.
// MOD = ctrl|alt|shift|super|win|cmd|meta (aliases: win=super on
// Win/Linux, cmd on macOS; left/right variants l-ctrl etc. optional).
// KEY = winit `Code` variant names (KeyA…KeyZ, Digit0…, F1…, Space,
// Comma… — verify exact list at implementation). Examples:
// "ctrl+shift+v", "super+alt+1". OS-reserved chords rejected with reason
// (reserved lists per OS = seg-09 §14.5). Defaults live in seg-09 §8.

pub struct PasteTarget { // captured BEFORE the popup opens (finding 7)
    pub app: AppId, pub window_title_hash: u64,
    pub handle: PlatformHandle, // HWND / X11 window / AX element / opaque
    pub captured_ms: i64,
}
pub enum PasteStrategy { // unified (review H-01)
    UnicodeInject, ClipboardRestore, ElevatedRoute, WriteAndToast,
    ExtensionAssisted, Unavailable,
}
pub trait PasteEngine: Send {
    fn strategy(&self) -> PasteStrategy;
    fn capture_target(&self) -> Result<PasteTarget>;
    fn paste_to(&self, target: &PasteTarget, clip: &ClipRef) -> Result<()>;
    // paste_to MUST revalidate the target (same app/handle, still alive);
    // stale target -> StaleTarget error, NEVER paste into the popup itself.
}

// Hosted by app-ui (see §1), implemented once over tray-icon:
pub trait TrayController: Send { /* icon, tooltip, menu, attention state */ }
pub trait AutostartManager { /* enable/disable/query per OS */ }
pub trait ForegroundApp: Send { fn active(&self) -> Option<AppId>; } // AutoSwitch (v2)
```

`PayloadMap = Vec<(mime, bytes)>`; multi-format write MUST be atomic
where the OS allows it. `sensitive=true` (or any OS opt-out signal)
means: record NOTHING, not even metadata (F-104).

## 5. Error model

Each crate defines a `thiserror::Error` enum; backend errors map into
`core::CoreError::Backend { backend, kind, message }` with kinds:
`Unavailable`, `Denied`, `Timeout`, `TooLarge`, `Unsupported`, `Io`,
`Internal`. Errors crossing IPC serialize as
`{code, backend?, message, retryable}`. Daemon MUST log full chains;
UI shows short messages + "details" expander. Errors and logs MUST
NEVER carry clip payload bytes, tokens, or full command text — hashes +
metadata only (seg-15 logging rules; review O-02).

## 6. Data flows

- **Capture**: backend `poll` → `CaptureEvent` → policy filter
  (F-103/104/404: formats, size, sensitive, ignores) → lazy `read` per
  wanted MIME → dedupe (SHA-256) → store → notify subscribers (UI/CLI/
  extension event stream).
- **Popup paste (session-based, review finding 7)**: hotkey/trigger →
  daemon captures `PasteTarget` FIRST → `popup.open` → UI lists via
  `history.list` (previews lazy) → user picks → popup dismisses →
  daemon RE-FOCUSES the target (per-OS step, seg-04/05/08; on Wayland =
  dismiss + rely on prior focus, extension-assisted where available) →
  `paste.execute(target)` → `PasteEngine::paste_to` (target revalidated;
  stale → clear error) → optional explicit re-store on top (a direct
  store call subject to normal dedupe — NOT a monitor event, f H-02).
  Amended by ADR-004 (D-026): the daemon pushes `ui.show_popup` with the
  target and the press's gesture; on Windows it raises the popup during
  the press; the pick sends `paste.execute` with that target and gesture.
- **Hotkey exec**: backend poll → `hotkey.fired` → daemon runs Permanent
  Clip (paste or macro, v2) without UI (target = foreground at fire time).
- **Macro (v2)**: expander resolves `[VAR]`s (needs popup/clipboard
  reads) → optional JS stage (rquickjs sandbox) → keystroke/`[RUN]` stage
  via backend capabilities → audit entry.
- **Extension (v1)**: ext pushes `capture.event`, pulls `history.list`;
  daemon pushes `config.changed`; heartbeats detect Shell restarts.
- **Self-write suppression (review finding 8):** every daemon-initiated
  clipboard write registers an expectation FIRST: content hashes +
  backend pre-state (Win sequence number, macOS changeCount, X11 owner
  + serial, Wayland offer token), TTL 2 s. The monitor consults the
  registry on each event: exact match → suppress once + consume; anything
  else (including a genuine user copy racing our write) is captured
  normally. The registry lives in core so dual X11+Wayland monitors share
  it. Suppression events are counted (not stored) for diagnostics.
- **Guarded restore (review finding 9):** after a ClipboardRestore paste,
  restore the saved clipboard ONLY if it still equals our temporary
  value (backend unchanged-indicator AND content-hash match). If the user
  copied meanwhile, their value WINS and no restore happens. Correctness
  depends on the GUARD, never on the delay alone (review H-02):
ClipboardRestore saves a faithful snapshot (canonical text+html+rtf+
image-png+urilist; exotic natives best-effort, documented). After
pasting, WAIT the delivery dwell (default 1 s, configurable — slow
targets read late), THEN restore ONLY if the clipboard still equals our
temp value (unchanged-indicator AND content-hash match). User copies
during dwell always win (no restore). Both rules are load-bearing:
dwell covers slow readers, the guard covers races.

## 7. Capability + degraded-mode model

Every backend reports `BackendCaps` at startup AND on change:

```rust
pub struct PasteCaps { // review H-01: level + strategy, never mixed
    pub level: Level, pub strategy: PasteStrategy, pub reason: String,
}
pub struct BackendCaps {
    pub monitor: Level, pub transfer: Level, pub hotkey: Level,
    pub paste: PasteCaps, pub tray: Level, pub autostart: bool,
    pub primary_selection: bool, pub notes: Vec<String>,
}
pub enum Level { Full, Degraded, Unavailable }
```

Rules: daemon MUST serve history/popup/config whenever the store works,
even if all backends are `Unavailable` (offline mode). UI MUST render the
caps matrix (About/Help + tray tooltip) so QA can see exactly what a box
supports. Extension disconnect flips GNOME caps without crashing.

## 8. Async + thread-affinity model (review finding 6)

tokio lives ONLY in `daemon` (+ `ipc::server`). NO tokio in `app-ui`,
CLI, or `core`: clients use `ipc::client` (std only) — review C-07. Channels: `std::sync::mpsc` and
`tokio::sync::{mpsc, broadcast}` only.

Services declare affinity in `platform`: `Worker` (any blocking thread)
vs `MainThread` (OS event loop). Backends are sync `poll` traits on
worker threads EXCEPT main-thread services:

| Service | Affinity | Loop |
|---|---|---|
| Tray + all windows | app-ui MAIN thread | eframe/winit loop (tray-icon rule) |
| macOS hotkeys | daemon MAIN thread | minimal CFRunLoop pump (global-hotkey rule) |
| Win monitor window + hotkeys | daemon pump thread (any) | message pump (works off-main) |
| X11/Wayland monitors | daemon worker threads | socket poll |
| Elevated helper (Win) | own process | job pipe |

Rationale: headless daemon MUST own hotkeys (finding 5), so the daemon —
not the UI — carries the macOS main-loop pump. No backend improvises
threading: affinity violations are a code-review blocker.

## 9. IPC protocol (crate `ipc`, v1; rewritten per review finding 10)

- Transport: Unix domain socket in the profile dir; Windows named pipe
  `\\.\pipe\clipmonger-<profilehash>` (per-PROFILE names — portable and
  installed instances never collide, review H-07). Server = tokio
  built-ins (`tokio::net` UnixListener + named pipes — review C-07,
  no extra crate); clients = std. Socket paths LONGER than 100 chars
  (Unix `sun_path` limit) fall back to a short link under
  `$XDG_RUNTIME_DIR`/`$TMPDIR` (review H-07). First-instance enforcement:
  daemon startup FAILS LOUDLY if the endpoint already exists (no pipe
  squatting); clients additionally verify the server PID image equals
  our daemon exe (same pattern as seg-04 §5) — review H-07.
- Framing: JSONL, UTF-8, max line 1 MiB (oversize line → drop connection
  + audit). Envelope (every frame):
  `{v:"ipc/v1", id, type:"req"|"res"|"event"|"cancel", method?, params?,
  result?, error?, seq?}`. Requests carry client-chosen `id`; responses
  echo it; `cancel{id}` aborts server work; hello negotiates
  (`hello{min,max}` → `welcome{chosen}`; mismatch → clean `bye`).
- Binary transport: base64 chunks (raw ≤256 KiB → ≤342 KiB/line, always
  under the cap): `blob.get{clip,mime}` → `blob.chunk{seq,eof,sha256}`*
  + per-blob total hash. Request/response and event streams share the
  socket but separate QUEUES: slow subscribers get events dropped after
  128 queued + a `resync-required` event (never block requests);
  subscribers idle >5 min are evicted with notice.
- Methods, COMPLETE per milestone (review H-06/M-14; every method gets
  struct pairs in `ipc::proto` + roundtrip vectors in T-210):
  v1 core: `status`, `caps.get`, `caps.refresh`, `history.list{query?,
  filters?}`, `history.get`, `history.delete`, `history.pin`,
  `history.edit`, `history.duplicate`, `paste.execute`,
  `monitor.set{on|paused_until}`, `hotkey.register/unregister`,
  `config.get/set`, `backup.create/restore`, `logs.tail`,
  `daemon.stop`, `events.subscribe`.
  v1 extension (seg-07): `capture.report`, `hotkey.fired`,
  push `config.changed`, `ping` (heartbeat), `blob.put{chunks}` for
  extension→daemon uploads >256 KiB (mirrors `blob.get`).
  v1 UI flow: daemon→UI push `ui.show_popup{target?}`; rule: a
  `popup.open` request with NO UI attached makes the daemon SPAWN the UI
  (launcher path) instead of failing — so CLI/compositor bindings always
  work (review H-06/O-01). Extension↔daemon uses the same methods.
  Amended by ADR-004 (D-026): the server answers `events.subscribe{topics}`
  with `{ok:true}` before any event; event frames carry the topic as
  `method`. `ui.show_popup{source, toggle, target?, gesture?, x11_time?,
  activation_token?}` is exclusive (`already_subscribed`), limited to the
  UI binary beside the daemon, held 10 s when nobody is subscribed, and
  its subscription ends with the connection (no idle eviction).
  `popup.open` returns `{delivered}` and never mints a gesture;
  `hotkey.fired` takes the popup trigger path (ignored on Windows).
  v2 adds: `macro.run/test`, `permanent.create/update/delete/move`,
  `group.*`, `autoswitch.state`, `bar.*`,
  `plugin.list/install/remove/enable/disable`, `plugin.permissions`
  (management; ADR-001). Third-party plugins speak a GATED SUBSET only
  (names frozen in v1, enforced in v2): allowlisted reads
  (`status`, `caps.get`, `history.list/get`), gated writes
  (`paste.execute` only with the capability + explicit user trigger),
  and `events.subscribe` for allowlisted topics (post-policy-filter
  events only). v3 adds: `sync.*`.
- Auth + permissions (review finding 29): user-only socket/pipe (Unix
  0600/0700; Windows explicit current-user-SID DACL) + per-profile token
  file (same user-only rule) sent in `hello`. No network listeners v1/v2.

## 10. Feature flags + build matrix

`--features backend-windows|backend-x11|backend-wayland|backend-macos`
(default: host-appropriate). `minimal` (daemon+cli, no UI) for servers.
CI builds NATIVE per OS: win-x64, linux-x64 (+arm64 in v3) in v1;
mac-arm64/x64 from v2 (macOS compile-only checks optional in v1 — review
finding 2) × {full, minimal} (seg-12). Linux→Windows cross = optional
later (verify toolchain if pursued); Linux→macOS cross explicitly OUT.

## 11. Test hooks (detail in seg-11)

- `platform::fakes` for every trait (scripted event feeds).
- Daemon `--headless --script <jsonl>` replays captures deterministically.
- IPC record/replay proxy for UI tests.
- `caps.get` golden-file per validation box (see seg-11 matrix).

## 12. Segment tests (T-2xx)

- T-201 `[unit]` IPC method roundtrips (all v1 methods, incl. error shapes).
- T-202 `[unit]` single-daemon lock contention (two instances, expect clean
  attach-or-exit).
- T-203 `[unit]` caps aggregation incl. extension-disconnect flip.
- T-204 `[unit]` fakes script determinism (same script → same event seq).
- T-205 `[integration]` daemon+CLI over real socket/pipe incl. 3 MiB blob
  via 256 KiB base64 chunks (12 chunks + total hash) + token-auth failure.
- T-206 `[unit]` self-write suppression matrix (exact match suppressed,
  racing user copy preserved, TTL expiry, dual-backend shared registry).
- T-207 `[unit]` guarded-restore predicate (equal→restore, changed→keep).
- T-208 `[unit]` paste-session revalidation (same/stale/dead target).
- T-209 `[integration]` lifecycle matrix: daemon crash→UI reconnect,
  UI crash→daemon survives, Quit stops both, double-launch attaches.
- T-210 `[unit]` IPC conformance vectors (envelope, chunking, cancel,
  version mismatch, oversize line, slow-subscriber eviction).
- T-211 `[integration]` endpoint security: pre-squatted pipe/socket →
  loud refusal; peer-image mismatch → drop; >100-char socket path →
  short-link fallback works.
- T-212 `[unit]` (v2; types frozen v1) plugin manifest parse + permission
  vectors (valid/invalid manifests, default-deny, unknown capability →
  refuse).
- T-213 `[integration]` (v2) hostile-plugin isolation: crashing/stalling/
  over-privileged plugin degrades itself only; daemon + other clients
  unaffected; denials audit-logged.

## 13. Verification tasks (close before/with backend code)

1. RESOLVED (Codex audit + review C-01): arboard lib.rs audited
   2026-10-16 (get/set/clear/file_list only — no monitoring, no RTF,
   no custom formats, no atomic multi-write) → DROPPED entirely.
   Direct OS transfer (Win32 seg-04 §2, AppKit seg-08 §2) + custom
   monitors per backend (Wayland via wl-clipboard-rs Watcher).
2. PARTIALLY RESOLVED (eframe/egui 0.37 docs inspected): viewport
   builders verified (decorations/level/position/size/taskbar/focus…);
   REMAINS: TOP enum path + per-OS behaviors → seg-09 §14.1 + T-905.
3. RESOLVED (review C-07): transports are tokio built-ins (server)
   + std (clients) — no selection needed. D-Bus binding is a SEPARATE
   selection owned by seg-06 §11.2.
4. rquickjs API surface check at macrolib time (seg-03 §7).

## 14. Threat model (review O-04)

Attackers: (a) same-user malicious process (reads our files/memory —
  OUT of scope to fully defeat; user-only perms raise the bar);
(b) malicious sync peer / cloud host (v3: untrusted; E2E + signed
manifests + quotas per seg-10); (c) LAN observer (v3: ciphertext only);
(d) malicious CLIPBOARD CONTENT (parsers must not panic/OOM — fuzzed
T-305/T-1105, decode caps seg-03 §8); (e) macro/`[RUN]` abuse (confirm
policy + audit + sandbox, seg-03 §7); (f) supply chain (locked deps,
cargo-deny gate, checksums — seg-11/12); (g) malicious or compromised
PLUGIN (v2+: untrusted; capability-gated IPC subset, install-time
permission prompt, no store/backend/token access, crash isolation per
ADR-001; curated list from v3+ only). Trusted: the OS kernel, the
user's account boundary, paired devices' operators (post-SAS).
Residual risks: R-009 (plaintext at rest), R-010 (elevated helper),
R-011 (extension session impact) — all explicit in plan.md.

## 15. Logging + diagnostics (review O-02)

One structured-logging facade + file rotation (verify selection at
implementation against M-11 criteria; used consistently by daemon, UI,
CLI, helper). Location: `<profile>/logs/clipmonger.log` (+ rotation
`….1..9`, 10 MB each); levels ERROR/WARN/INFO/DEBUG/TRACE with
per-module filter (`CLIPMONGER_LOG`). RULES: never log clip payloads,
tokens, SAS codes, or full command text (hashes/metadata only, §5);
window titles/paths allowed locally, REDACTED in shared evidence
(seg-11). Plugin install/enable/disable/permission-grant/denial events
(v2, ADR-001) are audit-logged with metadata only. `logs.tail` IPC method
serves the UI log viewer; daemon crash writes a final line + preserves
the current file for forensics.
