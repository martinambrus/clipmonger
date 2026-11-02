//! Ownership release and terminal decisions shared by Runner and its controls.

use std::collections::HashSet;
use std::time::{Duration, Instant};
use winit::event::WindowEvent;
use winit::window::WindowId;

use super::WindowKind;

/// Fits within the existing 2000ms allowance after Shell's 6000ms fallback.
pub(super) const TEARDOWN_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Directive {
    Run,
    WaitUntil(Instant),
    Exit,
    TimedOut(usize),
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum WindowDispatch {
    Normal(Option<WindowKind>),
    Shutdown(Directive),
}

/// IDs retain no Window owners. A terminal result stays guarded until loop exit.
pub(super) struct Shutdown {
    pending: HashSet<WindowId>,
    deadline: Instant,
    terminal: Option<Directive>,
}

impl Shutdown {
    pub(super) fn deadline(&self) -> Instant {
        self.deadline
    }

    fn advance(&mut self, now: Instant, destroyed: Option<WindowId>) -> Directive {
        if let Some(terminal) = self.terminal {
            return terminal;
        }
        // WaitUntil only wakes the loop. A queued final acknowledgment must not
        // turn a drain at/after its fixed deadline into successful completion.
        let directive = if now >= self.deadline {
            Directive::TimedOut(self.pending.len())
        } else {
            if let Some(id) = destroyed {
                self.pending.remove(&id);
            }
            if self.pending.is_empty() {
                Directive::Exit
            } else {
                return Directive::WaitUntil(self.deadline);
            }
        };
        self.terminal = Some(directive);
        directive
    }
}

/// The production transition: capture geometry while Settings still exists,
/// record value IDs, and release both complete hosts, including surface owners.
pub(super) fn begin<H>(
    shutdown: &mut Option<Shutdown>,
    popup: &mut Option<H>,
    settings: Option<&mut Option<H>>,
    orphan_pending: &mut HashSet<WindowId>,
    now: Instant,
    save_geometry: impl FnOnce(Option<&H>),
    window_id: impl Fn(&H) -> WindowId,
) -> Directive {
    if shutdown.is_some() {
        return poll(shutdown, now);
    }
    save_geometry(settings.as_deref().and_then(Option::as_ref));
    let mut pending = std::mem::take(orphan_pending);
    if let Some(host) = popup.as_ref() {
        pending.insert(window_id(host));
    }
    if let Some(host) = settings.as_deref().and_then(Option::as_ref) {
        pending.insert(window_id(host));
    }
    let terminal = pending.is_empty().then_some(Directive::Exit);
    *shutdown = Some(Shutdown {
        pending,
        deadline: now + TEARDOWN_TIMEOUT,
        terminal,
    });
    drop(popup.take());
    if let Some(slot) = settings {
        drop(slot.take());
    }
    poll(shutdown, now)
}

pub(super) fn poll(shutdown: &mut Option<Shutdown>, now: Instant) -> Directive {
    shutdown
        .as_mut()
        .map_or(Directive::Run, |state| state.advance(now, None))
}

/// Called before Runner looks for hosts, which are already gone during drain.
/// All ordinary events are suppressed while the drain or its terminal guard exists.
pub(super) fn window_event<H>(
    shutdown: &mut Option<Shutdown>,
    popup: Option<&H>,
    settings: Option<&H>,
    id: WindowId,
    event: &WindowEvent,
    now: Instant,
    window_id: impl Fn(&H) -> WindowId,
) -> WindowDispatch {
    if let Some(state) = shutdown.as_mut() {
        let destroyed = matches!(event, WindowEvent::Destroyed).then_some(id);
        return WindowDispatch::Shutdown(state.advance(now, destroyed));
    }
    let kind = if popup.is_some_and(|host| window_id(host) == id) {
        Some(WindowKind::Popup)
    } else if settings.is_some_and(|host| window_id(host) == id) {
        Some(WindowKind::Settings)
    } else {
        None
    };
    WindowDispatch::Normal(kind)
}

/// Map timeout decisions to the first production error through the same path
/// used for normal failures. This never replaces the cause that began draining.
pub(super) fn record_timeout<E>(
    directive: Directive,
    first: &mut Option<E>,
    timeout: impl FnOnce(usize) -> E,
) {
    if let Directive::TimedOut(pending) = directive {
        record_error(first, timeout(pending));
    }
}

/// Runner puts reconciliation (including activation), resume and user work behind
/// this gate. Copying the flag lets the callback borrow the whole Runner.
pub(super) fn ordinary_work(shutting_down: bool, work: impl FnOnce()) {
    if !shutting_down {
        work();
    }
}

pub(super) fn record_error<E>(first: &mut Option<E>, error: E) {
    if first.is_none() {
        *first = Some(error);
    }
}

/// The loop may fail after the error that initiated draining.
pub(super) fn finish<E>(first: Option<E>, loop_result: Result<(), E>) -> Result<(), E> {
    first.map_or(loop_result, Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex, Weak};

    fn id(value: u64) -> WindowId {
        WindowId::from(value)
    }

    struct MockWindow {
        id: WindowId,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl Drop for MockWindow {
        fn drop(&mut self) {
            self.events.lock().unwrap().push("window released");
        }
    }

    struct MockSurface {
        _window: Arc<MockWindow>,
    }

    struct MockHost {
        window: Arc<MockWindow>,
        _surface: MockSurface,
    }

    fn host(value: u64, events: &Arc<Mutex<Vec<&'static str>>>) -> (MockHost, Weak<MockWindow>) {
        let window = Arc::new(MockWindow {
            id: id(value),
            events: Arc::clone(events),
        });
        let weak = Arc::downgrade(&window);
        let surface = MockSurface {
            _window: Arc::clone(&window),
        };
        (
            MockHost {
                window,
                _surface: surface,
            },
            weak,
        )
    }

    fn begin_hosts(
        state: &mut Option<Shutdown>,
        popup: &mut Option<MockHost>,
        settings: &mut Option<MockHost>,
        orphans: &mut HashSet<WindowId>,
        now: Instant,
        save: impl FnOnce(Option<&MockHost>),
    ) -> Directive {
        begin(state, popup, Some(settings), orphans, now, save, |host| {
            host.window.id
        })
    }

    fn dispatch(
        state: &mut Option<Shutdown>,
        id: WindowId,
        event: &WindowEvent,
        now: Instant,
    ) -> Directive {
        match window_event::<MockHost>(state, None, None, id, event, now, |_| {
            panic!("removed hosts must not be looked up")
        }) {
            WindowDispatch::Normal(None) => Directive::Run,
            WindowDispatch::Shutdown(directive) => directive,
            other @ WindowDispatch::Normal(_) => panic!("unexpected host lookup: {other:?}"),
        }
    }

    /// This unchanged assertion is also the compiled old-transition negative's
    /// acceptance oracle. The negative mutates only the production begin body.
    #[test]
    fn complete_hosts_release_before_success_and_wait_for_actual_events() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let (popup, popup_window) = host(1, &events);
        let (settings, settings_window) = host(2, &events);
        // The surface really retains an additional Window owner; no external
        // strong Arc remains to manufacture release with a later test-side drop.
        assert_eq!(Arc::strong_count(&popup.window), 2);
        assert_eq!(Arc::strong_count(&settings.window), 2);
        let mut popup = Some(popup);
        let mut settings = Some(settings);
        let now = Instant::now();
        let mut state = None;
        assert_eq!(
            dispatch(&mut state, id(1), &WindowEvent::Destroyed, now),
            Directive::Run
        );
        let directive = begin_hosts(
            &mut state,
            &mut popup,
            &mut settings,
            &mut HashSet::new(),
            now,
            |host| {
                assert_eq!(host.unwrap().window.id, id(2));
                assert!(popup_window.upgrade().is_some());
                assert!(settings_window.upgrade().is_some());
                events.lock().unwrap().push("geometry saved");
            },
        );
        assert!(
            popup.is_none() && settings.is_none(),
            "complete hosts must be released before exit"
        );
        assert!(popup_window.upgrade().is_none() && settings_window.upgrade().is_none());
        assert_eq!(
            *events.lock().unwrap(),
            ["geometry saved", "window released", "window released"]
        );
        let wait = Directive::WaitUntil(now + TEARDOWN_TIMEOUT);
        assert_eq!(
            directive, wait,
            "success must await actual Destroyed callbacks"
        );
        // The same production event gate is called before host lookup, after
        // both slots have gone; ordinary events cannot stand in for Destroyed.
        assert_eq!(
            dispatch(&mut state, id(1), &WindowEvent::CloseRequested, now),
            wait
        );
        assert_eq!(
            dispatch(&mut state, id(99), &WindowEvent::Destroyed, now),
            wait
        );
        assert_eq!(
            dispatch(&mut state, id(1), &WindowEvent::Destroyed, now),
            wait
        );
        assert_eq!(state.as_ref().unwrap().pending.len(), 1);
        assert_eq!(
            dispatch(&mut state, id(1), &WindowEvent::Destroyed, now),
            wait
        );
        assert_eq!(state.as_ref().unwrap().pending.len(), 1);
        assert_eq!(
            dispatch(&mut state, id(2), &WindowEvent::Destroyed, now),
            Directive::Exit
        );
        assert_eq!(
            dispatch(&mut state, id(2), &WindowEvent::Destroyed, now),
            Directive::Exit
        );
        assert!(
            state.is_some(),
            "terminal state must keep ordinary work suppressed"
        );
    }

    #[test]
    fn destroyed_dispatch_precedes_production_host_lookup() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let (host, _) = host(1, &events);
        let now = Instant::now();
        let mut state = None;
        assert_eq!(
            window_event(
                &mut state,
                Some(&host),
                None,
                id(1),
                &WindowEvent::CloseRequested,
                now,
                |host| host.window.id
            ),
            WindowDispatch::Normal(Some(WindowKind::Popup)),
        );
        begin_hosts(
            &mut state,
            &mut None,
            &mut None,
            &mut HashSet::from([id(7)]),
            now,
            |_| {},
        );
        assert_eq!(
            window_event(
                &mut state,
                Some(&host),
                None,
                id(7),
                &WindowEvent::Destroyed,
                now,
                |_| panic!("shutdown must precede host lookup")
            ),
            WindowDispatch::Shutdown(Directive::Exit),
        );
    }

    #[test]
    fn no_host_exits_and_still_guards_ordinary_work() {
        let mut state = None;
        let mut saves = 0;
        let result = begin_hosts(
            &mut state,
            &mut None,
            &mut None,
            &mut HashSet::new(),
            Instant::now(),
            |host| {
                assert!(host.is_none());
                saves += 1;
            },
        );
        assert_eq!(result, Directive::Exit);
        assert_eq!(saves, 1);
        let mut no_settings_slot = None;
        assert_eq!(
            begin::<MockHost>(
                &mut no_settings_slot,
                &mut None,
                None,
                &mut HashSet::new(),
                Instant::now(),
                |host| assert!(host.is_none()),
                |host| host.window.id,
            ),
            Directive::Exit,
        );
        let mut activations = 0;
        ordinary_work(state.is_some(), || activations += 1);
        assert_eq!(activations, 0);
    }

    #[test]
    fn orphan_from_failed_setup_drains_even_without_hosts() {
        let now = Instant::now();
        let mut state = None;
        let mut orphans = HashSet::from([id(7)]);
        assert_eq!(
            begin_hosts(&mut state, &mut None, &mut None, &mut orphans, now, |_| {}),
            Directive::WaitUntil(now + TEARDOWN_TIMEOUT),
        );
        assert!(orphans.is_empty());
        assert_eq!(
            dispatch(&mut state, id(7), &WindowEvent::Destroyed, now),
            Directive::Exit
        );
    }

    #[test]
    fn fixed_deadline_reentry_suppression_and_timeout_result() {
        let now = Instant::now();
        let deadline = now + TEARDOWN_TIMEOUT;
        let mut state = None;
        let mut orphans = HashSet::from([id(1)]);
        begin_hosts(&mut state, &mut None, &mut None, &mut orphans, now, |_| {});
        let mut creations = 0;
        let mut activations = 0;
        ordinary_work(false, || {
            creations += 1;
            activations += 1;
        });
        assert_eq!(
            (creations, activations),
            (1, 1),
            "running positive control must execute work"
        );
        for _ in 0..3 {
            ordinary_work(state.is_some(), || {
                creations += 1;
                activations += 1;
            });
            let again = begin_hosts(
                &mut state,
                &mut None,
                &mut None,
                &mut orphans,
                now + Duration::from_millis(500),
                |_| panic!("re-entry must not save geometry or release again"),
            );
            assert_eq!(again, Directive::WaitUntil(deadline));
            assert_eq!(state.as_ref().unwrap().deadline(), deadline);
        }
        assert_eq!((creations, activations), (1, 1));
        assert_eq!(
            poll(
                &mut state,
                deadline.checked_sub(Duration::from_nanos(1)).unwrap()
            ),
            Directive::WaitUntil(deadline)
        );
        let directive = poll(&mut state, deadline);
        assert_eq!(directive, Directive::TimedOut(1));
        let mut first = None;
        record_timeout(directive, &mut first, |_| "timeout");
        assert_eq!(finish(first, Ok(())), Err("timeout"));
        ordinary_work(state.is_some(), || activations += 1);
        assert_eq!(activations, 1);
        assert_eq!(
            poll(&mut state, deadline + TEARDOWN_TIMEOUT),
            Directive::TimedOut(1)
        );
        assert_eq!(state.as_ref().unwrap().deadline(), deadline);
    }

    #[test]
    fn final_ack_at_or_after_deadline_is_an_error_and_cannot_renew_or_recover() {
        for lateness in [Duration::ZERO, Duration::from_millis(1)] {
            let now = Instant::now();
            let deadline = now + TEARDOWN_TIMEOUT;
            let mut state = None;
            begin_hosts(
                &mut state,
                &mut None,
                &mut None,
                &mut HashSet::from([id(1), id(2)]),
                now,
                |_| {},
            );
            assert_eq!(
                dispatch(&mut state, id(1), &WindowEvent::Destroyed, now),
                Directive::WaitUntil(deadline)
            );
            let directive = dispatch(
                &mut state,
                id(2),
                &WindowEvent::Destroyed,
                deadline + lateness,
            );
            assert_eq!(directive, Directive::TimedOut(1));
            let mut first = None;
            record_timeout(directive, &mut first, |_| "timeout");
            assert_eq!(finish(first, Ok(())), Err("timeout"));
            assert_eq!(
                dispatch(
                    &mut state,
                    id(2),
                    &WindowEvent::Destroyed,
                    deadline + lateness
                ),
                Directive::TimedOut(1)
            );
            assert_eq!(state.as_ref().unwrap().deadline(), deadline);
        }
    }

    #[test]
    fn ack_before_deadline_completes_and_terminal_success_is_stable() {
        let now = Instant::now();
        let deadline = now + TEARDOWN_TIMEOUT;
        let mut state = None;
        begin_hosts(
            &mut state,
            &mut None,
            &mut None,
            &mut HashSet::from([id(1)]),
            now,
            |_| {},
        );
        assert_eq!(
            dispatch(
                &mut state,
                id(1),
                &WindowEvent::Destroyed,
                deadline.checked_sub(Duration::from_nanos(1)).unwrap()
            ),
            Directive::Exit
        );
        assert_eq!(poll(&mut state, deadline), Directive::Exit);
        assert_eq!(finish::<&str>(None, Ok(())), Ok(()));
    }

    #[test]
    fn first_production_error_dominates_timeout_and_later_loop_error() {
        let mut first = None;
        record_error(&mut first, "surface setup");
        record_error(&mut first, "later window failure");
        record_timeout(Directive::TimedOut(1), &mut first, |_| "timeout");
        assert_eq!(
            finish(first, Err("later loop failure")),
            Err("surface setup")
        );
        assert_eq!(finish(None, Err("loop failure")), Err("loop failure"));
        assert_eq!(finish(Some("window setup"), Ok(())), Err("window setup"));
    }
}
