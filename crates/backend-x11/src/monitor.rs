//! X11 clipboard monitor: `XFixes` owner-change events plus the
//! freedesktop clipboard-manager role (`CLIPBOARD_MANAGER` + `SAVE_TARGETS`).
//!
//! A worker thread owns the single X connection: it watches both selections,
//! serves `SelectionRequest`s for persisted payloads, claims the manager
//! selection when free (capture-only coexistence otherwise), and performs
//! dying-owner `SAVE_TARGETS` handoffs. `poll` only drains the event queue.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clipmonger_platform::traits::ClipboardMonitor;
use clipmonger_platform::types::{
    Affinity, CaptureEvent, CaptureOrigin, Error, FormatOffer, Result, SubscribeKind,
};
use x11rb14::connection::Connection as _;
use x11rb14::protocol::xfixes::{self, SelectionEvent, SelectionEventMask};
use x11rb14::protocol::xproto::{self, Atom, Window};
use x11rb14::protocol::Event;
use x11rb14::rust_connection::RustConnection;

use crate::manager_logic::{self, ManagerState, SaveSession};
use crate::mime;
use crate::primary::PrimaryDebouncer;
use crate::session::{self, Atoms, ServerState, TRANSFER_TIMEOUT};

/// Minimum PRIMARY text length captured (harness: 20-char bursts pass, a
/// 2-byte probe is dropped, so any `3 <= min_len <= 20` works).
const PRIMARY_MIN_LEN: u64 = 3;
/// Password-hint target marking a capture sensitive.
const PASSWORD_HINT: &str = "x-kde-passwordManagerHint";

/// Monitor subscription options (frozen API: exactly these fields).
#[derive(Debug, Clone)]
pub struct MonitorOpts {
    /// Capture the PRIMARY selection (default off).
    pub capture_primary: bool,
    /// PRIMARY quiet period before one coalesced event (ms).
    pub debounce_ms: u64,
    /// Owner window classes never recorded (union of
    /// `monitor.ignores` + `privacy.denylist`).
    pub denylist: Vec<String>,
    /// Largest single transfer accepted (bytes).
    pub max_bytes: u64,
}

