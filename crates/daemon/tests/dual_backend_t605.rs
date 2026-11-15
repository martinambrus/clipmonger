//! T-605 `[integration]` dual-BACKEND merge (Xvfb + nested Wayland),
//! seg-06 §8 / §10.
//!
//! Starts its own Xvfb display and its own headless sway (private
//! `XDG_RUNTIME_DIR`, no `XWayland`) side by side, runs the real daemon
//! binary with BOTH `DISPLAY` and `WAYLAND_DISPLAY` pointing at them and a
//! temp profile, then copies text through an in-process X11 selection owner
//! and an in-process Wayland data-control peer and reads history over IPC.
//!
//! What the daemon guarantees (read from the code, not the spec prose):
//! * both session backends start concurrently (`setup_session_backends`
//!   registers `x11` and `wayland`, each with a live monitor);
//! * a Wayland copy is stored exactly once with origin `wayland`;
//! * with `WAYLAND_DISPLAY` set the session is not an X11 session, so
//!   every X11-watcher capture is treated as `XWayland`'s mirror of the
//!   Wayland clipboard and dropped before any payload read
//!   (`skip_x11_capture`, commit 0452f30). An X11-only copy therefore is
//!   observed by the X11 monitor (it asks the owner for TARGETS) but never
//!   stored, and an identical copy made on both sides inside the 2 s merge
//!   window yields one clip, tagged `wayland`.
//!
//! SKIP contract: when Xvfb or sway is missing or does not come up, the
//! test prints `SKIP ALL: <reason>` and passes. Every child (Xvfb, sway,
//! daemon) is killed and reaped on drop, also when an assertion fails.
//! The daemon never sees the caller's session: its environment is cleared
//! and rebuilt from the private display, compositor and temp dirs.
#![cfg(all(
    unix,
    not(target_os = "macos"),
    feature = "backend-x11",
    feature = "backend-wayland"
))]

use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{
    self, AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, PropMode, SelectionNotifyEvent,
    WindowClass,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

/// Seg-06 §8 merge window (`Merger::new(2000)` in the daemon).
const MERGE_WINDOW: Duration = Duration::from_secs(2);

fn have_binary(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(name).is_file()))
}

fn stamp() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
}

