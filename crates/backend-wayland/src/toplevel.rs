//! Active-window tracking over `zwlr_foreign_toplevel_manager_v1`, or on
//! COSMIC over `ext_foreign_toplevel_list_v1` with `zcosmic_toplevel_info_v1`
//! ([`crate::cosmic_toplevel`]).
//!
//! A Wayland client cannot see which window has focus. wlroots-based
//! compositors (Sway, Hyprland, and others) offer the foreign-toplevel
//! protocol to taskbars: it lists every toplevel window with its `app_id`,
//! title and state, including `activated`. COSMIC splits the same data over
//! the ext list (`app_id`, title) and its own info extension (state).
//! [`track`] follows that state and reports each change of the active
//! window. Compositors that offer neither set make [`track`] fail with
//! [`Error::Unavailable`].

use std::collections::HashMap;
use std::hash::Hash;

use clipmonger_platform::types::{Error, Result};
use wayland_client::backend::ObjectId;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::{event_created_child, Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_handle_v1::{
    self, ZwlrForeignToplevelHandleV1,
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_manager_v1::{
    self, ZwlrForeignToplevelManagerV1,
};

/// Interface name of the manager global.
const MANAGER_INTERFACE: &str = "zwlr_foreign_toplevel_manager_v1";
/// Interface name of the ext toplevel list global (COSMIC set).
pub(crate) const EXT_LIST_INTERFACE: &str = "ext_foreign_toplevel_list_v1";
/// Interface name of COSMIC's toplevel info global.
pub(crate) const COSMIC_INFO_INTERFACE: &str = "zcosmic_toplevel_info_v1";
/// `zcosmic_toplevel_info_v1.get_cosmic_toplevel` exists from version 2.
pub(crate) const COSMIC_INFO_MIN_VERSION: u32 = 2;
/// Highest manager version the bundled protocol file describes.
const MAX_VERSION: u32 = 3;
/// `zwlr_foreign_toplevel_handle_v1.state` entry: activated.
const STATE_ACTIVATED: u32 = 2;

/// The toplevel protocol set a compositor offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocols {
    /// `zwlr_foreign_toplevel_manager_v1` (wlroots family).
    Wlr,
    /// `ext_foreign_toplevel_list_v1` with `zcosmic_toplevel_info_v1` v2+.
    Cosmic,
}

/// Pick the protocol set from the advertised `(interface, version)`
/// globals: wlroots first, else the COSMIC pair.
#[must_use]
pub fn protocols_for<'a>(globals: impl IntoIterator<Item = (&'a str, u32)>) -> Option<Protocols> {
    let mut ext_list = false;
    let mut cosmic_info = false;
    for (interface, version) in globals {
        match interface {
            MANAGER_INTERFACE => return Some(Protocols::Wlr),
            EXT_LIST_INTERFACE => ext_list = true,
            COSMIC_INFO_INTERFACE => cosmic_info |= version >= COSMIC_INFO_MIN_VERSION,
            _ => {}
        }
    }
    (ext_list && cosmic_info).then_some(Protocols::Cosmic)
}

/// The active window after a change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Activation {
    /// The window's `app_id` (empty when no window is active).
    pub app_id: String,
    /// The window's title (empty when no window is active).
    pub title: String,
}

/// Whether a `state` event array holds the `activated` entry.
#[must_use]
pub fn state_is_activated(state: &[u8]) -> bool {
    state
        .as_chunks::<4>()
        .0
        .iter()
        .any(|&entry| u32::from_ne_bytes(entry) == STATE_ACTIVATED)
}

#[derive(Debug, Default)]
struct Toplevel {
    app_id: String,
    title: String,
    activated: bool,
}

/// Toplevel bookkeeping, separate from the protocol objects (pure).
///
/// Property events are pending until the window's `done`; `done` and
/// `closed` return the new [`Activation`] when the active window changed.
/// An empty activation means no toplevel is active now (the active one
/// closed or lost focus to a non-toplevel, such as a layer-shell panel).
#[derive(Debug)]
pub struct Tracker<K> {
    toplevels: HashMap<K, Toplevel>,
    active: Option<(K, String)>,
}

impl<K> Default for Tracker<K> {
    fn default() -> Self {
        Self {
            toplevels: HashMap::new(),
            active: None,
        }
    }
}

impl<K: Eq + Hash + Clone> Tracker<K> {
    /// A `title` event.
    pub fn title(&mut self, id: &K, title: String) {
        self.toplevels.entry(id.clone()).or_default().title = title;
    }

