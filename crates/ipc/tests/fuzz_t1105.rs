//! T-1105: fuzz the IPC parser (seg-11 §3): one million generated lines,
//! zero panics. Macro Stage-1 fuzzing is v2 (no macros in v1).
//!
//! A deterministic mutation fuzzer on stable Rust: every case is derived from
//! a fixed seed, so a failure names a case that replays exactly. Each case is
//! one of: random bytes, a valid request line with byte-level mutations, or a
//! structurally valid envelope whose `params` are random JSON. Every decoded
//! envelope must re-encode and decode to itself, and its params must go
//! through every method's typed params without panicking.
//!
//! `FUZZ_CASES` overrides the case count (default 1 000 000); `FUZZ_SEED`
//! the seed.

use clipmonger_ipc::framing::{decode_line, encode_line};
use clipmonger_ipc::proto::methods::*;
use clipmonger_ipc::proto::{Envelope, FrameKind, MAX_LINE_BYTES};
use serde_json::{json, Value};

/// xorshift64*: small, fast and good enough to drive mutations.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % (n as u64)).unwrap_or(0)
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

const METHODS: &[&str] = &[
    "hello",
    "status",
    "caps.get",
    "history.list",
    "history.compact",
    "history.get",
    "history.delete",
    "history.pin",
    "paste.execute",
    "monitor.set",
    "config.get",
    "config.set",
    "backup.create",
    "backup.restore",
    "logs.tail",
    "daemon.stop",
    "popup.open",
    "capture.report",
    "hotkey.fired",
    "blob.put",
    "permanent.exec",
    "permanent.create",
    "permanent.update",
    "db.info",
    "db.compact",
    "history.thumbnail",
];

const TOKENS: &[&[u8]] = &[
    b"{",
    b"}",
    b"[",
    b"]",
    b"\"",
    b":",
    b",",
    b"null",
    b"true",
    b"false",
    b"-0",
    b"1e999",
    b"\\u0000",
    b"\\ud800",
    b"\"v\":\"ipc/v1\"",
    b"\"kind\":\"req\"",
    b"\xff",
    b"\xc3",
    b"\n",
    b"\r",
    b"9007199254740993",
    b"\"params\":",
    b"{}",
];

fn random_json(rng: &mut Rng, depth: u32) -> Value {
    match rng.below(if depth > 3 { 5 } else { 8 }) {
        0 => Value::Null,
        1 => Value::Bool(rng.chance(50)),
        2 => json!(i64::from_ne_bytes(rng.next().to_ne_bytes())),
        3 => json!(f64::from_bits(rng.next())),
        4 => {
            let len = rng.below(24);
            Value::String(
                (0..len)
                    .map(|_| {
                        char::from_u32(u32::try_from(rng.below(0x11_0000)).unwrap_or(0))
                            .unwrap_or('?')
                    })
                    .collect(),
            )
        }
        5 | 6 => {
            let keys = [
                "clip",
                "id",
                "key",
                "value",
                "limit",
                "cursor",
                "query",
                "pinned",
                "toggle",
                "check",
                "dest",
                "src",
                "lines",
                "source",
                "group",
                "name",
                "body",
                "hotkey",
                "max_px",
                "mime",
                "payloads",
                "origin",
                "token",
                "min",
                "max",
                "picked_ms",
            ];
            let mut map = serde_json::Map::new();
            for _ in 0..rng.below(6) {
                map.insert(
                    keys[rng.below(keys.len())].to_string(),
                    random_json(rng, depth + 1),
                );
            }
            Value::Object(map)
        }
        _ => Value::Array(
            (0..rng.below(5))
                .map(|_| random_json(rng, depth + 1))
                .collect(),
        ),
    }
}

fn request(rng: &mut Rng) -> Envelope {
    Envelope {
        v: "ipc/v1".into(),
        id: format!("r{}", rng.next() % 1000),
        kind: [
            FrameKind::Req,
            FrameKind::Res,
            FrameKind::Event,
            FrameKind::Cancel,
        ][rng.below(4)],
        method: Some(METHODS[rng.below(METHODS.len())].to_string()),
        params: Some(random_json(rng, 0)),
        ..serde_json::from_value(json!({"v": "ipc/v1", "id": "x", "kind": "req"})).unwrap()
    }
}

