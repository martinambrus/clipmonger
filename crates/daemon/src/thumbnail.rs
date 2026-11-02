//! Bounded thumbnail decoder child and supervisor (P4 Part 2).

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const PROTOCOL_VERSION: u8 = 1;
pub const MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_OUTPUT_BYTES: usize = 512 * 1024;
pub const MAX_PIXELS: u64 = 64_000_000;
pub const MIN_PX: u32 = 1;
pub const MAX_PX: u32 = 256;
pub const DEFAULT_MAX_PX: u32 = 128;
pub const DEFAULT_TIMEOUT_MS: u64 = 1500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbnailOutput {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThumbnailError {
    InvalidInput(String),
    TooLarge(String),
    DecodeFailed(String),
    EncodeFailed(String),
    OutputTooLarge(String),
    Timeout,
    Busy,
    Cancelled,
    Shutdown,
    Protocol(String),
    Io(String),
    SpawnFailed(String),
}

impl ThumbnailError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidInput(_) => "invalid_input",
            Self::TooLarge(_) => "too_large",
            Self::DecodeFailed(_) => "decode_failed",
            Self::EncodeFailed(_) => "encode_failed",
            Self::OutputTooLarge(_) => "output_too_large",
            Self::Timeout => "timeout",
            Self::Busy => "busy",
            Self::Cancelled => "cancelled",
            Self::Shutdown => "shutdown",
            Self::Protocol(_) => "protocol_error",
            Self::Io(_) => "io",
            Self::SpawnFailed(_) => "spawn_failed",
        }
    }
    pub fn message(&self) -> String {
        match self {
            Self::InvalidInput(m) | Self::TooLarge(m) | Self::DecodeFailed(m) | Self::EncodeFailed(m) | Self::OutputTooLarge(m) | Self::Protocol(m) | Self::Io(m) | Self::SpawnFailed(m) => m.clone(),
            Self::Timeout => "thumbnail timeout".to_string(),
            Self::Busy => "thumbnail busy".to_string(),
            Self::Cancelled => "thumbnail cancelled".to_string(),
            Self::Shutdown => "thumbnail shutdown".to_string(),
        }
    }
}

impl std::fmt::Display for ThumbnailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code(), self.message())
    }
}
impl std::error::Error for ThumbnailError {}

fn checked_mul_u64(a: u64, b: u64) -> Option<u64> {
    a.checked_mul(b)
}

pub fn decode_thumbnail(input: &[u8], max_px: u32) -> Result<ThumbnailOutput, ThumbnailError> {
    if !(MIN_PX..=MAX_PX).contains(&max_px) {
        return Err(ThumbnailError::InvalidInput("max_px out of range".to_string()));
    }
    if input.len() > MAX_INPUT_BYTES {
        return Err(ThumbnailError::TooLarge("input exceeds 16MiB".to_string()));
    }
    if input.is_empty() {
        return Err(ThumbnailError::InvalidInput("empty input".to_string()));
    }
    if input.len() < 8 || input[0..8] != [137, 80, 78, 71, 13, 10, 26, 10] {
        return Err(ThumbnailError::DecodeFailed("not PNG".to_string()));
    }
    use image::codecs::png::{PngDecoder, PngEncoder};
    use image::{ImageDecoder, ImageEncoder, Limits};
    use std::io::Cursor;
    let mut limits = Limits::default();
    limits.max_alloc = Some(256 * 1024 * 1024);
    let cursor = Cursor::new(input);
    let decoder = PngDecoder::with_limits(cursor, limits.clone())
        .map_err(|_| ThumbnailError::DecodeFailed("png header failed".to_string()))?;
    let (width, height) = decoder.dimensions();
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_PIXELS {
        return Err(ThumbnailError::TooLarge("dimensions exceed 64MP".to_string()));
    }
    if width == 0 || height == 0 {
        return Err(ThumbnailError::InvalidInput("zero dimension".to_string()));
    }
    let rgba_bytes = checked_mul_u64(pixels, 4).ok_or_else(|| ThumbnailError::TooLarge("rgba overflow".to_string()))?;
    if rgba_bytes > 256 * 1024 * 1024 {
        return Err(ThumbnailError::TooLarge("rgba too large".to_string()));
    }
    let rgba = image::DynamicImage::from_decoder(decoder)
        .map_err(|_| ThumbnailError::DecodeFailed("decode failed".to_string()))?
        .to_rgba8();
    let (orig_w, orig_h) = (width, height);
    let (target_w, target_h) = compute_target_dimensions(orig_w, orig_h, max_px);
    let final_image = if target_w == orig_w && target_h == orig_h {
        rgba
    } else {
        let out_pixels = u64::from(target_w) * u64::from(target_h);
        if out_pixels > MAX_PIXELS {
            return Err(ThumbnailError::TooLarge("scaled dimensions exceed guard".to_string()));
        }
        let out_bytes = checked_mul_u64(out_pixels, 4).ok_or_else(|| ThumbnailError::TooLarge("scaled rgba overflow".to_string()))?;
        if out_bytes > 256 * 1024 * 1024 {
            return Err(ThumbnailError::TooLarge("scaled rgba too large".to_string()));
        }
        image::imageops::resize(&rgba, target_w, target_h, image::imageops::FilterType::Triangle)
    };
    let (fw, fh) = final_image.dimensions();
    let mut out = CappedWriter { bytes: Vec::new(), cap: MAX_OUTPUT_BYTES, exceeded: false };
    let encoded = PngEncoder::new(&mut out)
        .write_image(final_image.as_raw(), fw, fh, image::ExtendedColorType::Rgba8);
    if out.exceeded {
        return Err(ThumbnailError::OutputTooLarge("encoded png exceeds 512KiB".to_string()));
    }
    encoded.map_err(|_| ThumbnailError::EncodeFailed("png encode failed".to_string()))?;
    let out = out.bytes;
    Ok(ThumbnailOutput { png: out, width: fw, height: fh })
}

