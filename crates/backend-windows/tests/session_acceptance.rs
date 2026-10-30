//! WINBACK interop acceptance harness (ruler only).
//!
//! Windows-session tests for the frozen `clipmonger_backend_windows` API
//! (see `../../harness-spec.md`). This file is `#![cfg(windows)]` for the
//! whole test crate, so it compiles to an empty (passing) crate on Linux
//! and only runs on a real Windows session.
//!
//! SKIP contract: every test first probes clipboard/OS availability. When
//! the session cannot provide clipboard APIs (RDP-restricted, headless CI,
//! locked-down policy), the test prints a single `SKIP <test-id>: <reason>`
//! line and returns success. Count skips with:
//! `cargo test ... -- --nocapture | grep -c "^SKIP "`
//! A SKIP is never a pass claim for a real assert: any reachable assert
//! runs for real, and any reachable failure fails the test.
//!
//! Safety: no input synthesis, no foreground-window theft beyond the own
//! process, no sleeps except monitor/hotkey poll timeouts. `paste_to` is
//! deliberately NOT invoked (it would inject keystrokes into whatever window
//! has focus); revalidation/paste paths stay manual interop items.
#![cfg(windows)]

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clipmonger_backend_windows::{
    caps, current_process_integrity, target_integrity, Integrity, WinAutostart, WinHotkeys,
    WinMonitor, WinPaste, WinTransfer,
};
use clipmonger_platform::paste_policy::guarded_restore_decision;
use clipmonger_platform::traits::{
    AutostartManager, ClipboardMonitor, ClipboardTransfer, HotkeyManager, PasteEngine,
};
use clipmonger_platform::types::Error as PlatformError;
use clipmonger_platform::types::{HotkeyId, PasteStrategy, PlatformHandle, SubscribeKind};

/// True when `e` means "session cannot provide this API" (SKIP, not fail).
fn is_unavailable(e: &PlatformError) -> bool {
    matches!(e, PlatformError::Unavailable(_))
}

/// Unwrap a backend `Result`, converting session-unavailability into a
/// printed SKIP + early pass, and any other error into a hard failure.
macro_rules! guard {
    ($expr:expr, $test:expr) => {
        match $expr {
            Ok(v) => v,
            Err(e) => {
                if is_unavailable(&e) {
                    println!("SKIP {}: session API unavailable: {}", $test, e);
                    return;
                }
                panic!("{}: backend error (not a SKIP): {}", $test, e);
            }
        }
    };
}

/// Unique payload marker so monitor/write/read tests never collide with
/// real user clipboard content.
fn unique_marker(test: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("clipmonger-winback-{test}-{}-{}", std::process::id(), nanos)
}

fn assert_conflict(e: &PlatformError, test: &str) {
    let msg = e.to_string();
    match e {
        PlatformError::Denied(_) | PlatformError::Unsupported(_) => {}
        other => panic!("{test}: expected Denied/Unsupported conflict, got: {other}"),
    }
    assert!(
        msg.to_ascii_lowercase().contains("already"),
        "{test}: conflict error must mention \"already\", got: {msg}"
    );
}

