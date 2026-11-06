//! `KWin` (Plasma Wayland) integration: active-window tracking and auto-paste.
//!
//! A Wayland client cannot see which window has focus. On `KWin` the daemon
//! loads a small `KWin` script that calls back over D-Bus with the
//! `resourceClass`/`resourceName` of every window `KWin` activates. With that,
//! a popup paste waits for focus to return to the target (the popup is our
//! own window), picks Ctrl+Shift+V for terminals, and presses the chord
//! through `KWin`'s fake-input protocol
//! ([`clipmonger_backend_wayland::fake_input`]).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ashpd::zbus;

/// Object path the `KWin` script calls.
const OBJECT_PATH: &str = "/com/martinambrus/clipmonger/KWin";
/// Interface the `KWin` script calls.
const INTERFACE: &str = "com.martinambrus.clipmonger.KWin";
/// Plugin name the script is loaded under (also used to unload it).
const SCRIPT_NAME: &str = "clipmonger-active-window";
/// Window identities that are our own (the popup, the settings window).
const OWN_IDS: &[&str] = &["clipmonger", crate::portal::APP_ID];
/// Longest wait for focus to leave the popup after a pick.
const FOCUS_WAIT: Duration = Duration::from_millis(600);
/// Settle before the chord for small text (as the GNOME extension).
const SETTLE_SMALL: Duration = Duration::from_millis(60);
/// Settle before the chord for images and large text: GTK3 drops a paste
/// that arrives while it still queries the new owner's targets.
const SETTLE_LARGE: Duration = Duration::from_millis(200);
/// Text up to this size counts as small.
const SMALL_TEXT_BYTES: usize = 64 * 1024;

/// Focus as last reported by the `KWin` script.
#[derive(Debug, Default, Clone)]
pub struct Focus {
    /// Identities of the last activated window that was not ours.
    pub target: Option<Vec<String>>,
    /// Whether one of our windows is active right now.
    pub own_active: bool,
    /// Whether the script has reported at all.
    pub tracking: bool,
}

impl Focus {
    /// Record one activation.
    pub fn activated(&mut self, ids: Vec<String>) {
        self.tracking = true;
        let own = ids.iter().any(|id| {
            OWN_IDS
                .iter()
                .any(|own| id.eq_ignore_ascii_case(own))
        });
        self.own_active = own;
        if !own && ids.iter().any(|id| !id.is_empty()) {
            self.target = Some(ids);
        }
    }
}

/// Whether this session runs `KWin` on Wayland.
#[must_use]
pub fn is_kwin_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        && std::env::var("XDG_CURRENT_DESKTOP")
            .is_ok_and(|desktop| desktop.split(':').any(|d| d.eq_ignore_ascii_case("kde")))
}

/// The `KWin` script text, calling back to `bus_name`.
fn script_source(bus_name: &str) -> String {
    format!(
        "function report(w) {{\n\
         \x20   if (!w) return;\n\
         \x20   callDBus(\"{bus_name}\", \"{OBJECT_PATH}\", \"{INTERFACE}\", \"Activated\",\n\
         \x20       String(w.resourceClass || \"\"), String(w.resourceName || \"\"),\n\
         \x20       String(w.desktopFileName || \"\"));\n\
         }}\n\
         // KWin 6 names; KWin 5 calls them clientActivated/activeClient.\n\
         (workspace.windowActivated || workspace.clientActivated).connect(report);\n\
         report(workspace.activeWindow || workspace.activeClient);\n"
    )
}

/// Start the tracker thread on `KWin` Wayland sessions.
pub fn spawn_focus_tracker(state: Arc<crate::DaemonState>) {
    if !is_kwin_wayland() {
        return;
    }
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        if let Err(e) = runtime.block_on(track(&state)) {
            eprintln!("daemon: kwin focus tracking unavailable: {e}");
        }
    });
}

