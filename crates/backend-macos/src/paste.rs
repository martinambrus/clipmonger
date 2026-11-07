//! Clipboard-restore paste (seg-08 §4, B-22..B-24): snapshot the
//! pasteboard, write the clip, reactivate the captured app, post Cmd+V via
//! `CGEventPost`, then restore only while the pasteboard is still ours
//! (`changeCount` AND content hash, user copies win).
//!
//! Refusals after the write (target lost, Secure Input, no Accessibility)
//! leave the clip on the clipboard so the user can press Cmd+V (B-29).

use std::time::{Duration, Instant};

use clipmonger_platform::chord::Chord;
use clipmonger_platform::paste_policy::guarded_restore_decision;
use clipmonger_platform::traits::{ClipboardTransfer as _, PasteEngine};
use clipmonger_platform::types::{Affinity, ClipRef, Error, PasteStrategy, PasteTarget, Result};

use crate::caps::NEEDS_ACCESSIBILITY;
use crate::foreground::{pid_of, MacForeground};
use crate::keys::{self, KeyChord, Modifier};
use crate::mime;
use crate::os;
use crate::transfer::MacTransfer;

/// Dwell holding the clip on the clipboard before the guarded restore.
const DWELL_MS: u64 = 1200;
/// Longest wait for the reactivated target to come to the front.
const ACTIVATE_TIMEOUT_MS: u64 = 600;
/// Poll step while waiting for the target.
const ACTIVATE_POLL_MS: u64 = 25;
/// Pause between activation and the chord (window focus settles).
const CHORD_SETTLE_MS: u64 = 50;
/// Largest clipboard saved for restore.
const SNAPSHOT_MAX: u64 = 64 << 20;
/// UTF-16 units per injected event (`CGEventKeyboardSetUnicodeString`).
const UNICODE_CHUNK: usize = 20;
/// Pause between injected chunks.
const UNICODE_GAP_MS: u64 = 5;

/// Hook invoked around each clipboard write in a paste operation (same
/// shape as the X11 backend): it receives the canonical payload and the
/// actual write, so the daemon can register the self-write first.
pub type ClipboardWriteHook<'a> =
    dyn FnMut(&[(String, Vec<u8>)], &mut dyn FnMut() -> Result<()>) -> Result<()> + 'a;

/// Whether this process may post synthetic input; `prompt` shows the
/// system Accessibility prompt when not yet trusted.
#[must_use]
pub fn accessibility_trusted(prompt: bool) -> bool {
    os::accessibility_trusted(prompt)
}

/// Whether Secure Event Input is on (it swallows synthetic keys).
#[must_use]
pub fn secure_input_enabled() -> bool {
    os::secure_input_enabled()
}

/// Refuse synthetic input that macOS would drop.
fn input_allowed() -> Result<()> {
    if secure_input_enabled() {
        return Err(Error::Denied(
            "secure input is on (a password field or Secure Keyboard Entry): press Cmd+V"
                .to_string(),
        ));
    }
    if !accessibility_trusted(false) {
        return Err(Error::Denied(NEEDS_ACCESSIBILITY.to_string()));
    }
    Ok(())
}

/// Post `chord` (modifiers down, key, release in reverse).
fn post_chord(chord: &KeyChord) -> Result<()> {
    keys::press_sequence(chord, os::post_key)
}

/// Cmd+V.
fn paste_chord() -> KeyChord {
    KeyChord {
        mods: vec![Modifier {
            code: keys::KVK_COMMAND,
            flag: keys::FLAG_COMMAND,
        }],
        key: keys::KVK_V,
    }
}

/// Bytes compared by the guarded restore: the text payload when present,
/// else the first payload.
fn primary_payload(payloads: &[(String, Vec<u8>)]) -> Option<&(String, Vec<u8>)> {
    payloads
        .iter()
        .find(|(m, _)| mime::canonical_kind(m) == Some(mime::Kind::Text))
        .or_else(|| payloads.first())
}

fn content_hash(bytes: &[u8]) -> u64 {
    clipmonger_core::hash::fnv1a64(bytes)
}

/// macOS paste engine (`ClipboardRestore`).
pub struct MacPaste {
    transfer: MacTransfer,
    foreground: MacForeground,
}

impl MacPaste {
    /// Bind to the pasteboard and workspace.
    ///
    /// # Errors
    ///
    /// [`Error::Unavailable`] off macOS or without a user session.
    pub fn new() -> Result<Self> {
        Ok(Self {
            transfer: MacTransfer::new()?,
            foreground: MacForeground::new()?,
        })
    }

    /// Reactivate `pid` and wait until it is frontmost.
    fn bring_to_front(&self, pid: i32) -> Result<()> {
        if MacForeground::frontmost_pid() == Some(pid) {
            return Ok(());
        }
        let _ = self.foreground.activate(pid);
        let deadline = Instant::now() + Duration::from_millis(ACTIVATE_TIMEOUT_MS);
        loop {
            if MacForeground::frontmost_pid() == Some(pid) {
                std::thread::sleep(Duration::from_millis(CHORD_SETTLE_MS));
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(Error::Denied("target changed".to_string()));
            }
            std::thread::sleep(Duration::from_millis(ACTIVATE_POLL_MS));
        }
    }

    /// Paste stored `payloads` into `target` with a pass-through hook.
    ///
    /// # Errors
    ///
    /// As [`Self::paste_payloads_restoring`].
    pub fn paste_payloads(
        &self,
        target: &PasteTarget,
        clip: &ClipRef,
        payloads: &[(String, Vec<u8>)],
    ) -> Result<()> {
        self.paste_payloads_restoring(target, clip, payloads, true, &mut |_, write| write())
    }

