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
/// type normal (Mutter refuses focus to a parentless utility window), app id
/// [`clipmonger_core::DESKTOP_APP_ID`]. `position` is set only when given.
#[must_use]
pub fn popup_builder(title: &str, size: [f32; 2], position: Option<[f32; 2]>) -> ViewportBuilder {
    let mut builder = ViewportBuilder::default()
        .with_title(title)
        .with_app_id(clipmonger_core::DESKTOP_APP_ID)
        .with_inner_size(size)
        .with_decorations(false)
        .with_window_level(egui::WindowLevel::AlwaysOnTop)
        .with_taskbar(false)
        .with_active(true)
        .with_resizable(false)
        .with_visible(false)
        .with_window_type(egui::X11WindowType::Normal);
    if let Some(pos) = position {
        builder = builder.with_position(pos);
    }
    builder
}

/// Viewport builder of the settings window, size in points: decorated,
/// resizable, taskbar entry, active, visible at creation, app id [`clipmonger_core::DESKTOP_APP_ID`].
#[must_use]
pub fn settings_builder(title: &str, size: [f32; 2]) -> ViewportBuilder {
    ViewportBuilder::default()
        .with_title(title)
        .with_app_id(clipmonger_core::DESKTOP_APP_ID)
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

/// Build the attributes used by Host and retain only the fields that egui
/// should apply after creation. Native Settings geometry bypasses font zoom.
pub struct PreparedHost {
    pub attributes: WindowAttributes,
    pub after_creation: ViewportBuilder,
    show_after_setup: bool,
}

impl PreparedHost {
    /// Complete the actual host setup before explicitly showing a restored window.
    ///
    /// # Errors
    /// Returns the setup error without showing the window.
    pub fn finish_setup<W: OsWindow, T, E>(
        &self,
        window: &W,
        configure: impl FnOnce(&ViewportBuilder) -> Result<T, E>,
    ) -> Result<(T, Visibility), E> {
        let configured = configure(&self.after_creation)?;
        let mut visibility = if self.attributes.visible {
            Visibility::shown()
        } else {
            Visibility::hidden()
        };
        if self.show_after_setup {
            visibility.show(window);
        }
        Ok((configured, visibility))
    }
}

#[must_use]
pub fn prepare_host(
    ctx: &Context,
    mut builder: ViewportBuilder,
    class: &str,
    restore: Option<&super::geometry::NormalizedRestore>,
) -> PreparedHost {
    let mut attrs = attributes(ctx, builder.clone(), class);
    let show_after_setup = restore.is_some() && attrs.visible;
    if let Some(restore) = restore {
        attrs.inner_size = Some(
            winit::dpi::LogicalSize::new(restore.logical_size[0], restore.logical_size[1]).into(),
        );
        attrs.position = restore
            .physical_position
            .map(|pos| winit::dpi::PhysicalPosition::new(pos[0], pos[1]).into());
        // apply_viewport_builder_to_window scales these fields by OS DPI * zoom.
        // Geometry has already been expressed in native units above.
        builder.inner_size = None;
        builder.position = None;
        // Pinned winit on Windows applies visibility before size and position.
        // Keep construction and post-creation setup hidden, then show explicitly.
        attrs.visible = false;
        builder.visible = Some(false);
    }
    PreparedHost {
        attributes: attrs,
        after_creation: builder,
        show_after_setup,
    }
}

/// Apply platform activation metadata to each newly created popup.
#[must_use]
pub fn show_attributes(
    attributes: WindowAttributes,
    per_show_popup: bool,
    token: Option<&str>,
) -> WindowAttributes {
    #[cfg(target_os = "linux")]
    {
        use winit::platform::startup_notify::WindowAttributesExtStartupNotify as _;
        if let (true, Some(token)) = (per_show_popup, token) {
            return attributes
                .with_activation_token(winit::window::ActivationToken::from_raw(token.to_owned()));
        }
    }
    let _ = (per_show_popup, token);
    attributes
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

/// X11 window ID, when running on X11.
#[must_use]
pub fn x11_window(window: &Window) -> Option<u64> {
    match window.window_handle().ok()?.as_raw() {
        #[cfg(target_os = "linux")]
        RawWindowHandle::Xlib(handle) => Some(handle.window),
        RawWindowHandle::Xcb(handle) => Some(u64::from(handle.window.get())),
        _ => None,
    }
}

/// Whether the owned window is native-maximized via Win32 `IsZoomed`.
/// Returns `None` if a Windows handle cannot be obtained; other platforms report `Some(false)`.
/// The caller must be on the owning event-loop thread and keep `window` alive for the call.
#[must_use]
pub fn is_window_zoomed(window: &Window) -> Option<bool> {
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::IsZoomed;
        let raw = hwnd(window)? as *mut std::ffi::c_void;
        let handle = HWND(raw);
        // SAFETY: `handle` is a live borrowed HWND from `window` on the owning event-loop thread.
        // `IsZoomed` only inspects window state and does not retain the handle.
        #[allow(unsafe_code)]
        let zoomed = unsafe { IsZoomed(handle).as_bool() };
        Some(zoomed)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = window;
        Some(false)
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
    /// Focus using the originating X11 event time when available.
    fn focus_with_time(&self, _time: Option<u32>) {
        self.focus_window();
    }
}

impl OsWindow for Window {
    fn is_visible(&self) -> Option<bool> {
        // On X11 ask the server: winit marks a window visible only on a
        // VisibilityNotify, which a compositing window manager (Cinnamon)
        // does not send when the popup is mapped again.
        #[cfg(target_os = "linux")]
        if let Some(viewable) = x11_window(self).and_then(x11_is_viewable) {
            return Some(viewable);
        }
        Window::is_visible(self)
    }

    fn set_visible(&self, visible: bool) {
        Window::set_visible(self, visible);
    }

    fn is_minimized(&self) -> Option<bool> {
        Window::is_minimized(self)
    }

    fn has_focus(&self) -> bool {
        // On X11 ask the server: when the focus falls back to PointerRoot
        // with the pointer over our window (after an unmap), winit reports
        // focus the window does not hold.
        #[cfg(target_os = "linux")]
        if let Some(window) = x11_window(self) {
            if let Some(focused) = x11_has_input_focus(window) {
                return focused;
            }
        }
        Window::has_focus(self)
    }

    fn focus_window(&self) {
        #[cfg(target_os = "windows")]
        force_foreground(self);
        #[cfg(not(target_os = "windows"))]
        Window::focus_window(self);
    }

    fn focus_with_time(&self, time: Option<u32>) {
        #[cfg(target_os = "linux")]
        if let (Some(time), Some(window)) = (time, x11_window(self)) {
            if send_x11_activation(window, time) {
                return;
            }
        }
        let _ = time;
        OsWindow::focus_window(self);
    }
}

/// Whether the X server has `window` mapped and viewable; `None` when it
/// cannot be asked.
#[cfg(target_os = "linux")]
fn x11_is_viewable(window: u64) -> Option<bool> {
    use x11rb::protocol::xproto::{ConnectionExt as _, MapState};
    let window = u32::try_from(window).ok()?;
    let (connection, _) = x11rb::connect(None).ok()?;
    let attributes = connection
        .get_window_attributes(window)
        .ok()?
        .reply()
        .ok()?;
    Some(attributes.map_state == MapState::VIEWABLE)
}

/// Whether `window` holds the X input focus; `None` when the server cannot
/// be asked.
#[cfg(target_os = "linux")]
fn x11_has_input_focus(window: u64) -> Option<bool> {
    use x11rb::protocol::xproto::ConnectionExt as _;
    let window = u32::try_from(window).ok()?;
    let (connection, _) = x11rb::connect(None).ok()?;
    let focus = connection.get_input_focus().ok()?.reply().ok()?.focus;
    Some(focus == window)
}

/// Set `_NET_WM_USER_TIME` on `window` (EWMH): the time of the user input
/// that asked for it, which the window manager compares when the window
/// maps. Best effort.
#[cfg(target_os = "linux")]
pub fn set_x11_user_time(window: u64, time: u32) {
    use x11rb::{
        connection::Connection as _,
        protocol::xproto::{AtomEnum, ConnectionExt as _, PropMode},
        wrapper::ConnectionExt as _,
    };
    let set = || -> Option<()> {
        let window = u32::try_from(window).ok()?;
        let (connection, _) = x11rb::connect(None).ok()?;
        let atom = connection
            .intern_atom(false, b"_NET_WM_USER_TIME")
            .ok()?
            .reply()
            .ok()?
            .atom;
        connection
            .change_property32(PropMode::REPLACE, window, atom, AtomEnum::CARDINAL, &[time])
            .ok()?
            .check()
            .ok()?;
        connection.flush().ok()?;
        Some(())
    };
    let _ = set();
}

#[cfg(target_os = "linux")]
fn send_x11_activation(window: u64, time: u32) -> bool {
    use x11rb::{
        connection::Connection as _,
        protocol::xproto::{ConnectionExt as _, EventMask},
    };
    let send = || -> Option<()> {
        let window = u32::try_from(window).ok()?;
        let (connection, _) = x11rb::connect(None).ok()?;
        let root = connection.query_tree(window).ok()?.reply().ok()?.root;
        let atom = connection
            .intern_atom(false, b"_NET_ACTIVE_WINDOW")
            .ok()?
            .reply()
            .ok()?
            .atom;
        let event = x11_activation_event(window, atom, time);
        connection
            .send_event(
                false,
                root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                event,
            )
            .ok()?
            .check()
            .ok()?;
        connection.flush().ok()?;
        Some(())
    };
    send().is_some()
}

#[cfg(target_os = "linux")]
fn x11_activation_event(
    window: u32,
    atom: u32,
    time: u32,
) -> x11rb::protocol::xproto::ClientMessageEvent {
    // Source indication 2 (pager): the popup is summoned by the user's own
    // hotkey or tray click. As an application request (1), Cinnamon's
    // focus-stealing prevention refused it whenever another window (Mint's
    // Update Manager) held a newer user time, so the popup showed without
    // the keyboard and Esc, typing and Enter went elsewhere.
    x11rb::protocol::xproto::ClientMessageEvent::new(32, window, atom, [2, time, 0, 0, 0])
}

#[cfg(all(test, target_os = "linux"))]
mod activation_message_tests {
    #[test]
    fn net_active_window_message_carries_original_input_time() {
        let event = super::x11_activation_event(123, 456, 789);
        assert_eq!((event.window, event.type_), (123, 456));
        assert_eq!(event.data.as_data32(), [2, 789, 0, 0, 0]);
    }
}

/// Take the foreground for `window` without winit's synthetic Alt press. A
/// tray click leaves the shell holding the foreground lock, so a refused
/// `SetForegroundWindow` is retried while sharing the foreground thread's
/// input state, which the lock permits.
#[cfg(target_os = "windows")]
fn force_foreground(window: &Window) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow,
    };
    let Some(raw) = hwnd(window) else {
        return;
    };
    let hwnd = HWND(raw as *mut std::ffi::c_void);
    // SAFETY: hwnd belongs to the live borrowed window on this event-loop
    // thread, which owns a message queue; input is detached before returning.
    #[allow(unsafe_code)]
    unsafe {
        if SetForegroundWindow(hwnd).as_bool() {
            return;
        }
        let me = GetCurrentThreadId();
        let them = GetWindowThreadProcessId(GetForegroundWindow(), None);
        let attached = them != 0 && them != me && AttachThreadInput(me, them, true).as_bool();
        let _ = SetForegroundWindow(hwnd);
        let _ = BringWindowToTop(hwnd);
        if attached {
            let _ = AttachThreadInput(me, them, false);
        }
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
        ActivationPolicy::Supported
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

/// Try to activate the popup window for Windows/X11/macOS.
/// Returns `Done` when pending should be cleared, `Pending` when it should be retained.
/// On Wayland (`per_show`), always returns `Done` without focusing.
pub fn try_activate<W: OsWindow>(window: &W, per_show: bool) -> ActivationStatus {
    try_activate_with_policy(window, native_policy(per_show))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Activation {
    pending: bool,
    time: Option<u32>,
    /// Focus requests sent for the current activation.
    tries: u8,
    /// The window's focus flag may be stale: set while the popup is closed
    /// (an X server hands the focus to `PointerRoot` on unmap and winit
    /// reports that as focus on our hidden window), cleared by a real focus
    /// event after it opened.
    stale_focus: bool,
}

/// Focus requests per activation before giving up: a window manager may
/// ignore one sent before it has mapped the window (Cinnamon).
const ACTIVATION_TRIES: u8 = 8;

impl Activation {
    #[must_use]
    pub fn new() -> Self {
        Self {
            pending: false,
            time: None,
            tries: 0,
            stale_focus: false,
        }
    }

    /// A focus-in event arrived for the open popup: its focus flag is
    /// current again.
    pub fn focus_seen(&mut self) {
        self.stale_focus = false;
    }

    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.pending
    }

    /// Reconcile with an input timestamp, retaining it while the host is not visible.
    pub fn reconcile_with_time<W: OsWindow>(
        &mut self,
        request: bool,
        previous_open: bool,
        desired_open: bool,
        host: Option<&W>,
        policy: ActivationPolicy,
        time: Option<u32>,
    ) {
        let edge = !previous_open && desired_open;
        if !desired_open {
            self.pending = false;
            self.time = None;
            self.stale_focus = true;
            return;
        }
        if request || edge {
            self.time = time;
            self.tries = 0;
        }
        let mut pending = self.pending || request || edge;
        if pending {
            if let Some(window) = host {
                let status = if policy == ActivationPolicy::Unsupported {
                    ActivationStatus::Done
                } else if window.is_visible() != Some(true) {
                    ActivationStatus::Pending
                } else if window.is_minimized() == Some(true)
                    || (window.has_focus() && !self.stale_focus)
                {
                    ActivationStatus::Done
                } else {
                    // Keep asking until the window manager has given the
                    // focus (checked on the next pass), within a bound.
                    // Retries use CurrentTime (0): a window manager may
                    // refuse the press time once another window has a newer
                    // user time.
                    window.focus_with_time(if self.tries == 0 { self.time } else { Some(0) });
                    self.tries = self.tries.saturating_add(1);
                    if self.tries >= ACTIVATION_TRIES {
                        ActivationStatus::Done
                    } else {
                        ActivationStatus::Pending
                    }
                };
                if status == ActivationStatus::Done {
                    pending = false;
                }
            } else if policy == ActivationPolicy::Supported {
                pending = false;
            }
        }
        self.pending = pending;
        if !pending {
            self.time = None;
        }
    }

    pub fn reconcile<W: OsWindow>(
        &mut self,
        request: bool,
        previous_open: bool,
        desired_open: bool,
        host: Option<&W>,
        policy: ActivationPolicy,
    ) {
        self.reconcile_with_time(request, previous_open, desired_open, host, policy, None);
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
