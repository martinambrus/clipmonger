//! Native context must arrive before a placement command can be emitted.
use clipmonger_app_ui::{
    app::{AppParts, PopupApp},
    runner::{App, WindowKind},
};
use std::{path::PathBuf, sync::mpsc, time::Duration};

#[test]
fn placement_waits_for_native_context_with_monitor_size() {
    let (cmds, _commands) = mpsc::channel();
    let (_events, events) = mpsc::channel();
    let mut app = PopupApp::new(AppParts {
        profile: PathBuf::from("placement-fixture"),
        cmds,
        events,
        tray: Box::new(|_| Vec::new()),
        hold: None,
        open: true,
        status_period: Duration::from_secs(3600),
        trace: None,
        allow_foreground: Box::new(|| {}),
    });
    let ctx = egui::Context::default();
    let mut raw = egui::RawInput::default();
    raw.viewports
        .get_mut(&egui::ViewportId::ROOT)
        .unwrap()
        .monitor_size = Some(egui::vec2(1920.0, 1080.0));
    let output = ctx.run_logic(&raw, |ctx| app.logic(ctx, WindowKind::Popup));
    let positions: Vec<_> = output
        .viewport_commands
        .get(&egui::ViewportId::ROOT)
        .into_iter()
        .flatten()
        .filter(|command| matches!(command, egui::ViewportCommand::OuterPosition(_)))
        .collect();
    assert!(
        positions.is_empty(),
        "placement must wait for native context, got {positions:?}"
    );
}
