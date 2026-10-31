//! T-211 push channel over a Windows named pipe (needs a Windows session).
//!
//! Run: `cargo test -p clipmonger-ipc --features server --test push_pipe_acceptance`
#![cfg(all(feature = "server", windows))]

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use clipmonger_ipc::client::{Client, EventStream};
use clipmonger_ipc::proto::methods::SHOW_POPUP_TOPIC;
use clipmonger_ipc::proto::IpcError;
use clipmonger_ipc::server::{EventHub, Handler, PipePeerPid, ServeOptions, Server, TopicRule};
use serde_json::{json, Value};

const TOKEN: &str = "push-pipe-token";

static PIPE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_pipe(tag: &str) -> String {
    let n = PIPE_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!(r"\\.\pipe\clipmonger-push-{}-{tag}-{n}", std::process::id())
}

/// Next event within `limit`, or `None` (the blocked reader thread leaks).
/// The stream comes back so the subscription stays open.
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

#[tokio::test(flavor = "multi_thread")]
async fn t211_pipe_event_delivery_peer_pid_and_exclusivity() {
    let name = unique_pipe("deliver");
    let hub = EventHub::new(vec![TopicRule {
        topic: SHOW_POPUP_TOPIC.to_string(),
        exclusive: true,
    }]);
    let calls = Arc::new(AtomicUsize::new(0));
    let peer_pid: PipePeerPid = {
        let calls = Arc::clone(&calls);
        Arc::new(move |handle| {
            calls.fetch_add(1, Ordering::SeqCst);
            (handle != 0).then_some(4242)
        })
    };
    let handler: Handler = Arc::new(|method, _params| {
        Err(IpcError::new(
            "unknown_method",
            format!("unknown method: {method}"),
        ))
    });
    let options = ServeOptions {
        hub: Some(hub.clone()),
        pipe_peer_pid: Some(peer_pid),
        ..ServeOptions::default()
    };
    let server = Server::bind_pipe_with(&name, TOKEN.to_string(), handler, options)
        .expect("bind must succeed on a fresh pipe name");
    // No shutdown API: keep the server alive for the whole test.
    std::mem::forget(server);

    let stream = Client::connect_pipe(&name, TOKEN)
        .expect("connect")
        .subscribe(&[SHOW_POPUP_TOPIC], json!({}))
        .expect("subscribe");
    assert!(
        calls.load(Ordering::SeqCst) >= 1,
        "the pipe handle reaches the resolver"
    );
    let subs = hub.subscribers(SHOW_POPUP_TOPIC);
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].peer_pid, Some(4242));

    let payload = json!({"source": "hotkey", "target": 7});
    assert_eq!(hub.publish(SHOW_POPUP_TOPIC, payload.clone()), 1);
    let (_stream, topic, params) =
        next_within(stream, Duration::from_secs(2)).expect("event must arrive over the pipe");
    assert_eq!((topic.as_str(), params), (SHOW_POPUP_TOPIC, payload));

    match Client::connect_pipe(&name, TOKEN)
        .expect("connect")
        .subscribe(&[SHOW_POPUP_TOPIC], json!({}))
    {
        Ok(_) => panic!("a second subscriber must be refused"),
        Err(e) => assert_eq!(e.code, "already_subscribed"),
    }
}
