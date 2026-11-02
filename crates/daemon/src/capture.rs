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
    reconnect: impl FnMut() -> Result<M>,
    capture: impl FnMut(&[CaptureEvent]),
    sleep: impl FnMut(Duration),
    log: impl FnMut(&str),
) {
    run_opt(
        Some(monitor),
        timeout,
        shutdown,
        reconnect,
        capture,
        sleep,
        log,
        || {},
        || {},
    );
}

/// Poll a backend and publish availability through the status reader.
#[allow(clippy::too_many_arguments)]
pub fn run_backend<M: ClipboardMonitor>(
    monitor: Option<M>,
    timeout: Duration,
    shutdown: &AtomicBool,
    state: &crate::DaemonState,
    backend: &str,
    reconnect: impl FnMut() -> Result<M>,
    capture: impl FnMut(&[CaptureEvent]),
    log: impl FnMut(&str),
) {
    run_opt(
        monitor,
        timeout,
        shutdown,
        reconnect,
        capture,
        |delay| sleep_interruptible(shutdown, delay, std::thread::sleep),
        log,
        || state.set_monitor_available(backend, false),
        || state.set_monitor_available(backend, true),
    );
}

/// Construct and subscribe as one transaction; failed subscriptions never
/// enter the poll owner.
///
/// # Errors
/// Returns the factory or subscription error without admitting the monitor.
pub fn subscribed<M: ClipboardMonitor>(
    factory: impl FnOnce() -> Result<M>,
    kinds: &[clipmonger_platform::types::SubscribeKind],
) -> Result<M> {
    let mut monitor = factory()?;
    monitor.subscribe(kinds)?;
    Ok(monitor)
}

/// Keep backend backoff responsive to daemon shutdown.
pub fn sleep_interruptible(
    shutdown: &AtomicBool,
    mut delay: Duration,
    mut sleep: impl FnMut(Duration),
) {
    while !delay.is_zero() && !shutdown.load(Ordering::Relaxed) {
        let slice = delay.min(Duration::from_millis(20));
        sleep(slice);
        delay = delay.saturating_sub(slice);
    }
}

