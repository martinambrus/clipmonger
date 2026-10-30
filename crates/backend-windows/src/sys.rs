//! Raw Win32 helpers. This is the only module in the crate that may use
//! `unsafe`; every `unsafe` block carries a one-line safety note.
//!
//! Compiled only on Windows; on other systems this module is empty.

#![cfg(windows)]
#![allow(unsafe_code)]

use std::ffi::{c_void, OsString};
use std::os::windows::ffi::OsStringExt;
use std::sync::mpsc::Sender;

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, HWND};
use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_QUERY};
use windows_sys::Win32::System::DataExchange::{
    AddClipboardFormatListener, RemoveClipboardFormatListener,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegQueryValueExW, RegSetValueExW,
    HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, GetWindowLongPtrW,
    GetWindowThreadProcessId, PostMessageW, PostQuitMessage, RegisterClassExW, SetWindowLongPtrW,
    CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, GWLP_USERDATA, HWND_MESSAGE, MSG, WM_CLIPBOARDUPDATE,
    WM_CLOSE, WM_CREATE, WM_DESTROY, WNDCLASSEXW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowTextW, SetForegroundWindow,
};

/// Last Win32 error as a short string.
pub(crate) fn last_error() -> String {
    // Safety: GetLastError only reads the calling thread's error slot.
    let code = unsafe { GetLastError() };
    format!("win32 error {code}")
}

/// Encode as nul-terminated UTF-16 for Win32 `*W` calls.
fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---------------------------------------------------------------------------
// Clipboard session owner
// ---------------------------------------------------------------------------

/// Message-only window that owns one clipboard session. Opening with a real
/// window makes the open exclusive: two `OpenClipboard(NULL)` callers do not
/// exclude each other, and the other side's `CloseClipboard` ends ours
/// mid-operation. Created, used and destroyed on the calling thread.
pub(crate) struct ClipboardWindow(HWND);

impl ClipboardWindow {
    /// Create the window, or `None` when the session refuses one.
    pub(crate) fn new() -> Option<Self> {
        let class = to_wide("STATIC");
        // Safety: "STATIC" is a predefined class; message-only parent, no create params.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                std::ptr::null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        (!hwnd.is_null()).then_some(Self(hwnd))
    }

    /// The window handle, for `OpenClipboard`.
    pub(crate) fn hwnd(&self) -> HWND {
        self.0
    }
}

impl Drop for ClipboardWindow {
    fn drop(&mut self) {
        // Safety: the window was created on this thread and is destroyed exactly once.
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::DestroyWindow(self.0);
        }
    }
}

// ---------------------------------------------------------------------------
// Message-only monitor pump
// ---------------------------------------------------------------------------

/// The global-hotkey manager owns a raw HWND, so it is not auto-`Send`.
// Safety: every `WinHotkeys` is created and used from its owning pump
// thread only (the daemon keeps one manager on the backend thread); the
// handle is never touched concurrently.
unsafe impl Send for crate::hotkeys::WinHotkeys {}

