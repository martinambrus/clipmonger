//! Autostart over `HKCU\...\Run` plus the `StartupApproved` override.
//!
//! Portable mode skips autostart entirely (no registry writes, `is_enabled`
//! is always false). The elevated helper is never registered here.

use std::path::PathBuf;

use clipmonger_platform::traits::AutostartManager;
use clipmonger_platform::types::{Affinity, Error, Result};

/// Windows autostart manager.
pub struct WinAutostart {
    #[cfg_attr(not(windows), allow(dead_code))]
    exe: PathBuf,
    portable: bool,
}

impl WinAutostart {
    /// Create for `exe` (daemon binary); `portable` disables all registry I/O.
    #[must_use]
    pub fn new(exe: PathBuf, portable: bool) -> Self {
        Self { exe, portable }
    }
}

#[cfg(not(windows))]
fn unavailable() -> Error {
    Error::Unavailable("autostart needs a Windows session".to_string())
}

/// Whether the suite is effectively enabled: Run value present and not
/// disabled by the `StartupApproved` override (VERIFY on native Windows).
#[cfg(windows)]
fn suite_enabled() -> Result<bool> {
    let present = crate::sys::run_value_get()
        .map_err(|e| Error::Io(format!("autostart query failed: {e}")))?
        .is_some();
    if !present {
        return Ok(false);
    }
    if let Some(blob) = crate::sys::startup_approved_blob() {
        if let Some(enabled) = crate::autostart_logic::startup_approved_enabled(&blob) {
            return Ok(enabled);
        }
    }
    Ok(true)
}

impl AutostartManager for WinAutostart {
    const AFFINITY: Affinity = Affinity::Worker;

    fn enable(&mut self) -> Result<()> {
        if crate::autostart_logic::should_skip_autostart(self.portable) {
            return Ok(());
        }
        #[cfg(windows)]
        {
            let command = crate::autostart_logic::run_value(&self.exe, true);
            crate::sys::run_value_set(&command)
                .map_err(|e| Error::Io(format!("autostart enable failed: {e}")))
        }
        #[cfg(not(windows))]
        {
            Err(unavailable())
        }
    }

    fn disable(&mut self) -> Result<()> {
        if crate::autostart_logic::should_skip_autostart(self.portable) {
            return Ok(());
        }
        #[cfg(windows)]
        {
            crate::sys::run_value_delete()
                .map_err(|e| Error::Io(format!("autostart disable failed: {e}")))
        }
        #[cfg(not(windows))]
        {
            Err(unavailable())
        }
    }

    fn is_enabled(&self) -> Result<bool> {
        if crate::autostart_logic::should_skip_autostart(self.portable) {
            return Ok(false);
        }
        #[cfg(windows)]
        {
            suite_enabled()
        }
        #[cfg(not(windows))]
        {
            Err(unavailable())
        }
    }
}
