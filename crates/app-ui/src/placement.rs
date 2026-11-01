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

/// Snapshot of physical desktop for placement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopContext {
    pub monitors: Vec<Rect>,
    pub cursor: Option<(i32, i32)>,
    pub current_monitor: Option<Rect>,
    pub primary_monitor: Option<Rect>,
    pub popup_physical_size: (i32, i32),
    pub supports_global_position: bool,
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
        };
        assert!(place_with_context(Placement::Center, &ctx).is_none());
    }
}