/// T-405 transfer: multi-format atomic write round-trips; `formats()`
/// advertises canonical names; the unchanged indicator moves on write.
#[test]
fn t405_transfer_roundtrip() {
    const T: &str = "t405_transfer_roundtrip";
    let t = WinTransfer::new();
    let marker = unique_marker("t405");
    let text = marker.clone().into_bytes();
    let html = format!("<p>{marker}</p>").into_bytes();

    guard!(
        t.write(&[
            ("text/plain".to_string(), text.clone()),
            ("text/html".to_string(), html.clone()),
        ]),
        T
    );

    let formats = guard!(t.formats(), T);
    assert!(
        formats.iter().any(|f| f == "text/plain"),
        "{T}: formats() must contain canonical \"text/plain\", got: {formats:?}"
    );
    assert!(
        formats
            .iter()
            .any(|f| f.to_ascii_lowercase().contains("html")),
        "{T}: formats() must advertise an html format, got: {formats:?}"
    );

    let back_text = guard!(t.read("text/plain", 4 * 1024 * 1024), T);
    assert_eq!(back_text, text, "{T}: text/plain did not round-trip");
    let back_html = guard!(t.read("text/html", 4 * 1024 * 1024), T);
    assert_eq!(back_html, html, "{T}: text/html did not round-trip");

    let before = t.unchanged_indicator();
    guard!(
        t.write(&[("text/plain".to_string(), format!("{marker}-2").into_bytes())]),
        T
    );
    let after = t.unchanged_indicator();
    match (before, after) {
        (Some(a), Some(b)) => assert_ne!(a, b, "{T}: unchanged_indicator must move after a write"),
        _ => println!("SKIP {T}: unchanged_indicator not provided by session; move not asserted"),
    }
}

/// T-402 session opt-out: a raw `CanIncludeInClipboardHistory=0` format
/// write is not expressible through `ClipboardTransfer::write(PayloadMap)`.
/// Opt-out itself is covered by the pure matrix (wincore optout unit tests).
#[test]
fn t402_session_optout() {
    println!(
        "SKIP t402_session_optout: raw CanIncludeInClipboardHistory=0 format write \
         is not expressible via ClipboardTransfer::write; opt-out covered by pure matrix (T-402)"
    );
}

/// T-406 hotkeys: register/unregister round-trip plus duplicate-chord
/// conflict behavior. No input is synthesized, so firing is NOT tested
/// here (manual interop item); only registration semantics are asserted.
#[test]
fn t406_hotkey_fire() {
    const T: &str = "t406_hotkey_fire";
    let mut hk = WinHotkeys::new();

    match hk.register(HotkeyId(41), "ctrl+alt+F9") {
        Ok(()) => {}
        Err(e) => {
            if is_unavailable(&e) {
                println!("SKIP {T}: session hotkey API unavailable: {e}");
                return;
            }
            panic!("{T}: register ctrl+alt+F9 failed: {e}");
        }
    }

    // Conflicting register of the same chord must fail loudly.
    match hk.register(HotkeyId(42), "ctrl+alt+F9") {
        Ok(()) => {
            let _ = hk.unregister(HotkeyId(42));
            let mut hk2 = WinHotkeys::new();
            match hk2.register(HotkeyId(43), "ctrl+alt+F9") {
                Ok(()) => {
                    let _ = hk2.unregister(HotkeyId(43));
                    panic!("{T}: expected conflicting register to fail with \"already\"");
                }
                Err(e) => {
                    assert_conflict(&e, T);
                    let _ = hk2.unregister(HotkeyId(43));
                }
            }
        }
        Err(e) => assert_conflict(&e, T),
    }

    // Manager must stay usable after the conflict.
    guard!(hk.register(HotkeyId(44), "ctrl+alt+F8"), T);
    guard!(hk.poll(Duration::from_millis(100)), T);
    guard!(hk.unregister(HotkeyId(44)), T);
    guard!(hk.unregister(HotkeyId(41)), T);
    println!(
        "SKIP-note {T}: hotkey firing not synthesized in harness; registration semantics only"
    );
}

