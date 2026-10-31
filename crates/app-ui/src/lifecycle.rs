//! App-level attach and reconnect state machine.

// First retry delay; doubles per attempt.
const BACKOFF_FIRST_MS: u64 = 250;
// Delay ceiling for all retries.
const BACKOFF_MAX_MS: u64 = 5000;

use std::time::Duration;

/// Daemon link state owned by the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LifeState {
    /// Never started.
    #[default]
    Detached,
    /// Spawning or finding the daemon process.
    EnsuringDaemon,
    /// Handshake in flight.
    Connecting,
    /// Link up.
    Attached,
    /// Link lost, backing off between retries.
    Reconnecting,
    /// Quit requested, stopping the daemon.
    Stopping,
    /// Terminal failure needing user action.
    Failed,
}

/// Event driving the state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifeEvent {
    /// App started.
    Start,
    /// Daemon process is present.
    DaemonEnsured,
    /// Handshake and version check passed.
    Connected,
    /// Link dropped.
    Disconnected,
    /// A backoff timer fired.
    RetryDue,
    /// Tray asked to stop the daemon.
    Stop,
    /// Tray asked to quit the app.
    Quit,
}

/// Command the shell must run for a transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifeCmd {
    /// Spawn or locate the daemon process.
    EnsureDaemon,
    /// Open the IPC link and run the handshake.
    Connect,
    /// Fetch fresh status and list after attach.
    FetchState,
    /// Sleep before the next retry.
    WaitBackoff(Duration),
    /// Send `daemon.stop`.
    StopDaemon,
    /// Exit the app.
    Exit,
}

/// Pure attach and reconnect transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Lifecycle {
    state: LifeState,
}

impl Lifecycle {
    /// Build a detached lifecycle.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Read the current state.
    #[must_use]
    pub fn state(&self) -> LifeState {
        self.state
    }

    /// Apply one event and report the commands to run.
    pub fn on(&mut self, event: LifeEvent) -> Vec<LifeCmd> {
        match (self.state, event) {
            (LifeState::Detached, LifeEvent::Start) => {
                self.state = LifeState::EnsuringDaemon;
                vec![LifeCmd::EnsureDaemon]
            }
            (LifeState::EnsuringDaemon, LifeEvent::DaemonEnsured)
            | (LifeState::Reconnecting, LifeEvent::RetryDue) => {
                self.state = LifeState::Connecting;
                vec![LifeCmd::Connect]
            }
            (LifeState::Connecting | LifeState::Reconnecting, LifeEvent::Connected) => {
                self.state = LifeState::Attached;
                vec![LifeCmd::FetchState]
            }
            (LifeState::Attached | LifeState::Connecting, LifeEvent::Disconnected) => {
                self.state = LifeState::Reconnecting;
                vec![LifeCmd::WaitBackoff(next_backoff(0))]
            }
            (_, LifeEvent::Stop) => {
                self.state = LifeState::Stopping;
                vec![LifeCmd::StopDaemon]
            }
            (_, LifeEvent::Quit) => {
                self.state = LifeState::Stopping;
                vec![LifeCmd::Exit]
            }
            _ => Vec::new(),
        }
    }
}

/// Backoff before retry `attempt`: 250ms doubled, capped at 5s.
#[must_use]
pub fn next_backoff(attempt: u32) -> Duration {
    let doubled = BACKOFF_FIRST_MS.saturating_mul(1_u64 << attempt.min(8));
    Duration::from_millis(doubled.min(BACKOFF_MAX_MS))
}

/// How the shell starts, given whether a daemon link came up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartMode {
    /// Daemon linked: normal attached start.
    Attached,
    /// No daemon: the window still opens, offline (seg-09 S2/S12).
    Offline,
}

/// Start mode for a connect outcome. No daemon is never a reason to exit
/// before the window opens.
#[must_use]
pub fn start_mode(connected: bool) -> StartMode {
    if connected {
        StartMode::Attached
    } else {
        StartMode::Offline
    }
}

/// Outcome of claiming the one-UI-per-profile slot.
#[derive(Debug)]
pub enum InstanceClaim {
    /// This is the UI; the lock lives as long as the file (`None` when the
    /// lock file cannot be used, which leaves no protection, not no UI).
    Primary(Option<std::fs::File>),
    /// Another UI holds the profile.
    Secondary,
}

/// Claim the profile's UI slot: an exclusive lock on `<profile>/ui.lock`.
#[must_use]
pub fn claim_single_instance(profile: &std::path::Path) -> InstanceClaim {
    let _ = std::fs::create_dir_all(profile);
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(profile.join("ui.lock"))
    else {
        return InstanceClaim::Primary(None);
    };
    match file.try_lock() {
        Ok(()) => InstanceClaim::Primary(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => InstanceClaim::Secondary,
        Err(std::fs::TryLockError::Error(_)) => InstanceClaim::Primary(None),
    }
}

#[cfg(test)]
mod tests {
    use super::{start_mode, StartMode};

    #[test]
    fn no_daemon_still_opens_window_offline() {
        assert_eq!(start_mode(false), StartMode::Offline);
        assert_eq!(start_mode(true), StartMode::Attached);
    }
}
