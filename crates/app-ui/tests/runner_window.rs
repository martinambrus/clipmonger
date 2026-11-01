//! The popup's visibility helper against a fake window that mirrors winit
//! 0.30.13, the viewport builders of both windows and the command filter.
//!
//! Run: `cargo test -p clipmonger-app-ui --test runner_window`

use std::cell::{Cell, RefCell};

use clipmonger_app_ui::runner::window::{
    allows, attributes, popup_builder, settings_builder, OsWindow, Visibility, POPUP_CLASS,
    SETTINGS_CLASS,
};
use egui::{
    Context, CursorGrab, IMEPurpose, Pos2, Rect, ResizeDirection, ScreenshotCallback, SystemTheme,
    UserAttentionType, Vec2, ViewportBuilder, ViewportCommand, WindowLevel, X11WindowType,
};
use winit::dpi::{LogicalPosition, LogicalSize, Position, Size};

const NO_CALLS: [bool; 0] = [];

/// A window that behaves like winit 0.30.13: the VISIBLE flag only
/// `set_visible` changes, `set_visible(v)` does nothing while the flag already
/// equals `v` and otherwise sets flag and operating system state to `v`, and
/// `is_visible` reports the operating system state.
struct Fake {
    flag: Cell<bool>,
    os: Cell<bool>,
    blind: bool,
    calls: RefCell<Vec<bool>>,
    wasted: Cell<usize>,
}

impl Fake {
    fn new(visible: bool) -> Self {
        Self {
            flag: Cell::new(visible),
            os: Cell::new(visible),
            blind: false,
            calls: RefCell::new(Vec::new()),
            wasted: Cell::new(0),
        }
    }

    fn hidden() -> Self {
        Self::new(false)
    }

    fn shown() -> Self {
        Self::new(true)
    }

    /// A platform that cannot tell (`is_visible` is `None`).
    fn blind(mut self) -> Self {
        self.blind = true;
        self
    }

    /// Somebody else shows the window (the daemon's `ShowWindow`): winit's
    /// flag stays as it was.
    fn os_show(&self) {
        self.os.set(true);
    }

    /// Somebody else hides the window.
    fn os_hide(&self) {
        self.os.set(false);
    }

    fn on_screen(&self) -> bool {
        self.os.get()
    }

    fn calls(&self) -> Vec<bool> {
        self.calls.borrow().clone()
    }
}

impl OsWindow for Fake {
    fn is_visible(&self) -> Option<bool> {
        (!self.blind).then(|| self.os.get())
    }

    fn set_visible(&self, visible: bool) {
        self.calls.borrow_mut().push(visible);
        if self.flag.get() == visible {
            self.wasted.set(self.wasted.get() + 1);
        } else {
            self.flag.set(visible);
            self.os.set(visible);
        }
    }

    fn is_minimized(&self) -> Option<bool> {
        Some(false)
    }

    fn has_focus(&self) -> bool {
        false
    }

    fn focus_window(&self) {}
}

struct ActivateFake {
    visible: Cell<Option<bool>>,
    minimized: Cell<Option<bool>>,
    focused: Cell<bool>,
    focus_calls: Cell<usize>,
    visible_calls: Cell<usize>,
    per_show: bool,
}

impl ActivateFake {
    fn visible_unfocused(policy: clipmonger_app_ui::runner::window::ActivationPolicy) -> Self {
        Self {
            visible: Cell::new(Some(true)),
            minimized: Cell::new(Some(false)),
            focused: Cell::new(false),
            focus_calls: Cell::new(0),
            visible_calls: Cell::new(0),
            per_show: policy == clipmonger_app_ui::runner::window::ActivationPolicy::Unsupported,
        }
    }

    fn not_visible(policy: clipmonger_app_ui::runner::window::ActivationPolicy) -> Self {
        Self {
            visible: Cell::new(Some(false)),
            minimized: Cell::new(Some(false)),
            focused: Cell::new(false),
            focus_calls: Cell::new(0),
            visible_calls: Cell::new(0),
            per_show: policy == clipmonger_app_ui::runner::window::ActivationPolicy::Unsupported,
        }
    }

    fn minimized(_policy: clipmonger_app_ui::runner::window::ActivationPolicy) -> Self {
        Self {
            visible: Cell::new(Some(true)),
            minimized: Cell::new(Some(true)),
            focused: Cell::new(false),
            focus_calls: Cell::new(0),
            visible_calls: Cell::new(0),
            per_show: false,
        }
    }

    fn focused(_policy: clipmonger_app_ui::runner::window::ActivationPolicy) -> Self {
        Self {
            visible: Cell::new(Some(true)),
            minimized: Cell::new(Some(false)),
            focused: Cell::new(true),
            focus_calls: Cell::new(0),
            visible_calls: Cell::new(0),
            per_show: false,
        }
    }
}

impl OsWindow for ActivateFake {
    fn is_visible(&self) -> Option<bool> {
        self.visible.get()
    }

