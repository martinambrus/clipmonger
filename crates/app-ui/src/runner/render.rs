//! CPU rendering of egui frames and their presentation through softbuffer.

use std::sync::Arc;

use egui::{ClippedPrimitive, Color32, TexturesDelta};
use egui_software_backend::{BufferMutRef, ColorFieldOrder, EguiSoftwareRender};
use winit::dpi::PhysicalSize;
use winit::event_loop::OwnedDisplayHandle;
use winit::window::Window;

/// The software rasterizer of one egui context. Texture ids are per context,
/// so a context never shares its rasterizer, and a rasterizer never serves
/// two contexts.
///
/// Built with `EguiSoftwareRender::new(ColorFieldOrder::Bgra).with_caching(false)`:
/// the caching mode ignores in-place updates of a texture (its cache key holds
/// no texture content), the direct mode is as fast for popup-sized buffers.
pub struct Rasterizer {
    renderer: EguiSoftwareRender,
    pending: TexturesDelta,
}

impl Rasterizer {
    #[must_use]
    pub fn new() -> Self {
        Self {
            renderer: EguiSoftwareRender::new(ColorFieldOrder::Bgra).with_caching(false),
            pending: TexturesDelta::default(),
        }
    }

    /// Keep the texture changes of an egui pass whose frame is not painted
    /// (window hidden, minimized or without area). They are applied, in order
    /// and before the primitives are drawn, by the next [`Self::paint`] that
    /// draws. Dropping them would leave later frames without the font atlas.
    /// Appends with `TexturesDelta::append`.
    pub fn hold(&mut self, delta: TexturesDelta) {
        self.pending.append(delta);
    }

    /// Paint one frame into `pixels`, `width * height` words in row-major
    /// order, one pixel per word as `0xAARRGGBB` (blue in the lowest byte: the
    /// byte order of the renderer is Bgra and of a softbuffer buffer on
    /// little-endian targets). Returns whether a frame was drawn.
    ///
    /// Returns `false` and touches neither `pixels` nor the held texture
    /// changes when `width` or `height` is zero, `pixels.len()` is not
    /// `width * height` or `pixels_per_point` is not a positive finite number
    /// (nothing can be drawn with it). Otherwise: fills every pixel with `clear` made
    /// opaque (its alpha is ignored, so the result has alpha 255 everywhere),
    /// draws `primitives` over it at `pixels_per_point` with the held texture
    /// changes applied first (take them out of `self`, pass them to the
    /// renderer by reference, then `clear()` them), and returns `true`.
    pub fn paint(
        &mut self,
        pixels: &mut [u32],
        width: u32,
        height: u32,
        pixels_per_point: f32,
        clear: Color32,
        primitives: &[ClippedPrimitive],
    ) -> bool {
        if width == 0
            || height == 0
            || pixels.len() != (width as usize) * (height as usize)
            || !pixels_per_point.is_finite()
            || pixels_per_point <= 0.0
        {
            return false;
        }
        let clear = clear.to_opaque();
        let word = 0xFF00_0000
            | (u32::from(clear.r()) << 16)
            | (u32::from(clear.g()) << 8)
            | u32::from(clear.b());
        pixels.fill(word);
        let mut delta = std::mem::take(&mut self.pending);
        let bytes = bytemuck::cast_slice_mut::<u32, [u8; 4]>(pixels);
        let mut buffer = BufferMutRef::new(bytes, width as usize, height as usize);
        self.renderer
            .render(&mut buffer, primitives, &delta, pixels_per_point);
        delta.clear();
        true
    }
}

impl Default for Rasterizer {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Rasterizer {
    /// `TexturesDelta` asserts in debug builds when dropped non-empty: clear
    /// whatever is still held.
    fn drop(&mut self) {
        self.pending.clear();
    }
}

/// The softbuffer surface of one window.
pub struct Presenter {
    surface: softbuffer::Surface<OwnedDisplayHandle, Arc<Window>>,
}

impl Presenter {
    /// # Errors
    /// The softbuffer error when the surface cannot be created.
    pub fn new(
        context: &softbuffer::Context<OwnedDisplayHandle>,
        window: Arc<Window>,
    ) -> Result<Self, softbuffer::SoftBufferError> {
        Ok(Self {
            surface: softbuffer::Surface::new(context, window)?,
        })
    }

    /// Resize the surface to `size`, paint the frame into its buffer with
    /// [`Rasterizer::paint`] and present it. Ok(false) when nothing was shown:
    /// `size` has a zero dimension (a minimized Windows window) or the
    /// rasterizer refused the frame, in which case the buffer is not presented.
    ///
    /// # Errors
    /// The softbuffer error when the surface cannot be resized or its buffer
    /// not be obtained or presented. Texture changes held by `rasterizer`
    /// stay held in that case.
    pub fn present(
        &mut self,
        rasterizer: &mut Rasterizer,
        size: PhysicalSize<u32>,
        pixels_per_point: f32,
        clear: Color32,
        primitives: &[ClippedPrimitive],
    ) -> Result<bool, softbuffer::SoftBufferError> {
        let (Some(width), Some(height)) = (
            std::num::NonZeroU32::new(size.width),
            std::num::NonZeroU32::new(size.height),
        ) else {
            return Ok(false);
        };
        self.surface.resize(width, height)?;
        let mut buffer = self.surface.buffer_mut()?;
        let painted = rasterizer.paint(
            &mut buffer,
            size.width,
            size.height,
            pixels_per_point,
            clear,
            primitives,
        );
        if !painted {
            return Ok(false);
        }
        buffer.present()?;
        Ok(true)
    }
}
