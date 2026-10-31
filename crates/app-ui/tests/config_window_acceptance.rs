//! Config-window size regression (Step 6 joint manual QA, Ubuntu).
//!
//! T-906 found Apply/Revert unreachable: the config panel renders inside
//! the 480x360 popup viewport, with the button row below a greedy scroll
//! area. While config is open the shell window must grow to config size.
//!
//! Edit fields (Windows QA 2026-10-31): the Win32 settings window showed
//! string values JSON-quoted, so an edited `"ctrl+alt+j"` failed chord
//! validation and `popup.hotkey` could not change. Both windows now share
//! one text form per row kind that parses back unchanged.
//!
//! Run: `cargo test -p clipmonger-app-ui --test config_window_acceptance`

use clipmonger_app_ui::config_model::{parse_edit, value_text, ConfigDoc, Kind};
use clipmonger_app_ui::placement::{window_size_for, CONFIG_DEFAULT, POPUP_DEFAULT};
use serde_json::json;

#[test]
fn config_open_uses_the_larger_window() {
    assert_eq!(window_size_for(false), POPUP_DEFAULT);
    assert_eq!(window_size_for(true), CONFIG_DEFAULT);
    assert!(CONFIG_DEFAULT.0 > POPUP_DEFAULT.0);
    assert!(CONFIG_DEFAULT.1 > POPUP_DEFAULT.1);
    // Tall enough for tabs + rows + Apply/Revert row on screen together.
    assert!(CONFIG_DEFAULT.1 >= 600);
}

#[test]
fn edit_field_text_parses_back_for_every_kind() {
    let samples = [
        (Kind::Bool, json!(true)),
        (Kind::Int, json!(750)),
        (Kind::Text, json!("C:\\Users\\a b\\notes.txt")),
        (Kind::Chord, json!("ctrl+alt+j")),
        (Kind::Enum, json!("center")),
        (Kind::StringList, json!(["keepass.exe", "1password.exe"])),
        (Kind::StringList, json!([])),
        (Kind::ChordMap, json!({"paste": "enter"})),
        (
            Kind::PasteChords,
            json!([{"chord": "ctrl+shift+v", "app": "x"}]),
        ),
        (Kind::PasteChords, json!([])),
    ];
    for (kind, value) in samples {
        let text = value_text(kind, &value);
        assert_eq!(
            parse_edit(kind, &text),
            Some(value),
            "{kind:?} via {text:?}"
        );
    }
    assert_eq!(value_text(Kind::Chord, &json!("ctrl+alt+h")), "ctrl+alt+h");
    assert_eq!(
        value_text(Kind::StringList, &json!(["a.exe", "b.exe"])),
        "a.exe, b.exe"
    );
}

#[test]
fn edited_popup_hotkey_stages_for_apply() {
    let mut doc = ConfigDoc::from_snapshot(&json!({"config": {"popup": {"hotkey": "ctrl+alt+h"}}}));
    let shown = value_text(Kind::Chord, doc.get("popup.hotkey").expect("saved chord"));
    let edited = parse_edit(Kind::Chord, &shown.replace("+h", "+j")).expect("chord text parses");
    doc.stage("popup.hotkey", edited)
        .expect("edited chord stages");
    assert_eq!(
        doc.apply_plan(),
        vec![("popup.hotkey".to_string(), json!("ctrl+alt+j"))]
    );
}
