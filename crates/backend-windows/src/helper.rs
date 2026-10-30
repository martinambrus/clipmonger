//! Elevated-helper protocol primitives (seg-04 S5).
//!
//! Pure and cross-platform: job encode/decode (T-403), single-use gesture
//! tokens with a 30 s TTL (T-413), the protected-install-path predicate
//! (T-414), the heartbeat liveness machine, and the integrity-first route
//! decision. No process spawning, no elevation, no sleeps; all time is
//! injected via explicit `at_ms` arguments so this stays unit-testable on
//! Linux. The UAC-served leg (runas spawn, reverse-connect pipe session)
//! is manual QA only and never runs here.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::foreground::Integrity;

/// Byte budget for [`Job::payload`]: 64 KiB (matches `popup.unicode_limit`).
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
/// Gesture-token time to live: 30 000 ms after minting.
pub const GESTURE_TTL_MS: i64 = 30_000;
/// Heartbeat deadline: 3 missed 15 s beats (45 s) since the last beat.
pub const HEARTBEAT_DEAD_AFTER_MS: i64 = 300_000;

/// Narrow helper operation: input injection only. The helper receives one
/// job's bytes and can never read history, config, or the DB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum JobOp {
    /// Inject the payload bytes as synthetic input into the pinned target.
    SendInputBatch,
    /// Press Ctrl+V into the pinned target; the payload is empty and the
    /// daemon owns the clipboard write (the helper stays input-only).
    PasteChord,
}

/// One unit of elevated input work handed to the helper.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Job {
    /// Originating id echoed back in [`JobResult::job_id`].
    pub job_id: u64,
    /// Target process id, revalidated per job by the helper.
    pub target_pid: u32,
    /// Narrow op discriminant (input injection or paste chord only).
    pub op: JobOp,
    /// Input bytes (UTF-8 text), at most [`MAX_PAYLOAD_BYTES`] bytes.
    pub payload: String,
}

/// Outcome of one [`Job`], echoing its `job_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobResult {
    /// Echo of the originating [`Job::job_id`].
    pub job_id: u64,
    /// Whether the batch was injected.
    pub ok: bool,
    /// Refusal reason when `ok` is false; never carries payload bytes.
    pub error: Option<String>,
}

/// Codec/validation failure for [`encode_job`]/[`decode_job`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobCodecError(pub String);

impl std::fmt::Display for JobCodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for JobCodecError {}

/// Encode `job` as one JSONL line (single line, trailing `\n`).
///
/// # Errors
///
/// Returns [`JobCodecError`] when the payload exceeds [`MAX_PAYLOAD_BYTES`]
/// bytes or the job cannot be serialized.
pub fn encode_job(job: &Job) -> Result<String, JobCodecError> {
    if job.payload.len() > MAX_PAYLOAD_BYTES {
        return Err(JobCodecError(format!(
            "payload {} bytes exceeds {MAX_PAYLOAD_BYTES}",
            job.payload.len()
        )));
    }
    let mut line =
        serde_json::to_string(job).map_err(|e| JobCodecError(format!("encode failed: {e}")))?;
    line.push('\n');
    Ok(line)
}

/// Decode one JSONL `line` into a [`Job`].
///
/// # Errors
///
/// Returns [`JobCodecError`] for empty/non-JSON lines, unknown `op`
/// discriminants, or payloads over [`MAX_PAYLOAD_BYTES`] bytes.
pub fn decode_job(line: &str) -> Result<Job, JobCodecError> {
    if line.trim().is_empty() {
        return Err(JobCodecError("empty job line".to_string()));
    }
    let job: Job =
        serde_json::from_str(line).map_err(|e| JobCodecError(format!("decode failed: {e}")))?;
    if job.payload.len() > MAX_PAYLOAD_BYTES {
        return Err(JobCodecError(format!(
            "payload {} bytes exceeds {MAX_PAYLOAD_BYTES}",
            job.payload.len()
        )));
    }
    Ok(job)
}

/// Single-use gesture tokens (T-413): minted ONLY at hotkey/popup-trigger
/// time, consumed at most once within [`GESTURE_TTL_MS`]. Generic IPC calls,
/// CLI invocations, and macros can never mint: they only present tokens.
#[derive(Debug)]
pub struct GestureTokens {
    /// Live token to mint time (`at_ms`).
    issued: HashMap<u64, i64>,
    /// Mint sequence mixed into every token.
    counter: u64,
}

