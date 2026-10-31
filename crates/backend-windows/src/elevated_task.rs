//! Scheduled-task installer for the elevated helper (one-time UAC).
//!
//! Installed (Program Files) builds only: portable mode never touches the
//! Task Scheduler. The task is registered from XML by an elevated (`runas`)
//! `schtasks.exe`, so the single UAC prompt comes from Windows, not from
//! this code. Every task created here carries an ASCII identity marker
//! (hash of binary and arguments) in its description, so matching never
//! depends on the console code page of `schtasks` output.

use std::path::Path;

#[cfg(windows)]
const TASK_NAME: &str = "ClipmongerHelper";
/// Start of the identity marker in the task description.
const TASK_MARKER_PREFIX: &str = "(clipmonger-task ";

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

/// The registered helper task as XML, or `None` when there is none.
#[cfg(windows)]
fn task_xml() -> Option<String> {
    let output = schtasks()
        .args(["/query", "/tn", TASK_NAME, "/xml"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Action arguments of the helper task for `profile`.
#[cfg_attr(not(windows), allow(dead_code))]
fn task_arguments(profile: &Path) -> String {
    format!(
        "--profile-dir {} __elevated-persistent",
        crate::helper::quote_arg(&profile.to_string_lossy())
    )
}

/// Identity marker of the task that runs `exe` for `profile`: FNV-1a of
/// the lowercase binary path and the exact arguments, as ASCII hex.
#[cfg_attr(not(windows), allow(dead_code))]
fn task_marker(exe: &Path, profile: &Path) -> String {
    let source = format!(
        "{}\n{}",
        exe.to_string_lossy().to_lowercase(),
        task_arguments(profile)
    );
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in source.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{TASK_MARKER_PREFIX}{hash:016x})")
}

/// Escape text for an XML element.
#[cfg_attr(not(windows), allow(dead_code))]
fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Task definition: start the helper at this user's logon with highest
/// privileges; it is a long-lived idle process, so it is never stopped for
/// battery power or by a time limit, and it runs at normal priority so
/// pastes stay instant.
#[cfg_attr(not(windows), allow(dead_code))]
fn task_definition(exe: &Path, profile: &Path) -> String {
    let user = match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
        (Ok(domain), Ok(name)) => {
            format!(
                "<UserId>{}</UserId>",
                xml_escape(&format!("{domain}\\{name}"))
            )
        }
        (_, Ok(name)) => format!("<UserId>{}</UserId>", xml_escape(&name)),
        _ => String::new(),
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Clipmonger admin-window paste helper {marker}</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      {user}
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      {user}
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>5</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{command}</Command>
      <Arguments>{arguments}</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
        marker = task_marker(exe, profile),
        command = xml_escape(&exe.to_string_lossy()),
        arguments = xml_escape(&task_arguments(profile)),
    )
}

/// Whether the helper task exists and was created here for exactly `exe`
/// with `profile` (identity marker match).
#[must_use]
pub fn task_matches(exe: &Path, profile: &Path) -> bool {
    #[cfg(windows)]
    {
        task_xml().is_some_and(|xml| xml.contains(&task_marker(exe, profile)))
    }
    #[cfg(not(windows))]
    {
        let _ = (exe, profile);
        false
    }
}

/// Delete a helper task that was not created here (no identity marker):
/// older builds registered tasks that may run a user-writable binary, which
/// lets any same-user process run code elevated without UAC. Deleting needs
/// elevation, so the elevated helper calls this when it starts.
pub fn remove_legacy_task() {
    #[cfg(windows)]
    if task_xml().is_some_and(|xml| !xml.contains(TASK_MARKER_PREFIX)) {
        let _ = uninstall();
    }
}

/// Install via elevated `runas` prompt (one-time UAC); returns once the
/// elevated `schtasks` has finished. Replaces any task of the same name.
///
/// # Errors
///
/// Returns a message when the task file cannot be written, the UAC prompt
/// is cancelled, the launch is blocked, or `schtasks` rejects the task.
pub fn install_via_uac(exe: &Path, profile: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        let path =
            std::env::temp_dir().join(format!("clipmonger-helper-task-{}.xml", std::process::id()));
        // Task Scheduler reads the file as declared: UTF-16LE with a BOM.
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend(
            task_definition(exe, profile)
                .encode_utf16()
                .flat_map(u16::to_le_bytes),
        );
        std::fs::write(&path, bytes).map_err(|e| format!("task file write failed: {e}"))?;
        let params = format!(
            "/create /tn {TASK_NAME} /xml {} /f",
            crate::helper::quote_arg(&path.to_string_lossy())
        );
        let created = elevate_schtasks(&params);
        let _ = std::fs::remove_file(&path);
        created
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
    info.cbSize = u32::try_from(std::mem::size_of::<SHELLEXECUTEINFOW>())
        .expect("SHELLEXECUTEINFOW size fits in u32");
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = pars.as_ptr();
    info.nShow = SW_HIDE;
    let ok = unsafe { ShellExecuteExW(&raw mut info) };
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

/// Remove the helper task. The task is created elevated, so deleting it
/// needs an elevated caller too.
///
/// # Errors
///
/// Returns the `schtasks` message when the task cannot be deleted.
pub fn uninstall() -> Result<(), String> {
    #[cfg(windows)]
    {
        let output = schtasks()
            .args(["/delete", "/tn", TASK_NAME, "/f"])
            .output()
            .map_err(|e| format!("schtasks delete failed: {e}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).into_owned())
        }
    }
    #[cfg(not(windows))]
    {
        Err("elevated task needs Windows".to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = r"C:\Program Files\Clipmonger\clipmonger-daemon.exe";
    const PROFILE: &str = r"C:\Users\A B\AppData\Roaming\clipmonger";

    #[test]
    fn task_arguments_quote_paths_with_spaces() {
        assert_eq!(
            task_arguments(Path::new(PROFILE)),
            r#"--profile-dir "C:\Users\A B\AppData\Roaming\clipmonger" __elevated-persistent"#
        );
    }

    #[test]
    fn task_definition_keeps_the_helper_running() {
        let xml = task_definition(Path::new(EXE), Path::new(PROFILE));
        for needle in [
            "<Command>C:\\Program Files\\Clipmonger\\clipmonger-daemon.exe</Command>",
            "<Arguments>--profile-dir &quot;C:\\Users\\A B\\AppData\\Roaming\\clipmonger&quot; __elevated-persistent</Arguments>",
            "<RunLevel>HighestAvailable</RunLevel>",
            "<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>",
            "<StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>",
            "<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>",
            "<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>",
        ] {
            assert!(xml.contains(needle), "missing {needle} in\n{xml}");
        }
        assert!(xml.contains(&task_marker(Path::new(EXE), Path::new(PROFILE))));
    }

    #[test]
    fn task_marker_tracks_binary_and_profile() {
        let marker = task_marker(Path::new(EXE), Path::new(PROFILE));
        assert!(marker.starts_with(TASK_MARKER_PREFIX) && marker.ends_with(')'));
        assert!(
            marker.is_ascii(),
            "matched against code-page console output"
        );
        assert_eq!(
            marker,
            task_marker(Path::new(&EXE.to_uppercase()), Path::new(PROFILE)),
            "binary path compares case-insensitively"
        );
        assert_ne!(
            marker,
            task_marker(Path::new(EXE), Path::new(r"C:\Users\other\clipmonger"))
        );
    }
}