/// RAII handle for the hidden-window clipboard listener thread.
///
/// The HWND is stored as `usize` (handle values round-trip losslessly) so
/// this handle stays `Send`; the pump thread reconstitutes it locally.
pub(crate) struct PumpHandle {
    hwnd: usize,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl PumpHandle {
    /// Ask the pump thread to exit and wait for it.
    pub(crate) fn stop(&mut self) {
        // Safety: hwnd names a live message-only window owned by the pump thread.
        unsafe {
            PostMessageW(self.hwnd as HWND, WM_CLOSE, 0, 0);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Window procedure: forwards `WM_CLIPBOARDUPDATE` wakes to the channel.
unsafe extern "system" fn monitor_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    match msg {
        WM_CREATE => {
            // Safety: lParam is the CREATESTRUCT passed by CreateWindowExW below.
            let create = unsafe {
                &*(lparam as *const windows_sys::Win32::UI::WindowsAndMessaging::CREATESTRUCTW)
            };
            // Safety: lpCreateParams is the Box<Sender> pointer handed to CreateWindowExW.
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            }
            0
        }
        WM_CLIPBOARDUPDATE => {
            // Safety: USERDATA holds the Sender installed at WM_CREATE for this window.
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Sender<()>;
            if !ptr.is_null() {
                // Safety: pointer is valid until WM_DESTROY reclaims the Box.
                let _ = unsafe { &*ptr }.send(());
            }
            0
        }
        WM_DESTROY => {
            // Safety: reclaim the Box<Sender> installed at WM_CREATE; no further messages use it.
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut Sender<()>;
            if !ptr.is_null() {
                drop(unsafe { Box::from_raw(ptr) });
                // Safety: clearing USERDATA after reclaiming; window is being destroyed.
                unsafe {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                }
            }
            // Safety: no preconditions beyond calling on the window's thread.
            unsafe {
                PostQuitMessage(0);
            }
            0
        }
        _ => {
            // Safety: default handler for unhandled messages; hwnd is valid during dispatch.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}

/// Spawn the pump thread: hidden `HWND_MESSAGE` window plus
/// `AddClipboardFormatListener`; `WM_CLIPBOARDUPDATE` posts `()` to `wakes`.
///
/// The window is created on the pump thread itself: Win32 delivers a
/// window's messages to the thread that created it, so creating it on the
/// caller and pumping elsewhere would starve the pump (no wakes) and wedge
/// `stop` (the `WM_CLOSE` wake would sit in the caller's queue while the
/// pump's `join` waits forever).
pub(crate) fn start_pump(wakes: Sender<()>) -> Result<PumpHandle, String> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<usize, String>>();
    let thread = std::thread::spawn(move || {
        let class: Vec<u16> = to_wide("ClipmongerMonitor");
        // Safety: GetModuleHandleW(null) returns the current module handle or fails.
        let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
        if instance.is_null() {
            let _ = ready_tx.send(Err(format!("GetModuleHandleW failed: {}", last_error())));
            return;
        }
        let mut class_info = WNDCLASSEXW {
            cbSize: u32::try_from(std::mem::size_of::<WNDCLASSEXW>()).unwrap_or(u32::MAX),
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(monitor_wndproc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..Default::default()
        };
        // Safety: class struct is fully initialised; Atom 0 means failure or already-registered.
        let atom = unsafe { RegisterClassExW(&raw mut class_info) };
        if atom == 0 {
            // Safety: reading the thread error slot after a failed call.
            let code = unsafe { GetLastError() };
            if code != ERROR_CLASS_ALREADY_EXISTS {
                let _ = ready_tx.send(Err(format!("RegisterClassExW failed: win32 error {code}")));
                return;
            }
        }
        let boxed: Box<Sender<()>> = Box::new(wakes);
        let param = Box::into_raw(boxed).cast::<c_void>();
        // Safety: class is registered, strings are nul-terminated, param is reclaimed at WM_DESTROY.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                0,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                HWND_MESSAGE,
                std::ptr::null_mut(),
                instance,
                param,
            )
        };
        if hwnd.is_null() {
            drop(unsafe { Box::from_raw(param.cast::<Sender<()>>()) });
            let _ = ready_tx.send(Err(format!("CreateWindowExW failed: {}", last_error())));
            return;
        }
        // Safety: hwnd names a live window created above on this thread.
        let added = unsafe { AddClipboardFormatListener(hwnd) };
        if added == 0 {
            let err = format!("AddClipboardFormatListener refused: {}", last_error());
            // Safety: listener never attached; tearing down the window created above.
            // (DestroyWindow runs WM_DESTROY synchronously, which reclaims
            // the boxed sender, so it must not be reclaimed again here.)
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd);
            }
            let _ = ready_tx.send(Err(err));
            return;
        }
        let _ = ready_tx.send(Ok(hwnd as usize));
        // Safety: standard pump; GetMessageW returns 0 only on WM_QUIT.
        // WM_CLOSE (posted by `stop`) falls through to DefWindowProc, which
        // destroys the window; WM_DESTROY posts the quit that ends this loop,
        // so the `join` in `stop` cannot wait forever.
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&raw mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                DispatchMessageW(&raw const msg);
            }
        }
        // Safety: listener was added; removing before thread exit.
        unsafe {
            RemoveClipboardFormatListener(hwnd);
        }
    });
    // Wait (bounded) for the pump thread to install the listener.
    match ready_rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(Ok(hwnd)) => Ok(PumpHandle {
            hwnd,
            thread: Some(thread),
        }),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => Err("monitor pump thread exited during startup".to_string()),
    }
}

