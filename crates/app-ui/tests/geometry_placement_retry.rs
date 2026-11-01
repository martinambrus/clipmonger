//! Headless controls for the production native attributes and lifecycle seams.
//! They do not create OS windows or establish compositor behavior.

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use clipmonger_app_ui::placement::Rect;
use clipmonger_app_ui::runner::geometry::{
    self, GeometryState, GeometryWindow, Lifecycle, MonitorGeometry,
};
use clipmonger_app_ui::runner::window::{prepare_host, settings_builder, SETTINGS_CLASS};
use winit::dpi::{PhysicalPosition, PhysicalSize, Position};

fn fixture() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "cm-geometry-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&path).unwrap();
    path.join("state.json")
}

struct Native {
    size: PhysicalSize<u32>,
    scale: f64,
    position: [i32; 2],
    minimized: bool,
    maximized: bool,
    fullscreen: bool,
    position_queries: Cell<usize>,
}

impl Native {
    fn normal() -> Self {
        Self {
            size: PhysicalSize::new(1440, 1020),
            scale: 1.5,
            position: [-1800, -150],
            minimized: false,
            maximized: false,
            fullscreen: false,
            position_queries: Cell::new(0),
        }
    }
}
impl GeometryWindow for Native {
    fn inner_size(&self) -> PhysicalSize<u32> {
        self.size
    }
    fn outer_size(&self) -> PhysicalSize<u32> {
        PhysicalSize::new(self.size.width + 18, self.size.height + 45)
    }
    fn scale_factor(&self) -> f64 {
        self.scale
    }
    fn is_minimized(&self) -> Option<bool> {
        Some(self.minimized)
    }
    fn is_maximized(&self) -> bool {
        self.maximized
    }
    fn is_fullscreen(&self) -> bool {
        self.fullscreen
    }
    fn outer_position(&self) -> Option<PhysicalPosition<i32>> {
        self.position_queries.set(self.position_queries.get() + 1);
        Some(PhysicalPosition::new(self.position[0], self.position[1]))
    }
}

fn monitor() -> MonitorGeometry {
    MonitorGeometry {
        bounds: Rect {
            x: -1920,
            y: -200,
            w: 1920,
            h: 1080,
        },
        scale: 1.5,
    }
}

fn close(a: f64, b: f64) {
    assert!((a - b).abs() < 0.001, "{a} != {b}");
}

fn restored(
    state: &GeometryState,
    zoom: f32,
    global: bool,
) -> clipmonger_app_ui::runner::window::PreparedHost {
    restored_in_context(state, &egui::Context::default(), zoom, global)
}

fn restored_in_context(
    state: &GeometryState,
    ctx: &egui::Context,
    zoom: f32,
    global: bool,
) -> clipmonger_app_ui::runner::window::PreparedHost {
    ctx.set_zoom_factor(zoom);
    let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
    output.textures_delta.clear();
    close(f64::from(ctx.zoom_factor()), f64::from(zoom));
    let restore =
        state.normalized_for_restore([960.0, 680.0], &[monitor()], Some(monitor()), global);
    prepare_host(
        ctx,
        settings_builder("Settings", [960.0, 680.0]),
        SETTINGS_CLASS,
        Some(&restore),
    )
}

