//! Sway acceptance harness for the Wayland backend (frozen API, ruler-only).
//!
//! Every test takes a process-global serial lock (the binary must still be
//! run with `-- --test-threads=1 --nocapture`), starts its own headless
//! `sway` compositor, drives a fake peer over `wl-clipboard-rs` itself
//! (`copy` / `get_contents` / `clear`), and asserts the frozen backend API
//! (`WlMonitor`, `WlTransfer`, `WlPaste`, `probe_current`) end to end.
//!
//! SKIP contract: when the `sway` binary is missing (or no socket appears
//! within 10 s) each test prints `SKIP ALL: ...` and passes without
//! asserting anything else. A leg that needs an unavailable facility
//! (primary selection on a compositor that only offers wlr-data-control v1,
//! a WSLg-style `wayland-0` socket without data-control) prints a
//! `SKIP-note` for that leg only and still asserts the legs that can run.
//!
//! Every process spawned here is timeout-bounded: the `sway` socket wait is
//! 10 s, every monitor/poll loop carries an explicit `Instant` deadline,
//! sleeps only ever back a bounded poll (20-50 ms), and the `Drop` impl
//! reaps sway with a 5 s bounded wait plus stale-socket cleanup.
#![cfg(unix)]

use std::io::Read as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clipmonger_backend_wayland::{
    probe_current, PasteOutcome, WlMonitor, WlMonitorOpts, WlPaste, WlTransfer,
};
use clipmonger_platform::traits::{ClipboardMonitor, ClipboardTransfer, PasteEngine};
use clipmonger_platform::types::{CaptureOrigin, Error as PlatformError, PasteStrategy};
use wl_clipboard_rs::copy::{
    self as peer_copy, ClipboardType as CopyClipboardType, MimeSource, MimeType as CopyMimeType,
    Options as CopyOptions, Seat as CopySeat, ServeRequests, Source as CopySource,
};
use wl_clipboard_rs::paste::{
    self as peer_paste, ClipboardType as PasteClipboardType, MimeType as PasteMimeType,
    Seat as PasteSeat,
};

// ---------------------------------------------------------------------------
// Serial gate + Sway fixture
// ---------------------------------------------------------------------------

/// Process-global serial lock: `WAYLAND_DISPLAY` / `XDG_RUNTIME_DIR` are global.
static SERIAL: OnceLock<Mutex<()>> = OnceLock::new();

fn serial() -> MutexGuard<'static, ()> {
    SERIAL
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn have_binary(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(name).is_file()))
}

/// Owned headless sway compositor for one test. `Drop` kills and reaps it.
struct Sway {
    child: Child,
    runtime_dir: PathBuf,
    display: String,
    old_runtime_dir: Option<std::ffi::OsString>,
    old_display: Option<std::ffi::OsString>,
}

impl Sway {
    /// Spawn `sway -c <empty-config>` headless, wait for the Wayland socket
    /// (10 s budget). Also exports `XDG_RUNTIME_DIR` / `WAYLAND_DISPLAY`
    /// for the test process (serial callers only).
    fn start() -> Result<Self, String> {
        if !have_binary("sway") {
            return Err("sway binary not found on PATH".to_string());
        }
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let runtime_dir =
            std::env::temp_dir().join(format!("clipmonger-sway-{}-{}", std::process::id(), stamp));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&runtime_dir)
            .map_err(|e| format!("create runtime dir: {e}"))?;
        let config = runtime_dir.join("empty-config");
        std::fs::write(&config, "").map_err(|e| format!("write empty config: {e}"))?;
        let mut child = Command::new("sway")
            .arg("-c")
            .arg(&config)
            .env("XDG_RUNTIME_DIR", &runtime_dir)
            .env("WLR_BACKENDS", "headless")
            .env("WLR_RENDERER", "pixman")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("spawn sway: {e}"))?;
        // Scan $XDG_RUNTIME_DIR/wayland-* (<= 10 s).
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut display: Option<String> = None;
        while Instant::now() < deadline {
            if let Ok(entries) = std::fs::read_dir(&runtime_dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if name.starts_with("wayland-") && !name.ends_with(".lock") {
                        display = Some(name.into_owned());
                        break;
                    }
                }
            }
            if display.is_some() {
                break;
            }
            if let Ok(Some(status)) = child.try_wait() {
                let _ = std::fs::remove_dir_all(&runtime_dir);
                return Err(format!(
                    "sway exited early ({status}) before socket appeared"
                ));
            }
            thread::sleep(Duration::from_millis(100));
        }
        let Some(display) = display else {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_dir_all(&runtime_dir);
            return Err("no wayland-* socket appeared within 10 s".to_string());
        };
        let old_runtime_dir = std::env::var_os("XDG_RUNTIME_DIR");
        let old_display = std::env::var_os("WAYLAND_DISPLAY");
        std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir);
        std::env::set_var("WAYLAND_DISPLAY", &display);
        Ok(Self {
            child,
            runtime_dir,
            display,
            old_runtime_dir,
            old_display,
        })
    }

    fn display(&self) -> &str {
        &self.display
    }
}

