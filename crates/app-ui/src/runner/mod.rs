//! The runner: our own event loop for egui on winit with a CPU rasterizer.
//! A popup window and an optional settings window, each with its own egui
//! context. App logic runs on every pass, visible or hidden; the ui runs only
//! while the window is shown.

pub mod geometry;
#[cfg(all(test, target_os = "windows"))]
mod native_window_tests;
#[cfg(test)]
mod placement_tests;
pub mod render;
pub mod sched;
mod shutdown;
pub mod window;
#[cfg(target_os = "windows")]
mod windows_popup;

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{ClippedPrimitive, Color32, Context, RawInput, ViewportEvent, ViewportId, ViewportInfo};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy, OwnedDisplayHandle};
use winit::window::{Window, WindowId};

use window::Visibility;

/// The two kinds of window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WindowKind {
    Popup,
    Settings,
}

/// The logical state the app wants. The runner compares it with the previous
/// value after every tick and acts on changes only (edge-triggered): it never
/// hides a window only because the OS shows it while the app says closed, since
/// the daemon shows the popup before the app hears about it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Desired {
    /// The popup is open. false to true shows it, true to false hides it.
    pub popup_open: bool,
    /// The settings window is open. false to true creates the window, true to
    /// false destroys it (its egui context lives on).
    pub settings_open: bool,
    /// Leave the event loop and return from [`run`].
    pub exit: bool,
}

/// Platform metadata associated with a popup show or raise.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PopupActivation {
    /// X11 server time of the triggering input.
    pub x11_time: Option<u32>,
    /// Compositor-provided Wayland activation token.
    pub activation_token: Option<String>,
}

/// The application behind the windows.
pub trait App {
    /// Runs on every pass of every window, hidden or visible: drain queues,
    /// poll timers, set theme and zoom. Shows no ui. While a window is shown it
    /// runs inside `Context::run_ui`, so it can run more than once per frame;
    /// while hidden (or minimized or without area) inside `Context::run_logic`.
    fn logic(&mut self, ctx: &egui::Context, kind: WindowKind);

    /// Runs only for a shown window, inside `Context::run_ui`, after `logic`.
    fn ui(&mut self, ui: &mut egui::Ui, kind: WindowKind);

    /// The logical state the app wants, read after every tick.
    fn desired(&self) -> Desired;

    /// Preserve physical Super in popup shortcut events; other apps keep egui defaults.
    fn preserve_native_super(&self, _kind: WindowKind) -> bool {
        false
    }

    /// Whether a configured action owns native shortcut input and its text side effects.
    fn native_key_override(
        &self,
        _kind: WindowKind,
        _key: egui::Key,
        _modifiers: egui::Modifiers,
    ) -> bool {
        false
    }

    /// One-shot activation request for the popup, drained by the runner.
    fn take_popup_activation(&mut self) -> bool {
        false
    }

    /// Drain activation metadata associated with the pending popup show.
    fn take_popup_activation_metadata(&mut self) -> Option<PopupActivation> {
        None
    }

    /// Whether placement needs fresh desktop context.
    fn wants_desktop_context(&self) -> bool {
        false
    }

    /// Provide fresh desktop context snapshot.
    fn provide_desktop_context(&mut self, _ctx: crate::placement::DesktopContext) {}

    /// A native geometry change can resolve a previously unavailable monitor.
    fn desktop_geometry_changed(&mut self) {}
}

/// Window sizes and titles. Sizes are in points at zoom 1.
#[derive(Clone, Debug)]
pub struct Config {
    pub popup_title: String,
    pub popup_size: [f32; 2],
    pub popup_position: Option<[f32; 2]>,
    pub settings_title: String,
    pub settings_size: [f32; 2],
    pub settings_state_path: Option<std::path::PathBuf>,
}

/// What the app factory receives at start.
pub struct Startup {
    /// Wakes the loop from any thread.
    pub handle: Handle,
    /// The popup's Win32 handle, read on the loop thread (see
    /// [`window::hwnd`]); `None` off Windows and on Wayland, where the popup
    /// is created per show.
    pub popup_hwnd: Option<u64>,
    /// The popup X11 window ID when available.
    pub popup_x11_window: Option<u64>,
}

/// Cloneable, `Send` handle to the event loop.
#[derive(Clone)]
pub struct Handle {
    proxy: EventLoopProxy<UserEvent>,
}

impl Handle {
    /// Ask the loop to run `App::logic` on every window now. Never blocks;
    /// does nothing once the loop is gone.
    pub fn wake(&self) {
        let _ = self.proxy.send_event(UserEvent::Wake);
    }

    /// `wake` as a shareable closure, the type of `crate::push::Wake`.
    #[must_use]
    pub fn waker(&self) -> Arc<dyn Fn() + Send + Sync> {
        let handle = self.clone();
        Arc::new(move || handle.wake())
    }
}

/// Why [`run`] failed.
#[derive(Debug)]
pub enum RunError {
    EventLoop(winit::error::EventLoopError),
    Window(winit::error::OsError),
    NativeWindow(std::io::Error),
    Surface(softbuffer::SoftBufferError),
    Teardown(String),
}

impl RunError {
    /// Stable diagnostic class; excludes OS text and window/profile identifiers.
    #[must_use]
    pub fn runtime_class(&self) -> &'static str {
        match self {
            Self::EventLoop(_) => "event_loop",
            Self::Window(_) => "window",
            Self::NativeWindow(_) => "native_window",
            Self::Surface(_) => "surface",
            Self::Teardown(_) => "teardown",
        }
    }
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EventLoop(e) => write!(f, "event loop: {e}"),
            Self::Window(e) => write!(f, "window: {e}"),
            Self::NativeWindow(e) => write!(f, "native window: {e}"),
            Self::Surface(e) => write!(f, "surface: {e}"),
            Self::Teardown(m) => write!(f, "teardown: {m}"),
        }
    }
}

impl std::error::Error for RunError {}

/// Events posted to the loop from other threads or from egui.
enum UserEvent {
    /// [`Handle::wake`]: every window is due now.
    Wake,
    /// egui asked for a repaint of `kind` after `delay`; `pass` is the
    /// request's `current_cumulative_pass_nr`.
    Repaint {
        kind: WindowKind,
        delay: Duration,
        pass: u64,
    },
}

/// Run the event loop until [`Desired::exit`] is set.
///
/// `make_app` runs once, at the first resume, after the popup window was
/// created (on Wayland the popup is created per show, so none exists yet and
/// `Startup::popup_hwnd` is `None`); it receives the handle and the popup's HWND.
///
/// # Errors
/// [`RunError`] when the event loop, a window or a surface cannot be created.
pub fn run<A: App + 'static>(
    config: Config,
    make_app: impl FnOnce(Startup) -> A + 'static,
) -> Result<(), RunError> {
    let mut builder = EventLoop::<UserEvent>::with_user_event();
    // macOS: a menu-bar app (no Dock icon, no app menu).
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS as _};
        builder
            .with_activation_policy(ActivationPolicy::Accessory)
            .with_default_menu(false);
    }
    let event_loop = builder.build().map_err(RunError::EventLoop)?;
    let proxy = event_loop.create_proxy();
    let mut runner = Runner::new(config, proxy, Box::new(make_app));
    let loop_result = event_loop.run_app(&mut runner).map_err(RunError::EventLoop);
    shutdown::finish(runner.error, loop_result)
}

/// The OS window of a slot and everything bound to it.
struct Host {
    window: Arc<Window>,
    state: egui_winit::State,
    presenter: render::Presenter,
    present_error_logged: bool,
    info: ViewportInfo,
    visibility: Visibility,
    /// Wayland: present only after the compositor's frame callback.
    frame_throttle: bool,
    /// A frame went out and its frame callback has not arrived yet. A
    /// compositor may hold the buffers of a covered window (`KWin` does), and
    /// softbuffer then blocks the whole UI thread waiting for one; so passes
    /// run the UI logic but do not paint until `RedrawRequested`.
    awaiting_frame: bool,
    /// Keyboard focus as last reported. A covered Wayland window still gets
    /// frame callbacks from `KWin` but not its buffers back, so a window that
    /// lost focus paints once more and then waits for focus.
    focus: Focus,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    /// Not focused yet (a window paints freely until it first gets focus).
    Never,
    Focused,
    /// Lost focus; frames presented since.
    Lost {
        paints: u8,
    },
}

