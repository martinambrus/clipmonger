//! Rules of `PopupApp` (`clipmonger_app_ui::app`), the application behind the
//! runner's two windows, driven headlessly like the runner drives it.
//!
//! One egui `Context` per window kind. A visible pass is
//! `ctx.run_ui(raw, |ui| { app.logic(..); app.ui(..) })`, a hidden pass is
//! `ctx.run_logic(&raw, |ctx| app.logic(..))`. `raw` mimics the runner: the ROOT
//! `ViewportInfo` carries `focused`, `monitor_size` and `events`, key presses are
//! `egui::Event::Key` in `raw.events`. The bridge is two channels held by the test,
//! the tray is a shared queue, the trace is a shared byte sink and the
//! `allow_foreground` hook bumps a counter. Time is real only where time is the
//! thing under test.
//!
//! Tests assert on what the contract makes observable: commands on the bridge
//! channel, `desired()`, the `shell()` / `vm()` / `config()` getters, parsed trace
//! lines (without `t_ms`), repaint requests, viewport commands and painted text.
//!
//! Run: `cargo test -p clipmonger-app-ui --test app_rules`

use std::any::Any;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use clipmonger_app_ui::app::{now_ms, AppParts, PopupApp, Trace, DRAIN_CAP};
use clipmonger_app_ui::ipc_bridge::{UiCmd, UiEvent};
use clipmonger_app_ui::runner::{App, Desired, WindowKind};
use clipmonger_app_ui::shell::Context as PasteContext;
use clipmonger_app_ui::tray_logic::TrayAction;
use clipmonger_app_ui::{config_model, placement, strings};
use clipmonger_core::model::Clip;
use clipmonger_ipc::proto::methods::ShowPopupEvent;
use serde_json::{json, Value};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// The rig
// ---------------------------------------------------------------------------

/// A status period that never elapses within a test.
const HOUR: Duration = Duration::from_secs(3600);

/// A trace sink the test can read.
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

/// One repaint request seen by a context callback.
#[derive(Clone, Copy, Debug)]
struct Request {
    delay: Duration,
}

/// What the test holds of one window: its context, the ROOT viewport info the
/// runner would hand in and the events waiting for the next visible pass.
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

/// What one pass handed back to the runner.
struct Pass {
    /// Commands for the ROOT viewport.
    commands: Vec<egui::ViewportCommand>,
    /// Every text the pass painted (empty for a hidden pass).
    texts: Vec<String>,
    fills: Vec<egui::Color32>,
    bounds: Vec<(String, egui::Rect, egui::Rect)>,
}

impl Pass {
    /// The positions of the `OuterPosition` commands.
    fn positions(&self) -> Vec<egui::Pos2> {
        self.commands
            .iter()
            .filter_map(|command| match command {
                egui::ViewportCommand::OuterPosition(pos) => Some(*pos),
                _ => None,
            })
            .collect()
    }

    /// The sizes of the `InnerSize` commands.
    fn sizes(&self) -> Vec<egui::Vec2> {
        self.commands
            .iter()
            .filter_map(|command| match command {
                egui::ViewportCommand::InnerSize(size) => Some(*size),
                _ => None,
            })
            .collect()
    }

    fn painted(&self, text: &str) -> bool {
        self.texts.iter().any(|painted| painted == text)
    }
}

fn collect_text(shape: &egui::Shape, out: &mut Vec<String>) {
    match shape {
        egui::Shape::Text(text) => out.push(text.galley.text().to_string()),
        egui::Shape::Vec(shapes) => {
            for shape in shapes {
                collect_text(shape, out);
            }
        }
        _ => {}
    }
}

fn collect_fills(shape: &egui::Shape, out: &mut Vec<egui::Color32>) {
    match shape {
        egui::Shape::Rect(rect) => out.push(rect.fill),
        egui::Shape::Vec(shapes) => {
            for shape in shapes {
                collect_fills(shape, out);
            }
        }
        _ => {}
    }
}

/// Builder options of a rig.
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

    fn period(mut self, period: Duration) -> Self {
        self.status_period = period;
        self
    }
}

struct Rig {
    app: PopupApp,
    popup: Win,
    settings: Win,
    cmds: Arc<Mutex<mpsc::Receiver<UiCmd>>>,
    /// Commands already taken off the channel by the foreground hook.
    backlog: Arc<Mutex<Vec<UiCmd>>>,
    /// The commands that were on the channel each time the foreground hook ran.
    at_hook: Arc<Mutex<Vec<Vec<UiCmd>>>>,
    events: mpsc::Sender<UiEvent>,
    tray: Arc<Mutex<Vec<TrayAction>>>,
    tray_polls: Arc<AtomicUsize>,
    trace: Arc<Mutex<Vec<u8>>>,
    foreground: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<Request>>>,
    /// When the app was built: the shell clock starts after this.
    born: Instant,
}