    /// Paste with a hook around each clipboard write, including guarded restore.
    ///
    /// # Errors
    ///
    /// As [`Self::paste_payloads_restoring`].
    pub fn paste_payloads_with(
        &self,
        target: &PasteTarget,
        clip: &ClipRef,
        payloads: &[(String, Vec<u8>)],
        write: &mut ClipboardWriteHook<'_>,
    ) -> Result<()> {
        self.paste_payloads_restoring(target, clip, payloads, true, write)
    }

    /// Paste `payloads` into `target`; `restore` false leaves the clip on
    /// the clipboard instead of saving and restoring the previous value.
    ///
    /// # Errors
    ///
    /// [`Error::Unavailable`] for a dead target, [`Error::Denied`] when the
    /// target cannot be brought back, Secure Input is on or Accessibility
    /// is missing (the clip stays on the clipboard), and write/hook errors.
    pub fn paste_payloads_restoring(
        &self,
        target: &PasteTarget,
        clip: &ClipRef,
        payloads: &[(String, Vec<u8>)],
        restore: bool,
        write: &mut ClipboardWriteHook<'_>,
    ) -> Result<()> {
        let _ = clip;
        if payloads.is_empty() {
            return Err(Error::Unsupported("nothing to paste".to_string()));
        }
        let pid = pid_of(target)
            .ok_or_else(|| Error::Unavailable("paste target is not a macOS process".into()))?;
        let current = self.foreground.target_for_pid(pid)?;
        if current.app.exe != target.app.exe {
            return Err(Error::Denied(format!(
                "app changed: {} -> {}",
                target.app.exe, current.app.exe
            )));
        }

        // A clipboard too large to save is not restored (the paste goes on).
        let saved = if restore {
            self.transfer
                .snapshot(SNAPSHOT_MAX)
                .ok()
                .filter(|items| !items.is_empty())
        } else {
            None
        };
        let mut written = None;
        write(payloads, &mut || {
            written = Some(self.transfer.write_counted(payloads)?);
            Ok(())
        })?;
        let written = match written {
            Some(count) => count,
            None => self.transfer.change_count()?,
        };
        let primary = primary_payload(payloads);
        let written_hash = primary.map(|(_, bytes)| content_hash(bytes));

        self.bring_to_front(pid)?;
        input_allowed()?;
        post_chord(&paste_chord())?;

        let Some(saved) = saved else {
            return Ok(());
        };
        std::thread::sleep(Duration::from_millis(DWELL_MS));
        let indicator_same = self.transfer.change_count().ok() == Some(written);
        let hash_same = primary.is_some_and(|(mime, _)| {
            self.transfer
                .read(mime, SNAPSHOT_MAX)
                .ok()
                .map(|bytes| content_hash(&bytes))
                == written_hash
        });
        if guarded_restore_decision(indicator_same, hash_same) {
            let canonical = self.transfer.canonical(&saved);
            write(&canonical, &mut || self.transfer.restore(&saved).map(drop))?;
        }
        Ok(())
    }

    /// Type `text` into the frontmost app (`UnicodeInject`), 20 UTF-16
    /// units per event; the clipboard is untouched.
    ///
    /// # Errors
    ///
    /// [`Error::Denied`] for Secure Input or missing Accessibility.
    pub fn type_unicode(&self, text: &str) -> Result<()> {
        input_allowed()?;
        for (i, chunk) in crate::text::utf16_chunks(text, UNICODE_CHUNK)
            .iter()
            .enumerate()
        {
            if i > 0 {
                std::thread::sleep(Duration::from_millis(UNICODE_GAP_MS));
            }
            os::post_unicode(chunk)?;
        }
        Ok(())
    }

    /// Press `chord` (permanent-clip and per-app paste chords).
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] for unmapped keys, [`Error::Denied`] for
    /// Secure Input or missing Accessibility.
    pub fn press_chord(&self, chord: &Chord) -> Result<()> {
        let resolved = keys::resolve(chord)?;
        input_allowed()?;
        post_chord(&resolved)
    }
}

impl PasteEngine for MacPaste {
    const AFFINITY: Affinity = Affinity::Worker;

    fn strategy(&self) -> PasteStrategy {
        PasteStrategy::ClipboardRestore
    }

    fn capture_target(&self) -> Result<PasteTarget> {
        self.foreground.capture_target()
    }

    fn paste_to(&self, _target: &PasteTarget, _clip: &ClipRef) -> Result<()> {
        Err(Error::Unsupported(
            "macOS paste needs stored payloads (paste_payloads)".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_payload_prefers_text() {
        let payloads = vec![
            ("image/png".to_string(), vec![1]),
            (mime::TEXT.to_string(), b"t".to_vec()),
        ];
        assert_eq!(primary_payload(&payloads).unwrap().0, mime::TEXT);
        assert_eq!(primary_payload(&payloads[..1]).unwrap().0, "image/png");
        assert!(primary_payload(&[]).is_none());
    }

    #[test]
    fn paste_chord_is_command_v() {
        let chord = paste_chord();
        assert_eq!(chord.key, 9);
        assert_eq!(chord.flags(), keys::FLAG_COMMAND);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn unavailable_off_macos() {
        assert!(matches!(MacPaste::new(), Err(Error::Unavailable(_))));
    }
}
