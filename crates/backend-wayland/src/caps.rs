//! Capability constructors (pure; seg-06 S9).

use clipmonger_platform::types::{BackendCaps, Level, PasteCaps, PasteStrategy};

use crate::paste_logic::{fallback_for, PasteFallback, PRESS_CTRL_V_TOAST_ID};
use crate::probe::{has_data_control, ProbeState};

/// Capabilities for a probed Wayland session.
///
/// `primary` is the capture-primary config flag (combined with the probe's
/// `primary_supported`); `portal` reports `GlobalShortcuts` availability.
/// Paste is the v1 fallback only (W-F7): `Degraded(WriteAndToast)` where a
/// write path exists, else `Unavailable`. Tray is hosted by app-ui, so the
/// backend reports it unavailable here like `backend-x11`.
#[must_use]
pub fn caps_for(probe: &ProbeState, primary: bool, portal: bool) -> BackendCaps {
    let data = has_data_control(probe);
    let monitor = if data {
        Level::Full
    } else {
        Level::Unavailable
    };
    let transfer = if data {
        Level::Full
    } else {
        Level::Unavailable
    };
    let paste = match fallback_for(data, false) {
        PasteFallback::WriteAndToast => PasteCaps {
            level: Level::Degraded,
            strategy: PasteStrategy::WriteAndToast,
            reason: format!("toast:{PRESS_CTRL_V_TOAST_ID}"),
        },
        PasteFallback::Unavailable => PasteCaps {
            level: Level::Unavailable,
            strategy: PasteStrategy::Unavailable,
            reason: "no data-control and no extension write path".to_string(),
        },
    };
    let mut notes = Vec::new();
    if data {
        notes.push(format!("protocol:{:?}", probe.protocol));
        if probe.primary_supported {
            notes.push("primary-selection available".to_string());
        }
    } else {
        notes.push("no data-control: capture unavailable".to_string());
    }
    if portal {
        notes.push("hotkey via GlobalShortcuts portal".to_string());
    } else {
        notes.push("hotkey Unavailable: use compositor-native recipes".to_string());
    }
    BackendCaps {
        monitor,
        transfer,
        hotkey: if portal {
            Level::Full
        } else {
            Level::Unavailable
        },
        paste,
        tray: Level::Unavailable,
        autostart: true,
        primary_selection: primary && probe.primary_supported,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::{probe_step, Globals, ProbeState};

    fn probed(ext: Option<u32>, wlr: Option<u32>, primary_ext: bool) -> ProbeState {
        probe_step(
            &ProbeState::default(),
            &Globals {
                ext,
                wlr,
                primary_ext,
            },
        )
    }

    #[test]
    fn data_control_present_is_full_monitor_degraded_paste() {
        let caps = caps_for(&probed(Some(1), None, true), true, true);
        assert_eq!(caps.monitor, Level::Full);
        assert_eq!(caps.transfer, Level::Full);
        assert_eq!(caps.hotkey, Level::Full);
        assert_eq!(caps.paste.level, Level::Degraded);
        assert_eq!(caps.paste.strategy, PasteStrategy::WriteAndToast);
        assert!(caps.primary_selection);
        assert!(caps.autostart);
    }

    #[test]
    fn no_protocol_degrades_to_unavailable() {
        let caps = caps_for(&probed(None, None, false), true, false);
        assert_eq!(caps.monitor, Level::Unavailable);
        assert_eq!(caps.transfer, Level::Unavailable);
        assert_eq!(caps.hotkey, Level::Unavailable);
        assert_eq!(caps.paste.level, Level::Unavailable);
        assert_eq!(caps.paste.strategy, PasteStrategy::Unavailable);
        assert!(!caps.primary_selection);
    }

    #[test]
    fn primary_flag_requires_probe_support() {
        // Config on, probe off -> no primary.
        let caps = caps_for(&probed(None, Some(1), false), true, false);
        assert!(!caps.primary_selection);
        // Config off, probe on -> no primary.
        let caps = caps_for(&probed(Some(1), None, true), false, false);
        assert!(!caps.primary_selection);
    }
}
