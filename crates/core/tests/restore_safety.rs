use clipmonger_core::{store::Store, CoreError};
use std::{fs, path::PathBuf};
use uuid::Uuid;
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("restore-safety-{}", Uuid::now_v7()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn archive(&self) -> PathBuf {
        let zip = self.0.join("source.zip");
        Store::open(&self.0.join("source"))
            .unwrap()
            .backup_zip(&zip)
            .unwrap();
        zip
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn dropping_unused_preparation_removes_only_owned_stage() {
    let scratch = Scratch::new();
    let zip = scratch.archive();
    let profile = scratch.0.join("destination");
    fs::create_dir(&profile).unwrap();
    fs::write(profile.join(".restore-older"), b"retain").unwrap();
    let preparation = Store::prepare_restore(&zip, &profile).unwrap();
    let stage = preparation.stage_root().to_owned();
    drop(preparation);
    assert!(!stage.exists(), "unused preparation leaked its owned stage");
    assert_eq!(fs::read(profile.join(".restore-older")).unwrap(), b"retain");
}
#[cfg(unix)]
#[test]
fn trusted_profile_anchor_symlink_is_supported() {
    let scratch = Scratch::new();
    let zip = scratch.archive();
    let real = scratch.0.join("real");
    fs::create_dir(&real).unwrap();
    let link = scratch.0.join("chosen");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    Store::restore_zip(&zip, &link).unwrap();
    assert!(real.join("clipstore.sqlite3").is_file());
    assert!(fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
}
#[test]
fn wrong_managed_component_type_refused_during_preparation() {
    for name in [
        "clipstore.sqlite3",
        "clipstore.sqlite3-wal",
        "clipstore.sqlite3-shm",
        "blobs",
    ] {
        let scratch = Scratch::new();
        let zip = scratch.archive();
        let profile = scratch.0.join("destination");
        fs::create_dir(&profile).unwrap();
        let component = profile.join(name);
        let sentinel = if name == "blobs" {
            component
        } else {
            fs::create_dir(&component).unwrap();
            component.join("keep")
        };
        fs::write(&sentinel, b"wrong type").unwrap();
        assert!(Store::prepare_restore(&zip, &profile).is_err(), "{name}");
        assert_eq!(fs::read(sentinel).unwrap(), b"wrong type");
    }
}

#[test]
fn source_hardlink_to_managed_blob_is_refused_without_changes() {
    let scratch = Scratch::new();
    let zip = scratch.archive();
    let profile = scratch.0.join("destination");
    drop(Store::open(&profile).unwrap());
    fs::hard_link(&zip, profile.join("blobs").join("saved-archive")).unwrap();
    let before = fs::read(profile.join("clipstore.sqlite3")).unwrap();
    let err = Store::restore_zip(&zip, &profile).unwrap_err();
    assert!(matches!(err, CoreError::Unsupported { .. }), "{err}");
    assert_eq!(fs::read(profile.join("clipstore.sqlite3")).unwrap(), before);
    assert!(profile.join("blobs").join("saved-archive").exists());
}

#[cfg(unix)]
#[test]
fn source_symlink_to_managed_blob_is_refused() {
    let scratch = Scratch::new();
    let profile = scratch.0.join("destination");
    drop(Store::open(&profile).unwrap());
    let blob_archive = profile.join("blobs/saved-archive");
    fs::copy(scratch.archive(), &blob_archive).unwrap();
    let alias = scratch.0.join("source-link.zip");
    std::os::unix::fs::symlink(&blob_archive, &alias).unwrap();
    let before = fs::read(&blob_archive).unwrap();
    assert!(matches!(
        Store::restore_zip(&alias, &profile),
        Err(CoreError::Unsupported { .. })
    ));
    assert_eq!(fs::read(&blob_archive).unwrap(), before);
}

#[cfg(target_os = "linux")]
#[test]
fn restore_to_owned_second_filesystem_uses_destination_local_stage() {
    use std::os::unix::fs::MetadataExt as _;
    let scratch = Scratch::new();
    let second = PathBuf::from("/dev/shm");
    if !second.is_dir()
        || fs::metadata(&second).unwrap().dev() == fs::metadata(&scratch.0).unwrap().dev()
    {
        eprintln!("second-filesystem fixture unavailable");
        return;
    }
    let destination = Scratch(second.join(format!("restore-second-fs-{}", Uuid::now_v7())));
    fs::create_dir(&destination.0).unwrap();
    let preparation = Store::prepare_restore(&scratch.archive(), &destination.0).unwrap();
    assert_eq!(
        fs::metadata(preparation.stage_root()).unwrap().dev(),
        fs::metadata(&second).unwrap().dev()
    );
    let store = preparation.install_and_open().unwrap();
    store.integrity_full().unwrap();
    assert!(destination.0.join("clipstore.sqlite3").is_file());
}

#[cfg(unix)]
#[test]
fn managed_component_symlinks_refused_before_moves() {
    for name in [
        "clipstore.sqlite3",
        "clipstore.sqlite3-wal",
        "clipstore.sqlite3-shm",
        "blobs",
    ] {
        let scratch = Scratch::new();
        let zip = scratch.archive();
        let profile = scratch.0.join("destination");
        fs::create_dir(&profile).unwrap();
        let outside = scratch.0.join("sentinel");
        fs::write(&outside, b"untouched").unwrap();
        std::os::unix::fs::symlink(&outside, profile.join(name)).unwrap();
        assert!(
            matches!(
                Store::prepare_restore(&zip, &profile),
                Err(CoreError::Unsupported { .. })
            ),
            "{name}"
        );
        assert_eq!(fs::read(outside).unwrap(), b"untouched");
        assert!(fs::symlink_metadata(profile.join(name))
            .unwrap()
            .file_type()
            .is_symlink());
    }
}

#[cfg(unix)]
#[test]
fn private_preparation_preserves_existing_profile_permissions_on_bad_zip() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = Scratch::new();
    let profile = scratch.0.join("destination");
    fs::create_dir(&profile).unwrap();
    fs::set_permissions(&profile, fs::Permissions::from_mode(0o755)).unwrap();
    let prepared = Store::prepare_restore(&scratch.archive(), &profile).unwrap();
    assert_eq!(
        fs::metadata(prepared.stage_root())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    drop(prepared);
    let invalid = scratch.0.join("bad.zip");
    fs::write(&invalid, b"invalid").unwrap();
    assert!(Store::prepare_restore(&invalid, &profile).is_err());
    assert_eq!(
        fs::metadata(&profile).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert!(fs::read_dir(&profile).unwrap().next().is_none());
}
