//! Synchronous clipboard transfer: requestor reads (direct + INCR,
//! ~5 s deadline) and persistent owner-side serving of our own writes.
//!
//! `connect` creates a private connection, window, and serving thread so
//! `write` keeps answering conversions after it returns. Reads use
//! short-lived per-call connections and never touch the serving lock while
//! waiting, so a read from our own store cannot deadlock the server.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use clipmonger_platform::traits::ClipboardTransfer;
use clipmonger_platform::types::{Affinity, Error, PayloadMap, Result};
use x11rb14::connection::Connection as _;
use x11rb14::protocol::xproto::{self, Window};
use x11rb14::protocol::Event;

use crate::mime;
use crate::session::{self, Atoms, ServerState, TRANSFER_TIMEOUT};

/// X11 clipboard transfer.
pub struct X11Transfer {
    display: Option<String>,
    win: Window,
    serving: Arc<ServingConn>,
    shared: Arc<Mutex<TransferShared>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

/// Serving connection: locked before [`TransferShared`] everywhere.
struct ServingConn {
    conn: Mutex<x11rb14::rust_connection::RustConnection>,
    atoms: Atoms,
}

#[derive(Default)]
struct TransferShared {
    server: ServerState,
    /// `owner:<win>-ts:<serial>` (X-F3); `None` before any activity.
    indicator: Option<String>,
    serial: u64,
    /// Owner the indicator was recorded for (reads only refresh it when
    /// ownership moved, so a restore check never invalidates itself).
    last_owner: Option<Window>,
}

impl X11Transfer {
    /// Connect to `display` (or `$DISPLAY`) and start the serving thread.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unavailable`] without an X session.
    pub fn connect(display: Option<&str>) -> Result<Self> {
        #[cfg(not(unix))]
        {
            let _ = display;
            return Err(Error::Unavailable(
                "x11 transfer needs a Unix X11 session".to_string(),
            ));
        }
        #[cfg(unix)]
        {
            let display_name = session::display_name(display);
            let (conn, screen) = session::connect(display_name.as_deref())?;
            let win = session::create_window(&conn, screen)?;
            let atoms = Atoms::intern(&conn)?;
            let serving = Arc::new(ServingConn {
                conn: Mutex::new(conn),
                atoms,
            });
            let shared = Arc::new(Mutex::new(TransferShared::default()));
            let stop = Arc::new(AtomicBool::new(false));
            let worker_serving = Arc::clone(&serving);
            let worker_shared = Arc::clone(&shared);
            let worker_stop = Arc::clone(&stop);
            let worker = std::thread::spawn(move || {
                serve_loop(&worker_serving, &worker_shared, &worker_stop);
            });
            Ok(Self {
                display: display_name,
                win,
                serving,
                shared,
                stop,
                worker: Some(worker),
            })
        }
    }

    /// Multi-format atomic write, served persistently afterwards.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unavailable`] without an X session.
    pub fn write(&self, payload: &[(String, Vec<u8>)]) -> Result<()> {
        let conn = self
            .serving
            .conn
            .lock()
            .map_err(|e| Error::Io(format!("transfer conn lock failed: {e}")))?;
        let mut shared = self
            .shared
            .lock()
            .map_err(|e| Error::Io(format!("transfer state lock failed: {e}")))?;
        session::claim_selection(&conn, self.win, self.serving.atoms.clipboard)?;
        shared.server.store.payloads = payload.to_vec();
        shared.serial += 1;
        shared.last_owner = Some(self.win);
        shared.indicator = Some(format!("owner:{}-ts:{}", self.win, shared.serial));
        Ok(())
    }

