//! Paste chord over `zwp_virtual_keyboard_manager_v1` (wlroots family, COSMIC).
//!
//! Sway, Hyprland, other wlroots-based compositors and COSMIC offer the virtual
//! keyboard protocol to ordinary clients (`wtype` uses it). A virtual
//! keyboard brings its own XKB keymap. It uploads the seat's current keymap
//! (read from a `wl_keyboard`): a different keymap makes the compositor send
//! the focused client a new one in the middle of the chord, and GTK3 then
//! drops the modified key. Only when the seat sends no keymap does it fall
//! back to [`keymap_text`], which maps just the three keys of the chord.
//! Compositors that do not advertise the global make
//! [`VirtualKeyboard::connect`] fail with [`Error::Unavailable`].
//!
//! Key codes are Linux evdev codes; the keymap names them by XKB keycode
//! (evdev + 8), as `wl_keyboard` does.

use std::fs::File;
use std::io::Write as _;
use std::os::fd::{AsFd as _, OwnedFd};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use clipmonger_platform::types::{Error, Result};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_keyboard::{self, KeymapFormat, WlKeyboard};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy as _, QueueHandle, WEnum};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;

/// Interface name of the manager global.
const MANAGER_INTERFACE: &str = "zwp_virtual_keyboard_manager_v1";
/// evdev `KEY_LEFTCTRL`.
const KEY_LEFTCTRL: u32 = 29;
/// evdev `KEY_LEFTSHIFT`.
const KEY_LEFTSHIFT: u32 = 42;
/// evdev `KEY_V`.
const KEY_V: u32 = 47;
/// XKB keycodes are evdev codes plus this offset.
const XKB_OFFSET: u32 = 8;
/// `wl_keyboard` key state: released.
const RELEASED: u32 = 0;
/// `wl_keyboard` key state: pressed.
const PRESSED: u32 = 1;
/// XKB `Shift` modifier mask. XKB fixes the real modifiers' indices
/// (Shift 0, Lock 1, Control 2, Mod1..Mod5), so the masks hold for the
/// seat's keymap as well as for [`keymap_text`].
pub const SHIFT_MASK: u32 = 1;
/// XKB `Control` modifier mask (see [`SHIFT_MASK`]).
pub const CONTROL_MASK: u32 = 4;
/// Roundtrips to wait for the seat keyboard's `keymap` event.
const KEYMAP_ROUNDTRIPS: usize = 3;
/// Longest wait for the seat keyboard's `keymap` event.
const KEYMAP_WAIT: Duration = Duration::from_millis(300);
/// Pause between requests so the target sees a held modifier.
const KEY_GAP: Duration = Duration::from_millis(12);

/// Pause between the warm-up Shift tap and the chord.
const WARM_UP: Duration = Duration::from_millis(80);

/// One request of the paste chord.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// `key(evdev code, state)`.
    Key(u32, u32),
    /// `modifiers(depressed mask, 0, 0, 0)`.
    Modifiers(u32),
}

/// The XKB keymap the virtual keyboard uploads: `Control_L`, `Shift_L` and
/// `v` on their usual evdev keys. The compositor compiles it, resolving the
/// `complete` types and compatibility includes.
#[must_use]
pub fn keymap_text() -> String {
    let ctrl = KEY_LEFTCTRL + XKB_OFFSET;
    let shift = KEY_LEFTSHIFT + XKB_OFFSET;
    let v = KEY_V + XKB_OFFSET;
    format!(
        "xkb_keymap {{\n\
         \x20   xkb_keycodes \"clipmonger\" {{\n\
         \x20       minimum = 8;\n\
         \x20       maximum = 255;\n\
         \x20       <LCTL> = {ctrl};\n\
         \x20       <LFSH> = {shift};\n\
         \x20       <AB04> = {v};\n\
         \x20   }};\n\
         \x20   xkb_types \"clipmonger\" {{ include \"complete\" }};\n\
         \x20   xkb_compatibility \"clipmonger\" {{ include \"complete\" }};\n\
         \x20   xkb_symbols \"clipmonger\" {{\n\
         \x20       key <LCTL> {{ [ Control_L ] }};\n\
         \x20       key <LFSH> {{ [ Shift_L ] }};\n\
         \x20       key <AB04> {{ [ v, V ] }};\n\
         \x20       modifier_map Control {{ <LCTL> }};\n\
         \x20       modifier_map Shift {{ <LFSH> }};\n\
         \x20   }};\n\
         }};\n"
    )
}

