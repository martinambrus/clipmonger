//! Bounded metadata-only runtime diagnostics, separate from the security audit.
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAX_ACTIVE_BYTES: u64 = 1024 * 1024;
pub const MAX_RECORD_BYTES: usize = 4096;
pub const MAX_TAIL_BYTES: usize = 256 * 1024;
pub const MAX_TAIL_LINES: usize = 1000;
pub const MAX_FIELD_BYTES: usize = 512;
pub const MAX_FIELDS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Daemon,
    Ui,
}
impl Role {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Daemon => "daemon",
            Self::Ui => "ui",
        }
    }
    #[must_use]
    pub fn file_name(self) -> &'static str {
        match self {
            Self::Daemon => "daemon.log",
            Self::Ui => "ui.log",
        }
    }
    #[must_use]
    pub fn lock_name(self) -> &'static str {
        match self {
            Self::Daemon => "daemon.log.lock",
            Self::Ui => "ui.log.lock",
        }
    }
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "daemon" => Some(Self::Daemon),
            "ui" => Some(Self::Ui),
            _ => None,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warn,
    Error,
}
impl Severity {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SinkConfig {
    pub max_active_bytes: u64,
    pub max_record_bytes: usize,
    pub max_tail_bytes: usize,
    pub max_tail_lines: usize,
}
impl Default for SinkConfig {
    fn default() -> Self {
        Self {
            max_active_bytes: MAX_ACTIVE_BYTES,
            max_record_bytes: MAX_RECORD_BYTES,
            max_tail_bytes: MAX_TAIL_BYTES,
            max_tail_lines: MAX_TAIL_LINES,
        }
    }
}
impl SinkConfig {
    fn validate(&self) -> Result<(), String> {
        // 128 leaves space for every mandatory field, even at the largest timestamp.
        if !(128..=MAX_RECORD_BYTES).contains(&self.max_record_bytes)
            || self.max_active_bytes < self.max_record_bytes as u64
            || self.max_active_bytes > MAX_ACTIVE_BYTES
            || !(1..=MAX_TAIL_BYTES).contains(&self.max_tail_bytes)
            || !(1..=MAX_TAIL_LINES).contains(&self.max_tail_lines)
        {
            return Err("runtime_log.invalid_limits".into());
        }
        Ok(())
    }
}
fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

/// These classes never call Display or retain backend-provided messages/paths.
#[must_use]
pub fn core_class(error: &crate::CoreError) -> &'static str {
    use crate::CoreError;
    match error {
        CoreError::Backend { backend, .. } if backend == "sqlite" => "sqlite",
        CoreError::Backend { kind, .. } => match kind {
            crate::error::BackendKind::Unavailable => "backend_unavailable",
            crate::error::BackendKind::Denied => "backend_denied",
            crate::error::BackendKind::Timeout => "backend_timeout",
            crate::error::BackendKind::TooLarge => "backend_too_large",
            crate::error::BackendKind::Unsupported => "backend_unsupported",
            crate::error::BackendKind::Io => "backend_io",
            crate::error::BackendKind::Internal => "backend_internal",
        },
        _ => error.code(),
    }
}
/// Preserve available typed OS codes; SQLite's existing `CoreError` wrapper has
/// already erased its numeric code, so do not parse its arbitrary Display text.
#[must_use]
pub fn core_code(error: &crate::CoreError) -> String {
    match error {
        crate::CoreError::Io(error) => error
            .raw_os_error()
            .map_or_else(|| "io".into(), |code| code.to_string()),
        _ => error.code().into(),
    }
}
fn io_status(operation: &str, error: &std::io::Error) -> String {
    // Only internal operation identifiers and typed OS codes enter this formatter.
    format!(
        "runtime_log.{operation}:{:?}:{}",
        error.kind(),
        error.raw_os_error().unwrap_or(0)
    )
}
fn logs_dir(profile: &Path) -> PathBuf {
    profile.join("logs")
}
fn active_path(profile: &Path, role: Role) -> PathBuf {
    logs_dir(profile).join(role.file_name())
}
fn backup_path(profile: &Path, role: Role) -> PathBuf {
    logs_dir(profile).join(format!("{}.1", role.file_name()))
}
fn is_link(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    false
}
fn check_dir(profile: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(logs_dir(profile)) {
        Ok(m) if is_link(&m) || !m.is_dir() => Err("runtime_log.refused_logs_directory".into()),
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io_status("stat_directory", &e)),
    }
}
fn check_file(path: &Path) -> Result<Option<u64>, String> {
    match fs::symlink_metadata(path) {
        Ok(m) if is_link(&m) || !m.file_type().is_file() => Err("runtime_log.refused_file".into()),
        Ok(m) => Ok(Some(m.len())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_status("stat_file", &e)),
    }
}

