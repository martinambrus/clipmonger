//! Step 6 UI v1 acceptance ruler (seg-09 numbering) for `clipmonger_app_ui`.
//!
//! This file is the failing baseline: it names the public API the builder
//! must land. It does not touch the frozen pilot ruler in
//! `ui_acceptance.rs` (whose t9xx numbering means different things).
//!
//! Frozen builder API named by this file:
//!
//! `config_model`:
//!   `pub const ROWS: &[Row]` (28 rows, in the seg-03 S6 table order below)
//!   `pub struct Row { pub key: &'static str, pub tab: Tab,
//!                     pub kind: Kind, pub label: &'static str }`
//!   `pub enum Tab { History, Popup, Advanced, PermanentClips, Privacy,
//!                   Tray, Startup, General }`
//!   `pub enum Kind { Bool, Int, Text, Chord, Enum, StringList, ChordMap,
//!                    PasteChords }`
//!   `pub struct ConfigDoc` with
//!     `from_snapshot(snapshot: &Value) -> Self` (accepts the daemon
//!     `config.get` shape `{"config": {..}}`),
//!     `get(&self, key: &str) -> Option<&Value>` (saved value, dotted key),
//!     `staged(&self, key: &str) -> Option<&Value>`,
//!     `stage(&mut self, key: &str, value: Value) -> Result<(), String>`
//!       (validates first; staging the saved value clears the dirty flag),
//!     `is_dirty(&self, key: &str) -> bool`,
//!     `dirty_keys(&self) -> Vec<String>` (in ROWS order),
//!     `apply_plan(&self) -> Vec<(String, Value)>` (dirty rows, ROWS order;
//!       Apply is sequential `config.set`, see ledger U1-F1),
//!     `revert(&mut self)` (drop all staged edits, no IPC).
//!   `pub fn validate(key: &str, value: &Value) -> Result<(), String>`
//!     (Err carries a strings-module id, never a bare sentence)
//!   `pub fn filter_rows(query: &str) -> Vec<usize>`
//!     (case-insensitive substring over ROWS keys and label English text)
//!
//! chord:
//!   `pub fn parse_chord(input: &str) -> Result<KeyEvent, ChordError>`
//!   `pub fn format_chord(event: &KeyEvent) -> String` (canonical display,
//!     modifiers in ctrl, alt, shift, super order, joined with `+`)
//!   `pub fn reserved_reason(event: &KeyEvent, os: Os) -> Option<&'static str>`
//!     (Some = strings-module id explaining the OS reservation)
//!   `pub enum Os { Windows, X11, Wayland, Macos }`
//!   `pub enum ChordError` (Debug; every rejection path returns Err)
//!
//! `target_session`:
//!   `pub enum TargetSession { Idle, AwaitingTarget,
//!                             Ready { target: String }, Stale }`
//!   `pub enum PasteError { NotReady, Stale }`
//!   `TargetSession::new()`, `show(&mut self)`,
//!   `capture_target(&mut self, target: String)`, `focus_lost(&mut self)`,
//!   `dismiss(&mut self)`,
//!   `paste(&mut self) -> Result<String, PasteError>`
//!     (Ready pastes once and consumes the session back to Idle; Stale
//!     refuses with `PasteError::Stale` and never pastes)
//!
//! `tray_logic` (extended, pilot API kept):
//!   `pub enum MenuEntry { Item(&'static str), Separator }`
//!     (Item carries a strings-module id, not English text)
//!   `pub fn menu_entries(inputs: &TrayInputs) -> Vec<MenuEntry>`
//!   `TrayAction::About` (About is distinct from Quit; ledger/analysis S1d)
//!   platform `clipmonger_platform::types::TrayState::Paused` (U1-F6), and
//!   `TrayLogic::sync` maps the paused presentation state to it.
//!
//! `ipc_bridge` (pilot fns made public, param shapes fixed per U1-F10):
//!   `pub fn method_for(cmd: &UiCmd) -> &'static str`
//!   `pub fn params_for(cmd: &UiCmd) -> Value`
//!     (delete/pin/paste send the daemon shape `{"clip": ..}`, never `id`;
//!     daemon ground truth is `requested_clip` in crates/daemon/src/lib.rs)

use clipmonger_app_ui::{
    chord, config_model, ipc_bridge, keymap, placement, search, strings, target_session, tray_logic,
};
use clipmonger_core::model::Clip;
use clipmonger_platform::types::{TrayEvent, TrayState};
use serde_json::{json, Value};
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

fn row_keys() -> Vec<&'static str> {
    config_model::ROWS.iter().map(|row| row.key).collect()
}

fn row_index(want: &str) -> usize {
    row_keys()
        .iter()
        .position(|key| *key == want)
        .unwrap_or_else(|| panic!("ROWS must contain key {want:?}"))
}

