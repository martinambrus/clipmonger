//! IPC acceptance harness (foundation-v1, task IPC).
#![allow(clippy::unreadable_literal)]
//!
//! This file is the RULER, not the implementation. It pins the frozen contract
//! from `docs/seg-02-architecture.md` S9 plus ledger decisions D-F17..D-F20
//! and D-F23. The builder (`clipmonger-ipc`) MUST make this file compile and
//! pass without any edit to this file or to `Cargo.toml`.
//!
//! Run: `cargo test -p clipmonger-ipc --features server --test ipc_acceptance`
//!
//! Numbered assumptions the builder MUST match are listed in
//! `.agents/tmp/foundation-v1/ipc/harness/README.md` (A1..A16). Anything not
//! pinned there is deliberately asserted loosely here (e.g. welcome payload
//! fields are checked for presence, not exact shape).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use clipmonger_core::model::{Clip, Rev};
use clipmonger_ipc::framing;
use clipmonger_ipc::proto::methods::*;
use clipmonger_ipc::proto::{Envelope, FrameKind, IpcError};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use uuid::Uuid;

#[cfg(all(feature = "server", unix))]
use clipmonger_ipc::client::Client;
#[cfg(all(feature = "server", unix))]
use clipmonger_ipc::server::{EventBus, Server};
#[cfg(all(feature = "server", unix))]
use std::collections::BTreeMap;
#[cfg(all(feature = "server", unix))]
use std::io::{BufRead, BufReader, Write};
#[cfg(all(feature = "server", unix))]
use std::os::unix::net::UnixStream;
#[cfg(all(feature = "server", unix))]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(all(feature = "server", unix))]
use std::sync::{mpsc, Arc, Mutex};
#[cfg(all(feature = "server", unix))]
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Serialize -> deserialize -> equal. Proves a Params/Result pair is
/// serde-roundtrippable as the contract requires.
fn roundtrip<T>(v: &T)
where
    T: serde::Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let wire = serde_json::to_value(v).expect("params/result must serialize");
    let back: T = serde_json::from_value(wire).expect("params/result must deserialize");
    assert_eq!(v, &back, "roundtrip must preserve value");
}

/// Roundtrip the default value. Used for the few pairs whose exact fields are
/// not frozen (status/caps results): proves the pair exists and roundtrips.
fn roundtrip_default<T>()
where
    T: Default + serde::Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    roundtrip(&T::default());
}

fn sample_rev() -> Rev {
    Rev {
        wall_ms: 1_700_000_000_000,
        dev: "harness-dev".to_string(),
        ctr: 7,
    }
}

fn sample_clip() -> Clip {
    Clip {
        id: Uuid::now_v7(),
        created_ms: 1_700_000_000_000,
        source_app: Some("harness-app".to_string()),
        source_os: "linux".to_string(),
        pinned: false,
        removed: false,
        preview: "hello preview".to_string(),
        search_text: "hello preview".to_string(),
        formats: vec!["text/plain".to_string()],
        bytes_total: 13,
        content_hash: "deadbeef".to_string(),
        rev: sample_rev(),
        last_used_ms: 1_700_000_000_001,
        origin_dev: "harness-dev".to_string(),
        origin: "x11".to_string(),
    }
}

// --- Minimal SHA-256 (test-local): avoids adding a hash crate to the harness.
// Standard FIPS 180-4 implementation; self-checked by t210_sha256_selfcheck.
#[allow(clippy::many_single_char_names)]
fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, c) in chunk.as_chunks::<4>().0.iter().enumerate().take(16) {
            w[i] = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }
    let mut out = String::with_capacity(64);
    for word in h {
        let _ = write!(out, "{word:08x}");
    }
    out
}

