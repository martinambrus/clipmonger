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
/// `portal_bind_pending` status field). Success re-registers the wayland
/// caps with the portal hotkey live; failure re-registers them degraded
/// with a portal note. `Activated` fires mint a gesture token like the
/// X11/Windows hotkey threads.
#[cfg(all(unix, feature = "backend-wayland"))]
pub fn spawn_portal_session(state: Arc<crate::DaemonState>, chord: String, capture_primary: bool) {
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(e) => {
                note_failed(&state, capture_primary, &format!("portal runtime: {e}"));
                return;
            }
        };
        runtime.block_on(run_session(state, chord, capture_primary));
    });
}

#[cfg(all(unix, feature = "backend-wayland"))]
async fn run_session(state: Arc<crate::DaemonState>, chord: String, capture_primary: bool) {
    use clipmonger_backend_wayland::portal::{bind_requested, parse_activated};

    let shortcuts = match ashpd::desktop::global_shortcuts::GlobalShortcuts::new().await {
        Ok(shortcuts) => shortcuts,
        Err(e) => {
            note_failed(
                &state,
                capture_primary,
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
                capture_primary,
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
            capture_primary,
            &format!("portal bind failed, use compositor-native recipes: {e}"),
        );
        return;
    }
    state.register_backend(
        "wayland",
        clipmonger_backend_wayland::caps::caps_for(
            &clipmonger_backend_wayland::probe_current(),
            capture_primary,
            true,
        ),
    );
    let Ok(mut activated) = shortcuts.receive_activated().await else {
        return;
    };
    while let Some(signal) = futures_util::StreamExt::next(&mut activated).await {
        let payload = serde_json::json!({"shortcut_id": signal.shortcut_id()});
        if parse_activated(&payload).is_some() {
            state.mint_gesture_token();
            eprintln!("daemon: portal hotkey fired (daemon-event only)");
        }
    }
}

/// Re-register wayland caps degraded with a portal note (never a crash).
#[cfg(all(unix, feature = "backend-wayland"))]
fn note_failed(state: &Arc<crate::DaemonState>, capture_primary: bool, note: &str) {
    use clipmonger_platform::types::Level;
    let mut caps = clipmonger_backend_wayland::caps::caps_for(
        &clipmonger_backend_wayland::probe_current(),
        capture_primary,
        false,
    );
    // A missing portal degrades the hotkey leg only; capture keeps working.
    if caps.hotkey == Level::Unavailable {
        caps.hotkey = Level::Degraded;
        caps.notes.push(format!("hotkey Degraded: {note}"));
    }
    state.register_backend("wayland", caps);
}