/// X11 clipboard monitor.
pub struct X11Monitor {
    shared: Arc<Mutex<MonitorShared>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

#[derive(Debug, Default)]
struct MonitorShared {
    kinds: Vec<SubscribeKind>,
    queue: Vec<CaptureEvent>,
}

impl X11Monitor {
    /// Connect to `display` (or `$DISPLAY`) and start the worker thread.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unavailable`] without an X session.
    pub fn connect(display: Option<&str>, opts: MonitorOpts) -> Result<Self> {
        #[cfg(not(unix))]
        {
            let _ = (display, opts);
            return Err(Error::Unavailable(
                "x11 monitor needs a Unix X11 session".to_string(),
            ));
        }
        #[cfg(unix)]
        {
            let display_name = session::display_name(display);
            let shared = Arc::new(Mutex::new(MonitorShared::default()));
            let stop = Arc::new(AtomicBool::new(false));
            let worker_shared = Arc::clone(&shared);
            let worker_stop = Arc::clone(&stop);
            let worker = std::thread::spawn(move || {
                run_worker(display_name.as_deref(), opts, worker_shared, worker_stop);
            });
            Ok(Self {
                shared,
                stop,
                worker: Some(worker),
            })
        }
    }
}

impl Drop for X11Monitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl ClipboardMonitor for X11Monitor {
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
                if !shared.queue.is_empty() {
                    return Ok(std::mem::take(&mut shared.queue));
                }
                let dead = self.worker.as_ref().is_none_or(JoinHandle::is_finished);
                if poisoned || dead {
                    return Err(Error::Unavailable("x11 monitor worker exited".to_string()));
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

fn push_event(shared: &Arc<Mutex<MonitorShared>>, event: CaptureEvent) {
    if let Ok(mut guard) = shared.lock() {
        guard.queue.push(event);
    }
}

fn worker_event<T>(
    result: std::result::Result<Option<T>, impl std::fmt::Display>,
) -> Result<Option<T>> {
    result.map_err(|e| Error::Unavailable(format!("x11 monitor disconnected: {e}")))
}

fn is_terminal(error: &Error) -> bool {
    matches!(error, Error::Unavailable(msg) if msg.contains("monitor disconnected"))
}

fn conversion_result<T>(deferred: &session::DeferredQueue, result: Result<T>) -> Result<Option<T>> {
    deferred.check_terminal()?;
    match result {
        Ok(value) => Ok(Some(value)),
        Err(e) if is_terminal(&e) => Err(e),
        Err(_) => Ok(None),
    }
}

fn next_event(
    deferred: &mut session::DeferredQueue,
    poll: impl FnOnce() -> Result<Option<Event>>,
) -> Result<Option<Event>> {
    deferred.check_terminal()?;
    match deferred.pop_front() {
        Some(event) => Ok(Some(event)),
        None => poll(),
    }
}

struct Worker {
    conn: RustConnection,
    win: Window,
    atoms: Atoms,
    opts: MonitorOpts,
    shared: Arc<Mutex<MonitorShared>>,
    stop: Arc<AtomicBool>,
    server: ServerState,
    role: ManagerState,
    snapshotted_kinds: Vec<SubscribeKind>,
    debouncer: PrimaryDebouncer,
    primary_armed: bool,
    deferred: session::DeferredQueue,
}

fn run_worker(
    display: Option<&str>,
    opts: MonitorOpts,
    shared: Arc<Mutex<MonitorShared>>,
    stop: Arc<AtomicBool>,
) {
    let Ok((conn, screen)) = session::connect(display) else {
        return;
    };
    let Ok(win) = session::create_window(&conn, screen) else {
        return;
    };
    let Ok(atoms) = Atoms::intern(&conn) else {
        return;
    };
    let _ = xfixes::query_version(&conn, 5, 0)
        .ok()
        .and_then(|cookie| cookie.reply().ok());
    let _ = xfixes::select_selection_input(
        &conn,
        win,
        atoms.clipboard,
        SelectionEventMask::SET_SELECTION_OWNER,
    )
    .ok()
    .and_then(|cookie| cookie.check().ok());
    let _ = xfixes::select_selection_input(
        &conn,
        win,
        atoms.primary,
        SelectionEventMask::SET_SELECTION_OWNER,
    )
    .ok()
    .and_then(|cookie| cookie.check().ok());
    let _ = conn.flush();
    let debounce_ms = opts.debounce_ms;
    let mut worker = Worker {
        conn,
        win,
        atoms,
        opts,
        shared,
        stop,
        server: ServerState::new(),
        role: ManagerState::CaptureOnly,
        snapshotted_kinds: Vec::new(),
        debouncer: PrimaryDebouncer::new(debounce_ms, PRIMARY_MIN_LEN),
        primary_armed: false,
        deferred: session::DeferredQueue::new(),
    };
    while !worker.stop.load(Ordering::Relaxed) {
        if worker.pump_events().is_err() {
            break;
        }
        if worker.snapshot_new_kinds().is_err() {
            break;
        }
        worker.manager_watch();
        if worker.eval_primary().is_err() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

impl Worker {
    fn subscribed(&self) -> Vec<SubscribeKind> {
        self.shared
            .lock()
            .map(|guard| guard.kinds.clone())
            .unwrap_or_default()
    }

    fn wants(&self, kind: SubscribeKind) -> bool {
        self.subscribed().contains(&kind)
    }

    fn pump_events(&mut self) -> Result<()> {
        loop {
            match next_event(&mut self.deferred, || {
                worker_event(self.conn.poll_for_event())
            })? {
                Some(Event::XfixesSelectionNotify(event)) => {
                    if event.subtype == SelectionEvent::SET_SELECTION_OWNER {
                        self.on_owner_change(event.selection)?;
                    }
                }
                Some(Event::SelectionRequest(req)) => {
                    session::serve_selection_request(
                        &self.conn,
                        &self.atoms,
                        &req,
                        &mut self.server,
                    );
                }
                Some(Event::SelectionClear(clear)) => {
                    if clear.selection == self.atoms.manager {
                        self.role = manager_logic::on_selection_clear(true);
                    }
                }
                Some(Event::ClientMessage(msg)) => {
                    if msg.type_ == self.atoms.save_targets {
                        self.save_handoff()?;
                    }
                }
                Some(Event::PropertyNotify(event)) if event.state == xproto::Property::DELETE => {
                    session::serve_property_delete(
                        &self.conn,
                        event.window,
                        event.atom,
                        &mut self.server,
                    );
                }
                Some(_) => {}
                None => break,
            }
        }
        self.conn
            .flush()
            .map_err(|e| Error::Unavailable(format!("x11 monitor disconnected: {e}")))
    }

    fn on_owner_change(&mut self, selection: Atom) -> Result<()> {
        if selection == self.atoms.clipboard && self.wants(SubscribeKind::Clipboard) {
            self.clipboard_changed()?;
        } else if selection == self.atoms.primary
            && self.wants(SubscribeKind::Primary)
            && self.opts.capture_primary
        {
            self.primary_changed()?;
        }
        Ok(())
    }

    /// Snapshot newly subscribed kinds (covers owners that predate us).
    fn snapshot_new_kinds(&mut self) -> Result<()> {
        self.deferred.check_terminal()?;
        let kinds = self.subscribed();
        if kinds == self.snapshotted_kinds {
            return Ok(());
        }
        // Manager role is decided once real kinds arrive.
        let existing = session::selection_owner(&self.conn, self.atoms.manager) != 0;
        let role = manager_logic::on_start(existing);
        self.role = role;
        if role == ManagerState::Serving {
            let _ = session::claim_selection(&self.conn, self.win, self.atoms.manager);
        }
        for kind in &kinds {
            if self.snapshotted_kinds.contains(kind) {
                continue;
            }
            match kind {
                SubscribeKind::Clipboard => self.clipboard_changed()?,
                SubscribeKind::Primary => {
                    if self.opts.capture_primary {
                        self.primary_changed()?;
                    }
                }
            }
        }
        self.snapshotted_kinds = kinds;
        Ok(())
    }

    /// Claim the manager when a rival vacates it; note when we lose it.
    fn manager_watch(&mut self) {
        let owner = session::selection_owner(&self.conn, self.atoms.manager);
        if self.role == ManagerState::CaptureOnly && owner == 0 {
            if session::claim_selection(&self.conn, self.win, self.atoms.manager).is_ok() {
                self.role = manager_logic::on_manager_vacated();
            }
        } else if self.role == ManagerState::Serving && owner != 0 && owner != self.win {
            self.role = manager_logic::on_selection_clear(true);
        }
    }

    /// Read TARGETS from `selection` and map to canonical offers.
    fn selection_offers(&mut self, selection: Atom) -> Result<Option<Vec<String>>> {
        let mut serve = |event: session::OwnerEvent<'_>| {
            session::serve_owner_event(&self.conn, &self.atoms, &mut self.server, event)
        };
        let result = session::read_targets(
            &self.conn,
            selection,
            self.atoms.targets,
            self.win,
            self.atoms.tmp_prop,
            TRANSFER_TIMEOUT,
            &mut serve,
            &mut self.deferred,
        );
        let Some(targets) = conversion_result(&self.deferred, result)? else {
            return Ok(None);
        };
        let mut names = Vec::new();
        for atom in targets {
            if let Some(name) = session::atom_name(&self.conn, atom) {
                names.push(name);
            }
        }
        Ok(Some(names))
    }

    /// Owner window class of `selection` (best-effort, for the denylist).
    fn owner_class(&self, selection: Atom) -> String {
        let owner = session::selection_owner(&self.conn, selection);
        if owner == 0 {
            return String::new();
        }
        xproto::get_property(&self.conn, false, owner, self.atoms.wm_class, 0u32, 0, 64)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map_or_else(String::new, |reply| {
                String::from_utf8_lossy(&reply.value).into_owned()
            })
    }

    /// Whether a capture from `selection` with these target names is
    /// sensitive (denylisted owner class or a password-manager hint).
    fn is_sensitive(&self, selection: Atom, names: &[String]) -> bool {
        if names.iter().any(|name| name == PASSWORD_HINT) {
            return true;
        }
        if self.opts.denylist.is_empty() {
            return false;
        }
        let class = self.owner_class(selection).to_lowercase();
        if class.is_empty() {
            return false;
        }
        self.opts
            .denylist
            .iter()
            .any(|entry| !entry.is_empty() && class.contains(&entry.to_lowercase()))
    }

    fn clipboard_changed(&mut self) -> Result<()> {
        self.deferred.check_terminal()?;
        let owner = session::selection_owner(&self.conn, self.atoms.clipboard);
        if owner == 0 || owner == self.win {
            return Ok(());
        }
        let Some(names) = self.selection_offers(self.atoms.clipboard)? else {
            return Ok(());
        };
        let sensitive = self.is_sensitive(self.atoms.clipboard, &names);
        let offers = names
            .iter()
            .filter(|name| name.as_str() != "TARGETS" && name.as_str() != "TIMESTAMP")
            .filter_map(|name| {
                mime::x11_to_canonical(name).map(|mime| FormatOffer {
                    mime,
                    bytes_hint: None,
                })
            })
            .collect::<Vec<_>>();
        push_event(
            &self.shared,
            CaptureEvent {
                offers,
                origin: CaptureOrigin::Clipboard,
                sensitive,
            },
        );
        self.deferred.check_terminal()?;
        Ok(())
    }

    fn primary_changed(&mut self) -> Result<()> {
        self.deferred.check_terminal()?;
        let owner = session::selection_owner(&self.conn, self.atoms.primary);
        if owner == 0 || owner == self.win {
            return Ok(());
        }
        let target = self.atoms.utf8;
        let mut serve = |event: session::OwnerEvent<'_>| {
            session::serve_owner_event(&self.conn, &self.atoms, &mut self.server, event)
        };
        let bytes = session::convert_read(
            &self.conn,
            self.atoms.primary,
            target,
            self.win,
            self.atoms.tmp_prop,
            self.opts.max_bytes,
            TRANSFER_TIMEOUT,
            &mut serve,
            &mut self.deferred,
        )
        .map(|(_, value)| value);
        let bytes = conversion_result(&self.deferred, bytes)?.unwrap_or_default();
        let at = now_ms();
        if self.debouncer.offer(bytes.len() as u64, at) {
            self.emit_primary()?;
        } else {
            self.primary_armed = true;
        }
        Ok(())
    }

    fn eval_primary(&mut self) -> Result<()> {
        self.deferred.check_terminal()?;
        if !self.primary_armed {
            return Ok(());
        }
        if self.debouncer.poll(now_ms()).is_some() {
            self.primary_armed = false;
            self.emit_primary()?;
        }
        Ok(())
    }

    fn emit_primary(&mut self) -> Result<()> {
        let names = self
            .selection_offers(self.atoms.primary)?
            .unwrap_or_default();
        self.deferred.check_terminal()?;
        let sensitive = self.is_sensitive(self.atoms.primary, &names);
        let offers = names
            .iter()
            .filter(|name| name.as_str() != "TARGETS" && name.as_str() != "TIMESTAMP")
            .filter_map(|name| {
                mime::x11_to_canonical(name).map(|mime| FormatOffer {
                    mime,
                    bytes_hint: None,
                })
            })
            .collect::<Vec<_>>();
        push_event(
            &self.shared,
            CaptureEvent {
                offers,
                origin: CaptureOrigin::Primary,
                sensitive,
            },
        );
        Ok(())
    }

    /// Serve one dying-owner `SAVE_TARGETS` handoff (X-F5: notify only
    /// after every transfer completes; refuse with `None` on deadline).
    #[expect(
        clippy::too_many_lines,
        reason = "handoff ordering keeps conversion and final notification together"
    )]
    fn save_handoff(&mut self) -> Result<()> {
        self.deferred.check_terminal()?;
        if self.role != ManagerState::Serving {
            return Ok(());
        }
        let owner = session::selection_owner(&self.conn, self.atoms.clipboard);
        if owner == 0 || owner == self.win {
            return Ok(());
        }
        let mut serve = |event: session::OwnerEvent<'_>| {
            session::serve_owner_event(&self.conn, &self.atoms, &mut self.server, event)
        };
        let result = session::convert_read(
            &self.conn,
            self.atoms.clipboard,
            self.atoms.save_targets,
            self.win,
            self.atoms.tmp_prop,
            64 << 10,
            TRANSFER_TIMEOUT,
            &mut serve,
            &mut self.deferred,
        );
        let targets = conversion_result(&self.deferred, result)?
            .map(|(_, value)| value)
            .unwrap_or_default();
        if !targets.len().is_multiple_of(4) {
            return Ok(());
        }
        let (chunks, _) = targets.as_chunks::<4>();
        let names: Vec<String> = chunks
            .iter()
            .filter_map(|chunk| {
                let atom = Atom::from_ne_bytes(*chunk);
                if atom == self.atoms.targets || atom == self.atoms.timestamp {
                    return None;
                }
                session::atom_name(&self.conn, atom)
            })
            .collect();
        if names.is_empty() {
            return Ok(());
        }
        let mut session_job = SaveSession::new(&names, 5000);
        let mut saved: Vec<(String, Vec<u8>)> = Vec::new();
        for name in &names {
            if self.stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            let atom = xproto::intern_atom(&self.conn, false, name.as_bytes())
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .map_or(0, |reply| reply.atom);
            if atom == 0 {
                session_job.add_target_result(name, false);
                continue;
            }
            self.deferred.check_terminal()?;
            let mut serve = |event: session::OwnerEvent<'_>| {
                session::serve_owner_event(&self.conn, &self.atoms, &mut self.server, event)
            };
            let result = session::convert_read(
                &self.conn,
                self.atoms.clipboard,
                atom,
                self.win,
                self.atoms.tmp_prop,
                self.opts.max_bytes,
                TRANSFER_TIMEOUT,
                &mut serve,
                &mut self.deferred,
            );
            match conversion_result(&self.deferred, result)? {
                Some((_, bytes)) => {
                    if let Some(canonical) = mime::x11_to_canonical(name) {
                        saved.push((canonical, bytes));
                    }
                    session_job.add_target_result(name, true);
                }
                None => session_job.add_target_result(name, false),
            }
            self.deferred.check_terminal()?;
        }
        self.deferred.check_terminal()?;
        if session_job.complete() {
            // Notify only after every transfer finished (X-F5), then take
            // over CLIPBOARD so the persisted copy stays readable.
            let _ = session::send_selection_notify(
                &self.conn,
                owner,
                self.atoms.manager,
                self.atoms.save_targets,
                self.atoms.save_targets,
            );
            self.server.store.payloads = saved;
            let _ = session::claim_selection(&self.conn, self.win, self.atoms.clipboard);
        } else {
            // Deadline path: refuse (`None` property), discard partials.
            let _ = session::send_selection_notify(
                &self.conn,
                owner,
                self.atoms.manager,
                self.atoms.save_targets,
                0,
            );
        }
        self.deferred.check_terminal()?;
        Ok(())
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
            let mut monitor = X11Monitor {
                shared,
                stop,
                worker: Some(handle),
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
        let mut monitor = X11Monitor {
            shared,
            stop: Arc::new(AtomicBool::new(false)),
            worker: Some(handle),
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
        let mut monitor = X11Monitor {
            shared,
            stop: Arc::clone(&stop),
            worker: Some(handle),
        };
        assert_eq!(monitor.poll(Duration::ZERO).unwrap(), []);
        stop.store(true, Ordering::Relaxed);
        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !monitor.worker.as_ref().unwrap().is_finished() {
            assert!(Instant::now() < deadline, "finite shutdown gate timed out");
            std::thread::yield_now();
        }
        assert!(matches!(
            monitor.poll(Duration::ZERO),
            Err(Error::Unavailable(_))
        ));
    }
    #[test]
    fn connection_failure_is_terminal_but_idle_connection_is_not() {
        assert!(worker_event::<()>(Ok::<_, &str>(None)).unwrap().is_none());
        assert!(matches!(
            worker_event::<()>(Err("connection closed")),
            Err(Error::Unavailable(_))
        ));
    }
    #[test]
    fn deferred_queue_drains_before_new_poll_and_preserves_order() {
        let mut deferred = session::DeferredQueue::new();
        let save_atom = 999;
        let manager_atom = 888;
        let win = 42;
        let prop = 77;
        deferred
            .try_push(Event::ClientMessage(
                x11rb14::protocol::xproto::ClientMessageEvent {
                    response_type: 33,
                    sequence: 0,
                    window: win,
                    type_: save_atom,
                    format: 32,
                    data: [0u32; 5].into(),
                },
            ))
            .unwrap();
        deferred
            .try_push(Event::SelectionClear(
                x11rb14::protocol::xproto::SelectionClearEvent {
                    response_type: 0,
                    sequence: 0,
                    time: 0,
                    selection: manager_atom,
                    owner: 2,
                },
            ))
            .unwrap();
        deferred
            .try_push(Event::PropertyNotify(
                x11rb14::protocol::xproto::PropertyNotifyEvent {
                    response_type: 0,
                    sequence: 0,
                    window: win,
                    atom: prop,
                    time: 0,
                    state: x11rb14::protocol::xproto::Property::DELETE,
                },
            ))
            .unwrap();
        assert_eq!(deferred.len(), 3);
        let first = deferred.pop_front().unwrap();
        assert!(matches!(first, Event::ClientMessage(m) if m.type_ == 999));
        let second = deferred.pop_front().unwrap();
        assert!(matches!(second, Event::SelectionClear(m) if m.selection == 888));
        let third = deferred.pop_front().unwrap();
        assert!(matches!(third, Event::PropertyNotify(m) if m.atom == 77));
        assert!(deferred.is_empty());
    }

    #[test]
    fn deferred_overflow_fails_closed_and_sticky() {
        let mut deferred = session::DeferredQueue::with_capacity(1);
        deferred
            .try_push(Event::ClientMessage(
                x11rb14::protocol::xproto::ClientMessageEvent {
                    response_type: 33,
                    sequence: 0,
                    window: 1,
                    type_: 1,
                    format: 32,
                    data: [0u32; 5].into(),
                },
            ))
            .unwrap();
        let err = deferred
            .try_push(Event::ClientMessage(
                x11rb14::protocol::xproto::ClientMessageEvent {
                    response_type: 33,
                    sequence: 0,
                    window: 2,
                    type_: 2,
                    format: 32,
                    data: [0u32; 5].into(),
                },
            ))
            .unwrap_err();
        assert!(matches!(err, Error::Unavailable(m) if m.contains("overflow")));
        assert!(deferred.has_terminal());
        assert!(deferred.check_terminal().is_err());
        assert!(deferred
            .try_push(Event::SelectionClear(
                x11rb14::protocol::xproto::SelectionClearEvent {
                    response_type: 0,
                    sequence: 0,
                    time: 0,
                    selection: 10,
                    owner: 2,
                }
            ))
            .is_err());
    }

    #[test]
    fn terminal_error_is_not_swallowed_by_ok_unwrap_paths() {
        let mut deferred = session::DeferredQueue::new();
        deferred.set_terminal("x11 event poll failed: broken pipe");
        let check = deferred.check_terminal();
        assert!(matches!(&check, Err(Error::Unavailable(m)) if m.contains("monitor disconnected")));
        assert!(is_terminal(&check.unwrap_err()));
    }

    #[test]
    fn pump_next_event_rechecks_queue_after_each_handler() {
        fn event(id: u32) -> Event {
            Event::ClientMessage(xproto::ClientMessageEvent {
                response_type: 33,
                sequence: 0,
                window: 1,
                type_: id,
                format: 32,
                data: [0u32; 5].into(),
            })
        }
        let mut queue = session::DeferredQueue::new();
        let mut polls = 0;
        let first = next_event(&mut queue, || {
            polls += 1;
            Ok(Some(event(1)))
        })
        .unwrap();
        assert!(matches!(first, Some(Event::ClientMessage(m)) if m.type_ == 1));
        // An event consumed during this handler precedes the next connection event.
        queue.try_push(event(2)).unwrap();
        let next = next_event(&mut queue, || {
            polls += 1;
            Ok(Some(event(3)))
        })
        .unwrap();
        assert!(matches!(next, Some(Event::ClientMessage(m)) if m.type_ == 2));
        assert_eq!(polls, 1);
        let next = next_event(&mut queue, || {
            polls += 1;
            Ok(Some(event(3)))
        })
        .unwrap();
        assert!(matches!(next, Some(Event::ClientMessage(m)) if m.type_ == 3));
        queue.set_terminal("closed");
        assert!(next_event(&mut queue, || {
            polls += 1;
            Ok(Some(event(4)))
        })
        .is_err());
        assert_eq!(polls, 2);
    }

    #[test]
    fn conversion_result_keeps_refusal_nonterminal_and_stops_on_terminal() {
        let mut queue = session::DeferredQueue::new();
        assert!(
            conversion_result::<Vec<u8>>(&queue, Err(Error::Denied("refused".into())))
                .unwrap()
                .is_none()
        );
        assert!(
            conversion_result::<Vec<u8>>(&queue, Err(Error::Timeout("late".into())))
                .unwrap()
                .is_none()
        );
        assert!(conversion_result::<Vec<u8>>(
            &queue,
            Err(Error::Unavailable(
                "x11 monitor disconnected: closed".into()
            ))
        )
        .is_err());
        queue.set_terminal("overflow");
        assert!(conversion_result(&queue, Ok(vec![1])).is_err());
    }
}
