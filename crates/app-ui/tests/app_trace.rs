//! The UI trace (`clipmonger_app_ui::app::trace`): the JSON line of each event, the
//! words that say why the popup opened or hid, the `Trace` sink and its environment
//! switch, and the lines `PopupApp` writes for the transitions of its shell.
//!
//! A trace line carries only booleans and fixed words, never clip contents, window
//! titles or token values: the tests check that too. Lines are compared as parsed
//! JSON, without `t_ms`; key order inside a line is not part of the contract.
//!
//! Run: `cargo test -p clipmonger-app-ui --test app_trace`

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clipmonger_app_ui::app::trace::{hide_reason, line, open_source, TRACE_ENV};
use clipmonger_app_ui::app::{AppParts, PopupApp, Trace, TraceEvent};
use clipmonger_app_ui::ipc_bridge::{UiCmd, UiEvent};
use clipmonger_app_ui::runner::{App, WindowKind};
use clipmonger_app_ui::shell::Event;
use clipmonger_app_ui::tray_logic::TrayAction;
use clipmonger_core::model::Clip;
use clipmonger_ipc::proto::methods::ShowPopupEvent;
use serde_json::{json, Value};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|error| panic!("not JSON ({error}): {text:?}"))
}

fn unix_ms() -> u64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after 1970");
    u64::try_from(elapsed.as_millis()).expect("fits in u64")
}

/// A sink the test can read and that counts flushes.
#[derive(Default)]
struct SinkState {
    bytes: Vec<u8>,
    flushes: usize,
}

struct Sink(Arc<Mutex<SinkState>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("sink").bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.lock().expect("sink").flushes += 1;
        Ok(())
    }
}

/// A sink that refuses everything.
struct BrokenSink(Arc<AtomicUsize>);