impl Host {
    /// Present one frame. The first error of this window is printed, later
    /// ones are dropped.
    fn present(
        &mut self,
        rasterizer: &mut render::Rasterizer,
        size: PhysicalSize<u32>,
        pixels_per_point: f32,
        clear: Color32,
        primitives: &[ClippedPrimitive],
    ) {
        if self.frame_throttle {
            self.window.pre_present_notify();
        }
        let result = self
            .presenter
            .present(rasterizer, size, pixels_per_point, clear, primitives);
        if self.frame_throttle && matches!(result, Ok(true)) {
            // winit answers with `RedrawRequested` once the frame callback
            // arrives.
            self.awaiting_frame = true;
            self.window.request_redraw();
            if let Focus::Lost { paints } = &mut self.focus {
                *paints = paints.saturating_add(1);
            }
        }
        if let Err(error) = result {
            if !self.present_error_logged {
                self.present_error_logged = true;
                let _ = clipmonger_core::runtime_log::global_emit(
                    clipmonger_core::runtime_log::Role::Ui,
                    clipmonger_core::runtime_log::Severity::Error,
                    "present.failed",
                    &[("class", "surface")],
                );
                eprintln!("cannot present a frame: {error}");
            }
        }
    }
}

/// Native modifier snapshots are applied only to events appended by one translation.
#[derive(Default)]
struct NativeInput {
    modifiers: egui::Modifiers,
}

impl NativeInput {
    fn observe(&mut self, event: &WindowEvent, is_mac: bool) {
        match event {
            WindowEvent::ModifiersChanged(mods) => {
                let state = mods.state();
                self.modifiers = egui::Modifiers {
                    alt: state.alt_key(),
                    ctrl: state.control_key(),
                    shift: state.shift_key(),
                    mac_cmd: state.super_key(),
                    command: if is_mac {
                        state.super_key()
                    } else {
                        state.control_key()
                    },
                };
            }
            WindowEvent::Focused(false) | WindowEvent::Destroyed => self.reset(),
            _ => {}
        }
    }

    fn clipboard_override<A: App>(
        &self,
        app: &A,
        kind: WindowKind,
        key: &winit::keyboard::Key,
        pressed: bool,
        translated: &[egui::Event],
    ) -> Option<egui::Key> {
        self.clipboard_key(key, pressed, translated)
            .filter(|key| app.native_key_override(kind, *key, self.modifiers))
    }

    fn clipboard_key(
        &self,
        key: &winit::keyboard::Key,
        pressed: bool,
        translated: &[egui::Event],
    ) -> Option<egui::Key> {
        if !pressed {
            return None;
        }
        // Use the translator's effective key, including its logical-first physical
        // fallback. Re-mapping the native character would miss Option text like ø.
        if self.modifiers.alt {
            return translated.iter().find_map(|event| match event {
                egui::Event::Key {
                    key, pressed: true, ..
                } => Some(*key),
                _ => None,
            });
        }
        if !self.modifiers.command_only() {
            return None;
        }
        match key.as_ref() {
            winit::keyboard::Key::Character("c" | "C") => Some(egui::Key::C),
            winit::keyboard::Key::Character("x" | "X") => Some(egui::Key::X),
            winit::keyboard::Key::Character("v" | "V") => Some(egui::Key::V),
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.modifiers = egui::Modifiers::NONE;
    }

    fn finish(&self, input: &mut RawInput, start: usize, override_key: Option<egui::Key>) {
        // egui-winit can append Text after Key for printable Super chords on non-Mac.
        let mut appended = input.events.split_off(start);
        let has_key = appended
            .iter()
            .any(|event| matches!(event, egui::Event::Key { .. }));
        appended.retain_mut(|event| match event {
            egui::Event::Key { modifiers, .. } | egui::Event::ModifiersChanged(modifiers) => {
                *modifiers = self.modifiers;
                true
            }
            egui::Event::Text(_)
                if override_key.is_some() || (has_key && self.modifiers.mac_cmd) =>
            {
                false
            }
            _ => true,
        });
        if !has_key {
            if let Some(key) = override_key {
                appended.push(egui::Event::Key {
                    key,
                    physical_key: Some(key),
                    pressed: true,
                    repeat: false,
                    modifiers: self.modifiers,
                });
            }
        }
        input.events.extend(appended);
    }
}

/// One egui context with its rasterizer, deadline and, while it exists, its OS window.
struct Slot {
    kind: WindowKind,
    ctx: Context,
    rasterizer: render::Rasterizer,
    pending_raw: RawInput,
    deadline: sched::Deadline,
    host: Option<Host>,
    native_input: NativeInput,
}

/// Construct either window's egui context before it receives input.
fn new_context() -> Context {
    let ctx = Context::default();
    ctx.options_mut(|options| options.zoom_with_keyboard = false);
    ctx
}

impl Slot {
    fn new(
        kind: WindowKind,
        proxy: &EventLoopProxy<UserEvent>,
        fonts: &Arc<egui_system_font_provider::SystemFontProvider>,
    ) -> Self {
        let ctx = new_context();
        ctx.add_font_provider(fonts.clone());
        let proxy = proxy.clone();
        ctx.set_request_repaint_callback(move |info| {
            let _ = proxy.send_event(UserEvent::Repaint {
                kind,
                delay: info.delay,
                pass: info.current_cumulative_pass_nr,
            });
        });
        Self {
            kind,
            ctx,
            rasterizer: render::Rasterizer::new(),
            pending_raw: RawInput::default(),
            deadline: sched::Deadline::new(),
            host: None,
            native_input: NativeInput::default(),
        }
    }

    /// A frame can be seen: a host exists, `window.is_visible() != Some(false)`,
    /// `window.is_minimized() != Some(true)`, and `window.inner_size()` has width and height > 0.
    fn paintable(&self) -> bool {
        if let Some(host) = &self.host {
            if host.awaiting_frame {
                return false;
            }
            if host.frame_throttle && matches!(host.focus, Focus::Lost { paints } if paints >= 1) {
                return false;
            }
            if host.window.is_visible() == Some(false) {
                return false;
            }
            if host.window.is_minimized() == Some(true) {
                return false;
            }
            let size = host.window.inner_size();
            if size.width == 0 || size.height == 0 {
                return false;
            }
            true
        } else {
            false
        }
    }

    /// Passes run for the popup always and for settings only while its window exists.
    fn active(&self) -> bool {
        match self.kind {
            WindowKind::Popup => true,
            WindowKind::Settings => self.host.is_some(),
        }
    }
}

struct Runner<A: App> {
    config: Config,
    proxy: EventLoopProxy<UserEvent>,
    fonts: Arc<egui_system_font_provider::SystemFontProvider>,
    make_app: Option<Box<dyn FnOnce(Startup) -> A>>,
    app: Option<A>,
    surface_context: Option<softbuffer::Context<OwnedDisplayHandle>>,
    per_show: bool,
    popup: Slot,
    settings: Option<Slot>,
    last: Desired,
    pending_activation: window::Activation,
    error: Option<RunError>,
    geometry: geometry::GeometryState,
    shutdown: Option<shutdown::Shutdown>,
    orphan_pending: std::collections::HashSet<WindowId>,
}

impl<A: App> Runner<A> {
    fn new(
        config: Config,
        proxy: EventLoopProxy<UserEvent>,
        make_app: Box<dyn FnOnce(Startup) -> A>,
    ) -> Self {
        let fonts = Arc::new(egui_system_font_provider::SystemFontProvider::new());
        let popup = Slot::new(WindowKind::Popup, &proxy, &fonts);
        let geometry = geometry::GeometryState::new(config.settings_state_path.clone());
        Self {
            config,
            proxy,
            fonts,
            make_app: Some(make_app),
            app: None,
            surface_context: None,
            per_show: false,
            popup,
            settings: None,
            last: Desired::default(),
            pending_activation: window::Activation::new(),
            error: None,
            geometry,
            shutdown: None,
            orphan_pending: std::collections::HashSet::new(),
        }
    }

    fn slot_mut(&mut self, kind: WindowKind) -> Option<&mut Slot> {
        match kind {
            WindowKind::Popup => Some(&mut self.popup),
            WindowKind::Settings => self.settings.as_mut(),
        }
    }

    fn resumed_running(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(app) = self.app.as_mut() {
            if notify_desktop_geometry(app, DesktopGeometrySignal::Resumed) {
                self.popup.deadline.at(Instant::now());
                self.set_flow(event_loop);
            }
            return;
        }
        if let Err(e) = self.start(event_loop) {
            self.fail(event_loop, e);
        }
    }

