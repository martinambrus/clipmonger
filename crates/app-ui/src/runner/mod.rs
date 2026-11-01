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

/// One egui context with its rasterizer, deadline and, while it exists, its OS window.
struct Slot {
    kind: WindowKind,
    ctx: Context,
    rasterizer: render::Rasterizer,
    pending_raw: RawInput,
    deadline: sched::Deadline,
    host: Option<Host>,
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
                self.popup.host = Some(host);
            } else {
                self.popup.host = None;
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
                    slot.host = Some(host);
                }
            }
        } else if let Some(slot) = self.settings.as_mut() {
            slot.host = None;
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
            if let Some(slot) = self.slot_mut(kind) {
                if let Some(host) = slot.host.as_mut() {
                    if close {
                        host.info.events.push(ViewportEvent::Close);
                    }
                    let response = host.state.on_window_event(&host.window, &event);
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
