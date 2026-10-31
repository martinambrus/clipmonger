//! `Bridge::spawn_shared_with_wake`: the wake callback runs on the bridge thread
//! right after every reply event it queued, a push through the shared sender needs
//! no wake, and `spawn` / `spawn_shared` keep working without one.
//!
//! No daemon and no window: the transport is a fake that answers from a canned
//! outcome. Waiting is by channel with a generous timeout, never by a fixed sleep,
//! except where the test shows that something does NOT happen.
//!
//! Run: `cargo test -p clipmonger-app-ui --test bridge_wake`

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

use clipmonger_app_ui::ipc_bridge::{Bridge, Transport, UiCmd, UiEvent};
use clipmonger_app_ui::push::Wake;
use clipmonger_ipc::proto::methods::ShowPopupEvent;
use clipmonger_ipc::proto::IpcError;
use serde_json::{json, Value};

/// Long enough for a loaded machine, short enough to fail a hung test.
const PATIENCE: Duration = Duration::from_secs(5);
/// How long to watch for something that must not happen.
const QUIET: Duration = Duration::from_millis(150);

/// A transport that answers every call with one outcome. With a gate it blocks
/// in `call` until the test sends a unit, and it remembers the thread of every call.
struct Fake {
    outcome: Result<Value, IpcError>,
    threads: Arc<Mutex<Vec<ThreadId>>>,
    gate: Option<mpsc::Receiver<()>>,
}

impl Fake {
    fn new(outcome: Result<Value, IpcError>) -> Self {
        Self {
            outcome,
            threads: Arc::new(Mutex::new(Vec::new())),
            gate: None,
        }
    }

    fn ok() -> Self {
        Self::new(Ok(json!({"ok": true})))
    }
}

impl Transport for Fake {
    fn call(&mut self, _method: &str, _params: &Value) -> Result<Value, IpcError> {
        if let Some(gate) = &self.gate {
            let _ = gate.recv();
        }
        self.threads
            .lock()
            .expect("the call log")
            .push(thread::current().id());
        self.outcome.clone()
    }
}

/// The observations of a wake callback.
struct Wakes {
    count: Arc<AtomicUsize>,
    threads: Arc<Mutex<Vec<ThreadId>>>,
    signal: mpsc::Receiver<()>,
}

impl Wakes {
    /// Wait for the next wake; panics when none comes.
    fn next(&self) {
        self.signal
            .recv_timeout(PATIENCE)
            .expect("the bridge calls the wake callback");
    }

    /// No further wake arrives within [`QUIET`].
    fn assert_quiet(&self, what: &str) {
        assert!(
            self.signal.recv_timeout(QUIET).is_err(),
            "unexpected wake: {what}"
        );
    }

    fn count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}

/// A wake callback that counts, remembers its thread and signals the test.
fn watch() -> (Wake, Wakes) {
    let count = Arc::new(AtomicUsize::new(0));
    let threads = Arc::new(Mutex::new(Vec::new()));
    let (tx, rx) = mpsc::channel();
    let wake: Wake = {
        let count = Arc::clone(&count);
        let threads = Arc::clone(&threads);
        Arc::new(move || {
            threads
                .lock()
                .expect("the wake log")
                .push(thread::current().id());
            count.fetch_add(1, Ordering::SeqCst);
            let _ = tx.send(());
        })
    };
    (
        wake,
        Wakes {
            count,
            threads,
            signal: rx,
        },
    )
}

fn failure(code: &str, message: &str) -> Result<Value, IpcError> {
    Err(IpcError::new(code, message.to_string()))
}

fn next_event(events: &mpsc::Receiver<UiEvent>) -> UiEvent {
    events
        .recv_timeout(PATIENCE)
        .expect("the bridge queues the reply")
}

fn popup_push(source: &str) -> UiEvent {
    UiEvent::ShowPopup(ShowPopupEvent {
        source: source.to_string(),
        ..ShowPopupEvent::default()
    })
}