// ---------------------------------------------------------------------------
// t901_*: search filter + settings filter (T-901)
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
fn t901_clips_empty_and_case_insensitive() {
    let clips = search_clips();
    assert_eq!(search::filter_clips(&clips, ""), vec![0, 1, 2, 3]);
    assert_eq!(search::filter_clips(&clips, "hello"), vec![0, 1]);
    assert_eq!(search::filter_clips(&clips, "HELLO"), vec![0, 1]);
    assert_eq!(search::filter_clips(&clips, "HeLlO"), vec![0, 1]);
    assert_eq!(search::filter_clips(&clips, "needle"), vec![3]);
    assert_eq!(search::filter_clips(&clips, "qqq-nope"), [] as [usize; 0]);
    assert_eq!(search::filter_clips(&[], "hello"), [] as [usize; 0]);
}

#[test]
fn t901_clips_unicode_query_pins_current_lowercase_rule() {
    // search.rs lowercases with char::to_lowercase; pin the observed result
    // for a dotted-capital-I query rather than silently changing it.
    let clips = search_clips();
    assert_eq!(search::filter_clips(&clips, "\u{130}"), [] as [usize; 0]);
}

#[test]
fn t901_settings_filter_over_rows_keys() {
    let keys = row_keys();
    assert_eq!(config_model::ROWS.len(), 28);
    // Empty query returns every row in ROWS order.
    assert_eq!(
        search::filter_settings(&keys, ""),
        (0..28).collect::<Vec<usize>>()
    );
    // The 7 monitor.* rows are the first 7 rows (seg-03 S6 order).
    assert_eq!(
        search::filter_settings(&keys, "monitor."),
        vec![0, 1, 2, 3, 4, 5, 6]
    );
    // The 10 popup.* rows (case-insensitive).
    let popup_idx: Vec<usize> = (0..28).filter(|i| keys[*i].starts_with("popup.")).collect();
    assert_eq!(popup_idx.len(), 10, "expected 10 popup.* rows in ROWS");
    assert_eq!(search::filter_settings(&keys, "POPUP."), popup_idx);
    // "keys" selects exactly the popup.keys.* row.
    assert_eq!(
        search::filter_settings(&keys, "keys"),
        vec![row_index("popup.keys.*")]
    );
    assert_eq!(search::filter_settings(&keys, "zzz"), [] as [usize; 0]);
}

#[test]
fn t901_config_filter_rows_searches_keys_and_labels() {
    assert_eq!(
        config_model::filter_rows(""),
        (0..28).collect::<Vec<usize>>()
    );
    assert_eq!(
        config_model::filter_rows("monitor."),
        vec![0, 1, 2, 3, 4, 5, 6]
    );
    assert_eq!(
        config_model::filter_rows("POPUP."),
        (0..28)
            .filter(|i| config_model::ROWS[*i].key.starts_with("popup."))
            .collect::<Vec<usize>>()
    );
    assert_eq!(
        config_model::filter_rows("keys"),
        vec![row_index("popup.keys.*")]
    );
    assert_eq!(config_model::filter_rows("zzz"), [] as [usize; 0]);
    // Label search: a word from row 0's English label text finds row 0.
    let label_text = strings::text(config_model::ROWS[0].label);
    let word = label_text
        .split(|c: char| !c.is_alphabetic())
        .find(|w| w.len() >= 3)
        .expect("row 0 label must have a searchable word");
    assert!(
        config_model::filter_rows(&word.to_lowercase()).contains(&0),
        "filter_rows must search label text; word {word:?} from {label_text:?}"
    );
}

// ---------------------------------------------------------------------------
// t902_*: keymap defaults, chord parse/conflict/reserved matrix (T-902)
// ---------------------------------------------------------------------------

#[test]
fn t902_defaults_match_seg09_s8() {
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
        assert_eq!(
            km.action_for(&key(k, *c, *a, *s)).as_ref(),
            Some(want),
            "chord {k:?} ctrl={c} alt={a}"
        );
    }
    for n in 1u8..=9 {
        let digit = n.to_string();
        assert_eq!(
            km.action_for(&key(&digit, false, true, false)),
            Some(keymap::Action::GroupTab(n)),
            "Alt+{n}"
        );
    }
}

#[test]
fn t902_parse_chord_vectors() {
    let ev = chord::parse_chord("ctrl+alt+h").expect("ctrl+alt+h parses");
    assert_eq!(ev, key("h", true, true, false));
    // Case-insensitive modifier and key names.
    assert_eq!(
        chord::parse_chord("Ctrl+Alt+H").expect("mixed case parses"),
        ev
    );
    assert_eq!(
        chord::parse_chord("enter").expect("enter parses"),
        key("Enter", false, false, false)
    );
    assert_eq!(
        chord::parse_chord("alt+1").expect("alt+1 parses"),
        key("1", false, true, false)
    );
    assert_eq!(
        chord::parse_chord("shift+f5").expect("shift+f5 parses"),
        key("F5", false, false, true)
    );
    // win/super alias sets the super flag.
    let ev = chord::parse_chord("win+l").expect("win+l parses");
    assert!(ev.super_key, "win must map to super_key: {ev:?}");
    assert_eq!(ev.key, "l");
}