/// Shared recovery owner may retain `None` and retry the factory with bounded
/// backoff/stop. Updates actual reported capabilities on initial failure,
/// later unavailability and recovery while preserving unrelated caps.
#[allow(clippy::too_many_arguments)]
pub fn run_opt<M: ClipboardMonitor>(
    mut monitor: Option<M>,
    timeout: Duration,
    shutdown: &AtomicBool,
    mut reconnect: impl FnMut() -> Result<M>,
    mut capture: impl FnMut(&[CaptureEvent]),
    mut sleep: impl FnMut(Duration),
    mut log: impl FnMut(&str),
    mut on_degraded: impl FnMut(),
    mut on_recovered: impl FnMut(),
) {
    let mut failing = false;
    let mut degraded = monitor.is_none();
    if degraded {
        on_degraded();
    }
    let mut delay = Duration::from_millis(100);
    let cap = Duration::from_secs(2);
    while !shutdown.load(Ordering::Relaxed) {
        let result = if let Some(monitor) = monitor.as_mut() {
            monitor.poll(timeout)
        } else {
            match reconnect() {
                Ok(replacement) => {
                    monitor = Some(replacement);
                    continue;
                }
                Err(e) => Err(e),
            }
        };
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        match result {
            Ok(events) => {
                if failing {
                    log("monitor poll recovered");
                }
                if degraded {
                    on_recovered();
                    degraded = false;
                }
                failing = false;
                delay = Duration::from_millis(100);
                capture(&events);
            }
            Err(e) => {
                let unavailable = matches!(e, Error::Unavailable(_));
                if unavailable {
                    if !degraded {
                        on_degraded();
                        degraded = true;
                    }
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

    // Initial factory failure and recovery availability.
    #[test]
    fn initial_none_retries_factory_with_backoff_and_updates_caps() {
        let shutdown = AtomicBool::new(false);
        let mut attempts = 0;
        let mut degraded_calls = 0;
        let mut recovered_calls = 0;
        let mut captured = Vec::new();
        let mut waits = Vec::new();
        run_opt(
            None,
            Duration::ZERO,
            &shutdown,
            || {
                attempts += 1;
                if attempts < 3 {
                    return Err(Error::Unavailable("session still unavailable".to_string()));
                }
                let mut replacement = FakeMonitor::new(vec![vec![event(false)]]);
                replacement.subscribe(&[SubscribeKind::Clipboard]).unwrap();
                Ok(replacement)
            },
            |batch| {
                captured.extend_from_slice(batch);
                shutdown.store(true, Ordering::Relaxed);
            },
            |delay| waits.push(delay.as_millis()),
            |_| {},
            || degraded_calls += 1,
            || recovered_calls += 1,
        );
        assert_eq!(attempts, 3);
        assert_eq!(
            degraded_calls, 1,
            "initial None must trigger degraded caps once"
        );
        assert_eq!(
            recovered_calls, 1,
            "first successful poll must trigger recovered caps"
        );
        assert_eq!(captured.len(), 1);
        assert_eq!(waits, [100, 200]);
    }

    #[test]
    fn degraded_not_repeated_on_repeated_unavailable_and_recovered_once() {
        let shutdown = AtomicBool::new(false);
        let monitor = FakeMonitor::with_results(vec![
            Err(Error::Unavailable("pump died".to_string())),
            Err(Error::Unavailable("still down".to_string())),
        ]);
        let mut degraded = 0;
        let mut recovered = 0;
        let mut attempts = 0;
        let mut logs = Vec::new();
        run_opt(
            Some(monitor),
            Duration::ZERO,
            &shutdown,
            || {
                attempts += 1;
                if attempts == 1 {
                    return Err(Error::Unavailable("factory down".to_string()));
                }
                let mut m = FakeMonitor::new(vec![vec![event(false)]]);
                m.subscribe(&[SubscribeKind::Clipboard]).unwrap();
                Ok(m)
            },
            |batch| {
                if !batch.is_empty() {
                    shutdown.store(true, Ordering::Relaxed);
                }
            },
            |_| {},
            |line| logs.push(line.to_string()),
            || degraded += 1,
            || recovered += 1,
        );
        assert_eq!(degraded, 1, "degraded should be called once per streak");
        assert_eq!(recovered, 1);
        assert!(logs.iter().any(|line| line.contains("monitor poll failed")));
        assert!(logs.iter().any(|line| line.contains("recovered")));
    }

    #[test]
    fn alive_idle_returns_empty_not_unavailable() {
        let shutdown = AtomicBool::new(false);
        let monitor =
            FakeMonitor::with_results(vec![Ok(Vec::new()), Ok(Vec::new()), Ok(vec![event(false)])]);
        let mut captured = 0;
        run_opt(
            Some(monitor),
            Duration::ZERO,
            &shutdown,
            || panic!("idle must not trigger reconnect"),
            |batch| {
                captured += batch.len();
                if captured > 0 {
                    shutdown.store(true, Ordering::Relaxed);
                }
            },
            |_| {},
            |_| {},
            || {},
            || {},
        );
        assert_eq!(captured, 1);
    }

    #[test]
    fn queued_events_delivered_before_deferred_unavailable() {
        // Our FakeMonitor cannot test WinMonitor pending_error directly,
        // but we verify shared capture contract: if poll returns Ok with events
        // then next poll returns Unavailable, both are handled without loss.
        // The real Windows pending_error and X11 is_finished logic is tested
        // via backend unit tests; this verifies shared layer preserves queue.
        let shutdown = AtomicBool::new(false);
        let monitor = FakeMonitor::with_results(vec![
            Ok(vec![event(false)]),
            Err(Error::Unavailable("worker exited".to_string())),
        ]);
        let mut captured = Vec::new();
        let mut factory_calls = 0;
        run_opt(
            Some(monitor),
            Duration::ZERO,
            &shutdown,
            || {
                factory_calls += 1;
                let mut m = FakeMonitor::new(vec![vec![event(true)]]);
                m.subscribe(&[SubscribeKind::Clipboard]).unwrap();
                Ok(m)
            },
            |batch| {
                captured.extend_from_slice(batch);
                if captured.len() >= 2 {
                    shutdown.store(true, Ordering::Relaxed);
                }
            },
            |_| {},
            |_| {},
            || {},
            || {},
        );
        assert_eq!(captured, vec![event(false), event(true)]);
        assert_eq!(factory_calls, 1);
    }
    #[test]
    fn shutdown_during_poll_does_not_deliver_or_recreate() {
        struct StoppingMonitor<'a>(&'a AtomicBool);
        impl ClipboardMonitor for StoppingMonitor<'_> {
            const AFFINITY: clipmonger_platform::types::Affinity =
                clipmonger_platform::types::Affinity::Worker;
            fn subscribe(&mut self, _: &[SubscribeKind]) -> Result<()> {
                Ok(())
            }
            fn poll(&mut self, _: Duration) -> Result<Vec<CaptureEvent>> {
                self.0.store(true, Ordering::Relaxed);
                Ok(vec![event(false)])
            }
        }
        let stop = AtomicBool::new(false);
        run_opt(
            Some(StoppingMonitor(&stop)),
            Duration::ZERO,
            &stop,
            || panic!("recreated after stop"),
            |_| panic!("delivered after stop"),
            |_| panic!("slept after stop"),
            |_| {},
            || {},
            || {},
        );
    }
}
