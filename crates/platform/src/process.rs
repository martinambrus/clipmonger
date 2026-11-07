//! Process facts the daemon checks before trusting a peer.

use std::path::PathBuf;

/// Executable path of process `pid` (macOS: `proc_pidpath` from libSystem).
#[cfg(target_os = "macos")]
#[must_use]
#[allow(unsafe_code)] // Sole FFI call: libproc fills a caller-owned buffer.
pub fn image_path(pid: u32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    // PROC_PIDPATHINFO_MAXSIZE (4 * MAXPATHLEN).
    const MAX: u32 = 4096;
    extern "C" {
        fn proc_pidpath(pid: i32, buffer: *mut u8, buffersize: u32) -> i32;
    }
    let pid = i32::try_from(pid).ok()?;
    let mut buffer = vec![0_u8; MAX as usize];
    // SAFETY: `buffer` is valid for `MAX` bytes and outlives the call; the
    // function writes at most `buffersize` bytes and returns the length.
    let len = unsafe { proc_pidpath(pid, buffer.as_mut_ptr(), MAX) };
    let len = usize::try_from(len)
        .ok()
        .filter(|&len| len > 0 && len <= buffer.len())?;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(&buffer[..len])))
}

/// Executable path of process `pid` (not implemented on this platform).
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn image_path(pid: u32) -> Option<PathBuf> {
    let _ = pid;
    None
}
