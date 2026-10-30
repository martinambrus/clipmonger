//! Scheduled-task installer for the elevated helper (one-time UAC).
//!
//! Installed (Program Files) builds only: portable mode never touches the
//! Task Scheduler. Creation uses `schtasks.exe /create /rl HIGHEST` via a
//! `runas` `ShellExecute` so the single UAC prompt comes from Windows, not
//! from this code.

use std::path::Path;

const TASK_NAME: &str = "ClipmongerHelper";

/// `schtasks.exe` command that never opens a console window (the daemon is
/// windowless, so a console child would otherwise get a window of its own).
#[cfg(windows)]
fn schtasks() -> std::process::Command {
    use std::os::windows::process::CommandExt as _;
    /// `CREATE_NO_WINDOW` process creation flag.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut command = std::process::Command::new("schtasks");
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

#[cfg(windows)]
fn schtasks_exists() -> bool {
    schtasks()
        .arg("/query")
        .arg("/tn")
        .arg(TASK_NAME)
        .arg("/nh")
        .output()
        .is_ok_and(|o| o.status.success())
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
        "\"{}\" --profile-dir \"{}\" __elevated-persistent",
        exe.display(),
        profile.display()
    )
}

/// Install via elevated `runas` prompt (one-time UAC); returns once the
/// elevated `schtasks` has finished.
///
/// # Errors
///
/// Returns a message when the UAC prompt is cancelled, the launch is
/// blocked, or `schtasks` fails to create the task.
pub fn install_via_uac(exe: &Path, profile: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
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

/// Start the installed helper task now. Needs no UAC: the task carries the
/// elevation the user consented to when it was created.
///
/// # Errors
///
/// Returns a message when `schtasks` cannot run or refuses the task.
pub fn run_task() -> Result<(), String> {
    #[cfg(windows)]
    {
        let output = schtasks()
            .arg("/run")
            .arg("/tn")
            .arg(TASK_NAME)
            .output()
            .map_err(|e| format!("schtasks run failed: {e}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "schtasks run refused: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }
    #[cfg(not(windows))]
    {
        Err("elevated task needs Windows".to_string())
    }
}

#[cfg(windows)]
fn elevate_schtasks(params: &str) -> Result<(), String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;
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
        return Err(format!(
            "ShellExecuteEx(runas) refused: {}",
            crate::sys::last_error()
        ));
    }
    if info.hProcess.is_null() {
        return Ok(());
    }
    // Wait for the elevated schtasks so the caller can start the task next.
    let mut code: u32 = 1;
    unsafe {
        WaitForSingleObject(info.hProcess, 30_000);
        GetExitCodeProcess(info.hProcess, &raw mut code);
        CloseHandle(info.hProcess);
    }
    if code == 0 {
        Ok(())
    } else {
        Err(format!("schtasks create exited with code {code}"))
    }
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

/// Show the Yes/No consent dialog for admin-window support, worded for
/// installed (logon task) or portable (until exit, nothing installed) mode.
/// Returns true on Yes (on Windows), false on No/cancel/elsewhere.
#[cfg(windows)]
#[must_use]
pub fn prompt_consent(installed: bool) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_DEFBUTTON2, MB_ICONQUESTION, MB_SETFOREGROUND, MB_SYSTEMMODAL, MB_TOPMOST,
        MB_YESNO,
    };
    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
    let text = if installed {
        w("Enable Clipmonger in admin windows?\n\nYes = one-time Windows UAC prompt to install the admin helper; it then starts silently at logon.\nNo = ask again next time Clipmonger starts.")
    } else {
        w("Enable Clipmonger in admin windows?\n\nYes = Windows UAC prompt to start the admin helper until Clipmonger exits (portable mode: nothing is installed).\nNo = ask again next time Clipmonger starts.")
    };
    let cap = w("Clipmonger");
    let ret = unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            cap.as_ptr(),
            // "No" is the default: a stray Enter must not start elevation.
            MB_YESNO
                | MB_DEFBUTTON2
                | MB_ICONQUESTION
                | MB_SYSTEMMODAL
                | MB_SETFOREGROUND
                | MB_TOPMOST,
        )
    };
    ret == 6 // IDYES
}
#[cfg(not(windows))]
#[must_use]
pub fn prompt_consent(installed: bool) -> bool {
    let _ = installed;
    false
}
