//! Xvfb acceptance harness for the X11 backend (frozen API, ruler-only).
//!
//! Every test takes a process-global serial lock (the binary must still be
//! run with `-- --test-threads=1 --nocapture`), starts its own `Xvfb`
//! display, drives fake selection owners / scratch windows over `x11rb`,
//! and asserts the frozen backend API (`X11Monitor`, `X11Transfer`,
//! `X11Hotkeys`, `X11Paste`, `X11Autostart`, `caps`) end to end.
//!
//! SKIP contract: when the `Xvfb` binary is missing (or no free display
//! exists) each test prints `SKIP ALL: ...` and passes without asserting
//! anything else. A leg that needs an unavailable facility (XTEST, a
//! window manager) prints a `SKIP-note` for that leg only and still
//! asserts the legs that can run.
//!
//! Every process spawned here is timeout-bounded: the `Xvfb` socket wait
//! is 5 s, every selection/poll loop carries an explicit `Instant`
//! deadline, sleeps only ever back a bounded poll (5-100 ms), and the
//! `Drop` impl reaps the server with a 5 s bounded wait.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clipmonger_backend_x11::{
    caps, MonitorOpts, X11Autostart, X11Hotkeys, X11Monitor, X11Paste, X11Transfer,
};
use clipmonger_platform::traits::{
    AutostartManager, ClipboardMonitor, ClipboardTransfer, HotkeyManager, PasteEngine,
};
use clipmonger_platform::types::{
    AppId, CaptureOrigin, ClipRef, HotkeyId, PasteStrategy, PasteTarget, PlatformHandle,
    SubscribeKind,
};
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{
    self, Atom, ConnectionExt as _, EventMask, PropMode, Window, WindowClass,
};
use x11rb::protocol::xtest;
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

// ---------------------------------------------------------------------------
// Serial gate + Xvfb fixture
// ---------------------------------------------------------------------------

/// Process-global serial lock: selection ownership and `DISPLAY` are global.
static SERIAL: OnceLock<Mutex<()>> = OnceLock::new();

fn serial() -> MutexGuard<'static, ()> {
    SERIAL
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn have_binary(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            let candidate = dir.join(name);
            candidate.is_file()
        })
    })
}

/// Owned nested X server for one test. `Drop` kills and reaps it.
struct Xvfb {
    child: Child,
    display: String,
    display_no: u32,
}

impl Xvfb {
    /// Scan `:99..:130` for a free display, spawn the server, wait for the
    /// socket (5 s budget). Also exports `DISPLAY` for the test binary.
    fn start() -> Result<Self, String> {
        if !have_binary("Xvfb") {
            return Err("Xvfb binary not found on PATH".to_string());
        }
        for n in 99u32..130 {
            let sock = format!("/tmp/.X11-unix/X{n}");
            if Path::new(&sock).exists() {
                continue;
            }
            let display = format!(":{n}");
            let mut child = Command::new("Xvfb")
                .arg(&display)
                .arg("-screen")
                .arg("0")
                .arg("1600x1200x24")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("spawn Xvfb :{n}: {e}"))?;
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut up = false;
            while Instant::now() < deadline {
                if Path::new(&sock).exists() {
                    up = true;
                    break;
                }
                thread::sleep(Duration::from_millis(50));
                match child.try_wait() {
                    Ok(Some(status)) => {
                        eprintln!("note: Xvfb :{n} exited early ({status}); trying next display");
                        break;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        eprintln!("note: Xvfb :{n} try_wait failed ({e}); trying next display");
                        break;
                    }
                }
            }
            if up {
                std::env::set_var("DISPLAY", &display);
                return Ok(Self {
                    child,
                    display,
                    display_no: n,
                });
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        Err("no free X display in :99..:130".to_string())
    }

    fn addr(&self) -> &str {
        &self.display
    }
}

impl Drop for Xvfb {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(None) => thread::sleep(Duration::from_millis(50)),
                Ok(Some(_)) | Err(_) => return,
            }
        }
        eprintln!(
            "warn: Xvfb :{} did not exit within 5 s of kill; leaving to init",
            self.display_no
        );
    }
}

/// Take the serial lock and start a display. `None` means SKIP-and-pass.
fn fixture(test: &str) -> Option<(MutexGuard<'static, ()>, Xvfb)> {
    let guard = serial();
    match Xvfb::start() {
        Ok(xvfb) => Some((guard, xvfb)),
        Err(reason) => {
            eprintln!("SKIP ALL ({test}): {reason}");
            None
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0))
}

/// Bounded poll helper: run `f` until it returns `Some`, else `None` after
/// `budget`. Sleeps 20 ms between attempts (backs a bounded poll only).
fn poll_until<T>(budget: Duration, f: impl FnMut() -> Option<T>) -> Option<T> {
    poll_until_with(budget, Duration::from_millis(20), f)
}

fn poll_until_with<T>(
    budget: Duration,
    step: Duration,
    mut f: impl FnMut() -> Option<T>,
) -> Option<T> {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(value) = f() {
            return Some(value);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(step);
    }
}

// ---------------------------------------------------------------------------
// Atoms + fake selection owner (x11rb)
// ---------------------------------------------------------------------------

/// `SelectionNotify` event code.
const SELECTION_NOTIFY: u8 = 31;
/// `ClientMessage` event code.
const CLIENT_MESSAGE: u8 = 33;
/// INCR chunk size for the 5 MiB leg.
const INCR_CHUNK: usize = 64 * 1024;

struct Atoms {
    clipboard: Atom,
    primary: Atom,
    targets: Atom,
    timestamp: Atom,
    utf8: Atom,
    string: Atom,
    text_atom: Atom,
    html: Atom,
    incr: Atom,
    integer: Atom,
    save_targets: Atom,
}

impl Atoms {
    fn intern(conn: &RustConnection) -> Result<Self, String> {
        let atom = |name: &[u8]| {
            xproto::intern_atom(conn, false, name)
                .map_err(|e| format!("intern_atom: {e}"))?
                .reply()
                .map(|reply| reply.atom)
                .map_err(|e| format!("intern_atom reply: {e}"))
        };
        Ok(Self {
            clipboard: atom(b"CLIPBOARD")?,
            primary: atom(b"PRIMARY")?,
            targets: atom(b"TARGETS")?,
            timestamp: atom(b"TIMESTAMP")?,
            utf8: atom(b"UTF8_STRING")?,
            string: atom(b"STRING")?,
            text_atom: atom(b"TEXT")?,
            html: atom(b"text/html")?,
            incr: atom(b"INCR")?,
            integer: atom(b"INTEGER")?,
            save_targets: atom(b"SAVE_TARGETS")?,
        })
    }
}

fn atom_bytes(atoms: &[Atom]) -> Vec<u8> {
    let mut out = Vec::with_capacity(atoms.len() * 4);
    for atom in atoms {
        out.extend_from_slice(&atom.to_ne_bytes());
    }
    out
}

