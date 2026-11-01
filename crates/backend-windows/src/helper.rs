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

use crate::foreground::Integrity;

/// Byte budget for [`Job::payload`]: 64 KiB (matches `popup.unicode_limit`).
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
/// Gesture-token time to live: 30 000 ms after minting.
pub const GESTURE_TTL_MS: i64 = 30_000;

#[derive(Debug)]
pub enum GestureError {
    Entropy(clipmonger_core::entropy::Error),
    Exhausted,
}

impl std::fmt::Display for GestureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GestureError::Entropy(e) => write!(f, "entropy unavailable: {e}"),
            GestureError::Exhausted => write!(f, "gesture entropy exhausted"),
        }
    }
}

impl std::error::Error for GestureError {}
/// Heartbeat deadline: 3 missed 15 s beats (45 s) since the last beat.
pub const HEARTBEAT_DEAD_AFTER_MS: i64 = 45_000;

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
    /// Target top-level window (HWND value) captured when the popup opened;
    /// `None` falls back to the current foreground window.
    #[serde(default)]
    pub target_hwnd: Option<i64>,
    /// Absolute deadline (unix ms). The helper refuses to inject after it,
    /// so a late job can never type into whatever window is focused by then.
    #[serde(default)]
    pub expires_ms: Option<i64>,
}

/// Whether a job deadline has passed at `at_ms` (no deadline never expires).
#[must_use]
pub fn job_expired(expires_ms: Option<i64>, at_ms: i64) -> bool {
    expires_ms.is_some_and(|deadline| at_ms > deadline)
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
}

impl GestureTokens {
    /// Empty token store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            issued: HashMap::new(),
        }
    }

    /// Mint one fresh token at `at_ms`.
    ///
    /// # Errors
    /// Returns an error if entropy or token storage is unavailable, or retries exhaust.
    pub fn mint(&mut self, at_ms: i64) -> Result<u64, GestureError> {
        self.mint_with(at_ms, &mut clipmonger_core::entropy::random_u64)
    }

    fn mint_with<F>(&mut self, at_ms: i64, draw: &mut F) -> Result<u64, GestureError>
    where
        F: FnMut() -> Result<u64, clipmonger_core::entropy::Error>,
    {
        for _ in 0..8 {
            let candidate = draw().map_err(GestureError::Entropy)?;
            if candidate == 0 {
                continue;
            }
            if self.issued.contains_key(&candidate) {
                continue;
            }
            self.issued.insert(candidate, at_ms);
            return Ok(candidate);
        }
        Err(GestureError::Exhausted)
    }

    /// Consume `token` at `at_ms`: true exactly once while unexpired.
    /// Consumed, expired, and unknown tokens read false.
    pub fn consume(&mut self, token: u64, at_ms: i64) -> bool {
        let Some(minted) = self.issued.remove(&token) else {
            return false;
        };
        at_ms
            .checked_sub(minted)
            .is_some_and(|age| (0..=GESTURE_TTL_MS).contains(&age))
    }
}

