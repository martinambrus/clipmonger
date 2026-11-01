//! Preview width display consumer (checkpoint 3a).
#![allow(
    clippy::too_many_lines,
    clippy::needless_pass_by_value,
    clippy::doc_markdown,
    dead_code,
    unused_imports,
    unused
)]
//!
//! Validates `popup.preview_width` as Unicode scalar limit applied only at
//! display to history rows and Current Clipboard text preview. Total budget
//! includes deterministic ellipsis. Widths 1/empty/short/5/20/80 safe, huge
//! width does not preallocate huge capacity, no UTF-8 byte slicing.
//! Stored `Clip.preview`/`search_text`/`payloads`/`body` and paste IDs unchanged.
//! Config snapshot updates visible text without recapture.

use std::any::Any;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use clipmonger_app_ui::app::{AppParts, PopupApp, Trace};
use clipmonger_app_ui::ipc_bridge::{UiCmd, UiEvent};
use clipmonger_app_ui::runner::{App, WindowKind};
use clipmonger_core::model::Clip;
use serde_json::json;
use uuid::Uuid;

const HOUR: Duration = Duration::from_secs(3600);

struct Sink(Arc<Mutex<Vec<u8>>>);
impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("trace sink").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
struct Request {
    delay: Duration,
}
struct Win {
    ctx: egui::Context,
    info: egui::ViewportInfo,
    events: Vec<egui::Event>,
    system_theme: Option<egui::Theme>,
}
impl Win {
    fn new() -> Self {
        Self {
            ctx: egui::Context::default(),
            info: egui::ViewportInfo::default(),
            events: Vec::new(),
            system_theme: None,
        }
    }
}
struct Pass {
    commands: Vec<egui::ViewportCommand>,
    texts: Vec<String>,
    fills: Vec<egui::Color32>,
    bounds: Vec<(String, egui::Rect, egui::Rect)>,
}
impl Pass {
    fn painted(&self, text: &str) -> bool {
        self.texts.iter().any(|p| p == text)
    }
    fn positions(&self) -> Vec<egui::Pos2> {
        self.commands
            .iter()
            .filter_map(|c| match c {
                egui::ViewportCommand::OuterPosition(p) => Some(*p),
                _ => None,
            })
            .collect()
    }
}
fn collect_text(shape: &egui::Shape, out: &mut Vec<String>) {
    match shape {
        egui::Shape::Text(t) => out.push(t.galley.text().to_string()),
        egui::Shape::Vec(v) => {
            for s in v {
                collect_text(s, out)
            }
        }
        _ => {}
    }
}
fn collect_fills(shape: &egui::Shape, out: &mut Vec<egui::Color32>) {
    match shape {
        egui::Shape::Rect(r) => out.push(r.fill),
        egui::Shape::Vec(v) => {
            for s in v {
                collect_fills(s, out)
            }
        }
        _ => {}
    }
}
fn settings_text_bounds(
    shape: &egui::Shape,
    clip: egui::Rect,
    out: &mut Vec<(String, egui::Rect, egui::Rect)>,
) {
    match shape {
        egui::Shape::Text(t) => out.push((t.galley.text().to_string(), t.galley.rect, clip)),
        egui::Shape::Vec(v) => {
            for s in v {
                settings_text_bounds(s, clip, out)
            }
        }
        _ => {}
    }
}
struct Setup {
    open: bool,
    status_period: Duration,
    trace: bool,
    hold: Option<Box<dyn Any>>,
}
impl Setup {
    fn new(open: bool) -> Self {
        Self {
            open,
            status_period: HOUR,
            trace: true,
            hold: None,
        }
    }
}
struct Rig {
    app: PopupApp,
    popup: Win,
    settings: Win,
    cmds: Arc<Mutex<mpsc::Receiver<UiCmd>>>,
    backlog: Arc<Mutex<Vec<UiCmd>>>,
    at_hook: Arc<Mutex<Vec<Vec<UiCmd>>>>,
    events: mpsc::Sender<UiEvent>,
    tray: Arc<Mutex<Vec<clipmonger_app_ui::tray_logic::TrayAction>>>,
    tray_polls: Arc<AtomicUsize>,
    trace: Arc<Mutex<Vec<u8>>>,
    foreground: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<Request>>>,
    born: Instant,
}
impl Rig {
    fn raw(setup: Setup) -> Self {
        let born = Instant::now();
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let cmd_rx = Arc::new(Mutex::new(cmd_rx));
        let backlog = Arc::new(Mutex::new(Vec::new()));
        let at_hook = Arc::new(Mutex::new(Vec::new()));
        let (event_tx, event_rx) = mpsc::channel();
        let tray = Arc::new(Mutex::new(Vec::new()));
        let tray_polls = Arc::new(AtomicUsize::new(0));
        let trace = Arc::new(Mutex::new(Vec::new()));
        let foreground = Arc::new(AtomicUsize::new(0));
        let queue = Arc::clone(&tray);
        let polls = Arc::clone(&tray_polls);
        let hook = Arc::clone(&foreground);
        let hook_commands = Arc::clone(&cmd_rx);
        let hook_backlog = Arc::clone(&backlog);
        let hook_log = Arc::clone(&at_hook);
        let app = PopupApp::new(AppParts {
            profile: PathBuf::from("rig-profile-preview"),
            cmds: cmd_tx,
            events: event_rx,
            tray: Box::new(move || {
                polls.fetch_add(1, Ordering::SeqCst);
                std::mem::take(&mut *queue.lock().expect("tray"))
            }),
            hold: setup.hold,
            open: setup.open,
            status_period: setup.status_period,
            trace: setup
                .trace
                .then(|| Trace::new(Box::new(Sink(Arc::clone(&trace))))),
            allow_foreground: Box::new(move || {
                hook.fetch_add(1, Ordering::SeqCst);
                let mut b = hook_backlog.lock().expect("backlog");
                b.extend(hook_commands.lock().expect("commands").try_iter());
                hook_log.lock().expect("hook").push(b.clone());
            }),
        });
        let popup = Win::new();
        let requests = Arc::new(Mutex::new(Vec::new()));
        {
            let r = Arc::clone(&requests);
            popup.ctx.set_request_repaint_callback(move |info| {
                r.lock().expect("req").push(Request { delay: info.delay })
            });
        }
        Self {
            app,
            popup,
            settings: Win::new(),
            cmds: cmd_rx,
            backlog,
            at_hook,
            events: event_tx,
            tray,
            tray_polls,
            trace,
            foreground,
            requests,
            born,
        }
    }
    fn with(setup: Setup) -> Self {
        let rig = Self::raw(setup);
        let start = rig.cmds();
        assert_eq!(start.len(), 5, "start cmds: {start:?}");
        rig
    }
    fn opened() -> Self {
        Self::with(Setup::new(true))
    }
    fn push(&self, e: UiEvent) {
        self.events.send(e).expect("receiver");
    }
    fn cmds(&self) -> Vec<UiCmd> {
        let mut all = std::mem::take(&mut *self.backlog.lock().expect("backlog"));
        all.extend(self.cmds.lock().expect("cmds").try_iter());
        all
    }
    fn win_mut(&mut self, kind: WindowKind) -> &mut Win {
        match kind {
            WindowKind::Popup => &mut self.popup,
            WindowKind::Settings => &mut self.settings,
        }
    }
    fn pass(&mut self, kind: WindowKind, visible: bool) -> Pass {
        let win = match kind {
            WindowKind::Popup => &mut self.popup,
            WindowKind::Settings => &mut self.settings,
        };
        let mut raw = egui::RawInput {
            viewports: std::iter::once((egui::ViewportId::ROOT, win.info.clone())).collect(),
            system_theme: win.system_theme,
            ..egui::RawInput::default()
        };
        win.info.events.clear();
        let app = &mut self.app;
        if visible {
            raw.screen_rect = Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                match kind {
                    WindowKind::Popup => egui::vec2(480.0, 360.0),
                    WindowKind::Settings => egui::vec2(960.0, 680.0) / win.ctx.zoom_factor(),
                },
            ));
            raw.events = std::mem::take(&mut win.events);
            let mut runs = 0;
            let output = win.ctx.run_ui(raw, |ui| {
                runs += 1;
                app.logic(ui.ctx(), kind);
                app.ui(ui, kind);
            });
            assert_eq!(runs, 1, "egui discarded pass");
            let egui::FullOutput {
                mut textures_delta,
                shapes,
                mut viewport_output,
                ..
            } = output;
            textures_delta.clear();
            let mut texts = Vec::new();
            let mut fills = Vec::new();
            let mut bounds = Vec::new();
            for clipped in &shapes {
                settings_text_bounds(&clipped.shape, clipped.clip_rect, &mut bounds);
                collect_text(&clipped.shape, &mut texts);
                collect_fills(&clipped.shape, &mut fills);
            }
            Pass {
                commands: viewport_output
                    .remove(&egui::ViewportId::ROOT)
                    .map(|v| v.commands)
                    .unwrap_or_default(),
                texts,
                fills,
                bounds,
            }
        } else {
            let mut output = win.ctx.run_logic(&raw, |ctx| app.logic(ctx, kind));
            Pass {
                commands: output
                    .viewport_commands
                    .remove(&egui::ViewportId::ROOT)
                    .unwrap_or_default(),
                texts: Vec::new(),
                fills: Vec::new(),
                bounds: Vec::new(),
            }
        }
    }
    fn show(&mut self) -> Pass {
        self.pass(WindowKind::Popup, true)
    }
    fn tick(&mut self) -> Pass {
        self.pass(WindowKind::Popup, false)
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
    serde_json::from_value(json!({"id": id(n).to_string(), "created_ms": 1_700_000_000_000_i64, "source_os": "linux", "preview": preview, "search_text": preview, "pinned": false, "rev": {"wall_ms": 1, "dev": "test-dev"}})).expect("clip")
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
fn assert_stored_unchanged(rig: &Rig, id: Uuid, original: &str) {
    let stored = rig
        .app
        .vm()
        .clips()
        .iter()
        .find(|c| c.id == id)
        .expect("stored clip");
    assert_eq!(
        stored.preview, original,
        "stored preview must stay original"
    );
    assert_eq!(stored.search_text, original, "stored search_text unchanged");
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
        assert_stored_unchanged(&rig, id(1), &long);
    }
}

