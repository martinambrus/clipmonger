//! Popup key bindings with the documented v1 defaults.

use std::collections::HashMap;

/// Renderer-neutral key press handed to the keymap.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEvent {
    /// Key name such as `"Enter"`, `"Escape"`, `"F5"`, `"ArrowUp"` or `"a"`.
    pub key: String,
    /// Control modifier held.
    pub ctrl: bool,
    /// Alt modifier held.
    pub alt: bool,
    /// Shift modifier held.
    pub shift: bool,
    /// Super (OS logo) modifier held.
    pub super_key: bool,
}

impl KeyEvent {
    /// Build an event; `super_key` starts cleared.
    #[must_use]
    pub fn new(key: String, ctrl: bool, alt: bool, shift: bool) -> Self {
        Self {
            key,
            ctrl,
            alt,
            shift,
            super_key: false,
        }
    }

    /// Canonical chord form used as the binding-table key.
    #[must_use]
    pub fn chord(&self) -> (String, bool, bool, bool, bool) {
        (
            self.key.to_lowercase(),
            self.ctrl,
            self.alt,
            self.shift,
            self.super_key,
        )
    }
}

/// Popup action resolved from a key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// Move selection up.
    NavigateUp,
    /// Move selection down.
    NavigateDown,
    /// Move selection one page up.
    PageUp,
    /// Move selection one page down.
    PageDown,
    /// Move selection to the first row.
    Home,
    /// Move selection to the last row.
    End,
    /// Paste the selected clip.
    Paste,
    /// Paste the selected clip through the alternate route.
    PasteInto,
    /// Close the popup without acting.
    Dismiss,
    /// Delete the selected clip.
    Remove,
    /// Edit the selected clip.
    Edit,
    /// Pin or unpin the selected clip.
    Pin,
    /// Re-fetch the list.
    Refresh,
    /// Switch to permanent group tab `1..=9`.
    GroupTab(u8),
    /// Search text typed by the user.
    TypeChar(char),
    /// No bound action.
    None,
}

/// Binding table from canonical chords to actions.
#[derive(Debug, Clone, Default)]
pub struct Keymap {
    bindings: HashMap<(String, bool, bool, bool, bool), Action>,
}

impl Keymap {
    /// Binding table with the documented v1 defaults.
    #[must_use]
    pub fn defaults() -> Self {
        let mut map = Self::default();
        let plain = [
            ("ArrowUp", Action::NavigateUp),
            ("ArrowDown", Action::NavigateDown),
            ("PageUp", Action::PageUp),
            ("PageDown", Action::PageDown),
            ("Home", Action::Home),
            ("End", Action::End),
            ("Enter", Action::Paste),
            ("Escape", Action::Dismiss),
            ("Delete", Action::Remove),
            ("F5", Action::Refresh),
        ];
        for (key, action) in plain {
            map.bindings
                .insert((key.to_lowercase(), false, false, false, false), action);
        }
        map.bindings.insert(
            ("Enter".to_lowercase(), true, false, false, false),
            Action::PasteInto,
        );
        map.bindings.insert(
            ("e".to_lowercase(), true, false, false, false),
            Action::Edit,
        );
        map.bindings
            .insert(("p".to_lowercase(), true, false, false, false), Action::Pin);
        for n in 1_u8..=9 {
            map.bindings.insert(
                (n.to_string(), false, true, false, false),
                Action::GroupTab(n),
            );
        }
        map
    }

    /// Build defaults plus `popup.keys.*` overrides (action name to
    /// chord text). Unknown action names error, never panic.
    ///
    /// # Errors
    /// Returns a message for an unknown action, bad chord, or conflict.
    pub fn from_config_keys(
        keys: &std::collections::BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let mut map = Self::defaults();
        for (name, chord_text) in keys {
            let action = match name.as_str() {
                "paste" => Action::Paste,
                "paste_into" => Action::PasteInto,
                "dismiss" => Action::Dismiss,
                "remove" => Action::Remove,
                "edit" => Action::Edit,
                "pin" => Action::Pin,
                "refresh" => Action::Refresh,
                "navigate_up" => Action::NavigateUp,
                "navigate_down" => Action::NavigateDown,
                "page_up" => Action::PageUp,
                "page_down" => Action::PageDown,
                "home" => Action::Home,
                "end" => Action::End,
                other => return Err(format!("unknown popup action {other}")),
            };
            let event = crate::chord::parse_chord(chord_text).map_err(|error| error.to_string())?;
            // Replace any existing binding for this action, then bind.
            map.bindings.retain(|_, bound| *bound != action);
            map.bind(action, event)?;
        }
        Ok(map)
    }

    /// Resolve an event to its action, falling back to typing.
    #[must_use]
    pub fn action_for(&self, event: &KeyEvent) -> Option<Action> {
        if let Some(action) = self.bindings.get(&event.chord()) {
            return Some(*action);
        }
        if !event.ctrl && !event.alt && !event.super_key && event.key.chars().count() == 1 {
            return event.key.chars().next().map(Action::TypeChar);
        }
        Option::None
    }

    /// Bind an action to a chord, refusing occupied chords.
    ///
    /// # Errors
    ///
    /// Returns a message naming the chord when it is already bound.
    #[allow(clippy::needless_pass_by_value)]
    pub fn bind(&mut self, action: Action, chord: KeyEvent) -> Result<(), String> {
        let key = chord.chord();
        if self.bindings.contains_key(&key) {
            return Err(format!("chord {:?} is already bound", chord.key));
        }
        self.bindings.insert(key, action);
        Ok(())
    }
}
