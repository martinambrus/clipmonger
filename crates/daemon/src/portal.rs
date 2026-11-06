//! Wayland `GlobalShortcuts` portal session (seg-06 S4, W-F4).
//!
//! The live session is `ashpd`-async on a daemon-owned thread running its
//! own current-thread tokio runtime; the backend crate stays tokio-free.
//! Pure shortcut ids, `Activated` parsing, and bind planning come from
//! `clipmonger_backend_wayland::portal`. Any D-Bus/portal failure only
//! degrades caps hotkey with a portal note — it never crashes the daemon.

use std::sync::Arc;

/// Spawn the portal session thread (fire-and-forget).
///
/// The thread binds `popup.hotkey` through `CreateSession` + `BindShortcuts`
/// (the portal may show a system dialog; the UI forewarns from the
/// `portal_bind_pending` status field) and follows a changed chord live: it
/// closes the session and binds the new chord, so the desktop asks again.
/// A refused bind is not retried until the chord changes. While the GNOME
/// extension is connected it owns the chord and the portal binds nothing
/// (GNOME 48+ offers both). Success and failure update only the portal
/// hotkey capability and its owned notes. `Activated` fires trigger the
/// popup like the X11/Windows hotkey threads.
#[cfg(all(unix, feature = "backend-wayland"))]
pub fn spawn_portal_session(state: Arc<crate::DaemonState>, _capture_primary: bool) {
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(e) => {
                note_failed(&state, &format!("portal runtime: {e}"));
                return;
            }
        };
        runtime.block_on(run_session(state));
    });
}

/// What the portal thread does next.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PortalStep {
    /// Nothing to change.
    Idle,
    /// Close any session and bind this chord in a new one.
    Bind(String),
    /// Close the session: the GNOME extension owns the chord.
    Release,
}

/// Plan the next portal step. `extension` holds off the portal while the
/// GNOME extension is connected or may still connect (start-up and
/// reconnect grace); `bound` is the chord of the live session; `refused`
/// is the last chord the portal refused, not retried until it changes.
fn portal_step(
    wanted: &str,
    extension: bool,
    bound: Option<&str>,
    refused: Option<&str>,
) -> PortalStep {
    if extension {
        return if bound.is_some() {
            PortalStep::Release
        } else {
            PortalStep::Idle
        };
    }
    if wanted.is_empty() || bound == Some(wanted) || refused == Some(wanted) {
        return PortalStep::Idle;
    }
    PortalStep::Bind(wanted.to_string())
}

/// How long the portal waits for the GNOME extension on a GNOME desktop:
/// at daemon start, and after the extension went away (it reconnects with
/// backoff), so a short gap shows no portal dialog.
const EXTENSION_START_GRACE: std::time::Duration = std::time::Duration::from_secs(10);
const EXTENSION_GONE_GRACE: std::time::Duration = std::time::Duration::from_secs(30);
/// How often the thread re-reads the chord and the extension link.
const PORTAL_POLL: std::time::Duration = std::time::Duration::from_secs(1);

/// The desktop app id the daemon registers with the portal. Host (not
/// sandboxed) processes have none, and KDE's portal refuses a
/// `GlobalShortcuts` session without one ("An app id is required").
pub const APP_ID: &str = clipmonger_core::DESKTOP_APP_ID;

/// Whether the portal should hold off for the extension: connected now, or
/// on a GNOME desktop within the grace since start or since last seen.
fn extension_holds(
    connected: bool,
    gnome: bool,
    since_start: std::time::Duration,
    since_seen: Option<std::time::Duration>,
) -> bool {
    connected
        || (gnome
            && match since_seen {
                Some(gone) => gone < EXTENSION_GONE_GRACE,
                None => since_start < EXTENSION_START_GRACE,
            })
}