// ---------------------------------------------------------------------------
// Foreground window + integrity
// ---------------------------------------------------------------------------

/// Foreground window handle, or `None` when there is none.
pub fn foreground_hwnd() -> Option<HWND> {
    // Safety: GetForegroundWindow takes no arguments and has no preconditions.
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_null() {
        None
    } else {
        Some(hwnd)
    }
}

/// Whether `hwnd` still names an existing window.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[must_use]
pub fn is_window(hwnd: HWND) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::IsWindow;
    // Safety: IsWindow only validates the handle value.
    unsafe { IsWindow(hwnd) != 0 }
}

/// Make `hwnd` the foreground window: attach to the current foreground
/// thread's input state for the duration of the call (so the request is
/// honored without our own foreground rights), raise, activate, detach.
/// Returns whether `hwnd` is the foreground window afterwards.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn activate_window(hwnd: HWND) -> bool {
    use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows_sys::Win32::UI::WindowsAndMessaging::{BringWindowToTop, IsHungAppWindow};
    // Safety: plain window-manager calls on handle values; a failed attach
    // or activation only leaves the foreground unchanged.
    unsafe {
        // Activating a hung window, or sharing input state with a hung
        // foreground thread, can block this thread indefinitely.
        if IsHungAppWindow(hwnd) != 0 {
            return false;
        }
        let foreground = GetForegroundWindow();
        let fg_thread = if foreground.is_null() || IsHungAppWindow(foreground) != 0 {
            0
        } else {
            GetWindowThreadProcessId(foreground, std::ptr::null_mut())
        };
        let own_thread = GetCurrentThreadId();
        let attached = fg_thread != 0
            && fg_thread != own_thread
            && AttachThreadInput(own_thread, fg_thread, 1) != 0;
        BringWindowToTop(hwnd);
        SetForegroundWindow(hwnd);
        if attached {
            AttachThreadInput(own_thread, fg_thread, 0);
        }
        GetForegroundWindow() == hwnd
    }
}

/// Server process id of a connected named-pipe client handle, or `None`
/// when it cannot be determined (the helper's check that it talks to our
/// own daemon binary).
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[must_use]
pub fn pipe_server_pid(pipe_handle: *mut std::ffi::c_void) -> Option<u32> {
    use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
    let mut pid: u32 = 0;
    // Safety: handle is the caller's live pipe client handle; pid slot is
    // valid for the call.
    let ok = unsafe { GetNamedPipeServerProcessId(pipe_handle, &raw mut pid) };
    if ok == 0 {
        None
    } else {
        Some(pid)
    }
}

/// Owning process id of a `WinHwnd` handle value (daemon supervisor seam).
#[must_use]
pub fn hwnd_owner_pid(hwnd: i64) -> Option<u32> {
    // Safety: i64 handle round-trips through HWND on 64-bit Windows (the
    // only target this backend supports); the query never frees it.
    window_pid(hwnd as HWND)
}

/// Owning process id of `hwnd`, or `None` when it cannot be determined.
pub fn window_pid(hwnd: HWND) -> Option<u32> {
    let mut pid: u32 = 0;
    // Safety: pid pointer is valid for the call; return value is the thread id.
    unsafe {
        GetWindowThreadProcessId(hwnd, &raw mut pid);
    }
    if pid == 0 {
        None
    } else {
        Some(pid)
    }
}

