//! UIPILOT acceptance tests (ruler only) for `clipmonger_app_ui`.
//!
//! Frozen API under test: see
//! `.agents/tmp/ui-pilot-v1/pilot/harness-spec.md`.
//! Conventions: `t901_*` strings, `t902_*` search, `t903_*` keymap +
//! placement, `t904_*` popup + lifecycle + `ipc_bridge`, `t910_*` strings
//! conformance (no inline literals in view code).
//!
//! Assumption notes (also in pilot/harness/README.md):
//! - `Rect { x, y, w, h }` (origin + size, i32).
//! - `KeyEvent::new(key, ctrl, alt, shift)` accepts the key via `Into`
//!   (works for both `String` and `&str` params); struct literal uses fields
//!   `{ key, ctrl, alt, shift, super_key }`.
//! - `Transport::call(&mut self, method: &str, params: &Value)` returns
//!   `Result<Value, clipmonger_ipc::proto::IpcError>`.
//! - `Bridge::spawn` returns std-style `(Sender<UiCmd>, Receiver<UiEvent>)`
//!   with `send` / `recv_timeout`.
//! - `UiEvent::Error(String)` carries the guard message; `banner()` output
//!   compares equal to `strings::text(..)` via `==` either direction.
//! - Keymap defaults follow docs/seg-09-ui-ux.md S8.

use clipmonger_app_ui::{ipc_bridge, keymap, lifecycle, placement, popup, search, strings};
use clipmonger_core::model::Clip;
use serde_json::json;
use std::time::Duration;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn mkclip(id: &str, search_text: &str, preview: &str) -> Clip {
    serde_json::from_value(json!({
        "id": id,
        "created_ms": 1_700_000_000_000_i64,
        "source_os": "linux",
        "preview": preview,
        "search_text": search_text,
        "rev": { "wall_ms": 1, "dev": "test-dev" },
    }))
    .expect("test clip JSON must deserialize")
}

fn key(k: &str, ctrl: bool, alt: bool, shift: bool) -> keymap::KeyEvent {
    keymap::KeyEvent::new(k.into(), ctrl, alt, shift)
}

// ---------------------------------------------------------------------------
// t901_*: strings module
// ---------------------------------------------------------------------------

#[test]
fn t901_ids_all_nonempty_english() {
    assert!(
        !strings::IDS.is_empty(),
        "IDS must list every view string id"
    );
    for id in strings::IDS {
        assert!(!id.is_empty(), "string id must not be empty");
        let s = strings::text(id);
        assert!(!s.is_empty(), "id {id:?} must have English text");
        assert_ne!(s, "", "id {id:?} must never be blank");
    }
}

#[test]
fn t901_unknown_id_echoes_never_panics_or_blank() {
    for unknown in ["no.such.key", "", "popup.", "zzz"] {
        let got = strings::text(unknown);
        assert_eq!(got, unknown, "unknown id must echo itself, got {got:?}");
    }
}

#[test]
fn t901_view_keys_exist() {
    // Keys the popup/lifecycle views need (offline + stale-target flows).
    for id in ["popup.offline", "popup.stale_target"] {
        assert!(
            strings::IDS.contains(&id),
            "IDS must contain view key {id:?}"
        );
        assert_ne!(strings::text(id), "");
    }
}

// ---------------------------------------------------------------------------
// t902_*: search filters (T-901 vectors)
// ---------------------------------------------------------------------------

fn search_clips() -> Vec<Clip> {
    vec![
        mkclip(
            "11111111-1111-7111-8111-111111111111",
            "hello world",
            "hello",
        ),
        mkclip(
            "22222222-2222-7222-8222-222222222222",
            "HELLO there",
            "greet",
        ),
        mkclip("33333333-3333-7333-8333-333333333333", "unrelated", "other"),
        mkclip(
            "44444444-4444-7444-8444-444444444444",
            "zzz",
            "preview-only needle here",
        ),
    ]
}

#[test]
fn t902_clips_empty_query_returns_all_in_order() {
    let clips = search_clips();
    assert_eq!(search::filter_clips(&clips, ""), vec![0, 1, 2, 3]);
}