    fn set_visible(&self, _visible: bool) {
        self.visible_calls.set(self.visible_calls.get() + 1);
    }

    fn is_minimized(&self) -> Option<bool> {
        self.minimized.get()
    }

    fn has_focus(&self) -> bool {
        self.focused.get()
    }

    fn focus_window(&self) {
        self.focus_calls.set(self.focus_calls.get() + 1);
    }
}

#[test]
fn fake_follows_the_winit_rules() {
    let window = Fake::hidden();
    window.set_visible(true);
    assert!(window.flag.get() && window.on_screen());
    window.os_hide();
    window.set_visible(true);
    assert!(
        !window.on_screen(),
        "set_visible(true) is a no-op while the flag is true"
    );
    window.set_visible(false);
    window.set_visible(true);
    assert!(
        window.on_screen(),
        "clearing the flag first makes it work again"
    );
    window.os_hide();
    window.set_visible(false);
    assert!(!window.flag.get());
    assert_eq!(window.is_visible(), Some(false));
    assert_eq!(Fake::shown().blind().is_visible(), None);
}

#[test]
fn naive_set_visible_false_leaves_an_externally_shown_window_visible() {
    let window = Fake::hidden();
    window.os_show();
    assert!(window.on_screen());
    window.set_visible(false);
    assert!(window.on_screen(), "the failure the helper exists for");
    assert_eq!(window.calls(), [false]);
}

#[test]
fn constructors_set_the_shadow() {
    assert!(!Visibility::hidden().flag());
    assert!(Visibility::shown().flag());
    assert_ne!(Visibility::hidden(), Visibility::shown());
    assert_eq!(Visibility::hidden(), Visibility::hidden());
}

#[test]
fn hide_really_hides_an_externally_shown_window() {
    let window = Fake::hidden();
    let mut visibility = Visibility::hidden();
    window.os_show();
    visibility.hide(&window);
    assert!(!window.on_screen());
    assert!(!visibility.flag());
    assert!(!window.flag.get());
    assert_eq!(window.calls(), [true, false], "sync, then hide");
}

#[test]
fn hide_always_ends_with_set_visible_false() {
    let window = Fake::hidden();
    let mut visibility = Visibility::hidden();
    visibility.hide(&window);
    assert_eq!(window.calls(), [false], "also when everything is hidden");
    assert!(!window.on_screen());
    let window = Fake::shown();
    let mut visibility = Visibility::shown();
    visibility.hide(&window);
    assert_eq!(window.calls(), [false]);
    assert!(!window.on_screen());
    assert!(!visibility.flag());
}

#[test]
fn hide_after_an_external_hide_leaves_a_consistent_state() {
    let window = Fake::shown();
    let mut visibility = Visibility::shown();
    window.os_hide();
    visibility.hide(&window);
    assert_eq!(window.calls(), [false]);
    assert!(!window.on_screen());
    assert!(!visibility.flag() && !window.flag.get());
}

#[test]
fn show_shows_a_hidden_window() {
    let window = Fake::hidden();
    let mut visibility = Visibility::hidden();
    visibility.show(&window);
    assert!(window.on_screen());
    assert!(visibility.flag() && window.flag.get());
    assert_eq!(window.calls(), [true]);
}

#[test]
fn show_on_an_externally_shown_window_ends_with_flag_true_and_one_call() {
    let window = Fake::hidden();
    let mut visibility = Visibility::hidden();
    window.os_show();
    visibility.show(&window);
    assert!(window.on_screen());
    assert!(visibility.flag(), "shadow follows");
    assert!(window.flag.get(), "winit's flag follows");
    assert_eq!(
        window.calls(),
        [true],
        "the sync only, no spurious second show"
    );
}

#[test]
fn show_on_a_window_winit_already_shows_makes_no_call() {
    let window = Fake::shown();
    let mut visibility = Visibility::shown();
    visibility.show(&window);
    assert_eq!(window.calls(), NO_CALLS);
    assert!(window.on_screen() && visibility.flag());
}

#[test]
fn naive_set_visible_true_cannot_show_an_externally_hidden_window() {
    let window = Fake::shown();
    window.os_hide();
    window.set_visible(true);
    assert!(!window.on_screen(), "winit's flag is still true");
}

#[test]
fn show_after_an_external_hide_repairs_the_state() {
    let window = Fake::shown();
    let mut visibility = Visibility::shown();
    window.os_hide();
    visibility.show(&window);
    assert!(window.on_screen());
    assert!(visibility.flag() && window.flag.get());
    assert_eq!(
        window.calls(),
        [false, true],
        "clear winit's flag, then show"
    );
}

#[test]
fn sync_aligns_winit_with_an_external_show_once() {
    let window = Fake::hidden();
    let mut visibility = Visibility::hidden();
    window.os_show();
    visibility.sync(&window);
    assert_eq!(window.calls(), [true]);
    assert!(visibility.flag() && window.flag.get() && window.on_screen());
    visibility.sync(&window);
    visibility.sync(&window);
    assert_eq!(window.calls(), [true], "idempotent");
    assert_eq!(window.wasted.get(), 0);
}

