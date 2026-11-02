//! Owned readiness handoff for the clipboard pump. Only a finished thread
//! may be joined; an abandoned or stopping owner blocks replacement.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub(crate) struct Registry(Mutex<Vec<JoinHandle<()>>>);

pub(crate) struct Started<T> {
    pub(crate) value: T,
    pub(crate) thread: JoinHandle<()>,
}

impl Registry {
    pub(crate) const fn new() -> Self {
        Self(Mutex::new(Vec::new()))
    }

    pub(crate) fn retain(&self, thread: JoinHandle<()>) {
        // Poison cannot discard the last owner or allow a replacement.
        let mut pending = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pending.push(thread);
        Self::reap(&mut pending);
    }

    fn reap(pending: &mut Vec<JoinHandle<()>>) {
        let mut i = 0;
        while i < pending.len() {
            if pending[i].is_finished() {
                let _ = pending.swap_remove(i).join();
            } else {
                i += 1;
            }
        }
    }

    pub(crate) fn start<T: Copy + Send + 'static>(
        &self,
        timeout: Duration,
        prepare: impl FnOnce(&AtomicBool) -> Result<T, String> + Send + 'static,
        run: impl FnOnce(T) + Send + 'static,
        cleanup: impl FnOnce(T) + Send + 'static,
    ) -> Result<Started<T>, String> {
        // Admission, spawn, timeout retention and reap share one lock. No
        // concurrent caller can overwrite a pending join handle.
        let mut pending = self
            .0
            .lock()
            .map_err(|_| "pump ownership lock poisoned".to_string())?;
        Self::reap(&mut pending);
        if !pending.is_empty() {
            return Err("previous monitor pump still stopping or starting".to_string());
        }
        let (ready_tx, ready_rx) = mpsc::channel();
        let (accept_tx, accept_rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let thread = thread::spawn(move || match prepare(&worker_cancel) {
            Ok(value) => {
                handoff(value, &ready_tx, &accept_rx, &worker_cancel, run, cleanup);
            }
            Err(error) => {
                let _ = ready_tx.send(Err(error));
            }
        });
        match ready_rx.recv_timeout(timeout) {
            Ok(Ok(value)) if accept_tx.send(()).is_ok() => Ok(Started { value, thread }),
            outcome => {
                cancel.store(true, Ordering::Release);
                // Dropping accept_tx releases a producer that published just
                // as recv_timeout expired. Late prepare also cleans up.
                drop(accept_tx);
                pending.push(thread);
                Self::reap(&mut pending);
                Err(match outcome {
                    Ok(Err(error)) => error,
                    _ => "monitor pump startup abandoned; ownership retained until reaped"
                        .to_string(),
                })
            }
        }
    }
}

