//! Clipboard bitmap conversion (pure): Windows device-independent bitmaps
//! (`CF_DIB` / `CF_DIBV5`: an info header followed by pixels, no BMP file
//! header) to and from PNG, the canonical `image/png` payload.

use std::io::Cursor;

use image::codecs::bmp::BmpDecoder;
use image::codecs::png::{PngDecoder, PngEncoder};
use image::{DynamicImage, ExtendedColorType, ImageDecoder, ImageEncoder as _, RgbaImage};

/// Decode guards (seg-03 §8): bigger images are refused before decoding.
const MAX_SIDE: u32 = 8192;
const MAX_PIXELS: u64 = 64_000_000;

/// Largest clipboard bitmap worth reading: the guard's pixel limit at
/// 32 bits per pixel, plus room for headers and a palette.
pub const MAX_DIB_BYTES: u64 = MAX_PIXELS * 4 + 64 * 1024;

/// Decode a clipboard DIB and re-encode it as PNG.
///
/// # Errors
///
/// Returns a message when the DIB cannot be decoded, is over the decode
/// guard, or the PNG cannot be encoded.
pub fn dib_to_png(dib: &[u8]) -> Result<Vec<u8>, String> {
    let decoder = BmpDecoder::new_without_file_header(Cursor::new(dib))
        .map_err(|e| format!("bitmap decode failed: {e}"))?;
    check_size(decoder.dimensions())?;
    let mut rgba = DynamicImage::from_decoder(decoder)
        .map_err(|e| format!("bitmap decode failed: {e}"))?
        .to_rgba8();
    // Some apps publish 32-bit bitmaps whose alpha bytes are all zero; keep
    // those opaque instead of storing an invisible image.
    if rgba.pixels().all(|p| p.0[3] == 0) {
        for p in rgba.pixels_mut() {
            p.0[3] = u8::MAX;
        }
    }
    encode_png(&rgba)
}

/// Decode a PNG and build a `CF_DIB`: a 40-byte `BITMAPINFOHEADER`
/// (32 bpp, `BI_RGB`, bottom-up rows) followed by BGRA pixels.
///
/// # Errors
///
/// Returns a message when the PNG cannot be decoded or is over the decode
/// guard.
pub fn png_to_dib(png: &[u8]) -> Result<Vec<u8>, String> {
    let decoder =
        PngDecoder::new(Cursor::new(png)).map_err(|e| format!("PNG decode failed: {e}"))?;
    check_size(decoder.dimensions())?;
    let rgba = DynamicImage::from_decoder(decoder)
        .map_err(|e| format!("PNG decode failed: {e}"))?
        .to_rgba8();
    let (width, height) = rgba.dimensions();
    let too_large = || "image too large for a bitmap".to_string();
    let pixel_bytes =
        u32::try_from(u64::from(width) * u64::from(height) * 4).map_err(|_| too_large())?;
    let mut dib = Vec::with_capacity(40 + pixel_bytes as usize);
    dib.extend_from_slice(&40_u32.to_le_bytes()); // biSize
    dib.extend_from_slice(&i32::try_from(width).map_err(|_| too_large())?.to_le_bytes());
    // Positive height = bottom-up rows.
    dib.extend_from_slice(
        &i32::try_from(height)
            .map_err(|_| too_large())?
            .to_le_bytes(),
    );
    dib.extend_from_slice(&1_u16.to_le_bytes()); // biPlanes
    dib.extend_from_slice(&32_u16.to_le_bytes()); // biBitCount
    dib.extend_from_slice(&0_u32.to_le_bytes()); // biCompression = BI_RGB
    dib.extend_from_slice(&pixel_bytes.to_le_bytes()); // biSizeImage
    dib.extend_from_slice(&[0; 16]); // resolution, colors used/important
    for y in (0..height).rev() {
        for x in 0..width {
            let [r, g, b, a] = rgba.get_pixel(x, y).0;
            dib.extend_from_slice(&[b, g, r, a]);
        }
    }
    Ok(dib)
}

fn check_size((width, height): (u32, u32)) -> Result<(), String> {
    if width > MAX_SIDE || height > MAX_SIDE || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(format!("image {width}x{height} is over the decode limit"));
    }
    Ok(())
}

