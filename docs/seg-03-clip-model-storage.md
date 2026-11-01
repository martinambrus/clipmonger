# Clip model, formats, storage, config, macros

## 1. Clip model (crate `core`)

```rust
pub struct Clip {
    pub id: Uuid,            // v7: THE replication identity (stable forever)
    pub created_ms: i64,     // unix millis, UTC
    pub source_app: Option<String>, // when ForegroundApp available
    pub source_os: String,   // capture OS: "windows"|"linux"|"macos" (finding 33)
    pub pinned: bool, pub removed: bool, // soft-delete STORAGE from v1 (finding 37)
    pub preview: String,     // display snippet, re-renderable from payload
    pub search_text: String, // STABLE searchable text, width-independent (finding 32)
    pub formats: Vec<String>,// canonical MIME list (see §2)
    pub bytes_total: u64,
    pub content_hash: String,// hex SHA-256, LOCAL dedupe only, NEVER a
                             // distributed key (review findings 16, 34)
    pub rev: Rev,            // LWW key for mutable annotations (finding 16/17)
    pub last_used_ms: i64,   // move-to-top ordering (review C-06)
    pub origin_dev: String,  // capturing device id (v3 provenance)
    pub origin: String,      // capture backend: windows|x11|wayland|gnome|macos
}
pub struct Rev { pub wall_ms: i64, pub dev: String, pub ctr: u64 }
pub struct PermanentClip {
    pub id: Uuid, pub group: String, pub name: String,
    pub hotkey: Option<String>,      // chord string, e.g. "ctrl+alt+1"
    pub kind: PermanentKind,         // Paste | Macro | System
    pub body: String,                // text or macro source
    pub js: Option<String>,          // v2: JS stage source
    pub rev: Rev,                    // LWW key for sync (review finding 17)
}
```

`System` kind = `_System` group semantics (hotkey-only, never in popup).
All structs are serde roundtrippable; IPC reuses these types (no second
model). `removed=true` = soft-delete (storage v1, recovery UI v2, F-106);
purge is explicit + audited. NO `sensitive` stored flag (review finding
12): OS-privacy skips create no row at all (see §5).

## 2. Canonical formats + OS mappings

| Canonical | Windows | macOS | X11 | Wayland/data-control |
|---|---|---|---|---|
| text/plain;charset=utf-8 | CF_UNICODETEXT | public.utf8-plain-text | UTF8_STRING | text/plain;charset=utf-8 |
| text/plain | CF_TEXT | public.plain-text | STRING | text/plain |
| text/html | HTML Format | public.html | text/html | text/html |
| text/rtf | Rich Text Format | public.rtf | text/rtf | text/rtf |
| image/png (normalized) | PNG/CF_DIB→PNG | public.png | image/png | image/png |
| text/uri-list (files) | CF_HDROP | public.file-url | text/uri-list | text/uri-list |

Rules (review finding 33): canonical formats are SEMANTICALLY
NORMALIZED (text→UTF-8, images→PNG, line endings→`\n`), NOT lossless:
optional native payloads preserve OS-specific fidelity. Every capture
records `source_os`; native formats are stored as `x-os/<os>:<id>` where
`<id>` is the registry/format-atom name (e.g. `x-os/win:HTML Format`).
Paste prefers canonical; native blobs paste ONLY on the same OS, and
sync (v3) never ships incompatible natives blindly. Nothing is silently
dropped: unmappable OS formats become passthrough blobs. File lists store
URIs + existence snapshot (targets may move).

## 3. SQLite store (rusqlite, MIT, `bundled` + `backup` features)

One DB per profile: `clipstore.sqlite3` (+ WAL). Machine name in `meta`
(for sync disambiguation), NOT in the filename (cleaner portable mode).