// Append only complete escaped characters. Capacity and traversal are bounded before
// copying input; even a huge caller string or slice cannot enlarge the allocation.
fn json_string(out: &mut String, value: &str, budget: usize) {
    let end = out.len() + budget;
    out.push('"');
    for c in value.chars() {
        let cost = if matches!(c, '"' | '\\') {
            2
        } else if c.is_control() {
            6
        } else {
            c.len_utf8()
        };
        if out.len() + cost + 1 > end {
            break;
        }
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            _ if c.is_control() => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            _ => out.push(c),
        }
    }
    out.push('"');
}
fn format_record(
    config: &SinkConfig,
    sev: Severity,
    event: &str,
    fields: &[(&str, &str)],
) -> String {
    let mut out = String::with_capacity(config.max_record_bytes);
    let _ = write!(
        out,
        "{{\"ts\":{},\"sev\":\"{}\",\"event\":",
        now_ms(),
        sev.as_str()
    );
    let available = config.max_record_bytes - out.len() - 2;
    json_string(&mut out, event, available.min(96));
    for (key, value) in fields.iter().take(MAX_FIELDS) {
        // Stable metadata vocabulary; payload/secret/path fields cannot be admitted.
        if !matches!(
            *key,
            "operation" | "class" | "code" | "origin" | "count" | "backend"
        ) {
            continue;
        }
        let overhead = key.len() + 6; // comma + quoted key + colon + quoted value
        let remaining = config.max_record_bytes.saturating_sub(out.len() + 2);
        if remaining < overhead {
            break;
        }
        out.push(',');
        out.push('"');
        out.push_str(key);
        out.push_str("\":");
        json_string(
            &mut out,
            value,
            (remaining - key.len() - 4).min(MAX_FIELD_BYTES),
        );
    }
    out.push_str("}\n");
    out
}

