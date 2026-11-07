//! Shared autostart service: portable classification and daemon-owned injectable adapter.
//!
//! Exactly one daemon-owned service, native configured startup independent of
//! DISPLAY/WAYLAND_DISPLAY/--headless, portable ZERO manager calls, profile-aware sibling UI command.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use clipmonger_platform::traits::AutostartManager;
use clipmonger_platform::types::{Error, Result};

// ---------------------------------------------------------------------------
// Portable classification
// ---------------------------------------------------------------------------

/// Marker file beside the daemon executable that forces portable mode.
const PORTABLE_MARKER: &str = "portable.marker";

/// Result of portable classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortableKind {
    Portable,
    Installed,
}

/// Classify `profile_dir` relative to `exe_path`.
///
/// Portable when a `portable.marker` exists beside the daemon binary, or when
/// the canonical profile directory is identity-equal to the daemon executable
/// parent.  Never uses prefix matching; parent/child/sibling profiles are
/// distinguished.  Symlink to exe-dir is portable after canonicalization.
///
/// Returns `Err` (fail-closed to unavailable) on ambiguous or failed identity
/// resolution rather than guessing installed.
///
/// # Errors
///
/// Returns `Error::Unavailable` if path or directory identity resolution fails.
pub fn classify_portable(exe_path: &Path, profile_dir: &Path) -> Result<PortableKind> {
    let cwd = if exe_path.is_absolute() && profile_dir.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir().map_err(|_| unavailable())?
    };
    classify_with_cwd(exe_path, profile_dir, &cwd)
}

fn unavailable() -> Error {
    Error::Unavailable("autostart unavailable".to_string())
}

fn classify_with_cwd(exe_path: &Path, profile_dir: &Path, cwd: &Path) -> Result<PortableKind> {
    let exe = if exe_path.is_absolute() {
        exe_path.to_path_buf()
    } else {
        cwd.join(exe_path)
    };
    let exe_dir = exe.parent().ok_or_else(unavailable)?;
    match std::fs::metadata(exe_dir.join(PORTABLE_MARKER)) {
        Ok(metadata) if metadata.is_file() => return Ok(PortableKind::Portable),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(unavailable()),
    }
    let profile = if profile_dir.is_absolute() {
        profile_dir.to_path_buf()
    } else {
        cwd.join(profile_dir)
    };
    let exe_dir = std::fs::canonicalize(exe_dir).map_err(|_| unavailable())?;
    let profile = std::fs::canonicalize(profile).map_err(|_| unavailable())?;
    if same_directory(&exe_dir, &profile)? {
        Ok(PortableKind::Portable)
    } else {
        Ok(PortableKind::Installed)
    }
}

#[cfg(unix)]
fn same_directory(a: &Path, b: &Path) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let a = std::fs::File::open(a).map_err(|_| unavailable())?;
    let b = std::fs::File::open(b).map_err(|_| unavailable())?;
    let a = a.metadata().map_err(|_| unavailable())?;
    let b = b.metadata().map_err(|_| unavailable())?;
    if !a.is_dir() || !b.is_dir() {
        return Err(unavailable());
    }
    Ok((a.dev(), a.ino()) == (b.dev(), b.ino()))
}

#[cfg(windows)]
#[allow(unsafe_code)] // Win32 file identity requires an FFI call on opened handles.
fn same_directory(a: &Path, b: &Path) -> Result<bool> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS,
    };
    let open = |p: &Path| {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(p)
            .map_err(|_| unavailable())
    };
    let a = open(a)?;
    let b = open(b)?;
    if !a.metadata().map_err(|_| unavailable())?.is_dir()
        || !b.metadata().map_err(|_| unavailable())?.is_dir()
    {
        return Err(unavailable());
    }
    let identity = |f: &std::fs::File| -> Result<(u32, u32, u32)> {
        let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
        // SAFETY: `f` owns the live handle for the call, and `info` points to
        // writable storage for a BY_HANDLE_FILE_INFORMATION value.
        if unsafe { GetFileInformationByHandle(f.as_raw_handle().cast(), info.as_mut_ptr()) } == 0 {
            return Err(unavailable());
        }
        // SAFETY: the API succeeded and initialized the complete structure above.
        let info = unsafe { info.assume_init() };
        Ok((
            info.dwVolumeSerialNumber,
            info.nFileIndexHigh,
            info.nFileIndexLow,
        ))
    };
    Ok(identity(&a)? == identity(&b)?)
}

#[cfg(not(any(unix, windows)))]
fn same_directory(_: &Path, _: &Path) -> Result<bool> {
    Err(unavailable())
}

#[derive(Debug, PartialEq, Eq)]
enum Selection {
    Portable,
    Native,
    Unavailable,
}

fn selection(classification: &Result<PortableKind>, native_supported: bool) -> Selection {
    match classification {
        Ok(PortableKind::Portable) => Selection::Portable,
        Ok(PortableKind::Installed) if native_supported => Selection::Native,
        _ => Selection::Unavailable,
    }
}

