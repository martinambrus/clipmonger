//! X11 autostart over `~/.config/autostart/clipmonger.desktop`
//! (rendered/parsed by the pure `desktop` module; never shells out).

use std::path::PathBuf;

use clipmonger_platform::traits::AutostartManager;
use clipmonger_platform::types::{Affinity, Error, Result};

/// Desktop-entry autostart manager.
pub struct X11Autostart {
    exe: PathBuf,
    file: PathBuf,
    portable: bool,
}

impl X11Autostart {
    /// Manage the entry for `exe` under `root/autostart/`; `portable`
    /// disables everything (no-op, like the Windows portable guard).
    #[must_use]
    pub fn new(exe: PathBuf, mut root: PathBuf, portable: bool) -> Self {
        root.push("autostart");
        root.push("clipmonger.desktop");
        Self {
            exe,
            file: root,
            portable,
        }
    }
}

impl AutostartManager for X11Autostart {
    const AFFINITY: Affinity = Affinity::Worker;

    fn enable(&mut self) -> Result<()> {
        if self.portable {
            return Ok(());
        }
        if let Some(parent) = self.file.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Io(format!("autostart dir failed: {e}")))?;
        }
        let text = crate::desktop::render_desktop(&self.exe.to_string_lossy(), true, false);
        std::fs::write(&self.file, text)
            .map_err(|e| Error::Io(format!("autostart write failed: {e}")))?;
        Ok(())
    }

    fn disable(&mut self) -> Result<()> {
        if self.portable {
            return Ok(());
        }
        match std::fs::remove_file(&self.file) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::Io(format!("autostart remove failed: {e}"))),
        }
    }

    fn is_enabled(&self) -> Result<bool> {
        if self.portable {
            return Ok(false);
        }
        let text = match std::fs::read_to_string(&self.file) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(Error::Io(format!("autostart read failed: {e}"))),
        };
        Ok(crate::desktop::parse_desktop(&text).is_ok_and(|entry| !entry.hidden))
    }
}
