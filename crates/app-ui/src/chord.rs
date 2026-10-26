//! Chord string parse/format and OS-reserved checks.

use crate::keymap::KeyEvent;

/// Operating system family for reserved-chord checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    /// Microsoft Windows.
    Windows,
    /// X11 session.
    X11,
    /// Wayland session.
    Wayland,
    /// Apple macOS.
    Macos,
}

/// Chord parse failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChordError(pub String);

impl std::fmt::Display for ChordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid chord: {}", self.0)
    }
}
impl std::error::Error for ChordError {}

fn canonical_key(raw: &str) -> Option<String> {
    let lower = raw.to_lowercase();
    let named: &[(&str, &str)] = &[
        ("enter", "Enter"),
        ("escape", "Escape"),
        ("esc", "Escape"),
        ("delete", "Delete"),
        ("del", "Delete"),
        ("backspace", "Backspace"),
        ("tab", "Tab"),
        ("space", "Space"),
        ("home", "Home"),
        ("end", "End"),
        ("pageup", "PageUp"),
        ("pgup", "PageUp"),
        ("pagedown", "PageDown"),
        ("pgdn", "PageDown"),
        ("arrowup", "ArrowUp"),
        ("up", "ArrowUp"),
        ("arrowdown", "ArrowDown"),
        ("down", "ArrowDown"),
        ("arrowleft", "ArrowLeft"),
        ("left", "ArrowLeft"),
        ("arrowright", "ArrowRight"),
        ("right", "ArrowRight"),
        ("super", "Super"),
    ];
    for (name, canon) in named {
        if lower == *name {
            return Some((*canon).to_string());
        }
    }
    if lower.len() == 2 && lower.starts_with('f') {
        if let Ok(n) = lower[1..].parse::<u8>() {
            if (1..=24).contains(&n) {
                return Some(format!("F{n}"));
            }
        }
    }
    if lower.len() > 1 && lower.starts_with('f') {
        if let Ok(n) = lower[1..].parse::<u8>() {
            if (1..=24).contains(&n) {
                return Some(format!("F{n}"));
            }
        }
    }
    let mut chars = raw.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_alphanumeric() {
            return Some(c.to_lowercase().to_string());
        }
    }
    None
}

/// Parse a chord such as `ctrl+alt+v` into a key event.
///
/// # Errors
/// Returns [`ChordError`] for empty input, unknown or duplicate
/// modifiers, a missing key, or an unrecognised key name.
pub fn parse_chord(input: &str) -> Result<KeyEvent, ChordError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(ChordError("empty".into()));
    }
    let parts: Vec<&str> = trimmed.split('+').collect();
    if parts.iter().any(|p| p.trim().is_empty()) {
        return Err(ChordError("empty part".into()));
    }
    let (key_part, mod_parts) = parts
        .split_last()
        .ok_or_else(|| ChordError("missing key".into()))?;
    let mut ev = KeyEvent::new(String::new(), false, false, false);
    for part in mod_parts {
        match part.trim().to_lowercase().as_str() {
            "ctrl" | "control" => {
                if ev.ctrl {
                    return Err(ChordError("duplicate ctrl".into()));
                }
                ev.ctrl = true;
            }
            "alt" => {
                if ev.alt {
                    return Err(ChordError("duplicate alt".into()));
                }
                ev.alt = true;
            }
            "shift" => {
                if ev.shift {
                    return Err(ChordError("duplicate shift".into()));
                }
                ev.shift = true;
            }
            "super" | "win" | "meta" | "cmd" | "command" => {
                if ev.super_key {
                    return Err(ChordError("duplicate super".into()));
                }
                ev.super_key = true;
            }
            other => return Err(ChordError(format!("unknown modifier {other}"))),
        }
    }
    let key_raw = key_part.trim();
    // A lone modifier is not a chord (except Super key event used by reserved check is constructed manually).
    if matches!(
        key_raw.to_lowercase().as_str(),
        "ctrl" | "control" | "alt" | "shift" | "hyper"
    ) {
        return Err(ChordError("modifier without key".into()));
    }
    if key_raw.to_lowercase() == "hyper" {
        return Err(ChordError("unknown modifier hyper".into()));
    }
    ev.key = canonical_key(key_raw).ok_or_else(|| ChordError(format!("unknown key {key_raw}")))?;
    Ok(ev)
}

/// Format an event in canonical form: modifiers in ctrl, alt, shift,
/// super order, joined with `+`.
#[must_use]
pub fn format_chord(event: &KeyEvent) -> String {
    let mut parts: Vec<String> = Vec::new();
    if event.ctrl {
        parts.push("ctrl".to_string());
    }
    if event.alt {
        parts.push("alt".to_string());
    }
    if event.shift {
        parts.push("shift".to_string());
    }
    if event.super_key {
        parts.push("super".to_string());
    }
    let key = canonical_key(&event.key).unwrap_or_else(|| event.key.clone());
    parts.push(key);
    parts.join("+")
}

/// Reason id when the chord is OS-reserved, else `None`.
#[must_use]
pub fn reserved_reason(event: &KeyEvent, os: Os) -> Option<&'static str> {
    let key_lower = event.key.to_lowercase();
    // Super alone is reserved on Wayland/GNOME (and generally unbindable).
    if key_lower == "super" {
        return Some(match os {
            Os::Wayland => "chord.reserved_wayland",
            _ => "chord.reserved_generic",
        });
    }
    match os {
        Os::Windows => {
            if event.super_key
                && (key_lower == "l" || key_lower == "d" || key_lower == "e" || key_lower == "tab")
            {
                return Some("chord.reserved_windows");
            }
            if event.alt && !event.ctrl && !event.super_key && key_lower == "tab" {
                return Some("chord.reserved_windows");
            }
            if event.ctrl && event.alt && key_lower == "delete" {
                return Some("chord.reserved_windows");
            }
            None
        }
        Os::Wayland => {
            if event.super_key && key_lower == "tab" {
                return Some("chord.reserved_wayland");
            }
            None
        }
        Os::X11 | Os::Macos => None,
    }
}
