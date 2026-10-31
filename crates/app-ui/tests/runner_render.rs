//! CPU rendering of egui frames into word buffers: pixel format, clearing,
//! texture changes of unpainted passes and in-place texture updates. No
//! window; real egui contexts and the crate's `Rasterizer` only.
//!
//! Run: `cargo test -p clipmonger-app-ui --test runner_render`

use std::panic::{catch_unwind, AssertUnwindSafe};

use clipmonger_app_ui::runner::render::Rasterizer;
use egui::epaint::{ImageDelta, Mesh, Primitive, Vertex};
use egui::{
    ClippedPrimitive, Color32, ColorImage, Context, FullOutput, Pos2, RawInput, Rect, TextureId,
    TextureOptions, TexturesDelta, Vec2,
};

const CLEAR: Color32 = Color32::from_rgb(20, 40, 60);
const RED: Color32 = Color32::from_rgb(200, 30, 90);
const GREEN: Color32 = Color32::from_rgb(10, 220, 40);
const BLUE: Color32 = Color32::from_rgb(25, 60, 240);
const TEXT_PIXELS: usize = 100;

/// `0xAARRGGBB` of an opaque colour.
fn word(color: Color32) -> u32 {
    0xFF00_0000 | (u32::from(color.r()) << 16) | (u32::from(color.g()) << 8) | u32::from(color.b())
}

/// Words with an alpha below 255: never equal to an opaque colour.
fn garbage(len: usize) -> Vec<u32> {
    let mut state = 0x1234_5678_u32;
    (0..len)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state & 0x00FF_FFFF) | (((state >> 24) % 255) << 24)
        })
        .collect()
}

fn size_of(width: u32, height: u32) -> usize {
    usize::try_from(u64::from(width) * u64::from(height)).expect("buffer size")
}

fn index(width: u32, x: u32, y: u32) -> usize {
    usize::try_from(y * width + x).expect("index")
}

fn count_other(pixels: &[u32], color: Color32) -> usize {
    pixels.iter().filter(|&&p| p != word(color)).count()
}

/// Whole-buffer comparison that reports the first difference, not the buffers.
fn assert_same(actual: &[u32], expected: &[u32], width: u32, what: &str) {
    assert_eq!(actual.len(), expected.len(), "{what}: buffer lengths");
    let columns = usize::try_from(width).expect("width");
    let first = actual.iter().zip(expected).position(|(a, b)| a != b);
    assert!(
        first.is_none(),
        "{what}: {} words differ, first at ({}, {}): got {:08X}, expected {:08X}",
        actual.iter().zip(expected).filter(|(a, b)| a != b).count(),
        first.unwrap_or(0) % columns,
        first.unwrap_or(0) / columns,
        actual[first.unwrap_or(0)],
        expected[first.unwrap_or(0)],
    );
}

fn input(size: [f32; 2]) -> RawInput {
    RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(size[0], size[1]))),
        ..RawInput::default()
    }
}

/// What one egui pass hands to the backend.
struct Frame {
    delta: TexturesDelta,
    primitives: Vec<ClippedPrimitive>,
    ppp: f32,
}

fn run(ctx: &Context, size: [f32; 2], ui: impl FnMut(&mut egui::Ui)) -> Frame {
    let FullOutput {
        textures_delta,
        shapes,
        pixels_per_point,
        ..
    } = ctx.run_ui(input(size), ui);
    Frame {
        delta: textures_delta,
        primitives: ctx.tessellate(shapes, pixels_per_point),
        ppp: pixels_per_point,
    }
}

fn label_frame(ctx: &Context, size: [f32; 2], text: &str) -> Frame {
    run(ctx, size, |ui| {
        ui.label(text);
    })
}

