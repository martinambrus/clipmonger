//! Tokio Unix-socket JSONL server plus the in-memory subscriber event bus.

use std::collections::{HashMap, VecDeque};
#[cfg(not(windows))]
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::proto::{Envelope, FrameKind, IpcError, IPC_VERSION};

#[cfg(any(unix, windows))]
use crate::framing::{decode_line, encode_line};
#[cfg(any(unix, windows))]
use crate::proto::MAX_LINE_BYTES;
#[cfg(any(unix, windows))]
use std::collections::HashSet;
#[cfg(any(unix, windows))]
use std::sync::{Arc, Mutex};

/// Request handler: method name + params to result or typed error.
pub type Handler = std::sync::Arc<dyn Fn(String, Value) -> Result<Value, IpcError> + Send + Sync>;

/// Connection lifecycle event fired for a connection that completed hello.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LifecycleEvent {
    /// Hello succeeded; `client` is the hello `client` param (may be empty).
    Hello {
        /// Free-form client name from the hello params.
        client: String,
    },
    /// A hello-completed connection ended.
    Disconnected {
        /// Free-form client name from the hello params.
        client: String,
    },
}

/// Callback invoked on connection lifecycle events.
pub type LifecycleCb = std::sync::Arc<dyn Fn(LifecycleEvent) + Send + Sync>;

/// Per-subscriber queue cap before a `resync-required` marker is appended.
const QUEUE_CAP: usize = 128;
/// Overflow marker method pushed when a subscriber falls behind.
const RESYNC_METHOD: &str = "resync-required";

/// Single subscriber queue with idle tracking.
struct Subscriber {
    topics: Vec<String>,
    queue: VecDeque<Envelope>,
    last_active: Instant,
    resync_sent: bool,
}

/// In-memory fan-out with per-subscriber caps and injected-clock eviction.
pub struct EventBus {
    idle_limit: Duration,
    next_id: u64,
    subs: HashMap<u64, Subscriber>,
}

impl EventBus {
    /// Create a bus evicting subscribers idle longer than `idle_limit`.
    #[must_use]
    pub fn new(idle_limit: Duration) -> Self {
        Self {
            idle_limit,
            next_id: 0,
            subs: HashMap::new(),
        }
    }

    /// Subscribe to `topics`; returns the subscriber id.
    #[must_use]
    pub fn subscribe(&mut self, topics: Vec<String>) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.subs.insert(
            id,
            Subscriber {
                topics,
                queue: VecDeque::new(),
                last_active: Instant::now(),
                resync_sent: false,
            },
        );
        id
    }

    /// Fan out one event frame to subscribers of its method topic.
    #[allow(clippy::needless_pass_by_value)]
    pub fn publish(&mut self, event: Envelope) {
        let topic = event.method.clone().unwrap_or_default();
        for sub in self.subs.values_mut() {
            if !sub.topics.contains(&topic) {
                continue;
            }
            if sub.queue.len() >= QUEUE_CAP {
                if !sub.resync_sent {
                    sub.queue.push_back(resync_event());
                    sub.resync_sent = true;
                }
                continue;
            }
            sub.queue.push_back(event.clone());
        }
    }

    /// Drain a subscriber's queued events; unknown ids drain to empty.
    #[must_use]
    pub fn pending(&mut self, id: u64) -> Vec<Envelope> {
        match self.subs.get_mut(&id) {
            Some(sub) => {
                sub.last_active = Instant::now();
                sub.resync_sent = false;
                sub.queue.drain(..).collect()
            }
            None => Vec::new(),
        }
    }

    /// Evict subscribers idle past the limit as of `now`.
    pub fn tick(&mut self, now: Instant) {
        self.subs
            .retain(|_, sub| now.saturating_duration_since(sub.last_active) <= self.idle_limit);
    }
}

/// Overflow marker: an event frame with method `resync-required`.
fn resync_event() -> Envelope {
    Envelope {
        v: IPC_VERSION.to_string(),
        id: RESYNC_METHOD.to_string(),
        kind: FrameKind::Event,
        method: Some(RESYNC_METHOD.to_string()),
        params: None,
        result: None,
        error: None,
        seq: None,
    }
}

/// Tokio JSONL server bound to one Unix socket path or Windows named pipe.
pub struct Server {
    #[cfg(any(unix, windows))]
    _task: tokio::task::JoinHandle<()>,
}

impl Server {
    /// Bind `sock` and serve `handler` concurrently per connection.
    ///
    /// # Errors
    ///
    /// Returns `bind_failed` when the socket cannot be bound.
    #[cfg(unix)]
    #[allow(clippy::unused_async, clippy::unused_async_trait_impl)]
    pub async fn bind(sock: &Path, token: String, handler: Handler) -> Result<Self, IpcError> {
        Self::bind_with_lifecycle(sock, token, handler, None).await
    }

