use super::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc, Arc,
};
use std::time::Duration;

struct Profile(PathBuf);
impl Profile {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "cm-thumbnail-unit-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Profile {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).expect("remove owned profile");
    }
}
struct Worker(Option<std::thread::JoinHandle<Result<Value, IpcError>>>);
impl Worker {
    fn join(mut self) -> Result<Value, IpcError> {
        self.0.take().unwrap().join().expect("worker panicked")
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(worker) = self.0.take() {
            let result = worker.join();
            if !std::thread::panicking() {
                let _ = result.expect("worker panicked");
            }
        }
    }
}
#[test]
fn admission_precedes_snapshot_and_validation_precedes_admission() {
    let profile = Profile::new();
    let state = Arc::new(DaemonState::new(Store::open(&profile.0).unwrap()));
    let calls = Arc::new(AtomicUsize::new(0));
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let release_rx = Mutex::new(release_rx);
    let observed = calls.clone();
    state.set_thumbnail_snapshot_hook(Some(Arc::new(move || {
        observed.fetch_add(1, Ordering::SeqCst);
        let _ = entered_tx.send(());
        let _ = release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(3));
    })));
    let id = uuid::Uuid::now_v7();
    let worker_state = state.clone();
    let worker = Worker(Some(std::thread::spawn(move || {
        worker_state.handle(
            "history.thumbnail",
            &serde_json::json!({"clip": id.to_string()}),
        )
    })));
    entered_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("first snapshot entered");
    assert_eq!(
        state
            .handle(
                "history.thumbnail",
                &serde_json::json!({"clip": id.to_string()})
            )
            .unwrap_err()
            .code,
        "busy"
    );
    assert_eq!(
        state
            .handle(
                "history.thumbnail",
                &serde_json::json!({"clip": id.to_string(), "max_px": null})
            )
            .unwrap_err()
            .code,
        "invalid_params"
    );
    assert_eq!(
        state
            .handle(
                "history.thumbnail",
                &serde_json::json!({"clip": id.to_string(), "mime": null})
            )
            .unwrap_err()
            .code,
        "invalid_params"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    release_tx.send(()).unwrap();
    assert!(worker.join().unwrap()["thumbnail"].is_null());
}
#[test]
fn unwind_releases_admission() {
    let profile = Profile::new();
    let state = DaemonState::new(Store::open(&profile.0).unwrap());
    state.set_thumbnail_snapshot_hook(Some(Arc::new(|| panic!("controlled unwind"))));
    let params = serde_json::json!({"clip": uuid::Uuid::now_v7().to_string()});
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        state.handle("history.thumbnail", &params)
    }));
    assert!(result.is_err());
    state.set_thumbnail_snapshot_hook(None);
    assert!(state.handle("history.thumbnail", &params).unwrap()["thumbnail"].is_null());
}

