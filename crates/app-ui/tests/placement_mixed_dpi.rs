//! Public geometry contract. Collector-to-app mixed-DPI controls live in
//! `runner::placement_tests` so the native snapshot builder can stay private.

use clipmonger_app_ui::placement::{place_with_context, DesktopContext, Placement, Rect};

#[test]
fn explicit_physical_size_controls_public_center_and_cursor_geometry() {
    let monitor = Rect {
        x: -1920,
        y: -200,
        w: 1920,
        h: 1080,
    };
    for (size, cursor, center) in [
        ((480, 360), (-480, 520), (-1200, 160)),
        ((960, 720), (-960, 160), (-1440, -20)),
    ] {
        let context = DesktopContext {
            monitors: vec![monitor],
            cursor: Some((-10, 800)),
            current_monitor: Some(monitor),
            primary_monitor: None,
            popup_physical_size: size,
            supports_global_position: true,
            destination_scale: 1.0,
        };
        assert_eq!(
            place_with_context(Placement::Cursor, &context),
            Some(cursor)
        );
        assert_eq!(
            place_with_context(Placement::Center, &context),
            Some(center)
        );
    }
}

#[test]
fn unsupported_global_position_suppresses_both_public_placement_modes() {
    let context = DesktopContext {
        monitors: vec![Rect {
            x: -1920,
            y: -200,
            w: 1920,
            h: 1080,
        }],
        cursor: Some((-10, 800)),
        current_monitor: None,
        primary_monitor: None,
        popup_physical_size: (960, 720),
        supports_global_position: false,
        destination_scale: 1.0,
    };
    assert_eq!(place_with_context(Placement::Cursor, &context), None);
    assert_eq!(place_with_context(Placement::Center, &context), None);
}