struct CappedWriter { bytes: Vec<u8>, cap: usize, exceeded: bool }
impl Write for CappedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.len() > self.cap.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "output cap"));
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}

fn compute_target_dimensions(width: u32, height: u32, max_px: u32) -> (u32, u32) {
    if width <= max_px && height <= max_px {
        return (width, height);
    }
    let max_dim = width.max(height) as f64;
    let ratio = max_px as f64 / max_dim;
    let mut nw = (width as f64 * ratio).round() as u32;
    let mut nh = (height as f64 * ratio).round() as u32;
    if nw == 0 { nw = 1; }
    if nh == 0 { nh = 1; }
    while nw > max_px || nh > max_px {
        if nw > nh { nw -= 1; } else { nh -= 1; }
        if nw == 0 { nw = 1; }
        if nh == 0 { nh = 1; }
    }
    (nw, nh)
}

fn write_u32_be<W: Write>(w: &mut W, v: u32) -> std::io::Result<()> {
    w.write_all(&v.to_be_bytes())
}
fn read_u32_be<R: Read>(r: &mut R) -> std::io::Result<u32> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)?;
    Ok(u32::from_be_bytes(buf))
}
fn read_exact_bounded<R: Read>(r: &mut R, len: usize, cap: usize) -> Result<Vec<u8>, ThumbnailError> {
    if len > cap { return Err(ThumbnailError::TooLarge("length exceeds cap".to_string())); }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).map_err(|_| ThumbnailError::Protocol("truncated read".to_string()))?;
    Ok(buf)
}
pub fn run_thumbnail_child() -> i32 {
    match run_thumbnail_child_inner() {
        Ok(()) => 0,
        Err(code) => code,
    }
}
fn run_thumbnail_child_inner() -> Result<(), i32> {
    use std::io::{Read, Write};
    let stdin = std::io::stdin();
    let mut stdin_lock = stdin.lock();
    let stdout = std::io::stdout();
    let mut stdout_lock = stdout.lock();
    let mut ver = [0u8; 1];
    if stdin_lock.read_exact(&mut ver).is_err() {
        let _ = write_error_response(&mut stdout_lock, 1, "missing version");
        return Err(2);
    }
    if ver[0] != PROTOCOL_VERSION {
        let _ = write_error_response(&mut stdout_lock, 1, "bad version");
        return Err(2);
    }
    let max_px = match read_u32_be(&mut stdin_lock) {
        Ok(v) => v,
        Err(_) => { let _ = write_error_response(&mut stdout_lock, 1, "max_px read failed"); return Err(2); }
    };
    if !(MIN_PX..=MAX_PX).contains(&max_px) {
        let _ = write_error_response(&mut stdout_lock, 1, "max_px range");
        return Err(3);
    }
    let input_len = match read_u32_be(&mut stdin_lock) { Ok(v) => v as usize, Err(_) => { let _ = write_error_response(&mut stdout_lock, 1, "len read failed"); return Err(2); } };
    if input_len > MAX_INPUT_BYTES {
        let _ = write_error_response(&mut stdout_lock, 2, "input too large");
        return Err(3);
    }
    let input = match read_exact_bounded(&mut stdin_lock, input_len, MAX_INPUT_BYTES) {
        Ok(b) => b,
        Err(e) => { let _ = write_error_response(&mut stdout_lock, 1, &e.message()); return Err(2); }
    };
    let mut extra = [0u8; 1];
    match stdin_lock.read(&mut extra) {
        Ok(0) => {},
        Ok(_) => { let _ = write_error_response(&mut stdout_lock, 1, "trailing data"); return Err(2); }
        Err(_) => { let _ = write_error_response(&mut stdout_lock, 1, "trailing check failed"); return Err(2); }
    }
    match decode_thumbnail(&input, max_px) {
        Ok(out) => {
            if stdout_lock.write_all(&[PROTOCOL_VERSION, 0]).is_err() { return Err(4); }
            if write_u32_be(&mut stdout_lock, out.width).is_err() { return Err(4); }
            if write_u32_be(&mut stdout_lock, out.height).is_err() { return Err(4); }
            if write_u32_be(&mut stdout_lock, out.png.len() as u32).is_err() { return Err(4); }
            if stdout_lock.write_all(&out.png).is_err() { return Err(4); }
            let _ = stdout_lock.flush();
            Ok(())
        }
        Err(e) => {
            let code = match e {
                ThumbnailError::InvalidInput(_) => 1,
                ThumbnailError::DecodeFailed(_) => 3,
                ThumbnailError::TooLarge(_) => 2,
                ThumbnailError::OutputTooLarge(_) => 4,
                ThumbnailError::EncodeFailed(_) => 4,
                ThumbnailError::Protocol(_) => 2,
                _ => 5,
            };
            let msg = e.message();
            let bounded = if msg.len() > 256 { &msg[..256] } else { &msg };
            let _ = write_error_response(&mut stdout_lock, code as u8, bounded);
            Err(code)
        }
    }
}
fn write_error_response<W: Write>(w: &mut W, status: u8, msg: &str) -> std::io::Result<()> {
    w.write_all(&[PROTOCOL_VERSION, status])?;
    let bytes = msg.as_bytes();
    let len = std::cmp::min(bytes.len(), 256) as u32;
    write_u32_be(w, len)?;
    w.write_all(&bytes[..len as usize])?;
    w.flush()
}