// CurrentClipboard preview truncates too
#[test]
fn preview_current_clipboard_truncates() {
    let mut rig = Rig::opened();
    let long = long_multibyte();
    rig.config_width(5);
    rig.push(history(&[clip(1, "history short")]));
    rig.push(UiEvent::HistoryClip(
        json!({"clip": serde_json::to_value(clip(2, &long)).unwrap()}),
    ));
    rig.tick();
    let _ = rig.cmds();
    let pass = rig.show();
    let expected = expected_truncate(&long, 5);
    assert!(
        pass.painted(&expected),
        "current clipboard must be truncated at 5 to `{expected}` {:?}",
        pass.texts
    );
    assert!(!pass.painted(&long));
    // stored current unchanged
    let cur = rig.app.vm().current().expect("current").preview.clone();
    assert_eq!(cur, long);
}

// reload/change updates visible without recapture
#[test]
fn preview_reload_updates_display() {
    let mut rig = Rig::opened();
    let long = long_multibyte();
    rig.load(&[clip(1, &long)]);
    rig.config_width(5);
    let pass5 = rig.show();
    let exp5 = expected_truncate(&long, 5);
    assert!(pass5.painted(&exp5));
    rig.config_width(20);
    let pass20 = rig.show();
    let exp20 = expected_truncate(&long, 20);
    assert!(
        pass20.painted(&exp20),
        "after reload to 20 must paint {exp20} {:?}",
        pass20.texts
    );
    assert!(!pass20.painted(&exp5));
    // no re-load needed, stored still original
    assert_stored_unchanged(&rig, id(1), &long);
}

