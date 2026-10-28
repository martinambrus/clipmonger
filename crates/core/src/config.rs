//! v1 config: TOML serde structs, env overrides, portable path resolution.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::CoreError;

/// Config file version. Newer files load with a warning flag, never a refusal.
pub const CONFIG_VERSION: u32 = 1;

/// The six canonical MIME ids (seg-03 S2), the default `monitor.formats`.
pub const CANONICAL_FORMATS: [&str; 6] = [
    "text/plain;charset=utf-8",
    "text/plain",
    "text/html",
    "text/rtf",
    "image/png",
    "text/uri-list",
];

fn default_formats() -> Vec<String> {
    CANONICAL_FORMATS.iter().map(ToString::to_string).collect()
}

fn default_max_bytes() -> u64 {
    16 * 1024 * 1024
}

fn default_poll_ms() -> u64 {
    500
}

fn default_primary_debounce_ms() -> u64 {
    500
}

fn default_max_items() -> u64 {
    750
}

fn default_max_store_bytes() -> u64 {
    512 * 1024 * 1024
}

fn default_dedupe_window() -> usize {
    100
}

fn default_hotkey() -> String {
    "ctrl+alt+h".to_string()
}

fn default_placement() -> String {
    "cursor".to_string()
}

fn default_preview_width() -> u32 {
    80
}

fn default_true() -> bool {
    true
}

fn default_unicode_limit() -> u64 {
    65536
}

fn default_delivery_dwell_ms() -> u64 {
    1000
}

fn default_paste_chords() -> Vec<PasteChord> {
    vec![PasteChord {
        app: "*".to_string(),
        chord: "ctrl+v".to_string(),
    }]
}

/// How a paste is delivered to its target.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PasteDelivery {
    /// Write payloads to the clipboard, send Ctrl+V, then guarded-restore.
    #[default]
    ClipboardRestore,
    /// Type the text as synthetic Unicode keystrokes.
    UnicodeInject,
}

/// One paste chord binding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PasteChord {
    #[serde(default)]
    pub app: String,
    #[serde(default)]
    pub chord: String,
}

/// `[monitor]` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorConfig {
    #[serde(default = "default_formats")]
    pub formats: Vec<String>,
    #[serde(default = "default_max_bytes")]
    pub max_bytes: u64,
    #[serde(default)]
    pub min_len: u64,
    /// App classes to ignore. Default empty; per-OS classes land with backends.
    #[serde(default)]
    pub ignores: Vec<String>,
    #[serde(default = "default_poll_ms")]
    pub poll_ms: u64,
    #[serde(default)]
    pub capture_primary: bool,
    #[serde(default = "default_primary_debounce_ms")]
    pub primary_debounce_ms: u64,
    /// Unknown keys preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            formats: default_formats(),
            max_bytes: default_max_bytes(),
            min_len: 0,
            ignores: Vec::new(),
            poll_ms: default_poll_ms(),
            capture_primary: false,
            primary_debounce_ms: default_primary_debounce_ms(),
            extra: BTreeMap::new(),
        }
    }
}

/// `[history]` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryConfig {
    #[serde(default = "default_max_items")]
    pub max_items: u64,
    #[serde(default = "default_max_store_bytes")]
    pub max_store_bytes: u64,
    /// 0 = off.
    #[serde(default)]
    pub max_age_days: u64,
    #[serde(default = "default_dedupe_window")]
    pub dedupe_window: usize,
    /// Unknown keys preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            max_items: default_max_items(),
            max_store_bytes: default_max_store_bytes(),
            max_age_days: 0,
            dedupe_window: default_dedupe_window(),
            extra: BTreeMap::new(),
        }
    }
}

/// `[popup]` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PopupConfig {
    #[serde(default = "default_hotkey")]
    pub hotkey: String,
    #[serde(default = "default_placement")]
    pub placement: String,
    #[serde(default = "default_preview_width")]
    pub preview_width: u32,
    #[serde(default = "default_true")]
    pub dismiss_on_focus_loss: bool,
    /// Chord map (seg-09 S8 keymap).
    #[serde(default)]
    pub keys: BTreeMap<String, String>,
    #[serde(default = "default_paste_chords")]
    pub paste_chords: Vec<PasteChord>,
    #[serde(default = "default_unicode_limit")]
    pub unicode_limit: u64,
    #[serde(default = "default_delivery_dwell_ms")]
    pub delivery_dwell_ms: u64,
    #[serde(default)]
    pub delivery: PasteDelivery,
    #[serde(default = "default_true")]
    pub restore_clipboard: bool,
    /// Unknown keys preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

impl Default for PopupConfig {
    fn default() -> Self {
        Self {
            hotkey: default_hotkey(),
            placement: default_placement(),
            preview_width: default_preview_width(),
            dismiss_on_focus_loss: true,
            keys: BTreeMap::new(),
            paste_chords: default_paste_chords(),
            unicode_limit: default_unicode_limit(),
            delivery_dwell_ms: default_delivery_dwell_ms(),
            delivery: PasteDelivery::default(),
            restore_clipboard: true,
            extra: BTreeMap::new(),
        }
    }
}

