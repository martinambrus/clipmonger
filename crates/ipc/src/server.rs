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

/// An opt-in handler for one exact request method. Special protocol methods
/// (`hello` and `events.subscribe`) are never contextualized.
#[derive(Clone)]
pub struct ContextHandler {
    /// The exact method name.
    pub method: String,
    /// Cooperative cancellation flag, set on cancel or connection closure.
    pub handler: std::sync::Arc<
        dyn Fn(Value, std::sync::Arc<std::sync::atomic::AtomicBool>) -> Result<Value, IpcError>
            + Send
            + Sync,
    >,
}

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
        Self::bind_internal(sock, token, handler, options, None).await
    }

    /// Bind with an opt-in handler for one exact method.
    ///
    /// # Errors
    /// Returns `bind_failed` when the socket cannot be bound.
    #[cfg(unix)]
    pub async fn bind_with_context(
        sock: &Path,
        token: String,
        handler: Handler,
        options: ServeOptions,
        context: ContextHandler,
    ) -> Result<Self, IpcError> {
        Self::bind_internal(sock, token, handler, options, Some(context)).await
    }

    #[cfg(unix)]
    async fn bind_internal(
        sock: &Path,
        token: String,
        handler: Handler,
        options: ServeOptions,
        context: Option<ContextHandler>,
    ) -> Result<Self, IpcError> {
        let _ = std::fs::remove_file(sock);
        let listener = tokio::net::UnixListener::bind(sock)
            .map_err(|e| IpcError::new("bind_failed", e.to_string()))?;
        let task = tokio::spawn(accept_loop(listener, token, handler, options, context));
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
        Self::bind_pipe_internal(pipe_name, token, handler, options, None)
    }

    /// Bind a named pipe with an opt-in handler for one exact method.
    ///
    /// # Errors
    /// Returns `bind_failed` when the pipe cannot be bound.
    #[cfg(windows)]
    pub fn bind_pipe_with_context(
        pipe_name: &str,
        token: String,
        handler: Handler,
        options: ServeOptions,
        context: ContextHandler,
    ) -> Result<Self, IpcError> {
        Self::bind_pipe_internal(pipe_name, token, handler, options, Some(context))
    }

    #[cfg(windows)]
    fn bind_pipe_internal(
        pipe_name: &str,
        token: String,
        handler: Handler,
        options: ServeOptions,
        context: Option<ContextHandler>,
    ) -> Result<Self, IpcError> {
        let first = make_pipe_instance(pipe_name, true)?;
        let task = tokio::spawn(windows_accept_loop(
            pipe_name.to_string(),
            token,
            handler,
            first,
            options,
            context,
        ));
        Ok(Self { _task: task })
    }

    /// Context-binding stub on platforms without sockets or named pipes.
    ///
    /// # Errors
    /// Always fails with `unavailable`.
    #[cfg(not(any(unix, windows)))]
    pub async fn bind_with_context(
        _sock: &Path,
        _token: String,
        _handler: Handler,
        _options: ServeOptions,
        _context: ContextHandler,
    ) -> Result<Self, IpcError> {
        Err(IpcError::new(
            "unavailable",
            "ipc server needs a Unix socket".to_string(),
        ))
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
        Self {
            inner: Arc::clone(&self.inner),
        }
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
        self.write_bytes_checked(bytes, None).await.map(|_| ())
    }

    // The successful write is committed when the writer lock is held and the
    // cancellation flag is checked, before the first byte is written.
    async fn write_bytes_checked(
        &self,
        bytes: &[u8],
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<bool, ()>
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
        if cancel.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::SeqCst)) {
            return Ok(false);
        }
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
                            Ok(true)
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
type SharedCancels = Arc<Mutex<ContextState>>;

