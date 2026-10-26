//! Daemon skeleton: single-instance lock, rendezvous files, capability
//! aggregation, headless script replay, and the IPC request handler.
//!
//! Implements the frozen binary contract (seg-02 S1/S9, D-F20/F21/F22).
//! No payload bytes are ever logged; only metadata reaches stderr.

use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use clipmonger_core::{CoreError, Store};
use clipmonger_ipc::proto::IpcError;
use clipmonger_platform::types::{BackendCaps, Level, PasteStrategy};
use serde_json::Value;

/// Protocol tag written to the rendezvous file.
pub const PROTOCOL: &str = "ipc/v1";
/// Daemon crate version reported by `status`.
pub const DAEMON_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Note carried by every skeleton capability snapshot.
pub const SKELETON_NOTE: &str = "no backends in foundation skeleton";

/// Single-instance profile lock backed by `std::fs::File::try_lock`.
///
/// The lock file is `<profile>/daemon.lock`. A second `acquire` on the same
/// profile (same or other process) fails with a message containing
/// `already_running`. The lock releases when the guard drops.
pub struct DaemonLock {
    /// Held open for the guard lifetime; drop closes and releases.
    _file: fs::File,
}

impl DaemonLock {
    /// Acquire the exclusive profile lock.
    ///
    /// # Errors
    ///
    /// Returns an error containing `already_running` when another daemon
    /// holds the lock, or an I/O error when the profile cannot be prepared.
    pub fn acquire(profile_dir: &Path) -> Result<Self, CoreError> {
        fs::create_dir_all(profile_dir).map_err(CoreError::Io)?;
        let file = fs::File::create(profile_dir.join("daemon.lock")).map_err(CoreError::Io)?;
        file.try_lock().map_err(|_| CoreError::Unsupported {
            message: "already_running: another daemon holds the profile lock".to_string(),
        })?;
        Ok(Self { _file: file })
    }
}

/// Write `<profile>/rendezvous.json` as `{socket, protocol}` (never a token).
///
/// # Errors
///
/// Returns an I/O error when the profile dir or file cannot be written.
pub fn write_rendezvous(profile_dir: &Path, socket_path: &Path) -> Result<(), CoreError> {
    fs::create_dir_all(profile_dir).map_err(CoreError::Io)?;
    let body = serde_json::json!({
        "socket": socket_path.to_string_lossy(),
        "protocol": PROTOCOL,
    });
    let text = serde_json::to_string(&body).unwrap_or_else(|_| {
        format!(
            r#"{{"socket":"{}","protocol":"{PROTOCOL}"}}"#,
            socket_path.to_string_lossy()
        )
    });
    fs::write(profile_dir.join("rendezvous.json"), text).map_err(CoreError::Io)?;
    Ok(())
}

/// Read `<profile>/rendezvous.json`, returning `(socket_path, protocol)`.
///
/// # Errors
///
/// Returns an error when the file is missing or lacks a `socket` string.
pub fn read_rendezvous(profile_dir: &Path) -> Result<(PathBuf, String), CoreError> {
    let raw = fs::read_to_string(profile_dir.join("rendezvous.json")).map_err(CoreError::Io)?;
    let value: Value = serde_json::from_str(&raw).map_err(|e| CoreError::Config {
        message: format!("rendezvous is not JSON: {e}"),
    })?;
    let socket = value
        .get("socket")
        .and_then(Value::as_str)
        .ok_or_else(|| CoreError::Config {
            message: "rendezvous lacks a socket path".to_string(),
        })?;
    let protocol = value
        .get("protocol")
        .and_then(Value::as_str)
        .unwrap_or(PROTOCOL)
        .to_string();
    Ok((PathBuf::from(socket), protocol))
}

// ---------------------------------------------------------------------------
// Elevated-helper routing (seg-04 S5, WIN-F6 audit)
// ---------------------------------------------------------------------------

/// Gesture-token TTL in ms (matches the backend helper contract; kept as a
/// literal here so the daemon builds without the backend on non-Windows).
pub const GESTURE_TTL_MS: i64 = 30_000;
/// Helper heartbeat deadline: 3 missed 15 s beats.
pub const HELPER_DEAD_AFTER_MS: i64 = 45_000;

/// Append one JSONL audit event to `<profile>/audit.log`.
///
/// Callers pass metadata only (action, op hash, target exe name, ts,
/// result); payload bytes must never reach this function.
pub fn append_audit(profile_dir: &Path, event: Value) {
    if profile_dir.as_os_str().is_empty() {
        return;
    }
    let mut stamped = event;
    if stamped.get("ts").is_none() {
        stamped["ts"] = Value::from(now_ms());
    }
    let mut line = serde_json::to_string(&stamped).unwrap_or_default();
    if line.is_empty() {
        return;
    }
    line.push('\n');
    if fs::create_dir_all(profile_dir).is_err() {
        return;
    }
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(profile_dir.join("audit.log"))
    {
        let _ = file.write_all(line.as_bytes());
    }
}

/// Elevated-helper supervisor: on-demand runas spawn from a protected path
/// only, reverse-connect pipe, peer verification, 15 s heartbeat with
/// 45 s death, audit entries, refusal latching into caps paste Degraded.
///
/// The runas/UAC served leg is manual QA by the user; automated runs never
/// spawn (no spawn call exists outside the Windows supervisor thread).
pub struct HelperSupervisor {
    /// Last worker heartbeat (unix ms), shared with the supervisor thread.
    beat: std::sync::Arc<Mutex<Option<i64>>>,
    /// Live job channel to the supervisor thread (Windows only).
    #[cfg(all(windows, feature = "backend-windows"))]
    link: Option<HelperLink>,
}

/// One helper job handed to the supervisor thread.
#[cfg(all(windows, feature = "backend-windows"))]
struct SupervisorJob {
    /// Validated high-integrity target process id.
    target_pid: u32,
    /// Input text bytes (UTF-8).
    payload: String,
    /// Outcome channel back to the IPC handler.
    reply: std::sync::mpsc::Sender<Result<(), String>>,
}