    fn user_event_running(&mut self, event_loop: &ActiveEventLoop, event: &UserEvent) {
        let now = Instant::now();
        match *event {
            UserEvent::Wake => {
                self.popup.deadline.at(now);
                if let Some(slot) = self.settings.as_mut() {
                    slot.deadline.at(now);
                }
            }
            UserEvent::Repaint { kind, delay, pass } => {
                if let Some(slot) = self.slot_mut(kind) {
                    let current = slot.ctx.cumulative_pass_nr_for(ViewportId::ROOT);
                    slot.deadline.request(now, delay, pass, current);
                }
            }
        }
        self.set_flow(event_loop);
    }

    /// Save geometry and release complete hosts through the shared transition.
    fn begin_shutdown(&mut self, event_loop: &ActiveEventLoop) {
        let geometry = &mut self.geometry;
        let global_position = !self.per_show;
        let directive = shutdown::begin(
            &mut self.shutdown,
            &mut self.popup.host,
            self.settings.as_mut().map(|slot| &mut slot.host),
            &mut self.orphan_pending,
            Instant::now(),
            |host| {
                geometry.handle(
                    geometry::Lifecycle::Exit,
                    host.map(|host: &Host| &*host.window),
                    global_position,
                    |message| eprintln!("{message}"),
                );
            },
            |host| host.window.id(),
        );
        self.popup.native_input.reset();
        self.popup.deadline.clear();
        if let Some(slot) = self.settings.as_mut() {
            slot.native_input.reset();
            slot.deadline.clear();
        }
        self.apply_shutdown(event_loop, directive);
    }

    /// Apply a terminal decision without dropping the permanent shutdown guard.
    fn apply_shutdown(&mut self, event_loop: &ActiveEventLoop, directive: shutdown::Directive) {
        shutdown::record_timeout(directive, &mut self.error, |pending| {
            RunError::Teardown(format!(
                "teardown timed out with {pending} pending after {:?}",
                shutdown::TEARDOWN_TIMEOUT
            ))
        });
        match directive {
            shutdown::Directive::Run => {}
            shutdown::Directive::WaitUntil(deadline) => {
                event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(deadline));
            }
            shutdown::Directive::Exit | shutdown::Directive::TimedOut(_) => event_loop.exit(),
        }
    }

    /// Remember the first error and enter shutdown drain.
    fn fail(&mut self, event_loop: &ActiveEventLoop, error: RunError) {
        shutdown::record_error(&mut self.error, error);
        self.begin_shutdown(event_loop);
    }

    /// Create the OS window for a slot.
    #[allow(clippy::cast_possible_truncation, clippy::too_many_lines)]
    fn create_host(
        &mut self,
        event_loop: &ActiveEventLoop,
        kind: WindowKind,
        visible: bool,
        activation_token: Option<&str>,
    ) -> Result<Host, RunError> {
        let frame_throttle = window::is_wayland(event_loop);
        let builder = match kind {
            WindowKind::Popup => window::popup_builder(
                &self.config.popup_title,
                self.config.popup_size,
                self.config.popup_position,
            )
            .with_visible(visible),
            WindowKind::Settings => {
                window::settings_builder(&self.config.settings_title, self.config.settings_size)
                    .with_visible(visible)
            }
        };
        let class = match kind {
            WindowKind::Popup => window::POPUP_CLASS,
            WindowKind::Settings => window::SETTINGS_CLASS,
        };
        let ctx = match kind {
            WindowKind::Popup => &self.popup.ctx,
            WindowKind::Settings => {
                &self
                    .settings
                    .as_ref()
                    .expect("settings slot must exist")
                    .ctx
            }
        };
        let surface_context = self
            .surface_context
            .as_ref()
            .expect("surface_context must exist");
        let restore = if kind == WindowKind::Settings {
            let monitors: Vec<_> = event_loop
                .available_monitors()
                .map(|monitor| geometry::MonitorGeometry {
                    bounds: monitor_rect(&monitor),
                    scale: monitor.scale_factor(),
                })
                .collect();
            let preferred = event_loop
                .primary_monitor()
                .map(|monitor| geometry::MonitorGeometry {
                    bounds: monitor_rect(&monitor),
                    scale: monitor.scale_factor(),
                });
            Some(self.geometry.normalized_for_restore(
                self.config.settings_size,
                &monitors,
                preferred,
                !self.per_show,
            ))
        } else {
            None
        };
        // Explicit window creation to capture WindowId if Presenter fails.
        let mut prepared = window::prepare_host(ctx, builder.clone(), class, restore.as_ref());
        prepared.attributes = window::show_attributes(
            prepared.attributes,
            self.per_show && kind == WindowKind::Popup,
            activation_token,
        );
        let window = Arc::new(
            event_loop
                .create_window(prepared.attributes.clone())
                .map_err(RunError::Window)?,
        );
        let window_id = window.id();
        let host_res = (|| {
            #[cfg(target_os = "windows")]
            windows_popup::install(&window, kind).map_err(RunError::NativeWindow)?;
            let ((state, presenter, info), visibility) = prepared.finish_setup(&*window, |b| {
                egui_winit::apply_viewport_builder_to_window(ctx, &window, b);
                let state = egui_winit::State::new(
                    ctx.clone(),
                    ViewportId::ROOT,
                    &*window,
                    Some(window.scale_factor() as f32),
                    window.theme(),
                    None,
                );
                let presenter = render::Presenter::new(surface_context, Arc::clone(&window))
                    .map_err(RunError::Surface)?;
                let mut info = ViewportInfo::default();
                egui_winit::update_viewport_info(&mut info, ctx, &window, true);
                Ok::<_, RunError>((state, presenter, info))
            })?;
            Ok::<_, RunError>(Host {
                window,
                state,
                presenter,
                present_error_logged: false,
                info,
                visibility,
                frame_throttle,
                awaiting_frame: false,
                focus: Focus::Never,
            })
        })();
        match host_res {
            Ok(host) => Ok(host),
            Err(e) => {
                // Window was created and will be dropped here, posting Destroy.
                // Track its id so shutdown drain can account for it if needed.
                self.orphan_pending.insert(window_id);
                Err(e)
            }
        }
    }

    /// Initialize the runner on first resume.
    fn start(&mut self, event_loop: &ActiveEventLoop) -> Result<(), RunError> {
        let surface_context = softbuffer::Context::new(event_loop.owned_display_handle())
            .map_err(RunError::Surface)?;
        self.surface_context = Some(surface_context);
        self.per_show = window::is_wayland(event_loop);
        let mut popup_hwnd = None;
        let mut popup_x11_window = None;
        if !self.per_show {
            let host = self.create_host(event_loop, WindowKind::Popup, false, None)?;
            popup_hwnd = window::hwnd(&host.window);
            popup_x11_window = window::x11_window(&host.window);
            self.popup.native_input.reset();
            self.popup.host = Some(host);
        }
        let make_app = self.make_app.take().expect("make_app already taken");
        let handle = Handle {
            proxy: self.proxy.clone(),
        };
        let app = make_app(Startup {
            handle,
            popup_hwnd,
            popup_x11_window,
        });
        self.app = Some(app);
        self.popup.deadline.at(Instant::now());
        Ok(())
    }