/// Lowercased exe basename for `pid`, or `None` when it cannot be determined.
pub(crate) fn process_basename(pid: u32) -> Option<String> {
    // Safety: OpenProcess with a query flag; handle closed below on all paths.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let mut len: u32 = 1024;
    let mut buf = vec![0u16; len as usize];
    // Safety: buffer and length pointer are valid; handle has query rights.
    let ok = unsafe { QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &raw mut len) };
    // Safety: handle came from OpenProcess and is still open here.
    unsafe {
        CloseHandle(handle);
    }
    if ok == 0 {
        return None;
    }
    buf.truncate(len as usize);
    let path = OsString::from_wide(&buf).to_string_lossy().into_owned();
    let base = path.rsplit(['\\', '/']).next().unwrap_or(&path);
    Some(base.to_ascii_lowercase())
}

/// FNV-1a hash of the window title; 0 when the title is unavailable.
pub(crate) fn window_title_hash(hwnd: HWND) -> u64 {
    // Safety: length query first; hwnd names a window readable by this process.
    let len = unsafe { GetWindowTextW(hwnd, std::ptr::null_mut(), 0) };
    if len <= 0 {
        return 0;
    }
    let mut buf = vec![0u16; usize::try_from(len).unwrap_or(0) + 1];
    // Safety: buffer sized len+1 from the length query on the same window.
    let got = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), len + 1) };
    if got <= 0 {
        return 0;
    }
    buf.truncate(usize::try_from(got).unwrap_or(0));
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for unit in buf {
        hash ^= u64::from(unit);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// Whether `pid` runs elevated, or `None` when it cannot be determined.
pub fn process_elevated(pid: u32) -> Option<bool> {
    // Safety: OpenProcess with a query flag; handle closed below on all paths.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let elevated = token_elevated(handle);
    // Safety: handle came from OpenProcess and is still open here.
    unsafe {
        CloseHandle(handle);
    }
    elevated
}

/// Whether the current process runs elevated.
pub(crate) fn current_process_elevated() -> Option<bool> {
    // Safety: GetCurrentProcess returns a pseudo-handle needing no close.
    let pseudo: HANDLE = unsafe { GetCurrentProcess() };
    token_elevated(pseudo)
}

/// Read `TokenElevation` for a process or pseudo-handle; never closes it.
fn token_elevated(process: HANDLE) -> Option<bool> {
    let mut token: HANDLE = std::ptr::null_mut();
    // Safety: token pointer is valid; handle has query rights (or is the pseudo-handle).
    let ok = unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut token) };
    if ok == 0 || token.is_null() {
        return None;
    }
    let mut elevated: u32 = 0;
    let mut returned: u32 = 0;
    // Safety: output slot sized for a u32 elevation flag; token closed below.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            (&raw mut elevated).cast::<c_void>(),
            u32::try_from(std::mem::size_of::<u32>()).unwrap_or(u32::MAX),
            &raw mut returned,
        )
    };
    // Safety: token came from OpenProcessToken and is still open here.
    unsafe {
        CloseHandle(token);
    }
    if ok == 0 {
        None
    } else {
        Some(elevated != 0)
    }
}

// ---------------------------------------------------------------------------
// Autostart registry
// ---------------------------------------------------------------------------

/// HKCU subkey holding per-user Run values.
const RUN_SUBKEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
/// HKCU subkey holding `StartupApproved` overrides.
const APPROVED_SUBKEY: &str =
    "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run";
/// This daemon's Run value name.
pub(crate) const RUN_VALUE_NAME: &str = "clipmonger";
/// `RegCreateKeyExW` conflict: the window class is already registered.
const ERROR_CLASS_ALREADY_EXISTS: u32 = 1410;
/// `RegDeleteValueW`/`RegQueryValueExW` missing value.
const ERROR_FILE_NOT_FOUND: u32 = 2;