/// Whether the daemon is in portable mode.
///
/// Convenience wrapper over `classify_portable`; returns `Ok(true)` for portable,
/// `Ok(false)` for installed, `Err` for ambiguous.
///
/// # Errors
///
/// Returns `Error::Unavailable` if portable classification fails.
pub fn is_portable(exe_path: &Path, profile_dir: &Path) -> Result<bool> {
    match classify_portable(exe_path, profile_dir)? {
        PortableKind::Portable => Ok(true),
        PortableKind::Installed => Ok(false),
    }
}

// ---------------------------------------------------------------------------
// XDG autostart root (Unix)
// ---------------------------------------------------------------------------

/// Resolve XDG autostart config root respecting `XDG_CONFIG_HOME`.
///
/// Absolute `XDG_CONFIG_HOME` is used when present; otherwise valid absolute
/// `HOME/.config`.  Missing or non-absolute values yield `Unavailable` rather
/// than a profile-dir fallback.
///
/// # Errors
///
/// Returns `Error::Unavailable` if the selected environment path is missing,
/// empty, non-absolute, or contains NUL.
pub fn xdg_autostart_root() -> Result<PathBuf> {
    xdg_root_from(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

fn xdg_root_from(
    xdg: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Result<PathBuf> {
    if let Some(raw) = xdg {
        if raw.is_empty() {
            return Err(Error::Unavailable("XDG_CONFIG_HOME is empty".to_string()));
        }
        let path = PathBuf::from(&raw);
        if !path.is_absolute() {
            return Err(Error::Unavailable(
                "XDG_CONFIG_HOME is not absolute".to_string(),
            ));
        }
        // Reject NUL and other control that would break desktop-file handling.
        if let Some(s) = path.to_str() {
            if s.contains('\0') {
                return Err(Error::Unavailable(
                    "XDG_CONFIG_HOME contains NUL".to_string(),
                ));
            }
        }
        return Ok(path);
    }
    if let Some(home) = home {
        if home.is_empty() {
            return Err(Error::Unavailable("HOME is empty".to_string()));
        }
        let home_path = PathBuf::from(&home);
        if !home_path.is_absolute() {
            return Err(Error::Unavailable("HOME is not absolute".to_string()));
        }
        if let Some(s) = home_path.to_str() {
            if s.contains('\0') {
                return Err(Error::Unavailable("HOME contains NUL".to_string()));
            }
        }
        return Ok(home_path.join(".config"));
    }
    Err(Error::Unavailable(
        "no XDG_CONFIG_HOME or HOME for autostart root".to_string(),
    ))
}

// ---------------------------------------------------------------------------
// Sibling UI executable
// ---------------------------------------------------------------------------

/// Sibling UI executable next to the daemon binary.
pub fn sibling_ui_exe(daemon_exe: &Path) -> PathBuf {
    let dir = daemon_exe
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    dir.join(format!("clipmonger{}", std::env::consts::EXE_SUFFIX))
}

// ---------------------------------------------------------------------------
// Object-safe narrow adapter
// ---------------------------------------------------------------------------

/// Object-safe adapter over a generic `AutostartManager`.
///
/// `AutostartManager` is not object-safe due to `AFFINITY`; this narrow
/// trait captures only the OS operations we expose.
pub trait AutostartAdapter: Send + Sync {
    /// Enable autostart.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend cannot enable autostart.
    fn enable(&self) -> Result<()>;
    /// Disable autostart.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend cannot disable autostart.
    fn disable(&self) -> Result<()>;
    /// Query whether autostart is enabled.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend cannot query autostart.
    fn is_enabled(&self) -> Result<bool>;
}

struct GenericAdapter<M> {
    inner: Mutex<M>,
}

impl<M> AutostartAdapter for GenericAdapter<M>
where
    M: AutostartManager + Send,
{
    fn enable(&self) -> Result<()> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| Error::Internal("autostart adapter poisoned on enable".to_string()))?;
        guard.enable()
    }

    fn disable(&self) -> Result<()> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| Error::Internal("autostart adapter poisoned on disable".to_string()))?;
        guard.disable()
    }

    fn is_enabled(&self) -> Result<bool> {
        let guard = self
            .inner
            .lock()
            .map_err(|_| Error::Internal("autostart adapter poisoned on query".to_string()))?;
        guard.is_enabled()
    }
}

fn make_adapter<M>(manager: M) -> Arc<dyn AutostartAdapter>
where
    M: AutostartManager + Send + 'static,
{
    assert_eq!(M::AFFINITY, clipmonger_platform::types::Affinity::Worker);
    Arc::new(GenericAdapter {
        inner: Mutex::new(manager),
    })
}

// Preserve only the backend class; never retain or expose its raw message.
fn bounded_error(error: &Error) -> Error {
    const MESSAGE: &str = "autostart backend failed";
    match error {
        Error::Unavailable(_) | Error::CaptureChanged => Error::Unavailable(MESSAGE.into()),
        Error::Denied(_) | Error::CaptureExcluded => Error::Denied(MESSAGE.into()),
        Error::Timeout(_) => Error::Timeout(MESSAGE.into()),
        Error::TooLarge(_) => Error::TooLarge(MESSAGE.into()),
        Error::Unsupported(_) => Error::Unsupported(MESSAGE.into()),
        Error::Io(_) => Error::Io(MESSAGE.into()),
        Error::Internal(_) => Error::Internal(MESSAGE.into()),
    }
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

/// Startup outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupOutcome {
    SkippedPortable,
    SkippedDisabled,
    Enabled,
}

