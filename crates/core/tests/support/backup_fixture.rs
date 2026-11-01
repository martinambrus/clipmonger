//! Fixed seed shared by the zip 4 fixture generator and compatibility test.

use clipmonger_core::model::{Clip, PermanentClip, PermanentKind, Rev};
use uuid::Uuid;

pub const TEXT_MIME: &str = "text/plain;charset=utf-8";
pub const TEXTS: [&str; 2] = ["zip 4 ordinary text\n", "zip 4 pinned text: café\n"];
pub const IDS: [u128; 3] = [
    0x018b_cfe5_6800_7000_8000_0000_0000_0001,
    0x018b_cfe5_6800_7000_8000_0000_0000_0002,
    0x018b_cfe5_6800_7000_8000_0000_0000_0003,
];
pub const BLOB_MIME: &str = "application/octet-stream";
pub const BLOB_LEN: usize = 256 * 1024 + 64;
pub const BLOB_BYTE: u8 = 7;
pub const PERMANENT_ID: u128 = 0x018b_cfe5_6800_7000_8000_0000_0000_0004;
pub const PERMANENT_BODY: &str = "zip 4 permanent signature\n";
const WALL_MS: i64 = 1_700_000_000_000;
const DEVICE: &str = "zip4-fixture";

type Payloads = Vec<(String, Vec<u8>)>;

fn revision() -> Rev {
    Rev {
        wall_ms: WALL_MS,
        dev: DEVICE.to_string(),
        ctr: 0,
    }
}

pub fn clips() -> Vec<(Clip, Payloads)> {
    IDS.into_iter()
        .enumerate()
        .map(|(i, id)| {
            let (mime, bytes, preview) = if i < TEXTS.len() {
                (TEXT_MIME, TEXTS[i].as_bytes().to_vec(), TEXTS[i])
            } else {
                (BLOB_MIME, vec![BLOB_BYTE; BLOB_LEN], "zip 4 blob")
            };
            let clip = Clip {
                id: Uuid::from_u128(id),
                created_ms: WALL_MS,
                source_app: Some(DEVICE.to_string()),
                source_os: "linux".to_string(),
                pinned: i == 1,
                removed: false,
                preview: preview.to_string(),
                search_text: preview.to_string(),
                formats: vec![mime.to_string()],
                bytes_total: bytes.len() as u64,
                content_hash: format!("zip4-fixture-{id}"),
                rev: revision(),
                last_used_ms: WALL_MS,
                origin_dev: DEVICE.to_string(),
                origin: "x11".to_string(),
            };
            (clip, vec![(mime.to_string(), bytes)])
        })
        .collect()
}

pub fn permanent() -> PermanentClip {
    PermanentClip {
        id: Uuid::from_u128(PERMANENT_ID),
        group: "Work".to_string(),
        name: "Signature".to_string(),
        hotkey: Some("ctrl+alt+1".to_string()),
        kind: PermanentKind::Paste,
        body: PERMANENT_BODY.to_string(),
        js: None,
        rev: revision(),
    }
}