impl Write for BrokenSink {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(std::io::Error::other("write refused"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(std::io::Error::other("flush refused"))
    }
}

/// The lines of a byte buffer as parsed JSON objects, with `t_ms` taken out.
fn lines_of(bytes: &[u8]) -> Vec<Value> {
    let text = std::str::from_utf8(bytes).expect("UTF-8");
    assert!(
        text.is_empty() || text.ends_with('\n'),
        "every line ends with a newline: {text:?}"
    );
    text.lines()
        .map(|text| {
            let mut value = parse(text);
            let object = value.as_object_mut().expect("a line is an object");
            assert!(
                object.remove("t_ms").is_some_and(|stamp| stamp.is_u64()),
                "numeric t_ms in {text}"
            );
            value
        })
        .collect()
}

fn open_line(source: &str, target: bool, gesture: bool) -> Value {
    json!({"ev": "open", "source": source, "target": target, "gesture": gesture})
}

fn hide_line(reason: &str) -> Value {
    json!({"ev": "hide", "reason": reason})
}

fn paste_line(target: bool) -> Value {
    json!({"ev": "paste", "target": target})
}

fn error_line() -> Value {
    json!({"ev": "error"})
}

fn show_popup(source: &str, toggle: bool) -> Event {
    Event::Bridge(UiEvent::ShowPopup(ShowPopupEvent {
        source: source.to_string(),
        toggle,
        target: Some(1),
        gesture: Some(2),
        ..ShowPopupEvent::default()
    }))
}

// ---------------------------------------------------------------------------
// The JSON line
// ---------------------------------------------------------------------------

// line: one JSON object per event with t_ms, ev and the fields of that event
#[test]
fn rule_line_has_the_documented_fields() {
    let mut cases: Vec<(TraceEvent, Value)> = Vec::new();
    for source in [
        "hotkey",
        "request",
        "tray",
        "paste_failed",
        "start",
        "other",
    ] {
        for (target, gesture) in [(false, false), (true, false), (false, true), (true, true)] {
            cases.push((
                TraceEvent::Open {
                    source,
                    target,
                    gesture,
                },
                json!({"ev": "open", "source": source, "target": target, "gesture": gesture}),
            ));
        }
    }
    for reason in [
        "dismiss",
        "focus_lost",
        "close_request",
        "toggle",
        "tray",
        "pick",
        "other",
    ] {
        cases.push((
            TraceEvent::Hide { reason },
            json!({"ev": "hide", "reason": reason}),
        ));
    }
    for target in [false, true] {
        cases.push((
            TraceEvent::Paste { target },
            json!({"ev": "paste", "target": target}),
        ));
    }
    cases.push((TraceEvent::Error, json!({"ev": "error"})));
    for (event, mut expected) in cases {
        for t_ms in [0_u64, 1_700_000_000_123, u64::MAX] {
            let text = line(&event, t_ms);
            assert!(!text.contains('\n'), "no newline in {text:?}");
            expected["t_ms"] = json!(t_ms);
            assert_eq!(parse(&text), expected, "{event:?} at {t_ms}");
        }
    }
}

// line: the object has exactly the keys of its event
#[test]
fn rule_line_has_no_other_keys() {
    let keys = |event: &TraceEvent| -> Vec<String> {
        let mut keys: Vec<String> = parse(&line(event, 5))
            .as_object()
            .expect("an object")
            .keys()
            .cloned()
            .collect();
        keys.sort();
        keys
    };
    assert_eq!(
        keys(&TraceEvent::Open {
            source: "hotkey",
            target: true,
            gesture: true
        }),
        ["ev", "gesture", "source", "t_ms", "target"]
    );
    assert_eq!(
        keys(&TraceEvent::Hide { reason: "pick" }),
        ["ev", "reason", "t_ms"]
    );
    assert_eq!(
        keys(&TraceEvent::Paste { target: true }),
        ["ev", "t_ms", "target"]
    );
    assert_eq!(keys(&TraceEvent::Error), ["ev", "t_ms"]);
}

// ---------------------------------------------------------------------------
// Why the popup opened or hid
// ---------------------------------------------------------------------------

// open_source: hotkey for a hotkey push, request for any other push source, tray for
// TrayToggle, paste_failed for a failed paste, other for anything else
#[test]
fn rule_open_source_words() {
    let mut table: Vec<(Event, &str)> = vec![
        (show_popup("hotkey", false), "hotkey"),
        (show_popup("hotkey", true), "hotkey"),
        (show_popup("request", false), "request"),
        (show_popup("request", true), "request"),
        (show_popup("cli", false), "request"),
        (show_popup("", false), "request"),
        (show_popup("Hotkey", false), "request"),
        (Event::TrayToggle, "tray"),
        (
            Event::Bridge(UiEvent::PasteFailed("gone".to_string())),
            "paste_failed",
        ),
    ];
    let others = [
        Event::Dismiss,
        Event::FocusLost,
        Event::CloseRequest,
        Event::Pick {
            id: Uuid::from_u128(1),
            picked_ms: 5,
        },
        Event::PickPermanent {
            id: Uuid::from_u128(1),
        },
        Event::Hidden,
        Event::Key,
        Event::Refresh,
        Event::SettingsOpen,
        Event::SettingsClose,
        Event::Quit,
        Event::Bridge(UiEvent::Error("x".to_string())),
        Event::Bridge(UiEvent::PasteUnavailable),
        Event::Bridge(UiEvent::Offline("x".to_string())),
        Event::Bridge(UiEvent::Status(json!({}))),
        Event::Bridge(UiEvent::History(json!({}))),
        Event::Bridge(UiEvent::Config(json!({}))),
        Event::Bridge(UiEvent::Stopped),
        Event::Bridge(UiEvent::AnotherInstance),
    ];
    table.extend(others.into_iter().map(|event| (event, "other")));
    for (event, word) in table {
        assert_eq!(open_source(&event), word, "{event:?}");
    }
}

// hide_reason: dismiss, focus_lost, close_request, toggle (a daemon push), tray, pick
// (Pick and PickPermanent), other for anything else
#[test]
fn rule_hide_reason_words() {
    let mut table: Vec<(Event, &str)> = vec![
        (Event::Dismiss, "dismiss"),
        (Event::FocusLost, "focus_lost"),
        (Event::CloseRequest, "close_request"),
        (show_popup("hotkey", true), "toggle"),
        (show_popup("hotkey", false), "toggle"),
        (show_popup("request", false), "toggle"),
        (Event::TrayToggle, "tray"),
        (
            Event::Pick {
                id: Uuid::from_u128(1),
                picked_ms: 5,
            },
            "pick",
        ),
        (
            Event::PickPermanent {
                id: Uuid::from_u128(2),
            },
            "pick",
        ),
    ];
    let others = [
        Event::Hidden,
        Event::Key,
        Event::Refresh,
        Event::SettingsOpen,
        Event::SettingsClose,
        Event::Quit,
        Event::Bridge(UiEvent::PasteFailed("x".to_string())),
        Event::Bridge(UiEvent::Error("x".to_string())),
        Event::Bridge(UiEvent::Status(json!({}))),
        Event::Bridge(UiEvent::Stopped),
    ];
    table.extend(others.into_iter().map(|event| (event, "other")));
    for (event, word) in table {
        assert_eq!(hide_reason(&event), word, "{event:?}");
    }
}

// ---------------------------------------------------------------------------
// The sink
// ---------------------------------------------------------------------------

// Trace::record: one newline-terminated line per call, stamped with the unix time in ms,
// flushed after each line
#[test]
fn rule_record_writes_stamped_lines_and_flushes() {
    let state = Arc::new(Mutex::new(SinkState::default()));
    let mut trace = Trace::new(Box::new(Sink(Arc::clone(&state))));
    let events = [
        TraceEvent::Open {
            source: "tray",
            target: false,
            gesture: false,
        },
        TraceEvent::Hide { reason: "tray" },
        TraceEvent::Paste { target: false },
        TraceEvent::Error,
    ];
    let mut stamps = Vec::new();
    for (n, event) in events.iter().enumerate() {
        let before = unix_ms();
        trace.record(event);
        let after = unix_ms();
        {
            let state = state.lock().expect("sink");
            assert_eq!(state.flushes, n + 1, "flushed after record {n}");
            let text = std::str::from_utf8(&state.bytes).expect("UTF-8");
            assert_eq!(text.lines().count(), n + 1);
            let last = text.lines().last().expect("a line");
            let value = parse(last);
            let stamp = value["t_ms"].as_u64().expect("numeric t_ms");
            assert!(
                (before..=after).contains(&stamp),
                "t_ms {stamp} in {before}..={after}"
            );
            stamps.push(stamp);
            assert_eq!(
                value["ev"],
                json!(match event {
                    TraceEvent::Open { .. } => "open",
                    TraceEvent::Hide { .. } => "hide",
                    TraceEvent::Paste { .. } => "paste",
                    TraceEvent::Error => "error",
                })
            );
        }
        thread::sleep(Duration::from_millis(2));
    }
    assert!(
        stamps.windows(2).all(|pair| pair[0] <= pair[1]),
        "stamps {stamps:?}"
    );
    let state = state.lock().expect("sink");
    assert!(state.bytes.ends_with(b"\n"));
    assert_eq!(
        lines_of(&state.bytes),
        vec![
            open_line("tray", false, false),
            hide_line("tray"),
            paste_line(false),
            error_line()
        ]
    );
}

// Trace::record: write errors are ignored
#[test]
fn rule_record_ignores_write_errors() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut trace = Trace::new(Box::new(BrokenSink(Arc::clone(&calls))));
    for _ in 0..3 {
        trace.record(&TraceEvent::Error);
    }
    assert!(
        calls.load(Ordering::SeqCst) >= 3,
        "every record tried to write"
    );
}

// ---------------------------------------------------------------------------
// The environment switch
// ---------------------------------------------------------------------------

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Holds the environment lock, sets or clears the trace variable and restores it.
struct TraceEnv {
    _lock: MutexGuard<'static, ()>,
    previous: Option<OsString>,
}

