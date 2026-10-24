# Sync, sharing, portability (v3; F-701/702/703)

v3a = LAN sync. v3b = user-provided cloud. No accounts, no hosted infra,
E2E encryption ALWAYS (D-015, decided plan §5 Q7 — LAN + user-cloud,
hosted out; this segment specifies the mechanism). Mobile DROPPED per
D-016 (no v4+ proposal remains); §6 keeps the sync core portable as
good design.

## 1. Identity + pairing (v3a)

- Device identity = Noise static keypair (X25519); device ID = fingerprint
  of the public key. Keys at rest: file 0600 in profile dir; OS-keyring
  upgrade = VERIFY crate at v3 implementation (task §8.1).
- Discovery: mDNS `_arsclipx._tcp` via mdns-sd 0.21 (Apache-2.0 OR MIT,
  crates.io-verified; browser lifecycle + TXT-record API = VERIFY at v3
  design reopen, H-06; no async-runtime dep). TXT: device name + caps +
  protocol v1.
- Pairing: discover → pick device → Noise XX handshake (snow 0.10,
  Apache-2.0 OR MIT (crates.io-verified; packet framing/rekey/
  PSK-position = VERIFY in snow docs at v3 design reopen, review H-06);
  suite `Noise_XX_25519_ChaChaPoly_SHA256`
  — components visible in its feature map) → BOTH sides display a
  6-digit SAS code derived from the handshake hash (VERIFY derivation at
  implementation) → user confirms match → store peer static key as
  trusted (TOFU + verify). Unpair = delete key (+ optional wipe-ack).
- Repair/re-key: explicit action; new SAS ceremony each time.

## 2. Sync protocol (v3a, E2E over Noise TCP)

Transport: TCP via tokio (`net` feature enabled in v3 per seg-02 §3),
one Noise-encrypted session per peer pair (forward secrecy per session).
Messages (serde, versioned `sync/v1`):

- `hello {device, caps, cursor}` / `ack {cursor}`
- `clip_batch {clips: Vec<Clip>, payloads chunked >256 KiB}` (seg-03 model)
- `tombstones {ids}` (deletes propagate; purge is explicit per seg-03)
- `permanents {entities: [{perm, rev}], tombstones}` (OPT-IN per group; `_System` NEVER syncs; LWW per merge rule)
- `cursor` per peer for offline resume (daemon restart, laptop sleep)

