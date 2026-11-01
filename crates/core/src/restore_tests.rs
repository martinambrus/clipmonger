use super::{CoreError, PreparedRestore, RestoreFault, Store, DB_NAME, RESTORE_FAULTS};
use crate::model::{Clip, Rev};
use rusqlite::Connection;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use uuid::Uuid;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("restore-tx-{}", Uuid::now_v7()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        RESTORE_FAULTS.with(|faults| faults.borrow_mut().clear());
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn clip(label: &str) -> Clip {
    let id = Uuid::now_v7();
    Clip {
        id,
        created_ms: 1_700_000_000_000,
        source_app: None,
        source_os: "test".into(),
        pinned: true,
        removed: false,
        preview: label.into(),
        search_text: label.into(),
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
    }
}
fn seed(store: &Store, label: &str, byte: u8) -> Uuid {
    let clip = clip(label);
    store
        .insert_clip(&clip, &[("image/png".into(), vec![byte; 300_000])])
        .unwrap();
    clip.id
}
fn faults(points: &[RestoreFault]) {
    RESTORE_FAULTS.with(|faults| *faults.borrow_mut() = points.to_vec());
}
fn setup(scratch: &Scratch) -> (PathBuf, PathBuf, Uuid) {
    let source = Store::open(&scratch.0.join("source")).unwrap();
    seed(&source, "replacement", 7);
    let archive = scratch.0.join("source.zip");
    source.backup_zip(&archive).unwrap();
    let profile = scratch.0.join("destination");
    let original = Store::open(&profile).unwrap();
    let id = seed(&original, "original", 9);
    original.close().unwrap();
    for name in [
        "config.toml",
        "audit.log",
        "daemon.lock",
        "rendezvous.json",
        "clipstore.sqlite3.prev",
        "blobs.prev",
        ".clipstore.prev.sqlite3",
    ] {
        fs::write(profile.join(name), name.as_bytes()).unwrap();
    }
    fs::create_dir(profile.join(".blobs.prev")).unwrap();
    fs::write(profile.join(".blobs.prev/keep"), b"previous blobs").unwrap();
    fs::create_dir(profile.join(".restore-older")).unwrap();
    fs::write(profile.join(".restore-older/keep"), b"older recovery").unwrap();
    (archive, profile, id)
}
fn assert_original(store: &Store, id: Uuid, profile: &Path) {
    let (clip, payloads) = store.get_clip(id).unwrap().unwrap();
    assert_eq!(clip.preview, "original");
    assert!(clip.pinned);
    assert_eq!(payloads, vec![("image/png".into(), vec![9; 300_000])]);
    assert_eq!(store.list_history(100, 0, true).unwrap().len(), 1);
    for name in [
        "config.toml",
        "audit.log",
        "daemon.lock",
        "rendezvous.json",
        "clipstore.sqlite3.prev",
        "blobs.prev",
        ".clipstore.prev.sqlite3",
    ] {
        assert_eq!(fs::read(profile.join(name)).unwrap(), name.as_bytes());
    }
    assert_eq!(
        fs::read(profile.join(".blobs.prev/keep")).unwrap(),
        b"previous blobs"
    );
    assert_eq!(
        fs::read(profile.join(".restore-older/keep")).unwrap(),
        b"older recovery"
    );
}

#[test]
fn rollback_each_old_and_new_move_and_open_preserves_logical_data() {
    for fault in [
        RestoreFault::OldMove(0),
        RestoreFault::OldMove(1),
        RestoreFault::OldMove(2),
        RestoreFault::OldMove(3),
        RestoreFault::NewMove(0),
        RestoreFault::NewMove(1),
        RestoreFault::NewOpen,
    ] {
        let scratch = Scratch::new();
        let (archive, profile, _) = setup(&scratch);
        // A child exits without destructors, leaving committed original data in WAL.
        fs::remove_file(profile.join(DB_NAME)).unwrap();
        fs::remove_dir_all(profile.join("blobs")).unwrap();
        spawn_wal_child(&profile);
        assert!(
            fs::metadata(profile.join("clipstore.sqlite3-wal"))
                .unwrap()
                .len()
                > 0
        );
        assert!(profile.join("clipstore.sqlite3-shm").exists());
        let archive_before = fs::read(&archive).unwrap();
        let preparation = Store::prepare_restore(&archive, &profile).unwrap();
        faults(&[fault]);
        let failure = preparation.install_and_open().unwrap_err();
        assert!(failure.recovery_dir.is_none(), "{fault:?}: {failure:?}");
        let store = failure.store.unwrap();
        let id = store.list_history(100, 0, true).unwrap()[0].id;
        assert_original(&store, id, &profile);
        let new_id = seed(&store, "later capture", 11);
        assert!(store.get_clip(new_id).unwrap().is_some());
        assert_eq!(fs::read(&archive).unwrap(), archive_before);
    }
}

#[test]
fn real_later_move_failure_rolls_back_completed_moves() {
    for name in ["clipstore.sqlite3-wal", "clipstore.sqlite3-shm", "blobs"] {
        let scratch = Scratch::new();
        let (archive, profile, _) = setup(&scratch);
        fs::remove_file(profile.join(DB_NAME)).unwrap();
        fs::remove_dir_all(profile.join("blobs")).unwrap();
        spawn_wal_child(&profile);
        let prepared = Store::prepare_restore(&archive, &profile).unwrap();
        let blocked = prepared.old_dir.join(name);
        fs::create_dir(&blocked).unwrap();
        fs::write(blocked.join("block"), b"owned obstacle").unwrap();
        let failure = prepared.install_and_open().unwrap_err();
        assert!(matches!(failure.error, CoreError::Io(_)), "{failure:?}");
        let store = failure.store.unwrap();
        let id = store.list_history(100, 0, true).unwrap()[0].id;
        assert_original(&store, id, &profile);
    }
}

#[test]
fn actual_failed_candidate_open_cleans_candidate_sidecars_before_old_wal() {
    let scratch = Scratch::new();
    let (archive, profile, _) = setup(&scratch);
    fs::remove_file(profile.join(DB_NAME)).unwrap();
    fs::remove_dir_all(profile.join("blobs")).unwrap();
    spawn_wal_child(&profile);
    let prepared = Store::prepare_restore(&archive, &profile).unwrap();
    Connection::open(prepared.incoming.join(DB_NAME))
        .unwrap()
        .execute_batch("PRAGMA user_version=99")
        .unwrap();
    let failure = prepared.install_and_open().unwrap_err();
    assert!(matches!(
        failure.error,
        CoreError::DowngradeRefused { found: 99, .. }
    ));
    let store = failure.store.unwrap();
    let id = store.list_history(100, 0, true).unwrap()[0].id;
    assert_original(&store, id, &profile);
}

#[test]
fn catastrophic_rollback_and_old_reopen_failures_retain_recoverable_files() {
    for second in [
        RestoreFault::RollbackRemove,
        RestoreFault::RollbackRename,
        RestoreFault::OldOpen,
    ] {
        let scratch = Scratch::new();
        let (archive, profile, id) = setup(&scratch);
        faults(&[RestoreFault::NewOpen, second]);
        let failure = Store::prepare_restore(&archive, &profile)
            .unwrap()
            .install_and_open()
            .unwrap_err();
        assert!(failure.store.is_none());
        let recovery = failure.recovery_dir.unwrap();
        assert!(recovery.is_dir());
        assert!(failure.error.to_string().contains("NewOpen"));
        assert!(failure.error.to_string().contains(&format!("{second:?}")));
        let original = if recovery.join("old").join(DB_NAME).exists() {
            recovery.join("old")
        } else {
            profile.clone()
        };
        let store = Store::open(&original).unwrap();
        assert_eq!(
            store.get_clip(id).unwrap().unwrap().1,
            vec![("image/png".into(), vec![9; 300_000])]
        );
        drop(store);
        Store::restore_zip(&archive, &profile).unwrap();
        assert!(recovery.is_dir(), "fresh restore deleted older recovery");
        assert_eq!(
            fs::read(profile.join(".blobs.prev/keep")).unwrap(),
            b"previous blobs"
        );
        assert_eq!(
            fs::read(profile.join(".clipstore.prev.sqlite3")).unwrap(),
            b".clipstore.prev.sqlite3"
        );
        assert_eq!(
            fs::read(profile.join("config.toml")).unwrap(),
            b"config.toml"
        );
    }
}

#[test]
fn actual_rollback_remove_and_rename_failures_are_reported_without_draining() {
    let scratch = Scratch::new();
    let file = scratch.0.join("component");
    fs::write(&file, b"file ancestor").unwrap();
    let mut installed = vec![file.join("child")];
    let mut moved = Vec::new();
    assert!(PreparedRestore::rollback_moves(&mut moved, &mut installed).is_err());
    assert_eq!(installed.len(), 1);
    let backup = scratch.0.join("backup");
    fs::write(&backup, b"original").unwrap();
    let mut moved = vec![(backup.clone(), file.join("child"))];
    assert!(PreparedRestore::rollback_moves(&mut moved, &mut Vec::new()).is_err());
    assert_eq!(moved.len(), 1);
    assert_eq!(fs::read(backup).unwrap(), b"original");
}

#[test]
fn checked_close_failure_returns_original_usable_store() {
    let scratch = Scratch::new();
    let store = Store::open(&scratch.0).unwrap();
    let id = seed(&store, "original", 9);
    faults(&[RestoreFault::Close]);
    let (store, error) = store.close().unwrap_err();
    assert!(error.to_string().contains("Close"));
    assert!(store.get_clip(id).unwrap().is_some());
    store.close().unwrap();
}

#[test]
fn busy_checkpoint_returns_store_without_discarding_committed_wal() {
    let scratch = Scratch::new();
    let store = Store::open(&scratch.0).unwrap();
    seed(&store, "first", 9);
    let reader = Connection::open(scratch.0.join(DB_NAME)).unwrap();
    reader.execute_batch("BEGIN; SELECT * FROM clips").unwrap();
    let id = seed(&store, "later", 11);
    store
        .lock()
        .busy_timeout(std::time::Duration::ZERO)
        .unwrap();
    let (store, error) = store.close().unwrap_err();
    assert!(error.to_string().contains("busy"));
    assert!(store.get_clip(id).unwrap().is_some());
    reader.execute_batch("ROLLBACK").unwrap();
    store.close().unwrap();
}

fn spawn_wal_child(profile: &Path) {
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "store::restore_tests::committed_wal_child",
            "--ignored",
        ])
        .env("CLIPMONGER_TEST_WAL_PROFILE", profile)
        .status()
        .unwrap();
    assert!(status.success());
}
#[test]
#[ignore = "owned subprocess helper: exits without SQLite destructors"]
fn committed_wal_child() {
    let profile = PathBuf::from(std::env::var_os("CLIPMONGER_TEST_WAL_PROFILE").unwrap());
    let store = Store::open(&profile).unwrap();
    seed(&store, "original", 9);
    std::process::exit(0);
}

