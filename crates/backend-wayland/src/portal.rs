//! `GlobalShortcuts` portal pure logic (seg-06 S4, T-602).
//!
//! The live portal session (ashpd `GlobalShortcuts`, async, daemon-owned
//! runtime per W-F4) is a later session task; this module holds the pure,
//! unit-testable pieces: stable shortcut ids, `Activated` parsing, and the
//! bind plan from platform chord grammar to portal triggers.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One portal bind request derived from a platform chord.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindRequest {
    /// Stable portal shortcut id ([`shortcut_id`]).
    pub id: String,
    /// Portal `preferred_trigger` string (modifiers joined with `+`).
    pub trigger: String,
}

/// Stable portal shortcut id for a platform chord.
///
/// Lowercase ASCII `[a-z0-9-]` only: the chord is lowercased, the `win`
/// modifier aliases to `super`, and every run of non-alphanumeric characters
/// folds to a single `-` (leading/trailing dashes trimmed). Empty input maps
/// to `"hotkey"`.
#[must_use]
pub fn shortcut_id(chord: &str) -> String {
    let mut out = String::with_capacity(chord.len());
    let mut prev_dash = true; // Trim leading dashes.
    for token in chord.split('+') {
        let token = token.trim();
        // Alias the Windows key to the portal/Sway modifier name.
        let token = if token.eq_ignore_ascii_case("win") {
            "super"
        } else {
            token
        };
        if !out.is_empty() && !prev_dash {
            out.push('-');
        }
        prev_dash = true;
        for ch in token.chars() {
            if ch.is_ascii_alphanumeric() {
                out.push(ch.to_ascii_lowercase());
                prev_dash = false;
            } else if !prev_dash {
                out.push('-');
                prev_dash = true;
            }
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        return "hotkey".to_string();
    }
    out
}

/// Parse the shortcut id out of an `Activated` signal payload.
///
/// Assumed JSON shape (mirrors ashpd `Activated(session, shortcut_id,
/// timestamp, options)` mapped to an object): `{"shortcut_id": "<id>", ...
/// }`. Unknown ids are passed through untouched (the caller maps them to
/// `HotkeyId`; anything unmapped resolves to `None` there, never a panic).
/// Non-objects, missing keys, and non-string values yield `None`.
#[must_use]
pub fn parse_activated(payload: &Value) -> Option<String> {
    payload
        .get("shortcut_id")
        .or_else(|| payload.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

/// Build the portal bind plan for platform chords.
///
/// Each chord yields one [`BindRequest`] whose trigger normalizes modifiers
/// (`win` → `super`, lowercase, `+`-joined); the plan is idempotent, so a
/// rebind after `SessionClosed`/`ShortcutsChanged` replays the same vector.
#[must_use]
pub fn bind_requested(chords: &[String]) -> Vec<BindRequest> {
    chords
        .iter()
        .map(|chord| {
            let trigger = chord
                .split('+')
                .map(|token| {
                    let token = token.trim();
                    if token.eq_ignore_ascii_case("win") {
                        "super".to_string()
                    } else {
                        token.to_ascii_lowercase()
                    }
                })
                .collect::<Vec<_>>()
                .join("+");
            BindRequest {
                id: shortcut_id(chord),
                trigger,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn t602_shortcut_ids_are_stable_and_constrained() {
        assert_eq!(shortcut_id("Ctrl+Shift+P"), "ctrl-shift-p");
        assert_eq!(shortcut_id("Win+V"), "super-v");
        assert_eq!(shortcut_id("ctrl+shift+p"), "ctrl-shift-p");
        assert_eq!(shortcut_id(""), "hotkey");
        for id in [
            shortcut_id("Ctrl+Shift+P"),
            shortcut_id("Win+V"),
            shortcut_id("Alt+F1?"),
        ] {
            assert!(
                id.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "unexpected char in {id}"
            );
        }
    }

    #[test]
    fn t602_parse_activated_maps_known_and_unknown() {
        let payload = json!({"shortcut_id": "ctrl-shift-p", "timestamp": 123u64});
        assert_eq!(parse_activated(&payload), Some("ctrl-shift-p".to_string()));
        // Unknown id passes through; caller maps to None HotkeyId.
        let payload = json!({"shortcut_id": "other-app-key"});
        assert_eq!(parse_activated(&payload), Some("other-app-key".to_string()));
        // Missing / wrong-typed / empty -> None, no panic.
        assert_eq!(parse_activated(&json!({})), None);
        assert_eq!(parse_activated(&json!({"shortcut_id": 7})), None);
        assert_eq!(parse_activated(&json!({"shortcut_id": ""})), None);
        assert_eq!(parse_activated(&json!("ctrl-shift-p")), None);
    }

    #[test]
    fn t602_bind_plan_aliases_win_and_is_idempotent() {
        let chords = vec!["Win+V".to_string(), "Ctrl+Shift+P".to_string()];
        let first = bind_requested(&chords);
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].id, "super-v");
        assert_eq!(first[0].trigger, "super+v");
        assert_eq!(first[1].id, "ctrl-shift-p");
        assert_eq!(first[1].trigger, "ctrl+shift+p");
        // Double bind replays the same vector.
        assert_eq!(bind_requested(&chords), first);
    }
}
