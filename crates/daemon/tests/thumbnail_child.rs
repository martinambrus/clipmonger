use clipmonger_daemon::thumbnail::{ThumbnailService, ThumbnailError};
use image::ImageEncoder;
use std::time::Duration;

#[test]
fn production_child_returns_real_pixels_and_rejects_bad_input() {
    let executable = std::path::PathBuf::from(env!("CARGO_BIN_EXE_clipmonger-daemon"));
    let service = ThumbnailService::new_with_exe(executable, Duration::from_secs(3));
    let image = image::RgbaImage::from_pixel(4, 2, image::Rgba([10, 20, 30, 255]));
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(image.as_raw(), 4, 2, image::ExtendedColorType::Rgba8).unwrap();
    let output = service.decode(&png, 8).unwrap();
    assert_eq!((output.width, output.height), (4, 2));
    let decoded = image::load_from_memory(&output.png).unwrap().to_rgba8();
    assert_eq!(decoded.dimensions(), (4, 2));
    assert_eq!(decoded.get_pixel(0, 0).0, [10, 20, 30, 255]);
    assert!(matches!(service.decode(b"not PNG", 8), Err(ThumbnailError::DecodeFailed(_))));
    assert!(matches!(service.decode(b"\x89PNG\r\n\x1a\n", 8), Err(ThumbnailError::DecodeFailed(_))));
    assert!(matches!(service.decode(b"", 8), Err(ThumbnailError::InvalidInput(_))));
}

#[test]
fn early_child_mode_does_not_create_a_profile() {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    let root = std::env::temp_dir().join(format!("clipmonger-thumbnail-profile-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_clipmonger-daemon"));
    command.arg("__thumbnail-decode").env_clear().env("HOME", &root).env("XDG_CONFIG_HOME", &root)
        .env("XDG_DATA_HOME", &root).env("APPDATA", &root).env("LOCALAPPDATA", &root)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    if let Some(root) = std::env::var_os("SystemRoot") { command.env("SystemRoot", root); }
    let mut child = command.spawn().unwrap();
    let mut input = child.stdin.take().unwrap();
    input.write_all(&[1]).unwrap();
    input.write_all(&8u32.to_be_bytes()).unwrap();
    input.write_all(&3u32.to_be_bytes()).unwrap();
    input.write_all(b"bad").unwrap();
    drop(input);
    let mut response = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut response).unwrap();
    assert!(!child.wait().unwrap().success());
    assert_eq!(response.first(), Some(&1));
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    std::fs::remove_dir(root).unwrap();
}