/// Explicit apply outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExplicitOutcome {
    Enabled,
    Disabled,
}

/// Daemon-owned injectable autostart service.
///
/// Holds the narrow adapter, portable flag, and serialization state. The same
/// instance is retained in `DaemonState` for later explicit config/restore
/// operations. All OS operations are dispatched via `spawn_blocking` by the
/// caller; this type only serializes and enforces portable/availability rules.
#[derive(Clone)]
pub struct AutostartService {
    inner: Arc<ServiceInner>,
}
struct ServiceInner {
    adapter: Option<Arc<dyn AutostartAdapter>>,
    portable: bool,
    operation: Mutex<Option<Result<StartupOutcome>>>,
}

impl Default for AutostartService {
    fn default() -> Self {
        Self::unconfigured()
    }
}

impl AutostartService {
    fn new(adapter: Option<Arc<dyn AutostartAdapter>>, portable: bool) -> Self {
        Self {
            inner: Arc::new(ServiceInner {
                adapter,
                portable,
                operation: Mutex::new(None),
            }),
        }
    }
    #[must_use]
    pub fn unconfigured() -> Self {
        Self::new(None, false)
    }
    #[must_use]
    pub fn portable() -> Self {
        Self::new(None, true)
    }
    #[must_use]
    pub fn with_adapter(adapter: Arc<dyn AutostartAdapter>, portable: bool) -> Self {
        Self::new(Some(adapter), portable)
    }
    #[must_use]
    pub fn is_portable(&self) -> bool {
        self.inner.portable
    }
    #[must_use]
    pub fn is_available(&self) -> bool {
        !self.inner.portable && self.inner.adapter.is_some()
    }
    #[must_use]
    pub fn adapter_ptr(&self) -> Option<Arc<dyn AutostartAdapter>> {
        self.inner.adapter.clone()
    }

    /// Apply the startup setting once.
    ///
    /// # Errors
    ///
    /// Returns an error if a non-portable service has no adapter, the backend
    /// operation fails, or the operation lock or worker fails.
    pub async fn apply_startup(&self, enabled: bool) -> Result<StartupOutcome> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut startup = inner
                .operation
                .lock()
                .map_err(|_| Error::Internal("autostart lock failed".into()))?;
            if let Some(result) = &*startup {
                return result.as_ref().cloned().map_err(bounded_error);
            }
            let result = if inner.portable {
                Ok(StartupOutcome::SkippedPortable)
            } else {
                match inner.adapter.as_ref() {
                    None => Err(unavailable()),
                    Some(_) if !enabled => Ok(StartupOutcome::SkippedDisabled),
                    Some(adapter) => adapter
                        .enable()
                        .map(|()| StartupOutcome::Enabled)
                        .map_err(|error| bounded_error(&error)),
                }
            };
            let returned = result.as_ref().cloned().map_err(bounded_error);
            *startup = Some(result);
            returned
        })
        .await
        .map_err(|_| Error::Internal("autostart worker failed".into()))?
    }

    pub(crate) fn apply_explicit_sync(&self, enabled: bool) -> Result<ExplicitOutcome> {
        let inner = &self.inner;
        let _guard = inner
            .operation
            .lock()
            .map_err(|_| Error::Internal("autostart lock failed".into()))?;
        if inner.portable {
            return Err(unavailable());
        }
        let adapter = inner.adapter.as_ref().ok_or_else(unavailable)?;
        let outcome = if enabled {
            adapter.enable().map(|()| ExplicitOutcome::Enabled)
        } else {
            adapter.disable().map(|()| ExplicitOutcome::Disabled)
        };
        outcome.map_err(|error| bounded_error(&error))
    }

    /// Apply an explicit autostart setting.
    ///
    /// # Errors
    ///
    /// Returns an error if the service is unavailable, the backend operation
    /// fails, or the operation lock or worker fails.
    pub async fn apply_explicit(&self, enabled: bool) -> Result<ExplicitOutcome> {
        let service = self.clone();
        tokio::task::spawn_blocking(move || service.apply_explicit_sync(enabled))
            .await
            .map_err(|_| Error::Internal("autostart worker failed".into()))?
    }

    /// Query the actual autostart state.
    ///
    /// # Errors
    ///
    /// Returns an error if the service is unavailable, the backend query fails,
    /// or the operation lock or worker fails.
    pub async fn query(&self) -> Result<bool> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = inner
                .operation
                .lock()
                .map_err(|_| Error::Internal("autostart lock failed".into()))?;
            if inner.portable {
                return Err(unavailable());
            }
            inner
                .adapter
                .as_ref()
                .ok_or_else(unavailable)?
                .is_enabled()
                .map_err(|error| bounded_error(&error))
        })
        .await
        .map_err(|_| Error::Internal("autostart worker failed".into()))?
    }

    /// Build a service for `profile_dir` and `daemon_exe`.
    ///
    /// No hidden OS operations besides classification and adapter validation.
    /// Returns portable or unconfigured when appropriate; never guesses
    /// installed on ambiguous resolution.
    #[must_use]
    pub fn from_profile(profile_dir: &Path, daemon_exe: &Path) -> Self {
        if !daemon_exe.is_absolute() {
            return Self::unconfigured();
        }
        let supported = cfg!(any(all(windows, feature = "backend-windows"), linux_x11));
        let portable = match selection(&classify_portable(daemon_exe, profile_dir), supported) {
            Selection::Portable => return Self::portable(),
            Selection::Native => false,
            Selection::Unavailable => return Self::unconfigured(),
        };

        #[cfg(all(windows, feature = "backend-windows"))]
        {
            let ui_exe = sibling_ui_exe(daemon_exe);
            let Ok(profile_abs) = absolute_path(profile_dir) else {
                return Self::unconfigured();
            };
            match clipmonger_backend_windows::WinAutostart::with_profile(ui_exe, profile_abs, false)
            {
                Ok(manager) => {
                    return Self::with_adapter(make_adapter(manager), portable);
                }
                Err(_) => return Self::unconfigured(),
            }
        }

        #[cfg(linux_x11)]
        {
            let Ok(root) = xdg_autostart_root() else {
                return Self::unconfigured();
            };
            let ui_exe = sibling_ui_exe(daemon_exe);
            let Ok(profile_abs) = absolute_path(profile_dir) else {
                return Self::unconfigured();
            };
            match clipmonger_backend_x11::X11Autostart::with_profile(
                ui_exe,
                root,
                profile_abs,
                false,
            ) {
                Ok(manager) => {
                    return Self::with_adapter(make_adapter(manager), portable);
                }
                Err(_) => return Self::unconfigured(),
            }
        }

        #[cfg(not(any(all(windows, feature = "backend-windows"), linux_x11)))]
        {
            let _ = portable;
            return Self::unconfigured();
        }

        #[allow(unreachable_code)]
        Self::unconfigured()
    }

    /// Test helper: create a fake-backed service with counter semantics.
    #[cfg(test)]
    #[must_use]
    pub fn with_fake(fake: Arc<FakeAutostartCounter>, portable: bool) -> Self {
        Self::with_adapter(fake as Arc<dyn AutostartAdapter>, portable)
    }
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        // Validate no NUL etc. via path string check.
        if let Some(s) = path.to_str() {
            if s.contains('\0') {
                return Err(Error::Unavailable("absolute path contains NUL".to_string()));
            }
        }
        return Ok(path.to_path_buf());
    }
    let cur = std::env::current_dir()
        .map_err(|e| Error::Unavailable(format!("cannot resolve current dir: {e}")))?;
    Ok(cur.join(path))
}