/// Holds the frame's texture changes, paints it into a garbage-filled buffer.
fn paint(rasterizer: &mut Rasterizer, frame: Frame, width: u32, height: u32) -> Vec<u32> {
    rasterizer.hold(frame.delta);
    let mut pixels = garbage(size_of(width, height));
    assert!(rasterizer.paint(
        &mut pixels,
        width,
        height,
        frame.ppp,
        CLEAR,
        &frame.primitives
    ));
    pixels
}

fn full_delta(id: TextureId, color: Color32) -> TexturesDelta {
    let mut delta = TexturesDelta::default();
    let image = ColorImage::filled([4, 4], color);
    delta.push(id, ImageDelta::full(image, TextureOptions::NEAREST));
    delta
}

/// An update of the whole 4x4 texture, as a patch.
fn patch_delta(id: TextureId, color: Color32) -> TexturesDelta {
    let mut delta = TexturesDelta::default();
    let image = ColorImage::filled([4, 4], color);
    delta.push(
        id,
        ImageDelta::partial([0, 0], image, TextureOptions::NEAREST),
    );
    delta
}

/// A rectangle of one flat colour, textured with the middle of `texture`.
fn quad(texture: TextureId, rect: Rect, color: Color32) -> ClippedPrimitive {
    let uv = Pos2::new(0.5, 0.5);
    let corners = [
        rect.left_top(),
        rect.right_top(),
        rect.left_bottom(),
        rect.right_bottom(),
    ];
    let mut mesh = Mesh::with_texture(texture);
    mesh.vertices = corners
        .iter()
        .map(|&pos| Vertex { pos, uv, color })
        .collect();
    mesh.indices = vec![0, 1, 2, 2, 1, 3];
    ClippedPrimitive {
        clip_rect: Rect::from_min_size(Pos2::ZERO, Vec2::splat(1000.0)),
        primitive: Primitive::Mesh(mesh),
    }
}

fn square(min: f32, max: f32) -> Rect {
    Rect::from_min_max(Pos2::new(min, min), Pos2::new(max, max))
}

const WHITE_TEXTURE: TextureId = TextureId::Managed(0);
const USER_TEXTURE: TextureId = TextureId::Managed(7);

fn white_texture() -> TexturesDelta {
    let mut delta = TexturesDelta::default();
    let image = ColorImage::filled([1, 1], Color32::WHITE);
    delta.push(
        WHITE_TEXTURE,
        ImageDelta::full(image, TextureOptions::NEAREST),
    );
    delta
}

#[test]
fn solid_colour_is_an_argb_word_in_a_garbage_buffer() {
    let mut rasterizer = Rasterizer::new();
    rasterizer.hold(white_texture());
    let colour = Color32::from_rgb(10, 20, 30);
    let primitives = [quad(WHITE_TEXTURE, square(4.0, 12.0), colour)];
    let mut pixels = garbage(16 * 16);
    assert!(rasterizer.paint(&mut pixels, 16, 16, 1.0, CLEAR, &primitives));
    assert_eq!(word(colour), 0xFF0A_141E, "test helper");
    for y in 0..16 {
        for x in 0..16 {
            let got = pixels[index(16, x, y)];
            let inside = (5..11).contains(&x) && (5..11).contains(&y);
            let outside = !(3..13).contains(&x) || !(3..13).contains(&y);
            if inside {
                assert_eq!(got, 0xFF0A_141E, "inside ({x}, {y}): {got:08X}");
            } else if outside {
                assert_eq!(got, word(CLEAR), "outside ({x}, {y}): {got:08X}");
            }
            assert_eq!(got >> 24, 0xFF, "alpha at ({x}, {y}): {got:08X}");
        }
    }
}