// Called on the resource's creating thread; publication alone never grants
// permission to enter a blocking run loop.
fn handoff<T>(
    value: T,
    ready: &mpsc::Sender<Result<T, String>>,
    accepted: &mpsc::Receiver<()>,
    cancel: &AtomicBool,
    run: impl FnOnce(T),
    cleanup: impl FnOnce(T),
) where
    T: Copy,
{
    if ready.send(Ok(value)).is_ok() && accepted.recv().is_ok() && !cancel.load(Ordering::Acquire) {
        run(value);
    }
    cleanup(value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    fn wait_until(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !condition() {
            assert!(Instant::now() < deadline, "finite lifecycle gate timed out");
            thread::yield_now();
        }
    }

    #[test]
    fn abandoned_late_readiness_cleans_and_blocks_replacement_until_reaped() {
        let registry = Registry::new();
        let (release_tx, release_rx) = mpsc::channel();
        let cleaned = Arc::new(AtomicUsize::new(0));
        let cleanup_count = Arc::clone(&cleaned);
        assert!(registry
            .start(
                Duration::from_millis(10),
                move |_| {
                    release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                    Ok(7)
                },
                |_| panic!("abandoned startup entered blocking pump"),
                move |value| {
                    assert_eq!(value, 7);
                    cleanup_count.fetch_add(1, Ordering::SeqCst);
                }
            )
            .is_err());
        assert!(registry
            .start(Duration::from_secs(1), |_| Ok(()), |()| {}, |()| {})
            .is_err());
        release_tx.send(()).unwrap();
        wait_until(|| cleaned.load(Ordering::SeqCst) == 1);
        wait_until(|| {
            registry
                .0
                .lock()
                .unwrap()
                .iter()
                .all(JoinHandle::is_finished)
        });
        let next = registry
            .start(Duration::from_secs(1), |_| Ok(8), |_| {}, |_| {})
            .unwrap();
        registry.retain(next.thread);
        wait_until(|| {
            registry
                .0
                .lock()
                .unwrap()
                .iter()
                .all(JoinHandle::is_finished)
        });
        assert_eq!(next.value, 8);
    }

    #[test]
    fn accepted_readiness_runs_then_retains_stopping_owner() {
        let registry = Registry::new();
        let (stop_tx, stop_rx) = mpsc::channel();
        let started = registry
            .start(
                Duration::from_secs(1),
                |_| Ok(3),
                move |_| {
                    stop_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                },
                |_| {},
            )
            .unwrap();
        registry.retain(started.thread);
        assert!(registry
            .start(Duration::from_secs(1), |_| Ok(()), |()| {}, |()| {})
            .is_err());
        stop_tx.send(()).unwrap();
        wait_until(|| {
            registry
                .0
                .lock()
                .unwrap()
                .iter()
                .all(JoinHandle::is_finished)
        });
        let next = registry
            .start(Duration::from_secs(1), |_| Ok(()), |()| {}, |()| {})
            .unwrap();
        registry.retain(next.thread);
        wait_until(|| {
            registry
                .0
                .lock()
                .unwrap()
                .iter()
                .all(JoinHandle::is_finished)
        });
    }

    #[test]
    fn published_readiness_without_acceptance_never_runs() {
        let (ready_tx, ready_rx) = mpsc::channel();
        let (accept_tx, accept_rx) = mpsc::channel();
        let cleaned = Arc::new(AtomicBool::new(false));
        let worker_cleaned = Arc::clone(&cleaned);
        let worker = thread::spawn(move || {
            handoff(
                7,
                &ready_tx,
                &accept_rx,
                &AtomicBool::new(false),
                |_| panic!("published readiness entered pump without acceptance"),
                move |value| {
                    assert_eq!(value, 7);
                    worker_cleaned.store(true, Ordering::SeqCst);
                },
            );
        });
        assert_eq!(
            ready_rx
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .unwrap(),
            7
        );
        drop(accept_tx);
        wait_until(|| worker.is_finished());
        worker.join().unwrap();
        assert!(cleaned.load(Ordering::SeqCst));
    }

    #[test]
    fn concurrent_admission_cannot_overwrite_pending_owner() {
        let registry = Arc::new(Registry::new());
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let first_registry = Arc::clone(&registry);
        let first = thread::spawn(move || {
            first_registry.start(
                Duration::from_millis(20),
                move |_| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                    Ok(())
                },
                |()| panic!("abandoned run"),
                |()| {},
            )
        });
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let second_registry = Arc::clone(&registry);
        let second = thread::spawn(move || {
            second_registry.start(Duration::from_secs(1), |_| Ok(()), |()| {}, |()| {})
        });
        wait_until(|| first.is_finished() && second.is_finished());
        assert!(first.join().unwrap().is_err());
        assert!(second.join().unwrap().is_err());
        assert_eq!(registry.0.lock().unwrap().len(), 1);
        release_tx.send(()).unwrap();
        wait_until(|| {
            registry
                .0
                .lock()
                .unwrap()
                .iter()
                .all(JoinHandle::is_finished)
        });
    }

    #[test]
    fn poisoned_registry_refuses_admission_and_still_retains_shutdown_owner() {
        let registry = Arc::new(Registry::new());
        let poison_target = Arc::clone(&registry);
        let poisoner = thread::spawn(move || {
            let _guard = poison_target.0.lock().unwrap();
            panic!("poison ownership lock");
        });
        wait_until(|| poisoner.is_finished());
        assert!(poisoner.join().is_err());
        assert!(registry
            .start(Duration::from_secs(1), |_| Ok(()), |()| {}, |()| {})
            .is_err());
        let worker = thread::spawn(|| {});
        registry.retain(worker);
        wait_until(|| {
            registry
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .all(JoinHandle::is_finished)
        });
    }
}
