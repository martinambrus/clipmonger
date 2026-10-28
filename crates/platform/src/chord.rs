//! Hotkey chord parser: `MOD[+]MOD...[+]KEY`, case-insensitive.
//!
//! Canonical form preserves input modifier order with lowercase names,
//! e.g. `ctrl+alt+h` and `super+alt+Digit1`.

use std::fmt::Display;

use crate::types::{Error, Result};

/// Default popup hotkey on all OS (config-changeable).
pub const DEFAULT_POPUP_HOTKEY: &str = "ctrl+alt+h";

/// v1 reserved-chord tables are empty; the mechanism exists for seg-09 §14.5.
const RESERVED_WINDOWS: &[&str] = &[];
/// v1 reserved-chord tables are empty; the mechanism exists for seg-09 §14.5.
const RESERVED_LINUX: &[&str] = &[];
/// v1 reserved-chord tables are empty; the mechanism exists for seg-09 §14.5.
const RESERVED_MACOS: &[&str] = &[];

/// Parsed hotkey chord in canonical form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chord {
    /// Lowercase modifiers in input order (`super` covers win/cmd/meta).
    pub mods: Vec<String>,
    /// Canonical key name (`v`, `Digit1`, `F12`, `Space`, ...).
    pub key: String,
}

impl Display for Chord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = self.mods.clone();
        parts.push(self.key.clone());
        write!(f, "{}", parts.join("+"))
    }
}

/// Normalize one modifier segment.
///
/// # Errors
///
/// Returns [`Error::Unsupported`] for unknown modifier names.
fn norm_mod(seg: &str) -> Result<String> {
    let lower = seg.to_ascii_lowercase();
    let bare = lower
        .strip_prefix("l-")
        .or_else(|| lower.strip_prefix("r-"))
        .unwrap_or(&lower);
    match bare {
        "ctrl" | "alt" | "shift" | "super" => Ok(bare.to_string()),
        "win" | "cmd" | "meta" => Ok("super".to_string()),
        _ => Err(Error::Unsupported(format!("unknown modifier '{seg}'"))),
    }
}

/// Normalize the key segment.
///
/// # Errors
///
/// Returns [`Error::Unsupported`] for empty or unknown key names.
fn norm_key(seg: &str) -> Result<String> {
    if seg.is_empty() {
        return Err(Error::Unsupported("missing key".to_string()));
    }
    if seg.len() == 1 {
        let c = seg.chars().next().unwrap_or('?');
        if c.is_ascii_alphanumeric() {
            if c.is_ascii_digit() {
                return Ok(format!("Digit{c}"));
            }
            return Ok(c.to_ascii_lowercase().to_string());
        }
        return Err(Error::Unsupported(format!("bad key '{seg}'")));
    }
    let lower = seg.to_ascii_lowercase();
    if let Some(digits) = lower.strip_prefix("digit") {
        if digits.len() == 1 && digits.chars().all(|c| c.is_ascii_digit()) {
            return Ok(format!("Digit{digits}"));
        }
    }
    if let Some(rest) = lower.strip_prefix("key") {
        if rest.len() == 1 && rest.chars().all(|c| c.is_ascii_alphanumeric()) {
            return norm_key(rest);
        }
    }
    if let Some(n) = lower.strip_prefix('f') {
        if !n.is_empty() && n.len() <= 2 && n.chars().all(|c| c.is_ascii_digit()) {
            return Ok(format!("F{n}"));
        }
    }
    match lower.as_str() {
        "space" | "tab" | "enter" | "escape" | "backspace" | "delete" | "insert" | "home"
        | "end" | "comma" | "period" | "slash" | "semicolon" | "quote" | "backquote"
        | "backslash" | "minus" | "equal" | "bracketleft" | "bracketright" => {
            let mut name = lower;
            name[0..1].make_ascii_uppercase();
            Ok(name)
        }
        "pageup" => Ok("PageUp".to_string()),
        "pagedown" => Ok("PageDown".to_string()),
        "arrowup" => Ok("ArrowUp".to_string()),
        "arrowdown" => Ok("ArrowDown".to_string()),
        "arrowleft" => Ok("ArrowLeft".to_string()),
        "arrowright" => Ok("ArrowRight".to_string()),
        _ => Err(Error::Unsupported(format!("unknown key '{seg}'"))),
    }
}

/// Parse a chord string into canonical form.
///
/// # Errors
///
/// Returns [`Error::Unsupported`] for empty input, missing keys,
/// unknown modifiers, or unknown key names.
pub fn parse(input: &str) -> Result<Chord> {
    let segs: Vec<String> = input.split('+').map(|s| s.trim().to_string()).collect();
    if segs.len() == 1 && segs[0].is_empty() {
        return Err(Error::Unsupported("empty chord".to_string()));
    }
    if segs.iter().any(String::is_empty) {
        return Err(Error::Unsupported(format!("bad chord '{input}'")));
    }
    let (mod_segs, key_seg) = segs.split_at(segs.len() - 1);
    // A lone modifier name is not a key (`parse("ctrl")` fails in `norm_key`).
    let key = norm_key(&key_seg[0])?;
    let mut mods = Vec::new();
    for m in mod_segs {
        let n = norm_mod(m)?;
        if !mods.contains(&n) {
            mods.push(n);
        }
    }
    Ok(Chord { mods, key })
}

/// Check OS-reserved chords; v1 tables are empty so this is always false.
///
/// The `os` tag is `windows`, `linux`, or `macos` (anything else matches nothing).
#[must_use]
pub fn is_reserved(chord: &Chord, os: &str) -> bool {
    let table: &[&str] = match os {
        "windows" => RESERVED_WINDOWS,
        "linux" => RESERVED_LINUX,
        "macos" => RESERVED_MACOS,
        _ => &[],
    };
    table.contains(&chord.to_string().as_str())
}