#[test]
fn t902_parse_chord_rejections() {
    for bad in [
        "",
        "ctrl+",
        "ctrl+ctrl+v",
        "hyper+v",
        "ctrl+\u{1f4a3}",
        "ctrl",
    ] {
        assert!(
            chord::parse_chord(bad).is_err(),
            "chord {bad:?} must be rejected"
        );
    }
}

#[test]
fn t902_format_chord_roundtrips_canonical_defaults() {
    for canonical in [
        "ctrl+alt+h",
        "Enter",
        "Escape",
        "Delete",
        "F5",
        "ArrowUp",
        "alt+1",
        "shift+F5",
        "ctrl+Enter",
        "ctrl+e",
        "ctrl+p",
    ] {
        let ev =
            chord::parse_chord(canonical).unwrap_or_else(|_| panic!("{canonical:?} must parse"));
        assert_eq!(
            chord::format_chord(&ev),
            canonical,
            "format(parse({canonical:?})) must roundtrip"
        );
    }
    // Super displays under its canonical name, not the win alias.
    let ev = chord::parse_chord("win+l").expect("win+l parses");
    assert_eq!(chord::format_chord(&ev), "super+l");
}

#[test]
fn t902_conflict_pairs_error_already_bound() {
    let mut km = keymap::Keymap::defaults();
    // Paste onto the occupied Escape chord, and Refresh onto its own F5.
    for (action, chord_key) in [
        (keymap::Action::Paste, "Escape"),
        (keymap::Action::Refresh, "F5"),
        (keymap::Action::Dismiss, "Enter"),
    ] {
        let err = km
            .bind(action, key(chord_key, false, false, false))
            .expect_err("occupied chord must conflict");
        assert!(
            err.contains("already bound"),
            "conflict error must say 'already bound', got {err:?}"
        );
    }
}

#[test]
fn t902_rebind_capture_parse_then_bind() {
    // The capture-widget path: text typed in the editor parses, then binds.
    let mut km = keymap::Keymap::defaults();
    let captured = chord::parse_chord("ctrl+o").expect("capture parses");
    km.bind(keymap::Action::Paste, captured)
        .expect("free captured chord binds");
    assert_eq!(
        km.action_for(&key("o", true, false, false)),
        Some(keymap::Action::Paste)
    );
    // Rebinding through a parsed reserved chord is rejected by the
    // reserved check before it can ever reach the binding table.
    let reserved = chord::parse_chord("win+l").expect("win+l parses");
    assert!(chord::reserved_reason(&reserved, chord::Os::Windows).is_some());
}

#[test]
fn t902_reserved_chords_rejected_with_reason_id() {
    // (chord text, os, reserved?)
    let cases: &[(&str, chord::Os, bool)] = &[
        ("win+l", chord::Os::Windows, true),
        ("win+d", chord::Os::Windows, true),
        ("alt+tab", chord::Os::Windows, true),
        // Secure-attention sequence: unregistrable on Windows.
        ("ctrl+alt+delete", chord::Os::Windows, true),
        ("ctrl+alt+h", chord::Os::Windows, false),
        // Common terminal shortcut is not OS-reserved on X11.
        ("ctrl+alt+t", chord::Os::X11, false),
        ("ctrl+alt+h", chord::Os::X11, false),
        ("ctrl+alt+h", chord::Os::Wayland, false),
    ];
    for (text, os, want_reserved) in cases {
        let ev = chord::parse_chord(text)
            .unwrap_or_else(|_| panic!("{text:?} must parse before reserved check"));
        let reason = chord::reserved_reason(&ev, *os);
        assert_eq!(
            reason.is_some(),
            *want_reserved,
            "reserved check for {text:?} on {os:?}"
        );
        if let Some(id) = reason {
            // The reason is a real strings-module id with English text.
            assert!(
                strings::IDS.contains(&id),
                "reserved reason {id:?} must be in strings::IDS"
            );
            assert_ne!(strings::text(id), id);
            assert_ne!(strings::text(id), "");
        }
    }
    // Super alone is reserved on Wayland/GNOME (no key press to bind).
    let super_alone = keymap::KeyEvent {
        key: "Super".to_string(),
        ctrl: false,
        alt: false,
        shift: false,
        super_key: false,
    };
    assert!(chord::reserved_reason(&super_alone, chord::Os::Wayland).is_some());
}

// ---------------------------------------------------------------------------
// t903_*: placement math (T-903)
// ---------------------------------------------------------------------------

fn rect(x: i32, y: i32, w: i32, h: i32) -> placement::Rect {
    placement::Rect { x, y, w, h }
}

#[test]
fn t903_place_cursor_and_center_existing_vectors() {
    use placement::Placement;
    let mons = [rect(0, 0, 800, 600)];
    assert_eq!(
        placement::place(Placement::Cursor, (790, 590), &mons, (200, 100)),
        (600, 500)
    );
    assert_eq!(
        placement::place(Placement::Cursor, (10, 10), &mons, (200, 100)),
        (10, 10)
    );
    assert_eq!(
        placement::place(Placement::Center, (0, 0), &mons, (200, 100)),
        (300, 250)
    );
}

