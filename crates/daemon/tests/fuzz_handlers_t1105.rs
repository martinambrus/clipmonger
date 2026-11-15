//! T-1105, daemon side: random and mutated params through the daemon's
//! request handlers. No method may panic, whatever its params. Methods that
//! write to caller-chosen paths (`backup.*`), paste (`paste.execute`) or stop
//! the process are left out; everything else runs against a temp profile.
//!
//! `FUZZ_CASES` overrides the case count (default 20 000; the release gate
//! runs it with 200 000); `FUZZ_SEED` the seed.

use clipmonger_daemon::DaemonState;
use serde_json::{json, Value};

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
}

const METHODS: &[&str] = &[
    "status",
    "caps.get",
    "ping",
    "history.list",
    "history.compact",
    "history.compact.cancel",
    "history.get",
    "history.delete",
    "history.pin",
    "history.purge_removed",
    "history.thumbnail",
    "monitor.set",
    "config.get",
    "config.set",
    "logs.tail",
    "capture.report",
    "blob.put",
    "permanent.list",
    "permanent.create",
    "permanent.update",
    "permanent.delete",
    "permanent.groups",
    "permanent.group_create",
    "permanent.group_delete",
    "db.info",
    "db.compact",
    "no.such.method",
];

const KEYS: &[&str] = &[
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
    "permanent",
    "sensitive",
    "text",
    "seq",
    "data_b64",
    "eof",
    "sha256",
    "paused",
    "include_removed",
    "offset",
    "page",
    "request",
    "kind",
];

fn value(rng: &mut Rng, depth: u32) -> Value {
    match rng.below(if depth > 2 { 6 } else { 9 }) {
        0 => Value::Null,
        1 => Value::Bool(rng.next() & 1 == 1),
        2 => json!(rng.next() % 2000),
        3 => json!(i64::from_ne_bytes(rng.next().to_ne_bytes())),
        4 => json!(uuid_like(rng)),
        5 => {
            let len = rng.below(16);
            Value::String(
                (0..len)
                    .map(|_| {
                        char::from_u32(u32::try_from(rng.below(0x3000)).unwrap_or(0)).unwrap_or('x')
                    })
                    .collect(),
            )
        }
        6 | 7 => {
            let mut map = serde_json::Map::new();
            for _ in 0..rng.below(5) {
                map.insert(
                    KEYS[rng.below(KEYS.len())].to_string(),
                    value(rng, depth + 1),
                );
            }
            Value::Object(map)
        }
        _ => Value::Array((0..rng.below(4)).map(|_| value(rng, depth + 1)).collect()),
    }
}

fn uuid_like(rng: &mut Rng) -> String {
    let n = u128::from(rng.next()) << 64 | u128::from(rng.next());
    let id = uuid::Uuid::from_u128(n);
    if rng.below(4) == 0 {
        id.simple().to_string()
    } else {
        id.hyphenated().to_string()
    }
}

#[test]
fn t1105_daemon_handlers_never_panic_on_generated_params() {
    let cases: u64 = std::env::var("FUZZ_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000);
    let seed: u64 = std::env::var("FUZZ_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0x1105_DAE0);
    let dir = std::env::temp_dir().join(format!("cm-fuzz-handlers-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = clipmonger_core::Store::open(&dir).expect("store");
    let state = DaemonState::new(store);
    state.set_profile_dir(&dir);
    // Some real rows, so ids and handlers have something to find.
    for text in ["alpha", "beta", "gamma"] {
        state.ingest_capture(
            "x11",
            false,
            &[("text/plain".to_string(), text.as_bytes().to_vec())],
        );
    }
    let mut rng = Rng(seed | 1);
    let mut ok = 0_u64;
    for case in 0..cases {
        let method = METHODS[rng.below(METHODS.len())];
        let params = value(&mut rng, 0);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            state.handle(method, &params)
        }));
        match outcome {
            Ok(Ok(_)) => ok += 1,
            Ok(Err(_)) => {}
            Err(payload) => panic!(
                "case {case} (seed {seed:#x}): {method} panicked ({:?}) on {params}",
                payload.downcast_ref::<String>()
            ),
        }
    }
    eprintln!("T-1105 daemon: {cases} cases, {ok} succeeded, 0 panics (seed {seed:#x})");
    let _ = std::fs::remove_dir_all(&dir);
}