    /// Bind `sock` like [`Server::bind`], invoking `lifecycle` on hello
    /// success and on disconnect of hello-completed connections.
    ///
    /// # Errors
    ///
    /// Returns `bind_failed` when the socket cannot be bound.
    #[cfg(unix)]
    #[allow(clippy::unused_async, clippy::unused_async_trait_impl)]
    pub async fn bind_with_lifecycle(
        sock: &Path,
        token: String,
        handler: Handler,
        lifecycle: Option<LifecycleCb>,
    ) -> Result<Self, IpcError> {
        let _ = std::fs::remove_file(sock);
        let listener = tokio::net::UnixListener::bind(sock)
            .map_err(|e| IpcError::new("bind_failed", e.to_string()))?;
        let task = tokio::spawn(accept_loop(listener, token, handler, lifecycle));
        Ok(Self { _task: task })
    }

    /// Bind `pipe_name` and serve `handler` concurrently per connection.
    ///
    /// The first instance claims the name, so a second `bind_pipe` on the
    /// same name fails with `bind_failed`.
    ///
    /// # Errors
    ///
    /// Returns `bind_failed` when the pipe name is taken or cannot be bound.
    #[cfg(windows)]
    pub fn bind_pipe(pipe_name: &str, token: String, handler: Handler) -> Result<Self, IpcError> {
        Self::bind_pipe_with_lifecycle(pipe_name, token, handler, None)
    }

    /// Bind `pipe_name` like [`Server::bind_pipe`], with a lifecycle callback.
    ///
    /// # Errors
    ///
    /// Returns `bind_failed` when the pipe name is taken or cannot be bound.
    #[cfg(windows)]
    pub fn bind_pipe_with_lifecycle(
        pipe_name: &str,
        token: String,
        handler: Handler,
        lifecycle: Option<LifecycleCb>,
    ) -> Result<Self, IpcError> {
        let first = make_pipe_instance(pipe_name, true)?;
        let task = tokio::spawn(windows_accept_loop(
            pipe_name.to_string(),
            token,
            handler,
            first,
            lifecycle,
        ));
        Ok(Self { _task: task })
    }

    /// Bind stub on platforms without Unix sockets or named pipes.
    ///
    /// # Errors
    ///
    /// Always fails with `unavailable`.
    #[cfg(not(any(unix, windows)))]
    pub async fn bind(_sock: &Path, _token: String, _handler: Handler) -> Result<Self, IpcError> {
        Err(IpcError::new(
            "unavailable",
            "ipc server needs a Unix socket".to_string(),
        ))
    }

    /// Lifecycle-binding stub on platforms without sockets or named pipes.
    ///
    /// # Errors
    ///
    /// Always fails with `unavailable`.
    #[cfg(not(any(unix, windows)))]
    pub async fn bind_with_lifecycle(
        _sock: &Path,
        _token: String,
        _handler: Handler,
        _lifecycle: Option<LifecycleCb>,
    ) -> Result<Self, IpcError> {
        Err(IpcError::new(
            "unavailable",
            "ipc server needs a Unix socket".to_string(),
        ))
    }
}

#[cfg(any(unix, windows))]
type SharedWriter<S> = Arc<tokio::sync::Mutex<tokio::io::WriteHalf<S>>>;
#[cfg(any(unix, windows))]
type SharedCancels = Arc<Mutex<HashSet<String>>>;

#[cfg(unix)]
async fn accept_loop(
    listener: tokio::net::UnixListener,
    token: String,
    handler: Handler,
    lifecycle: Option<LifecycleCb>,
) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        tokio::spawn(serve_conn(
            stream,
            token.clone(),
            handler.clone(),
            lifecycle.clone(),
        ));
    }
}

#[cfg(windows)]
fn make_pipe_instance(
    pipe_name: &str,
    first: bool,
) -> Result<tokio::net::windows::named_pipe::NamedPipeServer, IpcError> {
    let mut opts = tokio::net::windows::named_pipe::ServerOptions::new();
    opts.first_pipe_instance(first)
        .reject_remote_clients(true)
        .pipe_mode(tokio::net::windows::named_pipe::PipeMode::Byte);
    opts.create(pipe_name)
        .map_err(|e| IpcError::new("bind_failed", e.to_string()))
}

