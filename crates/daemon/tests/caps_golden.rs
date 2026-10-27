//! Caps golden test (seg-11 §5 gate 2, headless leg; R-F7).
//!
//! Compares a normalized headless `DaemonState` caps snapshot against
//! `tests/golden/caps_headless.json`. No volatile fields exist in the
//! headless snapshot (no paths, timestamps, tokens or PIDs): the skeleton
//! snapshot is built only from static level/reason strings and booleans.
//! Keys are sorted recursively before comparison.
//!
//! Regenerate after an intentional caps change:
//! `CLIPMONGER_BLESS=1 cargo test -p clipmonger-daemon --test caps_golden`

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

static CTR: AtomicU64 = AtomicU64::new(0);

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join("caps_headless.json")
}

/// Recursively sort object keys (`serde_json` `Map` is already a
/// `BTreeMap` without `preserve_order`; this keeps the guarantee explicit).
fn normalize(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for key in keys {
                out.insert(key.clone(), normalize(&map[key]));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(normalize).collect()),
        other => other.clone(),
    }
}

#[test]
fn caps_headless_matches_golden() {
    let n = CTR.fetch_add(1, Ordering::SeqCst);
    let profile =
        std::env::temp_dir().join(format!("clipmonger-caps-golden-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&profile);
    std::fs::create_dir_all(&profile).expect("temp profile");
    let store = clipmonger_core::Store::open(&profile).expect("store opens");
    let state = clipmonger_daemon::DaemonState::new(store);
    let actual = normalize(&state.caps_snapshot());
    let actual_text = serde_json::to_string_pretty(&actual).expect("caps serialize") + "\n";

    let path = golden_path();
    if std::env::var_os("CLIPMONGER_BLESS").is_some_and(|v| v == "1") {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("golden dir");
        }
        std::fs::write(&path, &actual_text).expect("bless golden");
        let _ = std::fs::remove_dir_all(&profile);
        return;
    }

    let golden_text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "golden missing at {} ({e}); bless with CLIPMONGER_BLESS=1",
            path.display()
        )
    });
    let golden: Value = serde_json::from_str(&golden_text).expect("golden is JSON");
    assert_eq!(
        normalize(&golden),
        actual,
        "headless caps drifted from golden {}; bless with CLIPMONGER_BLESS=1 if intentional\nactual:\n{actual_text}",
        path.display()
    );
    let _ = std::fs::remove_dir_all(&profile);
}
