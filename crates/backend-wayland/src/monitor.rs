//! Wayland clipboard monitor via `wl-clipboard-rs` watchers.
//!
//! A worker thread owns each `watch::Watcher` (blocking `next_event`,
//! stopped via `CancelHandle`); `poll` only drains the shared queue, like
//! the `backend-x11` monitor. `Changed` becomes a [`CaptureEvent`];
//! `Cleared` emits nothing (the trait has no empty event). Primary
//! selection is captured only when both `primary` and `capture_primary`
//! are set, and primary bursts coalesce through [`PrimaryDebouncer`]
//! (W-F6: 500 ms quiet, min length 3).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clipmonger_platform::traits::ClipboardMonitor;
use clipmonger_platform::types::{
    Affinity, CaptureEvent, CaptureOrigin, Error, FormatOffer, Result, SubscribeKind,
};

use crate::primary::PrimaryDebouncer;

/// Password-hint MIME marking a capture sensitive (never stored).
const PASSWORD_HINT: &str = "x-kde-passwordManagerHint";
/// Preferred MIME probed to measure a primary offer's length.
const TEXT_MIME: &str = "text/plain";

/// Monitor subscription options (frozen API: exactly these fields).
#[derive(Debug, Clone, Copy)]
pub struct WlMonitorOpts {
    /// Subscribe to the primary selection (paired with `capture_primary`).
    pub primary: bool,
    /// Quiet period before one coalesced primary event (ms).
    pub debounce_ms: u64,
    /// Minimum primary byte length accepted.
    pub min_len: u64,
    /// Emit primary-selection events (default off).
    pub capture_primary: bool,
}

/// Pending primary offer coalesced poll-side until the quiet period ends.
#[derive(Debug, Clone)]
struct PendingPrimary {
    mimes: Vec<String>,
    sensitive: bool,
}

#[derive(Default)]
struct MonitorShared {
    kinds: Vec<SubscribeKind>,
    queue: Vec<CaptureEvent>,
    pending_primary: Option<PendingPrimary>,
    debouncer: PrimaryDebouncer,
    min_len: u64,
    cancels: Vec<wl_clipboard_rs::watch::CancelHandle>,
}

/// Wayland clipboard monitor.
pub struct WlMonitor {
    shared: Arc<Mutex<MonitorShared>>,
    stop: Arc<AtomicBool>,
    workers: Vec<JoinHandle<()>>,
}

