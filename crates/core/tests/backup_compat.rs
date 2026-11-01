//! Restore compatibility with an archive written by zip 4.6.1.

#[path = "support/backup_fixture.rs"]
mod backup_fixture;

use std::io::Write;
use std::path::{Path, PathBuf};

use clipmonger_core::store::Store;
use clipmonger_core::CoreError;
use uuid::Uuid;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("cm-backup-compat-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn restores_zip4_fixture_with_inline_blob_pinned_and_permanent_clips() {
    let scratch = Scratch::new();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/backup-zip4.zip");
    assert!(std::fs::metadata(&fixture).unwrap().len() < 64 * 1024);
    let profile = scratch.0.join("restored");
    Store::restore_zip(&fixture, &profile).unwrap();
    let store = Store::open(&profile).unwrap();
    assert_eq!(store.list_history(10, 0, false).unwrap().len(), 3);
    for (expected, payloads) in backup_fixture::clips() {
        let (actual, actual_payloads) = store.get_clip(expected.id).unwrap().unwrap();
        assert_eq!(actual, expected);
        assert_eq!(actual_payloads, payloads);
    }
    let expected = backup_fixture::permanent();
    assert_eq!(store.get_permanent(expected.id).unwrap(), Some(expected));
    assert_eq!(store.list_permanents(None).unwrap().len(), 1);
    assert_eq!(std::fs::read_dir(profile.join("blobs")).unwrap().count(), 1);
}

#[test]
fn restore_traversal_rejected_as_unsupported_without_escape() {
    let scratch = Scratch::new();
    let archive = scratch.0.join("traversal.zip");
    let escaped = scratch.0.join("escape.txt");
    // restore_zip extracts into a child of temp_dir; this entry would reach
    // our scratch directory through the parent of that extraction directory.
    let entry = format!(
        "../{}/escape.txt",
        scratch.0.file_name().unwrap().to_str().unwrap()
    );
    let mut writer = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
    writer
        .start_file(entry, zip::write::SimpleFileOptions::default())
        .unwrap();
    writer.write_all(b"escape").unwrap();
    writer.finish().unwrap();

    let err = Store::restore_zip(&archive, &scratch.0.join("profile")).unwrap_err();
    let did_escape = escaped.exists();
    assert!(matches!(err, CoreError::Unsupported { .. }), "{err:?}");
    assert!(
        !did_escape,
        "traversal entry escaped the extraction directory"
    );
}

#[cfg(unix)]
#[test]
fn restore_symlink_rejected_as_unsupported() {
    let scratch = Scratch::new();
    let archive = scratch.0.join("symlink.zip");
    let mut writer = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
    writer
        .add_symlink("link", "target", zip::write::SimpleFileOptions::default())
        .unwrap();
    writer.finish().unwrap();

    let err = Store::restore_zip(&archive, &scratch.0.join("profile")).unwrap_err();
    assert!(matches!(err, CoreError::Unsupported { .. }), "{err:?}");
}

fn profile_files(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn visit(root: &Path, dir: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.push((
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(&path).unwrap(),
                ));
            }
        }
    }
    let mut files = Vec::new();
    visit(root, root, &mut files);
    files.sort();
    files
}

fn assert_prefixed_name_rejected(name: &str) {
    let scratch = Scratch::new();
    let profile = scratch.0.join("profile");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/backup-zip4.zip");
    Store::restore_zip(&fixture, &profile).unwrap();
    let before = profile_files(&profile);
    let archive = scratch.0.join("prefixed.zip");
    let mut writer = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    writer.start_file("must-not-extract.txt", options).unwrap();
    writer
        .write_all(b"validation must finish before extraction")
        .unwrap();
    writer.start_file(name, options).unwrap();
    writer.write_all(b"unsafe name").unwrap();
    writer.finish().unwrap();

    // Corrupt the first entry's CRC in both headers. Any extraction would
    // fail with Io before reaching the unsafe name, rather than Unsupported.
    let mut bytes = std::fs::read(&archive).unwrap();
    let central = bytes.windows(4).position(|b| b == b"PK\x01\x02").unwrap();
    for offset in [14, central + 16] {
        bytes[offset] ^= 1;
    }
    std::fs::write(&archive, bytes).unwrap();

    let err = Store::restore_zip(&archive, &profile).unwrap_err();
    assert_eq!(
        profile_files(&profile),
        before,
        "profile changed for {name:?}"
    );
    assert!(!profile.join("abs.txt").exists());
    assert!(!profile.join("must-not-extract.txt").exists());
    assert!(
        matches!(err, CoreError::Unsupported { .. }),
        "{name:?}: {err:?}"
    );
}

#[test]
fn restore_prefixed_name_posix_root_rejected_before_extraction() {
    assert_prefixed_name_rejected("/abs.txt");
}

#[test]
fn restore_prefixed_name_windows_root_rejected_before_extraction() {
    assert_prefixed_name_rejected(r"\abs.txt");
}

#[test]
fn restore_prefixed_name_drive_rejected_before_extraction() {
    assert_prefixed_name_rejected(r"C:\abs.txt");
}

#[test]
fn restore_prefixed_name_unc_rejected_before_extraction() {
    assert_prefixed_name_rejected(r"\\srv\share\abs.txt");
}

#[test]
fn restore_prefixed_name_verbatim_rejected_before_extraction() {
    assert_prefixed_name_rejected(r"\\?\C:\abs.txt");
}
