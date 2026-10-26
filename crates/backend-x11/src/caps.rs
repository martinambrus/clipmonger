//! Capability constructors (pure).

use clipmonger_platform::types::{BackendCaps, Level, PasteCaps, PasteStrategy};

/// Full capabilities for a healthy X11 backend.
///
/// Primary selection is always supported (capture policy is config, not
/// capability); paste is clipboard-restore only (X11 has no Unicode
/// injection); tray is owned by app-ui via KSNI, so the backend reports
/// it unavailable here.
#[must_use]
pub fn normal_caps() -> BackendCaps {
    BackendCaps {
        monitor: Level::Full,
        transfer: Level::Full,
        hotkey: Level::Full,
        paste: PasteCaps {
            level: Level::Full,
            strategy: PasteStrategy::ClipboardRestore,
            reason: String::new(),
        },
        tray: Level::Unavailable,
        autostart: true,
        primary_selection: true,
        notes: vec!["tray hosted by app-ui via KSNI".to_string()],
    }
}

/// Capabilities with runtime probes applied: `xtest=false` degrades paste
/// (no synthetic input), `tray_available=true` lifts tray to full.
#[must_use]
pub fn caps_for(xtest: bool, tray_available: bool) -> BackendCaps {
    let mut caps = normal_caps();
    if !xtest {
        caps.paste = PasteCaps {
            level: Level::Degraded,
            strategy: PasteStrategy::ClipboardRestore,
            reason: "XTest missing: no synthetic input".to_string(),
        };
        caps.notes
            .push("paste Degraded: XTest missing, no synthetic input".to_string());
    }
    if tray_available {
        caps.tray = Level::Full;
    }
    caps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_reports_primary_and_restore() {
        let caps = normal_caps();
        assert_eq!(caps.monitor, Level::Full);
        assert_eq!(caps.transfer, Level::Full);
        assert!(caps.primary_selection);
        assert_eq!(caps.paste.strategy, PasteStrategy::ClipboardRestore);
        assert_eq!(caps.paste.level, Level::Full);
        assert!(caps.autostart);
    }

    #[test]
    fn clean_matches_normal() {
        assert_eq!(caps_for(true, false), normal_caps());
    }

    #[test]
    fn missing_xtest_degrades_paste() {
        let caps = caps_for(false, false);
        assert_eq!(caps.paste.level, Level::Degraded);
        assert_eq!(caps.paste.strategy, PasteStrategy::ClipboardRestore);
        assert_ne!(caps.paste.reason, "");
        assert_eq!(caps.monitor, Level::Full);
    }

    #[test]
    fn tray_host_lifts_tray() {
        assert_eq!(caps_for(true, true).tray, Level::Full);
        assert_eq!(caps_for(true, false).tray, Level::Unavailable);
    }
}
