//! Tray state logic over the platform tray controller.

use clipmonger_platform::traits::TrayController;
use clipmonger_platform::types::{TrayEvent, TrayState};

use crate::strings;

/// Local tray presentation state.
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
    /// Show the About view.
    About,
    /// Stop the daemon and exit.
    Quit,
}

/// One tray menu entry: a labelled item or a separator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuEntry {
    /// Item carrying a strings-module id.
    Item(&'static str),
    /// Visual separator.
    Separator,
}

impl Default for TrayInputs {
    fn default() -> Self {
        Self {
            paused: false,
            degraded: false,
            attached: true,
        }
    }
}

/// Menu entries for the current inputs (seg-09 S7 sole definition).
#[must_use]
pub fn menu_entries(inputs: &TrayInputs) -> Vec<MenuEntry> {
    let pause = if inputs.paused {
        "tray.resume"
    } else {
        "tray.pause"
    };
    vec![
        MenuEntry::Item("tray.show"),
        MenuEntry::Separator,
        MenuEntry::Item(pause),
        MenuEntry::Item("tray.pause_1h"),
        MenuEntry::Separator,
        MenuEntry::Item("tray.config"),
        MenuEntry::Separator,
        MenuEntry::Item("tray.about"),
        MenuEntry::Item("tray.quit"),
    ]
}

/// Inputs driving one tray sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    menu_entries(inputs)
        .iter()
        .filter_map(|entry| match entry {
            MenuEntry::Item(id) => Some(strings::text(id).to_string()),
            MenuEntry::Separator => None,
        })
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
            TrayUiState::Normal => TrayState::Idle,
            TrayUiState::Paused => TrayState::Paused,
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
                    } else if label == strings::text("tray.about") {
                        TrayAction::About
                    } else {
                        TrayAction::Quit
                    }
                }
            })
            .collect()
    }
}
