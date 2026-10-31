//! T-211 push channel: `events.subscribe` over a live socket (seg-02 §9).
//!
//! Run: `cargo test -p clipmonger-ipc --features server --test push_acceptance`
#![cfg(all(feature = "server", unix))]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use clipmonger_ipc::client::{Client, EventStream};
use clipmonger_ipc::proto::methods::{
    PopupOpenParams, PopupOpenResult, ShowPopupEvent, SHOW_POPUP_TOPIC,
};
use clipmonger_ipc::proto::IpcError;
use clipmonger_ipc::server::{EventHub, Handler, ServeOptions, Server, TopicRule};
use serde_json::{json, Value};

const TOKEN: &str = "push-token";
const CONFIG_TOPIC: &str = "config.changed";

static SOCK_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_sock(tag: &str) -> PathBuf {
    let n = SOCK_COUNTER.fetch_add(1, Ordering::SeqCst);
    let mut dir = std::env::temp_dir();
    dir.push(format!("clipmonger-push-{}-{tag}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("tempdir must be creatable");
    dir.push("t.sock");
    dir
}

fn rules() -> Vec<TopicRule> {
    vec![
        TopicRule {
            topic: SHOW_POPUP_TOPIC.to_string(),
            exclusive: true,
        },
        TopicRule {
            topic: CONFIG_TOPIC.to_string(),
            exclusive: false,
        },
    ]
}

fn echo_handler() -> Handler {
    Arc::new(|method, params| match method.as_str() {
        "echo" => Ok(params),
        other => Err(IpcError::new(
            "unknown_method",
            format!("unknown method: {other}"),
        )),
    })
}

async fn serve(sock: &Path, hub: Option<EventHub>) {
    let options = ServeOptions {
        hub,
        ..ServeOptions::default()
    };
    let server = Server::bind_with(sock, TOKEN.to_string(), echo_handler(), options)
        .await
        .expect("bind must succeed on a fresh socket");
    // No shutdown API: keep the server alive for the whole test.
    std::mem::forget(server);
}

fn subscribe(sock: &Path, topics: &[&str], extra: Value) -> Result<EventStream, IpcError> {
    Client::connect(sock, TOKEN)
        .expect("connect must succeed")
        .subscribe(topics, extra)
}

/// Next event within `limit`, or `None` (the blocked reader thread leaks).
fn next_within(mut stream: EventStream, limit: Duration) -> Option<(EventStream, String, Value)> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let got = stream.next_event();
        let _ = tx.send((stream, got));
    });
    match rx.recv_timeout(limit) {
        Ok((stream, Ok((topic, params)))) => Some((stream, topic, params)),
        _ => None,
    }
}

fn wait_until(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    done()
}

