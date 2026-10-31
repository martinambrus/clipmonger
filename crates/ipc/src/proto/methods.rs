//! Method Params/Result pairs for every v1 IPC method.

use clipmonger_core::model::{Clip, PermanentClip};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Monitor target state for `monitor.set`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MonitorState {
    On,
    PausedUntil(i64),
}

/// Empty params for `status`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusParams {}
/// Status output (shape is builder freedom; existence + roundtrip pinned).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusResult {}
/// Empty params for `caps.get`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapsGetParams {}
/// Capability matrix output (shape is builder freedom).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapsGetResult {}
/// Empty params for `caps.refresh`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapsRefreshParams {}
/// Refreshed capability matrix output (shape is builder freedom).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapsRefreshResult {}
/// Filters for `history.list`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_removed: Option<bool>,
}
/// Clip page for `history.list`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryListResult {
    #[serde(default)]
    pub clips: Vec<Clip>,
}
/// Clip lookup for `history.get`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryGetParams {
    pub clip: Uuid,
}
/// Clip lookup output for `history.get`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryGetResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip: Option<Clip>,
}
/// Soft-delete request for `history.delete`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryDeleteParams {
    pub clip: Uuid,
}
/// Soft-delete output for `history.delete`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryDeleteResult {
    pub deleted: bool,
}
/// Pin toggle for `history.pin`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryPinParams {
    pub clip: Uuid,
    pub pinned: bool,
}
/// Pin output for `history.pin`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryPinResult {
    pub pinned: bool,
}
/// Preview edit for `history.edit`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEditParams {
    pub clip: Uuid,
    pub preview: String,
}
/// Preview edit output for `history.edit`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEditResult {
    pub ok: bool,
}
/// Duplicate request for `history.duplicate`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryDuplicateParams {
    pub clip: Uuid,
}
/// Duplicate output for `history.duplicate`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryDuplicateResult {
    pub new_clip: Uuid,
}
/// Paste request for `paste.execute`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PasteExecuteParams {
    pub clip: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Value>,
}
/// Paste output for `paste.execute`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PasteExecuteResult {
    pub strategy: String,
}
/// Monitor change for `monitor.set`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorSetParams {
    pub state: MonitorState,
}
/// Monitor change output for `monitor.set`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorSetResult {
    pub ok: bool,
}
/// Hotkey registration for `hotkey.register`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeyRegisterParams {
    pub id: u32,
    pub chord: String,
}
/// Hotkey registration output for `hotkey.register`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeyRegisterResult {
    pub ok: bool,
}
/// Hotkey removal for `hotkey.unregister`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeyUnregisterParams {
    pub id: u32,
}
/// Hotkey removal output for `hotkey.unregister`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeyUnregisterResult {
    pub ok: bool,
}
/// Config read for `config.get`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigGetParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}
/// Config read output for `config.get`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigGetResult {
    pub config: Value,
}
/// Config write for `config.set`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigSetParams {
    pub key: String,
    pub value: Value,
}
/// Config write output for `config.set`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigSetResult {
    pub ok: bool,
}
/// Backup request for `backup.create`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupCreateParams {
    pub dest: String,
}
/// Backup output for `backup.create`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupCreateResult {
    pub path: String,
}
/// Restore request for `backup.restore`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupRestoreParams {
    pub src: String,
}
/// Restore output for `backup.restore`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupRestoreResult {
    pub ok: bool,
}
/// Log tail request for `logs.tail`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogsTailParams {
    pub lines: u64,
}
/// Log tail output for `logs.tail`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogsTailResult {
    #[serde(default)]
    pub lines: Vec<String>,
}
/// Empty params for `daemon.stop`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStopParams {}
/// Stop output for `daemon.stop`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStopResult {
    pub ok: bool,
}
/// Topic subscription for `events.subscribe`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventsSubscribeParams {
    #[serde(default)]
    pub topics: Vec<String>,
}
/// Subscription output for `events.subscribe`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventsSubscribeResult {
    pub ok: bool,
}
/// Event topic the daemon pushes to the UI to show its popup.
pub const SHOW_POPUP_TOPIC: &str = "ui.show_popup";
/// Payload of the daemon→UI `ui.show_popup` event.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShowPopupEvent {
    /// `hotkey` for a key press, `request` for `popup.open`.
    pub source: String,
    /// The press came while the popup was in front: hide it instead.
    #[serde(default)]
    pub toggle: bool,
    /// Paste target captured at the press (Windows HWND, X11 window id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<u64>,
    /// Single-use gesture token minted for the press.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gesture: Option<u64>,
    /// X11 server time of the press, for focus requests (reserved).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x11_time: Option<u32>,
    /// Wayland xdg-activation token for the press (reserved).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_token: Option<String>,
}
/// Empty params for `popup.open` (no gesture, no target).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PopupOpenParams {}
/// Output for `popup.open`: whether a running UI got the request (otherwise
/// it waits for the UI the daemon starts).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PopupOpenResult {
    pub delivered: bool,
}
/// Capture relay for `capture.report`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureReportParams {
    pub event: Value,
}
/// Capture relay output for `capture.report`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureReportResult {
    pub accepted: bool,
}
/// Hotkey fire notice for `hotkey.fired`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeyFiredParams {
    pub id: u32,
}
/// Hotkey fire output for `hotkey.fired`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeyFiredResult {
    pub ok: bool,
}
/// Hotkey conflict notice for `hotkey.conflict`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeyConflictParams {
    pub chord: String,
    #[serde(default)]
    pub reason: String,
}
/// Hotkey conflict output for `hotkey.conflict`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeyConflictResult {
    pub ok: bool,
}
/// Empty params for `ping`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PingParams {}
/// Heartbeat output for `ping`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PingResult {
    pub pong: bool,
}
/// One upload frame for `blob.put`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobPutParams {
    pub clip: Uuid,
    pub mime: String,
    pub seq: u64,
    pub data_b64: String,
    pub eof: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}
