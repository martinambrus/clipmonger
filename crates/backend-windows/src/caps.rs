//! Capability constructors (pure).

use clipmonger_platform::types::{BackendCaps, Level, PasteCaps, PasteStrategy};

/// Full capabilities for a healthy Windows backend.
///
/// Note: `image/png` reads the "PNG" format or decodes `CF_DIBV5`/`CF_DIB`
/// to PNG; writes publish "PNG" plus a `CF_DIB`. Every constructor here
/// carries the note so daemon-registered snapshots report it too.
#[must_use]
pub fn normal_caps() -> BackendCaps {
    BackendCaps {
        monitor: Level::Full,
        transfer: Level::Full,
        hotkey: Level::Full,
        paste: PasteCaps {
            level: Level::Full,
            strategy: PasteStrategy::UnicodeInject,
            reason: String::new(),
        },
        tray: Level::Full,
        autostart: true,
        primary_selection: false,
        notes: vec![
            "image/png: PNG format or CF_DIBV5/CF_DIB decoded; writes PNG + CF_DIB".to_string(),
        ],
    }
}

/// Capabilities with hotkey-conflict / elevation-refused degradation applied.
#[must_use]
pub fn caps_for(hotkey_conflict: bool, elevation_refused: bool) -> BackendCaps {
    let mut caps = normal_caps();
    if hotkey_conflict {
        caps.hotkey = Level::Degraded;
        caps.notes
            .push("hotkey Degraded: conflicting registration, kept running".to_string());
    }
    if elevation_refused {
        caps.paste = PasteCaps {
            level: Level::Degraded,
            strategy: PasteStrategy::ElevatedRoute,
            reason: "elevation refused".to_string(),
        };
        caps.notes
            .push("paste Degraded: elevation refused, retry available".to_string());
    }
    caps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_is_full() {
        let caps = normal_caps();
        assert_eq!(caps.monitor, Level::Full);
        assert_eq!(caps.transfer, Level::Full);
        assert_eq!(caps.hotkey, Level::Full);
        assert_eq!(caps.paste.level, Level::Full);
        assert_eq!(caps.tray, Level::Full);
        assert!(caps.autostart);
        assert!(!caps.primary_selection);
    }

    #[test]
    fn clean_matches_normal() {
        assert_eq!(caps_for(false, false), normal_caps());
    }

    #[test]
    fn hotkey_conflict_degrades() {
        let caps = caps_for(true, false);
        assert_eq!(caps.hotkey, Level::Degraded);
        assert_eq!(caps.monitor, Level::Full);
        assert_ne!(caps.notes, Vec::<String>::new());
    }

    #[test]
    fn elevation_refused_degrades_paste() {
        let caps = caps_for(false, true);
        assert_eq!(caps.paste.level, Level::Degraded);
        assert_eq!(caps.paste.strategy, PasteStrategy::ElevatedRoute);
        assert_ne!(caps.notes, Vec::<String>::new());
    }

    #[test]
    fn both_degrade() {
        let caps = caps_for(true, true);
        assert_eq!(caps.hotkey, Level::Degraded);
        assert_eq!(caps.paste.level, Level::Degraded);
        assert!(caps.notes.len() >= 2);
    }
}