    /// Update the event loop control flow from deadlines.
    fn set_flow(&self, event_loop: &ActiveEventLoop) {
        if let Some(shutdown) = &self.shutdown {
            event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                shutdown.deadline(),
            ));
            return;
        }
        let deadlines = [
            self.popup.deadline.next(),
            self.settings.as_ref().and_then(|s| s.deadline.next()),
        ];
        event_loop.set_control_flow(sched::control_flow(deadlines));
    }

    /// Run due passes and reconcile.
    fn tick(&mut self, event_loop: &ActiveEventLoop) {
        let directive = shutdown::poll(&mut self.shutdown, Instant::now());
        if directive != shutdown::Directive::Run {
            self.apply_shutdown(event_loop, directive);
            return;
        }
        let now = Instant::now();
        for kind in [WindowKind::Popup, WindowKind::Settings] {
            let should_run = {
                if let Some(slot) = self.slot_mut(kind) {
                    let run = slot.active() && slot.deadline.is_due(now);
                    if run || !slot.active() {
                        slot.deadline.clear();
                    }
                    run
                } else {
                    false
                }
            };
            if should_run {
                self.run_pass(kind);
            }
        }
        self.reconcile(event_loop);
        self.set_flow(event_loop);
    }

    /// Run one pass for a window.
    fn run_pass(&mut self, kind: WindowKind) {
        fn inner<A: App>(app: &mut A, slot: &mut Slot, kind: WindowKind) {
            let ctx = slot.ctx.clone();
            let paintable = slot.paintable();
            let mut raw = std::mem::take(&mut slot.pending_raw);
            if let Some(host) = slot.host.as_mut() {
                egui_winit::update_viewport_info(&mut host.info, &ctx, &host.window, false);
                let mut fresh = host.state.take_egui_input(&host.window);
                fresh.viewports = std::iter::once((ViewportId::ROOT, host.info.clone())).collect();
                host.info.events.clear();
                raw.append(fresh);
            }
            let commands = if paintable {
                let output = ctx.run_ui(raw, |ui| {
                    app.logic(ui.ctx(), kind);
                    app.ui(ui, kind);
                });
                let egui::FullOutput {
                    platform_output,
                    textures_delta,
                    shapes,
                    pixels_per_point,
                    mut viewport_output,
                } = output;
                slot.rasterizer.hold(textures_delta);
                if let Some(host) = slot.host.as_mut() {
                    host.state
                        .handle_platform_output(&host.window, platform_output);
                }
                let primitives = ctx.tessellate(shapes, pixels_per_point);
                let clear = ctx.global_style().visuals.panel_fill;
                if let Some(host) = slot.host.as_mut() {
                    let size = host.window.inner_size();
                    host.present(
                        &mut slot.rasterizer,
                        size,
                        pixels_per_point,
                        clear,
                        &primitives,
                    );
                }
                viewport_output
                    .remove(&ViewportId::ROOT)
                    .map(|viewport| viewport.commands)
            } else {
                let mut output = ctx.run_logic(&raw, |ctx| app.logic(ctx, kind));
                slot.pending_raw = raw;
                if let Some(host) = slot.host.as_mut() {
                    host.state
                        .handle_platform_output(&host.window, output.platform_output);
                }
                output.viewport_commands.remove(&ViewportId::ROOT)
            };
            let allowed: Vec<_> = commands
                .unwrap_or_default()
                .into_iter()
                .filter(window::allows)
                .collect();
            if !allowed.is_empty() {
                if let Some(host) = slot.host.as_mut() {
                    egui_winit::process_viewport_commands(
                        &ctx,
                        &mut host.info,
                        allowed,
                        &host.window,
                        &mut Vec::new(),
                    );
                }
            }
        }
        match kind {
            WindowKind::Popup => {
                let Self {
                    app: app_opt,
                    popup: slot,
                    ..
                } = self;
                let Some(app) = app_opt.as_mut() else {
                    return;
                };
                inner(app, slot, kind);
            }
            WindowKind::Settings => {
                let Self {
                    app: app_opt,
                    settings,
                    ..
                } = self;
                let Some(app) = app_opt.as_mut() else {
                    return;
                };
                let Some(slot) = settings.as_mut() else {
                    return;
                };
                inner(app, slot, kind);
            }
        }
    }

    /// Reconcile desired state with OS windows.
    fn reconcile(&mut self, event_loop: &ActiveEventLoop) {
        shutdown::ordinary_work(self.shutdown.is_some(), || {
            self.reconcile_running(event_loop);
        });
    }

    fn reconcile_running(&mut self, event_loop: &ActiveEventLoop) {
        let Some(app) = self.app.as_mut() else {
            return;
        };
        let desired = app.desired();
        let request = app.take_popup_activation();
        let metadata = app.take_popup_activation_metadata();
        let previous = std::mem::replace(&mut self.last, desired);
        if desired.exit {
            self.begin_shutdown(event_loop);
            return;
        }
        let now = Instant::now();
        if desired.popup_open != previous.popup_open {
            if let Err(e) = self.set_popup_open(
                event_loop,
                desired.popup_open,
                metadata
                    .as_ref()
                    .and_then(|m| m.activation_token.as_deref()),
                metadata.as_ref().and_then(|m| m.x11_time),
            ) {
                self.fail(event_loop, e);
                return;
            }
            self.popup.deadline.at(now);
        } else if self.per_show && request && desired.popup_open {
            // Wayland: a client cannot raise or focus its own mapped window
            // without a fresh activation token, and a popup that never had
            // the focus (opened behind another window) would stay there. A
            // hotkey raise shows it again as a new window, which gets the
            // press's token (and, on GNOME, the extension's placement and
            // activation).
            let token = metadata
                .as_ref()
                .and_then(|m| m.activation_token.as_deref());
            let reopened = self
                .set_popup_open(event_loop, false, None, None)
                .and_then(|()| self.set_popup_open(event_loop, true, token, None));
            if let Err(e) = reopened {
                self.fail(event_loop, e);
                return;
            }
            self.popup.deadline.at(now);
        }
        if desired.settings_open != previous.settings_open {
            if let Err(e) = self.set_settings_open(event_loop, desired.settings_open) {
                self.fail(event_loop, e);
                return;
            }
            if let Some(slot) = self.settings.as_mut() {
                slot.deadline.at(now);
            }
        }
        let policy = window::native_policy(self.per_show);
        if let Some(host) = self.popup.host.as_ref() {
            self.pending_activation.reconcile_with_time(
                request,
                previous.popup_open,
                desired.popup_open,
                Some(&*host.window),
                policy,
                metadata.as_ref().and_then(|m| m.x11_time),
            );
        } else {
            self.pending_activation.reconcile_with_time(
                request,
                previous.popup_open,
                desired.popup_open,
                None::<&winit::window::Window>,
                policy,
                metadata.as_ref().and_then(|m| m.x11_time),
            );
        }
        // An activation still waiting (window not mapped yet, or the focus
        // request not yet honoured) is retried on a short timer.
        if self.pending_activation.is_pending() {
            self.popup
                .deadline
                .at(now + std::time::Duration::from_millis(40));
        }
        let context = self
            .app
            .as_ref()
            .filter(|app| app.wants_desktop_context())
            .and_then(|_| self.collect_desktop_context(event_loop));
        if let Some(app) = self.app.as_mut() {
            if deliver_desktop_context(app, || context) {
                self.popup.deadline.at(Instant::now());
            }
        }
    }

    /// Show or hide the popup window.
    fn set_popup_open(
        &mut self,
        event_loop: &ActiveEventLoop,
        open: bool,
        activation_token: Option<&str>,
        x11_time: Option<u32>,
    ) -> Result<(), RunError> {
        #[cfg(not(target_os = "linux"))]
        let _ = x11_time;
        if self.per_show {
            if open {
                let host =
                    self.create_host(event_loop, WindowKind::Popup, true, activation_token)?;
                self.popup.native_input.reset();
                self.popup.host = Some(host);
            } else {
                self.popup.host = None;
                self.popup.native_input.reset();
            }
        } else if let Some(host) = self.popup.host.as_mut() {
            if open {
                // The press time as the window's user time: window managers
                // decide on map whether a new window may take the focus.
                #[cfg(target_os = "linux")]
                if let (Some(time), Some(window)) = (x11_time, window::x11_window(&host.window)) {
                    window::set_x11_user_time(window, time);
                }
                host.visibility.show(&*host.window);
            } else {
                host.visibility.hide(&*host.window);
                // macOS: hiding the app gives the focus back to the app used
                // before the popup (the paste target); a background process
                // may not activate it on macOS 14+.
                #[cfg(target_os = "macos")]
                if !self.last.settings_open {
                    clipmonger_backend_macos::ui::hide_app();
                }
            }
        }
        Ok(())
    }

    /// Show or hide the settings window.
    fn set_settings_open(
        &mut self,
        event_loop: &ActiveEventLoop,
        open: bool,
    ) -> Result<(), RunError> {
        if open {
            if self.settings.is_none() {
                let slot = Slot::new(WindowKind::Settings, &self.proxy, &self.fonts);
                self.settings = Some(slot);
            }
            let needs_host = self
                .settings
                .as_ref()
                .is_some_and(|slot| slot.host.is_none());
            if needs_host {
                let host = self.create_host(event_loop, WindowKind::Settings, true, None)?;
                if let Some(slot) = self.settings.as_mut() {
                    slot.native_input.reset();
                    slot.host = Some(host);
                }
            }
        } else {
            self.settings_geometry(geometry::Lifecycle::Close);
            if let Some(slot) = self.settings.as_mut() {
                slot.host = None;
                slot.native_input.reset();
                slot.deadline.clear();
            }
        }
        Ok(())
    }

    fn window_geometry_event(&mut self, kind: WindowKind, event: &WindowEvent) {
        if kind == WindowKind::Settings
            && matches!(
                event,
                WindowEvent::CloseRequested
                    | WindowEvent::Resized(_)
                    | WindowEvent::Moved(_)
                    | WindowEvent::ScaleFactorChanged { .. }
            )
        {
            self.settings_geometry(geometry::Lifecycle::Changed);
        }
        if let Some(app) = self.app.as_mut() {
            notify_desktop_geometry(app, DesktopGeometrySignal::Window(kind, event));
        }
    }

    fn settings_geometry(&mut self, lifecycle: geometry::Lifecycle) {
        let window = self
            .settings
            .as_ref()
            .and_then(|slot| slot.host.as_ref())
            .map(|host| &*host.window);
        self.geometry
            .handle(lifecycle, window, !self.per_show, |message| {
                eprintln!("{message}");
            });
    }

    fn collect_desktop_context(
        &self,
        event_loop: &ActiveEventLoop,
    ) -> Option<crate::placement::DesktopContext> {
        let host = self.popup.host.as_ref()?;
        let physical = host.window.inner_size();
        let current_scale = host.window.scale_factor();
        let monitors: Vec<_> = event_loop
            .available_monitors()
            .map(|monitor| (work_rect(&monitor), monitor.scale_factor()))
            .collect();
        Some(desktop_context_from_snapshot(
            (physical.width, physical.height),
            current_scale,
            &monitors,
            query_cursor(event_loop),
            host.window.current_monitor().as_ref().map(work_rect),
            event_loop.primary_monitor().as_ref().map(work_rect),
            !self.per_show,
        ))
    }
}