impl Rig {
    /// A rig whose start commands are still on the channel.
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
            profile: PathBuf::from("rig-profile"),
            cmds: cmd_tx,
            events: event_rx,
            tray: Box::new(move || {
                polls.fetch_add(1, Ordering::SeqCst);
                std::mem::take(&mut *queue.lock().expect("tray queue"))
            }),
            hold: setup.hold,
            open: setup.open,
            status_period: setup.status_period,
            trace: setup
                .trace
                .then(|| Trace::new(Box::new(Sink(Arc::clone(&trace))))),
            allow_foreground: Box::new(move || {
                hook.fetch_add(1, Ordering::SeqCst);
                let mut backlog = hook_backlog.lock().expect("backlog");
                backlog.extend(hook_commands.lock().expect("commands").try_iter());
                hook_log.lock().expect("hook log").push(backlog.clone());
            }),
        });
        let popup = Win::new();
        let requests = Arc::new(Mutex::new(Vec::new()));
        {
            let requests = Arc::clone(&requests);
            popup.ctx.set_request_repaint_callback(move |info| {
                requests
                    .lock()
                    .expect("requests")
                    .push(Request { delay: info.delay });
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

    /// A rig with the start commands taken off the channel.
    fn with(setup: Setup) -> Self {
        let open = setup.open;
        let rig = Self::raw(setup);
        let start = rig.cmds();
        assert_eq!(
            start.len(),
            if open { 6 } else { 4 },
            "the start commands: {start:?}"
        );
        if open {
            let current = start
                .iter()
                .find_map(|cmd| match cmd {
                    UiCmd::CompactCurrent(tag) => Some(*tag),
                    _ => None,
                })
                .expect("initial current request");
            rig.push(UiEvent::CompactCurrent(current, Ok(json!({"clips":[]}))));
            let tag = start
                .into_iter()
                .find_map(|cmd| match cmd {
                    UiCmd::CompactHistory(tag) => Some(tag),
                    _ => None,
                })
                .expect("initial history request");
            rig.push(UiEvent::CompactHistory(tag.clone(), Ok(json!({"owner":tag.params.owner,"generation":tag.params.generation,"rows":[],"cursor":null,"done":true,"bytes_read":0,"candidates":0}))));
        }
        rig
    }

    /// Start closed, status poll out of the way.
    fn closed() -> Self {
        Self::with(Setup::new(false))
    }

    /// Start open, status poll out of the way.
    fn opened() -> Self {
        Self::with(Setup::new(true))
    }

    // -- inputs ------------------------------------------------------------

    /// Queue a bridge event for the next Popup pass.
    fn push(&self, event: UiEvent) {
        self.events.send(event).expect("the app holds the receiver");
    }

    /// Queue tray actions for the next Popup pass.
    fn tray(&self, actions: &[TrayAction]) {
        self.tray
            .lock()
            .expect("tray queue")
            .extend(actions.iter().cloned());
    }

    /// Queue an egui event for the next visible Popup pass.
    fn input(&mut self, event: egui::Event) {
        self.popup.events.push(event);
    }

    fn key(&mut self, key: egui::Key) {
        self.input(key_event(key, true, egui::Modifiers::NONE));
    }

    /// The focus state the Popup window reports from now on.
    fn focus(&mut self, focused: Option<bool>) {
        self.popup.info.focused = focused;
    }

    /// The monitor size the Popup window reports from now on.
    fn monitor(&mut self, size: Option<egui::Vec2>) {
        self.popup.info.monitor_size = size;
    }

    /// The window manager asks the window of `kind` to close, at its next pass.
    fn close_request(&mut self, kind: WindowKind) {
        self.win_mut(kind)
            .info
            .events
            .push(egui::ViewportEvent::Close);
    }

    fn win_mut(&mut self, kind: WindowKind) -> &mut Win {
        match kind {
            WindowKind::Popup => &mut self.popup,
            WindowKind::Settings => &mut self.settings,
        }
    }

    // -- passes ------------------------------------------------------------

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
            assert_eq!(runs, 1, "egui discarded the pass: the rig expects one run");
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
                    .map(|viewport| viewport.commands)
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

    /// A hidden Popup pass.
    fn tick(&mut self) -> Pass {
        self.pass(WindowKind::Popup, false)
    }

    /// A visible Popup pass.
    fn show(&mut self) -> Pass {
        self.pass(WindowKind::Popup, true)
    }

    /// A hidden Settings pass.
    fn tick_settings(&mut self) -> Pass {
        self.pass(WindowKind::Settings, false)
    }

    /// A visible Settings pass.
    fn show_settings(&mut self) -> Pass {
        self.pass(WindowKind::Settings, true)
    }

    // -- observations ------------------------------------------------------

    /// The commands sent since the last call.
    fn cmds(&self) -> Vec<UiCmd> {
        let mut all = std::mem::take(&mut *self.backlog.lock().expect("backlog"));
        all.extend(self.cmds.lock().expect("commands").try_iter());
        all
    }

    fn desired(&self) -> Desired {
        self.app.desired()
    }

    fn is_open(&self) -> bool {
        self.desired().popup_open
    }

    /// The trace lines written so far, without their timestamps.
    fn trace(&self) -> Vec<Value> {
        let bytes = self.trace.lock().expect("trace sink").clone();
        let text = String::from_utf8(bytes).expect("the trace is UTF-8");
        assert!(
            text.is_empty() || text.ends_with('\n'),
            "every trace line ends with a newline: {text:?}"
        );
        text.lines()
            .map(|line| {
                let mut value: Value = serde_json::from_str(line).expect("a trace line is JSON");
                let object = value.as_object_mut().expect("a trace line is an object");
                assert!(
                    object.remove("t_ms").is_some_and(|stamp| stamp.is_u64()),
                    "a trace line has a numeric t_ms: {line}"
                );
                value
            })
            .collect()
    }

    /// The repaint requests since the last call.
    fn requests(&self) -> Vec<Request> {
        std::mem::take(&mut *self.requests.lock().expect("requests"))
    }

    /// Passes that let egui and the app settle, then forget their requests.
    fn warm_up(&mut self) {
        for _ in 0..3 {
            self.tick();
        }
        let _ = self.requests();
    }

    fn foreground_calls(&self) -> usize {
        self.foreground.load(Ordering::SeqCst)
    }

    fn error(&self) -> Option<String> {
        self.app.shell().error().map(str::to_string)
    }

    fn context(&self) -> PasteContext {
        self.app.shell().context()
    }

    fn config_error(&self) -> String {
        self.app.config().lock().expect("settings").error.clone()
    }

    fn answer_compact(&self, commands: &[UiCmd], clips: &[Clip]) {
        let tag = commands
            .iter()
            .find_map(|cmd| match cmd {
                UiCmd::CompactHistory(tag) => Some(tag),
                _ => None,
            })
            .expect("compact request");
        let rows: Vec<_> = clips.iter().map(|clip| json!({"id":clip.id,"pinned":clip.pinned,"last_used_ms":clip.last_used_ms,"preview":clip.preview,"preview_truncated":false,"kind":"text","thumbnail_hash":null})).collect();
        self.push(UiEvent::CompactHistory(tag.clone(), Ok(json!({"owner":tag.params.owner,"generation":tag.params.generation,"rows":rows,"cursor":null,"done":true,"bytes_read":0,"candidates":clips.len()}))));
    }

    /// Give the popup the clips and forget the commands that caused.
    fn load(&mut self, clips: &[Clip]) {
        self.push(history(clips));
        self.tick();
        let _ = self.cmds();
    }
}

// ---------------------------------------------------------------------------
// Builders
// ---------------------------------------------------------------------------

fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

fn clip(n: u128, preview: &str) -> Clip {
    serde_json::from_value(json!({
        "id": id(n).to_string(),
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

fn push_event(source: &str, toggle: bool, target: Option<u64>, gesture: Option<u64>) -> UiEvent {
    UiEvent::ShowPopup(ShowPopupEvent {
        source: source.to_string(),
        toggle,
        target,
        gesture,
        ..ShowPopupEvent::default()
    })
}

fn hotkey(target: u64, gesture: u64) -> UiEvent {
    push_event("hotkey", false, Some(target), Some(gesture))
}

fn request() -> UiEvent {
    push_event("request", false, None, None)
}

fn toggle() -> UiEvent {
    push_event("hotkey", true, Some(1), Some(2))
}

fn key_event(key: egui::Key, pressed: bool, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: Some(key),
        pressed,
        repeat: false,
        modifiers,
    }
}

fn status(paused: bool, version: &str) -> UiEvent {
    UiEvent::Status(json!({"monitoring_paused": paused, "version": version}))
}

/// The commands a closed-to-open transition sends.
fn assert_open_commands(commands: &[UiCmd]) {
    assert!(
        matches!(commands, [UiCmd::CompactHistory(tag), UiCmd::CompactCurrent(_), UiCmd::ListPermanents, UiCmd::PermGroups] | [UiCmd::CompactHistory(tag), UiCmd::ListPermanents, UiCmd::PermGroups] if tag.params.query.is_empty() && tag.params.cursor.is_none() && tag.page == 0),
        "open commands: {commands:?}"
    );
}
fn assert_compact_begin(commands: &[UiCmd]) {
    assert!(
        matches!(commands, [UiCmd::CompactHistory(tag)] | [UiCmd::CompactHistory(tag), UiCmd::CompactCurrent(_)] if tag.params.cursor.is_none() && tag.page == 0),
        "history refresh: {commands:?}"
    );
}

/// The commands that open the Settings window send.
fn settings_commands() -> Vec<UiCmd> {
    vec![
        UiCmd::ConfigGet,
        UiCmd::CapsGet,
        UiCmd::ListPermanents,
        UiCmd::PermGroups,
    ]
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

/// A body of a config reply.
fn config_reply(inner: &Value) -> UiEvent {
    UiEvent::Config(json!({ "config": inner }))
}

/// Fails with the items when there are any.
fn assert_none<T: std::fmt::Debug>(items: &[T], what: &str) {
    assert!(items.is_empty(), "{what}: {items:?}");
}

/// A permanent clip as the daemon lists it.
fn permanent_json(n: u128, group: &str, name: &str) -> Value {
    json!({
        "id": id(n).to_string(), "group": group, "name": name, "hotkey": null,
        "kind": "Paste", "body": "Regards", "js": null,
        "rev": {"wall_ms": 1, "dev": "test-dev", "ctr": 0}
    })
}

/// `true` while `started` is young enough for a "not yet due" check to be valid
/// on a loaded machine: the check is skipped, never failed, when the machine
/// stalled.
fn still_young(started: Instant, limit_ms: u64) -> bool {
    started.elapsed() < Duration::from_millis(limit_ms)
}

// ---------------------------------------------------------------------------
// 1. Start
// ---------------------------------------------------------------------------

// rule 1: new sends exactly the start commands, in order, and nothing else
#[test]
fn rule_start_sends_the_start_commands_in_order() {
    for open in [true, false] {
        let mut rig = Rig::raw(Setup::new(open));
        let start = rig.cmds();
        if open {
            assert!(
                matches!(start.as_slice(), [UiCmd::Status, UiCmd::ConfigGet, UiCmd::CompactHistory(tag), UiCmd::CompactCurrent(_), UiCmd::ListPermanents, UiCmd::PermGroups] if tag.params.cursor.is_none())
            );
        } else {
            assert_eq!(
                start,
                vec![
                    UiCmd::Status,
                    UiCmd::ConfigGet,
                    UiCmd::ListPermanents,
                    UiCmd::PermGroups
                ]
            );
        }
        rig.tick();
        rig.show();
        assert_eq!(
            rig.cmds(),
            vec![],
            "idle passes send nothing (open = {open})"
        );
    }
}

// rule 1: open true or false gives popup_open true or false; Settings closed; no exit
#[test]
fn rule_start_desired_follows_open() {
    for open in [true, false] {
        let rig = Rig::with(Setup::new(open));
        assert_eq!(
            rig.desired(),
            Desired {
                popup_open: open,
                settings_open: false,
                exit: false
            },
            "desired with open = {open}"
        );
        assert_eq!(rig.app.shell().desired().popup_open, open);
    }
}

// rule 1: with open the trace has one Open line from "start", without it none
#[test]
fn rule_start_trace_has_one_open_line_when_open() {
    let mut rig = Rig::opened();
    assert_eq!(rig.trace(), vec![open_line("start", false, false)]);
    rig.tick();
    rig.show();
    assert_eq!(
        rig.trace(),
        vec![open_line("start", false, false)],
        "passes add no line"
    );
    let rig = Rig::closed();
    assert!(rig.trace().is_empty(), "a closed start traces nothing");
}

// rule 1: the start state is empty and fresh for the profile
#[test]
fn rule_start_state_is_empty_and_fresh() {
    let rig = Rig::closed();
    assert_none(rig.app.vm().clips(), "clips");
    assert_eq!(rig.app.vm().query, "");
    assert_eq!(rig.app.vm().selected, 0);
    assert_eq!(rig.app.vm().banner(), "");
    assert_eq!(rig.app.shell().error(), None);
    assert!(!rig.app.shell().offline());
    assert_eq!(rig.context(), PasteContext::default());
    let config = rig.app.config().lock().expect("settings");
    assert_eq!(config.profile, PathBuf::from("rig-profile"));
    assert!(config.doc.is_none());
    assert_eq!(config.tab, "History");
    assert!(config.dark);
    assert!((config.font_scale - 1.0).abs() < f32::EPSILON);
    assert_none(&config.pending_apply, "config.pending_apply");
    assert_eq!(config.error, "");
}

/// Sets a flag when it is dropped.
struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

// rule 1: the hold (the tray icon) lives as long as the app
#[test]
fn rule_start_keeps_the_hold_for_the_life_of_the_app() {
    let dropped = Arc::new(AtomicBool::new(false));
    let mut setup = Setup::new(false);
    setup.hold = Some(Box::new(DropFlag(Arc::clone(&dropped))));
    let mut rig = Rig::with(setup);
    rig.tick();
    rig.show();
    assert!(
        !dropped.load(Ordering::SeqCst),
        "the hold is alive while the app runs"
    );
    drop(rig);
    assert!(dropped.load(Ordering::SeqCst), "the hold goes with the app");
}

// rule 1: with open, the search box takes the keyboard focus at the first render
#[test]
fn rule_start_open_focuses_the_search_box_at_the_first_render() {
    let mut rig = Rig::opened();
    assert!(rig.popup.ctx.memory(egui::Memory::focused).is_none());
    rig.show();
    assert!(
        rig.popup.ctx.memory(egui::Memory::focused).is_some(),
        "the search box has the focus after the first render"
    );
    // typed text goes into it
    rig.input(egui::Event::Text("abc".to_string()));
    rig.show();
    assert_eq!(rig.app.vm().query, "abc");
    assert!(rig.is_open());
    let commands = rig.cmds();
    assert!(
        matches!(commands.as_slice(), [UiCmd::CompactHistory(tag)] if tag.params.query == "abc")
    );
}

// rule 1: a start without open does not take the focus; the open that comes later does
#[test]
fn rule_start_closed_takes_no_focus_until_it_opens() {
    let mut rig = Rig::closed();
    rig.show();
    assert!(rig.popup.ctx.memory(egui::Memory::focused).is_none());
    rig.push(request());
    rig.tick();
    rig.show();
    assert!(
        rig.popup.ctx.memory(egui::Memory::focused).is_some(),
        "the open asked for the focus of the search box"
    );
}

// ---------------------------------------------------------------------------
// 2. Pushes
// ---------------------------------------------------------------------------

/// The ways to open a closed popup.
#[derive(Clone, Copy, Debug)]
enum Opener {
    Hotkey,
    Request,
    TrayToggle,
    TrayOpenPopup,
    PasteFailed,
}

const OPENERS: [Opener; 5] = [
    Opener::Hotkey,
    Opener::Request,
    Opener::TrayToggle,
    Opener::TrayOpenPopup,
    Opener::PasteFailed,
];

impl Opener {
    /// Queue the opening input and run one Popup pass.
    fn open(self, rig: &mut Rig) {
        match self {
            Self::Hotkey => rig.push(hotkey(1, 2)),
            Self::Request => rig.push(request()),
            Self::TrayToggle => rig.tray(&[TrayAction::Toggle]),
            Self::TrayOpenPopup => rig.tray(&[TrayAction::OpenPopup]),
            Self::PasteFailed => rig.push(UiEvent::PasteFailed("no".to_string())),
        }
        rig.tick();
    }

    /// The trace lines of the open.
    fn lines(self) -> Vec<Value> {
        match self {
            Self::Hotkey => vec![open_line("hotkey", true, true)],
            Self::Request | Self::TrayOpenPopup => vec![open_line("request", false, false)],
            Self::TrayToggle => vec![open_line("tray", false, false)],
            Self::PasteFailed => vec![open_line("paste_failed", false, false), error_line()],
        }
    }
}

// rule 2: a hotkey push opens the popup; the context holds its target and gesture
#[test]
fn rule_push_hotkey_opens_with_the_context() {
    let mut rig = Rig::closed();
    rig.push(hotkey(7, 9));
    rig.tick();
    assert!(rig.is_open());
    assert_eq!(
        rig.context(),
        PasteContext {
            target: Some(7),
            gesture: Some(9)
        }
    );
    assert_eq!(rig.trace(), vec![open_line("hotkey", true, true)]);
    assert_open_commands(&rig.cmds());
}

// rule 2: the Open line tells which parts of the context are set
#[test]
fn rule_push_hotkey_trace_flags_follow_the_context() {
    for (target, gesture) in [
        (None, None),
        (Some(1), None),
        (None, Some(2)),
        (Some(1), Some(2)),
    ] {
        let mut rig = Rig::closed();
        rig.push(push_event("hotkey", false, target, gesture));
        rig.tick();
        assert!(rig.is_open(), "target {target:?}, gesture {gesture:?}");
        assert_eq!(rig.context(), PasteContext { target, gesture });
        assert_eq!(
            rig.trace(),
            vec![open_line("hotkey", target.is_some(), gesture.is_some())],
            "target {target:?}, gesture {gesture:?}"
        );
    }
}

// rule 2: a request push opens with its target and never a gesture
#[test]
fn rule_push_request_opens_with_its_target_and_no_gesture() {
    for source in ["request", "cli", "second_launch"] {
        let mut rig = Rig::closed();
        rig.push(push_event(source, false, Some(5), Some(6)));
        rig.tick();
        assert!(rig.is_open(), "source {source}");
        assert_eq!(
            rig.context(),
            PasteContext {
                target: Some(5),
                gesture: None
            },
            "source {source}"
        );
        assert_eq!(
            rig.trace(),
            vec![open_line("request", true, false)],
            "source {source}"
        );
        assert_open_commands(&rig.cmds());
    }
}

// rule 2: a request push without a target opens with an empty context
#[test]
fn rule_push_request_without_target_opens_with_an_empty_context() {
    let mut rig = Rig::closed();
    rig.push(request());
    rig.tick();
    assert!(rig.is_open());
    assert_eq!(rig.context(), PasteContext::default());
    assert_eq!(rig.trace(), vec![open_line("request", false, false)]);
    assert_open_commands(&rig.cmds());
}

// rule 2: a toggle push while open hides the popup
#[test]
fn rule_push_toggle_while_open_hides() {
    let mut rig = Rig::opened();
    rig.push(toggle());
    rig.tick();
    assert!(!rig.is_open());
    assert_eq!(
        rig.trace(),
        vec![open_line("start", false, false), hide_line("toggle")]
    );
    assert_eq!(rig.cmds(), vec![]);
    assert_eq!(rig.context(), PasteContext::default());
}

// rule 2: a toggle push while closed does nothing
#[test]
fn rule_push_toggle_while_closed_does_nothing() {
    let mut rig = Rig::closed();
    rig.push(toggle());
    rig.tick();
    assert!(!rig.is_open());
    assert_none(&rig.trace(), "rig.trace()");
    assert_eq!(rig.cmds(), vec![]);
    assert_eq!(
        rig.context(),
        PasteContext::default(),
        "its gesture is never stored"
    );
}

// rule 2: a hotkey push while open replaces the context and opens nothing again
#[test]
fn rule_push_hotkey_while_open_replaces_the_context() {
    let mut rig = Rig::closed();
    rig.push(hotkey(1, 2));
    rig.tick();
    let _ = rig.cmds();
    rig.push(hotkey(3, 4));
    rig.tick();
    assert!(rig.is_open());
    assert_eq!(
        rig.context(),
        PasteContext {
            target: Some(3),
            gesture: Some(4)
        }
    );
    assert_eq!(
        rig.trace(),
        vec![open_line("hotkey", true, true)],
        "open to open is no transition"
    );
    assert!(
        rig.cmds().is_empty(),
        "the old in-flight request is coalesced"
    );
}

// rule 2: a request push while open keeps the context and sends nothing
#[test]
fn rule_push_request_while_open_keeps_the_context() {
    let mut rig = Rig::closed();
    rig.push(hotkey(1, 2));
    rig.tick();
    let _ = rig.cmds();
    rig.push(request());
    rig.tick();
    assert!(rig.is_open());
    assert_eq!(
        rig.context(),
        PasteContext {
            target: Some(1),
            gesture: Some(2)
        }
    );
    assert_eq!(rig.trace(), vec![open_line("hotkey", true, true)]);
    assert_eq!(rig.cmds(), vec![]);
}

// rule 2: every closed-to-open transition sends ListHistory, ListPermanents,
// PermGroups, in this order, after the shell's own effects
#[test]
fn rule_push_every_open_sends_history_then_permanents_then_groups() {
    for opener in OPENERS {
        let mut rig = Rig::closed();
        opener.open(&mut rig);
        assert!(rig.is_open(), "{opener:?}");
        assert_open_commands(&rig.cmds());
        assert_eq!(rig.trace(), opener.lines(), "{opener:?}");
    }
}

// rule 2: an open that finds the popup open sends no permanents refresh
#[test]
fn rule_push_open_while_open_sends_no_permanents_refresh() {
    for opener in [Opener::Request, Opener::TrayOpenPopup, Opener::PasteFailed] {
        let mut rig = Rig::opened();
        opener.open(&mut rig);
        assert!(rig.is_open(), "{opener:?}");
        let cmds = rig.cmds();
        assert!(
            !cmds.contains(&UiCmd::ListPermanents) && !cmds.contains(&UiCmd::PermGroups),
            "{opener:?} sent {cmds:?}"
        );
    }
}

// rule 2: after a hide, the next open sends the three commands again
#[test]
fn rule_push_reopen_sends_the_commands_again() {
    let mut rig = Rig::closed();
    for round in 0..3 {
        rig.push(request());
        rig.tick();
        let commands = rig.cmds();
        assert_open_commands(&commands);
        rig.answer_compact(&commands, &[]);
        rig.tick();
        rig.push(toggle());
        rig.tick();
        assert!(!rig.is_open(), "hide number {round}");
        assert_eq!(rig.cmds(), vec![], "a hide sends nothing, round {round}");
    }
}

// effects: Hide and Raise ask the window for nothing via viewport commands; the runner follows desired() and its activation hook
#[test]
fn rule_push_hide_and_raise_send_no_viewport_commands() {
    let mut rig = Rig::closed();
    let mut commands = Vec::new();
    rig.push(request());
    commands.extend(rig.tick().commands);
    rig.push(hotkey(1, 2));
    commands.extend(rig.tick().commands);
    rig.push(request());
    commands.extend(rig.tick().commands);
    rig.push(toggle());
    commands.extend(rig.tick().commands);
    rig.push(toggle());
    commands.extend(rig.tick().commands);
    assert!(commands.is_empty(), "viewport commands: {commands:?}");
}

// ---------------------------------------------------------------------------
// 3. The event cap
// ---------------------------------------------------------------------------

// rule 3: the cap is 32 events per pass
#[test]
fn rule_cap_is_thirty_two() {
    assert_eq!(DRAIN_CAP, 32);
}

// rule 3: 40 queued events: the first pass handles 32, the next the rest
#[test]
fn rule_cap_forty_events_take_two_passes() {
    let mut rig = Rig::closed();
    for n in 0..40 {
        rig.push(UiEvent::Error(format!("event {n}")));
    }
    rig.tick();
    assert_eq!(rig.error().as_deref(), Some("event 31"));
    rig.tick();
    assert_eq!(rig.error().as_deref(), Some("event 39"));
    rig.tick();
    assert_eq!(rig.error().as_deref(), Some("event 39"));
}

// rule 3: the cap is per pass, whatever the number of events
#[test]
fn rule_cap_table() {
    for queued in [0_usize, 1, 31, 32, 33, 64, 65] {
        let mut rig = Rig::closed();
        for n in 0..queued {
            rig.push(UiEvent::Error(format!("event {n}")));
        }
        let mut processed = 0;
        loop {
            rig.tick();
            processed = (processed + DRAIN_CAP).min(queued);
            let expected = (processed > 0).then(|| format!("event {}", processed - 1));
            assert_eq!(
                rig.error(),
                expected,
                "{queued} queued, {processed} processed"
            );
            if processed == queued {
                break;
            }
        }
    }
}

// rule 3: only Popup passes drain the bridge events
#[test]
fn rule_cap_settings_passes_drain_nothing() {
    let mut rig = Rig::closed();
    rig.push(UiEvent::Error("queued".into()));
    rig.tick_settings();
    rig.show_settings();
    assert_eq!(rig.cmds(), vec![], "a Settings pass leaves the queue alone");
    assert_eq!(rig.error(), None);
    rig.tick();
    assert_eq!(rig.error().as_deref(), Some("queued"));
}

// ---------------------------------------------------------------------------
// 4. History, status and the replies that fill the state
// ---------------------------------------------------------------------------

fn ids(rig: &Rig) -> Vec<Uuid> {
    rig.app.vm().clips().iter().map(|clip| clip.id).collect()
}

// rule 4: a well-formed new list reaches the popup state; the current slot is the first clip
#[test]
fn rule_history_new_list_reaches_the_popup_state() {
    let mut rig = Rig::closed();
    let (a, b) = (clip(1, "alpha"), clip(2, "beta"));
    rig.push(history(&[a.clone(), b.clone()]));
    rig.tick();
    assert_eq!(ids(&rig), vec![a.id, b.id]);
    assert_eq!(rig.app.vm().current().map(|clip| clip.id), Some(a.id));
    assert!(!rig.app.shell().offline());
    assert_eq!(rig.app.vm().banner(), "");
    rig.push(history(std::slice::from_ref(&b)));
    rig.tick();
    assert_eq!(
        ids(&rig),
        vec![b.id],
        "a different list replaces the shown one"
    );
    assert_eq!(rig.app.vm().current().map(|clip| clip.id), Some(b.id));
}

// rule 4: the same list again changes nothing (the current slot stays) and does not mark offline
#[test]
fn rule_history_same_list_changes_nothing() {
    let mut rig = Rig::closed();
    let (a, b, c) = (clip(1, "alpha"), clip(2, "beta"), clip(3, "gamma"));
    rig.load(&[a.clone(), b.clone()]);
    rig.push(UiEvent::HistoryClip(json!({ "clip": c })));
    rig.tick();
    assert_eq!(rig.app.vm().current().map(|clip| clip.id), Some(c.id));
    rig.push(history(&[a.clone(), b.clone()]));
    rig.tick();
    assert_eq!(ids(&rig), vec![a.id, b.id]);
    assert_eq!(
        rig.app.vm().current().map(|clip| clip.id),
        Some(c.id),
        "an unchanged list leaves the current slot alone"
    );
    assert!(!rig.app.shell().offline());
    assert_eq!(rig.app.vm().banner(), "");
}

// rule 4: an empty list is a well-formed reply: online
#[test]
fn rule_history_empty_list_is_online() {
    let mut rig = Rig::closed();
    rig.push(UiEvent::Offline("link down".to_string()));
    rig.tick();
    assert_eq!(rig.app.vm().banner(), strings::text("popup.offline"));
    rig.push(UiEvent::History(json!({"clips": []})));
    rig.tick();
    assert!(!rig.app.shell().offline());
    assert_eq!(rig.app.vm().banner(), "");
    assert_eq!(rig.error(), None);
    rig.load(&[clip(1, "alpha")]);
    rig.push(UiEvent::History(json!({"clips": []})));
    rig.tick();
    assert!(ids(&rig).is_empty(), "an empty list empties the shown one");
    assert!(!rig.app.shell().offline());
}

// rule 4: a malformed body marks offline and sets the error line to the app.error text
#[test]
fn rule_history_malformed_marks_offline_and_sets_the_error() {
    let bodies = [
        json!({}),
        json!({"clips": "none"}),
        json!({"clips": [5]}),
        json!({"clips": null}),
        Value::Null,
    ];
    for body in bodies {
        let mut rig = Rig::closed();
        rig.load(&[clip(1, "alpha")]);
        rig.push(UiEvent::History(body.clone()));
        rig.tick();
        assert!(rig.app.shell().offline(), "body {body}");
        assert_eq!(
            rig.app.vm().banner(),
            strings::text("popup.offline"),
            "body {body}"
        );
        assert_eq!(
            rig.error().as_deref(),
            Some(strings::text("app.error")),
            "body {body}"
        );
        assert_eq!(
            ids(&rig).len(),
            1,
            "a malformed reply keeps the list, body {body}"
        );
        rig.push(history(&[clip(1, "alpha")]));
        rig.tick();
        assert!(
            !rig.app.shell().offline(),
            "a good reply brings it back, body {body}"
        );
    }
}

// rule 4: an Offline event shows the offline banner; a later Status or History clears it
#[test]
fn rule_offline_event_shows_the_banner_and_a_reply_clears_it() {
    let clearing = [status(false, "1"), history(&[clip(1, "alpha")])];
    for reply in clearing {
        let mut rig = Rig::closed();
        rig.push(UiEvent::Offline("link down".to_string()));
        rig.tick();
        assert!(rig.app.shell().offline());
        assert_eq!(rig.app.vm().banner(), strings::text("popup.offline"));
        assert_eq!(rig.error(), None, "the error line is unchanged by Offline");
        rig.push(reply.clone());
        rig.tick();
        assert!(!rig.app.shell().offline(), "{reply:?}");
        assert_eq!(rig.app.vm().banner(), "", "{reply:?}");
    }
}

// rule 4: Status updates the paused flag, the status line and the version text
#[test]
fn rule_status_updates_paused_status_line_and_version() {
    let mut rig = Rig::closed();
    rig.push(status(true, "1.2.3"));
    rig.tick();
    assert_eq!(rig.app.vm().status_line(), strings::text("popup.paused"));
    assert_eq!(
        rig.app.config().lock().expect("settings").version_text,
        "1.2.3"
    );
    rig.push(status(false, "1.2.4"));
    rig.tick();
    assert_eq!(
        rig.app.vm().status_line(),
        strings::text("popup.count").replace("{count}", "0")
    );
    assert_eq!(
        rig.app.config().lock().expect("settings").version_text,
        "1.2.4"
    );
}

// rule 4: MonitorPaused sets the paused flag shown by the status line
#[test]
fn rule_monitor_paused_updates_the_status_line() {
    let mut rig = Rig::closed();
    rig.push(UiEvent::MonitorPaused(true));
    rig.tick();
    assert_eq!(rig.app.vm().status_line(), strings::text("popup.paused"));
    rig.push(UiEvent::MonitorPaused(false));
    rig.tick();
    assert_eq!(
        rig.app.vm().status_line(),
        strings::text("popup.count").replace("{count}", "0")
    );
}

// rule 4: Deleted removes the clip, Pinned sets the flag
#[test]
fn rule_deleted_and_pinned_update_the_clips() {
    let mut rig = Rig::closed();
    let (a, b, c) = (clip(1, "alpha"), clip(2, "beta"), clip(3, "gamma"));
    rig.load(&[a.clone(), b.clone(), c.clone()]);
    rig.push(UiEvent::Pinned(b.id, true));
    rig.tick();
    assert!(rig.app.vm().is_pinned(b.id));
    assert!(!rig.app.vm().is_pinned(a.id));
    rig.push(UiEvent::Pinned(b.id, false));
    rig.tick();
    assert!(!rig.app.vm().is_pinned(b.id));
    rig.push(UiEvent::Deleted(b.id));
    rig.tick();
    assert_eq!(ids(&rig), vec![a.id, c.id]);
    assert_eq!(rig.cmds(), vec![]);
}

// rule 4: a HistoryClip reply sets the current slot; a body without a clip changes nothing
#[test]
fn rule_history_clip_sets_the_current_slot() {
    let mut rig = Rig::closed();
    let (a, b) = (clip(1, "alpha"), clip(2, "beta"));
    rig.load(std::slice::from_ref(&a));
    rig.push(UiEvent::HistoryClip(json!({ "clip": b })));
    rig.tick();
    assert_eq!(rig.app.vm().current().map(|clip| clip.id), Some(b.id));
    rig.push(UiEvent::HistoryClip(json!({"clip": "nope"})));
    rig.push(UiEvent::HistoryClip(json!({})));
    rig.tick();
    assert_eq!(rig.app.vm().current().map(|clip| clip.id), Some(b.id));
}

// rule 4: PasteDone does nothing: the popup hid at the pick
#[test]
fn rule_paste_done_does_nothing() {
    let mut rig = Rig::opened();
    rig.push(UiEvent::PasteDone(id(1)));
    rig.tick();
    assert!(rig.is_open(), "a PasteDone reply never hides the popup");
    assert_eq!(rig.cmds(), vec![]);
    assert_eq!(rig.error(), None);
    assert_eq!(rig.trace(), vec![open_line("start", false, false)]);
    let mut rig = Rig::closed();
    rig.push(UiEvent::PasteDone(id(1)));
    rig.tick();
    assert!(!rig.is_open(), "nor does it show it");
    assert_none(&rig.trace(), "rig.trace()");
}

// rule 4: the replies that fill the Settings state
#[test]
fn rule_replies_fill_the_settings_state() {
    let mut rig = Rig::closed();
    rig.tray(&[TrayAction::OpenConfig]);
    rig.tick();
    assert_eq!(
        rig.cmds(),
        settings_commands(),
        "opening Settings queues four commands"
    );
    rig.push(UiEvent::Caps(json!({"clipboard": "ok"})));
    rig.push(UiEvent::LogsTail {
        source: "audit".into(),
        lines: vec!["one".into(), "two".into()],
        sink_error: None,
    });
    rig.push(UiEvent::ConfigError("bad config".to_string()));
    rig.push(UiEvent::PermGroups(
        json!({"groups": [{"name": "Work", "system": false, "count": 1}]}),
    ));
    rig.push(UiEvent::Permanents(
        json!({"permanents": [permanent_json(7, "Work", "Sig")]}),
    ));
    rig.push(UiEvent::Purged(3));
    rig.tick();
    let config = rig.app.config().lock().expect("settings");
    assert_eq!(config.caps_text, json!({"clipboard": "ok"}).to_string());
    assert_eq!(config.log_lines, vec!["one".to_string(), "two".to_string()]);
    assert_eq!(config.error, "bad config");
    assert_eq!(config.perm_vm.group_names(), vec!["Work".to_string()]);
    assert_eq!(config.perm_vm.permanents.len(), 1);
    assert_eq!(config.perm_vm.permanents[0].id, id(7));
    drop(config);
    assert_eq!(rig.cmds(), vec![], "none of these replies asks for more");
    assert_eq!(rig.error(), None, "the popup error line is not touched");
}

// rule 4: backup, restore and the permanent bookkeeping events
#[test]
fn rule_bookkeeping_events_update_state_and_ask_for_refreshes() {
    let mut rig = Rig::closed();
    rig.tray(&[TrayAction::OpenConfig]);
    rig.tick();
    let _ = rig.cmds();
    rig.push(UiEvent::BackupDone("/tmp/b.zip".to_string()));
    rig.tick();
    assert_eq!(
        rig.config_error(),
        format!("{} /tmp/b.zip", strings::text("config.backup_saved"))
    );
    assert_eq!(rig.cmds(), vec![]);
    rig.push(UiEvent::RestoreDone { warning: None });
    rig.tick();
    assert_eq!(rig.config_error(), strings::text("config.restore_done"));
    assert_eq!(rig.cmds(), vec![UiCmd::ConfigGetAfterRestore]);
    rig.app.config().lock().expect("settings").perm_group_new = "Fresh".to_string();
    rig.push(UiEvent::PermGroupsChanged);
    rig.tick();
    assert_eq!(
        rig.app.config().lock().expect("settings").perm_group_new,
        ""
    );
    assert_eq!(rig.cmds(), vec![UiCmd::ListPermanents, UiCmd::PermGroups]);
    rig.app
        .config()
        .lock()
        .expect("settings")
        .perm_vm
        .begin_new();
    rig.push(UiEvent::PermanentSaved);
    rig.tick();
    assert!(rig
        .app
        .config()
        .lock()
        .expect("settings")
        .perm_vm
        .draft
        .is_none());
    assert_eq!(rig.cmds(), vec![UiCmd::ListPermanents, UiCmd::PermGroups]);
    rig.push(UiEvent::Permanents(json!({"permanents": [
        permanent_json(9, "Work", "Sig"),
        permanent_json(10, "Work", "Address"),
    ]})));
    rig.tick();
    rig.app
        .config()
        .lock()
        .expect("settings")
        .perm_confirm_delete = Some(id(9));
    rig.push(UiEvent::PermanentDeleted(id(9)));
    rig.tick();
    {
        let config = rig.app.config().lock().expect("settings");
        assert!(config.perm_confirm_delete.is_none());
        let left: Vec<Uuid> = config
            .perm_vm
            .permanents
            .iter()
            .map(|clip| clip.id)
            .collect();
        assert_eq!(left, vec![id(10)], "the confirmed delete removed the clip");
    }
    assert_eq!(rig.cmds(), vec![UiCmd::PermGroups]);
}

// rule 4: a confirmed config key leaves the pending list and the staged edits
#[test]
fn rule_config_applied_settles_the_staged_edit() {
    let mut rig = Rig::closed();
    rig.push(config_reply(&json!({"history": {"max_items": 10}})));
    rig.tick();
    {
        let mut config = rig.app.config().lock().expect("settings");
        let doc = config.doc.as_mut().expect("the document");
        doc.stage("history.max_items", json!(20))
            .expect("a valid value");
        assert!(doc.is_dirty("history.max_items"));
        config.pending_apply = vec!["history.max_items".to_string(), "other".to_string()];
        config
            .edits
            .insert("history.max_items".to_string(), "20".to_string());
    }
    rig.push(UiEvent::ConfigApplied("history.max_items".to_string()));
    rig.tick();
    let config = rig.app.config().lock().expect("settings");
    assert_eq!(config.pending_apply, vec!["other".to_string()]);
    assert!(!config.edits.contains_key("history.max_items"));
    let doc = config.doc.as_ref().expect("the document");
    assert!(!doc.is_dirty("history.max_items"));
    assert_eq!(doc.get("history.max_items"), Some(&json!(20)));
}

// ---------------------------------------------------------------------------
// 5. Keys on a visible pass while open
// ---------------------------------------------------------------------------

fn paste_to(cmd: &UiCmd) -> (Uuid, Option<u64>, i64, Option<u64>) {
    let UiCmd::PasteClipTo(clip, target, picked_ms, gesture) = cmd else {
        panic!("not a PasteClipTo: {cmd:?}");
    };
    (*clip, *target, *picked_ms, *gesture)
}

// rule 5: Escape hides the popup (Hide with the dismiss reason)
#[test]
fn rule_keys_escape_hides_the_popup() {
    let mut rig = Rig::opened();
    rig.key(egui::Key::Escape);
    rig.show();
    assert!(!rig.is_open());
    assert_eq!(
        rig.trace(),
        vec![open_line("start", false, false), hide_line("dismiss")]
    );
    assert_eq!(rig.cmds(), vec![]);
    assert_eq!(
        rig.foreground_calls(),
        0,
        "a dismiss grants no foreground right"
    );
}

// rule 5: a closed popup takes no key
#[test]
fn rule_keys_while_closed_do_nothing() {
    let mut rig = Rig::closed();
    rig.load(&[clip(1, "alpha")]);
    rig.key(egui::Key::Escape);
    rig.key(egui::Key::Enter);
    rig.key(egui::Key::F5);
    rig.show();
    assert_none(&rig.trace(), "rig.trace()");
    assert_eq!(rig.cmds(), vec![]);
    assert_eq!(rig.foreground_calls(), 0);
}

// rule 5: a key press clears a shown error; a release does not
#[test]
fn rule_keys_a_press_clears_the_error() {
    let mut rig = Rig::opened();
    rig.push(UiEvent::PasteFailed("nope".to_string()));
    rig.show();
    assert_eq!(rig.error().as_deref(), Some("nope"));
    rig.input(key_event(egui::Key::A, false, egui::Modifiers::NONE));
    rig.show();
    assert_eq!(
        rig.error().as_deref(),
        Some("nope"),
        "a release keeps the error"
    );
    rig.key(egui::Key::A);
    rig.show();
    assert_eq!(rig.error(), None, "a press clears it");
    assert!(rig.is_open());
}

// rule 5: F5 sends ListHistory
#[test]
fn rule_keys_f5_asks_for_a_fresh_history() {
    let mut rig = Rig::opened();
    rig.key(egui::Key::F5);
    rig.show();
    assert_compact_begin(&rig.cmds());
    assert!(rig.is_open());
}

// rule 5: Enter with a hotkey target sends PasteClipTo in the same pass, allows the
// foreground once and traces Hide{pick} then Paste{target: true}
#[test]
fn rule_keys_enter_with_a_target_pastes_in_the_same_pass() {
    let mut rig = Rig::closed();
    rig.push(hotkey(77, 99));
    rig.push(history(&[clip(1, "alpha"), clip(2, "beta")]));
    rig.tick();
    let _ = rig.cmds();
    rig.key(egui::Key::Enter);
    let before = now_ms();
    rig.show();
    let after = now_ms();
    let cmds = rig.cmds();
    assert_eq!(cmds.len(), 1, "commands: {cmds:?}");
    let (picked, target, picked_ms, gesture) = paste_to(&cmds[0]);
    assert_eq!((picked, target, gesture), (id(1), Some(77), Some(99)));
    assert!(
        (before..=after).contains(&picked_ms),
        "picked_ms {picked_ms} is the time of the pick ({before}..={after})"
    );
    assert_eq!(
        rig.foreground_calls(),
        1,
        "allow_foreground ran exactly once"
    );
    assert!(!rig.is_open());
    assert_eq!(rig.context(), PasteContext::default());
    assert_eq!(
        rig.trace(),
        vec![
            open_line("hotkey", true, true),
            hide_line("pick"),
            paste_line(true)
        ]
    );
}

// effects in order: the foreground right is granted before the paste command is sent
#[test]
fn rule_keys_enter_allows_the_foreground_before_the_paste_goes_out() {
    let mut rig = Rig::closed();
    rig.push(hotkey(77, 99));
    rig.push(history(&[clip(1, "alpha")]));
    rig.tick();
    let _ = rig.cmds();
    rig.key(egui::Key::Enter);
    rig.show();
    let seen = rig.at_hook.lock().expect("hook log").clone();
    assert_eq!(seen.len(), 1, "the hook ran once");
    assert_eq!(
        seen[0],
        vec![],
        "no command was on the channel when the hook ran"
    );
    assert_eq!(rig.cmds().len(), 1, "and the paste went out after it");
}

// rule 5: Enter picks the selected row
#[test]
fn rule_keys_enter_picks_the_selected_clip() {
    let mut rig = Rig::closed();
    rig.push(hotkey(5, 6));
    rig.push(history(&[
        clip(1, "alpha"),
        clip(2, "beta"),
        clip(3, "gamma"),
    ]));
    rig.tick();
    let _ = rig.cmds();
    // End moves the selection and, unlike the arrow keys, not the egui focus
    rig.key(egui::Key::End);
    rig.show();
    assert_eq!(rig.app.vm().selected, 2);
    assert_eq!(rig.cmds(), vec![]);
    rig.key(egui::Key::Enter);
    rig.show();
    let cmds = rig.cmds();
    assert_eq!(cmds.len(), 1, "commands: {cmds:?}");
    assert_eq!(paste_to(&cmds[0]).0, id(3));
}

// rule 5: Enter on an empty list picks nothing
#[test]
fn rule_keys_enter_on_an_empty_list_does_nothing() {
    let mut rig = Rig::opened();
    rig.key(egui::Key::Enter);
    rig.show();
    assert!(rig.is_open());
    assert_eq!(rig.cmds(), vec![]);
    assert_eq!(rig.foreground_calls(), 0);
    assert_eq!(rig.trace(), vec![open_line("start", false, false)]);
}

// rule 5: Enter without a target sends nothing at the pick; the paste goes out at the
// first pass at least 150 ms after the pass that confirmed the hide
#[test]
fn rule_keys_enter_without_a_target_waits_for_the_hide_and_the_delay() {
    let mut rig = Rig::opened();
    rig.load(&[clip(1, "alpha"), clip(2, "beta")]);
    rig.key(egui::Key::Enter);
    let before = now_ms();
    rig.show();
    let after = now_ms();
    assert!(!rig.is_open());
    assert_eq!(rig.cmds(), vec![], "nothing goes out at the pick");
    assert_eq!(
        rig.foreground_calls(),
        1,
        "allow_foreground ran exactly once"
    );
    assert_eq!(
        rig.trace(),
        vec![open_line("start", false, false), hide_line("pick")]
    );

    let confirmed = Instant::now();
    rig.tick();
    if still_young(confirmed, 140) {
        assert_eq!(
            rig.cmds(),
            vec![],
            "not sent at the pass that confirms the hide"
        );
    }
    thread::sleep(Duration::from_millis(100));
    rig.tick();
    if still_young(confirmed, 140) {
        assert_eq!(
            rig.cmds(),
            vec![],
            "not due yet, 100 ms after the confirmation"
        );
    }
    thread::sleep(Duration::from_millis(60));
    let confirmed_ago = confirmed.elapsed();
    assert!(confirmed_ago >= Duration::from_millis(150));
    rig.tick();
    let cmds = rig.cmds();
    assert!(
        cmds.len() <= 1,
        "the commands the earlier passes did not drain: {cmds:?}"
    );
    let (picked, target, picked_ms, gesture) = paste_to(cmds.last().expect("the paste is due"));
    assert_eq!((picked, target, gesture), (id(1), None, None));
    assert!(
        (before..=after).contains(&picked_ms),
        "picked_ms {picked_ms} is the time of the pick ({before}..={after})"
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
    assert_eq!(rig.cmds(), vec![], "sent once");
}

// rule 5: typing into the search box changes the query and the rows, not the popup
#[test]
fn rule_keys_typing_filters_without_hiding() {
    let mut rig = Rig::opened();
    rig.load(&[clip(1, "alpha"), clip(2, "beta"), clip(3, "gamma")]);
    rig.show();
    rig.input(egui::Event::Text("et".to_string()));
    rig.show();
    assert_eq!(rig.app.vm().query, "et");
    let commands = rig.cmds();
    assert_compact_begin(&commands);
    rig.answer_compact(&commands, &[clip(2, "beta")]);
    rig.show();
    assert_eq!(rig.app.vm().visible(), vec![0]);
    assert!(rig.is_open());
    assert_eq!(rig.cmds(), vec![]);
    assert_eq!(rig.trace(), vec![open_line("start", false, false)]);
}

// rule 5: the arrow, Home and End keys move the selection without hiding
#[test]
fn rule_keys_arrows_move_the_selection_without_hiding() {
    let mut rig = Rig::opened();
    rig.load(&[clip(1, "alpha"), clip(2, "beta"), clip(3, "gamma")]);
    let steps = [
        (egui::Key::ArrowDown, 1),
        (egui::Key::ArrowDown, 2),
        (egui::Key::ArrowDown, 2),
        (egui::Key::ArrowUp, 1),
        (egui::Key::Home, 0),
        (egui::Key::End, 2),
    ];
    for (key, selected) in steps {
        rig.key(key);
        rig.show();
        assert_eq!(rig.app.vm().selected, selected, "after {key:?}");
        assert!(rig.is_open(), "after {key:?}");
    }
    assert_eq!(rig.cmds(), vec![]);
    assert_eq!(rig.trace(), vec![open_line("start", false, false)]);
}

// rule 5: an open resets the search box and the selection (ClearSearch, SelectNewest)
#[test]
fn rule_keys_an_open_clears_the_search_and_selects_the_first_row() {
    let mut rig = Rig::opened();
    rig.load(&[clip(1, "alpha"), clip(2, "beta"), clip(3, "gamma")]);
    rig.show();
    rig.input(egui::Event::Text("a".to_string()));
    rig.show();
    let commands = rig.cmds();
    rig.answer_compact(
        &commands,
        &[clip(1, "alpha"), clip(2, "beta"), clip(3, "gamma")],
    );
    rig.show();
    rig.key(egui::Key::ArrowDown);
    rig.show();
    assert_eq!(rig.app.vm().query, "a");
    assert_eq!(rig.app.vm().selected, 1);
    rig.push(hotkey(1, 2));
    rig.tick();
    assert_eq!(rig.app.vm().query, "", "an open empties the search box");
    assert_eq!(rig.app.vm().selected, 0, "an open selects the first row");
}

// ---------------------------------------------------------------------------
// 6. The hide is confirmed one pass later
// ---------------------------------------------------------------------------

// rule 6: Hidden reaches the shell at the pass after the hide; the 150 ms of a pending
// paste count from that pass, not from the pass that hid
#[test]
fn rule_hidden_confirmation_counts_from_the_next_pass() {
    let mut rig = Rig::opened();
    rig.load(&[clip(1, "alpha")]);
    rig.key(egui::Key::Enter);
    rig.show();
    assert!(!rig.is_open());
    assert_eq!(rig.cmds(), vec![]);
    thread::sleep(Duration::from_millis(200));
    let confirming = Instant::now();
    rig.tick();
    let mut sent = rig.cmds();
    if still_young(confirming, 140) {
        assert_eq!(
            sent,
            vec![],
            "200 ms after the pick, the pass that confirms the hide must not paste yet"
        );
    }
    thread::sleep(Duration::from_millis(160));
    rig.tick();
    sent.extend(rig.cmds());
    assert_eq!(sent.len(), 1, "commands: {sent:?}");
    assert_eq!(paste_to(&sent[0]).0, id(1));
}

// rule 6: the confirmation is not repeated and does not postpone the paste
#[test]
fn rule_hidden_confirmation_is_given_once() {
    let mut rig = Rig::opened();
    rig.load(&[clip(1, "alpha")]);
    rig.key(egui::Key::Enter);
    rig.show();
    let confirming = Instant::now();
    rig.tick();
    let mut sent = Vec::new();
    while confirming.elapsed() < Duration::from_millis(400) && sent.is_empty() {
        thread::sleep(Duration::from_millis(20));
        rig.tick();
        sent = rig.cmds();
    }
    assert_eq!(
        sent.len(),
        1,
        "the paste goes out while passes keep coming: later passes do not postpone it"
    );
}

// ---------------------------------------------------------------------------
// 7. A closed popup paints nothing
// ---------------------------------------------------------------------------

// rule 7: ui(Popup) while the shell is closed paints no text, not even stale rows
#[test]
fn rule_paint_closed_popup_paints_no_text() {
    let mut rig = Rig::closed();
    rig.load(&[clip(1, "alpha row"), clip(2, "beta row")]);
    let pass = rig.show();
    assert!(
        pass.texts.is_empty(),
        "painted while closed: {:?}",
        pass.texts
    );
}

// rule 7: while open the previews of the clips appear as text
#[test]
fn rule_paint_open_popup_shows_the_previews() {
    let mut rig = Rig::opened();
    rig.load(&[clip(1, "alpha row"), clip(2, "beta row")]);
    let pass = rig.show();
    assert!(pass.painted("alpha row"), "painted: {:?}", pass.texts);
    assert!(pass.painted("beta row"), "painted: {:?}", pass.texts);
}

// rule 7: right after a hide, before it is confirmed, the popup already paints nothing
#[test]
fn rule_paint_hide_empties_the_panel_at_once() {
    let mut rig = Rig::opened();
    rig.load(&[clip(1, "alpha row")]);
    assert!(rig.show().painted("alpha row"));
    rig.push(toggle());
    let pass = rig.show();
    assert!(!rig.is_open());
    assert!(
        pass.texts.is_empty(),
        "painted after the hide: {:?}",
        pass.texts
    );
}

// rule 7: a re-open paints the rows again
#[test]
fn rule_paint_reopen_paints_the_rows_again() {
    let mut rig = Rig::opened();
    rig.load(&[clip(1, "alpha row")]);
    rig.push(toggle());
    rig.show();
    rig.push(request());
    rig.show();
    assert!(rig.is_open());
    let commands = rig.cmds();
    rig.answer_compact(&commands, &[clip(1, "alpha row")]);
    let pass = rig.show();
    assert!(pass.painted("alpha row"), "painted: {:?}", pass.texts);
}

// The history list ends with "End of history", below the page buttons and the last row.
#[test]
fn compact_history_end_follows_the_last_row() {
    let mut rig = Rig::raw(Setup::new(true));
    let commands = rig.cmds();
    rig.answer_compact(&commands, &[clip(1, "alpha row"), clip(2, "beta row")]);
    let pass = rig.show();
    let bound = |label: &str| {
        pass.bounds
            .iter()
            .find(|(text, _, _)| text == label)
            .unwrap_or_else(|| panic!("{label:?} not painted: {:?}", pass.texts))
            .1
    };
    let end = bound(strings::text("popup.history_end"));
    assert!(end.top() >= bound("beta row").bottom());
    assert!(bound(strings::text("popup.first_page")).bottom() <= bound("alpha row").top());
}

// Settings: ui(Settings) paints the Settings body
#[test]
fn rule_paint_settings_window_shows_the_settings_body() {
    let mut rig = Rig::closed();
    let pass = rig.show_settings();
    assert!(
        pass.painted(strings::text("config.tab_history")),
        "painted: {:?}",
        pass.texts
    );
    assert!(pass.painted(strings::text("config.tab_about")));
    let popup = rig.show();
    assert!(
        !popup.painted(strings::text("config.tab_history")),
        "the popup does not paint the Settings tabs"
    );
}

// ---------------------------------------------------------------------------
// 8. Focus loss, as an edge on the popup window's focus
// ---------------------------------------------------------------------------

/// One focus cycle: the window gets the focus, then loses it.
fn focus_cycle(rig: &mut Rig) {
    rig.focus(Some(true));
    rig.tick();
    rig.focus(Some(false));
    rig.tick();
}

// rule 8: focused, then not focused: the popup hides with the focus_lost reason
#[test]
fn rule_focus_loss_after_focus_hides() {
    let mut rig = Rig::opened();
    rig.focus(Some(true));
    rig.tick();
    assert!(rig.is_open(), "focus alone hides nothing");
    rig.focus(Some(false));
    rig.tick();
    assert!(!rig.is_open());
    assert_eq!(
        rig.trace(),
        vec![open_line("start", false, false), hide_line("focus_lost")]
    );
    assert_eq!(rig.cmds(), vec![]);
}

// rule 8: the edge is seen on visible passes as well
#[test]
fn rule_focus_loss_is_seen_on_visible_passes() {
    let mut rig = Rig::opened();
    rig.focus(Some(true));
    rig.show();
    assert!(rig.is_open());
    rig.focus(Some(false));
    rig.show();
    assert!(!rig.is_open());
    assert_eq!(rig.trace().last(), Some(&hide_line("focus_lost")));
}

// rule 8: a popup that was never seen focused does not hide
#[test]
fn rule_focus_never_focused_does_not_hide() {
    let sequences = [
        vec![None, None, None],
        vec![Some(false), Some(false), Some(false)],
        vec![None, Some(false), None, Some(false)],
    ];
    for states in sequences {
        let mut rig = Rig::opened();
        for state in &states {
            rig.focus(*state);
            rig.tick();
            assert!(rig.is_open(), "states {states:?}");
        }
        assert_eq!(rig.trace(), vec![open_line("start", false, false)]);
    }
}

// rule 8: with dismiss_on_focus_loss off in a Config reply, losing the focus hides nothing
#[test]
fn rule_focus_loss_is_ignored_when_the_config_says_so() {
    let mut rig = Rig::opened();
    rig.push(config_reply(
        &json!({"popup": {"dismiss_on_focus_loss": false}}),
    ));
    rig.tick();
    focus_cycle(&mut rig);
    assert!(rig.is_open(), "the flag is off");
    rig.push(config_reply(
        &json!({"popup": {"dismiss_on_focus_loss": true}}),
    ));
    rig.tick();
    assert!(
        rig.is_open(),
        "the window still reports no focus, and it has not been seen focused since the loss"
    );
    focus_cycle(&mut rig);
    assert!(!rig.is_open(), "the flag is on again");
}

// rule 8: the focus state is ignored while the popup is closed
#[test]
fn rule_focus_state_is_ignored_while_closed() {
    let mut rig = Rig::closed();
    focus_cycle(&mut rig);
    focus_cycle(&mut rig);
    assert!(!rig.is_open());
    assert_none(&rig.trace(), "rig.trace()");
    assert_eq!(rig.cmds(), vec![]);
}

// rule 8: focus seen before an open does not count: the window must be seen focused again
#[test]
fn rule_focus_before_an_open_does_not_count() {
    let mut rig = Rig::closed();
    rig.focus(Some(true));
    rig.tick();
    rig.focus(Some(false));
    rig.push(request());
    rig.tick();
    assert!(rig.is_open());
    rig.tick();
    assert!(rig.is_open(), "Some(false) without a focus since the open");
}

// rule 8: after a hide and a re-open the cycle needs a new Some(true)
#[test]
fn rule_focus_cycle_needs_a_new_focus_after_a_reopen() {
    let mut rig = Rig::opened();
    focus_cycle(&mut rig);
    assert!(!rig.is_open());
    rig.push(request());
    rig.tick();
    assert!(rig.is_open());
    rig.tick();
    rig.tick();
    assert!(rig.is_open(), "the window still reports Some(false)");
    focus_cycle(&mut rig);
    assert!(!rig.is_open(), "a new cycle hides again");
    let hides = rig
        .trace()
        .iter()
        .filter(|line| **line == hide_line("focus_lost"))
        .count();
    assert_eq!(hides, 2);
}

// rule 8: a stale Event::WindowFocused(false) in the raw events does not hide a fresh popup
#[test]
fn rule_focus_stale_egui_event_does_not_hide_a_fresh_popup() {
    let mut rig = Rig::closed();
    // the window lost the focus while hidden: the egui event waits for the next visible pass
    rig.input(egui::Event::WindowFocused(false));
    rig.tick();
    rig.push(request());
    rig.tick();
    rig.focus(Some(true));
    rig.show();
    assert!(
        rig.is_open(),
        "the stale event was not a focus loss of this popup"
    );
    rig.show();
    assert!(rig.is_open());
    rig.focus(Some(false));
    rig.show();
    assert!(
        !rig.is_open(),
        "the real loss, seen in the viewport info, hides"
    );
}

// rule 8: a WindowFocused(false) in the events alone, with a focused window, hides nothing
#[test]
fn rule_focus_events_alone_never_hide() {
    let mut rig = Rig::opened();
    rig.focus(Some(true));
    rig.show();
    rig.input(egui::Event::WindowFocused(false));
    rig.show();
    assert!(rig.is_open());
    rig.input(egui::Event::WindowFocused(true));
    rig.focus(None);
    rig.show();
    assert!(rig.is_open());
}

// ---------------------------------------------------------------------------
// 9. Close
// ---------------------------------------------------------------------------

// rule 9: a Close in the ROOT viewport events hides an open popup (close_request)
#[test]
fn rule_close_request_hides_an_open_popup() {
    for visible in [false, true] {
        let mut rig = Rig::opened();
        rig.close_request(WindowKind::Popup);
        if visible {
            rig.show();
        } else {
            rig.tick();
        }
        assert!(!rig.is_open(), "visible = {visible}");
        assert_eq!(
            rig.trace(),
            vec![open_line("start", false, false), hide_line("close_request")]
        );
        rig.tick();
        assert_eq!(rig.trace().len(), 2, "the request is handled once");
    }
}

// rule 9: a Close while the popup is closed does nothing
#[test]
fn rule_close_request_while_closed_does_nothing() {
    let mut rig = Rig::closed();
    rig.close_request(WindowKind::Popup);
    rig.tick();
    assert!(!rig.is_open());
    assert_none(&rig.trace(), "rig.trace()");
    assert_eq!(rig.cmds(), vec![]);
    assert!(
        !rig.desired().exit,
        "closing the popup window never ends the app"
    );
}

// rule 9: on a Settings pass a Close closes Settings and leaves the popup alone
#[test]
fn rule_close_request_on_a_settings_pass_closes_settings() {
    for visible in [false, true] {
        let mut rig = Rig::opened();
        rig.tray(&[TrayAction::OpenConfig]);
        rig.tick();
        assert!(rig.desired().settings_open);
        rig.close_request(WindowKind::Settings);
        if visible {
            rig.show_settings();
        } else {
            rig.tick_settings();
        }
        assert!(!rig.desired().settings_open, "visible = {visible}");
        assert!(rig.is_open(), "the popup is untouched");
        assert_eq!(rig.trace(), vec![open_line("start", false, false)]);
    }
}

// rule 9: a Close on a Popup pass leaves Settings open
#[test]
fn rule_close_request_on_a_popup_pass_leaves_settings_open() {
    let mut rig = Rig::opened();
    rig.tray(&[TrayAction::OpenConfig]);
    rig.tick();
    rig.close_request(WindowKind::Popup);
    rig.tick();
    assert!(!rig.is_open());
    assert!(rig.desired().settings_open);
}

// ---------------------------------------------------------------------------
// 10. Tray
// ---------------------------------------------------------------------------

// rule 10: Toggle opens a closed popup (Open{tray}) and hides an open one (Hide{tray})
#[test]
fn rule_tray_toggle_opens_and_hides() {
    let mut rig = Rig::closed();
    rig.tray(&[TrayAction::Toggle]);
    rig.tick();
    assert!(rig.is_open());
    assert_eq!(rig.trace(), vec![open_line("tray", false, false)]);
    assert_open_commands(&rig.cmds());
    assert_eq!(rig.context(), PasteContext::default());
    rig.tray(&[TrayAction::Toggle]);
    rig.tick();
    assert!(!rig.is_open());
    assert_eq!(
        rig.trace(),
        vec![open_line("tray", false, false), hide_line("tray")]
    );
    assert_eq!(rig.cmds(), vec![]);
}

// rule 10: a Toggle right after a focus-loss hide stays hidden; a later one opens
#[test]
fn rule_tray_toggle_right_after_a_focus_loss_hide_stays_hidden() {
    let mut rig = Rig::opened();
    let before_hide = Instant::now();
    focus_cycle(&mut rig);
    assert!(!rig.is_open());
    rig.tray(&[TrayAction::Toggle]);
    rig.tick();
    if !still_young(before_hide, 290) {
        // a stalled machine: the guard of 300 ms may be over, nothing to check
        return;
    }
    assert!(!rig.is_open(), "the click only took the focus");
    assert_eq!(rig.cmds(), vec![]);
    assert_eq!(rig.trace().len(), 2, "no Open line: {:?}", rig.trace());
    thread::sleep(Duration::from_millis(350));
    rig.tray(&[TrayAction::Toggle]);
    rig.tick();
    assert!(rig.is_open(), "the guard is over");
    assert_eq!(rig.trace().last(), Some(&open_line("tray", false, false)));
}

// rule 10: OpenPopup is a request push: it opens with an empty context
#[test]
fn rule_tray_open_popup_is_a_request_push() {
    let mut rig = Rig::closed();
    rig.tray(&[TrayAction::OpenPopup]);
    rig.tick();
    assert!(rig.is_open());
    assert_eq!(rig.context(), PasteContext::default());
    assert_eq!(rig.trace(), vec![open_line("request", false, false)]);
    assert_open_commands(&rig.cmds());
}

// rule 10: OpenPopup on an open popup keeps its context and sends nothing
#[test]
fn rule_tray_open_popup_while_open_keeps_the_context() {
    let mut rig = Rig::closed();
    rig.push(hotkey(4, 5));
    rig.tick();
    let _ = rig.cmds();
    rig.tray(&[TrayAction::OpenPopup]);
    rig.tick();
    assert!(rig.is_open());
    assert_eq!(
        rig.context(),
        PasteContext {
            target: Some(4),
            gesture: Some(5)
        }
    );
    assert_eq!(rig.cmds(), vec![]);
    assert_eq!(rig.trace(), vec![open_line("hotkey", true, true)]);
}

// rule 10: OpenConfig and About open Settings and send the four commands; About selects its tab
#[test]
fn rule_tray_open_config_and_about_open_settings() {
    let mut rig = Rig::closed();
    rig.tray(&[TrayAction::OpenConfig]);
    rig.tick();
    assert!(rig.desired().settings_open);
    assert!(!rig.is_open(), "Settings does not open the popup");
    assert_eq!(rig.cmds(), settings_commands());
    assert_eq!(rig.app.config().lock().expect("settings").tab, "History");
    assert_none(&rig.trace(), "rig.trace()");
    let mut rig = Rig::closed();
    rig.tray(&[TrayAction::About]);
    rig.tick();
    assert!(rig.desired().settings_open);
    assert_eq!(rig.cmds(), settings_commands());
    assert_eq!(rig.app.config().lock().expect("settings").tab, "about");
}

// rule 10: OpenConfig keeps the tab that is selected; each open sends the commands again
#[test]
fn rule_tray_open_config_keeps_the_selected_tab() {
    let mut rig = Rig::closed();
    rig.app.config().lock().expect("settings").tab = "Popup".to_string();
    rig.tray(&[TrayAction::OpenConfig]);
    rig.tick();
    assert_eq!(rig.app.config().lock().expect("settings").tab, "Popup");
    assert_eq!(rig.cmds(), settings_commands());
    rig.tray(&[TrayAction::OpenConfig]);
    rig.tick();
    assert!(rig.desired().settings_open);
    assert_eq!(rig.cmds(), settings_commands());
}

// rule 10: TogglePause sends MonitorToggle each time
#[test]
fn rule_tray_toggle_pause_sends_monitor_toggle() {
    let mut rig = Rig::closed();
    rig.tray(&[TrayAction::TogglePause]);
    rig.tick();
    assert_eq!(rig.cmds(), vec![UiCmd::MonitorToggle]);
    rig.tray(&[TrayAction::TogglePause]);
    rig.tick();
    assert_eq!(rig.cmds(), vec![UiCmd::MonitorToggle]);
}

// rule 10: PauseFor1h sends MonitorToggle once and again only after the paused flag is cleared
#[test]
fn rule_tray_pause_for_1h_toggles_once_until_the_flag_clears() {
    let mut rig = Rig::closed();
    rig.tray(&[TrayAction::PauseFor1h]);
    rig.tick();
    assert_eq!(rig.cmds(), vec![UiCmd::MonitorToggle]);
    rig.push(UiEvent::MonitorPaused(true));
    rig.tick();
    rig.tray(&[TrayAction::PauseFor1h]);
    rig.tick();
    assert_eq!(rig.cmds(), vec![], "already paused");
    rig.push(UiEvent::MonitorPaused(false));
    rig.tick();
    rig.tray(&[TrayAction::PauseFor1h]);
    rig.tick();
    assert_eq!(
        rig.cmds(),
        vec![UiCmd::MonitorToggle],
        "the flag was cleared"
    );
}

// rule 10: a paused Status reply counts as paused for PauseFor1h
#[test]
fn rule_tray_pause_for_1h_does_nothing_when_a_status_says_paused() {
    let mut rig = Rig::closed();
    rig.push(status(true, "1"));
    rig.tick();
    rig.tray(&[TrayAction::PauseFor1h]);
    rig.tick();
    assert_eq!(rig.cmds(), vec![]);
    rig.push(status(false, "1"));
    rig.tick();
    rig.tray(&[TrayAction::PauseFor1h]);
    rig.tick();
    assert_eq!(rig.cmds(), vec![UiCmd::MonitorToggle]);
}

// rule 10: Quit sends Stop once
#[test]
fn rule_tray_quit_sends_stop_once() {
    let mut rig = Rig::closed();
    rig.tray(&[TrayAction::Quit]);
    rig.tick();
    assert_eq!(rig.cmds(), vec![UiCmd::Stop]);
    assert!(!rig.desired().exit, "the app waits for the answer");
    rig.tray(&[TrayAction::Quit]);
    rig.tick();
    assert_eq!(rig.cmds(), vec![], "a second Quit sends nothing");
}

// rule 10: Stopped, StopFailed and AnotherInstance set exit
#[test]
fn rule_tray_exit_follows_the_stop_replies() {
    let replies = [
        UiEvent::Stopped,
        UiEvent::StopFailed("refused".to_string()),
        UiEvent::AnotherInstance,
    ];
    for reply in replies {
        let mut rig = Rig::closed();
        rig.tray(&[TrayAction::Quit]);
        rig.tick();
        let _ = rig.cmds();
        rig.push(reply.clone());
        rig.tick();
        assert!(rig.desired().exit, "{reply:?} after Quit");
        let mut rig = Rig::closed();
        rig.push(reply.clone());
        rig.tick();
        assert!(rig.desired().exit, "{reply:?} on its own");
    }
}

// rule 10: no tray action but Toggle touches an open popup
#[test]
fn rule_tray_other_actions_leave_the_popup_alone() {
    let actions = [
        TrayAction::OpenConfig,
        TrayAction::About,
        TrayAction::TogglePause,
        TrayAction::PauseFor1h,
        TrayAction::OpenPopup,
        TrayAction::Quit,
    ];
    for action in actions {
        let mut rig = Rig::opened();
        rig.tray(std::slice::from_ref(&action));
        rig.tick();
        assert!(rig.is_open(), "{action:?} left the popup open");
        assert_eq!(rig.error(), None, "{action:?}");
        assert_eq!(
            rig.trace(),
            vec![open_line("start", false, false)],
            "{action:?} wrote a line"
        );
        assert_eq!(rig.context(), PasteContext::default(), "{action:?}");
    }
}

// rule 10: the tray closure is polled by Popup passes only, and its actions run in order
#[test]
fn rule_tray_is_polled_by_popup_passes_in_order() {
    let mut rig = Rig::closed();
    rig.tray(&[TrayAction::Toggle, TrayAction::Toggle]);
    rig.tick_settings();
    rig.show_settings();
    assert_eq!(
        rig.tray_polls.load(Ordering::SeqCst),
        0,
        "Settings passes do not poll"
    );
    assert!(!rig.is_open());
    rig.tick();
    assert_eq!(rig.tray_polls.load(Ordering::SeqCst), 1);
    assert!(!rig.is_open(), "open, then hide");
    assert_eq!(
        rig.trace(),
        vec![open_line("tray", false, false), hide_line("tray")]
    );
    rig.show();
    assert_eq!(
        rig.tray_polls.load(Ordering::SeqCst),
        2,
        "once per Popup pass"
    );
}

// ---------------------------------------------------------------------------
// 11. Config reply
// ---------------------------------------------------------------------------

fn light_config(scale: f64) -> UiEvent {
    config_reply(&json!({"ui": {"theme": {"mode": "light", "font_scale": scale}}}))
}

fn is_dark(ctx: &egui::Context) -> bool {
    ctx.global_style().visuals.dark_mode
}

#[test]
fn fix_x2_hidden_theme_wins_over_the_visible_system_theme() {
    for (mode, system_theme, visuals) in [
        ("dark", egui::Theme::Light, egui::Visuals::dark()),
        ("light", egui::Theme::Dark, egui::Visuals::light()),
    ] {
        let mut rig = Rig::opened();
        rig.push(config_reply(&json!({"ui": {"theme": {"mode": mode}}})));
        rig.tick();
        rig.popup.system_theme = Some(system_theme);
        rig.settings.system_theme = Some(system_theme);
        for kind in [WindowKind::Popup, WindowKind::Settings] {
            for _ in 0..2 {
                let pass = rig.pass(kind, true);
                assert_eq!(
                    is_dark(&rig.win_mut(kind).ctx),
                    visuals.dark_mode,
                    "{kind:?}: configured {mode} with system {system_theme:?}"
                );
                assert!(
                    pass.fills.contains(&visuals.panel_fill),
                    "{kind:?} must paint the configured {mode} panel: {:?}",
                    pass.fills
                );
            }
        }
    }
}

/// WCAG contrast ratio of two opaque colours.
fn contrast(a: egui::Color32, b: egui::Color32) -> f32 {
    fn luminance(c: egui::Color32) -> f32 {
        let lin = |v: u8| {
            let v = f32::from(v) / 255.0;
            if v <= 0.040_45 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
    }
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

// Error text (popup footer, Settings field errors) must stay readable: at
// least 4.5:1 against the panel in both themes and both windows.
#[test]
fn rule_error_colour_contrasts_with_the_panel_in_both_themes() {
    for mode in ["dark", "light"] {
        let mut rig = Rig::opened();
        rig.push(config_reply(&json!({"ui": {"theme": {"mode": mode}}})));
        rig.tick();
        for kind in [WindowKind::Popup, WindowKind::Settings] {
            rig.pass(kind, true);
            let visuals = rig.win_mut(kind).ctx.global_style().visuals.clone();
            let ratio = contrast(visuals.error_fg_color, visuals.panel_fill);
            assert!(
                ratio >= 4.5,
                "{kind:?} {mode}: error {:?} on panel {:?} has contrast {ratio}",
                visuals.error_fg_color,
                visuals.panel_fill
            );
        }
    }
}

fn assert_zoom(ctx: &egui::Context, expected: f32, what: &str) {
    let zoom = ctx.zoom_factor();
    assert!(
        (zoom - expected).abs() < 1e-6,
        "{what}: zoom factor {zoom}, expected {expected}"
    );
}

// rule 11: ui.theme {light, 1.4} reaches the Popup context after the next Popup pass and
// the Settings context after its pass; the Settings state follows
#[test]
fn rule_config_theme_reaches_both_contexts() {
    let mut rig = Rig::closed();
    rig.push(light_config(1.4));
    rig.show();
    {
        let config = rig.app.config().lock().expect("settings");
        assert!(!config.dark);
        assert!((config.font_scale - 1.4).abs() < 1e-6);
    }
    assert!(
        !is_dark(&rig.popup.ctx),
        "light visuals on the Popup context"
    );
    // egui applies a new zoom factor at the start of the next pass
    rig.show();
    assert_zoom(&rig.popup.ctx, 1.4, "popup");
    // the Settings context has had no pass yet
    assert!(is_dark(&rig.settings.ctx));
    assert_zoom(&rig.settings.ctx, 1.0, "settings before its pass");
    rig.show_settings();
    assert!(
        !is_dark(&rig.settings.ctx),
        "light visuals on the Settings context"
    );
    rig.show_settings();
    assert_zoom(&rig.settings.ctx, 1.4, "settings");
}

// rule 11: a hidden Popup pass applies the theme too
#[test]
fn rule_config_theme_is_applied_by_hidden_passes() {
    let mut rig = Rig::closed();
    rig.push(light_config(1.25));
    rig.tick();
    assert!(!is_dark(&rig.popup.ctx));
    rig.show();
    assert_zoom(&rig.popup.ctx, 1.25, "popup");
}

// rule 11: a Config reply without ui.theme leaves the defaults
#[test]
fn rule_config_without_a_theme_leaves_the_defaults() {
    let mut rig = Rig::closed();
    rig.push(config_reply(&json!({"popup": {"hotkey": "ctrl+alt+h"}})));
    rig.show();
    rig.show();
    rig.show_settings();
    rig.show_settings();
    let config = rig.app.config().lock().expect("settings");
    assert!(config.dark);
    assert!((config.font_scale - 1.0).abs() < f32::EPSILON);
    drop(config);
    assert!(is_dark(&rig.popup.ctx));
    assert_zoom(&rig.popup.ctx, 1.0, "popup");
    assert!(is_dark(&rig.settings.ctx));
    assert_zoom(&rig.settings.ctx, 1.0, "settings");
}

// rule 11: the theme follows later replies: a second change is applied again
#[test]
fn rule_config_theme_changes_are_applied_again() {
    let mut rig = Rig::closed();
    let changes = [
        (light_config(1.4), false, 1.4_f32),
        (
            config_reply(&json!({"ui": {"theme": {"mode": "dark", "font_scale": 1.0}}})),
            true,
            1.0,
        ),
        (light_config(0.9), false, 0.9),
        (light_config(1.1), false, 1.1),
        (
            config_reply(&json!({"ui": {"theme": {"mode": "dark", "font_scale": 1.1}}})),
            true,
            1.1,
        ),
    ];
    for (n, (reply, dark, scale)) in changes.into_iter().enumerate() {
        rig.push(reply);
        rig.show();
        rig.show();
        rig.show_settings();
        rig.show_settings();
        assert_eq!(is_dark(&rig.popup.ctx), dark, "change {n}: popup");
        assert_zoom(&rig.popup.ctx, scale, &format!("change {n}: popup"));
        assert_eq!(is_dark(&rig.settings.ctx), dark, "change {n}: settings");
        assert_zoom(&rig.settings.ctx, scale, &format!("change {n}: settings"));
    }
}

// rule 11: a theme edited in the Settings state is applied to both contexts by their next pass
#[test]
fn rule_config_state_edits_reach_both_contexts() {
    let mut rig = Rig::closed();
    rig.show();
    rig.show_settings();
    {
        let mut config = rig.app.config().lock().expect("settings");
        config.dark = false;
        config.font_scale = 1.2;
    }
    rig.show();
    rig.show();
    rig.show_settings();
    rig.show_settings();
    assert!(!is_dark(&rig.popup.ctx));
    assert_zoom(&rig.popup.ctx, 1.2, "popup");
    assert!(!is_dark(&rig.settings.ctx));
    assert_zoom(&rig.settings.ctx, 1.2, "settings");
}

// rule 11: the scale is compared as bits: the smallest change is applied
#[test]
fn rule_config_scale_change_is_compared_by_bits() {
    let mut rig = Rig::closed();
    rig.show();
    rig.show();
    let next = f32::from_bits(1.0_f32.to_bits() + 1);
    rig.app.config().lock().expect("settings").font_scale = next;
    rig.show();
    rig.show();
    assert_eq!(rig.popup.ctx.zoom_factor().to_bits(), next.to_bits());
}

// rule 11: popup.keys overrides reach the key bindings of the popup
#[test]
fn rule_config_key_override_reaches_the_popup_keys() {
    let mut rig = Rig::raw(Setup::new(true));
    let start = rig.cmds();
    let tag = start
        .into_iter()
        .find_map(|cmd| match cmd {
            UiCmd::CompactHistory(tag) => Some(tag),
            _ => None,
        })
        .expect("initial compact history request");
    rig.push(UiEvent::CompactHistory(tag.clone(), Ok(json!({"owner":tag.params.owner,"generation":tag.params.generation,"rows":[],"cursor":null,"done":true,"bytes_read":0,"candidates":0}))));
    rig.tick();
    let _ = rig.cmds();
    rig.push(config_reply(
        &json!({"popup": {"keys": {"refresh": "ctrl+End"}}}),
    ));
    rig.tick();
    rig.key(egui::Key::F5);
    rig.show();
    assert_eq!(rig.cmds(), vec![], "F5 no longer refreshes");
    rig.input(egui::Event::ModifiersChanged(egui::Modifiers::CTRL));
    rig.input(key_event(egui::Key::End, true, egui::Modifiers::CTRL));
    rig.show();
    let commands = rig.cmds();
    assert!(
        matches!(commands.as_slice(), [UiCmd::CompactHistory(tag)] if tag.params.query.is_empty() && tag.params.cursor.is_none()),
        "Ctrl+End refreshes: {commands:?}"
    );
}

// rule 11: a bad override is ignored: the default keys keep working
#[test]
fn rule_config_bad_key_override_is_ignored() {
    let mut rig = Rig::opened();
    rig.push(config_reply(
        &json!({"popup": {"keys": {"no_such_action": "x"}}}),
    ));
    rig.tick();
    rig.key(egui::Key::F5);
    rig.show();
    let commands = rig.cmds();
    assert_compact_begin(&commands);
    rig.answer_compact(&commands, &[]);
    rig.tick();
    rig.push(config_reply(
        &json!({"popup": {"keys": {"refresh": "not a chord"}}}),
    ));
    rig.tick();
    rig.key(egui::Key::F5);
    rig.show();
    assert_compact_begin(&rig.cmds());
}

// rule 11: the Settings document is replaced and its edits are cleared
#[test]
fn rule_config_replaces_the_document_and_clears_the_edits() {
    let mut rig = Rig::closed();
    rig.push(config_reply(&json!({"history": {"max_items": 10}})));
    rig.tick();
    {
        let mut config = rig.app.config().lock().expect("settings");
        assert_eq!(
            config
                .doc
                .as_ref()
                .and_then(|doc| doc.get("history.max_items")),
            Some(&json!(10))
        );
        config
            .edits
            .insert("history.max_items".to_string(), "99".to_string());
    }
    rig.push(config_reply(&json!({"history": {"max_items": 25}})));
    rig.tick();
    let config = rig.app.config().lock().expect("settings");
    assert_eq!(
        config
            .doc
            .as_ref()
            .and_then(|doc| doc.get("history.max_items")),
        Some(&json!(25))
    );
    assert!(config.edits.is_empty());
}

// ---------------------------------------------------------------------------
// 12. Settings errors
// ---------------------------------------------------------------------------

// rule 12: an Error while the Settings window is open shows in the Settings error line
#[test]
fn rule_error_while_settings_is_open_goes_to_settings() {
    let mut rig = Rig::opened();
    rig.tray(&[TrayAction::OpenConfig]);
    rig.tick();
    let _ = rig.cmds();
    rig.app.config().lock().expect("settings").pending_apply = vec!["a".to_string()];
    rig.push(UiEvent::Error("boom".to_string()));
    rig.tick();
    assert_eq!(rig.config_error(), "boom");
    assert_none(
        &rig.app.config().lock().expect("settings").pending_apply,
        "pending_apply",
    );
    assert_eq!(rig.error(), None, "the popup error line stays empty");
    assert_eq!(rig.trace(), vec![open_line("start", false, false)]);
}

// rule 12: an Error while an Apply is pending goes to the Settings error line
#[test]
fn rule_error_with_a_pending_apply_goes_to_settings_when_open() {
    let mut rig = Rig::closed();
    rig.tray(&[TrayAction::OpenConfig]);
    rig.tick();
    let _ = rig.cmds();
    rig.app.config().lock().expect("settings").pending_apply =
        vec!["popup.hotkey".to_string(), "history.max_items".to_string()];
    rig.push(UiEvent::Error("refused".to_string()));
    rig.tick();
    assert_eq!(rig.config_error(), "refused");
    assert_none(
        &rig.app.config().lock().expect("settings").pending_apply,
        "pending_apply",
    );
    assert_eq!(rig.error(), None);
    assert_none(&rig.trace(), "rig.trace()");
}

#[test]
fn rule_error_with_pending_apply_when_closed_goes_to_shell() {
    let mut rig = Rig::closed();
    rig.app.config().lock().expect("settings").pending_apply = vec!["popup.hotkey".to_string()];
    rig.push(UiEvent::Error("closed pending".to_string()));
    rig.tick();
    assert_eq!(
        rig.config_error(),
        "",
        "closed Error must not fill Settings even with pending_apply"
    );
    assert_eq!(
        rig.error().as_deref(),
        Some("closed pending"),
        "closed Error routes to shell"
    );
    assert_eq!(
        rig.app.config().lock().expect("settings").pending_apply,
        vec!["popup.hotkey".to_string()],
        "closed Error must not clear pending_apply (only open clears)"
    );
    assert_none(&rig.trace(), "closed Error does not open popup");
    assert!(!rig.is_open(), "closed Error must not force popup open");
}

// rule 12: otherwise the Error is the popup error line
#[test]
fn rule_error_otherwise_sets_the_popup_error_line() {
    let mut rig = Rig::opened();
    rig.push(UiEvent::Error("boom".to_string()));
    rig.tick();
    assert_eq!(rig.error().as_deref(), Some("boom"));
    assert_eq!(rig.config_error(), "");
    // after the Settings window closed, errors are the popup's again
    rig.tray(&[TrayAction::OpenConfig]);
    rig.tick();
    rig.close_request(WindowKind::Settings);
    rig.tick_settings();
    rig.push(UiEvent::Error("later".to_string()));
    rig.tick();
    assert_eq!(rig.error().as_deref(), Some("later"));
    assert_eq!(rig.config_error(), "");
}

// rule 12: PasteFailed while closed re-opens the popup with the error
#[test]
fn rule_paste_failed_while_closed_reopens_with_the_error() {
    let mut rig = Rig::closed();
    rig.push(UiEvent::PasteFailed("window gone".to_string()));
    rig.tick();
    assert!(rig.is_open());
    assert_eq!(rig.error().as_deref(), Some("window gone"));
    assert_eq!(rig.context(), PasteContext::default());
    assert_eq!(
        rig.trace(),
        vec![open_line("paste_failed", false, false), error_line()]
    );
    assert_open_commands(&rig.cmds());
}

// rule 12: PasteFailed while open sets the error and nothing else
#[test]
fn rule_paste_failed_while_open_sets_the_error_only() {
    let mut rig = Rig::opened();
    rig.push(UiEvent::PasteFailed("window gone".to_string()));
    rig.tick();
    assert!(rig.is_open());
    assert_eq!(rig.error().as_deref(), Some("window gone"));
    assert_eq!(rig.cmds(), vec![]);
    assert_eq!(
        rig.trace(),
        vec![open_line("start", false, false), error_line()]
    );
}

// ---------------------------------------------------------------------------
// 13. Timers
// ---------------------------------------------------------------------------

// rule 13: with a 40 ms period a Status goes out at the first pass after 40 ms, not before,
// and not again before the next period
#[test]
fn rule_timer_status_goes_out_after_the_period() {
    let period = Duration::from_millis(40);
    let mut rig = Rig::with(Setup::new(false).period(period));
    rig.tick();
    if still_young(rig.born, 39) {
        assert_eq!(rig.cmds(), vec![], "before the period");
    }
    thread::sleep(Duration::from_millis(50));
    let polled = Instant::now();
    rig.tick();
    assert_eq!(
        rig.cmds(),
        vec![UiCmd::Status],
        "the first pass after the period"
    );
    rig.tick();
    if still_young(polled, 39) {
        assert_eq!(rig.cmds(), vec![], "not again before the next period");
    }
    thread::sleep(Duration::from_millis(50));
    rig.tick();
    assert_eq!(
        rig.cmds(),
        vec![UiCmd::Status],
        "and again after the next period"
    );
}

// rule 13: the poll runs on hidden and on visible Popup passes, open or closed
#[test]
fn rule_timer_status_runs_on_every_kind_of_popup_pass() {
    for open in [false, true] {
        for visible in [false, true] {
            let mut rig = Rig::with(Setup::new(open).period(Duration::from_millis(20)));
            thread::sleep(Duration::from_millis(30));
            if visible {
                rig.show();
            } else {
                rig.tick();
            }
            assert_eq!(
                rig.cmds(),
                vec![UiCmd::Status],
                "open = {open}, visible = {visible}"
            );
        }
    }
}

// rule 13: Settings passes poll nothing
#[test]
fn rule_timer_settings_passes_send_no_status() {
    let mut rig = Rig::with(Setup::new(false).period(Duration::from_millis(5)));
    thread::sleep(Duration::from_millis(15));
    rig.tick_settings();
    rig.show_settings();
    assert_eq!(rig.cmds(), vec![]);
    rig.tick();
    assert_eq!(rig.cmds(), vec![UiCmd::Status]);
}

// rule 13: PauseFor1h has a deadline an hour away: no command appears before it
#[test]
fn rule_timer_pause_for_1h_adds_no_spurious_command() {
    let mut rig = Rig::closed();
    rig.tray(&[TrayAction::PauseFor1h]);
    rig.tick();
    assert_eq!(rig.cmds(), vec![UiCmd::MonitorToggle]);
    rig.push(UiEvent::MonitorPaused(true));
    for _ in 0..4 {
        rig.tick();
        rig.show();
    }
    assert_eq!(rig.cmds(), vec![], "nothing happens before the deadline");
    assert!(!rig.desired().exit);
}

// ---------------------------------------------------------------------------
// 14. Wake scheduling
// ---------------------------------------------------------------------------

fn nonzero(requests: &[Request]) -> Vec<Duration> {
    requests
        .iter()
        .map(|request| request.delay)
        .filter(|delay| !delay.is_zero())
        .collect()
}

// rule 14: an idle Popup pass records exactly one request, with a delay in (0, status_period]
#[test]
fn rule_wake_idle_pass_requests_one_delay_within_the_period() {
    let mut rig = Rig::closed();
    rig.warm_up();
    let mut previous: Option<Duration> = None;
    for n in 0..5 {
        rig.tick();
        let requests = rig.requests();
        assert_eq!(requests.len(), 1, "pass {n}: {requests:?}");
        let delay = requests[0].delay;
        assert!(
            delay > Duration::ZERO && delay <= HOUR,
            "pass {n}: delay {delay:?}"
        );
        if let Some(previous) = previous {
            assert!(
                previous >= delay + Duration::from_millis(15),
                "the wake is the fixed deadline last_poll + period, not a fresh period: \
                 {previous:?} then {delay:?}"
            );
        }
        previous = Some(delay);
        thread::sleep(Duration::from_millis(20));
    }
}

// rule 14: with a short period the delay stays within it and is never zero
#[test]
fn rule_wake_delay_stays_within_a_short_period() {
    let period = Duration::from_millis(250);
    let mut rig = Rig::with(Setup::new(true).period(period));
    rig.warm_up();
    for n in 0..20 {
        if n % 2 == 0 {
            rig.tick();
        } else {
            rig.tick_settings();
            continue;
        }
        let requests = rig.requests();
        assert_eq!(requests.len(), 1, "pass {n}: {requests:?}");
        let delay = requests[0].delay;
        assert!(
            delay > Duration::ZERO && delay <= period,
            "pass {n}: {delay:?}"
        );
    }
}

// rule 14: a Status poll restarts the period: the next wake is a full period away
#[test]
fn rule_wake_status_poll_restarts_the_period() {
    let period = Duration::from_millis(250);
    let mut rig = Rig::with(Setup::new(false).period(period));
    rig.warm_up();
    thread::sleep(Duration::from_millis(270));
    rig.tick();
    assert!(rig.cmds().contains(&UiCmd::Status));
    let requests = rig.requests();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert!(
        requests[0].delay > Duration::from_millis(150) && requests[0].delay <= period,
        "delay {:?}",
        requests[0].delay
    );
}

// rule 14: with a pending paste after the confirmation the delay is at most 150 ms
#[test]
fn rule_wake_pending_paste_wakes_within_the_paste_delay() {
    let mut rig = Rig::opened();
    rig.load(&[clip(1, "alpha")]);
    rig.key(egui::Key::Enter);
    rig.show();
    let _ = rig.requests();
    // the pass that confirms the hide; egui may owe the context one more pass first
    let mut delays = Vec::new();
    for _ in 0..3 {
        rig.tick();
        delays.extend(nonzero(&rig.requests()));
        if !delays.is_empty() {
            break;
        }
    }
    assert_eq!(delays.len(), 1, "non-zero requests: {delays:?}");
    assert!(
        delays[0] <= Duration::from_millis(150) && delays[0] >= Duration::from_millis(100),
        "the wake is the paste deadline: {:?}",
        delays[0]
    );
    // once the paste is out, the paste deadline is gone
    thread::sleep(Duration::from_millis(160));
    rig.tick();
    assert_eq!(rig.cmds().len(), 1);
    let after = nonzero(&rig.requests());
    assert_eq!(after.len(), 1, "{after:?}");
    assert!(
        after[0] > Duration::from_secs(60),
        "back to the status poll: {:?}",
        after[0]
    );
}

// rule 14: after Quit the delay is at most 6 s
#[test]
fn rule_wake_after_quit_is_within_six_seconds() {
    let mut rig = Rig::closed();
    rig.warm_up();
    rig.tray(&[TrayAction::Quit]);
    rig.tick();
    let requests = rig.requests();
    assert_eq!(requests.len(), 1, "{requests:?}");
    let delay = requests[0].delay;
    assert!(
        delay > Duration::from_secs(5) && delay <= Duration::from_secs(6),
        "the exit deadline is six seconds after the Quit: {delay:?}"
    );
    // the exit is set: the deadline is gone
    rig.push(UiEvent::Stopped);
    rig.tick();
    let requests = rig.requests();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert!(
        requests[0].delay > Duration::from_secs(60),
        "{:?}",
        requests[0].delay
    );
}

// rule 14: a pass with nothing in the future makes no request
#[test]
fn rule_wake_makes_no_request_when_nothing_is_in_the_future() {
    let mut rig = Rig::with(Setup::new(false).period(Duration::ZERO));
    rig.warm_up();
    let _ = rig.cmds();
    for n in 0..5 {
        rig.tick();
        assert_eq!(
            rig.cmds(),
            vec![UiCmd::Status],
            "pass {n}: a zero period polls every pass"
        );
        let requests = rig.requests();
        assert!(requests.is_empty(), "pass {n}: {requests:?}");
    }
}

// rule 14: no hidden pass ever requests a repaint of zero delay when nothing changed
#[test]
fn rule_wake_never_requests_a_zero_delay() {
    type Step = (&'static str, Box<dyn Fn(&mut Rig)>);
    let mut rig = Rig::closed();
    rig.warm_up();
    let mut steps: Vec<Step> = Vec::new();
    steps.push(("idle", Box::new(|_| {})));
    steps.push(("request push", Box::new(|rig| rig.push(request()))));
    steps.push((
        "history",
        Box::new(|rig| rig.push(history(&[clip(1, "alpha"), clip(2, "beta")]))),
    ));
    steps.push((
        "offline",
        Box::new(|rig| rig.push(UiEvent::Offline("x".into()))),
    ));
    steps.push(("status", Box::new(|rig| rig.push(status(true, "1")))));
    steps.push((
        "error",
        Box::new(|rig| rig.push(UiEvent::Error("e".into()))),
    ));
    steps.push(("focus", Box::new(|rig| rig.focus(Some(true)))));
    steps.push((
        "config",
        Box::new(|rig| rig.tray(&[TrayAction::OpenConfig])),
    ));
    steps.push(("toggle push", Box::new(|rig| rig.push(toggle()))));
    steps.push(("hotkey", Box::new(|rig| rig.push(hotkey(1, 2)))));
    steps.push(("focus lost", Box::new(|rig| rig.focus(Some(false)))));
    steps.push((
        "tray toggle",
        Box::new(|rig| rig.tray(&[TrayAction::Toggle])),
    ));
    steps.push(("pause", Box::new(|rig| rig.tray(&[TrayAction::PauseFor1h]))));
    steps.push(("quit", Box::new(|rig| rig.tray(&[TrayAction::Quit]))));
    for (name, step) in &steps {
        step(&mut rig);
        rig.tick();
        let requests = rig.requests();
        assert!(
            requests.iter().all(|request| !request.delay.is_zero()),
            "{name}: {requests:?}"
        );
        assert_eq!(
            requests.len(),
            1,
            "{name}: one request per pass: {requests:?}"
        );
        rig.tick();
        let requests = rig.requests();
        assert!(
            requests.iter().all(|request| !request.delay.is_zero()),
            "{name} (idle pass after it): {requests:?}"
        );
    }
}

// rule 14: a theme change asks egui for one repaint, not one per pass
#[test]
fn rule_wake_theme_is_applied_once_per_change() {
    let mut rig = Rig::closed();
    rig.warm_up();
    rig.push(light_config(1.4));
    rig.tick();
    for _ in 0..3 {
        rig.tick();
    }
    let _ = rig.requests();
    for n in 0..5 {
        rig.tick();
        let requests = rig.requests();
        assert!(
            requests.iter().all(|request| !request.delay.is_zero()),
            "pass {n} after the theme settled: {requests:?}"
        );
    }
}

// rule 14: the pause deadline is a wake while paused, and only while paused
#[test]
fn rule_wake_includes_the_pause_deadline_while_paused() {
    let mut rig = Rig::with(Setup::new(false).period(Duration::from_secs(7200)));
    rig.warm_up();
    let single = |rig: &Rig| {
        let requests = rig.requests();
        assert_eq!(requests.len(), 1, "{requests:?}");
        requests[0].delay
    };
    rig.tick();
    assert!(
        single(&rig) > Duration::from_secs(7000),
        "idle: the status poll"
    );
    rig.tray(&[TrayAction::PauseFor1h]);
    rig.tick();
    assert_eq!(rig.cmds(), vec![UiCmd::MonitorToggle]);
    assert!(
        single(&rig) > Duration::from_secs(7000),
        "not paused yet: the pause deadline is no wake"
    );
    rig.push(UiEvent::MonitorPaused(true));
    rig.tick();
    let delay = single(&rig);
    assert!(
        delay > Duration::from_secs(3500) && delay <= Duration::from_secs(3600),
        "paused: the pause deadline is the wake: {delay:?}"
    );
    rig.push(UiEvent::MonitorPaused(false));
    rig.tick();
    assert!(single(&rig) > Duration::from_secs(7000), "resumed");
    rig.push(UiEvent::MonitorPaused(true));
    rig.tick();
    assert!(
        single(&rig) > Duration::from_secs(7000),
        "resuming dropped the deadline for good"
    );
    rig.tray(&[TrayAction::TogglePause]);
    rig.tick();
    assert!(single(&rig) > Duration::from_secs(7000));
}

// rule 14: TogglePause drops the pause deadline
#[test]
fn rule_wake_toggle_pause_clears_the_pause_deadline() {
    let mut rig = Rig::with(Setup::new(false).period(Duration::from_secs(7200)));
    rig.warm_up();
    rig.tray(&[TrayAction::PauseFor1h]);
    rig.tick();
    rig.push(UiEvent::MonitorPaused(true));
    rig.tick();
    let _ = rig.requests();
    rig.tick();
    let requests = rig.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].delay <= Duration::from_secs(3600),
        "paused with a deadline: {:?}",
        requests[0].delay
    );
    rig.tray(&[TrayAction::TogglePause]);
    rig.tick();
    let requests = rig.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].delay > Duration::from_secs(7000),
        "the deadline is gone: {:?}",
        requests[0].delay
    );
}

// ---------------------------------------------------------------------------
// 15. Centering
// ---------------------------------------------------------------------------

fn desktop(cursor: Option<(i32, i32)>) -> placement::DesktopContext {
    let monitor = placement::Rect {
        x: -1920,
        y: -200,
        w: 1920,
        h: 1080,
    };
    placement::DesktopContext {
        monitors: vec![
            monitor,
            placement::Rect {
                x: 0,
                y: 100,
                w: 2560,
                h: 1440,
            },
        ],
        cursor,
        current_monitor: Some(monitor),
        primary_monitor: None,
        popup_physical_size: (720, 540),
        supports_global_position: true,
        destination_scale: 1.0,
    }
}

fn deliver(rig: &mut Rig, context: placement::DesktopContext) {
    assert!(clipmonger_app_ui::runner::deliver_desktop_context(
        &mut rig.app,
        || Some(context)
    ));
}

#[test]
fn rule_placement_waits_for_native_context_despite_monitor_size() {
    for visible in [false, true] {
        let mut rig = Rig::opened();
        rig.monitor(Some(egui::vec2(1920.0, 1080.0)));
        let pass = if visible { rig.show() } else { rig.tick() };
        assert_none(&pass.positions(), "placement commands");
        assert!(rig.app.wants_desktop_context());
        deliver(&mut rig, desktop(Some((-1700, 100))));
        let pass = rig.tick();
        // A rendered popup knows the size its clips need and centres that size.
        let size = match pass.sizes().as_slice() {
            [] => egui::vec2(720.0, 540.0),
            [size] if visible => *size,
            sizes => panic!("unexpected sizes {sizes:?}"),
        };
        let centre = egui::pos2(
            -1920.0 + ((1920.0 - size.x) / 2.0).floor(),
            -200.0 + ((1080.0 - size.y) / 2.0).floor(),
        );
        assert_eq!(pass.positions(), vec![centre]);
        assert!(!rig.app.wants_desktop_context());
        assert_none(&rig.tick().positions(), "placement commands");
    }
}

#[test]
fn rule_popup_size_follows_clips_and_stops_at_the_work_area_cap() {
    let mut sizes = Vec::new();
    for count in [3, 12, 200] {
        let mut rig = Rig::opened();
        let clips: Vec<_> = (1..=count)
            .map(|n| clip(n, &format!("History row {n}")))
            .collect();
        rig.push(history(&clips));
        rig.tick();
        rig.show();
        deliver(&mut rig, desktop(Some((-1700, 100))));
        let pass = rig.show();
        assert_eq!(pass.sizes().len(), 1, "{count} clips: one size");
        sizes.push(pass.sizes()[0]);
        assert_none(&rig.show().sizes(), "idle sizes");
    }
    assert!(sizes[0].y < sizes[1].y, "{sizes:?}");
    // 30% of the 1920 wide and 90% of the 1080 high work area.
    assert_eq!(sizes[2], egui::vec2(576.0, 972.0));
}

#[test]
fn rule_dpi_change_while_open_places_and_sizes_again() {
    let mut rig = Rig::opened();
    rig.show();
    deliver(&mut rig, desktop(Some((-1700, 100))));
    rig.show();
    rig.show();
    assert!(!rig.app.wants_desktop_context());
    rig.popup.info.native_pixels_per_point = Some(1.5);
    rig.show();
    assert!(
        rig.app.wants_desktop_context(),
        "a new scale needs a new snapshot"
    );
    deliver(&mut rig, desktop(Some((-1700, 100))));
    let pass = rig.show();
    assert_eq!(pass.sizes().len(), 1);
    assert_eq!(pass.positions().len(), 1);
    assert!(!rig.app.wants_desktop_context());
}

#[test]
fn rule_open_popup_grows_when_clips_arrive_and_stays_in_the_work_area() {
    let mut rig = Rig::opened();
    rig.push(config_reply(&json!({"popup": {"placement": "cursor"}})));
    rig.tick();
    rig.show();
    deliver(&mut rig, desktop(Some((-1700, 100))));
    let first = rig.show();
    assert_eq!(first.positions(), vec![egui::pos2(-1700.0, 100.0)]);
    let width = first.sizes()[0].x;
    let clips: Vec<_> = (1..=200)
        .map(|n| clip(n, &format!("History row {n}")))
        .collect();
    rig.push(history(&clips));
    rig.tick();
    let grown = rig.show();
    assert_eq!(grown.sizes(), vec![egui::vec2(width, 972.0)]);
    // The bottom edge stays at the work area's bottom (-200 + 1080).
    assert_eq!(grown.positions(), vec![egui::pos2(-1700.0, -92.0)]);
    assert_none(&rig.show().commands, "idle commands");
}

#[test]
fn rule_placement_mode_changes_with_unchanged_desktop_and_identical_config_is_idle() {
    let mut rig = Rig::opened();
    let context = desktop(Some((-1700, 100)));
    for (mode, expected) in [
        ("center", egui::pos2(-1320.0, 70.0)),
        ("cursor", egui::pos2(-1700.0, 100.0)),
        ("center", egui::pos2(-1320.0, 70.0)),
    ] {
        rig.push(config_reply(&json!({"popup": {"placement": mode}})));
        rig.tick();
        deliver(&mut rig, context.clone());
        assert_eq!(rig.tick().positions(), vec![expected]);
        rig.push(config_reply(&json!({"popup": {"placement": mode}})));
        assert_none(&rig.tick().positions(), "placement commands");
        assert!(!clipmonger_app_ui::runner::deliver_desktop_context(
            &mut rig.app,
            || panic!("identical Config queried native desktop")
        ));
    }
}

#[test]
fn rule_placement_reopens_with_fresh_cursor_but_already_open_raise_does_not_jump() {
    let mut rig = Rig::closed();
    rig.push(config_reply(&json!({"popup": {"placement": "cursor"}})));
    rig.tick();
    assert!(!clipmonger_app_ui::runner::deliver_desktop_context(
        &mut rig.app,
        || panic!("closed Config queried desktop")
    ));
    for cursor in [(-1700, 100), (300, 400)] {
        rig.tray(&[TrayAction::OpenPopup]);
        assert_none(&rig.tick().positions(), "placement commands");
        deliver(&mut rig, desktop(Some(cursor)));
        assert_eq!(
            rig.tick().positions(),
            vec![egui::pos2(
                f32::from(i16::try_from(cursor.0).unwrap()),
                f32::from(i16::try_from(cursor.1).unwrap())
            )]
        );
        rig.push(request());
        assert_none(&rig.tick().positions(), "placement commands");
        assert!(!clipmonger_app_ui::runner::deliver_desktop_context(
            &mut rig.app,
            || panic!("open raise queried desktop")
        ));
        rig.tray(&[TrayAction::Toggle]);
        rig.tick();
        assert!(!rig.app.desired().popup_open);
    }
}

#[test]
fn rule_placement_uses_actual_physical_size_and_converts_os_dpi_and_font_zoom_once() {
    let mut rig = Rig::opened();
    rig.popup.info.native_pixels_per_point = Some(1.5);
    rig.push(config_reply(
        &json!({"popup": {"placement": "cursor"}, "ui": {"theme": {"font_scale": 1.6}}}),
    ));
    rig.show();
    rig.show(); // egui applies the pending zoom at the next pass.
    deliver(&mut rig, desktop(Some((-1700, 100))));
    let position = rig.show().positions()[0];
    let physical = winit::dpi::PhysicalPosition::new(
        position.x * rig.popup.ctx.pixels_per_point(),
        position.y * rig.popup.ctx.pixels_per_point(),
    );
    assert!((rig.popup.ctx.pixels_per_point() - 2.4).abs() < 0.001);
    assert!((physical.x + 1700.0).abs() < 0.001);
    assert!((physical.y - 100.0).abs() < 0.001);
}

#[test]
fn rule_placement_unknown_platform_fallback_and_wayland_emit_no_synthetic_position() {
    let mut rig = Rig::opened();
    deliver(&mut rig, desktop(None));
    assert_eq!(rig.tick().positions(), vec![egui::pos2(-1320.0, 70.0)]);
    for wayland in [false, true] {
        rig.tray(&[TrayAction::Toggle]);
        rig.tick();
        rig.tray(&[TrayAction::OpenPopup]);
        rig.tick();
        let mut context = desktop(Some((-1700, 100)));
        if wayland {
            context.supports_global_position = false;
        } else {
            context.monitors.clear();
            context.current_monitor = None;
        }
        deliver(&mut rig, context);
        assert_none(&rig.tick().positions(), "placement commands");
    }
    rig.settings.info.monitor_size = Some(egui::vec2(1920.0, 1080.0));
    assert_none(&rig.tick_settings().positions(), "placement commands");
}

fn placement_is_idle(rig: &mut Rig) {
    assert_none(&rig.tick().positions(), "idle placement");
    assert!(!clipmonger_app_ui::runner::deliver_desktop_context(
        &mut rig.app,
        || panic!("idle pass queried desktop")
    ));
}

#[test]
fn rule_placement_recovers_missing_monitors_on_native_geometry_signal_without_polling() {
    use clipmonger_app_ui::runner::{notify_desktop_geometry, DesktopGeometrySignal};
    use winit::{
        dpi::{PhysicalPosition, PhysicalSize},
        event::WindowEvent,
    };
    for signal in 0..3 {
        let mut rig = Rig::opened();
        rig.push(config_reply(&json!({"popup": {"placement": "cursor"}})));
        rig.tick();
        let mut missing = desktop(Some((-1700, 100)));
        missing.monitors.clear();
        missing.current_monitor = None;
        if signal == 1 {
            missing.monitors.push(placement::Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            });
        }
        deliver(&mut rig, missing);
        for _ in 0..3 {
            placement_is_idle(&mut rig);
        }
        rig.push(config_reply(&json!({"popup": {"placement": "cursor"}})));
        placement_is_idle(&mut rig);
        rig.push(request());
        placement_is_idle(&mut rig);
        assert!(!notify_desktop_geometry(
            &mut rig.app,
            DesktopGeometrySignal::Window(WindowKind::Popup, &WindowEvent::RedrawRequested)
        ));
        assert!(!notify_desktop_geometry(
            &mut rig.app,
            DesktopGeometrySignal::Window(
                WindowKind::Settings,
                &WindowEvent::Resized(PhysicalSize::new(960, 680))
            )
        ));
        placement_is_idle(&mut rig);
        let moved = WindowEvent::Moved(PhysicalPosition::new(-1800, 0));
        let resized = WindowEvent::Resized(PhysicalSize::new(720, 540));
        let useful = match signal {
            0 => DesktopGeometrySignal::Window(WindowKind::Popup, &moved),
            1 => DesktopGeometrySignal::Window(WindowKind::Popup, &resized),
            _ => DesktopGeometrySignal::Resumed,
        };
        assert!(notify_desktop_geometry(&mut rig.app, useful));
        assert!(rig.app.desired().popup_open, "recover within the same open");
        deliver(&mut rig, desktop(Some((300, 400))));
        assert_eq!(rig.tick().positions(), vec![egui::pos2(300.0, 400.0)]);
        assert!(!notify_desktop_geometry(
            &mut rig.app,
            DesktopGeometrySignal::Resumed
        ));
        placement_is_idle(&mut rig);
    }
}

#[test]
fn rule_placement_wayland_has_no_retry_after_geometry_or_resume() {
    use clipmonger_app_ui::runner::{notify_desktop_geometry, DesktopGeometrySignal};
    let mut rig = Rig::opened();
    let mut unsupported = desktop(None);
    unsupported.supports_global_position = false;
    unsupported.monitors.clear();
    deliver(&mut rig, unsupported);
    for _ in 0..3 {
        assert!(!notify_desktop_geometry(
            &mut rig.app,
            DesktopGeometrySignal::Resumed
        ));
        assert!(!notify_desktop_geometry(
            &mut rig.app,
            DesktopGeometrySignal::Window(
                WindowKind::Popup,
                &winit::event::WindowEvent::Moved(winit::dpi::PhysicalPosition::new(0, 0))
            )
        ));
        placement_is_idle(&mut rig);
    }
}

// ---------------------------------------------------------------------------
// 16. The offline banner mirrors the shell
// ---------------------------------------------------------------------------

// rule 16: after every Popup pass the banner shows the offline text exactly when the shell is offline
#[test]
fn rule_offline_mirror_follows_the_shell_after_every_pass() {
    let events = [
        UiEvent::Offline("a".to_string()),
        status(false, "1"),
        UiEvent::Offline("b".to_string()),
        history(&[clip(1, "alpha")]),
        UiEvent::History(json!({"clips": 7})),
        UiEvent::History(json!({"clips": []})),
        UiEvent::Offline("c".to_string()),
        UiEvent::History(json!({})),
        status(true, "2"),
        UiEvent::Error("not a link problem".to_string()),
        UiEvent::Offline("d".to_string()),
    ];
    for visible in [false, true] {
        let mut rig = Rig::closed();
        for (n, event) in events.iter().enumerate() {
            rig.push(event.clone());
            if visible {
                rig.show();
            } else {
                rig.tick();
            }
            let offline = rig.app.shell().offline();
            let expected = if offline {
                strings::text("popup.offline")
            } else {
                ""
            };
            assert_eq!(
                rig.app.vm().banner(),
                expected,
                "event {n} ({event:?}), visible = {visible}"
            );
        }
        rig.tick();
        assert_eq!(
            rig.app.vm().banner() == strings::text("popup.offline"),
            rig.app.shell().offline()
        );
    }
}

// ---------------------------------------------------------------------------
// More rules of the focus edge, the order of a pass and the theme
// ---------------------------------------------------------------------------

// rule 8: a hide inside a visible pass clears the focus edge too: a re-open in the very
// next pass needs a new Some(true) even though that pass starts with the popup closed
#[test]
fn rule_focus_hide_by_key_then_reopen_next_pass_needs_new_focus() {
    let mut rig = Rig::opened();
    rig.focus(Some(true));
    rig.show();
    rig.key(egui::Key::Escape);
    rig.show();
    assert!(!rig.is_open());
    rig.focus(Some(false));
    rig.push(request());
    rig.tick();
    assert!(rig.is_open());
    rig.tick();
    assert!(
        rig.is_open(),
        "no new Some(true) since the open: the old focus does not count"
    );
}

// logic order: Close requests are handled before the bridge events of the same pass
#[test]
fn rule_order_close_request_runs_before_bridge_events() {
    let mut rig = Rig::opened();
    rig.close_request(WindowKind::Popup);
    rig.push(request());
    rig.tick();
    assert!(
        rig.is_open(),
        "closed by the request, then opened by the push"
    );
    assert_eq!(
        rig.trace(),
        vec![
            open_line("start", false, false),
            hide_line("close_request"),
            open_line("request", false, false)
        ]
    );
}

// logic order: bridge events are handled before the tray actions of the same pass
#[test]
fn rule_order_bridge_events_run_before_tray_actions() {
    let mut rig = Rig::closed();
    rig.push(request());
    rig.tray(&[TrayAction::Toggle]);
    rig.tick();
    assert!(
        !rig.is_open(),
        "opened by the push, then hidden by the click"
    );
    assert_eq!(
        rig.trace(),
        vec![open_line("request", false, false), hide_line("tray")]
    );
}

// logic order: a confirmed hide comes before everything else in the pass
#[test]
fn rule_order_hidden_confirmation_runs_before_a_new_open() {
    let mut rig = Rig::opened();
    rig.load(&[clip(1, "alpha")]);
    rig.key(egui::Key::Enter);
    rig.show();
    assert!(!rig.is_open());
    // the next pass opens the popup again: the pending paste must be dropped by that open
    // only after the confirmation had counted, and never be sent
    rig.push(request());
    rig.tick();
    assert!(rig.is_open());
    thread::sleep(Duration::from_millis(170));
    rig.tick();
    rig.tick();
    assert_open_commands(&rig.cmds());
}

// rule 11: a theme without a font scale keeps the scale that is stored
#[test]
fn rule_config_theme_without_a_scale_keeps_the_scale() {
    let mut rig = Rig::closed();
    rig.push(light_config(1.4));
    rig.tick();
    rig.push(config_reply(&json!({"ui": {"theme": {"mode": "dark"}}})));
    rig.show();
    rig.show();
    {
        let config = rig.app.config().lock().expect("settings");
        assert!(config.dark);
        assert!(
            (config.font_scale - 1.4).abs() < 1e-6,
            "scale {}",
            config.font_scale
        );
    }
    assert!(is_dark(&rig.popup.ctx));
    assert_zoom(&rig.popup.ctx, 1.4, "popup");
}

// rule 11: an empty popup.keys map brings the default keys back
#[test]
fn rule_config_empty_key_map_restores_the_defaults() {
    let mut rig = Rig::opened();
    rig.push(config_reply(
        &json!({"popup": {"keys": {"refresh": "ctrl+End"}}}),
    ));
    rig.tick();
    rig.key(egui::Key::F5);
    rig.show();
    assert_eq!(rig.cmds(), vec![], "overridden");
    rig.push(config_reply(&json!({"popup": {"keys": {}}})));
    rig.tick();
    rig.key(egui::Key::F5);
    rig.show();
    assert_compact_begin(&rig.cmds());
}

// rule 4: the confirmed key "popup.keys" settles the staged "popup.keys.*" edit
#[test]
fn rule_config_applied_popup_keys_settles_the_wildcard_edit() {
    let mut rig = Rig::closed();
    rig.push(config_reply(&json!({"popup": {"keys": {"refresh": "F6"}}})));
    rig.tick();
    {
        let mut config = rig.app.config().lock().expect("settings");
        let doc = config.doc.as_mut().expect("the document");
        doc.stage("popup.keys.*", json!({"refresh": "F7"}))
            .expect("a valid chord map");
        assert!(doc.is_dirty("popup.keys.*"));
        config.pending_apply = vec!["popup.keys".to_string()];
    }
    rig.push(UiEvent::ConfigApplied("popup.keys".to_string()));
    rig.tick();
    let config = rig.app.config().lock().expect("settings");
    assert!(
        config.pending_apply.is_empty(),
        "{:?}",
        config.pending_apply
    );
    let doc = config.doc.as_ref().expect("the document");
    assert!(!doc.is_dirty("popup.keys.*"));
    assert_eq!(doc.get("popup.keys"), Some(&json!({"refresh": "F7"})));
}

#[test]
fn fix_x4_capped_queue_requests_one_follow_up_without_losing_order() {
    let mut rig = Rig::closed();
    rig.warm_up();
    // Count explicit requests; egui also calls the integration callback while
    // consuming the previous zero-delay request at the start of a pass.
    rig.popup.ctx.set_request_repaint_callback(|_| {});
    let requests = Arc::clone(&rig.requests);
    rig.popup.ctx.set_repaint_observer(move |info| {
        requests
            .lock()
            .expect("requests")
            .push(Request { delay: info.delay });
    });
    for index in 0..(DRAIN_CAP + 3) {
        rig.push(UiEvent::ConfigError(format!("queued {index}")));
    }
    rig.tick();
    assert_eq!(
        rig.error().as_deref(),
        Some(format!("queued {}", DRAIN_CAP - 1).as_str()),
        "closed ConfigError routes to shell"
    );
    assert_eq!(
        rig.config_error(),
        "",
        "closed ConfigError must not fill Settings"
    );
    let requests = rig.requests();
    assert_eq!(requests.iter().filter(|r| r.delay.is_zero()).count(), 1);
    rig.tick();
    assert_eq!(
        rig.error().as_deref(),
        Some(format!("queued {}", DRAIN_CAP + 2).as_str()),
        "second batch routes to shell"
    );
    assert_eq!(
        rig.config_error(),
        "",
        "closed ConfigError still not in Settings"
    );
    assert!(rig.requests().iter().all(|r| !r.delay.is_zero()));
    for n in 0..DRAIN_CAP {
        rig.push(UiEvent::Error(format!("final {n}")));
    }
    rig.tick();
    assert_eq!(rig.error().as_deref(), Some("final 31"));
    assert!(rig.requests().iter().all(|r| !r.delay.is_zero()));
}

#[test]
fn fix_x7_single_and_double_tray_click_each_toggle_once() {
    use clipmonger_app_ui::tray_logic::{left_button_action, LeftButtonEvent};
    for sequence in [
        vec![LeftButtonEvent::Down, LeftButtonEvent::Up],
        vec![
            LeftButtonEvent::Down,
            LeftButtonEvent::Up,
            LeftButtonEvent::DoubleClick,
            LeftButtonEvent::Up,
        ],
    ] {
        let mut rig = Rig::closed();
        let start = Instant::now();
        let actions: Vec<_> = sequence
            .iter()
            .enumerate()
            .filter_map(|(i, event)| {
                left_button_action(*event, start + Duration::from_millis(i as u64 * 50))
            })
            .collect();
        assert_eq!(actions.len(), 1, "{sequence:?}");
        rig.tray(&actions);
        rig.tick();
        assert!(rig.is_open(), "{sequence:?}");
        assert_open_commands(&rig.cmds());
    }
}

#[test]
fn fix_fc3_closing_settings_clears_old_messages_before_reopening() {
    for visible in [false, true] {
        for event in [
            UiEvent::Error("old command error".to_string()),
            UiEvent::ConfigError("old configuration error".to_string()),
            UiEvent::BackupDone("old backup path".to_string()),
        ] {
            let mut rig = Rig::closed();
            rig.tray(&[TrayAction::OpenConfig]);
            rig.tick();
            rig.push(event);
            rig.tick();
            let old = rig.app.config().lock().expect("settings").error.clone();
            assert!(
                !old.is_empty(),
                "the Settings reply sets a message before close"
            );
            rig.close_request(WindowKind::Settings);
            rig.pass(WindowKind::Settings, visible);
            assert!(!rig.desired().settings_open);
            assert_eq!(rig.app.config().lock().expect("settings").error, "");
            rig.tray(&[TrayAction::OpenConfig]);
            rig.tick();
            assert!(!rig.show_settings().painted(&old));
        }
    }
}

#[test]
fn fix_fc3_closed_settings_drops_late_replies_after_pending_apply() {
    for visible in [false, true] {
        let mut rig = Rig::closed();
        rig.tray(&[TrayAction::OpenConfig]);
        rig.tick();
        let _ = rig.cmds();
        {
            let mut config = rig.app.config().lock().expect("settings");
            config.pending_apply = vec!["popup.hotkey".to_string()];
            config.error = "pending".to_string();
        }
        rig.close_request(WindowKind::Settings);
        rig.pass(WindowKind::Settings, visible);
        assert!(!rig.desired().settings_open);
        assert_eq!(rig.app.config().lock().expect("settings").error, "");
        assert!(
            rig.app
                .config()
                .lock()
                .expect("settings")
                .pending_apply
                .is_empty(),
            "close clears pending_apply"
        );
        rig.push(UiEvent::BackupDone("/tmp/late.zip".to_string()));
        rig.tick();
        assert_eq!(
            rig.config_error(),
            "",
            "late BackupDone must not fill closed Settings"
        );
        assert_eq!(rig.cmds(), vec![]);
        rig.push(UiEvent::RestoreDone { warning: None });
        rig.tick();
        assert_eq!(
            rig.config_error(),
            "",
            "late RestoreDone must not fill closed Settings"
        );
        assert_eq!(
            rig.cmds(),
            vec![UiCmd::ConfigGetAfterRestore],
            "restore still refreshes history"
        );
        rig.push(UiEvent::ConfigError("late config error".to_string()));
        rig.tick();
        assert_eq!(
            rig.config_error(),
            "",
            "late ConfigError must not fill closed Settings"
        );
        assert_eq!(
            rig.error().as_deref(),
            Some("late config error"),
            "late ConfigError routes to shell"
        );
        rig.push(UiEvent::Error("late error".to_string()));
        rig.tick();
        assert_eq!(
            rig.config_error(),
            "",
            "late Error must not fill closed Settings"
        );
        assert_eq!(rig.error().as_deref(), Some("late error"));
        assert!(
            rig.app
                .config()
                .lock()
                .expect("settings")
                .pending_apply
                .is_empty(),
            "late error must not leave pending_apply"
        );
        rig.tray(&[TrayAction::OpenConfig]);
        rig.tick();
        let painted = rig.show_settings().texts.join(" ");
        assert!(
            !painted.contains("late"),
            "reopening must not replay dropped late messages"
        );
        assert!(
            !painted.contains("pending"),
            "reopening must not replay pending text"
        );
    }
}

#[test]
fn r1_legacy_groups_renders_once_in_general_and_excluded_from_permanent_clips() {
    let mut rig = Rig::closed();
    // Prepare a doc that contains the legacy key and other history keys so rows exist
    let snapshot = serde_json::json!({
        "config": {
            "history": {"max_items": 100},
            "popup": {"hotkey": "ctrl+alt+h"},
            "permanents": {"groups": ["Work"]},
            "monitor": {"formats": ["text/plain"]}
        }
    });
    rig.push(UiEvent::Config(snapshot));
    rig.tick();
    rig.tray(&[TrayAction::OpenConfig]);
    rig.tick();
    let _ = rig.cmds();
    // History tab should not contain legacy row
    {
        rig.app.config().lock().expect("settings").tab = "History".to_string();
        let pass = rig.show_settings();
        let hits = pass
            .texts
            .iter()
            .filter(|t| *t == strings::text("config.permanents_groups"))
            .count();
        assert_eq!(hits, 0, "History tab must not show legacy groups");
    }
    // General tab must show legacy row exactly once
    {
        rig.app.config().lock().expect("settings").tab = "General".to_string();
        rig.app.config().lock().expect("settings").filter.clear();
        let pass = rig.show_settings();
        let hits = pass
            .texts
            .iter()
            .filter(|t| *t == strings::text("config.permanents_groups"))
            .count();
        assert_eq!(
            hits, 1,
            "General must show legacy row once: {:?}",
            pass.texts
        );
    }
    // PermanentClips tab must exclude legacy row
    {
        rig.app.config().lock().expect("settings").tab = "PermanentClips".to_string();
        rig.app.config().lock().expect("settings").filter.clear();
        let pass = rig.show_settings();
        let hits = pass
            .texts
            .iter()
            .filter(|t| *t == strings::text("config.permanents_groups"))
            .count();
        assert_eq!(
            hits, 0,
            "PermanentClips must not show legacy row: {:?}",
            pass.texts
        );
    }
    // Filtered rendering: General with filter "groups" still shows once
    {
        rig.app.config().lock().expect("settings").tab = "General".to_string();
        rig.app.config().lock().expect("settings").filter = "groups".to_string();
        let pass = rig.show_settings();
        let hits = pass
            .texts
            .iter()
            .filter(|t| *t == strings::text("config.permanents_groups"))
            .count();
        assert_eq!(
            hits, 1,
            "General filtered 'groups' must show legacy row once"
        );
    }
    // A filter searches every tab: on PermanentClips the legacy row still
    // shows exactly once (under General), never twice.
    {
        rig.app.config().lock().expect("settings").tab = "PermanentClips".to_string();
        rig.app.config().lock().expect("settings").filter = "groups".to_string();
        let pass = rig.show_settings();
        let hits = pass
            .texts
            .iter()
            .filter(|t| *t == strings::text("config.permanents_groups"))
            .count();
        assert_eq!(
            hits, 1,
            "filtered 'groups' must show legacy row once from any tab"
        );
    }
}

#[test]
fn settings_filter_searches_every_tab_under_tab_headings() {
    let mut rig = Rig::closed();
    rig.push(UiEvent::Config(json!({"config": {
        "history": {"max_items": 100},
        "popup": {"hotkey": "ctrl+alt+h"}
    }})));
    rig.tick();
    rig.tray(&[TrayAction::OpenConfig]);
    rig.tick();
    let _ = rig.cmds();
    rig.app.config().lock().expect("settings").tab = "History".to_string();
    let hotkey = strings::text("config.popup_hotkey");
    let max_items = strings::text("config.history_max_items");
    let popup = strings::text("config.tab_popup");
    let count = |texts: &[String], wanted: &str| texts.iter().filter(|t| *t == wanted).count();

    rig.app.config().lock().expect("settings").filter = "HOTKEY".to_string();
    let pass = rig.show_settings();
    assert_eq!(count(&pass.texts, hotkey), 1, "{:?}", pass.texts);
    assert_eq!(count(&pass.texts, max_items), 0, "{:?}", pass.texts);
    // Tab button plus the group heading.
    assert_eq!(count(&pass.texts, popup), 2, "{:?}", pass.texts);

    rig.app.config().lock().expect("settings").filter.clear();
    let pass = rig.show_settings();
    assert_eq!(count(&pass.texts, hotkey), 0, "{:?}", pass.texts);
    assert_eq!(count(&pass.texts, max_items), 1, "{:?}", pass.texts);
    assert_eq!(count(&pass.texts, popup), 1, "{:?}", pass.texts);
}

#[test]
fn r1_visible_rows_still_partition_the_legacy_config_key() {
    let tabs = [
        config_model::Tab::History,
        config_model::Tab::Popup,
        config_model::Tab::Advanced,
        config_model::Tab::PermanentClips,
        config_model::Tab::Privacy,
        config_model::Tab::Tray,
        config_model::Tab::Startup,
        config_model::Tab::General,
    ];
    assert_eq!(
        tabs.iter()
            .map(|tab| config_model::visible_rows(*tab).len())
            .sum::<usize>(),
        config_model::ROWS.len()
    );
    let keys = |tab| {
        config_model::visible_rows(tab)
            .into_iter()
            .map(|i| config_model::ROWS[i].key)
            .collect::<Vec<_>>()
    };
    assert!(keys(config_model::Tab::General).contains(&"permanents.groups"));
    assert!(!keys(config_model::Tab::PermanentClips).contains(&"permanents.groups"));
}

#[test]
fn r1_db_group_button_sends_immediately_without_applying_legacy_config() {
    let mut rig = Rig::closed();
    rig.push(UiEvent::Config(
        json!({"config": {"permanents": {"groups": ["Work"]}}}),
    ));
    rig.tick();
    rig.tray(&[TrayAction::OpenConfig]);
    rig.tick();
    let _ = rig.cmds();
    // The DB manager must send immediately, independently of staged config.
    {
        let mut cfg = rig.app.config().lock().expect("settings");
        cfg.tab = "PermanentClips".to_string();
        cfg.filter.clear();
        cfg.perm_group_new = "NewGroup".to_string();
        cfg.doc
            .as_mut()
            .expect("doc")
            .stage("permanents.groups", json!(["LegacyOnly"]))
            .expect("stage legacy groups");
    }
    let size = egui::vec2(960.0, 680.0);
    settings_frame(&mut rig, size, 1.0, Vec::new());
    click_settings_button(&mut rig, size, 1.0, strings::text("perm.new_group"));
    assert!(matches!(rig.cmds().as_slice(), [UiCmd::PermGroupCreate(name)] if name == "NewGroup"));
    assert!(rig
        .app
        .config()
        .lock()
        .expect("settings")
        .doc
        .as_ref()
        .expect("doc")
        .is_dirty("permanents.groups"));
}

/// Native logical dimensions become physical pixels through OS scale, then
/// egui points through OS scale times the app's settled font zoom.
fn settings_frame(
    rig: &mut Rig,
    native_size: egui::Vec2,
    os_scale: f32,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    let physical_size = native_size * os_scale;
    let pixels_per_point = os_scale * rig.settings.ctx.zoom_factor();
    let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, physical_size / pixels_per_point);
    let mut info = rig.settings.info.clone();
    info.native_pixels_per_point = Some(os_scale);
    let mut output = rig.settings.ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(viewport),
            viewports: std::iter::once((egui::ViewportId::ROOT, info)).collect(),
            events,
            ..egui::RawInput::default()
        },
        |ui| {
            rig.app.logic(ui.ctx(), WindowKind::Settings);
            rig.app.ui(ui, WindowKind::Settings);
        },
    );
    output.textures_delta.clear();
    output
}

fn settings_text_bounds(
    shape: &egui::Shape,
    clip: egui::Rect,
    out: &mut Vec<(String, egui::Rect, egui::Rect)>,
) {
    match shape {
        egui::Shape::Text(text) => out.push((
            text.galley.text().to_string(),
            egui::Rect::from_min_size(text.pos, text.galley.size()),
            clip,
        )),
        egui::Shape::Vec(shapes) => {
            for shape in shapes {
                settings_text_bounds(shape, clip, out);
            }
        }
        _ => {}
    }
}

/// Locate painted text, hover it, then inspect egui's actual widget response.
/// This also fails for disabled, clipped, or occluded controls.
fn settings_button(
    rig: &mut Rig,
    native_size: egui::Vec2,
    os_scale: f32,
    label: &str,
) -> egui::Response {
    let output = settings_frame(rig, native_size, os_scale, Vec::new());
    let mut texts = Vec::new();
    for shape in &output.shapes {
        settings_text_bounds(&shape.shape, shape.clip_rect, &mut texts);
    }
    let (_, text, clip) = texts
        .iter()
        .find(|(text, _, _)| text == label)
        .unwrap_or_else(|| panic!("missing button text {label:?}; painted {texts:?}"));
    let viewport = rig.settings.ctx.viewport_rect();
    assert!(
        viewport.contains_rect(*text),
        "{label:?} text {text:?} outside viewport {viewport:?}"
    );
    assert!(
        clip.contains_rect(*text),
        "{label:?} text {text:?} outside clip {clip:?}"
    );
    settings_frame(
        rig,
        native_size,
        os_scale,
        vec![egui::Event::PointerMoved(text.center())],
    );
    let hovered = rig
        .settings
        .ctx
        .interaction_snapshot(|s| s.hovered.iter().copied().collect::<Vec<_>>());
    let response = hovered
        .into_iter()
        .filter_map(|id| rig.settings.ctx.read_response(id))
        .find(|r| r.sense.senses_click() && r.rect.contains_rect(*text))
        .unwrap_or_else(|| panic!("no actual clickable response for {label:?} at {text:?}"));
    assert!(response.enabled(), "{label:?} must be enabled");
    assert!(
        viewport.contains_rect(response.rect),
        "{label:?} widget {:?} outside viewport {viewport:?}",
        response.rect
    );
    assert!(
        clip.contains_rect(response.rect),
        "{label:?} widget {:?} outside clip {clip:?}",
        response.rect
    );
    println!("control {label:?}: widget={:?} text={text:?} clip={clip:?} viewport={viewport:?} enabled={}", response.rect, response.enabled());
    response
}

fn click_settings_button(rig: &mut Rig, size: egui::Vec2, os_scale: f32, label: &str) {
    let response = settings_button(rig, size, os_scale, label);
    let pos = response.rect.center();
    for pressed in [true, false] {
        settings_frame(
            rig,
            size,
            os_scale,
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            }],
        );
    }
    assert_eq!(
        rig.settings.ctx.interaction_snapshot(|s| s.clicked),
        Some(response.id),
        "must click {label:?}"
    );
}

