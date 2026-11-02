//! Typed autostart configuration tests (Task22 Part1).
//! Covers default false, save/load, old [autostart] + unknown roundtrip,
//! root newer version, wrong typed rejection, and env true/false/invalid.
//! Env cases use isolated subprocess children to avoid global env races.

use clipmonger_core::config::Config;
use std::path::PathBuf;
use std::process::Command;
use uuid::Uuid;

struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("cm-autostart-{}", Uuid::now_v7()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl std::ops::Deref for ScratchDir {
    type Target = std::path::Path;
    fn deref(&self) -> &Self::Target { &self.0 }
}

impl Drop for ScratchDir {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

fn run_isolated(case: &str, autostart_env: Option<&str>) {
    let dir = ScratchDir::new();
    let marker = dir.join("completed");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", "autostart_isolated_helper", "--ignored", "--nocapture"])
        .env_clear()
        .env("TMPDIR", &dir.0)
        .env("TMP", &dir.0)
        .env("TEMP", &dir.0)
        .env("CM_AUTOSTART_CASE", case)
        .env("CM_AUTOSTART_MARKER", &marker);
    #[cfg(windows)]
    if let Some(root) = std::env::var_os("SystemRoot") { command.env("SystemRoot", root); }
    if let Some(value) = autostart_env { command.env("CLIPMONGER_AUTOSTART_ENABLED", value); }
    let output = command.output().expect("failed to start isolated config helper");
    assert!(output.status.success() && std::fs::read_to_string(&marker).ok().as_deref() == Some(case),
        "isolated case {case} failed or did not run; status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status, String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
}

fn case_defaults_to_disabled_when_missing() {
    let cfg = Config::default();
    assert_eq!(cfg.autostart.enabled, false);
    assert!(cfg.autostart.extra.is_empty());
    let parsed: Config = toml::from_str("").unwrap();
    assert_eq!(parsed.autostart.enabled, false);
    let parsed2: Config = toml::from_str("[monitor]\nmax_bytes = 1024\n").unwrap();
    assert_eq!(parsed2.autostart.enabled, false);
    let dir = ScratchDir::new();
    let cfg_missing = Config::load(&dir.join("missing.toml")).unwrap();
    assert_eq!(cfg_missing.autostart.enabled, false);
}

fn case_enabled_true_save_load_roundtrip() {
    let dir = ScratchDir::new();
    let path = dir.join("config.toml");
    let mut cfg = Config::default();
    cfg.autostart.enabled = true;
    cfg.save(&path).unwrap();
    let loaded = Config::load(&path).unwrap();
    assert_eq!(loaded.autostart.enabled, true);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("enabled = true"), "saved text missing enabled true: {text}");
    let reloaded: Config = toml::from_str(&text).unwrap();
    assert_eq!(reloaded.autostart.enabled, true);
    assert_eq!(reloaded.autostart.extra.len(), 0);
}

fn case_old_section_with_unknown_keys_roundtrip_and_root_preserved() {
    let dir = ScratchDir::new();
    let path = dir.join("config.toml");
    let raw = r#"
config_version = 1

[autostart]
enabled = true
unknown_keep = "keep-me"
extra_flag = 123

[history]
max_items = 500

[tray]
enabled = true
"#;
    std::fs::write(&path, raw).unwrap();
    let loaded = Config::load(&path).unwrap();
    assert_eq!(loaded.autostart.enabled, true);
    assert_eq!(loaded.autostart.extra.get("unknown_keep").and_then(|v| v.as_str()), Some("keep-me"));
    assert_eq!(loaded.autostart.extra.get("extra_flag").and_then(|v| v.as_integer()), Some(123));
    assert_eq!(loaded.history.max_items, 500);
    assert!(loaded.extra.contains_key("tray"), "root extra missing tray: {:?}", loaded.extra);
    loaded.save(&path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("keep-me"), "unknown autostart key not preserved: {text}");
    assert!(text.contains("extra_flag"), "unknown autostart flag not preserved: {text}");
    assert!(text.contains("tray"), "root unknown section not preserved: {text}");
    let reloaded = Config::load(&path).unwrap();
    assert_eq!(reloaded.autostart.enabled, true);
    assert_eq!(reloaded.autostart.extra.get("unknown_keep").and_then(|v| v.as_str()), Some("keep-me"));
    assert_eq!(reloaded.history.max_items, 500);
}

fn case_unknown_root_and_newer_version_flag() {
    let dir = ScratchDir::new();
    let path = dir.join("config.toml");
    let raw = r#"
config_version = 99
future_root = "retained"
[autostart]
enabled = false
future_autostart = "keep"
[history]
max_items = 10
"#;
    std::fs::write(&path, raw).unwrap();
    let loaded = Config::load(&path).unwrap();
    assert!(loaded.warned_newer_version, "newer version flag not set");
    assert_eq!(loaded.config_version, 99);
    assert_eq!(loaded.autostart.enabled, false);
    assert_eq!(loaded.autostart.extra.get("future_autostart").and_then(|v| v.as_str()), Some("keep"));
    assert_eq!(loaded.extra.get("future_root").and_then(|v| v.as_str()), Some("retained"));
    loaded.save(&path).unwrap();
    let reloaded = Config::load(&path).unwrap();
    assert_eq!(reloaded.config_version, 99);
    assert_eq!(reloaded.extra.get("future_root").and_then(|v| v.as_str()), Some("retained"));
    assert_eq!(reloaded.autostart.enabled, false);
}

fn case_wrong_typed_value_rejected() {
    let bad_cases = vec![
        "[autostart]\nenabled = \"true\"\n",
        "[autostart]\nenabled = \"yes\"\n",
        "[autostart]\nenabled = 1\n",
        "[autostart]\nenabled = 0\n",
    ];
    for raw in &bad_cases {
        let parsed: Result<Config, _> = toml::from_str(raw);
        assert!(parsed.is_err(), "should reject bool as string/int: {raw} got {parsed:?}");
        let dir = ScratchDir::new();
        let path = dir.join("config.toml");
        std::fs::write(&path, raw).unwrap();
        let loaded = Config::load(&path);
        assert!(loaded.is_err(), "Config::load should fail on wrong type: {raw}");
    }
    // config.set path: JSON roundtrip must also reject wrong types
    let cfg = Config::default();
    let mut root = serde_json::to_value(&cfg).unwrap();
    root["autostart"]["enabled"] = serde_json::json!("true");
    let result: Result<Config, _> = serde_json::from_value(root);
    assert!(result.is_err(), "JSON string for bool should fail");
}

fn case_env_true() {
    let dir = ScratchDir::new();
    let path = dir.join("config.toml");
    let mut cfg = Config::default();
    cfg.autostart.enabled = false;
    cfg.save(&path).unwrap();
    assert_eq!(std::env::var("CLIPMONGER_AUTOSTART_ENABLED").unwrap(), "true");
    let loaded = Config::load(&path).unwrap();
    assert_eq!(loaded.autostart.enabled, true);
}

fn case_env_false() {
    let dir = ScratchDir::new();
    let path = dir.join("config.toml");
    let mut cfg = Config::default();
    cfg.autostart.enabled = true;
    cfg.save(&path).unwrap();
    assert_eq!(std::env::var("CLIPMONGER_AUTOSTART_ENABLED").unwrap(), "false");
    let loaded = Config::load(&path).unwrap();
    assert_eq!(loaded.autostart.enabled, false);
}

fn case_env_invalid() {
    let dir = ScratchDir::new();
    let path = dir.join("config.toml");
    let mut cfg_true = Config::default();
    cfg_true.autostart.enabled = true;
    cfg_true.save(&path).unwrap();
    let loaded_true = Config::load(&path).unwrap();
    assert_eq!(loaded_true.autostart.enabled, true, "invalid env must retain true file value");

    let mut cfg_false = Config::default();
    cfg_false.autostart.enabled = false;
    cfg_false.save(&path).unwrap();
    let loaded_false = Config::load(&path).unwrap();
    assert_eq!(loaded_false.autostart.enabled, false, "invalid env must retain false file value");
}

fn case_save_preserves_enabled_false_explicit() {
    let dir = ScratchDir::new();
    let path = dir.join("config.toml");
    let mut cfg = Config::default();
    cfg.autostart.enabled = false;
    cfg.save(&path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("[autostart]"), "missing autostart section: {text}");
    assert!(text.contains("enabled = false"), "explicit false not saved: {text}");
    let loaded = Config::load(&path).unwrap();
    assert_eq!(loaded.autostart.enabled, false);
}

#[test]
fn autostart_defaults_to_disabled_when_missing() { run_isolated("defaults_to_disabled_when_missing", None); }

#[test]
fn autostart_enabled_true_save_load_roundtrip() { run_isolated("enabled_true_save_load_roundtrip", None); }

#[test]
fn autostart_old_section_with_unknown_keys_roundtrip_and_root_preserved() { run_isolated("old_section_with_unknown_keys_roundtrip_and_root_preserved", None); }

#[test]
fn autostart_unknown_root_and_newer_version_flag() { run_isolated("unknown_root_and_newer_version_flag", None); }

#[test]
fn autostart_wrong_typed_value_rejected() { run_isolated("wrong_typed_value_rejected", None); }

#[test]
fn autostart_save_preserves_enabled_false_explicit() { run_isolated("save_preserves_enabled_false_explicit", None); }

#[test]
fn autostart_env_true() { run_isolated("env_true", Some("true")); }

#[test]
fn autostart_env_false() { run_isolated("env_false", Some("false")); }

#[test]
fn autostart_env_invalid() { run_isolated("env_invalid", Some("notabool")); }

#[test]
#[ignore = "invoked only by the isolated parent tests"]
fn autostart_isolated_helper() {
    let case = std::env::var("CM_AUTOSTART_CASE").unwrap();
    match case.as_str() {
        "defaults_to_disabled_when_missing" => case_defaults_to_disabled_when_missing(),
        "enabled_true_save_load_roundtrip" => case_enabled_true_save_load_roundtrip(),
        "old_section_with_unknown_keys_roundtrip_and_root_preserved" => case_old_section_with_unknown_keys_roundtrip_and_root_preserved(),
        "unknown_root_and_newer_version_flag" => case_unknown_root_and_newer_version_flag(),
        "wrong_typed_value_rejected" => case_wrong_typed_value_rejected(),
        "save_preserves_enabled_false_explicit" => case_save_preserves_enabled_false_explicit(),
        "env_true" => case_env_true(),
        "env_false" => case_env_false(),
        "env_invalid" => case_env_invalid(),
        _ => panic!("unknown isolated case: {case}"),
    }
    std::fs::write(std::env::var_os("CM_AUTOSTART_MARKER").unwrap(), case).unwrap();
}
