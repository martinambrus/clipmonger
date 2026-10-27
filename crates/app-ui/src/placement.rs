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