impl TraceEnv {
    fn set(value: Option<&OsStr>) -> Self {
        let lock = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = std::env::var_os(TRACE_ENV);
        match value {
            Some(value) => std::env::set_var(TRACE_ENV, value),
            None => std::env::remove_var(TRACE_ENV),
        }
        Self {
            _lock: lock,
            previous,
        }
    }
}

impl Drop for TraceEnv {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(TRACE_ENV, value),
            None => std::env::remove_var(TRACE_ENV),
        }
    }
}

/// A scratch directory for the files of one test.
fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("app_trace")
        .join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    dir
}

// the variable that names the trace file is a documented interface of the probes
#[test]
fn rule_env_variable_name() {
    assert_eq!(TRACE_ENV, "CLIPMONGER_UI_TRACE");
}

// from_env: None when the variable is unset or empty
#[test]
fn rule_env_unset_or_empty_gives_no_trace() {
    {
        let _env = TraceEnv::set(None);
        assert!(Trace::from_env().is_none(), "unset");
    }
    {
        let _env = TraceEnv::set(Some(OsStr::new("")));
        assert!(Trace::from_env().is_none(), "empty");
    }
}

// from_env: None when the file cannot be opened
#[test]
fn rule_env_unopenable_file_gives_no_trace() {
    let dir = scratch("unopenable");
    let missing = dir.join("no-such-dir").join("trace.log");
    {
        let _env = TraceEnv::set(Some(missing.as_os_str()));
        assert!(Trace::from_env().is_none(), "a file in a missing directory");
    }
    {
        let _env = TraceEnv::set(Some(dir.as_os_str()));
        assert!(Trace::from_env().is_none(), "a directory");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// from_env: the file is created if needed and appended to
#[test]
fn rule_env_creates_and_appends() {
    let dir = scratch("append");
    let path = dir.join("trace.log");
    let env = TraceEnv::set(Some(path.as_os_str()));
    let mut first = Trace::from_env().expect("a trace for a new file");
    assert!(path.exists(), "the file is created when the trace is built");
    first.record(&TraceEvent::Open {
        source: "start",
        target: false,
        gesture: false,
    });
    first.record(&TraceEvent::Hide { reason: "dismiss" });
    let after_first = std::fs::read(&path).expect("read the trace");
    assert_eq!(
        lines_of(&after_first),
        vec![open_line("start", false, false), hide_line("dismiss")],
        "lines are on disk right after record"
    );
    let mut second = Trace::from_env().expect("a trace for an existing file");
    second.record(&TraceEvent::Error);
    let all = std::fs::read(&path).expect("read the trace");
    assert!(all.starts_with(&after_first), "the old lines are kept");
    assert_eq!(
        lines_of(&all),
        vec![
            open_line("start", false, false),
            hide_line("dismiss"),
            error_line()
        ]
    );
    drop(first);
    drop(second);
    drop(env);
    let _ = std::fs::remove_dir_all(&dir);
}

// from_env: a file with earlier content is appended to, not truncated
#[test]
fn rule_env_keeps_earlier_content() {
    let dir = scratch("keep");
    let path = dir.join("trace.log");
    std::fs::write(&path, b"{\"ev\":\"error\",\"t_ms\":1}\n").expect("seed the file");
    let env = TraceEnv::set(Some(path.as_os_str()));
    let mut trace = Trace::from_env().expect("a trace");
    trace.record(&TraceEvent::Paste { target: true });
    drop(trace);
    drop(env);
    let bytes = std::fs::read(&path).expect("read");
    let text = String::from_utf8(bytes).expect("UTF-8");
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some("{\"ev\":\"error\",\"t_ms\":1}"));
    let second = parse(lines.next().expect("the new line"));
    assert_eq!(second["ev"], json!("paste"));
    assert_eq!(second["target"], json!(true));
    assert_eq!(lines.next(), None);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// The lines PopupApp writes
// ---------------------------------------------------------------------------

struct Rig {
    app: PopupApp,
    ctx: egui::Context,
    info: egui::ViewportInfo,
    events: Vec<egui::Event>,
    cmds: mpsc::Receiver<UiCmd>,
    bridge: mpsc::Sender<UiEvent>,
    tray: Arc<Mutex<Vec<TrayAction>>>,
    sink: Arc<Mutex<SinkState>>,
    foreground: Arc<AtomicUsize>,
}

/// What a visible pass painted: the text with the centre of its galley.
struct Painted(Vec<(String, egui::Pos2)>);

impl Painted {
    fn centre_of(&self, text: &str) -> egui::Pos2 {
        self.0
            .iter()
            .find(|(painted, _)| painted == text)
            .map_or_else(
                || panic!("{text:?} was not painted: {:?}", self.0),
                |(_, pos)| *pos,
            )
    }
}

fn collect_text(shape: &egui::Shape, out: &mut Vec<(String, egui::Pos2)>) {
    match shape {
        egui::Shape::Text(text) => {
            let rect = egui::Rect::from_min_size(text.pos, text.galley.size());
            out.push((text.galley.text().to_string(), rect.center()));
        }
        egui::Shape::Vec(shapes) => {
            for shape in shapes {
                collect_text(shape, out);
            }
        }
        _ => {}
    }
}

impl Rig {
    fn build(open: bool, trace: Option<Box<dyn Write + Send>>) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let tray = Arc::new(Mutex::new(Vec::new()));
        let queue = Arc::clone(&tray);
        let foreground = Arc::new(AtomicUsize::new(0));
        let hook = Arc::clone(&foreground);
        let app = PopupApp::new(AppParts {
            profile: PathBuf::from("trace-profile"),
            cmds: cmd_tx,
            events: event_rx,
            tray: Box::new(move || std::mem::take(&mut *queue.lock().expect("tray queue"))),
            hold: None,
            open,
            status_period: Duration::from_secs(3600),
            trace: trace.map(Trace::new),
            allow_foreground: Box::new(move || {
                hook.fetch_add(1, Ordering::SeqCst);
            }),
        });
        let rig = Self {
            app,
            ctx: egui::Context::default(),
            info: egui::ViewportInfo::default(),
            events: Vec::new(),
            cmds: cmd_rx,
            bridge: event_tx,
            tray,
            sink: Arc::new(Mutex::new(SinkState::default())),
            foreground,
        };
        let _ = rig.cmds();
        rig
    }

    /// A rig that writes its trace to a readable sink.
    fn new(open: bool) -> Self {
        let state = Arc::new(Mutex::new(SinkState::default()));
        let mut rig = Self::build(open, Some(Box::new(Sink(Arc::clone(&state)))));
        rig.sink = state;
        rig
    }

    fn push(&self, event: UiEvent) {
        self.bridge.send(event).expect("the app holds the receiver");
    }

    fn tray(&self, action: TrayAction) {
        self.tray.lock().expect("tray queue").push(action);
    }

    fn input(&mut self, event: egui::Event) {
        self.events.push(event);
    }

    fn key(&mut self, key: egui::Key) {
        self.input(egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
    }

    fn raw(&mut self, visible: bool) -> egui::RawInput {
        let mut raw = egui::RawInput {
            viewports: std::iter::once((egui::ViewportId::ROOT, self.info.clone())).collect(),
            ..egui::RawInput::default()
        };
        self.info.events.clear();
        if visible {
            raw.screen_rect = Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(480.0, 360.0),
            ));
            raw.events = std::mem::take(&mut self.events);
        }
        raw
    }

    /// A hidden Popup pass.
    fn tick(&mut self) {
        let raw = self.raw(false);
        let app = &mut self.app;
        let _ = self
            .ctx
            .run_logic(&raw, |ctx| app.logic(ctx, WindowKind::Popup));
    }

    /// A visible Popup pass.
    fn show(&mut self) -> Painted {
        let raw = self.raw(true);
        let app = &mut self.app;
        let output = self.ctx.run_ui(raw, |ui| {
            app.logic(ui.ctx(), WindowKind::Popup);
            app.ui(ui, WindowKind::Popup);
        });
        let egui::FullOutput {
            mut textures_delta,
            shapes,
            ..
        } = output;
        textures_delta.clear();
        let mut painted = Vec::new();
        for clipped in &shapes {
            collect_text(&clipped.shape, &mut painted);
        }
        Painted(painted)
    }

    fn cmds(&self) -> Vec<UiCmd> {
        self.cmds.try_iter().collect()
    }

    fn is_open(&self) -> bool {
        self.app.desired().popup_open
    }

    fn bytes(&self) -> Vec<u8> {
        self.sink.lock().expect("sink").bytes.clone()
    }

    fn trace(&self) -> Vec<Value> {
        lines_of(&self.bytes())
    }
}