    /// Read one canonical MIME with a per-call connection.
    fn read_one(&self, mime: &str, max_bytes: u64) -> Result<Vec<u8>> {
        let (conn, screen) = session::connect(self.display.as_deref())?;
        let atoms = Atoms::intern(&conn)?;
        let win = session::create_window(&conn, screen)?;
        let target = atoms.atom_for_mime(&conn, mime)?;
        if target == 0 {
            return Err(Error::Unsupported(format!("unknown target '{mime}'")));
        }
        let mut serve = |_: &xproto::SelectionRequestEvent| {};
        let (_actual, value) = session::convert_read(
            &conn,
            atoms.clipboard,
            target,
            win,
            atoms.tmp_prop,
            max_bytes,
            TRANSFER_TIMEOUT,
            &mut serve,
        )?;
        let observed = session::selection_owner(&conn, atoms.clipboard);
        drop(conn);
        if let Ok(mut shared) = self.shared.lock() {
            if shared.last_owner != Some(observed) {
                shared.last_owner = Some(observed);
                shared.serial += 1;
                shared.indicator = Some(format!("owner:{observed}-ts:{}", shared.serial));
            }
        }
        Ok(value)
    }
}

/// A retired owner: its writer is gone but the serving connection stays
/// alive (still answering conversions) until process exit. X11 selections
/// need a living owner; dropping a writer must not blank the clipboard.
struct RetiredOwner {
    _serving: Arc<ServingConn>,
    _shared: Arc<Mutex<TransferShared>>,
    _stop: Arc<AtomicBool>,
    _worker: JoinHandle<()>,
}

static RETIRED: std::sync::LazyLock<Mutex<Vec<RetiredOwner>>> =
    std::sync::LazyLock::new(|| Mutex::new(Vec::new()));

/// Park a writer's serving state instead of stopping it.
fn retire_serving(
    serving: Arc<ServingConn>,
    shared: Arc<Mutex<TransferShared>>,
    stop: Arc<AtomicBool>,
    worker: JoinHandle<()>,
) {
    if let Ok(mut retired) = RETIRED.lock() {
        retired.push(RetiredOwner {
            _serving: serving,
            _shared: shared,
            _stop: stop,
            _worker: worker,
        });
    } else {
        stop.store(true, Ordering::Relaxed);
        let _ = worker.join();
    }
}

impl Drop for X11Transfer {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            retire_serving(
                Arc::clone(&self.serving),
                Arc::clone(&self.shared),
                Arc::clone(&self.stop),
                worker,
            );
        }
    }
}

/// Answer conversions of our owned window until `stop`.
fn serve_loop(
    serving: &Arc<ServingConn>,
    shared: &Arc<Mutex<TransferShared>>,
    stop: &Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Relaxed) {
        let event = serving
            .conn
            .lock()
            .ok()
            .and_then(|conn| conn.poll_for_event().ok().flatten());
        match event {
            Some(Event::SelectionRequest(req)) => {
                if let (Ok(conn), Ok(mut guard)) = (serving.conn.lock(), shared.lock()) {
                    session::serve_selection_request(
                        &conn,
                        &serving.atoms,
                        &req,
                        &mut guard.server,
                    );
                }
            }
            Some(Event::PropertyNotify(event)) if event.state == xproto::Property::DELETE => {
                if let (Ok(conn), Ok(mut guard)) = (serving.conn.lock(), shared.lock()) {
                    session::serve_property_delete(
                        &conn,
                        event.window,
                        event.atom,
                        &mut guard.server,
                    );
                }
            }
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

impl ClipboardTransfer for X11Transfer {
    const AFFINITY: Affinity = Affinity::Worker;

    fn formats(&self) -> Result<Vec<String>> {
        let (conn, screen) = session::connect(self.display.as_deref())?;
        let atoms = Atoms::intern(&conn)?;
        let win = session::create_window(&conn, screen)?;
        let mut serve = |_: &xproto::SelectionRequestEvent| {};
        let targets = session::read_targets(
            &conn,
            atoms.clipboard,
            atoms.targets,
            win,
            atoms.tmp_prop,
            TRANSFER_TIMEOUT,
            &mut serve,
        )?;
        let mut out = Vec::new();
        for atom in targets {
            let Some(name) = session::atom_name(&conn, atom) else {
                continue;
            };
            if name == "TARGETS" || name == "TIMESTAMP" {
                continue;
            }
            if let Some(canonical) = mime::x11_to_canonical(&name) {
                if !out.contains(&canonical) {
                    out.push(canonical);
                }
            }
        }
        Ok(out)
    }

    fn read(&self, mime: &str, max_bytes: u64) -> Result<Vec<u8>> {
        self.read_one(mime, max_bytes)
    }

    fn write(&self, payload: &PayloadMap) -> Result<()> {
        self.write(payload.as_slice())
    }

    fn unchanged_indicator(&self) -> Option<String> {
        self.shared
            .lock()
            .ok()
            .and_then(|guard| guard.indicator.clone())
    }
}

/// Scratch-window liveness helper shared with paste (window must exist).
pub(crate) fn window_exists(display: Option<&str>, win: Window) -> bool {
    let Ok((conn, _)) = session::connect(display) else {
        return false;
    };
    session::window_alive(&conn, win)
}