/// The requests for one paste chord, in order: each modifier key followed
/// by the modifier state it leaves, `v` pressed and released, then the
/// modifiers released in reverse.
#[must_use]
pub fn chord_steps(shift: bool) -> Vec<Step> {
    let mut mods = vec![(KEY_LEFTCTRL, CONTROL_MASK)];
    if shift {
        mods.push((KEY_LEFTSHIFT, SHIFT_MASK));
    }
    let mut steps = Vec::new();
    let mut mask = 0;
    for &(key, bit) in &mods {
        mask |= bit;
        steps.push(Step::Key(key, PRESSED));
        steps.push(Step::Modifiers(mask));
    }
    steps.push(Step::Key(KEY_V, PRESSED));
    steps.push(Step::Key(KEY_V, RELEASED));
    for &(key, bit) in mods.iter().rev() {
        mask &= !bit;
        steps.push(Step::Key(key, RELEASED));
        steps.push(Step::Modifiers(mask));
    }
    steps
}

/// Connection state: the seat keymap once `wl_keyboard.keymap` arrived.
#[derive(Default)]
struct State {
    keymap: Option<(OwnedFd, u32)>,
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Keymap { format, fd, size } = event {
            if format == WEnum::Value(KeymapFormat::XkbV1) && size > 0 {
                state.keymap = Some((fd, size));
            }
        }
    }
}

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

impl Dispatch<WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlSeat,
        _: <WlSeat as wayland_client::Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwpVirtualKeyboardManagerV1,
        _: <ZwpVirtualKeyboardManagerV1 as wayland_client::Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwpVirtualKeyboardV1,
        _: <ZwpVirtualKeyboardV1 as wayland_client::Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

fn unavailable(e: impl std::fmt::Display) -> Error {
    Error::Unavailable(format!("virtual keyboard: {e}"))
}

/// Whether the session compositor advertises the virtual keyboard manager.
/// Only reads the global list; no keyboard is created.
#[must_use]
pub fn available() -> bool {
    let Ok(conn) = Connection::connect_to_env() else {
        return false;
    };
    let Ok((globals, _queue)) = registry_queue_init::<State>(&conn) else {
        return false;
    };
    globals.contents().with_list(|list| {
        list.iter()
            .any(|global| global.interface == MANAGER_INTERFACE)
    })
}

