//! One fresh, bounded connection for a thumbnail request.
//!
//! Profile reads and Windows pipe opens are synchronous and cannot be preempted
//! by Tokio timers. Deadlines are checked immediately after those calls return.
use std::future::Future;
use std::path::Path;
use std::time::Duration;

use clipmonger_ipc::framing::{decode_line, encode_line};
use clipmonger_ipc::proto::methods::{HistoryThumbnailParams, HistoryThumbnailResult};
use clipmonger_ipc::proto::{Envelope, FrameKind, IPC_VERSION, MAX_LINE_BYTES};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::watch;
use tokio::time::{timeout_at, Instant};
use uuid::Uuid;

/// Fixed local failures; peer strings and profile paths are never returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchError {
    Cancelled,
    Deadline,
    Profile,
    Connect,
    Io,
    Protocol,
    Rejected,
    Stale,
}

fn cancelled(cancel: &watch::Receiver<bool>) -> bool {
    *cancel.borrow() || cancel.has_changed().is_err()
}
async fn bounded<T>(
    deadline: Instant,
    cancel: &mut watch::Receiver<bool>,
    work: impl Future<Output = T>,
) -> Result<T, FetchError> {
    if cancelled(cancel) {
        return Err(FetchError::Cancelled);
    }
    tokio::select! {
        biased;
        _ = cancel.wait_for(|flag| *flag) => Err(FetchError::Cancelled),
        result = timeout_at(deadline, work) => result.map_err(|_| FetchError::Deadline),
    }
}
fn stage(overall: Instant, duration: Duration) -> Instant {
    overall.min(Instant::now() + duration)
}
fn request(id: &str, method: &str, params: serde_json::Value) -> Envelope {
    Envelope {
        v: IPC_VERSION.into(),
        id: id.into(),
        kind: FrameKind::Req,
        method: Some(method.into()),
        params: Some(params),
        result: None,
        error: None,
        seq: None,
    }
}
async fn write_frame<S: AsyncWrite + Unpin>(
    stream: &mut S,
    frame: &Envelope,
    deadline: Instant,
    cancel: &mut watch::Receiver<bool>,
) -> Result<(), FetchError> {
    let bytes = encode_line(frame).map_err(|_| FetchError::Protocol)?;
    bounded(deadline, cancel, stream.write_all(&bytes))
        .await?
        .map_err(|_| FetchError::Io)
}
async fn read_frame<S: AsyncRead + Unpin>(
    stream: &mut S,
    deadline: Instant,
    cancel: &mut watch::Receiver<bool>,
) -> Result<Envelope, FetchError> {
    let mut line = Vec::new();
    let mut scratch = [0_u8; 8192];
    loop {
        let count = bounded(deadline, cancel, stream.read(&mut scratch))
            .await?
            .map_err(|_| FetchError::Io)?;
        if count == 0 {
            return Err(FetchError::Protocol);
        }
        if let Some(end) = scratch[..count].iter().position(|b| *b == b'\n') {
            if end + 1 != count || line.len() + end > MAX_LINE_BYTES {
                return Err(FetchError::Protocol);
            }
            line.extend_from_slice(&scratch[..=end]);
            return decode_line(&line).map_err(|_| FetchError::Protocol);
        }
        if line.len() + count > MAX_LINE_BYTES {
            return Err(FetchError::Protocol);
        }
        line.extend_from_slice(&scratch[..count]);
    }
}
fn response(frame: Envelope, id: &str) -> Result<serde_json::Value, FetchError> {
    if frame.v != IPC_VERSION
        || frame.kind != FrameKind::Res
        || frame.id != id
        || frame.method.is_some()
        || frame.params.is_some()
        || frame.seq.is_some()
    {
        return Err(FetchError::Protocol);
    }
    match (frame.result, frame.error) {
        (Some(value), None) => Ok(value),
        (None, Some(_)) => Err(FetchError::Rejected),
        _ => Err(FetchError::Protocol),
    }
}

