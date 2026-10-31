//! Global hotkeys over direct `grab_key` with dynamic lock-modifier
//! expansion (X-F6): every chord is grabbed under all combinations of the
//! lock masks discovered from the server modifier map, so chords fire
//! regardless of NumLock/CapsLock/ScrollLock state.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use clipmonger_platform::chord;
use clipmonger_platform::traits::HotkeyManager;
use clipmonger_platform::types::{Affinity, Error, HotkeyId, Result};
use x11rb14::connection::Connection as _;
use x11rb14::protocol::xproto::{self, ConnectionExt as _, GrabMode, ModMask, Window};
use x11rb14::rust_connection::RustConnection;

use crate::lockmods::{expand_lock_combos, locks_from_map};
use crate::session;

/// Keysyms used for lock discovery and keycode lookup.
const NUM_LOCK: u32 = 0xff7f;
const CAPS_LOCK: u32 = 0xffe5;
const SCROLL_LOCK: u32 = 0xff14;

/// One registration: keycode plus every grabbed modifier combination.
#[derive(Debug)]
struct Grab {
    keycode: u8,
    combos: Vec<u32>,
    /// Canonical modifier names (verified against the live keymap on the
    /// raw-event fallback path).
    mods: Vec<String>,
}

/// X11 hotkey manager.
pub struct X11Hotkeys {
    conn: Option<RustConnection>,
    root: Window,
    locks: Vec<u32>,
    by_id: HashMap<HotkeyId, Grab>,
    /// XI2 raw key presses bypass XKB server actions (e.g. Ctrl+Alt+Fn
    /// `SwitchScreen`, which the server consumes before grabs); they are
    /// only a fallback — a recent grab fire suppresses the raw echo.
    raw_ok: bool,
    last_grab: HashMap<u8, Instant>,
    last_raw: Option<(u8, u32)>,
    last_raw_at: HashMap<u8, Instant>,
    /// Keycodes down per the raw stream (reconstructed press/release
    /// order; the live keymap is already past back-to-back synthesis).
    raw_down: HashSet<u8>,
}

impl X11Hotkeys {
    /// Connect to `display` (or `$DISPLAY`) and discover lock masks.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unavailable`] without an X session.
    pub fn connect(display: Option<&str>) -> Result<Self> {
        #[cfg(not(unix))]
        {
            let _ = display;
            return Err(Error::Unavailable(
                "x11 hotkeys need a Unix X11 session".to_string(),
            ));
        }
        #[cfg(unix)]
        {
            let (conn, screen) = session::connect(display)?;
            let root = conn.setup().roots[screen].root;
            let locks = discover_locks(&conn);
            let raw_ok = enable_raw_fallback(&conn, root);
            conn.flush()
                .map_err(|e| Error::Io(format!("x11 flush failed: {e}")))?;
            Ok(Self {
                conn: Some(conn),
                root,
                locks,
                by_id: HashMap::new(),
                raw_ok,
                last_grab: HashMap::new(),
                last_raw: None,
                last_raw_at: HashMap::new(),
                raw_down: HashSet::new(),
            })
        }
    }

    /// Raw-press fallback: fire when a registered keycode arrives via XI2
    /// (the grab path never sees chords the server consumes) with every
    /// chord modifier down. Modifiers come from the reconstructed raw
    /// stream first (the live keymap is already past back-to-back
    /// synthesis) with the live keymap as backup. Multi-device echoes
    /// share one timestamp, and a recent grab or raw fire suppresses.
    fn on_raw_press(&mut self, keycode: u8, time: u32, fired: &mut Vec<HotkeyId>) {
        if self.last_raw == Some((keycode, time)) {
            return;
        }
        if self
            .last_raw_at
            .get(&keycode)
            .is_some_and(|at| at.elapsed() < Duration::from_millis(300))
        {
            return;
        }
        let mut matches: Vec<HotkeyId> = self
            .by_id
            .iter()
            .filter(|(_, grab)| grab.keycode == keycode)
            .map(|(id, _)| *id)
            .collect();
        if matches.is_empty() {
            return;
        }
        if self
            .last_grab
            .get(&keycode)
            .is_some_and(|at| at.elapsed() < Duration::from_millis(300))
        {
            return;
        }
        let keymap = self.live_keymap();
        matches.retain(|id| {
            self.by_id.get(id).is_some_and(|grab| {
                grab.mods.iter().all(|name| {
                    mod_keysyms(name)
                        .iter()
                        .filter_map(|keysym| self.keycode_for(*keysym).ok())
                        .any(|code| self.raw_down.contains(&code) || key_is_down(keymap, code))
                })
            })
        });
        if !matches.is_empty() {
            self.last_raw = Some((keycode, time));
            self.last_raw_at.insert(keycode, Instant::now());
            for id in matches {
                if !fired.contains(&id) {
                    fired.push(id);
                }
            }
        }
    }

