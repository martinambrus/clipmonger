//! Linux X11 backend pure logic (seg-05): no X connection here.
//!
//! Atom/MIME map, INCR transfer state machines, `.desktop` autostart
//! rendering/parsing, lock-modifier expansion, manager/save-targets logic,
//! primary-selection debouncing, and capability constructors. Session
//! modules (x11rb connection) arrive in the next task.

pub mod caps;
pub mod desktop;
pub mod incr;
pub mod lockmods;
pub mod manager_logic;
pub mod mime;
pub mod primary;

/// Healthy capability matrix for this backend (see [`caps::normal_caps`]).
#[must_use]
pub fn caps() -> clipmonger_platform::types::BackendCaps {
    caps::normal_caps()
}