#[test]
fn sync_does_nothing_otherwise() {
    for (shadow_shown, os_shown) in [(false, false), (true, true), (true, false)] {
        let window = Fake::new(shadow_shown);
        window.os.set(os_shown);
        let mut visibility = if shadow_shown {
            Visibility::shown()
        } else {
            Visibility::hidden()
        };
        visibility.sync(&window);
        assert_eq!(
            window.calls(),
            NO_CALLS,
            "shadow {shadow_shown}, os {os_shown}"
        );
        assert_eq!(visibility.flag(), shadow_shown);
        assert_eq!(window.on_screen(), os_shown, "sync never shows or hides");
    }
}

#[test]
fn without_os_knowledge_sync_does_nothing_and_show_and_hide_still_work() {
    let window = Fake::hidden().blind();
    let mut visibility = Visibility::hidden();
    visibility.sync(&window);
    assert_eq!(window.calls(), NO_CALLS);
    visibility.show(&window);
    assert_eq!(window.calls(), [true]);
    assert!(visibility.flag() && window.on_screen());
    visibility.show(&window);
    assert_eq!(
        window.calls(),
        [true, false, true],
        "cannot tell: clear and show"
    );
    assert!(visibility.flag() && window.on_screen());
    visibility.hide(&window);
    assert_eq!(window.calls().last(), Some(&false));
    assert!(!visibility.flag() && !window.on_screen());
}

#[derive(Clone, Copy, Debug)]
enum Op {
    Show,
    Hide,
    Sync,
    OsShow,
    OsHide,
}

const OPS: [Op; 5] = [Op::Show, Op::Hide, Op::Sync, Op::OsShow, Op::OsHide];
/// Where the platform cannot tell (Wayland) nobody else shows the window.
const OWN_OPS: [Op; 3] = [Op::Show, Op::Hide, Op::Sync];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sight {
    Clear,
    Blind,
}

/// The contract of `Visibility` written out on plain state.
struct Spec {
    shadow: bool,
    flag: bool,
    os: bool,
    sight: Sight,
    calls: Vec<bool>,
}

impl Spec {
    fn set_visible(&mut self, visible: bool) {
        self.calls.push(visible);
        if self.flag != visible {
            self.flag = visible;
            self.os = visible;
        }
    }

    fn known(&self) -> Option<bool> {
        (self.sight == Sight::Clear).then_some(self.os)
    }

    fn sync(&mut self) {
        if !self.shadow && self.known() == Some(true) {
            self.set_visible(true);
            self.shadow = true;
        }
    }

    fn apply(&mut self, op: Op) {
        match op {
            Op::Show => {
                self.sync();
                if self.known() != Some(true) {
                    if self.shadow {
                        self.set_visible(false);
                    }
                    self.set_visible(true);
                    self.shadow = true;
                }
            }
            Op::Hide => {
                self.sync();
                self.set_visible(false);
                self.shadow = false;
            }
            Op::Sync => self.sync(),
            Op::OsShow => self.os = true,
            Op::OsHide => self.os = false,
        }
    }
}

fn apply(visibility: &mut Visibility, window: &Fake, op: Op) {
    match op {
        Op::Show => visibility.show(window),
        Op::Hide => visibility.hide(window),
        Op::Sync => visibility.sync(window),
        Op::OsShow => window.os_show(),
        Op::OsHide => window.os_hide(),
    }
}

/// Every sequence of up to `depth` operations from one start state.
fn explore(start_shown: bool, blind: bool, depth: usize, prefix: &mut Vec<Op>) {
    let window = if blind {
        Fake::new(start_shown).blind()
    } else {
        Fake::new(start_shown)
    };
    let mut visibility = if start_shown {
        Visibility::shown()
    } else {
        Visibility::hidden()
    };
    let mut spec = Spec {
        shadow: start_shown,
        flag: start_shown,
        os: start_shown,
        sight: if blind { Sight::Blind } else { Sight::Clear },
        calls: Vec::new(),
    };
    for (step, &op) in prefix.iter().enumerate() {
        let wasted_before = window.wasted.get();
        let calls_before = window.calls().len();
        apply(&mut visibility, &window, op);
        spec.apply(op);
        let context =
            format!("start shown {start_shown}, blind {blind}, ops {prefix:?}, step {step}");
        assert_eq!(window.calls(), spec.calls, "calls: {context}");
        assert_eq!(window.flag.get(), spec.flag, "winit flag: {context}");
        assert_eq!(window.on_screen(), spec.os, "os state: {context}");
        assert_eq!(visibility.flag(), spec.shadow, "shadow: {context}");
        assert_eq!(
            visibility.flag(),
            window.flag.get(),
            "shadow mirrors winit: {context}"
        );
        match op {
            Op::Show => {
                assert!(window.on_screen() && visibility.flag(), "show: {context}");
                assert_eq!(
                    window.wasted.get(),
                    wasted_before,
                    "show wasted a call: {context}"
                );
            }
            Op::Hide => {
                assert!(!window.on_screen() && !visibility.flag(), "hide: {context}");
                assert_eq!(
                    window.calls().last(),
                    Some(&false),
                    "hide ends false: {context}"
                );
                assert!(window.wasted.get() <= wasted_before + 1, "hide: {context}");
            }
            Op::Sync => {
                assert_eq!(
                    window.wasted.get(),
                    wasted_before,
                    "sync wasted a call: {context}"
                );
                let calls_after = window.calls().len();
                visibility.sync(&window);
                assert_eq!(
                    window.calls().len(),
                    calls_after,
                    "sync idempotent: {context}"
                );
                assert!(
                    calls_after - calls_before <= 1,
                    "sync calls once at most: {context}"
                );
            }
            Op::OsShow | Op::OsHide => {
                assert_eq!(window.calls().len(), calls_before, "external op: {context}");
            }
        }
    }
    if depth > 0 {
        for &op in if blind { &OWN_OPS[..] } else { &OPS[..] } {
            prefix.push(op);
            explore(start_shown, blind, depth - 1, prefix);
            prefix.pop();
        }
    }
}