fn settings_layout_controls(font_scale: f32, native_height: f32) {
    let mut rig = Rig::closed();
    rig.push(UiEvent::Config(json!({"config": {
        "history": {"max_items": 100, "max_age_days": 30},
        "popup": {"hotkey": "ctrl+alt+h", "preview_width": 400},
        "monitor": {"formats": ["text/plain"], "max_bytes": 1000},
        "permanents": {"groups": ["Work"]},
        "privacy": {"strict": false}, "tray": {"enabled": true},
        "autostart": {"enabled": false},
        "ui": {"theme": {"mode": "dark", "font_scale": font_scale}}
    }})));
    rig.tick();
    rig.tray(&[TrayAction::OpenConfig]);
    rig.tick();
    rig.push(UiEvent::Permanents(json!({"permanents": [permanent_json(1, "Work", "ClipOne"), permanent_json(2, "Work", "ClipTwo")]})));
    rig.push(UiEvent::PermGroups(
        json!({"groups": [{"name": "Work", "system": false, "count": 2}]}),
    ));
    rig.tick();
    let size = egui::vec2(960.0, native_height);
    for _ in 0..3 {
        settings_frame(&mut rig, egui::vec2(960.0, 680.0), 1.0, Vec::new());
    }
    settings_frame(&mut rig, size, 1.0, Vec::new());
    assert!((rig.settings.ctx.zoom_factor() - font_scale).abs() < 1e-6);
    assert!((rig.settings.ctx.pixels_per_point() - font_scale).abs() < 1e-6);
    let expected_viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, size / font_scale);
    assert_eq!(
        rig.settings.ctx.input(egui::InputState::viewport_rect),
        expected_viewport
    );
    assert_eq!(
        rig.settings.ctx.viewport_rect(),
        egui::emath::GuiRounding::round_ui(expected_viewport)
    );
    let _ = rig.cmds();
    for (tab, draft) in [
        ("History", false),
        ("PermanentClips", false),
        ("PermanentClips", true),
    ] {
        {
            let mut cfg = rig.app.config().lock().expect("settings");
            cfg.tab = tab.to_string();
            if draft {
                cfg.perm_vm.begin_new();
            }
            let doc = cfg.doc.as_mut().expect("doc");
            assert_eq!(doc.get("history.max_items"), Some(&json!(100)));
            doc.stage("history.max_items", json!(999))
                .expect("dirty edit");
        }
        // Resize the same dirty editor from the default native dimensions.
        if native_height < 680.0 {
            settings_frame(&mut rig, egui::vec2(960.0, 680.0), 1.0, Vec::new());
        }
        settings_frame(&mut rig, size, 1.0, Vec::new());
        println!("case native=960x{native_height} os=1 font={font_scale} tab={tab} draft={draft}");
        // Both controls must be enabled and fully visible before activation.
        settings_button(&mut rig, size, 1.0, strings::text("config.apply"));
        settings_button(&mut rig, size, 1.0, strings::text("config.revert"));
        click_settings_button(&mut rig, size, 1.0, strings::text("config.apply"));
        assert!(
            matches!(rig.cmds().as_slice(), [UiCmd::ConfigSet(key, value), UiCmd::ConfigGet] if key == "history.max_items" && value == &json!(999))
        );
        assert_eq!(
            rig.app.config().lock().expect("settings").pending_apply,
            ["history.max_items"]
        );
        click_settings_button(&mut rig, size, 1.0, strings::text("config.revert"));
        assert!(
            rig.cmds().is_empty(),
            "Revert must not send config or DB commands"
        );
        let cfg = rig.app.config().lock().expect("settings");
        assert_eq!(
            cfg.doc.as_ref().expect("doc").dirty_keys(),
            Vec::<String>::new()
        );
        assert_eq!(
            cfg.doc.as_ref().expect("doc").get("history.max_items"),
            Some(&json!(100))
        );
        assert_eq!(
            cfg.perm_vm.draft.is_some(),
            draft,
            "config actions preserve permanent draft"
        );
        assert_eq!(cfg.perm_vm.permanents.len(), 2);
    }
}