#[test]
fn pixels_are_row_major_in_non_square_buffers() {
    let colour = Color32::from_rgb(10, 20, 30);
    let rect = Rect::from_min_max(Pos2::new(6.0, 2.0), Pos2::new(9.0, 6.0));
    for (width, height) in [(40, 10), (10, 40), (7, 19)] {
        let mut rasterizer = Rasterizer::new();
        rasterizer.hold(white_texture());
        let primitives = [quad(WHITE_TEXTURE, rect, colour)];
        let mut pixels = garbage(size_of(width, height));
        assert!(rasterizer.paint(&mut pixels, width, height, 1.0, CLEAR, &primitives));
        for y in 0..height {
            for x in 0..width {
                let got = pixels[index(width, x, y)];
                if (6..9).contains(&x) && (2..6).contains(&y) {
                    assert_eq!(got, word(colour), "{width}x{height} inside ({x}, {y})");
                } else if !(5..10).contains(&x) || !(1..7).contains(&y) {
                    assert_eq!(got, word(CLEAR), "{width}x{height} outside ({x}, {y})");
                }
            }
        }
    }
}

#[test]
fn clear_fills_every_pixel_of_the_buffer() {
    let mut rasterizer = Rasterizer::new();
    let colours = [
        CLEAR,
        Color32::BLACK,
        Color32::WHITE,
        Color32::from_rgb(1, 2, 3),
        Color32::from_rgb(255, 0, 128),
    ];
    for (width, height) in [(1, 1), (37, 23), (23, 37), (64, 64), (200, 3)] {
        for clear in colours {
            let mut pixels = garbage(size_of(width, height));
            assert!(rasterizer.paint(&mut pixels, width, height, 1.0, clear, &[]));
            assert_eq!(
                count_other(&pixels, clear),
                0,
                "{width}x{height} cleared to {clear:?}"
            );
        }
    }
}

#[test]
fn translucent_clear_still_gives_alpha_255_everywhere() {
    let mut rasterizer = Rasterizer::new();
    rasterizer.hold(white_texture());
    let clear = Color32::from_rgba_unmultiplied(200, 100, 50, 128);
    // Its alpha is ignored; whether the colour stays as stored or is
    // un-multiplied is left to the implementation.
    let accepted = [
        word(Color32::from_rgb(clear.r(), clear.g(), clear.b())),
        word(clear.to_opaque()),
    ];
    let glass = Color32::from_rgba_premultiplied(50, 0, 0, 100);
    let primitives = [quad(WHITE_TEXTURE, square(8.0, 24.0), glass)];
    let mut pixels = garbage(32 * 32);
    assert!(rasterizer.paint(&mut pixels, 32, 32, 1.0, clear, &primitives));
    for (i, &p) in pixels.iter().enumerate() {
        assert_eq!(p >> 24, 0xFF, "alpha of pixel {i}: {p:08X}");
    }
    assert!(accepted.contains(&pixels[0]), "corner {:08X}", pixels[0]);
    assert!(accepted.contains(&pixels[32 * 32 - 1]));
    // A translucent shape over the clear colour is blended, not replaced.
    assert_ne!(pixels[index(32, 16, 16)], pixels[0]);
}

#[test]
fn translucent_shape_over_black_keeps_alpha_255() {
    let mut rasterizer = Rasterizer::new();
    rasterizer.hold(white_texture());
    let glass = Color32::from_rgba_premultiplied(50, 0, 0, 100);
    let primitives = [quad(WHITE_TEXTURE, square(8.0, 24.0), glass)];
    let mut pixels = garbage(32 * 32);
    assert!(rasterizer.paint(&mut pixels, 32, 32, 1.0, Color32::BLACK, &primitives));
    assert_eq!(pixels[index(32, 16, 16)], 0xFF32_0000);
    assert_eq!(pixels[0], 0xFF00_0000);
}