#[test]
fn t903_place_negative_origin_monitor() {
    use placement::Placement;
    let mons = [rect(-1280, 0, 1280, 1024), rect(0, 0, 1920, 1080)];
    let (x, y) = placement::place(Placement::Cursor, (-10, 500), &mons, (200, 100));
    assert!(
        (-1280..=-200).contains(&x) && (0..=924).contains(&y),
        "cursor on the negative-origin monitor must land inside it, got ({x},{y})"
    );
}

#[test]
fn t903_place_shared_edge_belongs_to_right_monitor() {
    use placement::Placement;
    let mons = [rect(0, 0, 800, 600), rect(800, 0, 800, 600)];
    let (x, y) = placement::place(Placement::Cursor, (800, 300), &mons, (200, 100));
    assert!(
        (800..=1400).contains(&x) && (0..=500).contains(&y),
        "x=800 is the right monitor's first column, got ({x},{y})"
    );
}

#[test]
fn t903_place_empty_monitors_and_oversized() {
    use placement::Placement;
    assert_eq!(
        placement::place(Placement::Cursor, (50, 50), &[], (200, 100)),
        (0, 0)
    );
    assert_eq!(
        placement::place(Placement::Center, (50, 50), &[], (200, 100)),
        (0, 0)
    );
    // Oversized in Cursor mode clamps to the monitor origin, as Center does.
    let mons = [rect(0, 0, 800, 600)];
    assert_eq!(
        placement::place(Placement::Cursor, (100, 100), &mons, (900, 700)),
        (0, 0)
    );
    assert_eq!(
        placement::place(Placement::Center, (0, 0), &mons, (900, 700)),
        (0, 0)
    );
}

// ---------------------------------------------------------------------------
// t905_*: popup monitor rect from runtime monitor size (T-905, seg-09 §1)
// ---------------------------------------------------------------------------

#[test]
fn t905_monitor_rect_falls_back_only_when_size_unknown() {
    assert_eq!(
        placement::monitor_rect_from_size(None),
        placement::Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080
        }
    );
    assert_eq!(
        placement::monitor_rect_from_size(Some((2560, 1440))),
        placement::Rect {
            x: 0,
            y: 0,
            w: 2560,
            h: 1440
        }
    );
    assert_eq!(
        placement::monitor_rect_from_size(Some((1920, 1080))),
        placement::Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080
        }
    );
}

// ---------------------------------------------------------------------------
// t909_*: settings-coverage matrix (T-909) + ConfigDoc behaviour
// ---------------------------------------------------------------------------

/// The exact 28 v1 keys (seg-03 S6, analysis S5 T-909), in table order,
/// each with its seg-09 S4 tab (ledger U1-F2) and control kind.
const EXPECTED_ROWS: &[(&str, config_model::Tab, config_model::Kind)] = {
    use config_model::{Kind as K, Tab as T};
    &[
        ("monitor.formats", T::History, K::StringList),
        ("monitor.max_bytes", T::History, K::Int),
        ("monitor.min_len", T::History, K::Int),
        ("monitor.ignores", T::History, K::StringList),
        ("monitor.poll_ms", T::Advanced, K::Int),
        ("monitor.capture_primary", T::History, K::Bool),
        ("monitor.primary_debounce_ms", T::Advanced, K::Int),
        ("history.max_items", T::History, K::Int),
        ("history.max_store_bytes", T::History, K::Int),
        ("history.max_age_days", T::History, K::Int),
        ("history.dedupe_window", T::History, K::Int),
        ("popup.hotkey", T::Popup, K::Chord),
        ("popup.placement", T::Popup, K::Enum),
        ("popup.preview_width", T::Popup, K::Int),
        ("popup.dismiss_on_focus_loss", T::Popup, K::Bool),
        ("popup.keys.*", T::Popup, K::ChordMap),
        ("popup.paste_chords", T::Popup, K::PasteChords),
        ("popup.unicode_limit", T::Advanced, K::Int),
        ("popup.delivery_dwell_ms", T::Advanced, K::Int),
        ("permanents.groups", T::PermanentClips, K::StringList),
        ("privacy.strict", T::Privacy, K::Bool),
        ("privacy.denylist", T::Privacy, K::StringList),
        ("tray.enabled", T::Tray, K::Bool),
        ("autostart.enabled", T::Startup, K::Bool),
        ("updates.check", T::General, K::Bool),
        ("logging.level", T::Advanced, K::Enum),
        ("popup.delivery", T::Popup, K::Enum),
        ("popup.restore_clipboard", T::Popup, K::Bool),
    ]
};

