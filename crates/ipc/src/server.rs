//! Tokio Unix-socket JSONL server plus the in-memory subscriber event bus.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::proto::{Envelope, FrameKind, IpcError, IPC_VERSION};

#[cfg(unix)]
use crate::framing::{decode_line, encode_line};
#[cfg(unix)]
use crate::proto::MAX_LINE_BYTES;
#[cfg(unix)]
use std::collections::HashSet;
#[cfg(unix)]
use std::sync::{Arc, Mutex};

/// Request handler: method name + params to result or typed error.
pub type Handler = std::sync::Arc<dyn Fn(String, Value) -> Result<Value, IpcError> + Send + Sync>;

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

/// Tokio JSONL server bound to one Unix socket path.
pub struct Server {
    #[cfg(unix)]
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
        let _ = std::fs::remove_file(sock);
        let listener = tokio::net::UnixListener::bind(sock)
            .map_err(|e| IpcError::new("bind_failed", e.to_string()))?;
        let task = tokio::spawn(accept_loop(listener, token, handler));
        Ok(Self { _task: task })
    }

    /// Bind stub on platforms without Unix sockets.
    ///
    /// # Errors
    ///
    /// Always fails with `unavailable`.
    #[cfg(not(unix))]
    pub async fn bind(_sock: &Path, _token: String, _handler: Handler) -> Result<Self, IpcError> {
        Err(IpcError::new(
            "unavailable",
            "ipc server needs a Unix socket".to_string(),
        ))
    }
}

#[cfg(unix)]
type SharedWriter = Arc<tokio::sync::Mutex<tokio::net::unix::OwnedWriteHalf>>;
#[cfg(unix)]
type SharedCancels = Arc<Mutex<HashSet<String>>>;

#[cfg(unix)]
async fn accept_loop(listener: tokio::net::UnixListener, token: String, handler: Handler) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        tokio::spawn(serve_conn(stream, token.clone(), handler.clone()));
    }
}

#[cfg(unix)]
async fn serve_conn(stream: tokio::net::UnixStream, token: String, handler: Handler) {
    let (read_half, write_half) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(read_half);
    let writer: SharedWriter = Arc::new(tokio::sync::Mutex::new(write_half));
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
    let cancelled: SharedCancels = Arc::new(Mutex::new(HashSet::new()));
    while let Some(frame) = next_frame(&mut reader).await {
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
                    answer_hello(&writer, &env, &token).await;
                } else {
                    dispatch(writer.clone(), cancelled.clone(), handler.clone(), env);
                }
            }
            FrameKind::Res | FrameKind::Event => {}
        }
    }
}

#[cfg(unix)]
fn hello_token_ok(hello: &Envelope, token: &str) -> bool {
    hello
        .params
        .as_ref()
        .and_then(|params| params.get("token"))
        .and_then(Value::as_str)
        == Some(token)
}

#[cfg(unix)]
async fn answer_hello(writer: &SharedWriter, hello: &Envelope, token: &str) {
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

#[cfg(unix)]
fn dispatch(writer: SharedWriter, cancelled: SharedCancels, handler: Handler, req: Envelope) {
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

#[cfg(unix)]
fn is_cancelled(cancelled: &SharedCancels, id: &str) -> bool {
    match cancelled.lock() {
        Ok(set) => set.contains(id),
        Err(_) => false,
    }
}

#[cfg(unix)]
fn cancelled_error() -> IpcError {
    IpcError::new("cancelled", "request cancelled".to_string())
}

#[cfg(unix)]
async fn send_res(writer: &SharedWriter, id: &str, result: Option<Value>, error: Option<IpcError>) {
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

#[cfg(unix)]
async fn next_frame(
    reader: &mut tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
) -> Option<Result<Envelope, IpcError>> {
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