impl Drop for Sway {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let deadline = Instant::now() + Duration::from_secs(5);
        while let Ok(None) = self.child.try_wait() {
            if Instant::now() >= deadline {
                eprintln!("warn: sway did not exit within 5 s of kill; leaving to init");
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        match self.old_runtime_dir.take() {
            Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
            None => std::env::remove_var("XDG_RUNTIME_DIR"),
        }
        match self.old_display.take() {
            Some(v) => std::env::set_var("WAYLAND_DISPLAY", v),
            None => std::env::remove_var("WAYLAND_DISPLAY"),
        }
        let _ = std::fs::remove_dir_all(&self.runtime_dir);
    }
}

/// Take the serial lock and start a compositor. `None` means SKIP-and-pass.
fn fixture(test: &str) -> Option<(MutexGuard<'static, ()>, Sway)> {
    let guard = serial();
    match Sway::start() {
        Ok(sway) => Some((guard, sway)),
        Err(reason) => {
            eprintln!("SKIP ALL ({test}): {reason}");
            None
        }
    }
}

/// Bounded poll helper: run `f` until it returns `Some`, else `None` after
/// `budget`. Sleeps 20 ms between attempts (backs a bounded poll only).
fn poll_until<T>(budget: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(value) = f() {
            return Some(value);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn drain_monitor(
    monitor: &mut WlMonitor,
    budget: Duration,
) -> Vec<clipmonger_platform::types::CaptureEvent> {
    let mut out = Vec::new();
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        match monitor.poll(Duration::from_millis(100)) {
            Ok(events) => out.extend(events),
            Err(e) => {
                eprintln!("note: monitor poll failed: {e}");
                break;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Fake peer over wl-clipboard-rs
// ---------------------------------------------------------------------------

fn peer_copy_multi(
    payloads: &[(&str, &[u8])],
    sensitive: bool,
    primary: bool,
) -> Result<(), String> {
    let mut options = CopyOptions::new();
    options
        .clipboard(if primary {
            CopyClipboardType::Primary
        } else {
            CopyClipboardType::Regular
        })
        .seat(CopySeat::All)
        .sensitive(sensitive);
    let sources = payloads
        .iter()
        .map(|(mime, bytes)| MimeSource {
            source: CopySource::Bytes(bytes.to_vec().into_boxed_slice()),
            mime_type: CopyMimeType::Specific((*mime).to_string()),
        })
        .collect();
    options
        .copy_multi(sources)
        .map_err(|e| format!("peer copy_multi: {e}"))
}

/// Peer copy that serves at most `n` paste requests, then the serving
/// process exits (owner-gone simulation for `t604_kill_owner_persistence`).
fn peer_copy_oneshot(payloads: &[(&str, &[u8])], n: usize) -> Result<(), String> {
    let mut options = CopyOptions::new();
    options
        .clipboard(CopyClipboardType::Regular)
        .seat(CopySeat::All)
        .serve_requests(ServeRequests::Only(n));
    let sources = payloads
        .iter()
        .map(|(mime, bytes)| MimeSource {
            source: CopySource::Bytes(bytes.to_vec().into_boxed_slice()),
            mime_type: CopyMimeType::Specific((*mime).to_string()),
        })
        .collect();
    options
        .copy_multi(sources)
        .map_err(|e| format!("peer copy_multi oneshot: {e}"))
}

fn peer_read(mime: &str, primary: bool) -> Result<Vec<u8>, String> {
    let clipboard = if primary {
        PasteClipboardType::Primary
    } else {
        PasteClipboardType::Regular
    };
    let (mut reader, _mime) = peer_paste::get_contents(
        clipboard,
        PasteSeat::Unspecified,
        PasteMimeType::Specific(mime),
    )
    .map_err(|e| format!("peer get_contents({mime}): {e}"))?;
    let mut buf = Vec::new();
    reader
        .read_to_end(&mut buf)
        .map_err(|e| format!("peer read body({mime}): {e}"))?;
    Ok(buf)
}

fn peer_clear() -> Result<(), String> {
    peer_copy::clear(CopyClipboardType::Regular, CopySeat::All)
        .map_err(|e| format!("peer clear: {e}"))
}

fn monitor_opts(capture_primary: bool) -> WlMonitorOpts {
    WlMonitorOpts {
        primary: capture_primary,
        debounce_ms: 500,
        min_len: 3,
        capture_primary,
    }
}

// ---------------------------------------------------------------------------
// T-604: capture + transfer readback
// ---------------------------------------------------------------------------

#[test]
fn t604_capture_readback() {
    let Some((_guard, _sway)) = fixture("t604_capture_readback") else {
        return;
    };
    // Registry must show a data-control protocol under sway.
    let probe = probe_current();
    assert!(
        probe.protocol != clipmonger_backend_wayland::probe::Protocol::None,
        "expected data-control under sway, got {probe:?}"
    );
    let mut monitor =
        WlMonitor::connect(monitor_opts(false)).expect("WlMonitor::connect under sway");
    let transfer = WlTransfer::connect().expect("WlTransfer::connect under sway");

    let text = b"clipmonger sway readback";
    let html = b"<b>clipmonger sway readback</b>";
    peer_copy_multi(&[("text/plain", text), ("text/html", html)], false, false)
        .expect("peer copy text+html");

    let event = poll_until(Duration::from_secs(5), || {
        monitor
            .poll(Duration::from_millis(200))
            .ok()
            .and_then(|events| events.into_iter().next())
    })
    .expect("monitor event within 5 s");
    assert_eq!(event.origin, CaptureOrigin::Clipboard);
    assert!(!event.sensitive, "plain copy must not be sensitive");
    let mimes: Vec<&str> = event.offers.iter().map(|o| o.mime.as_str()).collect();
    assert!(
        mimes.contains(&"text/plain"),
        "offers {mimes:?} miss text/plain"
    );
    assert!(
        mimes.contains(&"text/html"),
        "offers {mimes:?} miss text/html"
    );

    assert_eq!(
        transfer
            .read("text/plain", 1 << 20)
            .expect("transfer read text"),
        text
    );
    assert_eq!(
        transfer
            .read("text/html", 1 << 20)
            .expect("transfer read html"),
        html
    );
}

// ---------------------------------------------------------------------------
// T-604: password-hint copy is flagged sensitive, never stored
// ---------------------------------------------------------------------------

#[test]
fn t604_password_hint_skip() {
    let Some((_guard, _sway)) = fixture("t604_password_hint_skip") else {
        return;
    };
    let mut monitor =
        WlMonitor::connect(monitor_opts(false)).expect("WlMonitor::connect under sway");

    peer_copy_multi(&[("text/plain", b"super-secret-password")], true, false)
        .expect("peer sensitive copy");

    let event = poll_until(Duration::from_secs(5), || {
        monitor
            .poll(Duration::from_millis(200))
            .ok()
            .and_then(|events| events.into_iter().next())
    })
    .expect("monitor event for sensitive copy within 5 s");
    // The daemon would skip storage on this flag; the harness asserts the
    // flag itself, not storage.
    assert!(
        event.sensitive,
        "password-hint copy must set sensitive=true"
    );
    assert_eq!(event.origin, CaptureOrigin::Clipboard);
}

// ---------------------------------------------------------------------------
// T-604: clear produces no capture event
// ---------------------------------------------------------------------------

#[test]
fn t604_clear_no_event() {
    let Some((_guard, _sway)) = fixture("t604_clear_no_event") else {
        return;
    };
    let mut monitor =
        WlMonitor::connect(monitor_opts(false)).expect("WlMonitor::connect under sway");
    // Seed the clipboard so the clear is a real transition.
    peer_copy_multi(&[("text/plain", b"seed before clear")], false, false).expect("peer seed copy");
    let _ = poll_until(Duration::from_secs(5), || {
        monitor
            .poll(Duration::from_millis(200))
            .ok()
            .and_then(|events| events.into_iter().next())
    });

    peer_clear().expect("peer clear");
    let events = drain_monitor(&mut monitor, Duration::from_millis(1500));
    assert!(
        events.is_empty(),
        "Cleared must emit NO CaptureEvent, got {events:?}"
    );
}

// ---------------------------------------------------------------------------
// T-604: owner exit; transfer write restores readability
// ---------------------------------------------------------------------------

#[test]
fn t604_kill_owner_persistence() {
    // NOTE (spec): clips persist in the daemon store in production; that leg
    // lives in the daemon smoke test. This harness leg only proves the
    // transfer-level half: once the peer owner is gone, `get_contents`
    // errors (or reads empty), and a `WlTransfer::write` of the same payload
    // restores readability.
    let Some((_guard, _sway)) = fixture("t604_kill_owner_persistence") else {
        return;
    };
    let transfer = WlTransfer::connect().expect("WlTransfer::connect under sway");
    let text = b"owner-exit payload";

    peer_copy_oneshot(&[("text/plain", text)], 1).expect("peer oneshot copy");
    // Consume the single served request (whoever serves it: peer read here,
    // or the monitor's own fetch); afterwards the owner is gone.
    let _ = peer_read("text/plain", false);
    let gone = poll_until(Duration::from_secs(3), || {
        match peer_read("text/plain", false) {
            Err(_) => Some(true),
            Ok(body) if body.is_empty() => Some(true),
            Ok(_) => None,
        }
    });
    assert!(
        gone.unwrap_or(false),
        "after peer exit, get_contents must error or read empty"
    );

    let payload = vec![("text/plain".to_string(), text.to_vec())];
    transfer
        .write(&payload)
        .expect("transfer write restores payload");
    assert_eq!(
        transfer
            .read("text/plain", 1 << 20)
            .expect("read after write"),
        text
    );
    // `unchanged_indicator` is a synthesized `offer:<n>` counter that must
    // change on every write.
    let first = transfer.unchanged_indicator();
    transfer.write(&payload).expect("second write");
    let second = transfer.unchanged_indicator();
    assert!(
        first.is_some() && second.is_some(),
        "offer counter must exist"
    );
    assert_ne!(first, second, "offer counter must change per write");
}

// ---------------------------------------------------------------------------
// T-609: primary-selection policy + debounce
// ---------------------------------------------------------------------------

#[test]
fn t609_primary_policy() {
    let Some((_guard, _sway)) = fixture("t609_primary_policy") else {
        return;
    };
    // Policy off: PRIMARY copies never surface.
    let mut monitor =
        WlMonitor::connect(monitor_opts(false)).expect("WlMonitor::connect under sway");
    if let Err(e) = peer_copy_multi(&[("text/plain", b"primary when disabled")], false, true) {
        eprintln!("SKIP-note (t609_primary_policy): primary copy unsupported here: {e}");
        return;
    }
    let events = drain_monitor(&mut monitor, Duration::from_millis(1500));
    assert!(
        events.is_empty(),
        "capture_primary=false must suppress primary, got {events:?}"
    );
    drop(monitor);

    // Policy on (debounce 500 ms / min len 3): a burst of 5 primary copies
    // coalesces to exactly one event; a 2-byte copy emits nothing.
    let mut monitor =
        WlMonitor::connect(monitor_opts(true)).expect("WlMonitor::connect primary on");
    for i in 0..5 {
        peer_copy_multi(
            &[(
                "text/plain",
                format!("primary burst {i} padding").as_bytes(),
            )],
            false,
            true,
        )
        .expect("peer primary burst copy");
        thread::sleep(Duration::from_millis(50));
    }
    let mut burst_events = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline && burst_events.is_empty() {
        burst_events.extend(
            monitor
                .poll(Duration::from_millis(200))
                .expect("monitor poll during burst"),
        );
    }
    // Drain one more quiet window to catch duplicates.
    burst_events.extend(drain_monitor(&mut monitor, Duration::from_millis(1500)));
    assert_eq!(
        burst_events.len(),
        1,
        "burst of 5 must coalesce to exactly 1 event, got {burst_events:?}"
    );
    assert_eq!(burst_events[0].origin, CaptureOrigin::Primary);

    peer_copy_multi(&[("text/plain", b"xy")], false, true).expect("peer short primary copy");
    let short_events = drain_monitor(&mut monitor, Duration::from_millis(1500));
    assert!(
        short_events.is_empty(),
        "len-2 primary copy must be dropped, got {short_events:?}"
    );
}

// ---------------------------------------------------------------------------
// T-608: paste fallback live
// ---------------------------------------------------------------------------

#[test]
fn t608_fallback_table_live() {
    let Some((_guard, sway)) = fixture("t608_fallback_table_live") else {
        return;
    };
    eprintln!("note: sway on {}", sway.display());
    let paste = WlPaste::new(true);
    let payload = vec![("text/plain".to_string(), b"paste me via sway".to_vec())];
    let outcome: PasteOutcome = paste
        .paste_payloads(&payload)
        .expect("paste_payloads under sway");
    assert_eq!(outcome.strategy, PasteStrategy::WriteAndToast);
    assert_eq!(outcome.toast, Some("paste.press_ctrl_v"));
    // Trait-level conformance: WlPaste implements PasteEngine (`paste_to`
    // stays Unsupported per the frozen API, mirroring sibling backends).
    // `ClipRef` needs the `uuid` crate to construct, so the harness pins
    // the trait bound plus strategy/outcome consistency instead.
    #[allow(clippy::items_after_statements)]
    fn assert_paste_engine<T: PasteEngine>() {}
    assert_paste_engine::<WlPaste>();
    assert_eq!(paste.strategy(), outcome.strategy);
    // The fallback write must land on the clipboard for the peer to read.
    let body = peer_read("text/plain", false).expect("peer reads pasted payload");
    assert_eq!(body, b"paste me via sway");

    // WSLg leg (no data-control): only runs when the ambient session looks
    // like WSLg; otherwise SKIP-note. This leg never touches the sway
    // fixture env above beyond holding the serial lock.
    match std::env::var("WAYLAND_DISPLAY") {
        Ok(v) if v == "wayland-0" => {
            eprintln!("note (t608 WSLg leg): ambient WAYLAND_DISPLAY=wayland-0; no sway override");
        }
        _ => {
            eprintln!("SKIP-note (t608 WSLg leg): no ambient WAYLAND_DISPLAY=wayland-0 here");
        }
    }
}

#[test]
fn t608_fallback_wslg_no_data_control() {
    // Fixture-free leg: expects a WSLg-style `wayland-0` socket WITHOUT
    // data-control. Any other machine prints SKIP-note and passes.
    let _guard = serial();
    let ambient = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    if ambient != "wayland-0" {
        eprintln!("SKIP-note (t608 WSLg leg): WAYLAND_DISPLAY={ambient:?}, need wayland-0");
        return;
    }
    if probe_current().protocol != clipmonger_backend_wayland::probe::Protocol::None {
        eprintln!("SKIP-note (t608 WSLg leg): wayland-0 offers data-control here; leg needs none");
        return;
    }
    let paste = WlPaste::new(false);
    let payload = vec![("text/plain".to_string(), b"wslg paste".to_vec())];
    match paste.paste_payloads(&payload) {
        Err(PlatformError::Unavailable(_)) => {}
        other => panic!("WSLg paste must fail Unavailable-ish, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Suite-end sway cleanup proof (runs last by name)
// ---------------------------------------------------------------------------

#[test]
fn zzz_sway_cleanup_proof() {
    let _guard = serial();
    // Every fixture `Drop` kills + reaps its compositor, so no sway may be
    // alive when the suite ends. `pgrep` itself is bounded by `timeout`.
    let status = Command::new("timeout")
        .arg("10")
        .arg("pgrep")
        .arg("-x")
        .arg("sway")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(s) if s.code() == Some(1) => {}
        Ok(s) => panic!("sway cleanup proof failed: pgrep exit {s} (sway still running?)"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("SKIP-note (zzz): `timeout`/`pgrep` missing; cannot prove cleanup");
        }
        Err(e) => panic!("sway cleanup proof failed to run pgrep: {e}"),
    }
}