```sql
PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
CREATE TABLE meta(k TEXT PRIMARY KEY, v TEXT NOT NULL); -- schema_version, machine, created, privacy_skips
CREATE TABLE clips(
  id TEXT PRIMARY KEY, created_ms INTEGER NOT NULL, source_app TEXT,
  source_os TEXT NOT NULL, pinned INTEGER NOT NULL DEFAULT 0,
  removed INTEGER NOT NULL DEFAULT 0, preview TEXT NOT NULL DEFAULT '',
  search_text TEXT NOT NULL DEFAULT '', bytes_total INTEGER NOT NULL DEFAULT 0,
  content_hash TEXT NOT NULL, rev_ms INTEGER NOT NULL, rev_dev TEXT NOT NULL,
  rev_ctr INTEGER NOT NULL DEFAULT 0, last_used_ms INTEGER NOT NULL,
  origin_dev TEXT NOT NULL DEFAULT '', origin TEXT NOT NULL DEFAULT '');
CREATE INDEX idx_clips_created ON clips(created_ms DESC);
CREATE INDEX idx_clips_content ON clips(content_hash);
CREATE TABLE clip_payloads(
  clip_id TEXT NOT NULL REFERENCES clips(id) ON DELETE CASCADE,
  mime TEXT NOT NULL, external INTEGER NOT NULL DEFAULT 0,
  data BLOB, -- NULL when external=1 (see §4)
  blob_key TEXT, -- content key when external=1 (see §4)
  native_id TEXT, -- x-os/<os>:<id> for passthrough fidelity (see §2)
  PRIMARY KEY(clip_id, mime));
CREATE TABLE clip_blobs( -- content-addressed external store (finding 11)
  blob_key TEXT PRIMARY KEY, sha256 TEXT NOT NULL, bytes INTEGER NOT NULL,
  codec TEXT NOT NULL DEFAULT 'raw', created_ms INTEGER NOT NULL);
CREATE TABLE perm_groups(name TEXT PRIMARY KEY, system INTEGER NOT NULL DEFAULT 0);
CREATE TABLE permanents(
  id TEXT PRIMARY KEY, grp TEXT NOT NULL REFERENCES perm_groups(name),
  name TEXT NOT NULL, hotkey TEXT, kind TEXT NOT NULL,
  body TEXT NOT NULL, js TEXT,
  rev_ms INTEGER NOT NULL, rev_dev TEXT NOT NULL, rev_ctr INTEGER NOT NULL DEFAULT 0);
-- Full-text search (finding 32; FTS5 preferred, LIKE fallback):
CREATE VIRTUAL TABLE clips_fts USING fts5(clip_id UNINDEXED, text);
```

- Open path (review finding 13; check semantics verified against
  sqlite.org/pragma: `quick_check` is the faster O(N) check,
  `integrity_check` the fuller O(N log N) one): normal open +
  `user_version`/schema validation → numbered migrations (rollback on
  failure into recovery/read-only mode — a migration bug is NEVER
  auto-declared "corruption") → routine `quick_check` at startup (fast
  path) → full `integrity_check` + `foreign_key_check` only for repair
  and manual diagnostics. True corruption: preserve DB+WAL+SHM
  consistently, copy aside as `clipstore.corrupt-<ts>.sqlite3`, offer
  recovery/export, and SHOW all of it (F-402: never silent).
- Consistency: single writer = daemon (seg-02 §1). `backup` API feeds the
  ZIP export (F-602) without locking the UI.
- Retention job: enforce count/size/age caps (F-404) on insert + hourly;
  `VACUUM INTO` for compact (F-404); log what was purged.
- sqlx (async multi-DB) was considered and REJECTED: async pool +
  runtime imposition is wrong for a single-user local daemon.

## 4. Blob policy (review finding 11)

Payload ≤256 KiB → inline `clip_payloads.data`. Larger → external row in
`clip_blobs` + file `blobs/<blob_key>` where `blob_key` =
`sha256hex[0..32]-<bytes>` (content-addressed, unique, no content hints);
`clip_payloads.blob_key` locates it, `data=NULL`. Readers MUST handle both.
Write path (crash-safe): temp file → write → fsync → atomic rename →
insert row (same order on replace; fsync dir on Unix). GC: mark-and-sweep
at startup + after retention (refs from `clip_payloads` + in-flight jobs);
orphan files/dangling rows are quarantined to `blobs/orphan-<ts>/` +
logged, never silently deleted. `bytes_total` counts logical bytes. Caps
(F-103/404) apply pre-store; oversize captures become metadata-only
STUBS (user-visible "too large" state, not silent loss — "tombstone" is
reserved for deletion replication, review finding 12).