#[test]
fn every_short_sequence_of_show_hide_sync_and_external_changes_stays_consistent() {
    for start_shown in [false, true] {
        for blind in [false, true] {
            explore(start_shown, blind, 5, &mut Vec::new());
        }
    }
}

fn is_unset_or<T: PartialEq + Copy>(actual: Option<T>, wanted: T) -> bool {
    actual.is_none() || actual == Some(wanted)
}

#[test]
fn popup_builder_describes_a_hidden_undecorated_utility_window() {
    let b = popup_builder("Clipmonger popup", [480.0, 360.0], None);
    assert_eq!(b.title.as_deref(), Some("Clipmonger popup"));
    assert_eq!(b.inner_size, Some(Vec2::new(480.0, 360.0)));
    assert_eq!(b.position, None, "position is set only when given");
    assert_eq!(b.decorations, Some(false));
    assert_eq!(b.window_level, Some(WindowLevel::AlwaysOnTop));
    assert_eq!(b.taskbar, Some(false));
    assert_eq!(b.visible, Some(false));
    assert_eq!(b.resizable, Some(false));
    assert!(is_unset_or(b.active, true), "active: {:?}", b.active);
    assert_eq!(b.window_type, Some(X11WindowType::Utility));
    assert_eq!(b.app_id.as_deref(), Some("clipmonger"));
}

#[test]
fn popup_builder_takes_size_title_and_position_from_its_arguments() {
    let b = popup_builder("Other", [123.5, 77.25], Some([10.0, 20.5]));
    assert_eq!(b.title.as_deref(), Some("Other"));
    assert_eq!(b.inner_size, Some(Vec2::new(123.5, 77.25)));
    assert_eq!(b.position, Some(Pos2::new(10.0, 20.5)));
    assert_eq!(b.visible, Some(false), "still created hidden");
    let b = popup_builder("Origin", [10.0, 10.0], Some([0.0, 0.0]));
    assert_eq!(
        b.position,
        Some(Pos2::ZERO),
        "a given position of zero is kept"
    );
}

#[test]
fn settings_builder_describes_a_normal_visible_window() {
    let b: ViewportBuilder = settings_builder("Clipmonger settings", [640.0, 520.0]);
    assert_eq!(b.title.as_deref(), Some("Clipmonger settings"));
    assert_eq!(b.inner_size, Some(Vec2::new(640.0, 520.0)));
    assert_eq!(b.position, None);
    assert!(
        is_unset_or(b.decorations, true),
        "decorations: {:?}",
        b.decorations
    );
    assert!(
        is_unset_or(b.resizable, true),
        "resizable: {:?}",
        b.resizable
    );
    assert!(is_unset_or(b.taskbar, true), "taskbar: {:?}", b.taskbar);
    assert!(is_unset_or(b.active, true), "active: {:?}", b.active);
    assert!(is_unset_or(b.visible, true), "visible: {:?}", b.visible);
    assert!(is_unset_or(b.window_level, WindowLevel::Normal));
    assert!(is_unset_or(b.window_type, X11WindowType::Normal));
    assert_eq!(b.app_id.as_deref(), Some("clipmonger"));
}

#[test]
fn class_names_are_the_ones_the_daemon_looks_for() {
    assert_eq!(POPUP_CLASS, "ClipmongerPopup");
    assert_eq!(SETTINGS_CLASS, "ClipmongerSettings");
}