/// Live channel to the Windows supervisor thread.
#[cfg(all(windows, feature = "backend-windows"))]
struct HelperLink {
    /// Job queue owned by the supervisor thread.
    jobs: std::sync::mpsc::Sender<SupervisorJob>,
}

impl HelperSupervisor {
    /// Idle supervisor with no helper and no heartbeat.
    #[must_use]
    pub fn new() -> Self {
        Self {
            beat: std::sync::Arc::new(Mutex::new(None)),
            #[cfg(all(windows, feature = "backend-windows"))]
            link: None,
        }
    }

    /// Record a worker heartbeat at `at_ms`.
    pub fn note_beat(&self, at_ms: i64) {
        if let Ok(mut slot) = self.beat.lock() {
            *slot = Some(at_ms);
        }
    }

    /// True while within [`HELPER_DEAD_AFTER_MS`] of the last heartbeat.
    #[must_use]
    pub fn alive(&self, at_ms: i64) -> bool {
        self.beat
            .lock()
            .ok()
            .and_then(|slot| *slot)
            .is_some_and(|beat| at_ms >= beat && at_ms - beat <= HELPER_DEAD_AFTER_MS)
    }

    /// Ensure the supervisor thread exists (Windows only); elsewhere there
    /// is no helper channel and every elevated paste stays `Unavailable`.
    #[cfg(all(windows, feature = "backend-windows"))]
    fn ensure_link(&mut self, profile: &Path) -> std::sync::mpsc::Sender<SupervisorJob> {
        if let Some(link) = &self.link {
            return link.jobs.clone();
        }
        let (jobs_tx, jobs_rx) = std::sync::mpsc::channel::<SupervisorJob>();
        let beat = std::sync::Arc::clone(&self.beat);
        let profile = profile.to_path_buf();
        std::thread::spawn(move || supervisor_thread(profile, beat, jobs_rx));
        self.link = Some(HelperLink {
            jobs: jobs_tx.clone(),
        });
        jobs_tx
    }

    /// Drop a dead channel so the next elevated paste re-spawns (+prompt).
    #[cfg(all(windows, feature = "backend-windows"))]
    fn drop_link(&mut self) {
        self.link = None;
    }
}

impl Default for HelperSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

