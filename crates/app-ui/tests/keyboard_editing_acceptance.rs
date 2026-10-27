//! Keyboard-while-searching regression (Step 6 joint manual QA, Ubuntu).
//!
//! T-907 found Up/Down did nothing while the search box had focus: the
//! shell swallowed every special key during text editing. Navigation keys
//! must reach the view-model (seg-09 section 8: type = search, Up/Down
//! navigate); text-editing keys must not (Delete would soft-delete a clip).
//!
//! Run: `cargo test -p clipmonger-app-ui --test keyboard_editing_acceptance`

use clipmonger_app_ui::popup;

#[test]
fn navigation_keys_forward_while_editing() {
    for name in [
        "ArrowUp",
        "ArrowDown",
        "Home",
        "End",
        "PageUp",
        "PageDown",
        "Enter",
        "Escape",
        "F5",
    ] {
        assert!(
            popup::forwards_key_when_editing(name),
            "{name} must forward while searching"
        );
    }
}

#[test]
fn editing_keys_do_not_forward_while_editing() {
    for name in ["Delete", "p", "e", "1"] {
        assert!(
            !popup::forwards_key_when_editing(name),
            "{name} must stay with the text field while searching"
        );
    }
}