#[test]
fn attributes_follow_the_builder() {
    let ctx = Context::default();
    let popup = attributes(
        &ctx,
        popup_builder("Popup", [480.0, 360.0], Some([10.0, 20.0])),
        POPUP_CLASS,
    );
    assert_eq!(popup.title, "Popup");
    assert!(!popup.visible && !popup.decorations && !popup.resizable);
    assert!(popup.active);
    assert_eq!(popup.window_level, winit::window::WindowLevel::AlwaysOnTop);
    assert_eq!(
        popup.inner_size,
        Some(Size::Logical(LogicalSize::new(480.0, 360.0)))
    );
    assert_eq!(
        popup.position,
        Some(Position::Logical(LogicalPosition::new(10.0, 20.0)))
    );
    let settings = attributes(
        &ctx,
        settings_builder("Settings", [600.0, 500.0]),
        SETTINGS_CLASS,
    );
    assert_eq!(settings.title, "Settings");
    assert!(settings.visible && settings.decorations && settings.resizable && settings.active);
    assert_eq!(settings.window_level, winit::window::WindowLevel::Normal);
    assert_eq!(settings.position, None);
    assert_eq!(
        settings.inner_size,
        Some(Size::Logical(LogicalSize::new(600.0, 500.0)))
    );
}

/// One entry per variant of `ViewportCommand`: its number and whether the
/// runner executes it. No wildcard: a new variant stops this from compiling.
fn variant(command: &ViewportCommand) -> (usize, bool) {
    match command {
        ViewportCommand::Close => (0, false),
        ViewportCommand::CancelClose => (1, false),
        ViewportCommand::Title(_) => (2, false),
        ViewportCommand::Transparent(_) => (3, false),
        ViewportCommand::Visible(_) => (4, false),
        ViewportCommand::StartDrag => (5, false),
        ViewportCommand::OuterPosition(_) => (6, true),
        ViewportCommand::InnerSize(_) => (7, true),
        ViewportCommand::MinInnerSize(_) => (8, false),
        ViewportCommand::MaxInnerSize(_) => (9, false),
        ViewportCommand::ResizeIncrements(_) => (10, false),
        ViewportCommand::BeginResize(_) => (11, false),
        ViewportCommand::Resizable(_) => (12, false),
        ViewportCommand::EnableButtons { .. } => (13, false),
        ViewportCommand::Minimized(_) => (14, false),
        ViewportCommand::Maximized(_) => (15, false),
        ViewportCommand::Fullscreen(_) => (16, false),
        ViewportCommand::SetMonitor(_) => (17, false),
        ViewportCommand::Decorations(_) => (18, false),
        ViewportCommand::WindowLevel(_) => (19, false),
        ViewportCommand::Icon(_) => (20, false),
        ViewportCommand::IMERect(_) => (21, false),
        ViewportCommand::IMEAllowed(_) => (22, false),
        ViewportCommand::IMEPurpose(_) => (23, false),
        ViewportCommand::Focus => (24, false),
        ViewportCommand::RequestUserAttention(_) => (25, false),
        ViewportCommand::SetTheme(_) => (26, false),
        ViewportCommand::ContentProtected(_) => (27, false),
        ViewportCommand::CursorPosition(_) => (28, false),
        ViewportCommand::CursorGrab(_) => (29, false),
        ViewportCommand::CursorVisible(_) => (30, false),
        ViewportCommand::MousePassthrough(_) => (31, false),
        ViewportCommand::Screenshot(_) => (32, false),
        ViewportCommand::RequestCut => (33, false),
        ViewportCommand::RequestCopy => (34, false),
        ViewportCommand::RequestPaste => (35, false),
        ViewportCommand::RequestPaintWhileHidden => (36, false),
    }
}

const VARIANTS: usize = 37;

/// At least one value of every variant, both values of every flag.
fn every_command() -> Vec<ViewportCommand> {
    use ViewportCommand as C;
    let mut all = vec![
        C::Close,
        C::CancelClose,
        C::Title("title".to_owned()),
        C::StartDrag,
        C::OuterPosition(Pos2::ZERO),
        C::OuterPosition(Pos2::new(-50.0, 300.0)),
        C::OuterPosition(Pos2::new(1.0e9, -1.0e9)),
        C::InnerSize(Vec2::new(480.0, 360.0)),
        C::InnerSize(Vec2::ZERO),
        C::InnerSize(Vec2::new(1.0e6, 3.5)),
        C::MinInnerSize(Vec2::new(100.0, 100.0)),
        C::MaxInnerSize(Vec2::new(900.0, 900.0)),
        C::ResizeIncrements(Some(Vec2::new(8.0, 8.0))),
        C::ResizeIncrements(None),
        C::BeginResize(ResizeDirection::North),
        C::BeginResize(ResizeDirection::SouthEast),
        C::SetMonitor(1),
        C::WindowLevel(WindowLevel::AlwaysOnTop),
        C::WindowLevel(WindowLevel::Normal),
        C::WindowLevel(WindowLevel::AlwaysOnBottom),
        C::Icon(None),
        C::IMERect(Rect::from_min_size(Pos2::ZERO, Vec2::new(10.0, 10.0))),
        C::IMEPurpose(IMEPurpose::Normal),
        C::IMEPurpose(IMEPurpose::Password),
        C::Focus,
        C::RequestUserAttention(UserAttentionType::Critical),
        C::RequestUserAttention(UserAttentionType::Reset),
        C::SetTheme(SystemTheme::Dark),
        C::CursorPosition(Pos2::new(5.0, 5.0)),
        C::CursorGrab(CursorGrab::Locked),
        C::CursorGrab(CursorGrab::None),
        C::Screenshot(ScreenshotCallback::new(|_| {})),
        C::RequestCut,
        C::RequestCopy,
        C::RequestPaste,
        C::RequestPaintWhileHidden,
    ];
    for flag in [true, false] {
        all.extend([
            C::Transparent(flag),
            C::Visible(flag),
            C::Resizable(flag),
            C::Minimized(flag),
            C::Maximized(flag),
            C::Fullscreen(flag),
            C::Decorations(flag),
            C::IMEAllowed(flag),
            C::ContentProtected(flag),
            C::CursorVisible(flag),
            C::MousePassthrough(flag),
        ]);
        for minimized in [true, false] {
            all.push(C::EnableButtons {
                close: flag,
                minimized,
                maximize: !flag,
            });
        }
    }
    all
}