#[test]
fn l1_settings_apply_revert_visible_at_native_size() {
    settings_layout_controls(1.0, 680.0);
}
#[test]
fn l1_settings_native_130() {
    settings_layout_controls(1.3, 680.0);
}
#[test]
fn l1_settings_native_160() {
    settings_layout_controls(1.6, 680.0);
}
#[test]
fn l1_settings_short_100() {
    settings_layout_controls(1.0, 400.0);
}
#[test]
fn l1_settings_short_130() {
    settings_layout_controls(1.3, 400.0);
}
#[test]
fn l1_settings_short_160() {
    settings_layout_controls(1.6, 400.0);
}

/// No filled rect other than the panel lies under `rect` (text drawn without
/// a tinted box).
fn assert_plain_on_panel(
    shapes: &[egui::epaint::ClippedShape],
    rect: egui::Rect,
    panel_fill: egui::Color32,
) {
    fn filled_rects(shape: &egui::Shape, out: &mut Vec<(egui::Rect, egui::Color32)>) {
        match shape {
            egui::Shape::Rect(rect) if rect.fill != egui::Color32::TRANSPARENT => {
                out.push((rect.rect, rect.fill));
            }
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    filled_rects(shape, out);
                }
            }
            _ => {}
        }
    }
    let mut fills = Vec::new();
    for clipped in shapes {
        filled_rects(&clipped.shape, &mut fills);
    }
    assert!(
        fills
            .iter()
            .all(|(fill_rect, fill)| *fill == panel_fill || !fill_rect.intersects(rect)),
        "{rect:?} must be plain text on the panel: {fills:?}"
    );
}

