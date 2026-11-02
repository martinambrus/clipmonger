//! X11 autostart over `~/.config/autostart/clipmonger.desktop`
//! (rendered/parsed by the pure `desktop` module; never shells out).

use std::path::{Path, PathBuf};

use clipmonger_platform::traits::AutostartManager;
use clipmonger_platform::types::{Affinity, Error, Result};

/// Desktop-entry autostart manager.
pub struct X11Autostart {
    exe: PathBuf,
    file: PathBuf,
    portable: bool,
    profile_dir: Option<PathBuf>,
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| Error::Unavailable("autostart path is not Unicode".to_string()))
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
            profile_dir: None,
        }
    }

    /// Manage with profile support under `root/autostart/`; validates
    /// `exe` and `profile_dir` fallibly (NUL/CR/LF rejection) before
    /// creation.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid or non-Unicode paths.
    pub fn with_profile(
        exe: PathBuf,
        mut root: PathBuf,
        profile_dir: PathBuf,
        portable: bool,
    ) -> Result<Self> {
        crate::desktop::try_render_desktop(path_str(&exe)?, true, false, Some(path_str(&profile_dir)?))?;
        root.push("autostart");
        root.push("clipmonger.desktop");
        Ok(Self {
            exe,
            file: root,
            portable,
            profile_dir: Some(profile_dir),
        })
    }

    /// Borrowed-path variant for testing.
    pub fn try_with_profile(
        exe: &Path,
        root: &Path,
        profile_dir: &Path,
        portable: bool,
    ) -> Result<Self> {
        crate::desktop::try_render_desktop(path_str(exe)?, true, false, Some(path_str(profile_dir)?))?;
        let mut file = root.to_path_buf();
        file.push("autostart");
        file.push("clipmonger.desktop");
        Ok(Self {
            exe: exe.to_path_buf(),
            file,
            portable,
            profile_dir: Some(profile_dir.to_path_buf()),
        })
    }
}

impl AutostartManager for X11Autostart {
    const AFFINITY: Affinity = Affinity::Worker;

    fn enable(&mut self) -> Result<()> {
        if self.portable {
            return Ok(());
        }
        let profile = self.profile_dir.as_deref().map(path_str).transpose()?;
        let text = crate::desktop::try_render_desktop(path_str(&self.exe)?, true, false, profile)?;
        if let Some(parent) = self.file.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Io(format!("autostart dir failed: {e}")))?;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn legacy_new_retained() {
        let dir = std::env::temp_dir();
        let m = X11Autostart::new(Path::new("/usr/bin/clipmonger").to_path_buf(), dir, false);
        assert!(m.profile_dir.is_none());
    }

    #[test]
    fn profile_new_validates() {
        let dir = std::env::temp_dir();
        let ok = X11Autostart::with_profile(
            Path::new("/usr/bin/clipmonger").to_path_buf(),
            dir.clone(),
            Path::new("/home/alice/profiles/work").to_path_buf(),
            false,
        );
        assert!(ok.is_ok());
        let bad = X11Autostart::with_profile(
            Path::new("/usr/bin/clipmonger").to_path_buf(),
            dir,
            Path::new("/bad\npath").to_path_buf(),
            false,
        );
        assert!(bad.is_err());
    }

    #[test]
    fn enable_query_disable_with_profile_synthetic_root() {
        let root = tempfile_like_root();
        let exe = Path::new("/usr/bin/clipmonger");
        let profile = Path::new("/home/alice/My Profiles/work profile");
        let mut m = X11Autostart::with_profile(
            exe.to_path_buf(),
            root.clone(),
            profile.to_path_buf(),
            false,
        )
        .unwrap();
        m.enable().expect("enable should write");
        let file = root.join("autostart").join("clipmonger.desktop");
        let text = std::fs::read_to_string(&file).expect("file written");
        let entry = crate::desktop::parse_desktop(&text).expect("parse written");
        assert_eq!(entry.exec, exe.to_string_lossy());
        assert_eq!(entry.profile.as_deref(), Some(profile.to_string_lossy().as_ref()));
        assert!(entry.minimized);
        assert!(!entry.hidden);
        assert!(m.is_enabled().unwrap(), "should be enabled");
        m.disable().expect("disable");
        assert!(!m.is_enabled().unwrap());
        // no HOME leakage: file under synthetic root only
        assert!(!file.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn enable_with_reserved_chars_exact_file() {
        let root = tempfile_like_root();
        let exe = Path::new("/opt/My Apps/clipmonger");
        let profile = Path::new("/home/alice/profiles/foo%bar$baz\\qux`tick\"quote");
        let mut m = X11Autostart::with_profile(
            exe.to_path_buf(),
            root.clone(),
            profile.to_path_buf(),
            false,
        )
        .unwrap();
        m.enable().unwrap();
        let file = root.join("autostart").join("clipmonger.desktop");
        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(text, r#"[Desktop Entry]
Type=Application
Name=Clipmonger
Exec="/opt/My Apps/clipmonger" --minimized --profile-dir "/home/alice/profiles/foo%%bar\\$baz\\\\qux\\`tick\\"quote"
Hidden=false
Terminal=false
"#);
        let entry = crate::desktop::parse_desktop(&text).unwrap();
        assert_eq!(entry.exec, exe.to_string_lossy());
        assert_eq!(entry.profile.unwrap(), profile.to_string_lossy().to_string());
        assert!(m.is_enabled().unwrap());
        m.disable().unwrap();
        assert!(!m.is_enabled().unwrap());
        assert!(!file.exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn portable_zero_writes() {
        let root = tempfile_like_root();
        let exe = Path::new("/usr/bin/clipmonger");
        let profile = Path::new("/home/alice/profiles/work");
        let mut m = X11Autostart::with_profile(
            exe.to_path_buf(),
            root.clone(),
            profile.to_path_buf(),
            true,
        )
        .unwrap();
        m.enable().unwrap();
        assert!(!root.join("autostart").join("clipmonger.desktop").exists());
        assert_eq!(m.is_enabled().unwrap(), false);
        m.disable().unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn malformed_not_written() {
        let root = tempfile_like_root();
        let bad = X11Autostart::with_profile(
            Path::new("/usr/bin/clipmonger").to_path_buf(),
            root.clone(),
            Path::new("/bad\ninjection").to_path_buf(),
            false,
        );
        assert!(bad.is_err());
        assert!(!root.join("autostart").join("clipmonger.desktop").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    fn tempfile_like_root() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("clipmonger-test-{}-{}", std::process::id(), uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
    #[cfg(unix)]
    #[test]
    fn non_unicode_paths_are_rejected_before_writes() {
        use std::os::unix::ffi::OsStrExt;
        let root = tempfile_like_root();
        let bad = Path::new(std::ffi::OsStr::from_bytes(b"/bad\xff"));
        for (exe, profile) in [(bad, Path::new("/profile")), (Path::new("/exe"), bad)] {
            assert!(matches!(X11Autostart::try_with_profile(exe, &root, profile, false), Err(Error::Unavailable(_))));
            assert!(!root.join("autostart").exists());
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

}
