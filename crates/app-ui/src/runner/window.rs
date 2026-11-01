//! Window creation and the popup's visibility helper.

use egui::{Context, ViewportBuilder, ViewportCommand};
use winit::event_loop::ActiveEventLoop;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Window, WindowAttributes};

/// Windows class name of the popup window (winit's default is "Window Class").
pub const POPUP_CLASS: &str = "ClipmongerPopup";
/// Windows class name of the settings window.
pub const SETTINGS_CLASS: &str = "ClipmongerSettings";

/// Viewport builder of the popup, sizes in points. Undecorated, always on top,
/// without taskbar entry, not resizable, active, created hidden, X11 window
/// type utility, app id "clipmonger". `position` is set only when given.
#[must_use]
pub fn popup_builder(title: &str, size: [f32; 2], position: Option<[f32; 2]>) -> ViewportBuilder {
    let mut builder = ViewportBuilder::default()
        .with_title(title)
        .with_app_id("clipmonger")
        .with_inner_size(size)
        .with_decorations(false)
        .with_window_level(egui::WindowLevel::AlwaysOnTop)
        .with_taskbar(false)
        .with_active(true)
        .with_resizable(false)
        .with_visible(false)
        .with_window_type(egui::X11WindowType::Utility);
    if let Some(pos) = position {
        builder = builder.with_position(pos);
    }
    builder
}

/// Viewport builder of the settings window, size in points: decorated,
/// resizable, taskbar entry, active, visible at creation, app id "clipmonger".
#[must_use]
pub fn settings_builder(title: &str, size: [f32; 2]) -> ViewportBuilder {
    ViewportBuilder::default()
        .with_title(title)
        .with_app_id("clipmonger")
        .with_inner_size(size)
        .with_decorations(true)
        .with_resizable(true)
        .with_taskbar(true)
        .with_active(true)
        .with_visible(true)
}

/// Winit window attributes for `builder`, made by
/// `egui_winit::create_winit_window_attributes(ctx, builder)`; on Windows the
/// window class is then set to `class` (`WindowAttributesExtWindows::with_class_name`).
/// `class` is unused on other platforms. Nothing else is changed.
#[must_use]
pub fn attributes(ctx: &Context, builder: ViewportBuilder, class: &str) -> WindowAttributes {
    let attrs = egui_winit::create_winit_window_attributes(ctx, builder);
    #[cfg(target_os = "windows")]
    {
        use winit::platform::windows::WindowAttributesExtWindows as _;
        attrs.with_class_name(class)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = class;
        attrs
    }
}

/// The Win32 window handle of `window` as `u64` (`hwnd as usize as u64`, never
/// negative), `None` on other platforms. Winit refuses the handle off the
/// event loop thread, so call this on that thread.
#[must_use]
pub fn hwnd(window: &Window) -> Option<u64> {
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get().cast_unsigned() as u64),
        _ => None,
    }
}

/// Whether the event loop runs on Wayland, where winit can neither hide,
/// raise nor place a window: the popup is then created per show and destroyed
/// on hide.
#[must_use]
pub fn is_wayland(event_loop: &ActiveEventLoop) -> bool {
    #[cfg(target_os = "linux")]
    {
        use winit::platform::wayland::ActiveEventLoopExtWayland as _;
        event_loop.is_wayland()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = event_loop;
        false
    }
}

/// Whether the runner executes this egui viewport command of a window. Only
/// `OuterPosition` and `InnerSize` are executed. Every other command is
/// dropped, in particular all that change winit's window flags (`Visible`,
/// `Decorations`, `Resizable`, `EnableButtons`, `Minimized`, `Maximized`,
/// `Fullscreen`, `WindowLevel`, `Transparent`, `MousePassthrough`): a flag
/// change re-applies winit's idea of the window's visibility (see
/// [`Visibility`]), and `Focus` would inject an Alt key press on Windows.
#[must_use]
pub fn allows(command: &ViewportCommand) -> bool {
    matches!(
        command,
        ViewportCommand::OuterPosition(_) | ViewportCommand::InnerSize(_)
    )
}

/// The two window calls [`Visibility`] needs, so that its logic is testable
/// without a window.
pub trait OsWindow {
    /// What the operating system says (`IsWindowVisible` on Windows); `None`
    /// when the platform cannot tell (Wayland).
    fn is_visible(&self) -> Option<bool>;
    /// Winit's `Window::set_visible`.
    fn set_visible(&self, visible: bool);
    /// Whether the window is minimized.
    fn is_minimized(&self) -> Option<bool>;
    /// Whether the window has the keyboard focus.
    fn has_focus(&self) -> bool;
    /// Bring the window to the front.
    fn focus_window(&self);
}

impl OsWindow for Window {
    fn is_visible(&self) -> Option<bool> {
        Window::is_visible(self)
    }

    fn set_visible(&self, visible: bool) {
        Window::set_visible(self, visible);
    }

    fn is_minimized(&self) -> Option<bool> {
        Window::is_minimized(self)
    }