#[test]
fn t902_clips_case_insensitive_substring() {
    let clips = search_clips();
    assert_eq!(search::filter_clips(&clips, "hello"), vec![0, 1]);
    assert_eq!(search::filter_clips(&clips, "HELLO"), vec![0, 1]);
    assert_eq!(search::filter_clips(&clips, "HeLlO"), vec![0, 1]);
}

#[test]
fn t902_clips_preview_field_searched() {
    let clips = search_clips();
    assert_eq!(search::filter_clips(&clips, "needle"), vec![3]);
}

#[test]
fn t902_clips_no_match_is_empty() {
    let clips = search_clips();
    assert_eq!(search::filter_clips(&clips, "qqq-nope"), [] as [usize; 0]);
    assert_eq!(search::filter_clips(&[], "hello"), [] as [usize; 0]);
}

#[test]
fn t902_settings_filter_same_rule() {
    let keys = ["clipboard.max_items", "popup.hotkey", "sync.enabled"];
    assert_eq!(search::filter_settings(&keys, ""), vec![0, 1, 2]);
    assert_eq!(search::filter_settings(&keys, "POPUP"), vec![1]);
    assert_eq!(search::filter_settings(&keys, "."), vec![0, 1, 2]);
    assert_eq!(search::filter_settings(&keys, "nope"), [] as [usize; 0]);
    assert_eq!(search::filter_settings(&[], "x"), [] as [usize; 0]);
}

// ---------------------------------------------------------------------------
// t903_*: keymap defaults/conflicts (T-902) + placement math (T-903)
// ---------------------------------------------------------------------------

#[test]
fn t903_defaults_match_seg09_s8() {
    use keymap::Action::*;
    let km = keymap::Keymap::defaults();
    let cases: &[(&str, bool, bool, bool, keymap::Action)] = &[
        ("ArrowUp", false, false, false, NavigateUp),
        ("ArrowDown", false, false, false, NavigateDown),
        ("PageUp", false, false, false, PageUp),
        ("PageDown", false, false, false, PageDown),
        ("Home", false, false, false, Home),
        ("End", false, false, false, End),
        ("Enter", false, false, false, Paste),
        ("Escape", false, false, false, Dismiss),
        ("Enter", true, false, false, PasteInto),
        ("Delete", false, false, false, Remove),
        ("e", true, false, false, Edit),
        ("p", true, false, false, Pin),
        ("F5", false, false, false, Refresh),
    ];
    for (k, c, a, s, want) in cases {
        let got = km.action_for(&key(k, *c, *a, *s));
        assert_eq!(got.as_ref(), Some(want), "chord {k:?} ctrl={c} alt={a}");
    }
    for n in 1u8..=9 {
        let digit = n.to_string();
        let got = km.action_for(&key(&digit, false, true, false));
        assert_eq!(got, Some(keymap::Action::GroupTab(n)), "Alt+{n}");
    }
}

#[test]
fn t903_typing_key_yields_typechar() {
    let km = keymap::Keymap::defaults();
    assert_eq!(
        km.action_for(&key("a", false, false, false)),
        Some(keymap::Action::TypeChar('a'))
    );
    // Modifier chords are never plain typing.
    assert_ne!(
        km.action_for(&key("a", true, false, false)),
        Some(keymap::Action::TypeChar('a'))
    );
}

#[test]
fn t903_bind_conflict_errors() {
    let mut km = keymap::Keymap::defaults();
    // Rebinding to an occupied chord must fail.
    let err = km.bind(keymap::Action::Refresh, key("F5", false, false, false));
    assert!(err.is_err(), "duplicate chord bind must Err, got {err:?}");
    // Free chord binds fine and resolves.
    km.bind(keymap::Action::Refresh, key("F6", false, false, false))
        .expect("free chord must bind");
    assert_eq!(
        km.action_for(&key("F6", false, false, false)),
        Some(keymap::Action::Refresh)
    );
}

