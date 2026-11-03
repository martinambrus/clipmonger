//! Clipboard-restore paste: save the current clipboard, write the clip,
//! send Ctrl+V over synthetic input, then restore the saved value only
//! when it is still ours (guarded restore — user copies always win).

use std::time::{Duration, Instant};

use clipmonger_platform::paste_policy::{check_target, guarded_restore_decision};
use clipmonger_platform::traits::{ClipboardTransfer as _, PasteEngine};
use clipmonger_platform::types::{
    Affinity, ClipRef, Error, PasteStrategy, PasteTarget, PlatformHandle, Result,
};

use crate::foreground::X11Foreground;
use crate::session;
use crate::transfer::{window_exists, X11Transfer};
use x11rb14::connection::Connection as _;
use x11rb14::protocol::xproto::{self, ConnectionExt as _, EventMask};

/// Dwell holding the clip on the clipboard before the guarded restore.
const DWELL_MS: u64 = 1200;
/// Canonical text MIME preferred for save/restore comparisons.
const TEXT_MIME: &str = "text/plain;charset=utf-8";

/// Hook invoked around each clipboard write in a paste operation.
pub type ClipboardWriteHook<'a> =
    dyn FnMut(&[(String, Vec<u8>)], &mut dyn FnMut() -> Result<()>) -> Result<()> + 'a;

/// Wait for physically held modifiers to be released without synthesizing key releases.
///
/// # Errors
/// Returns an error if the X server cannot be queried or the deadline expires.
pub fn wait_modifiers_released(display: Option<&str>, timeout: Duration) -> Result<()> {
    let (conn, _) = session::connect(display)?;
    // Use the server's modifier map rather than assumed keysyms: this also
    // covers layouts with AltGr or remapped modifier keys.
    let reply = conn
        .get_modifier_mapping()
        .map_err(|e| Error::Io(format!("modifier keymap: {e}")))?
        .reply()
        .map_err(|e| Error::Io(format!("modifier keymap: {e}")))?;
    let codes: Vec<u8> = reply
        .keycodes
        .into_iter()
        .filter(|code| *code != 0)
        .collect();
    if codes.is_empty() {
        return Err(Error::Unavailable("modifier keymap unavailable".into()));
    }
    wait_modifier_state(timeout, || {
        let keys = conn
            .query_keymap()
            .map_err(|e| Error::Io(format!("modifier state: {e}")))?
            .reply()
            .map_err(|e| Error::Io(format!("modifier state: {e}")))?
            .keys;
        Ok(codes
            .iter()
            .all(|code| keys[usize::from(*code / 8)] & (1 << (*code % 8)) == 0))
    })
}

fn wait_modifier_state(
    timeout: Duration,
    mut released: impl FnMut() -> Result<bool>,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if released()? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::Denied("modifiers remained held".into()));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// X11 paste engine (`ClipboardRestore` only).
pub struct X11Paste {
    transfer: X11Transfer,
    display: Option<String>,
}

impl X11Paste {
    /// Report health of the retained clipboard-serving connection.
    ///
    /// # Errors
    /// Returns an error when health cannot be determined.
    pub fn serving_health(&self) -> Result<bool> {
        self.transfer.serving_health()
    }

    /// Stop and join the serving worker only after its connection is known dead.
    ///
    /// # Errors
    /// Refuses to discard a healthy or unknown clipboard owner.
    pub fn close_dead_serving(&mut self) -> Result<()> {
        self.transfer.close_dead_serving()
    }

    /// Connect to `display` (or `$DISPLAY`).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unavailable`] without an X session.
    pub fn connect(display: Option<&str>) -> Result<Self> {
        #[cfg(not(unix))]
        {
            let _ = display;
            return Err(Error::Unavailable(
                "x11 paste needs a Unix X11 session".to_string(),
            ));
        }
        #[cfg(unix)]
        {
            Ok(Self {
                transfer: X11Transfer::connect(display)?,
                display: session::display_name(display),
            })
        }
    }

    /// Paste stored `payloads` into a revalidated `target` (mirrors the
    /// Windows inherent method: the trait `paste_to` has no payload bytes).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unavailable`] without an X session or when no
    /// target can be focused, [`Error::Denied`] for stale targets, and
    /// [`Error::Io`] when synthetic input fails.
    pub fn paste_payloads(
        &self,
        target: &PasteTarget,
        clip: &ClipRef,
        payloads: &[(String, Vec<u8>)],
    ) -> Result<()> {
        self.paste_payloads_with(target, clip, payloads, &mut |_, write| write())
    }