#[cfg(unix)]
async fn accept_loop(
    listener: tokio::net::UnixListener,
    token: String,
    handler: Handler,
    options: ServeOptions,
    context: Option<ContextHandler>,
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
        tokio::spawn(serve_conn_with_context(
            stream,
            token.clone(),
            handler.clone(),
            options.clone(),
            peer_pid,
            context.clone(),
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
    context: Option<ContextHandler>,
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
                tokio::spawn(serve_conn_with_context(
                    connected,
                    token.clone(),
                    handler.clone(),
                    options.clone(),
                    peer_pid,
                    context.clone(),
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
    serve_conn_with_context(stream, token, handler, options, peer_pid, None).await;
}

#[cfg(any(unix, windows))]
async fn serve_conn_with_context<S>(
    stream: S,
    token: String,
    handler: Handler,
    options: ServeOptions,
    peer_pid: Option<u32>,
    context: Option<ContextHandler>,
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
    let hello_ok = matches!(
        answer_hello(&writer, &hello, &token).await,
        Ok(ReplyDelivery::Original)
    );
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
    if let Some(context) = context {
        serve_context_requests(
            reader,
            &writer,
            &token,
            handler,
            context,
            hub.as_ref().map(|hub| (hub, peer_pid, &mut subscription)),
        )
        .await;
    } else {
        serve_requests(
            &mut reader,
            &writer,
            &token,
            handler,
            hub.as_ref().map(|hub| (hub, peer_pid, &mut subscription)),
        )
        .await;
    }
    writer.close_and_take().await;
    drop(subscription);
    if let Some(cb) = &lifecycle {
        cb(LifecycleEvent::Disconnected { client });
    }
}

/// A connection's live subscription: hub id + its event writer task.
#[cfg(any(unix, windows))]
struct Subscription {
    id: u64,
    task: tokio::task::JoinHandle<()>,
    hub: EventHub,
}
#[cfg(any(unix, windows))]
impl Drop for Subscription {
    fn drop(&mut self) {
        self.task.abort();
        self.hub.unsubscribe(self.id);
    }
}

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
    let cancelled: SharedCancels = Arc::new(Mutex::new(ContextState::default()));
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
                cancelled
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .cancelled
                    .insert(env.id);
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
    if let Some((_, _, slot)) = events {
        drop(slot.take());
    }
}

// Bounded per connection: at most 128 active contextual calls and 16 queued
// special frames, each of which has passed the one-MiB frame bound.
#[cfg(any(unix, windows))]
const CONTEXT_CAP: usize = 128;
#[cfg(any(unix, windows))]
const SPECIAL_CAP: usize = 16;

#[cfg(any(unix, windows))]
#[derive(Default)]
struct ContextState {
    cancelled: HashSet<String>,
    active: HashMap<String, Vec<Arc<std::sync::atomic::AtomicBool>>>,
    count: usize,
    closed: bool,
}
#[cfg(any(unix, windows))]
type ContextRegistry = Arc<Mutex<ContextState>>;

#[cfg(any(unix, windows))]
fn close_context(state: &ContextRegistry) {
    let mut state = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.closed = true;
    for flags in state.active.values() {
        for flag in flags {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

#[cfg(any(unix, windows))]
struct ContextRegistration {
    state: ContextRegistry,
    id: String,
    flag: Arc<std::sync::atomic::AtomicBool>,
}
#[cfg(any(unix, windows))]
impl Drop for ContextRegistration {
    fn drop(&mut self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(flags) = state.active.get_mut(&self.id) {
            flags.retain(|flag| !Arc::ptr_eq(flag, &self.flag));
            if flags.is_empty() {
                state.active.remove(&self.id);
            }
        }
        state.count -= 1;
    }
}

#[cfg(any(unix, windows))]
fn register_context(state: &ContextRegistry, id: &str) -> Option<ContextRegistration> {
    let mut inner = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if inner.closed || inner.count == CONTEXT_CAP {
        return None;
    }
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(
        inner.cancelled.contains(id),
    ));
    inner
        .active
        .entry(id.to_string())
        .or_default()
        .push(Arc::clone(&flag));
    inner.count += 1;
    Some(ContextRegistration {
        state: Arc::clone(state),
        id: id.to_string(),
        flag,
    })
}

#[cfg(any(unix, windows))]
struct ContextOwner<S> {
    state: ContextRegistry,
    writer: SharedWriter<S>,
    reader: tokio::task::AbortHandle,
}
#[cfg(any(unix, windows))]
impl<S> Drop for ContextOwner<S> {
    fn drop(&mut self) {
        close_context(&self.state);
        self.writer.trigger_close();
        self.reader.abort();
    }
}

#[cfg(any(unix, windows))]
async fn serve_context_requests<S, R>(
    mut reader: tokio::io::BufReader<R>,
    writer: &SharedWriter<S>,
    token: &str,
    handler: Handler,
    context: ContextHandler,
    mut events: Option<(&EventHub, Option<u32>, &mut Option<Subscription>)>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let state: ContextRegistry = Arc::new(Mutex::new(ContextState::default()));
    // Ordinary and contextual calls share the persistent cancellation set.
    let cancelled = Arc::clone(&state);
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Envelope>(SPECIAL_CAP);
    let read_writer = writer.clone();
    let read_state = Arc::clone(&state);
    let has_hub = events.is_some();
    let reader_task = tokio::spawn(async move {
        loop {
            let frame = tokio::select! {
                biased;
                _ = read_writer.wait_closed() => break,
                frame = next_frame(&mut reader) => frame,
            };
            let env = match frame {
                None => break,
                Some(Err(e)) if e.code == "oversize" => break,
                Some(Err(_)) => continue,
                Some(Ok(env)) => env,
            };
            match env.kind {
                FrameKind::Cancel => {
                    let mut inner = read_state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    inner.cancelled.insert(env.id.clone());
                    if let Some(flags) = inner.active.get(&env.id) {
                        for flag in flags {
                            flag.store(true, std::sync::atomic::Ordering::SeqCst);
                        }
                    }
                }
                FrameKind::Req => {
                    let method = env.method.as_deref();
                    if method == Some("hello") || (method == Some("events.subscribe") && has_hub) {
                        if tx.try_send(env).is_err() {
                            break;
                        }
                    } else if method != Some("events.subscribe")
                        && method == Some(context.method.as_str())
                    {
                        let Some(registration) = register_context(&read_state, &env.id) else {
                            break;
                        };
                        dispatch_context(read_writer.clone(), context.clone(), env, registration);
                    } else {
                        dispatch(read_writer.clone(), cancelled.clone(), handler.clone(), env);
                    }
                }
                FrameKind::Res | FrameKind::Event => {}
            }
        }
        close_context(&read_state);
        read_writer.trigger_close();
    });
    let owner = ContextOwner {
        state,
        writer: writer.clone(),
        reader: reader_task.abort_handle(),
    };
    while let Some(env) = rx.recv().await {
        if writer.is_closed() {
            break;
        }
        let delivered = if env.method.as_deref() == Some("hello") {
            answer_hello(writer, &env, token).await.map(|_| ())
        } else if let Some((hub, peer_pid, slot)) = events.as_mut() {
            subscribe(writer, hub, &env, *peer_pid, slot).await
        } else {
            Ok(())
        };
        if delivered.is_err() {
            break;
        }
    }
    drop(owner);
    let _ = reader_task.await;
    if let Some((_, _, slot)) = events {
        drop(slot.take());
    }
}

#[cfg(any(unix, windows))]
fn dispatch_context<S>(
    writer: SharedWriter<S>,
    context: ContextHandler,
    req: Envelope,
    registration: ContextRegistration,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        use std::sync::atomic::Ordering;
        let flag = Arc::clone(&registration.flag);
        let id = req.id;
        if flag.load(Ordering::SeqCst) {
            let _ = send_res(&writer, &id, None, Some(cancelled_error())).await;
            return;
        }
        let call_flag = Arc::clone(&flag);
        let outcome = tokio::task::spawn_blocking(move || {
            if call_flag.load(Ordering::SeqCst) {
                return Err(cancelled_error());
            }
            (context.handler)(req.params.unwrap_or(Value::Null), call_flag)
        })
        .await;
        if flag.load(Ordering::SeqCst) {
            let _ = send_res(&writer, &id, None, Some(cancelled_error())).await;
            return;
        }
        match outcome {
            Ok(Ok(value)) => {
                let _ = send_context_success(&writer, &id, value, &flag).await;
            }
            Ok(Err(error)) => {
                let _ = send_res(&writer, &id, None, Some(error)).await;
            }
            Err(_) => {
                let _ = send_res(
                    &writer,
                    &id,
                    None,
                    Some(IpcError::new("internal", "handler task failed".to_string())),
                )
                .await;
            }
        }
        drop(registration);
    });
}

#[cfg(any(unix, windows))]
async fn send_context_success<S>(
    writer: &SharedWriter<S>,
    id: &str,
    value: Value,
    flag: &std::sync::atomic::AtomicBool,
) -> Result<(), ()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let env = Envelope {
        v: IPC_VERSION.to_string(),
        id: id.to_string(),
        kind: FrameKind::Res,
        method: None,
        params: None,
        result: Some(value),
        error: None,
        seq: None,
    };
    match encode_line(&env) {
        Ok(bytes) => {
            if !writer.write_bytes_checked(&bytes, Some(flag)).await? {
                send_res(writer, id, None, Some(cancelled_error())).await?;
            }
        }
        // Use the normal response path as the common fallback oracle.
        Err(_) => {
            send_res(writer, id, env.result, None).await?;
        }
    }
    Ok(())
}

#[cfg(any(unix, windows))]
struct PendingSubscription<'a> {
    hub: &'a EventHub,
    id: u64,
    armed: bool,
}
#[cfg(any(unix, windows))]
impl Drop for PendingSubscription<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.hub.unsubscribe(self.id);
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
        return send_res(writer, &req.id, None, Some(error))
            .await
            .map(|_| ());
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
            let mut pending = PendingSubscription {
                hub,
                id,
                armed: true,
            };
            let ok = serde_json::json!({"ok": true});
            match send_res(writer, &req.id, Some(ok), None).await {
                Ok(ReplyDelivery::Original) => {
                    let task = tokio::spawn(write_events(writer.clone(), hub.clone(), id, wake));
                    *slot = Some(Subscription {
                        id,
                        task,
                        hub: hub.clone(),
                    });
                    pending.armed = false;
                    Ok(())
                }
                Ok(ReplyDelivery::Fallback) => Ok(()),
                Err(()) => Err(()),
            }
        }
        Err(error) => send_res(writer, &req.id, None, Some(error))
            .await
            .map(|_| ()),
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
async fn answer_hello<S>(
    writer: &SharedWriter<S>,
    hello: &Envelope,
    token: &str,
) -> Result<ReplyDelivery, ()>
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
        Ok(set) => set.cancelled.contains(id),
        Err(poisoned) => poisoned.into_inner().cancelled.contains(id),
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
    loop {
        let available = match reader.fill_buf().await {
            Ok(bytes) => bytes,
            Err(_) => return None,
        };
        if available.is_empty() {
            return if line.is_empty() {
                None
            } else {
                Some(decode_line(&line))
            };
        }
        let take = available
            .iter()
            .position(|&byte| byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if line.len().saturating_add(take) > MAX_LINE_BYTES + 1 {
            return Some(Err(IpcError::new(
                "oversize",
                "line exceeds cap".to_string(),
            )));
        }
        let complete = available[take - 1] == b'\n';
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if complete {
            return Some(decode_line(&line));
        }
    }
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
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.0).poll_read(cx, buf)
        }
    }
    impl AsyncWrite for FailWrite {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "fixture",
            )))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn failed_hello_write_emits_no_lifecycle() {
        let (server, mut client) = tokio::io::duplex(4096);
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&events);
        let options = ServeOptions {
            lifecycle: Some(Arc::new(move |e| seen.lock().unwrap().push(e))),
            ..ServeOptions::default()
        };
        let handler: Handler = Arc::new(|_, _| Ok(Value::Null));
        let task = tokio::spawn(serve_conn(
            FailWrite(server),
            "tok".into(),
            handler,
            options,
            None,
        ));
        let hello = Envelope {
            v: IPC_VERSION.into(),
            id: "h".into(),
            kind: FrameKind::Req,
            method: Some("hello".into()),
            params: Some(serde_json::json!({"token":"tok", "client":"c"})),
            result: None,
            error: None,
            seq: None,
        };
        let welcome = Envelope {
            v: IPC_VERSION.into(),
            id: hello.id.clone(),
            kind: FrameKind::Res,
            method: None,
            params: None,
            result: Some(
                serde_json::json!({"chosen": 1, "daemon_version": env!("CARGO_PKG_VERSION")}),
            ),
            error: None,
            seq: None,
        };
        assert!(encode_line(&welcome).is_ok());
        client
            .write_all(&encode_line(&hello).unwrap())
            .await
            .unwrap();
        timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        assert!(events.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn failed_subscribe_write_removes_subscriber() {
        let (server, _client) = tokio::io::duplex(4096);
        let (_read, write) = tokio::io::split(FailWrite(server));
        let writer = SharedWriter::new(write);
        let hub = EventHub::new(vec![TopicRule {
            topic: "foo".into(),
            exclusive: true,
        }]);
        let req = Envelope {
            v: IPC_VERSION.into(),
            id: "s".into(),
            kind: FrameKind::Req,
            method: Some("events.subscribe".into()),
            params: Some(serde_json::json!({"topics":["foo"]})),
            result: None,
            error: None,
            seq: None,
        };
        let mut subscription = None;
        assert!(encode_line(&req).is_ok());
        let ack = Envelope {
            v: IPC_VERSION.into(),
            id: req.id.clone(),
            kind: FrameKind::Res,
            method: None,
            params: None,
            result: Some(serde_json::json!({"ok": true})),
            error: None,
            seq: None,
        };
        assert!(encode_line(&ack).is_ok());
        assert!(subscribe(&writer, &hub, &req, None, &mut subscription)
            .await
            .is_err());
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
        timeout(Duration::from_secs(5), client.read_exact(&mut byte))
            .await
            .unwrap()
            .unwrap();
        // Hold the slot until this task has queued for it, independently of scheduler timing.
        let second = writer.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let waiting = tokio::spawn(async move {
            let mut operation = Box::pin(second.write_bytes(b"wait"));
            let mut started = Some(started_tx);
            std::future::poll_fn(|cx| match operation.as_mut().poll(cx) {
                Poll::Pending => {
                    if let Some(tx) = started.take() {
                        let _ = tx.send(());
                    }
                    Poll::Pending
                }
                ready => ready,
            })
            .await
        });
        timeout(Duration::from_secs(5), started_rx)
            .await
            .unwrap()
            .unwrap();
        writer.trigger_close();
        timeout(Duration::from_secs(5), writer.close_and_take())
            .await
            .unwrap();
        assert!(timeout(Duration::from_secs(5), writing)
            .await
            .unwrap()
            .unwrap()
            .is_err());
        assert!(timeout(Duration::from_secs(5), waiting)
            .await
            .unwrap()
            .unwrap()
            .is_err());
        drop(read);
        let mut remaining = Vec::new();
        timeout(Duration::from_secs(5), client.read_to_end(&mut remaining))
            .await
            .unwrap()
            .unwrap();
        // The retained SharedWriter clones cannot keep the underlying stream alive.
        assert!(writer.is_closed());
    }
}