fn rect(x: i32, y: i32, w: i32, h: i32) -> placement::Rect {
    placement::Rect { x, y, w, h }
}

#[test]
fn t903_place_cursor_clamps_fully_inside() {
    use placement::Placement;
    let mons = [rect(0, 0, 800, 600)];
    // Cursor near bottom-right: popup must still fit entirely.
    let (x, y) = placement::place(Placement::Cursor, (790, 590), &mons, (200, 100));
    assert_eq!((x, y), (600, 500));
    // Cursor top-left stays put.
    assert_eq!(
        placement::place(Placement::Cursor, (10, 10), &mons, (200, 100)),
        (10, 10)
    );
}

#[test]
fn t903_place_center_mode() {
    use placement::Placement;
    let mons = [rect(0, 0, 800, 600)];
    assert_eq!(
        placement::place(Placement::Center, (0, 0), &mons, (200, 100)),
        (300, 250)
    );
}

#[test]
fn t903_place_multi_monitor_nearest() {
    use placement::Placement;
    let mons = [rect(0, 0, 800, 600), rect(800, 0, 800, 600)];
    let (x, y) = placement::place(Placement::Cursor, (900, 590), &mons, (200, 100));
    assert!(
        (800..=1400).contains(&x) && (0..=500).contains(&y),
        "must clamp inside nearest (right) monitor, got ({x},{y})"
    );
    // Oversized popup still originates inside the monitor.
    let (x2, y2) = placement::place(Placement::Center, (0, 0), &mons, (900, 700));
    assert_eq!((x2, y2), (0, 0));
}

// ---------------------------------------------------------------------------
// t904_*: popup VM + lifecycle + ipc_bridge (T-904 / T-908)
// ---------------------------------------------------------------------------

fn popup_clips() -> Vec<Clip> {
    vec![
        mkclip("11111111-1111-7111-8111-111111111111", "alpha one", "a1"),
        mkclip("22222222-2222-7222-8222-222222222222", "alpha two", "a2"),
        mkclip("33333333-3333-7333-8333-333333333333", "beta", "b"),
    ]
}

#[test]
fn t904_popup_empty_never_underflows() {
    let mut vm = popup::PopupVm::new();
    vm.set_clips(vec![]);
    vm.set_current(None);
    assert_eq!(vm.selected, 0);
    assert_eq!(vm.visible(), [] as [usize; 0]);
    // Navigation on empty list: no panic, selection stays 0.
    vm.on_key(key("ArrowDown", false, false, false));
    vm.on_key(key("ArrowUp", false, false, false));
    assert_eq!(vm.selected, 0);
}

#[test]
fn t904_popup_visible_is_search_filtered() {
    let mut vm = popup::PopupVm::new();
    vm.set_clips(popup_clips());
    vm.query = "alpha".to_string();
    assert_eq!(vm.visible(), vec![0, 1]);
    vm.query.clear();
    assert_eq!(vm.visible(), vec![0, 1, 2]);
}

#[test]
fn t904_popup_typing_filters_via_on_key() {
    let mut vm = popup::PopupVm::new();
    vm.set_clips(popup_clips());
    for ch in "alp".chars() {
        let ev = key(&ch.to_string(), false, false, false);
        vm.on_key(ev);
    }
    assert_eq!(vm.query, "alp");
    assert_eq!(vm.visible(), vec![0, 1]);
}

#[test]
fn t904_popup_enter_pastes_and_escape_dismisses() {
    use popup::VmEffect;
    let clips = popup_clips();
    let mut vm = popup::PopupVm::new();
    vm.set_clips(clips.clone());
    vm.set_current(Some(clips[1].clone()));
    let eff = vm.on_key(key("Enter", false, false, false));
    assert!(
        matches!(eff, VmEffect::Paste(id) if id == clips[vm.selected].id),
        "Enter must paste selected clip, got {eff:?}"
    );
    let eff = vm.on_key(key("Escape", false, false, false));
    assert!(
        matches!(eff, VmEffect::Dismiss),
        "Esc must dismiss, got {eff:?}"
    );
}