pub struct ThumbnailService {
    exe: PathBuf,
    timeout: Duration,
    active: Arc<Mutex<bool>>,
    shutdown: Arc<AtomicBool>,
    init_error: Option<String>,
    #[cfg(test)] fixture: Option<(PathBuf, Arc<Mutex<Option<(u32, bool, bool)>>>)>,
    #[cfg(test)] cleanup_hook: Option<Arc<dyn Fn() + Send + Sync>>,
    #[cfg(test)] live_checks: Option<Arc<std::sync::atomic::AtomicUsize>>,
    #[cfg(test)] wait_hook: Option<Arc<dyn Fn() + Send + Sync>>,
}

pub struct ThumbnailPermit<'a> {
    service: &'a ThumbnailService,
    _guard: std::sync::MutexGuard<'a, bool>,
}

impl<'a> ThumbnailPermit<'a> {
    pub fn decode(&self, input: &[u8], max_px: u32) -> Result<ThumbnailOutput, ThumbnailError> {
        self.decode_with_cancel(input, max_px, None)
    }
    pub fn decode_with_cancel(&self, input: &[u8], max_px: u32, cancel: Option<Arc<AtomicBool>>) -> Result<ThumbnailOutput, ThumbnailError> {
        if !(MIN_PX..=MAX_PX).contains(&max_px) {
            return Err(ThumbnailError::InvalidInput("max_px out of range".to_string()));
        }
        if input.len() > MAX_INPUT_BYTES {
            return Err(ThumbnailError::TooLarge("input exceeds 16MiB".to_string()));
        }
        if let Some(err) = self.service.check_unavailable() {
            return Err(err);
        }
        self.service.spawn_and_wait(input, max_px, cancel)
    }
}