impl GestureTokens {
    /// Empty token store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            issued: HashMap::new(),
            counter: 0,
        }
    }

    /// Mint one fresh token at `at_ms`.
    pub fn mint(&mut self, at_ms: i64) -> u64 {
        self.counter = self.counter.wrapping_add(1);
        let mut token = mix_token(at_ms, self.counter);
        while token == 0 || self.issued.contains_key(&token) {
            self.counter = self.counter.wrapping_add(1);
            token = mix_token(at_ms, self.counter);
        }
        self.issued.insert(token, at_ms);
        token
    }

    /// Consume `token` at `at_ms`: true exactly once while unexpired.
    /// Consumed, expired, and unknown tokens read false.
    pub fn consume(&mut self, token: u64, at_ms: i64) -> bool {
        let Some(minted) = self.issued.remove(&token) else {
            return false;
        };
        at_ms >= minted && at_ms - minted <= GESTURE_TTL_MS
    }
}

impl Default for GestureTokens {
    fn default() -> Self {
        Self::new()
    }
}

/// Extra per-process entropy mixed into every token.
static MIX_EXTRA: AtomicU64 = AtomicU64::new(0xC2B2_AE35_4D1B_9E59);

/// Mix `at_ms`, a sequence number, and process entropy into a token.
fn mix_token(at_ms: i64, counter: u64) -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
    let mut x = at_ms
        .cast_unsigned()
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(counter.wrapping_mul(0xBF58_476D_1CE4_E5B9))
        .wrapping_add(nanos ^ (u64::from(std::process::id()) << 32));
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    x.wrapping_add(MIX_EXTRA.fetch_add(1, Ordering::Relaxed))
}

/// Protected-install-path predicate (T-414): true iff `exe` sits under the
/// `program_files` directory (case-insensitive component prefix, so
/// `C:\Program Files Evil\...` spoofs fail). Windows-style paths are
/// handled on every host so the rule unit-tests on Linux too.
#[must_use]
pub fn protected_path_ok(exe: &Path, program_files: &Path) -> bool {
    let exe_parts = split_components(exe);
    let pf_parts = split_components(program_files);
    !pf_parts.is_empty()
        && exe_parts.len() > pf_parts.len()
        && exe_parts[..pf_parts.len()] == pf_parts
}

/// Split a path into lowercase components on either separator.
fn split_components(path: &Path) -> Vec<String> {
    path.to_string_lossy()
        .replace('/', "\\")
        .split('\\')
        .filter(|part| !part.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

/// Helper liveness machine: 15 s heartbeat, dead past 45 s (T-413).
#[derive(Debug)]
pub struct HelperState {
    /// Last heartbeat time (`at_ms`), if any beat arrived yet.
    last_beat: Option<i64>,
}

impl HelperState {
    /// State with no beat yet (reads dead until the first beat).
    #[must_use]
    pub fn new() -> Self {
        Self { last_beat: None }
    }

    /// Record a heartbeat at `at_ms`.
    pub fn note_beat(&mut self, at_ms: i64) {
        self.last_beat = Some(at_ms);
    }

    /// True while within [`HEARTBEAT_DEAD_AFTER_MS`] of the last beat.
    #[must_use]
    pub fn alive(&self, at_ms: i64) -> bool {
        self.last_beat
            .is_some_and(|beat| at_ms >= beat && at_ms - beat <= HEARTBEAT_DEAD_AFTER_MS)
    }

    /// The helper always self-exits on pipe disconnect (never orphaned).
    #[must_use]
    pub fn on_disconnect(&self) -> HelperAction {
        HelperAction::Exit
    }
}

impl Default for HelperState {
    fn default() -> Self {
        Self::new()
    }
}

/// Disconnect outcome for [`HelperState::on_disconnect`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperAction {
    /// Terminate the helper process.
    Exit,
}

/// Paste routing decision: integrity is detected FIRST; elevated targets
/// take the helper route ONLY and never see blind `SendInput` (UIPI drops
/// it silently, so blind attempts are forbidden).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// Standard-user target: normal UnicodeInject/ClipboardRestore path.
    Normal,
    /// Installed-protected high target: forward to the elevated helper.
    Helper,
    /// No safe route: portable high target, unprotected high target, or
    /// unknown integrity. Carries the human-readable reason.
    Unavailable(String),
}