/// T-411 guarded restore: a user copy during the dwell must NOT restore;
/// an unchanged slot MUST restore. Decided through the real platform
/// `guarded_restore_decision` predicate fed with real transfer state.
#[test]
fn t411_guarded_restore() {
    const T: &str = "t411_guarded_restore";
    let t = WinTransfer::new();

    // Best-effort save of the user's current text for end-of-test restore.
    let saved = match t.read("text/plain", 4 * 1024 * 1024) {
        Ok(b) => Some(b),
        Err(e) => {
            if is_unavailable(&e) {
                println!("SKIP {T}: session clipboard unavailable: {e}");
                return;
            }
            None
        }
    };

    let temp: Vec<u8> = unique_marker("t411-temp").into_bytes();
    let user: Vec<u8> = unique_marker("t411-user").into_bytes();

    // Slot holds our temp value.
    guard!(t.write(&[("text/plain".to_string(), temp.clone())]), T);
    let ind_temp = t.unchanged_indicator();
    let read_temp = guard!(t.read("text/plain", 4 * 1024 * 1024), T);
    assert_eq!(read_temp, temp, "{T}: temp write did not stick");

    // Simulate a user copy racing the dwell.
    guard!(t.write(&[("text/plain".to_string(), user.clone())]), T);
    let ind_user = t.unchanged_indicator();
    let read_user = guard!(t.read("text/plain", 4 * 1024 * 1024), T);
    assert_eq!(read_user, user, "{T}: simulated user copy did not stick");

    // Changed case: indicator and content both moved -> must NOT restore.
    let indicator_same = ind_temp == ind_user;
    let hash_same = read_temp == read_user;
    assert!(
        !indicator_same || !hash_same,
        "{T}: setup broken: user copy did not change observable state"
    );
    assert!(
        !guarded_restore_decision(indicator_same, hash_same),
        "{T}: changed slot must NOT restore"
    );

    // Unchanged case: re-read with no intervening write -> MUST restore.
    let ind_now = t.unchanged_indicator();
    let read_now = guard!(t.read("text/plain", 4 * 1024 * 1024), T);
    let indicator_same_now = ind_user == ind_now;
    let hash_same_now = read_user == read_now;
    assert!(
        indicator_same_now,
        "{T}: setup broken: indicator moved with no intervening write"
    );
    assert!(
        hash_same_now,
        "{T}: setup broken: content moved with no write"
    );
    assert!(
        guarded_restore_decision(indicator_same_now, hash_same_now),
        "{T}: unchanged slot MUST restore"
    );

    // Best-effort restore of the user's original text.
    if let Some(orig) = saved {
        let _ = t.write(&[("text/plain".to_string(), orig)]);
    }
}

/// T-408 integrity: own-process capture must never report High integrity
/// (which would refuse paste per seg-04 S4). Strategy must be UnicodeInject.
#[test]
fn t408_integrity_medium() {
    const T: &str = "t408_integrity_medium";
    let paste = WinPaste::new(1024 * 1024);
    assert_eq!(
        paste.strategy(),
        PasteStrategy::UnicodeInject,
        "{T}: pilot strategy must be UnicodeInject"
    );

    let own = current_process_integrity();
    if own == Integrity::High {
        println!("SKIP {T}: test process is elevated (High); cannot assert Medium");
        return;
    }
    assert_eq!(
        own,
        Integrity::Medium,
        "{T}: own process integrity must be Medium, got: {own:?}"
    );

    let target = match paste.capture_target() {
        Ok(t) => t,
        Err(e) => {
            if is_unavailable(&e) {
                println!("SKIP {T}: no capturable target in session: {e}");
                return;
            }
            panic!("{T}: capture_target failed: {e}");
        }
    };
    assert!(
        matches!(target.handle, PlatformHandle::WinHwnd(_)),
        "{T}: capture_target must yield a WinHwnd handle, got: {:?}",
        target.handle
    );
    let integrity = target_integrity(&target.handle);
    assert_ne!(
        integrity,
        Integrity::High,
        "{T}: own-process target must not be High integrity"
    );
}