impl<'a> Drop for ThumbnailPermit<'a> {
    fn drop(&mut self) {
        *self._guard = false;
    }
}
impl ThumbnailService {
    pub fn new() -> Result<Self, ThumbnailError> {
        let exe = current_exe_absolute().map_err(|_| ThumbnailError::SpawnFailed("exe resolve failed".to_string()))?;
        Ok(Self { exe, timeout: Duration::from_millis(DEFAULT_TIMEOUT_MS), active: Arc::new(Mutex::new(false)), shutdown: Arc::new(AtomicBool::new(false)), init_error: None, #[cfg(test)] fixture: None, #[cfg(test)] cleanup_hook: None, #[cfg(test)] live_checks: None, #[cfg(test)] wait_hook: None })
    }
    pub fn new_for_daemon() -> Self {
        match current_exe_absolute() {
            Ok(exe) => Self { exe, timeout: Duration::from_millis(DEFAULT_TIMEOUT_MS), active: Arc::new(Mutex::new(false)), shutdown: Arc::new(AtomicBool::new(false)), init_error: None, #[cfg(test)] fixture: None, #[cfg(test)] cleanup_hook: None, #[cfg(test)] live_checks: None, #[cfg(test)] wait_hook: None },
            Err(_) => Self { exe: PathBuf::new(), timeout: Duration::from_millis(DEFAULT_TIMEOUT_MS), active: Arc::new(Mutex::new(false)), shutdown: Arc::new(AtomicBool::new(false)), init_error: Some("exe resolve failed".to_string()), #[cfg(test)] fixture: None, #[cfg(test)] cleanup_hook: None, #[cfg(test)] live_checks: None, #[cfg(test)] wait_hook: None },
        }
    }
    #[cfg(test)]
    pub(crate) fn controlled_fixture(path: PathBuf, observation: Arc<Mutex<Option<(u32, bool, bool)>>>, hook: Option<Arc<dyn Fn() + Send + Sync>>, live_checks: Arc<std::sync::atomic::AtomicUsize>, wait_hook: Option<Arc<dyn Fn() + Send + Sync>>) -> Self {
        let mut service = Self::new_with_exe(std::env::current_exe().expect("test executable"), Duration::from_secs(5));
        service.fixture = Some((path, observation));
        service.cleanup_hook = hook;
        service.live_checks = Some(live_checks);
        service.wait_hook = wait_hook;
        service
    }
    fn check_unavailable(&self) -> Option<ThumbnailError> {
        self.init_error.as_ref().map(|m| ThumbnailError::SpawnFailed(m.clone()))
    }
    pub fn try_admit(&self) -> Result<ThumbnailPermit<'_>, ThumbnailError> {
        if let Some(err) = self.check_unavailable() {
            return Err(err);
        }
        if self.is_shutdown() {
            return Err(ThumbnailError::Shutdown);
        }
        let mut guard = match self.active.try_lock() {
            Ok(g) => g,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return Err(ThumbnailError::Busy),
        };
        if *guard {
            return Err(ThumbnailError::Busy);
        }
        *guard = true;
        if self.is_shutdown() {
            *guard = false;
            return Err(ThumbnailError::Shutdown);
        }
        if let Some(err) = self.check_unavailable() {
            *guard = false;
            return Err(err);
        }
        Ok(ThumbnailPermit { service: self, _guard: guard })
    }
    pub fn shutdown_and_wait(&self) {
        self.shutdown();
        self.wait_idle();
    }
    pub fn wait_idle(&self) {
        #[cfg(test)]
        if let Some(hook) = &self.wait_hook { hook(); }
        let guard = match self.active.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let _ = *guard;
    }
    #[doc(hidden)]
    pub fn new_with_exe(exe: PathBuf, timeout: Duration) -> Self {
        let bounded = timeout.clamp(Duration::from_millis(50), Duration::from_millis(10_000));
        Self { exe, timeout: bounded, active: Arc::new(Mutex::new(false)), shutdown: Arc::new(AtomicBool::new(false)), init_error: None, #[cfg(test)] fixture: None, #[cfg(test)] cleanup_hook: None, #[cfg(test)] live_checks: None, #[cfg(test)] wait_hook: None }
    }
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout.clamp(Duration::from_millis(10), Duration::from_millis(10_000));
        self
    }
    pub fn shutdown(&self) { self.shutdown.store(true, Ordering::SeqCst); }
    pub fn is_shutdown(&self) -> bool { self.shutdown.load(Ordering::SeqCst) }
    pub fn decode(&self, input: &[u8], max_px: u32) -> Result<ThumbnailOutput, ThumbnailError> {
        self.decode_with_cancel(input, max_px, None)
    }
    pub fn decode_with_cancel(&self, input: &[u8], max_px: u32, cancel: Option<Arc<AtomicBool>>) -> Result<ThumbnailOutput, ThumbnailError> {
        if !(MIN_PX..=MAX_PX).contains(&max_px) { return Err(ThumbnailError::InvalidInput("max_px out of range".to_string())); }
        if input.len() > MAX_INPUT_BYTES { return Err(ThumbnailError::TooLarge("input exceeds 16MiB".to_string())); }
        let permit = self.try_admit()?;
        permit.decode_with_cancel(input, max_px, cancel)
    }
    fn spawn_and_wait(&self, input: &[u8], max_px: u32, cancel: Option<Arc<AtomicBool>>) -> Result<ThumbnailOutput, ThumbnailError> {
        use std::process::{Command, Stdio};
        // Spawn itself and OS scheduling cannot be preempted; account for them once they return.
        let deadline = Instant::now() + self.timeout;
        let exe_abs = self.exe.canonicalize().map_err(|_| ThumbnailError::SpawnFailed("exe resolve failed".into()))?;
        if !exe_abs.is_absolute() { return Err(ThumbnailError::SpawnFailed("exe not absolute".into())); }
        let mut cmd = Command::new(exe_abs);
        cmd.arg("__thumbnail-decode").env_clear().stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        // Windows process startup may need SystemRoot; no profile or application environment is inherited.
        #[cfg(test)]
        if let Some((path, _)) = &self.fixture {
            cmd = Command::new(&self.exe);
            cmd.args(["--exact", "thumbnail::tests::fixture_child", "--nocapture"])
                .env_clear().env("THUMBNAIL_FIXTURE_FILE", path)
                .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            if let Some(root) = std::env::var_os("SystemRoot") { cmd.env("SystemRoot", root); }
            cmd.creation_flags(0x0800_0000);
        }
        if self.is_shutdown() { return Err(ThumbnailError::Shutdown); }
        if cancel.as_ref().is_some_and(|c| c.load(Ordering::SeqCst)) { return Err(ThumbnailError::Cancelled); }
        if Instant::now() >= deadline { return Err(ThumbnailError::Timeout); }
        let child = cmd.spawn().map_err(|_| ThumbnailError::SpawnFailed("spawn failed".into()))?;
        // Installed immediately, before extracting pipes, allocating input or starting threads.
        let mut guard = ProcessGuard { child: Some(child), writer: None, reader: None };
        #[cfg(test)]
        let child_pid = guard.child.as_ref().unwrap().id();
        let mut stdin = guard.child.as_mut().unwrap().stdin.take().ok_or_else(|| ThumbnailError::Io("no stdin".into()))?;
        let mut stdout = guard.child.as_mut().unwrap().stdout.take().ok_or_else(|| ThumbnailError::Io("no stdout".into()))?;
        let input_owned = input.to_vec();
        guard.writer = Some(std::thread::Builder::new().name("thumbnail-writer".into()).spawn(move || {
            stdin.write_all(&[PROTOCOL_VERSION]).and_then(|_| write_u32_be(&mut stdin, max_px))
                .and_then(|_| write_u32_be(&mut stdin, input_owned.len() as u32))
                .and_then(|_| stdin.write_all(&input_owned)).and_then(|_| stdin.flush())
                .map_err(|_| ThumbnailError::Io("request write failed".into()))
        }).map_err(|_| ThumbnailError::Io("writer spawn failed".into()))?);
        let (reader_errors, reader_notifications) = std::sync::mpsc::sync_channel(1);
        #[cfg(test)]
        let fixture_prefix = self.fixture.is_some();
        guard.reader = Some(std::thread::Builder::new().name("thumbnail-reader".into()).spawn(move || {
            #[cfg(test)]
            let result = if fixture_prefix {
                read_fixture_prefix(&mut stdout).and_then(|_| read_response_bounded(&mut stdout, max_px))
            } else { read_response_bounded(&mut stdout, max_px) };
            #[cfg(not(test))]
            let result = read_response_bounded(&mut stdout, max_px);
            if let Err(ThumbnailError::Protocol(message)) = &result {
                // Exactly one bounded notification; the join handle retains the result.
                let _ = reader_errors.try_send(ThumbnailError::Protocol(message.clone()));
            }
            result
        }).map_err(|_| ThumbnailError::Io("reader spawn failed".into()))?);
        let reason = loop {
            if self.is_shutdown() { break Some(ThumbnailError::Shutdown); }
            if cancel.as_ref().is_some_and(|c| c.load(Ordering::SeqCst)) { break Some(ThumbnailError::Cancelled); }
            if let Ok(error) = reader_notifications.try_recv() { break Some(error); }
            if Instant::now() >= deadline { break Some(ThumbnailError::Timeout); }
            match guard.child.as_mut().unwrap().try_wait() {
                Ok(Some(_)) => break None,
                Ok(None) => {
                    #[cfg(test)]
                    if let Some(checks) = &self.live_checks { checks.fetch_add(1, Ordering::SeqCst); }
                    std::thread::sleep(Duration::from_millis(5));
                },
                Err(_) => break Some(ThumbnailError::Io("child wait failed".into())),
            }
        };
        if reason.is_some() && guard.child.as_mut().unwrap().kill().is_err() {
            match guard.child.as_mut().unwrap().try_wait() {
                Ok(Some(_)) => (),
                _ => return Err(ThumbnailError::Io("child kill failed".into())),
            }
        }
        let status = guard.child.as_mut().unwrap().wait().map_err(|_| ThumbnailError::Io("child reap failed".into()))?;
        guard.child.take();
        // Production child never creates descendants, so reaping closes its pipe ends.
        let writer = guard.writer.take().unwrap().join().map_err(|_| ThumbnailError::Io("writer panicked".into()))?;
        let reader = guard.reader.take().unwrap().join().map_err(|_| ThumbnailError::Io("reader panicked".into()))?;
        #[cfg(test)]
        if let Some((_, observation)) = &self.fixture {
            *observation.lock().unwrap() = Some((child_pid, true, true));
            if let Some(hook) = &self.cleanup_hook { hook(); }
        }
        if let Some(reason) = reason { return Err(reason); }
        if Instant::now() >= deadline { return Err(ThumbnailError::Timeout); }
        if let Err(ThumbnailError::Protocol(message)) = &reader {
            return Err(ThumbnailError::Protocol(message.clone()));
        }
        writer?;
        match reader {
            Ok(output) if status.success() => Ok(output),
            Ok(_) => Err(ThumbnailError::Protocol("child exited unsuccessfully".into())),
            Err(ThumbnailError::Protocol(e)) => Err(ThumbnailError::Protocol(e)),
            Err(_) if status.success() => Err(ThumbnailError::Protocol("error response with successful exit".into())),
            Err(e) => {
                let expected = match &e {
                    ThumbnailError::InvalidInput(_) => 1,
                    ThumbnailError::TooLarge(_) => 2,
                    ThumbnailError::DecodeFailed(_) => 3,
                    ThumbnailError::OutputTooLarge(_) => 4,
                    _ => return Err(ThumbnailError::Protocol("unexpected response".into())),
                };
                if status.code() != Some(expected) { return Err(ThumbnailError::Protocol("response status mismatch".into())); }
                Err(e)
            },
        }
    }

}
// Drop order is deliberate: terminate/reap the one owned child before joining pipe threads.
struct ProcessGuard {
    child: Option<std::process::Child>,
    writer: Option<std::thread::JoinHandle<Result<(), ThumbnailError>>>,
    reader: Option<std::thread::JoinHandle<Result<ThumbnailOutput, ThumbnailError>>>,
}
impl Drop for ProcessGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() { let _ = child.kill(); let _ = child.wait(); }
        if let Some(thread) = self.writer.take() { let _ = thread.join(); }
        if let Some(thread) = self.reader.take() { let _ = thread.join(); }
    }
}
fn current_exe_absolute() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    if exe.is_absolute() { Ok(exe) } else { Ok(std::env::current_dir()?.join(exe)) }
}
// Libtest writes a banner before executing the fixture. In unit tests only,
// consume that banner up to an explicit raw marker written by the fixture.
#[cfg(test)]
const FIXTURE_MARKER: &[u8] = b"\0CM-THUMB-FIXTURE\0";
#[cfg(test)]
fn read_fixture_prefix<R: Read>(reader: &mut R) -> Result<(), ThumbnailError> {
    let mut matched = 0;
    for _ in 0..4096 {
        let mut byte = [0];
        reader.read_exact(&mut byte).map_err(|_| ThumbnailError::Protocol("fixture marker missing".into()))?;
        if byte[0] == FIXTURE_MARKER[matched] { matched += 1; }
        else { matched = usize::from(byte[0] == FIXTURE_MARKER[0]); }
        if matched == FIXTURE_MARKER.len() { return Ok(()); }
    }
    Err(ThumbnailError::Protocol("fixture marker too long".into()))
}
fn read_response_bounded<R: Read>(r: &mut R, max_px: u32) -> Result<ThumbnailOutput, ThumbnailError> {
    let mut ver = [0u8; 1];
    r.read_exact(&mut ver).map_err(|_| ThumbnailError::Protocol("missing version".to_string()))?;
    if ver[0] != PROTOCOL_VERSION { return Err(ThumbnailError::Protocol("bad version".to_string())); }
    let mut status = [0u8; 1];
    r.read_exact(&mut status).map_err(|_| ThumbnailError::Protocol("missing status".to_string()))?;
    if status[0] == 0 {
        let width = read_u32_be(r).map_err(|_| ThumbnailError::Protocol("width read failed".to_string()))?;
        let height = read_u32_be(r).map_err(|_| ThumbnailError::Protocol("height read failed".to_string()))?;
        if width == 0 || height == 0 || width > max_px || height > max_px { return Err(ThumbnailError::Protocol("invalid dims".to_string())); }
        if u64::from(width) * u64::from(height) > MAX_PIXELS { return Err(ThumbnailError::Protocol("dims exceed guard".to_string())); }
        let len = read_u32_be(r).map_err(|_| ThumbnailError::Protocol("len read failed".to_string()))? as usize;
        if len > MAX_OUTPUT_BYTES { return Err(ThumbnailError::Protocol("output len too large".to_string())); }
        let data = read_exact_bounded(r, len, MAX_OUTPUT_BYTES)?;
        let mut extra = [0u8; 1];
        match r.read(&mut extra) {
            Ok(0) => {},
            Ok(_) => return Err(ThumbnailError::Protocol("trailing data".to_string())),
            Err(_) => return Err(ThumbnailError::Protocol("trailing check failed".to_string())),
        }
        use image::ImageDecoder;
        let decoder = image::codecs::png::PngDecoder::new(std::io::Cursor::new(&data))
            .map_err(|_| ThumbnailError::Protocol("invalid PNG".into()))?;
        if decoder.dimensions() != (width, height) { return Err(ThumbnailError::Protocol("PNG dimensions mismatch".into())); }
        Ok(ThumbnailOutput { png: data, width, height })
    } else {
        let msg_len = read_u32_be(r).map_err(|_| ThumbnailError::Protocol("msg len failed".to_string()))? as usize;
        if msg_len > 256 { return Err(ThumbnailError::Protocol("msg len too large".to_string())); }
        if !(1..=4).contains(&status[0]) { return Err(ThumbnailError::Protocol("unknown status".into())); }
        let _msg_bytes = read_exact_bounded(r, msg_len, 256)?;
        let msg = "thumbnail child rejected request".to_string();
        let err = match status[0] {
            1 => ThumbnailError::InvalidInput(msg),
            2 => ThumbnailError::TooLarge(msg),
            3 => ThumbnailError::DecodeFailed(msg),
            4 => ThumbnailError::OutputTooLarge(msg),
            _ => ThumbnailError::Protocol(msg),
        };
        let mut extra = [0u8; 1];
        match r.read(&mut extra) {
            Ok(0) => (),
            _ => return Err(ThumbnailError::Protocol("trailing check failed".into())),
        }
        Err(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageEncoder;

    fn png(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba(color));
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out).write_image(
            image.as_raw(), width, height, image::ExtendedColorType::Rgba8).unwrap();
        out
    }

    #[test]
    fn fixture_child() {
        let Some(path) = std::env::var_os("THUMBNAIL_FIXTURE_FILE") else { return };
        let mut request = Vec::new();
        std::io::stdin().read_to_end(&mut request).unwrap();
        std::fs::write(&path, std::process::id().to_string()).unwrap();
        let mut output = std::io::stdout().lock();
        output.write_all(FIXTURE_MARKER).unwrap();
        if path.to_string_lossy().ends_with("malformed") {
            output.write_all(&[0]).unwrap(); // invalid protocol version
        }
        output.flush().unwrap();
        if path.to_string_lossy().ends_with("controlled") {
            let end = Instant::now() + Duration::from_secs(5);
            let release = PathBuf::from(&path).with_extension("release");
            while !release.exists() && Instant::now() < end { std::thread::sleep(Duration::from_millis(5)); }
            if !release.exists() { return; }
            let mut input = request.as_slice();
            let mut version = [0];
            input.read_exact(&mut version).unwrap();
            assert_eq!(version[0], PROTOCOL_VERSION);
            let max_px = read_u32_be(&mut input).unwrap();
            let len = read_u32_be(&mut input).unwrap() as usize;
            assert_eq!(len, input.len());
            let result = decode_thumbnail(input, max_px).unwrap();
            output.write_all(&[PROTOCOL_VERSION, 0]).unwrap();
            write_u32_be(&mut output, result.width).unwrap();
            write_u32_be(&mut output, result.height).unwrap();
            write_u32_be(&mut output, result.png.len() as u32).unwrap();
            output.write_all(&result.png).unwrap();
            output.flush().unwrap();
            std::process::exit(0);
        }
        // No shell or descendants; finite fallback if the parent fails to kill us.
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end { std::thread::sleep(Duration::from_millis(10)); }
    }

    fn owned_fixture(label: &str, timeout: Duration) -> (Arc<ThumbnailService>, PathBuf, Arc<Mutex<Option<(u32, bool, bool)>>>) {
        let path = std::env::temp_dir().join(format!("clipmonger-thumb-{}-{}", std::process::id(), label));
        let _ = std::fs::remove_file(&path);
        let observation = Arc::new(Mutex::new(None));
        let mut service = ThumbnailService::new_with_exe(std::env::current_exe().unwrap(), timeout);
        service.fixture = Some((path.clone(), observation.clone()));
        (Arc::new(service), path, observation)
    }

    fn await_fixture(path: &std::path::Path) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if let Ok(pid) = std::fs::read_to_string(path) { if let Ok(pid) = pid.parse() { return pid; } }
            assert!(Instant::now() < deadline, "fixture failed to start");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn active_job_busy_cancel_and_reap() {
        let (service, path, observation) = owned_fixture("cancel", Duration::from_secs(3));
        let cancellation = Arc::new(AtomicBool::new(false));
        let worker = service.clone();
        let signal = cancellation.clone();
        let join = std::thread::spawn(move || worker.decode_with_cancel(b"x", 8, Some(signal)));
        let pid = await_fixture(&path);
        assert_eq!(service.decode(b"x", 8).unwrap_err(), ThumbnailError::Busy);
        let start = Instant::now();
        cancellation.store(true, Ordering::SeqCst);
        assert_eq!(join.join().unwrap().unwrap_err(), ThumbnailError::Cancelled);
        assert!(start.elapsed() < Duration::from_millis(1500), "cancel did not terminate promptly");
        assert_eq!(*observation.lock().unwrap(), Some((pid, true, true)));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn active_job_shutdown_and_reap() {
        let (service, path, observation) = owned_fixture("shutdown", Duration::from_secs(3));
        let worker = service.clone();
        let join = std::thread::spawn(move || worker.decode(b"x", 8));
        let pid = await_fixture(&path);
        let start = Instant::now();
        service.shutdown();
        assert_eq!(join.join().unwrap().unwrap_err(), ThumbnailError::Shutdown);
        assert!(start.elapsed() < Duration::from_millis(1500), "shutdown did not terminate promptly");
        assert_eq!(*observation.lock().unwrap(), Some((pid, true, true)));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn active_job_timeout_and_reap() {
        let (service, path, observation) = owned_fixture("timeout", Duration::from_millis(300));
        let worker = service.clone();
        let start = Instant::now();
        let join = std::thread::spawn(move || worker.decode(b"x", 8));
        let pid = await_fixture(&path);
        assert_eq!(join.join().unwrap().unwrap_err(), ThumbnailError::Timeout);
        assert!(start.elapsed() < Duration::from_millis(1500), "timeout did not terminate promptly");
        assert_eq!(*observation.lock().unwrap(), Some((pid, true, true)));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn malformed_owned_child_response_is_protocol_error() {
        let (service, path, observation) = owned_fixture("malformed", Duration::from_secs(3));
        let start = Instant::now();
        let result = service.decode(b"x", 8);
        assert_eq!(result.unwrap_err(), ThumbnailError::Protocol("bad version".into()));
        assert!(start.elapsed() < Duration::from_millis(1500), "malformed child did not terminate promptly");
        let pid = await_fixture(&path);
        assert_eq!(*observation.lock().unwrap(), Some((pid, true, true)));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn actual_decode_preserves_pixels_and_does_not_upscale() {
        let data = png(4, 2, [10, 20, 30, 255]);
        for (size, expected) in [(8, (4, 2)), (2, (2, 1))] {
            let out = decode_thumbnail(&data, size).unwrap();
            assert_eq!((out.width, out.height), expected);
            let decoded = image::load_from_memory(&out.png).unwrap().to_rgba8();
            assert_eq!(decoded.dimensions(), expected);
            assert_eq!(decoded.get_pixel(0, 0).0, [10, 20, 30, 255]);
        }
    }

    #[test]
    fn actual_png_header_rejects_more_than_64_megapixels() {
        // Rewrite IHDR dimensions and CRC on a valid PNG. The decoder sees the
        // real header, then rejects the dimensions before allocating image data.
        let mut data = png(1, 1, [0, 0, 0, 255]);
        data[16..20].copy_from_slice(&8001u32.to_be_bytes());
        data[20..24].copy_from_slice(&8000u32.to_be_bytes());
        let mut crc = 0xffff_ffffu32;
        for byte in &data[12..29] {
            crc ^= u32::from(*byte);
            for _ in 0..8 { crc = (crc >> 1) ^ (0xedb8_8320 & (0u32.wrapping_sub(crc & 1))); }
        }
        data[29..33].copy_from_slice(&(!crc).to_be_bytes());
        assert!(matches!(decode_thumbnail(&data, 128), Err(ThumbnailError::TooLarge(_))));
    }

    #[test]
    fn actual_encoder_stops_at_writer_cap() {
        let image = image::RgbaImage::from_fn(64, 64, |x, y| image::Rgba([x as u8, y as u8, (x ^ y) as u8, 255]));
        let mut writer = CappedWriter { bytes: Vec::new(), cap: 64, exceeded: false };
        let result = image::codecs::png::PngEncoder::new(&mut writer).write_image(
            image.as_raw(), 64, 64, image::ExtendedColorType::Rgba8);
        assert!(result.is_err());
        assert!(writer.exceeded);
        assert!(writer.bytes.len() <= 64);
    }

    #[test]
    fn response_framing_is_strict() {
        for data in [vec![1, 1, 0, 0, 0, 3, b'x'], vec![1, 9, 0, 0, 0, 0], vec![1, 1, 0, 0, 0, 0, 42]] {
            assert!(matches!(read_response_bounded(&mut &data[..], 8), Err(ThumbnailError::Protocol(_))));
        }
        let image = png(4, 2, [1, 2, 3, 255]);
        let mut response = vec![1, 0];
        response.extend_from_slice(&4u32.to_be_bytes());
        response.extend_from_slice(&2u32.to_be_bytes());
        response.extend_from_slice(&(image.len() as u32).to_be_bytes());
        response.extend_from_slice(&image);
        assert!(matches!(read_response_bounded(&mut &response[..], 2), Err(ThumbnailError::Protocol(_))));
        response[2..6].copy_from_slice(&3u32.to_be_bytes());
        assert!(matches!(read_response_bounded(&mut &response[..], 8), Err(ThumbnailError::Protocol(_))));
    }
}
