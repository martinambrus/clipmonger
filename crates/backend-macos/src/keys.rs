//! Canonical chord -> macOS virtual key codes (Carbon `kVK_*`) and
//! `CGEventFlags` (pure).

use clipmonger_platform::chord::Chord;
use clipmonger_platform::types::{Error, Result};

/// `kVK_ANSI_V`.
pub const KVK_V: u16 = 0x09;
/// `kVK_Command`.
pub const KVK_COMMAND: u16 = 0x37;
/// `kVK_Shift`.
pub const KVK_SHIFT: u16 = 0x38;
/// `kVK_Option`.
pub const KVK_OPTION: u16 = 0x3A;
/// `kVK_Control`.
pub const KVK_CONTROL: u16 = 0x3B;

/// `kCGEventFlagMaskShift`.
pub const FLAG_SHIFT: u64 = 0x0002_0000;
/// `kCGEventFlagMaskControl`.
pub const FLAG_CONTROL: u64 = 0x0004_0000;
/// `kCGEventFlagMaskAlternate` (Option).
pub const FLAG_OPTION: u64 = 0x0008_0000;
/// `kCGEventFlagMaskCommand`.
pub const FLAG_COMMAND: u64 = 0x0010_0000;

/// One modifier: its key code and event flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Modifier {
    /// Virtual key code of the left-hand key.
    pub code: u16,
    /// `CGEventFlags` bit.
    pub flag: u64,
}

/// Resolved chord: modifiers in order, then the key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyChord {
    /// Modifiers to hold.
    pub mods: Vec<Modifier>,
    /// Main key code.
    pub key: u16,
}

impl KeyChord {
    /// Combined flags of all modifiers.
    #[must_use]
    pub fn flags(&self) -> u64 {
        self.mods.iter().fold(0, |acc, m| acc | m.flag)
    }
}

/// Map a canonical modifier (`ctrl`, `alt`/Option, `shift`, `super`/Command).
///
/// # Errors
///
/// [`Error::Unsupported`] for unknown names.
pub fn modifier(name: &str) -> Result<Modifier> {
    match name {
        "ctrl" => Ok(Modifier {
            code: KVK_CONTROL,
            flag: FLAG_CONTROL,
        }),
        "alt" => Ok(Modifier {
            code: KVK_OPTION,
            flag: FLAG_OPTION,
        }),
        "shift" => Ok(Modifier {
            code: KVK_SHIFT,
            flag: FLAG_SHIFT,
        }),
        "super" => Ok(Modifier {
            code: KVK_COMMAND,
            flag: FLAG_COMMAND,
        }),
        other => Err(Error::Unsupported(format!("unknown modifier '{other}'"))),
    }
}

/// ANSI letter positions `a`..`z` (`kVK_ANSI_A`..).
const LETTERS: [u16; 26] = [
    0x00, 0x0B, 0x08, 0x02, 0x0E, 0x03, 0x05, 0x04, 0x22, 0x26, 0x28, 0x25, 0x2E, 0x2D, 0x1F, 0x23,
    0x0C, 0x0F, 0x01, 0x11, 0x20, 0x09, 0x0D, 0x07, 0x10, 0x06,
];
/// ANSI digits `0`..`9`.
const DIGITS: [u16; 10] = [0x1D, 0x12, 0x13, 0x14, 0x15, 0x17, 0x16, 0x1A, 0x1C, 0x19];
/// `F1`..`F20`.
const FKEYS: [u16; 20] = [
    0x7A, 0x78, 0x63, 0x76, 0x60, 0x61, 0x62, 0x64, 0x65, 0x6D, 0x67, 0x6F, 0x69, 0x6B, 0x71, 0x6A,
    0x40, 0x4F, 0x50, 0x5A,
];

/// Map a canonical key name ([`clipmonger_platform::chord`]) to a key code.
///
/// # Errors
///
/// [`Error::Unsupported`] for keys without a macOS code.
pub fn key_code(key: &str) -> Result<u16> {
    let bad = || Error::Unsupported(format!("unsupported key '{key}'"));
    if key.len() == 1 {
        let c = key.chars().next().ok_or_else(bad)?;
        if c.is_ascii_lowercase() {
            return Ok(LETTERS[usize::from(c as u8 - b'a')]);
        }
        return Err(bad());
    }
    if let Some(d) = key.strip_prefix("Digit") {
        let n: usize = d.parse().map_err(|_| bad())?;
        return DIGITS.get(n).copied().ok_or_else(bad);
    }
    if let Some(n) = key.strip_prefix('F') {
        if let Ok(n) = n.parse::<usize>() {
            return n
                .checked_sub(1)
                .and_then(|i| FKEYS.get(i))
                .copied()
                .ok_or_else(bad);
        }
    }
    Ok(match key {
        "Enter" => 0x24,
        "Tab" => 0x30,
        "Space" => 0x31,
        "Backspace" => 0x33,
        "Escape" => 0x35,
        "Delete" => 0x75,
        "Insert" => 0x72,
        "Home" => 0x73,
        "End" => 0x77,
        "PageUp" => 0x74,
        "PageDown" => 0x79,
        "ArrowLeft" => 0x7B,
        "ArrowRight" => 0x7C,
        "ArrowDown" => 0x7D,
        "ArrowUp" => 0x7E,
        "Equal" => 0x18,
        "Minus" => 0x1B,
        "BracketRight" => 0x1E,
        "BracketLeft" => 0x21,
        "Quote" => 0x27,
        "Semicolon" => 0x29,
        "Backslash" => 0x2A,
        "Comma" => 0x2B,
        "Slash" => 0x2C,
        "Period" => 0x2F,
        "Backquote" => 0x32,
        _ => return Err(bad()),
    })
}

