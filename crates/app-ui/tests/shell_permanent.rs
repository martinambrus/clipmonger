//! Rules of the permanent pick in the shell state machine: `Event::PickPermanent`,
//! the release of the pending exec by `Shell::tick` and the deadline reported by
//! `Shell::next_deadline`, as written in the doc comments of `shell.rs`.
//!
//! The tests are tables of named cases. A case builds a shell, feeds it events and
//! ticks at given times and, after every step, compares the exact effect list and
//! every getter (`desired`, `context`, `error`, `offline`, `next_deadline`) with the
//! expectation. A step lists only what it changes: a getter it does not mention must
//! still hold the value from before. Cases run on the times of the table and on a
//! clock that starts much later (only differences between times matter), except the
//! cases that pin the arithmetic at the end of the `u64` range. A failure names the
//! case and the step.
//!
//! Run: `cargo test -p clipmonger-app-ui --test shell_permanent`

use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};

use clipmonger_app_ui::ipc_bridge::{UiCmd, UiEvent};
use clipmonger_app_ui::shell::{
    Context, Desired, Effect, Event, Millis, Shell, PASTE_DELAY_MS, QUIT_TIMEOUT_MS, TRAY_GUARD_MS,
};
use clipmonger_ipc::proto::methods::ShowPopupEvent;
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
    Then::Ctx(Context {
        target: Some(target),
        gesture: Some(gesture),
    })
}

fn no_ctx() -> Then {
    Then::Ctx(Context::default())
}

fn err(text: &str) -> Then {
    Then::Error(Some(text.to_string()))
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
}

struct Case {
    name: String,
    open: bool,
    /// Clock shifts the case runs on.
    shifts: &'static [Millis],
    steps: Vec<Step>,
}

/// A clock that starts this long after the times of the tables.
const LATE_CLOCK_MS: Millis = 7_000_000_000;

impl Case {
    fn new(name: &str, open: bool) -> Self {
        Self {
            name: name.to_string(),
            open,
            shifts: &[0, LATE_CLOCK_MS],
            steps: Vec::new(),
        }
    }

    /// Run on the table's clock only: the times are absolute.
    fn fixed_clock(mut self) -> Self {
        self.shifts = &[0];
        self
    }

    /// Feed one event at `at` and expect `effects` and the listed changes.
    fn ev(mut self, at: Millis, event: Event, effects: Vec<Effect>, then: &[Then]) -> Self {
        self.steps.push(Step {
            at,
            input: Input::Handle(event),
            effects,
            then: then.to_vec(),
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
        });
        self
    }
}

fn run_input(shell: &mut Shell, at: Millis, input: &Input) -> Vec<Effect> {
    match input {
        Input::Handle(event) => shell.handle(event.clone(), at),
        Input::Tick => shell.tick(at),
    }
}