/// Supervisor thread body (Windows only): create the reverse-connect pipe,
/// runas-spawn the worker, verify the peer, then serve jobs with a 15 s
/// heartbeat watch (3 misses = dead). Every wait is bounded; UAC cancel or
/// peer mismatch fails jobs with an `elevation refused` message.
///
/// One long state machine by design (owned thread entry takes its channels
/// by value), so length and by-value lints are allowed here.
#[cfg(all(windows, feature = "backend-windows"))]
#[allow(clippy::too_many_lines, clippy::needless_pass_by_value)]
fn supervisor_thread(
    profile: PathBuf,
    beat: std::sync::Arc<Mutex<Option<i64>>>,
    jobs: std::sync::mpsc::Receiver<SupervisorJob>,
) {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
    use tokio::net::windows::named_pipe::{PipeMode, ServerOptions};

    let fail_all = |jobs: &std::sync::mpsc::Receiver<SupervisorJob>, message: &str| {
        append_audit(
            &profile,
            serde_json::json!({"kind": "helper", "action": "refused", "result": message}),
        );
        while let Ok(job) = jobs.try_recv() {
            let _ = job.reply.send(Err(message.to_string()));
        }
    };

    let note_beat = |at_ms: i64| {
        if let Ok(mut slot) = beat.lock() {
            *slot = Some(at_ms);
        }
    };

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            fail_all(
                &jobs,
                &format!("elevation refused: helper runtime failed: {e}"),
            );
            return;
        }
    };

    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("clipmonger-daemon"));
    let exe_dir = exe
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    if profile.starts_with(&exe_dir) {
        fail_all(
            &jobs,
            "unavailable: portable layout, elevated route unavailable",
        );
        return;
    }
    if !clipmonger_backend_windows::helper::protected_path_ok(
        &exe,
        &clipmonger_backend_windows::program_files_dir(),
    ) {
        fail_all(
            &jobs,
            "unavailable: unprotected install path, elevated route unavailable",
        );
        return;
    }

    let pipe_name = format!(
        r"\\.\pipe\clipmonger-help-{}",
        clipmonger_backend_windows::helper::random_pipe_segment()
    );
    let listener = match ServerOptions::new()
        .first_pipe_instance(true)
        .reject_remote_clients(true)
        .pipe_mode(PipeMode::Byte)
        .create(&pipe_name)
    {
        Ok(listener) => listener,
        Err(e) => {
            fail_all(
                &jobs,
                &format!("elevation refused: helper pipe failed: {e}"),
            );
            return;
        }
    };
    append_audit(
        &profile,
        serde_json::json!({"kind": "helper", "action": "spawn", "result": "runas prompt"}),
    );
    if let Err(e) =
        clipmonger_backend_windows::spawn_elevated_worker(&exe.to_string_lossy(), &pipe_name)
    {
        fail_all(
            &jobs,
            &format!("elevation refused: helper spawn refused: {e}"),
        );
        return;
    }
    let server = match runtime.block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(90), listener.connect()).await
    }) {
        Ok(Ok(())) => listener,
        Ok(Err(e)) => {
            fail_all(
                &jobs,
                &format!("elevation refused: helper connect failed: {e}"),
            );
            return;
        }
        Err(_) => {
            fail_all(
                &jobs,
                "elevation refused: helper connect timed out (UAC consent?)",
            );
            return;
        }
    };
    if !helper_peer_ok(&server, &exe) {
        append_audit(
            &profile,
            serde_json::json!({"kind": "helper", "action": "alert", "result": "peer mismatch"}),
        );
        fail_all(&jobs, "elevation refused: helper peer mismatch");
        return;
    }
    note_beat(now_ms());
    let (read_half, mut write_half) = tokio::io::split(server);
    let mut reader = tokio::io::BufReader::new(read_half);
    let mut missed: u32 = 0;
    loop {
        match jobs.recv_timeout(std::time::Duration::from_secs(15)) {
            Ok(job) => {
                let line = match clipmonger_backend_windows::helper::encode_job(
                    &clipmonger_backend_windows::helper::Job {
                        job_id: 1,
                        target_pid: job.target_pid,
                        op: clipmonger_backend_windows::helper::JobOp::SendInputBatch,
                        payload: job.payload.clone(),
                    },
                ) {
                    Ok(line) => line,
                    Err(e) => {
                        let _ = job.reply.send(Err(format!("backend: {e}")));
                        continue;
                    }
                };
                let outcome = runtime.block_on(async {
                    if tokio::time::timeout(
                        std::time::Duration::from_secs(120),
                        write_half.write_all(line.as_bytes()),
                    )
                    .await
                    .is_err()
                    {
                        return Err("helper write timed out".to_string());
                    }
                    let mut result_line = String::new();
                    if tokio::time::timeout(
                        std::time::Duration::from_secs(120),
                        reader.read_line(&mut result_line),
                    )
                    .await
                    .is_err()
                    {
                        return Err("helper result timed out".to_string());
                    }
                    match serde_json::from_str::<Value>(&result_line) {
                        Ok(value) if value.get("heartbeat").is_some() => {
                            Ok("heartbeat".to_string())
                        }
                        Ok(value) => {
                            if value.get("ok").and_then(Value::as_bool) == Some(true) {
                                Ok(String::new())
                            } else {
                                Err(value
                                    .get("error")
                                    .and_then(Value::as_str)
                                    .unwrap_or("helper refused")
                                    .to_string())
                            }
                        }
                        Err(_) => Err("helper result unreadable".to_string()),
                    }
                });
                match &outcome {
                    Ok(marker) if marker == "heartbeat" => {
                        note_beat(now_ms());
                        let _ = job.reply.send(Err("helper busy: retry".to_string()));
                    }
                    Ok(marker) if marker.is_empty() => {
                        note_beat(now_ms());
                        append_audit(
                            &profile,
                            serde_json::json!({
                                "kind": "helper",
                                "action": "job",
                                "op_hash": fnv_hex(job.payload.as_bytes()),
                                "result": "ok",
                            }),
                        );
                        let _ = job.reply.send(Ok(()));
                    }
                    _ => {
                        let message = match &outcome {
                            Ok(marker) => format!("helper protocol error: {marker:?}"),
                            Err(e) => e.clone(),
                        };
                        append_audit(
                            &profile,
                            serde_json::json!({
                                "kind": "helper",
                                "action": "job",
                                "op_hash": fnv_hex(job.payload.as_bytes()),
                                "result": message,
                            }),
                        );
                        let _ = job.reply.send(Err(message));
                        return;
                    }
                }
                missed = 0;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let mut heartbeat = String::new();
                let got = runtime.block_on(async {
                    tokio::time::timeout(
                        std::time::Duration::from_secs(20),
                        reader.read_line(&mut heartbeat),
                    )
                    .await
                });
                match got {
                    Ok(Ok(0) | Err(_)) | Err(_) => {
                        missed += 1;
                        if missed >= 3 {
                            append_audit(
                                &profile,
                                serde_json::json!({
                                    "kind": "helper",
                                    "action": "dead",
                                    "result": "3 missed heartbeats",
                                }),
                            );
                            return;
                        }
                    }
                    Ok(Ok(_)) => {
                        missed = 0;
                        note_beat(now_ms());
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Verify the connected helper peer: image path must equal our own exe and
/// its token must be high-integrity. Mismatch drops the connection.
#[cfg(all(windows, feature = "backend-windows"))]
fn helper_peer_ok(server: &tokio::net::windows::named_pipe::NamedPipeServer, exe: &Path) -> bool {
    use std::os::windows::io::AsRawHandle as _;
    let Some(pid) = clipmonger_backend_windows::pipe_client_pid(server.as_raw_handle().cast())
    else {
        return false;
    };
    let own = exe.to_string_lossy().to_ascii_lowercase();
    let peer_ok = clipmonger_backend_windows::process_image_path(pid)
        .is_some_and(|image| image.to_ascii_lowercase() == own);
    peer_ok && clipmonger_backend_windows::process_token_high(pid) == Some(true)
}

/// Map a helper failure message to a wire error: refusals latch caps paste
/// Degraded, everything is audited with metadata only (never payloads).
#[cfg(all(windows, feature = "backend-windows"))]
fn helper_ipc_error(
    state: &DaemonState,
    target: &clipmonger_platform::types::PasteTarget,
    message: &str,
) -> IpcError {
    let profile = state
        .profile_dir
        .lock()
        .map_or_else(|_| PathBuf::new(), |dir| dir.clone());
    append_audit(
        &profile,
        serde_json::json!({
            "kind": "helper",
            "action": "route_error",
            "target": target.app.exe,
            "result": message,
        }),
    );
    if message.starts_with("elevation refused") {
        state.note_elevation_refused(message);
        IpcError::new("denied", format!("paste.execute: {message}"))
    } else if message.starts_with("unavailable") {
        IpcError::new("unavailable", format!("paste.execute: {message}"))
    } else {
        IpcError::new("backend", format!("paste.execute: {message}"))
    }
}

/// Skeleton capability aggregator over registered backend snapshots.
///
/// The foundation skeleton has no live backends: `snapshot` reports the
/// store-backed monitor as degraded and everything else unavailable with a
/// skeleton note. `set_extension_connected(false)` marks the (future) GNOME
/// entry unavailable via an extra note.
///
/// A live backend registers its typed [`BackendCaps`] via `register_backend`
/// (WIN-F3); the string `register` API is kept for non-platform reporters.
/// Once a typed snapshot is present the skeleton levels are replaced by it.
pub struct CapsAggregator {
    /// Backend snapshots by name, kept for future aggregation.
    registered: Mutex<HashMap<String, String>>,
    /// GNOME extension link state.
    extension_connected: Mutex<bool>,
    /// Live typed backend snapshot (single backend in v1).
    backend: Mutex<Option<(String, BackendCaps)>>,
}

/// Render a [`Level`] as the lowercase status string.
fn level_str(level: Level) -> &'static str {
    match level {
        Level::Full => "full",
        Level::Degraded => "degraded",
        Level::Unavailable => "unavailable",
    }
}

/// Render a [`PasteStrategy`] as the lowercase status string.
fn strategy_str(strategy: PasteStrategy) -> &'static str {
    match strategy {
        PasteStrategy::UnicodeInject => "unicode_inject",
        PasteStrategy::ClipboardRestore => "clipboard_restore",
        PasteStrategy::ElevatedRoute => "elevated_route",
        PasteStrategy::WriteAndToast => "write_and_toast",
        PasteStrategy::ExtensionAssisted => "extension_assisted",
        PasteStrategy::Unavailable => "unavailable",
    }
}

impl CapsAggregator {
    /// Create an aggregator with the extension link up and no backends.
    #[must_use]
    pub fn new() -> Self {
        Self {
            registered: Mutex::new(HashMap::new()),
            extension_connected: Mutex::new(true),
            backend: Mutex::new(None),
        }
    }

    /// Register one backend's capability summary under `name`.
    pub fn register(&self, name: String, summary: String) {
        if let Ok(mut map) = self.registered.lock() {
            map.insert(name, summary);
        }
    }

    /// Register one backend's typed capability snapshot under `name`.
    pub fn register_backend(&self, name: &str, caps: BackendCaps) {
        if let Ok(mut slot) = self.backend.lock() {
            *slot = Some((name.to_string(), caps));
        }
    }

    /// Flip the GNOME extension link state.
    pub fn set_extension_connected(&self, connected: bool) {
        if let Ok(mut flag) = self.extension_connected.lock() {
            *flag = connected;
        }
    }

    /// Aggregate snapshot as JSON (levels as lowercase strings).
    #[must_use]
    pub fn snapshot(&self) -> Value {
        let extension_up = self.extension_connected.lock().map_or(true, |f| *f);
        if let Some(typed) = self.backend.lock().ok().and_then(|slot| slot.clone()) {
            return Self::typed_snapshot(&typed.1, extension_up);
        }
        let mut notes = vec![Value::from(SKELETON_NOTE)];
        if !extension_up {
            notes.push(Value::from(
                "gnome extension disconnected: paste unavailable",
            ));
        }
        serde_json::json!({
            "monitor": {"level": "degraded", "reason": SKELETON_NOTE},
            "transfer": {"level": "unavailable", "reason": SKELETON_NOTE},
            "hotkey": {"level": "unavailable", "reason": SKELETON_NOTE},
            "paste": {"level": "unavailable", "strategy": "unavailable", "reason": SKELETON_NOTE},
            "tray": {"level": "unavailable", "reason": SKELETON_NOTE},
            "autostart": false,
            "primary_selection": false,
            "extension_connected": extension_up,
            "notes": notes,
        })
    }

    /// Render a typed backend snapshot into the status JSON shape.
    fn typed_snapshot(caps: &BackendCaps, extension_up: bool) -> Value {
        let mut notes: Vec<Value> = caps
            .notes
            .iter()
            .map(|note| Value::from(note.as_str()))
            .collect();
        if !extension_up {
            notes.push(Value::from(
                "gnome extension disconnected: paste unavailable",
            ));
        }
        let reason = |level: Level| {
            if level == Level::Full {
                String::new()
            } else {
                caps.notes.join("; ")
            }
        };
        serde_json::json!({
            "monitor": {"level": level_str(caps.monitor), "reason": reason(caps.monitor)},
            "transfer": {"level": level_str(caps.transfer), "reason": reason(caps.transfer)},
            "hotkey": {"level": level_str(caps.hotkey), "reason": reason(caps.hotkey)},
            "paste": {
                "level": level_str(caps.paste.level),
                "strategy": strategy_str(caps.paste.strategy),
                "reason": caps.paste.reason.clone(),
            },
            "tray": {"level": level_str(caps.tray), "reason": reason(caps.tray)},
            "autostart": caps.autostart,
            "primary_selection": caps.primary_selection,
            "extension_connected": extension_up,
            "notes": notes,
        })
    }
}

impl Default for CapsAggregator {
    fn default() -> Self {
        Self::new()
    }
}

/// Current wall-clock time in unix millis.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0))
}

/// Fresh random id string for a replayed clip.
fn new_id(index: u64) -> String {
    if let Ok(raw) = fs::read_to_string("/proc/sys/kernel/random/uuid") {
        let trimmed = raw.trim().to_string();
        if !trimmed.is_empty() {
            return trimmed;
        }
    }
    format!("01900000-0000-7000-8000-{index:012x}")
}

/// Trivial hex fingerprint of bytes for the stored `content_hash` field.
///
/// The store computes its own canonical dedupe hash from payloads; this only
/// fills the row metadata.
fn fnv_hex(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Outcome of storing real monitor payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoreDecision {
    /// Stored (or dedupe-moved) a row.
    Stored,
    /// Sensitive: counted in `privacy_skips`, no row written.
    Sensitive,
    /// Store write failed; carries metadata only, never payloads.
    Failed,
}

/// Store real monitor payloads: sensitive events only bump `privacy_skips`,
/// everything else becomes a clip row. `origin` names the capture backend
/// (`windows`, `x11`, ...); `id_seed` feeds id generation.
fn store_payloads(
    store: &Store,
    origin: &str,
    sensitive: bool,
    payloads: &[(String, Vec<u8>)],
    id_seed: u64,
) -> (StoreDecision, Option<String>) {
    if sensitive {
        let _ = store.record_privacy_skip();
        return (StoreDecision::Sensitive, None);
    }
    if payloads.is_empty() {
        return (
            StoreDecision::Failed,
            Some("no readable payload".to_string()),
        );
    }
    let now = now_ms();
    let text = payloads
        .iter()
        .find(|(mime, _)| mime == "text/plain")
        .map(|(_, bytes)| String::from_utf8_lossy(bytes).into_owned());
    let preview = text
        .as_deref()
        .map(|t| t.chars().take(200).collect::<String>())
        .unwrap_or_default();
    let formats: Vec<String> = payloads.iter().map(|(mime, _)| mime.clone()).collect();
    let bytes_total: u64 = payloads.iter().map(|(_, bytes)| bytes.len() as u64).sum();
    let mut fingerprint = Vec::new();
    for (_, bytes) in payloads {
        fingerprint.extend_from_slice(bytes);
    }
    let clip_json = serde_json::json!({
        "id": new_id(id_seed),
        "created_ms": now,
        "source_os": origin,
        "pinned": false,
        "removed": false,
        "preview": preview,
        "search_text": text.unwrap_or_default(),
        "formats": formats,
        "bytes_total": bytes_total,
        "content_hash": fnv_hex(&fingerprint),
        "rev": {"wall_ms": now, "dev": "daemon", "ctr": 0},
        "last_used_ms": now,
        "origin_dev": "daemon",
        "origin": origin,
    });
    let clip: clipmonger_core::model::Clip = match serde_json::from_value(clip_json) {
        Ok(clip) => clip,
        Err(e) => {
            return (
                StoreDecision::Failed,
                Some(format!("clip build failed: {e}")),
            );
        }
    };
    match store.insert_clip(&clip, payloads) {
        Ok(_) => (StoreDecision::Stored, None),
        Err(e) => (StoreDecision::Failed, Some(format!("store failed: {e}"))),
    }
}

/// Store one headless replay line; returns the JSON report line for it.
fn process_capture(store: &Store, line: &str, index: u64) -> String {
    let event: Value = match serde_json::from_str(line) {
        Ok(event) => event,
        Err(_) => {
            return serde_json::json!({
                "accepted": false, "stored": false,
                "reason": "invalid_json", "index": index,
            })
            .to_string();
        }
    };
    if event.get("sensitive").and_then(Value::as_bool) == Some(true) {
        let _ = store.record_privacy_skip();
        return serde_json::json!({
            "accepted": false, "stored": false,
            "reason": "sensitive", "index": index,
        })
        .to_string();
    }
    let now = now_ms();
    let origin = event
        .get("origin")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let payload = format!("headless replay {index}").into_bytes();
    let clip_json = serde_json::json!({
        "id": new_id(index),
        "created_ms": now,
        "source_os": "linux",
        "pinned": false,
        "removed": false,
        "preview": format!("headless replay {index}"),
        "search_text": format!("headless replay {index}"),
        "formats": ["text/plain"],
        "bytes_total": payload.len(),
        "content_hash": fnv_hex(&payload),
        "rev": {"wall_ms": now, "dev": "daemon", "ctr": 0},
        "last_used_ms": now,
        "origin_dev": "daemon",
        "origin": origin,
    });
    let clip: clipmonger_core::model::Clip = match serde_json::from_value(clip_json) {
        Ok(clip) => clip,
        Err(e) => {
            return serde_json::json!({
                "accepted": false, "stored": false,
                "reason": format!("clip build failed: {e}"), "index": index,
            })
            .to_string();
        }
    };
    match store.insert_clip(&clip, &[("text/plain".to_string(), payload)]) {
        Ok(_) => serde_json::json!({
            "accepted": true, "stored": true, "index": index,
        })
        .to_string(),
        Err(e) => serde_json::json!({
            "accepted": false, "stored": false,
            "reason": format!("store failed: {e}"), "index": index,
        })
        .to_string(),
    }
}

/// Headless `--script` semantics: one JSON report line per input line.
///
/// Sensitive events are counted but record nothing. Pure apart from the
/// store writes; no sockets involved.
#[must_use]
pub fn replay_script(store: &Store, lines: impl Iterator<Item = String>) -> Vec<String> {
    lines
        .enumerate()
        .map(|(index, line)| {
            process_capture(store, &line, u64::try_from(index).unwrap_or_default())
        })
        .collect()
}

/// Shared daemon request state behind the IPC handler.
pub struct DaemonState {
    /// Clip store for history methods.
    store: Store,
    /// Capability aggregator for status/caps methods.
    caps: CapsAggregator,
    /// Monitor pause flag toggled by `monitor.set`.
    monitoring_paused: Mutex<bool>,
    /// Paste delivery budget (`popup.unicode_limit`, `popup.delivery_dwell_ms`).
    paste_limits: Mutex<(u64, u64)>,
    /// Profile dir for the audit log (set once at startup).
    profile_dir: Mutex<PathBuf>,
    /// Live single-use gesture tokens (hotkey/popup trigger path only).
    gesture_tokens: Mutex<HashMap<u64, i64>>,
    /// Gesture mint sequence mixed into every token.
    gesture_counter: Mutex<u64>,
    /// Elevation-refusal latch (caps paste Degraded until restart).
    elevation_refused: Mutex<bool>,
    /// Elevated-helper supervisor (Windows runas thread; idle elsewhere).
    helper: Mutex<HelperSupervisor>,
}

impl DaemonState {
    /// Wrap an opened store with fresh skeleton caps.
    #[must_use]
    pub fn new(store: Store) -> Self {
        Self {
            store,
            caps: CapsAggregator::new(),
            monitoring_paused: Mutex::new(false),
            paste_limits: Mutex::new((65536, 1000)),
            profile_dir: Mutex::new(PathBuf::new()),
            gesture_tokens: Mutex::new(HashMap::new()),
            gesture_counter: Mutex::new(0),
            elevation_refused: Mutex::new(false),
            helper: Mutex::new(HelperSupervisor::new()),
        }
    }

    /// Point audit/helper events at `<profile>` (call once at startup).
    pub fn set_profile_dir(&self, dir: &Path) {
        if let Ok(mut slot) = self.profile_dir.lock() {
            *slot = dir.to_path_buf();
        }
    }

    /// Mint one single-use gesture token. HOTKEY/POPUP TRIGGER PATH ONLY:
    /// the generic `paste.execute` IPC arm must never call this (a
    /// same-user caller driving `paste.execute` gets standard-user paste
    /// at most).
    pub fn mint_gesture_token(&self) -> u64 {
        let at_ms = now_ms();
        let seq = if let Ok(mut counter) = self.gesture_counter.lock() {
            *counter = counter.wrapping_add(1);
            *counter
        } else {
            at_ms.cast_unsigned()
        };
        let mut token = at_ms
            .cast_unsigned()
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(seq.wrapping_mul(0xBF58_476D_1CE4_E5B9));
        if token == 0 {
            token = 0x9E37_79B9_7F4A_7C15;
        }
        if let Ok(mut tokens) = self.gesture_tokens.lock() {
            tokens.retain(|_, minted| at_ms >= *minted && at_ms - *minted <= GESTURE_TTL_MS);
            tokens.insert(token, at_ms);
        }
        token
    }

    /// Consume the newest unexpired trigger token (daemon-side spend: the
    /// popup presents no token in v1, so a paste within the trigger window
    /// spends the trigger's token once; generic callers still cannot mint).
    #[cfg_attr(not(all(windows, feature = "backend-windows")), allow(dead_code))]
    fn consume_trigger_token(&self, at_ms: i64) -> bool {
        let Ok(mut tokens) = self.gesture_tokens.lock() else {
            return false;
        };
        let newest = tokens
            .iter()
            .filter(|(_, minted)| at_ms >= **minted && at_ms - **minted <= GESTURE_TTL_MS)
            .max_by_key(|(_, minted)| **minted)
            .map(|(token, _)| *token);
        match newest {
            Some(token) => {
                tokens.remove(&token);
                true
            }
            Option::None => false,
        }
    }

    /// Consume one gesture token: true exactly once while unexpired.
    #[cfg_attr(not(all(windows, feature = "backend-windows")), allow(dead_code))]
    fn consume_gesture_token(&self, token: u64, at_ms: i64) -> bool {
        let minted = self
            .gesture_tokens
            .lock()
            .ok()
            .and_then(|mut tokens| tokens.remove(&token));
        minted.is_some_and(|minted| at_ms >= minted && at_ms - minted <= GESTURE_TTL_MS)
    }

    /// Latch elevation refusal: caps paste Degraded plus an audit entry.
    /// Never retry-spawns in a loop; the next attempt re-prompts.
    pub fn note_elevation_refused(&self, reason: &str) {
        if let Ok(mut flag) = self.elevation_refused.lock() {
            *flag = true;
        }
        let profile = self
            .profile_dir
            .lock()
            .map_or_else(|_| PathBuf::new(), |dir| dir.clone());
        append_audit(
            &profile,
            serde_json::json!({
                "kind": "helper",
                "action": "elevation_refused",
                "result": reason,
            }),
        );
    }

    /// Whether elevation was refused (caps paste Degraded until restart).
    #[must_use]
    pub fn elevation_refused(&self) -> bool {
        self.elevation_refused.lock().is_ok_and(|flag| *flag)
    }

    /// Helper route state for `status`: route, liveness, refusal latch.
    #[must_use]
    pub fn helper_route_state(&self) -> Value {
        let alive = self
            .helper
            .lock()
            .is_ok_and(|supervisor| supervisor.alive(now_ms()));
        serde_json::json!({
            "route": if alive { "helper" } else { "unavailable" },
            "alive": alive,
            "elevation_refused": self.elevation_refused(),
        })
    }

    /// Capability snapshot for `status`/`caps.*`.
    #[must_use]
    pub fn caps_snapshot(&self) -> Value {
        let mut snapshot = self.caps.snapshot();
        if self.elevation_refused() {
            if let Some(paste) = snapshot.get_mut("paste") {
                paste["level"] = Value::from("degraded");
                paste["strategy"] = Value::from("elevated_route");
                paste["reason"] = Value::from("elevation refused");
            }
            if let Some(notes) = snapshot.get_mut("notes").and_then(Value::as_array_mut) {
                notes.push(Value::from(
                    "paste Degraded: elevation refused, retry available",
                ));
            }
        }
        snapshot
    }

    /// Typed backend snapshot for live backends (WIN-F3).
    pub fn register_backend(&self, name: &str, caps: BackendCaps) {
        self.caps.register_backend(name, caps);
    }

    /// Override the paste delivery budget from config.
    pub fn set_paste_limits(&self, unicode_limit: u64, dwell_ms: u64) {
        if let Ok(mut limits) = self.paste_limits.lock() {
            *limits = (unicode_limit, dwell_ms);
        }
    }

    /// Current paste delivery budget.
    #[cfg_attr(not(all(windows, feature = "backend-windows")), allow(dead_code))]
    fn paste_budget(&self) -> (u64, u64) {
        self.paste_limits
            .lock()
            .map_or((65536, 1000), |limits| *limits)
    }

    /// Ingest one live monitor observation with already-read `payloads`.
    ///
    /// Sensitive observations only bump `privacy_skips` and store nothing.
    /// Returns whether a row was stored.
    pub fn ingest_capture(
        &self,
        origin: &str,
        sensitive: bool,
        payloads: &[(String, Vec<u8>)],
    ) -> bool {
        let seed = u64::try_from(now_ms().max(0)).unwrap_or_default();
        matches!(
            store_payloads(&self.store, origin, sensitive, payloads, seed),
            (StoreDecision::Stored, _)
        )
    }

    /// Find a stored clip id by its string form without naming the id type.
    fn find_clip(&self, id: &str) -> Option<clipmonger_core::model::Clip> {
        let clips = self
            .store
            .list_history(u64::MAX, 0, true)
            .unwrap_or_default();
        clips.into_iter().find(|c| c.id.to_string() == id)
    }

    /// Whether the monitor is currently paused.
    fn paused(&self) -> bool {
        self.monitoring_paused.lock().is_ok_and(|flag| *flag)
    }

    /// Requested clip id from `params`, looked up in the store.
    fn requested_clip(&self, params: &Value) -> Option<clipmonger_core::model::Clip> {
        let id = params
            .get("clip")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        self.find_clip(&id)
    }

    /// Dispatch one `history.*` method.
    fn handle_history(&self, method: &str, params: &Value) -> Result<Value, IpcError> {
        match method {
            "history.list" => {
                let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(100);
                let offset = params.get("offset").and_then(Value::as_u64).unwrap_or(0);
                let include_removed = params
                    .get("include_removed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let clips = self
                    .store
                    .list_history(limit, offset, include_removed)
                    .map_err(|e| IpcError::new("backend", e.to_string()))?;
                Ok(serde_json::json!({"clips": clips}))
            }
            "history.get" => match self.requested_clip(params) {
                Some(found) => match self.store.get_clip(found.id) {
                    Ok(Some((clip, _))) => Ok(serde_json::json!({"clip": clip})),
                    Ok(None) => Ok(serde_json::json!({"clip": null})),
                    Err(e) => Err(IpcError::new("backend", e.to_string())),
                },
                None => Ok(serde_json::json!({"clip": null})),
            },
            "history.delete" => match self.requested_clip(params) {
                Some(found) => self
                    .store
                    .soft_delete(found.id)
                    .map_err(|e| IpcError::new("backend", e.to_string()))
                    .map(|()| serde_json::json!({"deleted": true})),
                None => Ok(serde_json::json!({"deleted": false})),
            },
            "history.pin" => {
                let pinned = params
                    .get("pinned")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                match self.requested_clip(params) {
                    Some(found) => match self.store.get_clip(found.id) {
                        Ok(Some((mut clip, payloads))) => {
                            clip.pinned = pinned;
                            self.store
                                .insert_clip(&clip, &payloads)
                                .map_err(|e| IpcError::new("backend", e.to_string()))
                                .map(|_| serde_json::json!({"pinned": pinned}))
                        }
                        Ok(None) => Ok(serde_json::json!({"pinned": false})),
                        Err(e) => Err(IpcError::new("backend", e.to_string())),
                    },
                    None => Ok(serde_json::json!({"pinned": false})),
                }
            }
            _ => {
                let purged = self.store.purge_removed();
                Ok(serde_json::json!({"purged": purged}))
            }
        }
    }

    /// Dispatch `monitor.set`: explicit state wins, `toggle` flips the flag.
    fn handle_monitor(&self, params: &Value) -> Result<Value, IpcError> {
        let mut flag = self
            .monitoring_paused
            .lock()
            .map_err(|_| IpcError::new("internal", "monitor flag poisoned".to_string()))?;
        if params.get("toggle").and_then(Value::as_bool) == Some(true) {
            *flag = !*flag;
        } else if let Some(state) = params.get("state") {
            *flag = state.as_str().unwrap_or("On") != "On";
        }
        let paused = *flag;
        drop(flag);
        Ok(serde_json::json!({"ok": true, "monitoring_paused": paused}))
    }

    /// Dispatch `capture.report` without recording sensitive events.
    fn handle_capture(&self, params: &Value) -> Value {
        let sensitive = params
            .get("event")
            .and_then(|event| event.get("sensitive"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if sensitive {
            return serde_json::json!({"accepted": false});
        }
        let line = params
            .get("event")
            .map_or_else(|| "{}".to_string(), Value::to_string);
        let stamp = u64::try_from(now_ms().max(0)).unwrap_or_default();
        let report = process_capture(&self.store, &line, stamp);
        let accepted = serde_json::from_str::<Value>(&report)
            .ok()
            .and_then(|value| value.get("accepted").and_then(Value::as_bool))
            .unwrap_or(false);
        serde_json::json!({"accepted": accepted})
    }

    /// Dispatch `paste.execute` through the live Windows backend, or refuse
    /// with `unavailable` when no live backend is compiled in.
    ///
    /// High-integrity targets take the elevated-helper route ONLY, gated on
    /// a fresh single-use gesture token (`gesture` param). Generic IPC
    /// callers cannot mint tokens, so tokenless elevated requests are
    /// refused without ever spawning.
    fn handle_paste_execute(&self, params: &Value) -> Result<Value, IpcError> {
        #[cfg(all(windows, feature = "backend-windows"))]
        {
            use clipmonger_backend_windows::Integrity;
            use clipmonger_platform::traits::PasteEngine as _;
            use clipmonger_platform::types::ClipRef;
            let found = self.requested_clip(params).ok_or_else(|| {
                IpcError::new("backend", "paste.execute: unknown clip".to_string())
            })?;
            let payloads = self
                .store
                .get_clip(found.id)
                .map_err(|e| IpcError::new("backend", e.to_string()))?
                .map(|(_, payloads)| payloads)
                .unwrap_or_default();
            let (unicode_limit, dwell_ms) = self.paste_budget();
            let paste = clipmonger_backend_windows::WinPaste::new(
                usize::try_from(unicode_limit).unwrap_or(65536),
            )
            .with_dwell_ms(dwell_ms);
            let target = paste
                .capture_target()
                .map_err(|e| IpcError::new("unavailable", format!("paste.execute: {e}")))?;
            if clipmonger_backend_windows::target_integrity(&target.handle) == Integrity::High {
                let token = params.get("gesture").and_then(Value::as_u64);
                let token_ok = token.is_some_and(|t| self.consume_gesture_token(t, now_ms()))
                    || self.consume_trigger_token(now_ms());
                if !token_ok {
                    self.note_elevation_refused("missing or invalid gesture token");
                    return Err(IpcError::new(
                        "denied",
                        "paste.execute: elevation refused (gesture required)".to_string(),
                    ));
                }
                return self.paste_via_helper(&target, &payloads);
            }
            paste
                .paste_payloads(&target, &ClipRef(found.id), &payloads)
                .map_err(|e| {
                    let message = e.to_string();
                    if message.starts_with("denied") || message.contains("refused") {
                        IpcError::new("denied", format!("paste.execute: {e}"))
                    } else {
                        IpcError::new("backend", format!("paste.execute: {e}"))
                    }
                })?;
            Ok(serde_json::json!({"pasted": true}))
        }
        #[cfg(not(all(windows, feature = "backend-windows")))]
        {
            let _ = (self, params);
            Err(IpcError::new(
                "unavailable",
                format!("paste unavailable: {SKELETON_NOTE}"),
            ))
        }
    }

    /// Paste into a high-integrity target via the elevated helper
    /// (Windows only): resolve the target pid, hand one `send_input_batch`
    /// job to the supervisor thread, and wait bounded for its result.
    /// Refusals latch caps paste Degraded; portable/unprotected layouts
    /// stay `Unavailable` with no spawn attempt.
    #[cfg(all(windows, feature = "backend-windows"))]
    fn paste_via_helper(
        &self,
        target: &clipmonger_platform::types::PasteTarget,
        payloads: &[(String, Vec<u8>)],
    ) -> Result<Value, IpcError> {
        use clipmonger_platform::types::PlatformHandle;
        let hwnd = match &target.handle {
            PlatformHandle::WinHwnd(hwnd) => *hwnd,
            _ => {
                return Err(IpcError::new(
                    "unavailable",
                    "paste.execute: helper route needs a window target".to_string(),
                ));
            }
        };
        let pid = clipmonger_backend_windows::hwnd_owner_pid(hwnd).ok_or_else(|| {
            IpcError::new(
                "unavailable",
                "paste.execute: helper route cannot resolve target".to_string(),
            )
        })?;
        let text = payloads
            .iter()
            .find(|(mime, _)| mime == "text/plain")
            .map(|(_, bytes)| bytes.clone())
            .unwrap_or_default();
        let payload = String::from_utf8(text).map_err(|_| {
            IpcError::new(
                "backend",
                "paste.execute: helper route needs UTF-8 text".to_string(),
            )
        })?;
        let profile = self
            .profile_dir
            .lock()
            .map_or_else(|_| PathBuf::new(), |dir| dir.clone());
        let jobs_tx = self
            .helper
            .lock()
            .map_err(|_| {
                IpcError::new(
                    "internal",
                    "paste.execute: helper state poisoned".to_string(),
                )
            })?
            .ensure_link(&profile);
        let (reply_tx, reply_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let job = SupervisorJob {
            target_pid: pid,
            payload,
            reply: reply_tx,
        };
        if jobs_tx.send(job).is_err() {
            if let Ok(mut supervisor) = self.helper.lock() {
                supervisor.drop_link();
            }
            return Err(IpcError::new(
                "backend",
                "paste.execute: helper channel lost".to_string(),
            ));
        }
        match reply_rx.recv_timeout(std::time::Duration::from_secs(150)) {
            Ok(Ok(())) => {
                if let Ok(supervisor) = self.helper.lock() {
                    supervisor.note_beat(now_ms());
                }
                Ok(serde_json::json!({"pasted": true}))
            }
            Ok(Err(e)) => Err(helper_ipc_error(self, target, &e)),
            Err(_) => {
                if let Ok(mut supervisor) = self.helper.lock() {
                    supervisor.drop_link();
                }
                Err(IpcError::new(
                    "backend",
                    "paste.execute: helper result timed out".to_string(),
                ))
            }
        }
    }

    /// Dispatch one IPC method to a JSON result or typed wire error.
    ///
    /// # Errors
    ///
    /// Returns the store's error for failing history methods, or the
    /// skeleton `unavailable` error for `paste.execute` without a backend.
    pub fn handle(&self, method: &str, params: &Value) -> Result<Value, IpcError> {
        match method {
            "status" => Ok(serde_json::json!({
                "caps": self.caps_snapshot(),
                "helper": self.helper_route_state(),
                "up": true,
                "running": true,
                "version": DAEMON_VERSION,
                "monitoring_paused": self.paused(),
            })),
            "caps.get" | "caps.refresh" => Ok(self.caps.snapshot()),
            "ping" => Ok(serde_json::json!({"pong": true})),
            "paste.execute" => self.handle_paste_execute(params),
            method if method.starts_with("history.") => self.handle_history(method, params),
            "monitor.set" => self.handle_monitor(params),
            "capture.report" => Ok(self.handle_capture(params)),
            _ => Ok(serde_json::json!({"ok": true})),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::{now_ms, CapsAggregator, DaemonState};

    #[test]
    fn trigger_token_spends_once_daemon_side() {
        let dir = std::env::temp_dir().join(format!("cm-gesture-{}", std::process::id()));
        let store = clipmonger_core::Store::open(&dir).expect("store");
        let state = DaemonState::new(store);
        assert!(
            !state.consume_trigger_token(now_ms()),
            "no trigger, no spend"
        );
        state.mint_gesture_token();
        assert!(
            state.consume_trigger_token(now_ms()),
            "trigger token spends"
        );
        assert!(!state.consume_trigger_token(now_ms()), "single use only");
    }

    #[test]
    fn caps_extension_flip_marks_gnome_unavailable() {
        let caps = CapsAggregator::new();
        let connected = caps.snapshot();
        assert_eq!(
            connected
                .get("extension_connected")
                .and_then(Value::as_bool),
            Some(true)
        );
        caps.set_extension_connected(false);
        let flipped = caps.snapshot();
        assert_eq!(
            flipped.get("extension_connected").and_then(Value::as_bool),
            Some(false)
        );
        let gnome_noted = flipped
            .get("notes")
            .and_then(Value::as_array)
            .is_some_and(|notes| {
                notes
                    .iter()
                    .any(|note| note.as_str().unwrap_or_default().contains("gnome"))
            });
        assert!(
            gnome_noted,
            "flipped snapshot must note the gnome entry, got: {flipped}"
        );
        assert_eq!(
            flipped
                .get("monitor")
                .and_then(|m| m.get("level"))
                .and_then(Value::as_str),
            Some("degraded")
        );
    }
}