#[test]
fn fix_n3_long_history_keeps_error_and_settings_painted_inside_window() {
    type Painted = (String, egui::Rect, egui::Rect, Option<egui::Color32>);
    fn text_bounds(shape: &egui::Shape, clip: egui::Rect, out: &mut Vec<Painted>) {
        match shape {
            egui::Shape::Text(text) => out.push((
                text.galley.text().to_string(),
                egui::Rect::from_min_size(text.pos, text.galley.size()),
                clip,
                text.galley.job.sections.first().map(|s| s.format.color),
            )),
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    text_bounds(shape, clip, out);
                }
            }
            _ => {}
        }
    }
    let mut rig = Rig::opened();
    let clips: Vec<_> = (1..=60)
        .map(|n| clip(n, &format!("History row {n}")))
        .collect();
    rig.push(history(&clips));
    rig.push(UiEvent::PasteFailed(
        "paste failed with a long history".to_string(),
    ));
    rig.tick();
    let window = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(480.0, 360.0));
    for _ in 0..2 {
        let mut output = rig.popup.ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(window),
                ..egui::RawInput::default()
            },
            |ui| {
                rig.app.logic(ui.ctx(), WindowKind::Popup);
                rig.app.ui(ui, WindowKind::Popup);
            },
        );
        output.textures_delta.clear();
        let mut texts = Vec::new();
        for clipped in &output.shapes {
            text_bounds(&clipped.shape, clipped.clip_rect, &mut texts);
        }
        let status = rig.app.vm().status_line();
        let footer = [
            "paste failed with a long history",
            strings::text("config.open_config"),
            status.as_str(),
        ];
        for expected in footer {
            assert!(
                texts.iter().any(|(text, rect, clip, _)| {
                    text == expected && window.contains_rect(*rect) && clip.contains_rect(*rect)
                }),
                "{expected:?} must be painted inside the window and its clip: {texts:?}"
            );
        }
        let error_rect = texts
            .iter()
            .find(|(text, ..)| text == "paste failed with a long history")
            .map(|(_, rect, ..)| *rect)
            .expect("error text painted");
        let panel_fill = rig.popup.ctx.global_style().visuals.panel_fill;
        assert_plain_on_panel(&output.shapes, error_rect, panel_fill);
        let error_fg = rig.popup.ctx.global_style().visuals.error_fg_color;
        assert!(
            texts.iter().any(|(text, _, _, color)| {
                text == "paste failed with a long history" && *color == Some(error_fg)
            }),
            "the paste error must be painted in the error colour: {texts:?}"
        );
        let footer_rects: Vec<egui::Rect> = texts
            .iter()
            .filter(|(text, ..)| footer.contains(&text.as_str()))
            .map(|(_, rect, ..)| *rect)
            .collect();
        for (text, rect, clip, _) in texts
            .iter()
            .filter(|(text, ..)| text.starts_with("History row"))
        {
            let visible = rect.intersect(*clip);
            if !visible.is_positive() {
                continue;
            }
            assert!(
                footer_rects
                    .iter()
                    .all(|footer| !footer.intersects(visible)),
                "{text:?} at {visible:?} overlaps the footer {footer_rects:?}"
            );
        }
    }
}

