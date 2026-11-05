//! Popup preview width changes painted text while preserving clip data and picks.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clipmonger_app_ui::app::{AppParts, PopupApp};
use clipmonger_app_ui::ipc_bridge::{method_for, params_for, UiCmd, UiEvent};
use clipmonger_app_ui::runner::{App, WindowKind};
use clipmonger_core::model::Clip;
use clipmonger_ipc::proto::methods::ShowPopupEvent;
use serde_json::json;
use uuid::Uuid;

struct Pass {
    texts: Vec<String>,
    rects: Vec<(String, egui::Rect)>,
}
impl Pass {
    fn painted(&self, text: &str) -> bool {
        self.texts.iter().any(|painted| painted == text)
    }
}

fn collect_text(shape: &egui::Shape, out: &mut Vec<(String, egui::Rect)>) {
    match shape {
        egui::Shape::Text(text) => out.push((
            text.galley.text().to_string(),
            text.galley.rect.translate(text.pos.to_vec2()),
        )),
        egui::Shape::Vec(shapes) => {
            for shape in shapes {
                collect_text(shape, out);
            }
        }
        _ => {}
    }
}

struct Rig {
    app: PopupApp,
    ctx: egui::Context,
    input: Vec<egui::Event>,
    cmds: mpsc::Receiver<UiCmd>,
    events: mpsc::Sender<UiEvent>,
}
impl Rig {
    fn opened() -> Self {
        let rig = Self::opened_with_pending();
        let _ = rig.cmds();
        rig
    }
    fn opened_with_pending() -> Self {
        let (cmd_tx, cmds) = mpsc::channel();
        let (events, event_rx) = mpsc::channel();
        let app = PopupApp::new(AppParts {
            profile: PathBuf::from("rig-profile-preview"),
            cmds: cmd_tx,
            events: event_rx,
            tray: Box::new(|_| Vec::new()),
            hold: None,
            open: true,
            status_period: Duration::from_secs(3600),
            trace: None,
            allow_foreground: Box::new(|| {}),
        });
        Self {
            app,
            ctx: egui::Context::default(),
            input: Vec::new(),
            cmds,
            events,
        }
    }
    fn push(&self, event: UiEvent) {
        self.events.send(event).expect("receiver");
    }
    fn cmds(&self) -> Vec<UiCmd> {
        self.cmds.try_iter().collect()
    }
    fn show(&mut self) -> Pass {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(480.0, 360.0),
            )),
            events: std::mem::take(&mut self.input),
            ..egui::RawInput::default()
        };
        let app = &mut self.app;
        let mut runs = 0;
        let mut output = self.ctx.run_ui(raw, |ui| {
            runs += 1;
            app.logic(ui.ctx(), WindowKind::Popup);
            app.ui(ui, WindowKind::Popup);
        });
        output.textures_delta.clear(); // No renderer consumes textures in this headless rig.
        assert_eq!(runs, 1, "egui discarded pass");
        let mut rects = Vec::new();
        for clipped in &output.shapes {
            collect_text(&clipped.shape, &mut rects);
        }
        let texts = rects.iter().map(|(text, _)| text.clone()).collect();
        Pass { texts, rects }
    }
    fn tick(&mut self) {
        let app = &mut self.app;
        let _ = self.ctx.run_logic(&egui::RawInput::default(), |ctx| {
            app.logic(ctx, WindowKind::Popup);
        });
    }
    fn load(&mut self, clips: &[Clip]) {
        self.push(history(clips));
        self.tick();
        let _ = self.cmds();
    }
    fn config_width(&mut self, width: u32) {
        self.push(UiEvent::Config(
            json!({"config": {"popup": {"preview_width": width}}}),
        ));
        self.tick();
        let _ = self.cmds();
    }
}

fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}
fn clip(n: u128, preview: &str) -> Clip {
    serde_json::from_value(json!({
        "id": id(n).to_string(),
        "created_ms": 1_700_000_000_000_i64,
        "source_os": "linux",
        "preview": preview,
        "search_text": format!("{preview} SEARCH_ONLY_{n}"),
        "source_app": "preview fixture",
        "formats": ["text/plain", "text/html"],
        "bytes_total": 4096,
        "content_hash": "fixture-digest",
        "origin_dev": "fixture-device",
        "origin": "x11",
        "last_used_ms": 1_700_000_000_001_i64,
        "pinned": false,
        "rev": {"wall_ms": 1, "dev": "test-dev"},
    }))
    .expect("clip")
}
fn history(clips: &[Clip]) -> UiEvent {
    UiEvent::History(json!({"clips": clips}))
}
fn expected_truncate(preview: &str, width: u32) -> String {
    let effective = if width == 0 { 1 } else { width as usize };
    let count = preview.chars().count();
    if count <= effective {
        return preview.to_string();
    }
    if effective == 1 {
        return "…".to_string();
    }
    let mut out: String = preview.chars().take(effective - 1).collect();
    out.push('…');
    out
}
fn long_multibyte() -> String {
    // 50+ scalars, multibyte crab, ensures truncation needs scalar counting not bytes
    let mut s = String::new();
    for i in 0..40 {
        let _ = std::fmt::Write::write_fmt(&mut s, format_args!("a{i}🦀"));
    }
    s
}
fn assert_stored_unchanged(rig: &Rig, original: &Clip) {
    let stored = rig
        .app
        .vm()
        .clips()
        .iter()
        .find(|clip| clip.id == original.id)
        .expect("stored clip");
    assert_eq!(stored, original, "complete stored clip must stay unchanged");
}

// popup.preview_lines keeps up to N lines; a multi-line row does not overlap the next
#[test]
fn preview_lines_keep_line_breaks_without_overlap() {
    let mut rig = Rig::opened();
    let multi = clip(1, "L1\nL2\nL3\nL4");
    let next = clip(2, "next row");
    rig.load(&[multi.clone(), next]);
    rig.push(UiEvent::Config(
        json!({"config": {"popup": {"preview_width": 80, "preview_lines": 2}}}),
    ));
    rig.tick();
    let _ = rig.cmds();
    let pass = rig.show();
    assert!(pass.painted("L1\nL2…"), "texts {:?}", pass.texts);
    let rect = |text: &str| {
        pass.rects
            .iter()
            .find(|(painted, _)| painted == text)
            .map_or_else(
                || panic!("`{text}` not painted: {:?}", pass.texts),
                |(_, rect)| *rect,
            )
    };
    let first = rect("L1\nL2…");
    let second = rect("next row");
    assert!(
        first.bottom() <= second.top(),
        "rows overlap: {first:?} vs {second:?}"
    );
    assert_stored_unchanged(&rig, &multi);
}

// history rows at 5/20/80 show truncated multibyte, not verbatim long
#[test]
fn preview_history_truncates_at_5_20_80() {
    let mut rig = Rig::opened();
    let long = long_multibyte();
    let clip1 = clip(1, &long);
    rig.load(std::slice::from_ref(&clip1));
    for width in [5u32, 20, 80] {
        rig.config_width(width);
        let pass = rig.show();
        let expected = expected_truncate(&long, width);
        assert!(
            pass.painted(&expected),
            "width {width} must paint truncated `{expected}` texts {:?} preview {}",
            pass.texts,
            long
        );
        if long.chars().count() > width as usize {
            assert!(
                !pass.painted(&long),
                "long verbatim must not be painted at width {width}"
            );
        } else {
            assert!(pass.painted(&long));
        }
        assert_stored_unchanged(&rig, &clip1);
    }
}

// The current clipboard is no longer painted above the history list.
#[test]
fn preview_current_clipboard_is_not_painted() {
    let mut rig = Rig::opened();
    let current = clip(2, &long_multibyte());
    let history_clip = clip(1, "history short");
    rig.load(std::slice::from_ref(&history_clip));
    rig.push(UiEvent::HistoryClip(json!({"clip": current})));
    rig.tick();
    for width in [5, 20, 80] {
        rig.config_width(width);
        let pass = rig.show();
        let expected = expected_truncate(&current.preview, width);
        assert!(!pass.painted(&expected), "width {width}: {:?}", pass.texts);
        assert!(!pass.painted(&current.preview));
        assert!(pass.painted(&expected_truncate("history short", width)));
        assert_eq!(rig.app.vm().current(), Some(&current));
        assert_stored_unchanged(&rig, &history_clip);
    }
}