/// Hand-rolled `SelectionNotify` for a request (trailing pad is zeroes).
fn notify_bytes(
    req_time: u32,
    requestor: Window,
    selection: Atom,
    target: Atom,
    property: Atom,
) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[0] = SELECTION_NOTIFY;
    bytes[4..8].copy_from_slice(&req_time.to_ne_bytes());
    bytes[8..12].copy_from_slice(&requestor.to_ne_bytes());
    bytes[12..16].copy_from_slice(&selection.to_ne_bytes());
    bytes[16..20].copy_from_slice(&target.to_ne_bytes());
    bytes[20..24].copy_from_slice(&property.to_ne_bytes());
    bytes
}

fn send_notify(
    conn: &RustConnection,
    requestor: Window,
    req_time: u32,
    selection: Atom,
    target: Atom,
    property: Atom,
) -> Result<(), String> {
    xproto::send_event(
        conn,
        false,
        requestor,
        EventMask::NO_EVENT,
        notify_bytes(req_time, requestor, selection, target, property),
    )
    .map_err(|e| format!("send_event notify: {e}"))?
    .check()
    .map_err(|e| format!("send_event notify check: {e}"))
}

/// What the fake owner serves, shared with the test thread.
#[derive(Debug, Clone)]
struct OwnerConfig {
    /// `"CLIPBOARD"` or `"PRIMARY"`.
    selection: &'static str,
    text: Arc<Mutex<Vec<u8>>>,
    html: Arc<Mutex<Vec<u8>>>,
    /// When true, never answer conversions (timeout leg).
    silent: bool,
    /// Serve payloads larger than this via INCR (0 = never).
    incr_threshold: usize,
    /// Drop the connection after the first INCR chunk (truncation leg).
    truncate_incr: bool,
    /// Bump to force the owner thread to re-claim ownership itself.
    reown: Arc<AtomicUsize>,
    /// Set once the owner thread holds the selection.
    owned: Arc<AtomicBool>,
}

#[derive(Debug, Clone)]
enum OwnerNote {
    Served {
        target: String,
    },
    SaveTargetsConvert,
    // Logged by the owner thread for debugging; never asserted, only written.
    #[allow(dead_code)]
    IncrChunk {
        index: usize,
        len: usize,
    },
    IncrTruncated,
    SaveTargetsNotifySeen,
    SelectionCleared,
}

type OwnerLog = Arc<Mutex<Vec<(Instant, OwnerNote)>>>;

fn note(log: &OwnerLog, entry: OwnerNote) {
    log.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push((Instant::now(), entry));
}

/// Serve one `SelectionRequest` per the behavior. Returns `OwnerDone` when
/// the thread must exit (truncated INCR drops the connection).
// Test fixture: params mirror SelectionRequest fields; bundling them would obscure the tests.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn serve_request(
    conn: &RustConnection,
    atoms: &Atoms,
    selection: Atom,
    _win: Window,
    req_time: u32,
    requestor: Window,
    target: Atom,
    property: Atom,
    behavior: &OwnerConfig,
    log: &OwnerLog,
    prop_watch: &mut bool,
) -> Result<bool, String> {
    let name_of = |atom: Atom| -> String {
        if atom == atoms.targets {
            "TARGETS".to_string()
        } else if atom == atoms.timestamp {
            "TIMESTAMP".to_string()
        } else if atom == atoms.utf8 || atom == atoms.string || atom == atoms.text_atom {
            "TEXT".to_string()
        } else if atom == atoms.html {
            "text/html".to_string()
        } else if atom == atoms.save_targets {
            "SAVE_TARGETS".to_string()
        } else {
            format!("atom({atom})")
        }
    };
    if behavior.silent {
        return Ok(true);
    }
    if target == atoms.targets {
        let list = atom_bytes(&[
            atoms.targets,
            atoms.timestamp,
            atoms.utf8,
            atoms.string,
            atoms.html,
        ]);
        xproto::change_property(
            conn,
            PropMode::REPLACE,
            requestor,
            property,
            xproto::AtomEnum::ATOM,
            32,
            u32::try_from(list.len() / 4).expect("atom list len fits u32"),
            &list,
        )
        .map_err(|e| format!("serve TARGETS: {e}"))?
        .check()
        .map_err(|e| format!("serve TARGETS check: {e}"))?;
        note(
            log,
            OwnerNote::Served {
                target: "TARGETS".to_string(),
            },
        );
        send_notify(conn, requestor, req_time, selection, target, property)?;
        return Ok(true);
    }
    if target == atoms.timestamp {
        xproto::change_property(
            conn,
            PropMode::REPLACE,
            requestor,
            property,
            atoms.integer,
            32,
            1,
            &req_time.to_ne_bytes(),
        )
        .map_err(|e| format!("serve TIMESTAMP: {e}"))?
        .check()
        .map_err(|e| format!("serve TIMESTAMP check: {e}"))?;
        send_notify(conn, requestor, req_time, selection, target, property)?;
        return Ok(true);
    }
    if target == atoms.save_targets {
        // ICCCM 2.8: the dying owner reports which targets the manager
        // should persist.
        let list = atom_bytes(&[atoms.utf8, atoms.html]);
        xproto::change_property(
            conn,
            PropMode::REPLACE,
            requestor,
            property,
            xproto::AtomEnum::ATOM,
            32,
            u32::try_from(list.len() / 4).expect("atom list len fits u32"),
            &list,
        )
        .map_err(|e| format!("serve SAVE_TARGETS: {e}"))?
        .check()
        .map_err(|e| format!("serve SAVE_TARGETS check: {e}"))?;
        note(log, OwnerNote::SaveTargetsConvert);
        send_notify(conn, requestor, req_time, selection, target, property)?;
        return Ok(true);
    }
    let (payload, reply_type) =
        if target == atoms.utf8 || target == atoms.string || target == atoms.text_atom {
            (
                behavior
                    .text
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone(),
                atoms.utf8,
            )
        } else if target == atoms.html {
            (
                behavior
                    .html
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone(),
                atoms.html,
            )
        } else {
            // Refuse unknown targets with property None.
            send_notify(conn, requestor, req_time, selection, target, x11rb::NONE)?;
            note(
                log,
                OwnerNote::Served {
                    target: format!("refused:{}", name_of(target)),
                },
            );
            return Ok(true);
        };
    if behavior.incr_threshold > 0 && payload.len() > behavior.incr_threshold {
        return serve_incr(
            conn, atoms, selection, requestor, property, target, req_time, &payload, reply_type,
            behavior, log, prop_watch,
        );
    }
    xproto::change_property(
        conn,
        PropMode::REPLACE,
        requestor,
        property,
        reply_type,
        8,
        u32::try_from(payload.len()).expect("payload len fits u32"),
        &payload,
    )
    .map_err(|e| format!("serve text: {e}"))?
    .check()
    .map_err(|e| format!("serve text check: {e}"))?;
    note(
        log,
        OwnerNote::Served {
            target: name_of(target),
        },
    );
    send_notify(conn, requestor, req_time, selection, target, property)?;
    Ok(true)
}