#[test]
fn the_command_list_covers_every_variant() {
    let mut seen = [false; VARIANTS];
    for command in every_command() {
        seen[variant(&command).0] = true;
    }
    assert!(seen.iter().all(|&s| s), "missing variants: {seen:?}");
}

#[test]
fn allows_executes_only_position_and_size() {
    for command in every_command() {
        assert_eq!(allows(&command), variant(&command).1, "{command:?}");
    }
}

#[test]
fn allows_refuses_what_changes_window_flags_or_focus() {
    use ViewportCommand as C;
    for flag in [true, false] {
        for command in [
            C::Visible(flag),
            C::Decorations(flag),
            C::Resizable(flag),
            C::Minimized(flag),
            C::Maximized(flag),
            C::Fullscreen(flag),
            C::Transparent(flag),
            C::MousePassthrough(flag),
        ] {
            assert!(!allows(&command), "{command:?}");
        }
    }
    for command in [
        C::Focus,
        C::Close,
        C::Title("x".to_owned()),
        C::WindowLevel(WindowLevel::AlwaysOnTop),
        C::WindowLevel(WindowLevel::Normal),
        C::EnableButtons {
            close: true,
            minimized: true,
            maximize: true,
        },
    ] {
        assert!(!allows(&command), "{command:?}");
    }
    assert!(allows(&C::OuterPosition(Pos2::new(3.0, 4.0))));
    assert!(allows(&C::InnerSize(Vec2::new(3.0, 4.0))));
}

// D1: shared explicit production policy exercised via fake, native policy selected via cfg
#[test]
fn activation_policy_native_selection() {
    use clipmonger_app_ui::runner::window::{native_policy, ActivationPolicy};
    assert_eq!(
        native_policy(true),
        ActivationPolicy::Unsupported,
        "per_show always unsupported"
    );
    #[cfg(target_os = "windows")]
    {
        assert_eq!(native_policy(false), ActivationPolicy::Unsupported);
    }
    #[cfg(not(target_os = "windows"))]
    {
        assert_eq!(native_policy(false), ActivationPolicy::Supported);
    }
}

#[test]
fn activation_try_activate_with_policy_supported_focuses() {
    use clipmonger_app_ui::runner::window::{
        try_activate_with_policy, ActivationPolicy, ActivationStatus,
    };
    let fake = ActivateFake::visible_unfocused(ActivationPolicy::Supported);
    let status = try_activate_with_policy(&fake, ActivationPolicy::Supported);
    assert_eq!(status, ActivationStatus::Done);
    assert_eq!(fake.focus_calls.get(), 1);
    assert_eq!(fake.visible_calls.get(), 0, "no visibility manipulation");
}

#[test]
fn activation_try_activate_with_policy_unsupported_discards() {
    use clipmonger_app_ui::runner::window::{
        try_activate_with_policy, ActivationPolicy, ActivationStatus,
    };
    let fake = ActivateFake::visible_unfocused(ActivationPolicy::Unsupported);
    let status = try_activate_with_policy(&fake, ActivationPolicy::Unsupported);
    assert_eq!(status, ActivationStatus::Done);
    assert_eq!(fake.focus_calls.get(), 0);
    assert_eq!(fake.visible_calls.get(), 0);
}

#[test]
fn activation_try_activate_with_policy_retains_not_visible() {
    use clipmonger_app_ui::runner::window::{
        try_activate_with_policy, ActivationPolicy, ActivationStatus,
    };
    let fake = ActivateFake::not_visible(ActivationPolicy::Supported);
    let status = try_activate_with_policy(&fake, ActivationPolicy::Supported);
    assert_eq!(status, ActivationStatus::Pending);
    assert_eq!(fake.focus_calls.get(), 0);
    assert_eq!(fake.visible_calls.get(), 0);
    fake.visible.set(Some(true));
    let status = try_activate_with_policy(&fake, ActivationPolicy::Supported);
    assert_eq!(status, ActivationStatus::Done);
    assert_eq!(fake.focus_calls.get(), 1);
    assert_eq!(fake.visible_calls.get(), 0);
}

