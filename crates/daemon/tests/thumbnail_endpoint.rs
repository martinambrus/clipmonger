use clipmonger_core::{hash::content_hash, model::{Clip, Rev}, Store};
use clipmonger_daemon::{b64_decode, thumbnail::ThumbnailService, DaemonState};
use image::ImageEncoder;
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

struct Profile(PathBuf);
impl Profile {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("cm-thumb-endpoint-{}-{}", std::process::id(), Uuid::now_v7()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Profile {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).expect("remove owned thumbnail profile");
    }
}
fn clip(payloads: &[(String, Vec<u8>)]) -> Clip {
    Clip {
        id: Uuid::now_v7(), created_ms: 1000, source_app: None, source_os: "test".into(),
        pinned: false, removed: false, preview: "preview".into(), search_text: "search".into(),
        formats: payloads.iter().map(|(mime, _)| mime.clone()).collect(), bytes_total: 0,
        content_hash: content_hash(payloads),
        rev: Rev { wall_ms: 1000, dev: "test".into(), ctr: 1 }, last_used_ms: 1000,
        origin_dev: "test".into(), origin: "test".into(),
    }
}
fn png(w: u32, h: u32) -> Vec<u8> {
    let image = image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([(x / 2) as u8, (y / 2) as u8, 42, 255]));
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out).write_image(image.as_raw(), w, h, image::ExtendedColorType::Rgba8).unwrap();
    out
}
fn insert(store: &Store, data: Vec<u8>) -> Clip {
    let payloads = vec![("image/png".to_string(), data)];
    let clip = clip(&payloads);
    store.insert_clip(&clip, &payloads).unwrap();
    clip
}
fn state(store: Store) -> DaemonState {
    DaemonState::new_with_thumbnail(store, ThumbnailService::new_with_exe(
        PathBuf::from(env!("CARGO_BIN_EXE_clipmonger-daemon")), Duration::from_secs(3)))
}
fn request(state: &DaemonState, id: Uuid) -> serde_json::Value {
    state.handle("history.thumbnail", &serde_json::json!({"clip": id.to_string()})).unwrap()
}
#[test]
fn real_child_decodes_default_size_gradient_and_no_upscale() {
    let profile = Profile::new();
    let store = Store::open(&profile.0).unwrap();
    let large = insert(&store, png(256, 128));
    let small = insert(&store, png(4, 2));
    let state = state(store);
    let result = request(&state, large.id);
    let thumb = &result["thumbnail"];
    assert_eq!(thumb["clip"], large.id.to_string());
    assert_eq!(thumb["content_hash"], large.content_hash);
    assert_eq!(thumb["mime"], "image/png");
    assert_eq!(thumb["width"], 128);
    assert_eq!(thumb["height"], 64);
    let bytes = b64_decode(thumb["png_base64"].as_str().unwrap()).unwrap();
    let image = image::load_from_memory(&bytes).unwrap().to_rgba8();
    assert_eq!(image.dimensions(), (128, 64));
    assert_ne!(image.get_pixel(0, 0), image.get_pixel(127, 63));
    assert_eq!(image.get_pixel(127, 63).0[2..], [42, 255]);
    let small = request(&state, small.id);
    assert_eq!(small["thumbnail"]["width"], 4);
    assert_eq!(small["thumbnail"]["height"], 2);
}
#[test]
fn failures_and_absences_release_same_service() {
    let profile = Profile::new();
    let store = Store::open(&profile.0).unwrap();
    let good = insert(&store, png(8, 4));
    let corrupt = insert(&store, b"not a png".to_vec());
    let empty = insert(&store, vec![]);
    let external = insert(&store, vec![3; 256 * 1024 + 64]);
    let stub = clip(&[]);
    store.insert_metadata_capture(&stub).unwrap();
    let text_payloads = vec![("text/plain".into(), b"text".to_vec())];
    let text = clip(&text_payloads);
    store.insert_clip(&text, &text_payloads).unwrap();
    let deleted = insert(&store, png(3, 2));
    store.soft_delete(deleted.id).unwrap();
    let conn = rusqlite::Connection::open(profile.0.join("clipstore.sqlite3")).unwrap();
    let key: String = conn.query_row("SELECT blob_key FROM clip_payloads WHERE clip_id=?1", [external.id.to_string()], |row| row.get(0)).unwrap();
    drop(conn);
    std::fs::remove_file(profile.0.join("blobs").join(key)).unwrap();
    let state = state(store);
    for id in [Uuid::now_v7(), stub.id, text.id, deleted.id] {
        assert!(request(&state, id)["thumbnail"].is_null());
        assert!(request(&state, good.id)["thumbnail"].is_object());
    }
    for id in [corrupt.id, empty.id, external.id] {
        assert!(state.handle("history.thumbnail", &serde_json::json!({"clip": id.to_string()})).is_err());
        assert!(request(&state, good.id)["thumbnail"].is_object());
    }
}
#[test]
fn validation_and_typed_null() {
    use clipmonger_ipc::proto::methods::HistoryThumbnailResult;
    let profile = Profile::new();
    let state = state(Store::open(&profile.0).unwrap());
    let id = Uuid::now_v7().to_string();
    for params in [serde_json::json!({}), serde_json::json!({"clip": 1}),
        serde_json::json!({"clip": "bad"}), serde_json::json!({"clip": id, "max_px": null}),
        serde_json::json!({"clip": id, "max_px": 0}), serde_json::json!({"clip": id, "max_px": 257}),
        serde_json::json!({"clip": id, "max_px": 1.5}), serde_json::json!({"clip": id, "mime": null}),
        serde_json::json!({"clip": id, "mime": "text/plain"})] {
        assert_eq!(state.handle("history.thumbnail", &params).unwrap_err().code, "invalid_params");
    }
    let value = serde_json::to_value(HistoryThumbnailResult { thumbnail: None }).unwrap();
    assert_eq!(value, serde_json::json!({"thumbnail": null}));
    assert_eq!(serde_json::from_value::<HistoryThumbnailResult>(value).unwrap().thumbnail, None);
    for (text, bytes) in [("Zg==", &b"f"[..]), ("Zm8=", &b"fo"[..]), ("Zm9v", &b"foo"[..])] {
        assert_eq!(b64_decode(text).unwrap(), bytes);
    }
}
