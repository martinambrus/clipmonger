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

/// The icon of one state at every generated size, as `(edge, straight RGBA)`
/// (`scripts/make-icons.py`), smallest first.
macro_rules! tray_icon_sizes {
    ($state:literal) => {
        [
            tray_icon_sizes!(@one $state, 16, "16"),
            tray_icon_sizes!(@one $state, 20, "20"),
            tray_icon_sizes!(@one $state, 22, "22"),
            tray_icon_sizes!(@one $state, 24, "24"),
            tray_icon_sizes!(@one $state, 32, "32"),
            tray_icon_sizes!(@one $state, 48, "48"),
        ]
    };
    (@one $state:literal, $edge:literal, $name:literal) => {
        (
            $edge,
            include_bytes!(concat!("../../../assets/icons/tray/", $state, "-", $name, ".rgba")).as_slice(),
        )
    };
}

/// Square RGBA tray images of one state as `(edge, bytes)`.
pub type TrayIconSizes = [(u32, &'static [u8]); 6];

const NORMAL_ICONS: TrayIconSizes = tray_icon_sizes!("normal");
const PAUSED_ICONS: TrayIconSizes = tray_icon_sizes!("paused");
const DEGRADED_ICONS: TrayIconSizes = tray_icon_sizes!("degraded");

/// The state the tray icon shows for `inputs`: a problem wins over paused.
#[must_use]
pub fn icon_state(inputs: &TrayInputs) -> TrayUiState {
    if !inputs.attached || inputs.degraded {
        TrayUiState::Attention
    } else if inputs.paused {
        TrayUiState::Paused
    } else {
        TrayUiState::Normal
    }
}

/// Every generated size of the tray icon for `state`.
#[must_use]
pub fn icon_sizes(state: TrayUiState) -> &'static TrayIconSizes {
    match state {
        TrayUiState::Normal => &NORMAL_ICONS,
        TrayUiState::Paused => &PAUSED_ICONS,
        TrayUiState::Attention => &DEGRADED_ICONS,
    }
}

/// The smallest tray image of `state` at least `wanted` pixels wide, or the
/// largest one, as `(edge, straight RGBA)`.
#[must_use]
pub fn icon_rgba(state: TrayUiState, wanted: u32) -> (u32, &'static [u8]) {
    let sizes = icon_sizes(state);
    sizes
        .iter()
        .find(|(edge, _)| *edge >= wanted)
        .copied()
        .unwrap_or(sizes[sizes.len() - 1])
}

/// Action the shell must run for one tray event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayAction {
    /// Open the popup.
    OpenPopup,
    /// Left click on the tray icon: hide the popup when it is open, else open it.
    Toggle,
    /// Native button-down: toggle using the press time for the focus-loss guard.
    ToggleAt(std::time::Instant),
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

/// Left-button events delivered by the native tray.
#[derive(Debug, Clone, Copy)]
pub enum LeftButtonEvent {
    /// Button pressed.
    Down,
    /// Button released.
    Up,
    /// The second press of a double-click.
    DoubleClick,
}

/// Translate one native left-button event at an injected monotonic time.
#[must_use]
pub fn left_button_action(event: LeftButtonEvent, at: std::time::Instant) -> Option<TrayAction> {
    // Windows delivers Down, Up, DoubleClick, Up for a double-click.
    // Act on the first press only; releases cannot undo a focus-loss hide.
    matches!(event, LeftButtonEvent::Down).then_some(TrayAction::ToggleAt(at))
}
