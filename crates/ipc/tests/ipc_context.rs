#![cfg(all(unix, feature = "server"))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc as std_mpsc, Arc};
use std::time::Duration;

use clipmonger_ipc::framing::{decode_line, encode_line};
use clipmonger_ipc::proto::{Envelope, FrameKind, IPC_VERSION};
use clipmonger_ipc::server::{ContextHandler, Handler, ServeOptions, Server};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::time::timeout;

const LIMIT: Duration = Duration::from_secs(5);

fn frame(id: &str, kind: FrameKind, method: Option<&str>, params: Option<Value>) -> Envelope {
    Envelope {
        v: IPC_VERSION.into(),
        id: id.into(),
        kind,
        method: method.map(str::to_owned),
        params,
        result: None,
        error: None,
        seq: None,
    }
}

async fn send(write: &mut OwnedWriteHalf, env: Envelope) {
    timeout(LIMIT, write.write_all(&encode_line(&env).unwrap()))
        .await
        .unwrap()
        .unwrap();
}

async fn receive(read: &mut BufReader<OwnedReadHalf>) -> Envelope {
    let mut line = Vec::new();
    let count = timeout(LIMIT, read.read_until(b'\n', &mut line))
        .await
        .unwrap()
        .unwrap();
    assert_ne!(count, 0, "unexpected EOF");
    decode_line(&line).unwrap()
}

async fn connect(path: &std::path::Path) -> (BufReader<OwnedReadHalf>, OwnedWriteHalf) {
    let stream = timeout(LIMIT, tokio::net::UnixStream::connect(path))
        .await
        .unwrap()
        .unwrap();
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);
    send(
        &mut write,
        frame(
            "hello",
            FrameKind::Req,
            Some("hello"),
            Some(json!({"token":"tok"})),
        ),
    )
    .await;
    assert!(receive(&mut read).await.result.is_some());
    (read, write)
}

struct Observation {
    label: String,
    flag: Arc<AtomicBool>,
    release: Option<std_mpsc::Sender<()>>,
    done: Option<std_mpsc::Receiver<bool>>,
}
impl Drop for Observation {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        // A bounded observation also runs during unwinding after a failed assertion.
        let Some(done) = self.done.take() else {
            return;
        };
        let finished = done.recv_timeout(LIMIT + Duration::from_secs(1)) == Ok(true);
        if !std::thread::panicking() {
            assert!(finished, "handler did not finish: {}", self.label);
        }
    }
}
struct Completion {
    sender: Option<std_mpsc::Sender<bool>>,
    success: bool,
}
impl Drop for Completion {
    fn drop(&mut self) {
        if let Some(done) = self.sender.take() {
            let _ = done.send(self.success);
        }
    }
}