## 5. Privacy rules (F-104, backend-mapped in seg-04–08)

Any OS privacy signal (Windows exclusion formats, macOS concealed-type,
offered `x-kde-passwordManagerHint` MIME (review H-03),
data-control restrictions) → store NOTHING:
no row, no payload, no preview, no source_app — only the aggregate
`privacy.strict` (H-08) additionally forces text-only capture at
ingest: non-text clips are dropped with the same no-row treatment.
`privacy_skips` counter in `meta` for the About page. This MUST be covered
by per-backend `[integration]` tests with synthetic signals.
At-rest posture (review finding 30, DECIDED and user-confirmed):
local history, blobs, config, and backup ZIPs are PLAINTEXT, protected
only by OS account security + user-only file permissions (§6). UI + docs
MUST warn that exports/backups contain plaintext clipboard data. Optional
encrypted exports are a future track (after sync keys exist in v3); they
MUST NOT be promised in v1.
- Audit log (review H-08): append-only `<profile>/audit.log`, JSONL lines
  `{ts, kind, hashes, result}` (never payloads); rotation 10×1 MB,
  retention 90 days. Aggregate counters (`privacy_skips`) live in `meta`.
  v2 adds plugin lifecycle entries (install/enable/disable/permission
  grant/denial per ADR-001, metadata only).
- O-03 decisions: NO secure-delete in v1 (SSD overwrite is theater —
  documented, not promised); lock-screen = keep running (documented);
  clear-on-exit = manual `purge-removed` CLI (M-09); SQLCipher evaluated
  only if encrypted-store demand arises (revisit gate, never v1).

## 6. Config + paths

- TOML file via `toml` crate + serde structs (`core::config::Config`,
  versioned with `config_version`). Unknown-key preservation (review
  finding 14): `#[serde(flatten)] extra: BTreeMap<String, toml::Value>`
  at EVERY config level (root + each section); comments are NOT promised
  preservation in v1 (documented). Forward-version roundtrip = T-311.
- Env overrides: `CLIPMONGER_<SECTION>_<KEY>` (typed parse, logged).
- Locations via `directories` crate: config = `config_dir/clipmonger/`,
  data(DB) = `data_dir/clipmonger/`, cache(previews) = `cache_dir/clipmonger/`.
  Portable mode (marker file `portable.marker` next to binary OR running
  from removable media): ALL THREE resolve beside the binary (F-504).
- Permissions (review finding 29): profile files user-only — Unix
  0600 files / 0700 dirs; Windows explicit current-user-SID DACL. Applies
  to config, DB+WAL, blobs, token, logs, backups, IPC endpoints.
- v1 key TABLE (review M-04 — sole definition; T-906 walks it; UI tab
  per key; v2/v3 groups reserved NOW: `[macros]`, `[sync]`, `[sharing]`,
  `[plugins]`, `[ui]`):

  | Key | Type | Default | Tab | Since |
  |---|---|---|---|---|
  | monitor.formats | string[] | [all canonical] | History | v1 |
  | monitor.max_bytes | int | 16 MiB | History | v1 |
  | monitor.min_len | int | 0 | History | v1 |
  | monitor.ignores | string[] (app classes) | [password mgrs] | History | v1 |
  | monitor.poll_ms | int | 500 (macOS/X11-poll) | History | v1 |
  | monitor.capture_primary | bool | false | History | v1 (H-11) |
  | monitor.primary_debounce_ms | int | 500 | History | v1 |
  | history.max_items | int | 750 | History | v1 |
  | history.max_store_bytes | int | 512 MiB | History | v1 |
  | history.max_age_days | int | 0 (off) | History | v1 |
  | history.dedupe_window | int | 100 | History | v1 |
  | popup.hotkey | chord | per-OS default (§9-8) | History | v1 |
  | popup.placement | enum | cursor | History | v1 |
  | popup.preview_width | int | 80 | History | v1 |
  | popup.dismiss_on_focus_loss | bool | true | History | v1 |
  | popup.keys.* | chord map | seg-09 §8 | History | v1 |
  | popup.paste_chords | [{app,chord}] | [{*,ctrl/cmd+v}] | History | v1 (M-08) |
  | popup.unicode_limit | int | 65536 | History | v1 (M-18) |
  | popup.delivery_dwell_ms | int | 1000 | History | v1 (H-02) |
  | permanents.groups | managed in UI | [] | Permanents | v1 |
  | privacy.strict | bool | false | History | v1 (H-08: text-only, no source_app, halved retention) |
  | privacy.denylist | string[] (app-ids) | [] | History | v1 |
  | tray.enabled/icon/menu opts | mixed | on | General | v1 |
  | autostart.enabled | bool | false | General | v1 |
  | updates.check | bool | true (+first-run notice) | General | v1 (M-16) |
  | logging.level | enum | INFO | General | v1 |
  | popup.delivery | enum | clipboard_restore | Popup | v1 (clipboard_restore = write+Ctrl+V with guarded restore; unicode_inject = synthetic keystrokes; elevated route follows the same choice via clipboard write + helper chord) |
  | popup.restore_clipboard | bool | true | Popup | v1 (false = the pasted clip stays on the clipboard after a clipboard_restore paste) |
  | ui.locale | string (BCP-47) | en | General | v2 (ADR-002) |
  | ui.fallback_locale | string (BCP-47) | en | General | v2 (ADR-002) |
  | plugins.enabled | bool | true | Plugins | v2 (ADR-001) |
  | plugins.<id>.enabled | bool (managed in UI) | true | Plugins | v2 (ADR-001) |
  | run.confirm_policy | enum | permanent-only | Permanents | v2 (macros) |