/// Build placement context from one native snapshot, predicting the physical size
/// after a move to the destination monitor without querying the desktop again.
fn desktop_context_from_snapshot(
    current_physical: (u32, u32),
    current_scale: f64,
    raw_monitors: &[(crate::placement::Rect, f64)],
    cursor: Option<(i32, i32)>,
    current_monitor: Option<crate::placement::Rect>,
    primary_monitor: Option<crate::placement::Rect>,
    supports_global_position: bool,
) -> crate::placement::DesktopContext {
    let monitors: Vec<_> = raw_monitors.iter().map(|(rect, _)| *rect).collect();
    let destination =
        crate::placement::destination_monitor(cursor, &monitors, current_monitor, primary_monitor);
    // Without a matching destination scale, preserve the known physical size.
    let destination_scale = raw_monitors
        .iter()
        .find(|(rect, _)| Some(*rect) == destination)
        .map_or(current_scale, |(_, scale)| *scale);
    let popup_physical_size = crate::placement::predicted_physical_size(
        current_physical,
        current_scale,
        destination_scale,
    );
    crate::placement::DesktopContext {
        monitors,
        cursor,
        current_monitor,
        primary_monitor,
        popup_physical_size,
        supports_global_position,
        destination_scale,
    }
}

/// Deliver exactly one requested native snapshot after host reconciliation.
/// The collector is lazy: an identical Config or an already-open raise does no query.
pub fn deliver_desktop_context(
    app: &mut impl App,
    collect: impl FnOnce() -> Option<crate::placement::DesktopContext>,
) -> bool {
    if !app.wants_desktop_context() {
        return false;
    }
    let Some(context) = collect() else {
        return false;
    };
    app.provide_desktop_context(context);
    true
}

/// Native signals that may make missing monitor geometry available.
#[derive(Clone, Copy)]
pub enum DesktopGeometrySignal<'a> {
    Resumed,
    Window(WindowKind, &'a WindowEvent),
}

/// Wake only unresolved placement; ordinary frames, Config and Raise do not retry.
pub fn notify_desktop_geometry(app: &mut impl App, signal: DesktopGeometrySignal<'_>) -> bool {
    let relevant = match signal {
        DesktopGeometrySignal::Resumed => true,
        DesktopGeometrySignal::Window(kind, event) => {
            kind == WindowKind::Popup
                && matches!(
                    event,
                    WindowEvent::Moved(_)
                        | WindowEvent::Resized(_)
                        | WindowEvent::ScaleFactorChanged { .. }
                )
        }
    };
    if relevant {
        app.desktop_geometry_changed();
        app.wants_desktop_context()
    } else {
        false
    }
}

fn monitor_rect(monitor: &winit::monitor::MonitorHandle) -> crate::placement::Rect {
    let position = monitor.position();
    let size = monitor.size();
    crate::placement::Rect {
        x: position.x,
        y: position.y,
        w: i32::try_from(size.width).unwrap_or(i32::MAX),
        h: i32::try_from(size.height).unwrap_or(i32::MAX),
    }
}

/// Usable area of a monitor for the popup: the work area without the taskbar
/// on Windows, without the menu bar and Dock on macOS, the whole monitor
/// elsewhere.
fn work_rect(monitor: &winit::monitor::MonitorHandle) -> crate::placement::Rect {
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFO};
        use winit::platform::windows::MonitorHandleExtWindows as _;
        let mut info = MONITORINFO {
            cbSize: u32::try_from(std::mem::size_of::<MONITORINFO>()).unwrap_or(0),
            ..MONITORINFO::default()
        };
        // SAFETY: the handle comes from winit's live monitor enumeration and
        // `info` is a writable MONITORINFO with its size set.
        #[allow(unsafe_code)]
        let ok = unsafe { GetMonitorInfoW(HMONITOR(monitor.hmonitor() as _), &raw mut info) };
        if ok.as_bool() {
            let work = info.rcWork;
            return crate::placement::Rect {
                x: work.left,
                y: work.top,
                w: work.right.saturating_sub(work.left),
                h: work.bottom.saturating_sub(work.top),
            };
        }
    }
    #[cfg(target_os = "macos")]
    {
        let position = monitor.position();
        if let Some(area) = clipmonger_backend_macos::ui::work_area(position.x, position.y) {
            return crate::placement::Rect {
                x: area.x,
                y: area.y,
                w: area.w,
                h: area.h,
            };
        }
    }
    monitor_rect(monitor)
}

fn query_cursor(event_loop: &ActiveEventLoop) -> Option<(i32, i32)> {
    if window::is_wayland(event_loop) {
        return None;
    }
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::{Foundation::POINT, UI::WindowsAndMessaging::GetCursorPos};
        let mut point = POINT::default();
        // SAFETY: the event-loop thread owns this valid, writable POINT.
        #[allow(unsafe_code)]
        if unsafe { GetCursorPos(&raw mut point) }.is_ok() {
            return Some((point.x, point.y));
        }
    }
    #[cfg(target_os = "macos")]
    if let Some(position) = clipmonger_backend_macos::ui::cursor() {
        return Some(position);
    }
    #[cfg(target_os = "linux")]
    {
        use winit::platform::x11::ActiveEventLoopExtX11 as _;
        use x11rb::{connection::Connection as _, protocol::xproto::ConnectionExt as _};
        if event_loop.is_x11() {
            let (connection, _) = x11rb::connect(None).ok()?;
            // A separate connection may default to a different X screen. The
            // pointer belongs to the root whose same_screen response is true.
            for screen in &connection.setup().roots {
                let pointer = connection.query_pointer(screen.root).ok()?.reply().ok()?;
                if pointer.same_screen {
                    return Some((i32::from(pointer.root_x), i32::from(pointer.root_y)));
                }
            }
        }
    }
    None
}

