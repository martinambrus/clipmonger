//! T-204 determinism, T-206 suppression, T-207 guarded restore, T-208 revalidation.

use clipmonger_platform::fakes::{FakeHotkeys, FakeMonitor};
use clipmonger_platform::paste_policy::{
    guarded_restore_decision, validate_target, SuppressionRegistry, SUPPRESSION_TTL_MS,
};
use clipmonger_platform::traits::{ClipboardMonitor, HotkeyManager};
use clipmonger_platform::types::{
    AppId, CaptureEvent, CaptureOrigin, FormatOffer, HotkeyId, PasteTarget, PlatformHandle,
};

// T-204: same script twice -> identical event sequence.
fn script() -> Vec<Vec<CaptureEvent>> {
    vec![
        vec![CaptureEvent {
            offers: vec![FormatOffer {
                mime: "text/plain".to_string(),
                bytes_hint: Some(5),
            }],
            origin: CaptureOrigin::Clipboard,
            sensitive: false,
        }],
        vec![],
        vec![CaptureEvent {
            offers: vec![],
            origin: CaptureOrigin::Primary,
            sensitive: true,
        }],
    ]
}

#[test]
fn t204_same_script_twice_identical_seq() {
    let run = |mut m: FakeMonitor| {
        let mut out = Vec::new();
        for _ in 0..3 {
            out.push(m.poll(std::time::Duration::from_millis(0)).unwrap());
        }
        out
    };
    let a = run(FakeMonitor::new(script()));
    let b = run(FakeMonitor::new(script()));
    assert_eq!(a, b);
}

#[test]
fn t204_hotkey_script_deterministic() {
    let run = |mut h: FakeHotkeys| {
        (0..2)
            .map(|_| h.poll(std::time::Duration::from_millis(0)).unwrap())
            .collect::<Vec<_>>()
    };
    let mk = || FakeHotkeys::new(vec![vec![HotkeyId(1)], vec![HotkeyId(1), HotkeyId(2)]]);
    assert_eq!(run(mk()), run(mk()));
}

// T-206 cells.
#[test]
fn t206_exact_match_suppressed_once() {
    let mut reg = SuppressionRegistry::new();
    reg.register("h1".to_string(), Some("s1".to_string()), 1000);
    assert!(reg.should_suppress("h1", Some("s1"), 1500));
    // Consume-once: second identical event is captured normally.
    assert!(!reg.should_suppress("h1", Some("s1"), 1500));
    assert_eq!(reg.suppressed_count(), 1);
}

#[test]
fn t206_racing_copy_captured() {
    let mut reg = SuppressionRegistry::new();
    reg.register("h1".to_string(), Some("s1".to_string()), 1000);
    // Genuine user copy racing our write: different hash -> captured.
    assert!(!reg.should_suppress("user-hash", Some("s9"), 1200));
    // Same hash but changed pre-state (user copy replaced ours) -> captured.
    assert!(!reg.should_suppress("h1", Some("s2"), 1200));
    // Our own event still suppresses after the race.
    assert!(reg.should_suppress("h1", Some("s1"), 1200));
}

#[test]
fn t206_ttl_expiry() {
    let mut reg = SuppressionRegistry::new();
    reg.register("h1".to_string(), None, 1000);
    assert!(!reg.should_suppress("h1", None, 1000 + SUPPRESSION_TTL_MS + 1));
    assert_eq!(reg.suppressed_count(), 0);
}

#[test]
fn t206_dual_registry_sharing() {
    // One registry shared by two monitors (X11 + Wayland): either consults it.
    let mut reg = SuppressionRegistry::new();
    reg.register("h1".to_string(), Some("s1".to_string()), 5000);
    assert!(reg.should_suppress("h1", Some("s1"), 5100)); // monitor A
    assert!(!reg.should_suppress("h1", Some("s1"), 5100)); // monitor B sees nothing left
}

#[test]
fn t206_hash_only_when_no_prestate() {
    let mut reg = SuppressionRegistry::new();
    reg.register("h1".to_string(), None, 1000);
    assert!(reg.should_suppress("h1", Some("anything"), 1500));
}

// T-207: 4-cell AND truth table.
#[test]
fn t207_guarded_restore_and_table() {
    assert!(guarded_restore_decision(true, true));
    assert!(!guarded_restore_decision(true, false));
    assert!(!guarded_restore_decision(false, true));
    assert!(!guarded_restore_decision(false, false));
}

// T-208 cells.
fn target(app: &str, handle: PlatformHandle) -> PasteTarget {
    PasteTarget {
        app: AppId {
            exe: app.to_string(),
            title_hash: 7,
        },
        window_title_hash: 7,
        handle,
        captured_ms: 100,
    }
}

#[test]
fn t208_same_ok() {
    let t = target("notepad.exe", PlatformHandle::WinHwnd(42));
    assert!(validate_target(
        &t,
        Some(&target("notepad.exe", PlatformHandle::WinHwnd(42)))
    )
    .is_ok());
}

#[test]
fn t208_stale_app_and_handle() {
    let captured = target("a.exe", PlatformHandle::WinHwnd(1));
    assert!(validate_target(
        &captured,
        Some(&target("b.exe", PlatformHandle::WinHwnd(1)))
    )
    .is_err());
    assert!(validate_target(
        &captured,
        Some(&target("a.exe", PlatformHandle::WinHwnd(2)))
    )
    .is_err());
    let mut moved = target("a.exe", PlatformHandle::WinHwnd(1));
    moved.window_title_hash = 99;
    assert!(validate_target(&captured, Some(&moved)).is_err());
}

#[test]
fn t208_dead_target() {
    let captured = target("a.exe", PlatformHandle::Opaque("x".to_string()));
    assert!(validate_target(&captured, None).is_err());
}