/// T-405 monitor: subscribe, write a unique text, and observe a matching
/// `CaptureEvent` within 5 s. The sensitive=true path needs raw formats and
/// is NOT testable here (see SKIP note).
#[test]
fn t405_monitor_capture() {
    const T: &str = "t405_monitor_capture";
    let mut mon = match WinMonitor::new() {
        Ok(m) => m,
        Err(e) => {
            println!("SKIP {T}: monitor unavailable in session: {e}");
            return;
        }
    };
    guard!(mon.subscribe(&[SubscribeKind::Clipboard]), T);

    let t = WinTransfer::new();
    let marker = unique_marker("t405mon");
    guard!(
        t.write(&[("text/plain".to_string(), marker.clone().into_bytes())]),
        T
    );

    let mut found = false;
    let mut saw_sensitive_only = false;
    for _ in 0..10 {
        let events = guard!(mon.poll(Duration::from_millis(500)), T);
        for ev in &events {
            if ev.offers.iter().any(|o| o.mime == "text/plain") && !ev.sensitive {
                found = true;
                break;
            }
            if ev.sensitive {
                saw_sensitive_only = true;
            }
        }
        if found {
            break;
        }
    }
    if saw_sensitive_only && !found {
        println!("SKIP-note {T}: only sensitive events observed; text offer not asserted");
    }
    assert!(
        found,
        "{T}: no CaptureEvent with text/plain offer within 5 s"
    );
    println!("SKIP-note {T}: sensitive=true path needs raw formats; not testable here");
}

/// Backend caps smoke: `caps()` must report the healthy Windows matrix
/// (Full monitor/transfer/hotkey, UnicodeInject paste, autostart on,
/// no primary selection).
#[test]
fn t_caps_backend() {
    const T: &str = "t_caps_backend";
    let c = caps();
    assert_eq!(
        c.monitor,
        clipmonger_platform::types::Level::Full,
        "{T}: monitor"
    );
    assert_eq!(
        c.transfer,
        clipmonger_platform::types::Level::Full,
        "{T}: transfer"
    );
    assert_eq!(
        c.hotkey,
        clipmonger_platform::types::Level::Full,
        "{T}: hotkey"
    );
    assert_eq!(
        c.paste.level,
        clipmonger_platform::types::Level::Full,
        "{T}: paste level"
    );
    assert_eq!(
        c.paste.strategy,
        PasteStrategy::UnicodeInject,
        "{T}: paste strategy"
    );
    assert!(c.autostart, "{T}: autostart must be supported");
    assert!(!c.primary_selection, "{T}: no primary selection on Windows");
}

/// Autostart portable smoke: portable `enable()` is an Ok no-op and
/// `is_enabled()` stays false. Non-portable enable is NOT called here
/// (it would write the HKCU Run value; manual interop item).
#[test]
fn t_autostart_portable() {
    const T: &str = "t_autostart_portable";
    let exe = PathBuf::from("C:\\clipmonger\\clipmonger-daemon.exe");
    let mut m = WinAutostart::new(exe, true);
    match m.enable() {
        Ok(()) => {}
        Err(e) => {
            if is_unavailable(&e) {
                println!("SKIP {T}: autostart unavailable in session: {e}");
                return;
            }
            panic!("{T}: portable enable must be an Ok no-op: {e}");
        }
    }
    let enabled = guard!(m.is_enabled(), T);
    assert!(!enabled, "{T}: portable is_enabled must be false");
}

/// Opaque 4x3 sample image and its PNG (opaque: `CF_DIB` readers ignore
/// alpha).
fn sample_png() -> (image::RgbaImage, Vec<u8>) {
    let img = image::RgbaImage::from_fn(4, 3, |x, y| {
        image::Rgba([
            u8::try_from(x * 60).unwrap_or(0),
            u8::try_from(y * 100).unwrap_or(0),
            170,
            255,
        ])
    });
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(img.clone())
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("encode sample png");
    (img, png)
}

fn decode_rgba(png: &[u8]) -> image::RgbaImage {
    image::load_from_memory(png).expect("decode png").to_rgba8()
}