/// Open (creating if needed) a HKCU subkey; caller must `RegCloseKey`.
fn open_key(
    subkey: &str,
    write: bool,
) -> Result<windows_sys::Win32::System::Registry::HKEY, String> {
    let wide = to_wide(subkey);
    let mut key: windows_sys::Win32::System::Registry::HKEY = std::ptr::null_mut();
    let access = if write {
        KEY_SET_VALUE
    } else {
        KEY_QUERY_VALUE
    };
    // Safety: subkey string is nul-terminated; key slot receives the opened handle.
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            wide.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            access,
            std::ptr::null(),
            &raw mut key,
            std::ptr::null_mut(),
        )
    };
    if status != 0 {
        return Err(format!("RegCreateKeyExW({subkey}) failed: status {status}"));
    }
    Ok(key)
}

/// Read this daemon's Run command line, or `None` when absent.
pub(crate) fn run_value_get() -> Result<Option<String>, String> {
    let key = open_key(RUN_SUBKEY, false)?;
    let name = to_wide(RUN_VALUE_NAME);
    let mut kind: u32 = 0;
    let mut len: u32 = 0;
    // Safety: length query with null data; key was just opened for query.
    let status = unsafe {
        RegQueryValueExW(
            key,
            name.as_ptr(),
            std::ptr::null_mut(),
            &raw mut kind,
            std::ptr::null_mut(),
            &raw mut len,
        )
    };
    if status != 0 {
        // Safety: key came from open_key and is still open here.
        unsafe {
            RegCloseKey(key);
        }
        return Ok(None);
    }
    let mut buf = vec![0u16; len as usize / 2 + 1];
    let mut actual: u32 = u32::try_from(buf.len())
        .unwrap_or(u32::MAX)
        .saturating_mul(2);
    // Safety: buffer sized from the length query on the same value.
    let status = unsafe {
        RegQueryValueExW(
            key,
            name.as_ptr(),
            std::ptr::null_mut(),
            &raw mut kind,
            buf.as_mut_ptr().cast::<u8>(),
            &raw mut actual,
        )
    };
    // Safety: key came from open_key and is still open here.
    unsafe {
        RegCloseKey(key);
    }
    if status != 0 || kind != REG_SZ {
        return Ok(None);
    }
    let units = (actual as usize / 2).min(buf.len());
    let mut wide = buf[..units].to_vec();
    while wide.last() == Some(&0) {
        wide.pop();
    }
    Ok(Some(
        OsString::from_wide(&wide).to_string_lossy().into_owned(),
    ))
}

/// Write this daemon's Run command line (`REG_SZ`).
pub(crate) fn run_value_set(command: &str) -> Result<(), String> {
    let key = open_key(RUN_SUBKEY, true)?;
    let name = to_wide(RUN_VALUE_NAME);
    let value = to_wide(command);
    let bytes = u32::try_from(value.len())
        .unwrap_or(u32::MAX)
        .saturating_mul(2);
    // Safety: name and value buffers are nul-terminated; key opened for set.
    let status = unsafe {
        RegSetValueExW(
            key,
            name.as_ptr(),
            0,
            REG_SZ,
            value.as_ptr().cast::<u8>(),
            bytes,
        )
    };
    // Safety: key came from open_key and is still open here.
    unsafe {
        RegCloseKey(key);
    }
    if status != 0 {
        return Err(format!("RegSetValueExW failed: status {status}"));
    }
    Ok(())
}

