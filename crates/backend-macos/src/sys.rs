//! Raw C FFI (macOS only): `CGEvent` posting, Accessibility trust, Secure
//! Input, and the `CoreServices` UTI constants `AppKit` lacks before 10.13.
//!
//! The only plain-C unsafe boundary of the crate (`appkit` holds the
//! Objective-C sends). Every call here takes plain values or pointers it
//! owns for the duration of the call.
#![allow(unsafe_code)]

use std::ffi::c_void;

use clipmonger_platform::types::{Error, Result};

type CFTypeRef = *const c_void;
/// Toll-free bridged to `NSString`.
pub(crate) type CFStringRef = *const c_void;
type CGEventRef = *mut c_void;

/// `kCGHIDEventTap`.
const HID_EVENT_TAP: u32 = 0;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: CFTypeRef);
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventCreateKeyboardEvent(source: *const c_void, key: u16, down: bool) -> CGEventRef;
    fn CGEventSetFlags(event: CGEventRef, flags: u64);
    fn CGEventKeyboardSetUnicodeString(event: CGEventRef, len: std::ffi::c_ulong, s: *const u16);
    fn CGEventPost(tap: u32, event: CGEventRef);
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> u8;
    static kAXTrustedCheckOptionPrompt: CFStringRef;
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn IsSecureEventInputEnabled() -> u8;
}

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    static kUTTypeFileURL: CFStringRef;
    static kUTTypeUTF16PlainText: CFStringRef;
}

/// `kUTTypeFileURL` (`NSPasteboardTypeFileURL` needs 10.13).
pub(crate) fn uti_file_url() -> CFStringRef {
    // Safety: immutable framework constant.
    unsafe { kUTTypeFileURL }
}

/// `kUTTypeUTF16PlainText` (no `AppKit` constant).
pub(crate) fn uti_utf16() -> CFStringRef {
    // Safety: immutable framework constant.
    unsafe { kUTTypeUTF16PlainText }
}

/// `kAXTrustedCheckOptionPrompt`.
pub(crate) fn ax_prompt_key() -> CFStringRef {
    // Safety: immutable framework constant.
    unsafe { kAXTrustedCheckOptionPrompt }
}

/// `AXIsProcessTrustedWithOptions(options)`; `options` is a live
/// `CFDictionary` (or null) owned by the caller.
pub(crate) fn ax_trusted_with(options: *const c_void) -> bool {
    // Safety: the caller keeps `options` alive across the call.
    unsafe { AXIsProcessTrustedWithOptions(options) != 0 }
}

/// Whether Secure Event Input is on (password field, Secure Keyboard Entry).
pub(crate) fn secure_input_enabled() -> bool {
    // Safety: no arguments, no state.
    unsafe { IsSecureEventInputEnabled() != 0 }
}

/// Owned `CGEvent`, released on drop.
struct Event(CGEventRef);

impl Drop for Event {
    fn drop(&mut self) {
        // Safety: created by a CGEventCreate* call and released once.
        unsafe { CFRelease(self.0.cast_const()) }
    }
}

fn keyboard_event(code: u16, down: bool) -> Result<Event> {
    // Safety: null source is allowed; the result is checked for null.
    let event = unsafe { CGEventCreateKeyboardEvent(std::ptr::null(), code, down) };
    if event.is_null() {
        return Err(Error::Io("CGEventCreateKeyboardEvent failed".to_string()));
    }
    Ok(Event(event))
}

/// Post one key event with explicit modifier `flags` to the HID tap.
pub(crate) fn post_key(code: u16, down: bool, flags: u64) -> Result<()> {
    let event = keyboard_event(code, down)?;
    // Safety: `event.0` is a live keyboard event.
    unsafe {
        CGEventSetFlags(event.0, flags);
        CGEventPost(HID_EVENT_TAP, event.0);
    }
    Ok(())
}

/// Post a down/up pair carrying `units` as the typed string.
pub(crate) fn post_unicode(units: &[u16]) -> Result<()> {
    let len = std::ffi::c_ulong::try_from(units.len())
        .map_err(|_| Error::Internal("unicode chunk too long".to_string()))?;
    for down in [true, false] {
        let event = keyboard_event(0, down)?;
        // Safety: `units` outlives the call; the event copies the string.
        unsafe {
            CGEventSetFlags(event.0, 0);
            CGEventKeyboardSetUnicodeString(event.0, len, units.as_ptr());
            CGEventPost(HID_EVENT_TAP, event.0);
        }
    }
    Ok(())
}
