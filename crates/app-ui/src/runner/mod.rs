//! The runner: our own event loop for egui on winit with a CPU rasterizer.
//! A popup window and an optional settings window, each with its own egui
//! context. App logic runs on every pass, visible or hidden; the ui runs only
//! while the window is shown.

pub mod render;
pub mod sched;
pub mod window;

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{
    ClippedPrimitive, Color32, Context, RawInput, ViewportBuilder, ViewportEvent, ViewportId,
    ViewportInfo,
};
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

    /// Whether a configured list action overrides a native clipboard shortcut.
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
}

/// Window sizes and titles. Sizes are in points at zoom 1.
#[derive(Clone, Debug)]
pub struct Config {
    pub popup_title: String,
    pub popup_size: [f32; 2],
    pub popup_position: Option<[f32; 2]>,
    pub settings_title: String,
    pub settings_size: [f32; 2],
}

/// What the app factory receives at start.
pub struct Startup {
    /// Wakes the loop from any thread.
    pub handle: Handle,
    /// The popup's Win32 handle, read on the loop thread (see
    /// [`window::hwnd`]); `None` off Windows and on Wayland, where the popup
    /// is created per show.
    pub popup_hwnd: Option<u64>,
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
    Surface(softbuffer::SoftBufferError),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EventLoop(e) => write!(f, "event loop: {e}"),
            Self::Window(e) => write!(f, "window: {e}"),
            Self::Surface(e) => write!(f, "surface: {e}"),
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
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .map_err(RunError::EventLoop)?;
    let proxy = event_loop.create_proxy();
    let mut runner = Runner::new(config, proxy, Box::new(make_app));
    event_loop
        .run_app(&mut runner)
        .map_err(RunError::EventLoop)?;
    runner.error.map_or(Ok(()), Err)
}

/// The OS window of a slot and everything bound to it.
struct Host {
    window: Arc<Window>,
    state: egui_winit::State,
    presenter: render::Presenter,
    present_error_logged: bool,
    info: ViewportInfo,
    visibility: Visibility,
}

