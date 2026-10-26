//! Wayland v1 paste: clipboard-write + toast fallback (seg-06 S7).
//!
//! Real injection (virtual-keyboard, `RemoteDesktop`, libei, extension,
//! helper) is spike-documented and NOT a v1 code path: with a data-control
//! write path the daemon writes the payload and the UI toasts
//! `paste.press_ctrl_v`; without one there is no paste path
//! ([`Error::Unavailable`]). The trait `paste_to` stays `Unsupported`,
//! mirroring the sibling backends (the trait carries only `ClipRef`, never
//! payload bytes).

use clipmonger_platform::traits::{ClipboardTransfer as _, PasteEngine};
use clipmonger_platform::types::{
    Affinity, ClipRef, Error, PasteStrategy, PasteTarget, PayloadMap, Result,
};

use crate::paste_logic::PRESS_CTRL_V_TOAST_ID;
use crate::transfer::WlTransfer;

/// v1 paste outcome: write + toast, never `Full`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PasteOutcome {
    /// Delivery strategy used.
    pub strategy: PasteStrategy,
    /// Toast string id rendered after a fallback write.
    pub toast: Option<&'static str>,
}

/// Wayland paste engine (v1 fallback only).
pub struct WlPaste {
    write_available: bool,
}

impl WlPaste {
    /// Create the fallback engine: `has_data_control` mirrors
    /// [`crate::probe::has_data_control`] at backend start.
    #[must_use]
    pub fn new(has_data_control: bool) -> Self {
        Self {
            write_available: has_data_control,
        }
    }

    /// Write stored `payloads` to the clipboard for manual Ctrl+V.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unavailable`] without a data-control write path
    /// (WSLg-style sessions) or when the write itself fails.
    pub fn paste_payloads(&self, payloads: &PayloadMap) -> Result<PasteOutcome> {
        if !self.write_available {
            return Err(Error::Unavailable(
                "wayland paste unavailable: no data-control write path".to_string(),
            ));
        }
        let transfer = WlTransfer::connect()?;
        transfer.write(payloads)?;
        Ok(PasteOutcome {
            strategy: PasteStrategy::WriteAndToast,
            toast: Some(PRESS_CTRL_V_TOAST_ID),
        })
    }
}

impl PasteEngine for WlPaste {
    const AFFINITY: Affinity = Affinity::Worker;

    fn strategy(&self) -> PasteStrategy {
        if self.write_available {
            PasteStrategy::WriteAndToast
        } else {
            PasteStrategy::Unavailable
        }
    }

    fn capture_target(&self) -> Result<PasteTarget> {
        Err(Error::Unsupported(
            "wayland v1 has no foreground target: paste skips refocus".to_string(),
        ))
    }

    fn paste_to(&self, _target: &PasteTarget, _clip: &ClipRef) -> Result<()> {
        Err(Error::Unsupported(
            "wayland v1 paste needs stored payloads via paste_payloads".to_string(),
        ))
    }
}
