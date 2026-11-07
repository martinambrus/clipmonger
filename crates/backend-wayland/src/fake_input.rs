//! `KWin` paste chord over `org_kde_kwin_fake_input`.
//!
//! `KWin` (Plasma) offers this protocol to clients it trusts. KDE Connect
//! uses it for its remote keyboard. `KWin` trusts a client whose executable
//! has a `.desktop` file declaring
//! `X-KDE-Wayland-Interfaces=org_kde_kwin_fake_input`. Other compositors
//! do not advertise the global, and [`FakeInput::connect`] fails with
//! [`Error::Unavailable`].
//!
//! The chord uses Linux evdev key codes; `KWin` maps them through the
//! active keymap, like a real keyboard.

use std::time::Duration;

use clipmonger_platform::types::{Error, Result};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};
use wayland_protocols_plasma::fake_input::client::org_kde_kwin_fake_input::OrgKdeKwinFakeInput;

/// evdev `KEY_LEFTCTRL`.
const KEY_LEFTCTRL: u32 = 29;
/// evdev `KEY_LEFTSHIFT`.
const KEY_LEFTSHIFT: u32 = 42;
/// evdev `KEY_V`.
const KEY_V: u32 = 47;
/// `wl_keyboard` key state: released.
const RELEASED: u32 = 0;
/// `wl_keyboard` key state: pressed.
const PRESSED: u32 = 1;
/// `keyboard_key` exists from version 4.
const MIN_VERSION: u32 = 4;
/// Highest version the bundled protocol file describes.
const MAX_VERSION: u32 = 5;
/// Pause between key events so the target sees a held modifier.
const KEY_GAP: Duration = Duration::from_millis(12);

/// The key events for one paste chord, in order (`(key, state)`).
#[must_use]
pub fn chord_events(shift: bool) -> Vec<(u32, u32)> {
    let mut mods = vec![KEY_LEFTCTRL];
    if shift {
        mods.push(KEY_LEFTSHIFT);
    }
    let mut events: Vec<(u32, u32)> = mods.iter().map(|&key| (key, PRESSED)).collect();
    events.push((KEY_V, PRESSED));
    events.push((KEY_V, RELEASED));
    events.extend(mods.iter().rev().map(|&key| (key, RELEASED)));
    events
}

struct State;

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as wayland_client::Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<OrgKdeKwinFakeInput, ()> for State {
    fn event(
        _: &mut Self,
        _: &OrgKdeKwinFakeInput,
        _: <OrgKdeKwinFakeInput as wayland_client::Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// One authenticated fake-input connection.
pub struct FakeInput {
    queue: EventQueue<State>,
    input: OrgKdeKwinFakeInput,
}

impl FakeInput {
    /// Connect to the session compositor and bind its fake-input global.
    ///
    /// # Errors
    ///
    /// [`Error::Unavailable`] without a Wayland connection or when the
    /// compositor does not offer `org_kde_kwin_fake_input` (v4+) to us.
    pub fn connect() -> Result<Self> {
        let conn = Connection::connect_to_env()
            .map_err(|e| Error::Unavailable(format!("wayland connect: {e}")))?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn)
            .map_err(|e| Error::Unavailable(format!("wayland registry: {e}")))?;
        let input: OrgKdeKwinFakeInput = globals
            .bind(&queue.handle(), MIN_VERSION..=MAX_VERSION, ())
            .map_err(|e| Error::Unavailable(format!("kwin fake input: {e}")))?;
        input.authenticate(
            "Clipmonger".to_string(),
            "Paste the clipboard item you picked".to_string(),
        );
        queue
            .roundtrip(&mut State)
            .map_err(|e| Error::Unavailable(format!("kwin fake input: {e}")))?;
        Ok(Self { queue, input })
    }

    /// Press and release the paste chord (`Ctrl+V`, or `Ctrl+Shift+V`).
    ///
    /// # Errors
    ///
    /// [`Error::Unavailable`] when the compositor connection fails. `KWin`
    /// drops events from an untrusted client without an error, so success
    /// here does not prove the chord arrived.
    pub fn paste_chord(&mut self, shift: bool) -> Result<()> {
        for (key, state) in chord_events(shift) {
            self.input.keyboard_key(key, state);
            self.queue
                .flush()
                .map_err(|e| Error::Unavailable(format!("kwin fake input: {e}")))?;
            std::thread::sleep(KEY_GAP);
        }
        self.queue
            .roundtrip(&mut State)
            .map_err(|e| Error::Unavailable(format!("kwin fake input: {e}")))?;
        Ok(())
    }
}

impl Drop for FakeInput {
    fn drop(&mut self) {
        if wayland_client::Proxy::version(&self.input) >= 5 {
            self.input.destroy();
            let _ = self.queue.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chord_presses_modifiers_first_and_releases_in_reverse() {
        assert_eq!(chord_events(false), [(29, 1), (47, 1), (47, 0), (29, 0)]);
        assert_eq!(
            chord_events(true),
            [(29, 1), (42, 1), (47, 1), (47, 0), (42, 0), (29, 0)]
        );
    }
}