// The handler owns a finite release wait. Tokens are observed only after a
// same-connection ping proves the server processed all earlier input frames.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_duplicates_empty_id_and_disconnect_are_connection_local() {
    let path = std::env::temp_dir().join(format!(
        "cm-context-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir(&path).unwrap();
    struct Dir(std::path::PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _dir = Dir(path.clone());
    let sock = path.join("ipc.sock");
    let (seen_tx, mut seen_rx) = tokio::sync::mpsc::channel::<Observation>(8);
    let contextual = ContextHandler {
        method: "history.thumbnail".into(),
        handler: Arc::new(move |params, flag| {
            let (release, wait) = std_mpsc::channel();
            let (done_tx, done) = std_mpsc::channel();
            let mut completion = Completion {
                sender: Some(done_tx),
                success: false,
            };
            let label = params["label"].as_str().unwrap().to_owned();
            if let Err(error) = seen_tx.blocking_send(Observation {
                label,
                flag,
                release: Some(release),
                done: Some(done),
            }) {
                let mut observation = error.0;
                observation.release.take();
                observation.done.take();
                return Err(clipmonger_ipc::proto::IpcError::new(
                    "fixture",
                    "observation receiver closed".into(),
                ));
            }
            wait.recv_timeout(LIMIT).map_err(|_| {
                clipmonger_ipc::proto::IpcError::new("timeout", "fixture release".into())
            })?;
            completion.success = true;
            Ok(json!({"value": true}))
        }),
    };
    let handler: Handler = Arc::new(|_, _| Ok(json!({"pong":true})));
    let _server = Server::bind_with_context(
        &sock,
        "tok".into(),
        handler,
        ServeOptions::default(),
        contextual,
    )
    .await
    .unwrap();
    let (mut first_read, mut first_write) = connect(&sock).await;
    let (second_read, mut second_write) = connect(&sock).await;
    for label in ["first-1", "first-2"] {
        send(
            &mut first_write,
            frame(
                "",
                FrameKind::Req,
                Some("history.thumbnail"),
                Some(json!({"label":label})),
            ),
        )
        .await;
    }
    send(
        &mut second_write,
        frame(
            "",
            FrameKind::Req,
            Some("history.thumbnail"),
            Some(json!({"label":"second-1"})),
        ),
    )
    .await;
    let mut observations = Vec::new();
    for _ in 0..3 {
        observations.push(timeout(LIMIT, seen_rx.recv()).await.unwrap().unwrap());
    }
    observations.sort_by(|a, b| a.label.cmp(&b.label));
    assert_eq!(
        observations
            .iter()
            .map(|o| o.label.as_str())
            .collect::<Vec<_>>(),
        ["first-1", "first-2", "second-1"]
    );
    send(&mut first_write, frame("", FrameKind::Cancel, None, None)).await;
    send(
        &mut first_write,
        frame("barrier", FrameKind::Req, Some("ping"), None),
    )
    .await;
    assert_eq!(receive(&mut first_read).await.id, "barrier");
    assert!(observations[0].flag.load(Ordering::SeqCst));
    assert!(observations[1].flag.load(Ordering::SeqCst));
    assert!(!observations[2].flag.load(Ordering::SeqCst));
    drop(second_write);
    drop(second_read);
    // Reader EOF is asynchronous; release only after observing its flag with a
    // finite deadline, yielding so the connection task can run.
    timeout(LIMIT, async {
        while !observations[2].flag.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(observations);
    for _ in 0..2 {
        let reply = receive(&mut first_read).await;
        assert_eq!(reply.id, "");
        assert_eq!(reply.error.unwrap().code, "cancelled");
    }
    // The existing cancel set is persistent, including for the empty ID.
    send(
        &mut first_write,
        frame("", FrameKind::Req, Some("history.thumbnail"), None),
    )
    .await;
    let reused = receive(&mut first_read).await;
    assert_eq!(reused.error.unwrap().code, "cancelled");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fatal_response_closes_only_its_connection_with_active_context() {
    use clipmonger_ipc::proto::{IpcError, MAX_LINE_BYTES};
    let path = std::env::temp_dir().join(format!(
        "cm-fatal-context-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir(&path).unwrap();
    struct Dir(std::path::PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _dir = Dir(path.clone());
    let sock = path.join("ipc.sock");
    let (seen_tx, mut seen_rx) = tokio::sync::mpsc::channel::<Observation>(2);
    let contextual = ContextHandler {
        method: "history.thumbnail".into(),
        handler: Arc::new(move |params, flag| {
            let (release, wait) = std_mpsc::channel();
            let (done_tx, done) = std_mpsc::channel();
            let mut completion = Completion {
                sender: Some(done_tx),
                success: false,
            };
            let observation = Observation {
                label: params["label"].as_str().unwrap().into(),
                flag,
                release: Some(release),
                done: Some(done),
            };
            if let Err(error) = seen_tx.blocking_send(observation) {
                let mut observation = error.0;
                observation.release.take();
                observation.done.take();
                return Err(IpcError::new(
                    "fixture",
                    "observation receiver closed".into(),
                ));
            }
            wait.recv_timeout(LIMIT)
                .map_err(|_| IpcError::new("timeout", "fixture release".into()))?;
            completion.success = true;
            Ok(json!({"value":true}))
        }),
    };
    let handler: Handler = Arc::new(|method, _| {
        if method == "big" {
            Ok(Value::String("x".repeat(2 * MAX_LINE_BYTES)))
        } else {
            Ok(json!({"pong":true}))
        }
    });
    let _server = Server::bind_with_context(
        &sock,
        "tok".into(),
        handler,
        ServeOptions::default(),
        contextual,
    )
    .await
    .unwrap();
    let (mut first_read, mut first_write) = connect(&sock).await;
    let (mut second_read, mut second_write) = connect(&sock).await;
    send(
        &mut first_write,
        frame(
            "same",
            FrameKind::Req,
            Some("history.thumbnail"),
            Some(json!({"label":"first"})),
        ),
    )
    .await;
    send(
        &mut second_write,
        frame(
            "same",
            FrameKind::Req,
            Some("history.thumbnail"),
            Some(json!({"label":"second"})),
        ),
    )
    .await;
    let mut observations = vec![
        timeout(LIMIT, seen_rx.recv()).await.unwrap().unwrap(),
        timeout(LIMIT, seen_rx.recv()).await.unwrap().unwrap(),
    ];
    observations.sort_by(|a, b| a.label.cmp(&b.label));
    assert_eq!(observations[0].label, "first");
    assert_eq!(observations[1].label, "second");
    let empty = frame("", FrameKind::Req, Some("big"), None);
    let overhead = encode_line(&empty).unwrap().len() - 1;
    let id = "a".repeat(MAX_LINE_BYTES - overhead);
    let fatal = frame(&id, FrameKind::Req, Some("big"), None);
    let response = Envelope {
        v: IPC_VERSION.into(),
        id: id.clone(),
        kind: FrameKind::Res,
        method: None,
        params: None,
        result: Some(Value::String("x".repeat(2 * MAX_LINE_BYTES))),
        error: None,
        seq: None,
    };
    let fallback = Envelope {
        result: None,
        error: Some(IpcError::new(
            "oversize",
            "response exceeds frame limit".into(),
        )),
        ..response.clone()
    };
    assert!(encode_line(&fatal).is_ok());
    assert!(encode_line(&response).is_err());
    assert!(encode_line(&fallback).is_err());
    send(&mut first_write, fatal).await;
    let mut bytes = Vec::new();
    assert_eq!(
        timeout(LIMIT, first_read.read_until(b'\n', &mut bytes))
            .await
            .unwrap()
            .unwrap(),
        0,
        "fatal reply must yield actual EOF"
    );
    assert!(observations[0].flag.load(Ordering::SeqCst));
    assert!(!observations[1].flag.load(Ordering::SeqCst));
    send(
        &mut second_write,
        frame("barrier", FrameKind::Req, Some("ping"), None),
    )
    .await;
    assert_eq!(receive(&mut second_read).await.id, "barrier");
    assert!(!observations[1].flag.load(Ordering::SeqCst));
    drop(observations);
    let reply = receive(&mut second_read).await;
    assert_eq!(reply.id, "same");
    assert!(reply.result.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_completion_preserves_peer_and_later_registration() {
    let path = std::env::temp_dir().join(format!(
        "cm-duplicate-context-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir(&path).unwrap();
    struct Dir(std::path::PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _dir = Dir(path.clone());
    let sock = path.join("ipc.sock");
    let (seen_tx, mut seen_rx) = tokio::sync::mpsc::channel::<Observation>(4);
    let context = ContextHandler {
        method: "history.thumbnail".into(),
        handler: Arc::new(move |params, flag| {
            let (release, wait) = std_mpsc::channel();
            let (done_tx, done) = std_mpsc::channel();
            let mut completion = Completion {
                sender: Some(done_tx),
                success: false,
            };
            let observation = Observation {
                label: params["label"].as_str().unwrap().into(),
                flag,
                release: Some(release),
                done: Some(done),
            };
            if let Err(error) = seen_tx.blocking_send(observation) {
                let mut observation = error.0;
                observation.release.take();
                observation.done.take();
                return Err(clipmonger_ipc::proto::IpcError::new(
                    "fixture",
                    "observation receiver closed".into(),
                ));
            }
            wait.recv_timeout(LIMIT).map_err(|_| {
                clipmonger_ipc::proto::IpcError::new("timeout", "fixture release".into())
            })?;
            completion.success = true;
            Ok(json!({"ok":true}))
        }),
    };
    let handler: Handler = Arc::new(|_, _| Ok(json!({"pong":true})));
    let _server = Server::bind_with_context(
        &sock,
        "tok".into(),
        handler,
        ServeOptions::default(),
        context,
    )
    .await
    .unwrap();
    let (mut read, mut write) = connect(&sock).await;
    for label in ["first", "peer"] {
        send(
            &mut write,
            frame(
                "same",
                FrameKind::Req,
                Some("history.thumbnail"),
                Some(json!({"label":label})),
            ),
        )
        .await;
    }
    let mut observed = vec![
        timeout(LIMIT, seen_rx.recv()).await.unwrap().unwrap(),
        timeout(LIMIT, seen_rx.recv()).await.unwrap().unwrap(),
    ];
    observed.sort_by(|a, b| a.label.cmp(&b.label));
    assert_eq!(observed[0].label, "first");
    assert_eq!(observed[1].label, "peer");
    let completed = observed.remove(0);
    let completed_flag = Arc::clone(&completed.flag);
    drop(completed);
    let response = receive(&mut read).await;
    assert_eq!(response.id, "same");
    assert!(response.result.is_some());
    send(
        &mut write,
        frame(
            "same",
            FrameKind::Req,
            Some("history.thumbnail"),
            Some(json!({"label":"later"})),
        ),
    )
    .await;
    let later = timeout(LIMIT, seen_rx.recv()).await.unwrap().unwrap();
    assert_eq!(later.label, "later");
    assert!(!Arc::ptr_eq(&observed[0].flag, &later.flag));
    assert!(!Arc::ptr_eq(&completed_flag, &later.flag));
    send(&mut write, frame("same", FrameKind::Cancel, None, None)).await;
    send(
        &mut write,
        frame("barrier", FrameKind::Req, Some("ping"), None),
    )
    .await;
    assert_eq!(receive(&mut read).await.id, "barrier");
    assert!(!completed_flag.load(Ordering::SeqCst));
    assert!(observed[0].flag.load(Ordering::SeqCst));
    assert!(later.flag.load(Ordering::SeqCst));
    drop(later);
    drop(observed);
    for _ in 0..2 {
        let reply = receive(&mut read).await;
        assert_eq!(reply.id, "same");
        assert_eq!(reply.error.unwrap().code, "cancelled");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initial_hello_is_special_even_when_declared_context_method() {
    let path = std::env::temp_dir().join(format!(
        "cm-hello-context-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir(&path).unwrap();
    struct Dir(std::path::PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _dir = Dir(path.clone());
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let context_calls = Arc::clone(&calls);
    let context = ContextHandler {
        method: "hello".into(),
        handler: Arc::new(move |_, _| {
            context_calls.fetch_add(1, Ordering::SeqCst);
            Ok(Value::Null)
        }),
    };
    let handler: Handler = Arc::new(|_, _| Ok(Value::Null));
    let sock = path.join("ipc.sock");
    let _server = Server::bind_with_context(
        &sock,
        "tok".into(),
        handler,
        ServeOptions::default(),
        context,
    )
    .await
    .unwrap();
    let (mut read, mut write) = connect(&sock).await;
    send(
        &mut write,
        frame(
            "second-hello",
            FrameKind::Req,
            Some("hello"),
            Some(json!({"token":"tok"})),
        ),
    )
    .await;
    assert_eq!(receive(&mut read).await.id, "second-hello");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
