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
    fn CFStringCreateWithCString(
        alloc: *const c_void,
        s: *const std::ffi::c_char,
        encoding: u32,
    ) -> CFStringRef;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventCreateKeyboardEvent(source: *const c_void, key: u16, down: bool) -> CGEventRef;
    fn CGEventSetFlags(event: CGEventRef, flags: u64);
    fn CGEventKeyboardSetUnicodeString(event: CGEventRef, len: std::ffi::c_ulong, s: *const u16);
    fn CGEventPost(tap: u32, event: CGEventRef);
    fn CGEventSourceFlagsState(state_id: i32) -> u64;
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> u8;
    static kAXTrustedCheckOptionPrompt: CFStringRef;
    fn AXUIElementCreateSystemWide() -> CFTypeRef;
    fn AXUIElementSetMessagingTimeout(element: CFTypeRef, seconds: f32) -> i32;
    fn AXUIElementCopyAttributeValue(
        element: CFTypeRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> i32;
    fn AXUIElementGetPid(element: CFTypeRef, pid: *mut i32) -> i32;
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

/// Pid of the application that has the keyboard focus, asked live through
/// the Accessibility API (needs Accessibility trust). Unlike
/// `NSWorkspace.frontmostApplication`, which some macOS versions (10.15)
/// refresh only late in a background process, this reflects an activation
/// as soon as the window server has done it.
pub(crate) fn ax_focused_app_pid() -> Option<i32> {
    const UTF8: u32 = 0x0800_0100;
    // Safety: every CF object created here is checked for null and released
    // once; the out pointers are locals that live across the calls.
    unsafe {
        let attr =
            CFStringCreateWithCString(std::ptr::null(), c"AXFocusedApplication".as_ptr(), UTF8);
        if attr.is_null() {
            return None;
        }
        let system = AXUIElementCreateSystemWide();
        if system.is_null() {
            CFRelease(attr);
            return None;
        }
        AXUIElementSetMessagingTimeout(system, 0.25);
        let mut app: CFTypeRef = std::ptr::null();
        let err = AXUIElementCopyAttributeValue(system, attr, &raw mut app);
        CFRelease(attr);
        CFRelease(system);
        if err != 0 || app.is_null() {
            return None;
        }
        let mut pid = 0;
        let err = AXUIElementGetPid(app, &raw mut pid);
        CFRelease(app);
        (err == 0 && pid > 0).then_some(pid)
    }
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

/// Modifier flags (Shift, Control, Option, Command) held on the hardware
/// keyboard right now (`kCGEventSourceStateHIDSystemState`).
pub(crate) fn held_modifier_flags() -> u64 {
    const HID_SYSTEM_STATE: i32 = 1;
    // Shift | Control | Alternate | Command (CGEventFlags masks).
    const MODIFIERS: u64 = 0x0002_0000 | 0x0004_0000 | 0x0008_0000 | 0x0010_0000;
    // Safety: pure query without pointers.
    unsafe { CGEventSourceFlagsState(HID_SYSTEM_STATE) & MODIFIERS }
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
