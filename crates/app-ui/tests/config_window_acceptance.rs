//! Config-window size regression (Step 6 joint manual QA, Ubuntu).
//!
//! T-906 found Apply/Revert unreachable: the config panel renders inside
//! the 480x360 popup viewport, with the button row below a greedy scroll
//! area. While config is open the shell window must grow to config size.
//!
//! Run: `cargo test -p clipmonger-app-ui --test config_window_acceptance`

use clipmonger_app_ui::placement::{window_size_for, CONFIG_DEFAULT, POPUP_DEFAULT};

#[test]
fn config_open_uses_the_larger_window() {
    assert_eq!(window_size_for(false), POPUP_DEFAULT);
    assert_eq!(window_size_for(true), CONFIG_DEFAULT);
    assert!(CONFIG_DEFAULT.0 > POPUP_DEFAULT.0);
    assert!(CONFIG_DEFAULT.1 > POPUP_DEFAULT.1);
    // Tall enough for tabs + rows + Apply/Revert row on screen together.
    assert!(CONFIG_DEFAULT.1 >= 600);
}