    /// An `app_id` event.
    pub fn app_id(&mut self, id: &K, app_id: String) {
        self.toplevels.entry(id.clone()).or_default().app_id = app_id;
    }

    /// A `state` event.
    pub fn state(&mut self, id: &K, activated: bool) {
        self.toplevels.entry(id.clone()).or_default().activated = activated;
    }

    /// A `done` event: the window's properties are now current.
    pub fn done(&mut self, id: &K) -> Option<Activation> {
        let toplevel = self.toplevels.get(id)?;
        let is_active = self.active.as_ref().is_some_and(|(active, _)| active == id);
        if toplevel.activated {
            let same = self
                .active
                .as_ref()
                .is_some_and(|(active, app_id)| active == id && *app_id == toplevel.app_id);
            if same {
                return None;
            }
            self.active = Some((id.clone(), toplevel.app_id.clone()));
            return Some(Activation {
                app_id: toplevel.app_id.clone(),
                title: toplevel.title.clone(),
            });
        }
        if is_active {
            self.active = None;
            return Some(Activation::default());
        }
        None
    }

    /// The `done` of several windows at once (COSMIC's info `done`):
    /// windows that lost activation first, so the result ends on the
    /// window that gained it.
    pub fn done_all(&mut self, ids: impl IntoIterator<Item = K>) -> Vec<Activation> {
        let (gained, lost): (Vec<K>, Vec<K>) = ids
            .into_iter()
            .partition(|id| self.toplevels.get(id).is_some_and(|t| t.activated));
        lost.iter()
            .chain(gained.iter())
            .filter_map(|id| self.done(id))
            .collect()
    }

    /// A `closed` event: the window is gone.
    pub fn closed(&mut self, id: &K) -> Option<Activation> {
        self.toplevels.remove(id);
        if self.active.as_ref().is_some_and(|(active, _)| active == id) {
            self.active = None;
            return Some(Activation::default());
        }
        None
    }
}

struct State {
    tracker: Tracker<ObjectId>,
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

/// Registry probe state (no events).
struct Probe;

impl Dispatch<WlRegistry, GlobalListContents> for Probe {
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

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_foreign_toplevel_manager_v1::Event::Finished = event {
            state.finished = true;
        }
    }

    event_created_child!(State, ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_foreign_toplevel_handle_v1::Event;
        let id = handle.id();
        match event {
            Event::Title { title } => state.tracker.title(&id, title),
            Event::AppId { app_id } => state.tracker.app_id(&id, app_id),
            Event::State { state: entries } => {
                state.tracker.state(&id, state_is_activated(&entries));
            }
            Event::Done => {
                let change = state.tracker.done(&id);
                state.report(change);
            }
            Event::Closed => {
                let change = state.tracker.closed(&id);
                state.report(change);
                handle.destroy();
            }
            _ => {}
        }
    }
}

/// The toplevel protocol set the session compositor offers. Only reads the
/// global list.
#[must_use]
pub fn offered() -> Option<Protocols> {
    let conn = Connection::connect_to_env().ok()?;
    let (globals, _queue) = registry_queue_init::<Probe>(&conn).ok()?;
    globals.contents().with_list(|list| {
        protocols_for(
            list.iter()
                .map(|global| (global.interface.as_str(), global.version)),
        )
    })
}

/// Whether the session compositor offers a toplevel protocol set [`track`]
/// can follow.
#[must_use]
pub fn available() -> bool {
    offered().is_some()
}

/// Follow the active toplevel, calling `on_change` with each new active
/// window. Blocks until the compositor ends the stream or the connection;
/// run it on a dedicated thread.
///
/// # Errors
///
/// [`Error::Unavailable`] without a Wayland connection, when the compositor
/// offers neither protocol set ([`Protocols`]), or when the connection
/// fails.
pub fn track(on_change: Box<dyn FnMut(&Activation) + Send>) -> Result<()> {
    match offered() {
        Some(Protocols::Wlr) => track_wlr(on_change),
        Some(Protocols::Cosmic) => crate::cosmic_toplevel::track(on_change),
        None => Err(Error::Unavailable(
            "no foreign-toplevel protocol offered".to_string(),
        )),
    }
}

