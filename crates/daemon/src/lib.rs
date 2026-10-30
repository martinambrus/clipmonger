//! Daemon skeleton: single-instance lock, rendezvous files, capability
//! aggregation, headless script replay, and the IPC request handler.
//!
//! Implements the frozen binary contract (seg-02 S1/S9, D-F20/F21/F22).
//! No payload bytes are ever logged; only metadata reaches stderr.

use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use clipmonger_core::{CoreError, Store};
use clipmonger_ipc::proto::IpcError;
use clipmonger_platform::types::{BackendCaps, Level, PasteStrategy};
use serde_json::Value;

/// Protocol tag written to the rendezvous file.
pub const PROTOCOL: &str = "ipc/v1";
/// Daemon crate version reported by `status`.
#[cfg(all(unix, feature = "backend-wayland"))]
pub mod portal;

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

/// Paste refusal while no verified elevated helper is connected.
#[cfg(all(windows, feature = "backend-windows"))]
const HELPER_NOT_RUNNING: &str = "unavailable: admin helper is not running";
/// The helper connection dropped during a job.
#[cfg(all(windows, feature = "backend-windows"))]
const HELPER_LOST: &str = "helper connection lost";
/// The helper did not answer within the job deadline (treated as hung).
#[cfg(all(windows, feature = "backend-windows"))]
const HELPER_TIMED_OUT: &str = "helper result timed out";
/// Refusal for a paste whose deadline passed before it could run.
#[cfg(all(windows, feature = "backend-windows"))]
const PASTE_EXPIRED: &str = "unavailable: paste request expired";
/// Paste deadline: a clip picked in the popup (or a paste request without a
/// pick time) must be delivered within this many ms, else it is refused;
/// the helper enforces the same absolute deadline before injecting.
#[cfg(all(windows, feature = "backend-windows"))]
const PASTE_DEADLINE_MS: i64 = 2_000;

/// Elevated-helper supervisor: an always-listening pipe server for the
/// elevated helper (UAC-launched worker or logon-task helper), peer
/// verification, liveness from the pipe itself (helper exit closes it, no
/// heartbeat needed), per-job deadlines, audit entries, refusal latching
/// into caps paste Degraded.
///
/// The library never launches the helper; the daemon binary's consent flow
/// does (manual QA leg, never automated).
pub struct HelperSupervisor {
    /// Whether a verified helper is connected right now.
    connected: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Job queue of the supervisor thread (Windows only; `None` until started).
    #[cfg(all(windows, feature = "backend-windows"))]
    jobs: Option<tokio::sync::mpsc::UnboundedSender<SupervisorJob>>,
}

/// One helper job handed to the supervisor thread.
#[cfg(all(windows, feature = "backend-windows"))]
struct SupervisorJob {
    /// Validated high-integrity target process id.
    target_pid: u32,
    /// Target top-level window the helper must paste into.
    target_hwnd: Option<i64>,
    /// Helper operation (text injection or paste chord).
    op: clipmonger_backend_windows::helper::JobOp,
    /// Input text bytes (UTF-8); empty for a paste-chord job.
    payload: String,
    /// Absolute deadline (unix ms); the job never runs after it.
    expires_ms: i64,
    /// Outcome channel back to the IPC handler.
    reply: std::sync::mpsc::Sender<Result<(), String>>,
}

impl HelperSupervisor {
    /// Idle supervisor with no helper connected.
    #[must_use]
    pub fn new() -> Self {
        Self {
            connected: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            #[cfg(all(windows, feature = "backend-windows"))]
            jobs: None,
        }
    }

    /// True while a verified helper is connected.
    #[must_use]
    pub fn connected(&self) -> bool {
        self.connected.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Start the supervisor thread once (Windows only). It listens on the
    /// profile's helper pipe for the whole daemon lifetime, so a helper can
    /// attach whenever it starts (logon task or UAC consent).
    #[cfg(all(windows, feature = "backend-windows"))]
    pub fn start(&mut self, profile: &Path) {
        if self.jobs.is_some() {
            return;
        }
        let (jobs_tx, jobs_rx) = tokio::sync::mpsc::unbounded_channel::<SupervisorJob>();
        let connected = std::sync::Arc::clone(&self.connected);
        let profile = profile.to_path_buf();
        std::thread::spawn(move || helper_supervisor_thread(&profile, &connected, jobs_rx));
        self.jobs = Some(jobs_tx);
    }
}

impl Default for HelperSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

/// Supervisor thread body (Windows only): one current-thread runtime that
/// serves helper sessions back to back for the daemon lifetime.
#[cfg(all(windows, feature = "backend-windows"))]
#[allow(clippy::needless_pass_by_value)]
fn helper_supervisor_thread(
    profile: &Path,
    connected: &std::sync::atomic::AtomicBool,
    mut jobs: tokio::sync::mpsc::UnboundedReceiver<SupervisorJob>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            append_audit(
                profile,
                serde_json::json!({
                    "kind": "helper",
                    "action": "refused",
                    "result": format!("helper runtime failed: {e}"),
                }),
            );
            return;
        }
    };
    runtime.block_on(supervise_helper(profile, connected, &mut jobs));
}

/// Listen for the helper, verify it, serve one session, repeat. While no
/// verified helper is connected every job fails at once, so no paste can
/// queue up and run later against whatever window is focused by then.
#[cfg(all(windows, feature = "backend-windows"))]
async fn supervise_helper(
    profile: &Path,
    connected: &std::sync::atomic::AtomicBool,
    jobs: &mut tokio::sync::mpsc::UnboundedReceiver<SupervisorJob>,
) {
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use tokio::net::windows::named_pipe::{PipeMode, ServerOptions};

    let pipe_name = clipmonger_backend_windows::helper::persistent_pipe_name(profile);
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("clipmonger-daemon"));
    let mut job_id: u64 = 0;
    let mut last_error = String::new();
    let mut mismatch_logged = false;
    loop {
        let server = match ServerOptions::new()
            .first_pipe_instance(true)
            .max_instances(1)
            .reject_remote_clients(true)
            .pipe_mode(PipeMode::Byte)
            .create(&pipe_name)
        {
            Ok(server) => server,
            Err(e) => {
                // The previous instance may still be closing, or another
                // process holds the name; report each distinct error once.
                let message = format!("helper pipe failed: {e}");
                if message != last_error {
                    append_audit(
                        profile,
                        serde_json::json!({"kind": "helper", "action": "pipe_error", "result": message}),
                    );
                    last_error = message;
                }
                if !fail_jobs_for(jobs, Duration::from_millis(500)).await {
                    return;
                }
                continue;
            }
        };
        last_error.clear();
        match wait_for_helper(&server, jobs).await {
            None => return,
            Some(false) => {
                if !fail_jobs_for(jobs, Duration::from_millis(50)).await {
                    return;
                }
                continue;
            }
            Some(true) => {}
        }
        if !helper_peer_ok(&server, &exe) {
            // A foreign or stale helper keeps retrying; audit it once per
            // streak instead of once per attempt.
            if !mismatch_logged {
                append_audit(
                    profile,
                    serde_json::json!({"kind": "helper", "action": "alert", "result": "peer mismatch"}),
                );
                mismatch_logged = true;
            }
            drop(server);
            if !fail_jobs_for(jobs, Duration::from_secs(2)).await {
                return;
            }
            continue;
        }
        mismatch_logged = false;
        connected.store(true, Ordering::SeqCst);
        append_audit(
            profile,
            serde_json::json!({"kind": "helper", "action": "connected", "result": "ok"}),
        );
        let keep_running = serve_helper_session(server, profile, jobs, &mut job_id).await;
        connected.store(false, Ordering::SeqCst);
        append_audit(
            profile,
            serde_json::json!({"kind": "helper", "action": "disconnected", "result": "pipe closed"}),
        );
        if !keep_running {
            return;
        }
    }
}

