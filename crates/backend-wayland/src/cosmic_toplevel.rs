//! COSMIC active-window tracking: `ext_foreign_toplevel_list_v1` for the
//! windows (`app_id`, title, lifetime) and `zcosmic_toplevel_info_v1` for
//! their state, including `activated`.
//!
//! cosmic-comp does not offer `zwlr_foreign_toplevel_manager_v1`. For each
//! ext handle this asks the info global for its `zcosmic_toplevel_handle_v1`
//! (`get_cosmic_toplevel`, info v2+); state changes arrive there and are
//! complete at the info global's `done`. Both feed the same
//! [`Tracker`] as the wlroots path, so callers see the same
//! [`Activation`]s.

use std::collections::{HashMap, HashSet};

use clipmonger_platform::types::{Error, Result};
use wayland_client::backend::ObjectId;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::{event_created_child, Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::{
    self, ExtForeignToplevelHandleV1,
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_list_v1::{
    self, ExtForeignToplevelListV1,
};

use crate::cosmic_protocol::toplevel_info::client::zcosmic_toplevel_handle_v1::{
    self, ZcosmicToplevelHandleV1,
};
use crate::cosmic_protocol::toplevel_info::client::zcosmic_toplevel_info_v1::{
    self, ZcosmicToplevelInfoV1,
};
use crate::toplevel::{state_is_activated, Activation, Tracker, COSMIC_INFO_MIN_VERSION};

/// Highest info version the vendored protocol file describes.
const COSMIC_INFO_MAX_VERSION: u32 = 3;

struct State {
    tracker: Tracker<ObjectId>,
    info: ZcosmicToplevelInfoV1,
    /// COSMIC handle per ext handle, destroyed with it.
    cosmic: HashMap<ObjectId, ZcosmicToplevelHandleV1>,
    /// Ext handles whose COSMIC state changed since the last info `done`.
    dirty: HashSet<ObjectId>,
    on_change: Box<dyn FnMut(&Activation) + Send>,
    finished: bool,
}

impl State {
    fn report(&mut self, change: Option<Activation>) {
        if let Some(activation) = change {
            (self.on_change)(&activation);
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } => {
                let id = toplevel.id();
                let cosmic = state.info.get_cosmic_toplevel(&toplevel, qh, id.clone());
                state.cosmic.insert(id, cosmic);
            }
            ext_foreign_toplevel_list_v1::Event::Finished => state.finished = true,
            _ => {}
        }
    }

    event_created_child!(State, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_foreign_toplevel_handle_v1::Event;
        let id = handle.id();
        match event {
            Event::Title { title } => state.tracker.title(&id, title),
            Event::AppId { app_id } => state.tracker.app_id(&id, app_id),
            Event::Done => {
                let change = state.tracker.done(&id);
                state.report(change);
            }
            Event::Closed => {
                state.dirty.remove(&id);
                let change = state.tracker.closed(&id);
                state.report(change);
                if let Some(cosmic) = state.cosmic.remove(&id) {
                    cosmic.destroy();
                }
                handle.destroy();
            }
            _ => {}
        }
    }
}

impl Dispatch<ZcosmicToplevelInfoV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZcosmicToplevelInfoV1,
        event: zcosmic_toplevel_info_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zcosmic_toplevel_info_v1::Event::Done = event {
            let dirty: Vec<ObjectId> = state.dirty.drain().collect();
            for change in state.tracker.done_all(dirty) {
                state.report(Some(change));
            }
        }
    }

    // The version 1 `toplevel` event is not sent from version 2 on; a
    // handle it creates has no ext handle and is ignored.
    event_created_child!(State, ZcosmicToplevelInfoV1, [
        zcosmic_toplevel_info_v1::EVT_TOPLEVEL_OPCODE => (ZcosmicToplevelHandleV1, ObjectId::null()),
    ]);
}

impl Dispatch<ZcosmicToplevelHandleV1, ObjectId> for State {
    fn event(
        state: &mut Self,
        _: &ZcosmicToplevelHandleV1,
        event: zcosmic_toplevel_handle_v1::Event,
        ext_id: &ObjectId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // COSMIC's `state` enum shares the xdg-toplevel values, so
        // `activated` is 2 as in the wlroots protocol.
        if let zcosmic_toplevel_handle_v1::Event::State { state: entries } = event {
            if ext_id.is_null() || !state.cosmic.contains_key(ext_id) {
                return;
            }
            state.tracker.state(ext_id, state_is_activated(&entries));
            state.dirty.insert(ext_id.clone());
        }
    }
}

/// Follow the active toplevel on COSMIC (see [`crate::toplevel::track`]).
///
/// # Errors
///
/// [`Error::Unavailable`] without a Wayland connection, when either global
/// is missing (or the info global is older than version 2), or when the
/// connection fails.
pub fn track(on_change: Box<dyn FnMut(&Activation) + Send>) -> Result<()> {
    let unavailable =
        |e: &dyn std::fmt::Display| Error::Unavailable(format!("cosmic toplevel info: {e}"));
    let conn = Connection::connect_to_env().map_err(|e| unavailable(&e))?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(|e| unavailable(&e))?;
    let qh = queue.handle();
    let info: ZcosmicToplevelInfoV1 = globals
        .bind(&qh, COSMIC_INFO_MIN_VERSION..=COSMIC_INFO_MAX_VERSION, ())
        .map_err(|e| unavailable(&e))?;
    let _list: ExtForeignToplevelListV1 =
        globals.bind(&qh, 1..=1, ()).map_err(|e| unavailable(&e))?;
    let mut state = State {
        tracker: Tracker::default(),
        info,
        cosmic: HashMap::new(),
        dirty: HashSet::new(),
        on_change,
        finished: false,
    };
    while !state.finished {
        queue
            .blocking_dispatch(&mut state)
            .map_err(|e| unavailable(&e))?;
    }
    Ok(())
}
