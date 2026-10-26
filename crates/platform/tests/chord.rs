//! Chord parser vectors: aliases, canonical forms, errors, reserved mechanism.

use clipmonger_platform::chord::{is_reserved, parse, DEFAULT_POPUP_HOTKEY};

#[test]
fn canonical_lowercase_letters() {
    assert_eq!(parse("ctrl+shift+v").unwrap().to_string(), "ctrl+shift+v");
    assert_eq!(parse("CTRL+ALT+V").unwrap().to_string(), "ctrl+alt+v");
}

#[test]
fn super_aliases_win_cmd_meta() {
    assert_eq!(parse("win+alt+1").unwrap().to_string(), "super+alt+Digit1");
    assert_eq!(parse("cmd+shift+a").unwrap().to_string(), "super+shift+a");
    assert_eq!(parse("meta+x").unwrap().to_string(), "super+x");
}

#[test]
fn bare_digit_alias_to_digit_code() {
    assert_eq!(
        parse("super+alt+1").unwrap().to_string(),
        "super+alt+Digit1"
    );
    assert_eq!(parse("ctrl+0").unwrap().to_string(), "ctrl+Digit0");
}

#[test]
fn key_prefix_alias_to_bare_letter() {
    assert_eq!(parse("ctrl+KeyV").unwrap().to_string(), "ctrl+v");
    assert_eq!(parse("ctrl+keyv").unwrap().to_string(), "ctrl+v");
}

#[test]
fn named_and_function_keys() {
    assert_eq!(parse("ctrl+Space").unwrap().to_string(), "ctrl+Space");
    assert_eq!(parse("ctrl+space").unwrap().to_string(), "ctrl+Space");
    assert_eq!(parse("alt+F1").unwrap().to_string(), "alt+F1");
    assert_eq!(parse("alt+f12").unwrap().to_string(), "alt+F12");
}

#[test]
fn default_popup_hotkey_parses() {
    assert_eq!(
        parse(DEFAULT_POPUP_HOTKEY).unwrap().to_string(),
        "ctrl+alt+v"
    );
}

#[test]
fn bad_chords_error() {
    for bad in [
        "",
        "   ",
        "ctrl+",
        "+v",
        "ctrl++v",
        "ctrl",
        "hyper+x",
        "ctrl+BogusKey99",
    ] {
        assert!(parse(bad).is_err(), "expected error for {bad:?}");
    }
}

#[test]
fn reserved_mechanism_empty_tables() {
    let chord = parse("ctrl+alt+v").unwrap();
    for os in ["windows", "linux", "macos", "weird-os"] {
        assert!(!is_reserved(&chord, os), "v1 tables are empty (os={os})");
    }
}