/// Wait for a helper to connect; jobs arriving meanwhile fail at once.
/// `None` means the job queue closed (daemon stopping).
#[cfg(all(windows, feature = "backend-windows"))]
async fn wait_for_helper(
    server: &tokio::net::windows::named_pipe::NamedPipeServer,
    jobs: &mut tokio::sync::mpsc::UnboundedReceiver<SupervisorJob>,
) -> Option<bool> {
    let connect = server.connect();
    tokio::pin!(connect);
    loop {
        tokio::select! {
            result = &mut connect => return Some(result.is_ok()),
            job = jobs.recv() => {
                let job = job?;
                let _ = job.reply.send(Err(HELPER_NOT_RUNNING.to_string()));
            }
        }
    }
}

/// Fail every job for `wait` (no helper to serve them); false when the job
/// queue closed.
#[cfg(all(windows, feature = "backend-windows"))]
async fn fail_jobs_for(
    jobs: &mut tokio::sync::mpsc::UnboundedReceiver<SupervisorJob>,
    wait: std::time::Duration,
) -> bool {
    let pause = tokio::time::sleep(wait);
    tokio::pin!(pause);
    loop {
        tokio::select! {
            () = &mut pause => return true,
            job = jobs.recv() => {
                let Some(job) = job else {
                    return false;
                };
                let _ = job.reply.send(Err(HELPER_NOT_RUNNING.to_string()));
            }
        }
    }
}

/// Serve jobs over one verified helper connection until it closes; returns
/// false only when the job queue closed (daemon stopping).
#[cfg(all(windows, feature = "backend-windows"))]
async fn serve_helper_session(
    server: tokio::net::windows::named_pipe::NamedPipeServer,
    profile: &Path,
    jobs: &mut tokio::sync::mpsc::UnboundedReceiver<SupervisorJob>,
    job_id: &mut u64,
) -> bool {
    use tokio::io::AsyncBufReadExt as _;

    let (read_half, mut write_half) = tokio::io::split(server);
    // A dedicated reader turns the stream into whole lines; the helper
    // exiting closes the pipe, which closes this channel.
    let (lines_tx, mut lines) = tokio::sync::mpsc::unbounded_channel::<String>();
    let reader = tokio::spawn(async move {
        let mut reader = tokio::io::BufReader::new(read_half);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {
                    if lines_tx.send(line).is_err() {
                        return;
                    }
                }
            }
        }
    });
    let keep_running = loop {
        tokio::select! {
            job = jobs.recv() => {
                let Some(job) = job else {
                    break false;
                };
                if clipmonger_backend_windows::helper::job_expired(Some(job.expires_ms), now_ms()) {
                    // Queued behind a slow job: never deliver it late.
                    let _ = job.reply.send(Err(PASTE_EXPIRED.to_string()));
                    continue;
                }
                *job_id = job_id.wrapping_add(1);
                let started = std::time::Instant::now();
                let outcome = run_helper_job(&mut write_half, &mut lines, *job_id, &job).await;
                append_audit(
                    profile,
                    serde_json::json!({
                        "kind": "helper",
                        "action": "job",
                        "op_hash": fnv_hex(job.payload.as_bytes()),
                        "ms": u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                        "result": match &outcome {
                            Ok(()) => "ok".to_string(),
                            Err(e) => e.clone(),
                        },
                    }),
                );
                // A helper that misses its deadline is treated as hung: end
                // the session so status and the consent flow see it gone.
                let lost = matches!(&outcome, Err(e) if e == HELPER_LOST || e == HELPER_TIMED_OUT);
                let _ = job.reply.send(outcome);
                if lost {
                    break true;
                }
            }
            line = lines.recv() => {
                // Only late results of timed-out jobs land here; EOF ends
                // the session.
                if line.is_none() {
                    break true;
                }
            }
        }
    };
    reader.abort();
    // Let the aborted reader drop its pipe half before the caller creates
    // the next first-instance pipe under the same name.
    let _ = reader.await;
    keep_running
}

