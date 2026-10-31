//! UI side of the daemon push channel: push mapping, the one-UI-per-profile
//! lock, the shared event queue, and the live listener (seg-02 §9).

use std::time::Duration;

use clipmonger_app_ui::ipc_bridge::{Bridge, Transport, UiCmd, UiEvent};
use clipmonger_app_ui::lifecycle::{claim_single_instance, InstanceClaim};
use clipmonger_app_ui::push::push_event;
use clipmonger_ipc::proto::methods::{ShowPopupEvent, SHOW_POPUP_TOPIC};
use clipmonger_ipc::proto::IpcError;
use serde_json::{json, Value};

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("cm-ui-push-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn show_popup_push_maps_to_a_shell_event() {
    let event = push_event(
        SHOW_POPUP_TOPIC,
        json!({"source": "hotkey", "toggle": false, "target": 42, "gesture": 7}),
    );
    assert_eq!(
        event,
        Some(UiEvent::ShowPopup(ShowPopupEvent {
            source: "hotkey".to_string(),
            toggle: false,
            target: Some(42),
            gesture: Some(7),
            x11_time: None,
            activation_token: None,
        }))
    );
    assert_eq!(
        push_event("config.changed", json!({})),
        None,
        "other topics"
    );
    assert_eq!(
        push_event(SHOW_POPUP_TOPIC, json!({"source": 5})),
        None,
        "malformed payloads are dropped"
    );
}

#[test]
fn one_ui_per_profile() {
    let profile = temp_dir("lock");
    let InstanceClaim::Primary(Some(lock)) = claim_single_instance(&profile) else {
        panic!("the first UI owns the profile");
    };
    assert!(
        matches!(claim_single_instance(&profile), InstanceClaim::Secondary),
        "a second UI must hand over"
    );
    drop(lock);
    assert!(
        matches!(
            claim_single_instance(&profile),
            InstanceClaim::Primary(Some(_))
        ),
        "the slot frees when the first UI exits"
    );
}

#[test]
fn pushes_share_the_command_event_queue() {
    struct Echo;
    impl Transport for Echo {
        fn call(&mut self, _method: &str, _params: &Value) -> Result<Value, IpcError> {
            Ok(json!({}))
        }
    }
    let (cmds, push, events) = Bridge::spawn_shared(Box::new(Echo));
    push.send(UiEvent::AnotherInstance).expect("push");
    cmds.send(UiCmd::Status).expect("command");
    let first = events.recv_timeout(Duration::from_secs(2)).expect("push");
    let second = events.recv_timeout(Duration::from_secs(2)).expect("reply");
    assert_eq!(first, UiEvent::AnotherInstance);
    assert!(matches!(second, UiEvent::Status(_)), "{second:?}");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn listener_delivers_pushes_and_yields_to_another_instance() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc};

    use clipmonger_app_ui::push::spawn_listener;
    use clipmonger_ipc::client::Client;
    use clipmonger_ipc::server::{EventHub, Handler, ServeOptions, Server, TopicRule};
    const TOKEN: &str = "ui-push-token";
    let dir = temp_dir("live");
    std::fs::create_dir_all(&dir).expect("dir");
    let sock = dir.join("u.sock");
    let hub = EventHub::new(vec![TopicRule {
        topic: SHOW_POPUP_TOPIC.to_string(),
        exclusive: true,
    }]);
    let handler: Handler = Arc::new(|_, _| Ok(json!({})));
    let options = ServeOptions {
        hub: Some(hub.clone()),
        ..ServeOptions::default()
    };
    let server = Server::bind_with(&sock, TOKEN.to_string(), handler, options)
        .await
        .expect("bind");
    std::mem::forget(server);

    let wakes = Arc::new(AtomicUsize::new(0));
    let (tx, rx) = mpsc::channel();
    let connect_sock = sock.clone();
    let counter = Arc::clone(&wakes);
    spawn_listener(
        move || Client::connect(&connect_sock, TOKEN).ok(),
        json!({}),
        tx,
        Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }),
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while hub.subscribers(SHOW_POPUP_TOPIC).is_empty() {
        assert!(std::time::Instant::now() < deadline, "listener subscribes");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        hub.publish(SHOW_POPUP_TOPIC, json!({"source": "request"})),
        1
    );
    let event = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("push arrives");
    assert!(
        matches!(&event, UiEvent::ShowPopup(show) if show.source == "request"),
        "{event:?}"
    );
    assert!(wakes.load(Ordering::SeqCst) >= 1, "the shell is woken");

    // A second UI's listener finds the popup taken and reports it.
    let (tx2, rx2) = mpsc::channel();
    let second_sock = sock.clone();
    spawn_listener(
        move || Client::connect(&second_sock, TOKEN).ok(),
        json!({}),
        tx2,
        Arc::new(|| {}),
    );
    assert_eq!(
        rx2.recv_timeout(Duration::from_secs(5)).expect("refusal"),
        UiEvent::AnotherInstance
    );
}