fn clip(n: u128, preview: &str) -> Clip {
    serde_json::from_value(json!({
        "id": Uuid::from_u128(n).to_string(),
        "created_ms": 1_700_000_000_000_i64,
        "source_os": "linux",
        "preview": preview,
        "pinned": false,
        "rev": {"wall_ms": 1, "dev": "test-dev"},
    }))
    .expect("the test clip is valid")
}

fn history(clips: &[Clip]) -> UiEvent {
    UiEvent::History(json!({ "clips": clips }))
}

fn hotkey(target: Option<u64>, gesture: Option<u64>) -> UiEvent {
    UiEvent::ShowPopup(ShowPopupEvent {
        source: "hotkey".to_string(),
        target,
        gesture,
        ..ShowPopupEvent::default()
    })
}

fn request() -> UiEvent {
    UiEvent::ShowPopup(ShowPopupEvent {
        source: "request".to_string(),
        ..ShowPopupEvent::default()
    })
}

fn toggle() -> UiEvent {
    UiEvent::ShowPopup(ShowPopupEvent {
        source: "hotkey".to_string(),
        toggle: true,
        ..ShowPopupEvent::default()
    })
}

// PopupApp: every open source writes its word, and the flags of the new context
#[test]
fn rule_app_open_lines_name_the_source() {
    type Action = Box<dyn Fn(&mut Rig)>;
    let cases: Vec<(&str, Action, Vec<Value>)> = vec![
        (
            "hotkey with target and gesture",
            Box::new(|rig| rig.push(hotkey(Some(7), Some(9)))),
            vec![open_line("hotkey", true, true)],
        ),
        (
            "hotkey with a target only",
            Box::new(|rig| rig.push(hotkey(Some(7), None))),
            vec![open_line("hotkey", true, false)],
        ),
        (
            "hotkey with a gesture only",
            Box::new(|rig| rig.push(hotkey(None, Some(9)))),
            vec![open_line("hotkey", false, true)],
        ),
        (
            "hotkey with neither",
            Box::new(|rig| rig.push(hotkey(None, None))),
            vec![open_line("hotkey", false, false)],
        ),
        (
            "request",
            Box::new(|rig| rig.push(request())),
            vec![open_line("request", false, false)],
        ),
        (
            "tray click",
            Box::new(|rig| rig.tray(TrayAction::Toggle)),
            vec![open_line("tray", false, false)],
        ),
        (
            "tray menu entry",
            Box::new(|rig| rig.tray(TrayAction::OpenPopup)),
            vec![open_line("request", false, false)],
        ),
        (
            "failed paste",
            Box::new(|rig| rig.push(UiEvent::PasteFailed("gone".to_string()))),
            vec![open_line("paste_failed", false, false), error_line()],
        ),
    ];
    for (name, action, expected) in cases {
        let mut rig = Rig::new(false);
        action(&mut rig);
        rig.tick();
        assert!(rig.is_open(), "{name}");
        assert_eq!(rig.trace(), expected, "{name}");
    }
}

