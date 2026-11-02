#![cfg(all(feature = "server", unix))]
//! Real socket controls for bounded replies and connection ownership.
use std::io::{BufRead, BufReader, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use clipmonger_ipc::framing;
use clipmonger_ipc::proto::{Envelope, FrameKind, IpcError, MAX_LINE_BYTES, IPC_VERSION};
use clipmonger_ipc::server::{EventHub, Handler, ServeOptions, Server, TopicRule};
use serde_json::{json, Value};

const TOKEN: &str = "ipc-response-token";
const DEADLINE: Duration = Duration::from_secs(5);
static NEXT: AtomicU64 = AtomicU64::new(0);

struct SocketDir(PathBuf);
impl SocketDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("i{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir(&dir).expect("exclusive fixture directory");
        let path = dir.join("s");
        assert!(path.as_os_str().as_bytes().len() < 104, "TMPDIR too long for AF_UNIX: {path:?}");
        Self(dir)
    }
    fn path(&self) -> PathBuf { self.0.join("s") }
}
impl Drop for SocketDir {
    fn drop(&mut self) { let _ = std::fs::remove_file(self.path()); let _ = std::fs::remove_dir(&self.0); }
}
fn req(id: &str, method: &str, params: Option<Value>) -> Envelope {
    Envelope { v: IPC_VERSION.into(), id: id.into(), kind: FrameKind::Req, method: Some(method.into()), params, result: None, error: None, seq: None }
}
fn response(id: &str, result: Option<Value>, error: Option<IpcError>) -> Envelope {
    Envelope { v: IPC_VERSION.into(), id: id.into(), kind: FrameKind::Res, method: None, params: None, result, error, seq: None }
}
fn fallback(id: &str) -> Envelope {
    response(id, None, Some(IpcError::new("oversize", "response exceeds frame limit".into())))
}
fn large() -> Value { Value::String("x".repeat(2 * MAX_LINE_BYTES)) }
fn send(w: &mut UnixStream, env: &Envelope) {
    w.write_all(&framing::encode_line(env).expect("actual request must encode")).unwrap();
}
fn recv(r: &mut BufReader<UnixStream>) -> Option<Envelope> {
    let mut line = String::new();
    match r.read_line(&mut line) {
        Ok(0) => None,
        Ok(_) => Some(framing::decode_line(line.trim_end_matches(['\r', '\n']).as_bytes()).expect("valid response")),
        Err(e) => panic!("read failed (timeout is not EOF): {e}"),
    }
}
fn connect(path: &PathBuf) -> (UnixStream, BufReader<UnixStream>) {
    let mut w = UnixStream::connect(path).unwrap();
    w.set_read_timeout(Some(DEADLINE)).unwrap();
    w.set_write_timeout(Some(DEADLINE)).unwrap();
    let mut r = BufReader::new(w.try_clone().unwrap());
    send(&mut w, &req("hello", "hello", Some(json!({"token":TOKEN,"client":"control"}))));
    assert!(recv(&mut r).unwrap().error.is_none());
    (w, r)
}
fn handler() -> Handler {
    Arc::new(|method, params| match method.as_str() {
        "big" => Ok(large()),
        "ping" => Ok(json!({"pong":1})),
        _ => Ok(params),
    })
}
async fn server(path: &PathBuf, handler: Handler, hub: Option<EventHub>) -> Server {
    Server::bind_with(path, TOKEN.into(), handler, ServeOptions { hub, ..ServeOptions::default() }).await.unwrap()
}
fn fatal_request() -> Envelope {
    // Choose the exact maximal valid request, then establish both response premises.
    let empty = req("", "big", None);
    let overhead = framing::encode_line(&empty).unwrap().len() - 1;
    let id = "a".repeat(MAX_LINE_BYTES - overhead);
    let request = req(&id, "big", None);
    assert!(framing::encode_line(&request).is_ok());
    assert!(framing::encode_line(&response(&id, Some(large()), None)).is_err());
    assert!(framing::encode_line(&fallback(&id)).is_err());
    request
}
fn assert_pong(w: &mut UnixStream, r: &mut BufReader<UnixStream>) {
    send(w, &req("p", "ping", None));
    let env = recv(r).expect("pong");
    assert_eq!(env.id, "p");
    assert_eq!(env.result, Some(json!({"pong":1})));
}

