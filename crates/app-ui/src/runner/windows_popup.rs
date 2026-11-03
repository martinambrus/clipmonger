//! Persistent native popup policy. Winit replaces EXSTYLE on flag changes,
//! including visibility, so keep the tool-window style at that boundary.

use std::io;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongW, SetWindowLongW, SetWindowPos, GWL_EXSTYLE, STYLESTRUCT, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WM_NCDESTROY, WM_STYLECHANGING,
    WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};
use winit::window::Window;

use super::WindowKind;

// The callback address and ID together identify this subclass. There is no
// heap state, borrowed Rust data or reference count behind the native callback.
const SUBCLASS_ID: usize = 1;

const fn popup_style(style: u32) -> u32 {
    (style & !WS_EX_APPWINDOW.0) | WS_EX_TOOLWINDOW.0
}

/// Configure a newly constructed host on its owning event-loop thread, before
/// egui can apply further flags or show it. Only the still-hidden popup needs
/// this policy; Settings retains its ordinary native window behavior.
pub(super) fn install(window: &Window, kind: WindowKind) -> io::Result<()> {
    if kind != WindowKind::Popup {
        return Ok(());
    }
    let raw = super::window::hwnd(window)
        .ok_or_else(|| io::Error::other("popup has no native Windows handle"))?;
    let hwnd = HWND(raw as *mut std::ffi::c_void);
    // SAFETY: hwnd belongs to the live borrowed window on this event-loop
    // thread. The static callback retains no Rust data and removes itself at
    // WM_NCDESTROY. All native calls below stay on that same thread.
    #[allow(unsafe_code)]
    unsafe {
        if !SetWindowSubclass(hwnd, Some(popup_subclass), SUBCLASS_ID, 0).as_bool() {
            return Err(io::Error::other(
                "cannot install popup native window policy",
            ));
        }
        let desired = popup_style(GetWindowLongW(hwnd, GWL_EXSTYLE).cast_unsigned());
        SetWindowLongW(hwnd, GWL_EXSTYLE, desired.cast_signed());
        SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER,
        )
        .map_err(|error| io::Error::other(format!("cannot apply popup frame policy: {error}")))?;
        if GetWindowLongW(hwnd, GWL_EXSTYLE).cast_unsigned() != desired {
            return Err(io::Error::other("cannot apply popup native window policy"));
        }
    }
    Ok(())
}

// No allocation, logging, indexing, panicking operations or borrowed state in
// this FFI callback. All other style bits and messages retain winit's behavior.
#[allow(unsafe_code)]
unsafe extern "system" fn popup_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass_id: usize,
    _data: usize,
) -> LRESULT {
    // SAFETY: the subclass helper supplies this live HWND on its owning thread.
    // For WM_STYLECHANGING, Win32 supplies a writable STYLESTRUCT for the call.
    // The pointer is used only synchronously and is never retained.
    unsafe {
        if message == WM_STYLECHANGING
            && wparam.0.cast_signed() == GWL_EXSTYLE.0 as isize
            && lparam.0 != 0
        {
            let styles = lparam.0 as *mut STYLESTRUCT;
            (*styles).styleNew = popup_style((*styles).styleNew);
        } else if message == WM_NCDESTROY {
            let _ = RemoveWindowSubclass(hwnd, Some(popup_subclass), subclass_id);
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
}