    /// Current 32-byte keymap (`None` when the server will not answer).
    fn live_keymap(&self) -> Option<[u8; 32]> {
        self.conn
            .as_ref()?
            .query_keymap()
            .ok()?
            .reply()
            .ok()
            .map(|reply| reply.keys)
    }

    /// Look up the keycode currently mapped to `keysym`.
    fn keycode_for(&self, keysym: u32) -> Result<u8> {
        let conn = self.conn.as_ref().ok_or_else(closed)?;
        let setup = conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let reply = conn
            .get_keyboard_mapping(min, max - min + 1)
            .map_err(|e| Error::Io(format!("x11 keymap query failed: {e}")))?
            .reply()
            .map_err(|e| Error::Io(format!("x11 keymap reply failed: {e}")))?;
        let per = reply.keysyms_per_keycode as usize;
        for (index, chunk) in reply.keysyms.chunks(per).enumerate() {
            if chunk.first() == Some(&keysym) {
                let offset =
                    u8::try_from(index).map_err(|_| Error::Io("keymap overflow".to_string()))?;
                return min
                    .checked_add(offset)
                    .ok_or_else(|| Error::Io("keymap overflow".to_string()));
            }
        }
        Err(Error::Unsupported(format!(
            "no keycode mapped to keysym {keysym:#x}"
        )))
    }
}

/// Whether `keycode` is down in a `query_keymap` bitmap.
fn key_is_down(keymap: Option<[u8; 32]>, keycode: u8) -> bool {
    keymap.is_some_and(|keys| {
        let index = usize::from(keycode) / 8;
        let bit = usize::from(keycode) % 8;
        keys.get(index).is_some_and(|byte| byte & (1 << bit) != 0)
    })
}

fn closed() -> Error {
    Error::Unavailable("x11 hotkeys are closed".to_string())
}

/// Select XI2 raw key presses on `root` (best-effort fallback for chords
/// the server consumes via XKB actions before grabs).
fn enable_raw_fallback(conn: &RustConnection, root: Window) -> bool {
    use x11rb14::protocol::xinput as xi;
    use x11rb14::protocol::xinput::ConnectionExt as _;
    if xi::xi_query_version(conn, 2, 0)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .is_none()
    {
        return false;
    }
    let mask = xi::XIEventMask::from((1u32 << 13) | (1u32 << 14));
    conn.xinput_xi_select_events(
        root,
        &[xi::EventMask {
            deviceid: 0,
            mask: vec![mask],
        }],
    )
    .ok()
    .and_then(|cookie| cookie.check().ok())
    .is_some()
}

/// Keysyms covering one canonical modifier (left/right variants).
fn mod_keysyms(name: &str) -> &'static [u32] {
    match name {
        "shift" => &[0xffe1, 0xffe2],
        "ctrl" => &[0xffe3, 0xffe4],
        "alt" => &[0xffe9, 0xffe8],
        "super" => &[0xffeb, 0xffec],
        _ => &[],
    }
}

impl HotkeyManager for X11Hotkeys {
    const AFFINITY: Affinity = Affinity::Worker;

