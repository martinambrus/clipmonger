//! Global hotkey registration over the `global-hotkey` crate.
//!
//! Registration is parsed through the shared [`clipmonger_platform::chord`]
//! grammar; a conflicting chord fails loudly per id (with "already" in the
//! message) while the manager stays usable. Firing is observed through the
//! crate's channel in `poll` (WIN-F5: no extra pump).

#[cfg(windows)]
use std::collections::HashMap;
use std::time::Duration;
#[cfg(windows)]
use std::time::Instant;

#[cfg(windows)]
use clipmonger_platform::chord;
use clipmonger_platform::traits::HotkeyManager;
use clipmonger_platform::types::{Affinity, Error, HotkeyId, Result};

/// Global hotkey manager.
pub struct WinHotkeys {
    #[cfg(windows)]
    manager: Option<global_hotkey::GlobalHotKeyManager>,
    #[cfg(windows)]
    by_id: HashMap<HotkeyId, global_hotkey::hotkey::HotKey>,
    #[cfg(windows)]
    by_ghk: HashMap<u32, HotkeyId>,
}

impl WinHotkeys {
    /// Infallible constructor; a backend failure surfaces per call.
    #[must_use]
    pub fn new() -> Self {
        #[cfg(windows)]
        {
            let manager = global_hotkey::GlobalHotKeyManager::new().ok();
            Self {
                manager,
                by_id: HashMap::new(),
                by_ghk: HashMap::new(),
            }
        }
        #[cfg(not(windows))]
        {
            Self {}
        }
    }
}

impl Default for WinHotkeys {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(windows)]
fn unavailable() -> Error {
    Error::Unavailable("global hotkeys need a Windows session".to_string())
}

#[cfg(windows)]
fn conflict(chord_text: &str, detail: impl std::fmt::Display) -> Error {
    Error::Denied(format!(
        "hotkey chord already registered ({chord_text}): {detail}"
    ))
}

/// Drain this thread's message queue once. `WM_HOTKEY` is dispatched to
/// the `global-hotkey` hidden window only while its owning thread pumps.
#[cfg(windows)]
fn pump_thread_messages() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    };
    // Safety: `msg` is a valid out-param; the null HWND selects this
    // thread's queue, which includes its windows' messages.
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while PeekMessageW(&raw mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            let _ = TranslateMessage(&raw const msg);
            DispatchMessageW(&raw const msg);
        }
    }
}

/// Map canonical modifiers to `global-hotkey` modifiers.
#[cfg(windows)]
fn ghk_mods(mods: &[String]) -> Result<global_hotkey::hotkey::Modifiers> {
    use global_hotkey::hotkey::Modifiers;
    let mut out = Modifiers::empty();
    for m in mods {
        match m.as_str() {
            "ctrl" => out |= Modifiers::CONTROL,
            "alt" => out |= Modifiers::ALT,
            "shift" => out |= Modifiers::SHIFT,
            "super" => out |= Modifiers::META,
            other => return Err(Error::Unsupported(format!("unknown modifier '{other}'"))),
        }
    }
    Ok(out)
}

/// Map a canonical letter key to its code.
#[cfg(windows)]
fn letter_code(c: char, key: &str) -> Result<global_hotkey::hotkey::Code> {
    use global_hotkey::hotkey::Code;
    match c {
        'a' => Ok(Code::KeyA),
        'b' => Ok(Code::KeyB),
        'c' => Ok(Code::KeyC),
        'd' => Ok(Code::KeyD),
        'e' => Ok(Code::KeyE),
        'f' => Ok(Code::KeyF),
        'g' => Ok(Code::KeyG),
        'h' => Ok(Code::KeyH),
        'i' => Ok(Code::KeyI),
        'j' => Ok(Code::KeyJ),
        'k' => Ok(Code::KeyK),
        'l' => Ok(Code::KeyL),
        'm' => Ok(Code::KeyM),
        'n' => Ok(Code::KeyN),
        'o' => Ok(Code::KeyO),
        'p' => Ok(Code::KeyP),
        'q' => Ok(Code::KeyQ),
        'r' => Ok(Code::KeyR),
        's' => Ok(Code::KeyS),
        't' => Ok(Code::KeyT),
        'u' => Ok(Code::KeyU),
        'v' => Ok(Code::KeyV),
        'w' => Ok(Code::KeyW),
        'x' => Ok(Code::KeyX),
        'y' => Ok(Code::KeyY),
        'z' => Ok(Code::KeyZ),
        _ => Err(Error::Unsupported(format!("bad key '{key}'"))),
    }
}