#[cfg(all(unix, feature = "backend-wayland"))]
async fn run_session(state: Arc<crate::DaemonState>) {
    use ashpd::desktop::global_shortcuts::GlobalShortcuts;
    use clipmonger_backend_wayland::portal::parse_activated;
    use std::time::Instant;

    // Must precede every other portal call on the shared connection. A
    // portal older than 1.19 has no Registry; it then derives the id itself.
    let registered = match ashpd::AppID::try_from(APP_ID) {
        Ok(app_id) => ashpd::register_host_app(app_id).await.map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    if let Err(e) = &registered {
        eprintln!("daemon: portal app id {APP_ID} not registered: {e}");
    }
    let shortcuts = match GlobalShortcuts::new().await {
        Ok(shortcuts) => shortcuts,
        Err(e) => {
            note_failed(
                &state,
                &format!("portal unreachable, use compositor-native recipes: {e}"),
            );
            return;
        }
    };
    let Ok(mut activated) = shortcuts.receive_activated().await else {
        note_failed(&state, "portal activation signal unavailable");
        return;
    };
    let gnome = std::env::var("XDG_CURRENT_DESKTOP")
        .is_ok_and(|desktop| desktop.split(':').any(|d| d.eq_ignore_ascii_case("gnome")));
    let started = Instant::now();
    let mut extension_seen: Option<Instant> = None;
    let mut session = None;
    let mut bound: Option<String> = None;
    let mut refused: Option<String> = None;
    loop {
        let connected = state.extension_connected();
        if connected {
            extension_seen = Some(Instant::now());
        }
        let holds = extension_holds(
            connected,
            gnome,
            started.elapsed(),
            extension_seen.map(|seen| seen.elapsed()),
        );
        let wanted = state.popup_hotkey();
        match portal_step(&wanted, holds, bound.as_deref(), refused.as_deref()) {
            PortalStep::Idle => {}
            PortalStep::Release => {
                if let Some(old) = session.take() {
                    let _ = ashpd::desktop::Session::close(&old).await;
                }
                bound = None;
                note_extension(&state);
            }
            PortalStep::Bind(chord) => {
                if let Some(old) = session.take() {
                    let _ = ashpd::desktop::Session::close(&old).await;
                }
                bound = None;
                if let Some(new) = bind_chord(&shortcuts, &state, &chord).await {
                    refused = None;
                    bound = Some(chord);
                    session = Some(new);
                } else {
                    refused = Some(chord);
                }
            }
        }
        tokio::select! {
            signal = futures_util::StreamExt::next(&mut activated) => {
                let Some(signal) = signal else {
                    note_failed(&state, "portal activation signal ended");
                    return;
                };
                let payload = serde_json::json!({"shortcut_id": signal.shortcut_id()});
                if bound.is_some() && parse_activated(&payload).is_some() {
                    let activation_token = shortcut_activation_token(signal.options());
                    state.trigger_popup_activation(clipmonger_platform::types::HotkeyActivation {
                        x11_time: None,
                        activation_token,
                    });
                }
            }
            () = tokio::time::sleep(PORTAL_POLL) => {}
        }
    }
}

/// Bind `chord` in a new portal session (the desktop may ask the user).
/// Publishes the portal hotkey result; `None` when the session or the bind
/// failed (the session is closed again).
#[cfg(all(unix, feature = "backend-wayland"))]
async fn bind_chord(
    shortcuts: &ashpd::desktop::global_shortcuts::GlobalShortcuts,
    state: &crate::DaemonState,
    chord: &str,
) -> Option<ashpd::desktop::Session<ashpd::desktop::global_shortcuts::GlobalShortcuts>> {
    use ashpd::desktop::global_shortcuts::{BindShortcutsOptions, NewShortcut};
    use clipmonger_backend_wayland::portal::bind_requested;

    // ashpd's CreateSessionOptions type is private; Default is the only spelling.
    #[allow(clippy::default_trait_access)]
    let session = match shortcuts.create_session(Default::default()).await {
        Ok(session) => session,
        Err(e) => {
            note_failed(
                state,
                &format!("portal session failed, use compositor-native recipes: {e}"),
            );
            return None;
        }
    };
    let requests: Vec<NewShortcut> = bind_requested(&[chord.to_string()])
        .iter()
        .map(|request| {
            NewShortcut::new(request.id.clone(), "Clipmonger popup")
                .preferred_trigger(Some(request.trigger.as_str()))
        })
        .collect();
    state.set_portal_bind_pending(true);
    let outcome = shortcuts
        .bind_shortcuts(&session, &requests, None, BindShortcutsOptions::default())
        .await;
    state.set_portal_bind_pending(false);
    if let Err(e) = outcome {
        let _ = ashpd::desktop::Session::close(&session).await;
        note_failed(
            state,
            &format!("portal bind failed, use compositor-native recipes: {e}"),
        );
        return None;
    }
    note_bound(state);
    Some(session)
}

#[cfg(all(unix, feature = "backend-wayland"))]
fn shortcut_activation_token(
    options: &std::collections::HashMap<String, ashpd::zvariant::OwnedValue>,
) -> Option<String> {
    options
        .get("activation_token")
        .and_then(|value| <&str>::try_from(value).ok())
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
}

const PORTAL_LIVE: &str = "hotkey via GlobalShortcuts portal";
const PORTAL_RELEASED: &str = "hotkey via GNOME extension (portal holds no binding)";

/// Publish only the portal-owned hotkey result, retaining capture and policy.
fn note_bound(state: &crate::DaemonState) {
    publish_hotkey(state, None);
}

fn note_failed(state: &crate::DaemonState, note: &str) {
    publish_hotkey(state, Some(note));
}

/// The GNOME extension owns the chord; the portal holds no binding.
fn note_extension(state: &crate::DaemonState) {
    publish_hotkey(state, None);
    state.caps.update_backend("wayland", |caps| {
        caps.notes.retain(|note| note != PORTAL_LIVE);
        caps.notes.push(PORTAL_RELEASED.to_string());
    });
}

fn publish_hotkey(state: &crate::DaemonState, failure: Option<&str>) {
    use clipmonger_platform::types::Level;
    const UNAVAILABLE: &str = "hotkey Unavailable: use compositor-native recipes";
    const LIVE: &str = PORTAL_LIVE;
    state.caps.update_backend("wayland", |caps| {
        caps.notes.retain(|note| {
            note != UNAVAILABLE
                && note != LIVE
                && note != PORTAL_RELEASED
                && !note.starts_with("hotkey Degraded: portal ")
        });
        if let Some(note) = failure {
            caps.hotkey = Level::Degraded;
            caps.notes.push(format!("hotkey Degraded: {note}"));
        } else {
            caps.hotkey = Level::Full;
            caps.notes.push(LIVE.to_string());
        }
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn portal_step_binds_follows_changes_and_yields_to_the_extension() {
        use super::{portal_step, PortalStep};
        let bind = |chord: &str| PortalStep::Bind(chord.to_string());
        assert_eq!(
            portal_step("ctrl+alt+h", false, None, None),
            bind("ctrl+alt+h")
        );
        assert_eq!(
            portal_step("ctrl+alt+h", false, Some("ctrl+alt+h"), None),
            PortalStep::Idle,
            "bound: nothing to do"
        );
        assert_eq!(
            portal_step("ctrl+alt+j", false, Some("ctrl+alt+h"), None),
            bind("ctrl+alt+j"),
            "a changed chord rebinds live"
        );
        assert_eq!(
            portal_step("ctrl+alt+j", false, None, Some("ctrl+alt+j")),
            PortalStep::Idle,
            "a refused chord waits for a change: no dialog loop"
        );
        assert_eq!(
            portal_step("ctrl+alt+k", false, None, Some("ctrl+alt+j")),
            bind("ctrl+alt+k")
        );
        assert_eq!(
            portal_step("ctrl+alt+h", true, Some("ctrl+alt+h"), None),
            PortalStep::Release,
            "the extension takes over a bound chord"
        );
        assert_eq!(
            portal_step("ctrl+alt+h", true, None, None),
            PortalStep::Idle
        );
        assert_eq!(portal_step("", false, None, None), PortalStep::Idle);
    }

    #[test]
    fn extension_holds_off_the_portal_on_gnome_within_its_grace() {
        use super::{extension_holds, EXTENSION_GONE_GRACE, EXTENSION_START_GRACE};
        use std::time::Duration;
        let s = Duration::from_secs;
        assert!(
            extension_holds(true, false, s(100), None),
            "connected anywhere"
        );
        assert!(
            extension_holds(false, true, s(1), None),
            "GNOME start grace"
        );
        assert!(!extension_holds(false, true, EXTENSION_START_GRACE, None));
        assert!(
            extension_holds(false, true, s(100), Some(s(5))),
            "short reconnect gap"
        );
        assert!(!extension_holds(
            false,
            true,
            s(100),
            Some(EXTENSION_GONE_GRACE)
        ));
        assert!(
            !extension_holds(false, false, s(0), None),
            "KDE binds at once"
        );
    }

    #[cfg(all(unix, feature = "backend-wayland"))]
    #[test]
    fn activated_signal_options_preserve_only_valid_tokens() {
        use ashpd::zvariant::{OwnedValue, Str};
        let mut options = std::collections::HashMap::new();
        options.insert(
            "activation_token".to_string(),
            OwnedValue::from(Str::from("portal-token")),
        );
        assert_eq!(
            super::shortcut_activation_token(&options).as_deref(),
            Some("portal-token")
        );
        options.insert("activation_token".to_string(), OwnedValue::from(7u32));
        assert_eq!(super::shortcut_activation_token(&options), None);
        options.clear();
        assert_eq!(super::shortcut_activation_token(&options), None);
    }

    use super::*;
    use clipmonger_platform::fakes::FakeMonitor;
    use clipmonger_platform::types::{
        BackendCaps, CaptureEvent, CaptureOrigin, Error, Level, PasteCaps, PasteStrategy,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    fn check_readers(
        state: &crate::DaemonState,
        baseline: &serde_json::Value,
        monitor: &str,
        hotkey: &str,
    ) {
        let actual = state.handle("status", &serde_json::json!({})).unwrap()["caps"].clone();
        assert_eq!(
            actual,
            state.handle("caps.get", &serde_json::json!({})).unwrap()
        );
        assert_eq!(
            actual,
            state
                .handle("caps.refresh", &serde_json::json!({}))
                .unwrap()
        );
        assert_eq!(actual["backends"]["wayland"]["monitor"]["level"], monitor);
        assert_eq!(actual["backends"]["wayland"]["hotkey"]["level"], hotkey);
        for key in ["paste", "autostart", "primary_selection"] {
            assert_eq!(
                actual["backends"]["wayland"][key],
                baseline["backends"]["wayland"][key]
            );
        }
        for key in ["transfer", "tray"] {
            assert_eq!(
                actual["backends"]["wayland"][key]["level"],
                baseline["backends"]["wayland"][key]["level"]
            );
        }
        assert_eq!(
            actual["backends"]["unrelated"],
            baseline["backends"]["unrelated"]
        );
        let notes = actual["backends"]["wayland"]["notes"].as_array().unwrap();
        for note in [
            "privacy retained",
            "helper retained",
            "hotkey Degraded: unrelated policy retained",
        ] {
            assert!(notes.contains(&serde_json::Value::from(note)));
        }
        assert!(!notes.contains(&serde_json::Value::from(
            "hotkey Unavailable: use compositor-native recipes"
        )));
        let live_note = serde_json::Value::from("hotkey via GlobalShortcuts portal");
        assert_eq!(notes.contains(&live_note), hotkey == "full");
        assert_eq!(
            notes.iter().any(|note| note
                .as_str()
                .is_some_and(|note| note.starts_with("hotkey Degraded: portal "))),
            hotkey == "degraded"
        );
        if monitor == "degraded" {
            assert!(notes.contains(&serde_json::Value::from(
                "monitor Degraded: session clipboard unavailable"
            )));
        }
    }

    #[test]
    fn portal_writers_preserve_dead_monitor_and_unrelated_caps_during_recovery() {
        let dir = std::env::temp_dir().join(format!("portal-caps-{}", uuid::Uuid::now_v7()));
        let state = crate::DaemonState::new(clipmonger_core::Store::open(&dir).unwrap());
        let caps = BackendCaps {
            monitor: Level::Full,
            transfer: Level::Degraded,
            hotkey: Level::Unavailable,
            paste: PasteCaps {
                level: Level::Degraded,
                strategy: PasteStrategy::ElevatedRoute,
                reason: "helper policy".into(),
            },
            tray: Level::Unavailable,
            autostart: false,
            primary_selection: true,
            notes: vec![
                "privacy retained".into(),
                "helper retained".into(),
                "hotkey Degraded: unrelated policy retained".into(),
                "hotkey Unavailable: use compositor-native recipes".into(),
            ],
        };
        state.register_backend("wayland", caps.clone());
        state.register_backend("unrelated", caps);
        let baseline = state.caps_snapshot();

        let stop = AtomicBool::new(false);
        let mut attempts = 0;
        crate::capture::run_backend(
            Some(FakeMonitor::with_results(vec![Err(Error::Unavailable(
                "watcher exited".into(),
            ))])),
            Duration::ZERO,
            &stop,
            &state,
            "wayland",
            || {
                attempts += 1;
                if attempts == 1 {
                    note_bound(&state);
                    check_readers(&state, &baseline, "degraded", "full");
                    return Err(Error::Unavailable("factory still unavailable".into()));
                }
                note_failed(&state, "portal bind failed");
                check_readers(&state, &baseline, "degraded", "degraded");
                if attempts == 2 {
                    return Err(Error::Unavailable("factory still unavailable".into()));
                }
                Ok(FakeMonitor::with_results(vec![Ok(vec![CaptureEvent {
                    native_sequence: None,
                    offers: Vec::new(),
                    origin: CaptureOrigin::Clipboard,
                    sensitive: true,
                }])]))
            },
            |events| {
                assert_eq!(events.len(), 1);
                assert!(events[0].sensitive);
                check_readers(&state, &baseline, "full", "degraded");
                note_bound(&state);
                check_readers(&state, &baseline, "full", "full");
                note_failed(&state, "portal session failed");
                check_readers(&state, &baseline, "full", "degraded");
                stop.store(true, Ordering::Relaxed);
            },
            |_| {},
        );
        assert_eq!(attempts, 3);
    }
}
