//! Shared capture polling loop, with injectable waiting and diagnostics.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clipmonger_platform::traits::ClipboardMonitor;
use clipmonger_platform::types::{CaptureEvent, Error, Result};

/// Poll a session monitor until shutdown and deliver each batch to capture.
/// Errors retry with 100 ms exponential backoff capped at two seconds. Log
/// once per failure streak and again on recovery; any successful poll resets
/// the delay. The factory must recreate and subscribe an unavailable monitor.
pub fn run<M: ClipboardMonitor>(
    monitor: M,
    timeout: Duration,
    shutdown: &AtomicBool,
    mut reconnect: impl FnMut() -> Result<M>,
    mut capture: impl FnMut(&[CaptureEvent]),
    mut sleep: impl FnMut(Duration),
    mut log: impl FnMut(&str),
) {
    let mut monitor = Some(monitor);
    let mut failing = false;
    let mut delay = Duration::from_millis(100);
    let cap = Duration::from_secs(2);
    while !shutdown.load(Ordering::Relaxed) {
        let result = if let Some(monitor) = monitor.as_mut() {
            monitor.poll(timeout)
        } else {
            match reconnect() {
                Ok(replacement) => {
                    monitor = Some(replacement);
                    // Only a successful poll ends the failure streak.
                    continue;
                }
                Err(e) => Err(e),
            }
        };
        match result {
            Ok(events) => {
                if failing {
                    log("monitor poll recovered");
                }
                failing = false;
                delay = Duration::from_millis(100);
                capture(&events);
            }
            Err(e) => {
                if matches!(e, Error::Unavailable(_)) {
                    // A dead pump cannot recover by polling it again. Drop
                    // the old monitor and retry the subscribed factory.
                    monitor = None;
                }
                if !failing {
                    log(&format!("monitor poll failed: {e}"));
                }
                failing = true;
                sleep(delay);
                delay = (delay * 2).min(cap);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clipmonger_platform::fakes::FakeMonitor;
    use clipmonger_platform::types::{CaptureOrigin, Error, FormatOffer, SubscribeKind};

    fn event(sensitive: bool) -> CaptureEvent {
        CaptureEvent {
            offers: vec![FormatOffer {
                mime: "text/plain".to_string(),
                bytes_hint: None,
            }],
            origin: CaptureOrigin::Clipboard,
            sensitive,
        }
    }

    fn transient() -> Result<Vec<CaptureEvent>> {
        Err(Error::Io("temporary clipboard read failure".to_string()))
    }

    #[test]
    fn poll_error_then_events_still_capture() {
        let shutdown = AtomicBool::new(false);
        let expected = vec![event(false), event(true)];
        let monitor = FakeMonitor::with_results(vec![transient(), Ok(expected.clone())]);
        let mut captured = Vec::new();
        let mut waits = Vec::new();
        let mut logs = Vec::new();
        run(
            monitor,
            Duration::ZERO,
            &shutdown,
            || panic!("transient errors must keep the live monitor"),
            |batch| {
                captured.extend_from_slice(batch);
                shutdown.store(true, Ordering::Relaxed);
            },
            |delay| waits.push(delay),
            |line| logs.push(line.to_string()),
        );
        assert_eq!(captured, expected);
        assert_eq!(waits, [Duration::from_millis(100)]);
        assert_eq!(logs.len(), 2);
        assert!(logs[0].contains("monitor poll failed"));
        assert!(logs[1].contains("recovered"));
    }

    #[test]
    fn repeated_errors_back_off_to_cap_and_reset_after_idle_success() {
        let shutdown = AtomicBool::new(false);
        let mut script: Vec<_> = (0..9).map(|_| transient()).collect();
        script.extend([Ok(Vec::new()), transient(), Ok(vec![event(false)])]);
        let mut waits = Vec::new();
        let mut logs = Vec::new();
        let mut captures = 0;
        run(
            FakeMonitor::with_results(script),
            Duration::ZERO,
            &shutdown,
            || panic!("transient errors must keep the live monitor"),
            |batch| {
                captures += batch.len();
                if !batch.is_empty() {
                    shutdown.store(true, Ordering::Relaxed);
                }
            },
            |delay| waits.push(delay.as_millis()),
            |line| logs.push(line.to_string()),
        );
        assert_eq!(captures, 1);
        assert_eq!(
            waits,
            [100, 200, 400, 800, 1600, 2000, 2000, 2000, 2000, 100]
        );
        assert_eq!(logs.len(), 4);
        assert!(logs[0].contains("failed"));
        assert!(logs[1].contains("recovered"));
        assert!(logs[2].contains("failed"));
        assert!(logs[3].contains("recovered"));
    }

    #[test]
    fn dead_pump_reconnects_and_failed_reconnect_keeps_backoff() {
        let shutdown = AtomicBool::new(false);
        let monitor = FakeMonitor::with_results(vec![Err(Error::Unavailable(
            "monitor pump thread exited".to_string(),
        ))]);
        let mut attempts = 0;
        let mut waits = Vec::new();
        let mut logs = Vec::new();
        let mut captured = Vec::new();
        run(
            monitor,
            Duration::ZERO,
            &shutdown,
            || {
                attempts += 1;
                if attempts <= 2 {
                    return Err(Error::Unavailable("session still unavailable".to_string()));
                }
                let mut replacement = FakeMonitor::new(vec![vec![event(false)]]);
                replacement.subscribe(&[SubscribeKind::Clipboard])?;
                assert_eq!(replacement.kinds, [SubscribeKind::Clipboard]);
                Ok(replacement)
            },
            |batch| {
                captured.extend_from_slice(batch);
                shutdown.store(true, Ordering::Relaxed);
            },
            |delay| waits.push(delay.as_millis()),
            |line| logs.push(line.to_string()),
        );
        assert_eq!(attempts, 3);
        assert_eq!(captured, [event(false)]);
        assert_eq!(waits, [100, 200, 400]);
        assert_eq!(logs.len(), 2);
        assert!(logs[0].contains("monitor pump thread exited"));
        assert!(logs[1].contains("recovered"));
    }

    #[test]
    fn shutdown_during_backoff_does_not_reconnect_or_capture() {
        let shutdown = AtomicBool::new(false);
        let monitor = FakeMonitor::with_results(vec![Err(Error::Unavailable(
            "monitor pump thread exited".to_string(),
        ))]);
        let mut waits = 0;
        run(
            monitor,
            Duration::ZERO,
            &shutdown,
            || panic!("shutdown must prevent reconnect"),
            |_| panic!("shutdown must prevent capture"),
            |_| {
                waits += 1;
                shutdown.store(true, Ordering::Relaxed);
            },
            |_| {},
        );
        assert_eq!(waits, 1);
    }
}