#[test]
fn pixels_per_point_scales_the_primitives() {
    let mut rasterizer = Rasterizer::new();
    rasterizer.hold(white_texture());
    let colour = Color32::from_rgb(10, 20, 30);
    let primitives = [quad(WHITE_TEXTURE, square(2.0, 12.0), colour)];
    let mut at_two = garbage(40 * 40);
    assert!(rasterizer.paint(&mut at_two, 40, 40, 2.0, CLEAR, &primitives));
    assert_eq!(at_two[index(40, 15, 15)], word(colour), "inside at 2.0");
    assert_eq!(at_two[index(40, 6, 6)], word(colour));
    assert_eq!(at_two[index(40, 30, 30)], word(CLEAR), "outside at 2.0");
    let mut at_one = garbage(40 * 40);
    assert!(rasterizer.paint(&mut at_one, 40, 40, 1.0, CLEAR, &primitives));
    assert_eq!(at_one[index(40, 6, 6)], word(colour));
    assert_eq!(at_one[index(40, 15, 15)], word(CLEAR), "outside at 1.0");
}

#[test]
fn paint_is_repeatable_into_the_same_buffer() {
    let mut rasterizer = Rasterizer::new();
    rasterizer.hold(white_texture());
    let glass = Color32::from_rgba_premultiplied(50, 0, 0, 100);
    let primitives = [quad(WHITE_TEXTURE, square(8.0, 24.0), glass)];
    let mut pixels = garbage(32 * 32);
    assert!(rasterizer.paint(&mut pixels, 32, 32, 1.0, CLEAR, &primitives));
    let first = pixels.clone();
    assert!(rasterizer.paint(&mut pixels, 32, 32, 1.0, CLEAR, &primitives));
    assert_same(&pixels, &first, 32, "nothing accumulates between frames");
}

#[test]
fn text_is_visible_in_the_first_frame() {
    let ctx = Context::default();
    let mut rasterizer = Rasterizer::new();
    let frame = label_frame(&ctx, [100.0, 60.0], "Hello world");
    let pixels = paint(&mut rasterizer, frame, 100, 60);
    let lit = count_other(&pixels, CLEAR);
    assert!(
        lit > TEXT_PIXELS,
        "only {lit} pixels differ from the clear colour"
    );
    assert!(pixels.iter().all(|&p| p >> 24 == 0xFF));
    assert_eq!(
        pixels[index(100, 99, 59)],
        word(CLEAR),
        "far corner untouched"
    );
}

#[test]
fn text_stays_when_later_frames_bring_no_texture_changes() {
    let ctx = Context::default();
    let mut rasterizer = Rasterizer::new();
    let first = paint(
        &mut rasterizer,
        label_frame(&ctx, [100.0, 60.0], "Hello world"),
        100,
        60,
    );
    for _ in 0..3 {
        let frame = label_frame(&ctx, [100.0, 60.0], "Hello world");
        assert!(
            frame.delta.is_empty(),
            "the test needs frames without changes"
        );
        assert_same(
            &paint(&mut rasterizer, frame, 100, 60),
            &first,
            100,
            "no changes",
        );
    }
}

#[test]
fn texture_changes_of_an_unpainted_pass_reach_the_first_painted_frame() {
    // Painting every pass.
    let ctx = Context::default();
    let mut always = Rasterizer::new();
    let _ = paint(
        &mut always,
        label_frame(&ctx, [100.0, 60.0], "Hello world"),
        100,
        60,
    );
    let expected = paint(
        &mut always,
        label_frame(&ctx, [100.0, 60.0], "Hello world"),
        100,
        60,
    );
    // The first pass is not painted (hidden window): its changes are held.
    let ctx = Context::default();
    let mut rasterizer = Rasterizer::new();
    let hidden = label_frame(&ctx, [100.0, 60.0], "Hello world");
    assert!(
        !hidden.delta.is_empty(),
        "the first pass carries the font atlas"
    );
    rasterizer.hold(hidden.delta);
    let pixels = paint(
        &mut rasterizer,
        label_frame(&ctx, [100.0, 60.0], "Hello world"),
        100,
        60,
    );
    assert!(count_other(&pixels, CLEAR) > TEXT_PIXELS, "frame is blank");
    assert_same(&pixels, &expected, 100, "against painting every pass");
}