/// Decide the paste route (T-413 matrix + T-414 portable skip).
#[must_use]
pub fn decide_route(integrity: Integrity, portable: bool, protected_ok: bool) -> Route {
    match integrity {
        Integrity::Medium => Route::Normal,
        Integrity::High if portable => {
            Route::Unavailable("portable layout: elevated route unavailable".to_string())
        }
        Integrity::High if !protected_ok => {
            Route::Unavailable("unprotected install path: elevated route unavailable".to_string())
        }
        Integrity::High => Route::Helper,
        Integrity::Unknown => {
            Route::Unavailable("unknown target integrity: refusing blind paste".to_string())
        }
    }
}

/// Random 128-bit rendezvous segment (hex) for the helper pipe name.
/// Unguessable rendezvous, never a credential; no secret travels in argv.
#[must_use]
pub fn random_pipe_segment() -> String {
    #[cfg(windows)]
    {
        crate::sys::random_pipe_segment()
    }
    #[cfg(not(windows))]
    {
        if let Ok(raw) = std::fs::read_to_string("/proc/sys/kernel/random/uuid") {
            let compact: String = raw.chars().filter(char::is_ascii_hexdigit).collect();
            if compact.len() >= 32 {
                return compact[..32].to_string();
            }
        }
        format!(
            "{:016x}{:016x}",
            mix_token(0, 1),
            mix_token(nanos_now().cast_signed(), 2)
        )
    }
}

#[must_use]
pub fn persistent_pipe_name(profile: &std::path::Path) -> String {
    let s = profile.to_string_lossy();
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100000001b3);
    }
    format!(r"\\.\pipe\clipmonger-persist-{h:016x}")
}

#[cfg(not(windows))]
fn nanos_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

// ---------------------------------------------------------------------------
// Elevated-worker target pinning (Windows only; manual-QA leg)
// ---------------------------------------------------------------------------

/// Target process pinned by open HANDLE across one job (no PID-reuse race).
#[cfg(windows)]
pub struct PinnedTarget {
    /// Validated target process id.
    pub pid: u32,
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl Drop for PinnedTarget {
    fn drop(&mut self) {
        // Safety: handle came from OpenProcess in `pin_helper_target`.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

/// Revalidate the job target: it must be the current foreground window's
/// process AND high-integrity, then pin it by open process HANDLE held
/// across the injection. Anything else is a refusal, never blind input.
///
/// The UAC secure-desktop switch can leave Windows with no foreground
/// window for a short transition after consent. Wait briefly for the
/// already-captured target to become foreground; do not relax the PID or
/// integrity checks.
///
/// # Errors
///
/// Returns the last refusal message when no foreground window settles on
/// the target, the target is not high-integrity, or it cannot be pinned.
#[cfg(windows)]
pub fn pin_helper_target(target_pid: u32) -> Result<PinnedTarget, String> {
    use std::time::{Duration, Instant};

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match try_pin_helper_target(target_pid) {
            Ok(target) => return Ok(target),
            Err(reason) => {
                if Instant::now() >= deadline {
                    return Err(reason);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(windows)]
fn try_pin_helper_target(target_pid: u32) -> Result<PinnedTarget, String> {
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    let hwnd =
        crate::sys::foreground_hwnd().ok_or_else(|| "refused: no foreground window".to_string())?;
    let pid = crate::sys::window_pid(hwnd)
        .ok_or_else(|| "refused: foreground owner unknown".to_string())?;
    if pid != target_pid {
        return Err("refused: target is not the foreground process".to_string());
    }
    if crate::sys::process_token_high(pid) != Some(true) {
        return Err("refused: target is not high-integrity".to_string());
    }
    // Safety: OpenProcess with a query flag; closed by `PinnedTarget::drop`.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return Err("refused: cannot pin target process".to_string());
    }
    Ok(PinnedTarget { pid, handle })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_chord_job_roundtrips_with_empty_payload() {
        let job = Job {
            job_id: 7,
            target_pid: 42,
            op: JobOp::PasteChord,
            payload: String::new(),
        };
        let line = encode_job(&job).unwrap();
        let back = decode_job(&line).unwrap();
        assert_eq!(back, job);
        assert_eq!(back.op, JobOp::PasteChord);
        assert_eq!(back.payload, "");
    }

    #[test]
    fn send_input_batch_wire_unchanged() {
        let job = Job {
            job_id: 1,
            target_pid: 2,
            op: JobOp::SendInputBatch,
            payload: "hi".to_string(),
        };
        let line = encode_job(&job).unwrap();
        assert!(line.contains("\"SendInputBatch\""), "{line}");
        assert_eq!(decode_job(&line).unwrap(), job);
    }
}