// raise-design: actual reported scenario - open hotkey, disable dismiss, another hotkey exactly one activation with new context
#[test]
fn rule_raise_hotkey_while_open_requests_activation_with_new_context() {
    let mut rig = Rig::closed();
    rig.push(hotkey(1, 2));
    rig.tick();
    assert!(rig.is_open());
    let _ = rig.cmds();
    rig.push(config_reply(
        &json!({"popup": {"dismiss_on_focus_loss": false}}),
    ));
    rig.tick();
    rig.focus(Some(true));
    rig.tick();
    rig.focus(Some(false));
    rig.tick();
    assert!(rig.is_open(), "dismiss disabled so still open");
    rig.push(hotkey(3, 4));
    rig.tick();
    assert!(rig.is_open());
    assert_eq!(
        rig.context(),
        PasteContext {
            target: Some(3),
            gesture: Some(4)
        }
    );
    assert!(
        rig.app.take_popup_activation(),
        "second hotkey while open must request activation"
    );
    rig.tick();
    assert!(!rig.app.take_popup_activation(), "idle must not re-request");
}

// raise-design: request while open preserves search/selection/context and no reopen
#[test]
fn rule_raise_request_while_open_preserves_state_and_no_reopen() {
    let mut rig = Rig::closed();
    rig.push(hotkey(1, 2));
    rig.tick();
    let _ = rig.cmds();
    rig.push(config_reply(
        &json!({"popup": {"dismiss_on_focus_loss": false}}),
    ));
    rig.tick();
    rig.focus(Some(true));
    rig.tick();
    rig.focus(Some(false));
    rig.tick();
    rig.app.vm_mut().query = "hello".to_string();
    rig.app.vm_mut().selected = 2;
    let before_context = rig.context();
    let before_trace = rig.trace();
    rig.push(request());
    rig.tick();
    assert!(rig.is_open());
    assert_eq!(
        rig.context(),
        before_context,
        "request while open keeps context"
    );
    assert_eq!(rig.app.vm().query, "hello");
    assert_eq!(rig.app.vm().selected, 2);
    assert!(rig.app.take_popup_activation());
    assert_eq!(rig.trace(), before_trace, "no open transition");
    assert_eq!(rig.cmds(), vec![], "request while open sends no refresh");
}

// raise-design: hide clears pending activation
#[test]
fn rule_raise_hide_clears_pending() {
    let mut rig = Rig::opened();
    rig.push(hotkey(5, 6));
    rig.tick();
    // hide must clear pending without intervening drain
    rig.push(toggle());
    rig.tick();
    assert!(!rig.is_open());
    assert!(!rig.app.take_popup_activation(), "hide must clear pending");
    // true same-drain raise+hide in one tick
    let mut rig2 = Rig::closed();
    rig2.push(hotkey(1, 2));
    rig2.tick();
    rig2.push(config_reply(
        &json!({"popup": {"dismiss_on_focus_loss": false}}),
    ));
    rig2.tick();
    rig2.focus(Some(true));
    rig2.tick();
    rig2.focus(Some(false));
    rig2.tick();
    rig2.push(hotkey(3, 4));
    rig2.push(toggle());
    rig2.tick();
    assert!(!rig2.is_open(), "hide must close");
    assert!(
        !rig2.app.take_popup_activation(),
        "same-tick hide must clear pending"
    );
}

// raise-design: batch coalescing - several Raises before drain coalesce to one
#[test]
fn rule_raise_batch_coalesces() {
    let mut rig = Rig::closed();
    rig.push(hotkey(1, 2));
    rig.tick();
    let _ = rig.cmds();
    rig.push(config_reply(
        &json!({"popup": {"dismiss_on_focus_loss": false}}),
    ));
    rig.tick();
    rig.focus(Some(true));
    rig.tick();
    rig.focus(Some(false));
    rig.tick();
    rig.push(hotkey(3, 4));
    rig.push(hotkey(5, 6));
    rig.tick();
    assert_eq!(
        rig.context(),
        PasteContext {
            target: Some(5),
            gesture: Some(6)
        }
    );
    assert!(rig.app.take_popup_activation());
    assert!(!rig.app.take_popup_activation(), "coalesced to one");
}

// raise-design: tray Show while open preserves and activates
#[test]
fn rule_raise_tray_show_while_open_preserves_and_activates() {
    let mut rig = Rig::opened();
    rig.push(hotkey(10, 11));
    rig.tick();
    let _ = rig.cmds();
    // drain hotkey latch so tray's own request is verified
    assert!(rig.app.take_popup_activation());
    let _ = rig.app.take_popup_activation();
    rig.push(config_reply(
        &json!({"popup": {"dismiss_on_focus_loss": false}}),
    ));
    rig.tick();
    rig.focus(Some(true));
    rig.tick();
    rig.focus(Some(false));
    rig.tick();
    rig.app.vm_mut().query = "search".to_string();
    rig.app.vm_mut().selected = 1;
    let ctx = rig.context();
    let before_trace = rig.trace();
    rig.tray(&[TrayAction::OpenPopup]);
    rig.tick();
    assert!(rig.is_open());
    assert_eq!(rig.context(), ctx, "tray show preserves context");
    assert_eq!(rig.app.vm().query, "search");
    assert_eq!(rig.app.vm().selected, 1);
    assert_eq!(rig.trace(), before_trace, "no open trace");
    assert_eq!(rig.cmds(), vec![], "no refresh");
    assert!(
        rig.app.take_popup_activation(),
        "tray show must request activation"
    );
}

#[test]
fn runtime_log_reducer_keeps_selection_errors_and_readable_lines() {
    let mut rig = Rig::closed();
    {
        let mut config = rig.app.config().lock().unwrap();
        assert_eq!(config.log_source, "audit");
        config.log_lines = vec!["old audit".into()];
        config.select_log_source("daemon");
        assert_eq!(config.log_lines, Vec::<String>::new());
    }
    rig.push(UiEvent::LogsTail {
        source: "audit".into(),
        lines: vec!["late audit".into()],
        sink_error: None,
    });
    rig.tick();
    assert!(
        rig.app.config().lock().unwrap().log_lines.is_empty(),
        "late reply cannot populate selected daemon stream"
    );
    rig.push(UiEvent::LogsTail {
        source: "daemon".into(),
        lines: vec!["readable daemon".into()],
        sink_error: Some("runtime_log.refused_file".into()),
    });
    rig.tick();
    {
        let config = rig.app.config().lock().unwrap();
        assert_eq!(config.log_lines, ["readable daemon"]);
        assert_eq!(config.log_error, "runtime_log.refused_file");
    }
    rig.app.config().lock().unwrap().tab = "about".into();
    let painted = rig.show_settings();
    assert!(painted.painted("daemon: runtime_log.refused_file"));
    assert!(
        painted.painted("readable daemon"),
        "sink failure does not hide readable lines"
    );
    rig.push(UiEvent::LogsTailError {
        source: "ui".into(),
        message: "late other error".into(),
    });
    rig.tick();
    assert_eq!(
        rig.app.config().lock().unwrap().log_error,
        "runtime_log.refused_file"
    );
    rig.push(UiEvent::LogsTailError {
        source: "daemon".into(),
        message: "read failed".into(),
    });
    rig.tick();
    {
        let config = rig.app.config().lock().unwrap();
        assert_eq!(config.log_lines, ["readable daemon"]);
        assert_eq!(config.log_error, "read failed");
    }
    rig.push(UiEvent::LogsTail {
        source: "daemon".into(),
        lines: vec!["healthy".into()],
        sink_error: None,
    });
    rig.tick();
    assert_eq!(rig.app.config().lock().unwrap().log_error, "");
    rig.app.config().lock().unwrap().select_log_source("ui");
    assert_eq!(
        rig.app.config().lock().unwrap().log_lines,
        Vec::<String>::new()
    );
}