#[test]
fn t904_popup_offline_banner() {
    let mut vm = popup::PopupVm::new();
    vm.set_clips(popup_clips());
    vm.set_offline(true);
    assert_eq!(vm.banner(), strings::text("popup.offline"));
    vm.set_offline(false);
    assert_ne!(vm.banner(), strings::text("popup.offline"));
}

#[test]
fn t904_popup_stale_target_blocks_paste() {
    use popup::VmEffect;
    let clips = popup_clips();
    let mut vm = popup::PopupVm::new();
    vm.set_clips(clips);
    vm.set_target_valid(false);
    let eff = vm.on_key(key("Enter", false, false, false));
    assert!(
        matches!(eff, VmEffect::None),
        "stale target: Enter must yield None (never a mis-paste), got {eff:?}"
    );
    assert_eq!(vm.banner(), strings::text("popup.stale_target"));
}

#[test]
fn t904_lifecycle_happy_path_and_backoff() {
    use lifecycle::{LifeEvent, LifeState};
    let mut lc = lifecycle::Lifecycle::new();
    assert_eq!(lc.state(), LifeState::Detached);
    lc.on(LifeEvent::Start);
    assert_ne!(lc.state(), LifeState::Detached);
    lc.on(LifeEvent::DaemonEnsured);
    lc.on(LifeEvent::Connected);
    assert_eq!(lc.state(), LifeState::Attached);
    // Pure-transition smoke: every event accepted from attached, no panic.
    for evt in [
        LifeEvent::Disconnected,
        LifeEvent::RetryDue,
        LifeEvent::Connected,
        LifeEvent::Stop,
        LifeEvent::Quit,
    ] {
        lc.on(evt);
    }
    // U-F6 backoff: 250ms * 2^n, capped at 5s.
    assert_eq!(lifecycle::next_backoff(0), Duration::from_millis(250));
    assert_eq!(lifecycle::next_backoff(1), Duration::from_millis(500));
    assert_eq!(lifecycle::next_backoff(2), Duration::from_millis(1000));
    assert_eq!(lifecycle::next_backoff(10), Duration::from_millis(5000));
    assert_eq!(lifecycle::next_backoff(100), Duration::from_millis(5000));
}

// --- ipc_bridge fakes (no sockets) ---

struct Fake {
    respond:
        fn(&str, &serde_json::Value) -> Result<serde_json::Value, clipmonger_ipc::proto::IpcError>,
}

