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
                let mut shared = self
                    .shared
                    .lock()
                    .map_err(|e| Error::Io(format!("monitor state lock failed: {e}")))?;
                if !shared.queue.is_empty() {
                    return Ok(std::mem::take(&mut shared.queue));
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
    };
    while !worker.stop.load(Ordering::Relaxed) {
        worker.pump_events();
        worker.snapshot_new_kinds();
        worker.manager_watch();
        worker.eval_primary();
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

    fn pump_events(&mut self) {
        loop {
            match self.conn.poll_for_event() {
                Ok(Some(Event::XfixesSelectionNotify(event))) => {
                    if event.subtype == SelectionEvent::SET_SELECTION_OWNER {
                        self.on_owner_change(event.selection);
                    }
                }
                Ok(Some(Event::SelectionRequest(req))) => {
                    session::serve_selection_request(
                        &self.conn,
                        &self.atoms,
                        &req,
                        &mut self.server,
                    );
                }
                Ok(Some(Event::SelectionClear(clear))) => {
                    if clear.selection == self.atoms.manager {
                        self.role = manager_logic::on_selection_clear(true);
                    }
                }
                Ok(Some(Event::ClientMessage(msg))) => {
                    if msg.type_ == self.atoms.save_targets {
                        self.save_handoff();
                    }
                }
                Ok(Some(Event::PropertyNotify(event)))
                    if event.state == xproto::Property::DELETE =>
                {
                    session::serve_property_delete(
                        &self.conn,
                        event.window,
                        event.atom,
                        &mut self.server,
                    );
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break,
            }
        }
        let _ = self.conn.flush();
    }

    fn on_owner_change(&mut self, selection: Atom) {
        if selection == self.atoms.clipboard && self.wants(SubscribeKind::Clipboard) {
            self.clipboard_changed();
        } else if selection == self.atoms.primary
            && self.wants(SubscribeKind::Primary)
            && self.opts.capture_primary
        {
            self.primary_changed();
        }
    }

    /// Snapshot newly subscribed kinds (covers owners that predate us).
    fn snapshot_new_kinds(&mut self) {
        let kinds = self.subscribed();
        if kinds == self.snapshotted_kinds {
            return;
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
                SubscribeKind::Clipboard => self.clipboard_changed(),
                SubscribeKind::Primary => {
                    if self.opts.capture_primary {
                        self.primary_changed();
                    }
                }
            }
        }
        self.snapshotted_kinds = kinds;
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
    fn selection_offers(&mut self, selection: Atom) -> Option<Vec<String>> {
        let mut serve = |req: &xproto::SelectionRequestEvent| {
            session::serve_selection_request(&self.conn, &self.atoms, req, &mut self.server);
        };
        let targets = session::read_targets(
            &self.conn,
            selection,
            self.atoms.targets,
            self.win,
            self.atoms.tmp_prop,
            TRANSFER_TIMEOUT,
            &mut serve,
        )
        .ok()?;
        let mut names = Vec::new();
        for atom in targets {
            if let Some(name) = session::atom_name(&self.conn, atom) {
                names.push(name);
            }
        }
        Some(names)
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

    fn clipboard_changed(&mut self) {
        let owner = session::selection_owner(&self.conn, self.atoms.clipboard);
        if owner == 0 || owner == self.win {
            return;
        }
        let Some(names) = self.selection_offers(self.atoms.clipboard) else {
            return;
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
    }

    fn primary_changed(&mut self) {
        let owner = session::selection_owner(&self.conn, self.atoms.primary);
        if owner == 0 || owner == self.win {
            return;
        }
        let target = self.atoms.utf8;
        let mut serve = |req: &xproto::SelectionRequestEvent| {
            session::serve_selection_request(&self.conn, &self.atoms, req, &mut self.server);
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
        )
        .map(|(_, value)| value)
        .unwrap_or_default();
        let at = now_ms();
        if self.debouncer.offer(bytes.len() as u64, at) {
            self.emit_primary();
        } else {
            self.primary_armed = true;
        }
    }

    fn eval_primary(&mut self) {
        if !self.primary_armed {
            return;
        }
        if self.debouncer.poll(now_ms()).is_some() {
            self.primary_armed = false;
            self.emit_primary();
        }
    }

    fn emit_primary(&mut self) {
        let names = self
            .selection_offers(self.atoms.primary)
            .unwrap_or_default();
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
    }

    /// Serve one dying-owner `SAVE_TARGETS` handoff (X-F5: notify only
    /// after every transfer completes; refuse with `None` on deadline).
    fn save_handoff(&mut self) {
        if self.role != ManagerState::Serving {
            return;
        }
        let owner = session::selection_owner(&self.conn, self.atoms.clipboard);
        if owner == 0 || owner == self.win {
            return;
        }
        let mut serve = |req: &xproto::SelectionRequestEvent| {
            session::serve_selection_request(&self.conn, &self.atoms, req, &mut self.server);
        };
        let targets = session::convert_read(
            &self.conn,
            self.atoms.clipboard,
            self.atoms.save_targets,
            self.win,
            self.atoms.tmp_prop,
            64 << 10,
            TRANSFER_TIMEOUT,
            &mut serve,
        )
        .map(|(_, value)| value)
        .unwrap_or_default();
        if !targets.len().is_multiple_of(4) {
            return;
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
            return;
        }
        let mut session_job = SaveSession::new(&names, 5000);
        let mut saved: Vec<(String, Vec<u8>)> = Vec::new();
        for name in &names {
            if self.stop.load(Ordering::Relaxed) {
                return;
            }
            let atom = xproto::intern_atom(&self.conn, false, name.as_bytes())
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .map_or(0, |reply| reply.atom);
            if atom == 0 {
                session_job.add_target_result(name, false);
                continue;
            }
            let mut serve = |req: &xproto::SelectionRequestEvent| {
                session::serve_selection_request(&self.conn, &self.atoms, req, &mut self.server);
            };
            match session::convert_read(
                &self.conn,
                self.atoms.clipboard,
                atom,
                self.win,
                self.atoms.tmp_prop,
                self.opts.max_bytes,
                TRANSFER_TIMEOUT,
                &mut serve,
            ) {
                Ok((_, bytes)) => {
                    if let Some(canonical) = mime::x11_to_canonical(name) {
                        saved.push((canonical, bytes));
                    }
                    session_job.add_target_result(name, true);
                }
                Err(_) => session_job.add_target_result(name, false),
            }
        }
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
    }
}
