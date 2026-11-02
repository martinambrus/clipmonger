use super::*;
use std::sync::{atomic::{AtomicUsize, Ordering}, mpsc, Arc};
use std::time::Duration;

struct Profile(PathBuf);
impl Profile {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("cm-thumbnail-unit-{}-{}", std::process::id(), uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Profile {
    fn drop(&mut self) { std::fs::remove_dir_all(&self.0).expect("remove owned profile"); }
}
struct Worker(Option<std::thread::JoinHandle<Result<Value, IpcError>>>);
impl Worker {
    fn join(mut self) -> Result<Value, IpcError> { self.0.take().unwrap().join().expect("worker panicked") }
}
impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(worker) = self.0.take() {
            let result = worker.join();
            if !std::thread::panicking() { let _ = result.expect("worker panicked"); }
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
        let _ = release_rx.lock().unwrap().recv_timeout(Duration::from_secs(3));
    })));
    let id = uuid::Uuid::now_v7();
    let worker_state = state.clone();
    let worker = Worker(Some(std::thread::spawn(move || worker_state.handle("history.thumbnail", &serde_json::json!({"clip": id.to_string()})))));
    entered_rx.recv_timeout(Duration::from_secs(2)).expect("first snapshot entered");
    assert_eq!(state.handle("history.thumbnail", &serde_json::json!({"clip": id.to_string()})).unwrap_err().code, "busy");
    assert_eq!(state.handle("history.thumbnail", &serde_json::json!({"clip": id.to_string(), "max_px": null})).unwrap_err().code, "invalid_params");
    assert_eq!(state.handle("history.thumbnail", &serde_json::json!({"clip": id.to_string(), "mime": null})).unwrap_err().code, "invalid_params");
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
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| state.handle("history.thumbnail", &params)));
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
    fn release(&self) { std::fs::write(&self.release, b"release").expect("release fixture"); }
    fn join_decoder(&mut self) -> Result<Value, IpcError> { self.decoder.take().unwrap().join().expect("decoder worker panicked") }
    fn join_restore(&mut self) -> Result<Value, IpcError> { self.restore.take().unwrap().join().expect("restore worker panicked") }
}
impl Drop for Controlled {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.release, b"release");
        for handle in [self.decoder.take(), self.restore.take()].into_iter().flatten() {
            let joined = handle.join();
            if !std::thread::panicking() { joined.expect("owned worker panicked").ok(); }
        }
    }
}
fn await_pid(path: &Path) -> u32 {
    let end = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(pid) = text.parse() { return pid; }
        }
        assert!(std::time::Instant::now() < end, "fixture did not signal its PID");
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn fixture_png(color: [u8; 4]) -> Vec<u8> {
    use image::ImageEncoder;
    let image = image::RgbaImage::from_pixel(4, 2, image::Rgba(color));
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out).write_image(image.as_raw(), 4, 2, image::ExtendedColorType::Rgba8).unwrap();
    out
}
fn fixture_clip(id: uuid::Uuid, bytes: Vec<u8>) -> (clipmonger_core::model::Clip, Vec<(String, Vec<u8>)>) {
    let payloads = vec![("image/png".into(), bytes)];
    let clip = clipmonger_core::model::Clip {
        id, created_ms: 1000, source_app: None, source_os: "test".into(), pinned: false, removed: false,
        preview: "test".into(), search_text: "test".into(), formats: vec!["image/png".into()], bytes_total: 0,
        content_hash: clipmonger_core::hash::content_hash(&payloads),
        rev: clipmonger_core::model::Rev { wall_ms: 1000, dev: "test".into(), ctr: 1 },
        last_used_ms: 1000, origin_dev: "test".into(), origin: "test".into(),
    };
    (clip, payloads)
}
fn controlled_state(store: Store, path: &Path, hook: Option<Arc<dyn Fn() + Send + Sync>>, wait_hook: Option<Arc<dyn Fn() + Send + Sync>>) -> (Arc<DaemonState>, Cleanup, Arc<AtomicUsize>) {
    let observation = Arc::new(Mutex::new(None));
    let live_checks = Arc::new(AtomicUsize::new(0));
    let service = crate::thumbnail::ThumbnailService::controlled_fixture(path.to_path_buf(), observation.clone(), hook, live_checks.clone(), wait_hook);
    (Arc::new(DaemonState::new_with_thumbnail(store, service)), observation, live_checks)
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
    let decoder = std::thread::spawn(move || state1.handle("history.thumbnail", &serde_json::json!({"clip": id.to_string()})));
    let mut owned = Controlled { release: path.with_extension("release"), decoder: Some(decoder), restore: None };
    let pid = await_pid(&path);
    let state2 = state.clone();
    let archive_arg = archive.to_string_lossy().into_owned();
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    owned.restore = Some(std::thread::spawn(move || {
        let result = state2.handle("backup.restore", &serde_json::json!({"src": archive_arg}));
        let _ = done_tx.send(());
        result
    }));
    done_rx.recv_timeout(Duration::from_secs(3)).expect("restore while decoder remains active");
    owned.join_restore().expect("restore succeeded");
    let previous = live_checks.load(Ordering::SeqCst);
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while live_checks.load(Ordering::SeqCst) == previous {
        assert!(std::time::Instant::now() < deadline, "supervisor did not observe the child alive after restore");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(observation.lock().unwrap().is_none());
    assert_eq!(state.handle("history.thumbnail", &serde_json::json!({"clip": id.to_string()})).unwrap_err().code, "busy");
    owned.release();
    let result = owned.join_decoder().unwrap();
    assert_eq!(result["thumbnail"]["content_hash"], original.content_hash);
    let data = b64_decode(result["thumbnail"]["png_base64"].as_str().unwrap()).unwrap();
    assert_eq!(image::load_from_memory(&data).unwrap().to_rgba8().get_pixel(0, 0).0, [10, 20, 30, 255]);
    assert_eq!(*observation.lock().unwrap(), Some((pid, true, true)));
    let restored = state.with_store(|store| store.get_payload(id, "image/png", 1_000_000).map_err(|e| IpcError::new(e.code(), "store error".to_string()))).unwrap().unwrap();
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
        let _ = release_rx.lock().unwrap().recv_timeout(Duration::from_secs(3));
    });
    let (wait_go_tx, wait_go_rx) = mpsc::sync_channel(1);
    let (wait_arrived_tx, wait_arrived_rx) = mpsc::sync_channel(1);
    let wait_go_rx = Mutex::new(wait_go_rx);
    let wait_hook = Arc::new(move || {
        if wait_go_rx.lock().unwrap().recv_timeout(Duration::from_secs(3)).is_ok() {
            let _ = wait_arrived_tx.send(());
        }
    });
    let (state, observation, live_checks) = controlled_state(store, &path, Some(hook), Some(wait_hook));
    let worker_state = state.clone();
    let decoder = std::thread::spawn(move || worker_state.handle("history.thumbnail", &serde_json::json!({"clip": id.to_string()})));
    let mut owned = Controlled { release: path.with_extension("release"), decoder: Some(decoder), restore: None };
    let pid = await_pid(&path);
    let previous = live_checks.load(Ordering::SeqCst);
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while live_checks.load(Ordering::SeqCst) == previous {
        assert!(std::time::Instant::now() < deadline, "supervisor did not observe the child alive before shutdown");
        std::thread::sleep(Duration::from_millis(5));
    }
    let shutdown_state = state.clone();
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    owned.restore = Some(std::thread::spawn(move || {
        shutdown_state.thumbnail_shutdown_and_wait();
        let _ = done_tx.send(());
        Ok(serde_json::json!({"done": true}))
    }));
    entered_rx.recv_timeout(Duration::from_secs(2)).expect("cleanup hook entered");
    assert_eq!(*observation.lock().unwrap(), Some((pid, true, true)));
    wait_go_tx.send(()).unwrap();
    wait_arrived_rx.recv_timeout(Duration::from_secs(2)).expect("shutdown did not reach wait_idle");
    assert!(matches!(done_rx.recv_timeout(Duration::from_millis(100)), Err(mpsc::RecvTimeoutError::Timeout)));
    assert_eq!(state.handle("history.thumbnail", &serde_json::json!({"clip": id.to_string()})).unwrap_err().code, "shutdown");
    release_tx.send(()).unwrap();
    done_rx.recv_timeout(Duration::from_secs(2)).expect("shutdown completed");
    owned.join_restore().unwrap();
    assert_eq!(owned.join_decoder().unwrap_err().code, "shutdown");
}

#[test]
fn base64_segments_and_padding_round_trip() {
    let segment = clipmonger_ipc::proto::CHUNK_RAW_BYTES / 3 * 3;
    for len in [0, 1, 2, 3, segment - 1, segment, segment + 1, segment + 2] {
        let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        let encoded = encode_thumbnail_png(&data);
        assert_eq!(b64_decode(&encoded).unwrap(), data);
    }
    assert_eq!(encode_thumbnail_png(b"f"), "Zg==");
    assert_eq!(encode_thumbnail_png(b"fo"), "Zm8=");
    assert_eq!(encode_thumbnail_png(b"foo"), "Zm9v");
}
