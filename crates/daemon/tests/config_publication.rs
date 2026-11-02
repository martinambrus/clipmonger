use clipmonger_core::Store;
use clipmonger_daemon::DaemonState;
use serde_json::json;
use std::{fs, path::PathBuf};
use uuid::Uuid;
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn config_save_failure_does_not_publish_live_settings() {
    let scratch = Scratch(std::env::temp_dir().join(format!("config-save-{}", Uuid::now_v7())));
    let state = DaemonState::new(Store::open(&scratch.0).unwrap());
    state.set_profile_dir(&scratch.0);
    let before = state.handle("config.get", &json!({})).unwrap();
    fs::create_dir(scratch.0.join("config.toml")).unwrap();
    assert!(state
        .handle(
            "config.set",
            &json!({"key":"popup.unicode_limit","value":31})
        )
        .is_err());
    assert_eq!(state.handle("config.get", &json!({})).unwrap(), before);
}