/// [`track`] over `zwlr_foreign_toplevel_manager_v1`.
fn track_wlr(on_change: Box<dyn FnMut(&Activation) + Send>) -> Result<()> {
    let unavailable =
        |e: &dyn std::fmt::Display| Error::Unavailable(format!("foreign toplevel: {e}"));
    let conn = Connection::connect_to_env().map_err(|e| unavailable(&e))?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(|e| unavailable(&e))?;
    let _manager: ZwlrForeignToplevelManagerV1 = globals
        .bind(&queue.handle(), 1..=MAX_VERSION, ())
        .map_err(|e| unavailable(&e))?;
    let mut state = State {
        tracker: Tracker::default(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(states: &[u32]) -> Vec<u8> {
        states.iter().flat_map(|s| s.to_ne_bytes()).collect()
    }

    fn active(app_id: &str, title: &str) -> Activation {
        Activation {
            app_id: app_id.to_string(),
            title: title.to_string(),
        }
    }

    #[test]
    fn activated_entry_is_found_among_others() {
        assert!(state_is_activated(&entries(&[0, 2])));
        assert!(state_is_activated(&entries(&[2])));
        assert!(!state_is_activated(&entries(&[0, 1, 3])));
        assert!(!state_is_activated(&[]));
        assert!(!state_is_activated(&[2, 0]), "a partial entry is ignored");
    }

    #[test]
    fn wlroots_wins_and_cosmic_needs_both_globals() {
        let wlr = [
            ("wl_seat", 7),
            (MANAGER_INTERFACE, 3),
            (EXT_LIST_INTERFACE, 1),
        ];
        assert_eq!(protocols_for(wlr), Some(Protocols::Wlr));
        let cosmic = [(EXT_LIST_INTERFACE, 1), (COSMIC_INFO_INTERFACE, 3)];
        assert_eq!(protocols_for(cosmic), Some(Protocols::Cosmic));
        assert_eq!(
            protocols_for([(EXT_LIST_INTERFACE, 1), (COSMIC_INFO_INTERFACE, 1)]),
            None,
            "info v1 has no get_cosmic_toplevel"
        );
        assert_eq!(protocols_for([(COSMIC_INFO_INTERFACE, 3)]), None);
        assert_eq!(protocols_for([(EXT_LIST_INTERFACE, 1)]), None);
        assert_eq!(protocols_for([]), None);
    }

    #[test]
    fn batched_done_reports_the_gained_window_last() {
        let mut tracker = Tracker::default();
        tracker.app_id(&1, "foot".to_string());
        tracker.state(&1, true);
        assert_eq!(tracker.done_all([1]), [active("foot", "")]);
        // One batch: the popup gains focus, the terminal loses it; the
        // gained window is listed first but reported last.
        tracker.app_id(&2, "com.martinambrus.clipmonger".to_string());
        tracker.state(&2, true);
        tracker.state(&1, false);
        assert_eq!(
            tracker.done_all([2, 1]),
            [
                Activation::default(),
                active("com.martinambrus.clipmonger", "")
            ]
        );
        // Back: the popup loses focus, the terminal gains it.
        tracker.state(&2, false);
        tracker.state(&1, true);
        assert_eq!(
            tracker.done_all([1, 2]),
            [Activation::default(), active("foot", "")]
        );
        assert!(tracker.done_all([1, 2]).is_empty(), "no change, no report");
    }

    #[test]
    fn properties_report_on_done_and_only_on_change() {
        let mut tracker = Tracker::default();
        tracker.app_id(&1, "foot".to_string());
        tracker.title(&1, "~".to_string());
        tracker.state(&1, true);
        assert_eq!(tracker.done(&1), Some(active("foot", "~")));
        tracker.title(&1, "vim".to_string());
        assert_eq!(tracker.done(&1), None, "a title change alone is quiet");
        tracker.app_id(&2, "firefox".to_string());
        tracker.state(&2, false);
        assert_eq!(tracker.done(&2), None, "an inactive window is quiet");
    }

    #[test]
    fn focus_moves_between_windows_in_either_event_order() {
        let mut tracker = Tracker::default();
        tracker.app_id(&1, "foot".to_string());
        tracker.state(&1, true);
        assert_eq!(tracker.done(&1), Some(active("foot", "")));
        // The new window reports first, the old one's deactivation after.
        tracker.app_id(&2, "com.martinambrus.clipmonger".to_string());
        tracker.state(&2, true);
        assert_eq!(
            tracker.done(&2),
            Some(active("com.martinambrus.clipmonger", ""))
        );
        tracker.state(&1, false);
        assert_eq!(tracker.done(&1), None);
        // The popup closes and focus returns.
        assert_eq!(tracker.closed(&2), Some(Activation::default()));
        tracker.state(&1, true);
        assert_eq!(tracker.done(&1), Some(active("foot", "")));
        // The active window loses focus to nothing.
        tracker.state(&1, false);
        assert_eq!(tracker.done(&1), Some(Activation::default()));
        assert_eq!(tracker.closed(&1), None);
    }
}
