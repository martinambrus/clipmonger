//! Owned thumbnail worker and bounded CPU-pixel cache.
//!
//! Shutdown joins the worker. Synchronous profile I/O, OS pipe opens, PNG
//! decoding and OS scheduling cannot be preempted by the async deadline.
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc, Arc, Condvar, Mutex,
};
use std::time::{Duration, Instant};

use crate::push::Wake;
use crate::thumbnail_transport::{self, FetchError};
use uuid::Uuid;

const QUEUE_CAP: usize = 8;
const IMAGE_CAP: usize = 64;
const BYTE_CAP: usize = 8 * 1024 * 1024;
const PNG_CAP: usize = 512 * 1024;
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

/// Full row identity, including the owner of eventual UI textures.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Key {
    pub clip: Uuid,
    pub content_hash: String,
    pub max_px: u32,
    pub generation: u64,
    pub context: u64,
}
impl Key {
    fn valid(&self) -> bool {
        (1..=256).contains(&self.max_px)
            && clipmonger_core::model::valid_clip_content_hash(&self.content_hash)
    }
}
/// An opaque request epoch issued by the cache for one visible identity.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Request {
    key: Key,
    epoch: u64,
}
impl Request {
    #[must_use]
    pub fn key(&self) -> &Key {
        &self.key
    }
}
fn new_request(key: Key) -> Option<Request> {
    // `try_update` is newer than the workspace MSRV.
    #[allow(deprecated)]
    let epoch = NEXT_REQUEST
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .ok()?;
    Some(Request { key, epoch })
}
/// Validated RGBA pixels; no texture is shared between contexts.
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}
/// Fixed error categories without peer or profile data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Retryable,
    Invalid,
    Cancelled,
    Deadline,
    Closed,
}
/// A completion is accepted by the cache only while its full key is visible.
pub struct Completion {
    pub request: Request,
    pub result: Result<Option<Pixels>, Failure>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    Queued,
    Duplicate,
    Deferred,
    Obsolete,
    Closed,
}
struct Job {
    request: Request,
    deadline: Instant,
}
struct State {
    queue: VecDeque<Job>,
    pending: HashSet<Request>,
    visible: HashSet<Key>,
    active: Option<(Request, tokio::sync::watch::Sender<bool>)>,
    stopped: bool,
}
#[cfg(test)]
#[derive(Clone, Debug)]
#[cfg_attr(windows, allow(dead_code))] // Viewport trace consumers are Unix-only tests.
pub(crate) struct WorkerTrace {
    pub(crate) event: &'static str,
    pub(crate) id: Uuid,
    pub(crate) epoch: u64,
}
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    #[cfg(test)]
    trace_enabled: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    trace: Mutex<Vec<WorkerTrace>>,
}
impl Shared {
    #[cfg(test)]
    fn trace(&self, event: &'static str, request: &Request) {
        if self.trace_enabled.load(Ordering::Relaxed) {
            let mut trace = self
                .trace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if trace.len() < 1024 {
                trace.push(WorkerTrace {
                    event,
                    id: request.key.clip,
                    epoch: request.epoch,
                });
            }
        }
    }
}