#[test]
fn early_move_plus_rollback_failure_keeps_all_committed_wal_material() {
    for component in 1..=3 {
        let scratch = Scratch::new();
        let (archive, profile, _) = setup(&scratch);
        fs::remove_file(profile.join(DB_NAME)).unwrap();
        fs::remove_dir_all(profile.join("blobs")).unwrap();
        spawn_wal_child(&profile);
        faults(&[
            RestoreFault::OldMove(component),
            RestoreFault::RollbackRename,
        ]);
        let failure = Store::prepare_restore(&archive, &profile)
            .unwrap()
            .install_and_open()
            .unwrap_err();
        assert!(failure.store.is_none());
        let recovery = failure.recovery_dir.unwrap();
        assert!(recovery.join("old").join(DB_NAME).is_file());
        assert!(failure
            .error
            .to_string()
            .contains(&format!("OldMove({component})")));
        assert!(failure.error.to_string().contains("RollbackRename"));
        // An incomplete rollback may split original components between old/ and
        // their original names. Reconstruct an owned copy to prove recoverability.
        let recovered = scratch.0.join("manual-recovery");
        fs::create_dir(&recovered).unwrap();
        for name in [
            DB_NAME,
            "clipstore.sqlite3-wal",
            "clipstore.sqlite3-shm",
            "blobs",
        ] {
            let saved = recovery.join("old").join(name);
            let source = if saved.exists() {
                saved
            } else {
                profile.join(name)
            };
            if name == "blobs" {
                fs::create_dir(recovered.join(name)).unwrap();
                for entry in fs::read_dir(source).unwrap().map(Result::unwrap) {
                    fs::copy(entry.path(), recovered.join(name).join(entry.file_name())).unwrap();
                }
            } else {
                fs::copy(source, recovered.join(name)).unwrap();
            }
        }
        let store = Store::open(&recovered).unwrap();
        let clips = store.list_history(100, 0, true).unwrap();
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].preview, "original");
        assert_eq!(
            store.get_clip(clips[0].id).unwrap().unwrap().1,
            vec![("image/png".into(), vec![9; 300_000])]
        );
    }
}
