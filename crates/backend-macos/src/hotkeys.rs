//! Global hotkeys over `global-hotkey` (Carbon `RegisterEventHotKey`).
//!
//! The manager must be created and used on the main thread, whose run loop
//! [`crate::main_loop::run`] keeps in `NSApplication.run`; it lives in a
//! main-thread `thread_local` and every register/unregister is marshalled
//! there with [`crate::main_loop::on_main`]. Firing arrives on the crate's
//! process-wide channel, read by `poll` on any thread.

#[cfg(target_os = "macos")]
use std::collections::HashMap;
use std::time::Duration;

use clipmonger_platform::chord::{self, Chord};
use clipmonger_platform::traits::HotkeyManager;
use clipmonger_platform::types::{Affinity, Error, HotkeyId, Result};

/// W3C `KeyboardEvent.code` name of a canonical chord key (`a` -> `KeyA`).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn w3c_code(key: &str) -> String {
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_lowercase() => format!("Key{}", c.to_ascii_uppercase()),
        _ => key.to_string(),
    }
}

/// Check a chord maps (modifiers known, key has a W3C code name).
fn check_chord(parsed: &Chord) -> Result<()> {
    for m in &parsed.mods {
        crate::keys::modifier(m)?;
    }
    crate::keys::key_code(&parsed.key).map(drop)
}

#[cfg(target_os = "macos")]
mod imp {
    use std::cell::RefCell;
    use std::str::FromStr as _;

    use global_hotkey::hotkey::{Code, HotKey, Modifiers};
    use global_hotkey::GlobalHotKeyManager;

    use clipmonger_platform::chord::Chord;
    use clipmonger_platform::types::{Error, Result};

    use crate::main_loop;

    thread_local! {
        static MANAGER: RefCell<Option<GlobalHotKeyManager>> = const { RefCell::new(None) };
    }

    /// Run `f` with the main-thread manager, creating it on first use.
    pub(super) fn with_manager<R: Send>(
        f: impl FnOnce(&GlobalHotKeyManager) -> Result<R> + Send,
    ) -> Result<R> {
        if !main_loop::is_main_thread() && !main_loop::is_running() {
            return Err(Error::Unavailable(
                "global hotkeys need the main-thread loop (main_loop::run)".to_string(),
            ));
        }
        main_loop::on_main(|| {
            MANAGER.with(|cell| {
                let mut slot = cell.borrow_mut();
                if slot.is_none() {
                    let manager = GlobalHotKeyManager::new()
                        .map_err(|e| Error::Unavailable(format!("hotkey manager failed: {e}")))?;
                    *slot = Some(manager);
                }
                match slot.as_ref() {
                    Some(manager) => f(manager),
                    None => Err(Error::Internal("hotkey manager missing".to_string())),
                }
            })
        })
    }

    pub(super) fn to_hotkey(parsed: &Chord) -> Result<HotKey> {
        let mut mods = Modifiers::empty();
        for m in &parsed.mods {
            mods |= match m.as_str() {
                "ctrl" => Modifiers::CONTROL,
                "alt" => Modifiers::ALT,
                "shift" => Modifiers::SHIFT,
                "super" => Modifiers::META,
                other => return Err(Error::Unsupported(format!("unknown modifier '{other}'"))),
            };
        }
        let code = Code::from_str(&super::w3c_code(&parsed.key))
            .map_err(|_| Error::Unsupported(format!("unsupported key '{}'", parsed.key)))?;
        Ok(HotKey::new(Some(mods), code))
    }
}

#[cfg(target_os = "macos")]
fn conflict(chord_text: &str, detail: impl std::fmt::Display) -> Error {
    Error::Denied(format!(
        "hotkey chord already registered ({chord_text}): {detail}"
    ))
}

/// Global hotkey manager.
pub struct MacHotkeys {
    #[cfg(target_os = "macos")]
    by_id: HashMap<HotkeyId, global_hotkey::hotkey::HotKey>,
    #[cfg(target_os = "macos")]
    by_ghk: HashMap<u32, HotkeyId>,
}

