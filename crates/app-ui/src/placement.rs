//! Pure popup geometry over monitor rectangles.

// Monitor rectangle: origin plus size, all device pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// Left edge in device pixels.
    pub x: i32,
    /// Top edge in device pixels.
    pub y: i32,
    /// Width in device pixels.
    pub w: i32,
    /// Height in device pixels.
    pub h: i32,
}

/// Popup anchor mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Open at the cursor, clamped inside the nearest monitor.
    Cursor,
    /// Open centered on the containing monitor.
    Center,
}

/// Clamp a value into a closed range.
fn clamp(value: i32, low: i32, high: i32) -> i32 {
    value.max(low).min(high)
}

/// Pick the monitor containing the cursor, else the nearest one.
fn pick(cursor: (i32, i32), monitors: &[Rect]) -> Rect {
    let mut best = monitors[0];
    let mut best_dist = i64::MAX;
    for monitor in monitors {
        let inside = cursor.0 >= monitor.x
            && cursor.0 < monitor.x.saturating_add(monitor.w)
            && cursor.1 >= monitor.y
            && cursor.1 < monitor.y.saturating_add(monitor.h);
        if inside {
            return *monitor;
        }
        let dx = i64::from(
            clamp(cursor.0, monitor.x, monitor.x.saturating_add(monitor.w))
                .saturating_sub(cursor.0),
        );
        let dy = i64::from(
            clamp(cursor.1, monitor.y, monitor.y.saturating_add(monitor.h))
                .saturating_sub(cursor.1),
        );
        let dist = dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy));
        if dist < best_dist {
            best_dist = dist;
            best = *monitor;
        }
    }
    best
}

/// Monitor that `place_with_context` would use for either placement mode.
#[must_use]
pub(crate) fn destination_monitor(
    cursor: Option<(i32, i32)>,
    monitors: &[Rect],
    current_monitor: Option<Rect>,
    primary_monitor: Option<Rect>,
) -> Option<Rect> {
    if monitors.is_empty() {
        return None;
    }
    if let Some(pos) = cursor {
        Some(pick(pos, monitors))
    } else {
        current_monitor
            .or(primary_monitor)
            .or_else(|| monitors.first().copied())
    }
}

/// Scale-aware physical size prediction for mixed-DPI moves.
///
/// `current_physical` is the host window's present physical size on its
/// current monitor at `current_scale`. The logical size is
/// `current_physical / current_scale` and the predicted destination size is
/// `logical * dest_scale` rounded to nearest physical pixel. Falls back to
/// `current_physical` when scales are non-finite or non-positive.
#[expect(
    clippy::cast_possible_truncation,
    reason = "rounded physical dimensions intentionally saturate to i32"
)]
#[must_use]
pub(crate) fn predicted_physical_size(
    current_physical: (u32, u32),
    current_scale: f64,
    dest_scale: f64,
) -> (i32, i32) {
    let unchanged = (
        i32::try_from(current_physical.0).unwrap_or(i32::MAX),
        i32::try_from(current_physical.1).unwrap_or(i32::MAX),
    );
    if !current_scale.is_finite()
        || current_scale <= 0.0
        || !dest_scale.is_finite()
        || dest_scale <= 0.0
        || (current_scale - dest_scale).abs() <= f64::EPSILON
    {
        return unchanged;
    }
    let logical_w = f64::from(current_physical.0) / current_scale;
    let logical_h = f64::from(current_physical.1) / current_scale;
    let pred_w = (logical_w * dest_scale).round() as i32;
    let pred_h = (logical_h * dest_scale).round() as i32;
    if pred_w > 0 && pred_h > 0 {
        (pred_w, pred_h)
    } else {
        unchanged
    }
}

/// Shell window size in the popup state.
pub const POPUP_DEFAULT: (i32, i32) = (480, 360);

/// Shell window size while the config window is open (seg-09 section 4:
/// the maximalist config needs tabs, rows and Apply/Revert co-visible).
pub const CONFIG_DEFAULT: (i32, i32) = (960, 680);

/// Window size for the shell state: config open grows the one window.
#[must_use]
pub fn window_size_for(config_open: bool) -> (i32, i32) {
    if config_open {
        CONFIG_DEFAULT
    } else {
        POPUP_DEFAULT
    }
}