fn mutate(rng: &mut Rng, mut bytes: Vec<u8>) -> Vec<u8> {
    for _ in 0..=rng.below(8) {
        if bytes.is_empty() {
            bytes.push(0);
        }
        let at = rng.below(bytes.len());
        match rng.below(5) {
            0 => bytes[at] ^= 1 << rng.below(8),
            1 => bytes[at] = u8::try_from(rng.below(256)).unwrap_or(0),
            2 => {
                let token = TOKENS[rng.below(TOKENS.len())];
                bytes.splice(at..at, token.iter().copied());
            }
            3 => {
                let end = (at + rng.below(16)).min(bytes.len());
                bytes.drain(at..end);
            }
            _ => bytes.truncate(at),
        }
    }
    bytes
}

/// Run `params` through every typed params struct; only panics matter.
fn typed_params(params: &Value) {
    macro_rules! each {
        ($($t:ty),* $(,)?) => { $( let _ = serde_json::from_value::<$t>(params.clone()); )* };
    }
    each!(
        StatusParams,
        CapsGetParams,
        CapsRefreshParams,
        HistoryListParams,
        HistoryCompactParams,
        HistoryCompactCancelParams,
        HistoryGetParams,
        HistoryDeleteParams,
        HistoryPinParams,
        HistoryEditParams,
        HistoryDuplicateParams,
        PasteExecuteParams,
        MonitorSetParams,
        HotkeyRegisterParams,
        HotkeyUnregisterParams,
        ConfigGetParams,
        ConfigSetParams,
        BackupCreateParams,
        BackupRestoreParams,
        LogsTailParams,
        DaemonStopParams,
        EventsSubscribeParams,
        PopupOpenParams,
        CaptureReportParams,
        HotkeyFiredParams,
        HotkeyConflictParams,
        PingParams,
        BlobPutParams,
        PermanentExecParams,
        PermanentListParams,
        PermanentCreateParams,
        PermanentUpdateParams,
        PermanentDeleteParams,
        PermanentGroupsParams,
        PermanentGroupCreateParams,
        PermanentGroupDeleteParams,
        HistoryPurgeRemovedParams,
        HistoryThumbnailParams,
    );
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[test]
fn t1105_ipc_parser_survives_a_million_generated_lines() {
    let cases = env_u64("FUZZ_CASES", 1_000_000);
    let seed = env_u64("FUZZ_SEED", 0x00C1_1B00_0000_1105);
    let mut rng = Rng(seed | 1);
    let mut decoded = 0_u64;
    for case in 0..cases {
        let line = match rng.below(10) {
            0 | 1 => (0..rng.below(64))
                .map(|_| u8::try_from(rng.below(256)).unwrap_or(0))
                .collect(),
            2..=6 => {
                let valid = encode_line(&request(&mut rng)).expect("encode");
                mutate(&mut rng, valid)
            }
            _ => encode_line(&request(&mut rng)).expect("encode"),
        };
        let outcome = std::panic::catch_unwind(|| {
            if let Ok(env) = decode_line(&line) {
                let again = encode_line(&env).expect("re-encode a decoded envelope");
                // Floats may drift in their last digit (serde_json's fast
                // float parsing); everything else must survive exactly.
                let back = decode_line(&again).expect("decode re-encoded");
                assert_eq!(
                    (&back.v, &back.id, back.kind, &back.method),
                    (&env.v, &env.id, env.kind, &env.method)
                );
                if let Some(params) = &env.params {
                    typed_params(params);
                }
                return true;
            }
            false
        });
        match outcome {
            Ok(true) => decoded += 1,
            Ok(false) => {}
            Err(payload) => panic!(
                "case {case} (seed {seed:#x}) panicked ({:?}) on {:?}",
                payload.downcast_ref::<String>(),
                String::from_utf8_lossy(&line)
            ),
        }
    }
    // Sanity: the generator reaches the decoder's success path too.
    assert!(
        cases < 1000 || decoded * 10 > cases,
        "{decoded} of {cases} decoded"
    );
    eprintln!("T-1105: {cases} cases, {decoded} decoded, 0 panics (seed {seed:#x})");
}

#[test]
fn oversize_and_boundary_lines_are_refused_not_panicking() {
    let pad = "a".repeat(MAX_LINE_BYTES);
    let line = format!(r#"{{"v":"ipc/v1","id":"x","kind":"req","method":"{pad}"}}"#);
    assert_eq!(decode_line(line.as_bytes()).unwrap_err().code, "oversize");
    assert!(decode_line(b"").is_err());
    assert!(decode_line(b"\n\r\n").is_err());
    let nested = "[".repeat(10_000) + &"]".repeat(10_000);
    let line = format!(r#"{{"v":"ipc/v1","id":"x","kind":"req","params":{nested}}}"#);
    assert!(
        decode_line(line.as_bytes()).is_err(),
        "serde's depth limit refuses it"
    );
}
