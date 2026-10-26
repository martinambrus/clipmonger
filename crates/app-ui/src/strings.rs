//! Frozen v1 string catalog with a locale fallback rule.

// Each id below has exactly one English text; unknown ids echo back.
const ENTRIES: &[(&str, &str)] = &[
    ("popup.search_hint", "Type to filter"),
    ("popup.current", "Current item"),
    ("popup.empty", "Nothing here yet"),
    ("popup.count", "{count} items"),
    ("popup.paused", "Monitoring paused"),
    ("popup.degraded", "Some features limited"),
    ("popup.offline", "Connection lost. Reconnecting."),
    (
        "popup.stale_target",
        "Target changed. Pick the destination again.",
    ),
    ("popup.paste_ok", "Pasted"),
    ("popup.paste_blocked", "Paste is not available right now"),
    (
        "paste.press_ctrl_v",
        "Copied to clipboard. Press Ctrl+V to paste.",
    ),
    ("popup.delete_confirm", "Delete this entry?"),
    ("popup.pin", "Pin entry"),
    ("popup.unpin", "Unpin entry"),
    ("popup.refresh", "Refresh"),
    ("popup.dismiss", "Dismiss"),
    ("popup.show", "Show popup"),
    ("tray.show", "Show popup"),
    ("tray.pause", "Pause monitoring"),
    ("tray.resume", "Resume monitoring"),
    ("tray.pause_1h", "Pause for one hour"),
    ("tray.config", "Open settings"),
    ("tray.about", "About"),
    ("tray.quit", "Quit"),
    ("tray.paused_tip", "Paused"),
    ("app.offline_banner", "Offline. Retrying."),
    ("app.error", "Something went wrong"),
];

/// Every view string id shipped in v1.
pub const IDS: &[&str] = &[
    "popup.search_hint",
    "popup.current",
    "popup.empty",
    "popup.count",
    "popup.paused",
    "popup.degraded",
    "popup.offline",
    "popup.stale_target",
    "popup.paste_ok",
    "popup.paste_blocked",
    "paste.press_ctrl_v",
    "popup.delete_confirm",
    "popup.pin",
    "popup.unpin",
    "popup.refresh",
    "popup.dismiss",
    "popup.show",
    "tray.show",
    "tray.pause",
    "tray.resume",
    "tray.pause_1h",
    "tray.config",
    "tray.about",
    "tray.quit",
    "tray.paused_tip",
    "app.offline_banner",
    "app.error",
];

/// Look up the English text for an id, echoing unknown ids back.
#[must_use]
pub fn text(id: &str) -> &str {
    for (known, value) in ENTRIES {
        if *known == id {
            return value;
        }
    }
    id
}
