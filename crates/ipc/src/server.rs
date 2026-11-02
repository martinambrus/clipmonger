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

    /// Drop a subscriber and its queued events.
    pub fn unsubscribe(&mut self, id: u64) {
        self.subs.remove(&id);
    }

    /// Number of subscribers listening on `topic`.
    #[must_use]
    pub fn subscriber_count(&self, topic: &str) -> usize {
        self.subs
            .values()
            .filter(|sub| sub.topics.iter().any(|t| t == topic))
            .count()
    }
}

/// Subscription rule for one event topic.
#[derive(Clone, Debug)]
pub struct TopicRule {
    /// Topic name (the event frame's `method`).
    pub topic: String,
    /// At most one subscriber at a time; later ones get `already_subscribed`.
    pub exclusive: bool,
}

/// Daemon policy for one subscription: topic, the subscriber's OS process id
/// (when known) and its `events.subscribe` params.
pub type SubscribeGuard = std::sync::Arc<dyn Fn(&str, Option<u32>, &Value) -> bool + Send + Sync>;

/// One live subscriber as the daemon sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct SubscriberInfo {
    /// OS process id of the subscribed connection's peer, when known.
    pub peer_pid: Option<u32>,
    /// The `events.subscribe` params the subscriber sent.
    pub params: Value,
}

/// How long an event published to nobody waits for its first subscriber.
const HOLD_TTL: Duration = Duration::from_secs(10);

/// Server-side fan-out of daemon events to `events.subscribe` connections.
/// Clones share the same subscribers.
#[derive(Clone)]
pub struct EventHub {
    inner: std::sync::Arc<std::sync::Mutex<HubInner>>,
}

struct HubInner {
    bus: EventBus,
    rules: Vec<TopicRule>,
    guard: Option<SubscribeGuard>,
    subs: HashMap<u64, HubSub>,
    held: HashMap<String, (Envelope, Instant)>,
    hold_ttl: Duration,
    next_event: u64,
}

struct HubSub {
    topics: Vec<String>,
    wake: std::sync::Arc<tokio::sync::Notify>,
    info: SubscriberInfo,
}

impl EventHub {
    /// Hub serving only the topics in `rules`.
    #[must_use]
    pub fn new(rules: Vec<TopicRule>) -> Self {
        Self {
            inner: std::sync::Arc::new(std::sync::Mutex::new(HubInner {
                // Subscribers live as long as their connection; no idle eviction.
                bus: EventBus::new(Duration::MAX),
                rules,
                guard: None,
                subs: HashMap::new(),
                held: HashMap::new(),
                hold_ttl: HOLD_TTL,
                next_event: 0,
            })),
        }
    }

    /// Check every subscription with `guard` before it is accepted.
    #[must_use]
    pub fn with_guard(self, guard: SubscribeGuard) -> Self {
        if let Ok(mut inner) = self.inner.lock() {
            inner.guard = Some(guard);
        }
        self
    }

    /// Override how long a held event waits for a subscriber.
    #[must_use]
    pub fn with_hold_ttl(self, ttl: Duration) -> Self {
        if let Ok(mut inner) = self.inner.lock() {
            inner.hold_ttl = ttl;
        }
        self
    }

    /// Publish `params` on `topic`; returns how many subscribers got it.
    #[must_use]
    pub fn publish(&self, topic: &str, params: Value) -> usize {
        self.send(topic, params, false)
    }

    /// Like [`EventHub::publish`], but with nobody subscribed the event is
    /// held and handed to the next subscriber of `topic` (returns 0 then).
    #[must_use]
    pub fn publish_or_hold(&self, topic: &str, params: Value) -> usize {
        self.send(topic, params, true)
    }

    /// Live subscribers of `topic`.
    #[must_use]
    pub fn subscribers(&self, topic: &str) -> Vec<SubscriberInfo> {
        let Ok(inner) = self.inner.lock() else {
            return Vec::new();
        };
        inner
            .subs
            .values()
            .filter(|sub| sub.topics.iter().any(|t| t == topic))
            .map(|sub| sub.info.clone())
            .collect()
    }