impl<A: App> ApplicationHandler<UserEvent> for Runner<A> {
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.settings_geometry(geometry::Lifecycle::Exit);
    }

    /// Handle resumed event.
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        shutdown::ordinary_work(self.shutdown.is_some(), || self.resumed_running(event_loop));
    }

    /// Handle wake and repaint events.
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        shutdown::ordinary_work(self.shutdown.is_some(), || {
            self.user_event_running(event_loop, &event);
        });
    }

    /// Handle window events.
    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        // Dispatch owns acknowledgment-before-lookup ordering, including after
        // the complete hosts have been removed from their slots.
        let dispatch = shutdown::window_event(
            &mut self.shutdown,
            self.popup.host.as_ref(),
            self.settings.as_ref().and_then(|slot| slot.host.as_ref()),
            window_id,
            &event,
            Instant::now(),
            |host| host.window.id(),
        );
        let kind = match dispatch {
            shutdown::WindowDispatch::Normal(kind) => kind,
            shutdown::WindowDispatch::Shutdown(directive) => {
                self.apply_shutdown(event_loop, directive);
                return;
            }
        };
        let now = Instant::now();
        let close = matches!(event, WindowEvent::CloseRequested);
        if kind == Some(WindowKind::Popup)
            && self.last.popup_open
            && matches!(event, WindowEvent::Focused(true))
        {
            self.pending_activation.focus_seen();
        }
        if let Some(kind) = kind {
            self.window_geometry_event(kind, &event);
            let preserve_super = self
                .app
                .as_ref()
                .is_some_and(|app| app.preserve_native_super(kind));
            let app = self.app.as_ref();
            let slot = match kind {
                WindowKind::Popup => Some(&mut self.popup),
                WindowKind::Settings => self.settings.as_mut(),
            };
            if let Some(slot) = slot {
                if let Some(host) = slot.host.as_mut() {
                    if close {
                        host.info.events.push(ViewportEvent::Close);
                    }
                    if matches!(event, WindowEvent::RedrawRequested) && host.awaiting_frame {
                        host.awaiting_frame = false;
                        slot.deadline.at(now);
                    }
                    if let WindowEvent::Focused(focused) = event {
                        if focused {
                            host.focus = Focus::Focused;
                            slot.deadline.at(now);
                        } else if host.focus == Focus::Focused {
                            host.focus = Focus::Lost { paints: 0 };
                        }
                    }
                    let start = host.state.egui_input_mut().events.len();
                    if preserve_super {
                        slot.native_input.observe(&event, slot.ctx.os().is_mac());
                    }
                    let response = host.state.on_window_event(&host.window, &event);
                    if preserve_super {
                        let input = host.state.egui_input_mut();
                        let override_key = app.and_then(|app| {
                            let WindowEvent::KeyboardInput {
                                event,
                                is_synthetic: false,
                                ..
                            } = &event
                            else {
                                return None;
                            };
                            slot.native_input.clipboard_override(
                                app,
                                kind,
                                &event.logical_key,
                                event.state.is_pressed(),
                                &input.events[start..],
                            )
                        });
                        slot.native_input.finish(input, start, override_key);
                    }
                    if response.repaint || close {
                        slot.deadline.at(now);
                    }
                }
            }
        }
        self.set_flow(event_loop);
    }

    /// Run tick on wait.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.tick(event_loop);
    }
}

#[cfg(test)]
mod zoom_tests {
    use super::*;

    #[test]
    fn fix_x6_both_window_contexts_disable_keyboard_zoom_at_creation() {
        for kind in [WindowKind::Popup, WindowKind::Settings] {
            let ctx = new_context();
            assert!(
                !ctx.options(|options| options.zoom_with_keyboard),
                "{kind:?}: keyboard zoom must be off before the first pass"
            );
        }
    }
}

#[cfg(test)]
mod native_input_tests {
    use super::*;
    use winit::keyboard::ModifiersState;

    fn key(pressed: bool, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key: egui::Key::J,
            physical_key: Some(egui::Key::J),
            pressed,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn native_input_patches_key_before_text_and_preserves_batched_snapshots() {
        for is_mac in [false, true] {
            let mut adapter = NativeInput::default();
            let mut input = RawInput::default();
            adapter.observe(
                &WindowEvent::ModifiersChanged(ModifiersState::SUPER.into()),
                is_mac,
            );
            input.events.extend([
                key(true, egui::Modifiers::NONE),
                egui::Event::Text("j".into()),
            ]);
            adapter.finish(&mut input, 0, None);
            adapter.observe(
                &WindowEvent::ModifiersChanged(ModifiersState::empty().into()),
                is_mac,
            );
            let start = input.events.len();
            input.events.push(key(false, egui::Modifiers::NONE));
            adapter.finish(&mut input, start, None);
            assert_eq!(input.events.len(), 2, "shortcut text must not reach search");
            assert!(
                matches!(input.events[0], egui::Event::Key { pressed:true, modifiers, .. } if modifiers.mac_cmd && modifiers.command == is_mac && !modifiers.ctrl)
            );
            assert!(
                matches!(input.events[1], egui::Event::Key { pressed:false, modifiers, .. } if modifiers == egui::Modifiers::NONE)
            );
        }
    }

    fn shortcut_app() -> (crate::app::PopupApp, Context) {
        let (app, ctx, _, _) = shortcut_app_with_keys(
            &serde_json::json!({"navigate_down":"super+j", "end":"ctrl+c", "home":"ctrl+x", "page_down":"ctrl+v"}),
            false,
        );
        (app, ctx)
    }

    fn shortcut_app_with_keys(
        keys: &serde_json::Value,
        permanent: bool,
    ) -> (
        crate::app::PopupApp,
        Context,
        std::sync::mpsc::Receiver<crate::ipc_bridge::UiCmd>,
        std::sync::mpsc::Sender<crate::ipc_bridge::UiEvent>,
    ) {
        use crate::app::{AppParts, PopupApp};
        use crate::ipc_bridge::{UiCmd, UiEvent};
        use serde_json::json;
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<UiCmd>();
        let (event_tx, event_rx) = std::sync::mpsc::channel();
        let mut app = PopupApp::new(AppParts {
            profile: std::path::PathBuf::from("native-input-fixture"),
            cmds: cmd_tx,
            events: event_rx,
            tray: Box::new(|_| Vec::new()),
            hold: None,
            open: true,
            status_period: Duration::from_secs(3600),
            trace: None,
            allow_foreground: Box::new(|| {}),
        });
        event_tx
            .send(UiEvent::Config(json!({"config":{"popup":{"keys":keys}}})))
            .expect("config");
        app.vm_mut().set_clips((1..=3).map(|n| serde_json::from_value(json!({
            "id":uuid::Uuid::from_u128(n), "created_ms":1, "source_os":"linux", "preview":"row", "pinned":false,
            "rev":{"wall_ms":1,"dev":"fixture"}
        })).expect("clip")).collect());
        if permanent {
            event_tx
                .send(UiEvent::PermGroups(
                    json!({"groups":[{"name":"Work","system":false,"count":3}]}),
                ))
                .expect("groups");
            event_tx.send(UiEvent::Permanents(json!({"permanents":(1..=3).map(|n| json!({
                "id":uuid::Uuid::from_u128(1000+n), "group":"Work", "name":format!("row {n}"), "kind":"Paste", "body":"row body", "hotkey":null, "js":null, "rev":{"wall_ms":1,"dev":"fixture","ctr":0}
            })).collect::<Vec<_>>()}))).expect("permanents");
        }
        let ctx = new_context();
        frame(&mut app, &ctx, RawInput::default());
        if permanent {
            let _ = cmd_rx.try_iter().collect::<Vec<_>>();
        } else {
            answer_history(&mut app, &ctx, &cmd_rx, &event_tx, "");
        }
        if permanent {
            app.config().lock().expect("config").perm_source = Some(0);
            app.vm_mut().selected = 2;
            frame(&mut app, &ctx, RawInput::default());
        }
        (app, ctx, cmd_rx, event_tx)
    }

    fn answer_history(
        app: &mut crate::app::PopupApp,
        ctx: &Context,
        commands: &std::sync::mpsc::Receiver<crate::ipc_bridge::UiCmd>,
        events: &std::sync::mpsc::Sender<crate::ipc_bridge::UiEvent>,
        query: &str,
    ) {
        use crate::ipc_bridge::{UiCmd, UiEvent};
        let mut found = false;
        for cmd in commands.try_iter() {
            match cmd {
                UiCmd::CompactHistory(tag) => {
                    assert_eq!(tag.params.query, query);
                    let rows: Vec<_> = if query.is_empty() {
                        (1..=3).map(|n| serde_json::json!({
                            "id": uuid::Uuid::from_u128(n), "pinned": false,
                            "last_used_ms": 1, "preview": format!("row {n}"),
                            "preview_truncated": false, "kind": "text", "thumbnail_hash": null
                        })).collect()
                    } else {
                        Vec::new()
                    };
                    events.send(UiEvent::CompactHistory(tag.clone(), Ok(serde_json::json!({
                        "owner": tag.params.owner, "generation": tag.params.generation,
                        "rows": rows, "cursor": null, "done": true, "bytes_read": 0, "candidates": 3
                    })))).unwrap();
                    found = true;
                }
                UiCmd::CompactCurrent(tag) => events
                    .send(UiEvent::CompactCurrent(
                        tag,
                        Ok(serde_json::json!({"clips": []})),
                    ))
                    .unwrap(),
                _ => {}
            }
        }
        assert!(found, "expected compact request for {query:?}");
        frame(app, ctx, RawInput::default());
    }

    fn clear_search(app: &mut crate::app::PopupApp, ctx: &Context) {
        let modifiers = egui::Modifiers {
            ctrl: true,
            command: true,
            ..egui::Modifiers::NONE
        };
        let events = [egui::Key::A, egui::Key::Backspace]
            .into_iter()
            .map(|key| egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers: if key == egui::Key::A {
                    modifiers
                } else {
                    egui::Modifiers::NONE
                },
            })
            .collect();
        frame(
            app,
            ctx,
            RawInput {
                events,
                ..RawInput::default()
            },
        );
        assert_eq!(app.vm().query, "");
    }