/// `[privacy]` section.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PrivacyConfig {
    #[serde(default)]
    pub strict: bool,
    #[serde(default)]
    pub denylist: Vec<String>,
    /// Unknown keys preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

/// `[windows]` section (Windows-only, ignored elsewhere).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowsConfig {
    /// Auto-elevate helper at logon via Scheduled Task (one-time UAC).
    /// When true the daemon installs `ClipmongerHelper` task and keeps
    /// the High IL hotkey/paste helper alive for admin windows.
    #[serde(default)]
    pub helper_auto_elevate: bool,
    /// Unknown keys preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

impl Default for WindowsConfig {
    fn default() -> Self {
        Self {
            helper_auto_elevate: false,
            extra: BTreeMap::new(),
        }
    }
}


/// Top-level config. Unknown root keys (e.g. future `[tray]`, `[ui]`
/// sections) survive in `extra`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_config_version")]
    pub config_version: u32,
    #[serde(default)]
    pub monitor: MonitorConfig,
    #[serde(default)]
    pub history: HistoryConfig,
    #[serde(default)]
    pub popup: PopupConfig,
    #[serde(default)]
    pub privacy: PrivacyConfig,
    #[serde(default)]
    pub windows: WindowsConfig,
    /// Unknown root-level keys preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
    /// Set by `load` when the file is newer than this code. Skipped in TOML.
    #[serde(skip, default)]
    pub warned_newer_version: bool,
}

fn default_config_version() -> u32 {
    CONFIG_VERSION
}

impl Default for Config {
    fn default() -> Self {
        Self {
            config_version: CONFIG_VERSION,
            monitor: MonitorConfig::default(),
            history: HistoryConfig::default(),
            popup: PopupConfig::default(),
            privacy: PrivacyConfig::default(),
            windows: WindowsConfig::default(),
            extra: BTreeMap::new(),
            warned_newer_version: false,
        }
    }
}

