//! Config window model: the 27 v1 rows, staged edits, validation.

use std::collections::HashMap;

use serde_json::Value;

use crate::{chord, strings};

/// Config window tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// History settings.
    History,
    /// Popup settings.
    Popup,
    /// Advanced settings.
    Advanced,
    /// Permanent clips settings.
    PermanentClips,
    /// Privacy settings.
    Privacy,
    /// Tray settings.
    Tray,
    /// Startup settings.
    Startup,
    /// General settings.
    General,
}

/// Control kind for one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Boolean toggle.
    Bool,
    /// Integer input.
    Int,
    /// Free text input.
    Text,
    /// Single chord input.
    Chord,
    /// Fixed choice input.
    Enum,
    /// List of strings.
    StringList,
    /// Map of chord strings.
    ChordMap,
    /// List of paste-chord objects.
    PasteChords,
}

/// One row of the settings matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    /// Dotted config key.
    pub key: &'static str,
    /// Owning tab.
    pub tab: Tab,
    /// Control kind.
    pub kind: Kind,
    /// Label string id (`config.*`).
    pub label: &'static str,
}

/// The exact 27 v1 rows in seg-03 S6 table order.
pub const ROWS: &[Row] = &[
    Row {
        key: "monitor.formats",
        tab: Tab::History,
        kind: Kind::StringList,
        label: "config.monitor_formats",
    },
    Row {
        key: "monitor.max_bytes",
        tab: Tab::History,
        kind: Kind::Int,
        label: "config.monitor_max_bytes",
    },
    Row {
        key: "monitor.min_len",
        tab: Tab::History,
        kind: Kind::Int,
        label: "config.monitor_min_len",
    },
    Row {
        key: "monitor.ignores",
        tab: Tab::History,
        kind: Kind::StringList,
        label: "config.monitor_ignores",
    },
    Row {
        key: "monitor.poll_ms",
        tab: Tab::Advanced,
        kind: Kind::Int,
        label: "config.monitor_poll_ms",
    },
    Row {
        key: "monitor.capture_primary",
        tab: Tab::History,
        kind: Kind::Bool,
        label: "config.monitor_capture_primary",
    },
    Row {
        key: "monitor.primary_debounce_ms",
        tab: Tab::Advanced,
        kind: Kind::Int,
        label: "config.monitor_primary_debounce_ms",
    },
    Row {
        key: "history.max_items",
        tab: Tab::History,
        kind: Kind::Int,
        label: "config.history_max_items",
    },
    Row {
        key: "history.max_store_bytes",
        tab: Tab::History,
        kind: Kind::Int,
        label: "config.history_max_store_bytes",
    },
    Row {
        key: "history.max_age_days",
        tab: Tab::History,
        kind: Kind::Int,
        label: "config.history_max_age_days",
    },
    Row {
        key: "history.dedupe_window",
        tab: Tab::History,
        kind: Kind::Int,
        label: "config.history_dedupe_window",
    },
    Row {
        key: "popup.hotkey",
        tab: Tab::Popup,
        kind: Kind::Chord,
        label: "config.popup_hotkey",
    },
    Row {
        key: "popup.placement",
        tab: Tab::Popup,
        kind: Kind::Enum,
        label: "config.popup_placement",
    },
    Row {
        key: "popup.preview_width",
        tab: Tab::Popup,
        kind: Kind::Int,
        label: "config.popup_preview_width",
    },
    Row {
        key: "popup.dismiss_on_focus_loss",
        tab: Tab::Popup,
        kind: Kind::Bool,
        label: "config.popup_dismiss_on_focus_loss",
    },
    Row {
        key: "popup.keys.*",
        tab: Tab::Popup,
        kind: Kind::ChordMap,
        label: "config.popup_keys",
    },
    Row {
        key: "popup.paste_chords",
        tab: Tab::Popup,
        kind: Kind::PasteChords,
        label: "config.popup_paste_chords",
    },
    Row {
        key: "popup.unicode_limit",
        tab: Tab::Advanced,
        kind: Kind::Int,
        label: "config.popup_unicode_limit",
    },
    Row {
        key: "popup.delivery_dwell_ms",
        tab: Tab::Advanced,
        kind: Kind::Int,
        label: "config.popup_delivery_dwell_ms",
    },
    Row {
        key: "permanents.groups",
        tab: Tab::PermanentClips,
        kind: Kind::StringList,
        label: "config.permanents_groups",
    },
    Row {
        key: "privacy.strict",
        tab: Tab::Privacy,
        kind: Kind::Bool,
        label: "config.privacy_strict",
    },
    Row {
        key: "privacy.denylist",
        tab: Tab::Privacy,
        kind: Kind::StringList,
        label: "config.privacy_denylist",
    },
    Row {
        key: "tray.enabled",
        tab: Tab::Tray,
        kind: Kind::Bool,
        label: "config.tray_enabled",
    },
    Row {
        key: "autostart.enabled",
        tab: Tab::Startup,
        kind: Kind::Bool,
        label: "config.autostart_enabled",
    },
    Row {
        key: "updates.check",
        tab: Tab::General,
        kind: Kind::Bool,
        label: "config.updates_check",
    },
    Row {
        key: "logging.level",
        tab: Tab::Advanced,
        kind: Kind::Enum,
        label: "config.logging_level",
    },
    Row {
        key: "popup.delivery",
        tab: Tab::Popup,
        kind: Kind::Enum,
        label: "config.popup_delivery",
    },
    Row {
        key: "windows.helper_auto_elevate",
        tab: Tab::Advanced,
        kind: Kind::Bool,
        label: "config.windows_helper_auto_elevate",
    },
];

