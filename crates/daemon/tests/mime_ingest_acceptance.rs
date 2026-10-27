//! MIME ingest regression (Step 6 manual QA, Ubuntu 24.04, 2026-10-27).
//!
//! X11's canonical text MIME is `text/plain;charset=utf-8`; the daemon's
//! ingest matched only the exact string `text/plain`, so real captures were
//! stored with empty `preview`/`search_text` and rendered as blank rows.
//!
//! Run: `cargo test -p clipmonger-daemon --test mime_ingest_acceptance`

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use clipmonger_core::Store;
use clipmonger_daemon::DaemonState;
use serde_json::{json, Value};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn fresh_state(tag: &str) -> (PathBuf, DaemonState) {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("cm-mime-ingest-{tag}-{n}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp profile must be creatable");
    let store = Store::open(&dir).expect("open store");
    let state = DaemonState::new(store);
    state.set_profile_dir(&dir);
    (dir, state)
}

fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let mut word: u32 = 0;
        for (i, &byte) in chunk.iter().enumerate() {
            word |= u32::from(byte) << (16 - 8 * i);
        }
        let chars = (chunk.len() * 8).div_ceil(6);
        for i in 0..4 {
            if i < chars {
                out.push(ALPHABET[((word >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[test]
fn charset_text_capture_stores_preview_and_search_text() {
    let (_dir, state) = fresh_state("charset");
    let got = state
        .handle(
            "capture.report",
            &json!({
                "event": {"sensitive": false, "origin": "Clipboard"},
                "payloads": {"text/plain;charset=utf-8": base64_encode(b"QA-ALPHA-123")},
            }),
        )
        .expect("capture.report");
    assert_eq!(got.get("accepted"), Some(&Value::Bool(true)));

    let list = state
        .handle("history.list", &json!({}))
        .expect("history.list");
    let clips = list
        .get("clips")
        .and_then(Value::as_array)
        .expect("clips array");
    assert_eq!(clips.len(), 1, "one clip stored, got {clips:?}");
    assert_eq!(
        clips[0].get("preview").and_then(Value::as_str),
        Some("QA-ALPHA-123"),
        "charset text capture must fill preview"
    );
    assert_eq!(
        clips[0].get("search_text").and_then(Value::as_str),
        Some("QA-ALPHA-123"),
        "charset text capture must fill search_text"
    );
}
