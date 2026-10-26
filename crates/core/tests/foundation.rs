//! Foundation Step 1 CORE-MODEL controls-first tests.
//! T-301 model serde; T-311 unknown nested config keys; T-306 config
//! parse/defaults/env-override/portable; T-305 hash part; T-313 text part.

use clipmonger_core::{config, hash, model, policy};
use uuid::Uuid;

fn sample_clip() -> model::Clip {
    model::Clip {
        id: Uuid::now_v7(),
        created_ms: 1_700_000_000_000,
        source_app: Some("app.exe".to_string()),
        source_os: "windows".to_string(),
        pinned: false,
        removed: false,
        preview: "hello".to_string(),
        search_text: "hello".to_string(),
        formats: vec!["text/plain;charset=utf-8".to_string()],
        bytes_total: 5,
        content_hash: "abc".to_string(),
        rev: model::Rev {
            wall_ms: 1_700_000_000_000,
            dev: "dev1".to_string(),
            ctr: 0,
        },
        last_used_ms: 1_700_000_000_000,
        origin_dev: "dev1".to_string(),
        origin: "windows".to_string(),
    }
}

// T-301: model serde roundtrip + unknown-field tolerance.
#[test]
fn t301_clip_roundtrip() {
    let clip = sample_clip();
    let json = serde_json::to_string(&clip).unwrap();
    let back: model::Clip = serde_json::from_str(&json).unwrap();
    assert_eq!(clip, back);
}

#[test]
fn t301_clip_unknown_field_tolerated() {
    let mut v = serde_json::to_value(sample_clip()).unwrap();
    v["future_field"] = serde_json::json!("new");
    v["rev"]["future_nested"] = serde_json::json!(1);
    let back: model::Clip = serde_json::from_value(v).unwrap();
    assert_eq!(back.preview, "hello");
}

#[test]
fn t301_perm_roundtrip_and_system_kind() {
    let p = model::PermanentClip {
        id: Uuid::now_v7(),
        group: "g".to_string(),
        name: "n".to_string(),
        hotkey: Some("ctrl+alt+1".to_string()),
        kind: model::PermanentKind::System,
        body: "body".to_string(),
        js: None,
        rev: model::Rev {
            wall_ms: 1,
            dev: "d".to_string(),
            ctr: 0,
        },
    };
    let json = serde_json::to_string(&p).unwrap();
    let back: model::PermanentClip = serde_json::from_str(&json).unwrap();
    assert_eq!(p, back);
}

// T-311: unknown nested config keys survive roundtrip.
#[test]
fn t311_forward_config_roundtrip() {
    let toml_src = r#"
config_version = 2
[monitor]
formats = ["text/plain"]
future_key = "keep-me"
[history]
max_items = 10
[popup]
hotkey = "ctrl+alt+v"
[privacy]
strict = false
[tray]
enabled = true
"#;
    let cfg: config::Config = toml::from_str(toml_src).unwrap();
    let out = toml::to_string(&cfg).unwrap();
    let back: config::Config = toml::from_str(&out).unwrap();
    assert!(out.contains("future_key"), "section extra lost: {out}");
    assert!(out.contains("keep-me"));
    assert!(back.extra.contains_key("tray"), "root extra lost: {out}");
    let _ = back;
}

// T-306: config parse/defaults/env-override/portable resolution.
#[test]
fn t306_config_defaults() {
    let cfg = config::Config::default();
    assert_eq!(cfg.config_version, 1);
    assert_eq!(cfg.popup.hotkey, "ctrl+alt+v");
    assert_eq!(cfg.history.max_items, 750);
    assert_eq!(cfg.history.dedupe_window, 100);
    assert_eq!(cfg.monitor.formats.len(), 6);
    assert_eq!(cfg.monitor.poll_ms, 500);
}