type Cleanup = Arc<Mutex<Option<(u32, bool, bool)>>>;
struct Controlled {
    release: PathBuf,
    decoder: Option<std::thread::JoinHandle<Result<Value, IpcError>>>,
    restore: Option<std::thread::JoinHandle<Result<Value, IpcError>>>,
}
impl Controlled {
    fn release(&self) {
        std::fs::write(&self.release, b"release").expect("release fixture");
    }
    fn join_decoder(&mut self) -> Result<Value, IpcError> {
        self.decoder
            .take()
            .unwrap()
            .join()
            .expect("decoder worker panicked")
    }
    fn join_restore(&mut self) -> Result<Value, IpcError> {
        self.restore
            .take()
            .unwrap()
            .join()
            .expect("restore worker panicked")
    }
}
impl Drop for Controlled {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.release, b"release");
        for handle in [self.decoder.take(), self.restore.take()]
            .into_iter()
            .flatten()
        {
            let joined = handle.join();
            if !std::thread::panicking() {
                joined.expect("owned worker panicked").ok();
            }
        }
    }
}
fn await_pid(path: &Path) -> u32 {
    let end = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(pid) = text.parse() {
                return pid;
            }
        }
        assert!(
            std::time::Instant::now() < end,
            "fixture did not signal its PID"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn fixture_png(color: [u8; 4]) -> Vec<u8> {
    use image::ImageEncoder;
    let image = image::RgbaImage::from_pixel(4, 2, image::Rgba(color));
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(image.as_raw(), 4, 2, image::ExtendedColorType::Rgba8)
        .unwrap();
    out
}
fn fixture_clip(
    id: uuid::Uuid,
    bytes: Vec<u8>,
) -> (clipmonger_core::model::Clip, Vec<(String, Vec<u8>)>) {
    let payloads = vec![("image/png".into(), bytes)];
    let clip = clipmonger_core::model::Clip {
        id,
        created_ms: 1000,
        source_app: None,
        source_os: "test".into(),
        pinned: false,
        removed: false,
        preview: "test".into(),
        search_text: "test".into(),
        formats: vec!["image/png".into()],
        bytes_total: 0,
        content_hash: clipmonger_core::hash::content_hash(&payloads),
        rev: clipmonger_core::model::Rev {
            wall_ms: 1000,
            dev: "test".into(),
            ctr: 1,
        },
        last_used_ms: 1000,
        origin_dev: "test".into(),
        origin: "test".into(),
    };
    (clip, payloads)
}
fn controlled_state(
    store: Store,
    path: &Path,
    hook: Option<Arc<dyn Fn() + Send + Sync>>,
    wait_hook: Option<Arc<dyn Fn() + Send + Sync>>,
) -> (Arc<DaemonState>, Cleanup, Arc<AtomicUsize>) {
    let observation = Arc::new(Mutex::new(None));
    let live_checks = Arc::new(AtomicUsize::new(0));
    let service = crate::thumbnail::ThumbnailService::controlled_fixture(
        path.to_path_buf(),
        observation.clone(),
        hook,
        live_checks.clone(),
        wait_hook,
    );
    (
        Arc::new(DaemonState::new_with_thumbnail(store, service)),
        observation,
        live_checks,
    )
}
#[test]
fn restore_completes_with_owned_child_and_returns_original_snapshot() {
    let profile = Profile::new();
    let live = profile.0.join("live");
    let alternate = profile.0.join("alternate");
    let archive = profile.0.join("restore.zip");
    let id = uuid::Uuid::now_v7();
    let (original, original_data) = fixture_clip(id, fixture_png([10, 20, 30, 255]));
    let (replacement, replacement_data) = fixture_clip(id, fixture_png([70, 80, 90, 255]));
    assert_ne!(original.content_hash, replacement.content_hash);
    {
        let store = Store::open(&alternate).unwrap();
        store.insert_clip(&replacement, &replacement_data).unwrap();
        store.backup_zip(&archive).unwrap();
    }
    let store = Store::open(&live).unwrap();
    store.insert_clip(&original, &original_data).unwrap();
    let path = profile.0.join("child.controlled");
    let (state, observation, live_checks) = controlled_state(store, &path, None, None);
    state.set_profile_dir(&live);
    let state1 = state.clone();
    let decoder = std::thread::spawn(move || {
        state1.handle(
            "history.thumbnail",
            &serde_json::json!({"clip": id.to_string()}),
        )
    });
    let mut owned = Controlled {
        release: path.with_extension("release"),
        decoder: Some(decoder),
        restore: None,
    };
    let pid = await_pid(&path);
    let state2 = state.clone();
    let archive_arg = archive.to_string_lossy().into_owned();
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    owned.restore = Some(std::thread::spawn(move || {
        let result = state2.handle("backup.restore", &serde_json::json!({"src": archive_arg}));
        let _ = done_tx.send(());
        result
    }));
    done_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("restore while decoder remains active");
    owned.join_restore().expect("restore succeeded");
    let previous = live_checks.load(Ordering::SeqCst);
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while live_checks.load(Ordering::SeqCst) == previous {
        assert!(
            std::time::Instant::now() < deadline,
            "supervisor did not observe the child alive after restore"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(observation.lock().unwrap().is_none());
    assert_eq!(
        state
            .handle(
                "history.thumbnail",
                &serde_json::json!({"clip": id.to_string()})
            )
            .unwrap_err()
            .code,
        "busy"
    );
    owned.release();
    let result = owned.join_decoder().unwrap();
    assert_eq!(result["thumbnail"]["content_hash"], original.content_hash);
    let data = b64_decode(result["thumbnail"]["png_base64"].as_str().unwrap()).unwrap();
    assert_eq!(
        image::load_from_memory(&data)
            .unwrap()
            .to_rgba8()
            .get_pixel(0, 0)
            .0,
        [10, 20, 30, 255]
    );
    assert_eq!(*observation.lock().unwrap(), Some((pid, true, true)));
    let restored = state
        .with_store(|store| {
            store
                .get_payload(id, "image/png", 1_000_000)
                .map_err(|e| IpcError::new(e.code(), "store error".to_string()))
        })
        .unwrap()
        .unwrap();
    assert_eq!(restored.content_hash, replacement.content_hash);
}
#[test]
fn shutdown_waits_for_owned_child_and_both_pipe_joins() {
    let profile = Profile::new();
    let store = Store::open(&profile.0).unwrap();
    let id = uuid::Uuid::now_v7();
    let (clip, payloads) = fixture_clip(id, fixture_png([10, 20, 30, 255]));
    store.insert_clip(&clip, &payloads).unwrap();
    let path = profile.0.join("child.controlled");
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let release_rx = Mutex::new(release_rx);
    let hook = Arc::new(move || {
        let _ = entered_tx.send(());
        let _ = release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(3));
    });
    let (wait_go_tx, wait_go_rx) = mpsc::sync_channel(1);
    let (wait_arrived_tx, wait_arrived_rx) = mpsc::sync_channel(1);
    let wait_go_rx = Mutex::new(wait_go_rx);
    let wait_hook = Arc::new(move || {
        if wait_go_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(3))
            .is_ok()
        {
            let _ = wait_arrived_tx.send(());
        }
    });
    let (state, observation, live_checks) =
        controlled_state(store, &path, Some(hook), Some(wait_hook));
    let worker_state = state.clone();
    let decoder = std::thread::spawn(move || {
        worker_state.handle(
            "history.thumbnail",
            &serde_json::json!({"clip": id.to_string()}),
        )
    });
    let mut owned = Controlled {
        release: path.with_extension("release"),
        decoder: Some(decoder),
        restore: None,
    };
    let pid = await_pid(&path);
    let previous = live_checks.load(Ordering::SeqCst);
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while live_checks.load(Ordering::SeqCst) == previous {
        assert!(
            std::time::Instant::now() < deadline,
            "supervisor did not observe the child alive before shutdown"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let shutdown_state = state.clone();
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    owned.restore = Some(std::thread::spawn(move || {
        shutdown_state.thumbnail_shutdown_and_wait();
        let _ = done_tx.send(());
        Ok(serde_json::json!({"done": true}))
    }));
    entered_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("cleanup hook entered");
    assert_eq!(*observation.lock().unwrap(), Some((pid, true, true)));
    wait_go_tx.send(()).unwrap();
    wait_arrived_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("shutdown did not reach wait_idle");
    assert!(matches!(
        done_rx.recv_timeout(Duration::from_millis(100)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    assert_eq!(
        state
            .handle(
                "history.thumbnail",
                &serde_json::json!({"clip": id.to_string()})
            )
            .unwrap_err()
            .code,
        "shutdown"
    );
    release_tx.send(()).unwrap();
    done_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("shutdown completed");
    owned.join_restore().unwrap();
    assert_eq!(owned.join_decoder().unwrap_err().code, "shutdown");
}

#[test]
fn base64_segments_and_padding_round_trip() {
    let segment = clipmonger_ipc::proto::CHUNK_RAW_BYTES / 3 * 3;
    for len in [0, 1, 2, 3, segment - 1, segment, segment + 1, segment + 2] {
        let data: Vec<u8> = (0..len).map(|i| u8::try_from(i % 251).unwrap()).collect();
        let encoded = encode_thumbnail_png(&data);
        assert_eq!(b64_decode(&encoded).unwrap(), data);
    }
    assert_eq!(encode_thumbnail_png(b"f"), "Zg==");
    assert_eq!(encode_thumbnail_png(b"fo"), "Zm8=");
    assert_eq!(encode_thumbnail_png(b"foo"), "Zm9v");
}

#[cfg(unix)]
mod wire_cancel {
    use super::*;
    use clipmonger_ipc::{
        framing::{decode_line, encode_line},
        proto::{Envelope, FrameKind, IPC_VERSION},
        server::{Handler, ServeOptions, Server},
    };
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::unix::{OwnedReadHalf, OwnedWriteHalf},
        time::timeout,
    };

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
    async fn send(w: &mut OwnedWriteHalf, f: Envelope) {
        timeout(
            Duration::from_secs(2),
            w.write_all(&encode_line(&f).unwrap()),
        )
        .await
        .unwrap()
        .unwrap();
    }
    async fn receive(r: &mut BufReader<OwnedReadHalf>) -> Envelope {
        let mut line = Vec::new();
        assert_ne!(
            timeout(Duration::from_secs(2), r.read_until(b'\n', &mut line))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        decode_line(&line).unwrap()
    }
    async fn connect(path: &Path) -> (BufReader<OwnedReadHalf>, OwnedWriteHalf) {
        let (r, mut w) = tokio::net::UnixStream::connect(path)
            .await
            .unwrap()
            .into_split();
        let mut r = BufReader::new(r);
        send(
            &mut w,
            frame(
                "hello",
                FrameKind::Req,
                Some("hello"),
                Some(serde_json::json!({"token":"tok"})),
            ),
        )
        .await;
        assert!(receive(&mut r).await.result.is_some());
        (r, w)
    }
    async fn wait_until(mut condition: impl FnMut() -> bool) {
        timeout(Duration::from_secs(2), async {
            while !condition() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for observation");
    }
    async fn parsed_pid(path: &Path) -> u32 {
        timeout(Duration::from_secs(2), async {
            loop {
                if let Some(pid) = std::fs::read_to_string(path)
                    .ok()
                    .and_then(|text| text.parse().ok())
                {
                    return pid;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("fixture did not write a parseable PID")
    }
    struct Release(PathBuf);
    impl Drop for Release {
        fn drop(&mut self) {
            let _ = std::fs::write(&self.0, b"release");
        }
    }

    #[allow(clippy::too_many_lines)] // One fixture covers the full child lifecycle.
    async fn active_child(disconnect: bool) {
        let profile = Profile::new();
        let store = Store::open(&profile.0).unwrap();
        let id = uuid::Uuid::now_v7();
        let (clip, payloads) = fixture_clip(id, fixture_png([10, 20, 30, 255]));
        store.insert_clip(&clip, &payloads).unwrap();
        let path = profile.0.join("child.controlled");
        let _release = Release(path.with_extension("release"));
        let (state, observation, live) = controlled_state(store, &path, None, None);
        let handler_state = state.clone();
        let handler: Handler =
            Arc::new(move |method, params| handler_state.handle(&method, &params));
        let socket = profile.0.join("ipc.sock");
        let _server = Server::bind_with_context(
            &socket,
            "tok".into(),
            handler,
            ServeOptions::default(),
            DaemonState::thumbnail_context_handler(state.clone()),
        )
        .await
        .unwrap();
        let (first_read, mut first_write) = connect(&socket).await;
        let mut first_read = Some(first_read);
        let (mut second_read, mut second_write) = connect(&socket).await;
        let params = serde_json::json!({"clip":id.to_string()});
        send(
            &mut first_write,
            frame(
                "same",
                FrameKind::Req,
                Some("history.thumbnail"),
                Some(params.clone()),
            ),
        )
        .await;
        let pid = parsed_pid(&path).await;
        let previous = live.load(Ordering::SeqCst);
        wait_until(|| live.load(Ordering::SeqCst) > previous).await;
        assert!(observation.lock().unwrap().is_none());
        send(
            &mut second_write,
            frame(
                "same",
                FrameKind::Req,
                Some("history.thumbnail"),
                Some(params.clone()),
            ),
        )
        .await;
        assert_eq!(receive(&mut second_read).await.error.unwrap().code, "busy");
        if disconnect {
            drop(first_write);
            drop(first_read.take());
        } else {
            send(
                &mut first_write,
                frame("same", FrameKind::Cancel, None, None),
            )
            .await;
        }
        wait_until(|| observation.lock().unwrap().is_some()).await;
        assert_eq!(*observation.lock().unwrap(), Some((pid, true, true)));
        if !disconnect {
            let reply = receive(first_read.as_mut().unwrap()).await;
            assert_eq!(reply.error.unwrap().code, "cancelled");
        }
        wait_until(|| state.thumbnail.try_admit().is_ok()).await;
        // The other connection retains its same ID and can reuse the permit.
        std::fs::remove_file(&path).unwrap();
        *observation.lock().unwrap() = None;
        let previous = live.load(Ordering::SeqCst);
        send(
            &mut second_write,
            frame(
                "same",
                FrameKind::Req,
                Some("history.thumbnail"),
                Some(params),
            ),
        )
        .await;
        let second_pid = parsed_pid(&path).await;
        wait_until(|| live.load(Ordering::SeqCst) > previous).await;
        std::fs::write(path.with_extension("release"), b"release").unwrap();
        let result = receive(&mut second_read).await.result.unwrap();
        assert_eq!(result["thumbnail"]["content_hash"], clip.content_hash);
        let data = b64_decode(result["thumbnail"]["png_base64"].as_str().unwrap()).unwrap();
        assert_eq!(
            image::load_from_memory(&data)
                .unwrap()
                .to_rgba8()
                .get_pixel(0, 0)
                .0,
            [10, 20, 30, 255]
        );
        assert_eq!(*observation.lock().unwrap(), Some((second_pid, true, true)));
        send(
            &mut second_write,
            frame("ping", FrameKind::Req, Some("ping"), None),
        )
        .await;
        assert_eq!(
            receive(&mut second_read).await.result.unwrap()["pong"],
            true
        );
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_reaps_owned_child_and_preserves_other_connection() {
        active_child(false).await;
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn disconnect_reaps_owned_child_and_preserves_other_connection() {
        active_child(true).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_during_snapshot_prevents_spawn_and_releases_permit() {
        struct ReleaseSender(Option<mpsc::SyncSender<()>>);
        impl Drop for ReleaseSender {
            fn drop(&mut self) {
                if let Some(tx) = self.0.take() {
                    let _ = tx.try_send(());
                }
            }
        }
        let profile = Profile::new();
        let store = Store::open(&profile.0).unwrap();
        let id = uuid::Uuid::now_v7();
        let (clip, payloads) = fixture_clip(id, fixture_png([1, 2, 3, 255]));
        store.insert_clip(&clip, &payloads).unwrap();
        let path = profile.0.join("child.controlled");
        let (state, observation, _) = controlled_state(store, &path, None, None);
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let entered_tx = Mutex::new(Some(entered_tx));
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let release_rx = Mutex::new(release_rx);
        let mut release = ReleaseSender(Some(release_tx));
        state.set_thumbnail_snapshot_hook(Some(Arc::new(move || {
            if let Some(tx) = entered_tx.lock().unwrap().take() {
                let _ = tx.send(());
            }
            let _ = release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3));
        })));
        let handler_state = state.clone();
        let handler: Handler =
            Arc::new(move |method, params| handler_state.handle(&method, &params));
        let socket = profile.0.join("ipc.sock");
        let _server = Server::bind_with_context(
            &socket,
            "tok".into(),
            handler,
            ServeOptions::default(),
            DaemonState::thumbnail_context_handler(state.clone()),
        )
        .await
        .unwrap();
        let (mut read, mut write) = connect(&socket).await;
        send(
            &mut write,
            frame(
                "job",
                FrameKind::Req,
                Some("history.thumbnail"),
                Some(serde_json::json!({"clip": id.to_string()})),
            ),
        )
        .await;
        timeout(Duration::from_secs(2), entered_rx)
            .await
            .unwrap()
            .unwrap();
        send(&mut write, frame("job", FrameKind::Cancel, None, None)).await;
        send(
            &mut write,
            frame("barrier", FrameKind::Req, Some("ping"), None),
        )
        .await;
        assert_eq!(receive(&mut read).await.id, "barrier");
        release.0.take().unwrap().send(()).unwrap();
        assert_eq!(receive(&mut read).await.error.unwrap().code, "cancelled");
        assert!(!path.exists());
        assert!(observation.lock().unwrap().is_none());
        assert_eq!(state.thumbnail_payload_reads.load(Ordering::SeqCst), 0);
        state.set_thumbnail_snapshot_hook(None);
        assert!(state
            .handle(
                "history.thumbnail",
                &serde_json::json!({"clip":uuid::Uuid::now_v7().to_string()})
            )
            .unwrap()["thumbnail"]
            .is_null());
        assert_eq!(state.thumbnail_payload_reads.load(Ordering::SeqCst), 1);
    }
}