    fn frame(app: &mut crate::app::PopupApp, ctx: &Context, input: RawInput) {
        let mut output = ctx.run_ui(input, |ui| {
            app.logic(ui.ctx(), WindowKind::Popup);
            app.ui(ui, WindowKind::Popup);
        });
        output.textures_delta.clear();
    }

    #[test]
    fn native_input_super_batch_dispatches_once_without_search_text() {
        let (mut app, ctx) = shortcut_app();
        let mut adapter = NativeInput::default();
        adapter.observe(
            &WindowEvent::ModifiersChanged(ModifiersState::SUPER.into()),
            false,
        );
        let mut input = RawInput {
            events: vec![
                key(true, egui::Modifiers::NONE),
                egui::Event::Text("j".into()),
            ],
            ..RawInput::default()
        };
        adapter.finish(&mut input, 0, None);
        adapter.observe(
            &WindowEvent::ModifiersChanged(ModifiersState::empty().into()),
            false,
        );
        let start = input.events.len();
        input.events.extend([
            key(false, egui::Modifiers::NONE),
            egui::Event::ModifiersChanged(egui::Modifiers::NONE),
        ]);
        adapter.finish(&mut input, start, None);
        frame(&mut app, &ctx, input);
        assert_eq!(
            app.vm().selected,
            1,
            "one native shortcut must dispatch exactly once"
        );
        assert!(
            app.vm().query.is_empty(),
            "shortcut must not fall back to search text"
        );
    }

    #[test]
    fn native_input_clipboard_overrides_preserve_text_editing() {
        let (mut app, ctx, commands, events) = shortcut_app_with_keys(
            &serde_json::json!({"navigate_down":"super+j", "end":"ctrl+c", "home":"ctrl+x", "page_down":"ctrl+v"}),
            false,
        );
        let mut adapter = NativeInput::default();
        adapter.observe(
            &WindowEvent::ModifiersChanged(ModifiersState::CONTROL.into()),
            false,
        );
        // Clipboard translation remains text editing when the search field owns focus.
        let native_v = winit::keyboard::Key::Character("v".into());
        assert!(ctx.text_edit_focused());
        assert_eq!(
            adapter.clipboard_override(&app, WindowKind::Popup, &native_v, true, &[]),
            Some(egui::Key::V)
        );
        let mut paste = RawInput {
            events: vec![egui::Event::Paste("needle".into())],
            ..RawInput::default()
        };
        adapter.finish(&mut paste, 0, Some(egui::Key::V));
        frame(&mut app, &ctx, paste);
        assert_eq!(app.vm().query, "needle");
        answer_history(&mut app, &ctx, &commands, &events, "needle");
        clear_search(&mut app, &ctx);
        answer_history(&mut app, &ctx, &commands, &events, "");
        ctx.memory_mut(|memory| {
            if let Some(id) = memory.focused() {
                memory.surrender_focus(id);
            }
        });
        frame(&mut app, &ctx, RawInput::default());
        assert!(!ctx.text_edit_focused());
        for (name, translated, selected) in [
            ("c", egui::Event::Copy, 2),
            ("x", egui::Event::Cut, 0),
            ("v", egui::Event::Paste("must not enter search".into()), 2),
        ] {
            let native = winit::keyboard::Key::Character(name.into());
            let override_key =
                adapter.clipboard_override(&app, WindowKind::Popup, &native, true, &[]);
            assert!(
                override_key.is_some(),
                "configured {name} must survive native clipboard translation"
            );
            let mut input = RawInput {
                events: vec![translated],
                ..RawInput::default()
            };
            adapter.finish(&mut input, 0, override_key);
            assert_eq!(
                input.events.len(),
                2,
                "preserve clipboard input until actual frame focus is known"
            );
            frame(&mut app, &ctx, input);
            assert_eq!(app.vm().selected, selected);
            assert_eq!(app.vm().query, "");
        }
    }

    fn second_history_row(ctx: &Context, search: egui::Rect) -> egui::Rect {
        let mut rows = ctx
            .interactive_rects_last_pass()
            .into_iter()
            .filter(|rect| rect.top() > search.bottom() && rect.height() <= 25.0)
            .collect::<Vec<_>>();
        rows.sort_by(|a, b| a.top().total_cmp(&b.top()));
        // A text row's clickable scope and its label share one rect.
        rows.dedup();
        // The three distinct fixture labels have identical geometry and regular
        // vertical spacing. Tabs and pagination controls must not be mistaken
        // for a history row just because they are below the search box.
        let groups: Vec<_> = rows
            .windows(3)
            .filter(|group| {
                let [a, b, c] = group else { unreachable!() };
                (a.left() - b.left()).abs() < 0.1
                    && (b.left() - c.left()).abs() < 0.1
                    && a.size() == b.size()
                    && b.size() == c.size()
                    && (b.top() - a.top() - (c.top() - b.top())).abs() < 0.1
            })
            .collect();
        assert_eq!(
            groups.len(),
            1,
            "expected the three rendered history labels: {rows:?}"
        );
        groups[0][1]
    }

    #[test]
    fn native_input_clipboard_focus_changes_in_same_batch() {
        let (mut app, ctx, commands, events) = shortcut_app_with_keys(
            &serde_json::json!({"navigate_down":"super+j", "end":"ctrl+c", "home":"ctrl+x", "page_down":"ctrl+v"}),
            false,
        );
        let search_id = ctx
            .memory(egui::Memory::focused)
            .expect("search owns focus");
        let search = ctx
            .read_response(search_id)
            .expect("actual search widget")
            .rect;
        ctx.memory_mut(|memory| memory.surrender_focus(search_id));
        frame(&mut app, &ctx, RawInput::default());
        assert!(!ctx.text_edit_focused());
        let mut adapter = NativeInput::default();
        adapter.observe(
            &WindowEvent::ModifiersChanged(ModifiersState::CONTROL.into()),
            false,
        );
        let click = |pos| {
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        };
        let native_v = winit::keyboard::Key::Character("v".into());
        let override_key =
            adapter.clipboard_override(&app, WindowKind::Popup, &native_v, true, &[]);
        let mut input = RawInput {
            events: click(search.center()),
            ..RawInput::default()
        };
        let start = input.events.len();
        input.events.push(egui::Event::Paste("needle".into()));
        adapter.finish(&mut input, start, override_key);
        frame(&mut app, &ctx, input);
        assert!(
            ctx.text_edit_focused(),
            "actual search click at {search:?} must focus text editor"
        );
        assert_eq!(
            app.vm().query,
            "needle",
            "same-batch search click must retain native paste"
        );
        assert_eq!(
            app.vm().selected,
            0,
            "paste into search must not also navigate the list"
        );
        answer_history(&mut app, &ctx, &commands, &events, "needle");
        clear_search(&mut app, &ctx);
        answer_history(&mut app, &ctx, &commands, &events, "");
        frame(&mut app, &ctx, RawInput::default());
        assert!(ctx.text_edit_focused());
        let row = second_history_row(&ctx, search);
        let native_c = winit::keyboard::Key::Character("c".into());
        let override_key =
            adapter.clipboard_override(&app, WindowKind::Popup, &native_c, true, &[]);
        let mut input = RawInput {
            events: click(row.center()),
            ..RawInput::default()
        };
        let start = input.events.len();
        input.events.push(egui::Event::Copy);
        adapter.finish(&mut input, start, override_key);
        frame(&mut app, &ctx, input);
        assert!(!ctx.text_edit_focused());
        assert_eq!(
            app.vm().selected,
            2,
            "same-batch list click must honor configured copy chord"
        );
        assert_eq!(app.vm().query, "");
    }

