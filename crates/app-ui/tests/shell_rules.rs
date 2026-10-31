//! Rules of the shell state machine (`clipmonger_app_ui::shell`), the history
//! reply helpers of the popup and the bridge events that feed them.
//!
//! Shell tests are tables of named cases. A case builds a shell, feeds it
//! events and ticks at given times and, after every step, compares the exact
//! effect list and every getter (`desired`, `context`, `error`, `offline`,
//! `next_deadline`) with the expectation. A step lists only what it changes: a
//! getter it does not mention must still hold the value from before. A step
//! marked inert must also leave the shell behaving as before: a fixed set of
//! probe sequences must give the same trace on a copy taken before the step
//! and on the shell after it. Every case runs twice, on the times of the table
//! and on a clock that starts much later, because only differences between
//! times matter. A failure names the case and the step.

use std::any::Any;
use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Duration;

use clipmonger_app_ui::ipc_bridge::{Bridge, Transport, UiCmd, UiEvent};
use clipmonger_app_ui::popup::{self, HistoryApply, PopupVm};
use clipmonger_app_ui::shell::{
    Context, Desired, Effect, Event, Millis, Shell, PASTE_DELAY_MS, QUIT_TIMEOUT_MS, TRAY_GUARD_MS,
};
use clipmonger_app_ui::strings;
use clipmonger_core::model::Clip;
use clipmonger_ipc::proto::methods::ShowPopupEvent;
use clipmonger_ipc::proto::IpcError;
use serde_json::{json, Value};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Case machinery
// ---------------------------------------------------------------------------

/// What the getters of a shell report.
#[derive(Clone, Debug, PartialEq)]
struct View {
    desired: Desired,
    context: Context,
    error: Option<String>,
    offline: bool,
    deadline: Option<Millis>,
}

impl View {
    fn fresh(open: bool) -> Self {
        Self {
            desired: Desired {
                popup_open: open,
                settings_open: false,
                exit: false,
            },
            context: Context::default(),
            error: None,
            offline: false,
            deadline: None,
        }
    }

    fn of(shell: &Shell) -> Self {
        Self {
            desired: shell.desired(),
            context: shell.context(),
            error: shell.error().map(str::to_string),
            offline: shell.offline(),
            deadline: shell.next_deadline(),
        }
    }

    /// Apply the expected changes; deadlines move with a clock that starts
    /// `shift` milliseconds later.
    fn apply(&mut self, changes: &[Then], shift: Millis) {
        for change in changes {
            match change {
                Then::Open(on) => self.desired.popup_open = *on,
                Then::Settings(on) => self.desired.settings_open = *on,
                Then::Exit => self.desired.exit = true,
                Then::Ctx(context) => self.context = *context,
                Then::Error(text) => self.error.clone_from(text),
                Then::Offline(on) => self.offline = *on,
                Then::Due(at) => self.deadline = at.map(|time| time + shift),
            }
        }
    }
}

/// One change of the expected getters after a step.
#[derive(Clone, Debug)]
enum Then {
    Open(bool),
    Settings(bool),
    Exit,
    Ctx(Context),
    Error(Option<String>),
    Offline(bool),
    Due(Option<Millis>),
}

fn open(on: bool) -> Then {
    Then::Open(on)
}

fn settings(on: bool) -> Then {
    Then::Settings(on)
}

fn exit() -> Then {
    Then::Exit
}

fn ctx(target: u64, gesture: u64) -> Then {
    ctx_of(Some(target), Some(gesture))
}

fn ctx_of(target: Option<u64>, gesture: Option<u64>) -> Then {
    Then::Ctx(Context { target, gesture })
}

fn no_ctx() -> Then {
    Then::Ctx(Context::default())
}

fn err(text: &str) -> Then {
    Then::Error(Some(text.to_string()))
}

fn no_err() -> Then {
    Then::Error(None)
}

fn offline(on: bool) -> Then {
    Then::Offline(on)
}

fn due(at: Millis) -> Then {
    Then::Due(Some(at))
}

fn no_due() -> Then {
    Then::Due(None)
}

/// The input of one step.
#[derive(Clone, Debug)]
enum Input {
    Handle(Event),
    Tick,
}

struct Step {
    at: Millis,
    input: Input,
    effects: Vec<Effect>,
    then: Vec<Then>,
    inert: bool,
}

struct Case {
    name: String,
    open: bool,
    steps: Vec<Step>,
}

impl Case {
    fn new(name: &str, open: bool) -> Self {
        Self {
            name: name.to_string(),
            open,
            steps: Vec::new(),
        }
    }

    /// Feed one event at `at` and expect `effects` and the listed changes.
    fn ev(mut self, at: Millis, event: Event, effects: Vec<Effect>, then: &[Then]) -> Self {
        self.steps.push(Step {
            at,
            input: Input::Handle(event),
            effects,
            then: then.to_vec(),
            inert: false,
        });
        self
    }

    /// Tick at `at` and expect `effects` and the listed changes.
    fn tick(mut self, at: Millis, effects: Vec<Effect>, then: &[Then]) -> Self {
        self.steps.push(Step {
            at,
            input: Input::Tick,
            effects,
            then: then.to_vec(),
            inert: false,
        });
        self
    }

    /// Mark the last step: it must not change how the shell behaves later.
    fn inert(mut self) -> Self {
        if let Some(step) = self.steps.last_mut() {
            step.inert = true;
        }
        self
    }
}

fn run_input(shell: &mut Shell, at: Millis, input: &Input) -> Vec<Effect> {
    match input {
        Input::Handle(event) => shell.handle(event.clone(), at),
        Input::Tick => shell.tick(at),
    }
}

/// Probe sequences, with times relative to `from`.
fn probes(from: Millis) -> Vec<Vec<(Millis, Input)>> {
    let handle = |offset: Millis, event: Event| (from + offset, Input::Handle(event));
    let tick = |offset: Millis| (from + offset, Input::Tick);
    vec![
        // focus loss, then tray clicks inside and outside the guard
        vec![
            handle(0, Event::FocusLost),
            handle(100, Event::TrayToggle),
            handle(350, Event::TrayToggle),
            tick(400),
        ],
        // another hide first: the guard must be off afterwards
        vec![
            handle(0, Event::Dismiss),
            handle(10, Event::TrayToggle),
            handle(20, Event::Hidden),
            tick(1_000),
        ],
        // a pick, its confirmation and the deferred paste
        vec![
            handle(0, pick(77)),
            handle(10, Event::Hidden),
            tick(159),
            tick(160),
            tick(10_000),
        ],
        // a confirmation on its own
        vec![handle(0, Event::Hidden), tick(10), tick(1_000)],
        // the timers as they stand
        vec![tick(0), tick(149), tick(150), tick(5_999), tick(6_000)],
        // quit
        vec![
            handle(0, Event::Quit),
            handle(10, Event::Quit),
            tick(5_999),
            tick(6_009),
            tick(6_010),
        ],
        // pushes
        vec![
            handle(0, request()),
            handle(1, hotkey(1, 2)),
            handle(2, toggle(3, 4)),
            handle(3, request()),
            handle(4, hotkey(5, 6)),
            handle(5, Event::CloseRequest),
        ],
        // a paste failure and what follows it
        vec![
            handle(0, paste_failed("probe")),
            handle(1, Event::Key),
            handle(2, Event::Dismiss),
            handle(3, hotkey(5, 6)),
            handle(4, Event::FocusLost),
        ],
        // replies
        vec![
            handle(0, error_ev("probe")),
            handle(1, Event::Key),
            handle(2, offline_ev("probe")),
            handle(3, status_ev()),
            handle(4, history_ev(Value::Null)),
            handle(5, history_ev(json!({"clips": []}))),
            handle(6, Event::SettingsOpen),
            handle(7, Event::SettingsClose),
            handle(8, Event::Refresh),
        ],
    ]
}

/// Effects and getters of every probe run on its own copy of `shell`.
fn trace(shell: &Shell, from: Millis) -> Vec<String> {
    let mut lines = Vec::new();
    for (n, probe) in probes(from).into_iter().enumerate() {
        let mut copy = shell.clone();
        for (at, input) in probe {
            let effects = run_input(&mut copy, at, &input);
            lines.push(format!(
                "probe {n} at {at} ms: {effects:?} then {:?}",
                View::of(&copy)
            ));
        }
    }
    lines
}

fn check_view(shell: &Shell, want: &View, label: &str) -> Result<(), String> {
    let have = View::of(shell);
    if have == *want {
        Ok(())
    } else {
        Err(format!("{label}: getters {have:?}, expected {want:?}"))
    }
}

/// Run one case on a clock that starts `shift` milliseconds after the times of
/// the table.
fn run_case(case: &Case, shift: Millis) -> Result<(), String> {
    let mut shell = Shell::new(case.open);
    let mut want = View::fresh(case.open);
    check_view(&shell, &want, "new shell")?;
    let mut clock = 0;
    for (n, step) in case.steps.iter().enumerate() {
        let at = step.at + shift;
        let label = format!("step {n} ({:?} at {at} ms)", step.input);
        if step.at < clock {
            return Err(format!("{label}: the table moves the clock backwards"));
        }
        clock = step.at;
        let before = step.inert.then(|| shell.clone());
        let got = run_input(&mut shell, at, &step.input);
        if got != step.effects {
            return Err(format!(
                "{label}: effects {got:?}, expected {:?}",
                step.effects
            ));
        }
        want.apply(&step.then, shift);
        check_view(&shell, &want, &label)?;
        if let Some(before) = before {
            if trace(&before, at) != trace(&shell, at) {
                return Err(format!("{label}: the step changed how the shell behaves"));
            }
        }
    }
    Ok(())
}

fn panic_text(payload: &(dyn Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "no message".to_string()
    }
}

/// A clock that starts this long after the times of the tables: only the
/// differences between times matter, so every case must behave the same.
const LATE_CLOCK_MS: Millis = 7_000_000_000;