#[test]
fn texture_changes_dropped_with_an_unpainted_pass_leave_a_blank_frame() {
    let ctx = Context::default();
    let mut rasterizer = Rasterizer::new();
    let mut hidden = label_frame(&ctx, [100.0, 60.0], "Hello world");
    hidden.delta.clear();
    let pixels = paint(
        &mut rasterizer,
        label_frame(&ctx, [100.0, 60.0], "Hello world"),
        100,
        60,
    );
    assert_eq!(
        count_other(&pixels, CLEAR),
        0,
        "no font atlas, nothing to draw"
    );
}

#[test]
fn several_unpainted_passes_with_new_glyphs_reach_the_first_painted_frame() {
    let texts = ["Hello", "Hello world 12345", "Zebra quick fox jumps!"];
    let ctx = Context::default();
    let mut always = Rasterizer::new();
    let mut expected = Vec::new();
    for text in texts {
        expected = paint(&mut always, label_frame(&ctx, [160.0, 40.0], text), 160, 40);
    }
    let ctx = Context::default();
    let mut rasterizer = Rasterizer::new();
    for text in &texts[..2] {
        rasterizer.hold(label_frame(&ctx, [160.0, 40.0], text).delta);
    }
    let pixels = paint(
        &mut rasterizer,
        label_frame(&ctx, [160.0, 40.0], texts[2]),
        160,
        40,
    );
    assert!(count_other(&pixels, CLEAR) > TEXT_PIXELS, "frame is blank");
    assert_same(&pixels, &expected, 160, "against painting every pass");
}

/// One pixel in the middle of a quad textured with `USER_TEXTURE` after the
/// held changes `deltas` were applied.
fn texture_pixel(deltas: Vec<TexturesDelta>) -> u32 {
    let mut rasterizer = Rasterizer::new();
    for delta in deltas {
        rasterizer.hold(delta);
    }
    let primitives = [quad(USER_TEXTURE, square(2.0, 14.0), Color32::WHITE)];
    let mut pixels = garbage(16 * 16);
    assert!(rasterizer.paint(&mut pixels, 16, 16, 1.0, CLEAR, &primitives));
    pixels[index(16, 8, 8)]
}

#[test]
fn several_held_deltas_apply_in_order() {
    let id = USER_TEXTURE;
    assert_eq!(texture_pixel(vec![full_delta(id, RED)]), word(RED));
    assert_eq!(
        texture_pixel(vec![full_delta(id, RED), full_delta(id, GREEN)]),
        word(GREEN),
        "a later full upload wins"
    );
    assert_eq!(
        texture_pixel(vec![full_delta(id, RED), patch_delta(id, GREEN)]),
        word(GREEN),
        "a patch applies on top of the held upload"
    );
    assert_eq!(
        texture_pixel(vec![
            full_delta(id, RED),
            patch_delta(id, GREEN),
            patch_delta(id, BLUE),
        ]),
        word(BLUE),
        "the last patch wins"
    );
    assert_eq!(
        texture_pixel(vec![
            full_delta(id, RED),
            patch_delta(id, GREEN),
            full_delta(id, BLUE),
            patch_delta(id, RED),
        ]),
        word(RED)
    );
}

#[test]
fn held_deltas_are_applied_once_and_the_texture_stays() {
    let mut rasterizer = Rasterizer::new();
    rasterizer.hold(full_delta(USER_TEXTURE, GREEN));
    let primitives = [quad(USER_TEXTURE, square(2.0, 14.0), Color32::WHITE)];
    for _ in 0..3 {
        let mut pixels = garbage(16 * 16);
        assert!(rasterizer.paint(&mut pixels, 16, 16, 1.0, CLEAR, &primitives));
        assert_eq!(pixels[index(16, 8, 8)], word(GREEN));
    }
}