- Backup/restore ZIP (F-602): `manifest.toml` + `clipstore.sqlite3` (via backup API) + `blobs/` + schema versions and optional raw `config.toml` (exact UTF-8 bytes with `config_bytes`/`config_sha256`; missing file omits entry/fields, never reserialize env-overridden live config).
  Legacy/no-config archives preserve existing destination `config.toml` and live config (fresh destination stays absent); never infer deletion from omission. Restore is TRANSACTIONAL (review finding 31): destination-local owned stage → validate present config digest/length/UTF-8/TOML plus manifest/schema/checksums/size caps → reject path traversal/symlinks/overlarge entries → run DB validation → atomic swap with rollback on reported errors and retained recovery directory. Some backend monitor/formats/poll/hotkey values snapshot at daemon start and require restart after config restore (documented limit, no backend hot reload). NEVER overwrite the only good profile pre-validation.

## 7. Macros (v2 design, frozen now so core needs no refactor)

- **Stage 1 — `[VAR]` expansion** (pure Rust, no I/O except declared
  reads): verified core from ArsClip help: `[POPUPITEM=n]`,
  `[CLIPBRDCURRENT]` (+date/time variants), `[TAB]` + nav/edit keystroke
  names, `[COPYWAIT=ms]`, `[RUN]...[/ENDRUN]`. Rules: max 32 nesting,
  5 s timeout, unknown `[VAR]` = literal text + warning (never error).
  Injection rules (review H-05 — macro bodies splice UNTRUSTED
  history): expansion is SINGLE-PASS (substituted text is INERT, never
  rescanned); `[RUN]` bodies come from the macro SOURCE only, never from
  expanded output; `[POPUPITEM]`/`[CLIPBRDCURRENT]` splices are data-only.
  **Completion task**: expand the list against the ArsClip binary's
  built-in help during macrolib implementation (online help is thin).
- **Stage 2 — JavaScript** (rquickjs, MIT, QuickJS-NG/ES2020): per-macro
  `Runtime` (<300 µs lifecycle per upstream), NO fs/net/timers except
  the injected API: `getPopupClip(i)`, `getClipboard()`, `setClipboard(s)`,
  `deletePopupClip(i)` (verified ArsClip name), `run(cmd)` (routes to
  Stage 3 policy), `alert(s)` (daemon log + UI toast). Memory + instruction
  caps enforced; infinite loops MUST be killed by timeout. Boa (pure-Rust
  JS) was considered and REJECTED: self-described experimental + heavier
  than QuickJS for snippet workloads.
