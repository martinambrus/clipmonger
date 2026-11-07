//! Helpers for the UI process on macOS: pointer position and usable screen
//! area in winit's coordinates (physical pixels, origin at the top-left of
//! the main display), and handing the focus back after a pick.
//!
//! Must be called on the main thread (`AppKit`); elsewhere they return
//! `None` / do nothing.
#![cfg_attr(target_os = "macos", allow(unsafe_code))]

/// A rectangle in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhysRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// One screen in Quartz points (origin top-left of the main display) with
/// its backing scale factor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenGeom {
    /// Whole screen, Quartz points.
    pub frame: (f64, f64, f64, f64),
    /// Screen without the menu bar and the Dock, Quartz points.
    pub visible: (f64, f64, f64, f64),
    pub scale: f64,
}

/// Cocoa rectangle (origin bottom-left of the main display) to Quartz
/// (origin top-left), given the main display's height.
#[must_use]
pub fn cocoa_to_quartz(rect: (f64, f64, f64, f64), main_height: f64) -> (f64, f64, f64, f64) {
    let (x, y, w, h) = rect;
    (x, main_height - (y + h), w, h)
}

#[allow(clippy::cast_possible_truncation)] // screen coordinates fit i32
fn to_phys(rect: (f64, f64, f64, f64), scale: f64) -> PhysRect {
    PhysRect {
        x: (rect.0 * scale).round() as i32,
        y: (rect.1 * scale).round() as i32,
        w: (rect.2 * scale).round() as i32,
        h: (rect.3 * scale).round() as i32,
    }
}

/// Usable area of the screen whose physical origin is `(x, y)` (a winit
/// monitor position), from `screens`.
#[must_use]
pub fn work_area_in(screens: &[ScreenGeom], x: i32, y: i32) -> Option<PhysRect> {
    screens.iter().find_map(|screen| {
        let frame = to_phys(screen.frame, screen.scale);
        ((frame.x - x).abs() <= 1 && (frame.y - y).abs() <= 1)
            .then(|| to_phys(screen.visible, screen.scale))
    })
}

/// Physical position of a Quartz point, scaled by the screen containing it.
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub fn point_in(screens: &[ScreenGeom], px: f64, py: f64) -> Option<(i32, i32)> {
    let scale = screens
        .iter()
        .find(|s| {
            let (x, y, w, h) = s.frame;
            px >= x && px < x + w && py >= y && py < y + h
        })
        .map_or(1.0, |s| s.scale);
    Some(((px * scale).round() as i32, (py * scale).round() as i32))
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{cocoa_to_quartz, ScreenGeom};
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSScreen};

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGPoint {
        x: f64,
        y: f64,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreate(source: *const std::ffi::c_void) -> *mut std::ffi::c_void;
        fn CGEventGetLocation(event: *mut std::ffi::c_void) -> CGPoint;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: *const std::ffi::c_void);
    }

    pub(super) fn screens() -> Vec<ScreenGeom> {
        let Some(mtm) = MainThreadMarker::new() else {
            return Vec::new();
        };
        let all = NSScreen::screens(mtm);
        let Some(main) = all.firstObject() else {
            return Vec::new();
        };
        let main_height = main.frame().size.height;
        all.iter()
            .map(|screen| {
                let rect = |r: objc2_foundation::NSRect| {
                    cocoa_to_quartz(
                        (r.origin.x, r.origin.y, r.size.width, r.size.height),
                        main_height,
                    )
                };
                ScreenGeom {
                    frame: rect(screen.frame()),
                    visible: rect(screen.visibleFrame()),
                    // Safety: NSScreen responds to backingScaleFactor (10.7+).
                    scale: unsafe { objc2::msg_send![&*screen, backingScaleFactor] },
                }
            })
            .collect()
    }

    pub(super) fn cursor_points() -> Option<(f64, f64)> {
        // Safety: a null source creates an event carrying the current
        // pointer location; the event is released right after reading it.
        unsafe {
            let event = CGEventCreate(std::ptr::null());
            if event.is_null() {
                return None;
            }
            let point = CGEventGetLocation(event);
            CFRelease(event);
            Some((point.x, point.y))
        }
    }

    pub(super) fn hide_app() {
        if let Some(mtm) = MainThreadMarker::new() {
            NSApplication::sharedApplication(mtm).hide(None);
        }
    }
}

/// Usable area (without menu bar and Dock) of the monitor at physical
/// position `(x, y)`.
#[must_use]
pub fn work_area(x: i32, y: i32) -> Option<PhysRect> {
    #[cfg(target_os = "macos")]
    {
        work_area_in(&imp::screens(), x, y)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (x, y);
        None
    }
}

/// Pointer position in physical pixels.
#[must_use]
pub fn cursor() -> Option<(i32, i32)> {
    #[cfg(target_os = "macos")]
    {
        let (x, y) = imp::cursor_points()?;
        point_in(&imp::screens(), x, y)
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Activate the app with process id `pid` (the paste target). Called by
/// the UI while it is the active app: macOS 14+ lets the active app pass
/// activation on, but refuses a background process (the daemon).
pub fn activate_pid(pid: u64) {
    if let (Ok(pid), Ok(foreground)) = (i32::try_from(pid), crate::MacForeground::new()) {
        let _ = foreground.activate(pid);
    }
}

/// Hide this app so macOS gives the focus back to the app used before it
/// (the paste target). Works where a background process may no longer
/// activate another app (macOS 14+).
pub fn hide_app() {
    #[cfg(target_os = "macos")]
    imp::hide_app();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cocoa_rect_flips_to_top_left_origin() {
        // A 1440x900 main display with a 25 pt menu bar and a 70 pt Dock.
        assert_eq!(
            cocoa_to_quartz((0.0, 70.0, 1440.0, 805.0), 900.0),
            (0.0, 25.0, 1440.0, 805.0)
        );
    }

    #[test]
    fn work_area_matches_monitor_origin_and_scales() {
        let screens = [
            ScreenGeom {
                frame: (0.0, 0.0, 1440.0, 900.0),
                visible: (0.0, 25.0, 1440.0, 805.0),
                scale: 2.0,
            },
            ScreenGeom {
                frame: (1440.0, 0.0, 1920.0, 1080.0),
                visible: (1440.0, 25.0, 1920.0, 1055.0),
                scale: 1.0,
            },
        ];
        assert_eq!(
            work_area_in(&screens, 0, 0),
            Some(PhysRect {
                x: 0,
                y: 50,
                w: 2880,
                h: 1610
            })
        );
        assert_eq!(
            work_area_in(&screens, 1440, 0),
            Some(PhysRect {
                x: 1440,
                y: 25,
                w: 1920,
                h: 1055
            })
        );
        assert_eq!(work_area_in(&screens, 7, 7), None);
        assert_eq!(point_in(&screens, 100.0, 50.0), Some((200, 100)));
        assert_eq!(point_in(&screens, 1500.0, 50.0), Some((1500, 50)));
    }
}