    fn register(&mut self, id: HotkeyId, chord_text: &str) -> Result<()> {
        if self.conn.is_none() {
            return Err(closed());
        }
        let parsed = chord::parse(chord_text)?;
        let base = base_mask(&parsed.mods)?;
        let keycode = self.keycode_for(keysym_for_key(&parsed.key)?)?;
        let combos = expand_lock_combos(base, &self.locks);
        // Replace any previous registration under this id first.
        let _ = self.unregister(id);
        let conn = self.conn.as_ref().ok_or_else(closed)?;
        let mut grabbed = Vec::new();
        for combo in &combos {
            let mask = ModMask::from((combo & 0xffff) as u16);
            let grab = xproto::grab_key(
                conn,
                true,
                self.root,
                mask,
                keycode,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            )
            .map_err(|e| Error::Io(format!("hotkey grab failed ({chord_text}): {e}")))?;
            match grab.check() {
                Ok(()) => grabbed.push(*combo),
                Err(e) => {
                    for done in grabbed {
                        let _ = xproto::ungrab_key(
                            conn,
                            keycode,
                            self.root,
                            ModMask::from((done & 0xffff) as u16),
                        )
                        .ok()
                        .and_then(|cookie| cookie.check().ok());
                    }
                    let _ = conn.flush();
                    let detail = e.to_string();
                    // A failed grab inside a live session is a conflict
                    // (`BadAccess`: another client holds the chord).
                    if matches!(e, x11rb14::errors::ReplyError::X11Error(_)) {
                        return Err(Error::Denied(format!(
                            "hotkey chord already registered ({chord_text}): {detail}"
                        )));
                    }
                    return Err(Error::Io(format!(
                        "hotkey grab failed ({chord_text}): {detail}"
                    )));
                }
            }
        }
        let _ = conn.flush();
        self.by_id.insert(
            id,
            Grab {
                keycode,
                combos,
                mods: parsed.mods.clone(),
            },
        );
        Ok(())
    }

    fn unregister(&mut self, id: HotkeyId) -> Result<()> {
        let Some(grab) = self.by_id.remove(&id) else {
            return Ok(());
        };
        if let Some(conn) = self.conn.as_ref() {
            for combo in grab.combos {
                let _ = xproto::ungrab_key(
                    conn,
                    grab.keycode,
                    self.root,
                    ModMask::from((combo & 0xffff) as u16),
                )
                .ok()
                .and_then(|cookie| cookie.check().ok());
            }
            let _ = conn.flush();
        }
        Ok(())
    }

    fn poll(&mut self, timeout: Duration) -> Result<Vec<HotkeyId>> {
        let deadline = Instant::now() + timeout;
        let mut fired = Vec::new();
        while Instant::now() < deadline {
            let event = {
                let conn = self.conn.as_ref().ok_or_else(closed)?;
                match conn.poll_for_event() {
                    Ok(event) => event,
                    Err(e) => return Err(Error::Io(format!("hotkey poll failed: {e}"))),
                }
            };
            match event {
                Some(x11rb14::protocol::Event::KeyPress(press)) => {
                    self.last_grab.insert(press.detail, Instant::now());
                    // The XI2 raw path may have reported this press already.
                    let raw_fired = self
                        .last_raw_at
                        .get(&press.detail)
                        .is_some_and(|at| at.elapsed() < Duration::from_millis(300));
                    for (id, grab) in &self.by_id {
                        if grab.keycode == press.detail && !raw_fired && !fired.contains(id) {
                            fired.push(*id);
                        }
                    }
                }
                Some(x11rb14::protocol::Event::XinputRawKeyPress(raw)) => {
                    if self.raw_ok {
                        if let Ok(keycode) = u8::try_from(raw.detail) {
                            self.raw_down.insert(keycode);
                            self.on_raw_press(keycode, raw.time, &mut fired);
                        }
                    }
                }
                Some(x11rb14::protocol::Event::XinputRawKeyRelease(raw)) => {
                    if self.raw_ok {
                        if let Ok(keycode) = u8::try_from(raw.detail) {
                            self.raw_down.remove(&keycode);
                        }
                    }
                }
                Some(_) => {}
                None => std::thread::sleep(Duration::from_millis(5)),
            }
            if !fired.is_empty() {
                // Report the press now, not at the deadline.
                break;
            }
        }
        Ok(fired)
    }
}

/// Canonical modifier names to the X modifier mask.
fn base_mask(mods: &[String]) -> Result<u32> {
    let mut out = 0u32;
    for name in mods {
        match name.as_str() {
            "shift" => out |= 0x01,
            "ctrl" => out |= 0x04,
            "alt" => out |= 0x08,
            "super" => out |= 0x40,
            other => return Err(Error::Unsupported(format!("unknown modifier '{other}'"))),
        }
    }
    Ok(out)
}