pub struct RuntimeSink {
    profile: PathBuf,
    role: Role,
    config: SinkConfig,
    // Held for the entire sink lifetime, including between writes/rotations.
    _lock_file: File,
    write_mutex: Mutex<()>,
    last_error: Mutex<Option<String>>,
}
impl RuntimeSink {
    /// Open a process-role sink with the default byte and record limits.
    ///
    /// # Errors
    /// Returns a stable status if the profile or managed paths are refused, the
    /// role lock is unavailable, or initialization and initial rotation fail.
    pub fn new(profile: &Path, role: Role) -> Result<Self, String> {
        Self::new_with_config(profile, role, SinkConfig::default())
    }
    /// Open a process-role sink with validated limits and a held role lock.
    ///
    /// # Errors
    /// Returns a stable status for invalid limits, an empty profile, refused
    /// managed paths, lock contention, or initialization and initial rotation IO.
    pub fn new_with_config(profile: &Path, role: Role, config: SinkConfig) -> Result<Self, String> {
        config.validate()?;
        if profile.as_os_str().is_empty() {
            return Err("runtime_log.empty_profile".into());
        }
        if !check_dir(profile)? {
            fs::create_dir_all(logs_dir(profile)).map_err(|e| io_status("create_directory", &e))?;
        }
        check_dir(profile)?;
        let lock_path = logs_dir(profile).join(role.lock_name());
        check_file(&lock_path)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)
            .map_err(|e| io_status("open_lock", &e))?;
        lock.try_lock()
            .map_err(|_| "runtime_log.lock_unavailable".to_string())?;
        let active = active_path(profile, role);
        let backup = backup_path(profile, role);
        if check_file(&backup)?.is_some_and(|n| n > config.max_active_bytes) {
            Self::compact_locked(&backup, &config)?;
        }
        if check_file(&active)?.is_some_and(|n| n > config.max_active_bytes) {
            Self::rotate_locked(&active, &backup_path(profile, role), &config)?;
        }
        Ok(Self {
            profile: profile.into(),
            role,
            config,
            _lock_file: lock,
            write_mutex: Mutex::new(()),
            last_error: Mutex::new(None),
        })
    }
    fn compact_locked(path: &Path, config: &SinkConfig) -> Result<(), String> {
        let lines = tail_file_bounded(
            path,
            MAX_TAIL_LINES,
            usize::try_from(config.max_active_bytes)
                .map_err(|_| "runtime_log.invalid_limits".to_string())?,
        )?;
        let mut file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(path)
            .map_err(|e| io_status("compact_backup", &e))?;
        for line in lines {
            file.write_all(line.as_bytes())
                .and_then(|()| file.write_all(b"\n"))
                .map_err(|e| io_status("compact_backup", &e))?;
        }
        file.flush().map_err(|e| io_status("flush_backup", &e))
    }
    fn rotate_locked(active: &Path, backup: &Path, config: &SinkConfig) -> Result<(), String> {
        let size = check_file(active)?;
        let oversized = size.is_some_and(|n| n > config.max_active_bytes);
        // Existing oversized logs keep only a bounded suffix of whole records.
        let suffix = if oversized {
            Some(tail_file_bounded(
                active,
                MAX_TAIL_LINES,
                usize::try_from(config.max_active_bytes)
                    .map_err(|_| "runtime_log.invalid_limits".to_string())?,
            )?)
        } else {
            None
        };
        if check_file(backup)?.is_some() {
            fs::remove_file(backup).map_err(|e| io_status("remove_backup", &e))?;
        }
        if size.is_some() {
            fs::rename(active, backup).map_err(|e| io_status("rotate", &e))?;
            if let Some(lines) = suffix {
                let mut file = OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(backup)
                    .map_err(|e| io_status("compact_backup", &e))?;
                for line in lines {
                    file.write_all(line.as_bytes())
                        .and_then(|()| file.write_all(b"\n"))
                        .map_err(|e| io_status("compact_backup", &e))?;
                }
                file.flush().map_err(|e| io_status("flush_backup", &e))?;
            }
        }
        Ok(())
    }
    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().map_or_else(
            |_| Some("runtime_log.poisoned_status".into()),
            |g| g.clone(),
        )
    }
    pub fn role(&self) -> Role {
        self.role
    }
    pub fn active_path(&self) -> PathBuf {
        active_path(&self.profile, self.role)
    }
    pub fn backup_path(&self) -> PathBuf {
        backup_path(&self.profile, self.role)
    }
    /// Append one bounded diagnostic record, rotating the active file as needed.
    ///
    /// # Errors
    /// Returns a stable status if the writer is poisoned, managed paths are
    /// refused, or checking, rotating, opening, writing or flushing fails.
    pub fn emit(&self, sev: Severity, event: &str, fields: &[(&str, &str)]) -> Result<(), String> {
        let Ok(_guard) = self.write_mutex.lock() else {
            let error = "runtime_log.poisoned_writer".to_string();
            if let Ok(mut status) = self.last_error.lock() {
                *status = Some(error.clone());
            }
            return Err(error);
        };
        let result: Result<(), String> = (|| {
            if !check_dir(&self.profile)? {
                return Err("runtime_log.missing_directory".into());
            }
            let active = self.active_path();
            let record = format_record(&self.config, sev, event, fields);
            if check_file(&active)?.is_some_and(|n| {
                n.saturating_add(record.len() as u64) > self.config.max_active_bytes
            }) {
                Self::rotate_locked(&active, &self.backup_path(), &self.config)?;
            }
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(active)
                .map_err(|e| io_status("open", &e))?;
            file.write_all(record.as_bytes())
                .map_err(|e| io_status("write", &e))?;
            file.flush().map_err(|e| io_status("flush", &e))?;
            drop(file); // No active writer is retained across a Windows rename.
            Ok(())
        })();
        if let Ok(mut status) = self.last_error.lock() {
            *status = result.as_ref().err().cloned();
        }
        result
    }
    /// Read a bounded suffix of whole lines from this sink's active file.
    ///
    /// # Errors
    /// Returns a stable status for refused managed paths, IO failures or invalid
    /// UTF-8 in the selected complete lines.
    pub fn tail(&self, want: usize) -> Result<Vec<String>, String> {
        tail(&self.profile, self.role, want, &self.config)
    }
}