#[test]
fn t909_rows_are_exactly_the_28_v1_keys() {
    assert_eq!(
        config_model::ROWS.len(),
        28,
        "ROWS must hold exactly the 28 v1 rows"
    );
    let got: Vec<&str> = row_keys();
    let want: Vec<&str> = EXPECTED_ROWS.iter().map(|(key, _, _)| *key).collect();
    // Key set equality (order is pinned separately by t901/t909 tab test).
    let mut got_sorted = got.clone();
    let mut want_sorted = want.clone();
    got_sorted.sort_unstable();
    want_sorted.sort_unstable();
    assert_eq!(
        got_sorted, want_sorted,
        "ROWS key set must equal seg-03 S6 v1"
    );
    assert_eq!(got, want, "ROWS must be in seg-03 S6 table order");
}

#[test]
fn t909_each_row_has_tab_kind_and_label_id() {
    for (row, (key, tab, kind)) in config_model::ROWS.iter().zip(EXPECTED_ROWS.iter()) {
        assert_eq!(row.key, *key);
        assert_eq!(row.tab, *tab, "tab for {key}");
        assert_eq!(row.kind, *kind, "control kind for {key}");
        assert!(
            row.label.starts_with("config."),
            "label for {key} must be a config.* string id, got {:?}",
            row.label
        );
        assert!(
            strings::IDS.contains(&row.label),
            "label id {:?} for {key} must exist in strings::IDS",
            row.label
        );
    }
}

#[test]
fn t909_no_dead_tab_rows() {
    // D-014: v2/v3-only surfaces must not appear as rows.
    for excluded in [
        "ui.locale",
        "ui.fallback_locale",
        "plugins.enabled",
        "run.confirm_policy",
        "config_version",
    ] {
        assert!(
            !row_keys().contains(&excluded),
            "excluded key {excluded:?} must not have a row"
        );
    }
    for row in config_model::ROWS {
        assert!(
            !row.key.starts_with("plugins.") && !row.key.starts_with("sync."),
            "no Plugins/Sync rows in v1, found {:?}",
            row.key
        );
    }
}

fn config_snapshot() -> Value {
    let cfg = clipmonger_core::config::Config::default();
    let inner = serde_json::to_value(&cfg).expect("default config serialises");
    json!({ "config": inner })
}

#[test]
fn t909_config_doc_reads_config_get_snapshot() {
    let doc = config_model::ConfigDoc::from_snapshot(&config_snapshot());
    assert_eq!(doc.get("history.max_items"), Some(&json!(750)));
    assert_eq!(doc.get("popup.hotkey"), Some(&json!("ctrl+alt+h")));
    assert_eq!(doc.get("popup.dismiss_on_focus_loss"), Some(&json!(true)));
    // extra-only rows are absent until set (round-trip, not default).
    assert_eq!(doc.get("tray.enabled"), None);
    assert_eq!(doc.get("no.such.key"), None);
    assert_eq!(doc.dirty_keys().len(), 0);
}

#[test]
fn t909_config_doc_staged_edits_dirty_and_revert() {
    let mut doc = config_model::ConfigDoc::from_snapshot(&config_snapshot());
    doc.stage("history.max_items", json!(42))
        .expect("valid edit stages");
    assert!(doc.is_dirty("history.max_items"));
    assert_eq!(doc.dirty_keys(), vec!["history.max_items".to_string()]);
    assert_eq!(doc.staged("history.max_items"), Some(&json!(42)));
    // Saved value is untouched until Apply.
    assert_eq!(doc.get("history.max_items"), Some(&json!(750)));
    // Staging an extra-only key works (preserved via extra daemon-side).
    doc.stage("tray.enabled", json!(true))
        .expect("extra key stages");
    assert!(doc.is_dirty("tray.enabled"));
    // Staging the saved value again clears the dirty flag.
    doc.stage("history.max_items", json!(750))
        .expect("restaging saved value stages");
    assert!(!doc.is_dirty("history.max_items"));
    doc.revert();
    assert_eq!(doc.dirty_keys().len(), 0);
    assert_eq!(doc.staged("tray.enabled"), None);
}

#[test]
fn t909_apply_plan_is_dirty_rows_in_rows_order() {
    let mut doc = config_model::ConfigDoc::from_snapshot(&config_snapshot());
    // Stage in reverse ROWS order; the plan must still follow ROWS order
    // (Apply = sequential config.set in that pinned order, U1-F1).
    doc.stage("popup.placement", json!("center"))
        .expect("placement stages");
    doc.stage("history.max_items", json!(42))
        .expect("max_items stages");
    assert_eq!(
        doc.dirty_keys(),
        vec![
            "history.max_items".to_string(),
            "popup.placement".to_string()
        ]
    );
    assert_eq!(
        doc.apply_plan(),
        vec![
            ("history.max_items".to_string(), json!(42)),
            ("popup.placement".to_string(), json!("center")),
        ]
    );
}