#[test]
fn ui_own_sink_failure_is_painted_without_daemon_connectivity() {
    use clipmonger_core::runtime_log::{self, Role};
    let dir = std::env::temp_dir().join(format!(
        "cm-runtime-ui-reducer-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("logs"), "obstruction").unwrap();
    assert!(runtime_log::init_global(&dir, Role::Ui).is_err());
    let mut rig = Rig::closed();
    {
        let mut config = rig.app.config().lock().unwrap();
        config.tab = "about".into();
        config.select_log_source("ui");
        config.log_lines = vec!["last readable UI line".into()];
    }
    let pass = rig.show_settings();
    assert!(pass.painted("ui sink: runtime_log.refused_logs_directory"));
    assert!(pass.painted("last readable UI line"));
    std::fs::remove_dir_all(dir).unwrap();
}

fn active_list_rig(permanent: bool, count: u128) -> Rig {
    let mut rig = Rig::opened();
    rig.push(history(
        &(1..=count)
            .map(|n| clip(n, &format!("History row {n}")))
            .collect::<Vec<_>>(),
    ));
    rig.push(UiEvent::Permanents(json!({"permanents": (1..=count).map(|n| permanent_json(1000+n, "Work", &format!("Permanent row {n}"))).collect::<Vec<_>>()})));
    rig.push(UiEvent::PermGroups(
        json!({"groups": [{"name":"Work", "system":false,"count":count}]}),
    ));
    rig.tick();
    rig.app.config().lock().expect("config").perm_source = permanent.then_some(0);
    rig.show();
    rig.show();
    let _ = rig.cmds();
    rig
}

fn popup_widget(rig: &mut Rig, label: &str) -> egui::Response {
    let pass = rig.show();
    let (_, text, clip) = pass
        .bounds
        .iter()
        .rev()
        .find(|(text, _, _)| text == label)
        .unwrap_or_else(|| panic!("missing {label:?}: {:?}", pass.texts));
    let text = *text;
    let clip = *clip;
    assert!(
        clip.contains_rect(text),
        "{label:?} text {text:?} outside scroll clip {clip:?}"
    );
    assert!(rig.popup.ctx.viewport_rect().contains_rect(text));
    rig.input(egui::Event::PointerMoved(text.center()));
    rig.show();
    let hovered = rig
        .popup
        .ctx
        .interaction_snapshot(|s| s.hovered.iter().copied().collect::<Vec<_>>());
    let responses: Vec<_> = hovered
        .into_iter()
        .filter_map(|id| rig.popup.ctx.read_response(id))
        .collect();
    let response = responses
        .iter()
        .find(|r| r.sense.senses_click() && r.rect.contains(text.center()))
        .unwrap_or_else(|| panic!("actual row widget {label} text={text:?} hovered={responses:?}"))
        .clone();
    assert!(response.enabled());
    assert!(
        clip.contains_rect(response.rect),
        "widget {:?} outside {clip:?}",
        response.rect
    );
    assert!(rig.popup.ctx.viewport_rect().contains_rect(response.rect));
    println!("{label}: widget={:?} clip={clip:?}", response.rect);
    response
}

fn click_popup_widget(rig: &mut Rig, label: &str) {
    let response = popup_widget(rig, label);
    for pressed in [true, false] {
        rig.input(egui::Event::PointerButton {
            pos: response.rect.center(),
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        rig.show();
    }
    assert_eq!(
        rig.popup.ctx.interaction_snapshot(|s| s.clicked),
        Some(response.id)
    );
}

/// Press and release the primary button at `pos` on the popup.
fn click_popup_at(rig: &mut Rig, pos: egui::Pos2) {
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

fn picked_clip(rig: &mut Rig) -> Vec<Uuid> {
    finish_popup_pick(rig);
    rig.cmds()
        .into_iter()
        .filter_map(|cmd| match cmd {
            UiCmd::PasteClipTo(clip, ..) => Some(clip),
            _ => None,
        })
        .collect()
}

// A history row and a permanent row are hovered, highlighted and picked
// anywhere across the row, not only over their text.
#[test]
fn rule_list_rows_are_hovered_and_picked_across_their_width() {
    for permanent in [false, true] {
        let prefix = if permanent { "Permanent" } else { "History" };
        let label = format!("{prefix} row 2");
        let mut rig = active_list_rig(permanent, 3);
        let pass = rig.show();
        let text = pass
            .bounds
            .iter()
            .find(|(text, ..)| *text == label)
            .map(|(_, rect, _)| *rect)
            .expect("row text painted");
        let hover_fill = rig
            .popup
            .ctx
            .global_style()
            .visuals
            .widgets
            .hovered
            .weak_bg_fill;
        assert!(!pass.fills.contains(&hover_fill), "{prefix}: no hover yet");
        let row_end = egui::pos2(text.right() + 150.0, text.center().y);
        assert!(rig.popup.ctx.viewport_rect().contains(row_end));
        rig.input(egui::Event::PointerMoved(row_end));
        rig.show();
        let pass = rig.show();
        assert!(
            pass.fills.contains(&hover_fill),
            "{prefix}: hovering the row end highlights the row: {:?}",
            pass.fills
        );
        let hovered: Vec<egui::Response> = rig
            .popup
            .ctx
            .interaction_snapshot(|s| s.hovered.iter().copied().collect::<Vec<_>>())
            .into_iter()
            .filter_map(|id| rig.popup.ctx.read_response(id))
            .filter(|r| r.sense.senses_click())
            .collect();
        assert!(
            hovered
                .iter()
                .any(|r| r.rect.contains(row_end) && r.rect.contains(text.center())),
            "{prefix}: one clickable row spans its text and its end: {hovered:?}"
        );
        // The first click selects the row, the second picks it.
        click_popup_at(&mut rig, row_end);
        let selected = if permanent {
            rig.app.config().lock().expect("config").perm_vm.selected
        } else {
            rig.app.vm().selected
        };
        assert_eq!(selected, 1, "{prefix}: the first click selects the row");
        click_popup_at(&mut rig, row_end);
        finish_popup_pick(&mut rig);
        let commands = rig.cmds();
        if permanent {
            assert_eq!(commands, vec![UiCmd::ExecPermanent(id(1002))]);
        } else {
            assert!(
                matches!(commands.as_slice(), [UiCmd::PasteClipTo(picked, ..)] if *picked == id(2)),
                "{commands:?}"
            );
        }
    }
}

// A cut preview shows its full text in a tooltip; a whole one has none.
#[test]
fn rule_cut_history_rows_show_their_full_preview_on_hover() {
    let long = format!("Long row {}", "x".repeat(150));
    for (preview, tooltip) in [(long.as_str(), true), ("Short row", false)] {
        let mut rig = Rig::opened();
        rig.push(history(&[clip(1, preview)]));
        rig.tick();
        rig.popup
            .ctx
            .all_styles_mut(|style| style.interaction.tooltip_delay = 0.0);
        let pass = rig.show();
        let (shown, text) = pass
            .bounds
            .iter()
            .find(|(text, ..)| text.starts_with(&preview[..5]))
            .map(|(shown, rect, _)| (shown.clone(), *rect))
            .expect("row painted");
        assert_eq!(shown != preview, tooltip, "{preview:?} shown as {shown:?}");
        rig.input(egui::Event::PointerMoved(text.center()));
        rig.show();
        rig.show();
        let pass = rig.show();
        assert_eq!(
            pass.texts.iter().filter(|t| t.as_str() == preview).count(),
            usize::from(preview == shown) + usize::from(tooltip),
            "{preview:?}: tooltip {tooltip}: {:?}",
            pass.texts
        );
    }
}

// An image row shows only its thumbnail (no empty text box) and is picked
// by clicking the thumbnail or the rest of the row.
#[test]
fn rule_image_rows_are_picked_through_the_thumbnail() {
    for x_offset in [20.0, 200.0] {
        let mut rig = Rig::opened();
        let mut image = clip(2, "");
        image.formats = vec!["image/png".to_string()];
        rig.push(history(&[clip(1, "Text row"), image]));
        rig.tick();
        rig.show();
        let text_row = popup_widget(&mut rig, "Text row");
        let pass = rig.show();
        let list_clip = pass
            .bounds
            .iter()
            .find(|(text, ..)| text == "Text row")
            .map(|(_, _, clip)| *clip)
            .expect("text row painted");
        assert!(
            pass.bounds
                .iter()
                .all(|(text, _, clip)| *clip != list_clip || !text.is_empty()),
            "no empty text in the list: {:?}",
            pass.bounds
        );
        // The image row follows the text row; its thumbnail is 40 points tall.
        let pos = egui::pos2(
            text_row.rect.left() + x_offset,
            text_row.rect.bottom() + 20.0 + rig.popup.ctx.global_style().spacing.item_spacing.y,
        );
        click_popup_at(&mut rig, pos);
        assert_eq!(rig.app.vm().selected, 1, "click at x+{x_offset} selects");
        click_popup_at(&mut rig, pos);
        assert_eq!(picked_clip(&mut rig), vec![id(2)], "click at x+{x_offset}");
    }
}

#[test]
fn history_row_context_menu_handles_text_and_thumbnail_clicks() {
    for thumbnail in [false, true] {
        for pin in [true, false] {
            let mut rig = active_list_rig(false, 3);
            if thumbnail {
                // Only image rows carry a thumbnail slot.
                let images: Vec<Clip> = (1..=3)
                    .map(|n| {
                        let mut clip = clip(n, &format!("History row {n}"));
                        clip.formats = vec!["image/png".to_string()];
                        clip
                    })
                    .collect();
                rig.load(&images);
                rig.show();
            }
            // Resolve the actual clickable text child before aiming at it.
            let child = popup_widget(&mut rig, "History row 2");
            let pass = rig.show();
            let text = pass
                .bounds
                .iter()
                .find(|(label, _, _)| label == "History row 2")
                .unwrap()
                .1;
            assert!(child.rect.contains(text.center()));
            let position = if thumbnail {
                egui::pos2(text.left() - 24.0, text.center().y)
            } else {
                text.center()
            };
            rig.input(egui::Event::PointerMoved(position));
            rig.show();
            for pressed in [true, false] {
                rig.input(egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Secondary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
                rig.show();
            }
            let menu = rig.show();
            assert!(menu.painted(strings::text("popup.pin")));
            assert!(menu.painted(strings::text("popup.delete")));
            assert!(
                rig.cmds().is_empty(),
                "opening a menu must not activate the row"
            );
            click_popup_widget(
                &mut rig,
                strings::text(if pin { "popup.pin" } else { "popup.delete" }),
            );
            let expected = if pin {
                UiCmd::PinClip(id(2), true)
            } else {
                UiCmd::DeleteClip(id(2))
            };
            assert_eq!(rig.cmds(), vec![expected]);
            assert!(rig.is_open());
        }
    }
}

fn finish_popup_pick(rig: &mut Rig) {
    assert!(!rig.is_open(), "pick must close popup before execution");
    rig.tick();
    thread::sleep(Duration::from_millis(170));
    rig.tick();
}

#[test]
fn active_list_permanent_navigation_and_history_actions_are_isolated() {
    let mut rig = active_list_rig(true, 3);
    rig.app.vm_mut().selected = 1;
    for key in [egui::Key::ArrowDown, egui::Key::End] {
        rig.key(key);
        rig.show();
    }
    assert_eq!(rig.app.config().lock().expect("config").perm_vm.selected, 2);
    for (key, modifiers) in [
        (egui::Key::Delete, egui::Modifiers::NONE),
        (egui::Key::P, egui::Modifiers::CTRL),
        (egui::Key::E, egui::Modifiers::CTRL),
    ] {
        rig.input(key_event(key, true, modifiers));
        rig.show();
    }
    assert_eq!(rig.app.vm().selected, 1);
    assert_eq!(rig.app.vm().clips().len(), 3);
    assert_eq!(rig.cmds(), vec![]);
    // Move the pointer off the list so focused-row activation cannot consume Enter.
    rig.popup.ctx.memory_mut(|m| {
        if let Some(id) = m.focused() {
            m.surrender_focus(id);
        }
    });
    rig.key(egui::Key::Enter);
    rig.show();
    finish_popup_pick(&mut rig);
    assert_eq!(rig.cmds(), vec![UiCmd::ExecPermanent(id(1003))]);
}

fn filtered_permanent_pick(mouse: bool) {
    let mut rig = active_list_rig(true, 2);
    rig.app.vm_mut().query = "row 2".into();
    rig.app.vm_mut().selected = 1;
    rig.show();
    rig.show();
    if mouse {
        click_popup_widget(&mut rig, "Permanent row 2");
    } else {
        rig.key(egui::Key::Enter);
        rig.show();
    }
    finish_popup_pick(&mut rig);
    assert_eq!(rig.cmds(), vec![UiCmd::ExecPermanent(id(1002))]);
    assert_eq!(rig.app.vm().selected, 1);
}

#[test]
fn active_list_filtered_keyboard_activation() {
    filtered_permanent_pick(false);
}

#[test]
fn active_list_filtered_mouse_activation() {
    filtered_permanent_pick(true);
}

#[test]
fn active_list_empty_activation() {
    let mut rig = active_list_rig(true, 2);
    rig.app.vm_mut().query = "no matches".into();
    rig.key(egui::Key::Enter);
    rig.show();
    assert_eq!(rig.cmds(), vec![]);
}

#[test]
fn active_list_configured_chords_use_each_event_modifiers() {
    let mut rig = active_list_rig(true, 4);
    rig.push(config_reply(&json!({"popup":{"keys":{"navigate_down":"ctrl+j", "end":"shift+end", "home":"F21", "page_down":"F22", "page_up":"F23", "paste":"F24"}}})));
    rig.tick();
    let _ = rig.cmds();
    for (key, modifiers, selected) in [
        (egui::Key::J, egui::Modifiers::CTRL, 1),
        (egui::Key::End, egui::Modifiers::SHIFT, 3),
        (egui::Key::F21, egui::Modifiers::NONE, 0),
        (egui::Key::F22, egui::Modifiers::NONE, 3),
        (egui::Key::F23, egui::Modifiers::NONE, 0),
    ] {
        rig.input(key_event(key, true, modifiers));
        rig.input(key_event(key, false, egui::Modifiers::NONE));
        rig.input(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
        rig.show();
        assert_eq!(
            rig.app.config().lock().expect("config").perm_vm.selected,
            selected,
            "{key:?}"
        );
    }
    rig.key(egui::Key::F24);
    rig.show();
    finish_popup_pick(&mut rig);
    assert_eq!(rig.cmds(), vec![UiCmd::ExecPermanent(id(1001))]);
}

fn scroll_controls(permanent: bool) {
    let mut rig = active_list_rig(permanent, 60);
    let prefix = if permanent { "Permanent" } else { "History" };
    for (key, row) in [
        (egui::Key::End, 60),
        (egui::Key::Home, 1),
        (egui::Key::PageDown, 11),
    ] {
        rig.key(key);
        rig.show();
        rig.show();
        println!(
            "key={key:?} history selected={} permanent selected={}",
            rig.app.vm().selected,
            rig.app.config().lock().expect("config").perm_vm.selected
        );
        popup_widget(&mut rig, &format!("{prefix} row {row}"));
    }
    rig.key(egui::Key::End);
    rig.show();
    rig.show();
    let selected = popup_widget(&mut rig, &format!("{prefix} row 60"));
    // A real wheel event scrolls away from the selected row.
    rig.input(egui::Event::PointerMoved(selected.rect.center()));
    rig.input(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        source: egui::MouseWheelSource::Unknown,
        phase: egui::TouchPhase::Move,
        delta: egui::vec2(0.0, 450.0),
        modifiers: egui::Modifiers::NONE,
    });
    rig.show();
    for _ in 0..20 {
        rig.show();
    }
    let pass = rig.show();
    assert!(
        !pass
            .bounds
            .iter()
            .any(|(text, rect, clip)| text == &format!("{prefix} row 60")
                && clip.intersects(*rect)),
        "idle must not undo wheel scrolling"
    );
    rig.key(egui::Key::End);
    rig.show();
    rig.show();
    popup_widget(&mut rig, &format!("{prefix} row 60"));
    rig.key(egui::Key::Enter);
    rig.show();
    finish_popup_pick(&mut rig);
    let commands = rig.cmds();
    if permanent {
        assert_eq!(commands, vec![UiCmd::ExecPermanent(id(1060))]);
    } else {
        assert!(
            commands
                .iter()
                .any(|cmd| matches!(cmd, UiCmd::PasteClipTo(picked, ..) if *picked == id(60))),
            "{commands:?}"
        );
    }
}

#[test]
fn active_list_history_scroll_reveals_and_preserves_wheel() {
    scroll_controls(false);
}

#[test]
fn active_list_permanent_scroll_reveals_and_preserves_wheel() {
    scroll_controls(true);
}

#[test]
fn active_list_settings_reply_preserves_unfiltered_selection() {
    let mut rig = active_list_rig(true, 3);
    rig.app.vm_mut().query = "no popup matches".into();
    rig.tray(&[TrayAction::OpenConfig]);
    rig.tick();
    rig.app.config().lock().expect("config").perm_vm.selected = 2;
    rig.push(UiEvent::Permanents(json!({"permanents": (1..=3).map(|n| permanent_json(1000+n,"Work", &format!("Permanent row {n}"))).collect::<Vec<_>>()})));
    rig.push(UiEvent::PermGroups(
        json!({"groups":[{"name":"Work","system":false,"count":3}]}),
    ));
    rig.tick_settings();
    assert_eq!(
        rig.app
            .config()
            .lock()
            .expect("config")
            .perm_vm
            .activate_selected(),
        Some(id(1003))
    );
    rig.show();
    assert_eq!(
        rig.app
            .config()
            .lock()
            .expect("config")
            .perm_vm
            .activate_selected(),
        Some(id(1003)),
        "passive popup repaint must preserve the Settings selection"
    );
}

#[test]
fn active_list_search_editing_and_focused_widget_activation_stay_separate() {
    let mut rig = active_list_rig(true, 3);
    rig.app.vm_mut().selected = 2;
    rig.app.config().lock().expect("config").perm_vm.selected = 2;
    rig.input(egui::Event::Text("row 2".into()));
    rig.show();
    rig.show();
    assert_eq!(rig.app.vm().query, "row 2");
    assert_eq!(
        rig.app.vm().selected,
        2,
        "searching permanents leaves hidden history alone"
    );
    assert_eq!(rig.app.config().lock().expect("config").perm_vm.selected, 0);
    rig.key(egui::Key::Delete);
    rig.show();
    assert!(
        rig.cmds().is_empty(),
        "editing Delete must not remove hidden history"
    );
    let button = popup_widget(&mut rig, strings::text("config.open_config"));
    button.request_focus();
    rig.key(egui::Key::Enter);
    rig.show();
    assert!(rig.desired().settings_open);
    assert!(rig.is_open(), "focused button must not additionally paste");
    assert!(!rig
        .cmds()
        .iter()
        .any(|c| matches!(c, UiCmd::ExecPermanent(_) | UiCmd::PasteClipTo(..))));
}

#[test]
fn active_list_source_switch_and_replaced_pending_rows_settle() {
    let mut rig = active_list_rig(false, 60);
    rig.key(egui::Key::End);
    rig.show();
    // Replace the row that has an outstanding reveal before it can paint.
    rig.push(history(&[clip(99, "Replacement")]));
    rig.show();
    rig.show();
    popup_widget(&mut rig, "Replacement");
    rig.input(key_event(egui::Key::Num1, true, egui::Modifiers::ALT));
    rig.show();
    rig.show();
    popup_widget(&mut rig, "Permanent row 1");
    rig.key(egui::Key::End);
    rig.show();
    // Same-sized permanent replacement changes ID, then an empty result clears it.
    rig.push(UiEvent::Permanents(json!({"permanents":(1..=60).map(|n| permanent_json(2000+n,"Work", &format!("Replacement permanent {n}"))).collect::<Vec<_>>()})));
    rig.show();
    rig.show();
    popup_widget(&mut rig, "Replacement permanent 60");
    rig.push(UiEvent::Permanents(json!({"permanents":[]})));
    rig.input(egui::Event::PointerGone);
    for _ in 0..20 {
        rig.show();
    }
    let _ = rig.requests();
    for _ in 0..3 {
        rig.show();
    }
    assert!(
        !rig.requests().iter().any(|r| r.delay == Duration::ZERO),
        "stale pending row must not request perpetual repaint"
    );
    rig.key(egui::Key::Enter);
    rig.show();
    assert!(rig.is_open());
    assert_eq!(rig.cmds(), vec![]);
}

#[test]
fn active_list_return_to_history_clamps_only_when_displayed() {
    let mut rig = active_list_rig(true, 3);
    rig.app.vm_mut().selected = 2;
    rig.input(egui::Event::Text("row 1".into()));
    rig.show();
    rig.show();
    assert_eq!(rig.app.vm().selected, 2);
    click_popup_widget(&mut rig, strings::text("perm.tab_history"));
    let commands = rig.cmds();
    rig.answer_compact(&commands, &[clip(1, "History row 1")]);
    rig.show();
    assert_eq!(rig.app.vm().selected, 0);
    popup_widget(&mut rig, "History row 1");
    // A focused source tab retains its own Enter/Space behavior; release it to pick.
    rig.popup.ctx.memory_mut(|m| {
        if let Some(id) = m.focused() {
            m.surrender_focus(id);
        }
    });
    rig.key(egui::Key::Enter);
    rig.show();
    finish_popup_pick(&mut rig);
    assert!(rig
        .cmds()
        .iter()
        .any(|cmd| matches!(cmd, UiCmd::PasteClipTo(picked, ..) if *picked == id(1))));
}

fn query_edit_then_navigation_pick(permanent: bool) {
    let mut rig = active_list_rig(permanent, 3);
    if permanent {
        rig.app.vm_mut().selected = 2;
    }
    rig.input(egui::Event::Text("row".into()));
    if permanent {
        rig.key(egui::Key::ArrowDown);
    }
    rig.show();
    if !permanent {
        let commands = rig.cmds();
        rig.answer_compact(
            &commands,
            &[
                clip(1, "History row 1"),
                clip(2, "History row 2"),
                clip(3, "History row 3"),
            ],
        );
        rig.show();
        rig.key(egui::Key::ArrowDown);
        rig.show();
    }
    assert_eq!(rig.app.vm().query, "row");
    if permanent {
        assert_eq!(rig.app.config().lock().expect("config").perm_vm.selected, 1);
        assert_eq!(rig.app.vm().selected, 2);
    } else {
        assert_eq!(rig.app.vm().selected, 1);
    }
    // Enter is in the following frame, after the query was already reconciled.
    rig.key(egui::Key::Enter);
    rig.show();
    finish_popup_pick(&mut rig);
    let commands = rig.cmds();
    if permanent {
        assert_eq!(commands, vec![UiCmd::ExecPermanent(id(1002))]);
    } else {
        assert!(
            matches!(commands.as_slice(), [UiCmd::PasteClipTo(picked,..)] if *picked == id(2)),
            "{commands:?}"
        );
    }
}

#[test]
fn active_list_query_edit_then_history_navigation_keeps_the_picked_row() {
    query_edit_then_navigation_pick(false);
}

#[test]
fn active_list_query_edit_then_permanent_navigation_keeps_the_picked_row() {
    query_edit_then_navigation_pick(true);
}

#[test]
fn compact_search_edit_paths_send_full_query_and_keep_server_hits() {
    let mut rig = Rig::raw(Setup::new(true));
    let first = rig
        .cmds()
        .into_iter()
        .find_map(|c| match c {
            UiCmd::CompactHistory(t) => Some(t),
            _ => None,
        })
        .unwrap();
    rig.push(UiEvent::CompactHistory(first.clone(), Ok(json!({"owner":first.params.owner,"generation":first.params.generation,"rows":[],"cursor":null,"done":true,"bytes_read":0,"candidates":0}))));
    rig.show();
    let _ = rig.cmds();
    rig.input(egui::Event::Text("needle".into()));
    rig.show();
    let search = rig
        .cmds()
        .into_iter()
        .find_map(|c| match c {
            UiCmd::CompactHistory(t) => Some(t),
            _ => None,
        })
        .expect("text edit begins search");
    assert_eq!(search.params.query, "needle");
    let hit = json!({"id":id(1),"pinned":false,"last_used_ms":1,"preview":"short","preview_truncated":true,"kind":"text","thumbnail_hash":null});
    rig.push(UiEvent::CompactHistory(search.clone(), Ok(json!({"owner":search.params.owner,"generation":search.params.generation,"rows":[hit],"cursor":null,"done":true,"bytes_read":0,"candidates":1}))));
    let pass = rig.show();
    assert!(
        pass.painted("short"),
        "server hit outside preview remains visible"
    );
    assert_eq!(rig.app.vm().visible(), vec![0]);
    let button = popup_widget(&mut rig, strings::text("config.open_config"));
    button.request_focus();
    rig.key(egui::Key::A);
    rig.show();
    let keyboard = rig
        .cmds()
        .into_iter()
        .find_map(|c| match c {
            UiCmd::CompactHistory(t) => Some(t),
            _ => None,
        })
        .expect("key handling begins search");
    assert_eq!(keyboard.params.query, "needlea");
}

#[test]
fn compact_page_buttons_advance_and_restart_and_restore_drops_old_reply() {
    let mut rig = Rig::raw(Setup::new(true));
    let first = rig
        .cmds()
        .into_iter()
        .find_map(|c| match c {
            UiCmd::CompactHistory(t) => Some(t),
            _ => None,
        })
        .unwrap();
    let row = |n, name| json!({"id":id(n),"pinned":false,"last_used_ms":1,"preview":name,"preview_truncated":false,"kind":"text","thumbnail_hash":null});
    rig.push(UiEvent::CompactHistory(first.clone(), Ok(json!({"owner":first.params.owner,"generation":first.params.generation,"rows":[row(1,"first")],"cursor":"token","done":false,"bytes_read":0,"candidates":1}))));
    let pass = rig.show();
    assert!(pass.painted("first"));
    let _ = rig.cmds();
    let first_page = pass
        .bounds
        .iter()
        .find(|(text, _, _)| text == strings::text("popup.first_page"))
        .expect("First page painted")
        .1;
    for pressed in [true, false] {
        rig.input(egui::Event::PointerButton {
            pos: first_page.center(),
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        rig.show();
    }
    assert!(
        !rig.cmds()
            .iter()
            .any(|c| matches!(c, UiCmd::CompactHistory(_) | UiCmd::CancelCompactHistory(_))),
        "First page is disabled on the first page"
    );
    click_popup_widget(&mut rig, strings::text("popup.next_page"));
    let page = rig
        .cmds()
        .into_iter()
        .find_map(|c| match c {
            UiCmd::CompactHistory(t) => Some(t),
            _ => None,
        })
        .expect("next page request");
    assert_eq!(page.page, 1);
    assert_eq!(page.params.cursor.as_deref(), Some("token"));
    rig.push(UiEvent::CompactHistory(page.clone(), Ok(json!({"owner":page.params.owner,"generation":page.params.generation,"rows":[row(2,"second")],"cursor":"token","done":false,"bytes_read":0,"candidates":1}))));
    assert!(rig.show().painted("second"));
    click_popup_widget(&mut rig, strings::text("popup.first_page"));
    let commands = rig.cmds();
    assert!(commands
        .iter()
        .any(|c| matches!(c, UiCmd::CancelCompactHistory(p) if p.cursor == "token")));
    let restart = commands
        .into_iter()
        .find_map(|c| match c {
            UiCmd::CompactHistory(t) => Some(t),
            _ => None,
        })
        .unwrap();
    assert_eq!(restart.page, 0);
    assert!(restart.params.cursor.is_none());
    rig.push(UiEvent::RestoreDone { warning: None });
    rig.push(UiEvent::CompactHistory(
        restart.clone(),
        Err(clipmonger_ipc::proto::IpcError::new(
            "unavailable",
            "old failure".into(),
        )),
    ));
    rig.show();
    let restored = rig
        .cmds()
        .into_iter()
        .find_map(|c| match c {
            UiCmd::CompactHistory(t) => Some(t),
            _ => None,
        })
        .expect("fresh scan after restore");
    assert_ne!(restored.restore_epoch, restart.restore_epoch);
    assert!(!rig.app.shell().offline());
}

#[test]
fn compact_popup_sparse_scan_and_stale_query_reply() {
    let mut rig = Rig::raw(Setup::new(true));
    let first = rig
        .cmds()
        .into_iter()
        .find_map(|c| match c {
            UiCmd::CompactHistory(t) => Some(t),
            _ => None,
        })
        .unwrap();
    rig.push(UiEvent::CompactHistory(first.clone(), Ok(json!({"owner":first.params.owner,"generation":first.params.generation,"rows":[],"cursor":"sparse","done":false,"bytes_read":1,"candidates":1}))));
    rig.show();
    let progress = rig
        .cmds()
        .into_iter()
        .find_map(|c| match c {
            UiCmd::CompactHistory(t) => Some(t),
            _ => None,
        })
        .expect("sparse chunk schedules continuation");
    assert_eq!(progress.params.cursor.as_deref(), Some("sparse"));
    rig.input(egui::Event::Text("new".into()));
    rig.show();
    let commands = rig.cmds();
    assert!(commands
        .iter()
        .any(|c| matches!(c, UiCmd::CancelCompactHistory(p) if p.cursor == "sparse")));
    rig.push(UiEvent::CompactHistory(progress.clone(), Ok(json!({"owner":progress.params.owner,"generation":progress.params.generation,"rows":[{"id":id(1),"pinned":false,"last_used_ms":1,"preview":"stale hit","preview_truncated":false,"kind":"text","thumbnail_hash":null}],"cursor":"sparse","done":false,"bytes_read":1,"candidates":1}))));
    let pass = rig.show();
    assert!(!pass.painted("stale hit"));
    let fresh = rig
        .cmds()
        .into_iter()
        .find_map(|c| match c {
            UiCmd::CompactHistory(t) => Some(t),
            _ => None,
        })
        .expect("latest query starts after old request completes");
    assert_eq!(fresh.params.query, "new");
    rig.push(UiEvent::CompactHistory(fresh.clone(), Ok(json!({"owner":fresh.params.owner,"generation":fresh.params.generation,"rows":[],"cursor":"next","done":false,"bytes_read":1,"candidates":1}))));
    rig.show();
    let next = rig
        .cmds()
        .into_iter()
        .find_map(|c| match c {
            UiCmd::CompactHistory(t) => Some(t),
            _ => None,
        })
        .expect("fresh sparse scan continues");
    rig.push(UiEvent::CompactHistory(next.clone(), Ok(json!({"owner":next.params.owner,"generation":next.params.generation,"rows":[{"id":id(2),"pinned":false,"last_used_ms":1,"preview":"later hit","preview_truncated":false,"kind":"text","thumbnail_hash":null}],"cursor":null,"done":true,"bytes_read":1,"candidates":1}))));
    assert!(rig.show().painted("later hit"));
}

#[test]
fn compact_current_reply_survives_search_change() {
    let mut rig = Rig::raw(Setup::new(true));
    let commands = rig.cmds();
    let current = commands
        .iter()
        .find_map(|c| match c {
            UiCmd::CompactCurrent(tag) => Some(*tag),
            _ => None,
        })
        .expect("independent current fetch");
    let first = commands
        .into_iter()
        .find_map(|c| match c {
            UiCmd::CompactHistory(tag) => Some(tag),
            _ => None,
        })
        .unwrap();
    rig.input(egui::Event::Text("search".into()));
    rig.show();
    let item = clip(1, "current");
    rig.push(UiEvent::CompactHistory(first.clone(), Ok(json!({"owner":first.params.owner,"generation":first.params.generation,"rows":[],"cursor":null,"done":true,"bytes_read":1,"candidates":0}))));
    rig.push(UiEvent::CompactCurrent(
        current,
        Ok(json!({"clips":[item]})),
    ));
    rig.show();
    assert_eq!(
        rig.app.vm().current().map(|c| c.id),
        Some(id(1)),
        "typing before initial page must not discard current"
    );
}

#[test]
fn compact_current_refresh_and_restore_supersede_old_replies_during_search() {
    let mut rig = Rig::raw(Setup::new(true));
    let initial = rig
        .cmds()
        .into_iter()
        .find_map(|c| match c {
            UiCmd::CompactCurrent(tag) => Some(tag),
            _ => None,
        })
        .unwrap();
    rig.input(egui::Event::Text("search".into()));
    rig.show();
    rig.key(egui::Key::F5);
    rig.show();
    assert!(
        !rig.cmds()
            .iter()
            .any(|c| matches!(c, UiCmd::CompactCurrent(_))),
        "refresh coalesces while a lookup is pending"
    );
    rig.push(UiEvent::CompactCurrent(
        initial,
        Ok(json!({"clips":[clip(1, "old")]})),
    ));
    rig.show();
    assert!(rig.app.vm().current().is_none());
    let refresh = rig
        .cmds()
        .into_iter()
        .find_map(|c| match c {
            UiCmd::CompactCurrent(tag) => Some(tag),
            _ => None,
        })
        .expect("refresh issues latest lookup");
    rig.push(UiEvent::CompactCurrent(
        refresh,
        Ok(json!({"clips":[clip(2, "refreshed")]})),
    ));
    rig.show();
    assert_eq!(rig.app.vm().current().map(|c| c.id), Some(id(2)));
    rig.push(UiEvent::RestoreDone { warning: None });
    rig.show();
    let restore = rig
        .cmds()
        .into_iter()
        .find_map(|c| match c {
            UiCmd::CompactCurrent(tag) => Some(tag),
            _ => None,
        })
        .expect("restore with a search requests current");
    assert!(rig.app.vm().current().is_none());
    rig.push(UiEvent::CompactCurrent(
        refresh,
        Ok(json!({"clips":[clip(1, "old")]})),
    ));
    rig.push(UiEvent::CompactCurrent(
        restore,
        Ok(json!({"clips":[clip(3, "restored")]})),
    ));
    rig.show();
    assert_eq!(rig.app.vm().current().map(|c| c.id), Some(id(3)));
    rig.push(UiEvent::CompactCurrent(
        refresh,
        Err(clipmonger_ipc::proto::IpcError::new(
            "unavailable",
            "late".into(),
        )),
    ));
    rig.show();
    assert_eq!(rig.app.vm().current().map(|c| c.id), Some(id(3)));
}

#[test]
fn compact_current_after_page_survives_typing_and_reopen_rejects_old() {
    let mut rig = Rig::raw(Setup::new(true));
    let commands = rig.cmds();
    let first = commands
        .iter()
        .find_map(|c| match c {
            UiCmd::CompactHistory(t) => Some(t.clone()),
            _ => None,
        })
        .unwrap();
    let current = commands
        .iter()
        .find_map(|c| match c {
            UiCmd::CompactCurrent(t) => Some(*t),
            _ => None,
        })
        .unwrap();
    rig.push(UiEvent::CompactHistory(first.clone(), Ok(json!({"owner":first.params.owner,"generation":first.params.generation,"rows":[],"cursor":null,"done":true,"bytes_read":1,"candidates":0}))));
    rig.show();
    rig.input(egui::Event::Text("query".into()));
    rig.show();
    rig.push(UiEvent::CompactCurrent(
        current,
        Ok(json!({"clips":[clip(1, "current")]})),
    ));
    rig.show();
    assert_eq!(rig.app.vm().current().map(|c| c.id), Some(id(1)));
    rig.key(egui::Key::Escape);
    rig.show();
    rig.tray(&[TrayAction::OpenPopup]);
    rig.tick();
    let fresh = rig
        .cmds()
        .into_iter()
        .find_map(|c| match c {
            UiCmd::CompactCurrent(t) => Some(t),
            _ => None,
        })
        .expect("reopen requests current");
    rig.push(UiEvent::CompactCurrent(
        current,
        Ok(json!({"clips":[clip(2, "old")]})),
    ));
    rig.push(UiEvent::CompactCurrent(
        fresh,
        Ok(json!({"clips":[clip(3, "new")]})),
    ));
    rig.show();
    assert_eq!(rig.app.vm().current().map(|c| c.id), Some(id(3)));
}

#[test]
fn compact_transport_failure_enters_offline_and_recovers_both_views() {
    for (fail_compact, fail_current) in [(true, false), (false, true), (true, true)] {
        let mut rig = Rig::raw(Setup::new(true));
        let commands = rig.cmds();
        let scan = commands
            .iter()
            .find_map(|c| match c {
                UiCmd::CompactHistory(t) => Some(t.clone()),
                _ => None,
            })
            .unwrap();
        let current = commands
            .iter()
            .find_map(|c| match c {
                UiCmd::CompactCurrent(t) => Some(*t),
                _ => None,
            })
            .unwrap();
        rig.push(status(false, "1"));
        rig.show();
        assert!(!rig.app.shell().offline());
        let failure = || clipmonger_ipc::proto::IpcError::new("unavailable", "lost link".into());
        let scan_result = if fail_compact {
            Err(failure())
        } else {
            Ok(
                json!({"owner":scan.params.owner,"generation":scan.params.generation,"rows":[],"cursor":null,"done":true,"bytes_read":0,"candidates":0}),
            )
        };
        let current_result = if fail_current {
            Err(failure())
        } else {
            Ok(json!({"clips":[]}))
        };
        rig.push(UiEvent::CompactHistory(scan, scan_result));
        rig.push(UiEvent::CompactCurrent(current, current_result));
        rig.show();
        assert!(
            rig.app.shell().offline(),
            "eligible transport error must enter offline, compact={fail_compact}, current={fail_current}"
        );
        rig.push(status(false, "1"));
        rig.show();
        assert!(!rig.app.shell().offline());
        let commands = rig.cmds();
        let fresh_scan = commands
            .iter()
            .find_map(|c| match c {
                UiCmd::CompactHistory(t) => Some(t.clone()),
                _ => None,
            })
            .expect("recovery scan");
        let fresh_current = commands
            .iter()
            .find_map(|c| match c {
                UiCmd::CompactCurrent(t) => Some(*t),
                _ => None,
            })
            .expect("recovery current");
        rig.push(UiEvent::CompactHistory(fresh_scan.clone(), Ok(json!({"owner":fresh_scan.params.owner,"generation":fresh_scan.params.generation,"rows":[{"id":id(1),"pinned":false,"last_used_ms":1,"preview":"recovered","preview_truncated":false,"kind":"text","thumbnail_hash":null}],"cursor":null,"done":true,"bytes_read":1,"candidates":1}))));
        rig.push(UiEvent::CompactCurrent(
            fresh_current,
            Ok(json!({"clips":[clip(1, "recovered")]})),
        ));
        assert!(rig.show().painted("recovered"));
        assert_eq!(rig.app.vm().current().map(|c| c.id), Some(id(1)));
    }
}

#[test]
fn obsolete_transport_errors_do_not_mark_replacement_views_offline() {
    for scenario in ["query", "restore", "reopen"] {
        let mut rig = Rig::raw(Setup::new(true));
        let commands = rig.cmds();
        let scan = commands
            .iter()
            .find_map(|c| match c {
                UiCmd::CompactHistory(t) => Some(t.clone()),
                _ => None,
            })
            .unwrap();
        let current = commands
            .iter()
            .find_map(|c| match c {
                UiCmd::CompactCurrent(t) => Some(*t),
                _ => None,
            })
            .unwrap();
        rig.push(status(false, "1"));
        rig.show();
        if scenario == "restore" {
            rig.push(UiEvent::RestoreDone { warning: None });
            rig.show();
        } else {
            rig.input(egui::Event::Text("new query".into()));
            rig.show();
            if scenario == "reopen" {
                rig.key(egui::Key::Escape);
                rig.show();
                rig.tray(&[TrayAction::OpenPopup]);
                rig.tick();
            }
        }
        let _ = rig.cmds();
        let fail = || clipmonger_ipc::proto::IpcError::new("io_failed", "old outage".into());
        rig.push(UiEvent::CompactHistory(scan, Err(fail())));
        if scenario != "query" {
            rig.push(UiEvent::CompactCurrent(current, Err(fail())));
        }
        rig.show();
        assert!(
            !rig.app.shell().offline(),
            "obsolete error marked new view offline, scenario={scenario}"
        );
        let commands = rig.cmds();
        assert!(commands
            .iter()
            .any(|c| matches!(c, UiCmd::CompactHistory(_))));
        if scenario != "query" {
            assert!(commands
                .iter()
                .any(|c| matches!(c, UiCmd::CompactCurrent(_))));
        }
        rig.push(status(false, "1"));
        rig.show();
        assert!(!rig.app.shell().offline());
        assert!(
            !rig.cmds()
                .iter()
                .any(|c| matches!(c, UiCmd::CompactHistory(_) | UiCmd::CompactCurrent(_))),
            "status should not reset new view for obsolete errors"
        );
    }
}

#[test]
fn restore_blocks_stale_apply_until_refreshed_config_and_keeps_warning() {
    use clipmonger_app_ui::ipc_bridge::RestoreWarning;
    let mut rig = Rig::closed();
    rig.push(config_reply(&json!({"history":{"max_items":100}})));
    rig.tick();
    rig.tray(&[TrayAction::OpenConfig]);
    rig.tick();
    let _ = rig.cmds();
    let size = egui::vec2(960.0, 680.0);
    {
        let mut cfg = rig.app.config().lock().unwrap();
        cfg.tab = "History".into();
        cfg.doc
            .as_mut()
            .unwrap()
            .stage("history.max_items", json!(999))
            .unwrap();
    }
    let apply_pos = settings_button(&mut rig, size, 1.0, strings::text("config.apply"))
        .rect
        .center();
    rig.app.config().lock().unwrap().tab = "databases".into();
    click_settings_button(&mut rig, size, 1.0, strings::text("config.backup_restore"));
    assert!(matches!(rig.cmds().as_slice(), [UiCmd::BackupRestore(_)]));
    {
        let cfg = rig.app.config().lock().unwrap();
        assert!(cfg.restore_pending);
        assert!(cfg.doc.is_none());
        assert_eq!(cfg.pending_apply, [] as [String; 0]);
        assert!(cfg.edits.is_empty());
    }
    // An older reply already in the bridge queue cannot repopulate the form.
    rig.push(config_reply(&json!({"history":{"max_items":100}})));
    rig.tick();
    assert!(rig.app.config().lock().unwrap().doc.is_none());
    // Even if an edit was staged by a queued interaction, the disabled action
    // cannot submit it while the restore is in flight.
    {
        let mut cfg = rig.app.config().lock().unwrap();
        cfg.tab = "History".into();
        let mut doc = config_model::ConfigDoc::from_snapshot(
            &json!({"config":{"history":{"max_items":100}}}),
        );
        doc.stage("history.max_items", json!(999)).unwrap();
        cfg.doc = Some(doc);
    }
    for pressed in [true, false] {
        settings_frame(
            &mut rig,
            size,
            1.0,
            vec![
                egui::Event::PointerMoved(apply_pos),
                egui::Event::PointerButton {
                    pos: apply_pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
    assert_eq!(rig.cmds(), [] as [clipmonger_app_ui::ipc_bridge::UiCmd; 0]);
    rig.push(UiEvent::RestoreDone {
        warning: Some(RestoreWarning::ApplyFailed),
    });
    rig.tick();
    assert_eq!(rig.cmds(), vec![UiCmd::ConfigGetAfterRestore]);
    assert!(rig.app.config().lock().unwrap().restore_pending);
    for pressed in [true, false] {
        settings_frame(
            &mut rig,
            size,
            1.0,
            vec![
                egui::Event::PointerMoved(apply_pos),
                egui::Event::PointerButton {
                    pos: apply_pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
    assert_eq!(rig.cmds(), [] as [clipmonger_app_ui::ipc_bridge::UiCmd; 0]);
    let message = rig.config_error();
    assert!(message.contains(strings::text("config.restore_done")));
    assert!(message.contains(strings::text("config.restore_warning_apply")));
    rig.push(UiEvent::RestoredConfig(
        json!({"config":{"history":{"max_items":23}}}),
    ));
    rig.tick();
    let cfg = rig.app.config().lock().unwrap();
    assert!(!cfg.restore_pending);
    assert_eq!(cfg.error, message);
    let doc = cfg.doc.as_ref().unwrap();
    assert_eq!(doc.get("history.max_items"), Some(&json!(23)));
    assert_eq!(doc.dirty_keys(), [] as [String; 0]);
    assert!(cfg.edits.is_empty());
}

#[test]
fn restore_failure_refreshes_before_reenabling_and_refresh_failure_recovers() {
    let mut rig = Rig::closed();
    rig.tray(&[TrayAction::OpenConfig]);
    rig.tick();
    let _ = rig.cmds();
    rig.app.config().lock().unwrap().restore_pending = true;
    rig.push(UiEvent::RestoreFailed("restore failed".into()));
    rig.tick();
    assert_eq!(rig.config_error(), "restore failed");
    assert!(rig.app.config().lock().unwrap().restore_pending);
    assert_eq!(rig.cmds(), vec![UiCmd::ConfigGetAfterRestore]);
    rig.push(UiEvent::RestoredConfigError("refresh failed".into()));
    rig.tick();
    assert!(!rig.app.config().lock().unwrap().restore_pending);
    assert_eq!(rig.config_error(), "refresh failed");
    rig.push(config_reply(&json!({"history":{"max_items":12}})));
    rig.tick();
    assert_eq!(
        rig.app
            .config()
            .lock()
            .unwrap()
            .doc
            .as_ref()
            .unwrap()
            .get("history.max_items"),
        Some(&json!(12))
    );
}

#[test]
fn restore_warning_with_closed_settings_reaches_shell_and_refreshes() {
    use clipmonger_app_ui::ipc_bridge::RestoreWarning;
    let mut rig = Rig::closed();
    rig.push(UiEvent::RestoreDone {
        warning: Some(RestoreWarning::Unknown),
    });
    rig.tick();
    let message = rig.error().expect("shell message");
    assert!(message.contains(strings::text("config.restore_done")));
    assert!(message.contains(strings::text("config.restore_warning_unknown")));
    assert_eq!(rig.config_error(), "");
    assert_eq!(rig.cmds(), vec![UiCmd::ConfigGetAfterRestore]);
}

#[test]
fn restore_warning_survives_refresh_errors_in_settings_and_shell() {
    use clipmonger_app_ui::ipc_bridge::RestoreWarning;
    for settings_open in [true, false] {
        for config_error in [true, false] {
            let mut rig = Rig::closed();
            if settings_open {
                rig.tray(&[TrayAction::OpenConfig]);
                rig.tick();
                let _ = rig.cmds();
            }
            rig.app.config().lock().unwrap().restore_pending = true;
            rig.push(UiEvent::RestoreDone {
                warning: Some(RestoreWarning::ApplyFailed),
            });
            rig.tick();
            assert_eq!(rig.cmds(), vec![UiCmd::ConfigGetAfterRestore]);
            let refresh_error = "refresh failed";
            if config_error {
                rig.push(UiEvent::RestoredConfigError(refresh_error.into()));
            } else {
                rig.push(UiEvent::Error(refresh_error.into()));
            }
            rig.tick();
            let status = if settings_open {
                rig.config_error()
            } else {
                rig.error().unwrap()
            };
            assert!(
                status.contains(strings::text("config.restore_done")),
                "{status}"
            );
            assert!(
                status.contains(strings::text("config.restore_warning_apply")),
                "{status}"
            );
            assert!(status.contains(refresh_error), "{status}");
            if config_error {
                assert!(!rig.app.config().lock().unwrap().restore_pending);
            } else {
                rig.push(UiEvent::RestoredConfig(json!({"config":{}})));
                rig.tick();
                assert!(!rig.app.config().lock().unwrap().restore_pending);
            }
        }
    }
}

#[test]
fn compact_backend_errors_do_not_mark_the_transport_offline() {
    for code in ["not_ready", "stale_cursor", "metadata_too_large"] {
        let mut rig = Rig::raw(Setup::new(true));
        let scan = rig
            .cmds()
            .into_iter()
            .find_map(|c| match c {
                UiCmd::CompactHistory(t) => Some(t),
                _ => None,
            })
            .unwrap();
        rig.push(UiEvent::CompactHistory(
            scan,
            Err(clipmonger_ipc::proto::IpcError::new(code, "backend".into())),
        ));
        rig.show();
        assert!(
            !rig.app.shell().offline(),
            "backend error {code} is not a lost link"
        );
    }
}

#[test]
fn restored_missing_and_partial_theme_resets_previous_theme_defaults() {
    for (restored, expected_dark, expected_scale) in [
        (json!({"history":{"max_items":23}}), true, 1.0),
        (
            json!({"history":{"max_items":23},"ui":{"theme":{"mode":"light"}}}),
            false,
            1.0,
        ),
        (
            json!({"history":{"max_items":23},"ui":{"theme":{"font_scale":1.2}}}),
            true,
            1.2,
        ),
    ] {
        let mut rig = Rig::closed();
        rig.push(light_config(1.4));
        rig.show();
        rig.show();
        rig.show_settings();
        rig.show_settings();
        assert!(!is_dark(&rig.popup.ctx));
        assert_zoom(&rig.popup.ctx, 1.4, "previous popup");
        {
            let mut config = rig.app.config().lock().unwrap();
            config.restore_pending = true;
            config
                .doc
                .as_mut()
                .unwrap()
                .stage("history.max_items", json!(999))
                .unwrap();
            config
                .edits
                .insert("history.max_items".into(), "999".into());
        }
        rig.push(UiEvent::RestoreDone { warning: None });
        rig.tick();
        rig.push(UiEvent::RestoredConfig(json!({"config":restored})));
        rig.tick();
        {
            let config = rig.app.config().lock().unwrap();
            let doc = config.doc.as_ref().unwrap();
            assert_eq!(doc.get("history.max_items"), Some(&json!(23)));
            assert_eq!(doc.dirty_keys(), [] as [String; 0]);
            assert!(
                config.edits.is_empty(),
                "restore clears old buffers before redraw"
            );
        }
        rig.show();
        rig.show();
        rig.show_settings();
        rig.show_settings();
        let config = rig.app.config().lock().unwrap();
        assert!(!config.restore_pending);
        assert_eq!(config.dark, expected_dark);
        assert!((config.font_scale - expected_scale).abs() < 1e-6);
        let doc = config.doc.as_ref().unwrap();
        assert_eq!(doc.dirty_keys(), [] as [String; 0]);
        assert_eq!(doc.get("history.max_items"), Some(&json!(23)));
        assert_eq!(
            config.edits.get("history.max_items").map(String::as_str),
            Some("23")
        );
        assert!(!config.edits.values().any(|value| value == "999"));
        drop(config);
        assert_eq!(is_dark(&rig.popup.ctx), expected_dark);
        assert_zoom(&rig.popup.ctx, expected_scale, "restored popup");
        assert_eq!(is_dark(&rig.settings.ctx), expected_dark);
        assert_zoom(&rig.settings.ctx, expected_scale, "restored settings");
    }
}

#[test]
fn popup_activation_metadata_follows_show_raise_and_hide() {
    let mut rig = Rig::closed();
    rig.push(clipmonger_app_ui::push::push_event(
        clipmonger_ipc::proto::methods::SHOW_POPUP_TOPIC,
        serde_json::json!({ "source": "hotkey", "x11_time": 812, "activation_token": "wayland-token" }),
    ).expect("wire event"));
    rig.tick();
    assert_eq!(
        rig.app.take_popup_activation_metadata(),
        Some(clipmonger_app_ui::runner::PopupActivation {
            x11_time: Some(812),
            activation_token: Some("wayland-token".into())
        })
    );
    assert_eq!(rig.app.take_popup_activation_metadata(), None);
    rig.push(UiEvent::ShowPopup(ShowPopupEvent {
        source: "hotkey".into(),
        x11_time: Some(813),
        ..ShowPopupEvent::default()
    }));
    rig.push(toggle());
    rig.tick();
    assert_eq!(rig.app.take_popup_activation_metadata(), None);
}

fn settings_texts(rig: &mut Rig, size: egui::Vec2) -> Vec<(String, egui::Rect, egui::Rect)> {
    let output = settings_frame(rig, size, 1.0, Vec::new());
    let mut texts = Vec::new();
    for shape in &output.shapes {
        settings_text_bounds(&shape.shape, shape.clip_rect, &mut texts);
    }
    texts
}

fn settings_click_at(rig: &mut Rig, size: egui::Vec2, pos: egui::Pos2) {
    settings_frame(rig, size, 1.0, vec![egui::Event::PointerMoved(pos)]);
    for pressed in [true, false] {
        settings_frame(
            rig,
            size,
            1.0,
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            }],
        );
    }
}

fn settings_rig(tab: &str) -> Rig {
    let mut rig = Rig::closed();
    rig.push(UiEvent::Config(json!({"config": {
        "popup": {"placement": "cursor"},
        "autostart": {"enabled": false}
    }})));
    rig.tick();
    rig.tray(&[TrayAction::OpenConfig]);
    rig.tick();
    rig.app.config().lock().expect("settings").tab = tab.to_string();
    rig
}

#[test]
fn settings_enum_row_is_a_dropdown_that_stages_the_pick() {
    let size = egui::vec2(960.0, 680.0);
    let mut rig = settings_rig("Popup");
    for _ in 0..3 {
        settings_frame(&mut rig, size, 1.0, Vec::new());
    }
    click_settings_button(&mut rig, size, 1.0, "cursor");
    click_settings_button(&mut rig, size, 1.0, "center");
    let cfg = rig.app.config().lock().expect("settings");
    assert_eq!(
        cfg.doc.as_ref().expect("doc").staged("popup.placement"),
        Some(&json!("center"))
    );
}

#[test]
fn portable_start_on_login_is_disabled_with_a_reason() {
    let size = egui::vec2(960.0, 680.0);
    let label = strings::text("config.autostart_enabled");
    let reason = strings::text("config.restore_warning_portable");
    for portable in [false, true] {
        let mut rig = settings_rig("Startup");
        rig.push(UiEvent::Status(json!({"autostart_portable": portable})));
        rig.tick();
        for _ in 0..3 {
            settings_frame(&mut rig, size, 1.0, Vec::new());
        }
        let texts = settings_texts(&mut rig, size);
        assert_eq!(
            texts.iter().any(|(text, _, _)| text == reason),
            portable,
            "portable reason painted only when portable: {texts:?}"
        );
        // The checkbox caption is the last painted copy of the row label.
        let (_, rect, _) = texts
            .iter()
            .rev()
            .find(|(text, _, _)| text == label)
            .unwrap_or_else(|| panic!("missing {label:?}: {texts:?}"));
        settings_click_at(&mut rig, size, rect.center());
        let cfg = rig.app.config().lock().expect("settings");
        let staged = cfg
            .doc
            .as_ref()
            .expect("doc")
            .staged("autostart.enabled")
            .cloned();
        assert_eq!(
            staged,
            (!portable).then_some(json!(true)),
            "portable={portable}"
        );
    }
}

fn settings_type(rig: &mut Rig, size: egui::Vec2, text: &str) {
    settings_frame(rig, size, 1.0, vec![egui::Event::Text(text.to_string())]);
    settings_frame(rig, size, 1.0, Vec::new());
}

#[test]
fn settings_invalid_text_shows_inline_error_and_keeps_focus() {
    let size = egui::vec2(960.0, 680.0);
    let mut rig = Rig::closed();
    rig.push(UiEvent::Config(
        json!({"config": {"history": {"max_items": 100}}}),
    ));
    rig.tick();
    rig.tray(&[TrayAction::OpenConfig]);
    rig.tick();
    rig.app.config().lock().expect("settings").tab = "History".to_string();
    for _ in 0..3 {
        settings_frame(&mut rig, size, 1.0, Vec::new());
    }
    let invalid = strings::text("config.invalid_value");
    let texts = settings_texts(&mut rig, size);
    let (_, rect, _) = texts
        .iter()
        .find(|(text, _, _)| text == "100")
        .unwrap_or_else(|| panic!("missing max_items field: {texts:?}"));
    settings_click_at(&mut rig, size, rect.right_center() - egui::vec2(1.0, 0.0));

    settings_type(&mut rig, size, "1");
    let staged = |rig: &Rig| {
        rig.app
            .config()
            .lock()
            .expect("settings")
            .doc
            .as_ref()
            .expect("doc")
            .staged("history.max_items")
            .cloned()
    };
    assert_eq!(staged(&rig), Some(json!(1001)));

    settings_type(&mut rig, size, "x");
    let texts = settings_texts(&mut rig, size);
    assert!(
        texts.iter().any(|(text, _, _)| text == invalid),
        "inline error painted: {texts:?}"
    );
    assert_eq!(staged(&rig), None, "the older valid value is unstaged");
    assert_eq!(rig.config_error(), "", "the top error line stays empty");

    // The field keeps focus even when the top error line appears above it.
    rig.app.config().lock().expect("settings").error = "daemon note".to_string();
    settings_type(&mut rig, size, "y");
    let cfg = rig.app.config().lock().expect("settings");
    assert_eq!(
        cfg.edits.get("history.max_items").map(String::as_str),
        Some("1001xy")
    );
}

#[test]
fn settings_refused_apply_marks_the_field_and_keeps_its_text() {
    let size = egui::vec2(960.0, 680.0);
    let mut rig = Rig::closed();
    let snapshot = json!({"config": {"history": {"max_items": 100}}});
    rig.push(UiEvent::Config(snapshot.clone()));
    rig.tick();
    rig.tray(&[TrayAction::OpenConfig]);
    rig.tick();
    {
        let mut cfg = rig.app.config().lock().expect("settings");
        cfg.tab = "History".to_string();
        cfg.edits
            .insert("history.max_items".to_string(), "5000".to_string());
        cfg.pending_apply = vec![
            "history.max_items".to_string(),
            "popup.preview_width".to_string(),
        ];
    }
    rig.push(UiEvent::ConfigSetFailed {
        key: "history.max_items".to_string(),
        message: "refused by daemon".to_string(),
    });
    rig.tick();
    // The Config reload that follows Apply keeps the refused field's text.
    rig.push(UiEvent::Config(snapshot));
    rig.tick();
    {
        let cfg = rig.app.config().lock().expect("settings");
        assert_eq!(cfg.error, "", "not the top error line");
        assert_eq!(
            cfg.pending_apply,
            vec!["popup.preview_width".to_string()],
            "other keys still apply"
        );
        assert_eq!(
            cfg.edits.get("history.max_items").map(String::as_str),
            Some("5000")
        );
        assert_eq!(cfg.focus_field.as_deref(), Some("history.max_items"));
    }
    let texts = settings_texts(&mut rig, size);
    assert!(
        texts.iter().any(|(text, _, _)| text == "refused by daemon"),
        "{texts:?}"
    );
    assert_eq!(
        rig.app.config().lock().expect("settings").focus_field,
        None,
        "focus request consumed by the frame"
    );
    assert_eq!(rig.error(), None, "the popup error line is not touched");
}
