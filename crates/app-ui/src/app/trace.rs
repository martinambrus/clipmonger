//! The UI trace: one JSON line per shell transition, for automated probes.
//! Only booleans and fixed words go in: never clip contents, window titles or
//! token values.

use std::io::Write;

use crate::ipc_bridge::UiEvent;
use crate::shell::Event;

/// Environment variable that names the trace file.
pub const TRACE_ENV: &str = "CLIPMONGER_UI_TRACE";

/// One traced transition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TraceEvent {
    /// The popup opened. `source` is one of `hotkey`, `request`, `tray`,
    /// `paste_failed`, `start`, `other`; `target` and `gesture` tell whether the
    /// paste context holds them.
    Open {
        source: &'static str,
        target: bool,
        gesture: bool,
    },
    /// The popup hid. `reason` is one of `dismiss`, `focus_lost`,
    /// `close_request`, `toggle`, `tray`, `pick`, `other`.
    Hide { reason: &'static str },
    /// A paste command went out; `target` tells whether it named a window.
    Paste { target: bool },
    /// An error line is shown.
    Error,
}

/// The JSON line (no newline) of `event` at unix time `t_ms`:
/// `{"t_ms":N,"ev":"open","source":"hotkey","target":true,"gesture":false}`,
/// `{"t_ms":N,"ev":"hide","reason":"dismiss"}`,
/// `{"t_ms":N,"ev":"paste","target":true}` or `{"t_ms":N,"ev":"error"}`.
#[must_use]
pub fn line(event: &TraceEvent, t_ms: u64) -> String {
    let value = match event {
        TraceEvent::Open {
            source,
            target,
            gesture,
        } => serde_json::json!({
            "t_ms": t_ms, "ev": "open", "source": source, "target": target, "gesture": gesture,
        }),
        TraceEvent::Hide { reason } => {
            serde_json::json!({ "t_ms": t_ms, "ev": "hide", "reason": reason })
        }
        TraceEvent::Paste { target } => {
            serde_json::json!({ "t_ms": t_ms, "ev": "paste", "target": target })
        }
        TraceEvent::Error => serde_json::json!({ "t_ms": t_ms, "ev": "error" }),
    };
    value.to_string()
}

/// Why an event opened the popup: `hotkey` for a daemon push with source
/// `hotkey`, `request` for any other push source, `tray` for `TrayToggle`,
/// `paste_failed` for a failed paste, `other` for anything else.
#[must_use]
pub fn open_source(event: &Event) -> &'static str {
    match event {
        Event::Bridge(UiEvent::ShowPopup(push)) => {
            if push.source == "hotkey" {
                "hotkey"
            } else {
                "request"
            }
        }
        Event::TrayToggle => "tray",
        Event::Bridge(UiEvent::PasteFailed(_)) => "paste_failed",
        _ => "other",
    }
}

/// Why an event hid the popup: `dismiss`, `focus_lost`, `close_request`,
/// `toggle` (a daemon push), `tray` (`TrayToggle`), `pick` (`Pick` and
/// `PickPermanent`), `other` for anything else.
#[must_use]
pub fn hide_reason(event: &Event) -> &'static str {
    match event {
        Event::Dismiss => "dismiss",
        Event::FocusLost => "focus_lost",
        Event::CloseRequest => "close_request",
        Event::Bridge(UiEvent::ShowPopup(_)) => "toggle",
        Event::TrayToggle => "tray",
        Event::Pick { .. } | Event::PickPermanent { .. } => "pick",
        _ => "other",
    }
}

/// A line sink that appends [`TraceEvent`]s as JSON lines.
pub struct Trace {
    sink: Box<dyn Write + Send>,
}

impl Trace {
    /// A trace that writes to `sink`.
    #[must_use]
    pub fn new(sink: Box<dyn Write + Send>) -> Self {
        Self { sink }
    }

    /// The trace named by the environment variable [`TRACE_ENV`]: the file is
    /// created if needed and appended to. `None` when the variable is unset or
    /// empty or the file cannot be opened.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let path = std::env::var_os(TRACE_ENV).filter(|path| !path.is_empty())?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()?;
        Some(Self::new(Box::new(file)))
    }

    /// Write one line for `event` stamped with the current unix time in
    /// milliseconds, then flush. Write errors are ignored.
    pub fn record(&mut self, event: &TraceEvent) {
        let t_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| {
                u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
            });
        let _ = writeln!(self.sink, "{}", line(event, t_ms));
        let _ = self.sink.flush();
    }
}