#[test]
fn native_attributes_survive_close_reopen_and_persisted_reload_at_dpi_and_font_zoom() {
    let path = fixture();
    let mut state = GeometryState::new(Some(path.clone()));
    let mut host = Some(Native::normal());
    state.handle(Lifecycle::Changed, host.as_ref(), true, |message| {
        panic!("{message}")
    });
    assert!(!path.exists(), "drag events must not write on every change");
    state.handle(Lifecycle::Close, host.as_ref(), true, |message| {
        panic!("{message}")
    });
    host.take(); // the runner destroys only the host, retaining GeometryState and Context.
    let surviving_context = egui::Context::default();
    for zoom in [0.8, 1.6, 1.6] {
        let prepared = restored_in_context(&state, &surviving_context, zoom, true);
        let size: PhysicalSize<u32> = prepared.attributes.inner_size.unwrap().to_physical(1.5);
        assert_eq!(size, PhysicalSize::new(1440, 1020));
        assert_eq!(
            prepared.attributes.position,
            Some(Position::Physical(PhysicalPosition::new(-1800, -150)))
        );
        assert!(
            prepared.after_creation.inner_size.is_none(),
            "egui must not reapply size with font zoom"
        );
        assert!(
            prepared.after_creation.position.is_none(),
            "egui must not rescale physical position"
        );
        host = Some(Native {
            size,
            ..Native::normal()
        });
        state.handle(Lifecycle::Close, host.as_ref(), true, |message| {
            panic!("{message}")
        });
        host.take();
    }
    let reloaded = GeometryState::new(Some(path));
    let prepared = restored(&reloaded, 1.6, true);
    assert_eq!(
        prepared
            .attributes
            .inner_size
            .unwrap()
            .to_physical::<u32>(1.5),
        PhysicalSize::new(1440, 1020)
    );
    close(
        f64::from(reloaded.load_for_restore().unwrap().logical_width),
        960.0,
    );
}

#[test]
fn quit_while_settings_open_captures_and_persists_current_normal_host() {
    let path = fixture();
    let mut state = GeometryState::new(Some(path.clone()));
    state.handle(Lifecycle::Exit, Some(&Native::normal()), true, |message| {
        panic!("{message}")
    });
    let stored = geometry::load(&path).unwrap();
    close(f64::from(stored.logical_height), 680.0);
    assert_eq!(
        (stored.physical_x, stored.physical_y),
        (Some(-1800), Some(-150))
    );
}

#[test]
fn event_close_and_quit_keep_last_normal_geometry_for_all_non_normal_states() {
    for lifecycle in [Lifecycle::Changed, Lifecycle::Close, Lifecycle::Exit] {
        for mode in 0..4 {
            let path = fixture();
            let mut state = GeometryState::new(Some(path.clone()));
            state.handle(Lifecycle::Close, Some(&Native::normal()), true, |message| {
                panic!("{message}")
            });
            let before = std::fs::read(&path).unwrap();
            let mut transient = Native {
                size: PhysicalSize::new(2880, 1620),
                position: [0, 0],
                ..Native::normal()
            };
            match mode {
                0 => transient.minimized = true,
                1 => transient.maximized = true,
                2 => transient.fullscreen = true,
                _ => transient.size.width = 0,
            }
            state.handle(lifecycle, Some(&transient), true, |message| {
                panic!("{message}")
            });
            assert_eq!(state.load_for_restore(), geometry::load(&path));
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert_eq!(transient.position_queries.get(), 0);
        }
    }
}

#[test]
fn wayland_size_only_capture_and_attributes_never_query_or_set_global_position() {
    let path = fixture();
    let mut state = GeometryState::new(Some(path.clone()));
    let native = Native::normal();
    state.handle(Lifecycle::Exit, Some(&native), false, |message| {
        panic!("{message}")
    });
    assert_eq!(
        native.position_queries.get(),
        0,
        "Wayland must never call a global position getter"
    );
    let stored = geometry::load(&path).unwrap();
    assert_eq!((stored.physical_x, stored.physical_y), (None, None));
    let prepared = restored(&state, 1.6, false);
    assert!(prepared.attributes.position.is_none());
    assert!(prepared.after_creation.position.is_none());
    assert_eq!(
        prepared
            .attributes
            .inner_size
            .unwrap()
            .to_physical::<u32>(1.5),
        PhysicalSize::new(1440, 1020)
    );
}