    /// Paste with a hook around each clipboard write, including guarded restore.
    ///
    /// # Errors
    /// Returns an error when the target, transfer, or write hook fails.
    pub fn paste_payloads_with(
        &self,
        target: &PasteTarget,
        clip: &ClipRef,
        payloads: &[(String, Vec<u8>)],
        write: &mut ClipboardWriteHook<'_>,
    ) -> Result<()> {
        let _ = clip;
        if payloads.is_empty() {
            return Err(Error::Unsupported("nothing to paste".to_string()));
        }
        let foreground = X11Foreground::connect(self.display.as_deref())?;
        // Revalidate when a foreground window is known (bare Xvfb has no
        // window manager, so an unknown foreground proceeds after an
        // existence probe of the target window instead).
        if foreground.capture_target().ok().is_some() {
            let current = foreground.capture_target().ok();
            if check_target(target, current.as_ref()).is_err() {
                refocus_window(self.display.as_deref(), target_window(target));
                let again = foreground.capture_target().ok();
                check_target(target, again.as_ref())?;
            }
        } else {
            let win = target_window(target);
            if !window_exists(self.display.as_deref(), win) {
                return Err(Error::Unavailable("paste target dead".to_string()));
            }
        }

        let saved = self
            .transfer
            .read(TEXT_MIME, 64 << 20)
            .ok()
            .map(|bytes| (TEXT_MIME.to_string(), bytes));
        write(payloads, &mut || self.transfer.write(payloads))?;
        let written = self.transfer.unchanged_indicator();
        let written_bytes = payload_bytes(payloads).unwrap_or_default();

        refocus_window(self.display.as_deref(), target_window(target));
        send_ctrl_v()?;
        std::thread::sleep(Duration::from_millis(DWELL_MS));

        let current_bytes = self.transfer.read(TEXT_MIME, 64 << 20).ok();
        let now_indicator = self.transfer.unchanged_indicator();
        let indicator_same = now_indicator == written && now_indicator.is_some();
        let hash_same = current_bytes.as_deref() == Some(written_bytes.as_slice());
        if guarded_restore_decision(indicator_same, hash_same) {
            if let Some(original) = saved {
                let payload = std::slice::from_ref(&original);
                write(payload, &mut || self.transfer.write(payload))?;
            }
        }
        Ok(())
    }
}

impl PasteEngine for X11Paste {
    const AFFINITY: Affinity = Affinity::Worker;

    fn strategy(&self) -> PasteStrategy {
        PasteStrategy::ClipboardRestore
    }

    fn capture_target(&self) -> Result<PasteTarget> {
        X11Foreground::connect(self.display.as_deref())?.capture_target()
    }

    fn paste_to(&self, _target: &PasteTarget, _clip: &ClipRef) -> Result<()> {
        Err(Error::Unsupported(
            "x11 paste needs stored payloads (paste_payloads)".to_string(),
        ))
    }
}

/// Window id named by a paste target (0 when not an X11 handle).
fn target_window(target: &PasteTarget) -> u32 {
    match target.handle {
        PlatformHandle::X11Window(win) => u32::try_from(win).unwrap_or(0),
        _ => 0,
    }
}

/// Bytes pasted for the guarded-restore comparison: the text payload when
/// present, else the first payload.
fn payload_bytes(payloads: &[(String, Vec<u8>)]) -> Option<Vec<u8>> {
    payloads
        .iter()
        .find(|(mime, _)| mime == TEXT_MIME)
        .or_else(|| payloads.first())
        .map(|(_, bytes)| bytes.clone())
}

/// Best-effort focus of `win` (X-F7: `set_input_focus` first, then the
/// `EWMH` `_NET_ACTIVE_WINDOW` message; failures are ignored).
fn refocus_window(display: Option<&str>, win: u32) {
    if win == 0 {
        return;
    }
    let Ok((conn, _)) = session::connect(display) else {
        return;
    };
    let Ok(atoms) = session::Atoms::intern(&conn) else {
        return;
    };
    let win32 = win;
    let _ = xproto::set_input_focus(
        &conn,
        xproto::InputFocus::PARENT,
        win32,
        x11rb14::CURRENT_TIME,
    )
    .ok()
    .and_then(|cookie| cookie.check().ok());
    let mut bytes = [0u8; 32];
    bytes[0] = 33; // ClientMessage
    bytes[1] = 32; // format
    bytes[4..8].copy_from_slice(&win32.to_ne_bytes());
    bytes[8..12].copy_from_slice(&atoms.net_active.to_ne_bytes());
    if let Ok(screen) = session::connect(display).map(|(_, screen)| screen) {
        let root = conn.setup().roots[screen].root;
        let _ = xproto::send_event(&conn, false, root, EventMask::SUBSTRUCTURE_REDIRECT, bytes)
            .ok()
            .and_then(|cookie| cookie.check().ok());
    }
    let _ = conn.flush();
}

/// Send the `Ctrl+V` chord over synthetic input (`XTest`).
fn send_ctrl_v() -> Result<()> {
    #[cfg(not(unix))]
    {
        return Err(Error::Unavailable(
            "synthetic input needs a Unix X11 session".to_string(),
        ));
    }
    #[cfg(unix)]
    {
        use enigo::{Direction, Key, Keyboard as _};

        let mut enigo = enigo::Enigo::new(&enigo::Settings::default())
            .map_err(|e| Error::Unavailable(format!("synthetic input unavailable: {e}")))?;
        enigo
            .key(Key::Control, Direction::Press)
            .map_err(|e| Error::Io(format!("synthetic Ctrl press failed: {e}")))?;
        let chord = enigo.key(Key::Unicode('v'), Direction::Click);
        enigo
            .key(Key::Control, Direction::Release)
            .map_err(|e| Error::Io(format!("synthetic Ctrl release failed: {e}")))?;
        chord.map_err(|e| Error::Io(format!("synthetic paste chord failed: {e}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod modifier_wait_tests {
    use super::*;
    #[test]
    fn waits_until_released_and_refuses_timeout_or_query_failure() {
        let mut reads = 0;
        assert!(wait_modifier_state(Duration::from_secs(1), || {
            reads += 1;
            Ok(reads == 2)
        })
        .is_ok());
        assert_eq!(reads, 2);
        assert!(wait_modifier_state(Duration::ZERO, || Ok(false)).is_err());
        assert!(
            wait_modifier_state(Duration::from_secs(1), || Err(Error::Unavailable(
                "query".into()
            )))
            .is_err()
        );
    }
}