/// Send one job and wait for its matching result. The helper enforces the
/// same deadline, so nothing is injected after this side gives up.
#[cfg(all(windows, feature = "backend-windows"))]
async fn run_helper_job(
    write_half: &mut tokio::io::WriteHalf<tokio::net::windows::named_pipe::NamedPipeServer>,
    lines: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
    job_id: u64,
    job: &SupervisorJob,
) -> Result<(), String> {
    use std::time::Duration;
    use tokio::io::AsyncWriteExt as _;

    let line =
        clipmonger_backend_windows::helper::encode_job(&clipmonger_backend_windows::helper::Job {
            job_id,
            target_pid: job.target_pid,
            op: job.op,
            payload: job.payload.clone(),
            target_hwnd: job.target_hwnd,
            expires_ms: Some(job.expires_ms),
        })
        .map_err(|e| format!("backend: {e}"))?;
    let written = tokio::time::timeout(Duration::from_secs(1), async {
        write_half.write_all(line.as_bytes()).await?;
        write_half.flush().await
    })
    .await;
    if !matches!(written, Ok(Ok(()))) {
        return Err(HELPER_LOST.to_string());
    }
    let remaining_ms = u64::try_from(job.expires_ms - now_ms()).unwrap_or(0);
    let deadline = tokio::time::Instant::now() + Duration::from_millis(remaining_ms + 1_500);
    loop {
        let line = match tokio::time::timeout_at(deadline, lines.recv()).await {
            Ok(Some(line)) => line,
            Ok(None) => return Err(HELPER_LOST.to_string()),
            Err(_) => return Err(HELPER_TIMED_OUT.to_string()),
        };
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if value.get("job_id").and_then(Value::as_u64) != Some(job_id) {
            continue;
        }
        if value.get("ok").and_then(Value::as_bool) == Some(true) {
            return Ok(());
        }
        return Err(value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("helper refused")
            .to_string());
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
/// Once typed snapshots are present the skeleton levels are replaced by the
/// merged multi-backend view (seg-06 S8 dual-backend).
pub struct CapsAggregator {
    /// Backend snapshots by name, kept for future aggregation.
    registered: Mutex<HashMap<String, String>>,
    /// GNOME extension link state.
    extension_connected: Mutex<bool>,
    /// Live typed backend snapshots by name (`windows`, `x11`, `wayland`).
    backends: Mutex<HashMap<String, BackendCaps>>,
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
    /// Create an aggregator with no extension connected and no backends.
    #[must_use]
    pub fn new() -> Self {
        Self {
            registered: Mutex::new(HashMap::new()),
            extension_connected: Mutex::new(false),
            backends: Mutex::new(HashMap::new()),
        }
    }

    /// Register one backend's capability summary under `name`.
    pub fn register(&self, name: String, summary: String) {
        if let Ok(mut map) = self.registered.lock() {
            map.insert(name, summary);
        }
    }

    /// Register one backend's typed capability snapshot under `name`.
    ///
    /// Re-registering the same name replaces that backend's entry only;
    /// other backends are untouched (dual-backend merge, seg-06 S8).
    pub fn register_backend(&self, name: &str, caps: BackendCaps) {
        if let Ok(mut backends) = self.backends.lock() {
            backends.insert(name.to_string(), caps);
        }
    }

    /// Update one registered backend's paste strategy in place.
    pub fn set_paste_strategy(&self, name: &str, strategy: PasteStrategy) {
        if let Ok(mut backends) = self.backends.lock() {
            if let Some(caps) = backends.get_mut(name) {
                if caps.paste.strategy != PasteStrategy::ElevatedRoute {
                    caps.paste.strategy = strategy;
                }
            }
        }
    }

    /// Flip the GNOME extension link state.
    pub fn set_extension_connected(&self, connected: bool) {
        if let Ok(mut flag) = self.extension_connected.lock() {
            *flag = connected;
        }
    }

    /// Aggregate snapshot as JSON (levels as lowercase strings).
    ///
    /// With live backends the top-level keys keep their existing shape with
    /// best-of levels, and a per-backend breakdown rides in the additive
    /// `backends` map.
    #[must_use]
    pub fn snapshot(&self) -> Value {
        let extension_up = self.extension_connected.lock().map_or(true, |f| *f);
        let backends = self
            .backends
            .lock()
            .map(|slot| slot.clone())
            .unwrap_or_default();
        if !backends.is_empty() {
            return Self::merged_snapshot(&backends, extension_up);
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

    /// Rank a level for best-of merging (Full > Degraded > Unavailable).
    fn rank(level: Level) -> u8 {
        match level {
            Level::Full => 2,
            Level::Degraded => 1,
            Level::Unavailable => 0,
        }
    }

    /// Merge per-backend snapshots: top-level keys keep their shape with
    /// best-of levels, notes carry `[name]` prefixes, and the per-backend
    /// breakdown rides in the additive `backends` map.
    fn merged_snapshot(backends: &HashMap<String, BackendCaps>, extension_up: bool) -> Value {
        let mut names: Vec<&String> = backends.keys().collect();
        names.sort();
        let pick = |level: fn(&BackendCaps) -> Level| {
            names
                .iter()
                .map(|name| level(&backends[*name]))
                .max_by_key(|level| Self::rank(*level))
                .unwrap_or(Level::Unavailable)
        };
        let paste = names
            .iter()
            .map(|name| &backends[*name].paste)
            .max_by_key(|paste| Self::rank(paste.level));
        let mut prefixed_notes = Vec::new();
        for name in &names {
            for note in &backends[*name].notes {
                prefixed_notes.push(format!("[{name}] {note}"));
            }
        }
        let merged = BackendCaps {
            monitor: pick(|caps| caps.monitor),
            transfer: pick(|caps| caps.transfer),
            hotkey: pick(|caps| caps.hotkey),
            paste: paste
                .cloned()
                .unwrap_or(clipmonger_platform::types::PasteCaps {
                    level: Level::Unavailable,
                    strategy: PasteStrategy::Unavailable,
                    reason: String::new(),
                }),
            tray: pick(|caps| caps.tray),
            autostart: names.iter().any(|name| backends[*name].autostart),
            primary_selection: names.iter().any(|name| backends[*name].primary_selection),
            notes: prefixed_notes,
        };
        let mut snapshot = Self::typed_snapshot(&merged, extension_up);
        let mut breakdown = serde_json::Map::new();
        for name in names {
            breakdown.insert(
                (*name).clone(),
                Self::typed_snapshot(&backends[name], extension_up),
            );
        }
        snapshot["backends"] = Value::Object(breakdown);
        snapshot
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

/// Decode standard padded base64; `None` on any malformed input.
fn b64_decode(text: &str) -> Option<Vec<u8>> {
    fn value(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let pad = chunk.iter().rev().take_while(|&&b| b == b'=').count();
        if pad > 2 || (pad > 0 && !chunk[4 - pad..].iter().all(|&b| b == b'=')) {
            return None;
        }
        let mut vals = [0u8; 4];
        for (i, &b) in chunk.iter().enumerate() {
            vals[i] = if b == b'=' { 0 } else { value(b)? };
        }
        let triple = (u32::from(vals[0]) << 18)
            | (u32::from(vals[1]) << 12)
            | (u32::from(vals[2]) << 6)
            | u32::from(vals[3]);
        out.push(((triple >> 16) & 0xFF) as u8);
        if pad < 2 {
            out.push(((triple >> 8) & 0xFF) as u8);
        }
        if pad < 1 {
            out.push((triple & 0xFF) as u8);
        }
    }
    Some(out)
}

/// Lowercase hex SHA-256 of `data` (self-contained FIPS 180-4).
#[allow(clippy::too_many_lines, clippy::many_single_char_names)]
fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];
    let mut state: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    let mut padded = data.to_vec();
    let bit_len = u64::try_from(data.len())
        .unwrap_or(u64::MAX)
        .wrapping_mul(8);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());
    let (blocks, _) = padded.as_chunks::<64>();
    for block in blocks {
        let mut w = [0_u32; 64];
        let (words, _) = block.as_chunks::<4>();
        for (i, word) in words.iter().enumerate().take(16) {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h) = (
            state[0], state[1], state[2], state[3], state[4], state[5], state[6], state[7],
        );
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }
    let mut out = String::with_capacity(64);
    for word in state {
        use std::fmt::Write as _;
        let _ = write!(out, "{word:08x}");
    }
    out
}

/// In-flight `blob.put` assembly for one `(clip, mime)` stream.
struct BlobAsm {
    data: Vec<u8>,
    next_seq: u64,
    updated_ms: i64,
}

/// Apply one dotted config key (`section.field`) to `config`.
///
/// Known fields are typed by the `Config` serde shape; unknown fields are
/// preserved through its `extra` maps. Implemented via a JSON round-trip so
/// no TOML value type is needed here.
fn apply_config_key(
    config: &mut clipmonger_core::Config,
    key: &str,
    value: &Value,
) -> Result<(), String> {
    if key.trim().is_empty() {
        return Err("config.set: empty key".to_string());
    }
    let mut root = serde_json::to_value(&*config).map_err(|e| e.to_string())?;
    let parts: Vec<&str> = key.split('.').collect();
    if parts.iter().any(|part| part.is_empty()) {
        return Err(format!("{key}: empty key segment"));
    }
    let mut cursor = &mut root;
    for part in &parts[..parts.len() - 1] {
        let obj = cursor
            .as_object_mut()
            .ok_or_else(|| format!("{key}: not an object"))?;
        cursor = obj
            .entry((*part).to_string())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if !cursor.is_object() {
            *cursor = Value::Object(serde_json::Map::new());
        }
    }
    let last = parts[parts.len() - 1];
    cursor
        .as_object_mut()
        .ok_or_else(|| format!("{key}: not an object"))?
        .insert(last.to_string(), value.clone());
    *config = serde_json::from_value(root).map_err(|e| format!("{key}: {e}"))?;
    Ok(())
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
        .find(|(mime, _)| mime.split(';').next().map(str::trim) == Some("text/plain"))
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
    /// Clip store for history methods. Interior mutability so
    /// `backup.restore` can reopen the store on the restored profile
    /// while other arms hold only read guards.
    store: RwLock<Store>,
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
    /// Portal shortcut bind outstanding (system dialog may be showing).
    portal_bind_pending: Mutex<bool>,
    /// Loaded config served by `config.get`/`config.set`.
    config: Mutex<clipmonger_core::Config>,
    /// In-flight `blob.put` assemblies keyed by `(clip, mime)`.
    blob_buf: Mutex<HashMap<(String, String), BlobAsm>>,
    /// Latest `hotkey.conflict` note surfaced in caps notes.
    hotkey_conflict_note: Mutex<Option<String>>,
    /// Dual-backend merge window state (seg-06 S8, W-F5: 2000 ms).
    #[cfg(all(unix, feature = "backend-wayland"))]
    merger: Mutex<clipmonger_backend_wayland::merge::Merger>,
}

impl DaemonState {
    /// Wrap an opened store with fresh skeleton caps.
    #[must_use]
    pub fn new(store: Store) -> Self {
        Self::with_config(store, clipmonger_core::Config::default())
    }

    /// Wrap an opened store with the loaded `config`.
    #[must_use]
    pub fn with_config(store: Store, config: clipmonger_core::Config) -> Self {
        Self {
            store: RwLock::new(store),
            caps: CapsAggregator::new(),
            monitoring_paused: Mutex::new(false),
            paste_limits: Mutex::new((65536, 1000)),
            profile_dir: Mutex::new(PathBuf::new()),
            gesture_tokens: Mutex::new(HashMap::new()),
            gesture_counter: Mutex::new(0),
            elevation_refused: Mutex::new(false),
            helper: Mutex::new(HelperSupervisor::new()),
            portal_bind_pending: Mutex::new(false),
            config: Mutex::new(config),
            blob_buf: Mutex::new(HashMap::new()),
            hotkey_conflict_note: Mutex::new(None),
            #[cfg(all(unix, feature = "backend-wayland"))]
            merger: Mutex::new(clipmonger_backend_wayland::merge::Merger::new(2000)),
        }
    }

    /// Replace the stored config (call once at startup with the loaded one).
    pub fn set_config(&self, config: clipmonger_core::Config) {
        if let Ok(mut slot) = self.config.lock() {
            *slot = config;
        }
    }

    /// Flip the GNOME extension link state in caps.
    pub fn set_extension_connected(&self, connected: bool) {
        self.caps.set_extension_connected(connected);
    }

    /// Stored config as JSON (full document).
    fn config_json(&self) -> Value {
        self.config.lock().map_or(Value::Null, |config| {
            serde_json::to_value(&*config).unwrap_or(Value::Null)
        })
    }

    /// Point audit/helper events at `<profile>` (call once at startup).
    pub fn set_profile_dir(&self, dir: &Path) {
        if let Ok(mut slot) = self.profile_dir.lock() {
            *slot = dir.to_path_buf();
        }
        #[cfg(all(windows, feature = "backend-windows"))]
        if let Ok(mut sup) = self.helper.lock() {
            sup.start(dir);
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

    /// Whether a verified elevated helper is connected right now.
    #[must_use]
    pub fn helper_connected(&self) -> bool {
        self.helper
            .lock()
            .is_ok_and(|supervisor| supervisor.connected())
    }

    /// Helper route state for `status`: route, liveness, refusal latch.
    #[must_use]
    pub fn helper_route_state(&self) -> Value {
        let alive = self.helper_connected();
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
        if let Ok(note) = self.hotkey_conflict_note.lock() {
            if let Some(note) = note.as_ref() {
                if let Some(notes) = snapshot.get_mut("notes").and_then(Value::as_array_mut) {
                    notes.push(Value::from(note.clone()));
                }
            }
        }
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

    /// Typed backend snapshot for live backends (WIN-F3). For the Windows
    /// backend, a configured clipboard-restore delivery is reflected in
    /// the reported paste strategy.
    pub fn register_backend(&self, name: &str, mut caps: BackendCaps) {
        if name == "windows" {
            let delivery = self
                .config
                .lock()
                .map_or(clipmonger_core::PasteDelivery::ClipboardRestore, |c| {
                    c.popup.delivery
                });
            if delivery == clipmonger_core::PasteDelivery::ClipboardRestore
                && caps.paste.strategy != PasteStrategy::ElevatedRoute
            {
                caps.paste.strategy = PasteStrategy::ClipboardRestore;
            }
        }
        self.caps.register_backend(name, caps);
    }

    /// Override the paste delivery budget from config.
    pub fn set_paste_limits(&self, unicode_limit: u64, dwell_ms: u64) {
        if let Ok(mut limits) = self.paste_limits.lock() {
            *limits = (unicode_limit, dwell_ms);
        }
    }

    /// Current paste delivery budget; delivery is read live from the
    /// stored config so UI changes apply without a restart.
    #[cfg_attr(not(all(windows, feature = "backend-windows")), allow(dead_code))]
    fn paste_budget(&self) -> (u64, u64, clipmonger_core::PasteDelivery) {
        let (unicode_limit, dwell_ms) = self
            .paste_limits
            .lock()
            .map_or((65536, 1000), |limits| *limits);
        let delivery = self
            .config
            .lock()
            .map_or(clipmonger_core::PasteDelivery::ClipboardRestore, |config| {
                config.popup.delivery
            });
        (unicode_limit, dwell_ms, delivery)
    }

    /// Mark a portal shortcut bind as outstanding or settled (W-F4).
    pub fn set_portal_bind_pending(&self, pending: bool) {
        if let Ok(mut flag) = self.portal_bind_pending.lock() {
            *flag = pending;
        }
    }

    /// Whether a portal shortcut bind is outstanding.
    #[must_use]
    pub fn portal_bind_pending(&self) -> bool {
        self.portal_bind_pending.lock().is_ok_and(|flag| *flag)
    }

    /// Ingest one live monitor observation with already-read `payloads`.
    ///
    /// Sensitive observations only bump `privacy_skips` and store nothing.
    /// With the wayland feature, bridged X11↔Wayland duplicates inside the
    /// merge window are suppressed (first delivery wins, seg-06 S8 W-F5).
    /// Returns whether a row was stored.
    pub fn ingest_capture(
        &self,
        origin: &str,
        sensitive: bool,
        payloads: &[(String, Vec<u8>)],
    ) -> bool {
        #[cfg(all(unix, feature = "backend-wayland"))]
        {
            if !sensitive && !payloads.is_empty() {
                let key = clipmonger_backend_wayland::merge::MergeKey::of(&payloads.to_vec());
                let mimes: Vec<String> = payloads.iter().map(|(mime, _)| mime.clone()).collect();
                let at_ms = u64::try_from(now_ms().max(0)).unwrap_or_default();
                let duplicate = self.merger.lock().is_ok_and(|mut merger| {
                    merger.offer(origin, &key, &mimes, at_ms)
                        == clipmonger_backend_wayland::merge::MergeDecision::SuppressDuplicate
                });
                if duplicate {
                    return false;
                }
            }
        }
        let seed = u64::try_from(now_ms().max(0)).unwrap_or_default();
        let Ok(store) = self.store.read() else {
            return false;
        };
        matches!(
            store_payloads(&store, origin, sensitive, payloads, seed),
            (StoreDecision::Stored, _)
        )
    }

    /// Find a stored clip id by its string form without naming the id type.
    fn find_clip(&self, id: &str) -> Option<clipmonger_core::model::Clip> {
        // Bounded scan window: `u64::MAX` overflows the SQL `LIMIT` cast
        // and silently yields no rows (every get/paste misses).
        let Ok(store) = self.store.read() else {
            return None;
        };
        let clips = store.list_history(10_000, 0, true).unwrap_or_default();
        clips.into_iter().find(|c| c.id.to_string() == id)
    }

    /// Whether the monitor is currently paused.
    fn paused(&self) -> bool {
        self.monitoring_paused.lock().is_ok_and(|flag| *flag)
    }

    /// Read guard on the clip store for `&self` arms.
    fn store_read(&self) -> Result<std::sync::RwLockReadGuard<'_, Store>, IpcError> {
        self.store
            .read()
            .map_err(|_| IpcError::new("internal", "store lock poisoned".to_string()))
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
                    .store_read()?
                    .list_history(limit, offset, include_removed)
                    .map_err(|e| IpcError::new("backend", e.to_string()))?;
                Ok(serde_json::json!({"clips": clips}))
            }
            "history.get" => match self.requested_clip(params) {
                Some(found) => match self.store_read()?.get_clip(found.id) {
                    Ok(Some((clip, _))) => Ok(serde_json::json!({"clip": clip})),
                    Ok(None) => Ok(serde_json::json!({"clip": null})),
                    Err(e) => Err(IpcError::new("backend", e.to_string())),
                },
                None => Ok(serde_json::json!({"clip": null})),
            },
            "history.delete" => match self.requested_clip(params) {
                Some(found) => self
                    .store_read()?
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
                    Some(found) => match self.store_read()?.get_clip(found.id) {
                        Ok(Some((mut clip, payloads))) => {
                            clip.pinned = pinned;
                            self.store_read()?
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
            "history.purge_removed" => {
                let purged = self.store_read()?.purge_removed();
                Ok(serde_json::json!({"purged": purged}))
            }
            other => Err(IpcError::new(
                "unknown_method",
                format!("unknown method: {other}"),
            )),
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

    /// Dispatch `capture.report`: decode `{event, payloads:{mime: base64}}`
    /// and store the real payloads via `ingest_capture` (G-F3). Sensitive
    /// events only bump the privacy counter through the same path.
    fn handle_capture(&self, params: &Value) -> Value {
        let event = params.get("event").cloned().unwrap_or(Value::Null);
        let sensitive = event
            .get("sensitive")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if sensitive {
            self.ingest_capture("gnome", true, &[]);
            return serde_json::json!({"accepted": false});
        }
        let payloads_value = params
            .get("payloads")
            .or_else(|| event.get("payloads"))
            .cloned()
            .unwrap_or(Value::Null);
        let mut payloads: Vec<(String, Vec<u8>)> = Vec::new();
        match &payloads_value {
            Value::Object(map) => {
                let mut mimes: Vec<&String> = map.keys().collect();
                mimes.sort();
                for mime in mimes {
                    let Some(encoded) = map.get(mime).and_then(Value::as_str) else {
                        return serde_json::json!({"accepted": false});
                    };
                    let Some(bytes) = b64_decode(encoded) else {
                        return serde_json::json!({"accepted": false});
                    };
                    payloads.push((mime.clone(), bytes));
                }
            }
            Value::Array(items) => {
                for item in items {
                    let (Some(mime), Some(encoded)) = (
                        item.get("mime").and_then(Value::as_str),
                        item.get("inline_b64")
                            .or_else(|| item.get("data_b64"))
                            .or_else(|| item.get("b64"))
                            .and_then(Value::as_str),
                    ) else {
                        continue;
                    };
                    let Some(bytes) = b64_decode(encoded) else {
                        return serde_json::json!({"accepted": false});
                    };
                    payloads.push((mime.to_string(), bytes));
                }
            }
            _ => {}
        }
        if payloads.is_empty() {
            return serde_json::json!({"accepted": false});
        }
        let accepted = self.ingest_capture("gnome", false, &payloads);
        serde_json::json!({"accepted": accepted})
    }

    /// Dispatch `blob.put`: assemble chunks keyed by `(clip, mime)`; on
    /// `eof`, verify the total SHA-256 when present and ingest the assembled
    /// payload as one capture.
    fn handle_blob_put(&self, params: &Value) -> Result<Value, IpcError> {
        let clip = params
            .get("clip")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let mime = params
            .get("mime")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if clip.is_empty() || mime.is_empty() {
            return Err(IpcError::new(
                "backend",
                "blob.put: clip and mime required".to_string(),
            ));
        }
        let seq = params
            .get("seq")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX);
        let eof = params.get("eof").and_then(Value::as_bool).unwrap_or(false);
        let sha256 = params
            .get("sha256")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let chunk = params
            .get("data_b64")
            .and_then(Value::as_str)
            .and_then(b64_decode)
            .ok_or_else(|| IpcError::new("backend", "blob.put: bad base64 chunk".to_string()))?;
        let max_bytes = self
            .config
            .lock()
            .map_or(16 * 1024 * 1024, |config| config.monitor.max_bytes);
        let key = (clip, mime.clone());
        let assembled: Option<Vec<u8>> = {
            let mut guard = self
                .blob_buf
                .lock()
                .map_err(|_| IpcError::new("internal", "blob state poisoned".to_string()))?;
            let now = now_ms();
            guard.retain(|_, asm| now >= asm.updated_ms && now - asm.updated_ms <= 60_000);
            let asm = guard.entry(key.clone()).or_insert_with(|| BlobAsm {
                data: Vec::new(),
                next_seq: 0,
                updated_ms: now,
            });
            if seq != asm.next_seq {
                guard.remove(&key);
                return Err(IpcError::new(
                    "backend",
                    "blob.put: non-contiguous seq".to_string(),
                ));
            }
            asm.data.extend_from_slice(&chunk);
            asm.next_seq = asm.next_seq.wrapping_add(1);
            asm.updated_ms = now;
            if asm.data.len() as u64 > max_bytes {
                guard.remove(&key);
                return Err(IpcError::new(
                    "backend",
                    "blob.put: assembled blob exceeds max_bytes".to_string(),
                ));
            }
            if eof {
                guard.remove(&key).map(|done| done.data)
            } else {
                None
            }
        };
        if let Some(data) = assembled {
            if let Some(expected) = sha256 {
                if !expected.eq_ignore_ascii_case(&sha256_hex(&data)) {
                    return Err(IpcError::new(
                        "backend",
                        "blob.put: sha256 mismatch".to_string(),
                    ));
                }
            }
            self.ingest_capture("gnome", false, &[(mime, data)]);
        }
        Ok(serde_json::json!({"ok": true}))
    }

    /// Dispatch `config.get`: full stored config JSON, or one dotted key.
    fn handle_config_get(&self, params: &Value) -> Value {
        let full = self.config_json();
        let Some(key) = params.get("key").and_then(Value::as_str) else {
            return serde_json::json!({"config": full});
        };
        if key.is_empty() {
            return serde_json::json!({"config": full});
        }
        let mut cursor = &full;
        for part in key.split('.') {
            match cursor.get(part) {
                Some(next) => cursor = next,
                None => return serde_json::json!({"config": Value::Null}),
            }
        }
        serde_json::json!({"config": cursor.clone()})
    }

    /// Dispatch `config.set`: apply one dotted key and persist
    /// `<profile>/config.toml`.
    fn handle_config_set(&self, params: &Value) -> Result<Value, IpcError> {
        let profile = self
            .profile_dir
            .lock()
            .map_or_else(|_| PathBuf::new(), |dir| dir.clone());
        if profile.as_os_str().is_empty() {
            return Err(IpcError::new(
                "unavailable",
                "config.set: no profile dir configured".to_string(),
            ));
        }
        let key = params
            .get("key")
            .and_then(Value::as_str)
            .ok_or_else(|| IpcError::new("backend", "config.set: key required".to_string()))?;
        let value = params.get("value").cloned().unwrap_or(Value::Null);
        let updated = {
            let mut config = self
                .config
                .lock()
                .map_err(|_| IpcError::new("internal", "config state poisoned".to_string()))?;
            apply_config_key(&mut config, key, &value)
                .map_err(|e| IpcError::new("backend", format!("config.set: {e}")))?;

            config.clone()
        };
        updated
            .save(&profile.join("config.toml"))
            .map_err(|e| IpcError::new("backend", format!("config.set: {e}")))?;
        if let Ok(mut cfg) = self.config.lock() {
            *cfg = updated.clone();
        }
        self.set_paste_limits(updated.popup.unicode_limit, updated.popup.delivery_dwell_ms);
        self.caps.set_paste_strategy(
            "windows",
            match updated.popup.delivery {
                clipmonger_core::PasteDelivery::ClipboardRestore => PasteStrategy::ClipboardRestore,
                clipmonger_core::PasteDelivery::UnicodeInject => PasteStrategy::UnicodeInject,
            },
        );
        Ok(serde_json::json!({"ok": true}))
    }

    /// Dispatch `backup.create`: snapshot the live store into a ZIP at `dest`.
    fn handle_backup_create(&self, params: &Value) -> Result<Value, IpcError> {
        let dest = params
            .get("dest")
            .and_then(Value::as_str)
            .ok_or_else(|| IpcError::new("backend", "backup.create: dest required".to_string()))?;
        self.store_read()?
            .backup_zip(Path::new(dest))
            .map_err(|e| IpcError::new(e.code(), e.to_string()))?;
        Ok(serde_json::json!({"path": dest}))
    }

    /// Dispatch `backup.restore`: transactionally restore a ZIP over the
    /// profile, then reopen the store so the same process serves the
    /// restored clips. A bad `src` leaves the old DB in place.
    fn handle_backup_restore(&self, params: &Value) -> Result<Value, IpcError> {
        let src = params
            .get("src")
            .and_then(Value::as_str)
            .ok_or_else(|| IpcError::new("backend", "backup.restore: src required".to_string()))?;
        let profile = self
            .profile_dir
            .lock()
            .map_or_else(|_| PathBuf::new(), |dir| dir.clone());
        if profile.as_os_str().is_empty() {
            return Err(IpcError::new(
                "unavailable",
                "backup.restore: no profile dir configured".to_string(),
            ));
        }
        Store::restore_zip(Path::new(src), &profile)
            .map_err(|e| IpcError::new(e.code(), e.to_string()))?;
        let reopened = Store::open(&profile).map_err(|e| IpcError::new(e.code(), e.to_string()))?;
        match self.store.write() {
            Ok(mut slot) => {
                *slot = reopened;
            }
            Err(_) => {
                return Err(IpcError::new("internal", "store lock poisoned".to_string()));
            }
        }
        Ok(serde_json::json!({"ok": true}))
    }

    /// Dispatch `logs.tail`: the last `lines` of `<profile>/audit.log`,
    /// capped at 1000. A missing log is an empty Ok; only audit lines
    /// are ever returned, never clipboard payload bytes.
    fn handle_logs_tail(&self, params: &Value) -> Result<Value, IpcError> {
        let want = params
            .get("lines")
            .and_then(Value::as_u64)
            .unwrap_or(100)
            .min(1000) as usize;
        let path = self
            .profile_dir
            .lock()
            .map_or_else(|_| PathBuf::new(), |dir| dir.join("audit.log"));
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(serde_json::json!({"lines": []}));
            }
            Err(e) => {
                return Err(IpcError::new(
                    "backend",
                    format!("logs.tail: cannot read audit log: {e}"),
                ));
            }
        };
        let all: Vec<&str> = text.lines().collect();
        let start = all.len().saturating_sub(want);
        let lines: Vec<&str> = all[start..].to_vec();
        Ok(serde_json::json!({"lines": lines}))
    }

    /// Dispatch one `permanent.*` method (perm.md §2).
    fn handle_permanent(&self, method: &str, params: &Value) -> Result<Value, IpcError> {
        let perm_err = |e: CoreError| IpcError::new(e.code(), e.to_string());
        match method {
            "permanent.list" => {
                let group = params.get("group").and_then(Value::as_str);
                let permanents = self
                    .store_read()?
                    .list_permanents(group)
                    .map_err(perm_err)?;
                Ok(serde_json::json!({"permanents": permanents}))
            }
            "permanent.create" => {
                let created = self
                    .store_read()?
                    .create_permanent(
                        params.get("group").and_then(Value::as_str).unwrap_or(""),
                        params.get("name").and_then(Value::as_str).unwrap_or(""),
                        params.get("hotkey").and_then(Value::as_str),
                        params.get("body").and_then(Value::as_str).unwrap_or(""),
                    )
                    .map_err(perm_err)?;
                Ok(serde_json::json!({"permanent": created}))
            }
            "permanent.update" => {
                let Some(raw) = params.get("permanent") else {
                    return Err(IpcError::new(
                        "backend",
                        "permanent.update: permanent required".to_string(),
                    ));
                };
                let clip: clipmonger_core::model::PermanentClip =
                    serde_json::from_value(raw.clone())
                        .map_err(|e| IpcError::new("backend", format!("permanent.update: {e}")))?;
                let updated = self
                    .store_read()?
                    .update_permanent(&clip)
                    .map_err(perm_err)?;
                Ok(serde_json::json!({"updated": updated}))
            }
            "permanent.delete" => {
                let id = params
                    .get("id")
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse::<uuid::Uuid>().ok())
                    .ok_or_else(|| {
                        IpcError::new("backend", "permanent.delete: id required".to_string())
                    })?;
                let deleted = self.store_read()?.delete_permanent(id).map_err(perm_err)?;
                Ok(serde_json::json!({"deleted": deleted}))
            }
            "permanent.groups" => {
                let groups = self.store_read()?.list_perm_groups().map_err(perm_err)?;
                let list: Vec<Value> = groups
                    .iter()
                    .map(|g| {
                        serde_json::json!({"name": g.name, "system": g.system, "count": g.count})
                    })
                    .collect();
                Ok(serde_json::json!({"groups": list}))
            }
            "permanent.group_create" => {
                let created = self
                    .store_read()?
                    .create_perm_group(params.get("name").and_then(Value::as_str).unwrap_or(""))
                    .map_err(perm_err)?;
                Ok(serde_json::json!({"created": created}))
            }
            "permanent.group_delete" => {
                let deleted = self
                    .store_read()?
                    .delete_perm_group(params.get("name").and_then(Value::as_str).unwrap_or(""))
                    .map_err(perm_err)?;
                Ok(serde_json::json!({"deleted": deleted}))
            }
            "permanent.exec" => self.handle_permanent_exec(params),
            other => Err(IpcError::new(
                "unknown_method",
                format!("unknown method: {other}"),
            )),
        }
    }

    /// Execute one Paste permanent through the shared paste path.
    fn handle_permanent_exec(&self, params: &Value) -> Result<Value, IpcError> {
        let perm_err = |e: CoreError| IpcError::new(e.code(), e.to_string());
        let id = params
            .get("id")
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<uuid::Uuid>().ok());
        let Some(id) = id else {
            return Err(IpcError::new(
                "backend",
                "permanent.exec: unknown permanent".to_string(),
            ));
        };
        let perm = self
            .store_read()?
            .get_permanent(id)
            .map_err(perm_err)?
            .ok_or_else(|| {
                IpcError::new("backend", "permanent.exec: unknown permanent".to_string())
            })?;
        if perm.kind != clipmonger_core::model::PermanentKind::Paste {
            return Err(IpcError::new(
                "unsupported",
                "permanent.exec: kind is v2".to_string(),
            ));
        }
        let payloads: Vec<(String, Vec<u8>)> = vec![(
            "text/plain;charset=utf-8".to_string(),
            perm.body.as_bytes().to_vec(),
        )];
        self.paste_payloads(
            clipmonger_platform::types::ClipRef(perm.id),
            &payloads,
            params,
        )?;
        Ok(serde_json::json!({"ok": true}))
    }

    /// Dispatch `paste.execute` through the live Windows backend, or refuse
    /// with `unavailable` when no live backend is compiled in.
    #[allow(clippy::unused_self)] // self used only in backend cfg branches
    fn handle_paste_execute(&self, params: &Value) -> Result<Value, IpcError> {
        #[cfg(any(
            all(windows, feature = "backend-windows"),
            all(unix, feature = "backend-wayland")
        ))]
        {
            let found = self.requested_clip(params).ok_or_else(|| {
                IpcError::new("backend", "paste.execute: unknown clip".to_string())
            })?;
            let payloads = self
                .store_read()?
                .get_clip(found.id)
                .map_err(|e| IpcError::new("backend", e.to_string()))?
                .map(|(_, payloads)| payloads)
                .unwrap_or_default();
            self.paste_payloads(
                clipmonger_platform::types::ClipRef(found.id),
                &payloads,
                params,
            )
        }
        #[cfg(not(any(
            all(windows, feature = "backend-windows"),
            all(unix, feature = "backend-wayland")
        )))]
        {
            let _ = params;
            Err(IpcError::new(
                "unavailable",
                format!("paste unavailable: {SKELETON_NOTE}"),
            ))
        }
    }

    /// Deliver already-resolved payloads through the live backend.
    ///
    /// High-integrity targets take the elevated-helper route ONLY, gated on
    /// a fresh single-use gesture token (`gesture` param). Generic IPC
    /// callers cannot mint tokens, so tokenless elevated requests are
    /// refused without ever spawning.
    // `self` is only used by the Windows arm; other backends are stateless.
    #[allow(clippy::unused_self)]
    fn paste_payloads(
        &self,
        clip: clipmonger_platform::types::ClipRef,
        payloads: &[(String, Vec<u8>)],
        params: &Value,
    ) -> Result<Value, IpcError> {
        #[cfg(all(windows, feature = "backend-windows"))]
        {
            use clipmonger_backend_windows::Integrity;
            use clipmonger_platform::traits::PasteEngine as _;
            let (unicode_limit, dwell_ms, delivery) = self.paste_budget();
            let restore = self
                .config
                .lock()
                .map(|cfg| cfg.popup.restore_clipboard)
                .unwrap_or(true);
            let paste = clipmonger_backend_windows::WinPaste::new(
                usize::try_from(unicode_limit).unwrap_or(65536),
            )
            .with_dwell_ms(dwell_ms)
            .with_clipboard_first(delivery == clipmonger_core::PasteDelivery::ClipboardRestore)
            .with_restore_clipboard(restore);
            // Deadline counted from the pick in the popup (or from this
            // request): a paste that cannot happen promptly is refused, never
            // performed late into whatever window has focus by then.
            let now = now_ms();
            let deadline_ms = params
                .get("picked_ms")
                .and_then(Value::as_i64)
                .map_or(now, |picked| picked.min(now))
                + PASTE_DEADLINE_MS;
            if now > deadline_ms {
                return Err(IpcError::new(
                    "unavailable",
                    format!("paste.execute: {PASTE_EXPIRED}"),
                ));
            }
            let target = match params.get("target_hwnd").and_then(Value::as_u64) {
                // The window that was foreground when the popup opened; the
                // foreground right now may still be settling after the hide.
                Some(hwnd) => {
                    clipmonger_backend_windows::foreground::target_for_hwnd(hwnd.cast_signed())
                }
                None => paste.capture_target(),
            }
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
                let res = self.paste_via_helper(&target, payloads, deadline_ms);
                if res.is_ok() {
                    let _ = self
                        .store
                        .read()
                        .ok()
                        .and_then(|s| s.bump_clip(clip.0, now_ms()).ok());
                }
                return res;
            }
            paste
                .paste_payloads(&target, &clip, payloads)
                .map_err(|e| {
                    let message = e.to_string();
                    if message.starts_with("denied") || message.contains("refused") {
                        IpcError::new("denied", format!("paste.execute: {e}"))
                    } else {
                        IpcError::new("backend", format!("paste.execute: {e}"))
                    }
                })?;
            let _ = self
                .store
                .read()
                .ok()
                .and_then(|s| s.bump_clip(clip.0, now_ms()).ok());
            Ok(serde_json::json!({"pasted": true}))
        }
        #[cfg(all(unix, feature = "backend-wayland"))]
        {
            use clipmonger_backend_wayland::{probe_current, WlPaste};
            let _ = (clip, params);
            let has_data =
                probe_current().protocol != clipmonger_backend_wayland::probe::Protocol::None;
            let paste = WlPaste::new(has_data);
            let outcome = paste
                .paste_payloads(&payloads.to_vec())
                .map_err(|e| IpcError::new("unavailable", format!("paste.execute: {e}")))?;
            let mut result = serde_json::json!({"pasted": true});
            if let Some(toast) = outcome.toast {
                result["toast"] = Value::from(toast);
            }
            Ok(result)
        }
        #[cfg(not(any(
            all(windows, feature = "backend-windows"),
            all(unix, feature = "backend-wayland")
        )))]
        {
            let _ = (self, clip, payloads, params);
            Err(IpcError::new(
                "unavailable",
                format!("paste unavailable: {SKELETON_NOTE}"),
            ))
        }
    }

    /// Paste into a high-integrity target via the elevated helper
    /// (Windows only): resolve the target pid, hand one job to the
    /// supervisor thread, and wait bounded for its result. Without a
    /// connected helper this fails at once, before the clipboard is touched.
    /// Refusals latch caps paste Degraded.
    #[cfg(all(windows, feature = "backend-windows"))]
    fn paste_via_helper(
        &self,
        target: &clipmonger_platform::types::PasteTarget,
        payloads: &[(String, Vec<u8>)],
        deadline_ms: i64,
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
        if !self.helper_connected() {
            return Err(helper_ipc_error(self, target, HELPER_NOT_RUNNING));
        }
        let (_, dwell_ms, delivery) = self.paste_budget();
        let restore = self
            .config
            .lock()
            .map(|cfg| cfg.popup.restore_clipboard)
            .unwrap_or(true);
        if delivery == clipmonger_core::PasteDelivery::ClipboardRestore {
            let swap = clipmonger_backend_windows::paste::ClipboardSwap::begin(payloads, restore)
                .map_err(|e| IpcError::new("backend", format!("paste.execute: {e}")))?;
            let outcome = self.send_helper_job(
                target,
                pid,
                hwnd,
                deadline_ms,
                clipmonger_backend_windows::helper::JobOp::PasteChord,
                String::new(),
            );
            let dwell = if restore {
                if outcome.is_ok() {
                    dwell_ms
                } else {
                    0
                }
            } else {
                0
            };
            if restore {
                swap.finish(dwell, true);
            }
            return outcome;
        }
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
        self.send_helper_job(
            target,
            pid,
            hwnd,
            deadline_ms,
            clipmonger_backend_windows::helper::JobOp::SendInputBatch,
            payload,
        )
    }

    /// Hand one job to the supervisor thread and wait bounded for its result
    /// (the job carries its own deadline, so a slow answer never turns into
    /// a late paste).
    #[cfg(all(windows, feature = "backend-windows"))]
    fn send_helper_job(
        &self,
        target: &clipmonger_platform::types::PasteTarget,
        pid: u32,
        hwnd: i64,
        deadline_ms: i64,
        op: clipmonger_backend_windows::helper::JobOp,
        payload: String,
    ) -> Result<Value, IpcError> {
        let jobs_tx = self
            .helper
            .lock()
            .map_err(|_| {
                IpcError::new(
                    "internal",
                    "paste.execute: helper state poisoned".to_string(),
                )
            })?
            .jobs
            .clone();
        let Some(jobs_tx) = jobs_tx else {
            return Err(helper_ipc_error(self, target, HELPER_NOT_RUNNING));
        };
        let (reply_tx, reply_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let job = SupervisorJob {
            target_pid: pid,
            target_hwnd: Some(hwnd),
            op,
            payload,
            expires_ms: deadline_ms,
            reply: reply_tx,
        };
        if jobs_tx.send(job).is_err() {
            return Err(helper_ipc_error(self, target, HELPER_NOT_RUNNING));
        }
        // The supervisor answers by the deadline plus its 1.5 s result grace.
        let remaining_ms = u64::try_from(deadline_ms - now_ms()).unwrap_or(0);
        match reply_rx.recv_timeout(std::time::Duration::from_millis(remaining_ms + 2_500)) {
            Ok(Ok(())) => Ok(serde_json::json!({"pasted": true})),
            Ok(Err(e)) => Err(helper_ipc_error(self, target, &e)),
            Err(_) => Err(helper_ipc_error(self, target, HELPER_TIMED_OUT)),
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
                "portal_bind_pending": self.portal_bind_pending(),
                "up": true,
                "running": true,
                "version": DAEMON_VERSION,
                "monitoring_paused": self.paused(),
            })),
            "caps.get" | "caps.refresh" => Ok(self.caps_snapshot()),
            "ping" => Ok(serde_json::json!({"pong": true})),
            "paste.execute" => self.handle_paste_execute(params),
            method if method.starts_with("history.") => self.handle_history(method, params),
            method if method.starts_with("permanent.") => self.handle_permanent(method, params),
            "monitor.set" => self.handle_monitor(params),
            "capture.report" => Ok(self.handle_capture(params)),
            "blob.put" => self.handle_blob_put(params),
            "hotkey.fired" => {
                let id = params.get("id").and_then(Value::as_u64);
                if id.is_none() || id == Some(1) {
                    self.mint_gesture_token();
                }
                Ok(serde_json::json!({"ok": true}))
            }
            "hotkey.conflict" => {
                let chord = params
                    .get("chord")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let reason = params
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let note = if reason.is_empty() {
                    format!("hotkey conflict: {chord}")
                } else {
                    format!("hotkey conflict: {chord}: {reason}")
                };
                if let Ok(mut slot) = self.hotkey_conflict_note.lock() {
                    *slot = Some(note);
                }
                Ok(serde_json::json!({"ok": true}))
            }
            "config.get" => Ok(self.handle_config_get(params)),
            "config.set" => self.handle_config_set(params),
            "backup.create" => self.handle_backup_create(params),
            "backup.restore" => self.handle_backup_restore(params),
            "logs.tail" => self.handle_logs_tail(params),
            other => Err(IpcError::new(
                "unknown_method",
                format!("unknown method: {other}"),
            )),
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

    fn fresh_state(name: &str) -> (std::path::PathBuf, DaemonState) {
        let dir = std::env::temp_dir().join(format!("cm-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = clipmonger_core::Store::open(&dir).expect("store");
        (dir, DaemonState::new(store))
    }

    fn history_texts(state: &DaemonState) -> Vec<String> {
        let list = state
            .handle("history.list", &serde_json::json!({}))
            .expect("history.list");
        list.get("clips")
            .and_then(Value::as_array)
            .map(|clips| {
                clips
                    .iter()
                    .map(|clip| {
                        clip.get("search_text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn capture_report_stores_real_payload_not_replay() {
        let (_dir, state) = fresh_state("capture-real");
        let got = state
            .handle(
                "capture.report",
                &serde_json::json!({
                    "event": {"sensitive": false, "origin": "Clipboard"},
                    "payloads": {"text/plain": "aGVsbG8gZ25vbWU="}
                }),
            )
            .expect("capture.report");
        assert_eq!(got.get("accepted").and_then(Value::as_bool), Some(true));
        let texts = history_texts(&state);
        assert!(
            texts.iter().any(|t| t == "hello gnome"),
            "real payload must be stored, got {texts:?}"
        );
        assert!(
            !texts.iter().any(|t| t.contains("headless replay")),
            "no synthetic replay rows, got {texts:?}"
        );
        let sensitive = state
            .handle(
                "capture.report",
                &serde_json::json!({
                    "event": {"sensitive": true},
                    "payloads": {"text/plain": "aGVsbG8gZ25vbWU="}
                }),
            )
            .expect("sensitive capture.report");
        assert_eq!(
            sensitive.get("accepted").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(history_texts(&state).len(), 1, "sensitive stores nothing");
    }

    #[test]
    fn blob_put_assembles_and_verifies_sha() {
        let (_dir, state) = fresh_state("blob-asm");
        let clip = "01900000-0000-7000-8000-000000000001";
        state
            .handle(
                "blob.put",
                &serde_json::json!({"clip": clip, "mime": "text/plain", "seq": 0, "data_b64": "aGVsbG8=", "eof": false}),
            )
            .expect("chunk 0");
        assert!(history_texts(&state).is_empty(), "no ingest before eof");
        state
            .handle(
                "blob.put",
                &serde_json::json!({"clip": clip, "mime": "text/plain", "seq": 1, "data_b64": "IGJsb2I=", "eof": true, "sha256": "e997afd18e5f6be004fc193aed2c90291e68ab2c7599a62538c935b7fca6ab0f"}),
            )
            .expect("chunk 1");
        assert!(
            history_texts(&state).iter().any(|t| t == "hello blob"),
            "assembled payload must be ingested"
        );
        // Bad seq and bad hash are backend errors, not silent ok.
        let bad_seq = state.handle(
            "blob.put",
            &serde_json::json!({"clip": clip, "mime": "text/plain", "seq": 5, "data_b64": "aGk=", "eof": true}),
        );
        assert_eq!(
            bad_seq.expect_err("bad seq").code,
            "backend",
            "non-contiguous seq must fail"
        );
        let bad_hash = state.handle(
            "blob.put",
            &serde_json::json!({"clip": clip, "mime": "text/plain", "seq": 0, "data_b64": "aGk=", "eof": true, "sha256": "00"}),
        );
        assert_eq!(bad_hash.expect_err("bad hash").code, "backend");
    }

    #[test]
    fn config_get_set_roundtrip_and_persist() {
        let (dir, state) = fresh_state("config-rt");
        // No profile dir yet: set is unavailable.
        let unavailable = state.handle(
            "config.set",
            &serde_json::json!({"key": "popup.hotkey", "value": "ctrl+alt+x"}),
        );
        assert_eq!(unavailable.expect_err("no profile").code, "unavailable");
        state.set_profile_dir(&dir);
        state
            .handle(
                "config.set",
                &serde_json::json!({"key": "popup.hotkey", "value": "ctrl+alt+x"}),
            )
            .expect("config.set known");
        let one = state
            .handle("config.get", &serde_json::json!({"key": "popup.hotkey"}))
            .expect("config.get key");
        assert_eq!(
            one.get("config").and_then(Value::as_str),
            Some("ctrl+alt+x")
        );
        state
            .handle(
                "config.set",
                &serde_json::json!({"key": "custom.thing", "value": 123}),
            )
            .expect("config.set unknown preserved");
        let unknown = state
            .handle("config.get", &serde_json::json!({"key": "custom.thing"}))
            .expect("config.get unknown");
        assert_eq!(unknown.get("config").and_then(Value::as_i64), Some(123));
        let loaded = clipmonger_core::Config::load(&dir.join("config.toml")).expect("reload");
        assert_eq!(
            loaded.popup.hotkey, "ctrl+alt+x",
            "set must persist config.toml"
        );
    }

    #[test]
    fn hotkey_fired_mints_and_conflict_notes_caps() {
        let (_dir, state) = fresh_state("hotkey-ipc");
        state
            .handle("hotkey.fired", &serde_json::json!({"id": 1}))
            .expect("hotkey.fired");
        assert!(
            state.consume_trigger_token(now_ms()),
            "hotkey.fired id=1 must mint a gesture token"
        );
        state
            .handle(
                "hotkey.conflict",
                &serde_json::json!({"chord": "ctrl+alt+h", "reason": "already grabbed"}),
            )
            .expect("hotkey.conflict");
        let status = state
            .handle("status", &serde_json::json!({}))
            .expect("status");
        let notes = status
            .pointer("/caps/notes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(
            notes
                .iter()
                .any(|n| n.as_str().unwrap_or_default().contains("ctrl+alt+h")),
            "conflict must surface in caps notes, got {notes:?}"
        );
    }

    #[test]
    fn unknown_history_method_is_unknown_method_and_never_purges() {
        let (_dir, state) = fresh_state("unknown-history");
        let before = history_texts(&state).len();
        let err = state
            .handle("history.bogus", &serde_json::json!({}))
            .expect_err("unknown history method must fail");
        assert_eq!(err.code, "unknown_method");
        assert_eq!(history_texts(&state).len(), before, "history untouched");
        for method in [
            "ui.show_popup",
            "events.subscribe",
            "permanent.bogus",
            "hotkey.register",
            "hotkey.unregister",
        ] {
            let err = state
                .handle(method, &serde_json::json!({}))
                .expect_err("unknown method must fail");
            assert_eq!(err.code, "unknown_method", "{method}");
        }
    }

    #[test]
    fn permanent_exec_refuses_hand_seeded_macro_row() {
        let (dir, state) = fresh_state("perm-macro-exec");
        let macro_id = uuid::Uuid::now_v7();
        let conn = rusqlite::Connection::open(dir.join("clipstore.sqlite3")).expect("raw db");
        conn.execute_batch("INSERT OR IGNORE INTO perm_groups(name, system) VALUES('Work', 0);")
            .expect("seed group");
        conn.execute(
            "INSERT INTO permanents(id, grp, name, hotkey, kind, body, js, rev_ms, rev_dev, rev_ctr) \
             VALUES(?1, 'Work', 'OldMacro', NULL, 'Macro', 'MACRO-BODY-SECRET', NULL, 1, 'dev', 0)",
            [macro_id.to_string()],
        )
        .expect("seed macro row");
        drop(conn);
        let err = state
            .handle(
                "permanent.exec",
                &serde_json::json!({"id": macro_id.to_string()}),
            )
            .expect_err("Macro exec must be refused in v1");
        assert_eq!(err.code, "unsupported", "got {err:?}");
        assert!(
            !err.message.contains("MACRO-BODY-SECRET"),
            "stored body must never appear in error text: {}",
            err.message
        );
    }

    #[test]
    fn caps_extension_flip_marks_gnome_unavailable() {
        let caps = CapsAggregator::new();
        assert_eq!(
            caps.snapshot()
                .get("extension_connected")
                .and_then(Value::as_bool),
            Some(false),
            "no extension has connected yet"
        );
        caps.set_extension_connected(true);
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