#[cfg(all(test, any(unix, windows)))]
mod context_registry_tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use tokio::io::AsyncReadExt as _;
    use tokio::time::timeout;

    #[test]
    fn duplicate_completion_reuse_empty_and_prior_cancel() {
        let state = Arc::new(Mutex::new(ContextState::default()));
        let first = register_context(&state, "").unwrap();
        let second = register_context(&state, "").unwrap();
        let second_flag = Arc::clone(&second.flag);
        drop(first);
        {
            let mut inner = state.lock().unwrap();
            inner.cancelled.insert(String::new());
            for flag in inner.active.get("").unwrap() {
                flag.store(true, Ordering::SeqCst);
            }
        }
        assert!(second_flag.load(Ordering::SeqCst));
        let reused = register_context(&state, "").unwrap();
        assert!(reused.flag.load(Ordering::SeqCst));
        drop(second);
        assert_eq!(state.lock().unwrap().active.get("").unwrap().len(), 1);
        close_context(&state);
        assert!(register_context(&state, "new").is_none());
        drop(reused);
        assert!(state.lock().unwrap().active.is_empty());
    }

    #[test]
    fn active_limit_releases_exact_registration_and_reuses_slot() {
        let state = Arc::new(Mutex::new(ContextState::default()));
        let mut leases: Vec<_> = (0..CONTEXT_CAP)
            .map(|_| register_context(&state, "same").unwrap())
            .collect();
        assert!(register_context(&state, "overflow").is_none());
        let survivor = Arc::clone(&leases[1].flag);
        drop(leases.remove(0));
        let replacement = register_context(&state, "same").unwrap();
        assert!(!Arc::ptr_eq(&survivor, &replacement.flag));
        let inner = state.lock().unwrap();
        assert_eq!(inner.count, CONTEXT_CAP);
        assert!(inner.active["same"]
            .iter()
            .any(|flag| Arc::ptr_eq(flag, &survivor)));
        assert!(inner.active["same"]
            .iter()
            .any(|flag| Arc::ptr_eq(flag, &replacement.flag)));
        drop(inner);
        drop(leases);
        drop(replacement);
        assert_eq!(state.lock().unwrap().count, 0);
    }

    #[tokio::test]
    async fn bounded_frames_reject_oversize_and_continue_after_short_malformed() {
        let valid = Envelope {
            v: IPC_VERSION.into(),
            id: "valid".into(),
            kind: FrameKind::Req,
            method: Some("ping".into()),
            params: None,
            result: None,
            error: None,
            seq: None,
        };
        let mut input = b"not-json\n".to_vec();
        input.extend(encode_line(&valid).unwrap());
        let mut reader = tokio::io::BufReader::with_capacity(7, &input[..]);
        assert!(next_frame(&mut reader).await.unwrap().is_err());
        assert_eq!(next_frame(&mut reader).await.unwrap().unwrap().id, "valid");
        assert!(next_frame(&mut reader).await.is_none());

        let mut boundary = vec![b'x'; MAX_LINE_BYTES];
        boundary.push(b'\n');
        let mut reader = tokio::io::BufReader::with_capacity(97, &boundary[..]);
        assert_ne!(
            next_frame(&mut reader).await.unwrap().unwrap_err().code,
            "oversize"
        );
        let oversized = vec![b'x'; MAX_LINE_BYTES + 2];
        let mut reader = tokio::io::BufReader::with_capacity(97, &oversized[..]);
        assert_eq!(
            next_frame(&mut reader).await.unwrap().unwrap_err().code,
            "oversize"
        );
    }

    struct ReleaseGuard(
        Option<std::sync::mpsc::Sender<()>>,
        std::sync::mpsc::Receiver<bool>,
    );
    impl Drop for ReleaseGuard {
        fn drop(&mut self) {
            if let Some(tx) = self.0.take() {
                let _ = tx.send(());
            }
            let succeeded = self.1.recv_timeout(Duration::from_secs(6)) == Ok(true);
            if !std::thread::panicking() {
                assert!(succeeded, "handler failed or did not finish");
            }
        }
    }
    struct Completion {
        sender: std::sync::mpsc::Sender<bool>,
        success: bool,
    }
    impl Drop for Completion {
        fn drop(&mut self) {
            let _ = self.sender.send(self.success);
        }
    }

    // A connected stream whose writes stay pending after reporting the first
    // actual write poll. Its read half continues to accept frames and EOF.
    struct PendingWrite {
        stream: tokio::io::DuplexStream,
        polled: Option<tokio::sync::oneshot::Sender<()>>,
    }
    impl tokio::io::AsyncRead for PendingWrite {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            tokio::io::AsyncRead::poll_read(std::pin::Pin::new(&mut self.stream), cx, buf)
        }
    }
    impl tokio::io::AsyncWrite for PendingWrite {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            _: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            if let Some(sender) = self.polled.take() {
                let _ = sender.send(());
            }
            std::task::Poll::Pending
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn eof_reaches_context_while_hello_write_is_pending() {
        pending_hello_closes(false).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_reaches_context_while_hello_write_is_pending() {
        pending_hello_closes(true).await;
    }

    async fn pending_hello_closes(cancel_first: bool) {
        use tokio::io::AsyncWriteExt as _;
        let (stream, mut client) = tokio::io::duplex(8192);
        let (polled_tx, polled_rx) = tokio::sync::oneshot::channel();
        let (read, write) = tokio::io::split(PendingWrite {
            stream,
            polled: Some(polled_tx),
        });
        let writer = SharedWriter::new(write);
        let (seen_tx, mut seen_rx) = tokio::sync::mpsc::channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let release_guard = ReleaseGuard(Some(release_tx), done_rx);
        let context = ContextHandler {
            method: "history.thumbnail".into(),
            handler: Arc::new(move |_, flag| {
                let mut completion = Completion {
                    sender: done_tx.clone(),
                    success: false,
                };
                seen_tx.blocking_send(flag).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
                completion.success = true;
                Ok(Value::Null)
            }),
        };
        let task = tokio::spawn(async move {
            let handler: Handler = Arc::new(|_, _| Ok(Value::Null));
            serve_context_requests(
                tokio::io::BufReader::new(read),
                &writer,
                "tok",
                handler,
                context,
                None,
            )
            .await;
        });
        let request = |id: &str, method: &str, params: Value| Envelope {
            v: IPC_VERSION.into(),
            id: id.into(),
            kind: FrameKind::Req,
            method: Some(method.into()),
            params: Some(params),
            result: None,
            error: None,
            seq: None,
        };
        client
            .write_all(&encode_line(&request("ctx", "history.thumbnail", Value::Null)).unwrap())
            .await
            .unwrap();
        let flag = timeout(Duration::from_secs(5), seen_rx.recv())
            .await
            .unwrap()
            .unwrap();
        client
            .write_all(
                &encode_line(&request("h", "hello", serde_json::json!({"token":"tok"}))).unwrap(),
            )
            .await
            .unwrap();
        timeout(Duration::from_secs(5), polled_rx)
            .await
            .unwrap()
            .unwrap();
        assert!(!flag.load(Ordering::SeqCst));
        if cancel_first {
            let mut cancel = request("ctx", "history.thumbnail", Value::Null);
            cancel.kind = FrameKind::Cancel;
            client
                .write_all(&encode_line(&cancel).unwrap())
                .await
                .unwrap();
            timeout(Duration::from_secs(5), async {
                while !flag.load(Ordering::SeqCst) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert!(!task.is_finished(), "hello write should still be pending");
        }
        drop(client);
        timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        assert!(flag.load(Ordering::SeqCst));
        drop(release_guard);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn eof_cancels_context_while_subscribe_ack_waits_and_unsubscribes() {
        pending_subscribe_closes_and_unsubscribes(0).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn special_queue_overflow_cancels_context_and_unsubscribes() {
        pending_subscribe_closes_and_unsubscribes(1).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_reaches_context_while_subscribe_ack_is_pending() {
        pending_subscribe_closes_and_unsubscribes(2).await;
    }

    async fn pending_subscribe_closes_and_unsubscribes(mode: u8) {
        use tokio::io::AsyncWriteExt as _;
        let (stream, mut client) = tokio::io::duplex(8192);
        let (polled_tx, polled_rx) = tokio::sync::oneshot::channel();
        let (read, write) = tokio::io::split(PendingWrite {
            stream,
            polled: Some(polled_tx),
        });
        let writer = SharedWriter::new(write);
        let hub = EventHub::new(vec![TopicRule {
            topic: "topic".into(),
            exclusive: true,
        }]);
        let task_hub = hub.clone();
        let task_writer = writer.clone();
        let (seen_tx, mut seen_rx) = tokio::sync::mpsc::channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let release_guard = ReleaseGuard(Some(release_tx), done_rx);
        let context = ContextHandler {
            method: "history.thumbnail".into(),
            handler: Arc::new(move |_, flag| {
                let mut completion = Completion {
                    sender: done_tx.clone(),
                    success: false,
                };
                seen_tx.blocking_send(flag).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
                completion.success = true;
                Ok(Value::Null)
            }),
        };
        let task = tokio::spawn(async move {
            let mut subscription = None;
            let handler: Handler = Arc::new(|_, _| Ok(Value::Null));
            serve_context_requests(
                tokio::io::BufReader::new(read),
                &task_writer,
                "tok",
                handler,
                context,
                Some((&task_hub, None, &mut subscription)),
            )
            .await;
            assert!(subscription.is_none());
        });
        let request = |id: &str, method: &str, params: Value| Envelope {
            v: IPC_VERSION.into(),
            id: id.into(),
            kind: FrameKind::Req,
            method: Some(method.into()),
            params: Some(params),
            result: None,
            error: None,
            seq: None,
        };
        client
            .write_all(&encode_line(&request("ctx", "history.thumbnail", Value::Null)).unwrap())
            .await
            .unwrap();
        let flag = timeout(Duration::from_secs(5), seen_rx.recv())
            .await
            .unwrap()
            .unwrap();
        client
            .write_all(
                &encode_line(&request(
                    "sub",
                    "events.subscribe",
                    serde_json::json!({"topics":["topic"]}),
                ))
                .unwrap(),
            )
            .await
            .unwrap();
        timeout(Duration::from_secs(5), async {
            while hub.subscribers("topic").is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        timeout(Duration::from_secs(5), polled_rx)
            .await
            .unwrap()
            .unwrap();
        assert!(!flag.load(Ordering::SeqCst));
        if mode == 1 {
            // The owner is blocked in the pending acknowledgement. The reader can
            // enqueue only SPECIAL_CAP more special requests before closing.
            let hello = encode_line(&request(
                "queued",
                "hello",
                serde_json::json!({"token":"tok"}),
            ))
            .unwrap();
            for _ in 0..=SPECIAL_CAP {
                timeout(Duration::from_secs(5), client.write_all(&hello))
                    .await
                    .unwrap()
                    .unwrap();
            }
        } else {
            if mode == 2 {
                let mut cancel = request("ctx", "history.thumbnail", Value::Null);
                cancel.kind = FrameKind::Cancel;
                client
                    .write_all(&encode_line(&cancel).unwrap())
                    .await
                    .unwrap();
                timeout(Duration::from_secs(5), async {
                    while !flag.load(Ordering::SeqCst) {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
                assert!(!task.is_finished());
                assert_eq!(hub.subscribers("topic").len(), 1);
            }
            client.shutdown().await.unwrap();
        }
        timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        assert!(flag.load(Ordering::SeqCst));
        assert!(hub.subscribers("topic").is_empty());
        drop(release_guard);
    }

    #[tokio::test]
    async fn queued_success_checks_flag_after_writer_lock() {
        let (stream, mut client) = tokio::io::duplex(4096);
        let (_read, write) = tokio::io::split(stream);
        let writer = SharedWriter::new(write);
        let lock = writer.inner.slot.lock().await;
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let operation_writer = writer.clone();
        let operation_flag = Arc::clone(&flag);
        let (pending_tx, pending_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            use std::future::Future as _;
            use std::task::Poll;
            let mut future = Box::pin(send_context_success(
                &operation_writer,
                "id",
                Value::Bool(true),
                &operation_flag,
            ));
            let mut pending = Some(pending_tx);
            std::future::poll_fn(|cx| match future.as_mut().poll(cx) {
                Poll::Pending => {
                    if let Some(tx) = pending.take() {
                        let _ = tx.send(());
                    }
                    Poll::Pending
                }
                ready => ready,
            })
            .await
        });
        timeout(Duration::from_secs(5), pending_rx)
            .await
            .unwrap()
            .unwrap();
        flag.store(true, Ordering::SeqCst);
        drop(lock);
        timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let mut line = Vec::new();
        timeout(Duration::from_secs(5), async {
            loop {
                let byte = client.read_u8().await.unwrap();
                line.push(byte);
                if byte == b'\n' {
                    break;
                }
            }
        })
        .await
        .unwrap();
        let response = decode_line(&line).unwrap();
        assert_eq!(response.error.unwrap().code, "cancelled");
        assert!(response.result.is_none());
    }

    #[tokio::test]
    async fn context_subscription_ack_precedes_held_event() {
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
        let hub = EventHub::new(vec![TopicRule {
            topic: "topic".into(),
            exclusive: false,
        }]);
        assert_eq!(hub.publish_or_hold("topic", serde_json::json!({"n":1})), 0);
        let task_hub = hub.clone();
        let (stream, client) = tokio::io::duplex(8192);
        let (read, write) = tokio::io::split(stream);
        let writer = SharedWriter::new(write);
        let task = tokio::spawn(async move {
            let handler: Handler = Arc::new(|_, _| Ok(Value::Null));
            let context = ContextHandler {
                method: "history.thumbnail".into(),
                handler: Arc::new(|_, _| panic!("unexpected context call")),
            };
            let mut subscription = None;
            serve_context_requests(
                tokio::io::BufReader::new(read),
                &writer,
                "tok",
                handler,
                context,
                Some((&task_hub, None, &mut subscription)),
            )
            .await;
            assert!(subscription.is_none());
        });
        let (read, mut write) = tokio::io::split(client);
        let mut read = tokio::io::BufReader::new(read);
        let req = Envelope {
            v: IPC_VERSION.into(),
            id: "sub".into(),
            kind: FrameKind::Req,
            method: Some("events.subscribe".into()),
            params: Some(serde_json::json!({"topics":["topic"]})),
            result: None,
            error: None,
            seq: None,
        };
        write.write_all(&encode_line(&req).unwrap()).await.unwrap();
        let mut line = Vec::new();
        assert_ne!(
            timeout(Duration::from_secs(5), read.read_until(b'\n', &mut line))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        let ack = decode_line(&line).unwrap();
        assert_eq!(ack.kind, FrameKind::Res);
        assert_eq!(ack.id, "sub");
        assert_eq!(ack.result, Some(serde_json::json!({"ok":true})));
        line.clear();
        assert_ne!(
            timeout(Duration::from_secs(5), read.read_until(b'\n', &mut line))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        let event = decode_line(&line).unwrap();
        assert_eq!(event.kind, FrameKind::Event);
        assert_eq!(event.method.as_deref(), Some("topic"));
        assert_eq!(event.params, Some(serde_json::json!({"n":1})));
        write.shutdown().await.unwrap();
        timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        assert!(hub.subscribers("topic").is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn error_and_panic_release_context_registration() {
        use tokio::io::AsyncBufReadExt as _;
        for panic_case in [false, true] {
            let (stream, client) = tokio::io::duplex(8192);
            let (_, write) = tokio::io::split(stream);
            let writer = SharedWriter::new(write);
            let state = Arc::new(Mutex::new(ContextState::default()));
            let lease = register_context(&state, "same").unwrap();
            let context = ContextHandler {
                method: "m".into(),
                handler: Arc::new(move |_, _| {
                    if panic_case {
                        panic!("expected fixture panic");
                    }
                    Err(IpcError::new("fixture_error", "expected".into()))
                }),
            };
            let req = Envelope {
                v: IPC_VERSION.into(),
                id: "same".into(),
                kind: FrameKind::Req,
                method: Some("m".into()),
                params: None,
                result: None,
                error: None,
                seq: None,
            };
            dispatch_context(writer, context, req, lease);
            let mut read = tokio::io::BufReader::new(client);
            let mut line = Vec::new();
            assert_ne!(
                timeout(Duration::from_secs(5), read.read_until(b'\n', &mut line))
                    .await
                    .unwrap()
                    .unwrap(),
                0
            );
            let response = decode_line(&line).unwrap();
            assert_eq!(
                response.error.unwrap().code,
                if panic_case {
                    "internal"
                } else {
                    "fixture_error"
                }
            );
            timeout(Duration::from_secs(5), async {
                while state.lock().unwrap().count != 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let replacement = register_context(&state, "same").unwrap();
            assert!(!replacement.flag.load(Ordering::SeqCst));
            drop(replacement);
            assert!(state.lock().unwrap().active.is_empty());
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn routing_excludes_special_and_nonrequest_frames() {
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
        for (method, with_hub, expected_context, expected_ordinary) in [
            ("hello", false, 0, 0),
            ("events.subscribe", false, 0, 1),
            ("events.subscribe", true, 0, 0),
            ("custom", false, 1, 0),
        ] {
            let (stream, client) = tokio::io::duplex(8192);
            let (read, write) = tokio::io::split(stream);
            let writer = SharedWriter::new(write);
            let contextual = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let ordinary = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let c = Arc::clone(&contextual);
            let o = Arc::clone(&ordinary);
            let method_owned = method.to_string();
            let task = tokio::spawn(async move {
                let context = ContextHandler {
                    method: method_owned,
                    handler: Arc::new(move |_, _| {
                        c.fetch_add(1, Ordering::SeqCst);
                        Ok(Value::Null)
                    }),
                };
                let handler: Handler = Arc::new(move |method, _| {
                    if method != "ping" {
                        o.fetch_add(1, Ordering::SeqCst);
                    }
                    Ok(Value::Null)
                });
                let hub = EventHub::new(vec![TopicRule {
                    topic: "topic".into(),
                    exclusive: false,
                }]);
                let mut slot = None;
                let events = if with_hub {
                    Some((&hub, None, &mut slot))
                } else {
                    None
                };
                serve_context_requests(
                    tokio::io::BufReader::new(read),
                    &writer,
                    "tok",
                    handler,
                    context,
                    events,
                )
                .await;
            });
            let (read, mut write) = tokio::io::split(client);
            let mut read = tokio::io::BufReader::new(read);
            let frame = |id: &str, kind: FrameKind, method: &str, params: Value| Envelope {
                v: IPC_VERSION.into(),
                id: id.into(),
                kind,
                method: Some(method.into()),
                params: Some(params),
                result: None,
                error: None,
                seq: None,
            };
            for kind in [FrameKind::Res, FrameKind::Event] {
                write
                    .write_all(&encode_line(&frame("ignored", kind, method, Value::Null)).unwrap())
                    .await
                    .unwrap();
            }
            let params = if method == "hello" {
                serde_json::json!({"token":"tok"})
            } else {
                serde_json::json!({"topics":["topic"]})
            };
            write
                .write_all(&encode_line(&frame("target", FrameKind::Req, method, params)).unwrap())
                .await
                .unwrap();
            let mut line = Vec::new();
            assert_ne!(
                timeout(Duration::from_secs(5), read.read_until(b'\n', &mut line))
                    .await
                    .unwrap()
                    .unwrap(),
                0
            );
            let reply = decode_line(&line).unwrap();
            assert_eq!(reply.id, "target");
            assert!(reply.error.is_none());
            write
                .write_all(
                    &encode_line(&frame("barrier", FrameKind::Req, "ping", Value::Null)).unwrap(),
                )
                .await
                .unwrap();
            line.clear();
            assert_ne!(
                timeout(Duration::from_secs(5), read.read_until(b'\n', &mut line))
                    .await
                    .unwrap()
                    .unwrap(),
                0
            );
            assert_eq!(decode_line(&line).unwrap().id, "barrier");
            assert_eq!(contextual.load(Ordering::SeqCst), expected_context);
            assert_eq!(ordinary.load(Ordering::SeqCst), expected_ordinary);
            write.shutdown().await.unwrap();
            timeout(Duration::from_secs(5), task)
                .await
                .unwrap()
                .unwrap();
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wire_cancel_before_writer_unlock_and_immediate_cancel() {
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
        for immediate in [false, true] {
            let (stream, client) = tokio::io::duplex(8192);
            let (read, write) = tokio::io::split(stream);
            let writer = SharedWriter::new(write);
            let lock = writer.inner.slot.lock().await;
            let task_writer = writer.clone();
            let (seen_tx, mut seen_rx) = tokio::sync::mpsc::channel(1);
            let (barrier_tx, mut barrier_rx) = tokio::sync::mpsc::channel(1);
            let task = tokio::spawn(async move {
                let context = ContextHandler {
                    method: "m".into(),
                    handler: Arc::new(move |_, flag| {
                        seen_tx.blocking_send(flag).unwrap();
                        Ok(Value::Null)
                    }),
                };
                let handler: Handler = Arc::new(move |_, _| {
                    barrier_tx.blocking_send(()).unwrap();
                    Ok(Value::Null)
                });
                serve_context_requests(
                    tokio::io::BufReader::new(read),
                    &task_writer,
                    "tok",
                    handler,
                    context,
                    None,
                )
                .await;
            });
            let (read, mut write) = tokio::io::split(client);
            let mut read = tokio::io::BufReader::new(read);
            let request = |id: &str, kind: FrameKind, method: &str| Envelope {
                v: IPC_VERSION.into(),
                id: id.into(),
                kind,
                method: Some(method.into()),
                params: None,
                result: None,
                error: None,
                seq: None,
            };
            let mut bytes = encode_line(&request("id", FrameKind::Req, "m")).unwrap();
            if immediate {
                bytes.extend(encode_line(&request("id", FrameKind::Cancel, "m")).unwrap());
            }
            write.write_all(&bytes).await.unwrap();
            if !immediate {
                let flag = timeout(Duration::from_secs(5), seen_rx.recv())
                    .await
                    .unwrap()
                    .unwrap();
                write
                    .write_all(&encode_line(&request("id", FrameKind::Cancel, "m")).unwrap())
                    .await
                    .unwrap();
                timeout(Duration::from_secs(5), async {
                    while !flag.load(Ordering::SeqCst) {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
            }
            // This handler starts only after the reader has processed Cancel.
            write
                .write_all(&encode_line(&request("barrier", FrameKind::Req, "ping")).unwrap())
                .await
                .unwrap();
            timeout(Duration::from_secs(5), barrier_rx.recv())
                .await
                .unwrap()
                .unwrap();
            drop(lock);
            let mut replies = Vec::new();
            for _ in 0..2 {
                let mut line = Vec::new();
                assert_ne!(
                    timeout(Duration::from_secs(5), read.read_until(b'\n', &mut line))
                        .await
                        .unwrap()
                        .unwrap(),
                    0
                );
                replies.push(decode_line(&line).unwrap());
            }
            let cancelled = replies.iter().find(|reply| reply.id == "id").unwrap();
            assert_eq!(cancelled.error.as_ref().unwrap().code, "cancelled");
            assert!(cancelled.result.is_none());
            assert!(replies.iter().any(|reply| reply.id == "barrier"));
            write.shutdown().await.unwrap();
            timeout(Duration::from_secs(5), task)
                .await
                .unwrap()
                .unwrap();
        }
    }
}
