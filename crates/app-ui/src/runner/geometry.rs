//! Runner-owned normal Settings geometry, independent of the native host lifetime.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::window::Window;

use crate::placement::Rect;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersistedGeometry {
    pub version: u32,
    pub logical_width: f32,
    pub logical_height: f32,
    pub physical_x: Option<i32>,
    pub physical_y: Option<i32>,
}

impl PersistedGeometry {
    #[must_use]
    pub fn validate(&self) -> bool {
        self.version == 1
            && self.logical_width.is_finite()
            && self.logical_height.is_finite()
            && (200.0..=8000.0).contains(&self.logical_width)
            && (150.0..=8000.0).contains(&self.logical_height)
    }
}

/// The native observations used by all three Settings capture paths.
pub trait GeometryWindow {
    fn inner_size(&self) -> PhysicalSize<u32>;
    fn outer_size(&self) -> PhysicalSize<u32>;
    fn scale_factor(&self) -> f64;
    fn is_minimized(&self) -> Option<bool>;
    fn is_maximized(&self) -> bool;
    fn is_fullscreen(&self) -> bool;
    fn outer_position(&self) -> Option<PhysicalPosition<i32>>;
    /// `None` means a required native observation was unavailable; reject capture.
    /// Platforms without a native query report `Some(false)`.
    fn is_native_maximized(&self) -> Option<bool> {
        Some(false)
    }
}