// ---------------------------------------------------------------------------
// Fake counter for testing
// ---------------------------------------------------------------------------

/// Counting fake adapter for deterministic tests.
///
/// Thread-safe counters prove exactly-once startup semantics and serialize
/// concurrent fake calls with bounded coordination.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct FakeAutostartCounter {
    state: Mutex<FakeState>,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct FakeState {
    enable_count: usize,
    disable_count: usize,
    query_count: usize,
    fail_enable: bool,
    fail_disable: bool,
    fail_query: bool,
}

#[cfg(test)]
impl FakeAutostartCounter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    /// # Panics
    ///
    /// Panics if the fake state lock is poisoned.
    pub fn counts(&self) -> (usize, usize, usize) {
        let s = self.state.lock().expect("fake poisoned");
        (s.enable_count, s.disable_count, s.query_count)
    }

    /// # Panics
    ///
    /// Panics if the fake state lock is poisoned.
    pub fn set_fail_enable(&self, fail: bool) {
        self.state.lock().expect("fake poisoned").fail_enable = fail;
    }

    /// # Panics
    ///
    /// Panics if the fake state lock is poisoned.
    pub fn set_fail_disable(&self, fail: bool) {
        self.state.lock().expect("fake poisoned").fail_disable = fail;
    }

    /// # Panics
    ///
    /// Panics if the fake state lock is poisoned.
    pub fn set_fail_query(&self, fail: bool) {
        self.state.lock().expect("fake poisoned").fail_query = fail;
    }

    /// # Panics
    ///
    /// Panics if the fake state lock is poisoned.
    pub fn reset_counts(&self) {
        let mut s = self.state.lock().expect("fake poisoned");
        s.enable_count = 0;
        s.disable_count = 0;
        s.query_count = 0;
    }
}

#[cfg(test)]
impl AutostartAdapter for FakeAutostartCounter {
    fn enable(&self) -> Result<()> {
        let mut s = self.state.lock().expect("fake poisoned on enable");
        s.enable_count += 1;
        if s.fail_enable {
            return Err(Error::Io("fake enable failure".to_string()));
        }
        Ok(())
    }

    fn disable(&self) -> Result<()> {
        let mut s = self.state.lock().expect("fake poisoned on disable");
        s.disable_count += 1;
        if s.fail_disable {
            return Err(Error::Io("fake disable failure".to_string()));
        }
        Ok(())
    }