#[test]
fn size_only_and_removed_monitor_restore_clamp_physical_size_with_observed_decorations() {
    for global in [false, true] {
        let mut state = GeometryState::new(None);
        let native = Native {
            size: PhysicalSize::new(12000, 12000),
            position: [i32::MAX, i32::MIN],
            ..Native::normal()
        };
        state.handle(Lifecycle::Changed, Some(&native), global, |message| {
            panic!("{message}")
        });
        let prepared = restored(&state, 1.6, global);
        let physical = prepared
            .attributes
            .inner_size
            .unwrap()
            .to_physical::<u32>(1.5);
        assert_eq!(physical, PhysicalSize::new(1902, 1035)); // plus 18x45 decoration fits 1920x1080.
        if global {
            let Position::Physical(position) = prepared.attributes.position.unwrap() else {
                panic!("physical position required")
            };
            assert_eq!(position, PhysicalPosition::new(-50, -200));
        } else {
            assert!(prepared.attributes.position.is_none());
        }
    }
}

#[test]
fn sibling_write_failure_keeps_previous_file_and_new_memory_and_reports_then_retries() {
    let path = fixture();
    let mut state = GeometryState::new(Some(path.clone()));
    let original = Native::normal();
    state.handle(Lifecycle::Close, Some(&original), true, |message| {
        panic!("{message}")
    });
    let before = std::fs::read(&path).unwrap();
    std::fs::create_dir(path.with_extension("tmp")).unwrap(); // actual sibling-write failure, no permission tricks.
    let changed = Native {
        size: PhysicalSize::new(1200, 900),
        ..Native::normal()
    };
    for lifecycle in [Lifecycle::Close, Lifecycle::Exit] {
        let mut diagnostics = Vec::new();
        state.handle(lifecycle, Some(&changed), true, |message| {
            diagnostics.push(message.to_owned());
        });
        assert_eq!(std::fs::read(&path).unwrap(), before);
        close(
            f64::from(state.load_for_restore().unwrap().logical_width),
            800.0,
        );
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].contains("cannot save Settings window geometry:"));
    }
    // Preserve the failed fixture and remove the obstacle by moving it aside.
    std::fs::rename(
        path.with_extension("tmp"),
        path.with_extension("blocked-sibling"),
    )
    .unwrap();
    state.handle(Lifecycle::Exit, None::<&Native>, true, |message| {
        panic!("{message}")
    });
    close(
        f64::from(geometry::load(&path).unwrap().logical_width),
        800.0,
    );
}

#[test]
fn missing_corrupt_unknown_and_zero_state_restore_default_native_size() {
    for data in [
        None,
        Some("not json"),
        Some(r#"{"version":99,"logical_width":960,"logical_height":680}"#),
        Some(r#"{"version":1,"logical_width":0,"logical_height":680}"#),
    ] {
        let path = fixture();
        if let Some(data) = data {
            std::fs::write(&path, data).unwrap();
        }
        let state = GeometryState::new(Some(path));
        let prepared = restored(&state, 1.6, true);
        assert!(state.load_for_restore().is_none());
        assert_eq!(
            prepared
                .attributes
                .inner_size
                .unwrap()
                .to_physical::<u32>(1.5),
            PhysicalSize::new(1440, 1020)
        );
        assert!(prepared.attributes.position.is_none());
    }
}

#[test]
fn size_only_prefers_primary_monitor_scale_and_unknown_monitor_preserves_native_size() {
    let mut state = GeometryState::new(None);
    state.handle(
        Lifecycle::Changed,
        Some(&Native {
            size: PhysicalSize::new(12000, 12000),
            ..Native::normal()
        }),
        false,
        |message| panic!("{message}"),
    );
    let primary = MonitorGeometry {
        bounds: Rect {
            x: 0,
            y: 0,
            w: 2560,
            h: 1440,
        },
        scale: 2.0,
    };
    let restore =
        state.normalized_for_restore([960.0, 680.0], &[monitor(), primary], Some(primary), false);
    close(restore.logical_size[0], 1271.0);
    close(restore.logical_size[1], 697.5);
    let unknown = state.normalized_for_restore([960.0, 680.0], &[], None, false);
    close(unknown.logical_size[0], 8000.0);
    assert!(unknown.physical_position.is_none());
}