/// Serve `payload` via the INCR protocol. Returns `Ok(false)` when the
/// thread must exit without finishing (truncation leg).
#[allow(clippy::too_many_arguments)]
fn serve_incr(
    conn: &RustConnection,
    atoms: &Atoms,
    selection: Atom,
    requestor: Window,
    property: Atom,
    target: Atom,
    req_time: u32,
    payload: &[u8],
    reply_type: Atom,
    behavior: &OwnerConfig,
    log: &OwnerLog,
    prop_watch: &mut bool,
) -> Result<bool, String> {
    if !*prop_watch {
        xproto::change_window_attributes(
            conn,
            requestor,
            &xproto::ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )
        .map_err(|e| format!("watch requestor props: {e}"))?
        .check()
        .map_err(|e| format!("watch requestor props check: {e}"))?;
        *prop_watch = true;
    }
    let size = u32::try_from(payload.len()).expect("INCR payload len fits u32");
    xproto::change_property(
        conn,
        PropMode::REPLACE,
        requestor,
        property,
        atoms.incr,
        32,
        1,
        &size.to_ne_bytes(),
    )
    .map_err(|e| format!("INCR announce: {e}"))?
    .check()
    .map_err(|e| format!("INCR announce check: {e}"))?;
    send_notify(conn, requestor, req_time, selection, target, property)?;
    for (index, chunk) in payload.chunks(INCR_CHUNK).enumerate() {
        if !wait_property_deleted(conn, requestor, property, Duration::from_secs(10)) {
            return Err("INCR: requestor never deleted property for next chunk".to_string());
        }
        xproto::change_property(
            conn,
            PropMode::REPLACE,
            requestor,
            property,
            reply_type,
            8,
            u32::try_from(chunk.len()).expect("INCR chunk len fits u32"),
            chunk,
        )
        .map_err(|e| format!("INCR chunk {index}: {e}"))?
        .check()
        .map_err(|e| format!("INCR chunk {index} check: {e}"))?;
        note(
            log,
            OwnerNote::IncrChunk {
                index,
                len: chunk.len(),
            },
        );
        if behavior.truncate_incr {
            note(log, OwnerNote::IncrTruncated);
            return Ok(false);
        }
    }
    if !wait_property_deleted(conn, requestor, property, Duration::from_secs(10)) {
        return Err("INCR: requestor never deleted property for end marker".to_string());
    }
    xproto::change_property(
        conn,
        PropMode::REPLACE,
        requestor,
        property,
        reply_type,
        8,
        0,
        &[],
    )
    .map_err(|e| format!("INCR end: {e}"))?
    .check()
    .map_err(|e| format!("INCR end check: {e}"))?;
    Ok(true)
}

/// Wait until `property` on `window` is deleted (bounded).
fn wait_property_deleted(
    conn: &RustConnection,
    window: Window,
    property: Atom,
    budget: Duration,
) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        match conn.poll_for_event() {
            Ok(Some(Event::PropertyNotify(event))) => {
                if event.window == window
                    && event.atom == property
                    && event.state == xproto::Property::DELETE
                {
                    return true;
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => thread::sleep(Duration::from_millis(5)),
            Err(_) => return false,
        }
    }
    false
}

/// Spawn the fake owner thread. Returns the join handle plus the shared
/// log; the owner window id is published through `win_slot` once claimed.
// Test fixture: the setup-then-serve flow reads as one sequence; splitting would obscure it.
#[allow(clippy::too_many_lines)]
fn spawn_owner(
    display: &str,
    behavior: OwnerConfig,
    stop: Arc<AtomicBool>,
    log: OwnerLog,
    win_slot: Arc<Mutex<Option<Window>>>,
) -> thread::JoinHandle<()> {
    let display = display.to_string();
    thread::spawn(move || {
        let (conn, screen_num) = match x11rb::connect(Some(display.as_str())) {
            Ok(pair) => pair,
            Err(e) => {
                eprintln!("owner: connect failed: {e}");
                return;
            }
        };
        let atoms = match Atoms::intern(&conn) {
            Ok(atoms) => atoms,
            Err(e) => {
                eprintln!("owner: {e}");
                return;
            }
        };
        let selection = if behavior.selection == "PRIMARY" {
            atoms.primary
        } else {
            atoms.clipboard
        };
        let root = conn.setup().roots[screen_num].root;
        let win: Window = match conn.generate_id() {
            Ok(id) => id,
            Err(e) => {
                eprintln!("owner: generate_id: {e}");
                return;
            }
        };
        if xproto::create_window(
            &conn,
            x11rb::COPY_DEPTH_FROM_PARENT,
            win,
            root,
            0,
            0,
            10,
            10,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &xproto::CreateWindowAux::new(),
        )
        .map_err(|e| format!("owner create_window: {e}"))
        .and_then(|cookie| {
            cookie
                .check()
                .map_err(|e| format!("owner create_window check: {e}"))
        })
        .is_err()
        {
            eprintln!("owner: create_window failed");
            return;
        }
        if xproto::set_selection_owner(&conn, win, selection, x11rb::CURRENT_TIME)
            .map_err(|e| format!("owner set_selection_owner: {e}"))
            .and_then(|cookie| {
                cookie
                    .check()
                    .map_err(|e| format!("owner set_selection_owner check: {e}"))
            })
            .is_err()
        {
            eprintln!("owner: set_selection_owner failed");
            return;
        }
        let _ = conn.flush();
        behavior.owned.store(true, Ordering::SeqCst);
        *win_slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(win);
        let mut reown_seen = behavior.reown.load(Ordering::SeqCst);
        let mut prop_watch = false;
        while !stop.load(Ordering::SeqCst) {
            let wanted = behavior.reown.load(Ordering::SeqCst);
            if wanted != reown_seen {
                reown_seen = wanted;
                let _ = xproto::set_selection_owner(&conn, win, selection, x11rb::CURRENT_TIME)
                    .map_err(|e| format!("owner reown: {e}"))
                    .and_then(|cookie| {
                        cookie
                            .check()
                            .map_err(|e| format!("owner reown check: {e}"))
                    });
                let _ = conn.flush();
            }
            let Ok(event) = conn.poll_for_event() else {
                break;
            };
            let Some(event) = event else {
                thread::sleep(Duration::from_millis(5));
                continue;
            };
            match event {
                Event::SelectionRequest(req) => {
                    if req.selection != selection || req.owner != win {
                        continue;
                    }
                    match serve_request(
                        &conn,
                        &atoms,
                        selection,
                        win,
                        req.time,
                        req.requestor,
                        req.target,
                        req.property,
                        &behavior,
                        &log,
                        &mut prop_watch,
                    ) {
                        Ok(true) => {}
                        Ok(false) => break, // truncated INCR: drop out
                        Err(e) => {
                            eprintln!("owner: serve failed: {e}");
                            break;
                        }
                    }
                    let _ = conn.flush();
                }
                Event::SelectionNotify(notify) => {
                    if notify.target == atoms.save_targets {
                        note(&log, OwnerNote::SaveTargetsNotifySeen);
                    }
                }
                Event::SelectionClear(_) => {
                    note(&log, OwnerNote::SelectionCleared);
                }
                _ => {}
            }
        }
    })
}