// PopupApp: every hide reason is written
#[test]
fn rule_app_hide_lines_name_the_reason() {
    type Action = Box<dyn Fn(&mut Rig)>;
    let cases: Vec<(&str, Action, &str)> = vec![
        (
            "dismiss key",
            Box::new(|rig| {
                rig.key(egui::Key::Escape);
                rig.show();
            }),
            "dismiss",
        ),
        (
            "focus loss",
            Box::new(|rig| {
                rig.info.focused = Some(true);
                rig.tick();
                rig.info.focused = Some(false);
                rig.tick();
            }),
            "focus_lost",
        ),
        (
            "close request",
            Box::new(|rig| {
                rig.info.events.push(egui::ViewportEvent::Close);
                rig.tick();
            }),
            "close_request",
        ),
        (
            "toggle push",
            Box::new(|rig| {
                rig.push(toggle());
                rig.tick();
            }),
            "toggle",
        ),
        (
            "tray click",
            Box::new(|rig| {
                rig.tray(TrayAction::Toggle);
                rig.tick();
            }),
            "tray",
        ),
        (
            "pick",
            Box::new(|rig| {
                rig.push(history(&[clip(1, "alpha")]));
                rig.tick();
                rig.key(egui::Key::Enter);
                rig.show();
            }),
            "pick",
        ),
    ];
    for (name, action, reason) in cases {
        let mut rig = Rig::new(true);
        action(&mut rig);
        assert!(!rig.is_open(), "{name}");
        assert_eq!(
            rig.trace(),
            vec![open_line("start", false, false), hide_line(reason)],
            "{name}"
        );
    }
}

// PopupApp: a pick with a target writes Hide{pick} then Paste{target: true}
#[test]
fn rule_app_pick_with_a_target_writes_hide_then_paste() {
    let mut rig = Rig::new(false);
    rig.push(hotkey(Some(7), Some(9)));
    rig.push(history(&[clip(1, "alpha")]));
    rig.tick();
    let _ = rig.cmds();
    rig.key(egui::Key::Enter);
    rig.show();
    assert_eq!(rig.cmds().len(), 1);
    assert_eq!(rig.foreground.load(Ordering::SeqCst), 1);
    assert_eq!(
        rig.trace(),
        vec![
            open_line("hotkey", true, true),
            hide_line("pick"),
            paste_line(true)
        ]
    );
}

// PopupApp: a pick without a target writes Paste{target: false} when the paste goes out
#[test]
fn rule_app_pick_without_a_target_writes_the_paste_when_it_is_sent() {
    let mut rig = Rig::new(true);
    rig.push(history(&[clip(1, "alpha")]));
    rig.tick();
    rig.key(egui::Key::Enter);
    rig.show();
    assert_eq!(
        rig.trace(),
        vec![open_line("start", false, false), hide_line("pick")],
        "no Paste line before the command goes out"
    );
    rig.tick();
    thread::sleep(Duration::from_millis(170));
    rig.tick();
    let cmds = rig.cmds();
    assert!(
        matches!(cmds.last(), Some(UiCmd::PasteClipTo(_, None, _, None))),
        "commands {cmds:?}"
    );
    assert_eq!(
        rig.trace(),
        vec![
            open_line("start", false, false),
            hide_line("pick"),
            paste_line(false)
        ]
    );
    rig.tick();
    assert_eq!(rig.trace().len(), 3, "one Paste line per command");
}

/// A group, a permanent in it and the popup showing that group's tab.
fn show_permanents(rig: &mut Rig) {
    rig.push(UiEvent::PermGroups(
        json!({"groups": [{"name": "Work", "system": false, "count": 1}]}),
    ));
    rig.push(UiEvent::Permanents(json!({"permanents": [{
        "id": Uuid::from_u128(77).to_string(), "group": "Work", "name": "Signature",
        "hotkey": null, "kind": "Paste", "body": "Regards", "js": null,
        "rev": {"wall_ms": 1, "dev": "test-dev", "ctr": 0}
    }]})));
    rig.tick();
    rig.app.config().lock().expect("settings").perm_source = Some(0);
}