/// Upload frame output for `blob.put`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobPutResult {
    pub ok: bool,
}
/// Permanent-clip exec for `permanent.exec`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermanentExecParams {
    pub id: Uuid,
}
/// Permanent-clip exec output for `permanent.exec`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermanentExecResult {
    pub ok: bool,
}
/// List filter for `permanent.list`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermanentListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}
/// Permanent page for `permanent.list`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermanentListResult {
    #[serde(default)]
    pub permanents: Vec<PermanentClip>,
}
/// Create request for `permanent.create` (id/kind/js/rev are server-owned).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermanentCreateParams {
    pub group: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hotkey: Option<String>,
    pub body: String,
}
/// Create output for `permanent.create`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermanentCreateResult {
    pub permanent: PermanentClip,
}
/// Update request for `permanent.update` (full edited clip).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermanentUpdateParams {
    pub permanent: PermanentClip,
}
/// Update output for `permanent.update`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermanentUpdateResult {
    pub updated: bool,
}
/// Delete request for `permanent.delete`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermanentDeleteParams {
    pub id: Uuid,
}
/// Delete output for `permanent.delete`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermanentDeleteResult {
    pub deleted: bool,
}
/// Empty params for `permanent.groups`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermanentGroupsParams {}
/// One group row for `permanent.groups`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermanentGroupInfo {
    pub name: String,
    pub system: bool,
    pub count: u64,
}
/// Groups output for `permanent.groups`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermanentGroupsResult {
    #[serde(default)]
    pub groups: Vec<PermanentGroupInfo>,
}
/// Group create for `permanent.group_create`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermanentGroupCreateParams {
    pub name: String,
}
/// Group create output for `permanent.group_create`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermanentGroupCreateResult {
    pub created: bool,
}
/// Group delete for `permanent.group_delete`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermanentGroupDeleteParams {
    pub name: String,
}
/// Group delete output for `permanent.group_delete`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermanentGroupDeleteResult {
    pub deleted: bool,
}
/// Empty params for `history.purge_removed`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryPurgeRemovedParams {}
/// Purge output for `history.purge_removed`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryPurgeRemovedResult {
    pub purged: u64,
}
