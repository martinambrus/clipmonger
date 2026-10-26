//! Popup target session state machine.

/// Target session for one popup opening.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetSession {
    /// No popup session.
    Idle,
    /// Popup shown, target not yet captured.
    AwaitingTarget,
    /// Target captured and ready to paste into.
    Ready {
        /// Captured target identifier.
        target: String,
    },
    /// Focus moved; paste must be refused.
    Stale,
}

/// Paste refusal reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteError {
    /// No ready target.
    NotReady,
    /// Target went stale.
    Stale,
}

impl TargetSession {
    /// New idle session.
    #[must_use]
    pub fn new() -> Self {
        Self::Idle
    }
    /// Show the popup; a fresh show recovers from stale/idle.
    pub fn show(&mut self) {
        *self = Self::AwaitingTarget;
    }
    /// Capture the paste target.
    pub fn capture_target(&mut self, target: String) {
        if !matches!(self, Self::Stale) {
            *self = Self::Ready { target };
        }
    }
    /// Focus lost: a ready/awaiting session goes stale.
    pub fn focus_lost(&mut self) {
        if matches!(self, Self::Ready { .. } | Self::AwaitingTarget) {
            *self = Self::Stale;
        }
    }
    /// Dismiss the popup.
    pub fn dismiss(&mut self) {
        *self = Self::Idle;
    }
    /// Paste once, consuming a ready session back to idle.
    ///
    /// # Errors
    /// Returns [`PasteError::NotReady`] without a ready target, or
    /// [`PasteError::Stale`] for a stale target (never pastes).
    pub fn paste(&mut self) -> Result<String, PasteError> {
        match std::mem::replace(self, Self::Idle) {
            Self::Ready { target } => Ok(target),
            Self::Stale => {
                *self = Self::Stale;
                Err(PasteError::Stale)
            }
            other => {
                *self = other;
                Err(PasteError::NotReady)
            }
        }
    }
}

impl Default for TargetSession {
    fn default() -> Self {
        Self::new()
    }
}