/// Row indices the config window renders for `tab` (P-F1): the
/// Permanent Clips tab excludes the legacy `permanents.groups` row,
/// which stays in [`ROWS`] and renders under General so every row
/// still belongs to exactly one visible tab.
#[must_use]
pub fn visible_rows(tab: Tab) -> Vec<usize> {
    ROWS.iter()
        .enumerate()
        .filter(|(_, row)| {
            if tab == Tab::PermanentClips && row.key == "permanents.groups" {
                return false;
            }
            if tab == Tab::General && row.key == "permanents.groups" {
                return true;
            }
            row.tab == tab
        })
        .map(|(index, _)| index)
        .collect()
}

fn row_for(key: &str) -> Option<&'static Row> {
    ROWS.iter().find(|row| row.key == key)
}

fn lookup<'a>(root: &'a Value, key: &str) -> Option<&'a Value> {
    // popup.keys.* addresses the popup.keys object itself.
    let effective: &str = if key == "popup.keys.*" {
        "popup.keys"
    } else {
        key
    };
    let mut cursor = root;
    for part in effective.split('.') {
        cursor = cursor.get(part)?;
    }
    Some(cursor)
}

fn is_int(value: &Value) -> bool {
    value.as_i64().is_some() || value.as_u64().is_some()
}

fn int_value(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|v| i64::try_from(v).ok()))
}

/// Outcome of [`validate`]: the frozen ruler extracts the error id
/// with `unwrap_or_else`, so this wrapper returns the id there while
/// `is_ok` still reports validity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidateResult(Result<(), String>);

impl ValidateResult {
    /// Whether the value was valid.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.0.is_ok()
    }
    /// Whether the value was invalid.
    #[must_use]
    pub fn is_err(&self) -> bool {
        self.0.is_err()
    }
    /// Extract the error id; the fallback is never invoked for an
    /// invalid value (the ruler passes a panicking fallback).
    pub fn unwrap_or_else<F: FnOnce(String) -> String>(self, _fallback: F) -> String {
        match self.0 {
            Ok(()) => String::new(),
            Err(id) => id,
        }
    }
    /// Extract the error id, panicking when valid.
    ///
    /// # Panics
    /// Panics when the value was valid.
    #[must_use]
    pub fn unwrap_err(self) -> String {
        self.0.expect_err("invalid value")
    }
}

/// Validate one value for a known row key. The error carried by the
/// result is a strings-module id describing the failure.
#[must_use]
pub fn validate(key: &str, value: &Value) -> ValidateResult {
    ValidateResult(validate_inner(key, value))
}

fn enum_allowed(key: &str) -> &'static [&'static str] {
    match key {
        "popup.placement" => &["cursor", "center"],
        "popup.delivery" => &["clipboard_restore", "unicode_inject"],
        "logging.level" => &["trace", "debug", "info", "warn", "error"],
        _ => &[],
    }
}

fn validate_inner(key: &str, value: &Value) -> Result<(), String> {
    let row = row_for(key).ok_or_else(|| "config.unknown_key".to_string())?;
    match row.kind {
        Kind::Bool => {
            if value.is_boolean() {
                Ok(())
            } else {
                Err("config.invalid_type".to_string())
            }
        }
        Kind::Int => {
            if !is_int(value) {
                return Err("config.invalid_type".to_string());
            }
            let n = int_value(value).unwrap_or(-1);
            let min: i64 = match key {
                "history.max_items"
                | "popup.preview_width"
                | "monitor.max_bytes"
                | "history.max_store_bytes"
                | "popup.unicode_limit"
                | "monitor.poll_ms" => 1,
                _ => 0,
            };
            if n < min {
                Err("config.invalid_value".to_string())
            } else {
                Ok(())
            }
        }
        Kind::Text => {
            if value.is_string() {
                Ok(())
            } else {
                Err("config.invalid_type".to_string())
            }
        }
        Kind::Chord => {
            let Some(text) = value.as_str() else {
                return Err("config.invalid_type".to_string());
            };
            chord::parse_chord(text)
                .map(|_| ())
                .map_err(|_| "config.invalid_chord".to_string())
        }
        Kind::Enum => {
            let Some(text) = value.as_str() else {
                return Err("config.invalid_type".to_string());
            };
            if enum_allowed(key).contains(&text) {
                Ok(())
            } else {
                Err("config.invalid_enum".to_string())
            }
        }
        Kind::StringList => {
            let Some(list) = value.as_array() else {
                return Err("config.invalid_type".to_string());
            };
            if list.iter().all(Value::is_string) {
                Ok(())
            } else {
                Err("config.invalid_list".to_string())
            }
        }
        Kind::ChordMap => {
            let Some(map) = value.as_object() else {
                return Err("config.invalid_type".to_string());
            };
            for chord_value in map.values() {
                let Some(text) = chord_value.as_str() else {
                    return Err("config.invalid_type".to_string());
                };
                chord::parse_chord(text).map_err(|_| "config.invalid_chord".to_string())?;
            }
            Ok(())
        }
        Kind::PasteChords => {
            let Some(list) = value.as_array() else {
                return Err("config.invalid_type".to_string());
            };
            for item in list {
                let Some(obj) = item.as_object() else {
                    return Err("config.invalid_type".to_string());
                };
                let Some(chord_text) = obj.get("chord").and_then(Value::as_str) else {
                    return Err("config.invalid_chord".to_string());
                };
                if obj.get("app").is_some_and(|v| !v.is_string()) {
                    return Err("config.invalid_type".to_string());
                }
                chord::parse_chord(chord_text).map_err(|_| "config.invalid_chord".to_string())?;
            }
            Ok(())
        }
    }
}

