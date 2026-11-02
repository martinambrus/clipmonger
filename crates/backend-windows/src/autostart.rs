//! Autostart over `HKCU\...\Run` plus the `StartupApproved` override.
//!
//! Portable mode skips autostart entirely (no registry writes, `is_enabled`
//! is always false). The elevated helper is never registered here.

use std::path::{Path, PathBuf};

use clipmonger_platform::traits::AutostartManager;
use clipmonger_platform::types::{Affinity, Error, Result};

/// Windows autostart manager.
pub struct WinAutostart {
    #[cfg_attr(not(windows), allow(dead_code))]
    exe: PathBuf,
    #[cfg_attr(not(windows), allow(dead_code))]
    profile_dir: Option<PathBuf>,
    portable: bool,
}

impl WinAutostart {
    /// Create for `exe` (daemon binary); `portable` disables all registry I/O.
    #[must_use]
    pub fn new(exe: PathBuf, portable: bool) -> Self {
        Self {
            exe,
            profile_dir: None,
            portable,
        }
    }

    /// Create a profile-aware manager for the sibling UI `clipmonger[.exe]`
    /// and an absolute `profile_dir`. Validates both paths fallibly (rejects
    /// NUL, quote, control characters) without touching the registry.
    ///
    /// # Errors
    ///
    /// Returns `Error::Unavailable` when either path is unrepresentable as a Windows
    /// command-line argument.
    pub fn with_profile(ui_exe: PathBuf, profile_dir: PathBuf, portable: bool) -> Result<Self> {
        crate::autostart_logic::run_value_with_profile(&ui_exe, &profile_dir, true)?;
        Ok(Self {
            exe: ui_exe,
            profile_dir: Some(profile_dir),
            portable,
        })
    }

    /// Fallible variant accepting borrowed paths for testing parity.
    ///
    /// # Errors
    ///
    /// Returns `Error::Unavailable` if either path is not a valid Windows
    /// autostart command argument.
    pub fn try_with_profile(ui_exe: &Path, profile_dir: &Path, portable: bool) -> Result<Self> {
        crate::autostart_logic::run_value_with_profile(ui_exe, profile_dir, true)?;
        Ok(Self {
            exe: ui_exe.to_path_buf(),
            profile_dir: Some(profile_dir.to_path_buf()),
            portable,
        })
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
            let command = if let Some(profile) = &self.profile_dir {
                crate::autostart_logic::run_value_with_profile(&self.exe, profile, true)?
            } else {
                crate::autostart_logic::run_value(&self.exe, true)
            };
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn legacy_constructor_retained() {
        let m = WinAutostart::new(Path::new("/usr/bin/clipmonger-daemon").to_path_buf(), false);
        assert!(m.profile_dir.is_none());
    }

    #[test]
    fn profile_constructor_validates() {
        let ok = WinAutostart::with_profile(
            Path::new("C:\\clipmonger.exe").to_path_buf(),
            Path::new("C:\\profiles\\work").to_path_buf(),
            false,
        );
        assert!(ok.is_ok());
        let bad = WinAutostart::with_profile(
            Path::new("C:\\bad\"exe.exe").to_path_buf(),
            Path::new("C:\\profiles\\work").to_path_buf(),
            false,
        );
        assert!(bad.is_err());
        let bad_profile = WinAutostart::with_profile(
            Path::new("C:\\clipmonger.exe").to_path_buf(),
            Path::new("C:\\bad\x01profile").to_path_buf(),
            false,
        );
        assert!(bad_profile.is_err());
    }

    #[test]
    fn portable_skips_enable_disable_query() {
        let mut m = WinAutostart::new(Path::new("C:\\clipmonger.exe").to_path_buf(), true);
        assert!(m.enable().is_ok());
        assert!(m.disable().is_ok());
        assert!(!m.is_enabled().unwrap());
        let mut mp = WinAutostart::with_profile(
            Path::new("C:\\clipmonger.exe").to_path_buf(),
            Path::new("C:\\profiles\\work").to_path_buf(),
            true,
        )
        .unwrap();
        assert!(mp.enable().is_ok());
        assert!(mp.disable().is_ok());
        assert!(!mp.is_enabled().unwrap());
    }

    #[test]
    fn no_malformed_registration_on_invalid_profile() {
        let bad = WinAutostart::with_profile(
            Path::new("C:\\clipmonger.exe").to_path_buf(),
            Path::new("C:\\bad\"path").to_path_buf(),
            false,
        );
        assert!(
            bad.is_err(),
            "should not create manager with malformed profile"
        );
    }
}