#[test]
fn in_place_texture_update_is_visible_in_the_next_frame() {
    let ctx = Context::default();
    let mut rasterizer = Rasterizer::new();
    let mut texture = ctx.load_texture(
        "swatch",
        ColorImage::filled([4, 4], RED),
        TextureOptions::NEAREST,
    );
    let size = [100.0, 60.0];
    let swatch = |ctx: &Context, texture: &egui::TextureHandle| {
        run(ctx, size, |ui| {
            ui.image((texture.id(), Vec2::new(32.0, 32.0)));
        })
    };
    let at = |pixels: &[u32]| pixels[index(100, 16, 16)];
    assert_eq!(
        at(&paint(&mut rasterizer, swatch(&ctx, &texture), 100, 60)),
        word(RED)
    );
    texture.set(ColorImage::filled([4, 4], GREEN), TextureOptions::NEAREST);
    let update = swatch(&ctx, &texture);
    assert!(!update.delta.is_empty(), "the update reaches the backend");
    assert_eq!(at(&paint(&mut rasterizer, update, 100, 60)), word(GREEN));
    texture.set(ColorImage::filled([4, 4], BLUE), TextureOptions::NEAREST);
    assert_eq!(
        at(&paint(&mut rasterizer, swatch(&ctx, &texture), 100, 60)),
        word(BLUE)
    );
    assert_eq!(
        at(&paint(&mut rasterizer, swatch(&ctx, &texture), 100, 60)),
        word(BLUE),
        "a frame without changes keeps it"
    );
}

#[test]
fn long_lived_rasterizer_matches_a_fresh_one_fed_the_same_textures() {
    let ctx = Context::default();
    let mut texture = ctx.load_texture(
        "swatch",
        ColorImage::filled([4, 4], RED),
        TextureOptions::NEAREST,
    );
    let mut long_lived = Rasterizer::new();
    let mut deltas = Vec::new();
    let mut last_primitives = Vec::new();
    let mut last_pixels = Vec::new();
    for (i, update) in [None, None, Some(GREEN), None, Some(BLUE)]
        .into_iter()
        .enumerate()
    {
        if let Some(colour) = update {
            texture.set(ColorImage::filled([4, 4], colour), TextureOptions::NEAREST);
        }
        let frame = run(&ctx, [120.0, 60.0], |ui| {
            ui.image((texture.id(), Vec2::new(32.0, 32.0)));
            ui.label(format!("frame {i}"));
        });
        deltas.push(frame.delta.clone());
        last_primitives.clone_from(&frame.primitives);
        last_pixels = paint(&mut long_lived, frame, 120, 60);
    }
    let mut fresh = Rasterizer::new();
    for delta in deltas {
        fresh.hold(delta);
    }
    let mut pixels = garbage(120 * 60);
    assert!(fresh.paint(&mut pixels, 120, 60, 1.0, CLEAR, &last_primitives));
    assert_eq!(pixels[index(120, 16, 16)], word(BLUE));
    assert!(count_other(&pixels, CLEAR) > TEXT_PIXELS);
    assert_same(&pixels, &last_pixels, 120, "fresh against long-lived");
}