// rule: the wake runs once after every reply event, in the order of the commands
#[test]
fn rule_bridge_wake_runs_once_after_every_reply() {
    let (wake, wakes) = watch();
    let (cmds, _push, events) = Bridge::spawn_shared_with_wake(Box::new(Fake::ok()), wake);
    let expected = [
        (UiCmd::Status, UiEvent::Status(json!({"ok": true}))),
        (UiCmd::ListHistory, UiEvent::History(json!({"ok": true}))),
        (UiCmd::ConfigGet, UiEvent::Config(json!({"ok": true}))),
        (UiCmd::CapsGet, UiEvent::Caps(json!({"ok": true}))),
        (
            UiCmd::ListPermanents,
            UiEvent::Permanents(json!({"ok": true})),
        ),
    ];
    for (n, (cmd, event)) in expected.iter().enumerate() {
        cmds.send(cmd.clone()).expect("the bridge thread runs");
        wakes.next();
        assert_eq!(&next_event(&events), event, "reply {n} to {cmd:?}");
        assert_eq!(wakes.count(), n + 1, "wakes after reply {n}");
    }
    wakes.assert_quiet("after the last reply");
    assert!(events.try_recv().is_err(), "no event beyond the replies");
}

// rule: commands sent in a burst are answered in order, one wake per reply
#[test]
fn rule_bridge_wake_counts_every_reply_of_a_burst() {
    let (wake, wakes) = watch();
    let (cmds, _push, events) = Bridge::spawn_shared_with_wake(Box::new(Fake::ok()), wake);
    let burst = [
        UiCmd::Status,
        UiCmd::ConfigGet,
        UiCmd::CapsGet,
        UiCmd::PermGroups,
    ];
    for cmd in &burst {
        cmds.send(cmd.clone()).expect("the bridge thread runs");
    }
    let mut got = Vec::new();
    for _ in &burst {
        wakes.next();
        got.push(next_event(&events));
    }
    assert_eq!(
        got,
        vec![
            UiEvent::Status(json!({"ok": true})),
            UiEvent::Config(json!({"ok": true})),
            UiEvent::Caps(json!({"ok": true})),
            UiEvent::PermGroups(json!({"ok": true})),
        ]
    );
    wakes.assert_quiet("after the burst");
    assert_eq!(wakes.count(), burst.len());
}

// rule: the wake runs after the event is queued, never before
#[test]
fn rule_bridge_wake_runs_after_the_event_is_queued() {
    let (tx_woke, rx_woke) = mpsc::channel::<()>();
    let seen: Arc<Mutex<Vec<Option<UiEvent>>>> = Arc::new(Mutex::new(Vec::new()));
    let queue: Arc<Mutex<Option<mpsc::Receiver<UiEvent>>>> = Arc::new(Mutex::new(None));
    let wake: Wake = {
        let seen = Arc::clone(&seen);
        let queue = Arc::clone(&queue);
        Arc::new(move || {
            let observed = queue
                .lock()
                .expect("the queue handle")
                .as_ref()
                .and_then(|events| events.try_recv().ok());
            seen.lock().expect("the observations").push(observed);
            let _ = tx_woke.send(());
        })
    };
    let (cmds, _push, events) = Bridge::spawn_shared_with_wake(Box::new(Fake::ok()), wake);
    *queue.lock().expect("the queue handle") = Some(events);
    for cmd in [UiCmd::Status, UiCmd::ConfigGet, UiCmd::CapsGet] {
        cmds.send(cmd).expect("the bridge thread runs");
        rx_woke.recv_timeout(PATIENCE).expect("a wake");
    }
    let seen = seen.lock().expect("the observations").clone();
    assert_eq!(
        seen,
        vec![
            Some(UiEvent::Status(json!({"ok": true}))),
            Some(UiEvent::Config(json!({"ok": true}))),
            Some(UiEvent::Caps(json!({"ok": true}))),
        ],
        "at every wake the reply is already in the queue"
    );
}