Merge rule (review findings 16, 17, 34): clips merge by REPLICATION
IDENTITY (`Clip.id`, UUIDv7) — union on id; payload bytes immutable per
id. `content_hash` is LOCAL-dedupe-only, never a distributed key: same
content captured on two devices = two ids (both kept; UI MAY
display-collapse by equal hash in v3.x). Mutable annotations
(pinned/removed) replicate as `(id, Rev)` records with deterministic LWW
(compare wall_ms, then dev, then ctr); deletes replicate as tombstones
keyed by id (retained ≥30 days, then GC'd with a generation watermark).
Permanents/groups are versioned MUTABLE entities: `(UUID, Rev)` + field
LWW + tombstones; simultaneous edits resolve deterministically to
highest Rev + an audit entry (no silent conflict copies). Ordering:
`(created_ms, id)`; ties impossible (UUID). File-list clips default
LOCAL-ONLY for sync (review finding 35): peers show labeled path
references with `paste: unavailable` unless cross-device file transfer
becomes a separate feature. Selective sync: per-group + per-format +
max-bytes egress filters (defaults reviewed at implementation).

## 3. User cloud (v3b)

- User points the app at THEIR folder (Dropbox/Nextcloud/WebDAV mount…).
  Watcher: `notify` 8.x (CC0, verified; tokio feature available).
- Blobs: one file per batch + SIGNED manifest. Signing identity
  (review finding 18): every device holds an Ed25519 signing keypair
  (ed25519-dalek 3.0, BSD-3-Clause, verified) generated at pairing and
  BOUND to its Noise static key (both fingerprints confirmed in the SAS
  ceremony). Manifest = `{gen, prev_manifest_hash, entries[], signer,
  sig}`: per-device generation numbers (replay/rollback detection),
  hash chain (ordering), Ed25519 signature (sender authentication).
  Payload encryption = age 0.12 (MIT OR Apache-2.0, verified) to ALL
  paired device keys. NOTE: age self-labels BETA — re-evaluate at v3;
  FALLBACK (no new deps, review finding 19): ChaChaPoly container with
  per-blob data keys WRAPPED to each paired device's persistent X25519
  static key (static-static ECDH + HKDF — fully OFFLINE, no live Noise
  session; v3b works with v3a disabled). Primitives already in tree via
  snow's resolver (chacha20poly1305/sha2/getrandom/curve25519).
- Blob names = random IDs (no content hints); manifest carries cursors so
  each device resumes. Cloud host sees ciphertext + sizes only. v3b works
  even with v3a disabled (pure sneakernet-cloud).

## 4. Import/export/portability (F-703, v2 basic → v3 full)

- Formats: our ZIP (seg-03 §6) + ArsClip-ini flavor export
  (best-effort, documented gaps) + JSON lines (for scripts/agents).
- v2: export/import clips+groups+settings. v3: scheduled auto-export
  (immutable versioned objects; concurrent writers produce conflict
  COPIES of export files, never merges). Live shared SQLite databases
  are FORBIDDEN (review finding 15 — SQLite: network locking "known to
  operate incorrectly... led to database corruption... rely upon it at
  your peril"); true merge happens only through §2/§3 protocols into
  each device's own local DB.

## 5. Privacy + threat model

- Ciphertext-only for network/cloud observers; metadata minimized (random
  blob names, no plaintext filenames). Peer revoke = key delete (+ local
  note that already-synced plaintext can't be un-sent — SAY SO in UI).
- LAN attacker gets Noise handshake only (XX = no identity leak to
  passive observers). Malicious peer gets only what sync filters allow.
- Audit: pair/unpair/sync-session start/end logged (no content).
- Unattended-operation approvals live ONLY in Settings → Sync (review
  H-04); every remote-control session notifies the controlling peer
  (review H-10). UI evidence: seg-09 §4.

## 6. Portability rule (mobile target REMOVED per D-016)

- `core::sync` (codec + crypto glue + merge + cursors) MUST be pure Rust:
  NO tokio/net/fs/mDNS inside — all I/O injected at edges (traits).
  (Kept as good design: testability + portability, no mobile target.)
  Blob-schema privacy deny-rules: sensitive kinds structurally
  unrepresentable in shareable output; directory sync = explicit opt-in
  (review H-12, M-12 §6); asserted by T-1018.
- Primitives restricted to the Noise suite (X25519, ChaCha20-Poly1305,
  SHA-256, HKDF-style).
- `sync/v1` wire + blob formats versioned and DOCUMENTED in this segment
  (conformance vectors in T-1001).

## 7. Segment tests (T-10xx)

- T-1001 `[unit]` codec roundtrips + `sync/v1` conformance vectors.
- T-1002 `[unit]` union-merge property tests (duplicates, tombstones,
  cursors, empty/full joins).
- T-1003 `[unit]` manifest parse + blob-name randomness (no content leak).
- T-1018 `[unit]` privacy deny-rules over the §6 blob schema: sensitive
  kinds (passwords, denylisted apps) are structurally unrepresentable in
  shareable output; directory sync requires explicit opt-in (H-12).
- T-1004 `[integration]` two daemons on localhost: discover→pair(SAS
  auto-confirm in test)→sync→tombstone→unpair→resume-after-restart.
- T-1005 `[integration]` v3b roundtrip via plain-dir fake cloud incl.
  conflict copies + cursor resume.
- T-1006 `[manual-qa]` two REAL machines (LAN + hotspot-LAN): pairing UX,
  SAS mismatch path, sleep/resume, revoke. QA-LEAD signoff.
- T-1007 `[manual-qa]` real user-cloud dir (Dropbox/Nextcloud): blobs
  appear, second device joins, host sees ciphertext only. QA-LEAD signoff.
- T-1008 `[integration]` malicious-peer matrix (quotas, replayed frames,
  rolled-back manifest gen, conflicting permanent edits → deterministic
  LWW + audit, no crash/desync).
- T-1009 `[integration]` file-list clips replicate as labeled path refs;
  peer paste reports `unavailable` (finding 35).
- T-1010 `[unit]` tombstone/GC vectors (id-keyed delete, 30-day retention,
  generation watermark, same-content-different-id collapse hint).

## 8. Verification tasks

1. OS-keyring crate for device keys (or stay file-0600 + documented).
2. SAS code derivation from handshake hash.
3. age stability re-check at v3 (else §3 fallback).
4. mDNS service-name/conflict behavior across routers.
5. tokio `net` (+`fs` if needed) enablement review for the daemon.
6. v2b ZIP auto-export hardening (review M-12 §4): scheduled export as
   the interim share path; conflict-copy rule already in §4.