#[tokio::test(flavor = "multi_thread")]
async fn fallback_keeps_connection_and_escaped_id() {
    let dir = SocketDir::new(); let path = dir.path();
    let _server = server(&path, handler(), None).await;
    let (mut w, mut r) = connect(&path);
    // Quotes and backslashes take two bytes on the wire.
    let id = "\\\"".repeat((MAX_LINE_BYTES - 2000) / 4);
    let request = req(&id, "big", None);
    assert!(framing::encode_line(&request).is_ok());
    assert!(framing::encode_line(&response(&id, Some(large()), None)).is_err());
    assert!(framing::encode_line(&fallback(&id)).is_ok());
    send(&mut w, &request);
    let got = recv(&mut r).expect("fallback");
    assert_eq!(got, fallback(&id));
    assert_pong(&mut w, &mut r);
}

struct Release(Option<mpsc::Sender<()>>);
impl Drop for Release { fn drop(&mut self) { if let Some(tx) = self.0.take() { let _ = tx.send(()); } } }

#[tokio::test(flavor = "multi_thread")]
async fn fatal_reply_closes_with_blocked_handler_and_subscription() {
    let dir = SocketDir::new(); let path = dir.path();
    let hub = EventHub::new(vec![TopicRule { topic: "foo".into(), exclusive: true }]);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release = Release(Some(release_tx));
    let release_rx = Mutex::new(release_rx);
    let timed_out = Arc::new(AtomicBool::new(false));
    let handler_timed_out = Arc::clone(&timed_out);
    let h: Handler = Arc::new(move |method, params| match method.as_str() {
        "block" => { entered_tx.send(()).unwrap(); if release_rx.lock().unwrap().recv_timeout(Duration::from_secs(20)).is_err() { handler_timed_out.store(true, Ordering::SeqCst); } Ok(json!({})) },
        "big" => Ok(large()), "ping" => Ok(json!({"pong":1})), _ => Ok(params),
    });
    let _server = server(&path, h, Some(hub.clone())).await;
    let (mut w, mut r) = connect(&path);
    send(&mut w, &req("s", "events.subscribe", Some(json!({"topics":["foo"]}))));
    assert_eq!(recv(&mut r).unwrap().id, "s");
    send(&mut w, &req("b", "block", None));
    entered_rx.recv_timeout(DEADLINE).expect("handler entered");
    let fatal = fatal_request();
    send(&mut w, &fatal);
    let start = Instant::now();
    assert!(recv(&mut r).is_none(), "fatal reply must close, not return a frame");
    assert!(start.elapsed() < DEADLINE, "EOF must be prompt");
    assert!(!timed_out.load(Ordering::SeqCst), "EOF must precede handler fallback");
    drop(release);
    let limit = Instant::now() + DEADLINE;
    while !hub.subscribers("foo").is_empty() && Instant::now() < limit { tokio::time::sleep(Duration::from_millis(10)).await; }
    assert!(hub.subscribers("foo").is_empty());
    let (mut other, mut other_r) = connect(&path);
    assert_pong(&mut other, &mut other_r);
}

#[tokio::test(flavor = "multi_thread")]
async fn held_event_follows_subscribe_ack() {
    let dir = SocketDir::new(); let path = dir.path();
    let hub = EventHub::new(vec![TopicRule { topic: "foo".into(), exclusive: false }]);
    let _server = server(&path, handler(), Some(hub.clone())).await;
    let (mut w, mut r) = connect(&path);
    assert_eq!(hub.publish_or_hold("foo", json!({"n":1})), 0);
    send(&mut w, &req("s", "events.subscribe", Some(json!({"topics":["foo"]}))));
    let ack = recv(&mut r).unwrap();
    assert_eq!(ack.kind, FrameKind::Res); assert_eq!(ack.id, "s"); assert_eq!(ack.result, Some(json!({"ok":true})));
    let event = recv(&mut r).unwrap();
    assert_eq!(event.kind, FrameKind::Event); assert_eq!(event.method.as_deref(), Some("foo")); assert_eq!(event.params, Some(json!({"n":1})));
    assert_pong(&mut w, &mut r);
}