/// A single owned thread and its bounded request and completion channels.
pub struct Worker {
    shared: Arc<Shared>,
    results: mpsc::Receiver<Completion>,
    thread: Option<std::thread::JoinHandle<()>>,
}
#[cfg(test)]
struct ExitHook {
    entered: mpsc::SyncSender<()>,
    release: mpsc::Receiver<()>,
    exited: mpsc::SyncSender<()>,
}
impl Worker {
    /// Create the runtime on the owned thread before reporting success.
    ///
    /// # Errors
    /// Returns `Closed` if the thread or its runtime cannot start.
    pub fn start(profile: PathBuf, wake: Wake) -> Result<Self, Failure> {
        #[cfg(test)]
        {
            Self::start_inner(profile, wake, None)
        }
        #[cfg(not(test))]
        {
            Self::start_inner(profile, wake)
        }
    }
    fn start_inner(
        profile: PathBuf,
        wake: Wake,
        #[cfg(test)] exit_hook: Option<ExitHook>,
    ) -> Result<Self, Failure> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                queue: VecDeque::new(),
                pending: HashSet::new(),
                visible: HashSet::new(),
                active: None,
                stopped: false,
            }),
            changed: Condvar::new(),
            #[cfg(test)]
            trace_enabled: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            trace: Mutex::new(Vec::new()),
        });
        let (result_tx, results) = mpsc::sync_channel(QUEUE_CAP);
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let worker_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("thumbnail-worker".into())
            .spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    let _ = started_tx.send(false);
                    return;
                };
                if started_tx.send(true).is_err() {
                    return;
                }
                run(&worker_shared, &profile, &wake, &result_tx, &runtime);
                #[cfg(test)]
                if let Some(hook) = exit_hook {
                    let _ = hook.entered.send(());
                    let _ = hook.release.recv_timeout(Duration::from_secs(4));
                    let _ = hook.exited.send(());
                }
            })
            .map_err(|_| Failure::Closed)?;
        if started_rx.recv() != Ok(true) {
            let _ = thread.join();
            return Err(Failure::Closed);
        }
        Ok(Self {
            shared,
            results,
            thread: Some(thread),
        })
    }
    #[cfg(test)]
    #[cfg(unix)]
    pub(crate) fn diagnostic_enable(&self) {
        self.shared.trace_enabled.store(true, Ordering::Relaxed);
    }
    #[cfg(test)]
    #[cfg(unix)]
    pub(crate) fn diagnostic_snapshot(&self) -> Vec<WorkerTrace> {
        self.shared
            .trace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    /// Replace visible identities. Excess entries are ignored and may be retried later.
    pub fn set_visible(&self, keys: impl IntoIterator<Item = Key>) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.visible = keys
            .into_iter()
            .filter(Key::valid)
            .take(IMAGE_CAP)
            .collect();
        let visible = state.visible.clone();
        let removed: Vec<_> = state
            .queue
            .iter()
            .filter(|j| !visible.contains(&j.request.key))
            .map(|j| j.request.clone())
            .collect();
        state.queue.retain(|j| visible.contains(&j.request.key));
        for key in removed {
            state.pending.remove(&key);
        }
        if let Some((request, cancel)) = &state.active {
            if !visible.contains(&request.key) {
                let _ = cancel.send(true);
            }
        }
    }
    /// Admit one visible request; duplicate current-slot/row requests coalesce.
    #[must_use]
    pub fn submit(&self, request: Request) -> Admission {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.stopped
            || self
                .thread
                .as_ref()
                .is_none_or(std::thread::JoinHandle::is_finished)
        {
            return Admission::Closed;
        }
        if !state.visible.contains(&request.key) {
            return Admission::Obsolete;
        }
        if state.pending.contains(&request) {
            return Admission::Duplicate;
        }
        if state.queue.len() >= QUEUE_CAP {
            return Admission::Deferred;
        }
        state.pending.insert(request.clone());
        #[cfg(test)]
        self.shared.trace("admitted", &request);
        state.queue.push_back(Job {
            request,
            deadline: Instant::now() + Duration::from_secs(3),
        });
        self.shared.changed.notify_one();
        Admission::Queued
    }
    /// Stop and join the owned worker, reporting a thread panic.
    ///
    /// # Errors
    /// Returns `Closed` if the worker thread panicked.
    pub fn shutdown(mut self) -> Result<(), Failure> {
        self.stop_and_join()
    }
    fn stop_and_join(&mut self) -> Result<(), Failure> {
        {
            let mut state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.stopped = true;
            state.queue.clear();
            if let Some((_, cancel)) = &state.active {
                let _ = cancel.send(true);
            }
            self.shared.changed.notify_all();
        }
        match self.thread.take() {
            Some(thread) => thread.join().map_err(|_| Failure::Closed),
            None => Ok(()),
        }
    }
    /// Drain one result, freeing its pending identity even when it became stale.
    #[must_use]
    pub fn try_recv(&self) -> Option<Completion> {
        let completion = self.results.try_recv().ok()?;
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
            .remove(&completion.request);
        Some(completion)
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.stop_and_join();
    }
}
fn run(
    shared: &Shared,
    profile: &std::path::Path,
    wake: &Wake,
    results: &mpsc::SyncSender<Completion>,
    runtime: &tokio::runtime::Runtime,
) {
    loop {
        let (job, mut cancel) = {
            let mut state = shared
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            while state.queue.is_empty() && !state.stopped {
                state = shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            if state.stopped {
                return;
            }
            let job = state.queue.pop_front().unwrap();
            #[cfg(test)]
            shared.trace("active_started", &job.request);
            let (tx, rx) = tokio::sync::watch::channel(false);
            state.active = Some((job.request.clone(), tx));
            (job, rx)
        };
        let result = if Instant::now() >= job.deadline {
            Err(Failure::Deadline)
        } else {
            let future = thumbnail_transport::fetch(
                profile,
                job.request.key.clip,
                &job.request.key.content_hash,
                job.request.key.max_px,
                &mut cancel,
            );
            match runtime.block_on(async {
                tokio::time::timeout_at(tokio::time::Instant::from_std(job.deadline), future).await
            }) {
                Err(_) => Err(Failure::Deadline),
                Ok(Err(error)) => Err(map_error(error)),
                Ok(Ok(response)) => match response.thumbnail {
                    Some(image) => decode(&image, job.request.key.max_px).map(Some),
                    None => Ok(None),
                },
            }
        };
        let result = if Instant::now() >= job.deadline {
            Err(Failure::Deadline)
        } else {
            result
        };
        let mut state = shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active = None;
        if state.stopped {
            return;
        }
        if state.visible.contains(&job.request.key) {
            let request = job.request.clone();
            if results
                .try_send(Completion {
                    request: job.request,
                    result,
                })
                .is_ok()
            {
                drop(state);
                wake();
            } else {
                #[cfg(test)]
                shared.trace("result_dropped_full", &request);
                state.pending.remove(&request);
            }
        } else {
            state.pending.remove(&job.request);
        }
    }
}
fn map_error(e: FetchError) -> Failure {
    match e {
        FetchError::Cancelled => Failure::Cancelled,
        FetchError::Deadline => Failure::Deadline,
        FetchError::Protocol | FetchError::Stale => Failure::Invalid,
        _ => Failure::Retryable,
    }
}

fn decode_base64(value: &str) -> Result<Vec<u8>, Failure> {
    fn digit(b: u8) -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes = value.as_bytes();
    if bytes.is_empty() || !bytes.len().is_multiple_of(4) || bytes.len() > PNG_CAP.div_ceil(3) * 4 {
        return Err(Failure::Invalid);
    }
    let padding = bytes.iter().rev().take_while(|b| **b == b'=').count();
    if padding > 2 {
        return Err(Failure::Invalid);
    }
    let len = (bytes.len() / 4)
        .checked_mul(3)
        .and_then(|n| n.checked_sub(padding))
        .ok_or(Failure::Invalid)?;
    if len > PNG_CAP {
        return Err(Failure::Invalid);
    }
    let mut out = Vec::with_capacity(len);
    for (i, chunk) in bytes.as_chunks::<4>().0.iter().enumerate() {
        let last = i + 1 == bytes.len() / 4;
        let a = digit(chunk[0]).ok_or(Failure::Invalid)?;
        let b = digit(chunk[1]).ok_or(Failure::Invalid)?;
        let c = if last && padding == 2 {
            if chunk[2] != b'=' || b & 15 != 0 {
                return Err(Failure::Invalid);
            }
            0
        } else {
            digit(chunk[2]).ok_or(Failure::Invalid)?
        };
        let d = if last && padding > 0 {
            if chunk[3] != b'=' || (padding == 1 && c & 3 != 0) {
                return Err(Failure::Invalid);
            }
            0
        } else {
            digit(chunk[3]).ok_or(Failure::Invalid)?
        };
        out.push((a << 2) | (b >> 4));
        if !last || padding < 2 {
            out.push((b << 4) | (c >> 2));
        }
        if !last || padding == 0 {
            out.push((c << 6) | d);
        }
    }
    Ok(out)
}
fn decode(
    image: &clipmonger_ipc::proto::methods::HistoryThumbnail,
    max_px: u32,
) -> Result<Pixels, Failure> {
    use image::ImageDecoder;
    let png = decode_base64(&image.png_base64)?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(max_px);
    limits.max_image_height = Some(max_px);
    limits.max_alloc = Some(4 * 1024 * 1024);
    let decoder = image::codecs::png::PngDecoder::with_limits(std::io::Cursor::new(png), limits)
        .map_err(|_| Failure::Invalid)?;
    let (width, height) = decoder.dimensions();
    if width == 0
        || height == 0
        || width > max_px
        || height > max_px
        || width != image.width
        || height != image.height
    {
        return Err(Failure::Invalid);
    }
    let len = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or(Failure::Invalid)?;
    if len > 256 * 256 * 4 {
        return Err(Failure::Invalid);
    }
    let rgba = image::DynamicImage::from_decoder(decoder)
        .map_err(|_| Failure::Invalid)?
        .to_rgba8()
        .into_raw();
    if rgba.len() != len {
        return Err(Failure::Invalid);
    }
    Ok(Pixels {
        width,
        height,
        rgba,
    })
}

enum Entry {
    Ready(Option<Pixels>),
    Retry { until: Instant, attempts: u8 },
    Invalid,
}
struct CacheEntry {
    entry: Entry,
    used: u64,
    bytes: usize,
}
/// Bounded CPU cache. Offscreen entries remain until explicit invalidation or LRU eviction.
/// Eviction forgets error suppression and invalidates its outstanding request epoch.
#[derive(Default)]
pub struct Cache {
    entries: HashMap<Key, CacheEntry>,
    visible: HashSet<Key>,
    requests: HashMap<Key, u64>,
    bytes: usize,
    clock: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheState {
    Offscreen,
    Missing,
    Ready,
    Cooldown,
    Invalid,
}
impl Cache {
    /// Replace current identities; deleted and offscreen keys cannot accept late results.
    pub fn set_visible(&mut self, keys: impl IntoIterator<Item = Key>) {
        self.visible = keys
            .into_iter()
            .filter(Key::valid)
            .take(IMAGE_CAP)
            .collect();
        self.requests.retain(|key, _| self.visible.contains(key));
    }
    /// Determine whether a visible key may be requested again.
    pub fn state(&mut self, key: &Key, now: Instant) -> CacheState {
        if !self.visible.contains(key) {
            return CacheState::Offscreen;
        }
        self.clock = self.clock.wrapping_add(1);
        let Some(entry) = self.entries.get_mut(key) else {
            return CacheState::Missing;
        };
        entry.used = self.clock;
        match &entry.entry {
            Entry::Ready(_) => CacheState::Ready,
            Entry::Invalid => CacheState::Invalid,
            Entry::Retry { until, .. } if now < *until => CacheState::Cooldown,
            Entry::Retry { .. } => CacheState::Missing,
        }
    }
    /// Return the retry deadline only for a visible key still cooling down.
    #[must_use]
    pub fn retry_deadline(&self, key: &Key, now: Instant) -> Option<Instant> {
        if !self.visible.contains(key) {
            return None;
        }
        match &self.entries.get(key)?.entry {
            Entry::Retry { until, .. } if now < *until => Some(*until),
            _ => None,
        }
    }
    /// Issue or coalesce a request epoch for a visible, retryable key.
    /// Abandon the returned request if the worker reports Deferred, Obsolete or Closed.
    pub fn begin(&mut self, key: &Key, now: Instant) -> Option<Request> {
        if self.state(key, now) != CacheState::Missing {
            return None;
        }
        if let Some(epoch) = self.requests.get(key) {
            return Some(Request {
                key: key.clone(),
                epoch: *epoch,
            });
        }
        let request = new_request(key.clone())?;
        self.requests.insert(key.clone(), request.epoch);
        Some(request)
    }
    /// Remove a request that the worker could not admit, without affecting a newer request.
    pub fn abandon(&mut self, request: &Request) {
        if self.requests.get(&request.key) == Some(&request.epoch) {
            self.requests.remove(&request.key);
        }
    }
    /// Explicitly forget one identity and invalidate its outstanding request.
    pub fn invalidate(&mut self, key: &Key) {
        self.requests.remove(key);
        if let Some(old) = self.entries.remove(key) {
            self.bytes -= old.bytes;
        }
    }
    /// Borrow pixels for upload in the context named by this key.
    pub fn pixels(&mut self, key: &Key) -> Option<&Pixels> {
        if !self.visible.contains(key) {
            return None;
        }
        self.clock = self.clock.wrapping_add(1);
        let entry = self.entries.get_mut(key)?;
        entry.used = self.clock;
        match &entry.entry {
            Entry::Ready(Some(pixels)) => Some(pixels),
            _ => None,
        }
    }
    /// Accept only a current completion, maintaining bounded LRU and retry state.
    pub fn accept(&mut self, completion: Completion, now: Instant) -> bool {
        let Request { key, epoch } = completion.request;
        if !self.visible.contains(&key) || self.requests.get(&key) != Some(&epoch) {
            return false;
        }
        self.requests.remove(&key);
        if matches!(&completion.result, Err(Failure::Cancelled)) {
            return false;
        }
        let previous = self.entries.remove(&key);
        if let Some(old) = &previous {
            self.bytes -= old.bytes;
        }
        let attempts = previous
            .as_ref()
            .and_then(|e| match &e.entry {
                Entry::Retry { attempts, .. } => Some(*attempts),
                _ => None,
            })
            .unwrap_or(0);
        let entry = match completion.result {
            Ok(pixels) => Entry::Ready(pixels),
            Err(Failure::Invalid) => Entry::Invalid,
            Err(Failure::Cancelled) => return false,
            Err(_) => {
                let attempts = attempts.saturating_add(1);
                let secs = (2_u64 << u32::from(attempts.saturating_sub(1).min(4))).min(30);
                Entry::Retry {
                    until: now + Duration::from_secs(secs),
                    attempts,
                }
            }
        };
        let bytes = match &entry {
            Entry::Ready(Some(pixels)) => pixels.rgba.len(),
            _ => 0,
        };
        if bytes > BYTE_CAP {
            return false;
        }
        while self.entries.len() >= IMAGE_CAP || self.bytes + bytes > BYTE_CAP {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, value)| value.used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some(old) = self.entries.remove(&oldest) {
                self.bytes -= old.bytes;
            }
            self.requests.remove(&oldest);
        }
        self.clock = self.clock.wrapping_add(1);
        self.bytes += bytes;
        self.entries.insert(
            key,
            CacheEntry {
                entry,
                used: self.clock,
                bytes,
            },
        );
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clipmonger_ipc::proto::methods::HistoryThumbnail;
    fn key(id: u64) -> Key {
        Key {
            clip: Uuid::from_u128(u128::from(id)),
            content_hash: "a".repeat(64),
            max_px: 128,
            generation: 1,
            context: 1,
        }
    }
    fn png(color: [u8; 4]) -> Vec<u8> {
        use image::ImageEncoder;
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(&color, 1, 1, image::ExtendedColorType::Rgba8)
            .unwrap();
        bytes
    }
    fn base64(bytes: &[u8]) -> String {
        let chars = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for c in bytes.chunks(3) {
            let a = c[0];
            let b = *c.get(1).unwrap_or(&0);
            let d = *c.get(2).unwrap_or(&0);
            out.push(chars[(a >> 2) as usize] as char);
            out.push(chars[((a & 3) << 4 | b >> 4) as usize] as char);
            out.push(if c.len() > 1 {
                chars[((b & 15) << 2 | d >> 6) as usize] as char
            } else {
                '='
            });
            out.push(if c.len() > 2 {
                chars[(d & 63) as usize] as char
            } else {
                '='
            });
        }
        out
    }
    fn image(color: [u8; 4]) -> HistoryThumbnail {
        HistoryThumbnail {
            clip: Uuid::nil(),
            content_hash: "a".repeat(64),
            mime: "image/png".into(),
            width: 1,
            height: 1,
            png_base64: base64(&png(color)),
        }
    }
    #[test]
    fn png_decode_checks_pixels_metadata_limits_and_canonical_base64() {
        for color in [[255, 0, 0, 255], [0, 0, 255, 255]] {
            let output = decode(&image(color), 128).unwrap();
            assert_eq!(output.rgba, color);
        }
        let mut bad = image([255, 0, 0, 255]);
        bad.width = 2;
        assert!(matches!(decode(&bad, 128), Err(Failure::Invalid)));
        bad = image([255, 0, 0, 255]);
        bad.png_base64 = base64(&vec![0; PNG_CAP + 1]);
        assert!(matches!(decode(&bad, 128), Err(Failure::Invalid)));
        assert!(decode_base64("Zh==").is_err());
        assert!(decode_base64("Zm9=").is_err());
        assert!(decode_base64("Zg=A").is_err());
        bad.png_base64 = "Zm9v".into();
        assert!(matches!(decode(&bad, 128), Err(Failure::Invalid)));
    }
    fn complete(
        cache: &mut Cache,
        key: &Key,
        result: Result<Option<Pixels>, Failure>,
        now: Instant,
    ) -> bool {
        let request = cache.begin(key, now).expect("request issued");
        cache.accept(Completion { request, result }, now)
    }
    #[test]
    fn cache_retains_errors_across_scroll_and_forgets_only_on_eviction() {
        let now = Instant::now();
        let a = key(1);
        let b = key(2);
        let mut cache = Cache::default();
        cache.set_visible([a.clone(), b.clone()]);
        assert!(complete(&mut cache, &a, Err(Failure::Invalid), now));
        assert!(complete(&mut cache, &b, Err(Failure::Retryable), now));
        cache.set_visible([]);
        assert_eq!(cache.state(&a, now), CacheState::Offscreen);
        cache.set_visible([a.clone(), b.clone()]);
        assert_eq!(
            cache.state(&a, now + Duration::from_secs(60)),
            CacheState::Invalid
        );
        assert_eq!(
            cache.state(&b, now + Duration::from_secs(1)),
            CacheState::Cooldown
        );
        assert_eq!(
            cache.state(&b, now + Duration::from_secs(2)),
            CacheState::Missing
        );
        assert!(complete(
            &mut cache,
            &b,
            Err(Failure::Retryable),
            now + Duration::from_secs(2)
        ));
        assert_eq!(
            cache.state(&b, now + Duration::from_secs(5)),
            CacheState::Cooldown
        );
        cache.invalidate(&a);
        assert_eq!(cache.state(&a, now), CacheState::Missing);
    }
    #[test]
    fn eviction_invalidates_inflight_epoch_even_when_identity_reappears() {
        let now = Instant::now();
        let a = key(1);
        let mut cache = Cache::default();
        let keys: Vec<_> = (1..=34).map(key).collect();
        cache.set_visible(keys.clone());
        assert!(complete(&mut cache, &a, Err(Failure::Retryable), now));
        let old = cache.begin(&a, now + Duration::from_secs(2)).unwrap();
        for k in &keys[1..] {
            assert!(complete(
                &mut cache,
                k,
                Ok(Some(Pixels {
                    width: 256,
                    height: 256,
                    rgba: vec![1; 256 * 256 * 4]
                })),
                now
            ));
        }
        assert!(cache.bytes <= BYTE_CAP);
        assert!(!cache.entries.contains_key(&a));
        cache.set_visible(keys.clone());
        assert_eq!(
            cache.state(&a, now + Duration::from_secs(3)),
            CacheState::Missing
        );
        let fresh = cache.begin(&a, now + Duration::from_secs(3)).unwrap();
        assert!(!cache.accept(
            Completion {
                request: old,
                result: Ok(None)
            },
            now
        ));
        assert!(cache.accept(
            Completion {
                request: fresh,
                result: Ok(None)
            },
            now
        ));
        let mut other = a.clone();
        other.context += 1;
        assert!(cache.pixels(&other).is_none());
    }
    #[test]
    fn obsolete_and_abandoned_epochs_cannot_accept_completions() {
        let now = Instant::now();
        let a = key(1);
        let mut changed = a.clone();
        changed.content_hash = "b".repeat(64);
        let mut cache = Cache::default();
        cache.set_visible([a.clone()]);
        let old = cache.begin(&a, now).unwrap();
        cache.set_visible([changed]);
        assert!(!cache.accept(
            Completion {
                request: old,
                result: Ok(None)
            },
            now
        ));
        cache.set_visible([a.clone()]);
        let deferred = cache.begin(&a, now).unwrap();
        cache.abandon(&deferred);
        let next = cache.begin(&a, now).unwrap();
        assert!(!cache.accept(
            Completion {
                request: deferred,
                result: Ok(None)
            },
            now
        ));
        assert!(cache.accept(
            Completion {
                request: next,
                result: Ok(None)
            },
            now
        ));
    }
    #[cfg(unix)]
    mod wire {
        use super::*;
        use clipmonger_ipc::{
            framing::{decode_line, encode_line},
            proto::{Envelope, FrameKind, IPC_VERSION},
        };
        use std::io::{BufRead, BufReader, Read, Write};
        use std::os::unix::net::{UnixListener, UnixStream};
        struct Profile(PathBuf);
        impl Profile {
            fn new() -> Self {
                let path = std::env::temp_dir().join(format!(
                    "cm-worker-{}-{}",
                    std::process::id(),
                    Uuid::now_v7()
                ));
                std::fs::create_dir(&path).unwrap();
                std::fs::write(path.join("token"), "token").unwrap();
                Self(path)
            }
        }
        impl Drop for Profile {
            fn drop(&mut self) {
                std::fs::remove_dir_all(&self.0).unwrap();
            }
        }
        struct Peer(Option<std::thread::JoinHandle<()>>);
        impl Peer {
            fn join(mut self) {
                self.0.take().unwrap().join().unwrap();
            }
        }
        impl Drop for Peer {
            fn drop(&mut self) {
                if let Some(t) = self.0.take() {
                    let result = t.join();
                    if !std::thread::panicking() {
                        result.unwrap();
                    }
                }
            }
        }
        fn listen(path: &std::path::Path) -> UnixListener {
            let listener = UnixListener::bind(path.join("clipmonger.sock")).unwrap();
            listener.set_nonblocking(true).unwrap();
            listener
        }
        fn accept(listener: &UnixListener) -> UnixStream {
            let end = Instant::now() + Duration::from_secs(3);
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(4)))
                            .unwrap();
                        return stream;
                    }
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < end =>
                    {
                        std::thread::yield_now();
                    }
                    Err(e) => panic!("accept failed: {e}"),
                }
            }
        }
        fn frame(reader: &mut BufReader<UnixStream>) -> Envelope {
            let mut bytes = Vec::new();
            assert_ne!(reader.read_until(b'\n', &mut bytes).unwrap(), 0);
            decode_line(&bytes).unwrap()
        }
        fn reply(stream: &mut UnixStream, id: &str, value: serde_json::Value) {
            let env = Envelope {
                v: IPC_VERSION.into(),
                id: id.into(),
                kind: FrameKind::Res,
                method: None,
                params: None,
                result: Some(value),
                error: None,
                seq: None,
            };
            stream.write_all(&encode_line(&env).unwrap()).unwrap();
        }
        fn handshake(listener: &UnixListener) -> (UnixStream, BufReader<UnixStream>, Envelope) {
            let mut stream = accept(listener);
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let hello = frame(&mut reader);
            assert_eq!(hello.params.unwrap()["token"], "token");
            reply(&mut stream, "hello-0", serde_json::json!({"chosen":1}));
            let req = frame(&mut reader);
            assert_eq!(req.method.as_deref(), Some("history.thumbnail"));
            (stream, reader, req)
        }
        fn request(key: &Key) -> Request {
            new_request(key.clone()).unwrap()
        }
        fn wake_channel() -> (Wake, mpsc::Receiver<()>) {
            let (tx, rx) = mpsc::channel();
            (
                Arc::new(move || {
                    let _ = tx.send(());
                }),
                rx,
            )
        }
        struct ExitControl {
            entered: mpsc::Receiver<()>,
            release: mpsc::SyncSender<()>,
            exited: mpsc::Receiver<()>,
            shutdown: Option<std::thread::JoinHandle<()>>,
        }
        impl ExitControl {
            fn start(profile: PathBuf, wake: Wake) -> (Worker, Self) {
                let (entered_tx, entered) = mpsc::sync_channel(1);
                let (release, release_rx) = mpsc::sync_channel(1);
                let (exited_tx, exited) = mpsc::sync_channel(1);
                let worker = Worker::start_inner(
                    profile,
                    wake,
                    Some(ExitHook {
                        entered: entered_tx,
                        release: release_rx,
                        exited: exited_tx,
                    }),
                )
                .unwrap();
                (
                    worker,
                    Self {
                        entered,
                        release,
                        exited,
                        shutdown: None,
                    },
                )
            }
            fn check_join(&mut self, worker: Worker) {
                let (done_tx, done_rx) = mpsc::sync_channel(1);
                self.shutdown = Some(std::thread::spawn(move || {
                    let _ = done_tx.send(worker.shutdown());
                }));
                self.entered
                    .recv_timeout(Duration::from_secs(2))
                    .expect("worker reached exit barrier");
                assert!(
                    matches!(
                        done_rx.recv_timeout(Duration::from_millis(100)),
                        Err(mpsc::RecvTimeoutError::Timeout)
                    ),
                    "shutdown returned before worker exit"
                );
                self.release.try_send(()).unwrap();
                self.exited
                    .recv_timeout(Duration::from_secs(2))
                    .expect("worker passed exit barrier");
                done_rx
                    .recv_timeout(Duration::from_secs(2))
                    .expect("shutdown completed")
                    .expect("worker joined");
                self.shutdown
                    .take()
                    .unwrap()
                    .join()
                    .expect("shutdown thread panicked");
            }
        }
        impl Drop for ExitControl {
            fn drop(&mut self) {
                let _ = self.release.try_send(());
                if let Some(thread) = self.shutdown.take() {
                    let _ = self.exited.recv_timeout(Duration::from_secs(5));
                    let result = thread.join();
                    if !std::thread::panicking() {
                        result.expect("shutdown thread panicked");
                    }
                }
            }
        }
        #[test]
        fn real_transport_decodes_pixels_and_wakes() {
            let profile = Profile::new();
            let listener = listen(&profile.0);
            let k = key(1);
            let expected = k.clone();
            let peer = Peer(Some(std::thread::spawn(move || {
                let (mut stream, _, req) = handshake(&listener);
                assert_eq!(req.params.unwrap()["clip"], expected.clip.to_string());
                let mut img = image([255, 0, 0, 255]);
                img.clip = expected.clip;
                reply(&mut stream, "t0", serde_json::json!({"thumbnail":img}));
            })));
            let (wake, wakes) = wake_channel();
            let worker = Worker::start(profile.0.clone(), wake).unwrap();
            worker.set_visible([k.clone()]);
            let r = request(&k);
            assert_eq!(worker.submit(r.clone()), Admission::Queued);
            assert_eq!(worker.submit(r), Admission::Duplicate);
            wakes
                .recv_timeout(Duration::from_secs(3))
                .expect("completion wake");
            let completion = worker.try_recv().expect("completion");
            assert_eq!(completion.result.unwrap().unwrap().rgba, [255, 0, 0, 255]);
            worker.shutdown().unwrap();
            peer.join();
        }
        #[test]
        fn worker_result_from_before_eviction_is_rejected_after_reintroduction() {
            struct Release(Option<mpsc::SyncSender<()>>);
            impl Drop for Release {
                fn drop(&mut self) {
                    if let Some(tx) = self.0.take() {
                        let _ = tx.try_send(());
                    }
                }
            }
            let profile = Profile::new();
            let listener = listen(&profile.0);
            let (ready_tx, ready_rx) = mpsc::sync_channel(1);
            let (release_tx, release_rx) = mpsc::sync_channel(1);
            let peer = Peer(Some(std::thread::spawn(move || {
                let (mut first, _, _) = handshake(&listener);
                ready_tx.send(()).unwrap();
                release_rx
                    .recv_timeout(Duration::from_secs(3))
                    .expect("first release");
                reply(&mut first, "t0", serde_json::json!({"thumbnail":null}));
                let (mut second, _, _) = handshake(&listener);
                reply(&mut second, "t0", serde_json::json!({"thumbnail":null}));
            })));
            let mut release = Release(Some(release_tx));
            let (wake, wakes) = wake_channel();
            let worker = Worker::start(profile.0.clone(), wake).unwrap();
            let now = Instant::now();
            let a = key(1);
            let keys: Vec<_> = (1..=34).map(key).collect();
            let mut cache = Cache::default();
            cache.set_visible(keys.clone());
            worker.set_visible(keys.clone());
            assert!(complete(&mut cache, &a, Err(Failure::Retryable), now));
            let old = cache.begin(&a, now + Duration::from_secs(2)).unwrap();
            assert_eq!(worker.submit(old), Admission::Queued);
            ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            for k in &keys[1..] {
                assert!(complete(
                    &mut cache,
                    k,
                    Ok(Some(Pixels {
                        width: 256,
                        height: 256,
                        rgba: vec![1; 256 * 256 * 4]
                    })),
                    now
                ));
            }
            cache.set_visible(keys.clone());
            worker.set_visible(keys);
            let fresh = cache.begin(&a, now + Duration::from_secs(3)).unwrap();
            release.0.take().unwrap().send(()).unwrap();
            wakes
                .recv_timeout(Duration::from_secs(2))
                .expect("old completion");
            assert!(!cache.accept(worker.try_recv().unwrap(), now));
            assert_eq!(worker.submit(fresh), Admission::Queued);
            wakes
                .recv_timeout(Duration::from_secs(2))
                .expect("new completion");
            assert!(cache.accept(worker.try_recv().unwrap(), now));
            worker.shutdown().unwrap();
            peer.join();
        }
        #[test]
        fn obsolete_active_job_closes_socket_and_worker_joins() {
            let profile = Profile::new();
            let listener = listen(&profile.0);
            let k = key(1);
            let (ready_tx, ready_rx) = mpsc::sync_channel(1);
            let (eof_tx, eof_rx) = mpsc::sync_channel(1);
            let peer = Peer(Some(std::thread::spawn(move || {
                let (_, mut reader, _) = handshake(&listener);
                ready_tx.send(()).unwrap();
                let mut byte = [0];
                assert_eq!(reader.read(&mut byte).unwrap(), 0);
                eof_tx.send(()).unwrap();
            })));
            let (wake, _) = wake_channel();
            let (worker, mut exit) = ExitControl::start(profile.0.clone(), wake);
            worker.set_visible([k.clone()]);
            assert_eq!(worker.submit(request(&k)), Admission::Queued);
            ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            let mut next = k;
            next.generation += 1;
            worker.set_visible([next]);
            eof_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("obsolete socket closed");
            exit.check_join(worker);
            peer.join();
        }
        #[test]
        fn queue_is_bounded_deduplicated_and_obsolete_jobs_are_discarded() {
            let profile = Profile::new();
            let listener = listen(&profile.0);
            let first = key(1);
            let (ready_tx, ready_rx) = mpsc::sync_channel(1);
            let (eof_tx, eof_rx) = mpsc::sync_channel(1);
            let peer = Peer(Some(std::thread::spawn(move || {
                let (_, mut reader, _) = handshake(&listener);
                ready_tx.send(()).unwrap();
                let mut b = [0];
                assert_eq!(reader.read(&mut b).unwrap(), 0);
                eof_tx.send(()).unwrap();
            })));
            let (wake, _) = wake_channel();
            let worker = Worker::start(profile.0.clone(), wake).unwrap();
            let keys: Vec<_> = (1..=10).map(key).collect();
            worker.set_visible(keys.clone());
            assert_eq!(worker.submit(request(&first)), Admission::Queued);
            ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            let queued: Vec<_> = keys[1..9].iter().map(request).collect();
            for r in &queued {
                assert_eq!(worker.submit(r.clone()), Admission::Queued);
            }
            assert_eq!(worker.submit(queued[0].clone()), Admission::Duplicate);
            assert_eq!(worker.submit(request(&keys[9])), Admission::Deferred);
            worker.set_visible([keys[9].clone()]);
            assert_eq!(worker.submit(request(&keys[1])), Admission::Obsolete);
            assert_eq!(worker.submit(request(&keys[9])), Admission::Queued);
            eof_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            worker.shutdown().unwrap();
            peer.join();
        }

        #[test]
        fn expired_queued_job_is_discarded_without_connecting() {
            let profile = Profile::new();
            let listener = listen(&profile.0);
            let (ready_tx, ready_rx) = mpsc::sync_channel(1);
            let (eof_tx, eof_rx) = mpsc::sync_channel(1);
            let peer = Peer(Some(std::thread::spawn(move || {
                let (_, mut reader, _) = handshake(&listener);
                ready_tx.send(()).unwrap();
                let mut byte = [0];
                assert_eq!(reader.read(&mut byte).unwrap(), 0);
                eof_tx.send(()).unwrap();
            })));
            let (wake, wakes) = wake_channel();
            let worker = Worker::start(profile.0.clone(), wake).unwrap();
            let first = key(1);
            let second = key(2);
            worker.set_visible([first.clone(), second.clone()]);
            assert_eq!(worker.submit(request(&first)), Admission::Queued);
            ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(worker.submit(request(&second)), Admission::Queued);
            {
                let mut state = worker.shared.state.lock().unwrap();
                state.queue.front_mut().unwrap().deadline = Instant::now()
                    .checked_sub(Duration::from_millis(1))
                    .unwrap();
            }
            worker.set_visible([second]);
            eof_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            wakes
                .recv_timeout(Duration::from_secs(2))
                .expect("expired completion wake");
            assert!(matches!(
                worker.try_recv().unwrap().result,
                Err(Failure::Deadline)
            ));
            worker.shutdown().unwrap();
            peer.join();
        }
        #[test]
        fn full_result_channel_drops_completion_and_shutdown_joins() {
            let profile = Profile::new();
            let listener = listen(&profile.0);
            let (done_tx, done_rx) = mpsc::sync_channel(1);
            let peer = Peer(Some(std::thread::spawn(move || {
                for _ in 0..9 {
                    let (mut stream, _, _) = handshake(&listener);
                    reply(&mut stream, "t0", serde_json::json!({"thumbnail":null}));
                }
                done_tx.send(()).unwrap();
            })));
            let (wake, wakes) = wake_channel();
            let (worker, mut exit) = ExitControl::start(profile.0.clone(), wake);
            let keys: Vec<_> = (1..=9).map(key).collect();
            worker.set_visible(keys.clone());
            for k in &keys[..8] {
                assert_eq!(worker.submit(request(k)), Admission::Queued);
                wakes.recv_timeout(Duration::from_secs(2)).unwrap();
            }
            let last = request(&keys[8]);
            assert_eq!(worker.submit(last.clone()), Admission::Queued);
            done_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                let finished = {
                    let state = worker.shared.state.lock().unwrap();
                    !state.pending.contains(&last) && state.active.is_none()
                };
                if finished {
                    break;
                }
                assert!(Instant::now() < deadline, "full result did not finish");
                std::thread::yield_now();
            }
            exit.check_join(worker);
            peer.join();
        }
    }
}