/// Resolve a parsed chord.
///
/// # Errors
///
/// [`Error::Unsupported`] for unknown modifiers or keys.
pub fn resolve(chord: &Chord) -> Result<KeyChord> {
    Ok(KeyChord {
        mods: chord
            .mods
            .iter()
            .map(|m| modifier(m))
            .collect::<Result<_>>()?,
        key: key_code(&chord.key)?,
    })
}

/// Press the modifiers, then the key, release in reverse; every pressed key
/// is released even when a later event fails. `send(code, down, flags)`.
///
/// # Errors
///
/// The first failure of `send`.
pub fn press_sequence(
    chord: &KeyChord,
    mut send: impl FnMut(u16, bool, u64) -> Result<()>,
) -> Result<()> {
    let mut keys: Vec<(u16, u64)> = Vec::new();
    let mut flags = 0;
    for m in &chord.mods {
        flags |= m.flag;
        keys.push((m.code, flags));
    }
    keys.push((chord.key, flags));
    let mut pressed = 0;
    let mut outcome = Ok(());
    for &(code, flags) in &keys {
        match send(code, true, flags) {
            Ok(()) => pressed += 1,
            Err(e) => {
                outcome = Err(e);
                break;
            }
        }
    }
    // A release carries the flags still held after it.
    for i in (0..pressed).rev() {
        let held = if i == keys.len() - 1 {
            keys[i].1
        } else {
            i.checked_sub(1).map_or(0, |j| keys[j].1)
        };
        let released = send(keys[i].0, false, held);
        if outcome.is_ok() {
            outcome = released;
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use clipmonger_platform::chord::parse;

    #[test]
    fn chord_keycode_map() {
        let paste = resolve(&parse("cmd+v").unwrap()).unwrap();
        assert_eq!(paste.key, KVK_V);
        assert_eq!(paste.flags(), FLAG_COMMAND);
        let popup = resolve(&parse("ctrl+alt+h").unwrap()).unwrap();
        assert_eq!(popup.key, 0x04);
        assert_eq!(popup.flags(), FLAG_CONTROL | FLAG_OPTION);
        for (key, code) in [
            ("a", 0x00),
            ("z", 0x06),
            ("q", 0x0C),
            ("0", 0x1D),
            ("1", 0x12),
            ("9", 0x19),
            ("F1", 0x7A),
            ("F12", 0x6F),
            ("enter", 0x24),
            ("tab", 0x30),
            ("space", 0x31),
            ("arrowup", 0x7E),
            ("arrowleft", 0x7B),
        ] {
            let chord = parse(&format!("shift+{key}")).unwrap();
            assert_eq!(resolve(&chord).unwrap().key, code, "{key}");
        }
        assert!(key_code("F21").is_err());
        assert!(key_code("Digit").is_err());
    }

    #[test]
    fn sequence_releases_in_reverse_with_held_flags() {
        let chord = resolve(&parse("ctrl+shift+v").unwrap()).unwrap();
        let mut events = Vec::new();
        press_sequence(&chord, |code, down, flags| {
            events.push((code, down, flags));
            Ok(())
        })
        .unwrap();
        let cs = FLAG_CONTROL | FLAG_SHIFT;
        assert_eq!(
            events,
            [
                (KVK_CONTROL, true, FLAG_CONTROL),
                (KVK_SHIFT, true, cs),
                (KVK_V, true, cs),
                (KVK_V, false, cs),
                (KVK_SHIFT, false, FLAG_CONTROL),
                (KVK_CONTROL, false, 0),
            ]
        );
        let mut events = Vec::new();
        let failed = press_sequence(&chord, |code, down, flags| {
            events.push((code, down, flags));
            if down && code == KVK_V {
                Err(Error::Io("v".into()))
            } else {
                Ok(())
            }
        });
        assert!(failed.is_err());
        assert_eq!(
            events.iter().filter(|e| !e.1).count(),
            2,
            "both modifiers released"
        );
    }
}
