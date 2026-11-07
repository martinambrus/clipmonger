//! Autostart via a `LaunchAgent` at
//! `~/Library/LaunchAgents/com.martinambrus.clipmonger.plist` (seg-08 §6):
//! file-based like the Linux `.desktop` entry, never shells out to
//! `launchctl`; launchd picks it up at the next login.

use std::path::{Path, PathBuf};

use clipmonger_platform::traits::AutostartManager;
use clipmonger_platform::types::{Affinity, Error, Result};

use crate::plist::{self, Agent};

/// Fixed bundle id (`CFBundleIdentifier`), also the agent label.
pub const BUNDLE_ID: &str = "com.martinambrus.clipmonger";
/// Agent file name.
pub const AGENT_FILE: &str = "com.martinambrus.clipmonger.plist";

/// `LaunchAgent` autostart manager.
pub struct MacAutostart {
    exe: PathBuf,
    file: PathBuf,
    portable: bool,
    profile_dir: Option<PathBuf>,
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| Error::Unavailable("autostart path is not Unicode".to_string()))
}

/// `~/Library/LaunchAgents` of the current user, from `$HOME`.
#[must_use]
pub fn default_agents_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/LaunchAgents"))
}

impl MacAutostart {
    /// Manage the agent for `exe` in `agents_dir`; `portable` disables
    /// everything (no-op, like the other backends).
    #[must_use]
    pub fn new(exe: PathBuf, mut agents_dir: PathBuf, portable: bool) -> Self {
        agents_dir.push(AGENT_FILE);
        Self {
            exe,
            file: agents_dir,
            portable,
            profile_dir: None,
        }
    }

    /// Manage with `--profile-dir`; validates both paths before creation.
    ///
    /// # Errors
    ///
    /// Returns an error for non-Unicode paths or control characters.
    pub fn with_profile(
        exe: PathBuf,
        mut agents_dir: PathBuf,
        profile_dir: PathBuf,
        portable: bool,
    ) -> Result<Self> {
        agents_dir.push(AGENT_FILE);
        let me = Self {
            exe,
            file: agents_dir,
            portable,
            profile_dir: Some(profile_dir),
        };
        plist::render(&me.agent()?)?;
        Ok(me)
    }

    /// The agent this manager writes.
    fn agent(&self) -> Result<Agent> {
        let mut args = vec![path_str(&self.exe)?.to_string(), "--minimized".to_string()];
        if let Some(profile) = &self.profile_dir {
            args.push("--profile-dir".to_string());
            args.push(path_str(profile)?.to_string());
        }
        Ok(Agent {
            label: BUNDLE_ID.to_string(),
            args,
            run_at_load: true,
        })
    }

    /// Path of the managed plist.
    #[must_use]
    pub fn file(&self) -> &Path {
        &self.file
    }
}

impl AutostartManager for MacAutostart {
    const AFFINITY: Affinity = Affinity::Worker;

    fn enable(&mut self) -> Result<()> {
        if self.portable {
            return Ok(());
        }
        let text = plist::render(&self.agent()?)?;
        if let Some(parent) = self.file.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Io(format!("launch agent dir failed: {e}")))?;
        }
        // Write-then-rename: launchd never sees a half-written plist.
        let tmp = self.file.with_extension("plist.tmp");
        std::fs::write(&tmp, text)
            .map_err(|e| Error::Io(format!("launch agent write failed: {e}")))?;
        std::fs::rename(&tmp, &self.file)
            .map_err(|e| Error::Io(format!("launch agent rename failed: {e}")))
    }

    fn disable(&mut self) -> Result<()> {
        if self.portable {
            return Ok(());
        }
        remove_if_present(&self.file)
    }

    fn is_enabled(&self) -> Result<bool> {
        if self.portable {
            return Ok(false);
        }
        let Some(agent) = read_agent(&self.file)? else {
            return Ok(false);
        };
        Ok(agent.run_at_load && agent.args.first().map(String::as_str) == self.exe.to_str())
    }
}

fn remove_if_present(file: &Path) -> Result<()> {
    match std::fs::remove_file(file) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::Io(format!("launch agent remove failed: {e}"))),
    }
}

/// The parsed agent, `None` when the file is absent or unreadable as ours.
fn read_agent(file: &Path) -> Result<Option<Agent>> {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::Io(format!("launch agent read failed: {e}"))),
    };
    Ok(plist::parse(&text).ok())
}

/// Delete our agent in `agents_dir` when the program it starts no longer
/// exists (app moved to the Trash, P-03). Returns whether it was removed.
///
/// # Errors
///
/// Returns [`Error::Io`] when the plist cannot be read or removed.
pub fn remove_stale_agent(agents_dir: &Path) -> Result<bool> {
    let file = agents_dir.join(AGENT_FILE);
    let Some(agent) = read_agent(&file)? else {
        return Ok(false);
    };
    let alive = agent
        .args
        .first()
        .is_some_and(|program| Path::new(program).exists());
    if alive {
        return Ok(false);
    }
    remove_if_present(&file)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "clipmonger-macos-test-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn t802_enable_query_disable() {
        let root = temp_root();
        let agents = root.join("LaunchAgents");
        let exe = root.join("Clipmonger.app/Contents/MacOS/clipmonger-ui");
        let profile = PathBuf::from("/Users/a/Library/Application Support/clip & monger");
        let mut m = MacAutostart::with_profile(exe.clone(), agents.clone(), profile.clone(), false)
            .unwrap();
        assert!(!m.is_enabled().unwrap());
        m.enable().unwrap();
        let text = std::fs::read_to_string(agents.join(AGENT_FILE)).unwrap();
        let agent = plist::parse(&text).unwrap();
        assert_eq!(agent.label, BUNDLE_ID);
        assert_eq!(
            agent.args,
            [
                exe.to_str().unwrap(),
                "--minimized",
                "--profile-dir",
                profile.to_str().unwrap()
            ]
        );
        assert!(m.is_enabled().unwrap());
        let other = MacAutostart::new(PathBuf::from("/elsewhere/ui"), agents.clone(), false);
        assert!(!other.is_enabled().unwrap(), "program path must match");
        m.disable().unwrap();
        assert!(!m.is_enabled().unwrap());
        assert!(!agents.join(AGENT_FILE).exists());
        m.disable().unwrap();
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn t802_stale_agent_removed_only_when_program_missing() {
        let root = temp_root();
        let agents = root.join("LaunchAgents");
        let exe = root.join("clipmonger-ui");
        std::fs::write(&exe, b"").unwrap();
        let mut m = MacAutostart::new(exe.clone(), agents.clone(), false);
        assert!(!remove_stale_agent(&agents).unwrap(), "no agent yet");
        m.enable().unwrap();
        assert!(!remove_stale_agent(&agents).unwrap(), "program exists");
        assert!(agents.join(AGENT_FILE).exists());
        std::fs::remove_file(&exe).unwrap();
        assert!(remove_stale_agent(&agents).unwrap());
        assert!(!agents.join(AGENT_FILE).exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn portable_and_invalid_paths_write_nothing() {
        let root = temp_root();
        let agents = root.join("LaunchAgents");
        let mut m = MacAutostart::with_profile(
            PathBuf::from("/x/ui"),
            agents.clone(),
            PathBuf::from("/p"),
            true,
        )
        .unwrap();
        m.enable().unwrap();
        assert!(!agents.exists());
        assert!(!m.is_enabled().unwrap());
        assert!(MacAutostart::with_profile(
            PathBuf::from("/x/ui"),
            agents.clone(),
            PathBuf::from("/bad\npath"),
            false
        )
        .is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