/// Press and release the primary pointer button at `pos`.
fn click(rig: &mut Rig, pos: egui::Pos2) {
    rig.input(egui::Event::PointerMoved(pos));
    rig.show();
    for pressed in [true, false] {
        rig.input(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        rig.show();
    }
}

// PopupApp: picking a permanent writes Hide{pick}, and Paste{target: false} when the
// exec goes out; allow_foreground runs once
#[test]
fn rule_app_permanent_pick_writes_hide_then_paste_without_a_target() {
    let mut rig = Rig::new(true);
    show_permanents(&mut rig);
    let painted = rig.show();
    let row = painted.centre_of("Signature");
    let _ = rig.cmds();
    click(&mut rig, row);
    assert!(
        !rig.is_open(),
        "the click on the selected permanent picked it"
    );
    assert_eq!(rig.foreground.load(Ordering::SeqCst), 1);
    assert_eq!(
        rig.trace(),
        vec![open_line("start", false, false), hide_line("pick")]
    );
    assert_eq!(
        rig.cmds(),
        vec![],
        "the exec waits for the hide and the delay"
    );
    rig.tick();
    thread::sleep(Duration::from_millis(170));
    rig.tick();
    assert_eq!(rig.cmds(), vec![UiCmd::ExecPermanent(Uuid::from_u128(77))]);
    assert_eq!(
        rig.trace(),
        vec![
            open_line("start", false, false),
            hide_line("pick"),
            paste_line(false)
        ]
    );
}

// PopupApp: an error line that is new or changed, or set again by PasteFailed, Error or
// PasteUnavailable, is written as Error; anything else is not
#[test]
fn rule_app_error_lines() {
    let mut rig = Rig::new(true);
    let mut expected = vec![open_line("start", false, false)];
    // Error: new, then set again with the same text, then changed
    for text in ["boom", "boom", "other"] {
        rig.push(UiEvent::Error(text.to_string()));
        rig.tick();
        expected.push(error_line());
        assert_eq!(rig.trace(), expected, "Error({text})");
    }
    // PasteFailed while open: set again
    for _ in 0..2 {
        rig.push(UiEvent::PasteFailed("other".to_string()));
        rig.tick();
        expected.push(error_line());
        assert_eq!(rig.trace(), expected, "PasteFailed");
    }
    // PasteUnavailable: changed, then set again
    for _ in 0..2 {
        rig.push(UiEvent::PasteUnavailable);
        rig.tick();
        expected.push(error_line());
        assert_eq!(rig.trace(), expected, "PasteUnavailable");
    }
    // events that leave the error line alone write nothing
    for event in [
        UiEvent::Offline("link".to_string()),
        UiEvent::Status(json!({"version": "1"})),
        history(&[clip(1, "alpha")]),
    ] {
        rig.push(event);
        rig.tick();
        assert_eq!(
            rig.trace(),
            expected,
            "events that do not touch the error line"
        );
    }
}

// PopupApp: a malformed history sets the error line once; the same error is not written
// again until something cleared it
#[test]
fn rule_app_malformed_history_writes_one_error_until_the_line_is_cleared() {
    let mut rig = Rig::new(true);
    let mut expected = vec![open_line("start", false, false)];
    rig.push(UiEvent::History(json!({"clips": "none"})));
    rig.tick();
    expected.push(error_line());
    assert_eq!(rig.trace(), expected, "new error line");
    rig.push(UiEvent::History(json!({"clips": "none"})));
    rig.tick();
    assert_eq!(rig.trace(), expected, "the same line again is no change");
    rig.key(egui::Key::A);
    rig.show();
    assert_eq!(rig.trace(), expected, "clearing the line writes nothing");
    rig.push(UiEvent::History(json!({"clips": "none"})));
    rig.tick();
    expected.push(error_line());
    assert_eq!(rig.trace(), expected, "new again after it was cleared");
    // a different error replaces it: changed
    rig.push(UiEvent::Error("different".to_string()));
    rig.tick();
    expected.push(error_line());
    assert_eq!(rig.trace(), expected);
    rig.push(UiEvent::History(json!({"clips": "none"})));
    rig.tick();
    expected.push(error_line());
    assert_eq!(rig.trace(), expected, "the line changed back");
}

// PopupApp: an Error that goes to the Settings error line is not an Error of the popup
#[test]
fn rule_app_settings_errors_write_nothing() {
    let mut rig = Rig::new(true);
    rig.tray(TrayAction::OpenConfig);
    rig.tick();
    rig.push(UiEvent::Error("settings problem".to_string()));
    rig.tick();
    assert_eq!(rig.trace(), vec![open_line("start", false, false)]);
}

// PopupApp: transitions that change nothing write nothing
#[test]
fn rule_app_quiet_events_write_nothing() {
    let mut rig = Rig::new(true);
    rig.push(request());
    rig.push(UiEvent::Offline("x".to_string()));
    rig.push(UiEvent::Status(json!({"monitoring_paused": true})));
    rig.push(UiEvent::PasteDone(Uuid::from_u128(1)));
    rig.tray(TrayAction::TogglePause);
    rig.tray(TrayAction::OpenConfig);
    rig.tick();
    rig.show();
    assert_eq!(rig.trace(), vec![open_line("start", false, false)]);
}

// the trace holds only booleans and fixed words, never clip contents, errors or ids
#[test]
fn rule_app_trace_holds_no_contents() {
    let mut rig = Rig::new(false);
    rig.push(hotkey(Some(987_654_321), Some(123_456_789)));
    rig.push(history(&[clip(1, "SECRET-PREVIEW")]));
    rig.tick();
    rig.push(UiEvent::Error("SECRET-ERROR-TEXT".to_string()));
    rig.push(UiEvent::PasteFailed("SECRET-PASTE-TEXT".to_string()));
    rig.tick();
    rig.key(egui::Key::A);
    rig.show();
    rig.key(egui::Key::Enter);
    rig.show();
    rig.tray(TrayAction::Toggle);
    rig.tick();
    let text = String::from_utf8(rig.bytes()).expect("UTF-8");
    for secret in [
        "SECRET",
        "987654321",
        "123456789",
        &Uuid::from_u128(1).to_string(),
        "trace-profile",
    ] {
        assert!(
            !text.contains(secret),
            "{secret:?} leaked into the trace: {text}"
        );
    }
    let allowed_keys: &[&str] = &["ev", "t_ms", "source", "target", "gesture", "reason"];
    let allowed_words = [
        "open",
        "hide",
        "paste",
        "error",
        "hotkey",
        "request",
        "tray",
        "paste_failed",
        "start",
        "other",
        "dismiss",
        "focus_lost",
        "close_request",
        "toggle",
        "pick",
    ];
    for text in text.lines() {
        let value = parse(text);
        for (key, field) in value.as_object().expect("an object") {
            assert!(allowed_keys.contains(&key.as_str()), "key {key} in {text}");
            match field {
                Value::Bool(_) | Value::Number(_) => {}
                Value::String(word) => {
                    assert!(
                        allowed_words.contains(&word.as_str()),
                        "word {word} in {text}"
                    );
                }
                other => panic!("{key}: {other} in {text}"),
            }
        }
    }
}

// PopupApp: every line is on its way out as soon as it is written
#[test]
fn rule_app_lines_are_flushed_at_once() {
    let mut rig = Rig::new(false);
    for event in [request(), toggle(), hotkey(Some(1), None), toggle()] {
        rig.push(event);
        rig.tick();
        let state = rig.sink.lock().expect("sink");
        let lines = lines_of(&state.bytes).len();
        assert_eq!(state.flushes, lines, "one flush per line");
    }
    assert_eq!(rig.sink.lock().expect("sink").flushes, 4);
}

// PopupApp: the stamps are unix milliseconds of the transition
#[test]
fn rule_app_lines_are_stamped_with_the_unix_time() {
    let mut rig = Rig::new(false);
    let before = unix_ms();
    rig.push(request());
    rig.tick();
    thread::sleep(Duration::from_millis(5));
    rig.push(toggle());
    rig.tick();
    let after = unix_ms();
    let text = String::from_utf8(rig.bytes()).expect("UTF-8");
    let stamps: Vec<u64> = text
        .lines()
        .map(|text| parse(text)["t_ms"].as_u64().expect("numeric t_ms"))
        .collect();
    assert_eq!(stamps.len(), 2);
    assert!(
        stamps.iter().all(|stamp| (before..=after).contains(stamp)),
        "{stamps:?}"
    );
    assert!(stamps[0] <= stamps[1]);
}

// PopupApp: without a trace nothing is written and everything else works
#[test]
fn rule_app_without_a_trace_works() {
    let mut rig = Rig::build(true, None);
    rig.push(history(&[clip(1, "alpha")]));
    rig.tick();
    rig.key(egui::Key::Escape);
    rig.show();
    assert!(!rig.is_open());
    rig.push(request());
    rig.tick();
    assert!(rig.is_open());
    assert_eq!(rig.cmds().len(), 3, "the three open commands");
    assert_eq!(rig.bytes(), Vec::<u8>::new());
}

// PopupApp: a sink that refuses its lines does not stop the app
#[test]
fn rule_app_survives_a_broken_sink() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut rig = Rig::build(true, Some(Box::new(BrokenSink(Arc::clone(&calls)))));
    rig.push(history(&[clip(1, "alpha")]));
    rig.tick();
    rig.key(egui::Key::Escape);
    rig.show();
    assert!(!rig.is_open());
    rig.push(request());
    rig.tick();
    assert!(rig.is_open());
    assert_eq!(rig.cmds().len(), 3, "the three open commands");
    assert!(calls.load(Ordering::SeqCst) > 0, "the sink was written to");
}