/// Send a `SAVE_TARGETS` `ClientMessage` to the manager window (ICCCM 2.8).
fn send_save_targets(
    conn: &RustConnection,
    manager_win: Window,
    save_targets: Atom,
    timestamp: u32,
) -> Result<(), String> {
    let mut bytes = [0u8; 32];
    bytes[0] = CLIENT_MESSAGE;
    bytes[1] = 32; // format
    bytes[4..8].copy_from_slice(&manager_win.to_ne_bytes());
    bytes[8..12].copy_from_slice(&save_targets.to_ne_bytes());
    bytes[12..16].copy_from_slice(&timestamp.to_ne_bytes());
    xproto::send_event(conn, false, manager_win, EventMask::NO_EVENT, bytes)
        .map_err(|e| format!("send SAVE_TARGETS: {e}"))?
        .check()
        .map_err(|e| format!("send SAVE_TARGETS check: {e}"))?;
    conn.flush().map_err(|e| format!("flush SAVE_TARGETS: {e}"))
}

/// Create a small mapped scratch window on `display` for paste targeting.
fn make_scratch(display: &str) -> Result<(RustConnection, Window), String> {
    let (conn, screen_num) =
        x11rb::connect(Some(display)).map_err(|e| format!("scratch connect: {e}"))?;
    let root = conn.setup().roots[screen_num].root;
    let win: Window = conn.generate_id().map_err(|e| format!("scratch id: {e}"))?;
    xproto::create_window(
        &conn,
        x11rb::COPY_DEPTH_FROM_PARENT,
        win,
        root,
        10,
        10,
        200,
        100,
        0,
        WindowClass::INPUT_OUTPUT,
        x11rb::COPY_FROM_PARENT,
        &xproto::CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )
    .map_err(|e| format!("scratch create: {e}"))?
    .check()
    .map_err(|e| format!("scratch create check: {e}"))?;
    xproto::map_window(&conn, win)
        .map_err(|e| format!("scratch map: {e}"))?
        .check()
        .map_err(|e| format!("scratch map check: {e}"))?;
    conn.flush().map_err(|e| format!("scratch flush: {e}"))?;
    Ok((conn, win))
}

/// Resolve a keysym to its current keycode via the keyboard mapping.
fn keysym_to_keycode(conn: &RustConnection, keysym: u32) -> Result<u8, String> {
    let setup = conn.setup();
    let min = setup.min_keycode;
    let max = setup.max_keycode;
    let reply = conn
        .get_keyboard_mapping(min, max - min + 1)
        .map_err(|e| format!("keymap: {e}"))?
        .reply()
        .map_err(|e| format!("keymap reply: {e}"))?;
    let per = reply.keysyms_per_keycode as usize;
    for (index, chunk) in reply.keysyms.chunks(per).enumerate() {
        if chunk.first() == Some(&keysym) {
            return Ok(min + u8::try_from(index).expect("keycode index fits u8"));
        }
    }
    Err(format!("keysym {keysym:#x} has no keycode"))
}

/// `query_extension` presence check for the XTEST leg.
fn has_xtest(display: &str) -> bool {
    let Ok((conn, _)) = x11rb::connect(Some(display)) else {
        return false;
    };
    xproto::query_extension(&conn, b"XTEST")
        .map_err(|e| format!("query_extension: {e}"))
        .and_then(|cookie| {
            cookie
                .reply()
                .map_err(|e| format!("query_extension reply: {e}"))
        })
        .is_ok_and(|reply| reply.present)
}

fn default_opts(capture_primary: bool, debounce_ms: u64) -> MonitorOpts {
    MonitorOpts {
        capture_primary,
        debounce_ms,
        denylist: Vec::new(),
        max_bytes: 64 << 20,
    }
}

