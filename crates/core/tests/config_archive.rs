use clipmonger_core::{
    model::{Clip, Rev},
    Config, Store,
};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
};
use uuid::Uuid;

const RAW: &[u8] = b"# exact archived comment\nconfig_version = 99\nfuture = 'retained'\n[history]\nmax_items = 3\nfuture_limit = 42\n[popup]\nunicode_limit = 23\ndelivery_dwell_ms = 45\ndelivery = 'unicode_inject'\nrestore_clipboard = false\n";
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("config-archive-{}", Uuid::now_v7()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn archive(&self, config: Option<&[u8]>) -> PathBuf {
        let profile = self.0.join("source");
        let store = Store::open(&profile).unwrap();
        seed(&store);
        if let Some(bytes) = config {
            fs::write(profile.join("config.toml"), bytes).unwrap();
        }
        let archive = self.0.join("source.zip");
        store.backup_zip(&archive).unwrap();
        archive
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn seed(store: &Store) -> Uuid {
    let id = Uuid::now_v7();
    let clip = Clip {
        id,
        created_ms: 1_700_000_000_000,
        source_app: None,
        source_os: "test".into(),
        pinned: true,
        removed: false,
        preview: "original".into(),
        search_text: "original".into(),
        formats: vec!["image/png".into()],
        bytes_total: 300_000,
        content_hash: id.to_string(),
        rev: Rev {
            wall_ms: 1_700_000_000_000,
            dev: "test".into(),
            ctr: 0,
        },
        last_used_ms: 1_700_000_000_000,
        origin_dev: "test".into(),
        origin: "test".into(),
    };
    store
        .insert_clip(&clip, &[("image/png".into(), vec![9; 300_000])])
        .unwrap();
    id
}
fn assert_data(store: &Store, id: Uuid) {
    let (clip, payloads) = store.get_clip(id).unwrap().unwrap();
    assert_eq!(clip.preview, "original");
    assert_eq!(payloads, vec![("image/png".into(), vec![9; 300_000])]);
}
fn entries(path: &Path) -> Vec<(String, Vec<u8>)> {
    let mut zip = zip::ZipArchive::new(fs::File::open(path).unwrap()).unwrap();
    (0..zip.len())
        .map(|index| {
            let mut entry = zip.by_index(index).unwrap();
            let name = entry.name().unwrap().to_string();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            (name, bytes)
        })
        .collect()
}
fn write_zip(path: &Path, entries: &[(String, Vec<u8>)]) {
    let mut zip = zip::ZipWriter::new(fs::File::create(path).unwrap());
    let options = zip::write::SimpleFileOptions::default().unix_permissions(0o600);
    for (name, bytes) in entries {
        zip.start_file(name, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
}
fn get<'a>(entries: &'a [(String, Vec<u8>)], name: &str) -> &'a [u8] {
    &entries.iter().find(|(n, _)| n == name).unwrap().1
}
#[test]
fn raw_config_bytes_metadata_and_roundtrip_are_exact() {
    let scratch = Scratch::new();
    let archive = scratch.archive(Some(RAW));
    let entries = entries(&archive);
    assert_eq!(get(&entries, "config.toml"), RAW);
    let manifest: toml::Value =
        toml::from_str(std::str::from_utf8(get(&entries, "manifest.toml")).unwrap()).unwrap();
    assert_eq!(
        manifest["config_bytes"].as_integer().unwrap(),
        i64::try_from(RAW.len()).unwrap()
    );
    let mut digest = String::new();
    for byte in Sha256::digest(RAW) {
        write!(&mut digest, "{byte:02x}").unwrap();
    }
    assert_eq!(manifest["config_sha256"].as_str().unwrap(), digest);
    let dest = scratch.0.join("destination");
    Store::restore_zip(&archive, &dest).unwrap();
    assert_eq!(fs::read(dest.join("config.toml")).unwrap(), RAW);
    let restored = Config::load(&dest.join("config.toml")).unwrap();
    assert!(restored.warned_newer_version);
    assert_eq!(restored.extra["future"].as_str(), Some("retained"));
}
#[test]
fn archive_reads_raw_file_in_owned_env_child() {
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "raw_environment_child", "--ignored"])
        .env("CLIPMONGER_HISTORY_MAX_ITEMS", "777")
        .status()
        .unwrap();
    assert!(status.success());
}
#[test]
#[ignore = "owned environment-isolated helper"]
fn raw_environment_child() {
    let scratch = Scratch::new();
    let archive = scratch.archive(Some(RAW));
    assert_eq!(
        Config::load(&scratch.0.join("source/config.toml"))
            .unwrap()
            .history
            .max_items,
        777
    );
    assert_eq!(get(&entries(&archive), "config.toml"), RAW);
}
#[test]
fn invalid_source_config_never_truncates_existing_zip() {
    let scratch = Scratch::new();
    let store = Store::open(&scratch.0.join("source")).unwrap();
    let config = scratch.0.join("source/config.toml");
    let zip = scratch.0.join("existing.zip");
    for bytes in [
        b"history = [".to_vec(),
        vec![0xff],
        vec![b' '; 1024 * 1024 + 1],
    ] {
        fs::write(&config, bytes).unwrap();
        fs::write(&zip, b"existing ZIP sentinel").unwrap();
        assert!(store.backup_zip(&zip).is_err());
        assert_eq!(fs::read(&zip).unwrap(), b"existing ZIP sentinel");
    }
    fs::remove_file(&config).unwrap();
    fs::create_dir(&config).unwrap();
    assert!(store.backup_zip(&zip).is_err());
    assert_eq!(fs::read(&zip).unwrap(), b"existing ZIP sentinel");
}
#[cfg(unix)]
#[test]
fn source_config_link_and_read_failure_preserve_zip() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let scratch = Scratch::new();
    let store = Store::open(&scratch.0.join("source")).unwrap();
    let config = scratch.0.join("source/config.toml");
    let zip = scratch.0.join("existing.zip");
    fs::write(&zip, b"existing ZIP sentinel").unwrap();
    let target = scratch.0.join("target");
    fs::write(&target, RAW).unwrap();
    symlink(&target, &config).unwrap();
    assert!(store.backup_zip(&zip).is_err());
    assert_eq!(fs::read(&target).unwrap(), RAW);
    fs::remove_file(&config).unwrap();
    fs::write(&config, RAW).unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o0)).unwrap();
    let result = store.backup_zip(&zip);
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(result.is_err(), "owned unreadable file must fail");
    assert_eq!(fs::read(&zip).unwrap(), b"existing ZIP sentinel");
}
#[test]
fn invalid_incoming_config_metadata_and_bytes_are_predestructive() {
    let cases: Vec<(Option<Vec<u8>>, &str)> = vec![
        (Some(RAW.to_vec()), "config_bytes = 'bad'\n"),
        (Some(RAW.to_vec()), "config_bytes = -1\n"),
        (Some(RAW.to_vec()), "config_bytes = 1\n"),
        (Some(RAW.to_vec()), "config_sha256 = 42\n"),
        (Some(RAW.to_vec()), "config_sha256 = 'ABC'\n"),
        (
            Some(RAW.to_vec()),
            "config_sha256 = '0000000000000000000000000000000000000000000000000000000000000000'\n",
        ),
        (None, "config_bytes = 1\n"),
        (
            None,
            "config_sha256 = '0000000000000000000000000000000000000000000000000000000000000000'\n",
        ),
        (Some(vec![0xff]), ""),
        (Some(b"[history\n".to_vec()), ""),
        (Some(b"[history]\nmax_items = 'wrong type'".to_vec()), ""),
        (Some(vec![b' '; 1024 * 1024 + 1]), ""),
    ];
    for (config, metadata) in cases {
        let scratch = Scratch::new();
        let archive = scratch.archive(None);
        let mut content = entries(&archive);
        content
            .iter_mut()
            .find(|(n, _)| n == "manifest.toml")
            .unwrap()
            .1
            .extend_from_slice(metadata.as_bytes());
        if let Some(config) = config {
            content.push(("config.toml".into(), config));
        }
        write_zip(&archive, &content);
        let archive_before = fs::read(&archive).unwrap();
        let dest = scratch.0.join("destination");
        let store = Store::open(&dest).unwrap();
        let id = seed(&store);
        fs::write(dest.join("config.toml"), b"# original config").unwrap();
        assert!(
            Store::prepare_restore(&archive, &dest).is_err(),
            "{metadata}"
        );
        assert_data(&store, id);
        assert_eq!(
            fs::read(dest.join("config.toml")).unwrap(),
            b"# original config"
        );
        assert_eq!(fs::read(&archive).unwrap(), archive_before);
    }
}
#[test]
fn present_config_without_optional_metadata_is_accepted() {
    let scratch = Scratch::new();
    let archive = scratch.archive(None);
    let mut content = entries(&archive);
    content.push(("config.toml".into(), RAW.to_vec()));
    write_zip(&archive, &content);
    let dest = scratch.0.join("destination");
    Store::restore_zip(&archive, &dest).unwrap();
    assert_eq!(fs::read(dest.join("config.toml")).unwrap(), RAW);
}
#[test]
fn config_source_aliases_refused_preserving_archive_and_destination() {
    for alias in ["direct", "hardlink", "symlink"] {
        #[cfg(not(unix))]
        if alias == "hardlink" {
            continue;
        }
        let scratch = Scratch::new();
        let archive = scratch.archive(Some(RAW));
        let dest = scratch.0.join("destination");
        let store = Store::open(&dest).unwrap();
        let id = seed(&store);
        let target = dest.join("config.toml");
        let source = match alias {
            "direct" => {
                fs::copy(&archive, &target).unwrap();
                target.clone()
            }
            "hardlink" => {
                fs::hard_link(&archive, &target).unwrap();
                archive.clone()
            }
            _ => {
                fs::copy(&archive, &target).unwrap();
                #[cfg(unix)]
                {
                    let link = scratch.0.join("link.zip");
                    std::os::unix::fs::symlink(&target, &link).unwrap();
                    link
                }
                #[cfg(not(unix))]
                {
                    continue;
                }
            }
        };
        let before = fs::read(&source).unwrap();
        store.close().unwrap();
        let result = Store::restore_zip(&source, &dest);
        // Check actual source/config bytes before inspecting the result: an
        // accepted alias must never silently replace the original ZIP.
        assert_eq!(
            fs::read(&source).unwrap(),
            before,
            "source ZIP changed: {alias}"
        );
        assert_eq!(
            fs::read(&target).unwrap(),
            before,
            "destination config changed: {alias}"
        );
        assert_data(&Store::open(&dest).unwrap(), id);
        assert!(result.is_err(), "source alias was accepted: {alias}");
    }
}
#[test]
fn incoming_config_wrong_destination_type_refused_before_close() {
    let scratch = Scratch::new();
    let archive = scratch.archive(Some(RAW));
    let dest = scratch.0.join("destination");
    let store = Store::open(&dest).unwrap();
    let id = seed(&store);
    fs::create_dir(dest.join("config.toml")).unwrap();
    fs::write(dest.join("config.toml/keep"), b"retain").unwrap();
    assert!(Store::prepare_restore(&archive, &dest).is_err());
    assert_data(&store, id);
    assert_eq!(fs::read(dest.join("config.toml/keep")).unwrap(), b"retain");
}
#[cfg(unix)]
#[test]
fn destination_config_link_is_conditional_and_rechecked_before_first_move() {
    let scratch = Scratch::new();
    let archive = scratch.archive(Some(RAW));
    let dest = scratch.0.join("destination");
    let store = Store::open(&dest).unwrap();
    let id = seed(&store);
    let target = scratch.0.join("outside");
    fs::write(&target, b"retain").unwrap();
    std::os::unix::fs::symlink(&target, dest.join("config.toml")).unwrap();
    assert!(Store::prepare_restore(&archive, &dest).is_err());
    assert_data(&store, id);
    fs::remove_file(dest.join("config.toml")).unwrap();
    let prepared = Store::prepare_restore(&archive, &dest).unwrap();
    std::os::unix::fs::symlink(&target, dest.join("config.toml")).unwrap();
    store.close().unwrap();
    let db_before = fs::read(dest.join("clipstore.sqlite3")).unwrap();
    let failure = prepared.install_and_open().unwrap_err();
    assert!(failure.store.is_some());
    assert!(failure.recovery_dir.is_none());
    assert_eq!(fs::read(dest.join("clipstore.sqlite3")).unwrap(), db_before);
    assert_eq!(fs::read(&target).unwrap(), b"retain");
    let legacy = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/backup-zip4.zip");
    failure.store.unwrap().close().unwrap();
    Store::restore_zip(&legacy, &dest).unwrap();
    assert!(fs::symlink_metadata(dest.join("config.toml"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read(target).unwrap(), b"retain");
}
#[test]
fn zip4_preserves_existing_config_and_fresh_absence() {
    let scratch = Scratch::new();
    let archive = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/backup-zip4.zip");
    let dest = scratch.0.join("existing");
    fs::create_dir(&dest).unwrap();
    fs::write(dest.join("config.toml"), RAW).unwrap();
    Store::restore_zip(&archive, &dest).unwrap();
    assert_eq!(fs::read(dest.join("config.toml")).unwrap(), RAW);
    let fresh = scratch.0.join("fresh");
    Store::restore_zip(&archive, &fresh).unwrap();
    assert!(!fresh.join("config.toml").exists());
}

#[test]
fn config_archive_wrong_entry_types_are_rejected_without_destination_changes() {
    for link in [false, true] {
        let scratch = Scratch::new();
        let archive = scratch.archive(None);
        let content = entries(&archive);
        let mut zip = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
        let options = zip::write::SimpleFileOptions::default().unix_permissions(0o600);
        for (name, bytes) in content {
            zip.start_file(name, options).unwrap();
            zip.write_all(&bytes).unwrap();
        }
        if link {
            zip.add_symlink("config.toml", "outside", options).unwrap();
        } else {
            zip.add_directory("config.toml/", options).unwrap();
        }
        zip.finish().unwrap();
        let before = fs::read(&archive).unwrap();
        let dest = scratch.0.join("destination");
        let store = Store::open(&dest).unwrap();
        let id = seed(&store);
        fs::write(dest.join("config.toml"), b"original config").unwrap();
        assert!(Store::prepare_restore(&archive, &dest).is_err());
        assert_data(&store, id);
        assert_eq!(
            fs::read(dest.join("config.toml")).unwrap(),
            b"original config"
        );
        assert_eq!(fs::read(&archive).unwrap(), before);
    }
}
#[test]
fn legacy_archive_at_config_path_remains_an_untouched_source() {
    let scratch = Scratch::new();
    let archive = scratch.archive(None);
    let dest = scratch.0.join("destination");
    fs::create_dir(&dest).unwrap();
    let source = dest.join("config.toml");
    fs::copy(&archive, &source).unwrap();
    let before = fs::read(&source).unwrap();
    Store::restore_zip(&source, &dest).unwrap();
    assert_eq!(fs::read(&source).unwrap(), before);
}