// --- Minimal standard-base64 decoder (test-local): RFC 4648 alphabet with
// padding. Self-checked by t210_b64_selfcheck.
fn b64_decode(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(4) {
        return Err("base64 length must be a multiple of 4".to_string());
    }
    let val = |c: u8| -> Result<u32, String> {
        match c {
            b'A'..=b'Z' => Ok(u32::from(c - b'A')),
            b'a'..=b'z' => Ok(u32::from(c - b'a' + 26)),
            b'0'..=b'9' => Ok(u32::from(c - b'0' + 52)),
            b'+' => Ok(62),
            b'/' => Ok(63),
            _ => Err(format!("bad base64 char: {c}")),
        }
    };
    let bytes = s.as_bytes();
    // Padding may only appear as the last 1-2 chars.
    let data_end = bytes.iter().position(|&c| c == b'=').unwrap_or(bytes.len());
    if bytes[data_end..].iter().any(|&c| c != b'=') {
        return Err("padding only allowed at end".to_string());
    }
    if bytes.len() - data_end > 2 {
        return Err("too much padding".to_string());
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for quad in bytes.as_chunks::<4>().0 {
        let mut pad = 0;
        for &c in quad {
            if c == b'=' {
                pad += 1;
            }
        }
        let mut n: u32 = 0;
        for (i, &c) in quad.iter().enumerate() {
            let v = if c == b'=' {
                if i < 2 {
                    return Err("padding in first half of quad".to_string());
                }
                0
            } else {
                val(c)?
            };
            n = (n << 6) | v;
        }
        out.push((n >> 16).to_be_bytes()[3]);
        if pad < 2 {
            out.push((n >> 8).to_be_bytes()[3]);
        }
        if pad < 1 {
            out.push(n.to_be_bytes()[3]);
        }
    }
    Ok(out)
}

#[cfg(all(feature = "server", unix))]
static SOCK_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Fresh tempdir-scoped Unix socket path (short; never triggers the >100-char
/// fallback). No external tempdir crate: plain std + pid + counter.
#[cfg(all(feature = "server", unix))]
fn unique_sock(tag: &str) -> PathBuf {
    let n = SOCK_COUNTER.fetch_add(1, Ordering::SeqCst);
    let mut dir = std::env::temp_dir();
    dir.push(format!("clipmonger-ipc-{}-{tag}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("tempdir must be creatable");
    dir.push("t.sock");
    dir
}

// ---------------------------------------------------------------------------
// T-201: method Params/Result roundtrips (one test per method)
// ---------------------------------------------------------------------------

#[test]
fn t201_status() {
    roundtrip(&StatusParams {});
    roundtrip_default::<StatusResult>();
}

#[test]
fn t201_caps_get() {
    roundtrip(&CapsGetParams {});
    roundtrip_default::<CapsGetResult>();
}

#[test]
fn t201_caps_refresh() {
    roundtrip(&CapsRefreshParams {});
    roundtrip_default::<CapsRefreshResult>();
}

#[test]
fn t201_history_list() {
    let clip = sample_clip();
    roundtrip(&HistoryListParams {
        query: Some("hello".to_string()),
        limit: Some(50),
        offset: Some(0),
        include_removed: Some(false),
    });
    roundtrip(&HistoryListParams {
        query: None,
        limit: None,
        offset: None,
        include_removed: None,
    });
    roundtrip(&HistoryListResult { clips: vec![clip] });
    roundtrip(&HistoryListResult { clips: vec![] });
}

#[test]
fn t201_history_get() {
    let id = Uuid::now_v7();
    roundtrip(&HistoryGetParams { clip: id });
    roundtrip(&HistoryGetResult {
        clip: Some(sample_clip()),
    });
    roundtrip(&HistoryGetResult { clip: None });
}

#[test]
fn t201_history_delete() {
    roundtrip(&HistoryDeleteParams {
        clip: Uuid::now_v7(),
    });
    roundtrip(&HistoryDeleteResult { deleted: true });
    roundtrip(&HistoryDeleteResult { deleted: false });
}

#[test]
fn t201_history_pin() {
    roundtrip(&HistoryPinParams {
        clip: Uuid::now_v7(),
        pinned: true,
    });
    roundtrip(&HistoryPinResult { pinned: false });
}

#[test]
fn t201_history_edit() {
    roundtrip(&HistoryEditParams {
        clip: Uuid::now_v7(),
        preview: "edited preview".to_string(),
    });
    roundtrip(&HistoryEditResult { ok: true });
}

#[test]
fn t201_history_duplicate() {
    roundtrip(&HistoryDuplicateParams {
        clip: Uuid::now_v7(),
    });
    roundtrip(&HistoryDuplicateResult {
        new_clip: Uuid::now_v7(),
    });
}

#[test]
fn t201_paste_execute() {
    roundtrip(&PasteExecuteParams {
        clip: Uuid::now_v7(),
        target: None,
    });
    roundtrip(&PasteExecuteParams {
        clip: Uuid::now_v7(),
        target: Some(json!({"app": "xterm"})),
    });
    roundtrip(&PasteExecuteResult {
        strategy: "UnicodeInject".to_string(),
    });
}

#[test]
fn t201_monitor_set() {
    roundtrip(&MonitorSetParams {
        state: MonitorState::On,
    });
    roundtrip(&MonitorSetParams {
        state: MonitorState::PausedUntil(1_700_000_000),
    });
    roundtrip(&MonitorSetResult { ok: true });
}

#[test]
fn t201_hotkey_register() {
    roundtrip(&HotkeyRegisterParams {
        id: 41,
        chord: "ctrl+alt+h".to_string(),
    });
    roundtrip(&HotkeyRegisterResult { ok: true });
}

#[test]
fn t201_hotkey_unregister() {
    roundtrip(&HotkeyUnregisterParams { id: 41 });
    roundtrip(&HotkeyUnregisterResult { ok: true });
}

#[test]
fn t201_config_get() {
    roundtrip(&ConfigGetParams { key: None });
    roundtrip(&ConfigGetParams {
        key: Some("monitor.formats".to_string()),
    });
    roundtrip(&ConfigGetResult {
        config: json!({"monitor": {"formats": ["text/plain"]}}),
    });
}

#[test]
fn t201_config_set() {
    roundtrip(&ConfigSetParams {
        key: "monitor.formats".to_string(),
        value: json!(["text/plain"]),
    });
    roundtrip(&ConfigSetResult { ok: true });
}

#[test]
fn t201_backup_create() {
    roundtrip(&BackupCreateParams {
        dest: "/tmp/backup.zip".to_string(),
    });
    roundtrip(&BackupCreateResult {
        path: "/tmp/backup.zip".to_string(),
    });
}

#[test]
fn t201_backup_restore() {
    roundtrip(&BackupRestoreParams {
        src: "/tmp/backup.zip".to_string(),
    });
    roundtrip(&BackupRestoreResult { ok: true });
}

#[test]
fn t201_logs_tail() {
    roundtrip(&LogsTailParams { lines: 100 });
    roundtrip(&LogsTailResult {
        lines: vec!["l1".to_string(), "l2".to_string()],
    });
}

#[test]
fn t201_daemon_stop() {
    roundtrip(&DaemonStopParams {});
    roundtrip(&DaemonStopResult { ok: true });
}

#[test]
fn t201_events_subscribe() {
    roundtrip(&EventsSubscribeParams {
        topics: vec!["history".to_string(), "config.changed".to_string()],
    });
    roundtrip(&EventsSubscribeResult { ok: true });
}

#[test]
fn t201_capture_report() {
    roundtrip(&CaptureReportParams {
        event: json!({"offers": [], "origin": "Clipboard"}),
    });
    roundtrip(&CaptureReportResult { accepted: true });
}

#[test]
fn t201_hotkey_fired() {
    roundtrip(&HotkeyFiredParams { id: 7 });
    roundtrip(&HotkeyFiredResult { ok: true });
}

#[test]
fn t201_ping() {
    roundtrip(&PingParams {});
    let r = PingResult { pong: true };
    roundtrip(&r);
    assert!(r.pong, "ping result must carry pong=true");
}

#[test]
fn t201_blob_put() {
    roundtrip(&BlobPutParams {
        clip: Uuid::now_v7(),
        mime: "image/png".to_string(),
        seq: 3,
        data_b64: "aGk=".to_string(),
        eof: false,
        sha256: None,
    });
    roundtrip(&BlobPutParams {
        clip: Uuid::now_v7(),
        mime: "image/png".to_string(),
        seq: 4,
        data_b64: String::new(),
        eof: true,
        sha256: Some(sha256_hex(b"hi")),
    });
    roundtrip(&BlobPutResult { ok: true });
}

#[test]
fn t201_permanent_exec() {
    roundtrip(&PermanentExecParams { id: Uuid::now_v7() });
    roundtrip(&PermanentExecResult { ok: true });
}

#[test]
fn t201_history_purge_removed() {
    roundtrip(&HistoryPurgeRemovedParams {});
    roundtrip(&HistoryPurgeRemovedResult { purged: 9 });
}

#[test]
fn t201_ipc_error_shape() {
    // IpcError is the only error shape allowed on the wire (D-F17).
    let e = IpcError {
        code: "unknown_method".to_string(),
        message: "no such method".to_string(),
        retryable: false,
        backend: None,
    };
    roundtrip(&e);
    let with_backend = IpcError {
        code: "backend_failed".to_string(),
        message: "x11 down".to_string(),
        retryable: true,
        backend: Some("x11".to_string()),
    };
    roundtrip(&with_backend);
    // Negative control: code/message/retryable survive; backend stays None.
    let wire = serde_json::to_value(&e).expect("serialize");
    assert_eq!(
        wire.get("code").and_then(Value::as_str),
        Some("unknown_method")
    );
    assert!(wire.get("backend").is_none() || wire["backend"].is_null());
}

// ---------------------------------------------------------------------------
// T-210: conformance vectors (envelope, framing, chunking, hashing, paths)
// ---------------------------------------------------------------------------

/// Test-local helper self-check: standard SHA-256 known-answer vectors.
#[test]
fn t210_sha256_selfcheck() {
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

/// Test-local helper self-check: standard base64 known-answer vectors.
#[test]
fn t210_b64_selfcheck() {
    assert_eq!(b64_decode("TWFu").expect("decode"), b"Man");
    assert_eq!(b64_decode("TWE=").expect("decode"), b"Ma");
    assert_eq!(b64_decode("").expect("decode"), b"");
    assert!(b64_decode("***=").is_err(), "bad alphabet must fail");
    assert!(
        b64_decode("TWF").is_err(),
        "length not multiple of 4 must fail"
    );
}

#[test]
fn t210_consts() {
    assert_eq!(
        clipmonger_ipc::proto::MAX_LINE_BYTES,
        1_048_576,
        "line cap is exactly 1 MiB"
    );
    assert_eq!(
        clipmonger_ipc::proto::CHUNK_RAW_BYTES,
        262_144,
        "chunk raw size is exactly 256 KiB"
    );
    assert_eq!(
        clipmonger_ipc::proto::IPC_VERSION,
        "ipc/v1",
        "protocol version string"
    );
}

#[test]
fn t210_framekind_wire() {
    // Wire values are lowercase per contract (D-F17/S9).
    assert_eq!(
        serde_json::to_value(FrameKind::Req).expect("ser"),
        json!("req")
    );
    assert_eq!(
        serde_json::to_value(FrameKind::Res).expect("ser"),
        json!("res")
    );
    assert_eq!(
        serde_json::to_value(FrameKind::Event).expect("ser"),
        json!("event")
    );
    assert_eq!(
        serde_json::to_value(FrameKind::Cancel).expect("ser"),
        json!("cancel")
    );
    // Negative control: unknown wire values must not parse as a kind.
    assert!(serde_json::from_value::<FrameKind>(json!("REQ")).is_err());
    assert!(serde_json::from_value::<FrameKind>(json!("reply")).is_err());
}

#[test]
fn t210_envelope_roundtrip_id_echo() {
    // Responses echo the request id; every frame carries v + id + kind.
    let req = Envelope {
        v: clipmonger_ipc::proto::IPC_VERSION.to_string(),
        id: "req-42".to_string(),
        kind: FrameKind::Req,
        method: Some("history.list".to_string()),
        params: Some(json!({"limit": 10})),
        result: None,
        error: None,
        seq: Some(3),
    };
    let bytes = framing::encode_line(&req).expect("encode");
    let back = framing::decode_line(&bytes).expect("decode");
    assert_eq!(back.v, "ipc/v1");
    assert_eq!(
        back.id, "req-42",
        "id must survive the roundtrip (echo shape)"
    );
    assert_eq!(back.method, Some("history.list".to_string()));
    assert_eq!(back.seq, Some(3));
}

#[test]
fn t210_version_mismatch_rejected() {
    // A frame whose version is not ipc/v1 must be rejected, never served.
    let bad = Envelope {
        v: "ipc/v0".to_string(),
        id: "x".to_string(),
        kind: FrameKind::Req,
        method: Some("ping".to_string()),
        params: None,
        result: None,
        error: None,
        seq: None,
    };
    let bytes = framing::encode_line(&bad).expect("encode of bad version still encodes");
    assert!(
        framing::decode_line(&bytes).is_err(),
        "decode must reject version mismatch"
    );
}

#[test]
fn t210_oversize_encode_rejected() {
    // encode_line rejects lines over MAX_LINE_BYTES (measured excl newline).
    let big = "x".repeat(1_048_576 + 1);
    let env = Envelope {
        v: clipmonger_ipc::proto::IPC_VERSION.to_string(),
        id: "big".to_string(),
        kind: FrameKind::Res,
        method: None,
        params: None,
        result: Some(json!({"blob": big})),
        error: None,
        seq: None,
    };
    assert!(
        framing::encode_line(&env).is_err(),
        "oversize line must be rejected by encode_line"
    );
    // A frame just under the cap must still encode.
    let ok_size = 1_048_576 - 512;
    let ok_env = Envelope {
        v: clipmonger_ipc::proto::IPC_VERSION.to_string(),
        id: "ok".to_string(),
        kind: FrameKind::Res,
        method: None,
        params: None,
        result: Some(json!({"pad": "y".repeat(ok_size)})),
        error: None,
        seq: None,
    };
    assert!(framing::encode_line(&ok_env).is_ok());
}

#[test]
fn t210_decode_garbage_rejected() {
    // Negative controls: non-JSON and wrong-shape lines never decode.
    assert!(framing::decode_line(b"not json at all").is_err());
    assert!(framing::decode_line(b"").is_err());
    assert!(framing::decode_line(b"[1,2,3]").is_err());
}

#[test]
fn t210_chunk_3mib_12_chunks() {
    // 3 MiB == 12 x 256 KiB data chunks exactly; reassembly hash must match.
    let mut bytes = Vec::with_capacity(3 * 1024 * 1024);
    for i in 0..3 * 1024 * 1024 {
        bytes.push(u8::try_from(i % 251).expect("remainder below 251 fits in u8"));
    }
    let expected = sha256_hex(&bytes);
    let chunks = framing::chunk_blob(&bytes);
    let data_chunks: Vec<_> = chunks.iter().filter(|c| !c.data_b64.is_empty()).collect();
    assert_eq!(
        data_chunks.len(),
        12,
        "3 MiB must yield exactly 12 data chunks"
    );
    for (i, c) in data_chunks.iter().enumerate() {
        assert_eq!(c.seq, i as u64, "seq numbers start at 0 and are contiguous");
        assert_eq!(
            c.data_b64.len(),
            349_528,
            "each full 256 KiB chunk encodes to 349528 base64 chars"
        );
    }
    assert!(
        chunks.last().expect("non-empty").eof,
        "final chunk carries eof"
    );
    assert_eq!(
        chunks.last().expect("non-empty").sha256.as_deref(),
        Some(expected.as_str()),
        "final chunk carries the total SHA-256 hex"
    );
    // Reassembly: concatenated decoded payloads equal the input.
    let mut rebuilt = Vec::with_capacity(bytes.len());
    for c in &chunks {
        rebuilt.extend_from_slice(&b64_decode(&c.data_b64).expect("valid base64"));
    }
    assert_eq!(rebuilt, bytes, "reassembled bytes must equal input");
    assert_eq!(sha256_hex(&rebuilt), expected);
}

#[test]
fn t210_chunk_empty_eof() {
    // D-F19 zero-length edge: empty input ends the stream with an empty eof chunk.
    let chunks = framing::chunk_blob(&[]);
    assert_ne!(chunks, [] as [clipmonger_ipc::framing::Chunk; 0]);
    let last = chunks.last().expect("non-empty");
    assert!(last.eof, "empty input terminates with eof");
    assert_eq!(last.data_b64, "");
}

#[test]
fn t210_chunk_partial_tail() {
    // Non-multiple input: last chunk is short but still carries eof.
    let bytes = vec![0xABu8; 262_144 + 100];
    let chunks = framing::chunk_blob(&bytes);
    let data: Vec<_> = chunks.iter().filter(|c| !c.data_b64.is_empty()).collect();
    assert_eq!(data.len(), 2);
    assert_eq!(data[0].seq, 0);
    assert_eq!(data[1].seq, 1);
    assert!(chunks.last().expect("non-empty").eof);
    let mut rebuilt = Vec::new();
    for c in &chunks {
        rebuilt.extend_from_slice(&b64_decode(&c.data_b64).expect("valid base64"));
    }
    assert_eq!(rebuilt, bytes);
}

#[test]
fn t210_profile_hash_stable_16hex() {
    let a = framing::profile_hash(Path::new("/home/u/.config/clipmonger"));
    let b = framing::profile_hash(Path::new("/home/u/.config/clipmonger"));
    assert_eq!(a, b, "profile hash must be stable");
    assert_eq!(a.len(), 16, "profile hash is 16 hex chars (D-F20)");
    assert!(
        a.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "profile hash is lowercase hex"
    );
    let other = framing::profile_hash(Path::new("/home/u/.config/other"));
    assert_ne!(a, other, "different profile dirs hash differently");
}

#[test]
fn t210_socket_fallback_over_100() {
    // D-F23: profile paths longer than 100 chars fall back under runtime_dir
    // as clipmonger-<profilehash>.sock.
    let long: String = std::iter::repeat_n('p', 120).collect();
    let mut profile = PathBuf::from("/home/u/.config/clipmonger");
    profile.push(format!("profiles-{long}"));
    assert!(profile.to_string_lossy().len() > 100);
    let runtime = Path::new("/tmp/rt");
    let got = framing::socket_path_for(&profile, runtime);
    let expected_name = format!("clipmonger-{}.sock", framing::profile_hash(&profile));
    assert_eq!(
        got,
        runtime.join(&expected_name),
        "long profile path must fall back to runtime dir with hashed name"
    );
    // Short profile paths stay under the profile dir (exact file name is
    // builder freedom; only the parent is pinned).
    let short = Path::new("/tmp/short-profile");
    let got_short = framing::socket_path_for(short, runtime);
    assert_eq!(
        got_short.parent(),
        Some(short),
        "short profile path must stay under the profile dir"
    );
}

// ---------------------------------------------------------------------------
// T-205: client <-> server over a real Unix socket in a tempdir.
// Gated on the `server` feature; run with `--features server`.
// ---------------------------------------------------------------------------

#[cfg(all(feature = "server", unix))]
const TEST_TOKEN: &str = "harness-token-1";

/// Build a request envelope (client-chosen id).
#[cfg(all(feature = "server", unix))]
fn req(id: &str, method: &str, params: Value) -> Envelope {
    Envelope {
        v: clipmonger_ipc::proto::IPC_VERSION.to_string(),
        id: id.to_string(),
        kind: FrameKind::Req,
        method: Some(method.to_string()),
        params: Some(params),
        result: None,
        error: None,
        seq: None,
    }
}

/// Build a cancel envelope for a previously sent request id.
#[cfg(all(feature = "server", unix))]
fn cancel_env(target_id: &str) -> Envelope {
    Envelope {
        v: clipmonger_ipc::proto::IPC_VERSION.to_string(),
        id: target_id.to_string(),
        kind: FrameKind::Cancel,
        method: None,
        params: None,
        result: None,
        error: None,
        seq: None,
    }
}

/// Shared handler for live tests: echo + blob accumulation + blocking call.
/// Blob state and the unblock channel are per-test (fresh per bind).
#[cfg(all(feature = "server", unix))]
fn test_handler(
    store: Arc<Mutex<BTreeMap<u64, Vec<u8>>>>,
    unblock: mpsc::Receiver<()>,
) -> Arc<dyn Fn(String, Value) -> Result<Value, IpcError> + Send + Sync> {
    let unblock = Mutex::new(unblock);
    Arc::new(move |method: String, params: Value| {
        let unknown = |m: &str| IpcError {
            code: "unknown_method".to_string(),
            message: format!("no such method: {m}"),
            retryable: false,
            backend: None,
        };
        match method.as_str() {
            "echo" => Ok(params),
            "block" => {
                unblock
                    .lock()
                    .expect("unblock lock")
                    .recv()
                    .expect("test must unblock the handler");
                Ok(json!({"unblocked": true}))
            }
            "blob.put" => {
                let p: BlobPutParams = serde_json::from_value(params).map_err(|e| IpcError {
                    code: "invalid_params".to_string(),
                    message: format!("bad blob.put params: {e}"),
                    retryable: false,
                    backend: None,
                })?;
                let raw = b64_decode(&p.data_b64).map_err(|e| IpcError {
                    code: "invalid_params".to_string(),
                    message: format!("bad base64: {e}"),
                    retryable: false,
                    backend: None,
                })?;
                store.lock().expect("store lock").insert(p.seq, raw);
                Ok(json!({"ok": true}))
            }
            "blob.assemble" => {
                let guard = store.lock().expect("store lock");
                let mut all = Vec::new();
                let mut seq = 0u64;
                while let Some(part) = guard.get(&seq) {
                    all.extend_from_slice(part);
                    seq += 1;
                }
                Ok(json!({"sha256": sha256_hex(&all), "bytes": all.len()}))
            }
            _ => Err(unknown(&method)),
        }
    })
}

/// Bind a server on a fresh tempdir socket. The returned handle is kept alive
/// by the caller until the test ends; no shutdown API is assumed.
#[cfg(all(feature = "server", unix))]
async fn bind_test_server(sock: &Path) -> (mpsc::Sender<()>, Arc<Mutex<BTreeMap<u64, Vec<u8>>>>) {
    let store: Arc<Mutex<BTreeMap<u64, Vec<u8>>>> = Arc::new(Mutex::new(BTreeMap::new()));
    let (tx, rx) = mpsc::channel::<()>();
    let handler = test_handler(Arc::clone(&store), rx);
    let Ok(server) = Server::bind(sock, TEST_TOKEN.to_string(), handler).await else {
        panic!("Server::bind must succeed on a fresh socket")
    };
    // Keep the server alive for the whole test without assuming a shutdown API.
    std::mem::forget(server);
    (tx, store)
}

/// Connect the real std client. `#[allow(unused_mut)]` tolerates both
/// `call(&self)` and `call(&mut self)` builder choices.
#[cfg(all(feature = "server", unix))]
fn connect_client(sock: &Path, token: &str) -> Client {
    match Client::connect(sock, token) {
        Ok(c) => c,
        Err(e) => panic!("Client::connect must succeed with the right token: {e:?}"),
    }
}

#[cfg(all(feature = "server", unix))]
#[tokio::test(flavor = "multi_thread")]
async fn t205_echo_call() {
    let sock = unique_sock("echo");
    let (_tx, _store) = bind_test_server(&sock).await;
    #[allow(unused_mut)]
    let mut client = connect_client(&sock, TEST_TOKEN);
    let params = json!({"hello": "world", "n": 3});
    let Ok(got) = client.call("echo", params.clone()) else {
        panic!("echo call must succeed")
    };
    assert_eq!(got, params, "echo handler must return params unchanged");
    // Negative control: unknown method must fail, not echo.
    assert!(
        client.call("no.such.method", json!({})).is_err(),
        "unknown method must be an error"
    );
}

#[cfg(all(feature = "server", unix))]
#[tokio::test(flavor = "multi_thread")]
async fn t205_blob_chunk_frames() {
    // 3 MiB through chunk frames over a real socket; server reassembles.
    let sock = unique_sock("blob");
    let (_tx, _store) = bind_test_server(&sock).await;
    #[allow(unused_mut)]
    let mut client = connect_client(&sock, TEST_TOKEN);
    let mut bytes = Vec::with_capacity(3 * 1024 * 1024);
    for i in 0..3 * 1024 * 1024 {
        bytes.push(u8::try_from(i % 251).expect("remainder below 251 fits in u8"));
    }
    let expected = sha256_hex(&bytes);
    let clip = Uuid::now_v7();
    let chunks = framing::chunk_blob(&bytes);
    let data: Vec<_> = chunks.iter().filter(|c| !c.data_b64.is_empty()).collect();
    assert_eq!(
        data.len(),
        12,
        "3 MiB must travel as exactly 12 data chunks"
    );
    for c in &chunks {
        let params = serde_json::to_value(&BlobPutParams {
            clip,
            mime: "image/png".to_string(),
            seq: c.seq,
            data_b64: c.data_b64.clone(),
            eof: c.eof,
            sha256: c.sha256.clone(),
        })
        .expect("blob params serialize");
        let Ok(res) = client.call("blob.put", params) else {
            panic!("blob.put seq {} must be accepted", c.seq)
        };
        assert_eq!(res.get("ok"), Some(&Value::Bool(true)));
    }
    let Ok(assembled) = client.call("blob.assemble", json!({"clip": clip, "mime": "image/png"}))
    else {
        panic!("blob.assemble must succeed")
    };
    assert_eq!(
        assembled.get("sha256").and_then(Value::as_str),
        Some(expected.as_str()),
        "server-reassembled hash must match the source bytes"
    );
    assert_eq!(
        assembled.get("bytes").and_then(Value::as_u64),
        Some(bytes.len() as u64)
    );
}

#[cfg(all(feature = "server", unix))]
#[tokio::test(flavor = "multi_thread")]
async fn t205_bad_token() {
    // D-F20: wrong token -> auth_failed, connection not established.
    let sock = unique_sock("token");
    let (_tx, _store) = bind_test_server(&sock).await;
    match Client::connect(&sock, "wrong-token") {
        Ok(_) => panic!("connect with a bad token must fail"),
        Err(e) => assert_eq!(e.code, "auth_failed", "bad token maps to auth_failed"),
    }
    // The server is still healthy for the right token afterwards.
    #[allow(unused_mut)]
    let mut client = connect_client(&sock, TEST_TOKEN);
    let Ok(got) = client.call("echo", json!({"after": "bad-token-attempt"})) else {
        panic!("server must survive a bad-token attempt")
    };
    assert_eq!(got, json!({"after": "bad-token-attempt"}));
}

// --- Raw-socket helpers (std only): used for cancel + oversize-drop, where
// the harness must speak frames the Client API does not expose. ---

/// Write one envelope as a JSONL line (tolerates builders whose `encode_line`
/// does or does not append the newline).
#[cfg(all(feature = "server", unix))]
fn send_env(stream: &mut UnixStream, env: &Envelope) {
    let mut bytes = framing::encode_line(env).expect("encode_line must succeed");
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    stream.write_all(&bytes).expect("frame write must succeed");
    stream.flush().expect("flush must succeed");
}

/// Read one envelope; None on EOF or read timeout (used to assert "no res").
#[cfg(all(feature = "server", unix))]
fn recv_env(reader: &mut BufReader<UnixStream>) -> Option<Envelope> {
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) => None,
        Ok(_) => {
            let trimmed = line.trim_end_matches(['\n', '\r']);
            Some(framing::decode_line(trimmed.as_bytes()).expect("server frames must decode"))
        }
        Err(e)
            if e.kind() == std::io::ErrorKind::TimedOut
                || e.kind() == std::io::ErrorKind::WouldBlock =>
        {
            None
        }
        Err(e) => panic!("frame read failed unexpectedly: {e}"),
    }
}

/// Raw hello handshake; returns (writer, reader, welcome frame).
#[cfg(all(feature = "server", unix))]
fn raw_hello(sock: &Path, id: &str, token: &str) -> (UnixStream, BufReader<UnixStream>, Envelope) {
    let writer = UnixStream::connect(sock).expect("raw connect must succeed");
    writer
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("timeout must be settable");
    let mut w = writer.try_clone().expect("clone writer");
    let mut r = BufReader::new(writer.try_clone().expect("clone reader"));
    send_env(
        &mut w,
        &req(
            id,
            "hello",
            json!({"min": 1, "max": 1, "token": token, "client": "ipc-harness"}),
        ),
    );
    let welcome = recv_env(&mut r).expect("server must answer hello");
    (w, r, welcome)
}

#[cfg(all(feature = "server", unix))]
#[tokio::test(flavor = "multi_thread")]
async fn t205_cancel_wire() {
    // Cancel semantics (builder picks the outcome): after cancel{id} the
    // in-flight request gets NO res or a `cancelled` error — never a success
    // result — and the connection stays usable afterwards.
    let sock = unique_sock("cancel");
    let (unblock, _store) = bind_test_server(&sock).await;
    let (mut w, mut r, welcome) = raw_hello(&sock, "h1", TEST_TOKEN);
    assert_eq!(welcome.id, "h1");
    assert!(welcome.error.is_none(), "hello must be welcomed, not bye");
    assert!(
        welcome.result.is_some() || welcome.method.is_some(),
        "welcome must carry a result payload or a welcome method marker"
    );

    send_env(&mut w, &req("r1", "block", json!({})));
    send_env(&mut w, &cancel_env("r1"));
    // r1b proves the server consumed the cancel before we unblock the handler
    // (same-connection FIFO); without this barrier the test would race.
    send_env(&mut w, &req("r1b", "echo", json!({"barrier": 1})));
    let mut saw_barrier = false;
    for _ in 0..20 {
        match recv_env(&mut r) {
            Some(f) if f.id == "r1b" => {
                assert!(f.error.is_none());
                saw_barrier = true;
                break;
            }
            Some(_) | None => {}
        }
    }
    assert!(saw_barrier, "barrier echo must answer while r1 is blocked");

    unblock.send(()).expect("unblock must send");
    send_env(&mut w, &req("r2", "echo", json!({"after": "cancel"})));

    let mut r1_success = false;
    let mut got_r2 = false;
    for _ in 0..20 {
        match recv_env(&mut r) {
            Some(f) if f.id == "r1" => {
                let is_success = f.error.is_none() && f.result.is_some();
                if is_success {
                    r1_success = true;
                } else if let Some(e) = &f.error {
                    assert_eq!(
                        e.code, "cancelled",
                        "a res for a cancelled id must carry code=cancelled"
                    );
                }
            }
            Some(f) if f.id == "r2" => {
                assert!(
                    f.error.is_none(),
                    "connection must stay usable after cancel"
                );
                assert_eq!(f.result, Some(json!({"after": "cancel"})));
                got_r2 = true;
                break;
            }
            Some(_) | None => {}
        }
    }
    assert!(
        !r1_success,
        "cancelled request must never get a success result"
    );
    assert!(
        got_r2,
        "post-cancel call must be served on the same connection"
    );
}

#[cfg(all(feature = "server", unix))]
#[tokio::test(flavor = "multi_thread")]
async fn t205_oversize_drop() {
    // Oversize line -> server drops THAT connection, but stays up.
    let sock = unique_sock("oversize");
    let (_tx, _store) = bind_test_server(&sock).await;
    let (mut w, mut r, welcome) = raw_hello(&sock, "h1", TEST_TOKEN);
    assert!(welcome.error.is_none());

    let big = vec![b'z'; 2 * 1024 * 1024];
    w.write_all(&big).expect("oversize write");
    w.write_all(b"\n").expect("newline write");
    let _ = w.flush();
    // The poisoned connection must never serve again: a follow-up echo on it
    // gets no response (EOF or timeout both read as None here). The write
    // itself may fail with EPIPE if the server already closed: that also
    // proves the drop, so write errors are ignored here.
    if let Ok(mut bytes) = framing::encode_line(&req("r9", "echo", json!({"lost": true}))) {
        if !bytes.ends_with(b"\n") {
            bytes.push(b'\n');
        }
        let _ = w.write_all(&bytes);
        let _ = w.flush();
    }
    let mut r9_answered = false;
    for _ in 0..6 {
        match recv_env(&mut r) {
            Some(f) if f.id == "r9" => {
                r9_answered = true;
                break;
            }
            _ => {}
        }
    }
    assert!(
        !r9_answered,
        "oversize connection must be dropped, not served"
    );
    // A fresh client still gets served: the server itself survived.
    #[allow(unused_mut)]
    let mut client = connect_client(&sock, TEST_TOKEN);
    let Ok(got) = client.call("echo", json!({"alive": 1})) else {
        panic!("server must survive an oversize drop")
    };
    assert_eq!(got, json!({"alive": 1}));
}

// ---------------------------------------------------------------------------
// T-205 (event bus): slow-subscriber overflow -> resync-required via tick().
// No wall-clock sleeps anywhere: the eviction clock is injected.
// ---------------------------------------------------------------------------

#[cfg(all(feature = "server", unix))]
fn bus_event(id: &str) -> Envelope {
    Envelope {
        v: clipmonger_ipc::proto::IPC_VERSION.to_string(),
        id: id.to_string(),
        kind: FrameKind::Event,
        method: Some("test".to_string()),
        params: Some(json!({"n": id})),
        result: None,
        error: None,
        seq: None,
    }
}

#[cfg(all(feature = "server", unix))]
#[test]
fn t205_eventbus_overflow_resync() {
    // D-F18: per-subscriber queue holds 128; overflow drops events and emits
    // a resync-required event instead of blocking or growing unboundedly.
    let mut bus = EventBus::new(Duration::from_secs(300));
    let sub = bus.subscribe(vec!["test".to_string()]);
    for i in 0..200 {
        bus.publish(bus_event(&format!("e{i:03}")));
    }
    let got = bus.pending(sub);
    assert_eq!(
        got.len(),
        129,
        "128 queued events plus exactly one resync-required"
    );
    for (i, env) in got.iter().enumerate().take(128) {
        assert_eq!(
            env.id,
            format!("e{i:03}"),
            "oldest events are kept in order"
        );
    }
    let last = &got[128];
    assert!(
        matches!(last.kind, FrameKind::Event),
        "resync-required is an event frame"
    );
    assert_eq!(
        last.method.as_deref(),
        Some("resync-required"),
        "overflow marker must be the resync-required push event"
    );
}

#[cfg(all(feature = "server", unix))]
#[test]
fn t205_eventbus_capacity_no_false_resync() {
    // Boundary: exactly 128 queued events fit with NO resync-required.
    let mut bus = EventBus::new(Duration::from_secs(300));
    let sub = bus.subscribe(vec!["test".to_string()]);
    for i in 0..128 {
        bus.publish(bus_event(&format!("q{i:03}")));
    }
    let got = bus.pending(sub);
    assert_eq!(got.len(), 128);
    assert!(
        got.iter()
            .all(|e| e.method.as_deref() != Some("resync-required")),
        "no resync below the overflow boundary"
    );
}

#[cfg(all(feature = "server", unix))]
#[test]
fn t205_eventbus_idle_evict_tick() {
    // Idle eviction is driven by the injected clock: tick(now), no sleeps.
    let t0 = Instant::now();
    let mut bus = EventBus::new(Duration::from_secs(300));
    let sub = bus.subscribe(vec!["test".to_string()]);
    bus.tick(t0 + Duration::from_secs(301));
    let got = bus.pending(sub);
    assert!(
        got.is_empty(),
        "subscriber idle past the limit must be evicted"
    );

    // A subscriber ticked within the limit keeps receiving.
    let mut bus2 = EventBus::new(Duration::from_secs(300));
    let sub2 = bus2.subscribe(vec!["test".to_string()]);
    bus2.tick(t0 + Duration::from_secs(60));
    bus2.publish(bus_event("still-here"));
    let got2 = bus2.pending(sub2);
    assert_eq!(
        got2.len(),
        1,
        "subscriber within the idle limit survives tick"
    );
    assert_eq!(got2[0].id, "still-here");
}