    fn send(&self, topic: &str, params: Value, hold: bool) -> usize {
        let Ok(mut inner) = self.inner.lock() else {
            return 0;
        };
        let env = inner.event(topic, params);
        let count = inner.bus.subscriber_count(topic);
        if count == 0 {
            if hold {
                inner.held.insert(topic.to_string(), (env, Instant::now()));
            }
            return 0;
        }
        inner.bus.publish(env);
        for sub in inner.subs.values() {
            if sub.topics.iter().any(|t| t == topic) {
                sub.wake.notify_one();
            }
        }
        count
    }

    /// Register a subscriber; the guard runs outside the hub lock.
    fn subscribe(
        &self,
        topics: Vec<String>,
        peer_pid: Option<u32>,
        params: Value,
    ) -> Result<(u64, std::sync::Arc<tokio::sync::Notify>), IpcError> {
        if topics.is_empty() {
            return Err(IpcError::new(
                "invalid_params",
                "events.subscribe needs topics".to_string(),
            ));
        }
        let guard = {
            let inner = self.lock_inner()?;
            for topic in &topics {
                inner.rule(topic)?;
            }
            inner.guard.clone()
        };
        if let Some(guard) = guard {
            if !topics.iter().all(|topic| guard(topic, peer_pid, &params)) {
                return Err(IpcError::new("denied", "subscription refused".to_string()));
            }
        }
        let mut inner = self.lock_inner()?;
        for topic in &topics {
            if inner.rule(topic)?.exclusive && inner.bus.subscriber_count(topic) > 0 {
                return Err(IpcError::new(
                    "already_subscribed",
                    format!("{topic} already has a subscriber"),
                ));
            }
        }
        let id = inner.bus.subscribe(topics.clone());
        let wake = std::sync::Arc::new(tokio::sync::Notify::new());
        let now = Instant::now();
        let ttl = inner.hold_ttl;
        for topic in &topics {
            if let Some((env, at)) = inner.held.remove(topic) {
                if now.saturating_duration_since(at) <= ttl {
                    inner.bus.publish(env);
                }
            }
        }
        inner.subs.insert(
            id,
            HubSub {
                topics,
                wake: std::sync::Arc::clone(&wake),
                info: SubscriberInfo { peer_pid, params },
            },
        );
        wake.notify_one();
        Ok((id, wake))
    }

    fn unsubscribe(&self, id: u64) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.bus.unsubscribe(id);
            inner.subs.remove(&id);
        }
    }

    fn drain(&self, id: u64) -> Vec<Envelope> {
        self.inner
            .lock()
            .map(|mut inner| inner.bus.pending(id))
            .unwrap_or_default()
    }

    fn lock_inner(&self) -> Result<std::sync::MutexGuard<'_, HubInner>, IpcError> {
        self.inner
            .lock()
            .map_err(|_| IpcError::new("internal", "event hub lock poisoned".to_string()))
    }
}

impl HubInner {
    fn rule(&self, topic: &str) -> Result<&TopicRule, IpcError> {
        self.rules
            .iter()
            .find(|rule| rule.topic == topic)
            .ok_or_else(|| IpcError::new("unknown_topic", format!("no events for {topic}")))
    }

    fn event(&mut self, topic: &str, params: Value) -> Envelope {
        self.next_event = self.next_event.wrapping_add(1);
        Envelope {
            v: IPC_VERSION.to_string(),
            id: format!("e{}", self.next_event),
            kind: FrameKind::Event,
            method: Some(topic.to_string()),
            params: Some(params),
            result: None,
            error: None,
            seq: None,
        }
    }
}

/// Resolves a connected named pipe's client process id from its raw handle
/// (Windows; supplied by the daemon, which owns the Win32 bindings).
pub type PipePeerPid = std::sync::Arc<dyn Fn(usize) -> Option<u32> + Send + Sync>;

