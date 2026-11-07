//! wlroots-family Wayland (Sway, Hyprland) and COSMIC: active-window
//! tracking for auto-paste.
//!
//! These compositors report every toplevel's `app_id` and activation over
//! the wlroots foreign-toplevel protocol, or on COSMIC over the ext
//! toplevel list with COSMIC's toplevel info
//! ([`clipmonger_backend_wayland::toplevel`]). The tracker thread feeds
//! each activation into the same [`crate::kwin::Focus`] state the `KWin`
//! script feeds, so [`crate::kwin::auto_paste`] applies the same rules:
//! wait for focus to leave our popup, Ctrl+Shift+V for terminals, never
//! paste while our own window is active.

use std::sync::Arc;

use clipmonger_backend_wayland::toplevel::{self, Activation};

/// Whether this Wayland session is neither `KWin` nor GNOME: the sessions
/// where the wlroots and COSMIC protocols (foreign toplevel, virtual
/// keyboard) are tried.
#[must_use]
pub fn is_wlr_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        && !crate::current_desktop_is("kde")
        && !crate::current_desktop_is("gnome")
}

/// Whether the daemon can press the paste chord itself on this session: a
/// wlroots-family or COSMIC session whose compositor offers both a
/// toplevel protocol set (to keep the chord out of our popup) and the
/// virtual keyboard.
#[must_use]
pub fn auto_paste_available() -> bool {
    is_wlr_wayland()
        && toplevel::available()
        && clipmonger_backend_wayland::virtual_keyboard::available()
}

/// The window identities recorded for one activation: the `app_id` only
/// (a title such as "kitty" in a browser tab must not pick the terminal
/// chord).
#[must_use]
pub fn activation_ids(activation: &Activation) -> Vec<String> {
    vec![activation.app_id.clone()]
}

/// Start the tracker thread on wlroots-family and COSMIC sessions whose
/// compositor offers a toplevel protocol set (no-op elsewhere).
pub fn spawn_focus_tracker(state: Arc<crate::DaemonState>) {
    if !is_wlr_wayland() || !toplevel::available() {
        return;
    }
    std::thread::spawn(move || {
        let on_change = Box::new(move |activation: &Activation| {
            state.window_activated(activation_ids(activation));
        });
        if let Err(e) = toplevel::track(on_change) {
            eprintln!("daemon: foreign-toplevel focus tracking stopped: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kwin::Focus;

    fn activation(app_id: &str, title: &str) -> Activation {
        Activation {
            app_id: app_id.to_string(),
            title: title.to_string(),
        }
    }

    #[test]
    fn activations_map_onto_focus() {
        let mut focus = Focus::default();
        focus.activated(activation_ids(&activation("foot", "~")));
        assert!(focus.tracking);
        assert!(!focus.own_active);
        assert_eq!(focus.target, Some(vec!["foot".to_string()]));
        focus.activated(activation_ids(&activation(
            crate::portal::APP_ID,
            "Clipmonger",
        )));
        assert!(focus.own_active, "our popup is never the target");
        assert_eq!(focus.target, Some(vec!["foot".to_string()]));
        // The popup closed and nothing else is active yet.
        focus.activated(activation_ids(&Activation::default()));
        assert!(!focus.own_active);
        assert_eq!(focus.target, Some(vec!["foot".to_string()]));
        focus.activated(activation_ids(&activation("firefox", "kitty - Search")));
        assert_eq!(focus.target, Some(vec!["firefox".to_string()]));
    }
}