fn start_owned_clipboard(
    display: &str,
    text: Vec<u8>,
    html: Vec<u8>,
) -> (
    OwnerConfig,
    Arc<AtomicBool>,
    OwnerLog,
    thread::JoinHandle<()>,
) {
    let behavior = OwnerConfig {
        selection: "CLIPBOARD",
        text: Arc::new(Mutex::new(text)),
        html: Arc::new(Mutex::new(html)),
        silent: false,
        incr_threshold: 0,
        truncate_incr: false,
        reown: Arc::new(AtomicUsize::new(0)),
        owned: Arc::new(AtomicBool::new(false)),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let log: OwnerLog = Arc::new(Mutex::new(Vec::new()));
    let win_slot = Arc::new(Mutex::new(None));
    let handle = spawn_owner(
        display,
        behavior.clone(),
        Arc::clone(&stop),
        Arc::clone(&log),
        Arc::clone(&win_slot),
    );
    assert!(
        poll_until(Duration::from_secs(5), || {
            behavior.owned.load(Ordering::SeqCst).then_some(())
        })
        .is_some(),
        "fake owner never claimed CLIPBOARD"
    );
    (behavior, stop, log, handle)
}

fn stop_owner(stop: &Arc<AtomicBool>, handle: thread::JoinHandle<()>) {
    stop.store(true, Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_secs(5);
    // Join is unbounded on its own; bound it by leaking the handle only if
    // it refuses to exit (it checks `stop` every ~5 ms, so this is a guard).
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            eprintln!("warn: owner thread did not exit in 5 s; detaching");
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let _ = handle.join();
}

// ---------------------------------------------------------------------------
// t500: capability probe (xtest present under Xvfb)
// ---------------------------------------------------------------------------

#[test]
fn t500_caps_probe_xtest() {
    let Some((_guard, xvfb)) = fixture("t500_caps_probe_xtest") else {
        return;
    };
    let display = xvfb.addr().to_string();
    let xtest_present = has_xtest(&display);
    let capabilities = caps(Some(display.as_str()));
    assert_eq!(
        capabilities.monitor,
        clipmonger_platform::types::Level::Full
    );
    assert_eq!(
        capabilities.transfer,
        clipmonger_platform::types::Level::Full
    );
    assert!(capabilities.primary_selection, "primary must be supported");
    assert!(capabilities.autostart, "autostart must be supported");
    assert_eq!(capabilities.paste.strategy, PasteStrategy::ClipboardRestore);
    if xtest_present {
        assert_eq!(
            capabilities.paste.level,
            clipmonger_platform::types::Level::Full,
            "paste must be Full when XTEST is present"
        );
    } else {
        eprintln!("SKIP-note (t500): XTEST absent, paste level may degrade");
    }
}

// ---------------------------------------------------------------------------
// t505: capture + readback of text+html from a fake owner
// ---------------------------------------------------------------------------

#[test]
fn t505_capture_and_readback() {
    let Some((_guard, xvfb)) = fixture("t505_capture_and_readback") else {
        return;
    };
    let display = xvfb.addr().to_string();
    let text = b"clipmonger ruler text".to_vec();
    let html = b"<p>clipmonger ruler html</p>".to_vec();

    let (_behavior, stop, _log, owner) =
        start_owned_clipboard(&display, text.clone(), html.clone());

    let mut monitor = X11Monitor::connect(Some(display.as_str()), default_opts(false, 0))
        .expect("monitor connect");
    monitor
        .subscribe(&[SubscribeKind::Clipboard])
        .expect("monitor subscribe clipboard");

    let event = poll_until(Duration::from_secs(5), || {
        monitor
            .poll(Duration::from_millis(100))
            .ok()
            .and_then(|events| {
                events
                    .into_iter()
                    .find(|event| event.origin == CaptureOrigin::Clipboard && !event.sensitive)
            })
    });
    let event = event.expect("monitor must yield a Clipboard capture within 5 s");
    assert!(!event.offers.is_empty(), "capture must carry format offers");

    let transfer = X11Transfer::connect(Some(display.as_str())).expect("transfer connect");
    let formats = transfer.formats().expect("transfer formats");
    assert!(
        formats
            .iter()
            .any(|mime| mime == "text/plain;charset=utf-8"),
        "formats must be canonical, got {formats:?}"
    );
    assert!(
        formats.iter().any(|mime| mime == "text/html"),
        "formats must include text/html, got {formats:?}"
    );
    let read_text = transfer
        .read("text/plain;charset=utf-8", 64 << 20)
        .expect("transfer read utf8");
    assert_eq!(read_text, text, "utf8 readback must match owner bytes");
    let read_html = transfer
        .read("text/html", 64 << 20)
        .expect("transfer read html");
    assert_eq!(read_html, html, "html readback must match owner bytes");
    assert!(
        transfer.unchanged_indicator().is_some(),
        "unchanged indicator must be present after activity"
    );

    stop_owner(&stop, owner);
}

// ---------------------------------------------------------------------------
// t505 SAVE_TARGETS: dying owner asks the backend-as-manager to persist
// ---------------------------------------------------------------------------

#[test]
fn t505_save_targets_dying_owner() {
    let Some((_guard, xvfb)) = fixture("t505_save_targets_dying_owner") else {
        return;
    };
    let display = xvfb.addr().to_string();
    let text = b"save-me text".to_vec();
    let html = b"<p>save-me html</p>".to_vec();

    // The backend monitor starts with no manager rival, so it serves
    // CLIPBOARD_MANAGER itself (Serving role).
    let mut monitor = X11Monitor::connect(Some(display.as_str()), default_opts(false, 0))
        .expect("monitor connect");
    monitor
        .subscribe(&[SubscribeKind::Clipboard])
        .expect("monitor subscribe clipboard");

    let (_behavior, stop, log, owner) = start_owned_clipboard(&display, text.clone(), html.clone());

    let probe = x11rb::connect(Some(display.as_str())).expect("probe connect");
    let probe_conn = probe.0;
    let manager_atom = xproto::intern_atom(&probe_conn, false, b"CLIPBOARD_MANAGER")
        .expect("intern manager")
        .reply()
        .expect("intern manager reply")
        .atom;
    let save_targets_atom = xproto::intern_atom(&probe_conn, false, b"SAVE_TARGETS")
        .expect("intern save_targets")
        .reply()
        .expect("intern save_targets reply")
        .atom;
    // The manager selection must be owned by the backend now (not by us).
    let manager_win = poll_until(Duration::from_secs(5), || {
        let current = xproto::get_selection_owner(&probe_conn, manager_atom)
            .ok()?
            .reply()
            .ok()?
            .owner;
        (current != 0).then_some(current)
    })
    .expect("backend must own CLIPBOARD_MANAGER within 5 s");

    // Dying-owner handshake: ask the manager to save, then drop the owner
    // connection so only the persisted copy can answer afterwards.
    send_save_targets(&probe_conn, manager_win, save_targets_atom, 0).expect("send SAVE_TARGETS");
    let saw_notify = poll_until(Duration::from_secs(15), || {
        let entries = log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let convert_at = entries
            .iter()
            .find_map(|(at, entry)| matches!(entry, OwnerNote::SaveTargetsConvert).then_some(*at));
        let notify_at = entries.iter().find_map(|(at, entry)| {
            matches!(entry, OwnerNote::SaveTargetsNotifySeen).then_some(*at)
        });
        match (convert_at, notify_at) {
            (Some(converted), Some(notified)) => Some((converted, notified)),
            _ => None,
        }
    });
    let (converted, notified) = saw_notify.expect("manager must convert SAVE_TARGETS and notify");
    assert!(
        notified >= converted,
        "manager must notify SAVE_TARGETS only after transfers complete"
    );
    // Both payload targets must have been pulled before the notify.
    let entries = log
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let text_served = entries
        .iter()
        .any(|(_, entry)| matches!(entry, OwnerNote::Served { target } if target == "TEXT"));
    let html_served = entries
        .iter()
        .any(|(_, entry)| matches!(entry, OwnerNote::Served { target } if target == "text/html"));
    assert!(
        text_served && html_served,
        "manager must persist both targets"
    );

    // Owner exits: the backend copy must survive the owner connection drop.
    stop_owner(&stop, owner);
    let transfer = X11Transfer::connect(Some(display.as_str())).expect("transfer connect");
    let after_text = transfer
        .read("text/plain;charset=utf-8", 64 << 20)
        .expect("persisted text must remain readable after owner exit");
    assert_eq!(after_text, text);
    let after_html = transfer
        .read("text/html", 64 << 20)
        .expect("persisted html must remain readable after owner exit");
    assert_eq!(after_html, html);
}

// ---------------------------------------------------------------------------
// t505 INCR: 5 MiB via INCR, then a truncated owner
// ---------------------------------------------------------------------------

#[test]
fn t505_incr_5mib() {
    let Some((_guard, xvfb)) = fixture("t505_incr_5mib") else {
        return;
    };
    let display = xvfb.addr().to_string();
    let big: Vec<u8> = (0..(5 * 1024 * 1024))
        .map(|i| u8::try_from(i % 251).expect("mod 251 fits u8"))
        .collect();

    let behavior = OwnerConfig {
        selection: "CLIPBOARD",
        text: Arc::new(Mutex::new(big.clone())),
        html: Arc::new(Mutex::new(b"<p>incr</p>".to_vec())),
        silent: false,
        incr_threshold: 1,
        truncate_incr: false,
        reown: Arc::new(AtomicUsize::new(0)),
        owned: Arc::new(AtomicBool::new(false)),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let log: OwnerLog = Arc::new(Mutex::new(Vec::new()));
    let win_slot = Arc::new(Mutex::new(None));
    let owner = spawn_owner(
        &display,
        behavior.clone(),
        Arc::clone(&stop),
        Arc::clone(&log),
        win_slot,
    );
    assert!(
        poll_until(Duration::from_secs(5), || {
            behavior.owned.load(Ordering::SeqCst).then_some(())
        })
        .is_some(),
        "INCR owner never claimed CLIPBOARD"
    );

    let transfer = X11Transfer::connect(Some(display.as_str())).expect("transfer connect");
    let started = Instant::now();
    let got = transfer
        .read("text/plain;charset=utf-8", 8 << 20)
        .expect("5 MiB INCR read must succeed");
    assert_eq!(got.len(), big.len(), "INCR byte count must match");
    assert_eq!(
        got, big,
        "INCR bytes must match (hash-equivalent full compare)"
    );
    eprintln!("note: 5 MiB INCR read took {:?}", started.elapsed());
    stop_owner(&stop, owner);

    // Truncated leg: owner drops after the first chunk.
    let small: Vec<u8> = (0..(2 * INCR_CHUNK))
        .map(|i| u8::try_from(i % 251).expect("mod 251 fits u8"))
        .collect();
    let behavior = OwnerConfig {
        selection: "CLIPBOARD",
        text: Arc::new(Mutex::new(small)),
        html: Arc::new(Mutex::new(Vec::new())),
        silent: false,
        incr_threshold: 1,
        truncate_incr: true,
        reown: Arc::new(AtomicUsize::new(0)),
        owned: Arc::new(AtomicBool::new(false)),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let log: OwnerLog = Arc::new(Mutex::new(Vec::new()));
    let win_slot = Arc::new(Mutex::new(None));
    let truncated = spawn_owner(
        &display,
        behavior.clone(),
        Arc::clone(&stop),
        Arc::clone(&log),
        win_slot,
    );
    assert!(
        poll_until(Duration::from_secs(5), || {
            behavior.owned.load(Ordering::SeqCst).then_some(())
        })
        .is_some(),
        "truncated owner never claimed CLIPBOARD"
    );
    let transfer = X11Transfer::connect(Some(display.as_str())).expect("transfer connect");
    let started = Instant::now();
    let err = transfer
        .read("text/plain;charset=utf-8", 8 << 20)
        .expect_err("truncated INCR must fail");
    let elapsed = started.elapsed();
    eprintln!("note: truncated INCR failed after {elapsed:?}: {err:?}");
    assert!(
        elapsed <= Duration::from_secs(8),
        "truncated INCR must fail within ~7 s, took {elapsed:?}"
    );
    stop_owner(&stop, truncated);
}

// ---------------------------------------------------------------------------
// t505 owner timeout: silent owner, bounded failure, monitor still polls
// ---------------------------------------------------------------------------

#[test]
fn t505_owner_timeout() {
    let Some((_guard, xvfb)) = fixture("t505_owner_timeout") else {
        return;
    };
    let display = xvfb.addr().to_string();

    let behavior = OwnerConfig {
        selection: "CLIPBOARD",
        text: Arc::new(Mutex::new(b"unreadable".to_vec())),
        html: Arc::new(Mutex::new(Vec::new())),
        silent: true,
        incr_threshold: 0,
        truncate_incr: false,
        reown: Arc::new(AtomicUsize::new(0)),
        owned: Arc::new(AtomicBool::new(false)),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let log: OwnerLog = Arc::new(Mutex::new(Vec::new()));
    let win_slot = Arc::new(Mutex::new(None));
    let owner = spawn_owner(
        &display,
        behavior.clone(),
        Arc::clone(&stop),
        Arc::clone(&log),
        win_slot,
    );
    assert!(
        poll_until(Duration::from_secs(5), || {
            behavior.owned.load(Ordering::SeqCst).then_some(())
        })
        .is_some(),
        "silent owner never claimed CLIPBOARD"
    );

    let transfer = X11Transfer::connect(Some(display.as_str())).expect("transfer connect");
    let started = Instant::now();
    let err = transfer
        .read("text/plain;charset=utf-8", 64 << 20)
        .expect_err("silent owner must produce a read error");
    let elapsed = started.elapsed();
    eprintln!("note: silent-owner read failed after {elapsed:?}: {err:?}");
    assert!(
        elapsed >= Duration::from_secs(4),
        "read must wait out the ~5 s deadline, failed after {elapsed:?}"
    );
    assert!(
        elapsed <= Duration::from_secs(8),
        "read must fail within ~5 s deadline, took {elapsed:?}"
    );

    // The monitor must still be usable afterwards.
    let mut monitor = X11Monitor::connect(Some(display.as_str()), default_opts(false, 0))
        .expect("monitor connect");
    monitor
        .subscribe(&[SubscribeKind::Clipboard])
        .expect("monitor subscribe after timeout");
    monitor
        .poll(Duration::from_millis(200))
        .expect("monitor must still poll after an owner timeout");

    stop_owner(&stop, owner);
}

#[allow(clippy::too_many_lines)]
// ---------------------------------------------------------------------------
// t509: pre-claimed manager -> backend observes, then takes over
// ---------------------------------------------------------------------------
#[test]
fn t509_existing_manager_capture_only() {
    let Some((_guard, xvfb)) = fixture("t509_existing_manager_capture_only") else {
        return;
    };
    let display = xvfb.addr().to_string();

    // Fake rival holds CLIPBOARD_MANAGER before the backend starts.
    let (rival_conn, rival_screen) = x11rb::connect(Some(display.as_str())).expect("rival connect");
    let rival_root = rival_conn.setup().roots[rival_screen].root;
    let rival_win: Window = rival_conn.generate_id().expect("rival id");
    xproto::create_window(
        &rival_conn,
        x11rb::COPY_DEPTH_FROM_PARENT,
        rival_win,
        rival_root,
        0,
        0,
        10,
        10,
        0,
        WindowClass::INPUT_OUTPUT,
        x11rb::COPY_FROM_PARENT,
        &xproto::CreateWindowAux::new(),
    )
    .expect("rival create")
    .check()
    .expect("rival create check");
    let manager_atom = xproto::intern_atom(&rival_conn, false, b"CLIPBOARD_MANAGER")
        .expect("intern manager")
        .reply()
        .expect("intern manager reply")
        .atom;
    xproto::set_selection_owner(&rival_conn, rival_win, manager_atom, x11rb::CURRENT_TIME)
        .expect("rival claim")
        .check()
        .expect("rival claim check");
    rival_conn.flush().expect("rival flush");

    // Backend starts against the pre-claimed manager: Observing role.
    let mut monitor = X11Monitor::connect(Some(display.as_str()), default_opts(false, 0))
        .expect("monitor connect");
    monitor
        .subscribe(&[SubscribeKind::Clipboard])
        .expect("monitor subscribe clipboard");

    // Backend must NOT have stolen the manager.
    let (watch_conn, _) = x11rb::connect(Some(display.as_str())).expect("watch connect");
    let still_rival = xproto::get_selection_owner(&watch_conn, manager_atom)
        .expect("watch query")
        .reply()
        .expect("watch reply")
        .owner;
    assert_eq!(
        still_rival, rival_win,
        "backend must not steal a live CLIPBOARD_MANAGER"
    );

    // Capture still works while observing: fake clipboard owner serves.
    let text = b"observed clipboard".to_vec();
    let (_behavior, clip_stop, _log, clip_owner) =
        start_owned_clipboard(&display, text.clone(), text.clone());
    let event = poll_until(Duration::from_secs(5), || {
        monitor
            .poll(Duration::from_millis(100))
            .ok()
            .and_then(|events| {
                events
                    .into_iter()
                    .find(|event| event.origin == CaptureOrigin::Clipboard)
            })
    });
    event.expect("observing monitor must still capture clipboard");
    stop_owner(&clip_stop, clip_owner);

    // Rival drops: the backend must claim the manager within 5 s.
    drop(rival_conn);
    let claimed = poll_until(Duration::from_secs(5), || {
        let current = xproto::get_selection_owner(&watch_conn, manager_atom)
            .ok()?
            .reply()
            .ok()?
            .owner;
        (current != 0 && current != rival_win).then_some(current)
    });
    claimed.expect("backend must claim CLIPBOARD_MANAGER after rival exit");
}

// ---------------------------------------------------------------------------
// t511: primary policy (disabled, burst coalescing, min-len drop)
// ---------------------------------------------------------------------------

#[test]
fn t511_primary_policy() {
    let Some((_guard, xvfb)) = fixture("t511_primary_policy") else {
        return;
    };
    let display = xvfb.addr().to_string();

    let behavior = OwnerConfig {
        selection: "PRIMARY",
        text: Arc::new(Mutex::new(b"primary-one".to_vec())),
        html: Arc::new(Mutex::new(Vec::new())),
        silent: false,
        incr_threshold: 0,
        truncate_incr: false,
        reown: Arc::new(AtomicUsize::new(0)),
        owned: Arc::new(AtomicBool::new(false)),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let log: OwnerLog = Arc::new(Mutex::new(Vec::new()));
    let win_slot = Arc::new(Mutex::new(None));
    let owner = spawn_owner(
        &display,
        behavior.clone(),
        Arc::clone(&stop),
        Arc::clone(&log),
        win_slot,
    );
    assert!(
        poll_until(Duration::from_secs(5), || {
            behavior.owned.load(Ordering::SeqCst).then_some(())
        })
        .is_some(),
        "primary owner never claimed PRIMARY"
    );
    let set_primary = |value: &[u8]| {
        *behavior
            .text
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = value.to_vec();
        behavior.reown.fetch_add(1, Ordering::SeqCst);
    };

    // Leg 1: capture disabled -> PRIMARY change yields no event (1.5 s).
    let mut monitor = X11Monitor::connect(Some(display.as_str()), default_opts(false, 0))
        .expect("monitor connect");
    monitor
        .subscribe(&[SubscribeKind::Primary])
        .expect("monitor subscribe primary");
    set_primary(b"primary-two");
    let leaked = poll_until(Duration::from_millis(1500), || {
        monitor
            .poll(Duration::from_millis(100))
            .ok()
            .and_then(|events| {
                events
                    .into_iter()
                    .find(|event| event.origin == CaptureOrigin::Primary)
            })
    });
    assert!(
        leaked.is_none(),
        "capture_primary=false must emit no Primary event"
    );
    drop(monitor);

    // Leg 2: debounce 500 ms -> burst of 5 rapid sets yields exactly 1.
    let mut monitor = X11Monitor::connect(Some(display.as_str()), default_opts(true, 500))
        .expect("primary monitor connect");
    monitor
        .subscribe(&[SubscribeKind::Primary])
        .expect("primary monitor subscribe");
    // Drain anything the fresh monitor reports for the pre-burst value.
    let _ = monitor.poll(Duration::from_millis(700));
    for i in 0..5 {
        set_primary(format!("burst-value-{i:02}-padded").as_bytes());
        thread::sleep(Duration::from_millis(50));
    }
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut primary_events = 0;
    while Instant::now() < deadline {
        let batch = monitor
            .poll(Duration::from_millis(100))
            .expect("primary poll");
        primary_events += batch
            .into_iter()
            .filter(|event| event.origin == CaptureOrigin::Primary)
            .count();
    }
    assert_eq!(
        primary_events, 1,
        "burst of 5 rapid PRIMARY sets must coalesce to exactly 1 event"
    );

    // Leg 3: 2-byte set is below min_len -> no event (1.5 s).
    set_primary(b"ab");
    let short = poll_until(Duration::from_millis(1500), || {
        monitor
            .poll(Duration::from_millis(100))
            .ok()
            .and_then(|events| {
                events
                    .into_iter()
                    .find(|event| event.origin == CaptureOrigin::Primary)
            })
    });
    assert!(
        short.is_none(),
        "2-byte PRIMARY set must be dropped by min_len"
    );

    stop_owner(&stop, owner);
}

// ---------------------------------------------------------------------------
// t506: hotkey register conflict + synthesized keypress
// ---------------------------------------------------------------------------

#[test]
fn t506_hotkey_conflict() {
    let Some((_guard, xvfb)) = fixture("t506_hotkey_conflict") else {
        return;
    };
    let chord = "ctrl+alt+F9";
    let display = xvfb.addr().to_string();

    let mut first = X11Hotkeys::connect(Some(display.as_str())).expect("hotkey A connect");
    first
        .register(HotkeyId(7), chord)
        .expect("hotkey A register");

    let mut second = X11Hotkeys::connect(Some(display.as_str())).expect("hotkey B connect");
    let conflict = second.register(HotkeyId(7), chord);
    let err = conflict.expect_err("second grab of the same chord must fail");
    let message = format!("{err:?}").to_lowercase();
    assert!(
        message.contains("already") || message.contains("grab"),
        "conflict error must say already/grab, got {err:?}"
    );
    // The first manager must still be alive after the rival failed.
    first
        .poll(Duration::from_millis(100))
        .expect("first manager must still poll after conflict");

    // Synthesized keypress leg: needs XTEST.
    if !has_xtest(&display) {
        eprintln!("SKIP-note (t506): XTEST absent, synthesized-key leg skipped");
        return;
    }
    let (conn, screen_num) = x11rb::connect(Some(display.as_str())).expect("xtest connect");
    let root = conn.setup().roots[screen_num].root;
    let ctrl = keysym_to_keycode(&conn, 0xffe3).expect("Control_L keycode");
    let alt = keysym_to_keycode(&conn, 0xffe9).expect("Alt_L keycode");
    let f9 = keysym_to_keycode(&conn, 0xffc6).expect("F9 keycode");
    for (code, down) in [
        (ctrl, true),
        (alt, true),
        (f9, true),
        (f9, false),
        (alt, false),
        (ctrl, false),
    ] {
        let kind = if down {
            xproto::KEY_PRESS_EVENT
        } else {
            xproto::KEY_RELEASE_EVENT
        };
        xtest::fake_input(&conn, kind, code, x11rb::CURRENT_TIME, root, 0, 0, 0)
            .expect("fake_input")
            .check()
            .expect("fake_input check");
    }
    conn.flush().expect("xtest flush");
    let fired = poll_until(Duration::from_secs(3), || {
        first
            .poll(Duration::from_millis(100))
            .ok()
            .and_then(|ids| ids.into_iter().find(|id| *id == HotkeyId(7)))
    });
    fired.expect("synthesized ctrl+alt+F9 must fire the registered hotkey");
}

// ---------------------------------------------------------------------------
// t507: paste into a scratch window (transfer + restore halves)
// ---------------------------------------------------------------------------

#[test]
fn t507_paste_into_scratch() {
    let Some((_guard, xvfb)) = fixture("t507_paste_into_scratch") else {
        return;
    };
    let display = xvfb.addr().to_string();
    let (_scratch_conn, scratch) = make_scratch(&display).expect("scratch window");

    let transfer = X11Transfer::connect(Some(display.as_str())).expect("transfer connect");
    let original = b"restore-me original".to_vec();
    transfer
        .write(&[("text/plain;charset=utf-8".to_string(), original.clone())])
        .expect("seed clipboard with original");

    let paste = X11Paste::connect(Some(display.as_str())).expect("paste connect");
    assert_eq!(
        paste.strategy(),
        PasteStrategy::ClipboardRestore,
        "X11 paste strategy must be ClipboardRestore"
    );
    let target = PasteTarget {
        app: AppId {
            exe: "xvfb-harness".to_string(),
            title_hash: 0,
        },
        window_title_hash: 0,
        handle: PlatformHandle::X11Window(u64::from(scratch)),
        captured_ms: now_ms(),
    };
    let clip = ClipRef(uuid::Uuid::now_v7());
    let payloads = vec![(
        "text/plain;charset=utf-8".to_string(),
        b"pasted-clip".to_vec(),
    )];

    // Transfer half: observe the clip bytes on the clipboard while the
    // paste is in flight (reader thread; the windows-pattern restore
    // happens synchronously inside `paste_payloads`, so a post-return
    // read would only ever see the restored original).
    let seen = Arc::new(AtomicBool::new(false));
    let paste_done = Arc::new(AtomicBool::new(false));
    let reader_seen = Arc::clone(&seen);
    let reader_display = display.clone();
    let reader_payload = payloads[0].1.clone();
    let reader_done = Arc::clone(&paste_done);
    let reader = thread::spawn(move || {
        let transfer = X11Transfer::connect(Some(reader_display.as_str())).ok()?;
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if let Ok(bytes) = transfer.read("text/plain;charset=utf-8", 64 << 20) {
                if bytes == reader_payload {
                    reader_seen.store(true, Ordering::SeqCst);
                    return Some(());
                }
            }
            if reader_done.load(Ordering::SeqCst) {
                return None;
            }
            thread::sleep(Duration::from_millis(20));
        }
        None
    });
    let paste_display = display.clone();
    let paste_flag = Arc::clone(&paste_done);
    let paster = thread::spawn(move || {
        let paste = X11Paste::connect(Some(paste_display.as_str())).ok()?;
        let result = paste.paste_payloads(&target, &clip, &payloads);
        paste_flag.store(true, Ordering::SeqCst);
        Some(result)
    });
    let outcome = poll_until(Duration::from_secs(30), || {
        if paster.is_finished() {
            Some(())
        } else {
            None
        }
    });
    assert!(outcome.is_some(), "paste_payloads must return within 30 s");
    // Join the observer (bounded: it exits on its own deadline regardless).
    let _ = reader.join();
    let paste_result = paster.join().expect("paster join").expect("paster connect");
    paste_result.expect("paste_payloads must succeed against a live target");
    assert!(
        seen.load(Ordering::SeqCst),
        "transfer half: clip bytes must appear on the clipboard during paste"
    );

    // Restore half: once the paste settles, the original must be back.
    let restored = poll_until(Duration::from_secs(15), || {
        transfer
            .read("text/plain;charset=utf-8", 64 << 20)
            .ok()
            .and_then(|bytes| (bytes == original).then_some(()))
    });
    assert!(
        restored.is_some(),
        "restore half: clipboard must hold the original after the dwell when unchanged"
    );

    // Full keystroke leg needs a window manager for focus; bare Xvfb has
    // none, so record that honestly instead of asserting synthetic input.
    eprintln!(
        "SKIP-note (t507): no window manager under bare Xvfb; synthetic-keystroke leg not asserted"
    );
}

// ---------------------------------------------------------------------------
// t503: session autostart via desktop entry (no X needed, still serial)
// ---------------------------------------------------------------------------

#[test]
fn t503_session_autostart() {
    let Some((_guard, _xvfb)) = fixture("t503_session_autostart") else {
        return;
    };
    let root = std::env::temp_dir().join(format!(
        "clipmonger-x11-autostart-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let exe = PathBuf::from("/usr/bin/clipmonger-daemon");

    let mut autostart = X11Autostart::new(exe.clone(), root.clone(), false);
    autostart.enable().expect("autostart enable");
    assert!(
        autostart.is_enabled().expect("is_enabled"),
        "enabled must read back true"
    );
    let desktop = root.join("autostart").join("clipmonger.desktop");
    assert!(desktop.is_file(), "enable must write {}", desktop.display());
    let body = std::fs::read_to_string(&desktop).expect("read desktop entry");
    assert!(
        body.contains("/usr/bin/clipmonger-daemon"),
        "Exec must name the daemon"
    );

    autostart.disable().expect("autostart disable");
    assert!(
        !autostart.is_enabled().expect("is_enabled after disable"),
        "disable must read back false"
    );

    // Portable mode is a no-op: nothing written, enabled stays false.
    let portable_root = root.join("portable-home");
    let mut portable = X11Autostart::new(exe, portable_root.clone(), true);
    portable.enable().expect("portable enable must still Ok");
    assert!(
        !portable.is_enabled().expect("portable is_enabled"),
        "portable mode must remain disabled (no-op)"
    );
    assert!(
        !portable_root
            .join("autostart")
            .join("clipmonger.desktop")
            .exists(),
        "portable mode must not write a desktop entry"
    );

    std::fs::remove_dir_all(&root).ok();
}