#[cfg(windows)]
async fn windows_accept_loop(
    pipe_name: String,
    token: String,
    handler: Handler,
    mut listener: tokio::net::windows::named_pipe::NamedPipeServer,
    lifecycle: Option<LifecycleCb>,
) {
    loop {
        if listener.connect().await.is_err() {
            let _ = listener.disconnect();
            continue;
        }
        match make_pipe_instance(&pipe_name, false) {
            Ok(next) => {
                let connected = std::mem::replace(&mut listener, next);
                tokio::spawn(serve_conn(
                    connected,
                    token.clone(),
                    handler.clone(),
                    lifecycle.clone(),
                ));
            }
            Err(_) => {
                let _ = listener.disconnect();
            }
        }
    }
}

#[cfg(any(unix, windows))]
async fn serve_conn<S>(stream: S, token: String, handler: Handler, lifecycle: Option<LifecycleCb>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (read_half, write_half) = tokio::io::split(stream);
    let mut reader = tokio::io::BufReader::new(read_half);
    let writer: SharedWriter<S> = Arc::new(tokio::sync::Mutex::new(write_half));
    let Some(Ok(hello)) = next_frame(&mut reader).await else {
        return;
    };
    if hello.kind != FrameKind::Req || hello.method.as_deref() != Some("hello") {
        return;
    }
    answer_hello(&writer, &hello, &token).await;
    if !hello_token_ok(&hello, &token) {
        return;
    }
    let client = hello
        .params
        .as_ref()
        .and_then(|params| params.get("client"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if let Some(cb) = &lifecycle {
        cb(LifecycleEvent::Hello {
            client: client.clone(),
        });
    }
    serve_requests(&mut reader, &writer, &token, handler, client.clone()).await;
    if let Some(cb) = &lifecycle {
        cb(LifecycleEvent::Disconnected { client });
    }
}

#[cfg(any(unix, windows))]
async fn serve_requests<S, R>(
    reader: &mut tokio::io::BufReader<R>,
    writer: &SharedWriter<S>,
    token: &str,
    handler: Handler,
    _client: String,
) where
    S: tokio::io::AsyncWrite + Unpin + Send + 'static,
    R: tokio::io::AsyncRead + Unpin,
{
    let cancelled: SharedCancels = Arc::new(Mutex::new(HashSet::new()));
    while let Some(frame) = next_frame(reader).await {
        let env = match frame {
            Ok(env) => env,
            Err(e) if e.code == "oversize" => return,
            Err(_) => continue,
        };
        match env.kind {
            FrameKind::Cancel => {
                if let Ok(mut set) = cancelled.lock() {
                    set.insert(env.id);
                }
            }
            FrameKind::Req => {
                if env.method.as_deref() == Some("hello") {
                    answer_hello(writer, &env, token).await;
                } else {
                    dispatch((*writer).clone(), cancelled.clone(), handler.clone(), env);
                }
            }
            FrameKind::Res | FrameKind::Event => {}
        }
    }
}

#[cfg(any(unix, windows))]
fn hello_token_ok(hello: &Envelope, token: &str) -> bool {
    hello
        .params
        .as_ref()
        .and_then(|params| params.get("token"))
        .and_then(Value::as_str)
        == Some(token)
}

#[cfg(any(unix, windows))]
async fn answer_hello<S>(writer: &SharedWriter<S>, hello: &Envelope, token: &str)
where
    S: tokio::io::AsyncWrite + Unpin,
{
    if hello_token_ok(hello, token) {
        let welcome = serde_json::json!({
            "chosen": 1,
            "daemon_version": env!("CARGO_PKG_VERSION"),
        });
        send_res(writer, &hello.id, Some(welcome), None).await;
    } else {
        let denied = IpcError {
            code: "auth_failed".to_string(),
            message: "bad token".to_string(),
            retryable: false,
            backend: None,
        };
        send_res(writer, &hello.id, None, Some(denied)).await;
    }
}

#[cfg(any(unix, windows))]
fn dispatch<S>(writer: SharedWriter<S>, cancelled: SharedCancels, handler: Handler, req: Envelope)
where
    S: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let id = req.id.clone();
        let method = req.method.clone().unwrap_or_default();
        let params = req.params.clone().unwrap_or(Value::Null);
        if is_cancelled(&cancelled, &id) {
            send_res(&writer, &id, None, Some(cancelled_error())).await;
            return;
        }
        let outcome = tokio::task::spawn_blocking(move || handler(method, params)).await;
        if is_cancelled(&cancelled, &id) {
            send_res(&writer, &id, None, Some(cancelled_error())).await;
            return;
        }
        match outcome {
            Ok(Ok(value)) => send_res(&writer, &id, Some(value), None).await,
            Ok(Err(error)) => send_res(&writer, &id, None, Some(error)).await,
            Err(_) => {
                let failed = IpcError::new("internal", "handler task failed".to_string());
                send_res(&writer, &id, None, Some(failed)).await;
            }
        }
    });
}