#[test]
fn t909_validate_rejects_bad_values_with_string_id() {
    assert!(config_model::validate("history.max_items", &json!(750)).is_ok());
    assert!(config_model::validate("popup.preview_width", &json!(80)).is_ok());
    assert!(config_model::validate("popup.placement", &json!("cursor")).is_ok());
    assert!(config_model::validate("popup.placement", &json!("center")).is_ok());
    assert!(config_model::validate("popup.hotkey", &json!("ctrl+alt+h")).is_ok());
    assert!(config_model::validate("monitor.capture_primary", &json!(true)).is_ok());
    for (key, value) in [
        ("history.max_items", json!(0)),
        ("history.max_items", json!("750")),
        ("popup.preview_width", json!(0)),
        ("popup.placement", json!("caret")),
        ("popup.hotkey", json!("hyper+v")),
        ("monitor.capture_primary", json!(1)),
        ("no.such.key", json!(1)),
    ] {
        let err = config_model::validate(key, &value)
            .unwrap_or_else(|_| panic!("validate({key}, {value}) must fail"));
        // Error is a strings-module id, never a bare English sentence.
        assert!(
            strings::IDS.contains(&err.as_str()),
            "validate error for {key} must be a string id, got {err:?}"
        );
        assert_ne!(strings::text(&err), err);
    }
    // Staging an invalid value fails and leaves the doc clean.
    let mut doc = config_model::ConfigDoc::from_snapshot(&config_snapshot());
    assert!(doc.stage("history.max_items", json!(0)).is_err());
    assert!(!doc.is_dirty("history.max_items"));
}

// ---------------------------------------------------------------------------
// t910_*: strings-module conformance (T-910)
// ---------------------------------------------------------------------------

#[test]
fn t910_every_row_label_has_english_text() {
    for row in config_model::ROWS {
        let text = strings::text(row.label);
        assert!(!text.is_empty(), "label {:?} must have text", row.label);
        assert_ne!(
            text, row.label,
            "label {:?} must resolve to English, not echo the id",
            row.label
        );
    }
}

#[test]
fn t910_tray_and_popup_model_ids_exist() {
    // Tray menu model ids (menu_entries is checked structurally in the
    // tray group; here every referenced id must resolve).
    for entry in tray_logic::menu_entries(&tray_logic::TrayInputs::default()) {
        if let tray_logic::MenuEntry::Item(id) = entry {
            assert!(
                strings::IDS.contains(&id),
                "tray menu id {id:?} must be in strings::IDS"
            );
            assert_ne!(strings::text(id), id);
        }
    }
    // Popup model ids used by the VM/banner/status paths.
    for id in [
        "popup.offline",
        "popup.stale_target",
        "popup.count",
        "popup.paused",
        "popup.paste_blocked",
    ] {
        assert!(strings::IDS.contains(&id), "IDS must contain {id:?}");
        assert_ne!(strings::text(id), id);
    }
    // Placeholder convention: {count} substitutes and leaves no braces.
    let rendered = strings::text("popup.count").replace("{count}", "3");
    assert!(rendered.contains('3'), "count placeholder must substitute");
    assert!(
        !rendered.contains('{'),
        "no placeholder may leak: {rendered:?}"
    );
}

#[test]
fn t910_string_call_sites_reference_known_ids() {
    // Every strings::text("..") call site in app-ui sources names a known id.
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
    assert!(!files.is_empty(), "crate must have src/*.rs files");
    let mut found = 0usize;
    for f in &files {
        let body = std::fs::read_to_string(f).expect("read src file");
        for needle in ["strings::text(\"", "strings::text (\""] {
            let mut rest = body.as_str();
            while let Some(pos) = rest.find(needle) {
                let after = &rest[pos + needle.len()..];
                let Some(end) = after.find('"') else { break };
                let id = &after[..end];
                found += 1;
                assert!(
                    strings::IDS.contains(&id),
                    "{} references unknown string id {id:?}",
                    f.display()
                );
                rest = &after[end..];
            }
        }
    }
    assert!(found > 0, "scan must find strings::text call sites");
}

#[test]
fn t910_all_ids_resolve_nonempty_and_not_themselves() {
    assert_ne!(strings::IDS.len(), 0);
    for id in strings::IDS {
        let text = strings::text(id);
        assert!(!text.is_empty(), "id {id:?} must have English text");
        assert_ne!(text, *id, "id {id:?} must not echo itself");
    }
}

// ---------------------------------------------------------------------------
// tray group: menu model, About action, Pause 1h, Paused state
// ---------------------------------------------------------------------------

#[test]
fn tray_menu_entries_match_seg09_s7_with_separators() {
    use tray_logic::MenuEntry::{Item, Separator};
    // Show / - / Pause / Pause 1h / - / Open config / - / About / Quit
    assert_eq!(
        tray_logic::menu_entries(&tray_logic::TrayInputs::default()),
        vec![
            Item("tray.show"),
            Separator,
            Item("tray.pause"),
            Item("tray.pause_1h"),
            Separator,
            Item("tray.config"),
            Separator,
            Item("tray.about"),
            Item("tray.quit"),
        ]
    );
    // Label-swap pause is kept (U1-F6): paused shows the resume id.
    let paused = tray_logic::TrayInputs {
        paused: true,
        degraded: false,
        attached: true,
    };
    let entries = tray_logic::menu_entries(&paused);
    assert_eq!(entries[2], Item("tray.resume"));
    assert!(entries.contains(&Item("tray.pause_1h")));
}

