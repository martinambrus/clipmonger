//! Frontmost application via `NSWorkspace.frontmostApplication` (seg-08
//! §7; no permission needed). Targets are processes: macOS gives no window
//! title without Accessibility or Screen Recording, so the title hash is 0.

use clipmonger_platform::traits::ForegroundApp;
use clipmonger_platform::types::{Affinity, AppId, Error, PasteTarget, PlatformHandle, Result};

use crate::os;

/// A running application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningApp {
    /// Process id (-1 when the app has none).
    pub pid: i32,
    /// `CFBundleIdentifier`, if bundled.
    pub bundle_id: Option<String>,
    /// Localized name.
    pub name: Option<String>,
}

impl RunningApp {
    /// Identity used as `AppId::exe`: bundle id, else name.
    #[must_use]
    pub fn ident(&self) -> String {
        self.bundle_id
            .clone()
            .or_else(|| self.name.clone())
            .unwrap_or_default()
    }

    /// Whether a denylist entry names this app (case-insensitive substring
    /// of the bundle id or name, as the X11 owner-class check).
    #[must_use]
    pub fn denylisted(&self, denylist: &[String]) -> bool {
        let ids: Vec<String> = [&self.bundle_id, &self.name]
            .into_iter()
            .flatten()
            .map(|s| s.to_lowercase())
            .collect();
        denylist.iter().any(|entry| {
            let entry = entry.trim().to_lowercase();
            !entry.is_empty() && ids.iter().any(|id| id.contains(&entry))
        })
    }
}

/// Paste-target handle for `pid`.
#[must_use]
pub fn handle_for(pid: i32) -> PlatformHandle {
    PlatformHandle::Opaque(format!("pid:{pid}"))
}

/// Process id named by a target handle (`pid:<n>`).
#[must_use]
pub fn pid_of(target: &PasteTarget) -> Option<i32> {
    match &target.handle {
        PlatformHandle::Opaque(text) => text.strip_prefix("pid:")?.parse().ok(),
        _ => None,
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn target_of(app: &RunningApp) -> PasteTarget {
    PasteTarget {
        app: AppId {
            exe: app.ident(),
            title_hash: 0,
        },
        window_title_hash: 0,
        handle: handle_for(app.pid),
        captured_ms: now_ms(),
    }
}

/// Frontmost-application query.
pub struct MacForeground {
    _private: (),
}

impl MacForeground {
    /// Bind to the workspace.
    ///
    /// # Errors
    ///
    /// [`Error::Unavailable`] off macOS or without a user session.
    pub fn new() -> Result<Self> {
        os::change_count()?;
        Ok(Self { _private: () })
    }

    /// Pid of the frontmost application.
    #[must_use]
    pub fn frontmost_pid() -> Option<i32> {
        os::frontmost().map(|app| app.pid).filter(|pid| *pid > 0)
    }

    /// Pid of the application with the keyboard focus, asked live through
    /// Accessibility when trusted (`NSWorkspace` can lag behind an activation
    /// on macOS 10.15), else [`Self::frontmost_pid`].
    #[must_use]
    pub fn focused_pid() -> Option<i32> {
        os::ax_focused_app_pid().or_else(Self::frontmost_pid)
    }

    /// The frontmost application.
    #[must_use]
    pub fn frontmost_app() -> Option<RunningApp> {
        os::frontmost()
    }

    /// Capture the frontmost application as a paste target.
    ///
    /// # Errors
    ///
    /// [`Error::Unavailable`] when no application is frontmost.
    pub fn capture_target(&self) -> Result<PasteTarget> {
        os::ax_focused_app_pid()
            .and_then(os::app_for_pid)
            .or_else(os::frontmost)
            .filter(|app| app.pid > 0)
            .map(|app| target_of(&app))
            .ok_or_else(|| Error::Unavailable("no frontmost application".to_string()))
    }

    /// The paste target for a running `pid` as it is now.
    ///
    /// # Errors
    ///
    /// [`Error::Unavailable`] when the process is gone.
    pub fn target_for_pid(&self, pid: i32) -> Result<PasteTarget> {
        os::app_for_pid(pid)
            .map(|app| target_of(&app))
            .ok_or_else(|| Error::Unavailable("paste target dead".to_string()))
    }

    /// Bring `pid` to the front (`activateWithOptions:`).
    ///
    /// # Errors
    ///
    /// [`Error::Unavailable`] when the process is gone or refuses.
    pub fn activate(&self, pid: i32) -> Result<()> {
        if os::activate(pid) {
            Ok(())
        } else {
            Err(Error::Unavailable(format!("cannot activate pid {pid}")))
        }
    }
}

impl ForegroundApp for MacForeground {
    const AFFINITY: Affinity = Affinity::Worker;

    fn active(&self) -> Option<AppId> {
        os::frontmost().map(|app| AppId {
            exe: app.ident(),
            title_hash: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_handles_roundtrip() {
        let app = RunningApp {
            pid: 4242,
            bundle_id: Some("com.apple.TextEdit".into()),
            name: Some("TextEdit".into()),
        };
        let target = target_of(&app);
        assert_eq!(target.app.exe, "com.apple.TextEdit");
        assert_eq!(pid_of(&target), Some(4242));
        let mut other = target.clone();
        other.handle = PlatformHandle::X11Window(1);
        assert_eq!(pid_of(&other), None);
        other.handle = PlatformHandle::Opaque("pid:x".into());
        assert_eq!(pid_of(&other), None);
    }

    #[test]
    fn denylist_matches_bundle_id_or_name_case_insensitively() {
        let app = RunningApp {
            pid: 1,
            bundle_id: Some("com.agilebits.onepassword7".into()),
            name: Some("1Password 7".into()),
        };
        assert!(app.denylisted(&["1password".into()]));
        assert!(app.denylisted(&["COM.AGILEBITS".into()]));
        assert!(!app.denylisted(&["keepass".into(), String::new()]));
        let bare = RunningApp {
            pid: 1,
            bundle_id: None,
            name: None,
        };
        assert!(!bare.denylisted(&["x".into()]));
        assert_eq!(bare.ident(), "");
    }
}