    fn has_focus(&self) -> bool {
        Window::has_focus(self)
    }

    fn focus_window(&self) {
        #[cfg(not(target_os = "windows"))]
        Window::focus_window(self);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivationPolicy {
    Supported,
    Unsupported,
}

#[must_use]
pub fn native_policy(per_show: bool) -> ActivationPolicy {
    if per_show {
        ActivationPolicy::Unsupported
    } else {
        #[cfg(target_os = "windows")]
        {
            ActivationPolicy::Unsupported
        }
        #[cfg(not(target_os = "windows"))]
        {
            ActivationPolicy::Supported
        }
    }
}

/// Result of an activation attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivationStatus {
    /// Activation was delivered or consumed (already focused/minimized/unsupported).
    Done,
    /// Host not ready yet, keep pending for next tick.
    Pending,
}

pub fn try_activate_with_policy<W: OsWindow>(
    window: &W,
    policy: ActivationPolicy,
) -> ActivationStatus {
    if policy == ActivationPolicy::Unsupported {
        return ActivationStatus::Done;
    }
    if window.is_visible() != Some(true) {
        return ActivationStatus::Pending;
    }
    if window.is_minimized() == Some(true) {
        return ActivationStatus::Done;
    }
    if window.has_focus() {
        return ActivationStatus::Done;
    }
    window.focus_window();
    ActivationStatus::Done
}

/// Try to activate the popup window for X11/macOS.
/// Returns `Done` when pending should be cleared, `Pending` when it should be retained.
/// On Windows and Wayland (`per_show`), always returns `Done` without focusing.
pub fn try_activate<W: OsWindow>(window: &W, per_show: bool) -> ActivationStatus {
    try_activate_with_policy(window, native_policy(per_show))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Activation {
    pending: bool,
}

impl Activation {
    #[must_use]
    pub fn new() -> Self {
        Self { pending: false }
    }

    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.pending
    }

    pub fn reconcile<W: OsWindow>(
        &mut self,
        request: bool,
        previous_open: bool,
        desired_open: bool,
        host: Option<&W>,
        policy: ActivationPolicy,
    ) {
        let edge = !previous_open && desired_open;
        let mut pending = self.pending || request || edge;
        if !desired_open {
            self.pending = false;
            return;
        }
        if pending {
            if let Some(window) = host {
                match try_activate_with_policy(window, policy) {
                    ActivationStatus::Done => pending = false,
                    ActivationStatus::Pending => {}
                }
            } else if policy == ActivationPolicy::Supported {
                pending = false;
            }
        }
        self.pending = pending;
    }
}

/// What winit believes about a window's visibility, next to the truth of the
/// operating system.
///
/// Winit keeps a VISIBLE flag that only `set_visible` changes, and
/// `set_visible(v)` does nothing at all while the flag already equals `v`. On
/// Windows the daemon shows the popup with a cross-process `ShowWindow`, which
/// leaves the flag false: a plain `set_visible(false)` is then a no-op and the
/// popup stays on screen, and any flag-changing setter would hide it. The
/// helper keeps a shadow of the flag and re-syncs before it acts. It never
/// derives anything but this bookkeeping from the OS state; whether the popup
/// is logically open belongs to the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Visibility {
    shadow: bool,
}

impl Visibility {
    /// For a window created hidden: shadow false.
    #[must_use]
    pub const fn hidden() -> Self {
        Self { shadow: false }
    }

    /// For a window created visible: shadow true.
    #[must_use]
    pub const fn shown() -> Self {
        Self { shadow: true }
    }

    /// The shadow flag: what winit believes after our last `set_visible`.
    #[must_use]
    pub const fn flag(&self) -> bool {
        self.shadow
    }

    /// Align winit's flag with a show done by someone else: when the shadow is
    /// false and `window.is_visible() == Some(true)`, call `set_visible(true)`
    /// once and set the shadow. Otherwise do nothing. Idempotent.
    pub fn sync(&mut self, window: &impl OsWindow) {
        if !self.shadow && window.is_visible() == Some(true) {
            window.set_visible(true);
            self.shadow = true;
        }
    }

    /// Hide the window for real: `sync`, then `set_visible(false)`, shadow false.
    /// Always calls `set_visible(false)` last, whatever the shadow was.
    pub fn hide(&mut self, window: &impl OsWindow) {
        self.sync(window);
        window.set_visible(false);
        self.shadow = false;
    }

    /// Show the window: `sync`; then, when `window.is_visible() != Some(true)`:
    /// if the shadow is true (somebody hid the window behind winit's back)
    /// call `set_visible(false)` first to clear winit's flag, then call
    /// `set_visible(true)` and set the shadow. A window the OS already shows
    /// gets no further call after the `sync`.
    pub fn show(&mut self, window: &impl OsWindow) {
        self.sync(window);
        if window.is_visible() != Some(true) {
            if self.shadow {
                window.set_visible(false);
            }
            window.set_visible(true);
            self.shadow = true;
        }
    }
}