// rule: the wake runs on the bridge thread, the one that runs the transport
#[test]
fn rule_bridge_wake_runs_on_the_bridge_thread() {
    let (wake, wakes) = watch();
    let transport = Fake::ok();
    let call_threads = Arc::clone(&transport.threads);
    let (cmds, _push, events) = Bridge::spawn_shared_with_wake(Box::new(transport), wake);
    for cmd in [UiCmd::Status, UiCmd::ListHistory] {
        cmds.send(cmd).expect("the bridge thread runs");
        wakes.next();
        let _ = next_event(&events);
    }
    let wake_threads = wakes.threads.lock().expect("the wake log").clone();
    let call_threads = call_threads.lock().expect("the call log").clone();
    assert_eq!(wake_threads.len(), 2);
    assert_eq!(call_threads.len(), 2);
    let here = thread::current().id();
    assert!(
        wake_threads.iter().all(|id| *id != here),
        "the wake must not run on the thread that sent the command"
    );
    assert!(
        wake_threads.iter().all(|id| *id == wake_threads[0]),
        "one bridge thread runs every wake"
    );
    assert_eq!(
        wake_threads, call_threads,
        "the wake runs on the thread of the transport"
    );
}

// rule: the wake does not run before the transport has answered
#[test]
fn rule_bridge_wake_waits_for_the_reply() {
    let (wake, wakes) = watch();
    let (gate_tx, gate_rx) = mpsc::channel();
    let mut transport = Fake::ok();
    transport.gate = Some(gate_rx);
    let (cmds, _push, events) = Bridge::spawn_shared_with_wake(Box::new(transport), wake);
    cmds.send(UiCmd::Status).expect("the bridge thread runs");
    wakes.assert_quiet("while the transport has not answered");
    assert_eq!(wakes.count(), 0);
    assert!(events.try_recv().is_err(), "no event before the answer");
    gate_tx.send(()).expect("release the call");
    wakes.next();
    assert_eq!(
        next_event(&events),
        UiEvent::Status(json!({"ok": true})),
        "the reply that the wake announced"
    );
}

// rule: error replies are replies too: each is queued and woken for
#[test]
fn rule_bridge_wake_runs_for_failed_calls() {
    let cases: Vec<(UiCmd, Result<Value, IpcError>, UiEvent)> = vec![
        (
            UiCmd::Status,
            failure("unavailable", "daemon down"),
            UiEvent::Offline("daemon down".to_string()),
        ),
        (
            UiCmd::ListHistory,
            failure("io_failed", "pipe broke"),
            UiEvent::Offline("pipe broke".to_string()),
        ),
        (
            UiCmd::ConfigGet,
            failure("internal", "no config"),
            UiEvent::Error("no config".to_string()),
        ),
        (
            UiCmd::Stop,
            failure("internal", "stop refused"),
            UiEvent::StopFailed("stop refused".to_string()),
        ),
        (
            UiCmd::PasteClipTo(uuid::Uuid::from_u128(1), Some(7), 5, None),
            failure("internal", "window gone"),
            UiEvent::PasteFailed("window gone".to_string()),
        ),
    ];
    for (cmd, outcome, event) in cases {
        let (wake, wakes) = watch();
        let (cmds, _push, events) =
            Bridge::spawn_shared_with_wake(Box::new(Fake::new(outcome)), wake);
        cmds.send(cmd.clone()).expect("the bridge thread runs");
        wakes.next();
        assert_eq!(next_event(&events), event, "reply to {cmd:?}");
        wakes.assert_quiet("one wake for one failed call");
    }
}

// rule: a push through the shared sender is queued without any wake
#[test]
fn rule_bridge_wake_not_needed_for_a_push() {
    let (wake, wakes) = watch();
    let (_cmds, push, events) = Bridge::spawn_shared_with_wake(Box::new(Fake::ok()), wake);
    push.send(popup_push("hotkey")).expect("the queue is open");
    assert_eq!(next_event(&events), popup_push("hotkey"));
    wakes.assert_quiet("a push is woken by its sender, not by the bridge");
    assert_eq!(wakes.count(), 0);
}