// the trace of one whole session, in order
#[test]
fn rule_app_session_trace_in_order() {
    let mut rig = Rig::new(true);
    rig.push(history(&[clip(1, "alpha")]));
    rig.tick();
    rig.key(egui::Key::Escape);
    rig.show();
    rig.push(hotkey(Some(5), Some(6)));
    rig.tick();
    rig.key(egui::Key::Enter);
    rig.show();
    rig.push(UiEvent::PasteFailed("window gone".to_string()));
    rig.tick();
    rig.tray(TrayAction::Toggle);
    rig.tick();
    rig.push(request());
    rig.tick();
    rig.push(toggle());
    rig.tick();
    assert_eq!(
        rig.trace(),
        vec![
            open_line("start", false, false),
            hide_line("dismiss"),
            open_line("hotkey", true, true),
            hide_line("pick"),
            paste_line(true),
            open_line("paste_failed", false, false),
            error_line(),
            hide_line("tray"),
            open_line("request", false, false),
            hide_line("toggle"),
        ]
    );
}

/// Reach a painted widget through keyboard navigation, without activating it.
fn focus_widget(rig: &mut Rig, text: &str) {
    let centre = rig.show().centre_of(text);
    for _ in 0..20 {
        rig.key(egui::Key::Tab);
        rig.show();
        let focused = rig.ctx.memory(egui::Memory::focused);
        if focused
            .and_then(|id| rig.ctx.read_response(id))
            .is_some_and(|response| response.rect.contains(centre))
        {
            return;
        }
        rig.input(egui::Event::Key {
            key: egui::Key::Tab,
            physical_key: Some(egui::Key::Tab),
            pressed: false,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        rig.show();
    }
    panic!("keyboard navigation never focused {text:?}");
}

#[test]
fn fix_x1_settings_activation_does_not_pick_a_clip() {
    for key in [egui::Key::Enter, egui::Key::Space] {
        let mut rig = Rig::new(false);
        rig.push(hotkey(Some(7), Some(9)));
        rig.push(history(&[clip(1, "alpha")]));
        rig.tick();
        focus_widget(
            &mut rig,
            clipmonger_app_ui::strings::text("config.open_config"),
        );
        let _ = rig.cmds();
        rig.key(key);
        rig.show();
        assert!(rig.app.desired().settings_open, "{key:?} opens Settings");
        assert!(rig.is_open(), "{key:?} must leave the popup open");
        rig.tick();
        thread::sleep(Duration::from_millis(170));
        rig.tick();
        assert!(rig.cmds().iter().all(|cmd| !matches!(
            cmd,
            UiCmd::PasteClipTo(..) | UiCmd::PasteClip(_) | UiCmd::ExecPermanent(_)
        )));
        assert_eq!(rig.foreground.load(Ordering::SeqCst), 0);
        assert_eq!(rig.trace(), vec![open_line("hotkey", true, true)]);
    }
}

#[test]
fn fix_x1_group_activation_does_not_pick_a_clip() {
    for key in [egui::Key::Enter, egui::Key::Space] {
        let mut rig = Rig::new(false);
        rig.push(hotkey(Some(7), Some(9)));
        rig.push(history(&[clip(1, "alpha")]));
        show_permanents(&mut rig);
        rig.app.config().lock().expect("settings").perm_source = None;
        focus_widget(&mut rig, "Work");
        let _ = rig.cmds();
        rig.key(key);
        rig.show();
        assert_eq!(
            rig.app.config().lock().expect("settings").perm_source,
            Some(0),
            "{key:?} selects the group"
        );
        assert!(rig.is_open(), "{key:?} must leave the popup open");
        rig.tick();
        thread::sleep(Duration::from_millis(170));
        rig.tick();
        assert_eq!(rig.cmds(), vec![], "no paste or exec after {key:?}");
        assert_eq!(rig.foreground.load(Ordering::SeqCst), 0);
        assert_eq!(rig.trace(), vec![open_line("hotkey", true, true)]);
    }
}

#[test]
fn fix_x3_permanent_error_reopens_and_traces_the_painted_error_once() {
    permanent_failure_reopens(permanent_backend_failure("exec failed"), "exec failed");
}

#[test]
fn fix_x3_permanent_unavailable_reopens_and_traces_the_painted_error_once() {
    permanent_failure_reopens(
        UiEvent::PasteUnavailable,
        clipmonger_app_ui::strings::text("popup.paste_blocked"),
    );
}

fn permanent_failure_reopens(failure: UiEvent, message: &str) {
    let mut rig = Rig::new(true);
    send_permanent(&mut rig);
    rig.push(failure);
    rig.tick();
    assert!(rig.is_open(), "{message}: a failed exec reopens the popup");
    assert_eq!(rig.app.shell().error(), Some(message));
    assert_eq!(
        rig.cmds(),
        vec![UiCmd::ListHistory, UiCmd::ListPermanents, UiCmd::PermGroups]
    );
    rig.show().centre_of(message);
    let expected = vec![
        open_line("start", false, false),
        hide_line("pick"),
        paste_line(false),
        open_line("paste_failed", false, false),
        error_line(),
    ];
    assert_eq!(rig.trace(), expected);
    rig.tick();
    rig.show().centre_of(message);
    assert_eq!(
        rig.trace(),
        expected,
        "a later pass does not repeat the error"
    );
    rig.tray(TrayAction::Toggle);
    rig.tick();
    let hidden_trace = rig.trace();
    rig.push(UiEvent::Error("unrelated later error".to_string()));
    rig.tick();
    assert!(!rig.is_open(), "the failed exec was already handled");
    assert_eq!(rig.trace(), hidden_trace);
}

fn send_permanent(rig: &mut Rig) {
    show_permanents(rig);
    let row = rig.show().centre_of("Signature");
    let _ = rig.cmds();
    click(rig, row);
    assert!(!rig.is_open());
    rig.tick();
    thread::sleep(Duration::from_millis(170));
    rig.tick();
    assert_eq!(rig.cmds(), vec![UiCmd::ExecPermanent(Uuid::from_u128(77))]);
}

#[test]
fn fix_x3_successful_permanent_does_not_reopen_for_a_later_error() {
    let mut rig = Rig::new(true);
    send_permanent(&mut rig);
    rig.push(UiEvent::PasteDone(Uuid::from_u128(77)));
    rig.tick();
    let before = rig.trace();
    rig.push(UiEvent::Error("unrelated later error".to_string()));
    rig.tick();
    assert!(!rig.is_open());
    assert_eq!(rig.trace(), before);
}

#[test]
fn fix_x3_permanent_error_reopens_even_with_settings_open() {
    let mut rig = Rig::new(true);
    send_permanent(&mut rig);
    rig.tray(TrayAction::OpenConfig);
    rig.tick();
    rig.push(permanent_backend_failure("exec failed"));
    rig.tick();
    assert!(rig.is_open());
    assert!(rig.app.desired().settings_open);
    assert_eq!(rig.app.shell().error(), Some("exec failed"));
    assert_eq!(rig.app.config().lock().expect("settings").error, "");
    rig.show().centre_of("exec failed");
    assert_eq!(
        rig.trace()
            .iter()
            .filter(|line| **line == error_line())
            .count(),
        1
    );
}

#[test]
fn fix_x3_closed_errors_without_an_exec_are_not_traced_or_reopened() {
    for failure in [
        UiEvent::Error("background".to_string()),
        UiEvent::PasteUnavailable,
    ] {
        let mut rig = Rig::new(false);
        rig.push(failure);
        rig.tick();
        assert!(!rig.is_open());
        assert!(rig.app.shell().error().is_some());
        assert_eq!(
            rig.trace(),
            Vec::<Value>::new(),
            "a hidden error is not shown"
        );
        rig.push(request());
        rig.tick();
        assert!(rig.app.shell().error().is_none());
        assert_eq!(rig.trace(), vec![open_line("request", false, false)]);
    }
}

/// Get the actual bridge reply to a failed permanent execution.
fn permanent_backend_failure(message: &str) -> UiEvent {
    use clipmonger_app_ui::ipc_bridge::{Bridge, Transport};
    use clipmonger_ipc::proto::IpcError;
    struct FailedExec(String);
    impl Transport for FailedExec {
        fn call(&mut self, method: &str, _: &Value) -> Result<Value, IpcError> {
            assert_eq!(method, "permanent.exec");
            Err(IpcError::new("backend", self.0.clone()))
        }
    }
    let (tx, rx) = Bridge::spawn(Box::new(FailedExec(message.to_string())));
    tx.send(UiCmd::ExecPermanent(Uuid::from_u128(77)))
        .expect("exec");
    rx.recv_timeout(Duration::from_secs(2)).expect("exec reply")
}

#[test]
fn fix_n1_unrelated_error_does_not_steal_a_pending_permanent_failure() {
    for settings in [false, true] {
        let mut rig = Rig::new(true);
        send_permanent(&mut rig);
        if settings {
            rig.tray(TrayAction::OpenConfig);
            rig.tick();
            rig.app
                .config()
                .lock()
                .expect("settings")
                .pending_apply
                .push("popup.font_scale".to_string());
        }
        let before = rig.trace();
        rig.push(UiEvent::Error("earlier command failed".to_string()));
        rig.tick();
        assert!(
            !rig.is_open(),
            "an earlier command must not reopen the popup"
        );
        assert_eq!(rig.trace(), before);
        if settings {
            let config = rig.app.config().lock().expect("settings");
            assert_eq!(config.error, "earlier command failed");
            assert!(config.pending_apply.is_empty());
        }
        rig.push(permanent_backend_failure("exec failed"));
        rig.tick();
        assert!(rig.is_open());
        assert_eq!(rig.app.shell().error(), Some("exec failed"));
        rig.show().centre_of("exec failed");
        assert_eq!(
            rig.trace()
                .iter()
                .filter(|line| **line == error_line())
                .count(),
            1
        );
    }
}