/// Canonical key name to keysym.
fn keysym_for_key(key: &str) -> Result<u32> {
    if key.len() == 1 {
        let c = key.chars().next().unwrap_or('?');
        if c.is_ascii_lowercase() {
            return Ok(u32::from(c));
        }
        return Err(Error::Unsupported(format!("bad key '{key}'")));
    }
    if let Some(digit) = key.strip_prefix("Digit") {
        if digit.len() == 1 {
            if let Some(d) = digit.chars().next().filter(char::is_ascii_digit) {
                return Ok(u32::from(d));
            }
        }
        return Err(Error::Unsupported(format!("bad key '{key}'")));
    }
    if let Some(n) = key.strip_prefix('F') {
        if let Ok(num) = n.parse::<u32>() {
            if (1..=12).contains(&num) {
                return Ok(0xffbe + num - 1);
            }
        }
        return Err(Error::Unsupported(format!("bad key '{key}'")));
    }
    match key {
        "Space" => Ok(0x20),
        "Tab" => Ok(0xff09),
        "Enter" => Ok(0xff0d),
        "Escape" => Ok(0xff1b),
        "Backspace" => Ok(0xff08),
        "Delete" => Ok(0xffff),
        "Insert" => Ok(0xff63),
        "Home" => Ok(0xff50),
        "End" => Ok(0xff57),
        "PageUp" => Ok(0xff55),
        "PageDown" => Ok(0xff56),
        "ArrowUp" => Ok(0xff52),
        "ArrowDown" => Ok(0xff54),
        "ArrowLeft" => Ok(0xff51),
        "ArrowRight" => Ok(0xff53),
        "Comma" => Ok(u32::from(b',')),
        "Period" => Ok(u32::from(b'.')),
        "Slash" => Ok(u32::from(b'/')),
        "Semicolon" => Ok(u32::from(b';')),
        "Quote" => Ok(u32::from(b'\'')),
        "Backquote" => Ok(u32::from(b'`')),
        "Backslash" => Ok(u32::from(b'\\')),
        "Minus" => Ok(u32::from(b'-')),
        "Equal" => Ok(u32::from(b'=')),
        "BracketLeft" => Ok(u32::from(b'[')),
        "BracketRight" => Ok(u32::from(b']')),
        _ => Err(Error::Unsupported(format!("unsupported key '{key}'"))),
    }
}

/// Discover lock masks from the server modifier map (never hardcoded):
/// the row holding NumLock/CapsLock/ScrollLock contributes its mask.
fn discover_locks(conn: &RustConnection) -> Vec<u32> {
    let mapping = conn
        .get_modifier_mapping()
        .ok()
        .and_then(|cookie| cookie.reply().ok());
    let keymap = conn.setup();
    let (min, max) = (keymap.min_keycode, keymap.max_keycode);
    let keysyms = conn
        .get_keyboard_mapping(min, max - min + 1)
        .ok()
        .and_then(|cookie| cookie.reply().ok());
    let (Some(mapping), Some(keysyms)) = (mapping, keysyms) else {
        return vec![0x02];
    };
    let per = mapping.keycodes_per_modifier() as usize;
    let sym_per = keysyms.keysyms_per_keycode as usize;
    let has_keysym = |code: u8, want: u32| -> bool {
        let Some(index) = code.checked_sub(min).map(usize::from) else {
            return false;
        };
        keysyms
            .keysyms
            .chunks(sym_per)
            .nth(index)
            .is_some_and(|chunk| chunk.contains(&want))
    };
    let mut num = None;
    let mut caps = 0x02u32;
    let mut scroll = None;
    for (row, chunk) in mapping.keycodes.chunks(per).enumerate() {
        let mask = 1u32 << row;
        for code in chunk {
            if *code == 0 {
                continue;
            }
            if has_keysym(*code, NUM_LOCK) {
                num = Some(mask);
            }
            if has_keysym(*code, CAPS_LOCK) {
                caps = mask;
            }
            if has_keysym(*code, SCROLL_LOCK) {
                scroll = Some(mask);
            }
        }
    }
    locks_from_map(num, caps, scroll)
}