impl WlMonitor {
    /// Connect to the compositor (`$WAYLAND_DISPLAY`) and start the watcher
    /// worker(s). The regular watcher must come up within 5 s or this
    /// returns [`Error::Unavailable`]; a failed primary watcher only
    /// disables primary events.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unavailable`] without a Wayland session or a
    /// data-control protocol.
    pub fn connect(opts: WlMonitorOpts) -> Result<Self> {
        let WlMonitorOpts {
            primary,
            debounce_ms,
            min_len,
            capture_primary,
        } = opts;
        let shared = Arc::new(Mutex::new(MonitorShared {
            debouncer: PrimaryDebouncer::new(debounce_ms, min_len),
            min_len,
            ..MonitorShared::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let ready = Arc::new(Mutex::new(None::<Result<()>>));

        let worker_shared = Arc::clone(&shared);
        let worker_stop = Arc::clone(&stop);
        let worker_ready = Arc::clone(&ready);
        let regular = std::thread::spawn(move || {
            run_regular_watcher(&worker_shared, &worker_stop, &worker_ready);
        });

        // Bounded readiness wait: the worker reports its `Watcher::new`
        // outcome; 5 s without a compositor answer is Unavailable.
        let deadline = Instant::now() + Duration::from_secs(5);
        let outcome = loop {
            let done = ready
                .lock()
                .is_ok_and(|slot| slot.as_ref().is_some_and(Result::is_ok));
            if done {
                break true;
            }
            let failed = ready.lock().is_ok_and(|slot| slot.is_some());
            if failed || Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        if !outcome {
            stop.store(true, Ordering::Relaxed);
            cancel_all(&shared);
            let _ = regular.join();
            return Err(Error::Unavailable(
                "wayland monitor unavailable: no data-control session".to_string(),
            ));
        }

        let mut monitor = Self {
            shared,
            stop,
            workers: vec![regular],
        };

        if primary && capture_primary {
            let worker_shared = Arc::clone(&monitor.shared);
            let worker_stop = Arc::clone(&monitor.stop);
            monitor.workers.push(std::thread::spawn(move || {
                run_primary_watcher(&worker_shared, &worker_stop);
            }));
        }
        Ok(monitor)
    }
}

impl Drop for WlMonitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        cancel_all(&self.shared);
        while let Some(worker) = self.workers.pop() {
            let _ = worker.join();
        }
    }
}

/// Unblock every watcher loop via its cancel handle (best-effort).
fn cancel_all(shared: &Arc<Mutex<MonitorShared>>) {
    let guard = shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for cancel in &guard.cancels {
        cancel.cancel();
    }
}

impl ClipboardMonitor for WlMonitor {
    const AFFINITY: Affinity = Affinity::Worker;

    fn subscribe(&mut self, kinds: &[SubscribeKind]) -> Result<()> {
        let mut shared = self
            .shared
            .lock()
            .map_err(|e| Error::Io(format!("monitor state lock failed: {e}")))?;
        shared.kinds = kinds.to_vec();
        Ok(())
    }

    fn poll(&mut self, timeout: Duration) -> Result<Vec<CaptureEvent>> {
        let deadline = Instant::now() + timeout;
        loop {
            {
                let (mut shared, poisoned) = match self.shared.lock() {
                    Ok(shared) => (shared, false),
                    Err(error) => (error.into_inner(), true),
                };
                let mut out = std::mem::take(&mut shared.queue);
                let fire = !poisoned
                    && shared.pending_primary.is_some()
                    && shared.debouncer.poll(now_ms()).is_some();
                if fire {
                    if let Some(pending) = shared.pending_primary.take() {
                        out.push(CaptureEvent {
                            offers: pending
                                .mimes
                                .iter()
                                .map(|mime| FormatOffer {
                                    mime: mime.clone(),
                                    bytes_hint: None,
                                })
                                .collect(),
                            origin: CaptureOrigin::Primary,
                            sensitive: pending.sensitive,
                        });
                    }
                }
                if !out.is_empty() {
                    return Ok(out);
                }
                let regular_dead = self.workers.first().is_none_or(JoinHandle::is_finished);
                if poisoned || regular_dead {
                    return Err(Error::Unavailable(
                        "wayland monitor worker exited".to_string(),
                    ));
                }
            }
            if Instant::now() >= deadline {
                return Ok(Vec::new());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn is_sensitive(mimes: &[String]) -> bool {
    mimes.iter().any(|mime| mime == PASSWORD_HINT)
}

fn wants(shared: &Arc<Mutex<MonitorShared>>, kind: SubscribeKind) -> bool {
    // No subscription yet means "everything" (matches the harness, which
    // polls without subscribing); an explicit list gates the kinds.
    shared.lock().map_or(true, |guard| {
        guard.kinds.is_empty() || guard.kinds.contains(&kind)
    })
}

/// Regular-clipboard watcher loop: `Changed` pushes an event, `Cleared`
/// pushes nothing.
fn run_regular_watcher(
    shared: &Arc<Mutex<MonitorShared>>,
    stop: &Arc<AtomicBool>,
    ready: &Arc<Mutex<Option<Result<()>>>>,
) {
    let mut watcher = match wl_clipboard_rs::watch::Watcher::new(
        wl_clipboard_rs::watch::ClipboardType::Regular,
        wl_clipboard_rs::paste::Seat::Unspecified,
    ) {
        Ok(watcher) => {
            if let Ok(mut guard) = shared.lock() {
                guard.cancels.push(watcher.cancel_handle());
            }
            if let Ok(mut slot) = ready.lock() {
                *slot = Some(Ok(()));
            }
            watcher
        }
        Err(e) => {
            if let Ok(mut slot) = ready.lock() {
                *slot = Some(Err(Error::Unavailable(format!("watcher: {e}"))));
            }
            return;
        }
    };
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let Ok(event) = watcher.next_event() else {
            break;
        };
        let Some(event) = event else { break };
        match event {
            wl_clipboard_rs::watch::ClipboardEvent::Changed {
                clipboard,
                mime_types,
                ..
            } => {
                if clipboard != wl_clipboard_rs::paste::ClipboardType::Regular {
                    continue;
                }
                if !wants(shared, SubscribeKind::Clipboard) {
                    continue;
                }
                let sensitive = is_sensitive(&mime_types);
                let event = CaptureEvent {
                    offers: mime_types
                        .iter()
                        .map(|mime| FormatOffer {
                            mime: mime.clone(),
                            bytes_hint: None,
                        })
                        .collect(),
                    origin: CaptureOrigin::Clipboard,
                    sensitive,
                };
                if let Ok(mut guard) = shared.lock() {
                    guard.queue.push(event);
                }
            }
            wl_clipboard_rs::watch::ClipboardEvent::Cleared { .. } => {}
        }
    }
}

/// Primary-selection watcher loop: measures each offer, drops short ones,
/// and coalesces bursts poll-side through the shared debouncer. A missing
/// primary protocol only disables primary events.
fn run_primary_watcher(shared: &Arc<Mutex<MonitorShared>>, stop: &Arc<AtomicBool>) {
    let mut watcher = match wl_clipboard_rs::watch::Watcher::new(
        wl_clipboard_rs::watch::ClipboardType::Primary,
        wl_clipboard_rs::paste::Seat::Unspecified,
    ) {
        Ok(watcher) => {
            if let Ok(mut guard) = shared.lock() {
                guard.cancels.push(watcher.cancel_handle());
            }
            watcher
        }
        Err(_) => return,
    };
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let Ok(event) = watcher.next_event() else {
            break;
        };
        let Some(event) = event else { break };
        match event {
            wl_clipboard_rs::watch::ClipboardEvent::Changed {
                clipboard,
                mime_types,
                mut offer,
            } => {
                if clipboard != wl_clipboard_rs::paste::ClipboardType::Primary {
                    continue;
                }
                if !wants(shared, SubscribeKind::Primary) {
                    continue;
                }
                let sensitive = is_sensitive(&mime_types);
                let probe_mime = mime_types
                    .iter()
                    .find(|mime| mime.as_str() == TEXT_MIME)
                    .or_else(|| mime_types.iter().find(|mime| mime.contains("text/plain")))
                    .or_else(|| mime_types.first());
                let len = probe_mime
                    .and_then(|mime| offer.receive(mime).ok())
                    .and_then(|reader| read_limited(reader, 64 << 10))
                    .map_or(0, |bytes| bytes.len() as u64);
                if let Ok(mut guard) = shared.lock() {
                    if len < guard.min_len {
                        continue;
                    }
                    if guard.debouncer.offer(len, now_ms()) {
                        guard.queue.push(CaptureEvent {
                            offers: mime_types
                                .iter()
                                .map(|mime| FormatOffer {
                                    mime: mime.clone(),
                                    bytes_hint: None,
                                })
                                .collect(),
                            origin: CaptureOrigin::Primary,
                            sensitive,
                        });
                    } else {
                        guard.pending_primary = Some(PendingPrimary {
                            mimes: mime_types,
                            sensitive,
                        });
                    }
                }
            }
            wl_clipboard_rs::watch::ClipboardEvent::Cleared { .. } => {}
        }
    }
}

/// Read a pipe fully with a byte cap (length probing only, never stored).
fn read_limited(mut reader: impl std::io::Read, cap: usize) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return Some(out),
            Ok(n) => {
                out.extend_from_slice(&buf[..n]);
                if out.len() > cap {
                    return Some(out);
                }
            }
            Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use std::sync::mpsc;

    fn finished(panic: bool) -> JoinHandle<()> {
        finish_worker(move || {
            assert!(!panic, "worker panic fixture");
        })
    }

    fn finish_worker(work: impl FnOnce() + Send + 'static) -> JoinHandle<()> {
        let handle = std::thread::spawn(work);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !handle.is_finished() {
            assert!(Instant::now() < deadline, "finite worker gate timed out");
            std::thread::yield_now();
        }
        handle
    }

    #[test]
    fn exited_or_panicked_worker_delivers_queue_then_unavailable() {
        for panic in [false, true] {
            let shared = Arc::new(Mutex::new(MonitorShared {
                queue: vec![CaptureEvent {
                    offers: Vec::new(),
                    origin: CaptureOrigin::Clipboard,
                    sensitive: true,
                }],
                ..MonitorShared::default()
            }));
            let stop = Arc::new(AtomicBool::new(false));
            let handle = finished(panic);
            let mut monitor = WlMonitor {
                shared,
                stop,
                workers: vec![handle],
            };
            let events = monitor.poll(Duration::ZERO).unwrap();
            assert_eq!(events.len(), 1);
            assert!(events[0].sensitive);
            assert!(matches!(
                monitor.poll(Duration::ZERO),
                Err(Error::Unavailable(_))
            ));
        }
    }

    #[test]
    fn panicked_producer_preserves_poisoned_queue_before_terminal_error() {
        let shared = Arc::new(Mutex::new(MonitorShared::default()));
        let producer = Arc::clone(&shared);
        let handle = finish_worker(move || {
            let mut queue = producer.lock().unwrap();
            queue.queue.push(CaptureEvent {
                offers: Vec::new(),
                origin: CaptureOrigin::Clipboard,
                sensitive: true,
            });
            panic!("producer failed while owning the queue");
        });
        let mut monitor = WlMonitor {
            shared,
            stop: Arc::new(AtomicBool::new(false)),
            workers: vec![handle],
        };
        assert_eq!(monitor.poll(Duration::ZERO).unwrap().len(), 1);
        assert!(matches!(
            monitor.poll(Duration::ZERO),
            Err(Error::Unavailable(_))
        ));
    }

    #[test]
    fn alive_idle_is_empty_and_stopped_worker_is_unavailable() {
        let shared = Arc::new(Mutex::new(MonitorShared::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (release_tx, release_rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let _ = release_rx.recv_timeout(Duration::from_secs(2));
        });
        let mut monitor = WlMonitor {
            shared,
            stop: Arc::clone(&stop),
            workers: vec![handle],
        };
        assert_eq!(monitor.poll(Duration::ZERO).unwrap(), []);
        stop.store(true, Ordering::Relaxed);
        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !monitor.workers[0].is_finished() {
            assert!(Instant::now() < deadline, "finite shutdown gate timed out");
            std::thread::yield_now();
        }
        assert!(matches!(
            monitor.poll(Duration::ZERO),
            Err(Error::Unavailable(_))
        ));
    }
    #[test]
    fn primary_only_exit_does_not_invalidate_regular_watcher() {
        let (release_tx, release_rx) = mpsc::channel();
        let regular = std::thread::spawn(move || {
            let _ = release_rx.recv_timeout(Duration::from_secs(2));
        });
        let mut monitor = WlMonitor {
            shared: Arc::new(Mutex::new(MonitorShared::default())),
            stop: Arc::new(AtomicBool::new(false)),
            workers: vec![regular, finished(false)],
        };
        assert_eq!(monitor.poll(Duration::ZERO).unwrap(), []);
        release_tx.send(()).unwrap();
    }
}
