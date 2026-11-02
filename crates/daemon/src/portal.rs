//! Wayland `GlobalShortcuts` portal session (seg-06 S4, W-F4).
//!
//! The live session is `ashpd`-async on a daemon-owned thread running its
//! own current-thread tokio runtime; the backend crate stays tokio-free.
//! Pure shortcut ids, `Activated` parsing, and bind planning come from
//! `clipmonger_backend_wayland::portal`. Any D-Bus/portal failure only
//! degrades caps hotkey with a portal note — it never crashes the daemon.

use std::sync::Arc;

/// Spawn the portal session thread for `chord` (fire-and-forget).
///
/// The thread attempts `CreateSession` + `BindShortcuts` on the first run
/// (the portal may show a system dialog; the UI forewarns from the
/// `portal_bind_pending` status field). Success and failure update only
/// the portal hotkey capability and its owned notes. `Activated` fires
/// trigger the popup like the X11/Windows hotkey threads.
#[cfg(all(unix, feature = "backend-wayland"))]
pub fn spawn_portal_session(state: Arc<crate::DaemonState>, chord: String, _capture_primary: bool) {
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
        runtime.block_on(run_session(state, chord));
    });
}

#[cfg(all(unix, feature = "backend-wayland"))]
async fn run_session(state: Arc<crate::DaemonState>, chord: String) {
    use clipmonger_backend_wayland::portal::{bind_requested, parse_activated};

    let shortcuts = match ashpd::desktop::global_shortcuts::GlobalShortcuts::new().await {
        Ok(shortcuts) => shortcuts,
        Err(e) => {
            note_failed(
                &state,
                &format!("portal unreachable, use compositor-native recipes: {e}"),
            );
            return;
        }
    };
    // ashpd's CreateSessionOptions type is private; Default is the only spelling.
    #[allow(clippy::default_trait_access)]
    let session = match shortcuts.create_session(Default::default()).await {
        Ok(session) => session,
        Err(e) => {
            note_failed(
                &state,
                &format!("portal session failed, use compositor-native recipes: {e}"),
            );
            return;
        }
    };
    let plan = bind_requested(&[chord]);
    let mut bound_shortcuts = Vec::new();
    for request in &plan {
        bound_shortcuts.push(
            ashpd::desktop::global_shortcuts::NewShortcut::new(
                request.id.clone(),
                "Clipmonger popup",
            )
            .preferred_trigger(Some(request.trigger.as_str())),
        );
    }
    state.set_portal_bind_pending(true);
    let bind_outcome = shortcuts
        .bind_shortcuts(
            &session,
            &bound_shortcuts,
            None,
            ashpd::desktop::global_shortcuts::BindShortcutsOptions::default(),
        )
        .await;
    state.set_portal_bind_pending(false);
    if let Err(e) = bind_outcome {
        note_failed(
            &state,
            &format!("portal bind failed, use compositor-native recipes: {e}"),
        );
        return;
    }
    note_bound(&state);
    let Ok(mut activated) = shortcuts.receive_activated().await else {
        return;
    };
    while let Some(signal) = futures_util::StreamExt::next(&mut activated).await {
        let payload = serde_json::json!({"shortcut_id": signal.shortcut_id()});
        if parse_activated(&payload).is_some() {
            state.trigger_popup(crate::popup::PopupSource::Press);
        }
    }
}

/// Publish only the portal-owned hotkey result, retaining capture and policy.
fn note_bound(state: &crate::DaemonState) {
    publish_hotkey(state, None);
}

fn note_failed(state: &crate::DaemonState, note: &str) {
    publish_hotkey(state, Some(note));
}

fn publish_hotkey(state: &crate::DaemonState, failure: Option<&str>) {
    use clipmonger_platform::types::Level;
    const UNAVAILABLE: &str = "hotkey Unavailable: use compositor-native recipes";
    const LIVE: &str = "hotkey via GlobalShortcuts portal";
    state.caps.update_backend("wayland", |caps| {
        caps.notes.retain(|note| {
            note != UNAVAILABLE && note != LIVE && !note.starts_with("hotkey Degraded: portal ")
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