impl Host {
    #[allow(clippy::cast_possible_truncation)]
    fn new(
        event_loop: &ActiveEventLoop,
        surface_context: &softbuffer::Context<OwnedDisplayHandle>,
        ctx: &Context,
        builder: &ViewportBuilder,
        class: &str,
        visibility: Visibility,
    ) -> Result<Self, RunError> {
        let attrs = window::attributes(ctx, builder.clone(), class);
        let window = event_loop.create_window(attrs).map_err(RunError::Window)?;
        egui_winit::apply_viewport_builder_to_window(ctx, &window, builder);
        let window = Arc::new(window);
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
        Ok(Self {
            window,
            state,
            presenter,
            present_error_logged: false,
            info,
            visibility,
        })
    }
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
        if let Err(error) =
            self.presenter
                .present(rasterizer, size, pixels_per_point, clear, primitives)
        {
            if !self.present_error_logged {
                self.present_error_logged = true;
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
    ) -> Option<egui::Key> {
        self.clipboard_key(key, pressed)
            .filter(|key| app.native_key_override(kind, *key, self.modifiers))
    }

    fn clipboard_key(&self, key: &winit::keyboard::Key, pressed: bool) -> Option<egui::Key> {
        if !pressed || !self.modifiers.command_only() {
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
}

impl<A: App> Runner<A> {
    fn new(
        config: Config,
        proxy: EventLoopProxy<UserEvent>,
        make_app: Box<dyn FnOnce(Startup) -> A>,
    ) -> Self {
        let fonts = Arc::new(egui_system_font_provider::SystemFontProvider::new());
        let popup = Slot::new(WindowKind::Popup, &proxy, &fonts);
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
        }
    }

    fn slot_mut(&mut self, kind: WindowKind) -> Option<&mut Slot> {
        match kind {
            WindowKind::Popup => Some(&mut self.popup),
            WindowKind::Settings => self.settings.as_mut(),
        }
    }

    /// Remember the error and leave the event loop.
    fn fail(&mut self, event_loop: &ActiveEventLoop, error: RunError) {
        self.error = Some(error);
        event_loop.exit();
    }

    /// Create the OS window for a slot.
    fn create_host(
        &self,
        event_loop: &ActiveEventLoop,
        kind: WindowKind,
        visible: bool,
    ) -> Result<Host, RunError> {
        let builder = match kind {
            WindowKind::Popup => window::popup_builder(
                &self.config.popup_title,
                self.config.popup_size,
                self.config.popup_position,
            )
            .with_visible(visible),
            WindowKind::Settings => {
                window::settings_builder(&self.config.settings_title, self.config.settings_size)
            }
        };
        let class = match kind {
            WindowKind::Popup => window::POPUP_CLASS,
            WindowKind::Settings => window::SETTINGS_CLASS,
        };
        let visibility = if visible {
            Visibility::shown()
        } else {
            Visibility::hidden()
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
        Host::new(
            event_loop,
            surface_context,
            ctx,
            &builder,
            class,
            visibility,
        )
    }

    /// Initialize the runner on first resume.
    fn start(&mut self, event_loop: &ActiveEventLoop) -> Result<(), RunError> {
        let surface_context = softbuffer::Context::new(event_loop.owned_display_handle())
            .map_err(RunError::Surface)?;
        self.surface_context = Some(surface_context);
        self.per_show = window::is_wayland(event_loop);
        let mut popup_hwnd = None;
        if !self.per_show {
            let host = self.create_host(event_loop, WindowKind::Popup, false)?;
            popup_hwnd = window::hwnd(&host.window);
            self.popup.native_input.reset();
            self.popup.host = Some(host);
        }
        let make_app = self.make_app.take().expect("make_app already taken");
        let handle = Handle {
            proxy: self.proxy.clone(),
        };
        let app = make_app(Startup { handle, popup_hwnd });
        self.app = Some(app);
        self.popup.deadline.at(Instant::now());
        Ok(())
    }

    /// Update the event loop control flow from deadlines.
    fn set_flow(&self, event_loop: &ActiveEventLoop) {
        let deadlines = [
            self.popup.deadline.next(),
            self.settings.as_ref().and_then(|s| s.deadline.next()),
        ];
        event_loop.set_control_flow(sched::control_flow(deadlines));
    }

    /// Run due passes and reconcile.
    fn tick(&mut self, event_loop: &ActiveEventLoop) {
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
        let Some(app) = self.app.as_mut() else {
            return;
        };
        let desired = app.desired();
        let request = app.take_popup_activation();
        let previous = std::mem::replace(&mut self.last, desired);
        if desired.exit {
            event_loop.exit();
            return;
        }
        let now = Instant::now();
        if desired.popup_open != previous.popup_open {
            if let Err(e) = self.set_popup_open(event_loop, desired.popup_open) {
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
            self.pending_activation.reconcile(
                request,
                previous.popup_open,
                desired.popup_open,
                Some(&*host.window),
                policy,
            );
        } else {
            self.pending_activation.reconcile(
                request,
                previous.popup_open,
                desired.popup_open,
                None::<&winit::window::Window>,
                policy,
            );
        }
    }

    /// Show or hide the popup window.
    fn set_popup_open(&mut self, event_loop: &ActiveEventLoop, open: bool) -> Result<(), RunError> {
        if self.per_show {
            if open {
                let host = self.create_host(event_loop, WindowKind::Popup, true)?;
                self.popup.native_input.reset();
                self.popup.host = Some(host);
            } else {
                self.popup.host = None;
                self.popup.native_input.reset();
            }
        } else if let Some(host) = self.popup.host.as_mut() {
            if open {
                host.visibility.show(&*host.window);
            } else {
                host.visibility.hide(&*host.window);
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
                let host = self.create_host(event_loop, WindowKind::Settings, true)?;
                if let Some(slot) = self.settings.as_mut() {
                    slot.native_input.reset();
                    slot.host = Some(host);
                }
            }
        } else if let Some(slot) = self.settings.as_mut() {
            slot.host = None;
            slot.native_input.reset();
            slot.deadline.clear();
        }
        Ok(())
    }
}

impl<A: App> ApplicationHandler<UserEvent> for Runner<A> {
    /// Handle resumed event.
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.app.is_some() {
            return;
        }
        if let Err(e) = self.start(event_loop) {
            self.fail(event_loop, e);
        }
    }

    /// Handle wake and repaint events.
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        let now = Instant::now();
        match event {
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

    /// Handle window events.
    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let now = Instant::now();
        let close = matches!(event, WindowEvent::CloseRequested);
        let kind = if let Some(host) = self.popup.host.as_ref() {
            if host.window.id() == window_id {
                Some(WindowKind::Popup)
            } else {
                None
            }
        } else {
            None
        };
        let kind = kind.or_else(|| {
            if let Some(slot) = self.settings.as_ref() {
                if let Some(host) = slot.host.as_ref() {
                    if host.window.id() == window_id {
                        return Some(WindowKind::Settings);
                    }
                }
            }
            None
        });
        if let Some(kind) = kind {
            let preserve_super = self
                .app
                .as_ref()
                .is_some_and(|app| app.preserve_native_super(kind));
            let slot = match kind {
                WindowKind::Popup => Some(&self.popup),
                WindowKind::Settings => self.settings.as_ref(),
            };
            let override_key = slot.and_then(|slot| {
                let WindowEvent::KeyboardInput {
                    event,
                    is_synthetic: false,
                    ..
                } = &event
                else {
                    return None;
                };
                slot.native_input.clipboard_override(
                    self.app.as_ref()?,
                    kind,
                    &event.logical_key,
                    event.state.is_pressed(),
                )
            });
            if let Some(slot) = self.slot_mut(kind) {
                if let Some(host) = slot.host.as_mut() {
                    if close {
                        host.info.events.push(ViewportEvent::Close);
                    }
                    let start = host.state.egui_input_mut().events.len();
                    if preserve_super {
                        slot.native_input.observe(&event, slot.ctx.os().is_mac());
                    }
                    let response = host.state.on_window_event(&host.window, &event);
                    if preserve_super {
                        slot.native_input
                            .finish(host.state.egui_input_mut(), start, override_key);
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
        use crate::app::{AppParts, PopupApp};
        use crate::ipc_bridge::{UiCmd, UiEvent};
        use serde_json::json;
        let (cmd_tx, _cmd_rx) = std::sync::mpsc::channel::<UiCmd>();
        let (event_tx, event_rx) = std::sync::mpsc::channel();
        let mut app = PopupApp::new(AppParts {
            profile: std::path::PathBuf::from("native-input-fixture"),
            cmds: cmd_tx,
            events: event_rx,
            tray: Box::new(Vec::new),
            hold: None,
            open: true,
            status_period: Duration::from_secs(3600),
            trace: None,
            allow_foreground: Box::new(|| {}),
        });
        event_tx
            .send(UiEvent::Config(
                json!({"config":{"popup":{"keys":{"navigate_down":"super+j", "end":"ctrl+c", "home":"ctrl+x", "page_down":"ctrl+v"}}}}),
            ))
            .expect("config");
        app.vm_mut().set_clips((1..=3).map(|n| serde_json::from_value(json!({
            "id":uuid::Uuid::from_u128(n), "created_ms":1, "source_os":"linux", "preview":"row", "pinned":false,
            "rev":{"wall_ms":1,"dev":"fixture"}
        })).expect("clip")).collect());
        let ctx = new_context();
        frame(&mut app, &ctx, RawInput::default());
        (app, ctx)
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
        let (mut app, ctx) = shortcut_app();
        let mut adapter = NativeInput::default();
        adapter.observe(
            &WindowEvent::ModifiersChanged(ModifiersState::CONTROL.into()),
            false,
        );
        // Clipboard translation remains text editing when the search field owns focus.
        let native_v = winit::keyboard::Key::Character("v".into());
        assert!(ctx.text_edit_focused());
        assert_eq!(
            adapter.clipboard_override(&app, WindowKind::Popup, &native_v, true),
            Some(egui::Key::V)
        );
        let mut paste = RawInput {
            events: vec![egui::Event::Paste("needle".into())],
            ..RawInput::default()
        };
        adapter.finish(&mut paste, 0, Some(egui::Key::V));
        frame(&mut app, &ctx, paste);
        assert_eq!(app.vm().query, "needle");
        app.vm_mut().query.clear();
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
            let override_key = adapter.clipboard_override(&app, WindowKind::Popup, &native, true);
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

    #[test]
    fn native_input_clipboard_focus_changes_in_same_batch() {
        let (mut app, ctx) = shortcut_app();
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
        let override_key = adapter.clipboard_override(&app, WindowKind::Popup, &native_v, true);
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
        app.vm_mut().query.clear();
        frame(&mut app, &ctx, RawInput::default());
        assert!(ctx.text_edit_focused());
        let mut rows = ctx
            .interactive_rects_last_pass()
            .into_iter()
            .filter(|rect| rect.top() > search.bottom() && rect.height() <= 25.0)
            .collect::<Vec<_>>();
        rows.sort_by(|a, b| a.top().total_cmp(&b.top()));
        let row = rows.get(1).expect("actual second history row");
        let native_c = winit::keyboard::Key::Character("c".into());
        let override_key = adapter.clipboard_override(&app, WindowKind::Popup, &native_c, true);
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