fn tail_file_bounded(path: &Path, want: usize, max_bytes: usize) -> Result<Vec<String>, String> {
    let Some(len) = check_file(path)? else {
        return Ok(Vec::new());
    };
    if want == 0 {
        return Ok(Vec::new());
    }
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()), // rotation race
        Err(e) => return Err(io_status("open_tail", &e)),
    };
    // One preceding byte identifies whether the suffix starts at a record boundary.
    let start = len.saturating_sub(max_bytes as u64);
    let read_start = start.saturating_sub(1);
    file.seek(SeekFrom::Start(read_start))
        .map_err(|e| io_status("seek_tail", &e))?;
    let mut bytes = Vec::with_capacity(max_bytes + 1);
    file.take((max_bytes + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| io_status("read_tail", &e))?;
    let mut first = 0;
    if start > 0 {
        first = if bytes.first() == Some(&b'\n') {
            1
        } else {
            bytes
                .iter()
                .position(|b| *b == b'\n')
                .map_or(bytes.len(), |n| n + 1)
        };
    }
    // A concurrently interrupted final write is not a complete record.
    let end = bytes
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(first, |n| n + 1)
        .max(first);
    let text = std::str::from_utf8(&bytes[first..end])
        .map_err(|_| "runtime_log.invalid_utf8".to_string())?;
    let mut lines: Vec<String> = text
        .lines()
        .rev()
        .take(want.min(MAX_TAIL_LINES))
        .map(str::to_owned)
        .collect();
    lines.reverse();
    Ok(lines)
}
/// Read bounded complete lines from the selected role's active file only.
///
/// # Errors
/// Returns a stable status for invalid limits, an empty profile, refused managed
/// paths, IO failures or invalid UTF-8 in the selected complete lines.
pub fn tail(
    profile: &Path,
    role: Role,
    want: usize,
    config: &SinkConfig,
) -> Result<Vec<String>, String> {
    config.validate()?;
    if profile.as_os_str().is_empty() {
        return Err("runtime_log.empty_profile".into());
    }
    if !check_dir(profile)? {
        return Ok(Vec::new());
    }
    tail_file_bounded(
        &active_path(profile, role),
        want.min(config.max_tail_lines),
        config.max_tail_bytes,
    )
}

#[derive(Default)]
struct GlobalSlot {
    sink: Option<RuntimeSink>,
    init_error: Option<String>,
}
static DAEMON_SINK: OnceLock<Mutex<GlobalSlot>> = OnceLock::new();
static UI_SINK: OnceLock<Mutex<GlobalSlot>> = OnceLock::new();
fn global_slot(role: Role) -> &'static Mutex<GlobalSlot> {
    match role {
        Role::Daemon => DAEMON_SINK.get_or_init(|| Mutex::new(GlobalSlot::default())),
        Role::Ui => UI_SINK.get_or_init(|| Mutex::new(GlobalSlot::default())),
    }
}
/// Initialize this process's role sink with default limits.
///
/// # Errors
/// Returns a stable status if the process slot is poisoned or already initialized,
/// or if opening the sink fails. Initialization failures remain in role status.
pub fn init_global(profile: &Path, role: Role) -> Result<(), String> {
    init_global_with_config(profile, role, SinkConfig::default())
}
/// Initialize this process's role sink with validated custom limits.
///
/// # Errors
/// Returns a stable status if the process slot is poisoned or already initialized,
/// or if opening the sink fails. Initialization failures remain in role status.
pub fn init_global_with_config(
    profile: &Path,
    role: Role,
    config: SinkConfig,
) -> Result<(), String> {
    let mut slot = global_slot(role)
        .lock()
        .map_err(|_| "runtime_log.poisoned_global".to_string())?;
    if slot.sink.is_some() {
        return Err("runtime_log.already_initialized".into());
    }
    match RuntimeSink::new_with_config(profile, role, config) {
        Ok(sink) => {
            slot.sink = Some(sink);
            slot.init_error = None;
            Ok(())
        }
        Err(error) => {
            slot.init_error = Some(error.clone());
            Err(error)
        }
    }
}
/// Append a bounded diagnostic through this process's initialized role sink.
///
/// # Errors
/// Returns a stable status for a poisoned or uninitialized process slot, a retained
/// initialization failure, or a sink write or rotation failure.
pub fn global_emit(
    role: Role,
    sev: Severity,
    event: &str,
    fields: &[(&str, &str)],
) -> Result<(), String> {
    let slot = global_slot(role)
        .lock()
        .map_err(|_| "runtime_log.poisoned_global".to_string())?;
    slot.sink
        .as_ref()
        .ok_or_else(|| {
            slot.init_error
                .clone()
                .unwrap_or_else(|| "runtime_log.not_initialized".into())
        })?
        .emit(sev, event, fields)
}
/// Process-local status: callers must never query another process's role slot.
#[must_use]
pub fn global_status(role: Role) -> Option<String> {
    global_slot(role).lock().map_or_else(
        |_| Some("runtime_log.poisoned_global".into()),
        |slot| {
            slot.init_error
                .clone()
                .or_else(|| slot.sink.as_ref().and_then(RuntimeSink::last_error))
        },
    )
}
/// Read a role's fixed active file with default tail limits.
///
/// # Errors
/// Returns a stable status for an empty profile, refused managed paths, IO failures
/// or invalid UTF-8 in the selected complete lines.
pub fn global_tail(profile: &Path, role: Role, want: usize) -> Result<Vec<String>, String> {
    tail(profile, role, want, &SinkConfig::default())
}