#[test]
fn activation_try_activate_focused_consumes_without_call() {
    use clipmonger_app_ui::runner::window::{
        try_activate_with_policy, ActivationPolicy, ActivationStatus,
    };
    let fake = ActivateFake::focused(ActivationPolicy::Supported);
    let status = try_activate_with_policy(&fake, ActivationPolicy::Supported);
    assert_eq!(status, ActivationStatus::Done);
    assert_eq!(fake.focus_calls.get(), 0);
    assert_eq!(fake.visible_calls.get(), 0);
}

#[test]
fn activation_try_activate_minimized_discards() {
    use clipmonger_app_ui::runner::window::{
        try_activate_with_policy, ActivationPolicy, ActivationStatus,
    };
    let fake = ActivateFake::minimized(ActivationPolicy::Supported);
    let status = try_activate_with_policy(&fake, ActivationPolicy::Supported);
    assert_eq!(status, ActivationStatus::Done);
    assert_eq!(fake.focus_calls.get(), 0);
    assert_eq!(fake.visible_calls.get(), 0);
}

// D2: stateful helper actually called by Runner, with fake recording show/hide/flag/focus
#[test]
fn activation_helper_notready_then_ready_no_app_pass() {
    use clipmonger_app_ui::runner::window::{Activation, ActivationPolicy};
    let fake = ActivateFake::not_visible(ActivationPolicy::Supported);
    let mut helper = Activation::new();
    // first-show: closed->open edge, host not ready (is_visible false)
    helper.reconcile(false, false, true, Some(&fake), ActivationPolicy::Supported);
    assert!(helper.is_pending(), "pending retained while not visible");
    assert_eq!(fake.focus_calls.get(), 0);
    assert_eq!(fake.visible_calls.get(), 0);
    // visibility notification makes ready, next reconcile with no app pass
    fake.visible.set(Some(true));
    helper.reconcile(false, true, true, Some(&fake), ActivationPolicy::Supported);
    assert!(!helper.is_pending());
    assert_eq!(fake.focus_calls.get(), 1);
    assert_eq!(fake.visible_calls.get(), 0);
}

#[test]
fn activation_helper_startopen() {
    use clipmonger_app_ui::runner::window::{Activation, ActivationPolicy};
    let fake = ActivateFake::visible_unfocused(ActivationPolicy::Supported);
    let mut helper = Activation::new();
    // startopen: previous false, desired true, edge triggers even without request
    helper.reconcile(false, false, true, Some(&fake), ActivationPolicy::Supported);
    assert!(!helper.is_pending());
    assert_eq!(fake.focus_calls.get(), 1);
    assert_eq!(fake.visible_calls.get(), 0);
}

#[test]
fn activation_helper_alreadyopen_raise_no_visibility() {
    use clipmonger_app_ui::runner::window::{Activation, ActivationPolicy};
    let fake = ActivateFake::visible_unfocused(ActivationPolicy::Supported);
    let mut helper = Activation::new();
    helper.reconcile(true, true, true, Some(&fake), ActivationPolicy::Supported);
    assert!(!helper.is_pending());
    assert_eq!(fake.focus_calls.get(), 1);
    assert_eq!(
        fake.visible_calls.get(),
        0,
        "raise on already-open must not touch visibility"
    );
}

#[test]
fn activation_helper_idle_after_delivery_no_repeat() {
    use clipmonger_app_ui::runner::window::{Activation, ActivationPolicy};
    let fake = ActivateFake::visible_unfocused(ActivationPolicy::Supported);
    let mut helper = Activation::new();
    helper.reconcile(true, true, true, Some(&fake), ActivationPolicy::Supported);
    assert_eq!(fake.focus_calls.get(), 1);
    // focus lost again but idle should not refocus
    fake.focused.set(false);
    fake.visible.set(Some(true));
    helper.reconcile(false, true, true, Some(&fake), ActivationPolicy::Supported);
    assert_eq!(
        fake.focus_calls.get(),
        1,
        "idle after delivery must not refocus even if focus lost again"
    );
    assert!(!helper.is_pending());
    assert_eq!(fake.visible_calls.get(), 0);
}

#[test]
fn activation_helper_pending_then_hide_cancels() {
    use clipmonger_app_ui::runner::window::{Activation, ActivationPolicy};
    let fake = ActivateFake::visible_unfocused(ActivationPolicy::Supported);
    let mut helper = Activation::new();
    // pending while not ready
    let not_ready = ActivateFake::not_visible(ActivationPolicy::Supported);
    helper.reconcile(
        true,
        false,
        true,
        Some(&not_ready),
        ActivationPolicy::Supported,
    );
    assert!(helper.is_pending());
    assert_eq!(not_ready.focus_calls.get(), 0);
    // hide cancels without focus
    helper.reconcile(false, true, false, Some(&fake), ActivationPolicy::Supported);
    assert!(!helper.is_pending());
    assert_eq!(fake.focus_calls.get(), 0);
    assert_eq!(fake.visible_calls.get(), 0);
}