// reload/change updates visible without recapture
#[test]
fn preview_reload_updates_display() {
    let mut rig = Rig::opened();
    let original = clip(1, &long_multibyte());
    rig.load(std::slice::from_ref(&original));
    // Each reply is a fresh Config snapshot, as received after change/reload.
    // No new history/current reply or clipboard capture occurs in this loop.
    for width in [5, 20, 80, 5] {
        rig.config_width(width);
        let pass = rig.show();
        let expected = expected_truncate(&original.preview, width);
        assert_eq!(
            pass.texts.iter().filter(|text| **text == expected).count(),
            1,
            "history must reflect snapshot width {width}"
        );
        assert!(!pass.painted(&original.preview));
        assert_stored_unchanged(&rig, &original);
        assert_eq!(rig.app.vm().current(), Some(&original));
    }
}

// Search and Enter use the actual popup input and emitted IPC command boundary.
#[test]
fn preview_full_query_still_finds_original_and_activation_correct() {
    let original = clip(10, &format!("{}-UNIQUE_TAIL_12345", long_multibyte()));
    let decoy = clip(11, "other short");
    for width in [5, 20, 80] {
        for query in ["UNIQUE_TAIL_12345", "SEARCH_ONLY_10"] {
            assert!(!expected_truncate(&original.preview, width).contains(query));
            let mut rig = Rig::opened_with_pending();
            rig.push(UiEvent::ShowPopup(ShowPopupEvent {
                source: "hotkey".to_string(),
                target: Some(77),
                gesture: Some(99),
                ..ShowPopupEvent::default()
            }));
            rig.tick();
            rig.push(UiEvent::Config(
                json!({"config": {"popup": {"preview_width": width}}}),
            ));
            rig.tick();
            let store = compact_store(&decoy, &original);
            let original_stored = store.get_clip(original.id).unwrap().unwrap();
            let decoy_stored = store.get_clip(decoy.id).unwrap().unwrap();
            for (input, stored) in [(&original, &original_stored.0), (&decoy, &decoy_stored.0)] {
                assert_eq!(stored.preview, input.preview);
                assert_eq!(stored.search_text, input.search_text);
                assert_eq!(stored.formats, input.formats);
            }
            serve_compact(&mut rig, &store, decoy.id);
            rig.show(); // Focus the search field before delivering text.
            rig.input.push(egui::Event::Text(query.to_string()));
            rig.show(); // TextEdit updates the query after this frame's rows.
            serve_compact(&mut rig, &store, decoy.id);
            let filtered = rig.show();
            assert_eq!(rig.app.vm().query, query);
            let rows = rig.app.vm().compact_rows().expect("compact history");
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].id, original.id);
            assert_eq!(rows[0].preview, original.preview);
            assert!(filtered.painted(&expected_truncate(&original.preview, width)));
            assert_eq!(
                filtered
                    .texts
                    .iter()
                    .filter(|text| **text == expected_truncate(&decoy.preview, width))
                    .count(),
                0,
                "the decoy is never painted in filtered rows"
            );
            assert_eq!(rig.app.vm().current(), Some(&decoy_stored.0));
            assert_eq!(
                store.get_clip(original.id).unwrap().unwrap(),
                original_stored
            );
            assert_eq!(store.get_clip(decoy.id).unwrap().unwrap(), decoy_stored);
            assert!(rig.cmds().is_empty(), "typing must not paste");

            rig.input.push(egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: Some(egui::Key::Enter),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
            let before = now_ms();
            rig.show();
            let after = now_ms();
            let commands = rig.cmds();
            let [command] = commands.as_slice() else {
                panic!("expected one paste command: {commands:?}");
            };
            let UiCmd::PasteClipTo(picked, target, picked_ms, gesture) = command else {
                panic!("expected PasteClipTo: {command:?}");
            };
            assert_eq!(
                (*picked, *target, *gesture),
                (original.id, Some(77), Some(99))
            );
            assert!((before..=after).contains(picked_ms));
            assert_eq!(method_for(command), "paste.execute");
            assert_eq!(
                params_for(command),
                json!({
                    "clip": original.id.to_string(),
                    "target_hwnd": 77,
                    "gesture": 99,
                    "picked_ms": picked_ms,
                })
            );
            assert_eq!(
                store.get_clip(original.id).unwrap().unwrap(),
                original_stored
            );
            assert_eq!(store.get_clip(decoy.id).unwrap().unwrap(), decoy_stored);
            rig.tick();
            assert!(rig.cmds().is_empty(), "Enter must emit the paste only once");
        }
    }
}