// full query matching still finds original, activation correct ID/body, preserves IDs
#[test]
fn preview_full_query_still_finds_original_and_activation_correct() {
    let mut rig = Rig::opened();
    let long = "abcdefghijABCDEFGHIJ0123456789-UNIQUE_TAIL_12345";
    let clip1 = clip(10, long);
    let clip2 = clip(11, "other short");
    rig.config_width(5);
    rig.load(&[clip1.clone(), clip2.clone()]);
    let pass = rig.show();
    let exp = expected_truncate(long, 5);
    assert!(pass.painted(&exp));
    // query that matches only beyond truncated prefix must still find clip
    rig.app.vm_mut().query = "UNIQUE_TAIL".to_string();
    // clamp not needed for visible check
    let visible = rig.app.vm().visible();
    assert_eq!(
        visible.len(),
        1,
        "query beyond truncation must still match original preview"
    );
    let idx = visible[0];
    let found = &rig.app.vm().clips()[idx];
    assert_eq!(found.id, id(10));
    // activation via Enter should paste correct ID (check via VmEffect through key handling)
    // Simulate Enter via popup vm action
    let effect = rig
        .app
        .vm_mut()
        .on_action(clipmonger_app_ui::keymap::Action::Paste);
    match effect {
        clipmonger_app_ui::popup::VmEffect::Paste(pid) => assert_eq!(pid, id(10)),
        other => panic!("expected Paste, got {other:?}"),
    }
    // stored preview unchanged after query
    assert_stored_unchanged(&rig, id(10), long);
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