/// Fallback monitor size used only when the runtime reports no monitor size.
pub const FALLBACK_MONITOR_SIZE: (u32, u32) = (1920, 1080);

/// Map the runtime monitor size to the placement monitor rectangle.
///
/// Falls back to 1920x1080 at the origin only when the size is unknown
/// (`None`); a reported size is used as-is.
#[must_use]
pub fn monitor_rect_from_size(monitor_size: Option<(u32, u32)>) -> Rect {
    let (w, h) = monitor_size.unwrap_or(FALLBACK_MONITOR_SIZE);
    Rect {
        x: 0,
        y: 0,
        w: i32::try_from(w).unwrap_or(i32::MAX),
        h: i32::try_from(h).unwrap_or(i32::MAX),
    }
}

/// Compute the popup top-left so it fits fully inside the monitor.
#[must_use]
pub fn place(
    mode: Placement,
    cursor: (i32, i32),
    monitors: &[Rect],
    size: (i32, i32),
) -> (i32, i32) {
    if monitors.is_empty() {
        return (0, 0);
    }
    let monitor = pick(cursor, monitors);
    let (w, h) = size;
    match mode {
        Placement::Cursor => {
            let max_x = monitor.x.saturating_add(monitor.w.saturating_sub(w));
            let max_y = monitor.y.saturating_add(monitor.h.saturating_sub(h));
            (
                clamp(cursor.0, monitor.x.min(max_x), max_x.max(monitor.x)),
                clamp(cursor.1, monitor.y.min(max_y), max_y.max(monitor.y)),
            )
        }
        Placement::Center => {
            let x = if w >= monitor.w {
                monitor.x
            } else {
                monitor.x.saturating_add(monitor.w.saturating_sub(w) / 2)
            };
            let y = if h >= monitor.h {
                monitor.y
            } else {
                monitor.y.saturating_add(monitor.h.saturating_sub(h) / 2)
            };
            (x, y)
        }
    }
}

/// Snapshot of physical desktop for placement. On Windows the monitor
/// rectangles are the work areas (without the taskbar).
#[derive(Debug, Clone, PartialEq)]
pub struct DesktopContext {
    pub monitors: Vec<Rect>,
    pub cursor: Option<(i32, i32)>,
    pub current_monitor: Option<Rect>,
    pub primary_monitor: Option<Rect>,
    pub popup_physical_size: (i32, i32),
    pub supports_global_position: bool,
    /// Native scale factor of the destination monitor.
    pub destination_scale: f64,
}

/// Smallest popup size in points.
pub const POPUP_MIN: (f32, f32) = (320.0, 160.0);

/// Popup size in physical pixels for a wanted size in points at `scale`
/// physical pixels per point: at most 30% of the work area's width and 90% of
/// its height, at least [`POPUP_MIN`] (the minimum wins on a tiny work area).
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "physical popup dimensions are small and rounded to whole pixels"
)]
#[must_use]
pub fn popup_size(wanted: (f32, f32), scale: f32, work: Rect) -> (i32, i32) {
    let w = (wanted.0 * scale)
        .min(work.w as f32 * 0.3)
        .max(POPUP_MIN.0 * scale);
    let h = (wanted.1 * scale)
        .min(work.h as f32 * 0.9)
        .max(POPUP_MIN.1 * scale);
    (w.round() as i32, h.round() as i32)
}