/// Delete this daemon's Run value; missing values are success.
pub(crate) fn run_value_delete() -> Result<(), String> {
    let key = open_key(RUN_SUBKEY, true)?;
    let name = to_wide(RUN_VALUE_NAME);
    // Safety: name is nul-terminated; key opened for set; missing value (2) is success.
    let status = unsafe { RegDeleteValueW(key, name.as_ptr()) };
    // Safety: key came from open_key and is still open here.
    unsafe {
        RegCloseKey(key);
    }
    if status != 0 && status as u32 != ERROR_FILE_NOT_FOUND {
        return Err(format!("RegDeleteValueW failed: status {status}"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Elevated-helper spawn + peer authentication (seg-04 S5)
//
// The runas/UAC served leg is manual QA only: nothing here is ever called
// from automated tests. The helper pipe name derives from the profile path
// (a rendezvous, never a credential; both sides verify the peer binary);
// NO secret travels in argv.
// ---------------------------------------------------------------------------

/// Spawn the elevated helper worker via `ShellExecuteEx` with the `runas`
/// verb (UAC consent prompt; the call returns once the prompt is answered).
/// `args` carries only `__elevated-worker --profile-dir <profile>`: the
/// worker derives the daemon pipe from the profile and both sides verify
/// the peer binary, so no secret travels in argv.
///
/// # Errors
///
/// Returns a message when the `runas` launch is refused (UAC cancel, missing
/// binary, policy block).
pub fn spawn_elevated_worker(exe: &str, profile: &std::path::Path) -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;
    let verb = to_wide("runas");
    let file = to_wide(exe);
    let params = to_wide(&format!(
        "__elevated-worker --profile-dir {}",
        crate::helper::quote_arg(&profile.to_string_lossy())
    ));
    // Safety: zeroed struct with only documented input fields set; string
    // pointers stay alive for the call; hProcess (when returned) closed below.
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = u32::try_from(std::mem::size_of::<SHELLEXECUTEINFOW>()).unwrap_or(u32::MAX);
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = SW_HIDE;
    // Safety: info only borrows live nul-terminated buffers above.
    let ok = unsafe { ShellExecuteExW(&raw mut info) };
    if ok == 0 {
        return Err(format!("ShellExecuteEx(runas) refused: {}", last_error()));
    }
    if !info.hProcess.is_null() {
        // Safety: handle came from ShellExecuteEx with NOCLOSEPROCESS.
        unsafe {
            CloseHandle(info.hProcess);
        }
    }
    Ok(())
}

/// Full image path for `pid` (peer authentication: must equal our own exe).
#[must_use]
pub fn process_image_path(pid: u32) -> Option<String> {
    // Safety: OpenProcess with a query flag; handle closed below on all paths.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let mut len: u32 = 32768;
    let mut buf = vec![0u16; len as usize];
    // Safety: buffer and length pointer are valid; handle has query rights.
    let ok = unsafe { QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &raw mut len) };
    // Safety: handle came from OpenProcess and is still open here.
    unsafe {
        CloseHandle(handle);
    }
    if ok == 0 {
        return None;
    }
    buf.truncate(len as usize);
    Some(OsString::from_wide(&buf).to_string_lossy().into_owned())
}

/// Whether `pid` runs with an elevated token (daemon-side check that the
/// helper peer is high-integrity).
#[must_use]
pub fn process_token_high(pid: u32) -> Option<bool> {
    // Safety: OpenProcess with a query flag; handle closed below on all paths.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let elevated = token_elevated(handle);
    // Safety: handle came from OpenProcess and is still open here.
    unsafe {
        CloseHandle(handle);
    }
    elevated
}

/// Client process id of a connected named-pipe server instance, or `None`
/// when it cannot be determined (peer-authentication seam for the daemon's
/// reverse-connect helper pipe; keeps `unsafe` inside this module).
///
/// Safe wrapper by design: the handle is only passed through to the OS
/// query, never dereferenced as Rust data.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[must_use]
pub fn pipe_client_pid(pipe_handle: *mut std::ffi::c_void) -> Option<u32> {
    use windows_sys::Win32::System::Pipes::GetNamedPipeClientProcessId;
    let mut pid: u32 = 0;
    // Safety: handle is the live connected pipe instance owned by the
    // caller; pid slot is valid for the call.
    let ok = unsafe { GetNamedPipeClientProcessId(pipe_handle, &raw mut pid) };
    if ok == 0 {
        None
    } else {
        Some(pid)
    }
}

/// Install `Program Files` directory for the protected-path rule, from the
/// shell known-folder store (not `%ProgramFiles%`, which any process can
/// set for the processes it starts). Empty when it cannot be resolved; an
/// empty path matches nothing, so the caller falls back to portable mode.
#[must_use]
pub fn program_files_dir() -> std::path::PathBuf {
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{FOLDERID_ProgramFiles, SHGetKnownFolderPath};
    let mut raw: windows_sys::core::PWSTR = std::ptr::null_mut();
    // Safety: valid GUID and out-pointer; the buffer is freed below.
    let hr = unsafe {
        SHGetKnownFolderPath(
            &FOLDERID_ProgramFiles,
            0,
            std::ptr::null_mut(),
            &raw mut raw,
        )
    };
    let mut path = std::path::PathBuf::new();
    if hr >= 0 && !raw.is_null() {
        let mut len = 0;
        // Safety: on success `raw` is a nul-terminated wide string.
        unsafe {
            while *raw.add(len) != 0 {
                len += 1;
            }
            path = OsString::from_wide(std::slice::from_raw_parts(raw, len)).into();
        }
    }
    if !raw.is_null() {
        // Safety: allocated by SHGetKnownFolderPath; freed exactly once.
        unsafe { CoTaskMemFree(raw.cast()) };
    }
    path
}

/// 32 hex chars of OS randomness for the helper pipe rendezvous name.
pub fn random_pipe_segment() -> String {
    use std::fmt::Write as _;
    use windows_sys::Win32::Security::Cryptography::{
        BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
    };
    let mut bytes = [0u8; 16];
    // Safety: output buffer is a live 16-byte array; null algorithm handle
    // selects the system-preferred RNG per BCRYPT_USE_SYSTEM_PREFERRED_RNG.
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            bytes.as_mut_ptr(),
            u32::try_from(bytes.len()).unwrap_or(u32::MAX),
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status != 0 {
        return fallback_pipe_segment();
    }
    let mut out = String::with_capacity(32);
    for byte in &bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Non-OS fallback when the system RNG is unavailable (never preferred).
fn fallback_pipe_segment() -> String {
    use std::fmt::Write as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    static CTR: AtomicU64 = AtomicU64::new(0);
    let n = CTR.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
    let mut x = nanos
        .wrapping_add(n.wrapping_mul(0x9E37_79B9_7F4A_7C15))
        .wrapping_add(u64::from(std::process::id()) << 32);
    let mut out = String::with_capacity(32);
    for _ in 0..2 {
        x ^= x >> 30;
        x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x ^= x >> 27;
        x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
        x ^= x >> 31;
        let _ = write!(out, "{x:016x}");
    }
    out
}

/// Raw `StartupApproved` override blob for this daemon's value, if present.
pub(crate) fn startup_approved_blob() -> Option<Vec<u8>> {
    let key = open_key(APPROVED_SUBKEY, false).ok()?;
    let name = to_wide(RUN_VALUE_NAME);
    let mut kind: u32 = 0;
    let mut len: u32 = 0;
    // Safety: length query with null data; key was just opened for query.
    let status = unsafe {
        RegQueryValueExW(
            key,
            name.as_ptr(),
            std::ptr::null_mut(),
            &raw mut kind,
            std::ptr::null_mut(),
            &raw mut len,
        )
    };
    if status != 0 || len == 0 || len > 1 << 20 {
        // Safety: key came from open_key and is still open here.
        unsafe {
            RegCloseKey(key);
        }
        return None;
    }
    let mut buf = vec![0u8; len as usize];
    // Safety: buffer sized from the length query on the same value.
    let status = unsafe {
        RegQueryValueExW(
            key,
            name.as_ptr(),
            std::ptr::null_mut(),
            &raw mut kind,
            buf.as_mut_ptr(),
            &raw mut len,
        )
    };
    // Safety: key came from open_key and is still open here.
    unsafe {
        RegCloseKey(key);
    }
    if status != 0 {
        return None;
    }
    buf.truncate(len as usize);
    Some(buf)
}