/// Map a canonical key name to its code.
#[cfg(windows)]
fn key_code(key: &str) -> Result<global_hotkey::hotkey::Code> {
    use global_hotkey::hotkey::Code;
    if key.len() == 1 {
        let c = key.chars().next().unwrap_or('?');
        if c.is_ascii_lowercase() {
            return letter_code(c, key);
        }
        return Err(Error::Unsupported(format!("bad key '{key}'")));
    }
    if let Some(digit) = key.strip_prefix("Digit") {
        return match digit {
            "0" => Ok(Code::Digit0),
            "1" => Ok(Code::Digit1),
            "2" => Ok(Code::Digit2),
            "3" => Ok(Code::Digit3),
            "4" => Ok(Code::Digit4),
            "5" => Ok(Code::Digit5),
            "6" => Ok(Code::Digit6),
            "7" => Ok(Code::Digit7),
            "8" => Ok(Code::Digit8),
            "9" => Ok(Code::Digit9),
            _ => Err(Error::Unsupported(format!("bad key '{key}'"))),
        };
    }
    match key {
        "F1" => Ok(Code::F1),
        "F2" => Ok(Code::F2),
        "F3" => Ok(Code::F3),
        "F4" => Ok(Code::F4),
        "F5" => Ok(Code::F5),
        "F6" => Ok(Code::F6),
        "F7" => Ok(Code::F7),
        "F8" => Ok(Code::F8),
        "F9" => Ok(Code::F9),
        "F10" => Ok(Code::F10),
        "F11" => Ok(Code::F11),
        "F12" => Ok(Code::F12),
        "Space" => Ok(Code::Space),
        "Tab" => Ok(Code::Tab),
        "Enter" => Ok(Code::Enter),
        "Escape" => Ok(Code::Escape),
        "Backspace" => Ok(Code::Backspace),
        "Delete" => Ok(Code::Delete),
        "Insert" => Ok(Code::Insert),
        "Home" => Ok(Code::Home),
        "End" => Ok(Code::End),
        "PageUp" => Ok(Code::PageUp),
        "PageDown" => Ok(Code::PageDown),
        "ArrowUp" => Ok(Code::ArrowUp),
        "ArrowDown" => Ok(Code::ArrowDown),
        "ArrowLeft" => Ok(Code::ArrowLeft),
        "ArrowRight" => Ok(Code::ArrowRight),
        "Comma" => Ok(Code::Comma),
        "Period" => Ok(Code::Period),
        "Slash" => Ok(Code::Slash),
        "Semicolon" => Ok(Code::Semicolon),
        "Quote" => Ok(Code::Quote),
        "Backquote" => Ok(Code::Backquote),
        "Backslash" => Ok(Code::Backslash),
        "Minus" => Ok(Code::Minus),
        "Equal" => Ok(Code::Equal),
        "BracketLeft" => Ok(Code::BracketLeft),
        "BracketRight" => Ok(Code::BracketRight),
        _ => Err(Error::Unsupported(format!("unsupported key '{key}'"))),
    }
}

/// Map a canonical chord to `global-hotkey` modifiers + code.
#[cfg(windows)]
fn to_ghk(
    chord: &chord::Chord,
) -> Result<(
    global_hotkey::hotkey::Modifiers,
    global_hotkey::hotkey::Code,
)> {
    Ok((ghk_mods(&chord.mods)?, key_code(&chord.key)?))
}

impl HotkeyManager for WinHotkeys {
    const AFFINITY: Affinity = Affinity::Worker;

    fn register(&mut self, id: HotkeyId, chord_text: &str) -> Result<()> {
        #[cfg(windows)]
        {
            let parsed = chord::parse(chord_text)?;
            let (mods, code) = to_ghk(&parsed)?;
            let manager = self.manager.as_ref().ok_or_else(unavailable)?;
            let hotkey = global_hotkey::hotkey::HotKey::new(Some(mods), code);
            let old = self.by_id.get(&id).copied();
            if old.is_some_and(|old| old.id() == hotkey.id()) {
                return Ok(());
            }
            // The new chord goes in before the old one is dropped, so a
            // refused change keeps the working chord.
            match manager.register(hotkey) {
                Ok(()) => {
                    if let Some(old) = old {
                        self.by_ghk.remove(&old.id());
                        let _ = manager.unregister(old);
                    }
                    self.by_ghk.insert(hotkey.id(), id);
                    self.by_id.insert(id, hotkey);
                    Ok(())
                }
                Err(e) => {
                    let detail = e.to_string();
                    if matches!(e, global_hotkey::Error::AlreadyRegistered(_))
                        || detail.to_ascii_lowercase().contains("already")
                        || detail.contains("1409")
                    {
                        Err(conflict(chord_text, detail))
                    } else {
                        Err(Error::Io(format!(
                            "hotkey register failed ({chord_text}): {detail}"
                        )))
                    }
                }
            }
        }
        #[cfg(not(windows))]
        {
            let _ = (id, chord_text);
            Err(Error::Unavailable(
                "global hotkeys need a Windows session".to_string(),
            ))
        }
    }

    fn unregister(&mut self, id: HotkeyId) -> Result<()> {
        #[cfg(windows)]
        {
            let Some(hotkey) = self.by_id.remove(&id) else {
                return Ok(());
            };
            self.by_ghk.remove(&hotkey.id());
            if let Some(manager) = self.manager.as_ref() {
                manager
                    .unregister(hotkey)
                    .map_err(|e| Error::Io(format!("hotkey unregister failed: {e}")))?;
            }
            Ok(())
        }
        #[cfg(not(windows))]
        {
            let _ = id;
            Err(Error::Unavailable(
                "global hotkeys need a Windows session".to_string(),
            ))
        }
    }

    fn poll(&mut self, timeout: Duration) -> Result<Vec<HotkeyId>> {
        #[cfg(windows)]
        {
            self.manager.as_ref().ok_or_else(unavailable)?;
            let deadline = Instant::now() + timeout;
            let mut fired = Vec::new();
            while Instant::now() < deadline {
                // WM_HOTKEY reaches the hidden window only when this thread
                // pumps messages (the manager's HWND lives on the thread
                // that constructed it); without the pump no fire arrives.
                pump_thread_messages();
                if let Ok(event) = global_hotkey::GlobalHotKeyEvent::receiver().try_recv() {
                    if event.state == global_hotkey::HotKeyState::Pressed {
                        if let Some(id) = self.by_ghk.get(&event.id) {
                            if !fired.contains(id) {
                                fired.push(*id);
                            }
                        }
                    }
                } else {
                    if !fired.is_empty() {
                        // Report the press now, not at the deadline.
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            Ok(fired)
        }
        #[cfg(not(windows))]
        {
            let _ = timeout;
            Err(Error::Unavailable(
                "global hotkeys need a Windows session".to_string(),
            ))
        }
    }
}