/// Kill and reap `child` with a 5 s bounded wait.
fn reap(child: &mut Child, what: &str) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    let _ = child.kill();
    let deadline = Instant::now() + Duration::from_secs(5);
    while let Ok(None) = child.try_wait() {
        if Instant::now() >= deadline {
            eprintln!("warn: {what} did not exit within 5 s of kill; leaving to init");
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn poll_until<T>(budget: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(value) = f() {
            return Some(value);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

// ---------------------------------------------------------------------------
// Xvfb (private display)
// ---------------------------------------------------------------------------

struct Xvfb {
    child: Child,
    display: String,
    display_no: u32,
}

impl Xvfb {
    /// Scan `:160..:200` (clear of the backend-x11 harness's `:99..:130`)
    /// for a free display and wait up to 5 s for its socket.
    fn start() -> Result<Self, String> {
        if !have_binary("Xvfb") {
            return Err("Xvfb binary not found on PATH".to_string());
        }
        for n in 160u32..200 {
            let sock = format!("/tmp/.X11-unix/X{n}");
            if Path::new(&sock).exists() || Path::new(&format!("/tmp/.X{n}-lock")).exists() {
                continue;
            }
            let display = format!(":{n}");
            let mut child = Command::new("Xvfb")
                .arg(&display)
                .args(["-screen", "0", "640x480x24", "-nolisten", "tcp"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("spawn Xvfb {display}: {e}"))?;
            let up = poll_until(Duration::from_secs(5), || {
                if Path::new(&sock).exists() {
                    return Some(true);
                }
                matches!(child.try_wait(), Ok(Some(_)) | Err(_)).then_some(false)
            });
            if up == Some(true) {
                return Ok(Self {
                    child,
                    display,
                    display_no: n,
                });
            }
            reap(&mut child, "Xvfb");
        }
        Err("no free X display in :160..:200".to_string())
    }
}

impl Drop for Xvfb {
    fn drop(&mut self) {
        let pid = self.child.id();
        reap(&mut self.child, "Xvfb");
        // SIGKILL leaves the lock and socket behind; remove them if ours.
        let lock = format!("/tmp/.X{}-lock", self.display_no);
        let ours = std::fs::read_to_string(&lock)
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
            == Some(pid);
        if ours {
            let _ = std::fs::remove_file(format!("/tmp/.X11-unix/X{}", self.display_no));
            let _ = std::fs::remove_file(lock);
        }
    }
}

// ---------------------------------------------------------------------------
// Headless sway (private XDG_RUNTIME_DIR)
// ---------------------------------------------------------------------------

struct Sway {
    child: Child,
    runtime_dir: PathBuf,
    display: String,
}

impl Sway {
    fn start() -> Result<Self, String> {
        if !have_binary("sway") {
            return Err("sway binary not found on PATH".to_string());
        }
        let runtime_dir = std::env::temp_dir().join(format!(
            "clipmonger-t605-sway-{}-{}",
            std::process::id(),
            stamp()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&runtime_dir)
            .map_err(|e| format!("create runtime dir: {e}"))?;
        let config = runtime_dir.join("config");
        // No XWayland: the X side of this test is the separate Xvfb, so no
        // compositor bridge can copy between the two selections.
        std::fs::write(&config, "xwayland disable\n")
            .map_err(|e| format!("write sway config: {e}"))?;
        let mut command = Command::new("sway");
        command
            .arg("-c")
            .arg(&config)
            .env_clear()
            .env("XDG_RUNTIME_DIR", &runtime_dir)
            .env("HOME", &runtime_dir)
            .env("WLR_BACKENDS", "headless")
            .env("WLR_RENDERER", "pixman")
            .env("WLR_LIBINPUT_NO_DEVICES", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(path) = std::env::var_os("PATH") {
            command.env("PATH", path);
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&runtime_dir);
                return Err(format!("spawn sway: {e}"));
            }
        };
        let display = poll_until(Duration::from_secs(10), || {
            std::fs::read_dir(&runtime_dir).ok().and_then(|entries| {
                entries.flatten().find_map(|entry| {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let lock = Path::new(&name)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("lock"));
                    (name.starts_with("wayland-") && !lock).then_some(name)
                })
            })
        });
        let Some(display) = display else {
            let status = child.try_wait().ok().flatten();
            reap(&mut child, "sway");
            let _ = std::fs::remove_dir_all(&runtime_dir);
            return Err(match status {
                Some(status) => format!("sway exited early ({status}) before its socket appeared"),
                None => "no wayland-* socket appeared within 10 s".to_string(),
            });
        };
        Ok(Self {
            child,
            runtime_dir,
            display,
        })
    }
}

impl Drop for Sway {
    fn drop(&mut self) {
        reap(&mut self.child, "sway");
        let _ = std::fs::remove_dir_all(&self.runtime_dir);
    }
}

// ---------------------------------------------------------------------------
// Daemon under test
// ---------------------------------------------------------------------------

struct Daemon {
    child: Child,
    root: PathBuf,
    client: Option<clipmonger_ipc::client::Client>,
}

impl Daemon {
    fn start(xvfb: &Xvfb, sway: &Sway) -> Result<Self, String> {
        let root = std::env::temp_dir().join(format!(
            "clipmonger-t605-{}-{}",
            std::process::id(),
            stamp()
        ));
        let profile = root.join("profile");
        let home = root.join("home");
        for dir in [&profile, &home] {
            std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        }
        let mut command = Command::new(env!("CARGO_BIN_EXE_clipmonger-daemon"));
        // Cleared env: no session bus (portal dialogs), no session display.
        command
            .arg("--profile-dir")
            .arg(&profile)
            .env_clear()
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("XDG_RUNTIME_DIR", &sway.runtime_dir)
            .env("WAYLAND_DISPLAY", &sway.display)
            .env("DISPLAY", &xvfb.display)
            .env("CLIPMONGER_NO_UI_SPAWN", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        for name in ["PATH", "TMPDIR"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let child = command.spawn().map_err(|e| format!("spawn daemon: {e}"))?;
        let mut daemon = Self {
            child,
            root,
            client: None,
        };
        let rendezvous = profile.join("rendezvous.json");
        let socket = poll_until(Duration::from_secs(15), || {
            let text = std::fs::read_to_string(&rendezvous).ok()?;
            let value: Value = serde_json::from_str(&text).ok()?;
            value.get("socket")?.as_str().map(PathBuf::from)
        });
        let Some(socket) = socket else {
            let status = daemon.child.try_wait().ok().flatten();
            return Err(format!("daemon never served (exit status {status:?})"));
        };
        let token = std::fs::read_to_string(profile.join("token"))
            .map_err(|e| format!("read token: {e}"))?;
        let client = poll_until(Duration::from_secs(5), || {
            clipmonger_ipc::client::Client::connect(&socket, token.trim()).ok()
        })
        .ok_or("IPC connect failed")?;
        daemon.client = Some(client);
        Ok(daemon)
    }

    fn call(&self, method: &str, params: Value) -> Value {
        self.client
            .as_ref()
            .expect("connected")
            .call(method, params)
            .unwrap_or_else(|e| panic!("IPC {method} failed: {e:?}"))
    }

    /// Live history rows whose text is `text`.
    fn rows_with_text(&self, text: &str) -> Vec<Value> {
        let list = self.call("history.list", json!({"limit": 500}));
        list["clips"]
            .as_array()
            .map(|clips| {
                clips
                    .iter()
                    .filter(|clip| clip["search_text"].as_str() == Some(text))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn wait_rows(&self, text: &str, budget: Duration) -> Vec<Value> {
        poll_until(budget, || {
            let rows = self.rows_with_text(text);
            (!rows.is_empty()).then_some(rows)
        })
        .unwrap_or_default()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(client) = self.client.take() {
            let _ = client.call("daemon.stop", json!({}));
        }
        let graceful = poll_until(Duration::from_secs(3), || {
            self.child.try_wait().ok().flatten().map(|_| ())
        });
        if graceful.is_none() {
            reap(&mut self.child, "daemon");
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

// ---------------------------------------------------------------------------
// X11 client: an in-process CLIPBOARD owner on the Xvfb display
// ---------------------------------------------------------------------------

#[derive(Default)]
struct OwnerLog {
    /// Requestor windows that asked for TARGETS.
    targets_requests: usize,
    /// Payload (text) conversions served.
    payload_reads: usize,
}

struct X11Owner {
    stop: Arc<AtomicBool>,
    log: Arc<Mutex<OwnerLog>>,
    worker: Option<JoinHandle<()>>,
}

impl X11Owner {
    fn copy(display: &str, text: &str) -> Result<Self, String> {
        let (conn, screen_num) =
            RustConnection::connect(Some(display)).map_err(|e| format!("x11 connect: {e}"))?;
        let root = conn.setup().roots[screen_num].root;
        let win = conn.generate_id().map_err(|e| format!("x11 id: {e}"))?;
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            win,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new(),
        )
        .map_err(|e| format!("x11 create_window: {e}"))?;
        let intern = |name: &str| -> Result<u32, String> {
            conn.intern_atom(false, name.as_bytes())
                .map_err(|e| format!("intern {name}: {e}"))?
                .reply()
                .map(|reply| reply.atom)
                .map_err(|e| format!("intern {name}: {e}"))
        };
        let clipboard = intern("CLIPBOARD")?;
        let targets = intern("TARGETS")?;
        let text_atoms = [
            intern("UTF8_STRING")?,
            intern("text/plain;charset=utf-8")?,
            intern("text/plain")?,
        ];
        conn.set_selection_owner(win, clipboard, x11rb::CURRENT_TIME)
            .map_err(|e| format!("set owner: {e}"))?;
        let owner = conn
            .get_selection_owner(clipboard)
            .map_err(|e| format!("get owner: {e}"))?
            .reply()
            .map_err(|e| format!("get owner: {e}"))?
            .owner;
        if owner != win {
            return Err("X11 CLIPBOARD ownership not taken".to_string());
        }
        conn.flush().map_err(|e| format!("flush: {e}"))?;

        let stop = Arc::new(AtomicBool::new(false));
        let log = Arc::new(Mutex::new(OwnerLog::default()));
        let worker = {
            let stop = Arc::clone(&stop);
            let log = Arc::clone(&log);
            let served = Served {
                targets,
                text_atoms,
                bytes: text.as_bytes().to_vec(),
            };
            thread::spawn(move || {
                serve_owner(&conn, &served, &stop, &log);
                let _ = conn.destroy_window(win);
                let _ = conn.flush();
            })
        };
        Ok(Self {
            stop,
            log,
            worker: Some(worker),
        })
    }

    fn targets_requests(&self) -> usize {
        self.log.lock().map_or(0, |log| log.targets_requests)
    }

    fn payload_reads(&self) -> usize {
        self.log.lock().map_or(0, |log| log.payload_reads)
    }
}

/// What one X11 owner offers.
struct Served {
    targets: u32,
    text_atoms: [u32; 3],
    bytes: Vec<u8>,
}

/// Answer `SelectionRequest`s (TARGETS and the text targets) until `stop`.
fn serve_owner(conn: &RustConnection, served: &Served, stop: &AtomicBool, log: &Mutex<OwnerLog>) {
    let Served {
        targets,
        text_atoms,
        bytes,
    } = served;
    let (targets, text_atoms) = (*targets, *text_atoms);
    while !stop.load(Ordering::Relaxed) {
        let event = match conn.poll_for_event() {
            Ok(Some(event)) => event,
            Ok(None) => {
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            Err(_) => return,
        };
        let Event::SelectionRequest(req) = event else {
            continue;
        };
        let property = if req.property == u32::from(AtomEnum::NONE) {
            req.target
        } else {
            req.property
        };
        let served = if req.target == targets {
            let mut offered = vec![targets];
            offered.extend_from_slice(&text_atoms);
            if let Ok(mut log) = log.lock() {
                log.targets_requests += 1;
            }
            conn.change_property32(
                PropMode::REPLACE,
                req.requestor,
                property,
                AtomEnum::ATOM,
                &offered,
            )
            .is_ok()
        } else if text_atoms.contains(&req.target) {
            if let Ok(mut log) = log.lock() {
                log.payload_reads += 1;
            }
            conn.change_property8(
                PropMode::REPLACE,
                req.requestor,
                property,
                req.target,
                bytes,
            )
            .is_ok()
        } else {
            false
        };
        let notify = SelectionNotifyEvent {
            response_type: xproto::SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: req.time,
            requestor: req.requestor,
            selection: req.selection,
            target: req.target,
            property: if served {
                property
            } else {
                u32::from(AtomEnum::NONE)
            },
        };
        let _ = conn.send_event(false, req.requestor, EventMask::NO_EVENT, notify);
        let _ = conn.flush();
    }
}

impl Drop for X11Owner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Wayland client: wl-clipboard-rs data-control copy on the nested sway
// ---------------------------------------------------------------------------

/// Copy `text` as the Wayland clipboard of `sway`. wl-clipboard-rs reads
/// `XDG_RUNTIME_DIR` / `WAYLAND_DISPLAY` from this process's environment,
/// which `main_env` pointed at the private compositor for the whole test.
fn wayland_copy(text: &str) -> Result<(), String> {
    use wl_clipboard_rs::copy::{ClipboardType, MimeSource, MimeType, Options, Seat, Source};
    let mut options = Options::new();
    options.clipboard(ClipboardType::Regular).seat(Seat::All);
    let sources = ["text/plain;charset=utf-8", "text/plain"]
        .iter()
        .map(|mime| MimeSource {
            source: Source::Bytes(text.as_bytes().to_vec().into_boxed_slice()),
            mime_type: MimeType::Specific((*mime).to_string()),
        })
        .collect();
    options
        .copy_multi(sources)
        .map_err(|e| format!("wayland copy: {e}"))
}

/// Point this process's Wayland env at the private compositor; restore the
/// caller's values on drop (this binary holds a single test).
struct WaylandEnv {
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl WaylandEnv {
    fn point_at(sway: &Sway) -> Self {
        let saved = ["XDG_RUNTIME_DIR", "WAYLAND_DISPLAY"]
            .into_iter()
            .map(|name| (name, std::env::var_os(name)))
            .collect();
        std::env::set_var("XDG_RUNTIME_DIR", &sway.runtime_dir);
        std::env::set_var("WAYLAND_DISPLAY", &sway.display);
        Self { saved }
    }
}

impl Drop for WaylandEnv {
    fn drop(&mut self) {
        for (name, value) in self.saved.drain(..) {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

fn backend_monitor_level(status: &Value, backend: &str) -> Option<String> {
    status
        .pointer(&format!("/caps/backends/{backend}/monitor/level"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn origins(rows: &[Value]) -> Vec<String> {
    rows.iter()
        .map(|row| row["origin"].as_str().unwrap_or("<none>").to_string())
        .collect()
}

#[test]
#[allow(clippy::too_many_lines)]
fn t605_dual_backend_merge_xvfb_and_nested_sway() {
    // Drop order (reverse of declaration): daemon, env, sway, Xvfb.
    let xvfb = match Xvfb::start() {
        Ok(xvfb) => xvfb,
        Err(reason) => {
            eprintln!("SKIP ALL: {reason}");
            return;
        }
    };
    let sway = match Sway::start() {
        Ok(sway) => sway,
        Err(reason) => {
            eprintln!("SKIP ALL: {reason}");
            return;
        }
    };
    let _env = WaylandEnv::point_at(&sway);
    let daemon = Daemon::start(&xvfb, &sway).unwrap_or_else(|e| panic!("daemon: {e}"));
    let nonce = stamp() % 1_000_000_007;

    // 1. Both session backends run concurrently, each with a live monitor.
    let both_full = poll_until(Duration::from_secs(10), || {
        let status = daemon.call("status", json!({}));
        let x11 = backend_monitor_level(&status, "x11");
        let wayland = backend_monitor_level(&status, "wayland");
        (x11.as_deref() == Some("full") && wayland.as_deref() == Some("full")).then_some(())
    });
    assert!(
        both_full.is_some(),
        "x11 and wayland monitors must both be up; status: {}",
        daemon.call("status", json!({}))["caps"]["backends"]
    );

    // 2. Distinct copies on each side. X11 first: the X11 monitor must see
    //    it (it asks the owner for TARGETS) ...
    let x11_text = format!("t605 x11-only copy {nonce}");
    let x11_owner = X11Owner::copy(&xvfb.display, &x11_text).expect("x11 copy");
    let seen = poll_until(Duration::from_secs(5), || {
        (x11_owner.targets_requests() > 0).then_some(())
    });
    assert!(
        seen.is_some(),
        "the daemon's X11 monitor must observe the Xvfb copy (TARGETS request)"
    );

    let wl_text = format!("t605 wayland-only copy {nonce}");
    wayland_copy(&wl_text).expect("wayland copy");
    let wl_rows = daemon.wait_rows(&wl_text, Duration::from_secs(10));
    assert_eq!(
        wl_rows.len(),
        1,
        "the Wayland copy must be stored exactly once"
    );
    assert_eq!(origins(&wl_rows), ["wayland"], "Wayland clip origin tag");

    // ... but with WAYLAND_DISPLAY set the daemon treats every X11-watcher
    // capture as XWayland's mirror and drops it unread (skip_x11_capture).
    // The Wayland clip landing after it bounds the wait; add a poll cycle.
    thread::sleep(Duration::from_millis(1500));
    let x11_rows = daemon.rows_with_text(&x11_text);
    assert!(
        x11_rows.is_empty(),
        "on a Wayland session the X11 watcher's capture is dropped as the XWayland \
         mirror; got rows with origins {:?}",
        origins(&x11_rows)
    );
    eprintln!(
        "note: x11 owner served {} TARGETS and {} payload request(s) for the X11-only copy",
        x11_owner.targets_requests(),
        x11_owner.payload_reads()
    );
    drop(x11_owner);

    // 3. Identical content on both sides inside the merge window: one clip,
    //    tagged wayland, in either order.
    for (order, x11_first) in [("x11-then-wayland", true), ("wayland-then-x11", false)] {
        let same = format!("t605 same text {order} {nonce}");
        let started = Instant::now();
        let owner;
        if x11_first {
            owner = X11Owner::copy(&xvfb.display, &same).expect("x11 copy");
            wayland_copy(&same).expect("wayland copy");
        } else {
            wayland_copy(&same).expect("wayland copy");
            owner = X11Owner::copy(&xvfb.display, &same).expect("x11 copy");
        }
        assert!(
            started.elapsed() < MERGE_WINDOW,
            "{order}: both copies must fall inside the merge window"
        );
        let observed = poll_until(Duration::from_secs(5), || {
            (owner.targets_requests() > 0).then_some(())
        });
        assert!(
            observed.is_some(),
            "{order}: the X11 monitor must observe the X11 copy"
        );
        let first = daemon.wait_rows(&same, Duration::from_secs(10));
        assert!(!first.is_empty(), "{order}: the shared text must be stored");
        // Let any late duplicate from the other backend arrive.
        thread::sleep(Duration::from_millis(1500));
        let rows = daemon.rows_with_text(&same);
        assert_eq!(
            rows.len(),
            1,
            "{order}: identical content on both backends must store one clip; origins {:?}",
            origins(&rows)
        );
        assert_eq!(origins(&rows), ["wayland"], "{order}: merged clip origin");
        drop(owner);
    }
}