#[test]
fn tray_about_action_is_distinct_from_quit() {
    let mut logic = tray_logic::TrayLogic::new();
    let mut tray = clipmonger_platform::fakes::FakeTray::new(vec![
        TrayEvent::MenuItem(strings::text("tray.about").to_string()),
        TrayEvent::MenuItem(strings::text("tray.pause_1h").to_string()),
        TrayEvent::MenuItem(strings::text("tray.quit").to_string()),
    ]);
    let actions = logic.on_events(&mut tray);
    assert_eq!(
        actions,
        vec![
            tray_logic::TrayAction::About,
            tray_logic::TrayAction::PauseFor1h,
            tray_logic::TrayAction::Quit,
        ],
        "About must not fall through to Quit; Pause 1h must exist"
    );
}

#[test]
fn tray_paused_maps_to_platform_paused_state() {
    // TrayState::Paused must exist in the platform types (U1-F6) and the
    // paused presentation must reach the platform, not collapse to Idle.
    let mut logic = tray_logic::TrayLogic::new();
    let mut tray = clipmonger_platform::fakes::FakeTray::default();
    logic.sync(
        &mut tray,
        &tray_logic::TrayInputs {
            paused: true,
            degraded: false,
            attached: true,
        },
    );
    assert_eq!(tray.state, Some(TrayState::Paused));
    assert_eq!(logic.state(), tray_logic::TrayUiState::Paused);

    let mut logic = tray_logic::TrayLogic::new();
    let mut tray = clipmonger_platform::fakes::FakeTray::default();
    logic.sync(&mut tray, &tray_logic::TrayInputs::default());
    assert_eq!(tray.state, Some(TrayState::Idle));
}

// ---------------------------------------------------------------------------
// target_session group (T-908 logic core)
// ---------------------------------------------------------------------------

#[test]
fn target_session_show_capture_paste_consumes() {
    use target_session::{PasteError, TargetSession};
    let mut session = TargetSession::new();
    assert_eq!(session, TargetSession::Idle);
    // Paste before any target is refused, never pastes.
    assert_eq!(session.paste(), Err(PasteError::NotReady));
    session.show();
    assert_eq!(session, TargetSession::AwaitingTarget);
    assert_eq!(session.paste(), Err(PasteError::NotReady));
    session.capture_target("editor-window".to_string());
    assert_eq!(
        session,
        TargetSession::Ready {
            target: "editor-window".to_string()
        }
    );
    assert_eq!(session.paste(), Ok("editor-window".to_string()));
    // A successful paste consumes the session.
    assert_eq!(session, TargetSession::Idle);
    assert_eq!(session.paste(), Err(PasteError::NotReady));
}

#[test]
fn target_session_focus_lost_stales_and_never_pastes() {
    use target_session::{PasteError, TargetSession};
    let mut session = TargetSession::new();
    session.show();
    session.capture_target("editor-window".to_string());
    session.focus_lost();
    assert_eq!(session, TargetSession::Stale);
    // Stale paste errors and stays stale (no mis-paste, no consumption).
    assert_eq!(session.paste(), Err(PasteError::Stale));
    assert_eq!(session, TargetSession::Stale);
    // A fresh show recovers: a new target can be captured and pasted.
    session.show();
    assert_eq!(session, TargetSession::AwaitingTarget);
    session.capture_target("other-window".to_string());
    assert_eq!(session.paste(), Ok("other-window".to_string()));
}

#[test]
fn target_session_dismiss_blocks_late_paste() {
    use target_session::{PasteError, TargetSession};
    let mut session = TargetSession::new();
    session.show();
    session.capture_target("editor-window".to_string());
    session.dismiss();
    assert_eq!(session, TargetSession::Idle);
    assert_eq!(session.paste(), Err(PasteError::NotReady));
}

// ---------------------------------------------------------------------------
// ipc_bridge group: daemon {"clip": ..} param shapes (U1-F10)
// ---------------------------------------------------------------------------

fn clip_id(n: u8) -> uuid::Uuid {
    uuid::Uuid::from_u128(u128::from(n))
}

