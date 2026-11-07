//! Clipboard monitor: a worker polls `NSPasteboard.changeCount` (`CopyQ`
//! pattern, seg-08 §1) and queues one event per change. Only type names are
//! read here; data stays with the lazy `MacTransfer::read`, so pasteboard
//! privacy (macOS 15.4+) never prompts from the monitor.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use clipmonger_platform::traits::ClipboardMonitor;
use clipmonger_platform::types::{
    Affinity, CaptureEvent, CaptureOrigin, Error, FormatOffer, Result, SubscribeKind,
};

use crate::caps::PasteboardAccess;
use crate::foreground::RunningApp;
use crate::mime::{self, TypeNames};
use crate::os;
use crate::transfer::sequence_of;

/// Default poll interval (`CopyQ`: 500 ms).
pub const DEFAULT_POLL_MS: u64 = 500;
/// How often the privacy state is re-read.
const ACCESS_REFRESH: Duration = Duration::from_secs(5);

/// Monitor options.
#[derive(Debug, Clone)]
pub struct MonitorOpts {
    /// `changeCount` poll interval (ms, 0 = default).
    pub poll_ms: u64,
    /// Frontmost apps never recorded (bundle id or name, case-insensitive;
    /// union of `monitor.ignores` + `privacy.denylist`).
    pub denylist: Vec<String>,
    /// Largest single transfer accepted (bytes; enforced by the reads).
    pub max_bytes: u64,
}

impl Default for MonitorOpts {
    fn default() -> Self {
        Self {
            poll_ms: DEFAULT_POLL_MS,
            denylist: Vec::new(),
            max_bytes: 64 << 20,
        }
    }
}

/// `changeCount`-polling monitor.
pub struct MacMonitor {
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

#[derive(Debug, Default)]
struct Shared {
    subscribed: bool,
    queue: Vec<CaptureEvent>,
}

/// Decide the event for one observed change (pure).
fn observe(
    count: isize,
    access: PasteboardAccess,
    types: &[String],
    front: Option<&RunningApp>,
    opts: &MonitorOpts,
    names: &TypeNames,
) -> Option<CaptureEvent> {
    // Denied: there is nothing we may read, so report nothing.
    if access == PasteboardAccess::Denied {
        return None;
    }
    let offers: Vec<FormatOffer> = mime::offers_for(types, names)
        .into_iter()
        .map(|mime| FormatOffer {
            mime,
            bytes_hint: None,
        })
        .collect();
    let sensitive =
        mime::is_sensitive(types) || front.is_some_and(|app| app.denylisted(&opts.denylist));
    if offers.is_empty() && !sensitive {
        // Cleared pasteboard.
        return None;
    }
    Some(CaptureEvent {
        native_sequence: Some(sequence_of(count)),
        offers,
        origin: CaptureOrigin::Clipboard,
        sensitive,
    })
}

impl MacMonitor {
    /// Start the poll worker. Changes before the start are not reported.
    ///
    /// # Errors
    ///
    /// [`Error::Unavailable`] off macOS or without a user session.
    pub fn new(opts: MonitorOpts) -> Result<Self> {
        let names = os::type_names()?;
        let start = os::change_count()?;
        let shared = Arc::new(Mutex::new(Shared::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let worker = {
            let shared = Arc::clone(&shared);
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("clipmonger-pasteboard".to_string())
                .spawn(move || run_worker(&opts, &names, start, &shared, &stop))
                .map_err(|e| Error::Io(format!("monitor thread failed: {e}")))?
        };
        Ok(Self {
            shared,
            stop,
            worker: Some(worker),
        })
    }
}

fn run_worker(
    opts: &MonitorOpts,
    names: &TypeNames,
    start: isize,
    shared: &Mutex<Shared>,
    stop: &AtomicBool,
) {
    let poll = Duration::from_millis(if opts.poll_ms == 0 {
        DEFAULT_POLL_MS
    } else {
        opts.poll_ms
    });
    let mut last = start;
    let mut access = os::access();
    let mut access_at = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        sleep_unless_stopped(poll, stop);
        let Ok(count) = os::change_count() else {
            // Pasteboard gone (session ending): stop; poll reports it.
            return;
        };
        if count == last {
            continue;
        }
        last = count;
        if access_at.elapsed() >= ACCESS_REFRESH {
            access = os::access();
            access_at = Instant::now();
        }
        let types = os::types().unwrap_or_default();
        let front = os::frontmost();
        let Some(event) = observe(count, access, &types, front.as_ref(), opts, names) else {
            continue;
        };
        let mut guard = match shared.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if guard.subscribed {
            guard.queue.push(event);
        }
    }
}

fn sleep_unless_stopped(total: Duration, stop: &AtomicBool) {
    let deadline = Instant::now() + total;
    while !stop.load(Ordering::Relaxed) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        std::thread::sleep(left.min(Duration::from_millis(50)));
    }
}

impl Drop for MacMonitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl ClipboardMonitor for MacMonitor {
    const AFFINITY: Affinity = Affinity::Worker;

