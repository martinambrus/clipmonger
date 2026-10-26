//! Shared platform types: handles, capture events, caps, errors.

/// Thread-affinity declared per platform trait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Affinity {
    /// May run on any blocking worker thread.
    Worker,
    /// Must run on the OS event-loop (main) thread.
    MainThread,
}

/// Clipboard kinds a monitor can subscribe to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscribeKind {
    /// Clipboard buffer.
    Clipboard,
    /// X11/Wayland primary selection only.
    Primary,
}

/// Registered hotkey identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HotkeyId(pub u32);

/// Foreground application identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppId {
    /// Executable name or bundle id.
    pub exe: String,
    /// Hash of the window title at capture time.
    pub title_hash: u64,
}

/// Opaque OS window/element handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlatformHandle {
    /// Win32 `HWND` value.
    WinHwnd(i64),
    /// X11 window id.
    X11Window(u64),
    /// Anything else (AX element, Wayland token, tests).
    Opaque(String),
}

/// Reference to a stored clip by id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClipRef(pub uuid::Uuid);

/// One offered clipboard format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatOffer {
    /// MIME type name.
    pub mime: String,
    /// Size hint in bytes, when known.
    pub bytes_hint: Option<u64>,
}

/// Where a capture event came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureOrigin {
    /// Clipboard buffer.
    Clipboard,
    /// X11/Wayland primary selection.
    Primary,
}

/// One monitor observation; `sensitive=true` means record NOTHING.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureEvent {
    /// Formats available for lazy read.
    pub offers: Vec<FormatOffer>,
    /// Buffer the event came from.
    pub origin: CaptureOrigin,
    /// Honor-and-drop when true (F-104).
    pub sensitive: bool,
}

/// Multi-format clipboard payload: `(mime, bytes)` pairs.
pub type PayloadMap = Vec<(String, Vec<u8>)>;

/// Paste destination captured BEFORE the popup opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteTarget {
    /// Application owning the target.
    pub app: AppId,
    /// Window title hash at capture time.
    pub window_title_hash: u64,
    /// OS window/element handle.
    pub handle: PlatformHandle,
    /// Capture time in ms.
    pub captured_ms: i64,
}

/// How a paste is delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteStrategy {
    /// Synthetic keystrokes.
    UnicodeInject,
    /// Temporary clipboard write + restore guard.
    ClipboardRestore,
    /// Route via the elevated helper (Windows).
    ElevatedRoute,
    /// Write + toast when injection is impossible.
    WriteAndToast,
    /// GNOME extension assisted paste.
    ExtensionAssisted,
    /// No paste path on this backend.
    Unavailable,
}

/// Backend capability level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Fully working.
    Full,
    /// Working with limits (see `reason`/`notes`).
    Degraded,
    /// Not available (offline mode still serves history).
    Unavailable,
}

/// Paste capability: level + strategy, never mixed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteCaps {
    /// Capability level.
    pub level: Level,
    /// Delivery strategy used at this level.
    pub strategy: PasteStrategy,
    /// Human-readable reason for degraded/unavailable.
    pub reason: String,
}

/// Full backend capability matrix reported at startup and on change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendCaps {
    /// Monitor capability.
    pub monitor: Level,
    /// Transfer capability.
    pub transfer: Level,
    /// Hotkey capability.
    pub hotkey: Level,
    /// Paste capability.
    pub paste: PasteCaps,
    /// Tray capability.
    pub tray: Level,
    /// OS autostart supported.
    pub autostart: bool,
    /// Primary selection supported.
    pub primary_selection: bool,
    /// Extra per-box notes.
    pub notes: Vec<String>,
}

/// Tray attention state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayState {
    /// Normal icon.
    Idle,
    /// Needs user attention.
    Attention,
}

/// Tray UI event surfaced to the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayEvent {
    /// Icon activated (click).
    Activated,
    /// Menu item chosen.
    MenuItem(String),
}

/// Platform error; kinds mirror `core::CoreError::Backend` kinds.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Backend unavailable.
    #[error("backend unavailable: {0}")]
    Unavailable(String),
    /// Access denied by OS/policy.
    #[error("denied: {0}")]
    Denied(String),
    /// Operation timed out.
    #[error("timeout: {0}")]
    Timeout(String),
    /// Payload over the size cap.
    #[error("too large: {0}")]
    TooLarge(String),
    /// Operation not supported by this backend.
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// OS I/O failure.
    #[error("io: {0}")]
    Io(String),
    /// Anything else.
    #[error("internal: {0}")]
    Internal(String),
}

/// Platform result alias.
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Map into the core backend error (seg-02 S5). `backend` names the source backend.
    #[must_use]
    pub fn into_core(self, backend: &str) -> clipmonger_core::CoreError {
        use clipmonger_core::error::BackendKind;
        let (kind, message) = match self {
            Self::Unavailable(m) => (BackendKind::Unavailable, m),
            Self::Denied(m) => (BackendKind::Denied, m),
            Self::Timeout(m) => (BackendKind::Timeout, m),
            Self::TooLarge(m) => (BackendKind::TooLarge, m),
            Self::Unsupported(m) => (BackendKind::Unsupported, m),
            Self::Io(m) => (BackendKind::Io, m),
            Self::Internal(m) => (BackendKind::Internal, m),
        };
        clipmonger_core::CoreError::Backend {
            backend: backend.to_owned(),
            kind,
            message,
        }
    }
}