#[cfg(unix)]
async fn connect(
    path: &Path,
    deadline: Instant,
    cancel: &mut watch::Receiver<bool>,
) -> Result<tokio::net::UnixStream, FetchError> {
    bounded(deadline, cancel, tokio::net::UnixStream::connect(path))
        .await?
        .map_err(|_| FetchError::Connect)
}
#[cfg(windows)]
async fn open_pipe<T>(
    mut open: impl FnMut() -> std::io::Result<T>,
    deadline: Instant,
    cancel: &mut watch::Receiver<bool>,
) -> Result<T, FetchError> {
    for attempt in 0..4 {
        if cancelled(cancel) {
            return Err(FetchError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(FetchError::Deadline);
        }
        // The synchronous open may overrun the deadline; it is checked on return.
        let opened = open();
        if cancelled(cancel) {
            return Err(FetchError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(FetchError::Deadline);
        }
        match opened {
            Ok(pipe) => return Ok(pipe),
            Err(e) if e.raw_os_error() == Some(231) && attempt < 3 => {
                bounded(
                    deadline,
                    cancel,
                    tokio::time::sleep(Duration::from_millis([25, 50, 100][attempt])),
                )
                .await?;
            }
            Err(_) => return Err(FetchError::Connect),
        }
    }
    Err(FetchError::Connect)
}
#[cfg(windows)]
async fn connect(
    path: &Path,
    deadline: Instant,
    cancel: &mut watch::Receiver<bool>,
) -> Result<tokio::net::windows::named_pipe::NamedPipeClient, FetchError> {
    open_pipe(
        || tokio::net::windows::named_pipe::ClientOptions::new().open(path),
        deadline,
        cancel,
    )
    .await
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    #[tokio::test]
    async fn local_pipe_uses_the_production_fetch() {
        let dir = std::env::temp_dir().join(format!(
            "cm-pipe-profile-{}-{}",
            std::process::id(),
            Uuid::now_v7()
        ));
        std::fs::create_dir(&dir).unwrap();
        struct Dir(std::path::PathBuf);
        impl Drop for Dir {
            fn drop(&mut self) {
                std::fs::remove_dir_all(&self.0).unwrap();
            }
        }
        let _dir = Dir(dir.clone());
        let name = format!(
            r"\\.\pipe\cm-thumb-{}-{}",
            std::process::id(),
            Uuid::now_v7()
        );
        std::fs::write(dir.join("token"), "synthetic-token").unwrap();
        std::fs::write(
            dir.join("rendezvous.json"),
            serde_json::json!({"socket":name}).to_string(),
        )
        .unwrap();
        let mut server = tokio::net::windows::named_pipe::ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .unwrap();
        let clip = Uuid::now_v7();
        let hash = "a".repeat(64);
        let peer = tokio::spawn(async move {
            timeout_at(Instant::now() + Duration::from_secs(2), server.connect())
                .await
                .unwrap()
                .unwrap();
            let (_tx, mut cancel) = watch::channel(false);
            let hello = read_frame(
                &mut server,
                Instant::now() + Duration::from_secs(2),
                &mut cancel,
            )
            .await
            .unwrap();
            assert_eq!(hello.params.unwrap()["token"], "synthetic-token");
            let reply = |id: &str, result| Envelope {
                v: IPC_VERSION.into(),
                id: id.into(),
                kind: FrameKind::Res,
                method: None,
                params: None,
                result: Some(result),
                error: None,
                seq: None,
            };
            write_frame(
                &mut server,
                &reply("hello-0", serde_json::json!({"chosen":1})),
                Instant::now() + Duration::from_secs(2),
                &mut cancel,
            )
            .await
            .unwrap();
            let req = read_frame(
                &mut server,
                Instant::now() + Duration::from_secs(2),
                &mut cancel,
            )
            .await
            .unwrap();
            assert_eq!(req.id, "t0");
            assert_eq!(req.method.as_deref(), Some("history.thumbnail"));
            write_frame(
                &mut server,
                &reply("t0", serde_json::json!({"thumbnail":null})),
                Instant::now() + Duration::from_secs(2),
                &mut cancel,
            )
            .await
            .unwrap();
        });
        let (_tx, mut cancel) = watch::channel(false);
        assert!(fetch(&dir, clip, &hash, 128, &mut cancel)
            .await
            .unwrap()
            .thumbnail
            .is_none());
        timeout_at(Instant::now() + Duration::from_secs(3), peer)
            .await
            .unwrap()
            .unwrap();
    }
    #[tokio::test]
    async fn pipe_busy_is_finite_and_synchronous_open_is_not_preempted() {
        let (_tx, mut cancel) = watch::channel(false);
        let mut attempts = 0;
        let result: Result<(), _> = open_pipe(
            || {
                attempts += 1;
                Err(std::io::Error::from_raw_os_error(231))
            },
            Instant::now() + Duration::from_secs(2),
            &mut cancel,
        )
        .await;
        assert_eq!(result.unwrap_err(), FetchError::Connect);
        assert_eq!(attempts, 4);
        let start = Instant::now();
        let result = open_pipe(
            || {
                std::thread::sleep(Duration::from_millis(80));
                Ok(())
            },
            start + Duration::from_millis(20),
            &mut cancel,
        )
        .await;
        assert_eq!(result.unwrap_err(), FetchError::Deadline);
        assert!(start.elapsed() >= Duration::from_millis(80));
    }
}

fn valid_base64_png(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() % 4 != 0 || bytes.len() > (512 * 1024_usize).div_ceil(3) * 4
    {
        return false;
    }
    let padding = bytes.iter().rev().take_while(|b| **b == b'=').count();
    if padding > 2
        || bytes[..bytes.len() - padding]
            .iter()
            .any(|b| !b.is_ascii_alphanumeric() && *b != b'+' && *b != b'/')
    {
        return false;
    }
    bytes.len() / 4 * 3 - padding <= 512 * 1024
}

/// Fetch one encoded PNG (or a null placeholder) using an independent connection.
/// Decoding and validating PNG pixels belongs to the caller's background worker.
pub async fn fetch(
    profile: &Path,
    clip: Uuid,
    expected_hash: &str,
    max_px: u32,
    cancel: &mut watch::Receiver<bool>,
) -> Result<HistoryThumbnailResult, FetchError> {
    let overall = Instant::now() + Duration::from_secs(3);
    if cancelled(cancel) {
        return Err(FetchError::Cancelled);
    }
    if !(1..=256).contains(&max_px)
        || expected_hash.len() != 64
        || !expected_hash
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(FetchError::Protocol);
    }
    let connection = crate::profile_connection::resolve(profile);
    if cancelled(cancel) {
        return Err(FetchError::Cancelled);
    }
    if Instant::now() >= overall {
        return Err(FetchError::Deadline);
    }
    let connection = connection.map_err(|_| FetchError::Profile)?;
    let mut stream = connect(
        &connection.endpoint,
        stage(overall, Duration::from_millis(500)),
        cancel,
    )
    .await?;
    let hello = request(
        "hello-0",
        "hello",
        serde_json::json!({"min":1,"max":1,"token":connection.token,"client":"clipmonger-ui-thumbnail"}),
    );
    let hello_deadline = stage(overall, Duration::from_millis(500));
    write_frame(&mut stream, &hello, hello_deadline, cancel).await?;
    let welcome = response(
        read_frame(&mut stream, hello_deadline, cancel).await?,
        "hello-0",
    )?;
    if welcome.get("chosen").and_then(|v| v.as_u64()) != Some(1) {
        return Err(FetchError::Protocol);
    }
    let params = HistoryThumbnailParams {
        clip,
        max_px: Some(max_px),
        mime: Some("image/png".into()),
    };
    let frame = request(
        "t0",
        "history.thumbnail",
        serde_json::to_value(params).map_err(|_| FetchError::Protocol)?,
    );
    write_frame(
        &mut stream,
        &frame,
        stage(overall, Duration::from_millis(250)),
        cancel,
    )
    .await?;
    let value = response(
        read_frame(
            &mut stream,
            stage(overall, Duration::from_millis(2200)),
            cancel,
        )
        .await?,
        "t0",
    )?;
    if !value.is_object() || value.get("thumbnail").is_none() {
        return Err(FetchError::Protocol);
    }
    let result: HistoryThumbnailResult =
        serde_json::from_value(value).map_err(|_| FetchError::Protocol)?;
    if let Some(image) = &result.thumbnail {
        if image.clip != clip || image.content_hash != expected_hash {
            return Err(FetchError::Stale);
        }
        if image.mime != "image/png"
            || image.width == 0
            || image.height == 0
            || image.width > max_px
            || image.height > max_px
            || !valid_base64_png(&image.png_base64)
        {
            return Err(FetchError::Protocol);
        }
    }
    if cancelled(cancel) {
        return Err(FetchError::Cancelled);
    }
    if Instant::now() >= overall {
        return Err(FetchError::Deadline);
    }
    Ok(result)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tokio::net::{UnixListener, UnixStream};

    struct Profile(PathBuf);
    impl Profile {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "cm-thumb-{}-{}",
                std::process::id(),
                Uuid::now_v7()
            ));
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("token"), " synthetic-token \n").unwrap();
            Self(path)
        }
    }
    impl Drop for Profile {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    struct Peer(Option<tokio::task::JoinHandle<()>>);
    impl Peer {
        async fn join(mut self) {
            timeout_at(
                Instant::now() + Duration::from_secs(4),
                self.0.take().unwrap(),
            )
            .await
            .unwrap()
            .unwrap();
        }
    }
    impl Drop for Peer {
        fn drop(&mut self) {
            if let Some(task) = self.0.take() {
                task.abort();
            }
        }
    }
    fn reply(id: &str, value: serde_json::Value) -> Envelope {
        Envelope {
            v: IPC_VERSION.into(),
            id: id.into(),
            kind: FrameKind::Res,
            method: None,
            params: None,
            result: Some(value),
            error: None,
            seq: None,
        }
    }
    async fn recv(stream: &mut UnixStream) -> Envelope {
        let (_tx, mut rx) = watch::channel(false);
        read_frame(stream, Instant::now() + Duration::from_secs(2), &mut rx)
            .await
            .unwrap()
    }
    async fn send(stream: &mut UnixStream, frame: Envelope) {
        stream
            .write_all(&encode_line(&frame).unwrap())
            .await
            .unwrap();
    }
    fn listener(profile: &Profile) -> UnixListener {
        UnixListener::bind(profile.0.join("clipmonger.sock")).unwrap()
    }
    fn peer(listener: UnixListener, clip: Uuid, answer: Vec<u8>) -> Peer {
        Peer(Some(tokio::spawn(async move {
            let (mut stream, _) =
                timeout_at(Instant::now() + Duration::from_secs(2), listener.accept())
                    .await
                    .unwrap()
                    .unwrap();
            let hello = recv(&mut stream).await;
            assert_eq!(hello.id, "hello-0");
            assert_eq!(hello.method.as_deref(), Some("hello"));
            assert_eq!(
                hello.params.unwrap(),
                serde_json::json!({"min":1,"max":1,"token":"synthetic-token","client":"clipmonger-ui-thumbnail"})
            );
            send(
                &mut stream,
                reply("hello-0", serde_json::json!({"chosen":1})),
            )
            .await;
            let request = recv(&mut stream).await;
            assert_eq!(request.id, "t0");
            assert_eq!(request.method.as_deref(), Some("history.thumbnail"));
            assert_eq!(
                request.params.unwrap(),
                serde_json::json!({"clip":clip,"max_px":128,"mime":"image/png"})
            );
            let _ = stream.write_all(&answer).await;
            let mut one = [0];
            assert!(matches!(
                timeout_at(
                    Instant::now() + Duration::from_secs(3),
                    stream.read(&mut one)
                )
                .await
                .unwrap(),
                Ok(0) | Err(_)
            ));
        })))
    }
    fn image(clip: Uuid, hash: &str) -> serde_json::Value {
        serde_json::json!({"thumbnail":{"clip":clip,"content_hash":hash,"mime":"image/png","width":1,"height":1,"png_base64":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/aXcAAAAASUVORK5CYII="}})
    }
    #[tokio::test]
    async fn wire_replies_are_correlated_and_validated() {
        let clip = Uuid::now_v7();
        let hash = "a".repeat(64);
        let other = Uuid::now_v7();
        let cases = vec![
            (reply("t0", image(clip, &hash)), None),
            (reply("t0", serde_json::json!({"thumbnail":null})), None),
            (
                reply("wrong", image(clip, &hash)),
                Some(FetchError::Protocol),
            ),
            (
                reply("t0", serde_json::json!({})),
                Some(FetchError::Protocol),
            ),
            (
                {
                    let mut f = reply("t0", image(clip, &hash));
                    f.error = Some(clipmonger_ipc::proto::IpcError::new(
                        "peer",
                        "secret".into(),
                    ));
                    f
                },
                Some(FetchError::Protocol),
            ),
            (
                reply(
                    "t0",
                    serde_json::json!({"thumbnail":{"clip":clip,"content_hash":hash,"mime":"image/png","width":0,"height":2,"png_base64":"Zm9v"}}),
                ),
                Some(FetchError::Protocol),
            ),
            (
                reply(
                    "t0",
                    serde_json::json!({"thumbnail":{"clip":clip,"content_hash":hash,"mime":"image/png","width":4,"height":2,"png_base64":"!!!="}}),
                ),
                Some(FetchError::Protocol),
            ),
            (reply("t0", image(other, &hash)), Some(FetchError::Stale)),
            (
                reply("t0", image(clip, &"b".repeat(64))),
                Some(FetchError::Stale),
            ),
            (
                {
                    let mut f = reply("t0", image(clip, &hash));
                    f.v = "wrong".into();
                    f
                },
                Some(FetchError::Protocol),
            ),
            (
                {
                    let mut f = reply("t0", image(clip, &hash));
                    f.kind = FrameKind::Event;
                    f
                },
                Some(FetchError::Protocol),
            ),
            (
                reply(
                    "t0",
                    serde_json::json!({"thumbnail":{"clip":clip,"content_hash":hash,"mime":"text/plain","width":4,"height":2,"png_base64":"Zm9v"}}),
                ),
                Some(FetchError::Protocol),
            ),
        ];
        for (frame, error) in cases {
            let profile = Profile::new();
            let listener = listener(&profile);
            // Serialize manually so the version-mismatch control reaches the peer.
            let mut bytes = serde_json::to_vec(&frame).unwrap();
            bytes.push(b'\n');
            let peer = peer(listener, clip, bytes);
            let (_tx, mut cancel) = watch::channel(false);
            let outcome = fetch(&profile.0, clip, &hash, 128, &mut cancel).await;
            assert_eq!(outcome.err(), error);
            peer.join().await;
        }
    }
    #[tokio::test]
    async fn malformed_oversize_unterminated_and_missing_peer_fail() {
        let clip = Uuid::now_v7();
        let hash = "a".repeat(64);
        for bytes in [
            b"{bad}\n".to_vec(),
            vec![b'x'; MAX_LINE_BYTES + 1],
            b"unterminated".to_vec(),
        ] {
            let profile = Profile::new();
            let listener = listener(&profile);
            let peer = peer(listener, clip, bytes);
            let (_tx, mut cancel) = watch::channel(false);
            assert!(fetch(&profile.0, clip, &hash, 128, &mut cancel)
                .await
                .is_err());
            peer.join().await;
        }
        let profile = Profile::new();
        let (_tx, mut cancel) = watch::channel(false);
        assert_eq!(
            fetch(&profile.0, clip, &hash, 128, &mut cancel)
                .await
                .unwrap_err(),
            FetchError::Connect
        );
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancellation_closes_stalled_hello_and_reply() {
        for stall_reply in [false, true] {
            let profile = Profile::new();
            let listener = listener(&profile);
            let clip = Uuid::now_v7();
            let hash = "a".repeat(64);
            let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
            let task = Peer(Some(tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let hello = recv(&mut stream).await;
                if stall_reply {
                    send(
                        &mut stream,
                        reply(&hello.id, serde_json::json!({"chosen":1})),
                    )
                    .await;
                    assert_eq!(
                        recv(&mut stream).await.method.as_deref(),
                        Some("history.thumbnail")
                    );
                }
                ready_tx.send(()).unwrap();
                let mut one = [0];
                assert_eq!(
                    timeout_at(
                        Instant::now() + Duration::from_secs(2),
                        stream.read(&mut one)
                    )
                    .await
                    .unwrap()
                    .unwrap(),
                    0
                );
            })));
            let (tx, mut cancel) = watch::channel(false);
            let path = profile.0.clone();
            let job =
                tokio::spawn(async move { fetch(&path, clip, &hash, 128, &mut cancel).await });
            timeout_at(Instant::now() + Duration::from_secs(2), ready_rx)
                .await
                .unwrap()
                .unwrap();
            tx.send(true).unwrap();
            assert_eq!(
                timeout_at(Instant::now() + Duration::from_secs(2), job)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap_err(),
                FetchError::Cancelled
            );
            task.join().await;
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bridge_status_completes_while_thumbnail_reply_is_stalled() {
        use crate::ipc_bridge::{Bridge, Transport, UiCmd, UiEvent};
        struct ClientTransport(clipmonger_ipc::client::Client);
        impl Transport for ClientTransport {
            fn call(
                &mut self,
                method: &str,
                params: &serde_json::Value,
            ) -> Result<serde_json::Value, clipmonger_ipc::proto::IpcError> {
                self.0.call(method, params.clone())
            }
        }
        let profile = Profile::new();
        let listener = listener(&profile);
        let clip = Uuid::now_v7();
        let hash = "a".repeat(64);
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let task = Peer(Some(tokio::spawn(async move {
            let (mut thumb, _) = listener.accept().await.unwrap();
            let hello = recv(&mut thumb).await;
            send(
                &mut thumb,
                reply(&hello.id, serde_json::json!({"chosen":1})),
            )
            .await;
            assert_eq!(
                recv(&mut thumb).await.method.as_deref(),
                Some("history.thumbnail")
            );
            ready_tx.send(()).unwrap();
            let (mut bridge, _) =
                timeout_at(Instant::now() + Duration::from_secs(2), listener.accept())
                    .await
                    .unwrap()
                    .unwrap();
            let hello = recv(&mut bridge).await;
            send(
                &mut bridge,
                reply(&hello.id, serde_json::json!({"chosen":1})),
            )
            .await;
            let status = recv(&mut bridge).await;
            assert_eq!(status.method.as_deref(), Some("status"));
            send(
                &mut bridge,
                reply(&status.id, serde_json::json!({"ok":true})),
            )
            .await;
            let mut byte = [0];
            assert_eq!(
                timeout_at(
                    Instant::now() + Duration::from_secs(2),
                    thumb.read(&mut byte)
                )
                .await
                .unwrap()
                .unwrap(),
                0
            );
        })));
        let (tx, mut cancel) = watch::channel(false);
        let path = profile.0.clone();
        let job = tokio::spawn(async move { fetch(&path, clip, &hash, 128, &mut cancel).await });
        timeout_at(Instant::now() + Duration::from_secs(2), ready_rx)
            .await
            .unwrap()
            .unwrap();
        let path = profile.0.join("clipmonger.sock");
        let result = tokio::task::spawn_blocking(move || {
            let client = clipmonger_ipc::client::Client::connect(&path, "synthetic-token").unwrap();
            let (commands, events) = Bridge::spawn(Box::new(ClientTransport(client)));
            commands.send(UiCmd::Status).unwrap();
            let event = events.recv_timeout(Duration::from_secs(2)).unwrap();
            drop(commands);
            event
        });
        assert!(matches!(
            timeout_at(Instant::now() + Duration::from_secs(2), result)
                .await
                .unwrap()
                .unwrap(),
            UiEvent::Status(_)
        ));
        tx.send(true).unwrap();
        assert_eq!(job.await.unwrap().unwrap_err(), FetchError::Cancelled);
        task.join().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn slow_drip_does_not_reset_deadline_or_replay_request() {
        let profile = Profile::new();
        let listener = listener(&profile);
        let clip = Uuid::now_v7();
        let hash = "a".repeat(64);
        let task = Peer(Some(tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let hello = recv(&mut stream).await;
            send(
                &mut stream,
                reply(&hello.id, serde_json::json!({"chosen":1})),
            )
            .await;
            assert_eq!(
                recv(&mut stream).await.method.as_deref(),
                Some("history.thumbnail")
            );
            // Continue writing small pieces until the client closes or the finite deadline expires.
            let end = Instant::now() + Duration::from_secs(3);
            while Instant::now() < end {
                if stream.write_all(b" ").await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            assert!(
                tokio::time::timeout(Duration::from_millis(200), listener.accept())
                    .await
                    .is_err(),
                "request was replayed"
            );
        })));
        let (_tx, mut cancel) = watch::channel(false);
        let started = Instant::now();
        assert_eq!(
            fetch(&profile.0, clip, &hash, 128, &mut cancel)
                .await
                .unwrap_err(),
            FetchError::Deadline
        );
        assert!(started.elapsed() < Duration::from_secs(3));
        task.join().await;
    }
    #[tokio::test]
    async fn cancellation_interrupts_yielding_connect_wait() {
        let (tx, mut cancel) = watch::channel(false);
        let signal = tokio::spawn(async move {
            tokio::task::yield_now().await;
            tx.send(true).unwrap();
        });
        let result = bounded(
            Instant::now() + Duration::from_secs(2),
            &mut cancel,
            std::future::pending::<()>(),
        )
        .await;
        assert_eq!(result.unwrap_err(), FetchError::Cancelled);
        signal.await.unwrap();
    }
}