impl Default for GestureTokens {
    fn default() -> Self {
        Self::new()
    }
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
///
/// # Errors
/// Returns an error when operating system entropy is unavailable.
pub fn random_pipe_segment() -> Result<String, clipmonger_core::entropy::Error> {
    let a = clipmonger_core::entropy::random_u64()?;
    let b = clipmonger_core::entropy::random_u64()?;
    Ok(format!("{a:016x}{b:016x}"))
}

/// Quote one command-line argument for the standard Windows argv parser.
/// Paths cannot contain `"`, so only trailing backslashes (which would
/// escape the closing quote) need doubling.
#[must_use]
pub fn quote_arg(value: &str) -> String {
    let trailing = value.len() - value.trim_end_matches('\\').len();
    format!("\"{value}{}\"", "\\".repeat(trailing))
}

#[must_use]
pub fn persistent_pipe_name(profile: &std::path::Path) -> String {
    let hash = clipmonger_core::hash::fnv1a64(profile.to_string_lossy().as_bytes());
    format!(r"\\.\pipe\clipmonger-persist-{hash:016x}")
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

/// Revalidate the job target and pin it by open process HANDLE held across
/// the injection: the job must be unexpired, the target window alive and
/// owned by `target_pid`, the process high-integrity, and the window the
/// foreground window. Anything else is a refusal, never blind input.
///
/// The popup hands focus back as it hides; if Windows has not settled on
/// the target yet, the helper (high integrity, so UIPI does not apply)
/// activates exactly that window and waits briefly. The wait is capped by
/// the job deadline, so a late job can never steal focus.
///
/// # Errors
///
/// Returns the refusal reason when the job expired, the target window is
/// gone or not high-integrity, it never became foreground, or it cannot be
/// pinned.
#[cfg(windows)]
pub fn pin_helper_target(job: &Job) -> Result<PinnedTarget, String> {
    use std::time::{Duration, Instant};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    if job_expired(job.expires_ms, unix_ms()) {
        return Err("refused: job expired".to_string());
    }
    let hwnd = match job.target_hwnd {
        Some(raw) => {
            let hwnd = raw as windows_sys::Win32::Foundation::HWND;
            if !crate::sys::is_window(hwnd) {
                return Err("refused: target window is gone".to_string());
            }
            hwnd
        }
        None => crate::sys::foreground_hwnd()
            .ok_or_else(|| "refused: no foreground window".to_string())?,
    };
    let pid =
        crate::sys::window_pid(hwnd).ok_or_else(|| "refused: target owner unknown".to_string())?;
    if pid != job.target_pid {
        return Err("refused: target window does not belong to target process".to_string());
    }
    if crate::sys::process_token_high(pid) != Some(true) {
        return Err("refused: target is not high-integrity".to_string());
    }
    let settle = Instant::now() + Duration::from_secs(1);
    let mut next_activate = Instant::now();
    while crate::sys::foreground_hwnd() != Some(hwnd) {
        if job_expired(job.expires_ms, unix_ms()) || Instant::now() >= settle {
            return Err("refused: target is not the foreground window".to_string());
        }
        if Instant::now() >= next_activate {
            crate::sys::activate_window(hwnd);
            next_activate = Instant::now() + Duration::from_millis(250);
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    // Safety: OpenProcess with a query flag; closed by `PinnedTarget::drop`.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return Err("refused: cannot pin target process".to_string());
    }
    Ok(PinnedTarget { pid, handle })
}

/// Wall clock in unix ms (job deadlines are wall-clock values shared by the
/// daemon and the helper on the same machine).
#[cfg(windows)]
fn unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistent_pipe_name_matches_fnv1a64_vectors() {
        for (profile, expected) in [
            ("", r"\\.\pipe\clipmonger-persist-cbf29ce484222325"),
            ("a", r"\\.\pipe\clipmonger-persist-af63dc4c8601ec8c"),
            ("foobar", r"\\.\pipe\clipmonger-persist-85944171f73967e8"),
            ("fo", r"\\.\pipe\clipmonger-persist-08985907b541d342"),
        ] {
            assert_eq!(
                persistent_pipe_name(std::path::Path::new(profile)),
                expected,
                "profile: {profile:?}"
            );
        }
    }

    #[test]
    fn persistent_pipe_name_proves_zero_padding_for_leading_zero_vector() {
        let pipe = persistent_pipe_name(std::path::Path::new("fo"));
        assert_eq!(pipe, r"\\.\pipe\clipmonger-persist-08985907b541d342");
        assert!(
            pipe.contains("08985907b541d342"),
            "must preserve leading zero padding"
        );
        assert_eq!(
            clipmonger_core::hash::fnv1a64(b"fo"),
            0x0898_5907_b541_d342,
            "core helper must agree on fo vector"
        );
    }

    #[test]
    fn paste_chord_job_roundtrips_with_empty_payload() {
        let job = Job {
            job_id: 7,
            target_pid: 42,
            op: JobOp::PasteChord,
            payload: String::new(),
            target_hwnd: Some(0x0003_0838),
            expires_ms: Some(1_000),
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
            target_hwnd: None,
            expires_ms: None,
        };
        let line = encode_job(&job).unwrap();
        assert!(line.contains("\"SendInputBatch\""), "{line}");
        assert_eq!(decode_job(&line).unwrap(), job);
    }

    #[test]
    fn job_without_target_or_deadline_still_decodes() {
        let line = "{\"job_id\":3,\"target_pid\":9,\"op\":\"PasteChord\",\"payload\":\"\"}\n";
        let job = decode_job(line).unwrap();
        assert_eq!(job.target_hwnd, None);
        assert_eq!(job.expires_ms, None);
    }

    #[test]
    fn job_expiry_is_strictly_after_deadline() {
        assert!(!job_expired(None, i64::MAX));
        assert!(!job_expired(Some(1_000), 999));
        assert!(!job_expired(Some(1_000), 1_000));
        assert!(job_expired(Some(1_000), 1_001));
    }
}

#[cfg(test)]
mod gesture_entropy_tests {
    use super::*;

    #[test]
    fn t413_unit_token_entropy_failure_leaves_no_entry() {
        const T: &str = "t413_unit_token_entropy_failure_leaves_no_entry";
        let mut tokens = GestureTokens::new();
        let mut failing = || Err(clipmonger_core::entropy::Error::UNSUPPORTED);
        assert!(
            tokens.mint_with(1000, &mut failing).is_err(),
            "{T}: must fail"
        );
        assert!(tokens.issued.is_empty(), "{T}: no entry on failure");
    }

    #[test]
    fn t413_unit_token_zero_then_success() {
        const T: &str = "t413_unit_token_zero_then_success";
        let mut tokens = GestureTokens::new();
        let mut draws = vec![0u64, 42u64].into_iter();
        let tok = tokens
            .mint_with(1000, &mut || {
                draws
                    .next()
                    .ok_or(clipmonger_core::entropy::Error::UNSUPPORTED)
            })
            .expect("mint");
        assert_eq!(tok, 42, "{T}: zero retry");
    }

    #[test]
    fn t413_unit_token_zero_exhaustion() {
        const T: &str = "t413_unit_token_zero_exhaustion";
        let mut tokens = GestureTokens::new();
        let mut calls = 0;
        let mut draws = || {
            calls += 1;
            Ok(0u64)
        };
        assert!(
            tokens.mint_with(1000, &mut draws).is_err(),
            "{T}: zero exhaustion must fail"
        );
        assert_eq!(calls, 8);
        assert!(tokens.issued.is_empty());
    }

    #[test]
    fn t413_unit_token_collision_then_success_preserves_original() {
        const T: &str = "t413_unit_token_collision_then_success_preserves_original";
        let mut tokens = GestureTokens::new();
        let first = tokens.mint_with(1000, &mut || Ok(100)).expect("first");
        assert_eq!(first, 100);
        let mut draws = vec![100u64, 200u64].into_iter();
        let second = tokens
            .mint_with(1001, &mut || {
                draws
                    .next()
                    .ok_or(clipmonger_core::entropy::Error::UNSUPPORTED)
            })
            .expect("second");
        assert_eq!(second, 200);
        assert_eq!(tokens.issued.get(&100), Some(&1000));
        assert_eq!(tokens.issued.get(&200), Some(&1001));
        assert!(tokens.consume(100, 1001), "{T}: first still valid");
        assert!(tokens.consume(200, 1002), "{T}: second valid");
    }

    #[test]
    fn t413_unit_token_collision_exhaustion_leaves_existing() {
        const T: &str = "t413_unit_token_collision_exhaustion_leaves_existing";
        let mut tokens = GestureTokens::new();
        tokens.mint_with(1000, &mut || Ok(55)).expect("first");
        let mut draws = || Ok(55);
        assert!(
            tokens.mint_with(1001, &mut draws).is_err(),
            "{T}: must fail"
        );
        assert!(tokens.consume(55, 1001), "{T}: first still present");
        assert!(!tokens.consume(55, 1002), "{T}: consumed once");
    }

    #[test]
    fn t413_unit_token_collision_then_entropy_failure_no_fallback() {
        const T: &str = "t413_unit_token_collision_then_entropy_failure_no_fallback";
        let mut tokens = GestureTokens::new();
        tokens.mint_with(1000, &mut || Ok(77)).expect("first");
        let mut calls = 0;
        let mut draw = || {
            calls += 1;
            if calls == 1 {
                Ok(77)
            } else {
                Err(clipmonger_core::entropy::Error::UNSUPPORTED)
            }
        };
        assert!(
            tokens.mint_with(1001, &mut draw).is_err(),
            "{T}: must fail closed"
        );
        assert!(tokens.consume(77, 1002), "{T}: original still present");
    }

    #[test]
    fn backwards_and_extreme_time_burn_tokens() {
        let mut tokens = GestureTokens::new();
        let token = tokens.mint_with(1000, &mut || Ok(1)).unwrap();
        assert!(!tokens.consume(token, 999));
        assert!(!tokens.consume(token, 1000));
        let token = tokens.mint_with(i64::MIN, &mut || Ok(2)).unwrap();
        assert!(!tokens.consume(token, i64::MAX));
    }
}