    fn subscribe(&mut self, kinds: &[SubscribeKind]) -> Result<()> {
        if kinds.contains(&SubscribeKind::Primary) {
            return Err(Error::Unsupported(
                "macOS has no primary selection".to_string(),
            ));
        }
        let mut shared = self
            .shared
            .lock()
            .map_err(|e| Error::Io(format!("monitor state lock failed: {e}")))?;
        shared.subscribed = kinds.contains(&SubscribeKind::Clipboard);
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
                if !shared.queue.is_empty() {
                    return Ok(std::mem::take(&mut shared.queue));
                }
                let dead = self.worker.as_ref().is_none_or(JoinHandle::is_finished);
                if poisoned || dead {
                    return Err(Error::Unavailable(
                        "macOS monitor worker exited".to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mime::test_names;

    fn types(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn change_produces_lazy_offers_with_sequence() {
        let names = test_names();
        let event = observe(
            42,
            PasteboardAccess::Unrestricted,
            &types(&["public.utf8-plain-text", "public.html"]),
            None,
            &MonitorOpts::default(),
            &names,
        )
        .unwrap();
        assert_eq!(event.native_sequence, Some(42));
        assert_eq!(event.origin, CaptureOrigin::Clipboard);
        assert!(!event.sensitive);
        let mimes: Vec<&str> = event.offers.iter().map(|o| o.mime.as_str()).collect();
        assert_eq!(mimes, [mime::TEXT, mime::HTML]);
    }

    #[test]
    fn markers_and_denylist_are_sensitive() {
        let names = test_names();
        let opts = MonitorOpts {
            denylist: vec!["1password".into()],
            ..MonitorOpts::default()
        };
        for marker in [mime::CONCEALED, mime::TRANSIENT, mime::AUTO_GENERATED] {
            let event = observe(
                1,
                PasteboardAccess::Ask,
                &types(&["public.utf8-plain-text", marker]),
                None,
                &opts,
                &names,
            )
            .unwrap();
            assert!(event.sensitive, "{marker}");
        }
        let app = RunningApp {
            pid: 9,
            bundle_id: Some("com.1password.1password".into()),
            name: Some("1Password".into()),
        };
        let event = observe(
            2,
            PasteboardAccess::Unrestricted,
            &types(&["public.utf8-plain-text"]),
            Some(&app),
            &opts,
            &names,
        )
        .unwrap();
        assert!(event.sensitive);
    }

    #[test]
    fn denied_or_empty_reports_nothing() {
        let names = test_names();
        let opts = MonitorOpts::default();
        let text = types(&["public.utf8-plain-text"]);
        assert!(observe(1, PasteboardAccess::Denied, &text, None, &opts, &names).is_none());
        assert!(observe(1, PasteboardAccess::Unrestricted, &[], None, &opts, &names).is_none());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn unavailable_off_macos() {
        assert!(matches!(
            MacMonitor::new(MonitorOpts::default()),
            Err(Error::Unavailable(_))
        ));
    }
}
