//! Live compositor probe: registry globals to [`ProbeState`].
//!
//! Connects to `$WAYLAND_DISPLAY`, dumps the registry once, and folds it
//! through [`probe_step`](crate::probe::probe_step). Any failure (no
//! compositor, failed roundtrip) yields the default `None` state —
//! capability mapping degrades honestly instead of crashing. The primary
//! verdict comes from
//! `wl_clipboard_rs::utils::is_primary_selection_supported()` and is only
//! meaningful beside a capable protocol (enforced by `probe_step`).

use wayland_client::protocol::wl_registry::{Event as RegistryEvent, WlRegistry};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};

use crate::probe::{probe_step, Globals, ProbeState};

struct State {
    ext: Option<u32>,
    wlr: Option<u32>,
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(
        _state: &mut Self,
        _: &WlRegistry,
        event: <WlRegistry as Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let RegistryEvent::Global {
            interface, version, ..
        } = event
        {
            match interface.as_str() {
                "ext_data_control_manager_v1" => {
                    _state.ext = Some(_state.ext.map_or(version, |prev| prev.max(version)));
                }
                "zwlr_data_control_manager_v1" => {
                    _state.wlr = Some(_state.wlr.map_or(version, |prev| prev.max(version)));
                }
                _ => {}
            }
        }
    }
}

/// Probe the ambient compositor once; `None` protocol when unreachable.
#[must_use]
pub fn probe_current() -> ProbeState {
    let Ok(conn) = Connection::connect_to_env() else {
        return ProbeState::default();
    };
    let display = conn.display();
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let _registry = display.get_registry(&qh, ());
    let mut state = State {
        ext: None,
        wlr: None,
    };
    if queue.roundtrip(&mut state).is_err() {
        return ProbeState::default();
    }
    let primary_ext = wl_clipboard_rs::utils::is_primary_selection_supported().unwrap_or(false);
    probe_step(
        &ProbeState::default(),
        &Globals {
            ext: state.ext,
            wlr: state.wlr,
            primary_ext,
        },
    )
}