#[test]
fn invalid_input_returns_false_touches_nothing_and_keeps_held_deltas() {
    let ctx = Context::default();
    let frame = label_frame(&ctx, [100.0, 60.0], "Hello world");
    let mut reference = Rasterizer::new();
    reference.hold(frame.delta.clone());
    let mut expected = garbage(100 * 60);
    assert!(reference.paint(&mut expected, 100, 60, frame.ppp, CLEAR, &frame.primitives));
    assert!(count_other(&expected, CLEAR) > TEXT_PIXELS);

    let mut rasterizer = Rasterizer::new();
    rasterizer.hold(frame.delta);
    let ppp = frame.ppp;
    let primitives = &frame.primitives;
    // (width, height, buffer length, pixels per point)
    let cases = [
        (0, 10, 100, ppp),
        (0, 10, 0, ppp),
        (10, 0, 100, ppp),
        (10, 0, 0, ppp),
        (0, 0, 0, ppp),
        (10, 10, 99, ppp),
        (10, 10, 101, ppp),
        (10, 10, 0, ppp),
        (20, 10, 100, ppp),
        (10, 20, 100, ppp),
        (10, 10, 100, 0.0),
        (10, 10, 100, -1.0),
        (10, 10, 100, f32::NAN),
        (10, 10, 100, f32::INFINITY),
        (10, 10, 100, f32::NEG_INFINITY),
    ];
    for (width, height, len, scale) in cases {
        let mut pixels = garbage(len);
        let before = pixels.clone();
        assert!(
            !rasterizer.paint(&mut pixels, width, height, scale, CLEAR, primitives),
            "{width}x{height}, {len} words, ppp {scale}"
        );
        assert_same(
            &pixels,
            &before,
            width.max(1),
            &format!("{width}x{height}, {len} words, ppp {scale}"),
        );
    }
    let mut pixels = garbage(100 * 60);
    assert!(rasterizer.paint(&mut pixels, 100, 60, frame.ppp, CLEAR, primitives));
    assert_same(
        &pixels,
        &expected,
        100,
        "held deltas survived the refused calls",
    );
}

#[test]
fn size_change_between_frames_does_not_panic() {
    let ctx = Context::default();
    let mut rasterizer = Rasterizer::new();
    let sizes: [(u16, u16); 7] = [
        (64, 48),
        (96, 72),
        (32, 24),
        (64, 48),
        (1, 1),
        (200, 30),
        (64, 48),
    ];
    for (width, height) in sizes {
        let size = [f32::from(width), f32::from(height)];
        let (width, height) = (u32::from(width), u32::from(height));
        let pixels = paint(
            &mut rasterizer,
            label_frame(&ctx, size, "Hello"),
            width,
            height,
        );
        assert_eq!(pixels.len(), size_of(width, height));
        assert!(pixels.iter().all(|&p| p >> 24 == 0xFF), "{width}x{height}");
    }
    let pixels = paint(
        &mut rasterizer,
        label_frame(&ctx, [64.0, 48.0], "Hello"),
        64,
        48,
    );
    assert!(
        count_other(&pixels, CLEAR) > TEXT_PIXELS / 4,
        "text after resizes"
    );
}

#[test]
fn dropping_a_rasterizer_with_held_deltas_does_not_panic() {
    let mut rasterizer = Rasterizer::new();
    rasterizer.hold(full_delta(USER_TEXTURE, RED));
    rasterizer.hold(patch_delta(USER_TEXTURE, GREEN));
    let dropped = catch_unwind(AssertUnwindSafe(move || drop(rasterizer)));
    assert!(dropped.is_ok(), "drop panicked with held texture changes");
    let ctx = Context::default();
    let mut rasterizer = Rasterizer::new();
    rasterizer.hold(label_frame(&ctx, [100.0, 60.0], "Hello").delta);
    let dropped = catch_unwind(AssertUnwindSafe(move || drop(rasterizer)));
    assert!(dropped.is_ok(), "drop panicked with held font changes");
}

#[test]
fn holding_an_empty_delta_changes_nothing() {
    let mut rasterizer = Rasterizer::new();
    rasterizer.hold(TexturesDelta::default());
    rasterizer.hold(full_delta(USER_TEXTURE, GREEN));
    rasterizer.hold(TexturesDelta::default());
    let primitives = [quad(USER_TEXTURE, square(2.0, 14.0), Color32::WHITE)];
    let mut pixels = garbage(16 * 16);
    assert!(rasterizer.paint(&mut pixels, 16, 16, 1.0, CLEAR, &primitives));
    assert_eq!(pixels[index(16, 8, 8)], word(GREEN));
}