/// Compute placement from full desktop context, honoring cursor/center rules and fallbacks.
#[must_use]
pub fn place_with_context(mode: Placement, ctx: &DesktopContext) -> Option<(i32, i32)> {
    if !ctx.supports_global_position {
        return None;
    }
    if ctx.monitors.is_empty() {
        return None;
    }
    match mode {
        Placement::Cursor => {
            if let Some(cursor) = ctx.cursor {
                Some(place(
                    Placement::Cursor,
                    cursor,
                    &ctx.monitors,
                    ctx.popup_physical_size,
                ))
            } else {
                // without cursor, center on known current/primary
                let fallback = ctx
                    .current_monitor
                    .or(ctx.primary_monitor)
                    .or_else(|| ctx.monitors.first().copied())?;
                let size = ctx.popup_physical_size;
                let x = if size.0 >= fallback.w {
                    fallback.x
                } else {
                    fallback.x + (fallback.w - size.0) / 2
                };
                let y = if size.1 >= fallback.h {
                    fallback.y
                } else {
                    fallback.y + (fallback.h - size.1) / 2
                };
                Some((x, y))
            }
        }
        Placement::Center => {
            if let Some(cursor) = ctx.cursor {
                // center on cursor's containing/nearest monitor
                let monitor = pick(cursor, &ctx.monitors);
                let size = ctx.popup_physical_size;
                let x = if size.0 >= monitor.w {
                    monitor.x
                } else {
                    monitor.x + (monitor.w - size.0) / 2
                };
                let y = if size.1 >= monitor.h {
                    monitor.y
                } else {
                    monitor.y + (monitor.h - size.1) / 2
                };
                Some((x, y))
            } else {
                let fallback = ctx
                    .current_monitor
                    .or(ctx.primary_monitor)
                    .or_else(|| ctx.monitors.first().copied())?;
                let size = ctx.popup_physical_size;
                let x = if size.0 >= fallback.w {
                    fallback.x
                } else {
                    fallback.x + (fallback.w - size.0) / 2
                };
                let y = if size.1 >= fallback.h {
                    fallback.y
                } else {
                    fallback.y + (fallback.h - size.1) / 2
                };
                Some((x, y))
            }
        }
    }
}

#[cfg(test)]
mod placement_context_tests {
    use super::*;
    #[test]
    fn cursor_uses_containing_monitor_with_negative_origin() {
        let monitors = vec![
            Rect {
                x: -1920,
                y: 0,
                w: 1920,
                h: 1080,
            },
            Rect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
            },
        ];
        let ctx = DesktopContext {
            monitors,
            cursor: Some((-1000, 500)),
            current_monitor: None,
            primary_monitor: None,
            popup_physical_size: (480, 360),
            supports_global_position: true,
            destination_scale: 1.0,
        };
        let pos = place_with_context(Placement::Cursor, &ctx).unwrap();
        assert!(pos.0 < 0, "should be on negative monitor: {pos:?}");
    }
    #[test]
    fn unknown_cursor_falls_back_to_primary() {
        let monitors = vec![Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        }];
        let ctx = DesktopContext {
            monitors: monitors.clone(),
            cursor: None,
            current_monitor: None,
            primary_monitor: Some(monitors[0]),
            popup_physical_size: (480, 360),
            supports_global_position: true,
            destination_scale: 1.0,
        };
        let pos = place_with_context(Placement::Center, &ctx).unwrap();
        assert_eq!(pos, (720, 360));
    }
    #[test]
    fn wayland_no_position() {
        let ctx = DesktopContext {
            monitors: vec![Rect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
            }],
            cursor: Some((100, 100)),
            current_monitor: None,
            primary_monitor: None,
            popup_physical_size: (480, 360),
            supports_global_position: false,
            destination_scale: 1.0,
        };
        assert!(place_with_context(Placement::Cursor, &ctx).is_none());
    }
    #[test]
    fn empty_monitors_no_position() {
        let ctx = DesktopContext {
            monitors: vec![],
            cursor: Some((0, 0)),
            current_monitor: None,
            primary_monitor: None,
            popup_physical_size: (480, 360),
            supports_global_position: true,
            destination_scale: 1.0,
        };
        assert!(place_with_context(Placement::Center, &ctx).is_none());
    }
    #[test]
    fn popup_size_caps_at_work_area_share() {
        let work = Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1040,
        };
        assert_eq!(popup_size((500.0, 300.0), 1.0, work), (500, 300));
        assert_eq!(popup_size((900.0, 2000.0), 1.0, work), (576, 936));
        assert_eq!(popup_size((500.0, 300.0), 1.5, work), (576, 450));
    }
    #[test]
    fn popup_size_keeps_minimum_on_small_work_area() {
        let work = Rect {
            x: 0,
            y: 0,
            w: 800,
            h: 150,
        };
        assert_eq!(popup_size((10.0, 10.0), 1.0, work), (320, 160));
        assert_eq!(popup_size((10.0, 10.0), 2.0, work), (640, 320));
    }
}
