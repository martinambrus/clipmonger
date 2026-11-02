//! Headless controls for the collector policy and the actual app viewport output.
//! The rig follows `tests/app_rules.rs`: one ROOT context, bridge events, and
//! the commands from the first logic/UI pass after context delivery.

use super::{
    deliver_desktop_context, desktop_context_from_snapshot, notify_desktop_geometry, App,
    DesktopGeometrySignal, WindowKind,
};
use crate::app::{AppParts, PopupApp};
use crate::ipc_bridge::{UiCmd, UiEvent};
use crate::placement::{DesktopContext, Rect};
use clipmonger_ipc::proto::methods::ShowPopupEvent;
use serde_json::json;
use std::sync::mpsc;
use std::time::Duration;
use winit::{dpi::PhysicalSize, event::WindowEvent};

fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
    Rect { x, y, w, h }
}

fn snapshot(
    physical: (u32, u32),
    current_scale: f64,
    scales: (f64, f64),
    cursor: Option<(i32, i32)>,
) -> DesktopContext {
    let left = rect(-1920, -200, 1920, 1080);
    let right = rect(0, 100, 2560, 1440);
    desktop_context_from_snapshot(
        physical,
        current_scale,
        &[(left, scales.0), (right, scales.1)],
        cursor,
        Some(left),
        Some(right),
        true,
    )
}

struct Rig {
    app: PopupApp,
    popup: egui::Context,
    events: mpsc::Sender<UiEvent>,
    _cmds: mpsc::Receiver<UiCmd>,
}

impl Rig {
    fn opened(mode: &str) -> Self {
        static NEXT_PROFILE: std::sync::atomic::AtomicUsize =
            std::sync::atomic::AtomicUsize::new(0);
        let (cmds, cmd_rx) = mpsc::channel();
        let (events, event_rx) = mpsc::channel();
        // Every control owns a unique throwaway profile under the coordinator's
        // TMPDIR. Keep it for inspection instead of touching a user's profile.
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let sequence = NEXT_PROFILE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let profile = std::env::temp_dir().join(format!(
            "clipmonger-dpi-{}-{nonce}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&profile).expect("create owned DPI control profile");
        let app = PopupApp::new(AppParts {
            profile,
            cmds,
            events: event_rx,
            tray: Box::new(Vec::new),
            hold: None,
            open: true,
            status_period: Duration::from_secs(3600),
            trace: None,
            allow_foreground: Box::new(|| {}),
        });
        let mut rig = Self {
            app,
            popup: egui::Context::default(),
            events,
            _cmds: cmd_rx,
        };
        rig.config(&json!({"popup": {"placement": mode}}));
        assert!(rig.pass(false).is_empty(), "wait for native context");
        assert!(rig.app.wants_desktop_context());
        rig
    }

    fn config(&self, config: &serde_json::Value) {
        self.events
            .send(UiEvent::Config(json!({"config": config})))
            .expect("config event");
    }

    fn pass(&mut self, visible: bool) -> Vec<egui::Pos2> {
        let raw = egui::RawInput::default();
        let app = &mut self.app;
        let commands = if visible {
            let mut runs = 0;
            let mut output = self.popup.run_ui(raw, |ui| {
                runs += 1;
                app.logic(ui.ctx(), WindowKind::Popup);
                app.ui(ui, WindowKind::Popup);
            });
            assert_eq!(runs, 1, "the control requires one UI pass");
            output.textures_delta.clear();
            output
                .viewport_output
                .remove(&egui::ViewportId::ROOT)
                .expect("ROOT viewport output")
                .commands
        } else {
            self.popup
                .run_logic(&raw, |ctx| app.logic(ctx, WindowKind::Popup))
                .viewport_commands
                .remove(&egui::ViewportId::ROOT)
                .unwrap_or_default()
        };
        let pixels_per_point = self.popup.pixels_per_point();
        commands
            .into_iter()
            .filter_map(|command| match command {
                egui::ViewportCommand::OuterPosition(pos) => Some(egui::pos2(
                    pos.x * pixels_per_point,
                    pos.y * pixels_per_point,
                )),
                _ => None,
            })
            .collect()
    }

    fn deliver(&mut self, context: DesktopContext) {
        assert!(deliver_desktop_context(&mut self.app, || Some(context)));
    }

    fn idle(&mut self) {
        assert!(self.pass(false).is_empty(), "unexpected idle position");
        assert!(!deliver_desktop_context(&mut self.app, || {
            panic!("idle pass queried native context")
        }));
    }
}

#[test]
#[expect(
    clippy::cast_precision_loss,
    reason = "fixture physical coordinates are bounded exactly representable integers"
)]
fn mixed_dpi_emits_actual_center_and_cursor_commands() {
    // Raw host size / native scales go through the same builder as the native
    // collector. Expected positions are independent, physical-pixel outcomes.
    let cases = [
        (
            (480, 360),
            1.0,
            (1.0, 2.0),
            (2500, 1500),
            (960, 720),
            (1600, 820),
            (800, 460),
        ),
        (
            (960, 720),
            2.0,
            (1.0, 2.0),
            (-10, 800),
            (480, 360),
            (-480, 520),
            (-1200, 160),
        ),
        (
            (480, 360),
            1.0,
            (2.0, 1.0),
            (-10, 800),
            (960, 720),
            (-960, 160),
            (-1440, -20),
        ),
        (
            (480, 360),
            1.0,
            (1.0, 1.0),
            (2500, 1500),
            (480, 360),
            (2080, 1180),
            (1040, 640),
        ),
        (
            (601, 451),
            1.25,
            (1.25, 1.5),
            (2500, 1500),
            (721, 541),
            (1839, 999),
            (919, 549),
        ),
    ];
    for (physical, current_scale, scales, cursor, size, cursor_pos, center_pos) in cases {
        for (mode, expected) in [("cursor", cursor_pos), ("center", center_pos)] {
            for visible in [false, true] {
                let mut rig = Rig::opened(mode);
                let context = snapshot(physical, current_scale, scales, Some(cursor));
                let collected_size = context.popup_physical_size;
                rig.deliver(context);
                let positions = rig.pass(visible);
                assert_eq!(positions.len(), 1, "{mode}: one actual position required");
                assert_eq!(
                    positions[0],
                    egui::pos2(expected.0 as f32, expected.1 as f32)
                );
                assert_eq!(collected_size, size);
                assert!(!rig.app.wants_desktop_context());
                rig.idle();
            }
        }
    }
}

