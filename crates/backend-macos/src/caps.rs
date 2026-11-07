//! Capability constructors (pure).

use clipmonger_platform::types::{BackendCaps, Level, PasteCaps, PasteStrategy};

/// Paste degradation reason without Accessibility.
pub const NEEDS_ACCESSIBILITY: &str =
    "needs Accessibility permission: System Settings > Privacy & Security > Accessibility";
/// Guidance when pasteboard privacy (macOS 15.4+) is not "Always allow".
pub const PASTEBOARD_GUIDANCE: &str =
    "set Clipmonger to \"Always allow\" in System Settings > Privacy & Security > Paste from Other Apps";

/// Pasteboard privacy state (`NSPasteboard.accessBehavior`, macOS 15.4+).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteboardAccess {
    /// Older OS, or "Always allow".
    Unrestricted,
    /// Reads may prompt the user ("Ask", also the never-prompted default).
    Ask,
    /// Reads are refused ("Always deny").
    Denied,
}

/// Full capabilities for a healthy macOS backend.
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
        tray: Level::Full,
        autostart: true,
        primary_selection: false,
        notes: vec![
            "image/png: PNG or TIFF decoded; writes PNG + TIFF".to_string(),
            "tray hosted by app-ui as a menu-bar extra".to_string(),
        ],
    }
}

/// Capabilities with runtime probes applied: `trusted` = Accessibility
/// granted (synthetic input), `access` = pasteboard privacy state.
#[must_use]
pub fn caps_for(trusted: bool, access: PasteboardAccess) -> BackendCaps {
    let mut caps = normal_caps();
    if !trusted {
        caps.paste = PasteCaps {
            level: Level::Degraded,
            strategy: PasteStrategy::ClipboardRestore,
            reason: NEEDS_ACCESSIBILITY.to_string(),
        };
        caps.notes.push(format!(
            "paste Degraded: {NEEDS_ACCESSIBILITY}; clips are left on the clipboard for Cmd+V"
        ));
    }
    match access {
        PasteboardAccess::Unrestricted => {}
        PasteboardAccess::Ask => {
            caps.monitor = Level::Degraded;
            caps.transfer = Level::Degraded;
            caps.notes.push(format!(
                "monitor Degraded: clipboard reads may prompt; {PASTEBOARD_GUIDANCE}"
            ));
        }
        PasteboardAccess::Denied => {
            caps.monitor = Level::Degraded;
            caps.transfer = Level::Degraded;
            caps.notes.push(format!(
                "monitor Degraded: clipboard access denied, nothing is captured; {PASTEBOARD_GUIDANCE}"
            ));
        }
    }
    caps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthy_is_full_without_primary() {
        let caps = caps_for(true, PasteboardAccess::Unrestricted);
        assert_eq!(caps, normal_caps());
        assert_eq!(caps.paste.level, Level::Full);
        assert_eq!(caps.paste.strategy, PasteStrategy::ClipboardRestore);
        assert!(!caps.primary_selection);
        assert!(caps.autostart);
    }

    #[test]
    fn untrusted_degrades_paste_only() {
        let caps = caps_for(false, PasteboardAccess::Unrestricted);
        assert_eq!(caps.paste.level, Level::Degraded);
        assert_eq!(caps.paste.reason, NEEDS_ACCESSIBILITY);
        assert_eq!(caps.monitor, Level::Full);
        assert_eq!(caps.hotkey, Level::Full);
    }

    #[test]
    fn pasteboard_privacy_degrades_monitor_and_transfer() {
        for access in [PasteboardAccess::Ask, PasteboardAccess::Denied] {
            let caps = caps_for(true, access);
            assert_eq!(caps.monitor, Level::Degraded);
            assert_eq!(caps.transfer, Level::Degraded);
            assert_eq!(caps.paste.level, Level::Full);
            assert!(caps.notes.iter().any(|n| n.contains("Always allow")));
        }
    }
}
