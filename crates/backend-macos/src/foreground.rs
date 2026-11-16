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

/// Lookups [`resolve_live`] makes before it gives up on a live process.
const RESOLVE_TRIES: u32 = 4;
/// Pause between those lookups.
const RESOLVE_PAUSE: std::time::Duration = std::time::Duration::from_millis(40);

/// `pid` as a running application. `NSRunningApplication` can miss a live
/// app for a moment when asked off the main thread (seen in the daemon on
/// macOS 13: `TextEdit` in front and focused, yet no app for its pid), which
/// failed popup pastes with "paste target dead". A miss counts only once
/// the process is really gone (`alive`); while it lives the lookup is asked
/// again on the main thread (`lookup_main`, `None` there when it cannot
/// run) and after a short `pause`, up to [`RESOLVE_TRIES`] times.
fn resolve_live(
    pid: i32,
    lookup: impl Fn(i32) -> Option<RunningApp>,
    lookup_main: impl Fn(i32) -> Option<RunningApp>,
    alive: impl Fn(i32) -> bool,
    pause: impl Fn(),
) -> Option<RunningApp> {
    for attempt in 0..RESOLVE_TRIES {
        if let Some(app) = lookup(pid) {
            return Some(app);
        }
        if !alive(pid) {
            return None;
        }
        if let Some(app) = lookup_main(pid) {
            return Some(app);
        }
        if attempt + 1 < RESOLVE_TRIES {
            pause();
        }
    }
    None
}

/// [`os::app_for_pid`] on the main thread, when the `AppKit` loop serves it
/// and this is another thread.
fn app_for_pid_on_main(pid: i32) -> Option<RunningApp> {
    if crate::main_loop::is_main_thread() || !crate::main_loop::is_running() {
        return None;
    }
    crate::main_loop::on_main(move || os::app_for_pid(pid))
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
        resolve_live(
            pid,
            os::app_for_pid,
            app_for_pid_on_main,
            os::process_exists,
            || std::thread::sleep(RESOLVE_PAUSE),
        )
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
    fn a_live_app_missed_by_the_lookup_is_asked_again_not_called_dead() {
        use std::cell::Cell;
        let app = || RunningApp {
            pid: 357,
            bundle_id: Some("com.apple.TextEdit".into()),
            name: Some("TextEdit".into()),
        };
        // Found at once.
        assert_eq!(
            resolve_live(357, |_| Some(app()), |_| None, |_| true, || {}),
            Some(app())
        );
        // Missed off the main thread, found on it.
        assert_eq!(
            resolve_live(357, |_| None, |_| Some(app()), |_| true, || {}),
            Some(app())
        );
        // Missed twice, then found after a pause.
        let calls = Cell::new(0);
        let flaky = |_| {
            calls.set(calls.get() + 1);
            (calls.get() > 2).then(app)
        };
        let pauses = Cell::new(0);
        assert_eq!(
            resolve_live(
                357,
                flaky,
                |_| None,
                |_| true,
                || pauses.set(pauses.get() + 1)
            ),
            Some(app())
        );
        assert_eq!(pauses.get(), 2);
        // A process that is gone is dead at once, without retries.
        let pauses = Cell::new(0);
        assert_eq!(
            resolve_live(
                357,
                |_| None,
                |_| None,
                |_| false,
                || pauses.set(pauses.get() + 1)
            ),
            None
        );
        assert_eq!(pauses.get(), 0);
        // A live process the lookup never finds: bounded retries, then none.
        let pauses = Cell::new(0);
        assert_eq!(
            resolve_live(
                357,
                |_| None,
                |_| None,
                |_| true,
                || pauses.set(pauses.get() + 1)
            ),
            None
        );
        assert_eq!(pauses.get(), RESOLVE_TRIES - 1);
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