/// Resolved profile directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl Config {
    /// Load from `path`. A missing file yields defaults. A newer
    /// `config_version` loads with `warned_newer_version` set (run + warn).
    /// Env overrides apply in both cases.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::Config`] when the file is not valid TOML, or
    /// [`CoreError::Io`] when an existing file cannot be read.
    pub fn load(path: &Path) -> Result<Self, CoreError> {
        let mut cfg = match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str::<Self>(&text).map_err(|e| CoreError::Config {
                message: e.to_string(),
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(CoreError::Io(e)),
        };
        if cfg.config_version > CONFIG_VERSION {
            cfg.warned_newer_version = true;
        }
        cfg.apply_env();
        Ok(cfg)
    }

    /// Save as TOML with user-only (0600) file permissions on Unix.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::Config`] when serialization fails, or
    /// [`CoreError::Io`] when the file cannot be written.
    pub fn save(&self, path: &Path) -> Result<(), CoreError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let text = toml::to_string(self).map_err(|e| CoreError::Config {
            message: e.to_string(),
        })?;
        std::fs::write(path, text)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut perms = std::fs::metadata(path)?.permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(path, perms)?;
        }
        Ok(())
    }

    /// Apply `CLIPMONGER_<SECTION>_<KEY>` overrides (scalars + comma lists).
    pub fn apply_env(&mut self) {
        if let Some(v) = env_str("CLIPMONGER_MONITOR_MAX_BYTES") {
            if let Ok(n) = v.parse::<u64>() {
                self.monitor.max_bytes = n;
            }
        }
        if let Some(v) = env_str("CLIPMONGER_MONITOR_MIN_LEN") {
            if let Ok(n) = v.parse::<u64>() {
                self.monitor.min_len = n;
            }
        }
        if let Some(v) = env_str("CLIPMONGER_MONITOR_POLL_MS") {
            if let Ok(n) = v.parse::<u64>() {
                self.monitor.poll_ms = n;
            }
        }
        if let Some(v) = env_str("CLIPMONGER_MONITOR_PRIMARY_DEBOUNCE_MS") {
            if let Ok(n) = v.parse::<u64>() {
                self.monitor.primary_debounce_ms = n;
            }
        }
        if let Some(v) = env_str("CLIPMONGER_MONITOR_CAPTURE_PRIMARY") {
            if let Ok(b) = v.parse::<bool>() {
                self.monitor.capture_primary = b;
            }
        }
        if let Some(v) = env_list("CLIPMONGER_MONITOR_FORMATS") {
            self.monitor.formats = v;
        }
        if let Some(v) = env_list("CLIPMONGER_MONITOR_IGNORES") {
            self.monitor.ignores = v;
        }
        if let Some(v) = env_str("CLIPMONGER_HISTORY_MAX_ITEMS") {
            if let Ok(n) = v.parse::<u64>() {
                self.history.max_items = n;
            }
        }
        if let Some(v) = env_str("CLIPMONGER_HISTORY_MAX_STORE_BYTES") {
            if let Ok(n) = v.parse::<u64>() {
                self.history.max_store_bytes = n;
            }
        }
        if let Some(v) = env_str("CLIPMONGER_HISTORY_MAX_AGE_DAYS") {
            if let Ok(n) = v.parse::<u64>() {
                self.history.max_age_days = n;
            }
        }
        if let Some(v) = env_str("CLIPMONGER_HISTORY_DEDUPE_WINDOW") {
            if let Ok(n) = v.parse::<usize>() {
                self.history.dedupe_window = n;
            }
        }
        if let Some(v) = env_str("CLIPMONGER_POPUP_HOTKEY") {
            self.popup.hotkey = v;
        }
        if let Some(v) = env_str("CLIPMONGER_POPUP_PLACEMENT") {
            self.popup.placement = v;
        }
        if let Some(v) = env_str("CLIPMONGER_POPUP_PREVIEW_WIDTH") {
            if let Ok(n) = v.parse::<u32>() {
                self.popup.preview_width = n;
            }
        }
        if let Some(v) = env_str("CLIPMONGER_POPUP_DISMISS_ON_FOCUS_LOSS") {
            if let Ok(b) = v.parse::<bool>() {
                self.popup.dismiss_on_focus_loss = b;
            }
        }
        if let Some(v) = env_str("CLIPMONGER_POPUP_UNICODE_LIMIT") {
            if let Ok(n) = v.parse::<u64>() {
                self.popup.unicode_limit = n;
            }
        }
        if let Some(v) = env_str("CLIPMONGER_POPUP_DELIVERY_DWELL_MS") {
            if let Ok(n) = v.parse::<u64>() {
                self.popup.delivery_dwell_ms = n;
            }
        }
        if let Some(v) = env_str("CLIPMONGER_PRIVACY_STRICT") {
            if let Ok(b) = v.parse::<bool>() {
                self.privacy.strict = b;
            }
        }
        if let Some(v) = env_list("CLIPMONGER_PRIVACY_DENYLIST") {
            self.privacy.denylist = v;
        }
    }

    /// Resolve config/data/cache dirs. Portable mode puts all three beside
    /// the binary (`exe_dir`); otherwise the OS dirs via `directories`.
    #[must_use]
    pub fn resolve_paths(portable: bool, exe_dir: &Path) -> AppPaths {
        if portable {
            return AppPaths {
                config_dir: exe_dir.to_path_buf(),
                data_dir: exe_dir.to_path_buf(),
                cache_dir: exe_dir.to_path_buf(),
            };
        }
        let base = directories::BaseDirs::new();
        let config_dir = base
            .as_ref()
            .and_then(|b| {
                b.config_dir()
                    .to_str()
                    .map(|_| b.config_dir().join("clipmonger"))
            })
            .unwrap_or_else(|| exe_dir.to_path_buf());
        let data_dir = base
            .as_ref()
            .and_then(|b| {
                b.data_dir()
                    .to_str()
                    .map(|_| b.data_dir().join("clipmonger"))
            })
            .unwrap_or_else(|| exe_dir.to_path_buf());
        let cache_dir = base
            .as_ref()
            .and_then(|b| {
                b.cache_dir()
                    .to_str()
                    .map(|_| b.cache_dir().join("clipmonger"))
            })
            .unwrap_or_else(|| exe_dir.to_path_buf());
        AppPaths {
            config_dir,
            data_dir,
            cache_dir,
        }
    }
}

fn env_str(key: &str) -> Option<String> {
    std::env::var(key).ok().map(|v| v.trim().to_string())
}

/// Comma-separated list env value; `None` when unset.
fn env_list(key: &str) -> Option<Vec<String>> {
    std::env::var(key).ok().map(|v| {
        v.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivery_defaults_to_clipboard_restore() {
        assert_eq!(
            Config::default().popup.delivery,
            PasteDelivery::ClipboardRestore
        );
        assert_eq!(PasteDelivery::default(), PasteDelivery::ClipboardRestore);
        let cfg: Config = toml::from_str("[popup]\n").unwrap();
        assert_eq!(cfg.popup.delivery, PasteDelivery::ClipboardRestore);
    }

    #[test]
    fn delivery_toml_roundtrip_both_variants() {
        for (delivery, text) in [
            (PasteDelivery::ClipboardRestore, "clipboard_restore"),
            (PasteDelivery::UnicodeInject, "unicode_inject"),
        ] {
            let mut cfg = Config::default();
            cfg.popup.delivery = delivery;
            let rendered = toml::to_string(&cfg).unwrap();
            assert!(
                rendered.contains(&format!("delivery = \"{text}\"")),
                "{rendered}"
            );
            let back: Config = toml::from_str(&rendered).unwrap();
            assert_eq!(back.popup.delivery, delivery);
        }
    }

    #[test]
    fn delivery_unknown_value_is_config_error() {
        let parsed: Result<Config, _> = toml::from_str("[popup]\ndelivery = \"bogus\"\n");
        assert!(parsed.is_err());
    }
}
