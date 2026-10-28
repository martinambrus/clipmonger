//! Scheduled-task installer for the elevated helper (one-time UAC).
//!
//! Portable mode never touches the Task Scheduler. Creation uses
//! `schtasks.exe /create /rl HIGHEST` via a `runas` ShellExecute so the
//! single UAC prompt comes from Windows, not from this code.

use std::path::Path;

const TASK_NAME: &str = "ClipmongerHelper";

#[cfg(windows)]
fn schtasks_exists() -> bool {
    std::process::Command::new("schtasks")
        .arg("/query")
        .arg("/tn")
        .arg(TASK_NAME)
        .arg("/nh")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn schtasks_exists() -> bool {
    false
}

/// Whether the helper task is registered.
#[must_use]
pub fn is_installed() -> bool {
    schtasks_exists()
}

#[cfg(windows)]
fn task_tr(exe: &Path, profile: &Path) -> String {
    format!(
        "\"{}\" --profile-dir \"{}\" __elevated-hotkey-helper",
        exe.display(),
        profile.display()
    )
}

/// Install the helper task (requires elevation - caller must ensure UAC).
#[cfg(windows)]
fn do_install(exe: &Path, profile: &Path) -> Result<(), String> {
    let tr = task_tr(exe, profile);
    let status = std::process::Command::new("schtasks")
        .arg("/create")
        .arg("/tn")
        .arg(TASK_NAME)
        .arg("/tr")
        .arg(&tr)
        .arg("/sc")
        .arg("ONLOGON")
        .arg("/rl")
        .arg("HIGHEST")
        .arg("/f")
        .output()
        .map_err(|e| format!("schtasks create failed: {e}"))?;
    if status.status.success() {
        Ok(())
    } else {
        Err(format!(
            "schtasks create refused: {}",
            String::from_utf8_lossy(&status.stderr)
        ))
    }
}

/// Install via elevated `runas` prompt (one-time UAC).
pub fn install_via_uac(exe: &Path, profile: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        if !crate::helper::protected_path_ok(exe, &crate::sys::program_files_dir()) {
            return Err("elevation refused: install to Program Files first".to_string());
        }
        let tr = task_tr(exe, profile);
        let params = format!("/create /tn {TASK_NAME} /tr \"{tr}\" /sc ONLOGON /rl HIGHEST /f");
        elevate_schtasks(&params)
    }
    #[cfg(not(windows))]
    {
        let _ = (exe, profile);
        Err("elevated task needs Windows".to_string())
    }
}

#[cfg(windows)]
fn elevate_schtasks(params: &str) -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;
    use windows_sys::Win32::Foundation::CloseHandle;
    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
    let verb = to_wide("runas");
    let file = to_wide("schtasks.exe");
    let pars = to_wide(params);
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = pars.as_ptr();
    info.nShow = SW_HIDE;
    let ok = unsafe { ShellExecuteExW(&mut info) };
    if ok == 0 {
        return Err(format!("ShellExecuteEx(runas) refused: {}", crate::sys::last_error()));
    }
    if !info.hProcess.is_null() {
        unsafe { CloseHandle(info.hProcess) };
    }
    Ok(())
}

/// Remove the helper task (no elevation needed for delete of user task).
pub fn uninstall() -> Result<(), String> {
    let status = std::process::Command::new("schtasks")
        .arg("/delete")
        .arg("/tn")
        .arg(TASK_NAME)
        .arg("/f")
        .output()
        .map_err(|e| format!("schtasks delete failed: {e}"))?;
    if status.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&status.stderr).into_owned())
    }
}

/// Show Yes/No consent dialog for admin-window support.
/// Returns true on Yes (on Windows), false on No/cancel/elsewhere.
#[cfg(windows)]
pub fn prompt_consent() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_YESNO, MB_ICONQUESTION};
    fn w(s: &str) -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() }
    let text = w("Enable clipboard in admin windows?\n\nYes = one-time UAC to install helper (then silent at logon).\nNo = ask again next restart.");
    let cap = w("Clipmonger");
    let ret = unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), cap.as_ptr(), MB_YESNO | MB_ICONQUESTION) };
    ret == 6 // IDYES
}
#[cfg(not(windows))]
pub fn prompt_consent() -> bool { false }


#[cfg(windows)]
pub fn notify_popup() -> Result<(), String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, WM_HOTKEY};
    fn w(s: &str) -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() }
    let cls = w("ClipmongerFallback");
    let hwnd = unsafe { FindWindowW(cls.as_ptr(), std::ptr::null()) };
    if hwnd.is_null() { return Err("popup not found".to_string()); }
    let ok = unsafe { PostMessageW(hwnd, WM_HOTKEY, 1, 0) };
    if ok==0 { Err("PostMessage failed".to_string()) } else { Ok(()) }
}
#[cfg(not(windows))]
pub fn notify_popup() -> Result<(), String> { Err("needs Windows".to_string()) }