    fn is_enabled(&self) -> Result<bool> {
        let mut s = self.state.lock().expect("fake poisoned on query");
        s.query_count += 1;
        if s.fail_query {
            return Err(Error::Io("fake query failure".to_string()));
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Arc;

    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp_root(prefix: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "clipmonger-autostart-{}-{}-{}",
            prefix,
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    // ---- classifier: equal / child / sibling / relative / symlink / marker ----

    #[test]
    fn marker_forces_portable_even_with_separate_profile() {
        let exe_root = temp_root("marker-exe");
        let _cleanup_exe_root = Cleanup(exe_root.clone());
        let profile_root = temp_root("marker-profile");
        let _cleanup_profile_root = Cleanup(profile_root.clone());
        let exe = exe_root.join("clipmonger-daemon");
        fs::write(&exe, b"").unwrap();
        fs::write(exe_root.join("portable.marker"), b"").unwrap();
        // profile is completely separate directory, but marker forces portable
        let kind = classify_portable(&exe, &profile_root).unwrap();
        assert_eq!(kind, PortableKind::Portable);
        let portable = is_portable(&exe, &profile_root).unwrap();
        assert!(portable);
        let _ = fs::remove_dir_all(&exe_root);
        let _ = fs::remove_dir_all(&profile_root);
    }

    #[test]
    fn canonical_equality_portable() {
        let root = temp_root("equal");
        let _cleanup_root = Cleanup(root.clone());
        let exe_dir = root.join("app");
        fs::create_dir_all(&exe_dir).unwrap();
        let exe = exe_dir.join("clipmonger-daemon");
        fs::write(&exe, b"").unwrap();
        // profile equal to exe_dir canonical
        let kind = classify_portable(&exe, &exe_dir).unwrap();
        assert_eq!(kind, PortableKind::Portable);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn child_profile_not_portable() {
        let root = temp_root("child");
        let _cleanup_root = Cleanup(root.clone());
        let exe_dir = root.join("app");
        fs::create_dir_all(&exe_dir).unwrap();
        let exe = exe_dir.join("clipmonger-daemon");
        fs::write(&exe, b"").unwrap();
        let child = exe_dir.join("profiles").join("work");
        fs::create_dir_all(&child).unwrap();
        let kind = classify_portable(&exe, &child).unwrap();
        assert_eq!(kind, PortableKind::Installed);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn sibling_profile_not_portable() {
        let root = temp_root("sibling");
        let _cleanup_root = Cleanup(root.clone());
        let exe_dir = root.join("app");
        let sibling = root.join("profiles").join("work");
        fs::create_dir_all(&exe_dir).unwrap();
        fs::create_dir_all(&sibling).unwrap();
        let exe = exe_dir.join("clipmonger-daemon");
        fs::write(&exe, b"").unwrap();
        let kind = classify_portable(&exe, &sibling).unwrap();
        assert_eq!(kind, PortableKind::Installed);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn parent_profile_not_portable() {
        let root = temp_root("parent");
        let _cleanup_root = Cleanup(root.clone());
        let exe_dir = root.join("app").join("bin");
        fs::create_dir_all(&exe_dir).unwrap();
        let exe = exe_dir.join("clipmonger-daemon");
        fs::write(&exe, b"").unwrap();
        let parent = root.join("app");
        let kind = classify_portable(&exe, &parent).unwrap();
        assert_eq!(kind, PortableKind::Installed);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn relative_paths_resolved_portable_when_equal_after_canonical() {
        let root = temp_root("relative");
        let _cleanup_root = Cleanup(root.clone());
        let exe_dir = root.join("app");
        fs::create_dir_all(&exe_dir).unwrap();
        let exe = exe_dir.join("clipmonger-daemon");
        fs::write(&exe, b"").unwrap();
        let kind =
            classify_with_cwd(Path::new("app/clipmonger-daemon"), Path::new("app"), &root).unwrap();
        assert_eq!(kind, PortableKind::Portable);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn symlink_profile_to_exe_dir_portable() {
        let root = temp_root("symlink");
        let _cleanup_root = Cleanup(root.clone());
        let exe_dir = root.join("app");
        fs::create_dir_all(&exe_dir).unwrap();
        let exe = exe_dir.join("clipmonger-daemon");
        fs::write(&exe, b"").unwrap();
        let link = root.join("link-profile");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&exe_dir, &link).unwrap();
        #[cfg(windows)]
        if std::os::windows::fs::symlink_dir(&exe_dir, &link).is_err() {
            let _ = fs::remove_dir_all(&root);
            return; // Windows may not allow symlink creation for this account.
        }
        let kind = classify_portable(&exe, &link).unwrap();
        assert_eq!(kind, PortableKind::Portable);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn identity_failure_unavailable() {
        let root = temp_root("missing");
        let _cleanup_root = Cleanup(root.clone());
        let res = classify_portable(&root.join("missing/daemon"), &root.join("missing-profile"));
        assert!(res.is_err(), "nonexistent canonical should fail closed");
        let _ = fs::remove_dir_all(root);
    }

    // ---- xdg root ----

    #[test]
    fn xdg_roots_are_selected_without_mutating_environment() {
        let root = temp_root("xdg");
        let _cleanup_root = Cleanup(root.clone());
        assert_eq!(
            xdg_root_from(Some(root.clone().into()), None).unwrap(),
            root
        );
        assert!(xdg_root_from(Some("relative/path".into()), Some(root.clone().into())).is_err());
        assert_eq!(
            xdg_root_from(None, Some(root.clone().into())).unwrap(),
            root.join(".config")
        );
        assert!(xdg_root_from(None, None).is_err());
        let _ = fs::remove_dir_all(root);
    }

    // ---- adapter/service: true once, false zero, portable zero, failure continues ----

    #[tokio::test]
    async fn startup_true_once_false_zero() {
        let fake = Arc::new(FakeAutostartCounter::new());
        let svc = AutostartService::with_fake(Arc::clone(&fake), false);
        // startup false => zero ops
        let r = svc.apply_startup(false).await.unwrap();
        assert_eq!(r, StartupOutcome::SkippedDisabled);
        assert_eq!(fake.counts(), (0, 0, 0));
        // A fresh daemon with startup true => exactly one enable.
        let svc = AutostartService::with_fake(Arc::clone(&fake), false);
        let r = svc.apply_startup(true).await.unwrap();
        assert_eq!(r, StartupOutcome::Enabled);
        assert_eq!(fake.counts(), (1, 0, 0));
    }

    #[tokio::test]
    async fn portable_all_methods_zero() {
        let fake = Arc::new(FakeAutostartCounter::new());
        let svc = AutostartService::with_fake(Arc::clone(&fake), true);
        // All methods must perform zero inner calls
        let _ = svc.apply_startup(true).await.unwrap();
        let _ = svc.apply_startup(false).await.unwrap();
        assert_eq!(fake.counts().0, 0);
        let res = svc.apply_explicit(true).await;
        assert!(res.is_err());
        assert_eq!(fake.counts(), (0, 0, 0));
        assert!(svc.apply_explicit(false).await.is_err());
        let res = svc.query().await;
        assert!(res.is_err());
        assert_eq!(fake.counts(), (0, 0, 0));
    }

    #[tokio::test]
    async fn unsupported_explicit_is_unavailable() {
        let svc = AutostartService::unconfigured();
        let res = svc.apply_explicit(true).await;
        assert!(matches!(res, Err(Error::Unavailable(_))));
        let res = svc.apply_explicit(false).await;
        assert!(matches!(res, Err(Error::Unavailable(_))));
    }

    #[tokio::test]
    async fn native_failure_bounded_continues() {
        let fake = Arc::new(FakeAutostartCounter::new());
        fake.set_fail_enable(true);
        let svc = AutostartService::with_fake(Arc::clone(&fake), false);
        let res = svc.apply_startup(true).await;
        assert!(res.is_err());
        assert_eq!(fake.counts().0, 1);
        // Service remains usable; next explicit should still be attempted
        fake.set_fail_enable(false);
        // startup already marked initialized, so need explicit path
        let res = svc.apply_explicit(true).await;
        assert!(res.is_ok());
        assert_eq!(fake.counts().0, 2);
    }

    #[tokio::test]
    async fn setup_called_twice_cannot_apply_twice() {
        let fake = Arc::new(FakeAutostartCounter::new());
        let svc = AutostartService::with_fake(Arc::clone(&fake), false);
        let r1 = svc.apply_startup(true).await.unwrap();
        let r2 = svc.apply_startup(true).await.unwrap();
        assert_eq!(r1, StartupOutcome::Enabled);
        assert_eq!(r2, StartupOutcome::Enabled);
        assert_eq!(fake.counts().0, 1);
    }

    #[tokio::test]
    async fn explicit_same_value_retries() {
        let fake = Arc::new(FakeAutostartCounter::new());
        let svc = AutostartService::with_fake(Arc::clone(&fake), false);
        svc.apply_explicit(true).await.unwrap();
        svc.apply_explicit(true).await.unwrap();
        assert_eq!(fake.counts(), (2, 0, 0));
        svc.apply_explicit(false).await.unwrap();
        svc.apply_explicit(false).await.unwrap();
        assert_eq!(fake.counts(), (2, 2, 0));
    }

    #[tokio::test]
    async fn concurrent_calls_serialized() {
        let fake = Arc::new(FakeAutostartCounter::new());
        let svc = Arc::new(AutostartService::with_fake(Arc::clone(&fake), false));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let s = Arc::clone(&svc);
            handles.push(tokio::spawn(async move {
                let _ = s.apply_explicit(true).await;
            }));
        }
        for h in handles {
            tokio::time::timeout(std::time::Duration::from_secs(4), h)
                .await
                .unwrap()
                .unwrap();
        }
        // All 8 should have been serialized and counted without loss
        assert_eq!(fake.counts().0, 8);
    }

    #[tokio::test]
    async fn actual_state_retains_one_service_and_retries() {
        let root = temp_root("state");
        let _cleanup_root = Cleanup(root.clone());
        let fake = Arc::new(FakeAutostartCounter::new());
        let other = Arc::new(FakeAutostartCounter::new());
        let store = clipmonger_core::Store::open(&root).unwrap();
        let state = crate::DaemonState::new(store);
        state
            .set_autostart_service(AutostartService::with_fake(fake.clone(), false))
            .unwrap();
        assert!(state
            .set_autostart_service(AutostartService::with_fake(other.clone(), false))
            .is_err());
        assert_eq!(
            state.apply_autostart_startup(true).await.unwrap(),
            StartupOutcome::Enabled
        );
        assert_eq!(
            state.apply_autostart_startup(true).await.unwrap(),
            StartupOutcome::Enabled
        );
        state.apply_autostart_explicit(true).await.unwrap();
        state.apply_autostart_explicit(true).await.unwrap();
        state.apply_autostart_explicit(false).await.unwrap();
        state.apply_autostart_explicit(false).await.unwrap();
        assert_eq!(fake.counts(), (3, 2, 0));
        assert_eq!(other.counts(), (0, 0, 0));
        drop(state);
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn actual_state_portable_disabled_unavailable_and_failure() {
        for (portable, enabled, fail) in [
            (true, true, false),
            (true, false, false),
            (false, false, false),
            (false, true, true),
        ] {
            let root = temp_root("state-cases");
            let _cleanup_root = Cleanup(root.clone());
            let fake = Arc::new(FakeAutostartCounter::new());
            fake.set_fail_enable(fail);
            let state = crate::DaemonState::new(clipmonger_core::Store::open(&root).unwrap());
            state
                .set_autostart_service(AutostartService::with_fake(fake.clone(), portable))
                .unwrap();
            let first = state.apply_autostart_startup(enabled).await;
            if fail {
                let err = first.unwrap_err().to_string();
                assert!(!err.contains("fake"));
                assert_eq!(
                    state
                        .apply_autostart_startup(enabled)
                        .await
                        .unwrap_err()
                        .to_string(),
                    err
                );
                fake.set_fail_enable(false);
                state.apply_autostart_explicit(true).await.unwrap();
                assert_eq!(fake.counts(), (2, 0, 0));
            } else if portable {
                assert_eq!(first.unwrap(), StartupOutcome::SkippedPortable);
                assert!(state.apply_autostart_explicit(true).await.is_err());
                assert!(state.apply_autostart_explicit(false).await.is_err());
                assert!(state.query_autostart().await.is_err());
                assert_eq!(fake.counts(), (0, 0, 0));
            } else {
                assert_eq!(first.unwrap(), StartupOutcome::SkippedDisabled);
                assert_eq!(fake.counts(), (0, 0, 0));
            }
            drop(state);
            let _ = fs::remove_dir_all(root);
        }
        let root = temp_root("state-unavailable");
        let _cleanup_root = Cleanup(root.clone());
        let state = crate::DaemonState::new(clipmonger_core::Store::open(&root).unwrap());
        state
            .set_autostart_service(AutostartService::unconfigured())
            .unwrap();
        assert!(state.apply_autostart_startup(true).await.is_err());
        assert!(state.apply_autostart_explicit(false).await.is_err());
        drop(state);
        let _ = fs::remove_dir_all(root);
    }
    #[tokio::test]
    async fn actual_state_preserves_bounded_backend_error_classes() {
        struct FailingAdapter(fn(String) -> Error);
        impl AutostartAdapter for FailingAdapter {
            fn enable(&self) -> Result<()> {
                Err((self.0)("secret-marker".into()))
            }
            fn disable(&self) -> Result<()> {
                Err((self.0)("secret-marker".into()))
            }
            fn is_enabled(&self) -> Result<bool> {
                Err((self.0)("secret-marker".into()))
            }
        }
        let cases: [fn(String) -> Error; 7] = [
            Error::Unavailable,
            Error::Denied,
            Error::Timeout,
            Error::TooLarge,
            Error::Unsupported,
            Error::Io,
            Error::Internal,
        ];
        for make_error in cases {
            let root = temp_root("state-error-class");
            let _cleanup_root = Cleanup(root.clone());
            let state = crate::DaemonState::new(clipmonger_core::Store::open(&root).unwrap());
            state
                .set_autostart_service(AutostartService::with_adapter(
                    Arc::new(FailingAdapter(make_error)),
                    false,
                ))
                .unwrap();
            let expected = std::mem::discriminant(&make_error(String::new()));
            for error in [
                state.apply_autostart_startup(true).await.unwrap_err(),
                state.apply_autostart_startup(true).await.unwrap_err(),
                state.apply_autostart_explicit(true).await.unwrap_err(),
                state.apply_autostart_explicit(false).await.unwrap_err(),
                state.query_autostart().await.unwrap_err(),
            ] {
                assert_eq!(std::mem::discriminant(&error), expected);
                let message = error.to_string();
                assert!(!message.contains("secret-marker"));
                assert!(message.len() <= 80);
            }
        }
    }

    #[tokio::test]
    async fn selection_and_state_apply_for_session_combinations() {
        for (_wayland, _display, _headless) in [
            (true, true, false),
            (true, false, false),
            (false, true, false),
            (false, false, false),
            (true, true, true),
        ] {
            for (classification, supported, expected) in [
                (Ok(PortableKind::Portable), true, Selection::Portable),
                (Ok(PortableKind::Installed), true, Selection::Native),
                (Ok(PortableKind::Installed), false, Selection::Unavailable),
                (Err(unavailable()), true, Selection::Unavailable),
            ] {
                let chosen = selection(&classification, supported);
                assert_eq!(chosen, expected);
                let root = temp_root("selection");
                let _cleanup = Cleanup(root.clone());
                let fake = Arc::new(FakeAutostartCounter::new());
                let service = match chosen {
                    Selection::Portable => AutostartService::with_fake(fake.clone(), true),
                    Selection::Native => AutostartService::with_fake(fake.clone(), false),
                    Selection::Unavailable => AutostartService::unconfigured(),
                };
                let state = crate::DaemonState::new(clipmonger_core::Store::open(&root).unwrap());
                state.set_autostart_service(service).unwrap();
                let result = state.apply_autostart_startup(true).await;
                match expected {
                    Selection::Portable => {
                        assert_eq!(result.unwrap(), StartupOutcome::SkippedPortable);
                        assert_eq!(fake.counts(), (0, 0, 0));
                    }
                    Selection::Native => {
                        assert_eq!(result.unwrap(), StartupOutcome::Enabled);
                        assert_eq!(fake.counts(), (1, 0, 0));
                    }
                    Selection::Unavailable => {
                        assert!(result.is_err());
                        assert_eq!(fake.counts(), (0, 0, 0));
                    }
                }
            }
        }
    }

    struct BlockingFake {
        entered: std::sync::mpsc::Sender<()>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
        active: std::sync::atomic::AtomicUsize,
        overlap: std::sync::atomic::AtomicBool,
        calls: std::sync::atomic::AtomicUsize,
        fail: bool,
    }
    impl AutostartAdapter for BlockingFake {
        fn enable(&self) -> Result<()> {
            use std::sync::atomic::Ordering::SeqCst;
            if self.active.fetch_add(1, SeqCst) != 0 {
                self.overlap.store(true, SeqCst);
            }
            let n = self.calls.fetch_add(1, SeqCst);
            if n == 0 {
                let _ = self.entered.send(());
                let _ = self
                    .release
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(3));
            }
            self.active.fetch_sub(1, SeqCst);
            if self.fail {
                Err(Error::Io("secret path".into()))
            } else {
                Ok(())
            }
        }
        fn disable(&self) -> Result<()> {
            self.enable()
        }
        fn is_enabled(&self) -> Result<bool> {
            self.enable().map(|()| false)
        }
    }

    fn blocking_fake(
        fail: bool,
    ) -> (
        Arc<BlockingFake>,
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::Sender<()>,
    ) {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        (
            Arc::new(BlockingFake {
                entered: entered_tx,
                release: Mutex::new(release_rx),
                active: 0.into(),
                overlap: false.into(),
                calls: 0.into(),
                fail,
            }),
            entered_rx,
            release_tx,
        )
    }

    #[tokio::test]
    async fn cancelled_caller_keeps_operation_serialized() {
        use std::sync::atomic::Ordering::SeqCst;
        let root = temp_root("cancel");
        let _cleanup_root = Cleanup(root.clone());
        let (fake, entered, release) = blocking_fake(false);
        let state = Arc::new(crate::DaemonState::new(
            clipmonger_core::Store::open(&root).unwrap(),
        ));
        state
            .set_autostart_service(AutostartService::with_adapter(fake.clone(), false))
            .unwrap();
        let first_state = state.clone();
        let first = tokio::spawn(async move { first_state.apply_autostart_explicit(true).await });
        tokio::task::spawn_blocking(move || {
            entered.recv_timeout(std::time::Duration::from_secs(2))
        })
        .await
        .unwrap()
        .unwrap();
        first.abort();
        let second_state = state.clone();
        let mut second =
            tokio::spawn(async move { second_state.apply_autostart_explicit(false).await });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut second)
                .await
                .is_err()
        );
        release.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(4), second)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(fake.calls.load(SeqCst), 2);
        assert!(!fake.overlap.load(SeqCst));
        drop(state);
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn concurrent_startup_waits_for_failed_first_result() {
        use std::sync::atomic::Ordering::SeqCst;
        let root = temp_root("startup-wait");
        let _cleanup_root = Cleanup(root.clone());
        let (fake, entered, release) = blocking_fake(true);
        let state = Arc::new(crate::DaemonState::new(
            clipmonger_core::Store::open(&root).unwrap(),
        ));
        state
            .set_autostart_service(AutostartService::with_adapter(fake.clone(), false))
            .unwrap();
        let first_state = state.clone();
        let first = tokio::spawn(async move { first_state.apply_autostart_startup(true).await });
        tokio::task::spawn_blocking(move || {
            entered.recv_timeout(std::time::Duration::from_secs(2))
        })
        .await
        .unwrap()
        .unwrap();
        let second_state = state.clone();
        let mut second =
            tokio::spawn(async move { second_state.apply_autostart_startup(true).await });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut second)
                .await
                .is_err()
        );
        release.send(()).unwrap();
        let a = tokio::time::timeout(std::time::Duration::from_secs(4), first)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .to_string();
        let b = tokio::time::timeout(std::time::Duration::from_secs(4), second)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .to_string();
        assert_eq!(a, b);
        assert!(!a.contains("secret"));
        assert_eq!(fake.calls.load(SeqCst), 1);
        drop(state);
        let _ = fs::remove_dir_all(root);
    }
}
