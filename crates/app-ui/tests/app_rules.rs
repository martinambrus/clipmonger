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
use clipmonger_app_ui::{placement, strings};
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
        let rig = Self::raw(setup);
        let start = rig.cmds();
        assert_eq!(start.len(), 5, "the start commands: {start:?}");
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
                egui::vec2(480.0, 360.0),
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
            for clipped in &shapes {
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
fn open_commands() -> Vec<UiCmd> {
    vec![UiCmd::ListHistory, UiCmd::ListPermanents, UiCmd::PermGroups]
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
        assert_eq!(
            rig.cmds(),
            vec![
                UiCmd::Status,
                UiCmd::ConfigGet,
                UiCmd::ListHistory,
                UiCmd::ListPermanents,
                UiCmd::PermGroups,
            ],
            "start commands with open = {open}"
        );
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
    assert_eq!(rig.cmds(), vec![]);
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
    assert_eq!(rig.cmds(), open_commands());
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

// rule 2: a request push opens with an empty context, whatever the push carries
#[test]
fn rule_push_request_opens_with_an_empty_context() {
    for source in ["request", "cli", "second_launch"] {
        let mut rig = Rig::closed();
        rig.push(push_event(source, false, Some(5), Some(6)));
        rig.tick();
        assert!(rig.is_open(), "source {source}");
        assert_eq!(rig.context(), PasteContext::default(), "source {source}");
        assert_eq!(
            rig.trace(),
            vec![open_line("request", false, false)],
            "source {source}"
        );
        assert_eq!(rig.cmds(), open_commands(), "source {source}");
    }
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
    assert_eq!(
        rig.cmds(),
        vec![UiCmd::ListHistory],
        "an open that finds the popup open sends no permanents refresh"
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
        assert_eq!(rig.cmds(), open_commands(), "{opener:?}");
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
        assert_eq!(rig.cmds(), open_commands(), "open number {round}");
        rig.push(toggle());
        rig.tick();
        assert!(!rig.is_open(), "hide number {round}");
        assert_eq!(rig.cmds(), vec![], "a hide sends nothing, round {round}");
    }
}

// effects: Hide and Raise ask the window for nothing; the runner follows desired()
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
    for _ in 0..40 {
        rig.push(UiEvent::RestoreDone);
    }
    rig.tick();
    assert_eq!(rig.cmds().len(), 32, "the first pass");
    rig.tick();
    assert_eq!(rig.cmds().len(), 8, "the second pass");
    rig.tick();
    assert_eq!(rig.cmds(), vec![], "nothing is left");
}

// rule 3: the cap is per pass, whatever the number of events
#[test]
fn rule_cap_table() {
    for (queued, passes) in [
        (0_usize, vec![0_usize]),
        (1, vec![1]),
        (31, vec![31]),
        (32, vec![32, 0]),
        (33, vec![32, 1, 0]),
        (64, vec![32, 32, 0]),
        (65, vec![32, 32, 1, 0]),
    ] {
        let mut rig = Rig::closed();
        for _ in 0..queued {
            rig.push(UiEvent::RestoreDone);
        }
        for (n, expected) in passes.into_iter().enumerate() {
            rig.tick();
            assert_eq!(rig.cmds().len(), expected, "{queued} queued, pass {n}");
        }
    }
}

// rule 3: only Popup passes drain the bridge events
#[test]
fn rule_cap_settings_passes_drain_nothing() {
    let mut rig = Rig::closed();
    rig.push(UiEvent::RestoreDone);
    rig.tick_settings();
    rig.show_settings();
    assert_eq!(rig.cmds(), vec![], "a Settings pass leaves the queue alone");
    rig.tick();
    assert_eq!(rig.cmds(), vec![UiCmd::ListHistory]);
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
    rig.push(UiEvent::Caps(json!({"clipboard": "ok"})));
    rig.push(UiEvent::LogsTail(vec![
        "one".to_string(),
        "two".to_string(),
    ]));
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
    rig.push(UiEvent::BackupDone("/tmp/b.zip".to_string()));
    rig.tick();
    assert_eq!(
        rig.config_error(),
        format!("{} /tmp/b.zip", strings::text("config.backup_saved"))
    );
    assert_eq!(rig.cmds(), vec![]);
    rig.push(UiEvent::RestoreDone);
    rig.tick();
    assert_eq!(rig.config_error(), strings::text("config.restore_done"));
    assert_eq!(rig.cmds(), vec![UiCmd::ListHistory]);
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
    assert_eq!(rig.cmds(), vec![UiCmd::ListHistory]);
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
    assert_eq!(rig.app.vm().visible(), vec![1]);
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
    let pass = rig.show();
    assert!(rig.is_open());
    assert!(pass.painted("alpha row"), "painted: {:?}", pass.texts);
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
    assert_eq!(rig.cmds(), open_commands());
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
    assert_eq!(rig.cmds(), open_commands());
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
    let mut rig = Rig::opened();
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
    assert_eq!(rig.cmds(), vec![UiCmd::ListHistory], "Ctrl+End refreshes");
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
    assert_eq!(rig.cmds(), vec![UiCmd::ListHistory]);
    rig.push(config_reply(
        &json!({"popup": {"keys": {"refresh": "not a chord"}}}),
    ));
    rig.tick();
    rig.key(egui::Key::F5);
    rig.show();
    assert_eq!(rig.cmds(), vec![UiCmd::ListHistory]);
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
fn rule_error_with_a_pending_apply_goes_to_settings() {
    let mut rig = Rig::closed();
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
    assert_eq!(rig.cmds(), open_commands());
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

// rule 12: PasteUnavailable shows the paste-blocked text
#[test]
fn rule_paste_unavailable_shows_the_blocked_text() {
    let mut rig = Rig::opened();
    rig.push(UiEvent::PasteUnavailable);
    rig.tick();
    assert_eq!(
        rig.error().as_deref(),
        Some(strings::text("popup.paste_blocked"))
    );
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

/// The centre of a monitor for the popup, as `placement` computes it.
fn centre_of(width: u16, height: u16) -> egui::Pos2 {
    let monitor = placement::monitor_rect_from_size(Some((u32::from(width), u32::from(height))));
    let (x, y) = placement::place(
        placement::Placement::Center,
        (0, 0),
        &[monitor],
        placement::POPUP_DEFAULT,
    );
    egui::pos2(
        f32::from(i16::try_from(x).expect("a sane x")),
        f32::from(i16::try_from(y).expect("a sane y")),
    )
}

fn monitor_of(width: u16, height: u16) -> egui::Vec2 {
    egui::vec2(f32::from(width), f32::from(height))
}

// rule 15: the first Popup pass with a monitor size yields exactly one OuterPosition, the
// centre per placement; later passes none
#[test]
fn rule_center_first_pass_with_a_monitor_size_moves_the_popup_once() {
    for visible in [false, true] {
        for open in [false, true] {
            let mut rig = Rig::with(Setup::new(open));
            rig.monitor(Some(monitor_of(1920, 1080)));
            let first = if visible { rig.show() } else { rig.tick() };
            assert_eq!(
                first.positions(),
                vec![centre_of(1920, 1080)],
                "visible = {visible}, open = {open}"
            );
            for pass in [rig.tick(), rig.show(), rig.tick(), rig.show()] {
                assert!(
                    pass.positions().is_empty(),
                    "later passes send none (visible = {visible}, open = {open})"
                );
            }
        }
    }
}

// rule 15: the position follows the monitor that was reported
#[test]
fn rule_center_follows_the_reported_monitor_size() {
    for (width, height) in [
        (1366, 768),
        (2560, 1440),
        (800, 600),
        (1920, 1200),
        (3840, 2160),
    ] {
        let mut rig = Rig::closed();
        rig.monitor(Some(monitor_of(width, height)));
        let pass = rig.tick();
        assert_eq!(
            pass.positions(),
            vec![centre_of(width, height)],
            "{width}x{height}"
        );
    }
    assert_ne!(centre_of(1366, 768), centre_of(2560, 1440));
}

// rule 15: without a monitor size there is no command, until one is known
#[test]
fn rule_center_without_a_monitor_size_sends_nothing_until_it_is_known() {
    let mut rig = Rig::closed();
    for _ in 0..3 {
        assert_none(&rig.tick().positions(), "rig.tick().positions()");
        assert_none(&rig.show().positions(), "rig.show().positions()");
    }
    rig.monitor(Some(monitor_of(1280, 720)));
    assert_eq!(rig.tick().positions(), vec![centre_of(1280, 720)]);
    assert_none(&rig.tick().positions(), "rig.tick().positions()");
}

// rule 15: the centre is sent once even when the monitor size changes later
#[test]
fn rule_center_happens_once() {
    let mut rig = Rig::closed();
    rig.monitor(Some(monitor_of(1280, 720)));
    assert_eq!(rig.tick().positions().len(), 1);
    rig.monitor(Some(monitor_of(2560, 1440)));
    assert_none(&rig.tick().positions(), "rig.tick().positions()");
    rig.monitor(None);
    rig.monitor(Some(monitor_of(1280, 720)));
    assert_none(&rig.tick().positions(), "rig.tick().positions()");
}

// rule 15: only the Popup window is centred
#[test]
fn rule_center_settings_passes_do_not_move_anything() {
    let mut rig = Rig::closed();
    rig.settings.info.monitor_size = Some(monitor_of(1920, 1080));
    assert_none(
        &rig.tick_settings().positions(),
        "rig.tick_settings().positions()",
    );
    assert_none(
        &rig.show_settings().positions(),
        "rig.show_settings().positions()",
    );
    rig.monitor(Some(monitor_of(1920, 1080)));
    assert_eq!(rig.tick().positions(), vec![centre_of(1920, 1080)]);
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
    assert_eq!(
        rig.cmds(),
        open_commands(),
        "the open dropped the pending paste"
    );
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
    assert_eq!(rig.cmds(), vec![UiCmd::ListHistory], "defaults again");
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
        rig.app.config().lock().expect("settings").error,
        format!("queued {}", DRAIN_CAP - 1)
    );
    let requests = rig.requests();
    assert_eq!(requests.iter().filter(|r| r.delay.is_zero()).count(), 1);
    rig.tick();
    assert_eq!(
        rig.app.config().lock().expect("settings").error,
        format!("queued {}", DRAIN_CAP + 2)
    );
    assert!(rig.requests().iter().all(|r| !r.delay.is_zero()));
    for _ in 0..DRAIN_CAP {
        rig.push(UiEvent::RestoreDone);
    }
    rig.tick();
    assert_eq!(rig.cmds().len(), DRAIN_CAP);
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
        assert_eq!(rig.cmds(), open_commands());
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
fn fix_n3_long_history_keeps_error_and_settings_painted_inside_window() {
    fn text_bounds(
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
        for expected in [
            "paste failed with a long history",
            strings::text("config.open_config"),
            rig.app.vm().status_line().as_str(),
        ] {
            assert!(
                texts.iter().any(|(text, rect, clip)| {
                    text == expected && window.contains_rect(*rect) && clip.contains_rect(*rect)
                }),
                "{expected:?} must be painted inside the window and its clip: {texts:?}"
            );
        }
    }
}