impl MacHotkeys {
    /// Create the main-thread manager.
    ///
    /// # Errors
    ///
    /// [`Error::Unavailable`] off macOS, or when called off the main thread
    /// before [`crate::main_loop::run`] serves it.
    pub fn new() -> Result<Self> {
        #[cfg(target_os = "macos")]
        {
            imp::with_manager(|_| Ok(()))?;
            Ok(Self {
                by_id: HashMap::new(),
                by_ghk: HashMap::new(),
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(unavailable())
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn unavailable() -> Error {
    Error::Unavailable("global hotkeys need a macOS session".to_string())
}

#[cfg(target_os = "macos")]
impl Drop for MacHotkeys {
    fn drop(&mut self) {
        let keys: Vec<_> = self.by_id.values().copied().collect();
        if keys.is_empty()
            || !(crate::main_loop::is_running() || crate::main_loop::is_main_thread())
        {
            return;
        }
        let _ = imp::with_manager(move |manager| {
            let _ = manager.unregister_all(&keys);
            Ok(())
        });
    }
}

impl HotkeyManager for MacHotkeys {
    const AFFINITY: Affinity = Affinity::Worker;

    fn register(&mut self, id: HotkeyId, chord_text: &str) -> Result<()> {
        let parsed = chord::parse(chord_text)?;
        check_chord(&parsed)?;
        #[cfg(target_os = "macos")]
        {
            let hotkey = imp::to_hotkey(&parsed)?;
            let old = self.by_id.get(&id).copied();
            if old.is_some_and(|old| old.id() == hotkey.id()) {
                return Ok(());
            }
            let text = chord_text.to_string();
            // The new chord goes in before the old one is dropped, so a
            // refused change keeps the working chord.
            imp::with_manager(move |manager| match manager.register(hotkey) {
                Ok(()) => {
                    if let Some(old) = old {
                        let _ = manager.unregister(old);
                    }
                    Ok(())
                }
                Err(global_hotkey::Error::AlreadyRegistered(_)) => {
                    Err(conflict(&text, "in use by this process"))
                }
                Err(e) => {
                    let detail = e.to_string();
                    // Carbon eventHotKeyExistsErr (-9878): another app owns it.
                    if detail.to_ascii_lowercase().contains("already") || detail.contains("-9878") {
                        Err(conflict(&text, detail))
                    } else {
                        Err(Error::Io(format!(
                            "hotkey register failed ({text}): {detail}"
                        )))
                    }
                }
            })?;
            if let Some(old) = old {
                self.by_ghk.remove(&old.id());
            }
            self.by_ghk.insert(hotkey.id(), id);
            self.by_id.insert(id, hotkey);
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = id;
            Err(unavailable())
        }
    }

    fn unregister(&mut self, id: HotkeyId) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            let Some(hotkey) = self.by_id.remove(&id) else {
                return Ok(());
            };
            self.by_ghk.remove(&hotkey.id());
            imp::with_manager(move |manager| {
                manager
                    .unregister(hotkey)
                    .map_err(|e| Error::Io(format!("hotkey unregister failed: {e}")))
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = id;
            Err(unavailable())
        }
    }

    fn poll(&mut self, timeout: Duration) -> Result<Vec<HotkeyId>> {
        #[cfg(target_os = "macos")]
        {
            use global_hotkey::{GlobalHotKeyEvent, HotKeyState};
            let receiver = GlobalHotKeyEvent::receiver();
            let deadline = std::time::Instant::now() + timeout;
            let mut fired = Vec::new();
            loop {
                let event = if fired.is_empty() {
                    let left = deadline.saturating_duration_since(std::time::Instant::now());
                    receiver.recv_timeout(left).ok()
                } else {
                    // Report the press now, not at the deadline.
                    receiver.try_recv().ok()
                };
                let Some(event) = event else {
                    break;
                };
                if event.state == HotKeyState::Pressed {
                    if let Some(id) = self.by_ghk.get(&event.id) {
                        if !fired.contains(id) {
                            fired.push(*id);
                        }
                    }
                }
            }
            Ok(fired)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = timeout;
            Err(unavailable())
        }
    }

    fn discard_pending(&mut self) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            while global_hotkey::GlobalHotKeyEvent::receiver()
                .try_recv()
                .is_ok()
            {}
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(unavailable())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_keys_map_to_w3c_codes() {
        assert_eq!(w3c_code("h"), "KeyH");
        assert_eq!(w3c_code("Digit1"), "Digit1");
        assert_eq!(w3c_code("F12"), "F12");
        assert_eq!(w3c_code("ArrowUp"), "ArrowUp");
        assert_eq!(w3c_code("BracketLeft"), "BracketLeft");
    }

    #[test]
    fn chord_check_rejects_unmapped_keys() {
        assert!(check_chord(&chord::parse("ctrl+alt+h").unwrap()).is_ok());
        assert!(check_chord(&chord::parse("super+F21").unwrap()).is_err());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn unavailable_off_macos() {
        assert!(matches!(MacHotkeys::new(), Err(Error::Unavailable(_))));
    }
}