#[test]
fn ipc_bridge_builders_send_clip_not_id() {
    let id = clip_id(1);
    let (method, params) = (
        ipc_bridge::method_for(&ipc_bridge::UiCmd::DeleteClip(id)),
        ipc_bridge::params_for(&ipc_bridge::UiCmd::DeleteClip(id)),
    );
    assert_eq!(method, "history.delete");
    assert_eq!(params, json!({ "clip": id.to_string() }));
    assert!(params.get("id").is_none(), "daemon reads clip, not id");

    let (method, params) = (
        ipc_bridge::method_for(&ipc_bridge::UiCmd::PinClip(id, true)),
        ipc_bridge::params_for(&ipc_bridge::UiCmd::PinClip(id, true)),
    );
    assert_eq!(method, "history.pin");
    assert_eq!(params, json!({ "clip": id.to_string(), "pinned": true }));

    let (method, params) = (
        ipc_bridge::method_for(&ipc_bridge::UiCmd::PasteClip(id)),
        ipc_bridge::params_for(&ipc_bridge::UiCmd::PasteClip(id)),
    );
    assert_eq!(method, "paste.execute");
    assert_eq!(params, json!({ "clip": id.to_string() }));

    let targeted = ipc_bridge::UiCmd::PasteClipTo(id, Some(0x0003_0838), 1_000);
    let (method, params) = (
        ipc_bridge::method_for(&targeted),
        ipc_bridge::params_for(&targeted),
    );
    assert_eq!(method, "paste.execute");
    assert_eq!(
        params,
        json!({ "clip": id.to_string(), "target_hwnd": 0x0003_0838_u64, "picked_ms": 1_000 })
    );
    let untargeted = ipc_bridge::UiCmd::PasteClipTo(id, None, 1_000);
    assert_eq!(
        ipc_bridge::params_for(&untargeted),
        json!({ "clip": id.to_string(), "picked_ms": 1_000 }),
        "no target: the daemon falls back to the foreground window"
    );

    let (method, params) = (
        ipc_bridge::method_for(&ipc_bridge::UiCmd::HotkeyFired),
        ipc_bridge::params_for(&ipc_bridge::UiCmd::HotkeyFired),
    );
    assert_eq!(method, "hotkey.fired");
    assert_eq!(params, json!({ "id": 1 }));
}

#[test]
fn ipc_bridge_wire_params_recorded_by_fake_transport() {
    use ipc_bridge::{UiCmd, UiEvent};
    // Fake transport records (method, params) and replies daemon-shaped.
    struct Rec {
        seen: std::sync::Arc<std::sync::Mutex<Vec<(String, Value)>>>,
    }
    impl ipc_bridge::Transport for Rec {
        fn call(
            &mut self,
            method: &str,
            params: &Value,
        ) -> Result<Value, clipmonger_ipc::proto::IpcError> {
            self.seen
                .lock()
                .expect("lock")
                .push((method.to_string(), params.clone()));
            Ok(match method {
                "history.delete" => json!({ "deleted": true }),
                "history.pin" => json!({ "pinned": true }),
                _ => json!({ "pasted": true }),
            })
        }
    }
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, Value)>::new()));
    let (tx, rx) = ipc_bridge::Bridge::spawn(Box::new(Rec { seen: seen.clone() }));
    let id = clip_id(2);
    tx.send(UiCmd::DeleteClip(id)).expect("send delete");
    tx.send(UiCmd::PinClip(id, true)).expect("send pin");
    tx.send(UiCmd::PasteClip(id)).expect("send paste");
    let events: Vec<UiEvent> = (0..3)
        .map(|_| {
            rx.recv_timeout(Duration::from_secs(2))
                .expect("bridge event")
        })
        .collect();
    assert!(matches!(events[0], UiEvent::Deleted(got) if got == id));
    assert!(matches!(events[1], UiEvent::Pinned(got, true) if got == id));
    assert!(matches!(events[2], UiEvent::PasteDone(got) if got == id));
    let got = seen.lock().expect("lock").clone();
    assert_eq!(
        got,
        vec![
            (
                "history.delete".to_string(),
                json!({ "clip": id.to_string() })
            ),
            (
                "history.pin".to_string(),
                json!({ "clip": id.to_string(), "pinned": true })
            ),
            (
                "paste.execute".to_string(),
                json!({ "clip": id.to_string() })
            ),
        ],
        "wire params must use the daemon clip shape for all three commands"
    );
}

#[test]
fn ipc_bridge_targeted_paste_failure_is_reported_not_dropped() {
    use ipc_bridge::{UiCmd, UiEvent};
    // Fake daemon: mints on hotkey.fired, refuses the targeted paste.
    struct Refuse;
    impl ipc_bridge::Transport for Refuse {
        fn call(
            &mut self,
            method: &str,
            _params: &Value,
        ) -> Result<Value, clipmonger_ipc::proto::IpcError> {
            match method {
                "hotkey.fired" => Ok(json!({ "ok": true })),
                _ => Err(clipmonger_ipc::proto::IpcError::new(
                    "unavailable",
                    "paste.execute: unavailable: admin helper is not running".to_string(),
                )),
            }
        }
    }
    let (tx, rx) = ipc_bridge::Bridge::spawn(Box::new(Refuse));
    let id = clip_id(3);
    tx.send(UiCmd::HotkeyFired).expect("send hotkey");
    tx.send(UiCmd::PasteClipTo(id, Some(42), 0))
        .expect("send paste");
    let first = rx.recv_timeout(Duration::from_secs(2)).expect("ack");
    let second = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("paste event");
    assert!(matches!(first, UiEvent::HotkeyAck), "{first:?}");
    assert!(
        matches!(&second, UiEvent::PasteFailed(message) if message.contains("admin helper")),
        "targeted paste refusal must carry the daemon reason: {second:?}"
    );
}