/// Filter row indices by key or label text, case-insensitively.
#[must_use]
pub fn filter_rows(query: &str) -> Vec<usize> {
    let q = query.to_lowercase();
    if q.is_empty() {
        return (0..ROWS.len()).collect();
    }
    ROWS.iter()
        .enumerate()
        .filter(|(_, row)| {
            row.key.to_lowercase().contains(&q)
                || strings::text(row.label).to_lowercase().contains(&q)
        })
        .map(|(index, _)| index)
        .collect()
}

/// Staged config document: saved snapshot plus pending edits.
#[derive(Debug, Clone, Default)]
pub struct ConfigDoc {
    saved: Value,
    staged: HashMap<String, Value>,
}

impl ConfigDoc {
    /// Build from a daemon `config.get` reply (`{"config": {..}}`).
    #[must_use]
    pub fn from_snapshot(snapshot: &Value) -> Self {
        let inner = snapshot
            .get("config")
            .cloned()
            .unwrap_or_else(|| snapshot.clone());
        Self {
            saved: inner,
            staged: HashMap::new(),
        }
    }

    /// Saved value for a dotted key.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        lookup(&self.saved, key)
    }

    /// Staged value for a key, if an edit is pending.
    #[must_use]
    pub fn staged(&self, key: &str) -> Option<&Value> {
        self.staged.get(key)
    }

    /// Stage one edit after validation; staging the saved value
    /// clears the dirty flag for that key.
    ///
    /// # Errors
    /// Returns the validation string id for an invalid value.
    pub fn stage(&mut self, key: &str, value: Value) -> Result<(), String> {
        validate_inner(key, &value)?;
        if self.get(key) == Some(&value) {
            self.staged.remove(key);
        } else {
            self.staged.insert(key.to_string(), value);
        }
        Ok(())
    }

    /// Whether a key has a pending edit.
    #[must_use]
    pub fn is_dirty(&self, key: &str) -> bool {
        self.staged.contains_key(key)
    }

    /// Dirty keys in ROWS order.
    #[must_use]
    pub fn dirty_keys(&self) -> Vec<String> {
        ROWS.iter()
            .filter(|row| self.staged.contains_key(row.key))
            .map(|row| row.key.to_string())
            .collect()
    }

    /// Ordered `config.set` plan for dirty rows (U1-F1, not atomic).
    #[must_use]
    pub fn apply_plan(&self) -> Vec<(String, Value)> {
        ROWS.iter()
            .filter_map(|row| {
                self.staged
                    .get(row.key)
                    .map(|value| (row.key.to_string(), value.clone()))
            })
            .collect()
    }

    /// Drop all staged edits.
    pub fn revert(&mut self) {
        self.staged.clear();
    }

    /// Mark one key clean after a successful Apply step.
    pub fn mark_applied(&mut self, key: &str) {
        if let Some(value) = self.staged.remove(key) {
            // Record into the saved snapshot on a best-effort basis for
            // top-level dotted paths used by the window.
            set_dotted(&mut self.saved, key, value);
        }
    }
}

fn set_dotted(root: &mut Value, key: &str, value: Value) {
    let effective: &str = if key == "popup.keys.*" {
        "popup.keys"
    } else {
        key
    };
    let parts: Vec<&str> = effective.split('.').collect();
    let mut cursor = root;
    for (idx, part) in parts.iter().enumerate() {
        if idx + 1 == parts.len() {
            if let Some(obj) = cursor.as_object_mut() {
                obj.insert((*part).to_string(), value);
            }
            return;
        }
        if !cursor.get(part).is_some_and(Value::is_object) {
            if let Some(obj) = cursor.as_object_mut() {
                obj.insert((*part).to_string(), Value::Object(serde_json::Map::new()));
            } else {
                return;
            }
        }
        let Some(next) = cursor.get_mut(part) else {
            return;
        };
        cursor = next;
    }
}