impl ipc_bridge::Transport for Fake {
    fn call(
        &mut self,
        method: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, clipmonger_ipc::proto::IpcError> {
        (self.respond)(method, params)
    }
}

#[allow(clippy::unnecessary_wraps)] // trait-shaped test double: must return Result
fn ok_true(
    _m: &str,
    _p: &serde_json::Value,
) -> Result<serde_json::Value, clipmonger_ipc::proto::IpcError> {
    Ok(json!({ "ok": true }))
}

fn unavailable(
    _m: &str,
    _p: &serde_json::Value,
) -> Result<serde_json::Value, clipmonger_ipc::proto::IpcError> {
    Err(clipmonger_ipc::proto::IpcError::new(
        "unavailable",
        "paste unavailable".to_string(),
    ))
}

#[test]
fn t904_bridge_r1_stub_ok_is_unconfirmed_error() {
    use ipc_bridge::{UiCmd, UiEvent};
    // delete with stub {"ok":true} (missing {deleted:true}) -> Error("unconfirmed").
    let (tx, rx) = ipc_bridge::Bridge::spawn(Box::new(Fake { respond: ok_true }));
    let id = mkclip("11111111-1111-7111-8111-111111111111", "x", "x").id;
    tx.send(UiCmd::DeleteClip(id)).expect("send delete");
    let ev = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("delete event");
    assert!(
        matches!(&ev, UiEvent::Error(m) if m.contains("unconfirmed")),
        "stub-ok delete must surface unconfirmed error, got {ev:?}"
    );
    // pin with stub {"ok":true} (missing {pinned:bool}) -> Error("unconfirmed").
    let (tx, rx) = ipc_bridge::Bridge::spawn(Box::new(Fake { respond: ok_true }));
    tx.send(UiCmd::PinClip(id, true)).expect("send pin");
    let ev = rx.recv_timeout(Duration::from_secs(2)).expect("pin event");
    assert!(
        matches!(&ev, UiEvent::Error(m) if m.contains("unconfirmed")),
        "stub-ok pin must surface unconfirmed error, got {ev:?}"
    );
}

#[test]
fn t904_bridge_paste_unavailable_never_fake_ok() {
    use ipc_bridge::{UiCmd, UiEvent};
    let (tx, rx) = ipc_bridge::Bridge::spawn(Box::new(Fake {
        respond: unavailable,
    }));
    let id = mkclip("22222222-2222-7222-8222-222222222222", "x", "x").id;
    tx.send(UiCmd::PasteClip(id)).expect("send paste");
    let ev = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("paste event");
    assert!(
        matches!(ev, UiEvent::PasteUnavailable),
        "unavailable paste must yield PasteUnavailable, got {ev:?}"
    );
}

#[test]
#[allow(clippy::items_after_statements)] // local test-double type is clearest beside its use
fn t904_bridge_cmd_method_mapping_smoke() {
    use ipc_bridge::UiCmd;
    // Each command reaches the transport with the right IPC method name and
    // the bridge stays alive for the whole session (no sockets involved).
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    struct Rec {
        seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }
    impl ipc_bridge::Transport for Rec {
        fn call(
            &mut self,
            method: &str,
            _p: &serde_json::Value,
        ) -> Result<serde_json::Value, clipmonger_ipc::proto::IpcError> {
            self.seen.lock().expect("lock").push(method.to_string());
            Ok(json!({}))
        }
    }
    let (tx, _rx) = ipc_bridge::Bridge::spawn(Box::new(Rec { seen: seen.clone() }));
    let id = mkclip("33333333-3333-7333-8333-333333333333", "x", "x").id;
    for cmd in [
        UiCmd::Status,
        UiCmd::ListHistory,
        UiCmd::DeleteClip(id),
        UiCmd::PinClip(id, false),
        UiCmd::PasteClip(id),
        UiCmd::Stop,
    ] {
        tx.send(cmd).expect("send");
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        if seen.lock().expect("lock").len() >= 6 || std::time::Instant::now() > deadline {
            break;
        }
        std::thread::yield_now();
    }
    let got = seen.lock().expect("lock").clone();
    assert_eq!(
        got,
        vec![
            "status",
            "history.list",
            "history.delete",
            "history.pin",
            "paste.execute",
            "daemon.stop",
        ]
    );
}

// ---------------------------------------------------------------------------
// t910_*: strings conformance — zero inline literals in view code
// ---------------------------------------------------------------------------

#[test]
fn t910_ids_roundtrip_and_nonempty() {
    for id in strings::IDS {
        let s = strings::text(id);
        assert!(!s.is_empty(), "IDS entry {id:?} must have text");
        assert!(
            s.len() >= 2,
            "IDS entry {id:?} text looks like a stub: {s:?}"
        );
    }
}

#[test]
fn t910_view_sources_contain_no_bare_literals() {
    // Small denylist of known user-facing sentences: view code must use
    // strings::text(..), never these literals inline. (v1 rule, ADR-002.)
    const DENY: &[&str] = &[
        "Paste into",
        "You are offline",
        "Clipboard history",
        "No longer available",
    ];
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    let mut dirs = vec![src];
    while let Some(d) = dirs.pop() {
        for e in std::fs::read_dir(&d).expect("src dir must exist") {
            let p = e.expect("dir entry").path();
            if p.is_dir() {
                dirs.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                files.push(p);
            }
        }
    }
    assert!(!files.is_empty(), "crate must have src/*.rs view files");
    for f in &files {
        let body = std::fs::read_to_string(f).expect("read src file");
        for lit in DENY {
            assert!(
                !body.contains(lit),
                "{} contains bare user-facing literal {lit:?}; use strings::text(..)",
                f.display()
            );
        }
    }
}