#[cfg(any(unix, windows))]
fn is_cancelled(cancelled: &SharedCancels, id: &str) -> bool {
    match cancelled.lock() {
        Ok(set) => set.contains(id),
        Err(_) => false,
    }
}

#[cfg(any(unix, windows))]
fn cancelled_error() -> IpcError {
    IpcError::new("cancelled", "request cancelled".to_string())
}

#[cfg(any(unix, windows))]
async fn send_res<S>(
    writer: &SharedWriter<S>,
    id: &str,
    result: Option<Value>,
    error: Option<IpcError>,
) where
    S: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt as _;

    let env = Envelope {
        v: IPC_VERSION.to_string(),
        id: id.to_string(),
        kind: FrameKind::Res,
        method: None,
        params: None,
        result,
        error,
        seq: None,
    };
    let Ok(bytes) = encode_line(&env) else {
        return;
    };
    let mut guard = writer.lock().await;
    let _ = guard.write_all(&bytes).await;
}

#[cfg(any(unix, windows))]
async fn next_frame<R>(reader: &mut tokio::io::BufReader<R>) -> Option<Result<Envelope, IpcError>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncBufReadExt as _;

    let mut line = Vec::new();
    let count = reader.read_until(b'\n', &mut line).await.ok()?;
    if count == 0 {
        return None;
    }
    if line.len() > MAX_LINE_BYTES + 1 {
        return Some(Err(IpcError::new(
            "oversize",
            "line exceeds cap".to_string(),
        )));
    }
    Some(decode_line(&line))
}

#[cfg(all(test, unix))]
mod tests {
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

    use super::{LifecycleEvent, Server};
    use crate::framing::encode_line;
    use crate::proto::{Envelope, FrameKind, IPC_VERSION};

    #[tokio::test(flavor = "multi_thread")]
    async fn lifecycle_fires_hello_and_disconnect() {
        let dir = std::env::temp_dir().join(format!("cm-lifecycle-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sock = dir.join("lc.sock");
        let events: Arc<Mutex<Vec<LifecycleEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&events);
        let cb = Arc::new(move |event: LifecycleEvent| {
            if let Ok(mut guard) = seen.lock() {
                guard.push(event);
            }
        });
        let handler: super::Handler =
            Arc::new(|_method, _params| Ok(serde_json::json!({"ok": true})));
        let server = Server::bind_with_lifecycle(&sock, "tok".to_string(), handler, Some(cb))
            .await
            .expect("bind");
        let mut stream = tokio::net::UnixStream::connect(&sock)
            .await
            .expect("connect");
        let hello = Envelope {
            v: IPC_VERSION.to_string(),
            id: "h1".to_string(),
            kind: FrameKind::Req,
            method: Some("hello".to_string()),
            params: Some(serde_json::json!({"token": "tok", "client": "gnome-ext"})),
            result: None,
            error: None,
            seq: None,
        };
        stream
            .write_all(&encode_line(&hello).expect("encode"))
            .await
            .expect("write hello");
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.expect("read welcome");
        assert!(line.contains("chosen"), "welcome expected, got {line}");
        // Bad-token connection must not fire lifecycle events.
        let mut bad = tokio::net::UnixStream::connect(&sock)
            .await
            .expect("connect bad");
        let bad_hello = Envelope {
            params: Some(serde_json::json!({"token": "nope", "client": "gnome-ext"})),
            ..hello.clone()
        };
        bad.write_all(&encode_line(&bad_hello).expect("encode"))
            .await
            .expect("write bad hello");
        let mut bad_reader = BufReader::new(bad);
        let mut bad_line = String::new();
        let _ = bad_reader.read_line(&mut bad_line).await;
        drop(bad_reader);
        drop(reader);
        for _ in 0..100 {
            let done = events.lock().is_ok_and(|guard| {
                guard.contains(&LifecycleEvent::Disconnected {
                    client: "gnome-ext".to_string(),
                })
            });
            if done {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let guard = events.lock().expect("events lock");
        assert_eq!(
            guard.first(),
            Some(&LifecycleEvent::Hello {
                client: "gnome-ext".to_string()
            }),
            "hello must fire first, got {guard:?}"
        );
        assert_eq!(
            guard
                .iter()
                .filter(|e| matches!(e, LifecycleEvent::Hello { .. }))
                .count(),
            1,
            "bad-token hello must not fire, got {guard:?}"
        );
        assert!(
            guard.contains(&LifecycleEvent::Disconnected {
                client: "gnome-ext".to_string()
            }),
            "disconnect must fire, got {guard:?}"
        );
        drop(guard);
        drop(server);
        let _ = std::fs::remove_file(&sock);
    }
}