#[test]
fn fixed_ack_size_premises() {
    let hello = req("", "hello", Some(json!({"token":TOKEN,"client":"control"})));
    let welcome = response("", Some(json!({"chosen":1,"daemon_version":env!("CARGO_PKG_VERSION")})), None);
    let subscribe = req("", "events.subscribe", Some(json!({"topics":["foo"]})));
    let ack = response("", Some(json!({"ok":true})), None);
    for (request, reply) in [(hello, welcome), (subscribe, ack)] {
        assert!(framing::encode_line(&reply).unwrap().len() <= framing::encode_line(&request).unwrap().len());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn fatal_hello_does_not_emit_lifecycle() {
    let dir = SocketDir::new();
    let path = dir.path();
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&events);
    let options = ServeOptions {
        lifecycle: Some(Arc::new(move |event| seen.lock().unwrap().push(event))),
        ..ServeOptions::default()
    };
    let _server = Server::bind_with(&path, "t".into(), handler(), options).await.unwrap();
    let empty = req("", "hello", Some(json!({"token":"t"})));
    let overhead = framing::encode_line(&empty).unwrap().len() - 1;
    let id = "a".repeat(MAX_LINE_BYTES - overhead);
    let hello = req(&id, "hello", Some(json!({"token":"t"})));
    let welcome = response(&id, Some(json!({"chosen":1,"daemon_version":env!("CARGO_PKG_VERSION")})), None);
    assert!(framing::encode_line(&hello).is_ok());
    assert!(framing::encode_line(&welcome).is_err());
    assert!(framing::encode_line(&fallback(&id)).is_err());
    let mut w = UnixStream::connect(&path).unwrap();
    w.set_read_timeout(Some(DEADLINE)).unwrap();
    w.set_write_timeout(Some(DEADLINE)).unwrap();
    let mut r = BufReader::new(w.try_clone().unwrap());
    send(&mut w, &hello);
    let start = Instant::now();
    assert!(recv(&mut r).is_none());
    assert!(start.elapsed() < DEADLINE);
    assert!(events.lock().unwrap().is_empty(), "failed hello must not emit lifecycle events");
    let mut other = UnixStream::connect(&path).unwrap();
    other.set_read_timeout(Some(DEADLINE)).unwrap();
    other.set_write_timeout(Some(DEADLINE)).unwrap();
    let mut other_r = BufReader::new(other.try_clone().unwrap());
    send(&mut other, &req("h", "hello", Some(json!({"token":"t"}))));
    assert!(recv(&mut other_r).unwrap().error.is_none());
    assert_pong(&mut other, &mut other_r);
}

#[tokio::test(flavor = "multi_thread")]
async fn subscription_error_fallback_preserves_existing_subscription() {
    let dir = SocketDir::new();
    let path = dir.path();
    let hub = EventHub::new(vec![TopicRule { topic: "foo".into(), exclusive: true }]);
    let _server = server(&path, handler(), Some(hub.clone())).await;
    let (mut w, mut r) = connect(&path);
    send(&mut w, &req("first", "events.subscribe", Some(json!({"topics":["foo"]}))));
    let ack = recv(&mut r).expect("first subscription acknowledgement");
    assert_eq!(ack.id, "first");
    assert_eq!(ack.result, Some(json!({"ok":true})));
    assert_eq!(hub.subscribers("foo").len(), 1);

    let fallback_overhead = framing::encode_line(&fallback("")).unwrap().len() - 1;
    let id = "a".repeat(MAX_LINE_BYTES - fallback_overhead);
    let request = req(&id, "events.subscribe", Some(json!({"topics":["foo"]})));
    let original_error = response(&id, None, Some(IpcError::new(
        "already_subscribed", "this connection is already subscribed".into(),
    )));
    assert!(framing::encode_line(&request).is_ok());
    assert!(framing::encode_line(&original_error).is_err());
    assert!(framing::encode_line(&fallback(&id)).is_ok());
    send(&mut w, &request);
    assert_eq!(recv(&mut r).expect("correlated fallback"), fallback(&id));
    assert_pong(&mut w, &mut r);
    assert_eq!(hub.subscribers("foo").len(), 1);
    assert_eq!(hub.publish("foo", json!({"after":"fallback"})), 1);
    let event = recv(&mut r).expect("existing subscription receives event");
    assert_eq!(event.kind, FrameKind::Event);
    assert_eq!(event.method.as_deref(), Some("foo"));
    assert_eq!(event.params, Some(json!({"after":"fallback"})));
}
