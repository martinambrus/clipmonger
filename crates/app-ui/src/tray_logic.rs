//! Tray state logic over the platform tray controller.

use clipmonger_platform::traits::TrayController;
use clipmonger_platform::types::{TrayEvent, TrayState};

use crate::strings;

/// Local tray presentation state (platform enum stays two-variant).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrayUiState {
    /// Monitoring running, link healthy.
    #[default]
    Normal,
    /// Monitoring paused by the user.
    Paused,
    /// Daemon needs user attention.
    Attention,
}

/// Action the shell must run for one tray event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayAction {
    /// Open the popup.
    OpenPopup,
    /// Flip the monitoring-paused flag.
    TogglePause,
    /// Pause monitoring for one hour.
    PauseFor1h,
    /// Open the settings window.
    OpenConfig,
    /// Stop the daemon and exit.
    Quit,
}

/// Inputs driving one tray sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TrayInputs {
    /// Monitoring paused flag.
    pub paused: bool,
    /// Caps degraded flag.
    pub degraded: bool,
    /// Daemon link up flag.
    pub attached: bool,
}

/// Menu labels for the current inputs, all from the string catalog.
#[must_use]
pub fn menu_model(inputs: &TrayInputs) -> Vec<String> {
    let pause = if inputs.paused {
        strings::text("tray.resume")
    } else {
        strings::text("tray.pause")
    };
    [
        strings::text("tray.show"),
        pause,
        strings::text("tray.pause_1h"),
        strings::text("tray.config"),
        strings::text("tray.about"),
        strings::text("tray.quit"),
    ]
    .iter()
    .map(ToString::to_string)
    .collect()
}

/// Pure tray mapping driven by tests and the shell.
#[derive(Debug, Default)]
pub struct TrayLogic {
    ui: TrayUiState,
}

impl TrayLogic {
    /// Build tray logic starting in the normal state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Read the presentation state.
    #[must_use]
    pub fn state(&self) -> TrayUiState {
        self.ui
    }

    /// Push state, tooltip and menu into the controller.
    pub fn sync<T: TrayController>(&mut self, tray: &mut T, inputs: &TrayInputs) {
        self.ui = if !inputs.attached || inputs.degraded {
            TrayUiState::Attention
        } else if inputs.paused {
            TrayUiState::Paused
        } else {
            TrayUiState::Normal
        };
        tray.set_state(match self.ui {
            TrayUiState::Normal | TrayUiState::Paused => TrayState::Idle,
            TrayUiState::Attention => TrayState::Attention,
        });
        tray.set_tooltip(if inputs.paused {
            strings::text("tray.paused_tip")
        } else {
            strings::text("tray.show")
        });
        tray.set_menu(&menu_model(inputs));
    }

    /// Drain controller events into shell actions.
    pub fn on_events<T: TrayController>(&mut self, tray: &mut T) -> Vec<TrayAction> {
        tray.poll_events()
            .iter()
            .map(|event| match event {
                TrayEvent::Activated => TrayAction::OpenPopup,
                TrayEvent::MenuItem(label) => {
                    if label == strings::text("tray.show") {
                        TrayAction::OpenPopup
                    } else if label == strings::text("tray.pause")
                        || label == strings::text("tray.resume")
                    {
                        TrayAction::TogglePause
                    } else if label == strings::text("tray.pause_1h") {
                        TrayAction::PauseFor1h
                    } else if label == strings::text("tray.config") {
                        TrayAction::OpenConfig
                    } else {
                        TrayAction::Quit
                    }
                }
            })
            .collect()
    }
}