// rule: pushes and replies share one queue in the order they were queued
#[test]
fn rule_bridge_wake_pushes_and_replies_share_the_queue() {
    let (wake, wakes) = watch();
    let (cmds, push, events) = Bridge::spawn_shared_with_wake(Box::new(Fake::ok()), wake);
    push.send(popup_push("hotkey")).expect("the queue is open");
    cmds.send(UiCmd::Status).expect("the bridge thread runs");
    wakes.next();
    push.send(popup_push("request")).expect("the queue is open");
    assert_eq!(next_event(&events), popup_push("hotkey"));
    assert_eq!(next_event(&events), UiEvent::Status(json!({"ok": true})));
    assert_eq!(next_event(&events), popup_push("request"));
    wakes.assert_quiet("only the reply is woken for");
    assert_eq!(wakes.count(), 1);
}

// rule: spawn_shared still works and needs no wake
#[test]
fn rule_bridge_wake_spawn_shared_still_works() {
    let (cmds, push, events) = Bridge::spawn_shared(Box::new(Fake::ok()));
    cmds.send(UiCmd::Status).expect("the bridge thread runs");
    assert_eq!(next_event(&events), UiEvent::Status(json!({"ok": true})));
    push.send(popup_push("request")).expect("the queue is open");
    assert_eq!(next_event(&events), popup_push("request"));
    cmds.send(UiCmd::ConfigGet).expect("the bridge thread runs");
    assert_eq!(next_event(&events), UiEvent::Config(json!({"ok": true})));
}

// rule: spawn still works and needs no wake
#[test]
fn rule_bridge_wake_spawn_still_works() {
    let (cmds, events) = Bridge::spawn(Box::new(Fake::ok()));
    cmds.send(UiCmd::CapsGet).expect("the bridge thread runs");
    assert_eq!(next_event(&events), UiEvent::Caps(json!({"ok": true})));
}

// rule: two bridges with their own wakes do not wake each other
#[test]
fn rule_bridge_wake_belongs_to_its_bridge() {
    let (busy_wake, busy) = watch();
    let (idle_wake, idle) = watch();
    let (busy_cmds, _busy_push, busy_events) =
        Bridge::spawn_shared_with_wake(Box::new(Fake::ok()), busy_wake);
    let (_idle_cmds, _idle_push, _idle_events) =
        Bridge::spawn_shared_with_wake(Box::new(Fake::ok()), idle_wake);
    busy_cmds
        .send(UiCmd::Status)
        .expect("the bridge thread runs");
    busy.next();
    let _ = next_event(&busy_events);
    idle.assert_quiet("the other bridge");
    assert_eq!(idle.count(), 0);
}

// rule: the bridge thread ends when the command sender is gone, and lets go of the wake
#[test]
fn rule_bridge_wake_thread_ends_with_the_commands() {
    let (wake, wakes) = watch();
    let keep = Arc::clone(&wake);
    let (cmds, push, events) = Bridge::spawn_shared_with_wake(Box::new(Fake::ok()), wake);
    cmds.send(UiCmd::Status).expect("the bridge thread runs");
    wakes.next();
    let _ = next_event(&events);
    assert!(
        Arc::strong_count(&keep) >= 2,
        "the bridge thread holds the wake while it runs"
    );
    drop(cmds);
    let started = Instant::now();
    while Arc::strong_count(&keep) > 1 {
        assert!(
            started.elapsed() < PATIENCE,
            "the bridge thread still holds the wake after the commands closed"
        );
        thread::sleep(Duration::from_millis(5));
    }
    // the queue stays usable for pushes
    push.send(popup_push("hotkey")).expect("the queue is open");
    assert_eq!(next_event(&events), popup_push("hotkey"));
    assert_eq!(wakes.count(), 1, "no wake without a reply");
}