#[test]
fn t211_proto_roundtrips() {
    let event = ShowPopupEvent {
        source: "hotkey".to_string(),
        toggle: false,
        target: Some(0x1234),
        gesture: Some(99),
        x11_time: None,
        activation_token: None,
    };
    let wire = serde_json::to_value(&event).expect("serialize");
    assert_eq!(
        wire,
        json!({"source": "hotkey", "toggle": false, "target": 0x1234, "gesture": 99}),
        "absent optional fields stay off the wire"
    );
    assert_eq!(
        serde_json::from_value::<ShowPopupEvent>(wire).expect("deserialize"),
        event
    );
    let params = serde_json::to_value(PopupOpenParams {}).expect("serialize");
    assert_eq!(
        serde_json::from_value::<PopupOpenParams>(params).expect("deserialize"),
        PopupOpenParams {}
    );
    let result = PopupOpenResult { delivered: true };
    let wire = serde_json::to_value(&result).expect("serialize");
    assert_eq!(
        serde_json::from_value::<PopupOpenResult>(wire).expect("deserialize"),
        result
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn t211_event_right_after_subscribe_arrives() {
    let sock = unique_sock("deliver");
    let hub = EventHub::new(rules());
    serve(&sock, Some(hub.clone())).await;
    let stream = subscribe(&sock, &[SHOW_POPUP_TOPIC], json!({})).expect("subscribe");
    let payload = json!({"source": "hotkey", "target": 42});
    assert_eq!(hub.publish(SHOW_POPUP_TOPIC, payload.clone()), 1);
    let (_, topic, params) =
        next_within(stream, Duration::from_secs(2)).expect("event must arrive");
    assert_eq!(topic, SHOW_POPUP_TOPIC);
    assert_eq!(params, payload);
}

#[tokio::test(flavor = "multi_thread")]
async fn t211_held_event_reaches_first_subscriber() {
    let sock = unique_sock("held");
    let hub = EventHub::new(rules());
    serve(&sock, Some(hub.clone())).await;
    let payload = json!({"source": "request"});
    assert_eq!(
        hub.publish_or_hold(SHOW_POPUP_TOPIC, payload.clone()),
        0,
        "nobody listens yet"
    );
    let stream = subscribe(&sock, &[SHOW_POPUP_TOPIC], json!({})).expect("subscribe");
    let (_, topic, params) =
        next_within(stream, Duration::from_secs(2)).expect("held event must arrive");
    assert_eq!((topic.as_str(), params), (SHOW_POPUP_TOPIC, payload));
}

#[tokio::test(flavor = "multi_thread")]
async fn t211_stale_held_event_is_dropped() {
    let sock = unique_sock("stale");
    let hub = EventHub::new(rules()).with_hold_ttl(Duration::from_millis(50));
    serve(&sock, Some(hub.clone())).await;
    let _ = hub.publish_or_hold(SHOW_POPUP_TOPIC, json!({"old": true}));
    std::thread::sleep(Duration::from_millis(150));
    let stream = subscribe(&sock, &[SHOW_POPUP_TOPIC], json!({})).expect("subscribe");
    assert_eq!(hub.publish(SHOW_POPUP_TOPIC, json!({"fresh": true})), 1);
    let (_, _, params) = next_within(stream, Duration::from_secs(2)).expect("event must arrive");
    assert_eq!(
        params,
        json!({"fresh": true}),
        "the expired event never arrives"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn t211_exclusive_topic_admits_one_subscriber_until_disconnect() {
    let sock = unique_sock("exclusive");
    let hub = EventHub::new(rules());
    serve(&sock, Some(hub.clone())).await;
    let first = subscribe(&sock, &[SHOW_POPUP_TOPIC], json!({})).expect("first subscribe");
    match subscribe(&sock, &[SHOW_POPUP_TOPIC], json!({})) {
        Ok(_) => panic!("a second subscriber must be refused"),
        Err(e) => assert_eq!(e.code, "already_subscribed"),
    }
    drop(first);
    assert!(
        wait_until(Duration::from_secs(2), || hub
            .subscribers(SHOW_POPUP_TOPIC)
            .is_empty()),
        "disconnect must release the topic"
    );
    subscribe(&sock, &[SHOW_POPUP_TOPIC], json!({})).expect("topic is free again");
}

#[tokio::test(flavor = "multi_thread")]
async fn t211_non_exclusive_topic_fans_out() {
    let sock = unique_sock("fanout");
    let hub = EventHub::new(rules());
    serve(&sock, Some(hub.clone())).await;
    let a = subscribe(&sock, &[CONFIG_TOPIC], json!({})).expect("subscribe a");
    let b = subscribe(&sock, &[CONFIG_TOPIC], json!({})).expect("subscribe b");
    assert_eq!(hub.publish(CONFIG_TOPIC, json!({"key": "x"})), 2);
    for stream in [a, b] {
        let (_, topic, _) = next_within(stream, Duration::from_secs(2)).expect("both receive");
        assert_eq!(topic, CONFIG_TOPIC);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn t211_refusals_unknown_topic_and_guard() {
    let sock = unique_sock("guard");
    let guard = Arc::new(|_topic: &str, _pid: Option<u32>, params: &Value| {
        params.get("popup_hwnd").and_then(Value::as_u64) == Some(7)
    });
    let hub = EventHub::new(rules()).with_guard(guard);
    serve(&sock, Some(hub.clone())).await;
    match subscribe(&sock, &["nope"], json!({})) {
        Ok(_) => panic!("unknown topics must be refused"),
        Err(e) => assert_eq!(e.code, "unknown_topic"),
    }
    match subscribe(&sock, &[SHOW_POPUP_TOPIC], json!({})) {
        Ok(_) => panic!("the guard must refuse this subscriber"),
        Err(e) => assert_eq!(e.code, "denied"),
    }
    assert_eq!(hub.subscribers(SHOW_POPUP_TOPIC), Vec::new());
    let _ok = subscribe(&sock, &[SHOW_POPUP_TOPIC], json!({"popup_hwnd": 7}))
        .expect("the guard admits this subscriber");
    let subs = hub.subscribers(SHOW_POPUP_TOPIC);
    assert_eq!(subs.len(), 1);
    assert_eq!(
        subs[0].peer_pid,
        Some(std::process::id()),
        "peer pid from the OS"
    );
    assert_eq!(subs[0].params.get("popup_hwnd"), Some(&json!(7)));
}

#[tokio::test(flavor = "multi_thread")]
async fn t211_requests_unaffected_and_no_hub_falls_through() {
    let sock = unique_sock("requests");
    let hub = EventHub::new(rules());
    serve(&sock, Some(hub)).await;
    let _stream = subscribe(&sock, &[SHOW_POPUP_TOPIC], json!({})).expect("subscribe");
    let client = Client::connect(&sock, TOKEN).expect("connect");
    assert_eq!(
        client.call("echo", json!({"n": 1})).expect("echo"),
        json!({"n": 1}),
        "requests keep working beside a subscription"
    );
    let plain = unique_sock("nohub");
    serve(&plain, None).await;
    match subscribe(&plain, &[SHOW_POPUP_TOPIC], json!({})) {
        Ok(_) => panic!("without a hub the handler answers"),
        Err(e) => assert_eq!(e.code, "unknown_method"),
    }
}