async fn track(state: &crate::DaemonState) -> zbus::Result<()> {
    use futures_util::StreamExt;

    let conn = zbus::Connection::session().await?;
    let Some(bus_name) = conn.unique_name().map(ToString::to_string) else {
        return Err(zbus::Error::Failure("no unique bus name".to_string()));
    };
    let mut stream = zbus::MessageStream::from(&conn);
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map_or_else(std::env::temp_dir, std::path::PathBuf::from);
    let path = dir.join(format!("{SCRIPT_NAME}.js"));
    std::fs::write(&path, script_source(&bus_name))
        .map_err(|e| zbus::Error::Failure(format!("write kwin script: {e}")))?;
    let scripting = zbus::Proxy::new(&conn, "org.kde.KWin", "/Scripting", "org.kde.kwin.Scripting")
        .await?;
    // A script left by an earlier daemon calls a bus name that is gone.
    let _: zbus::Result<bool> = scripting.call("unloadScript", &(SCRIPT_NAME,)).await;
    let id: i32 = scripting
        .call("loadScript", &(path.to_string_lossy().as_ref(), SCRIPT_NAME))
        .await?;
    if id < 0 {
        return Err(zbus::Error::Failure("kwin refused the script".to_string()));
    }
    // `KWin` 6 exports a loaded script at `/Scripting/Script<id>`, `KWin` 5
    // at `/<id>`.
    let mut ran = Err(zbus::Error::Failure("no script path".to_string()));
    for path in [format!("/Scripting/Script{id}"), format!("/{id}")] {
        let script =
            zbus::Proxy::new(&conn, "org.kde.KWin", path, "org.kde.kwin.Script").await?;
        ran = script.call::<_, _, ()>("run", &()).await;
        if ran.is_ok() {
            break;
        }
    }
    ran?;
    while let Some(message) = stream.next().await {
        let Ok(message) = message else { continue };
        let header = message.header();
        if header.message_type() != zbus::message::Type::MethodCall
            || header.path().map(zbus::zvariant::ObjectPath::as_str) != Some(OBJECT_PATH)
            || header.member().map(zbus::names::MemberName::as_str) != Some("Activated")
        {
            continue;
        }
        let ids: Vec<String> = message
            .body()
            .deserialize::<(String, String, String)>()
            .map(|(class, name, desktop)| vec![class, name, desktop])
            .unwrap_or_default();
        state.kwin_activated(ids);
        let _ = conn.reply(&header, &()).await;
    }
    Ok(())
}

/// Settle time before the chord for these payloads.
#[must_use]
pub fn settle_for(payloads: &[(String, Vec<u8>)]) -> Duration {
    let small_text = payloads.iter().all(|(mime, bytes)| {
        (mime.starts_with("text/") || mime == "UTF8_STRING" || mime == "STRING")
            && bytes.len() < SMALL_TEXT_BYTES
    });
    if small_text {
        SETTLE_SMALL
    } else {
        SETTLE_LARGE
    }
}

/// Wait for focus to leave our popup, then press the paste chord.
///
/// Returns `Ok(false)` when this is not `KWin` or `KWin` offers no fake input
/// (the caller then shows the press-Ctrl+V toast instead).
///
/// # Errors
///
/// When focus is still on our own window after [`FOCUS_WAIT`]: pasting
/// then would type into the popup.
pub fn auto_paste(
    focus: &Mutex<Focus>,
    payloads: &[(String, Vec<u8>)],
) -> Result<bool, String> {
    if !is_kwin_wayland() {
        return Ok(false);
    }
    let deadline = Instant::now() + FOCUS_WAIT;
    let read = || focus.lock().map(|f| f.clone()).unwrap_or_default();
    let mut now = read();
    while now.tracking && now.own_active && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        now = read();
    }
    if now.tracking && now.own_active {
        return Err("focus did not return from the popup".to_string());
    }
    let shift = now.target.as_ref().is_some_and(|ids| {
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        clipmonger_platform::paste_policy::paste_needs_shift(&ids)
    });
    std::thread::sleep(settle_for(payloads));
    let Ok(mut input) = clipmonger_backend_wayland::fake_input::FakeInput::connect() else {
        return Ok(false);
    };
    input.paste_chord(shift).map_err(|e| e.to_string())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn own_windows_never_become_the_target() {
        let mut focus = Focus::default();
        focus.activated(ids(&["org.kde.konsole", "konsole", "org.kde.konsole"]));
        focus.activated(ids(&["clipmonger", "clipmonger", ""]));
        assert!(focus.own_active);
        assert_eq!(focus.target.as_deref().map(|t| t[1].as_str()), Some("konsole"));
        focus.activated(ids(&[crate::portal::APP_ID, "", ""]));
        assert!(focus.own_active);
        focus.activated(ids(&["kwrite", "kwrite", "org.kde.kwrite"]));
        assert!(!focus.own_active);
        assert_eq!(focus.target.as_deref().map(|t| t[0].as_str()), Some("kwrite"));
    }

    #[test]
    fn settle_is_short_only_for_small_text() {
        let small = vec![("text/plain".to_string(), vec![b'a'; 10])];
        let big = vec![("text/plain".to_string(), vec![b'a'; SMALL_TEXT_BYTES])];
        let image = vec![("image/png".to_string(), vec![0; 10])];
        assert_eq!(settle_for(&small), SETTLE_SMALL);
        assert_eq!(settle_for(&big), SETTLE_LARGE);
        assert_eq!(settle_for(&image), SETTLE_LARGE);
    }

    #[test]
    fn script_calls_back_to_our_bus_name() {
        let source = script_source(":1.42");
        assert!(source.contains("callDBus(\":1.42\", \"/com/martinambrus/clipmonger/KWin\""));
        assert!(source.contains("(workspace.windowActivated || workspace.clientActivated)"));
        assert!(source.contains("workspace.activeWindow || workspace.activeClient"));
    }
}