#[test]
fn t306_config_parse_and_env_override() {
    let dir = std::env::temp_dir().join("cm-core-t306");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("clipmonger.toml");
    let cfg = config::Config::default();
    cfg.save(&path).unwrap();
    let loaded = config::Config::load(&path).unwrap();
    assert_eq!(loaded.popup.hotkey, "ctrl+alt+v");
    // SAFETY: single-threaded test env override; restore afterwards.
    std::env::set_var("CLIPMONGER_POPUP_HOTKEY", "ctrl+shift+x");
    std::env::set_var("CLIPMONGER_HISTORY_MAX_ITEMS", "42");
    let reloaded = config::Config::load(&path).unwrap();
    assert_eq!(reloaded.popup.hotkey, "ctrl+shift+x");
    assert_eq!(reloaded.history.max_items, 42);
    std::env::remove_var("CLIPMONGER_POPUP_HOTKEY");
    std::env::remove_var("CLIPMONGER_HISTORY_MAX_ITEMS");
    let _ = reloaded;
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn t306_portable_resolution() {
    let exe = std::env::temp_dir().join("cm-core-exe");
    let paths = config::Config::resolve_paths(true, &exe);
    assert!(paths.config_dir.starts_with(&exe));
    assert!(paths.data_dir.starts_with(&exe));
    assert!(paths.cache_dir.starts_with(&exe));
}

// T-305 hash part: MIME-order independence, CRLF normalization,
// x-os exclusion, ("ab","c") vs ("a","bc") distinct.
#[test]
fn t305_hash_mime_order_independent() {
    let a = vec![
        ("text/plain".to_string(), b"hi".to_vec()),
        ("text/html".to_string(), b"<b>hi</b>".to_vec()),
    ];
    let mut b = a.clone();
    b.reverse();
    assert_eq!(hash::content_hash(&a), hash::content_hash(&b));
}

#[test]
fn t305_hash_crlf_normalized() {
    let a = vec![("text/plain".to_string(), b"a\r\nb".to_vec())];
    let b = vec![("text/plain".to_string(), b"a\nb".to_vec())];
    assert_eq!(hash::content_hash(&a), hash::content_hash(&b));
}

#[test]
fn t305_hash_excludes_natives() {
    let a = vec![("text/plain".to_string(), b"hi".to_vec())];
    let b = vec![
        ("text/plain".to_string(), b"hi".to_vec()),
        ("x-os/win:HTML Format".to_string(), b"blob".to_vec()),
    ];
    assert_eq!(hash::content_hash(&a), hash::content_hash(&b));
}

#[test]
fn t305_hash_length_delimited() {
    // ("ab","c") vs ("a","bc") must differ under one MIME each.
    let a = vec![("text/plain".to_string(), b"ab".to_vec())];
    let b = vec![("text/plain".to_string(), b"abc".to_vec())];
    assert_ne!(hash::content_hash(&a), hash::content_hash(&b));
    // Same concatenated bytes under different MIME split must differ.
    let c = vec![
        ("text/html".to_string(), b"ab".to_vec()),
        ("text/plain".to_string(), b"c".to_vec()),
    ];
    let d = vec![
        ("text/html".to_string(), b"a".to_vec()),
        ("text/plain".to_string(), b"bc".to_vec()),
    ];
    assert_ne!(hash::content_hash(&c), hash::content_hash(&d));
}

// T-313 text part: search_text stable; strict omits source_app.
#[test]
fn t313_search_text_stable_and_strict() {
    let uris = vec!["file:///home/u/docs/report.pdf".to_string()];
    let a = hash::make_search_text("hello", &uris, Some("editor"), false);
    let b = hash::make_search_text("hello", &uris, Some("editor"), false);
    assert_eq!(a, b);
    assert!(a.contains("hello"));
    assert!(a.contains("report.pdf"));
    assert!(a.contains("editor"));
    let s = hash::make_search_text("hello", &uris, Some("editor"), true);
    assert!(s.contains("hello"));
    assert!(!s.contains("editor"), "strict must omit source_app: {s}");
}

// Policy: dedupe hit, retention halving, oversize stub.
#[test]
fn t_policy_dedupe_and_retention() {
    let id1 = Uuid::now_v7();
    let cands = vec![
        policy::DedupeCandidate {
            id: id1,
            content_hash: "h1".to_string(),
            removed: false,
            last_used_ms: 10,
        },
        policy::DedupeCandidate {
            id: Uuid::now_v7(),
            content_hash: "h1".to_string(),
            removed: true,
            last_used_ms: 20,
        },
    ];
    assert_eq!(policy::find_dedupe(&cands, "h1", 100), Some(id1));
    assert_eq!(policy::find_dedupe(&cands, "nope", 100), None);
    let (items, bytes, age) = policy::effective_limits(750, 512 * 1024 * 1024, 0, true);
    assert_eq!(items, 375);
    assert_eq!(bytes, 256 * 1024 * 1024);
    assert_eq!(age, 0);
    assert!(policy::needs_stub(17 * 1024 * 1024, 16 * 1024 * 1024));
    assert!(!policy::needs_stub(1024 * 1024, 16 * 1024 * 1024));
}