/// Run every case on the clock of the table and on a late clock; report all
/// failures together, each with its case name.
fn run(cases: Vec<Case>) {
    let total = cases.len();
    let mut failures = Vec::new();
    for case in cases {
        for shift in [0, LATE_CLOCK_MS] {
            let why = match catch_unwind(AssertUnwindSafe(|| run_case(&case, shift))) {
                Ok(Ok(())) => continue,
                Ok(Err(why)) => why,
                Err(payload) => format!("panicked: {}", panic_text(payload.as_ref())),
            };
            let clock = if shift == 0 {
                String::new()
            } else {
                format!(" (clock {shift} ms ahead)")
            };
            failures.push(format!("case '{}'{clock}: {why}", case.name));
            break;
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {total} cases failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// ---------------------------------------------------------------------------
// Builders for events and effects
// ---------------------------------------------------------------------------

const PICKED_MS: i64 = 1_700_000_000_123;

fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

fn push(source: &str, toggle: bool, target: Option<u64>, gesture: Option<u64>) -> Event {
    Event::Bridge(UiEvent::ShowPopup(ShowPopupEvent {
        source: source.to_string(),
        toggle,
        target,
        gesture,
        x11_time: None,
        activation_token: None,
    }))
}

fn hotkey(target: u64, gesture: u64) -> Event {
    push("hotkey", false, Some(target), Some(gesture))
}

fn hotkey_of(target: Option<u64>, gesture: Option<u64>) -> Event {
    push("hotkey", false, target, gesture)
}

fn request() -> Event {
    push("request", false, None, None)
}

fn toggle(target: u64, gesture: u64) -> Event {
    push("hotkey", true, Some(target), Some(gesture))
}

fn pick(n: u128) -> Event {
    Event::Pick {
        id: id(n),
        picked_ms: PICKED_MS,
    }
}

fn config(body: Value) -> Event {
    Event::Bridge(UiEvent::Config(body))
}

fn error_ev(text: &str) -> Event {
    Event::Bridge(UiEvent::Error(text.to_string()))
}

fn offline_ev(text: &str) -> Event {
    Event::Bridge(UiEvent::Offline(text.to_string()))
}

fn status_ev() -> Event {
    Event::Bridge(UiEvent::Status(json!({"version": "1"})))
}

fn history_ev(body: Value) -> Event {
    Event::Bridge(UiEvent::History(body))
}

fn paste_failed(text: &str) -> Event {
    Event::Bridge(UiEvent::PasteFailed(text.to_string()))
}

fn paste_unavailable() -> Event {
    Event::Bridge(UiEvent::PasteUnavailable)
}

fn paste_done(n: u128) -> Event {
    Event::Bridge(UiEvent::PasteDone(id(n)))
}

fn stopped() -> Event {
    Event::Bridge(UiEvent::Stopped)
}

fn stop_failed(text: &str) -> Event {
    Event::Bridge(UiEvent::StopFailed(text.to_string()))
}

fn another_instance() -> Event {
    Event::Bridge(UiEvent::AnotherInstance)
}

fn nothing() -> Vec<Effect> {
    Vec::new()
}

fn hide() -> Vec<Effect> {
    vec![Effect::Hide]
}

fn raise() -> Vec<Effect> {
    vec![Effect::Raise]
}

fn refresh() -> Vec<Effect> {
    vec![Effect::Send(UiCmd::ListHistory)]
}

fn stop() -> Vec<Effect> {
    vec![Effect::Send(UiCmd::Stop)]
}

fn open_seq() -> Vec<Effect> {
    vec![
        Effect::ClearSearch,
        Effect::Send(UiCmd::ListHistory),
        Effect::SelectNewest,
        Effect::FocusSearch,
    ]
}

fn raise_and_open() -> Vec<Effect> {
    let mut effects = raise();
    effects.extend(open_seq());
    effects
}

fn paste(n: u128, target: Option<u64>, gesture: Option<u64>) -> Effect {
    Effect::Send(UiCmd::PasteClipTo(id(n), target, PICKED_MS, gesture))
}

/// A pick of clip `n` in a popup that knows its target.
fn sent(n: u128, target: u64, gesture: Option<u64>) -> Vec<Effect> {
    vec![
        Effect::AllowForeground,
        Effect::Hide,
        paste(n, Some(target), gesture),
    ]
}

/// A pick without a target: the paste waits.
fn deferred() -> Vec<Effect> {
    vec![Effect::AllowForeground, Effect::Hide]
}

/// What the deferred paste of clip `n` looks like once it is released.
fn released(n: u128, gesture: Option<u64>) -> Vec<Effect> {
    vec![paste(n, None, gesture)]
}

fn text(string_id: &'static str) -> &'static str {
    strings::text(string_id)
}

fn flag_body(value: &Value) -> Value {
    json!({"config": {"popup": {"dismiss_on_focus_loss": value}}})
}

fn clip_json(n: u128, preview: &str, pinned: bool) -> Value {
    json!({
        "id": id(n).to_string(),
        "created_ms": 1_700_000_000_000_i64,
        "source_os": "linux",
        "preview": preview,
        "pinned": pinned,
        "rev": {"wall_ms": 1, "dev": "test-dev"},
    })
}

fn clip(n: u128, preview: &str, pinned: bool) -> Clip {
    serde_json::from_value(clip_json(n, preview, pinned)).expect("the test clip is valid")
}

fn history_body(clips: &[Clip]) -> Value {
    json!({ "clips": clips })
}

/// Reply bodies that are not history lists.
fn malformed_bodies() -> Vec<(&'static str, Value)> {
    vec![
        ("no clips field", json!({})),
        ("clips is a string", json!({"clips": "none"})),
        ("clips is a number", json!({"clips": 3})),
        ("clips is null", json!({"clips": null})),
        ("clips is an object", json!({"clips": {"a": 1}})),
        ("an element is not a clip", json!({"clips": [5]})),
        (
            "an element lacks its id",
            json!({"clips": [{"preview": "x"}]}),
        ),
        (
            "one bad element after good ones",
            json!({"clips": [clip_json(1, "one", false), {"id": "nope"}]}),
        ),
        ("the body is null", Value::Null),
        ("the body is a list", json!([])),
        ("the body is a string", json!("clips")),
    ]
}

// ---------------------------------------------------------------------------
// Start and the initial state
// ---------------------------------------------------------------------------

#[test]
fn start_commands_in_order() {
    assert_eq!(
        Shell::start(),
        vec![
            Effect::Send(UiCmd::Status),
            Effect::Send(UiCmd::ConfigGet),
            Effect::Send(UiCmd::ListHistory),
            Effect::Send(UiCmd::ListPermanents),
            Effect::Send(UiCmd::PermGroups),
        ]
    );
}

#[test]
fn a_new_shell_starts_clean() {
    run(vec![
        Case::new("created closed", false),
        Case::new("created open", true),
        Case::new("created closed, nothing is due", false)
            .tick(1_000_000, nothing(), &[])
            .inert(),
        Case::new("created open, nothing is due", true)
            .tick(1_000_000, nothing(), &[])
            .inert(),
    ]);
}

// ---------------------------------------------------------------------------
// R1 - R5: pushes from the daemon
// ---------------------------------------------------------------------------

#[test]
fn r1_hotkey_push_while_closed_opens_with_the_press_context() {
    let reserved = Event::Bridge(UiEvent::ShowPopup(ShowPopupEvent {
        source: "hotkey".to_string(),
        toggle: false,
        target: Some(7),
        gesture: Some(9),
        x11_time: Some(1_234),
        activation_token: Some("token".to_string()),
    }));
    run(vec![
        Case::new("target and gesture", false).ev(
            100,
            hotkey(7, 9),
            open_seq(),
            &[open(true), ctx(7, 9)],
        ),
        Case::new("target only", false).ev(
            100,
            hotkey_of(Some(7), None),
            open_seq(),
            &[open(true), ctx_of(Some(7), None)],
        ),
        Case::new("gesture only", false).ev(
            100,
            hotkey_of(None, Some(9)),
            open_seq(),
            &[open(true), ctx_of(None, Some(9))],
        ),
        Case::new("neither target nor gesture", false).ev(
            100,
            hotkey_of(None, None),
            open_seq(),
            &[open(true)],
        ),
        Case::new("reserved push fields are not part of the context", false).ev(
            100,
            reserved,
            open_seq(),
            &[open(true), ctx(7, 9)],
        ),
        Case::new("a second press after a hide opens again", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, Event::Dismiss, hide(), &[open(false), no_ctx()])
            .ev(300, hotkey(8, 10), open_seq(), &[open(true), ctx(8, 10)]),
        Case::new("the press time is not a deadline", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .tick(1_000_000, nothing(), &[]),
    ]);
}

#[test]
fn r2_request_push_while_closed_opens_without_context() {
    run(vec![
        Case::new("plain request", false).ev(100, request(), open_seq(), &[open(true)]),
        Case::new("request carrying a target and a gesture", false).ev(
            100,
            push("request", false, Some(7), Some(9)),
            open_seq(),
            &[open(true)],
        ),
        Case::new("an empty source is a request", false).ev(
            100,
            push("", false, Some(7), Some(9)),
            open_seq(),
            &[open(true)],
        ),
        Case::new("an unknown source is a request", false).ev(
            100,
            push("tray", false, Some(7), Some(9)),
            open_seq(),
            &[open(true)],
        ),
        Case::new("no context left over from a hotkey press", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, Event::Dismiss, hide(), &[open(false), no_ctx()])
            .ev(300, request(), open_seq(), &[open(true)]),
        Case::new("a request after a toggle hide has no context", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, toggle(8, 10), hide(), &[open(false), no_ctx()])
            .ev(300, request(), open_seq(), &[open(true)]),
        Case::new(
            "a toggle that found the popup closed leaves no gesture",
            false,
        )
        .ev(100, toggle(8, 10), nothing(), &[])
        .inert()
        .ev(200, request(), open_seq(), &[open(true)]),
    ]);
}

#[test]
fn r3_request_push_while_open_only_raises() {
    run(vec![
        Case::new("keeps the hotkey context", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, request(), raise(), &[])
            .inert(),
        Case::new("keeps an empty context", true)
            .ev(100, request(), raise(), &[])
            .inert(),
        Case::new(
            "a request carrying fields does not replace the context",
            false,
        )
        .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
        .ev(200, push("request", false, Some(1), Some(2)), raise(), &[])
        .inert(),
        Case::new("keeps the error line", true)
            .ev(100, error_ev("old"), nothing(), &[err("old")])
            .ev(200, request(), raise(), &[])
            .inert(),
        Case::new("keeps the offline flag", true)
            .ev(100, offline_ev("down"), nothing(), &[offline(true)])
            .ev(200, request(), raise(), &[])
            .inert(),
        Case::new("raises every time", true)
            .ev(100, request(), raise(), &[])
            .ev(200, request(), raise(), &[])
            .ev(300, request(), raise(), &[]),
        Case::new("another source also only raises", true)
            .ev(100, push("", false, None, None), raise(), &[])
            .inert(),
        Case::new(
            "a request after a request open keeps the empty context",
            false,
        )
        .ev(100, request(), open_seq(), &[open(true)])
        .ev(200, request(), raise(), &[])
        .inert(),
    ]);
}

#[test]
fn r4_hotkey_push_while_open_replaces_the_context() {
    run(vec![
        Case::new("replaces the context", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, hotkey(8, 10), raise_and_open(), &[ctx(8, 10)]),
        Case::new("a push without a target drops the old target", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(
                200,
                hotkey_of(None, Some(10)),
                raise_and_open(),
                &[ctx_of(None, Some(10))],
            ),
        Case::new("a push without a gesture drops the old gesture", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(
                200,
                hotkey_of(Some(8), None),
                raise_and_open(),
                &[ctx_of(Some(8), None)],
            ),
        Case::new("a push with neither empties the context", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, hotkey_of(None, None), raise_and_open(), &[no_ctx()]),
        Case::new("a popup opened without context takes the press", true).ev(
            100,
            hotkey(7, 9),
            raise_and_open(),
            &[ctx(7, 9)],
        ),
        Case::new("the same press twice", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, hotkey(7, 9), raise_and_open(), &[]),
        Case::new("three presses, the last one wins", false)
            .ev(100, hotkey(1, 2), open_seq(), &[open(true), ctx(1, 2)])
            .ev(200, hotkey(3, 4), raise_and_open(), &[ctx(3, 4)])
            .ev(300, hotkey(5, 6), raise_and_open(), &[ctx(5, 6)]),
        Case::new("clears the error line", true)
            .ev(100, error_ev("old"), nothing(), &[err("old")])
            .ev(200, hotkey(7, 9), raise_and_open(), &[ctx(7, 9), no_err()]),
        Case::new(
            "an error and then a hotkey push replace error and context",
            false,
        )
        .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
        .ev(200, error_ev("boom"), nothing(), &[err("boom")])
        .ev(
            300,
            hotkey(8, 10),
            raise_and_open(),
            &[ctx(8, 10), no_err()],
        ),
        Case::new("a paste failure and then a hotkey push", false)
            .ev(
                100,
                paste_failed("late"),
                open_seq(),
                &[open(true), err("late")],
            )
            .ev(
                200,
                hotkey(8, 10),
                raise_and_open(),
                &[ctx(8, 10), no_err()],
            ),
    ]);
}

#[test]
fn r5_toggle_push() {
    run(vec![
        Case::new("open: hides and clears the context", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, toggle(8, 10), hide(), &[open(false), no_ctx()]),
        Case::new("open without context: hides", true).ev(
            100,
            toggle(8, 10),
            hide(),
            &[open(false)],
        ),
        Case::new("closed: nothing changes", false)
            .ev(100, toggle(8, 10), nothing(), &[])
            .inert(),
        Case::new("closed: stays closed for a later toggle", false)
            .ev(100, toggle(8, 10), nothing(), &[])
            .ev(200, toggle(8, 10), nothing(), &[]),
        Case::new("a toggle of any source hides", true).ev(
            100,
            push("request", true, Some(8), Some(10)),
            hide(),
            &[open(false)],
        ),
        Case::new("a toggle without fields hides", true).ev(
            100,
            push("hotkey", true, None, None),
            hide(),
            &[open(false)],
        ),
        Case::new("the toggle gesture is not kept for the next request", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, toggle(8, 10), hide(), &[open(false), no_ctx()])
            .ev(300, request(), open_seq(), &[open(true)]),
        Case::new(
            "a hotkey push after a toggle hide opens with its own context",
            false,
        )
        .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
        .ev(200, toggle(8, 10), hide(), &[open(false), no_ctx()])
        .ev(300, hotkey(11, 12), open_seq(), &[open(true), ctx(11, 12)]),
        Case::new("a toggle hide and a toggle that finds it closed", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, toggle(8, 10), hide(), &[open(false), no_ctx()])
            .ev(300, toggle(8, 10), nothing(), &[])
            .inert(),
    ]);
}

// ---------------------------------------------------------------------------
// R6, R7: hides
// ---------------------------------------------------------------------------

/// Every way to hide an open popup that was opened by the press (7, 9), with
/// the effects it returns.
fn hide_kinds() -> Vec<(&'static str, Event, Vec<Effect>)> {
    vec![
        ("Esc", Event::Dismiss, hide()),
        ("close request", Event::CloseRequest, hide()),
        ("focus loss", Event::FocusLost, hide()),
        ("tray toggle", Event::TrayToggle, hide()),
        ("toggle push", toggle(8, 10), hide()),
        ("pick with a target", pick(1), sent(1, 7, Some(9))),
    ]
}

#[test]
fn r6_every_hide_clears_the_context() {
    let mut cases = Vec::new();
    for (label, event, effects) in hide_kinds() {
        cases.push(
            Case::new(&format!("{label} clears the context"), false)
                .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
                .ev(1_000, event, effects, &[open(false), no_ctx()])
                .ev(2_000, Event::TrayToggle, open_seq(), &[open(true)]),
        );
    }
    cases.push(
        Case::new("a pick without target clears the context", false)
            .ev(
                100,
                hotkey_of(None, Some(9)),
                open_seq(),
                &[open(true), ctx_of(None, Some(9))],
            )
            .ev(1_000, pick(1), deferred(), &[open(false), no_ctx()])
            .ev(2_000, request(), open_seq(), &[open(true)]),
    );
    cases.push(
        Case::new("the context is gone for a later request", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, Event::CloseRequest, hide(), &[open(false), no_ctx()])
            .ev(300, request(), open_seq(), &[open(true)]),
    );
    run(cases);
}

#[test]
fn r7_focus_loss_follows_the_config_flag() {
    let off = || config(flag_body(&json!(false)));
    let on = || config(flag_body(&json!(true)));
    let mut cases = vec![
        Case::new("hides by default", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, Event::FocusLost, hide(), &[open(false), no_ctx()]),
        Case::new("hides by default in a shell created open", true).ev(
            100,
            Event::FocusLost,
            hide(),
            &[open(false)],
        ),
        Case::new("flag off: the popup and its context stay", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, off(), nothing(), &[])
            .ev(300, Event::FocusLost, nothing(), &[])
            .inert()
            .ev(400, Event::FocusLost, nothing(), &[])
            .inert(),
        Case::new("flag off, then on: hides again", true)
            .ev(100, off(), nothing(), &[])
            .ev(200, Event::FocusLost, nothing(), &[])
            .ev(300, on(), nothing(), &[])
            .ev(400, Event::FocusLost, hide(), &[open(false)]),
        Case::new("flag on keeps hiding", true)
            .ev(100, on(), nothing(), &[])
            .ev(200, Event::FocusLost, hide(), &[open(false)]),
        Case::new("the flag is read while the popup is closed too", false)
            .ev(100, off(), nothing(), &[])
            .ev(200, request(), open_seq(), &[open(true)])
            .ev(300, Event::FocusLost, nothing(), &[])
            .inert(),
        Case::new("flag off: other hides are unaffected", false)
            .ev(100, off(), nothing(), &[])
            .ev(200, request(), open_seq(), &[open(true)])
            .ev(300, Event::Dismiss, hide(), &[open(false)])
            .ev(400, request(), open_seq(), &[open(true)])
            .ev(500, Event::CloseRequest, hide(), &[open(false)])
            .ev(600, request(), open_seq(), &[open(true)])
            .ev(700, toggle(1, 2), hide(), &[open(false)])
            .ev(800, request(), open_seq(), &[open(true)])
            .ev(900, Event::TrayToggle, hide(), &[open(false)]),
    ];
    // An absent or non-bool flag keeps what is set.
    let keeps = [
        ("absent", json!({"config": {"popup": {}}})),
        ("popup section absent", json!({"config": {}})),
        ("a string", flag_body(&json!("true"))),
        ("a number", flag_body(&json!(1))),
        ("null", flag_body(&Value::Null)),
        ("a list", flag_body(&json!([true]))),
    ];
    for (label, body) in keeps {
        cases.push(
            Case::new(
                &format!("flag off stays off when the new flag is {label}"),
                true,
            )
            .ev(100, off(), nothing(), &[])
            .ev(200, config(body.clone()), nothing(), &[])
            .ev(300, Event::FocusLost, nothing(), &[])
            .inert(),
        );
        cases.push(
            Case::new(
                &format!("flag on stays on when the new flag is {label}"),
                true,
            )
            .ev(200, config(body), nothing(), &[])
            .ev(300, Event::FocusLost, hide(), &[open(false)]),
        );
    }
    run(cases);
}

// ---------------------------------------------------------------------------
// R8: tray toggle and its guard
// ---------------------------------------------------------------------------

#[test]
fn r8_tray_toggle() {
    run(vec![
        Case::new("open: hides and clears the context", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, Event::TrayToggle, hide(), &[open(false), no_ctx()]),
        Case::new("closed: opens without context", false).ev(
            100,
            Event::TrayToggle,
            open_seq(),
            &[open(true)],
        ),
        Case::new("a shell created open: the first click hides", true).ev(
            100,
            Event::TrayToggle,
            hide(),
            &[open(false)],
        ),
        Case::new("click, click, click", false)
            .ev(100, Event::TrayToggle, open_seq(), &[open(true)])
            .ev(150, Event::TrayToggle, hide(), &[open(false)])
            .ev(200, Event::TrayToggle, open_seq(), &[open(true)]),
        Case::new("opens without the context of the last press", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, Event::FocusLost, hide(), &[open(false), no_ctx()])
            .ev(1_000, Event::TrayToggle, open_seq(), &[open(true)]),
        Case::new("the focus-loss hide, the click and the next click", true)
            .ev(1_000, Event::FocusLost, hide(), &[open(false)])
            .ev(1_100, Event::TrayToggle, nothing(), &[])
            .ev(5_000, Event::TrayToggle, open_seq(), &[open(true)])
            .ev(5_100, Event::TrayToggle, hide(), &[open(false)])
            .ev(5_200, Event::TrayToggle, open_seq(), &[open(true)]),
    ]);
}

#[test]
fn r8_tray_guard_boundary() {
    assert_eq!(TRAY_GUARD_MS, 300, "the guard is 300 ms");
    let mut cases = Vec::new();
    for delay in [0_u64, 1, 150, 298, 299, 300] {
        cases.push(
            Case::new(
                &format!("a click {delay} ms after the focus loss stays hidden"),
                true,
            )
            .ev(1_000, Event::FocusLost, hide(), &[open(false)])
            .ev(1_000 + delay, Event::TrayToggle, nothing(), &[])
            .inert(),
        );
    }
    for delay in [301_u64, 302, 450, 5_000] {
        cases.push(
            Case::new(
                &format!("a click {delay} ms after the focus loss opens"),
                true,
            )
            .ev(1_000, Event::FocusLost, hide(), &[open(false)])
            .ev(1_000 + delay, Event::TrayToggle, open_seq(), &[open(true)]),
        );
    }
    cases.push(
        Case::new("a focus loss at time zero counts as stored", true)
            .ev(0, Event::FocusLost, hide(), &[open(false)])
            .ev(300, Event::TrayToggle, nothing(), &[])
            .ev(301, Event::TrayToggle, open_seq(), &[open(true)]),
    );
    run(cases);
}

#[test]
fn r8_the_guard_time_stays_with_the_focus_loss() {
    run(vec![
        Case::new("guarded clicks do not move the stored time", true)
            .ev(1_000, Event::FocusLost, hide(), &[open(false)])
            .ev(1_100, Event::TrayToggle, nothing(), &[])
            .ev(1_300, Event::TrayToggle, nothing(), &[])
            .ev(1_301, Event::TrayToggle, open_seq(), &[open(true)]),
        Case::new("a guarded click does not clear the stored time", true)
            .ev(1_000, Event::FocusLost, hide(), &[open(false)])
            .ev(1_100, Event::TrayToggle, nothing(), &[])
            .ev(1_250, Event::TrayToggle, nothing(), &[]),
        Case::new("a second focus loss while closed does not refresh it", true)
            .ev(1_000, Event::FocusLost, hide(), &[open(false)])
            .ev(1_200, Event::FocusLost, nothing(), &[])
            .inert()
            .ev(1_350, Event::TrayToggle, open_seq(), &[open(true)]),
        Case::new("a confirmation of the hide does not clear it", true)
            .ev(1_000, Event::FocusLost, hide(), &[open(false)])
            .ev(1_010, Event::Hidden, nothing(), &[])
            .ev(1_100, Event::TrayToggle, nothing(), &[]),
        Case::new("events while closed leave the guard alone", true)
            .ev(1_000, Event::FocusLost, hide(), &[open(false)])
            .ev(1_020, Event::Dismiss, nothing(), &[])
            .ev(1_030, Event::CloseRequest, nothing(), &[])
            .ev(1_040, pick(1), nothing(), &[])
            .ev(1_050, Event::Hidden, nothing(), &[])
            .ev(1_060, Event::Key, nothing(), &[])
            .ev(1_070, Event::Refresh, refresh(), &[])
            .ev(1_080, Event::SettingsOpen, nothing(), &[settings(true)])
            .ev(1_090, Event::SettingsClose, nothing(), &[settings(false)])
            .ev(1_100, status_ev(), nothing(), &[])
            .ev(1_110, error_ev("e"), nothing(), &[err("e")])
            .ev(1_120, toggle(1, 2), nothing(), &[])
            .ev(1_200, Event::TrayToggle, nothing(), &[]),
        Case::new("a later focus loss starts a new guard", true)
            .ev(1_000, Event::FocusLost, hide(), &[open(false)])
            .ev(2_000, Event::TrayToggle, open_seq(), &[open(true)])
            .ev(2_100, Event::FocusLost, hide(), &[open(false)])
            .ev(2_200, Event::TrayToggle, nothing(), &[])
            .ev(2_401, Event::TrayToggle, open_seq(), &[open(true)]),
    ]);
}

#[test]
fn r8_only_a_focus_loss_hide_is_guarded() {
    let mut cases = Vec::new();
    for (label, event, effects) in hide_kinds() {
        if label == "focus loss" {
            continue;
        }
        cases.push(
            Case::new(&format!("a click right after {label} opens"), false)
                .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
                .ev(1_000, event, effects, &[open(false), no_ctx()])
                .ev(1_050, Event::TrayToggle, open_seq(), &[open(true)]),
        );
    }
    cases.push(
        Case::new("a click right after a pick without target opens", false)
            .ev(100, request(), open_seq(), &[open(true)])
            .ev(1_000, pick(1), deferred(), &[open(false)])
            .ev(1_050, Event::TrayToggle, open_seq(), &[open(true)]),
    );
    cases.push(
        Case::new(
            "a focus loss before an open does not guard the later hide",
            true,
        )
        .ev(1_000, Event::FocusLost, hide(), &[open(false)])
        .ev(1_050, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
        .ev(1_100, Event::Dismiss, hide(), &[open(false), no_ctx()])
        .ev(1_150, Event::TrayToggle, open_seq(), &[open(true)]),
    );
    run(cases);
}

#[test]
fn r8_the_guard_holds_back_the_tray_only() {
    run(vec![
        Case::new("a request right after a focus-loss hide opens", true)
            .ev(1_000, Event::FocusLost, hide(), &[open(false)])
            .ev(1_050, request(), open_seq(), &[open(true)]),
        Case::new("a hotkey press right after a focus-loss hide opens", true)
            .ev(1_000, Event::FocusLost, hide(), &[open(false)])
            .ev(1_050, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)]),
        Case::new("a paste failure right after a focus-loss hide opens", true)
            .ev(1_000, Event::FocusLost, hide(), &[open(false)])
            .ev(
                1_050,
                paste_failed("boom"),
                open_seq(),
                &[open(true), err("boom")],
            ),
        Case::new(
            "a toggle push after a focus-loss hide leaves the guard",
            true,
        )
        .ev(1_000, Event::FocusLost, hide(), &[open(false)])
        .ev(1_050, toggle(1, 2), nothing(), &[])
        .ev(1_100, Event::TrayToggle, nothing(), &[]),
    ]);
}

// ---------------------------------------------------------------------------
// R9: pick
// ---------------------------------------------------------------------------

#[test]
fn r9_pick_with_a_target_pastes_at_once() {
    let late_pick = Event::Pick {
        id: id(5),
        picked_ms: 42,
    };
    run(vec![
        Case::new("target and gesture", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, pick(1), sent(1, 7, Some(9)), &[open(false), no_ctx()])
            .ev(250, Event::Hidden, nothing(), &[])
            .tick(10_000, nothing(), &[]),
        Case::new("target without a gesture", false)
            .ev(
                100,
                hotkey_of(Some(7), None),
                open_seq(),
                &[open(true), ctx_of(Some(7), None)],
            )
            .ev(200, pick(2), sent(2, 7, None), &[open(false), no_ctx()]),
        Case::new("the context is the one of the last press", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, hotkey(8, 10), raise_and_open(), &[ctx(8, 10)])
            .ev(300, pick(3), sent(3, 8, Some(10)), &[open(false), no_ctx()]),
        Case::new("the pick time of the event is passed on", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(
                200,
                late_pick,
                vec![
                    Effect::AllowForeground,
                    Effect::Hide,
                    Effect::Send(UiCmd::PasteClipTo(id(5), Some(7), 42, Some(9))),
                ],
                &[open(false), no_ctx()],
            ),
        Case::new("a second pick finds the popup closed", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, pick(1), sent(1, 7, Some(9)), &[open(false), no_ctx()])
            .ev(300, pick(2), nothing(), &[])
            .inert(),
        Case::new("the paste reply changes nothing", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, pick(1), sent(1, 7, Some(9)), &[open(false), no_ctx()])
            .ev(300, paste_done(1), nothing(), &[])
            .inert(),
        Case::new("a pick after a request press uses the new context", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, Event::Dismiss, hide(), &[open(false), no_ctx()])
            .ev(300, request(), open_seq(), &[open(true)])
            .ev(400, pick(4), deferred(), &[open(false)]),
    ]);
}

#[test]
fn r9_pick_without_a_target_waits_for_the_confirmation() {
    assert_eq!(PASTE_DELAY_MS, 150, "the paste delay is 150 ms");
    run(vec![
        Case::new("gesture only: 149 ms is early, 150 ms is on time", false)
            .ev(
                100,
                hotkey_of(None, Some(9)),
                open_seq(),
                &[open(true), ctx_of(None, Some(9))],
            )
            .ev(200, pick(2), deferred(), &[open(false), no_ctx()])
            .tick(300, nothing(), &[])
            .tick(900, nothing(), &[])
            .ev(1_000, Event::Hidden, nothing(), &[due(1_150)])
            .tick(1_149, nothing(), &[])
            .tick(1_150, released(2, Some(9)), &[no_due()])
            .tick(1_151, nothing(), &[])
            .tick(9_999, nothing(), &[]),
        Case::new("no gesture either", false)
            .ev(100, request(), open_seq(), &[open(true)])
            .ev(200, pick(3), deferred(), &[open(false)])
            .ev(300, Event::Hidden, nothing(), &[due(450)])
            .tick(449, nothing(), &[])
            .tick(450, released(3, None), &[no_due()]),
        Case::new("a shell created open", true)
            .ev(100, pick(4), deferred(), &[open(false)])
            .ev(110, Event::Hidden, nothing(), &[due(260)])
            .tick(259, nothing(), &[])
            .tick(260, released(4, None), &[no_due()]),
        Case::new("a late tick releases the paste once", true)
            .ev(100, pick(4), deferred(), &[open(false)])
            .ev(200, Event::Hidden, nothing(), &[due(350)])
            .tick(5_000, released(4, None), &[no_due()])
            .tick(5_001, nothing(), &[])
            .tick(9_000, nothing(), &[]),
        Case::new("the first confirmation counts", true)
            .ev(200, pick(5), deferred(), &[open(false)])
            .ev(1_000, Event::Hidden, nothing(), &[due(1_150)])
            .ev(1_100, Event::Hidden, nothing(), &[])
            .inert()
            .tick(1_149, nothing(), &[])
            .tick(1_150, released(5, None), &[no_due()]),
        Case::new("confirmation and pick at time zero", true)
            .ev(0, pick(6), deferred(), &[open(false)])
            .ev(0, Event::Hidden, nothing(), &[due(150)])
            .tick(149, nothing(), &[])
            .tick(150, released(6, None), &[no_due()]),
        Case::new("a confirmation without a waiting paste is harmless", false)
            .ev(100, request(), open_seq(), &[open(true)])
            .ev(200, Event::Dismiss, hide(), &[open(false)])
            .ev(300, Event::Hidden, nothing(), &[])
            .inert()
            .tick(10_000, nothing(), &[]),
        Case::new("a second pick while waiting is ignored", true)
            .ev(100, pick(1), deferred(), &[open(false)])
            .ev(110, pick(2), nothing(), &[])
            .inert()
            .ev(120, Event::Hidden, nothing(), &[due(270)])
            .tick(270, released(1, None), &[no_due()]),
    ]);
}

#[test]
fn r9_each_pick_waits_for_its_own_confirmation() {
    run(vec![
        Case::new("second round", false)
            .ev(
                100,
                hotkey_of(None, Some(9)),
                open_seq(),
                &[open(true), ctx_of(None, Some(9))],
            )
            .ev(200, pick(7), deferred(), &[open(false), no_ctx()])
            .ev(300, Event::Hidden, nothing(), &[due(450)])
            .tick(450, released(7, Some(9)), &[no_due()])
            .ev(2_000, request(), open_seq(), &[open(true)])
            .ev(2_100, pick(8), deferred(), &[open(false)])
            .tick(2_200, nothing(), &[])
            .ev(2_300, Event::Hidden, nothing(), &[due(2_450)])
            .tick(2_449, nothing(), &[])
            .tick(2_450, released(8, None), &[no_due()]),
        Case::new("a round after a round that was never released", true)
            .ev(100, pick(1), deferred(), &[open(false)])
            .ev(200, Event::Hidden, nothing(), &[due(350)])
            .ev(250, request(), open_seq(), &[open(true), no_due()])
            .ev(300, pick(2), deferred(), &[open(false)])
            .tick(5_000, nothing(), &[])
            .ev(6_000, Event::Hidden, nothing(), &[due(6_150)])
            .tick(6_150, released(2, None), &[no_due()]),
        Case::new("a sent paste does not leave a waiting one behind", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, pick(1), sent(1, 7, Some(9)), &[open(false), no_ctx()])
            .ev(300, Event::Hidden, nothing(), &[])
            .tick(10_000, nothing(), &[])
            .ev(11_000, request(), open_seq(), &[open(true)])
            .ev(11_100, Event::Dismiss, hide(), &[open(false)])
            .ev(11_200, Event::Hidden, nothing(), &[])
            .tick(20_000, nothing(), &[]),
    ]);
}

// ---------------------------------------------------------------------------
// R10: paste failure
// ---------------------------------------------------------------------------

#[test]
fn r10_paste_failed_reopens_the_popup_with_an_error() {
    run(vec![
        Case::new("after a paste with a target", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, pick(1), sent(1, 7, Some(9)), &[open(false), no_ctx()])
            .ev(
                300,
                paste_failed("daemon refused"),
                open_seq(),
                &[open(true), err("daemon refused")],
            )
            .ev(400, Event::Key, nothing(), &[no_err()]),
        Case::new("closed: opens without context", false).ev(
            100,
            paste_failed("boom"),
            open_seq(),
            &[open(true), err("boom")],
        ),
        Case::new("open: only the error line changes", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, paste_failed("late"), nothing(), &[err("late")]),
        Case::new("open: a later failure replaces the line", true)
            .ev(100, paste_failed("one"), nothing(), &[err("one")])
            .ev(200, paste_failed("two"), nothing(), &[err("two")]),
        Case::new("a hide keeps the line", false)
            .ev(
                100,
                paste_failed("boom"),
                open_seq(),
                &[open(true), err("boom")],
            )
            .ev(200, Event::Dismiss, hide(), &[open(false)]),
        Case::new("the line clears on the next hotkey push", false)
            .ev(
                100,
                paste_failed("boom"),
                open_seq(),
                &[open(true), err("boom")],
            )
            .ev(200, Event::Dismiss, hide(), &[open(false)])
            .ev(
                300,
                hotkey(7, 9),
                open_seq(),
                &[open(true), ctx(7, 9), no_err()],
            ),
        Case::new("the line clears on the next request", false)
            .ev(
                100,
                paste_failed("boom"),
                open_seq(),
                &[open(true), err("boom")],
            )
            .ev(200, Event::Dismiss, hide(), &[open(false)])
            .ev(300, request(), open_seq(), &[open(true), no_err()]),
        Case::new("the line clears on the next tray click", false)
            .ev(
                100,
                paste_failed("boom"),
                open_seq(),
                &[open(true), err("boom")],
            )
            .ev(200, Event::Dismiss, hide(), &[open(false)])
            .ev(300, Event::TrayToggle, open_seq(), &[open(true), no_err()]),
        Case::new("the line clears on a key while closed", false)
            .ev(
                100,
                paste_failed("boom"),
                open_seq(),
                &[open(true), err("boom")],
            )
            .ev(200, Event::Dismiss, hide(), &[open(false)])
            .ev(300, Event::Key, nothing(), &[no_err()]),
    ]);
}

// ---------------------------------------------------------------------------
// R11: quit
// ---------------------------------------------------------------------------

#[test]
fn r11_quit_waits_for_the_stop_reply_or_six_seconds() {
    assert_eq!(QUIT_TIMEOUT_MS, 6_000, "the quit timeout is 6 s");
    run(vec![
        Case::new("the first quit sends stop and starts the timer", false).ev(
            1_000,
            Event::Quit,
            stop(),
            &[due(7_000)],
        ),
        Case::new("5.999 s is early, 6.0 s is on time", false)
            .ev(1_000, Event::Quit, stop(), &[due(7_000)])
            .tick(6_999, nothing(), &[])
            .tick(7_000, nothing(), &[exit(), no_due()]),
        Case::new("a tick after the deadline exits too", false)
            .ev(1_000, Event::Quit, stop(), &[due(7_000)])
            .tick(7_001, nothing(), &[exit(), no_due()]),
        Case::new("a tick long after the deadline exits", false)
            .ev(1_000, Event::Quit, stop(), &[due(7_000)])
            .tick(60_000, nothing(), &[exit(), no_due()]),
        Case::new("exit stays set", false)
            .ev(1_000, Event::Quit, stop(), &[due(7_000)])
            .tick(7_000, nothing(), &[exit(), no_due()])
            .tick(7_001, nothing(), &[])
            .tick(100_000, nothing(), &[]),
        Case::new(
            "a second quit is ignored and does not restart the timer",
            false,
        )
        .ev(1_000, Event::Quit, stop(), &[due(7_000)])
        .ev(5_000, Event::Quit, nothing(), &[])
        .inert()
        .tick(6_999, nothing(), &[])
        .tick(7_000, nothing(), &[exit(), no_due()]),
        Case::new("a quit at time zero", false)
            .ev(0, Event::Quit, stop(), &[due(6_000)])
            .tick(5_999, nothing(), &[])
            .tick(6_000, nothing(), &[exit(), no_due()]),
        Case::new("nothing is due before a quit", false)
            .tick(1_000_000, nothing(), &[])
            .inert(),
        Case::new("a quit leaves an open popup and its context alone", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, Event::Quit, stop(), &[due(6_200)]),
        Case::new("the timer runs while the popup is used", false)
            .ev(100, Event::Quit, stop(), &[due(6_100)])
            .ev(200, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(300, Event::Dismiss, hide(), &[open(false), no_ctx()])
            .tick(6_099, nothing(), &[])
            .tick(6_100, nothing(), &[exit(), no_due()]),
    ]);
}

#[test]
fn r11_stop_reply_or_error_ends_the_wait() {
    run(vec![
        Case::new("stopped before the timeout", false)
            .ev(1_000, Event::Quit, stop(), &[due(7_000)])
            .ev(2_000, stopped(), nothing(), &[exit(), no_due()])
            .tick(7_000, nothing(), &[]),
        Case::new("a stop error before the timeout", false)
            .ev(1_000, Event::Quit, stop(), &[due(7_000)])
            .ev(
                2_000,
                stop_failed("refused"),
                nothing(),
                &[exit(), no_due()],
            )
            .tick(7_000, nothing(), &[]),
        Case::new("stopped without a quit", false).ev(1_000, stopped(), nothing(), &[exit()]),
        Case::new("a stop error does not set the error line", false)
            .ev(1_000, Event::Quit, stop(), &[due(7_000)])
            .ev(
                2_000,
                stop_failed("refused"),
                nothing(),
                &[exit(), no_due()],
            ),
        Case::new("an error reply after a quit does not end the wait", false)
            .ev(1_000, Event::Quit, stop(), &[due(7_000)])
            .ev(2_000, error_ev("said no"), nothing(), &[err("said no")])
            .tick(6_999, nothing(), &[])
            .tick(7_000, nothing(), &[exit(), no_due()]),
        Case::new("an offline reply after a quit does not end the wait", false)
            .ev(1_000, Event::Quit, stop(), &[due(7_000)])
            .ev(2_000, offline_ev("down"), nothing(), &[offline(true)])
            .tick(6_999, nothing(), &[])
            .tick(7_000, nothing(), &[exit(), no_due()]),
        Case::new("a status reply after a quit does not end the wait", false)
            .ev(1_000, Event::Quit, stop(), &[due(7_000)])
            .ev(2_000, status_ev(), nothing(), &[])
            .tick(6_999, nothing(), &[])
            .tick(7_000, nothing(), &[exit(), no_due()]),
    ]);
}

#[test]
fn r11_stopped_and_another_instance_exit_at_any_time() {
    let exits = [
        ("stopped", stopped()),
        ("stop failed", stop_failed("refused")),
        ("another instance", another_instance()),
    ];
    let mut cases = Vec::new();
    for (label, event) in exits {
        cases.push(Case::new(&format!("{label} while closed"), false).ev(
            100,
            event.clone(),
            nothing(),
            &[exit()],
        ));
        cases.push(
            Case::new(
                &format!("{label} while open, the popup stays as it was"),
                false,
            )
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, event.clone(), nothing(), &[exit()]),
        );
        cases.push(
            Case::new(&format!("{label} with a paste waiting"), true)
                .ev(10, pick(1), deferred(), &[open(false)])
                .ev(20, Event::Hidden, nothing(), &[due(170)])
                .ev(30, event.clone(), nothing(), &[exit()]),
        );
        cases.push(
            Case::new(&format!("{label} while offline with an error"), false)
                .ev(10, error_ev("e"), nothing(), &[err("e")])
                .ev(20, offline_ev("o"), nothing(), &[offline(true)])
                .ev(30, event, nothing(), &[exit()]),
        );
    }
    cases.push(
        Case::new("another instance after a quit", false)
            .ev(1_000, Event::Quit, stop(), &[due(7_000)])
            .ev(2_000, another_instance(), nothing(), &[exit(), no_due()]),
    );
    run(cases);
}

// ---------------------------------------------------------------------------
// R12: history and offline
// ---------------------------------------------------------------------------

#[test]
fn r12_offline_is_set_by_transport_errors_and_malformed_history_only() {
    let two = history_body(&[clip(1, "one", false), clip(2, "two", true)]);
    let empty = || history_ev(json!({"clips": []}));
    let mut cases = vec![
        Case::new("an offline reply marks the shell offline", false).ev(
            100,
            offline_ev("link lost"),
            nothing(),
            &[offline(true)],
        ),
        Case::new("an offline reply leaves the error line", false)
            .ev(100, error_ev("old"), nothing(), &[err("old")])
            .ev(200, offline_ev("link lost"), nothing(), &[offline(true)]),
        Case::new("an offline reply leaves the popup and its context", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, offline_ev("link lost"), nothing(), &[offline(true)]),
        Case::new("a status reply brings the shell back online", false)
            .ev(100, offline_ev("link lost"), nothing(), &[offline(true)])
            .ev(200, status_ev(), nothing(), &[offline(false)]),
        Case::new("a status reply of any shape brings it back online", false)
            .ev(100, offline_ev("down"), nothing(), &[offline(true)])
            .ev(
                200,
                Event::Bridge(UiEvent::Status(Value::Null)),
                nothing(),
                &[offline(false)],
            )
            .ev(300, offline_ev("down"), nothing(), &[offline(true)])
            .ev(
                400,
                Event::Bridge(UiEvent::Status(json!({"monitoring_paused": true}))),
                nothing(),
                &[offline(false)],
            ),
        Case::new("a status reply leaves the error line", false)
            .ev(100, error_ev("old"), nothing(), &[err("old")])
            .ev(200, status_ev(), nothing(), &[]),
        Case::new("an empty history never marks offline", false)
            .ev(100, empty(), nothing(), &[])
            .ev(200, empty(), nothing(), &[]),
        Case::new("an empty history brings the shell back online", false)
            .ev(100, offline_ev("down"), nothing(), &[offline(true)])
            .ev(200, empty(), nothing(), &[offline(false)]),
        Case::new("the same history twice never marks offline", false)
            .ev(100, history_ev(two.clone()), nothing(), &[])
            .ev(200, history_ev(two.clone()), nothing(), &[])
            .ev(300, history_ev(two.clone()), nothing(), &[]),
        Case::new("a history reply brings the shell back online", false)
            .ev(100, offline_ev("down"), nothing(), &[offline(true)])
            .ev(200, history_ev(two.clone()), nothing(), &[offline(false)])
            .ev(300, history_ev(two.clone()), nothing(), &[]),
        Case::new("an error reply is not a transport error", false).ev(
            100,
            error_ev("denied"),
            nothing(),
            &[err("denied")],
        ),
        Case::new("an error reply leaves the offline flag", false)
            .ev(100, offline_ev("down"), nothing(), &[offline(true)])
            .ev(200, error_ev("denied"), nothing(), &[err("denied")]),
        Case::new("a good history after a bad one keeps the error line", false)
            .ev(
                100,
                history_ev(Value::Null),
                nothing(),
                &[offline(true), err(text("app.error"))],
            )
            .ev(200, history_ev(two), nothing(), &[offline(false)]),
    ];
    for (label, body) in malformed_bodies() {
        cases.push(
            Case::new(&format!("malformed history: {label}"), false)
                .ev(
                    100,
                    history_ev(body.clone()),
                    nothing(),
                    &[offline(true), err(text("app.error"))],
                )
                .ev(200, empty(), nothing(), &[offline(false)]),
        );
        cases.push(
            Case::new(&format!("malformed history while open: {label}"), false)
                .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
                .ev(150, error_ev("older"), nothing(), &[err("older")])
                .ev(
                    200,
                    history_ev(body),
                    nothing(),
                    &[offline(true), err(text("app.error"))],
                ),
        );
    }
    run(cases);
}

// ---------------------------------------------------------------------------
// R13: start and config
// ---------------------------------------------------------------------------

fn theme_fx(dark: bool, font_scale: Option<f64>) -> Effect {
    Effect::ApplyTheme { dark, font_scale }
}

fn keys_fx(pairs: &[(&str, &str)]) -> Effect {
    let map: BTreeMap<String, String> = pairs
        .iter()
        .map(|(name, chord)| ((*name).to_string(), (*chord).to_string()))
        .collect();
    Effect::ApplyKeys(map)
}

/// A config reply holding `doc` as its `config` snapshot, from a closed shell;
/// the reply must change nothing but the effects it returns.
fn config_case(label: &str, doc: Value, effects: Vec<Effect>) -> Case {
    let body = Value::Object(std::iter::once(("config".to_string(), doc)).collect());
    Case::new(label, false)
        .ev(100, config(body), effects, &[])
        .inert()
}

#[test]
fn r13_config_applies_the_theme() {
    let theme = |value: Value| json!({"ui": {"theme": value}});
    run(vec![
        config_case(
            "dark with a scale",
            theme(json!({"mode": "dark", "font_scale": 1.25})),
            vec![theme_fx(true, Some(1.25))],
        ),
        config_case(
            "light with a scale",
            theme(json!({"mode": "light", "font_scale": 0.75})),
            vec![theme_fx(false, Some(0.75))],
        ),
        config_case(
            "light without a scale",
            theme(json!({"mode": "light"})),
            vec![theme_fx(false, None)],
        ),
        config_case(
            "dark without a scale",
            theme(json!({"mode": "dark"})),
            vec![theme_fx(true, None)],
        ),
        config_case(
            "a scale without a mode is dark",
            theme(json!({"font_scale": 1.5})),
            vec![theme_fx(true, Some(1.5))],
        ),
        config_case(
            "an empty theme is dark",
            theme(json!({})),
            vec![theme_fx(true, None)],
        ),
        config_case(
            "an unknown mode is dark",
            theme(json!({"mode": "solarized", "font_scale": 1.5})),
            vec![theme_fx(true, Some(1.5))],
        ),
        config_case(
            "a mode that is not a string is dark",
            theme(json!({"mode": 5})),
            vec![theme_fx(true, None)],
        ),
        config_case(
            "an integer scale is a number",
            theme(json!({"mode": "light", "font_scale": 2})),
            vec![theme_fx(false, Some(2.0))],
        ),
        config_case(
            "a string scale is no scale",
            theme(json!({"mode": "light", "font_scale": "1.5"})),
            vec![theme_fx(false, None)],
        ),
        config_case(
            "a null scale is no scale",
            theme(json!({"mode": "light", "font_scale": null})),
            vec![theme_fx(false, None)],
        ),
        config_case("no ui section, no effect", json!({}), nothing()),
        config_case(
            "a ui section without a theme, no effect",
            json!({"ui": {"other": 1}}),
            nothing(),
        ),
    ]);
}

#[test]
fn r13_config_applies_the_keys() {
    run(vec![
        config_case(
            "a string map",
            json!({"popup": {"keys": {"copy": "ctrl+c", "pin": "ctrl+p"}}}),
            vec![keys_fx(&[("copy", "ctrl+c"), ("pin", "ctrl+p")])],
        ),
        config_case(
            "an empty map is applied too",
            json!({"popup": {"keys": {}}}),
            vec![keys_fx(&[])],
        ),
        config_case(
            "a map with a value that is not a string",
            json!({"popup": {"keys": {"copy": "ctrl+c", "pin": 5}}}),
            nothing(),
        ),
        config_case("a list", json!({"popup": {"keys": ["ctrl+c"]}}), nothing()),
        config_case("a string", json!({"popup": {"keys": "ctrl+c"}}), nothing()),
        config_case("a number", json!({"popup": {"keys": 5}}), nothing()),
        config_case(
            "a popup section without keys",
            json!({"popup": {"hotkey": "ctrl+alt+h"}}),
            nothing(),
        ),
    ]);
}

#[test]
fn r13_config_effects_come_in_order_and_at_once() {
    let both = json!({
        "ui": {"theme": {"mode": "light", "font_scale": 1.5}},
        "popup": {"keys": {"copy": "ctrl+c"}, "dismiss_on_focus_loss": false},
    });
    let all_effects = vec![theme_fx(false, Some(1.5)), keys_fx(&[("copy", "ctrl+c")])];
    run(vec![
        config_case(
            "theme first, then keys",
            json!({
                "ui": {"theme": {"mode": "light", "font_scale": 1.5}},
                "popup": {"keys": {"copy": "ctrl+c"}},
            }),
            all_effects.clone(),
        ),
        Case::new("theme, keys and the flag in one reply", true)
            .ev(100, config(json!({ "config": both })), all_effects, &[])
            .ev(200, Event::FocusLost, nothing(), &[])
            .inert(),
        config_case(
            "a bad keys map does not stop the theme",
            json!({"ui": {"theme": {"mode": "dark"}}, "popup": {"keys": 5}}),
            vec![theme_fx(true, None)],
        ),
        Case::new("a reply with a popup open changes nothing else", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(
                200,
                config(json!({"config": {"ui": {"theme": {"mode": "dark"}}}})),
                vec![theme_fx(true, None)],
                &[],
            )
            .inert(),
        Case::new("a reply leaves the error line and the offline flag", false)
            .ev(10, error_ev("e"), nothing(), &[err("e")])
            .ev(20, offline_ev("o"), nothing(), &[offline(true)])
            .ev(
                100,
                config(json!({"config": {"ui": {"theme": {"mode": "dark"}}}})),
                vec![theme_fx(true, None)],
                &[],
            )
            .inert(),
        Case::new("a body that is not a snapshot has no effect", false)
            .ev(100, config(Value::Null), nothing(), &[])
            .ev(110, config(json!({})), nothing(), &[])
            .ev(120, config(json!([])), nothing(), &[])
            .ev(130, config(json!({"config": 5})), nothing(), &[])
            .ev(140, config(json!({"config": null})), nothing(), &[])
            .inert(),
    ]);
}

// ---------------------------------------------------------------------------
// R14: every open
// ---------------------------------------------------------------------------

/// The ways to open a closed popup and what each leaves in the getters.
fn open_paths() -> Vec<(&'static str, Event, Vec<Then>)> {
    vec![
        ("hotkey push", hotkey(7, 9), vec![ctx(7, 9), no_err()]),
        ("request push", request(), vec![no_err()]),
        ("tray click", Event::TrayToggle, vec![no_err()]),
        ("paste failure", paste_failed("boom"), vec![err("boom")]),
    ]
}

#[test]
fn r14_every_open_runs_the_sequence_and_clears_the_error() {
    let mut cases = Vec::new();
    for (label, event, after) in open_paths() {
        let mut changes = vec![open(true)];
        changes.extend(after);
        cases.push(
            Case::new(&format!("{label} from closed"), false)
                .ev(10, error_ev("old"), nothing(), &[err("old")])
                .ev(100, event, open_seq(), &changes),
        );
    }
    cases.push(
        Case::new("hotkey push from open", true)
            .ev(10, error_ev("old"), nothing(), &[err("old")])
            .ev(100, hotkey(7, 9), raise_and_open(), &[ctx(7, 9), no_err()]),
    );
    run(cases);
}

#[test]
fn r14_every_open_drops_a_waiting_paste() {
    let mut cases = Vec::new();
    for (label, event, after) in open_paths() {
        for confirmed in [false, true] {
            let mut changes = vec![open(true), no_due()];
            changes.extend(after.clone());
            let state = if confirmed { "confirmed" } else { "waiting" };
            let mut case = Case::new(&format!("{label}, paste {state} before the open"), true).ev(
                10,
                pick(1),
                deferred(),
                &[open(false)],
            );
            if confirmed {
                case = case.ev(20, Event::Hidden, nothing(), &[due(170)]);
            }
            cases.push(
                case.ev(100, event.clone(), open_seq(), &changes)
                    .ev(200, Event::Dismiss, hide(), &[open(false), no_ctx()])
                    .ev(300, Event::Hidden, nothing(), &[])
                    .tick(10_000, nothing(), &[]),
            );
        }
    }
    run(cases);
}

// ---------------------------------------------------------------------------
// R15, R16: refresh, settings, desired state
// ---------------------------------------------------------------------------

#[test]
fn r15_refresh_asks_for_the_history_in_any_state() {
    run(vec![
        Case::new("closed", false)
            .ev(100, Event::Refresh, refresh(), &[])
            .inert(),
        Case::new("open with a context", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, Event::Refresh, refresh(), &[])
            .inert(),
        Case::new("offline with an error line", true)
            .ev(100, offline_ev("down"), nothing(), &[offline(true)])
            .ev(200, error_ev("e"), nothing(), &[err("e")])
            .ev(300, Event::Refresh, refresh(), &[])
            .inert(),
        Case::new("with a paste waiting", true)
            .ev(100, pick(1), deferred(), &[open(false)])
            .ev(200, Event::Hidden, nothing(), &[due(350)])
            .ev(250, Event::Refresh, refresh(), &[])
            .inert(),
        Case::new("with the settings window open", false)
            .ev(100, Event::SettingsOpen, nothing(), &[settings(true)])
            .ev(200, Event::Refresh, refresh(), &[])
            .inert(),
        Case::new("each press asks again", false)
            .ev(100, Event::Refresh, refresh(), &[])
            .ev(200, Event::Refresh, refresh(), &[]),
    ]);
}

#[test]
fn r15_settings_open_and_close_change_only_the_settings_flag() {
    run(vec![
        Case::new("open and close with a popup context", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, Event::SettingsOpen, nothing(), &[settings(true)])
            .ev(300, Event::SettingsClose, nothing(), &[settings(false)]),
        Case::new("open while the popup is closed", false)
            .ev(100, Event::SettingsOpen, nothing(), &[settings(true)])
            .ev(200, request(), open_seq(), &[open(true)])
            .ev(300, Event::SettingsClose, nothing(), &[settings(false)]),
        Case::new("open twice, close twice", false)
            .ev(100, Event::SettingsOpen, nothing(), &[settings(true)])
            .ev(200, Event::SettingsOpen, nothing(), &[])
            .inert()
            .ev(300, Event::SettingsClose, nothing(), &[settings(false)])
            .ev(400, Event::SettingsClose, nothing(), &[])
            .inert(),
        Case::new("a popup hide keeps the settings window", false)
            .ev(100, Event::SettingsOpen, nothing(), &[settings(true)])
            .ev(200, request(), open_seq(), &[open(true)])
            .ev(300, Event::Dismiss, hide(), &[open(false)]),
        Case::new("a pick keeps the settings window", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(150, Event::SettingsOpen, nothing(), &[settings(true)])
            .ev(200, pick(1), sent(1, 7, Some(9)), &[open(false), no_ctx()]),
        Case::new("a push keeps the settings window", false)
            .ev(100, Event::SettingsOpen, nothing(), &[settings(true)])
            .ev(200, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(300, hotkey(8, 10), raise_and_open(), &[ctx(8, 10)])
            .ev(400, toggle(1, 2), hide(), &[open(false), no_ctx()]),
        Case::new("the settings window does not stop a focus-loss hide", true)
            .ev(100, Event::SettingsOpen, nothing(), &[settings(true)])
            .ev(200, Event::FocusLost, hide(), &[open(false)]),
        Case::new("a stop reply keeps the settings window", false)
            .ev(100, Event::SettingsOpen, nothing(), &[settings(true)])
            .ev(200, stopped(), nothing(), &[exit()]),
    ]);
}

#[test]
fn r15_settings_window_and_popup_state_stay_apart() {
    let mut cases = vec![
        Case::new(
            "open and close leave context, error line and offline flag",
            false,
        )
        .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
        .ev(110, error_ev("e"), nothing(), &[err("e")])
        .ev(120, offline_ev("o"), nothing(), &[offline(true)])
        .ev(200, Event::SettingsOpen, nothing(), &[settings(true)])
        .ev(300, Event::SettingsClose, nothing(), &[settings(false)]),
        Case::new("open and close leave the timers", true)
            .ev(10, Event::Quit, stop(), &[due(6_010)])
            .ev(20, pick(1), deferred(), &[open(false)])
            .ev(30, Event::Hidden, nothing(), &[due(180)])
            .ev(40, Event::SettingsOpen, nothing(), &[settings(true)])
            .ev(50, Event::SettingsClose, nothing(), &[settings(false)])
            .tick(180, released(1, None), &[due(6_010)])
            .tick(6_010, nothing(), &[exit(), no_due()]),
    ];
    for (label, event, effects) in hide_kinds() {
        cases.push(
            Case::new(&format!("{label} keeps the settings window"), false)
                .ev(100, Event::SettingsOpen, nothing(), &[settings(true)])
                .ev(150, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
                .ev(1_000, event, effects, &[open(false), no_ctx()]),
        );
    }
    for (label, event, after) in open_paths() {
        let mut changes = vec![open(true)];
        changes.extend(after);
        cases.push(
            Case::new(&format!("a {label} keeps the settings window"), false)
                .ev(100, Event::SettingsOpen, nothing(), &[settings(true)])
                .ev(200, event, open_seq(), &changes),
        );
    }
    run(cases);
}

#[test]
fn r16_desired_is_open_settings_and_exit() {
    assert_eq!(Shell::new(false).desired(), Desired::default());
    assert_eq!(
        Shell::new(true).desired(),
        Desired {
            popup_open: true,
            settings_open: false,
            exit: false,
        }
    );
    run(vec![Case::new("every field moves on its own", false)
        .ev(10, Event::SettingsOpen, nothing(), &[settings(true)])
        .ev(20, hotkey(1, 2), open_seq(), &[open(true), ctx(1, 2)])
        .ev(30, Event::Dismiss, hide(), &[open(false), no_ctx()])
        .ev(40, Event::SettingsClose, nothing(), &[settings(false)])
        .ev(50, request(), open_seq(), &[open(true)])
        .ev(60, stopped(), nothing(), &[exit()])]);
}

// ---------------------------------------------------------------------------
// Replies and keys that touch the error line
// ---------------------------------------------------------------------------

#[test]
fn error_line_events() {
    let blocked = text("popup.paste_blocked");
    assert_ne!(blocked, "popup.paste_blocked", "the id has a catalog text");
    assert_ne!(text("app.error"), "app.error", "the id has a catalog text");
    run(vec![
        Case::new("an error reply sets the line while closed", false).ev(
            100,
            error_ev("boom"),
            nothing(),
            &[err("boom")],
        ),
        Case::new("an error reply sets the line while open", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, error_ev("boom"), nothing(), &[err("boom")]),
        Case::new("a later error replaces the line", true)
            .ev(100, error_ev("one"), nothing(), &[err("one")])
            .ev(200, error_ev("two"), nothing(), &[err("two")]),
        Case::new(
            "paste unavailable sets the catalog text while closed",
            false,
        )
        .ev(100, paste_unavailable(), nothing(), &[err(blocked)]),
        Case::new("paste unavailable sets the catalog text while open", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, paste_unavailable(), nothing(), &[err(blocked)]),
        Case::new("paste unavailable replaces an older line", true)
            .ev(100, error_ev("older"), nothing(), &[err("older")])
            .ev(200, paste_unavailable(), nothing(), &[err(blocked)]),
        Case::new("a key clears the line", true)
            .ev(100, error_ev("boom"), nothing(), &[err("boom")])
            .ev(200, Event::Key, nothing(), &[no_err()]),
        Case::new("a key clears the line while closed", false)
            .ev(100, error_ev("boom"), nothing(), &[err("boom")])
            .ev(200, Event::Key, nothing(), &[no_err()]),
        Case::new("a key without a line changes nothing", false)
            .ev(100, Event::Key, nothing(), &[])
            .inert(),
        Case::new(
            "a key keeps the popup, its context and the offline flag",
            false,
        )
        .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
        .ev(150, offline_ev("down"), nothing(), &[offline(true)])
        .ev(200, error_ev("boom"), nothing(), &[err("boom")])
        .ev(300, Event::Key, nothing(), &[no_err()]),
        Case::new("a hide keeps the line", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, error_ev("boom"), nothing(), &[err("boom")])
            .ev(300, Event::Dismiss, hide(), &[open(false), no_ctx()]),
        Case::new("an offline reply keeps the line", true)
            .ev(100, error_ev("boom"), nothing(), &[err("boom")])
            .ev(200, offline_ev("down"), nothing(), &[offline(true)]),
        Case::new("a pick keeps the line", false)
            .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(200, error_ev("boom"), nothing(), &[err("boom")])
            .ev(300, pick(1), sent(1, 7, Some(9)), &[open(false), no_ctx()]),
    ]);
}

// ---------------------------------------------------------------------------
// Events that change nothing
// ---------------------------------------------------------------------------

/// Bridge events the shell does not handle.
fn ignored_events() -> Vec<(&'static str, Event)> {
    let bridge = Event::Bridge;
    vec![
        ("Deleted", bridge(UiEvent::Deleted(id(1)))),
        ("Pinned", bridge(UiEvent::Pinned(id(1), true))),
        ("PasteDone", paste_done(1)),
        (
            "ConfigApplied",
            bridge(UiEvent::ConfigApplied("popup.hotkey".to_string())),
        ),
        ("Caps", bridge(UiEvent::Caps(json!({"x11": true})))),
        (
            "HistoryClip",
            bridge(UiEvent::HistoryClip(
                json!({"clip": clip_json(1, "one", false)}),
            )),
        ),
        ("MonitorPaused", bridge(UiEvent::MonitorPaused(true))),
        ("Purged", bridge(UiEvent::Purged(3))),
        (
            "Permanents",
            bridge(UiEvent::Permanents(json!({"permanents": []}))),
        ),
        ("PermanentSaved", bridge(UiEvent::PermanentSaved)),
        ("PermanentDeleted", bridge(UiEvent::PermanentDeleted(id(2)))),
        (
            "PermGroups",
            bridge(UiEvent::PermGroups(json!({"groups": ["a"]}))),
        ),
        ("PermGroupsChanged", bridge(UiEvent::PermGroupsChanged)),
        (
            "BackupDone",
            bridge(UiEvent::BackupDone("/tmp/backup.zip".to_string())),
        ),
        ("RestoreDone", bridge(UiEvent::RestoreDone)),
        (
            "LogsTail",
            bridge(UiEvent::LogsTail(vec!["line".to_string()])),
        ),
        (
            "ConfigError",
            bridge(UiEvent::ConfigError("bad value".to_string())),
        ),
    ]
}

#[test]
fn unhandled_bridge_events_change_nothing() {
    let mut cases = Vec::new();
    for (label, event) in ignored_events() {
        cases.push(
            Case::new(&format!("{label} while closed"), false)
                .ev(100, event.clone(), nothing(), &[])
                .inert(),
        );
        cases.push(
            Case::new(
                &format!("{label} while open with a context, an error and offline"),
                false,
            )
            .ev(10, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
            .ev(20, error_ev("e"), nothing(), &[err("e")])
            .ev(30, offline_ev("o"), nothing(), &[offline(true)])
            .ev(100, event.clone(), nothing(), &[])
            .inert(),
        );
        cases.push(
            Case::new(&format!("{label} with a confirmed paste waiting"), true)
                .ev(10, pick(1), deferred(), &[open(false)])
                .ev(20, Event::Hidden, nothing(), &[due(170)])
                .ev(100, event.clone(), nothing(), &[])
                .inert(),
        );
        cases.push(
            Case::new(&format!("{label} after a quit"), false)
                .ev(10, Event::Quit, stop(), &[due(6_010)])
                .ev(100, event, nothing(), &[])
                .inert(),
        );
    }
    run(cases);
}

#[test]
fn events_in_the_wrong_state_change_nothing() {
    let closed_events = [
        ("pick", pick(1)),
        ("dismiss", Event::Dismiss),
        ("focus loss", Event::FocusLost),
        ("close request", Event::CloseRequest),
    ];
    let mut cases = Vec::new();
    for (label, event) in closed_events {
        cases.push(
            Case::new(&format!("{label} in a shell created closed"), false)
                .ev(100, event.clone(), nothing(), &[])
                .inert()
                .ev(200, Event::Hidden, nothing(), &[])
                .tick(10_000, nothing(), &[]),
        );
        cases.push(
            Case::new(&format!("{label} after a hide"), false)
                .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
                .ev(200, Event::Dismiss, hide(), &[open(false), no_ctx()])
                .ev(300, event, nothing(), &[])
                .inert(),
        );
    }
    cases.push(
        Case::new("a confirmation while the popup is open", true)
            .ev(100, Event::Hidden, nothing(), &[])
            .inert(),
    );
    cases.push(
        Case::new(
            "a confirmation while open does not start a paste timer",
            false,
        )
        .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
        .ev(150, Event::Hidden, nothing(), &[])
        .inert()
        .ev(200, pick(1), sent(1, 7, Some(9)), &[open(false), no_ctx()])
        .tick(10_000, nothing(), &[]),
    );
    cases.push(
        Case::new("a toggle push in a closed shell, then a tray click", false)
            .ev(100, toggle(1, 2), nothing(), &[])
            .inert()
            .ev(150, Event::TrayToggle, open_seq(), &[open(true)]),
    );
    run(cases);
}

/// Events that must not disturb a waiting timer, with their own result.
fn unrelated_events() -> Vec<(Event, Vec<Effect>, Vec<Then>)> {
    vec![
        (Event::Key, nothing(), vec![]),
        (Event::Refresh, refresh(), vec![]),
        (Event::SettingsOpen, nothing(), vec![settings(true)]),
        (Event::SettingsClose, nothing(), vec![settings(false)]),
        (Event::Dismiss, nothing(), vec![]),
        (Event::CloseRequest, nothing(), vec![]),
        (Event::FocusLost, nothing(), vec![]),
        (error_ev("e"), nothing(), vec![err("e")]),
        (offline_ev("o"), nothing(), vec![offline(true)]),
        (status_ev(), nothing(), vec![offline(false)]),
        (
            history_ev(Value::Null),
            nothing(),
            vec![offline(true), err(text("app.error"))],
        ),
        (
            history_ev(json!({"clips": []})),
            nothing(),
            vec![offline(false)],
        ),
        (config(flag_body(&json!(false))), nothing(), vec![]),
        (paste_done(9), nothing(), vec![]),
        (
            paste_unavailable(),
            nothing(),
            vec![err(text("popup.paste_blocked"))],
        ),
        (toggle(1, 2), nothing(), vec![]),
        (pick(9), nothing(), vec![]),
        (Event::Hidden, nothing(), vec![]),
    ]
}

#[test]
fn a_waiting_paste_survives_unrelated_events() {
    let mut case = Case::new("events between the confirmation and the release", true)
        .ev(10, pick(1), deferred(), &[open(false)])
        .ev(20, Event::Hidden, nothing(), &[due(170)]);
    for (n, (event, effects, then)) in (30..).zip(unrelated_events()) {
        case = case.ev(n, event, effects, &then);
    }
    run(vec![case.tick(169, nothing(), &[]).tick(
        170,
        released(1, None),
        &[no_due()],
    )]);
}

#[test]
fn a_quit_timer_survives_unrelated_events() {
    let mut case = Case::new("events between the quit and the exit", false).ev(
        1_000,
        Event::Quit,
        stop(),
        &[due(7_000)],
    );
    for (n, (event, effects, then)) in (1_001..).zip(unrelated_events()) {
        case = case.ev(n, event, effects, &then);
    }
    run(vec![case.tick(6_999, nothing(), &[]).tick(
        7_000,
        nothing(),
        &[exit(), no_due()],
    )]);
}

#[test]
fn a_hide_keeps_the_error_line() {
    let mut cases = Vec::new();
    for (label, event, effects) in hide_kinds() {
        cases.push(
            Case::new(&format!("{label} keeps the error line"), false)
                .ev(100, hotkey(7, 9), open_seq(), &[open(true), ctx(7, 9)])
                .ev(200, error_ev("boom"), nothing(), &[err("boom")])
                .ev(1_000, event, effects, &[open(false), no_ctx()]),
        );
    }
    run(cases);
}

#[test]
fn events_do_not_run_the_timers() {
    run(vec![
        Case::new("a due paste waits for the tick", true)
            .ev(10, pick(1), deferred(), &[open(false)])
            .ev(20, Event::Hidden, nothing(), &[due(170)])
            .ev(5_000, Event::Refresh, refresh(), &[])
            .ev(5_001, Event::Key, nothing(), &[])
            .tick(5_002, released(1, None), &[no_due()]),
        Case::new("a due quit waits for the tick", false)
            .ev(1_000, Event::Quit, stop(), &[due(7_000)])
            .ev(9_000, Event::Key, nothing(), &[])
            .ev(9_001, Event::Refresh, refresh(), &[])
            .tick(9_002, nothing(), &[exit(), no_due()]),
    ]);
}

// ---------------------------------------------------------------------------
// The deadline
// ---------------------------------------------------------------------------

#[test]
fn next_deadline_in_every_state() {
    run(vec![
        Case::new("nothing pending", false).tick(1_000, nothing(), &[]),
        Case::new("a quit alone", false)
            .ev(1_000, Event::Quit, stop(), &[due(7_000)])
            .tick(2_000, nothing(), &[])
            .tick(7_000, nothing(), &[exit(), no_due()]),
        Case::new("a waiting paste without a confirmation has none", true)
            .ev(1_000, pick(1), deferred(), &[open(false)])
            .tick(5_000, nothing(), &[]),
        Case::new("a confirmed paste alone", true)
            .ev(1_000, pick(1), deferred(), &[open(false)])
            .ev(1_200, Event::Hidden, nothing(), &[due(1_350)])
            .tick(1_350, released(1, None), &[no_due()]),
        Case::new("paste due first, then the quit", true)
            .ev(1_000, Event::Quit, stop(), &[due(7_000)])
            .ev(1_100, pick(1), deferred(), &[open(false)])
            .ev(1_200, Event::Hidden, nothing(), &[due(1_350)])
            .tick(1_350, released(1, None), &[due(7_000)])
            .tick(7_000, nothing(), &[exit(), no_due()]),
        Case::new("quit due first, the paste after it", true)
            .ev(1_000, Event::Quit, stop(), &[due(7_000)])
            .ev(6_500, pick(1), deferred(), &[open(false)])
            .ev(6_900, Event::Hidden, nothing(), &[])
            .tick(6_999, nothing(), &[])
            .tick(7_000, nothing(), &[exit(), due(7_050)])
            .tick(7_049, nothing(), &[])
            .tick(7_050, released(1, None), &[no_due()]),
        Case::new("both due at the same time", true)
            .ev(1_000, Event::Quit, stop(), &[due(7_000)])
            .ev(6_700, pick(1), deferred(), &[open(false)])
            .ev(6_850, Event::Hidden, nothing(), &[])
            .tick(6_999, nothing(), &[])
            .tick(7_000, released(1, None), &[exit(), no_due()]),
        Case::new("a stop reply cancels the quit deadline", false)
            .ev(1_000, Event::Quit, stop(), &[due(7_000)])
            .ev(2_000, stopped(), nothing(), &[exit(), no_due()]),
        Case::new("a stop reply leaves the paste deadline", true)
            .ev(1_000, pick(1), deferred(), &[open(false)])
            .ev(1_100, Event::Hidden, nothing(), &[due(1_250)])
            .ev(1_200, stopped(), nothing(), &[exit()])
            .tick(1_249, nothing(), &[])
            .tick(1_250, released(1, None), &[no_due()]),
    ]);
}

// ---------------------------------------------------------------------------
// popup::parse_history and popup::apply_history
// ---------------------------------------------------------------------------

#[test]
fn popup_parse_history() {
    let one = clip(1, "one", false);
    let two = clip(2, "two", true);
    assert_eq!(
        popup::parse_history(&json!({"clips": []})),
        Some(Vec::new()),
        "an empty list is well-formed"
    );
    assert_eq!(
        popup::parse_history(&history_body(&[one.clone(), two.clone()])),
        Some(vec![one.clone(), two.clone()]),
        "two clips, in order"
    );
    assert_eq!(
        popup::parse_history(&json!({"clips": [], "total": 0})),
        Some(Vec::new()),
        "other fields of the body are ignored"
    );
    for (label, body) in malformed_bodies() {
        assert_eq!(popup::parse_history(&body), None, "{label}");
    }
}

/// One reply applied to the popup state in sequence.
struct Reply {
    label: &'static str,
    body: Value,
    want: HistoryApply,
    shown: Vec<Clip>,
}

/// A well-formed reply holding `list`; afterwards the popup shows `list`.
fn list_reply(label: &'static str, list: &[Clip], want: HistoryApply) -> Reply {
    Reply {
        label,
        body: history_body(list),
        want,
        shown: list.to_vec(),
    }
}

#[test]
fn popup_apply_history_reports_what_it_did() {
    let a = clip(1, "one", false);
    let b = clip(2, "two", false);
    let c = clip(3, "three", false);
    let a_pinned = clip(1, "one", true);
    let a_renamed = clip(1, "uno", false);
    let a_other_id = clip(9, "one", false);
    let base = vec![a.clone(), b.clone()];
    let mut steps = vec![
        list_reply("empty onto empty", &[], HistoryApply::Unchanged),
        list_reply("first list", &base, HistoryApply::Changed),
        list_reply("the same list", &base, HistoryApply::Unchanged),
        list_reply(
            "other order",
            &[b.clone(), a.clone()],
            HistoryApply::Changed,
        ),
        list_reply("back to the first order", &base, HistoryApply::Changed),
        list_reply(
            "only a pin flag differs",
            &[a_pinned, b.clone()],
            HistoryApply::Changed,
        ),
        list_reply("back to the first list", &base, HistoryApply::Changed),
        list_reply(
            "only a preview differs",
            &[a_renamed, b.clone()],
            HistoryApply::Changed,
        ),
        list_reply("back to the first list again", &base, HistoryApply::Changed),
        list_reply(
            "only an id differs",
            &[a_other_id, b.clone()],
            HistoryApply::Changed,
        ),
        list_reply(
            "back to the first list once more",
            &base,
            HistoryApply::Changed,
        ),
        list_reply("shorter", std::slice::from_ref(&a), HistoryApply::Changed),
        list_reply(
            "longer",
            &[a.clone(), b.clone(), c.clone()],
            HistoryApply::Changed,
        ),
        list_reply(
            "the longer list again",
            &[a.clone(), b.clone(), c.clone()],
            HistoryApply::Unchanged,
        ),
    ];
    // Malformed replies leave the shown list alone, wherever they come.
    let shown = vec![a, b, c];
    for (label, body) in malformed_bodies() {
        steps.push(Reply {
            label,
            body,
            want: HistoryApply::Malformed,
            shown: shown.clone(),
        });
    }
    steps.push(list_reply("list to empty", &[], HistoryApply::Changed));
    steps.push(list_reply(
        "empty onto empty again",
        &[],
        HistoryApply::Unchanged,
    ));
    let mut vm = PopupVm::new();
    let mut failures = Vec::new();
    for step in steps {
        let got = vm.apply_history(&step.body);
        if got != step.want {
            failures.push(format!(
                "{}: reported {got:?}, expected {:?}",
                step.label, step.want
            ));
        }
        if vm.clips() != step.shown.as_slice() {
            failures.push(format!(
                "{}: shows {:?}, expected {:?}",
                step.label,
                vm.clips(),
                step.shown
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn popup_apply_history_keeps_the_selection_in_range() {
    let a = clip(1, "one", false);
    let b = clip(2, "two", false);
    let c = clip(3, "three", false);
    let mut vm = PopupVm::new();
    assert_eq!(
        vm.apply_history(&history_body(&[a.clone(), b.clone(), c.clone()])),
        HistoryApply::Changed
    );
    vm.selected = 2;
    assert_eq!(
        vm.apply_history(&history_body(&[a.clone(), b.clone(), c.clone()])),
        HistoryApply::Unchanged
    );
    assert_eq!(vm.selected, 2, "an unchanged list keeps the selection");
    assert_eq!(
        vm.apply_history(&json!({"clips": "none"})),
        HistoryApply::Malformed
    );
    assert_eq!(vm.selected, 2, "a malformed reply keeps the selection");
    assert_eq!(vm.clips().len(), 3, "a malformed reply keeps the list");
    assert_eq!(
        vm.apply_history(&history_body(&[a.clone(), b.clone()])),
        HistoryApply::Changed
    );
    assert_eq!(vm.selected, 1, "a shorter list pulls the selection in");
    assert_eq!(vm.apply_history(&history_body(&[a])), HistoryApply::Changed);
    assert_eq!(vm.selected, 0, "a shorter list pulls the selection in");
    assert_eq!(
        vm.apply_history(&json!({"clips": []})),
        HistoryApply::Changed
    );
    assert_eq!(vm.selected, 0, "an empty list leaves the selection at zero");
}

// ---------------------------------------------------------------------------
// The bridge: stop and transport errors
// ---------------------------------------------------------------------------

/// A transport that answers every call with the same outcome.
struct Canned(Result<Value, IpcError>);

impl Transport for Canned {
    fn call(&mut self, _method: &str, _params: &Value) -> Result<Value, IpcError> {
        self.0.clone()
    }
}

fn failure(code: &str, message: &str) -> Result<Value, IpcError> {
    Err(IpcError::new(code, message.to_string()))
}

/// Send one command through a bridge over a transport that answers `outcome`.
fn bridge_event(cmd: UiCmd, outcome: Result<Value, IpcError>) -> UiEvent {
    let (commands, events) = Bridge::spawn(Box::new(Canned(outcome)));
    commands.send(cmd).expect("the bridge thread is running");
    events
        .recv_timeout(Duration::from_secs(5))
        .expect("the bridge answers")
}

#[test]
fn bridge_stop_outcomes() {
    for (code, message) in [
        ("internal", "stop refused"),
        ("unavailable", "daemon not running"),
        ("denied", "not allowed"),
        ("io_failed", "broken pipe"),
        ("connection_closed", "connection closed"),
    ] {
        assert_eq!(
            bridge_event(UiCmd::Stop, failure(code, message)),
            UiEvent::StopFailed(message.to_string()),
            "daemon.stop failing with code {code}"
        );
    }
    for body in [json!({}), json!({"stopping": true}), Value::Null] {
        assert_eq!(
            bridge_event(UiCmd::Stop, Ok(body.clone())),
            UiEvent::Stopped,
            "daemon.stop answering {body}"
        );
    }
}

#[test]
fn bridge_status_and_history_failures() {
    let transport_codes = ["unavailable", "io_failed", "connection_closed"];
    let other_codes = [
        "denied", "internal", "timeout", "io_error", "closed", "backend",
    ];
    for cmd in [UiCmd::Status, UiCmd::ListHistory] {
        for code in transport_codes {
            assert_eq!(
                bridge_event(cmd.clone(), failure(code, "daemon down")),
                UiEvent::Offline("daemon down".to_string()),
                "{cmd:?} failing with code {code}"
            );
        }
        for code in other_codes {
            assert_eq!(
                bridge_event(cmd.clone(), failure(code, "daemon said no")),
                UiEvent::Error("daemon said no".to_string()),
                "{cmd:?} failing with code {code}"
            );
        }
    }
    let status = json!({"version": "1", "monitoring_paused": false});
    assert_eq!(
        bridge_event(UiCmd::Status, Ok(status.clone())),
        UiEvent::Status(status)
    );
    let history = history_body(&[clip(1, "one", false)]);
    assert_eq!(
        bridge_event(UiCmd::ListHistory, Ok(history.clone())),
        UiEvent::History(history)
    );
    for cmd in [UiCmd::ConfigGet, UiCmd::ListPermanents, UiCmd::PermGroups] {
        for code in transport_codes {
            assert_eq!(
                bridge_event(cmd.clone(), failure(code, "daemon down")),
                UiEvent::Error("daemon down".to_string()),
                "{cmd:?} failing with code {code} stays an error"
            );
        }
    }
}