/// Run one case on a clock that starts `shift` milliseconds after the times of
/// the table.
fn run_case(case: &Case, shift: Millis) -> Result<(), String> {
    let mut shell = Shell::new(case.open);
    let mut want = View::fresh(case.open);
    let mut clock = 0;
    for (n, step) in case.steps.iter().enumerate() {
        let at = step.at + shift;
        let label = format!("step {n} ({:?} at {at} ms)", step.input);
        if step.at < clock {
            return Err(format!("{label}: the table moves the clock backwards"));
        }
        clock = step.at;
        let got = run_input(&mut shell, at, &step.input);
        if got != step.effects {
            return Err(format!(
                "{label}: effects {got:?}, expected {:?}",
                step.effects
            ));
        }
        want.apply(&step.then, shift);
        let have = View::of(&shell);
        if have != want {
            return Err(format!("{label}: getters {have:?}, expected {want:?}"));
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

/// Run every case on its clocks; report all failures together, each with its
/// case name.
fn run(cases: Vec<Case>) {
    let total = cases.len();
    let mut failures = Vec::new();
    for case in cases {
        for &shift in case.shifts {
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

fn request() -> Event {
    push("request", false, None, None)
}

fn toggle_push() -> Event {
    push("hotkey", true, Some(1), Some(2))
}

fn pick_clip(n: u128) -> Event {
    Event::Pick {
        id: id(n),
        picked_ms: PICKED_MS,
    }
}

fn pick_perm(n: u128) -> Event {
    Event::PickPermanent { id: id(n) }
}

fn paste_failed(text: &str) -> Event {
    Event::Bridge(UiEvent::PasteFailed(text.to_string()))
}

fn error_ev(text: &str) -> Event {
    Event::Bridge(UiEvent::Error(text.to_string()))
}

fn nothing() -> Vec<Effect> {
    Vec::new()
}

/// The effects of a pick: the foreground right, then the hide.
fn picked() -> Vec<Effect> {
    vec![Effect::AllowForeground, Effect::Hide]
}

/// The open sequence.
fn opened() -> Vec<Effect> {
    vec![
        Effect::ClearSearch,
        Effect::Send(UiCmd::ListHistory),
        Effect::SelectNewest,
        Effect::FocusSearch,
    ]
}

/// The open sequence of a push that finds the popup open.
fn raised() -> Vec<Effect> {
    let mut effects = vec![Effect::Raise];
    effects.extend(opened());
    effects
}

fn exec(n: u128) -> Vec<Effect> {
    vec![Effect::Send(UiCmd::ExecPermanent(id(n)))]
}

fn paste_to_foreground(n: u128) -> Vec<Effect> {
    vec![Effect::Send(UiCmd::PasteClipTo(
        id(n),
        None,
        PICKED_MS,
        None,
    ))]
}

// ---------------------------------------------------------------------------
// The pick itself
// ---------------------------------------------------------------------------

fn pick_cases() -> Vec<Case> {
    vec![
        // closed: nothing happens, nothing is remembered
        Case::new(
            "closed: the pick returns nothing and changes nothing",
            false,
        )
        .ev(0, pick_perm(1), nothing(), &[])
        .ev(10, Event::Hidden, nothing(), &[])
        .tick(1_000, nothing(), &[])
        .tick(100_000, nothing(), &[]),
        // open: the foreground right, then the hide, nothing more now
        Case::new("open: foreground right then hide, no send", true)
            .ev(0, pick_perm(1), picked(), &[open(false)])
            .tick(0, nothing(), &[])
            .tick(10_000, nothing(), &[]),
        // the context is ignored and cleared
        Case::new(
            "open with target and gesture: same effects, context cleared",
            false,
        )
        .ev(0, hotkey(7, 9), opened(), &[open(true), ctx(7, 9)])
        .ev(5, pick_perm(1), picked(), &[open(false), no_ctx()])
        .ev(10, Event::Hidden, nothing(), &[due(160)])
        .tick(159, nothing(), &[])
        .tick(160, exec(1), &[no_due()])
        .tick(1_000, nothing(), &[]),
        // a hotkey target, no gesture
        Case::new("open with a target only: still no targeted paste", false)
            .ev(
                0,
                push("hotkey", false, Some(5), None),
                opened(),
                &[
                    open(true),
                    Then::Ctx(Context {
                        target: Some(5),
                        gesture: None,
                    }),
                ],
            )
            .ev(5, pick_perm(1), picked(), &[open(false), no_ctx()])
            .ev(10, Event::Hidden, nothing(), &[due(160)])
            .tick(160, exec(1), &[no_due()]),
        // the error line is not a part of the hide
        Case::new("open with an error line: the pick keeps it", true)
            .ev(0, error_ev("boom"), nothing(), &[err("boom")])
            .ev(5, pick_perm(1), picked(), &[open(false)])
            .ev(10, Event::Hidden, nothing(), &[due(160)])
            .tick(160, exec(1), &[no_due()]),
        // the effects equal those of a clip pick without a target
        Case::new("a clip pick without target has the same effects", true)
            .ev(0, pick_clip(1), picked(), &[open(false)])
            .ev(10, Event::Hidden, nothing(), &[due(160)])
            .tick(160, paste_to_foreground(1), &[no_due()]),
        // the hide of a pick is not a focus-loss hide: the tray opens it at once
        Case::new("tray click right after the pick is not guarded", true)
            .ev(0, pick_perm(1), picked(), &[open(false)])
            .ev(
                TRAY_GUARD_MS / 10,
                Event::TrayToggle,
                opened(),
                &[open(true)],
            )
            .tick(1_000, nothing(), &[]),
    ]
}

// PickPermanent: closed does nothing; open gives [AllowForeground, Hide], clears the context, sends nothing yet
#[test]
fn rule_permanent_pick_cases() {
    run(pick_cases());
}

// ---------------------------------------------------------------------------
// The release
// ---------------------------------------------------------------------------

fn release_cases() -> Vec<Case> {
    vec![
        Case::new("no send before the popup is confirmed hidden", true)
            .ev(0, pick_perm(1), picked(), &[open(false)])
            .tick(0, nothing(), &[])
            .tick(149, nothing(), &[])
            .tick(150, nothing(), &[])
            .tick(151, nothing(), &[])
            .tick(100_000, nothing(), &[]),
        Case::new(
            "released exactly at hidden + 150 ms, not 1 ms earlier",
            true,
        )
        .ev(0, pick_perm(1), picked(), &[open(false)])
        .ev(10, Event::Hidden, nothing(), &[due(10 + PASTE_DELAY_MS)])
        .tick(10, nothing(), &[])
        .tick(158, nothing(), &[])
        .tick(159, nothing(), &[])
        .tick(160, exec(1), &[no_due()])
        .tick(161, nothing(), &[])
        .tick(100_000, nothing(), &[]),
        Case::new(
            "the delay counts from the confirmation, not from the pick",
            true,
        )
        .ev(0, pick_perm(1), picked(), &[open(false)])
        .tick(499, nothing(), &[])
        .ev(500, Event::Hidden, nothing(), &[due(650)])
        .tick(649, nothing(), &[])
        .tick(650, exec(1), &[no_due()]),
        Case::new("a late tick releases the exec once", true)
            .ev(0, pick_perm(1), picked(), &[open(false)])
            .ev(10, Event::Hidden, nothing(), &[due(160)])
            .tick(9_999, exec(1), &[no_due()])
            .tick(10_000, nothing(), &[])
            .tick(20_000, nothing(), &[]),
        Case::new("the first confirmation counts", true)
            .ev(0, pick_perm(1), picked(), &[open(false)])
            .ev(10, Event::Hidden, nothing(), &[due(160)])
            .ev(100, Event::Hidden, nothing(), &[])
            .ev(150, Event::Hidden, nothing(), &[])
            .tick(159, nothing(), &[])
            .tick(160, exec(1), &[no_due()]),
        Case::new("a confirmation while the popup is open is ignored", true)
            .ev(0, Event::Hidden, nothing(), &[])
            .ev(5, pick_perm(1), picked(), &[open(false)])
            .tick(1_000, nothing(), &[])
            .ev(1_000, Event::Hidden, nothing(), &[due(1_150)])
            .tick(1_149, nothing(), &[])
            .tick(1_150, exec(1), &[no_due()]),
        Case::new(
            "a confirmation without a pick stores nothing to release",
            false,
        )
        .ev(0, Event::Hidden, nothing(), &[])
        .tick(150, nothing(), &[])
        .tick(1_000, nothing(), &[]),
        Case::new(
            "the exec is released only once even when the clock stalls",
            true,
        )
        .ev(0, pick_perm(1), picked(), &[open(false)])
        .ev(10, Event::Hidden, nothing(), &[due(160)])
        .tick(160, exec(1), &[no_due()])
        .tick(160, nothing(), &[])
        .tick(160, nothing(), &[]),
        Case::new("a second cycle releases the second id only", true)
            .ev(0, pick_perm(1), picked(), &[open(false)])
            .ev(10, Event::Hidden, nothing(), &[due(160)])
            .tick(160, exec(1), &[no_due()])
            .ev(200, hotkey(3, 4), opened(), &[open(true), ctx(3, 4)])
            .ev(210, pick_perm(2), picked(), &[open(false), no_ctx()])
            .ev(220, Event::Hidden, nothing(), &[due(370)])
            .tick(369, nothing(), &[])
            .tick(370, exec(2), &[no_due()]),
    ]
}

// tick: no send before Hidden; ExecPermanent exactly at hidden + 150 ms, once; the first confirmation counts
#[test]
fn rule_permanent_release_cases() {
    run(release_cases());
}

// ---------------------------------------------------------------------------
// What drops or does not drop the pending exec
// ---------------------------------------------------------------------------

/// A case that picks a permanent, confirms the hide at 10 ms and then lets
/// `opener` open the popup at `open_at`; the open must drop the exec.
fn dropped_by(
    name: &str,
    opener: Event,
    open_at: Millis,
    effects: Vec<Effect>,
    changes: &[Then],
) -> Case {
    Case::new(name, true)
        .ev(0, pick_perm(1), picked(), &[open(false)])
        .ev(10, Event::Hidden, nothing(), &[due(160)])
        .ev(open_at, opener, effects, changes)
        .tick(160, nothing(), &[])
        .tick(1_000, nothing(), &[])
        .ev(
            1_001,
            Event::Dismiss,
            vec![Effect::Hide],
            &[open(false), no_ctx()],
        )
        .ev(1_002, Event::Hidden, nothing(), &[])
        .tick(1_200, nothing(), &[])
        .tick(100_000, nothing(), &[])
}

fn drop_cases() -> Vec<Case> {
    let with_deadline = [no_due(), open(true), no_ctx()];
    vec![
        dropped_by(
            "a hotkey push drops the exec",
            hotkey(5, 6),
            20,
            opened(),
            &[no_due(), open(true), ctx(5, 6)],
        ),
        dropped_by(
            "a request push drops the exec",
            request(),
            20,
            opened(),
            &with_deadline,
        ),
        dropped_by(
            "a tray click drops the exec",
            Event::TrayToggle,
            20,
            opened(),
            &with_deadline,
        ),
        dropped_by(
            "a failed paste that re-opens the popup drops the exec",
            paste_failed("gone"),
            20,
            opened(),
            &[no_due(), open(true), err("gone")],
        ),
        // the open comes before the hide is confirmed: nothing is waiting for it
        Case::new("an open before the confirmation drops the exec too", true)
            .ev(0, pick_perm(1), picked(), &[open(false)])
            .ev(5, request(), opened(), &[open(true)])
            .ev(8, Event::Dismiss, vec![Effect::Hide], &[open(false)])
            .ev(10, Event::Hidden, nothing(), &[])
            .tick(160, nothing(), &[])
            .tick(100_000, nothing(), &[]),
        Case::new(
            "a hotkey push while open replaces the context, the pick ignores it",
            true,
        )
        .ev(0, hotkey(1, 2), raised(), &[ctx(1, 2)])
        .ev(5, pick_perm(1), picked(), &[open(false), no_ctx()])
        .ev(10, Event::Hidden, nothing(), &[due(160)])
        .tick(160, exec(1), &[no_due()]),
    ]
}

// any open (hotkey, request, tray, failed paste) drops the pending exec, confirmed or not
#[test]
fn rule_permanent_open_drops_the_exec() {
    run(drop_cases());
}

fn keep_cases() -> Vec<Case> {
    vec![
        Case::new(
            "a toggle push while closed does not open and keeps the exec",
            true,
        )
        .ev(0, pick_perm(1), picked(), &[open(false)])
        .ev(10, Event::Hidden, nothing(), &[due(160)])
        .ev(20, toggle_push(), nothing(), &[])
        .tick(160, exec(1), &[no_due()]),
        Case::new("events that open nothing keep the exec", true)
            .ev(0, pick_perm(1), picked(), &[open(false)])
            .ev(10, Event::Hidden, nothing(), &[due(160)])
            .ev(20, Event::Dismiss, nothing(), &[])
            .ev(25, Event::FocusLost, nothing(), &[])
            .ev(30, Event::CloseRequest, nothing(), &[])
            .ev(40, Event::Key, nothing(), &[])
            .ev(50, Event::SettingsOpen, nothing(), &[settings(true)])
            .ev(60, Event::SettingsClose, nothing(), &[settings(false)])
            .ev(
                70,
                Event::Refresh,
                vec![Effect::Send(UiCmd::ListHistory)],
                &[],
            )
            .ev(80, error_ev("late"), nothing(), &[err("late")])
            .tick(159, nothing(), &[])
            .tick(160, exec(1), &[no_due()]),
        Case::new("a second permanent pick while closed is ignored", true)
            .ev(0, pick_perm(1), picked(), &[open(false)])
            .ev(5, pick_perm(2), nothing(), &[])
            .ev(10, Event::Hidden, nothing(), &[due(160)])
            .ev(20, pick_perm(3), nothing(), &[])
            .tick(160, exec(1), &[no_due()])
            .tick(1_000, nothing(), &[]),
        Case::new("a clip pick while an exec waits is ignored", true)
            .ev(0, pick_perm(1), picked(), &[open(false)])
            .ev(5, pick_clip(2), nothing(), &[])
            .ev(10, Event::Hidden, nothing(), &[due(160)])
            .tick(160, exec(1), &[no_due()])
            .tick(1_000, nothing(), &[]),
        Case::new("a permanent pick while a clip paste waits is ignored", true)
            .ev(0, pick_clip(2), picked(), &[open(false)])
            .ev(5, pick_perm(1), nothing(), &[])
            .ev(10, Event::Hidden, nothing(), &[due(160)])
            .tick(160, paste_to_foreground(2), &[no_due()])
            .tick(1_000, nothing(), &[]),
    ]
}

// events that open nothing leave the pending exec alone; a pick while closed is ignored
#[test]
fn rule_permanent_exec_survives_what_does_not_open() {
    run(keep_cases());
}

fn interplay_cases() -> Vec<Case> {
    vec![
        Case::new(
            "an open drops the clip paste and the next exec goes alone",
            true,
        )
        .ev(0, pick_clip(2), picked(), &[open(false)])
        .ev(10, Event::Hidden, nothing(), &[due(160)])
        .ev(
            20,
            hotkey(3, 4),
            opened(),
            &[open(true), ctx(3, 4), no_due()],
        )
        .ev(30, pick_perm(1), picked(), &[open(false), no_ctx()])
        .ev(40, Event::Hidden, nothing(), &[due(190)])
        .tick(189, nothing(), &[])
        .tick(190, exec(1), &[no_due()])
        .tick(1_000, nothing(), &[]),
        Case::new(
            "an open drops the exec and the next clip paste goes alone",
            true,
        )
        .ev(0, pick_perm(1), picked(), &[open(false)])
        .ev(10, Event::Hidden, nothing(), &[due(160)])
        .ev(20, request(), opened(), &[open(true), no_due()])
        .ev(30, pick_clip(2), picked(), &[open(false)])
        .ev(40, Event::Hidden, nothing(), &[due(190)])
        .tick(190, paste_to_foreground(2), &[no_due()])
        .tick(1_000, nothing(), &[]),
        Case::new(
            "a clip pick with a target sends at once and leaves no exec",
            true,
        )
        .ev(0, hotkey(8, 9), raised(), &[ctx(8, 9)])
        .ev(
            5,
            pick_clip(2),
            vec![
                Effect::AllowForeground,
                Effect::Hide,
                Effect::Send(UiCmd::PasteClipTo(id(2), Some(8), PICKED_MS, Some(9))),
            ],
            &[open(false), no_ctx()],
        )
        .ev(10, Event::Hidden, nothing(), &[])
        .tick(160, nothing(), &[]),
    ]
}

// a pending clip paste and a pending exec never both fire: an open drops either one
#[test]
fn rule_permanent_exec_and_clip_paste_do_not_mix() {
    run(interplay_cases());
}

// ---------------------------------------------------------------------------
// Deadlines and quit
// ---------------------------------------------------------------------------

fn deadline_cases() -> Vec<Case> {
    vec![
        Case::new(
            "no pending exec: no deadline, a stored confirmation changes nothing",
            false,
        )
        .ev(0, Event::Hidden, nothing(), &[])
        .tick(150, nothing(), &[]),
        Case::new(
            "a quit and an exec: the smaller deadline, then the other",
            true,
        )
        .ev(
            0,
            Event::Quit,
            vec![Effect::Send(UiCmd::Stop)],
            &[due(QUIT_TIMEOUT_MS)],
        )
        .ev(10, pick_perm(1), picked(), &[open(false)])
        .ev(20, Event::Hidden, nothing(), &[due(170)])
        .tick(169, nothing(), &[])
        .tick(170, exec(1), &[due(QUIT_TIMEOUT_MS)])
        .tick(QUIT_TIMEOUT_MS - 1, nothing(), &[])
        .tick(QUIT_TIMEOUT_MS, nothing(), &[exit(), no_due()]),
        Case::new(
            "an exec due after the quit deadline outlives the exit",
            true,
        )
        .ev(
            0,
            Event::Quit,
            vec![Effect::Send(UiCmd::Stop)],
            &[due(QUIT_TIMEOUT_MS)],
        )
        .ev(5_000, pick_perm(1), picked(), &[open(false)])
        .ev(5_900, Event::Hidden, nothing(), &[due(QUIT_TIMEOUT_MS)])
        .tick(
            QUIT_TIMEOUT_MS,
            nothing(),
            &[exit(), due(5_900 + PASTE_DELAY_MS)],
        )
        .tick(5_900 + PASTE_DELAY_MS - 1, nothing(), &[])
        .tick(5_900 + PASTE_DELAY_MS, exec(1), &[no_due()]),
        Case::new("an exec and the exit that fall due in the same tick", true)
            .ev(
                0,
                Event::Quit,
                vec![Effect::Send(UiCmd::Stop)],
                &[due(QUIT_TIMEOUT_MS)],
            )
            .ev(5_000, pick_perm(1), picked(), &[open(false)])
            .ev(
                QUIT_TIMEOUT_MS - PASTE_DELAY_MS,
                Event::Hidden,
                nothing(),
                &[due(QUIT_TIMEOUT_MS)],
            )
            .tick(QUIT_TIMEOUT_MS - 1, nothing(), &[])
            .tick(QUIT_TIMEOUT_MS, exec(1), &[exit(), no_due()]),
        Case::new("a quit after the pick joins the deadlines", true)
            .ev(0, pick_perm(1), picked(), &[open(false)])
            .ev(10, Event::Hidden, nothing(), &[due(160)])
            .ev(20, Event::Quit, vec![Effect::Send(UiCmd::Stop)], &[])
            .tick(160, exec(1), &[due(20 + QUIT_TIMEOUT_MS)])
            .tick(20 + QUIT_TIMEOUT_MS, nothing(), &[exit(), no_due()]),
        Case::new("a second quit changes no deadline", true)
            .ev(0, pick_perm(1), picked(), &[open(false)])
            .ev(10, Event::Hidden, nothing(), &[due(160)])
            .ev(20, Event::Quit, vec![Effect::Send(UiCmd::Stop)], &[])
            .ev(30, Event::Quit, nothing(), &[])
            .tick(160, exec(1), &[due(20 + QUIT_TIMEOUT_MS)]),
    ]
}

// next_deadline is the smaller of the exec deadline and the quit deadline; exit stays set, the exec outlives it
#[test]
fn rule_permanent_deadlines_and_quit() {
    run(deadline_cases());
}

// ---------------------------------------------------------------------------
// Arithmetic at the end of the clock
// ---------------------------------------------------------------------------

fn saturation_cases() -> Vec<Case> {
    let end = u64::MAX;
    vec![
        Case::new("hidden at the last millisecond: due then, no wrap", true)
            .fixed_clock()
            .ev(0, pick_perm(1), picked(), &[open(false)])
            .ev(end, Event::Hidden, nothing(), &[due(end)])
            .tick(end, exec(1), &[no_due()]),
        Case::new("hidden 100 ms before the end: the deadline saturates", true)
            .fixed_clock()
            .ev(0, pick_perm(1), picked(), &[open(false)])
            .ev(end - 100, Event::Hidden, nothing(), &[due(end)])
            .tick(end - 1, nothing(), &[])
            .tick(end, exec(1), &[no_due()]),
        Case::new(
            "hidden 149 ms before the end: one past the end saturates",
            true,
        )
        .fixed_clock()
        .ev(0, pick_perm(1), picked(), &[open(false)])
        .ev(end - 149, Event::Hidden, nothing(), &[due(end)])
        .tick(end - 1, nothing(), &[])
        .tick(end, exec(1), &[no_due()]),
        Case::new("hidden 150 ms before the end: due exactly at the end", true)
            .fixed_clock()
            .ev(0, pick_perm(1), picked(), &[open(false)])
            .ev(end - 150, Event::Hidden, nothing(), &[due(end)])
            .tick(end - 1, nothing(), &[])
            .tick(end, exec(1), &[no_due()]),
        Case::new(
            "hidden 151 ms before the end: due one millisecond earlier",
            true,
        )
        .fixed_clock()
        .ev(0, pick_perm(1), picked(), &[open(false)])
        .ev(end - 151, Event::Hidden, nothing(), &[due(end - 1)])
        .tick(end - 2, nothing(), &[])
        .tick(end - 1, exec(1), &[no_due()]),
        Case::new("a quit near the end saturates its deadline", true)
            .fixed_clock()
            .ev(
                end - 10,
                Event::Quit,
                vec![Effect::Send(UiCmd::Stop)],
                &[due(end)],
            )
            .tick(end - 1, nothing(), &[])
            .tick(end, nothing(), &[exit(), no_due()]),
        Case::new("an exec and a quit both near the end", true)
            .fixed_clock()
            .ev(0, pick_perm(1), picked(), &[open(false)])
            .ev(
                end - 20,
                Event::Quit,
                vec![Effect::Send(UiCmd::Stop)],
                &[due(end)],
            )
            .ev(end - 5, Event::Hidden, nothing(), &[due(end)])
            .tick(end - 1, nothing(), &[])
            .tick(end, exec(1), &[exit(), no_due()]),
        Case::new("a pick at the end of the clock", true)
            .fixed_clock()
            .ev(end, pick_perm(1), picked(), &[open(false)])
            .ev(end, Event::Hidden, nothing(), &[due(end)])
            .tick(end, exec(1), &[no_due()]),
    ]
}

// saturating arithmetic: deadlines near u64::MAX saturate and fire at u64::MAX, never wrap or panic
#[test]
fn rule_permanent_saturating_arithmetic_at_the_end_of_the_clock() {
    run(saturation_cases());
}

// ---------------------------------------------------------------------------
// Constants the rules above are written for
// ---------------------------------------------------------------------------

// the tables spell the delays as numbers: this ties them to the exported constants
#[test]
fn rule_permanent_delay_is_the_paste_delay() {
    // The tables spell the delay as 150; this ties them to the exported constant.
    assert_eq!(PASTE_DELAY_MS, 150);
    assert_eq!(QUIT_TIMEOUT_MS, 6_000);
    assert_eq!(TRAY_GUARD_MS, 300);
}