/// Raw bytes of one clipboard format, as other apps read them.
fn raw_format(id: u32) -> Option<Vec<u8>> {
    let _clip = clipboard_win::Clipboard::new_attempts(10).ok()?;
    let mut out = Vec::new();
    clipboard_win::raw::get_vec(id, &mut out).ok()?;
    Some(out)
}

/// Best-effort save of the user's current text for end-of-test restore;
/// `Err` means the session has no clipboard (SKIP).
fn save_user_text(t: &WinTransfer) -> Result<Option<Vec<u8>>, PlatformError> {
    match t.read("text/plain", 4 * 1024 * 1024) {
        Ok(b) => Ok(Some(b)),
        Err(e) if is_unavailable(&e) && clipboard_win::Clipboard::new_attempts(10).is_err() => {
            Err(e)
        }
        Err(_) => Ok(None),
    }
}

/// T-405 image: `image/png` publishes "PNG" plus a `CF_DIB` other apps can
/// read; the `CF_DIBV5` Windows synthesizes decodes to the same pixels; a
/// bitmap-only clipboard reads back as the same image.
#[test]
fn t405_image_roundtrip() {
    const T: &str = "t405_image_roundtrip";
    let t = WinTransfer::new();
    let saved = guard!(save_user_text(&t), T);
    let (img, png) = sample_png();

    guard!(t.write(&[("image/png".to_string(), png.clone())]), T);
    let formats = guard!(t.formats(), T);
    assert!(
        formats.iter().any(|f| f == "image/png"),
        "{T}: formats() must advertise image/png, got: {formats:?}"
    );
    let back = t
        .read("image/png", 16 * 1024 * 1024)
        .unwrap_or_else(|e| panic!("{T}: image/png read after write failed: {e}"));
    assert_eq!(back, png, "{T}: PNG format did not round-trip byte-exact");
    for (id, name) in [(8, "CF_DIB"), (17, "CF_DIBV5")] {
        let dib = raw_format(id).unwrap_or_else(|| panic!("{T}: {name} missing after write"));
        let decoded = clipmonger_backend_windows::dib::dib_to_png(&dib)
            .unwrap_or_else(|e| panic!("{T}: {name} decode failed: {e}"));
        assert_eq!(decode_rgba(&decoded), img, "{T}: {name} pixels differ");
    }

    // Bitmap-only clipboard, as from apps without PNG support.
    let dib = clipmonger_backend_windows::dib::png_to_dib(&png).expect("png_to_dib");
    {
        let _clip = clipboard_win::Clipboard::new_attempts(10).expect("open clipboard");
        clipboard_win::raw::empty().expect("empty clipboard");
        clipboard_win::raw::set_without_clear(8, &dib).expect("set CF_DIB");
    }
    let from_dib = t
        .read("image/png", 16 * 1024 * 1024)
        .unwrap_or_else(|e| panic!("{T}: bitmap-only read failed: {e}"));
    assert_eq!(
        decode_rgba(&from_dib),
        img,
        "{T}: bitmap-only clipboard did not read back as the same image"
    );

    if let Some(orig) = saved {
        let _ = t.write(&[("text/plain".to_string(), orig)]);
    }
}