fn encode_png(rgba: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(
            rgba.as_raw(),
            rgba.width(),
            rgba.height(),
            ExtendedColorType::Rgba8,
        )
        .map_err(|e| format!("PNG encode failed: {e}"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> RgbaImage {
        RgbaImage::from_fn(3, 2, |x, y| {
            image::Rgba([
                u8::try_from(x * 80).unwrap(),
                u8::try_from(y * 120).unwrap(),
                200,
                255,
            ])
        })
    }

    /// 32 bpp `BI_RGB` `BITMAPINFOHEADER` with no pixel data.
    fn header_only(width: i32, height: i32) -> Vec<u8> {
        let mut dib = Vec::new();
        dib.extend_from_slice(&40_u32.to_le_bytes());
        dib.extend_from_slice(&width.to_le_bytes());
        dib.extend_from_slice(&height.to_le_bytes());
        dib.extend_from_slice(&1_u16.to_le_bytes());
        dib.extend_from_slice(&32_u16.to_le_bytes());
        dib.extend_from_slice(&[0; 24]);
        dib
    }

    #[test]
    fn png_to_dib_to_png_keeps_pixels() {
        let png = encode_png(&sample()).unwrap();
        let back = dib_to_png(&png_to_dib(&png).unwrap()).unwrap();
        let decoded = image::load_from_memory(&back).unwrap().to_rgba8();
        assert_eq!(decoded, sample());
    }

    #[test]
    fn dib_header_is_32bpp_bottom_up_bgra() {
        let dib = png_to_dib(&encode_png(&sample()).unwrap()).unwrap();
        assert_eq!(u32::from_le_bytes(dib[0..4].try_into().unwrap()), 40);
        assert_eq!(i32::from_le_bytes(dib[4..8].try_into().unwrap()), 3);
        assert_eq!(i32::from_le_bytes(dib[8..12].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(dib[14..16].try_into().unwrap()), 32);
        assert_eq!(dib.len(), 40 + 3 * 2 * 4);
        // First stored row is the bottom row (y = 1): pixel (0, 1) in BGRA.
        assert_eq!(&dib[40..44], &[200, 120, 0, 255]);
    }

    #[test]
    fn bitfields_dib_with_zero_alpha_becomes_opaque() {
        // CF_DIBV5: BITMAPV5HEADER, 32 bpp BI_BITFIELDS with an alpha mask
        // and every alpha byte zero, as some apps publish it. BI_BITFIELDS
        // bitmaps repeat the RGB masks right after the header.
        let masks = [0x00FF_0000_u32, 0x0000_FF00, 0x0000_00FF, 0xFF00_0000];
        let mut dib = Vec::new();
        dib.extend_from_slice(&124_u32.to_le_bytes());
        dib.extend_from_slice(&1_i32.to_le_bytes());
        dib.extend_from_slice(&1_i32.to_le_bytes());
        dib.extend_from_slice(&1_u16.to_le_bytes());
        dib.extend_from_slice(&32_u16.to_le_bytes());
        dib.extend_from_slice(&3_u32.to_le_bytes()); // BI_BITFIELDS
        dib.extend_from_slice(&4_u32.to_le_bytes());
        dib.extend_from_slice(&[0; 16]);
        for mask in masks {
            dib.extend_from_slice(&mask.to_le_bytes());
        }
        dib.extend_from_slice(&[0; 68]); // color space .. reserved
        for mask in &masks[..3] {
            dib.extend_from_slice(&mask.to_le_bytes());
        }
        dib.extend_from_slice(&[10, 20, 30, 0]); // B, G, R, A = 0
        let png = dib_to_png(&dib).unwrap();
        let pixel = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(pixel.get_pixel(0, 0).0, [30, 20, 10, 255]);
    }

    fn hex(text: &str) -> Vec<u8> {
        let digits: Vec<u8> = text.bytes().filter(u8::is_ascii_hexdigit).collect();
        digits
            .chunks(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }

    /// Bitmaps Windows 11 synthesized from a 5x3 `CF_BITMAP` (raw
    /// `GetClipboardData` bytes, captured 2026-10-30). Both are 32 bpp
    /// `BI_BITFIELDS` with the RGB masks after the header; the V5 one also
    /// carries them inside its 124-byte header (alpha mask 0).
    const REAL_CF_DIB: &str = "
        28000000050000000300000001002000030000003c0000000000000000000000
        00000000000000000000ff0000ff0000ff000000aaa000ffaaa028ffaaa050ff
        aaa078ffaaa0a0ffaa5000ffaa5028ffaa5050ffaa5078ffaa50a0ffaa0000ff
        aa0028ffaa0050ffaa0078ffaa00a0ff";
    const REAL_CF_DIBV5: &str = "
        7c000000050000000300000001002000030000003c0000000000000000000000
        00000000000000000000ff0000ff0000ff000000000000004247527300000000
        0000000000000000000000000000000000000000000000000000000000000000
        000000000000000000000000040000000000000000000000000000000000ff00
        00ff0000ff000000aaa000ffaaa028ffaaa050ffaaa078ffaaa0a0ffaa5000ff
        aa5028ffaa5050ffaa5078ffaa50a0ffaa0000ffaa0028ffaa0050ffaa0078ff
        aa00a0ff";

    #[test]
    fn real_windows_bitmaps_decode() {
        let expected = RgbaImage::from_fn(5, 3, |x, y| {
            image::Rgba([
                u8::try_from(40 * x).unwrap(),
                u8::try_from(80 * y).unwrap(),
                170,
                255,
            ])
        });
        for (name, dib) in [("CF_DIB", REAL_CF_DIB), ("CF_DIBV5", REAL_CF_DIBV5)] {
            let dib = hex(dib);
            let png = dib_to_png(&dib).unwrap_or_else(|e| panic!("{name}: {e}"));
            let decoded = image::load_from_memory(&png).unwrap().to_rgba8();
            assert_eq!(decoded, expected, "{name}");
        }
    }

    #[test]
    fn decode_guard_refuses_before_reading_pixels() {
        // Headers alone: the guard must fire before any pixel read.
        let err = dib_to_png(&header_only(9000, 1)).unwrap_err();
        assert!(err.contains("decode limit"), "{err}");
        let err = dib_to_png(&header_only(8000, 8001)).unwrap_err();
        assert!(err.contains("decode limit"), "{err}");
        let wide = encode_png(&RgbaImage::new(9000, 1)).unwrap();
        let err = png_to_dib(&wide).unwrap_err();
        assert!(err.contains("decode limit"), "{err}");
        assert!(check_size((8192, 7812)).is_ok());
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(dib_to_png(b"not a bitmap").is_err());
        assert!(png_to_dib(b"not a png").is_err());
    }
}