/// Write `text` (NUL-terminated, as XKB expects) to an unlinked private
/// file and return it with its size.
fn keymap_file(text: &str) -> std::io::Result<(File, u32)> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let path = dir.join(format!("clipmonger-keymap-{}-{nanos}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    let _ = std::fs::remove_file(&path);
    file.write_all(text.as_bytes())?;
    file.write_all(&[0])?;
    file.flush()?;
    let size =
        u32::try_from(text.len() + 1).map_err(|_| std::io::Error::other("keymap too large"))?;
    Ok((file, size))
}

/// The seat's current XKB keymap (fd and size), read from a temporary
/// `wl_keyboard`; `None` when none arrives within [`KEYMAP_WAIT`].
fn seat_keymap(
    seat: &WlSeat,
    queue: &mut EventQueue<State>,
    state: &mut State,
) -> Option<(OwnedFd, u32)> {
    let keyboard = seat.get_keyboard(&queue.handle(), ());
    let deadline = Instant::now() + KEYMAP_WAIT;
    for _ in 0..KEYMAP_ROUNDTRIPS {
        if state.keymap.is_some() || Instant::now() >= deadline {
            break;
        }
        if queue.roundtrip(state).is_err() {
            break;
        }
    }
    if keyboard.version() >= 3 {
        keyboard.release();
    }
    state.keymap.take()
}

/// One virtual keyboard with a keymap uploaded.
pub struct VirtualKeyboard {
    queue: EventQueue<State>,
    keyboard: ZwpVirtualKeyboardV1,
    started: Instant,
}

impl VirtualKeyboard {
    /// Connect to the session compositor, create a virtual keyboard on the
    /// first seat and upload the seat's current keymap (else
    /// [`keymap_text`]).
    ///
    /// # Errors
    ///
    /// [`Error::Unavailable`] without a Wayland connection, without a seat,
    /// when the compositor does not offer `zwp_virtual_keyboard_manager_v1`,
    /// or when it rejects the keyboard (a protocol error ends the
    /// connection).
    pub fn connect() -> Result<Self> {
        let conn = Connection::connect_to_env().map_err(unavailable)?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(unavailable)?;
        let qh = queue.handle();
        let seat: WlSeat = globals.bind(&qh, 1..=5, ()).map_err(unavailable)?;
        let manager: ZwpVirtualKeyboardManagerV1 =
            globals.bind(&qh, 1..=1, ()).map_err(unavailable)?;
        let mut state = State::default();
        let seat_keymap = seat_keymap(&seat, &mut queue, &mut state);
        let keyboard = manager.create_virtual_keyboard(&seat, &qh, ());
        let format = u32::from(KeymapFormat::XkbV1);
        if let Some((fd, size)) = seat_keymap {
            keyboard.keymap(format, fd.as_fd(), size);
            queue.roundtrip(&mut state).map_err(unavailable)?;
        } else {
            let (file, size) = keymap_file(&keymap_text()).map_err(unavailable)?;
            keyboard.keymap(format, file.as_fd(), size);
            queue.roundtrip(&mut state).map_err(unavailable)?;
        }
        Ok(Self {
            queue,
            keyboard,
            started: Instant::now(),
        })
    }

    /// Press and release the paste chord (`Ctrl+V`, or `Ctrl+Shift+V`),
    /// sending key events and the matching modifier state.
    ///
    /// # Errors
    ///
    /// [`Error::Unavailable`] when the compositor connection fails.
    pub fn paste_chord(&mut self, shift: bool) -> Result<()> {
        // Warm-up: the first keys from a keyboard the seat has just switched
        // to reach GTK3 (native Wayland) before it has taken the switch in,
        // and it drops that chord. A lone Shift tap (no text, no shortcut)
        // makes the switch; the chord follows once the client has settled.
        for (key, state) in [(KEY_LEFTSHIFT, PRESSED), (KEY_LEFTSHIFT, RELEASED)] {
            let time = u32::try_from(self.started.elapsed().as_millis()).unwrap_or(u32::MAX);
            self.keyboard.key(time, key, state);
        }
        self.keyboard.modifiers(0, 0, 0, 0);
        self.queue
            .roundtrip(&mut State::default())
            .map_err(unavailable)?;
        std::thread::sleep(WARM_UP);
        for step in chord_steps(shift) {
            match step {
                Step::Key(key, state) => {
                    let time =
                        u32::try_from(self.started.elapsed().as_millis()).unwrap_or(u32::MAX);
                    self.keyboard.key(time, key, state);
                }
                Step::Modifiers(mask) => self.keyboard.modifiers(mask, 0, 0, 0),
            }
            self.queue.flush().map_err(unavailable)?;
            if matches!(step, Step::Key(..)) {
                std::thread::sleep(KEY_GAP);
            }
        }
        self.queue
            .roundtrip(&mut State::default())
            .map_err(unavailable)?;
        Ok(())
    }
}

impl Drop for VirtualKeyboard {
    fn drop(&mut self) {
        self.keyboard.destroy();
        let _ = self.queue.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keymap_names_the_chord_keys_by_xkb_keycode() {
        let text = keymap_text();
        assert!(text.contains("<LCTL> = 37;"));
        assert!(text.contains("<LFSH> = 50;"));
        assert!(text.contains("<AB04> = 55;"));
        assert!(text.contains("key <LCTL> { [ Control_L ] };"));
        assert!(text.contains("key <LFSH> { [ Shift_L ] };"));
        assert!(text.contains("key <AB04> { [ v, V ] };"));
        assert!(text.contains("modifier_map Control { <LCTL> };"));
        assert!(text.contains("modifier_map Shift { <LFSH> };"));
        assert!(text.contains("include \"complete\""));
        assert_eq!(
            text.matches('{').count(),
            text.matches('}').count(),
            "balanced braces"
        );
    }

    #[test]
    fn chord_reports_modifier_masks_around_the_keys() {
        use Step::{Key, Modifiers};
        assert_eq!(
            chord_steps(false),
            [
                Key(29, 1),
                Modifiers(4),
                Key(47, 1),
                Key(47, 0),
                Key(29, 0),
                Modifiers(0),
            ]
        );
        assert_eq!(
            chord_steps(true),
            [
                Key(29, 1),
                Modifiers(4),
                Key(42, 1),
                Modifiers(5),
                Key(47, 1),
                Key(47, 0),
                Key(42, 0),
                Modifiers(4),
                Key(29, 0),
                Modifiers(0),
            ]
        );
    }

    #[test]
    fn keymap_file_is_nul_terminated() {
        use std::io::{Read as _, Seek as _};
        let (mut file, size) = keymap_file("xkb_keymap {};").unwrap();
        assert_eq!(size, 15);
        file.rewind().unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"xkb_keymap {};\0");
    }
}