- **Stage 3 — `[RUN]`/keystrokes**: OS shell for commands (`cmd /c`, `sh -c`) and the desktop file/URL opener for documents/links (verify per-OS command at implementation)
  chosen per OS at implementation; scrubbed env; cwd = profile dir;
  timeout + kill; **user-confirm policy** (`always`/`permanent-only`/
  `never`) with default `permanent-only`; every execution audit-logged
  (command hash + ts + result). Audit logs NEVER contain clip payloads
  or command plaintext when a hash suffices (review finding 52).
  Keystroke simulation uses backend
  capabilities; where `Unavailable`, macros degrade with a clear message
  (Wayland — see seg-06 spike).

## 8. Dedupe / retention / search-text algorithms

- Canonical hash input (review finding 34, LOCAL dedupe only — never a
  distributed key): sorted canonical MIME keys, each followed by
  length-delimited NORMALIZED bytes (text: UTF-8 after `\r\n`→`\n`;
  images: decoded-pixel bytes; files: sorted URI list). Native
  passthrough (`x-os/*`) is EXCLUDED (documented: same content on two
  OSs hashes equal). SHA-256 hex = `content_hash`. On capture, search
  last N (config `dedupe_window`, default 100, cliphist-proven) for
  equal hash → move-to-top instead of duplicate.
- `search_text` (review finding 32): stable width-independent string =
  full text (or "" for non-text) + `\n` + URI basenames + `\n` +
  source_app — OMITTED when `privacy.strict` is set (H-08) —
  (+ permanent name for permanents). FTS5 index over it
  (verify rusqlite-bundled FTS5 at implementation; LIKE fallback).
  Display previews render from payload at query time (any width).
- Decode guards (review finding 36): images/native payloads enforce
  max dimension 8192 px, max 64 MP, per-decode timeout; violations →
  `TooLarge`/`Unsupported` stub, never OOM. Parser fuzz vectors in T-305.
- Primary selection (review H-11): `capture_primary` DEFAULTS OFF
  (selection-churn spam protection); when on: 500 ms debounce + min
  length applies + shared history list tagged with origin. UI control
  mandatory (D-014); backend notes in seg-05/06.
- Retention: `max_items` (default 750), `max_store_bytes` (default 512 MiB),
  `max_age_days` (default 0 = off). Enforcement oldest-first, pinned and
  permanents exempt. Every purge logged with counts. Under
  `privacy.strict` (H-08) all retention caps are HALVED.

## 9. Segment tests (T-3xx)

- T-301 `[unit]` model serde roundtrips + unknown-field tolerance.
- T-302 `[unit]` schema create+migrate across every migration step.
- T-303 `[unit]` open-path matrix: clean open, migration failure →
  rollback/recovery-mode (NOT auto-corrupt), true corruption →
  preserve+aside+export offer (review finding 13).
- T-304 `[unit]` macro Stage-1 vectors (nesting, timeout, unknown vars)
  + injection vectors (splice inertness, RUN-from-source-only, H-05).
- T-305 `[unit]` dedupe/retention/purge policy matrix + canonical-hash
  vectors (MIME order, natives excluded) + image/parser fuzz vectors.
- T-306 `[unit]` config parse/merge/env-override/portable resolution.
- T-307 `[integration]` backup-ZIP roundtrip incl. external blobs.
- T-308 `[integration]` WAL concurrent read during write burst.
- T-309 `[integration]` (v2) JS sandbox: no-fs/net proof + timeout kill +
  API surface conformance.
- T-310 `[integration]` external-blob crash recovery (kill mid-write →
  no dangling rows) + orphan GC quarantine run.
- T-311 `[unit]` forward-config roundtrip (unknown nested keys survive;
  comments may not).
- T-312 `[integration]` restore rollback (traversal/symlink/oversize/
  bad-manifest fixtures → profile untouched).
- T-313 `[unit]` search-text + FTS/LIKE parity vectors (preview-width
  changes never alter search results).
- T-314 `[unit]` downgrade refusal (newer schema + older code → clean
  error naming versions, never a write).

## 10. Verification tasks

1. rquickjs API check (Runtime/Context/loader shape for our sandbox).
2. Confirm bundled SQLite version ≥ 3.45 at build (rusqlite documents
   3.45.3+ baseline).
3. Full `[VAR]` completion against ArsClip built-in help (v2 task).
4. FTS5 availability in rusqlite-bundled SQLite (else LIKE-only + note).