#[test]
fn mixed_dpi_invalid_scales_preserve_current_physical_size() {
    for invalid in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        for (current_scale, destination_scale) in [(invalid, 2.0), (1.0, invalid)] {
            let mut rig = Rig::opened("cursor");
            let context = snapshot(
                (480, 360),
                current_scale,
                (1.0, destination_scale),
                Some((2500, 1500)),
            );
            assert_eq!(context.popup_physical_size, (480, 360));
            rig.deliver(context);
            assert_eq!(rig.pass(false), vec![egui::pos2(2080.0, 1180.0)]);
            rig.idle();
        }
    }
}

#[test]
#[expect(
    clippy::cast_precision_loss,
    reason = "fixture physical coordinates are bounded exactly representable integers"
)]
fn mixed_dpi_destination_selection_uses_cursor_nearest_then_native_fallbacks() {
    let left = rect(-1920, -200, 1920, 1080);
    let right = rect(0, 100, 2560, 1440);
    for (cursor, current, primary, expected_size, expected_center) in [
        (
            Some((4000, 1500)),
            Some(left),
            Some(left),
            (960, 720),
            (800, 460),
        ),
        (None, Some(right), Some(left), (960, 720), (800, 460)),
        (None, None, Some(right), (960, 720), (800, 460)),
        (None, None, None, (480, 360), (-1200, 160)),
    ] {
        let mut rig = Rig::opened("center");
        let context = desktop_context_from_snapshot(
            (480, 360),
            1.0,
            &[(left, 1.0), (right, 2.0)],
            cursor,
            current,
            primary,
            true,
        );
        assert_eq!(context.popup_physical_size, expected_size);
        rig.deliver(context);
        assert_eq!(
            rig.pass(false),
            vec![egui::pos2(
                expected_center.0 as f32,
                expected_center.1 as f32
            )]
        );
    }
}

#[test]
fn mixed_dpi_resolved_placement_stays_idle_on_config_raise_and_geometry() {
    let mut rig = Rig::opened("cursor");
    rig.deliver(snapshot((480, 360), 1.0, (1.0, 2.0), Some((2500, 1500))));
    assert_eq!(rig.pass(false), vec![egui::pos2(1600.0, 820.0)]);
    rig.config(&json!({"popup": {"placement": "cursor"}}));
    rig.idle();
    rig.config(&json!({"popup": {"placement": "cursor", "preview_width": 80}}));
    rig.idle();
    rig.events
        .send(UiEvent::ShowPopup(ShowPopupEvent {
            source: "request".into(),
            ..Default::default()
        }))
        .expect("raise event");
    rig.idle();
    for _ in 0..3 {
        assert!(!notify_desktop_geometry(
            &mut rig.app,
            DesktopGeometrySignal::Window(
                WindowKind::Popup,
                &WindowEvent::Resized(PhysicalSize::new(960, 720))
            )
        ));
        assert!(!notify_desktop_geometry(
            &mut rig.app,
            DesktopGeometrySignal::Resumed
        ));
        rig.idle();
    }
}

#[test]
fn mixed_dpi_missing_geometry_retries_once_and_wayland_never_retries() {
    for supported in [true, false] {
        let mut rig = Rig::opened("cursor");
        rig.deliver(desktop_context_from_snapshot(
            (480, 360),
            1.0,
            &[],
            Some((2500, 1500)),
            None,
            None,
            supported,
        ));
        rig.idle();
        assert_eq!(
            notify_desktop_geometry(&mut rig.app, DesktopGeometrySignal::Resumed),
            supported
        );
        if supported {
            rig.deliver(snapshot((480, 360), 1.0, (1.0, 2.0), Some((2500, 1500))));
            assert_eq!(rig.pass(false), vec![egui::pos2(1600.0, 820.0)]);
        }
        for _ in 0..3 {
            assert!(!notify_desktop_geometry(
                &mut rig.app,
                DesktopGeometrySignal::Resumed
            ));
            rig.idle();
        }
    }
}