impl GeometryWindow for Window {
    fn inner_size(&self) -> PhysicalSize<u32> {
        Self::inner_size(self)
    }
    fn outer_size(&self) -> PhysicalSize<u32> {
        Self::outer_size(self)
    }
    fn scale_factor(&self) -> f64 {
        Self::scale_factor(self)
    }
    fn is_minimized(&self) -> Option<bool> {
        Self::is_minimized(self)
    }
    fn is_maximized(&self) -> bool {
        Self::is_maximized(self)
    }
    fn is_fullscreen(&self) -> bool {
        self.fullscreen().is_some()
    }
    fn outer_position(&self) -> Option<PhysicalPosition<i32>> {
        Self::outer_position(self).ok()
    }
    fn is_native_maximized(&self) -> Option<bool> {
        crate::runner::window::is_window_zoomed(self)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Lifecycle {
    Changed,
    Close,
    Exit,
}

/// Physical monitor bounds and the OS scale used for native logical sizes.
#[derive(Clone, Copy, Debug)]
pub struct MonitorGeometry {
    pub bounds: Rect,
    pub scale: f64,
}

#[derive(Debug, Clone)]
pub struct GeometryState {
    path: Option<PathBuf>,
    last_valid: Option<PersistedGeometry>,
    pending: Option<PersistedGeometry>,
    decoration: [u32; 2],
}

impl GeometryState {
    #[must_use]
    pub fn new(path: Option<PathBuf>) -> Self {
        let last_valid = path.as_ref().and_then(|path| load(path));
        Self {
            path,
            last_valid,
            pending: None,
            decoration: [0, 0],
        }
    }

    /// Actual event/close/Quit capture and persistence decision. Position getters
    /// are never called on a compositor that owns global positioning.
    pub fn handle(
        &mut self,
        lifecycle: Lifecycle,
        window: Option<&impl GeometryWindow>,
        global_position: bool,
        mut diagnostic: impl FnMut(&str),
    ) {
        if let Some(window) = window {
            self.capture_window(window, global_position);
        }
        if matches!(lifecycle, Lifecycle::Close | Lifecycle::Exit) {
            if let Err(error) = self.save_if_needed() {
                diagnostic(&format!("cannot save Settings window geometry: {error}"));
            }
        }
    }

    fn capture_window(&mut self, window: &impl GeometryWindow, global_position: bool) {
        if window.is_minimized() == Some(true)
            || window.is_maximized()
            || window.is_native_maximized() != Some(false)
            || window.is_fullscreen()
        {
            return;
        }
        let physical = window.inner_size();
        let scale = window.scale_factor();
        if physical.width == 0 || physical.height == 0 || !scale.is_finite() || scale <= 0.0 {
            return;
        }
        let logical = physical.to_logical::<f32>(scale);
        let position = if global_position {
            window.outer_position()
        } else {
            None
        };
        let geom = PersistedGeometry {
            version: 1,
            logical_width: logical.width,
            logical_height: logical.height,
            physical_x: position.map(|pos| pos.x),
            physical_y: position.map(|pos| pos.y),
        };
        if geom.validate() {
            let outer = window.outer_size();
            self.decoration = [
                outer.width.saturating_sub(physical.width),
                outer.height.saturating_sub(physical.height),
            ];
            if self.last_valid.as_ref() != Some(&geom) {
                self.pending = Some(geom.clone());
                self.last_valid = Some(geom);
            }
        }
    }

    /// Coalesced edge write; a failed write retains both the new in-memory state
    /// and its pending retry, while the previous complete file remains intact.
    ///
    /// # Errors
    /// Returns the sibling-write or replacement error.
    fn save_if_needed(&mut self) -> std::io::Result<()> {
        if let (Some(pending), Some(path)) = (&self.pending, &self.path) {
            atomic_write(path, pending)?;
        }
        self.pending = None;
        Ok(())
    }

    #[must_use]
    pub fn load_for_restore(&self) -> Option<PersistedGeometry> {
        self.last_valid.clone().filter(PersistedGeometry::validate)
    }

    #[must_use]
    pub fn normalized_for_restore(
        &self,
        fallback_size: [f32; 2],
        monitors: &[MonitorGeometry],
        preferred: Option<MonitorGeometry>,
        global_position: bool,
    ) -> NormalizedRestore {
        let geom = self.load_for_restore();
        let mut size = geom.as_ref().map_or(fallback_size.map(f64::from), |geom| {
            [
                f64::from(geom.logical_width),
                f64::from(geom.logical_height),
            ]
        });
        let mut position = if global_position {
            geom.as_ref()
                .and_then(|geom| Some([geom.physical_x?, geom.physical_y?]))
        } else {
            None
        };
        let usable: Vec<_> = monitors
            .iter()
            .copied()
            .filter(|monitor| {
                monitor.bounds.w > 0
                    && monitor.bounds.h > 0
                    && monitor.scale.is_finite()
                    && monitor.scale > 0.0
            })
            .collect();
        let monitor = position
            .and_then(|pos| {
                usable
                    .iter()
                    .min_by_key(|monitor| distance(pos, monitor.bounds))
                    .copied()
            })
            .or_else(|| {
                preferred.filter(|monitor| usable.iter().any(|item| item.bounds == monitor.bounds))
            })
            .or_else(|| usable.first().copied());
        if let Some(monitor) = monitor {
            let bounds = monitor.bounds;
            let available = [
                f64::from(bounds.w) - f64::from(self.decoration[0]),
                f64::from(bounds.h) - f64::from(self.decoration[1]),
            ];
            for axis in 0..2 {
                size[axis] = size[axis].min(available[axis].max(1.0) / monitor.scale);
            }
            if let Some(pos) = &mut position {
                let outer_width = size[0] * monitor.scale + f64::from(self.decoration[0]);
                // Keep at least 50 physical pixels of the title bar reachable.
                pos[0] = clamp_coordinate(
                    f64::from(pos[0]),
                    f64::from(bounds.x) - outer_width + 50.0,
                    f64::from(bounds.x) + f64::from(bounds.w) - 50.0,
                );
                pos[1] = clamp_coordinate(
                    f64::from(pos[1]),
                    f64::from(bounds.y),
                    f64::from(bounds.y) + f64::from(bounds.h) - 50.0,
                );
            }
        }
        NormalizedRestore {
            logical_size: size,
            physical_position: position,
        }
    }
}

fn distance(point: [i32; 2], rect: Rect) -> i128 {
    let x = i128::from(point[0]);
    let y = i128::from(point[1]);
    let left = i128::from(rect.x);
    let top = i128::from(rect.y);
    let dx = x.clamp(left, left + i128::from(rect.w)) - x;
    let dy = y.clamp(top, top + i128::from(rect.h)) - y;
    dx * dx + dy * dy
}

// Bounds come from finite physical desktop coordinates and validated sizes.
#[allow(clippy::cast_possible_truncation)]
fn clamp_coordinate(value: f64, low: f64, high: f64) -> i32 {
    value.max(low.min(high)).min(high.max(low)).round() as i32
}

#[derive(Debug, Clone)]
pub struct NormalizedRestore {
    pub logical_size: [f64; 2],
    pub physical_position: Option<[i32; 2]>,
}

#[must_use]
pub fn load(path: &Path) -> Option<PersistedGeometry> {
    serde_json::from_slice::<PersistedGeometry>(&fs::read(path).ok()?)
        .ok()
        .filter(PersistedGeometry::validate)
}

/// Write a complete sibling before replacing the previous window state.
///
/// # Errors
/// Returns an error when serialization, writing or replacement fails.
pub fn atomic_write(path: &Path, geom: &PersistedGeometry) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let data = serde_json::to_vec_pretty(geom).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)
}