/// Optional server features.
#[derive(Clone, Default)]
pub struct ServeOptions {
    /// Connection lifecycle callback.
    pub lifecycle: Option<LifecycleCb>,
    /// Event fan-out for `events.subscribe`; without it the method reaches
    /// the handler like any other.
    pub hub: Option<EventHub>,
    /// Windows: client process id of a connected pipe.
    pub pipe_peer_pid: Option<PipePeerPid>,
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
        let options = ServeOptions {
            lifecycle,
            ..ServeOptions::default()
        };
        Self::bind_with(sock, token, handler, options).await
    }

    /// Bind `sock` like [`Server::bind`] with optional server features.
    ///
    /// # Errors
    ///
    /// Returns `bind_failed` when the socket cannot be bound.
    #[cfg(unix)]
    #[allow(clippy::unused_async, clippy::unused_async_trait_impl)]
    pub async fn bind_with(
        sock: &Path,
        token: String,
        handler: Handler,
        options: ServeOptions,
    ) -> Result<Self, IpcError> {
        let _ = std::fs::remove_file(sock);
        let listener = tokio::net::UnixListener::bind(sock)
            .map_err(|e| IpcError::new("bind_failed", e.to_string()))?;
        let task = tokio::spawn(accept_loop(listener, token, handler, options));
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
        let options = ServeOptions {
            lifecycle,
            ..ServeOptions::default()
        };
        Self::bind_pipe_with(pipe_name, token, handler, options)
    }

    /// Bind `pipe_name` like [`Server::bind_pipe`] with optional server
    /// features.
    ///
    /// # Errors
    ///
    /// Returns `bind_failed` when the pipe name is taken or cannot be bound.
    #[cfg(windows)]
    pub fn bind_pipe_with(
        pipe_name: &str,
        token: String,
        handler: Handler,
        options: ServeOptions,
    ) -> Result<Self, IpcError> {
        let first = make_pipe_instance(pipe_name, true)?;
        let task = tokio::spawn(windows_accept_loop(
            pipe_name.to_string(),
            token,
            handler,
            first,
            options,
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

    /// Options-binding stub on platforms without sockets or named pipes.
    ///
    /// # Errors
    ///
    /// Always fails with `unavailable`.
    #[cfg(not(any(unix, windows)))]
    pub async fn bind_with(
        _sock: &Path,
        _token: String,
        _handler: Handler,
        _options: ServeOptions,
    ) -> Result<Self, IpcError> {
        Err(IpcError::new(
            "unavailable",
            "ipc server needs a Unix socket".to_string(),
        ))
    }
}

#[cfg(any(unix, windows))]
struct SharedWriterInner<S> {
    slot: tokio::sync::Mutex<Option<tokio::io::WriteHalf<S>>>,
    closed: tokio::sync::watch::Sender<bool>,
}

#[cfg(any(unix, windows))]
struct SharedWriter<S> {
    inner: Arc<SharedWriterInner<S>>,
}

#[cfg(any(unix, windows))]
impl<S> Clone for SharedWriter<S> {
    fn clone(&self) -> Self {
        Self { inner: Arc::clone(&self.inner) }
    }
}

#[cfg(any(unix, windows))]
impl<S> SharedWriter<S> {
    fn new(half: tokio::io::WriteHalf<S>) -> Self {
        let (closed, _) = tokio::sync::watch::channel(false);
        Self {
            inner: Arc::new(SharedWriterInner {
                slot: tokio::sync::Mutex::new(Some(half)),
                closed,
            }),
        }
    }

    fn is_closed(&self) -> bool {
        *self.inner.closed.borrow()
    }

    fn trigger_close(&self) {
        self.inner.closed.send_replace(true);
    }

    async fn wait_closed(&self) {
        let mut closed = self.inner.closed.subscribe();
        while !*closed.borrow_and_update() {
            if closed.changed().await.is_err() {
                return;
            }
        }
    }

    async fn write_bytes(&self, bytes: &[u8]) -> Result<(), ()>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        use tokio::io::AsyncWriteExt as _;
        if self.is_closed() {
            return Err(());
        }
        let mut guard = tokio::select! {
            biased;
            _ = self.wait_closed() => return Err(()),
            g = self.inner.slot.lock() => g,
        };
        if self.is_closed() {
            return Err(());
        }
        let Some(half) = guard.as_mut() else {
            return Err(());
        };
        let res = tokio::select! {
            biased;
            _ = self.wait_closed() => return Err(()),
            r = half.write_all(bytes) => r,
        };
        match res {
            Ok(()) => {
                let flush_res = tokio::select! {
                    biased;
                    _ = self.wait_closed() => return Err(()),
                    r = half.flush() => r,
                };
                match flush_res {
                    Ok(()) => {
                        if self.is_closed() {
                            Err(())
                        } else {
                            Ok(())
                        }
                    }
                    Err(_) => {
                        self.trigger_close();
                        Err(())
                    }
                }
            }
            Err(_) => {
                self.trigger_close();
                Err(())
            }
        }
    }

    async fn close_and_take(&self) {
        self.trigger_close();
        let mut guard = self.inner.slot.lock().await;
        guard.take();
    }
}

#[cfg(any(unix, windows))]
type SharedCancels = Arc<Mutex<HashSet<String>>>;

#[cfg(unix)]
async fn accept_loop(
    listener: tokio::net::UnixListener,
    token: String,
    handler: Handler,
    options: ServeOptions,
) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let peer_pid = stream
            .peer_cred()
            .ok()
            .and_then(|cred| cred.pid())
            .and_then(|pid| u32::try_from(pid).ok());
        tokio::spawn(serve_conn(
            stream,
            token.clone(),
            handler.clone(),
            options.clone(),
            peer_pid,
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
    options: ServeOptions,
) {
    use std::os::windows::io::AsRawHandle as _;
    loop {
        if listener.connect().await.is_err() {
            let _ = listener.disconnect();
            continue;
        }
        match make_pipe_instance(&pipe_name, false) {
            Ok(next) => {
                let connected = std::mem::replace(&mut listener, next);
                let peer_pid = options
                    .pipe_peer_pid
                    .as_ref()
                    .and_then(|resolve| resolve(connected.as_raw_handle() as usize));
                tokio::spawn(serve_conn(
                    connected,
                    token.clone(),
                    handler.clone(),
                    options.clone(),
                    peer_pid,
                ));
            }
            Err(_) => {
                let _ = listener.disconnect();
            }
        }
    }
}

#[cfg(any(unix, windows))]
async fn serve_conn<S>(
    stream: S,
    token: String,
    handler: Handler,
    options: ServeOptions,
    peer_pid: Option<u32>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let lifecycle = options.lifecycle;
    let (read_half, write_half) = tokio::io::split(stream);
    let mut reader = tokio::io::BufReader::new(read_half);
    let writer = SharedWriter::new(write_half);
    let Some(Ok(hello)) = next_frame(&mut reader).await else {
        writer.trigger_close();
        writer.close_and_take().await;
        return;
    };
    if hello.kind != FrameKind::Req || hello.method.as_deref() != Some("hello") {
        writer.trigger_close();
        writer.close_and_take().await;
        return;
    }
    let hello_ok = matches!(answer_hello(&writer, &hello, &token).await, Ok(ReplyDelivery::Original));
    if !hello_token_ok(&hello, &token) {
        writer.close_and_take().await;
        return;
    }
    if !hello_ok {
        writer.close_and_take().await;
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
    let hub = options.hub;
    let mut subscription = None;
    serve_requests(
        &mut reader,
        &writer,
        &token,
        handler,
        hub.as_ref().map(|hub| (hub, peer_pid, &mut subscription)),
    )
    .await;
    writer.close_and_take().await;
    if let (Some(hub), Some((id, task))) = (&hub, subscription) {
        task.abort();
        hub.unsubscribe(id);
    }
    if let Some(cb) = &lifecycle {
        cb(LifecycleEvent::Disconnected { client });
    }
}

/// A connection's live subscription: hub id + its event writer task.
#[cfg(any(unix, windows))]
type Subscription = (u64, tokio::task::JoinHandle<()>);

#[cfg(any(unix, windows))]
async fn serve_requests<S, R>(
    reader: &mut tokio::io::BufReader<R>,
    writer: &SharedWriter<S>,
    token: &str,
    handler: Handler,
    mut events: Option<(&EventHub, Option<u32>, &mut Option<Subscription>)>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    R: tokio::io::AsyncRead + Unpin,
{
    let cancelled: SharedCancels = Arc::new(Mutex::new(HashSet::new()));
    loop {
        let frame_opt = tokio::select! {
            biased;
            _ = writer.wait_closed() => {
                break;
            }
            f = next_frame(reader) => f,
        };
        let Some(frame) = frame_opt else {
            break;
        };
        let env = match frame {
            Ok(env) => env,
            Err(e) if e.code == "oversize" => {
                writer.trigger_close();
                break;
            }
            Err(_) => continue,
        };
        match env.kind {
            FrameKind::Cancel => {
                if let Ok(mut set) = cancelled.lock() {
                    set.insert(env.id);
                }
            }
            FrameKind::Req => match (env.method.as_deref(), events.as_mut()) {
                (Some("hello"), _) => {
                    if answer_hello(writer, &env, token).await.is_err() {
                        break;
                    }
                }
                (Some("events.subscribe"), Some((hub, peer_pid, slot))) => {
                    if subscribe(writer, hub, &env, *peer_pid, slot).await.is_err() {
                        break;
                    }
                }
                _ => dispatch(writer.clone(), cancelled.clone(), handler.clone(), env),
            },
            FrameKind::Res | FrameKind::Event => {}
        }
    }
    writer.trigger_close();
    if let Some((hub, _, slot)) = events {
        if let Some((id, task)) = slot.take() {
            task.abort();
            hub.unsubscribe(id);
        }
    }
}

/// Answer `events.subscribe` and start the connection's event writer. The
/// answer is written before the writer starts, so it precedes every event.
#[cfg(any(unix, windows))]
async fn subscribe<S>(
    writer: &SharedWriter<S>,
    hub: &EventHub,
    req: &Envelope,
    peer_pid: Option<u32>,
    slot: &mut Option<Subscription>,
 ) -> Result<(), ()>
 where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    if slot.is_some() {
        let error = IpcError::new(
            "already_subscribed",
            "this connection is already subscribed".to_string(),
        );
        return send_res(writer, &req.id, None, Some(error)).await.map(|_| ());
    }
    let params = req.params.clone().unwrap_or(Value::Null);
    let topics: Vec<String> = params
        .get("topics")
        .and_then(Value::as_array)
        .map(|topics| {
            topics
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    match hub.subscribe(topics, peer_pid, params) {
        Ok((id, wake)) => {
            let ok = serde_json::json!({"ok": true});
            match send_res(writer, &req.id, Some(ok), None).await {
                Ok(ReplyDelivery::Original) => {
                    let task = tokio::spawn(write_events(writer.clone(), hub.clone(), id, wake));
                    *slot = Some((id, task));
                    Ok(())
                }
                Ok(ReplyDelivery::Fallback) => {
                    hub.unsubscribe(id);
                    Ok(())
                }
                Err(()) => {
                    hub.unsubscribe(id);
                    Err(())
                }
            }
        }
        Err(error) => send_res(writer, &req.id, None, Some(error)).await.map(|_| ()),
    }
}

/// Write a subscriber's queued events whenever the hub wakes it.
#[cfg(any(unix, windows))]
async fn write_events<S>(
    writer: SharedWriter<S>,
    hub: EventHub,
    id: u64,
    wake: Arc<tokio::sync::Notify>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    loop {
        tokio::select! {
            biased;
            _ = writer.wait_closed() => return,
            _ = wake.notified() => {
                for env in hub.drain(id) {
                    let Ok(bytes) = encode_line(&env) else {
                        continue;
                    };
                    if writer.write_bytes(&bytes).await.is_err() {
                        return;
                    }
                }
            }
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
async fn answer_hello<S>(writer: &SharedWriter<S>, hello: &Envelope, token: &str) -> Result<ReplyDelivery, ()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    if hello_token_ok(hello, token) {
        let welcome = serde_json::json!({
            "chosen": 1,
            "daemon_version": env!("CARGO_PKG_VERSION"),
        });
        send_res(writer, &hello.id, Some(welcome), None).await
    } else {
        let denied = IpcError {
            code: "auth_failed".to_string(),
            message: "bad token".to_string(),
            retryable: false,
            backend: None,
        };
        send_res(writer, &hello.id, None, Some(denied)).await
    }
}

#[cfg(any(unix, windows))]
fn dispatch<S>(writer: SharedWriter<S>, cancelled: SharedCancels, handler: Handler, req: Envelope)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let id = req.id.clone();
        let method = req.method.clone().unwrap_or_default();
        let params = req.params.clone().unwrap_or(Value::Null);
        if is_cancelled(&cancelled, &id) {
            let _ = send_res(&writer, &id, None, Some(cancelled_error())).await;
            return;
        }
        let outcome = tokio::task::spawn_blocking(move || handler(method, params)).await;
        if is_cancelled(&cancelled, &id) {
            let _ = send_res(&writer, &id, None, Some(cancelled_error())).await;
            return;
        }
        let _ = match outcome {
            Ok(Ok(value)) => send_res(&writer, &id, Some(value), None).await,
            Ok(Err(error)) => send_res(&writer, &id, None, Some(error)).await,
            Err(_) => {
                let failed = IpcError::new("internal", "handler task failed".to_string());
                send_res(&writer, &id, None, Some(failed)).await
            }
        };
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReplyDelivery {
    Original,
    Fallback,
}

#[cfg(any(unix, windows))]
async fn send_res<S>(
    writer: &SharedWriter<S>,
    id: &str,
    result: Option<Value>,
    error: Option<IpcError>,
) -> Result<ReplyDelivery, ()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
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
    match encode_line(&env) {
        Ok(bytes) => {
            if writer.write_bytes(&bytes).await.is_ok() {
                Ok(ReplyDelivery::Original)
            } else {
                Err(())
            }
        }
        Err(_) => {
            let fallback = Envelope {
                v: IPC_VERSION.to_string(),
                id: id.to_string(),
                kind: FrameKind::Res,
                method: None,
                params: None,
                result: None,
                error: Some(IpcError::new(
                    "oversize",
                    "response exceeds frame limit".to_string(),
                )),
                seq: None,
            };
            match encode_line(&fallback) {
                Ok(fb_bytes) => {
                    writer.write_bytes(&fb_bytes).await?;
                    Ok(ReplyDelivery::Fallback)
                }
                Err(_) => {
                    writer.trigger_close();
                    Err(())
                }
            }
        }
    }
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

#[cfg(all(test, any(unix, windows)))]
mod reply_close_tests {
    use super::*;
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
    use tokio::time::timeout;

    // Deterministic write failure while retaining an actual asynchronous read side.
    struct FailWrite(tokio::io::DuplexStream);
    impl AsyncRead for FailWrite {
        fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.0).poll_read(cx, buf)
        }
    }
    impl AsyncWrite for FailWrite {
        fn poll_write(self: Pin<&mut Self>, _: &mut Context<'_>, _: &[u8]) -> Poll<std::io::Result<usize>> {
            Poll::Ready(Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "fixture")))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> { Poll::Ready(Ok(())) }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> { Poll::Ready(Ok(())) }
    }

    #[tokio::test]
    async fn failed_hello_write_emits_no_lifecycle() {
        let (server, mut client) = tokio::io::duplex(4096);
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&events);
        let options = ServeOptions { lifecycle: Some(Arc::new(move |e| seen.lock().unwrap().push(e))), ..ServeOptions::default() };
        let handler: Handler = Arc::new(|_, _| Ok(Value::Null));
        let task = tokio::spawn(serve_conn(FailWrite(server), "tok".into(), handler, options, None));
        let hello = Envelope { v: IPC_VERSION.into(), id: "h".into(), kind: FrameKind::Req, method: Some("hello".into()), params: Some(serde_json::json!({"token":"tok", "client":"c"})), result: None, error: None, seq: None };
        let welcome = Envelope { v: IPC_VERSION.into(), id: hello.id.clone(), kind: FrameKind::Res, method: None, params: None, result: Some(serde_json::json!({"chosen": 1, "daemon_version": env!("CARGO_PKG_VERSION")})), error: None, seq: None };
        assert!(encode_line(&welcome).is_ok());
        client.write_all(&encode_line(&hello).unwrap()).await.unwrap();
        timeout(Duration::from_secs(5), task).await.unwrap().unwrap();
        assert!(events.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn failed_subscribe_write_removes_subscriber() {
        let (server, _client) = tokio::io::duplex(4096);
        let (_read, write) = tokio::io::split(FailWrite(server));
        let writer = SharedWriter::new(write);
        let hub = EventHub::new(vec![TopicRule { topic: "foo".into(), exclusive: true }]);
        let req = Envelope { v: IPC_VERSION.into(), id: "s".into(), kind: FrameKind::Req, method: Some("events.subscribe".into()), params: Some(serde_json::json!({"topics":["foo"]})), result: None, error: None, seq: None };
        let mut subscription = None;
        assert!(encode_line(&req).is_ok());
        let ack = Envelope { v: IPC_VERSION.into(), id: req.id.clone(), kind: FrameKind::Res, method: None, params: None, result: Some(serde_json::json!({"ok": true})), error: None, seq: None };
        assert!(encode_line(&ack).is_ok());
        assert!(subscribe(&writer, &hub, &req, None, &mut subscription).await.is_err());
        assert!(hub.subscribers("foo").is_empty());
        assert!(subscription.is_none());
        writer.close_and_take().await;
    }

    #[tokio::test]
    async fn close_releases_blocked_writer_and_waiter_and_drops_both_halves() {
        let (server, mut client) = tokio::io::duplex(1);
        let (read, write) = tokio::io::split(server);
        let writer = SharedWriter::new(write);
        let first = writer.clone();
        let writing = tokio::spawn(async move { first.write_bytes(&vec![7; 65536]).await });
        // Reading one byte proves the write acquired the slot and exceeded its buffer.
        let mut byte = [0; 1];
        timeout(Duration::from_secs(5), client.read_exact(&mut byte)).await.unwrap().unwrap();
        // Hold the slot until this task has queued for it, independently of scheduler timing.
        let second = writer.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let waiting = tokio::spawn(async move {
            let mut operation = Box::pin(second.write_bytes(b"wait"));
            let mut started = Some(started_tx);
            std::future::poll_fn(|cx| match operation.as_mut().poll(cx) {
                Poll::Pending => {
                    if let Some(tx) = started.take() { let _ = tx.send(()); }
                    Poll::Pending
                }
                ready => ready,
            }).await
        });
        timeout(Duration::from_secs(5), started_rx).await.unwrap().unwrap();
        writer.trigger_close();
        timeout(Duration::from_secs(5), writer.close_and_take()).await.unwrap();
        assert!(timeout(Duration::from_secs(5), writing).await.unwrap().unwrap().is_err());
        assert!(timeout(Duration::from_secs(5), waiting).await.unwrap().unwrap().is_err());
        drop(read);
        let mut remaining = Vec::new();
        timeout(Duration::from_secs(5), client.read_to_end(&mut remaining)).await.unwrap().unwrap();
        // The retained SharedWriter clones cannot keep the underlying stream alive.
        assert!(writer.is_closed());
    }
}