    fn alt_shortcut_control(permanent: bool, fallback: bool) {
        use crate::ipc_bridge::UiCmd;

        let (chord, text, effective) = if fallback {
            ("alt+o", "ø", egui::Key::O)
        } else {
            ("alt+j", "j", egui::Key::J)
        };
        let (mut app, ctx, commands, _) =
            shortcut_app_with_keys(&serde_json::json!({"navigate_down":chord}), permanent);
        assert!(ctx.text_edit_focused());
        let mut adapter = NativeInput::default();
        adapter.observe(
            &WindowEvent::ModifiersChanged(ModifiersState::ALT.into()),
            fallback,
        );
        let native = winit::keyboard::Key::Character(text.into());
        let mut input = RawInput {
            // Pinned egui-winit emits logical.or(physical), then printable Text.
            events: vec![
                egui::Event::Key {
                    key: effective,
                    physical_key: Some(effective),
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::ALT,
                },
                egui::Event::Text(text.into()),
            ],
            ..RawInput::default()
        };
        let owned =
            adapter.clipboard_override(&app, WindowKind::Popup, &native, true, &input.events);
        adapter.finish(&mut input, 0, owned);
        adapter.observe(
            &WindowEvent::ModifiersChanged(ModifiersState::empty().into()),
            fallback,
        );
        let start = input.events.len();
        input.events.extend([
            egui::Event::Key {
                key: effective,
                physical_key: Some(effective),
                pressed: false,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::ModifiersChanged(egui::Modifiers::NONE),
        ]);
        adapter.finish(&mut input, start, None);
        frame(&mut app, &ctx, input);
        assert_eq!(
            app.vm().query,
            "",
            "configured {chord} must not type its native {text} text"
        );
        if permanent {
            assert_eq!(app.config().lock().expect("config").perm_vm.selected, 1);
            assert_eq!(
                app.vm().selected,
                2,
                "permanent navigation must preserve hidden history"
            );
            assert_eq!(app.vm().clips().len(), 3);
        } else {
            assert_eq!(app.vm().selected, 1, "one key press must navigate once");
        }
        frame(
            &mut app,
            &ctx,
            RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: Some(egui::Key::Enter),
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..RawInput::default()
            },
        );
        assert!(!app.desired().popup_open);
        app.logic(&ctx, WindowKind::Popup);
        std::thread::sleep(Duration::from_millis(170));
        app.logic(&ctx, WindowKind::Popup);
        let picked = commands.try_iter().collect::<Vec<_>>();
        if permanent {
            assert_eq!(
                picked,
                vec![UiCmd::ExecPermanent(uuid::Uuid::from_u128(1002))]
            );
        } else {
            assert!(
                matches!(picked.as_slice(),[UiCmd::PasteClipTo(id,..)] if *id == uuid::Uuid::from_u128(2)),
                "{picked:?}"
            );
        }
    }

    #[test]
    fn native_input_alt_shortcut_history_pick_uses_unchanged_query() {
        alt_shortcut_control(false, false);
    }

    #[test]
    fn native_input_alt_shortcut_permanent_pick_preserves_hidden_history() {
        alt_shortcut_control(true, false);
    }

    #[test]
    fn native_input_alt_physical_fallback_history_pick_uses_unchanged_query() {
        alt_shortcut_control(false, true);
    }

    #[test]
    fn native_input_alt_physical_fallback_permanent_pick_preserves_hidden_history() {
        alt_shortcut_control(true, true);
    }

    #[test]
    fn native_input_alt_ownership_uses_logical_priority_over_physical() {
        for (chord, expected_query, expected_selected) in [("alt+o", "j", 0), ("alt+j", "", 1)] {
            let (mut app, ctx, _, _) =
                shortcut_app_with_keys(&serde_json::json!({"navigate_down":chord}), false);
            let mut adapter = NativeInput::default();
            adapter.observe(
                &WindowEvent::ModifiersChanged(ModifiersState::ALT.into()),
                false,
            );
            // A recognized logical J wins over physical O in pinned egui-winit.
            let mut input = RawInput {
                events: vec![
                    egui::Event::Key {
                        key: egui::Key::J,
                        physical_key: Some(egui::Key::O),
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::ALT,
                    },
                    egui::Event::Text("j".into()),
                ],
                ..RawInput::default()
            };
            let owned = adapter.clipboard_override(
                &app,
                WindowKind::Popup,
                &winit::keyboard::Key::Character("j".into()),
                true,
                &input.events,
            );
            adapter.finish(&mut input, 0, owned);
            frame(&mut app, &ctx, input);
            assert_eq!(app.vm().query, expected_query, "{chord}");
            assert_eq!(app.vm().selected, expected_selected, "{chord}");
        }
    }

    #[test]
    fn native_input_alt_shortcut_preserves_other_native_text_in_batch() {
        let (mut app, ctx, _, _) =
            shortcut_app_with_keys(&serde_json::json!({"navigate_down":"alt+o"}), false);
        let mut adapter = NativeInput::default();
        adapter.observe(
            &WindowEvent::ModifiersChanged(ModifiersState::ALT.into()),
            false,
        );
        let mut input = RawInput {
            events: vec![
                egui::Event::Text("before".into()),
                egui::Event::Key {
                    key: egui::Key::O,
                    physical_key: Some(egui::Key::O),
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::ALT,
                },
                egui::Event::Text("ø".into()),
            ],
            ..RawInput::default()
        };
        let owned = adapter.clipboard_override(
            &app,
            WindowKind::Popup,
            &winit::keyboard::Key::Character("ø".into()),
            true,
            &input.events[1..],
        );
        adapter.finish(&mut input, 1, owned);
        let start = input.events.len();
        input.events.push(egui::Event::Text("after".into()));
        adapter.finish(&mut input, start, None);
        frame(&mut app, &ctx, input);
        assert_eq!(
            app.vm().query,
            "beforeafter",
            "only the owned native key's appended text may be suppressed"
        );
    }

    #[test]
    fn native_input_unmatched_alt_and_option_text_remains_editable() {
        for (is_mac, text) in [(false, "j"), (true, "ø")] {
            let (mut app, ctx) = shortcut_app();
            let mut adapter = NativeInput::default();
            adapter.observe(
                &WindowEvent::ModifiersChanged(ModifiersState::ALT.into()),
                is_mac,
            );
            let native = winit::keyboard::Key::Character(text.into());
            let effective = if is_mac { egui::Key::O } else { egui::Key::J };
            let translated_key = egui::Event::Key {
                key: effective,
                physical_key: Some(effective),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::ALT,
            };
            let owned = adapter.clipboard_override(
                &app,
                WindowKind::Popup,
                &native,
                true,
                std::slice::from_ref(&translated_key),
            );
            assert_eq!(owned, None, "unbound Alt/Option is ordinary text input");
            let mut input = RawInput {
                events: vec![translated_key, egui::Event::Text(text.into())],
                ..RawInput::default()
            };
            adapter.finish(&mut input, 0, owned);
            frame(&mut app, &ctx, input);
            assert_eq!(app.vm().query, text);
            assert!(app.desired().popup_open);
        }
    }

    #[test]
    fn native_input_ctrl_focus_and_host_reset_do_not_invent_super() {
        for is_mac in [false, true] {
            let mut adapter = NativeInput::default();
            adapter.observe(
                &WindowEvent::ModifiersChanged(ModifiersState::CONTROL.into()),
                is_mac,
            );
            let mut input = RawInput {
                events: vec![key(true, egui::Modifiers::NONE)],
                ..RawInput::default()
            };
            adapter.finish(&mut input, 0, None);
            assert!(
                matches!(input.events[0], egui::Event::Key { modifiers, .. } if modifiers.ctrl && !modifiers.mac_cmd && modifiers.command != is_mac)
            );
            for event in [WindowEvent::Focused(false), WindowEvent::Destroyed] {
                adapter.observe(
                    &WindowEvent::ModifiersChanged(
                        (ModifiersState::SUPER | ModifiersState::ALT | ModifiersState::SHIFT)
                            .into(),
                    ),
                    is_mac,
                );
                adapter.observe(&event, is_mac);
                assert_eq!(adapter.modifiers, egui::Modifiers::NONE);
            }
            adapter.observe(
                &WindowEvent::ModifiersChanged(ModifiersState::SUPER.into()),
                is_mac,
            );
            adapter.reset();
            assert_eq!(adapter.modifiers, egui::Modifiers::NONE);
        }
    }
}