/// T-405 files: `text/uri-list` publishes `CF_HDROP` (names with spaces
/// and non-ASCII letters survive) plus a copy drop effect, and reads back
/// as the same list.
#[test]
fn t405_file_list_roundtrip() {
    const T: &str = "t405_file_list_roundtrip";
    let t = WinTransfer::new();
    let saved = guard!(save_user_text(&t), T);
    let dir = std::env::temp_dir().join(unique_marker("t405-files"));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let paths: Vec<String> = ["a b.txt", "léto (1).txt"]
        .iter()
        .map(|name| {
            let path = dir.join(name);
            std::fs::write(&path, b"x").expect("temp file");
            path.to_string_lossy().into_owned()
        })
        .collect();
    let list = clipmonger_backend_windows::uri::paths_to_uri_list(&paths).into_bytes();

    guard!(t.write(&[("text/uri-list".to_string(), list.clone())]), T);
    let formats = guard!(t.formats(), T);
    assert!(
        formats.iter().any(|f| f == "text/uri-list"),
        "{T}: formats() must advertise text/uri-list, got: {formats:?}"
    );
    let mut shell_paths = Vec::new();
    let mut drop_effect = Vec::new();
    {
        let _clip = clipboard_win::Clipboard::new_attempts(10).expect("open clipboard");
        clipboard_win::raw::get_file_list(&mut shell_paths).expect("read CF_HDROP");
        let id = clipboard_win::raw::register_format("Preferred DropEffect").expect("format");
        clipboard_win::raw::get_vec(id.get(), &mut drop_effect).expect("read drop effect");
    }
    assert_eq!(shell_paths, paths, "{T}: CF_HDROP paths differ");
    // A lone CF_HDROP crashes FreeRDP-based clipboard sync (RustDesk 1.4.9).
    assert_eq!(
        drop_effect.get(..4),
        Some(&1_u32.to_le_bytes()[..]),
        "{T}: file list must carry DROPEFFECT_COPY"
    );
    let back = guard!(t.read("text/uri-list", 4 * 1024 * 1024), T);
    assert_eq!(back, list, "{T}: text/uri-list did not round-trip");

    let _ = std::fs::remove_dir_all(&dir);
    if let Some(orig) = saved {
        let _ = t.write(&[("text/plain".to_string(), orig)]);
    }
}

/// T-411 restore fidelity: the guarded restore puts a saved image back
/// with its opt-out marker; `restore = false` leaves the paste in place.
#[test]
fn t411_swap_restores_formats() {
    use clipmonger_backend_windows::optout::EXCLUDE_FORMAT;
    use clipmonger_backend_windows::paste::ClipboardSwap;
    const T: &str = "t411_swap_restores_formats";
    let t = WinTransfer::new();
    let saved = guard!(save_user_text(&t), T);
    let (img, png) = sample_png();
    let paste = vec![(
        "text/plain".to_string(),
        unique_marker("t411-paste").into_bytes(),
    )];

    guard!(
        t.write(&[
            ("image/png".to_string(), png),
            (format!("x-os/{EXCLUDE_FORMAT}"), vec![0; 4]),
        ]),
        T
    );
    let swap = guard!(ClipboardSwap::begin(&paste, true), T);
    let during = guard!(t.read("text/plain", 4 * 1024 * 1024), T);
    assert_eq!(during, paste[0].1, "{T}: swap did not write the paste");
    let seq_swapped = t.unchanged_indicator();
    swap.finish(0, true);
    let back = t.read("image/png", 16 * 1024 * 1024).unwrap_or_else(|e| {
        panic!(
            "{T}: restore lost the image ({e}); seq {seq_swapped:?} -> {:?}, offers {:?}",
            t.unchanged_indicator(),
            t.formats()
        )
    });
    assert_eq!(decode_rgba(&back), img, "{T}: restore lost the image");
    let marker_kept = {
        let _clip = clipboard_win::Clipboard::new_attempts(10).expect("open clipboard");
        clipboard_win::raw::register_format(EXCLUDE_FORMAT)
            .is_some_and(|id| clipboard_win::raw::is_format_avail(id.get()))
    };
    assert!(marker_kept, "{T}: restore dropped the opt-out marker");
    assert!(
        t.read("text/plain", 4 * 1024 * 1024).is_err(),
        "{T}: restored clipboard still holds the paste text"
    );

    let swap = guard!(ClipboardSwap::begin(&paste, false), T);
    swap.finish(0, false);
    let kept = guard!(t.read("text/plain", 4 * 1024 * 1024), T);
    assert_eq!(kept, paste[0].1, "{T}: restore = false must keep the paste");

    if let Some(orig) = saved {
        let _ = t.write(&[("text/plain".to_string(), orig)]);
    }
}
