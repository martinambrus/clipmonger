//! Desktop notification for write-and-toast pastes (Linux Wayland without
//! the GNOME extension, e.g. KDE or wlroots): the daemon wrote the clip to
//! the clipboard and the user still has to press Ctrl+V. The popup has
//! already closed and permanent-hotkey pastes never show a window, so the
//! hint goes through `org.freedesktop.Notifications`. Best effort: a
//! missing notification server only loses the hint, never the paste.

use std::collections::HashMap;

use ashpd::zbus::{self, zvariant::Value};

/// Text shown for `paste.press_ctrl_v` (matches the UI catalog default).
pub(crate) const PRESS_CTRL_V_TEXT: &str = "Copied to clipboard. Press Ctrl+V to paste.";

/// How long the hint stays up, in milliseconds.
const EXPIRE_MS: i32 = 4000;

/// Arguments of `org.freedesktop.Notifications.Notify`.
type NotifyArgs<'a> = (
    &'a str,
    u32,
    &'a str,
    &'a str,
    &'a str,
    Vec<&'a str>,
    HashMap<&'a str, Value<'a>>,
    i32,
);

/// Build the `Notify` call for `body`, replacing notification `replaces`
/// (0 for a new one) so repeated pastes reuse one bubble.
fn notify_args(body: &str, replaces: u32) -> NotifyArgs<'_> {
    let mut hints = HashMap::new();
    hints.insert("transient", Value::from(true));
    hints.insert("urgency", Value::from(0u8));
    (
        "Clipmonger",
        replaces,
        "edit-paste",
        "Clipmonger",
        body,
        Vec::new(),
        hints,
        EXPIRE_MS,
    )
}

/// Id of the last hint shown, so the next one replaces it.
static LAST_ID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

async fn send(body: &str) -> zbus::Result<()> {
    let conn = zbus::Connection::session().await?;
    let replaces = LAST_ID.load(std::sync::atomic::Ordering::Relaxed);
    let reply = conn
        .call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &notify_args(body, replaces),
        )
        .await?;
    let id: u32 = reply.body().deserialize()?;
    LAST_ID.store(id, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

/// Show the press-Ctrl+V hint without blocking the paste reply.
pub(crate) fn press_ctrl_v() {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    handle.spawn(async {
        let _ = send(PRESS_CTRL_V_TEXT).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hint_is_transient_low_urgency_and_replaces_the_previous_one() {
        let (app, replaces, icon, summary, body, actions, hints, expire) =
            notify_args(PRESS_CTRL_V_TEXT, 7);
        assert_eq!(
            (app, icon, summary),
            ("Clipmonger", "edit-paste", "Clipmonger")
        );
        assert_eq!(replaces, 7);
        assert_eq!(body, "Copied to clipboard. Press Ctrl+V to paste.");
        assert_eq!(actions, [] as [&str; 0]);
        assert_eq!(hints.get("transient"), Some(&Value::from(true)));
        assert_eq!(hints.get("urgency"), Some(&Value::from(0u8)));
        assert_eq!(expire, EXPIRE_MS);
    }
}

#[cfg(test)]
mod live {
    /// Shows a real notification; run by hand on a desktop session.
    #[tokio::test]
    #[ignore = "needs a session bus with a notification server"]
    async fn live_hint_reaches_the_notification_server() {
        super::send(super::PRESS_CTRL_V_TEXT).await.unwrap();
        assert_ne!(super::LAST_ID.load(std::sync::atomic::Ordering::Relaxed), 0);
    }
}