#[test]
fn activation_helper_request_hide_batch_cancels() {
    use clipmonger_app_ui::runner::window::{Activation, ActivationPolicy};
    let fake = ActivateFake::visible_unfocused(ActivationPolicy::Supported);
    let mut helper = Activation::new();
    // request and hide in same reconcile (OR then cancel)
    helper.reconcile(true, true, false, Some(&fake), ActivationPolicy::Supported);
    assert!(!helper.is_pending());
    assert_eq!(fake.focus_calls.get(), 0);
    assert_eq!(fake.visible_calls.get(), 0);
}

#[test]
fn activation_helper_coalescing() {
    use clipmonger_app_ui::runner::window::{Activation, ActivationPolicy};
    let fake_not_ready = ActivateFake::not_visible(ActivationPolicy::Supported);
    let mut helper = Activation::new();
    helper.reconcile(
        true,
        false,
        true,
        Some(&fake_not_ready),
        ActivationPolicy::Supported,
    );
    assert!(helper.is_pending());
    // second request before delivery coalesces
    helper.reconcile(
        true,
        true,
        true,
        Some(&fake_not_ready),
        ActivationPolicy::Supported,
    );
    assert!(helper.is_pending());
    assert_eq!(fake_not_ready.focus_calls.get(), 0);
    // now ready, only one focus
    let fake_ready = ActivateFake::visible_unfocused(ActivationPolicy::Supported);
    // transfer pending to new fake by reusing helper pending true
    helper.reconcile(
        false,
        true,
        true,
        Some(&fake_ready),
        ActivationPolicy::Supported,
    );
    assert!(!helper.is_pending());
    assert_eq!(fake_ready.focus_calls.get(), 1);
    assert_eq!(fake_ready.visible_calls.get(), 0);
}

#[test]
fn activation_helper_focused_host_consumes() {
    use clipmonger_app_ui::runner::window::{Activation, ActivationPolicy};
    let fake = ActivateFake::focused(ActivationPolicy::Supported);
    let mut helper = Activation::new();
    helper.reconcile(true, true, true, Some(&fake), ActivationPolicy::Supported);
    assert!(!helper.is_pending());
    assert_eq!(fake.focus_calls.get(), 0);
    assert_eq!(fake.visible_calls.get(), 0);
}

#[test]
fn activation_helper_unsupported_windows_no_focus_no_visibility() {
    use clipmonger_app_ui::runner::window::{Activation, ActivationPolicy};
    let fake = ActivateFake::visible_unfocused(ActivationPolicy::Unsupported);
    let mut helper = Activation::new();
    helper.reconcile(true, true, true, Some(&fake), ActivationPolicy::Unsupported);
    assert!(!helper.is_pending());
    assert_eq!(fake.focus_calls.get(), 0, "Windows policy must not focus");
    assert_eq!(fake.visible_calls.get(), 0);
    // also startopen under Windows policy yields no focus
    let fake2 = ActivateFake::visible_unfocused(ActivationPolicy::Unsupported);
    let mut helper2 = Activation::new();
    helper2.reconcile(
        false,
        false,
        true,
        Some(&fake2),
        ActivationPolicy::Unsupported,
    );
    assert!(!helper2.is_pending());
    assert_eq!(fake2.focus_calls.get(), 0);
    assert_eq!(fake2.visible_calls.get(), 0);
}

#[test]
fn activation_helper_unsupported_wayland_no_focus_preserves_host() {
    use clipmonger_app_ui::runner::window::{Activation, ActivationPolicy};
    let fake = ActivateFake::visible_unfocused(ActivationPolicy::Unsupported);
    let mut helper = Activation::new();
    // already-open request on Wayland must not destroy/recreate host and must not focus
    helper.reconcile(true, true, true, Some(&fake), ActivationPolicy::Unsupported);
    assert!(!helper.is_pending());
    assert_eq!(
        fake.focus_calls.get(),
        0,
        "Wayland tokenless must not focus"
    );
    assert_eq!(fake.visible_calls.get(), 0);
    assert!(
        fake.per_show,
        "Wayland per_show host preserved, not recreated"
    );
}

#[test]
fn activation_helper_no_visibility_calls_on_supported() {
    use clipmonger_app_ui::runner::window::{Activation, ActivationPolicy};
    let fake = ActivateFake::visible_unfocused(ActivationPolicy::Supported);
    let mut helper = Activation::new();
    helper.reconcile(true, true, true, Some(&fake), ActivationPolicy::Supported);
    assert_eq!(
        fake.visible_calls.get(),
        0,
        "activation must not call set_visible"
    );
    assert_eq!(fake.focus_calls.get(), 1);
}
