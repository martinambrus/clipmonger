//! MIME preview-kind regression (Step 6 manual QA, Ubuntu 24.04, 2026-10-27).
//!
//! A clip whose only format is `text/plain;charset=utf-8` (X11 canonical)
//! was classified as a generic "Clip" instead of "Text clip".
//!
//! Run: `cargo test -p clipmonger-app-ui --test mime_preview_acceptance`

use clipmonger_app_ui::popup;
use clipmonger_core::model::Clip;
use serde_json::json;

fn clip_with_formats(formats: &[&str]) -> Clip {
    serde_json::from_value(json!({
        "id": "0191c2f4-7a2b-7c3d-8e4f-556677889900",
        "created_ms": 1_700_000_000_000_i64,
        "source_os": "linux",
        "preview": "QA-ALPHA-123",
        "search_text": "QA-ALPHA-123",
        "formats": formats,
        "rev": { "wall_ms": 1, "dev": "test-dev" },
    }))
    .expect("test clip JSON must deserialize")
}

#[test]
fn charset_text_format_is_a_text_clip() {
    let clip = clip_with_formats(&["text/plain;charset=utf-8"]);
    assert_eq!(popup::preview_kind(&clip), "popup.kind_text");
}

#[test]
fn plain_text_format_is_a_text_clip() {
    let clip = clip_with_formats(&["text/plain"]);
    assert_eq!(popup::preview_kind(&clip), "popup.kind_text");
}
