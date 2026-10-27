//! Perf snapshot (seg-11 §8 calibration; benches-free).
//!
//! Seeds a 10k-clip store via the public `Store` API and times
//! `Store::search`. Numbers are calibration data printed with
//! `--nocapture`; the only asserts are sanity (results correct, no
//! pathological blow-up). Run:
//! `cargo test -p clipmonger-core --test perf_snapshot -- --ignored --nocapture`
//! Marked `#[ignore]` so the normal workspace gate stays fast.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use clipmonger_core::model::{Clip, Rev};
use clipmonger_core::Store;
use uuid::Uuid;

static CTR: AtomicU64 = AtomicU64::new(0);

fn sample_clip(id: Uuid, text: &str, last_used_ms: i64) -> Clip {
    Clip {
        id,
        created_ms: last_used_ms,
        source_app: Some("perf".to_string()),
        source_os: "linux".to_string(),
        pinned: false,
        removed: false,
        preview: text.to_string(),
        search_text: text.to_string(),
        formats: vec!["text/plain;charset=utf-8".to_string()],
        bytes_total: text.len() as u64,
        content_hash: format!("hash-{}", id.as_simple()),
        rev: Rev {
            wall_ms: last_used_ms,
            dev: "perf".to_string(),
            ctr: 0,
        },
        last_used_ms,
        origin_dev: "perf".to_string(),
        origin: "x11".to_string(),
    }
}

#[test]
#[ignore = "perf calibration only: run explicitly with --ignored --nocapture"]
fn perf_10k_search_snapshot() {
    let n = CTR.fetch_add(1, Ordering::SeqCst);
    let profile: PathBuf =
        std::env::temp_dir().join(format!("clipmonger-perf-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&profile);
    std::fs::create_dir_all(&profile).expect("temp profile");
    let store = Store::open(&profile).expect("store opens");

    let seed_start = Instant::now();
    for i in 0..10_000_i64 {
        let text = format!("perfclip {i} common token group{}", i % 100);
        let clip = sample_clip(Uuid::now_v7(), &text, 1_700_000_000_000 + i);
        store
            .insert_clip(
                &clip,
                &[("text/plain;charset=utf-8".to_string(), text.into_bytes())],
            )
            .expect("insert");
    }
    let seed_ms = seed_start.elapsed().as_secs_f64() * 1000.0;

    // Warm-up + sanity: every clip contains "common".
    let hits = store.search("common").expect("search");
    assert_eq!(hits.len(), 10_000, "search sanity: all clips match");
    let hits = store.search("perfclip").expect("search");
    assert!(!hits.is_empty(), "search sanity: perfclip matches");

    let mut times_ms: Vec<f64> = Vec::new();
    for _ in 0..50 {
        let start = Instant::now();
        let hits = store.search("common token").expect("search");
        assert_eq!(hits.len(), 10_000);
        times_ms.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    times_ms.sort_by(|a, b| a.partial_cmp(b).expect("times finite"));
    let p50 = times_ms[times_ms.len() / 2];
    let p95 = times_ms[((times_ms.len() * 95) / 100).min(times_ms.len() - 1)];
    let mean =
        times_ms.iter().sum::<f64>() / f64::from(u32::try_from(times_ms.len()).unwrap_or(u32::MAX));
    println!(
        "perf_snapshot: seed_10k_ms={seed_ms:.1} search_10k n={} mean_ms={mean:.2} p50_ms={p50:.2} p95_ms={p95:.2} max_ms={:.2}",
        times_ms.len(),
        times_ms.last().copied().unwrap_or(0.0)
    );
    // Sanity only (calibration, not the §8 budget): 10k search must not
    // be pathologically slow on any box.
    assert!(
        p95 < 10_000.0,
        "search p95 sanity bound breached: {p95:.1} ms"
    );
    let _ = std::fs::remove_dir_all(&profile);
}
