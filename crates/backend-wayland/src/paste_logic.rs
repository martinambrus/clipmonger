//! Paste fallback truth table (pure; seg-06 S7, T-608, W-F7).
//!
//! v1 ships the fallback only: clipboard write + toast (`press_ctrl_v`).
//! Real injection (virtual-keyboard / `RemoteDesktop` / libei / extension /
//! helper) is spike-documented and NOT a v1 code path.

/// Toast string id rendered after a fallback write (W-F3 names the id;
///
/// the app-ui entry itself lands with the strings change).
pub const PRESS_CTRL_V_TOAST_ID: &str = "paste.press_ctrl_v";

/// v1 paste outcome for one backend state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteFallback {
    /// Daemon writes + toast; never `Full`.
    WriteAndToast,
    /// No write path: manual-copy action instead.
    Unavailable,
}

/// Fallback for `(data_write, extension)`.
///
/// Data-control write works → write + toast regardless of extension;
/// GNOME extension up → extension-assisted write + toast; neither → no
/// paste path. The focused-UI data-device row stays `Unavailable` until a
/// focused path is explicitly implemented and proven.
#[must_use]
pub fn fallback_for(data_write: bool, extension: bool) -> PasteFallback {
    if data_write || extension {
        PasteFallback::WriteAndToast
    } else {
        PasteFallback::Unavailable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t608_truth_table() {
        assert_eq!(fallback_for(true, true), PasteFallback::WriteAndToast);
        assert_eq!(fallback_for(true, false), PasteFallback::WriteAndToast);
        assert_eq!(fallback_for(false, true), PasteFallback::WriteAndToast);
        assert_eq!(fallback_for(false, false), PasteFallback::Unavailable);
    }

    #[test]
    fn toast_id_is_stable() {
        assert_eq!(PRESS_CTRL_V_TOAST_ID, "paste.press_ctrl_v");
    }
}