fn compact_store(decoy: &Clip, original: &Clip) -> clipmonger_core::Store {
    let profile = std::env::temp_dir().join(format!("preview-compact-{}", Uuid::now_v7()));
    std::fs::create_dir(&profile).unwrap();
    let store = clipmonger_core::Store::open(&profile).unwrap();
    for clip in [decoy, original] {
        store
            .insert_clip(
                clip,
                &[
                    (
                        "text/plain".to_owned(),
                        clip.search_text.as_bytes().to_vec(),
                    ),
                    (
                        "text/html".to_owned(),
                        format!("<p>{}</p>", clip.preview).into_bytes(),
                    ),
                ],
            )
            .unwrap();
    }
    store
        .prepare_compact_history_index(
            Duration::from_secs(5),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .unwrap();
    store
}

// Serve the commands actually emitted by the popup from a real compact scan.
fn serve_compact(rig: &mut Rig, store: &clipmonger_core::Store, current: Uuid) {
    for _ in 0..5 {
        let commands = rig.cmds();
        if commands.is_empty() {
            return;
        }
        for cmd in commands {
            match cmd {
                UiCmd::CompactHistory(tag) => {
                    assert!(tag.params.cursor.is_none(), "fixture fits in one scan step");
                    let owner = tag.params.owner;
                    let token = store.begin_compact_scan(owner, &tag.params.query).unwrap();
                    let chunk = store
                        .step_compact_scan(owner, token, 65536, 32, 50)
                        .unwrap();
                    assert!(chunk.done, "fixture fits in one scan step");
                    rig.push(UiEvent::CompactHistory(
                        tag.clone(),
                        Ok(json!({
                            "owner": owner, "generation": tag.params.generation,
                            "rows": chunk.rows, "cursor": null, "done": chunk.done,
                            "bytes_read": chunk.bytes_read, "candidates": chunk.candidates
                        })),
                    ));
                }
                UiCmd::CompactCurrent(tag) => {
                    let clip = store.get_clip(current).unwrap().unwrap().0;
                    rig.push(UiEvent::CompactCurrent(tag, Ok(json!({"clips": [clip]}))));
                }
                UiCmd::PasteClipTo(..) => panic!("typing must not paste"),
                _ => {}
            }
        }
        rig.tick();
    }
    panic!("compact fixture did not settle");
}

fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_millis(),
    )
    .expect("timestamp fits")
}

// width 1, empty, short, large preserved
#[test]
fn preview_width_edge_cases() {
    let mut rig = Rig::opened();
    // empty preview
    let empty = clip(1, "");
    rig.config_width(5);
    rig.load(&[empty]);
    let pass = rig.show();
    // empty should not panic and not produce ellipsis
    assert!(
        !pass.painted("…"),
        "empty preview must not become ellipsis at 5"
    );

    // short preview shorter than limit: preserved verbatim
    let short = "hi";
    let c2 = clip(2, short);
    rig.load(&[c2]);
    rig.config_width(80);
    let pass2 = rig.show();
    assert!(pass2.painted(short), "short preview must be verbatim at 80");

    // width 1: truncation yields single ellipsis
    let long = "abcdef";
    rig.load(&[clip(3, long)]);
    rig.config_width(1);
    let pass3 = rig.show();
    assert!(pass3.painted("…"), "width 1 long must paint ellipsis");
    assert!(!pass3.painted(long));
    assert!(!pass3.painted("a"));

    // large width preserves original
    rig.config_width(200);
    let pass4 = rig.show();
    assert!(pass4.painted(long), "large width must preserve original");

    // huge width does not preallocate huge capacity and preserves short
    rig.config_width(1_000_000);
    let pass5 = rig.show();
    assert!(pass5.painted(long), "huge width short preview preserved");

    // zero width safe (invalid but reachable): treated as 1
    rig.config_width(0);
    let pass6 = rig.show();
    assert!(pass6.painted("…"), "zero width invalid must be safe as 1");
}

// multibyte does not slice UTF-8 bytes and ellipsis included in budget
#[test]
fn preview_multibyte_counts_scalars() {
    let mut rig = Rig::opened();
    let preview = "🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀"; // 10 scalars, each 4 bytes
    rig.load(&[clip(1, preview)]);
    rig.config_width(5);
    let pass = rig.show();
    let expected = expected_truncate(preview, 5); // 4 crabs + ellipsis = 5
    assert_eq!(expected.chars().count(), 5);
    assert!(pass.painted(&expected));
    assert_eq!(expected, "🦀🦀🦀🦀…");
    // ensure no byte slicing panic (would have panicked already)
}
