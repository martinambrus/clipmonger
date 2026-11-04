//! Fixed persistent-helper registration and headless security controls.
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub(crate) fn path_text(path: &Path) -> Result<&str, String> {
    let value = path.to_str().ok_or("path is not valid Unicode")?;
    if value.is_empty() || value.contains(['\0', '"']) {
        return Err("unsupported path".into());
    }
    Ok(value)
}

pub(crate) fn valid_profile(value: &str) -> bool {
    let b = value.as_bytes();
    b.len() >= 3
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && b[2] == b'\\'
        && !value.chars().any(char::is_control)
        && !value.contains(['\0', '"', '/', '<', '>', '|', '*', '?'])
        && !value[3..].contains(':')
        && !value
            .split('\\')
            .any(|part| part == "." || part == ".." || part.ends_with([' ', '.']))
}

pub(crate) fn valid_sid(value: &str) -> bool {
    let parts: Vec<_> = value.split('-').collect();
    if !(4..=18).contains(&parts.len()) || parts[0] != "S" || parts[1] != "1" {
        return false;
    }
    parts[2..].iter().enumerate().all(|(i, part)| {
        !part.is_empty()
            && (part.len() == 1 || !part.starts_with('0'))
            && part.bytes().all(|b| b.is_ascii_digit())
            && part.parse::<u64>().is_ok_and(|n| {
                n <= if i == 0 {
                    (1u64 << 48) - 1
                } else {
                    u64::from(u32::MAX)
                }
            })
    })
}

fn task_arguments(profile: &Path) -> Result<String, String> {
    let profile = path_text(profile)?;
    if !valid_profile(profile) {
        return Err("unsupported absolute Windows profile path".into());
    }
    Ok(format!(
        "--profile-dir {} __elevated-persistent",
        crate::helper::quote_arg(profile)
    ))
}

#[cfg(any(windows, test))]
fn task_marker(exe: &Path, profile: &Path) -> Result<String, String> {
    let source = format!(
        "{}\n{}",
        path_text(exe)?.to_lowercase(),
        task_arguments(profile)?
    );
    Ok(format!(
        "(clipmonger-task {:016x})",
        clipmonger_core::hash::fnv1a64(source.as_bytes())
    ))
}

#[cfg(any(windows, test))]
fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(any(windows, test))]
pub(crate) fn task_definition(exe: &Path, profile: &Path, sid: &str) -> Result<String, String> {
    let command = path_text(exe)?;
    let arguments = task_arguments(profile)?;
    if !valid_sid(sid) {
        return Err("invalid SID".into());
    }
    let user = if sid.is_empty() {
        String::new()
    } else {
        format!("<UserId>{}</UserId>", xml_escape(sid))
    };
    Ok(format!(
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
        marker = task_marker(exe, profile)?,
        command = xml_escape(command),
        arguments = xml_escape(&arguments),
    ))
}

#[cfg(any(windows, test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskAction {
    pub command: String,
    pub arguments: String,
    pub working_directory: String,
    pub count: i32,
    pub is_exec: bool,
}
#[cfg(any(windows, test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskPrincipal {
    pub sid: String,
    pub interactive: bool,
    pub highest: bool,
}
#[cfg(any(windows, test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskTrigger {
    pub count: i32,
    pub sid: String,
    pub is_logon: bool,
    pub enabled: bool,
}
#[cfg(any(windows, test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskSettings {
    pub enabled: bool,
    pub disallow_battery: bool,
    pub stop_battery: bool,
    pub execution_time_limit: String,
    pub multiple_instances: i32,
    pub priority: i32,
}
#[cfg(any(windows, test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskSnapshot {
    pub action: TaskAction,
    pub principal: TaskPrincipal,
    pub trigger: TaskTrigger,
    pub settings: TaskSettings,
    pub descriptor_protected: bool,
    pub description: String,
}

#[cfg(any(windows, test))]
pub(crate) fn expected_snapshot(
    exe: &Path,
    profile: &Path,
    sid: &str,
) -> Result<TaskSnapshot, String> {
    Ok(TaskSnapshot {
        action: TaskAction {
            command: path_text(exe)?.into(),
            arguments: task_arguments(profile)?,
            working_directory: String::new(),
            count: 1,
            is_exec: true,
        },
        principal: TaskPrincipal {
            sid: sid.into(),
            interactive: true,
            highest: true,
        },
        trigger: TaskTrigger {
            count: 1,
            sid: sid.into(),
            is_logon: true,
            enabled: true,
        },
        settings: TaskSettings {
            enabled: true,
            disallow_battery: false,
            stop_battery: false,
            execution_time_limit: "PT0S".into(),
            multiple_instances: 2,
            priority: 5,
        },
        descriptor_protected: true,
        description: task_marker(exe, profile)?,
    })
}

#[cfg(any(windows, test))]
pub(crate) fn snapshot_matches(expected: &TaskSnapshot, actual: &TaskSnapshot) -> bool {
    let mut actual = actual.clone();
    actual.description.clone_from(&expected.description);
    // Windows executable paths compare without ASCII case; profile arguments do not.
    if !actual
        .action
        .command
        .eq_ignore_ascii_case(&expected.action.command)
    {
        return false;
    }
    actual.action.command.clone_from(&expected.action.command);
    actual == *expected
}

#[cfg(any(windows, test))]
pub(crate) fn verify_component_path(
    path: &Path,
    final_path: &Path,
    reparse: bool,
) -> Result<(), String> {
    if reparse {
        return Err("reparse point in installed path".into());
    }
    if !path_text(path)?.eq_ignore_ascii_case(path_text(final_path)?) {
        return Err("installed final path changed".into());
    }
    Ok(())
}

// Conservative writer policy supplements AccessCheck: a deliberately restricted
// requester token must not hide write grants available to another medium process.
#[cfg(any(windows, test))]
pub(crate) fn installed_acl_writers_trusted(
    owner: &str,
    grants: &[(String, u32)],
    dangerous: u32,
) -> bool {
    fn trusted(sid: &str) -> bool {
        matches!(
            sid,
            "S-1-5-18"
                | "S-1-5-32-544"
                | "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"
        )
    }
    trusted(owner)
        && grants
            .iter()
            .all(|(sid, mask)| mask & dangerous == 0 || trusted(sid))
}

/// Registrar failure classification, returned through the retained process exit code.
#[derive(Debug)]
pub enum RegistrarError {
    /// UAC used an account other than the original requester.
    AlternateAccount,
    /// Other registration failure.
    Other(String),
}

impl From<String> for RegistrarError {
    fn from(value: String) -> Self {
        Self::Other(value)
    }
}

impl std::fmt::Display for RegistrarError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlternateAccount => f.write_str("alternate-account UAC is unsupported; consent must use the original signed-in account"),
            Self::Other(detail) => f.write_str(detail),
        }
    }
}

/// Encode a registrar result as a small, static process exit classification.
#[must_use]
pub fn registrar_exit_code(result: &Result<(), RegistrarError>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(RegistrarError::AlternateAccount) => 102,
        Err(RegistrarError::Other(_)) => 101,
    }
}

#[cfg(any(windows, test))]
pub(crate) fn verify_registrar_identity(
    wire_sid: &str,
    requester: &str,
    registrar: &str,
) -> Result<(), RegistrarError> {
    if !valid_sid(wire_sid) || wire_sid != requester {
        return Err(RegistrarError::Other(
            "original requester SID does not match request".into(),
        ));
    }
    if registrar != requester {
        return Err(RegistrarError::AlternateAccount);
    }
    Ok(())
}

/// Parsed registrar input, accepted only by the narrow privileged entry point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallTaskArgs {
    pub profile_dir: PathBuf,
    pub requester_pid: u32,
    pub requester_sid: String,
}

/// Recognize the registrar before the daemon opens any profile or store.
/// Invalid Unicode is rejected without the lossy conversion used by normal UI arguments.
#[must_use]
pub fn parse_registrar_invocation(args: &[OsString]) -> Option<Result<InstallTaskArgs, String>> {
    if !args.iter().any(|a| a == "__elevated-install-task") {
        return None;
    }
    Some((|| {
        let args: Vec<_> = args
            .iter()
            .map(|a| a.to_str().ok_or("non-Unicode registrar argument"))
            .collect::<Result<_, _>>()?;
        if args.len() != 7 || args[0] != "__elevated-install-task" {
            return Err("registrar requires exactly one mode and three fields".into());
        }
        let mut profile = None;
        let mut pid = None;
        let mut sid = None;
        for pair in args[1..].as_chunks::<2>().0 {
            if pair[1].contains('\0') {
                return Err("NUL in registrar argument".into());
            }
            match pair[0] {
                "--profile-dir" if profile.is_none() && valid_profile(pair[1]) => {
                    profile = Some(PathBuf::from(pair[1]));
                }
                "--requester-pid" if pid.is_none() => {
                    if !pair[1].bytes().all(|b| b.is_ascii_digit()) {
                        return Err("invalid requester PID".into());
                    }
                    let parsed = pair[1]
                        .parse::<u32>()
                        .map_err(|_| "invalid requester PID")?;
                    if parsed == 0 {
                        return Err("invalid requester PID".into());
                    }
                    pid = Some(parsed);
                }
                "--requester-sid" if sid.is_none() && valid_sid(pair[1]) => {
                    sid = Some(pair[1].to_owned());
                }
                _ => return Err("unsupported or repeated registrar field".into()),
            }
        }
        Ok(InstallTaskArgs {
            profile_dir: profile.ok_or("missing profile")?,
            requester_pid: pid.ok_or("missing requester PID")?,
            requester_sid: sid.ok_or("missing requester SID")?,
        })
    })())
}

#[cfg(any(windows, test))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct LaunchRequest {
    executable: PathBuf,
    arguments: String,
}

#[cfg(any(windows, test))]
fn registrar_launch(
    exe: &Path,
    profile: &Path,
    sid: &str,
    pid: u32,
) -> Result<LaunchRequest, String> {
    path_text(exe)?;
    let profile = path_text(profile)?;
    if !valid_profile(profile) || !valid_sid(sid) || pid == 0 {
        return Err("invalid registrar input".into());
    }
    Ok(LaunchRequest {
        executable: exe.into(),
        arguments: format!(
            "__elevated-install-task --profile-dir {} --requester-pid {pid} --requester-sid {sid}",
            crate::helper::quote_arg(profile)
        ),
    })
}

#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy)]
pub(crate) enum RegistrarExit {
    LaunchFailed,
    Cancelled,
    MissingHandle,
    WaitFailed,
    Timeout,
    ExitQueryFailed,
    Exited(u32),
}

#[cfg(any(windows, test))]
fn checked_registrar_exit(exit: RegistrarExit) -> Result<(), String> {
    match exit {
        RegistrarExit::Exited(0) => Ok(()),
        RegistrarExit::Exited(102) => Err(RegistrarError::AlternateAccount.to_string()),
        other => Err(format!("helper registrar failed: {other:?}")),
    }
}

#[cfg(any(windows, test))]
trait InstallPlatform {
    type Guard;
    fn trust_current(&mut self) -> Result<(PathBuf, String, Self::Guard), String>;
    fn launch(&mut self, request: &LaunchRequest) -> RegistrarExit;
    fn readback(&mut self, sid: &str) -> Result<TaskSnapshot, String>;
    fn start(&mut self) -> Result<(), String>;
}

#[cfg(any(windows, test))]
fn install_with<P: InstallPlatform>(
    platform: &mut P,
    profile: &Path,
    pid: u32,
) -> Result<(), String> {
    let (exe, sid, _pin) = platform.trust_current()?;
    let request = registrar_launch(&exe, profile, &sid, pid)?;
    checked_registrar_exit(platform.launch(&request))?;
    let expected = expected_snapshot(&exe, profile, &sid)?;
    if !snapshot_matches(&expected, &platform.readback(&sid)?) {
        return Err("registered helper readback mismatch".into());
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn install_and_start<P: InstallPlatform>(
    platform: &mut P,
    profile: &Path,
    pid: u32,
) -> Result<(), String> {
    install_with(platform, profile, pid)?;
    platform.start()
}

/// Register and start only the current protected executable; the profile is never opened here.
/// # Errors
/// Consent launch, trust, process completion, or task readback failed.
pub fn install_via_uac(_exe: &Path, profile: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        install_and_start(&mut NativePlatform, profile, std::process::id())
    }
    #[cfg(not(windows))]
    {
        let _ = profile;
        Err("elevated task needs Windows".into())
    }
}

#[cfg(windows)]
struct NativePlatform;
#[cfg(windows)]
impl InstallPlatform for NativePlatform {
    type Guard = crate::sys::task::TrustedInstall;
    fn trust_current(&mut self) -> Result<(PathBuf, String, Self::Guard), String> {
        let trust = crate::sys::task::trust_current()?;
        Ok((trust.path.clone(), trust.sid.clone(), trust))
    }
    fn launch(&mut self, request: &LaunchRequest) -> RegistrarExit {
        crate::sys::task::launch(&request.executable, &request.arguments)
    }
    fn readback(&mut self, sid: &str) -> Result<TaskSnapshot, String> {
        crate::sys::task::read_snapshot(sid)
    }
    fn start(&mut self) -> Result<(), String> {
        crate::sys::task::run()
    }
}

/// Execute the already strictly parsed registrar request before normal daemon startup.
/// # Errors
/// The request, account, installation, scheduler, or security descriptor is invalid.
pub fn register_from_request(args: &InstallTaskArgs) -> Result<(), RegistrarError> {
    task_arguments(&args.profile_dir)?;
    if !valid_sid(&args.requester_sid) || args.requester_pid == 0 {
        return Err(RegistrarError::Other("invalid registrar identity".into()));
    }
    #[cfg(windows)]
    {
        crate::sys::task::register(args)
    }
    #[cfg(not(windows))]
    {
        Err(RegistrarError::Other("elevated task needs Windows".into()))
    }
}

#[must_use]
pub fn task_matches(_exe: &Path, profile: &Path) -> bool {
    #[cfg(windows)]
    {
        let Ok(trust) = crate::sys::task::trust_current() else {
            return false;
        };
        let Ok(expected) = expected_snapshot(&trust.path, profile, &trust.sid) else {
            return false;
        };
        crate::sys::task::read_snapshot(&trust.sid)
            .is_ok_and(|actual| snapshot_matches(&expected, &actual))
    }
    #[cfg(not(windows))]
    {
        let _ = profile;
        false
    }
}

// Inverse of quote_arg for this one fixed argument, with a roundtrip to reject appendages.
#[cfg(any(windows, test))]
fn profile_from_arguments(arguments: &str) -> Option<PathBuf> {
    let quoted = arguments
        .strip_prefix("--profile-dir ")?
        .strip_suffix(" __elevated-persistent")?;
    let body = quoted.strip_prefix('"')?.strip_suffix('"')?;
    let trailing = body.chars().rev().take_while(|c| *c == '\\').count();
    if trailing % 2 != 0 {
        return None;
    }
    let profile = format!(
        "{}{}",
        &body[..body.len() - trailing],
        "\\".repeat(trailing / 2)
    );
    if !valid_profile(&profile) {
        return None;
    }
    let path = PathBuf::from(profile);
    (task_arguments(&path).ok()? == arguments).then_some(path)
}

#[cfg(any(windows, test))]
fn legacy_is_safe(actual: &TaskSnapshot, protected: bool) -> bool {
    if !protected || !valid_sid(&actual.principal.sid) {
        return false;
    }
    let Some(profile) = profile_from_arguments(&actual.action.arguments) else {
        return false;
    };
    expected_snapshot(
        Path::new(&actual.action.command),
        &profile,
        &actual.principal.sid,
    )
    .is_ok_and(|expected| snapshot_matches(&expected, actual))
}

/// Unknown readback is reported separately and never treated as a safe task.
pub fn remove_legacy_task() {
    #[cfg(windows)]
    if let Err(error) = crate::sys::task::cleanup(legacy_is_safe) {
        eprintln!("helper task cleanup failed: {error}");
    }
}

/// # Errors
/// The scheduler cannot start the helper task.
pub fn run_task() -> Result<(), String> {
    #[cfg(windows)]
    {
        crate::sys::task::run()
    }
    #[cfg(not(windows))]
    {
        Err("elevated task needs Windows".into())
    }
}

/// # Errors
/// The scheduler cannot delete the helper task.
pub fn uninstall() -> Result<(), String> {
    #[cfg(windows)]
    {
        crate::sys::task::delete()
    }
    #[cfg(not(windows))]
    {
        Err("elevated task needs Windows".into())
    }
}

#[must_use]
pub fn prompt_consent(installed: bool) -> bool {
    #[cfg(windows)]
    {
        crate::sys::task::prompt(installed)
    }
    #[cfg(not(windows))]
    {
        let _ = installed;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const EXE: &str = r"C:\Program Files\Clipmonger\clipmonger-daemon.exe";
    const PROFILE: &str = r"C:\Users\A B\AppData\Roaming\clipmonger";
    const SID: &str = "S-1-5-21-111-222-333-1001";

    fn invocation() -> Vec<OsString> {
        [
            "__elevated-install-task",
            "--profile-dir",
            PROFILE,
            "--requester-pid",
            "123",
            "--requester-sid",
            SID,
        ]
        .into_iter()
        .map(OsString::from)
        .collect()
    }
    fn expected() -> TaskSnapshot {
        expected_snapshot(Path::new(EXE), Path::new(PROFILE), SID).unwrap()
    }

    #[test]
    fn strict_registrar_parser_requires_exact_mode_and_fields() {
        let args = invocation();
        assert_eq!(
            parse_registrar_invocation(&args)
                .unwrap()
                .unwrap()
                .requester_pid,
            123
        );
        assert!(parse_registrar_invocation(&[]).is_none());
        for (index, invalid) in [
            (0, "__elevated-worker"),
            (1, "--exe"),
            (2, "relative"),
            (2, "C:\\a\\..\\b"),
            (2, "C:\\a\0b"),
            (2, r"\\server\profile"),
            (3, "--profile-dir"),
            (4, "0"),
            (4, "+123"),
            (4, "4294967296"),
            (5, "--xml"),
            (6, "S-1-bad"),
            (6, "S-1-5-1)S:bad"),
            (6, "S-1-5-4294967296"),
        ] {
            let mut invalid_args = args.clone();
            invalid_args[index] = invalid.into();
            if index == 0 {
                invalid_args.push("__elevated-install-task".into());
            }
            assert!(
                parse_registrar_invocation(&invalid_args).unwrap().is_err(),
                "accepted {index}={invalid:?}"
            );
        }
        for extra in [
            "__elevated-install-task",
            "__elevated-persistent",
            "__elevated-worker",
            "--profile-dir",
        ] {
            let mut invalid = args.clone();
            invalid.push(extra.into());
            assert!(parse_registrar_invocation(&invalid).unwrap().is_err());
        }
        let mut missing = args;
        missing.pop();
        assert!(parse_registrar_invocation(&missing).unwrap().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn strict_registrar_rejects_non_unicode_without_profile_access() {
        use std::os::unix::ffi::OsStringExt;
        let mut args = invocation();
        args[2] = OsString::from_vec(vec![0xff]);
        assert!(parse_registrar_invocation(&args).unwrap().is_err());
    }

    #[cfg(windows)]
    #[test]
    fn strict_registrar_rejects_unpaired_utf16_without_profile_access() {
        use std::os::windows::ffi::OsStringExt;
        let mut args = invocation();
        args[2] = OsString::from_wide(&[0xd800]);
        assert!(parse_registrar_invocation(&args).unwrap().is_err());
    }

    #[test]
    fn production_xml_is_fixed_unicode_sid_and_exact_quoted_action() {
        let profile = Path::new(r"C:\Users\É & B\clipboard\");
        let xml = task_definition(Path::new(EXE), profile, SID).unwrap();
        for needle in [
            "encoding=\"UTF-16\"",
            "<LogonType>InteractiveToken</LogonType>",
            "<RunLevel>HighestAvailable</RunLevel>",
            "<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>",
            "<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>",
            "<StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>",
            "<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>",
            "<Priority>5</Priority>",
            "É &amp; B",
        ] {
            assert!(xml.contains(needle), "missing {needle}");
        }
        assert_eq!(xml.matches(&format!("<UserId>{SID}</UserId>")).count(), 2);
        assert!(xml.contains(&format!("<Command>{EXE}</Command>")));
        assert!(xml.contains(&format!(
            "<Arguments>{}</Arguments>",
            xml_escape(&task_arguments(profile).unwrap())
        )));
        assert!(task_arguments(profile)
            .unwrap()
            .ends_with(r#"\\" __elevated-persistent"#));
        assert_eq!(
            profile_from_arguments(&task_arguments(profile).unwrap()).as_deref(),
            Some(profile)
        );
        assert_eq!(xml_escape("&<>\""), "&amp;&lt;&gt;&quot;");
        assert!(task_definition(Path::new(EXE), Path::new(PROFILE), "S-1-bad").is_err());
    }

    #[test]
    fn structured_readback_rejects_forged_marker_and_every_changed_contract() {
        let correct = expected();
        let mut cases = Vec::new();
        macro_rules! changed { ($($field:ident).+, $value:expr) => {{ let mut s = correct.clone(); s.$($field).+ = $value; cases.push((stringify!($($field).+), s)); }}; }
        changed!(action.command, r"C:\Temp\evil.exe".into());
        changed!(
            action.arguments,
            task_arguments(Path::new(r"C:\Users\other\profile")).unwrap()
        );
        changed!(
            action.arguments,
            format!("{} --extra", correct.action.arguments)
        );
        changed!(action.working_directory, r"C:\Temp".into());
        changed!(action.count, 2);
        changed!(action.is_exec, false);
        changed!(principal.sid, "S-1-5-21-999".into());
        changed!(principal.interactive, false);
        changed!(principal.highest, false);
        changed!(trigger.count, 2);
        changed!(trigger.sid, "S-1-5-21-999".into());
        changed!(trigger.is_logon, false);
        changed!(trigger.enabled, false);
        changed!(settings.enabled, false);
        changed!(settings.disallow_battery, true);
        changed!(settings.stop_battery, true);
        changed!(settings.execution_time_limit, "PT1H".into());
        changed!(settings.multiple_instances, 1);
        changed!(settings.priority, 7);
        changed!(descriptor_protected, false);
        for (field, actual) in cases {
            assert!(
                !snapshot_matches(&correct, &actual),
                "forged marker accepted changed {field}"
            );
        }
        let mut no_marker = correct.clone();
        no_marker.description.clear();
        assert!(snapshot_matches(&correct, &no_marker));
        no_marker.description = "arbitrary descriptive metadata".into();
        no_marker.action.command = EXE.to_uppercase();
        assert!(snapshot_matches(&correct, &no_marker));
    }

    #[test]
    fn legacy_cleanup_checks_action_and_keeps_protected_other_profile() {
        let other =
            expected_snapshot(Path::new(EXE), Path::new(r"C:\Users\other\profile"), SID).unwrap();
        assert!(legacy_is_safe(&other, true));
        assert!(!legacy_is_safe(&other, false));
        let mut marked_unsafe = expected();
        marked_unsafe.action.command = r"C:\Temp\evil.exe".into();
        assert!(!legacy_is_safe(&marked_unsafe, false));
        for args in [
            format!("{} --extra", other.action.arguments),
            "--profile-dir x __elevated-persistent".into(),
        ] {
            let mut malformed = other.clone();
            malformed.action.arguments = args;
            assert!(!legacy_is_safe(&malformed, true));
        }
        let mut invalid = other;
        invalid.principal.highest = false;
        assert!(!legacy_is_safe(&invalid, true));
    }

    #[test]
    fn registrar_identity_is_original_token_sid_and_same_account() {
        assert!(verify_registrar_identity(SID, SID, SID).is_ok());
        assert!(verify_registrar_identity("S-1-5-21-999", SID, SID).is_err());
        assert!(verify_registrar_identity(SID, SID, "S-1-5-21-999")
            .unwrap_err()
            .to_string()
            .contains("alternate-account"));
        assert_eq!(
            registrar_exit_code(&verify_registrar_identity(SID, SID, SID)),
            0
        );
        assert_eq!(
            registrar_exit_code(&verify_registrar_identity(SID, SID, "S-1-5-21-999")),
            102
        );
        assert_eq!(
            registrar_exit_code(&verify_registrar_identity("S-1-5-21-999", SID, SID)),
            101
        );
        assert!(verify_registrar_identity("S-1-bad", "S-1-bad", "S-1-bad").is_err());
    }

    #[test]
    fn installed_components_require_stable_non_reparse_paths() {
        let path = Path::new(EXE);
        assert!(verify_component_path(path, path, false).is_ok());
        assert!(verify_component_path(path, Path::new(&EXE.to_uppercase()), false).is_ok());
        assert!(verify_component_path(path, path, true).is_err());
        assert!(verify_component_path(path, Path::new(r"C:\Users\user\evil.exe"), false).is_err());
    }

    #[test]
    fn installed_acl_requires_trusted_owners_and_writers_even_for_restricted_requesters() {
        let system = "S-1-5-18";
        let admin = "S-1-5-32-544";
        let installer = "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";
        let dangerous = 0x000d_0156; // delete, owner/DACL, write/append/EA/attributes and delete-child
        let grants = vec![
            (system.into(), u32::MAX),
            (admin.into(), u32::MAX),
            (SID.into(), 0x0012_0089),
        ];
        for owner in [system, admin, installer] {
            assert!(installed_acl_writers_trusted(owner, &grants, dangerous));
        }
        assert!(!installed_acl_writers_trusted(SID, &grants, dangerous));
        for bit in [0x1_0000, 0x4_0000, 0x8_0000, 0x2, 0x4, 0x10, 0x40, 0x100] {
            let mut mutable = grants.clone();
            mutable.push((SID.into(), bit));
            assert!(
                !installed_acl_writers_trusted(system, &mutable, dangerous),
                "untrusted mutation grant {bit:x}"
            );
        }
        // Ancestors above the install directory can allow creation, but cannot
        // allow removal of the pinned existing child or resetting its permissions.
        assert!(installed_acl_writers_trusted(
            system,
            &[(SID.into(), 0x4)],
            0x000d_0040
        ));
        assert!(!installed_acl_writers_trusted(
            system,
            &[(SID.into(), 0x40)],
            0x000d_0040
        ));
    }

    struct FakePlatform {
        trust_ok: bool,
        exit: RegistrarExit,
        actual: Option<TaskSnapshot>,
        requests: Vec<LaunchRequest>,
        readbacks: usize,
        starts: usize,
        pinned: std::rc::Rc<std::cell::Cell<bool>>,
    }
    impl Default for FakePlatform {
        fn default() -> Self {
            Self {
                trust_ok: true,
                exit: RegistrarExit::Exited(0),
                actual: Some(expected()),
                requests: Vec::new(),
                readbacks: 0,
                starts: 0,
                pinned: std::rc::Rc::new(std::cell::Cell::new(false)),
            }
        }
    }
    struct PinProbe(std::rc::Rc<std::cell::Cell<bool>>);
    impl Drop for PinProbe {
        fn drop(&mut self) {
            self.0.set(false);
        }
    }
    impl InstallPlatform for FakePlatform {
        type Guard = PinProbe;
        fn trust_current(&mut self) -> Result<(PathBuf, String, Self::Guard), String> {
            if !self.trust_ok {
                return Err("pin/ACL/identity refused".into());
            }
            self.pinned.set(true);
            Ok((EXE.into(), SID.into(), PinProbe(self.pinned.clone())))
        }
        fn launch(&mut self, request: &LaunchRequest) -> RegistrarExit {
            assert!(self.pinned.get(), "binary must stay pinned through launch");
            self.requests.push(request.clone());
            self.exit
        }
        fn readback(&mut self, _: &str) -> Result<TaskSnapshot, String> {
            assert!(
                self.pinned.get(),
                "binary must stay pinned through readback"
            );
            self.readbacks += 1;
            self.actual.clone().ok_or("query failure".into())
        }
        fn start(&mut self) -> Result<(), String> {
            self.starts += 1;
            Ok(())
        }
    }

    #[test]
    fn production_launch_has_no_cross_uac_file_or_arbitrary_action() {
        let mut platform = FakePlatform::default();
        install_and_start(&mut platform, Path::new(PROFILE), 123).unwrap();
        assert_eq!(platform.starts, 1);
        assert!(
            !platform.pinned.get(),
            "pin released after completed operation"
        );
        assert_eq!(platform.requests.len(), 1);
        let request = &platform.requests[0];
        assert_eq!(request.executable, Path::new(EXE));
        assert!(!request.arguments.contains("/xml"));
        assert_eq!(request.arguments, format!("__elevated-install-task --profile-dir {} --requester-pid 123 --requester-sid {SID}", crate::helper::quote_arg(PROFILE)));
    }

    #[test]
    fn alternate_account_exit_explains_refusal_and_stops_install() {
        let mut platform = FakePlatform {
            exit: RegistrarExit::Exited(102),
            ..FakePlatform::default()
        };
        let error = install_and_start(&mut platform, Path::new(PROFILE), 123).unwrap_err();
        assert!(
            error.contains("alternate-account UAC is unsupported"),
            "{error}"
        );
        assert_eq!(platform.readbacks, 0);
        assert_eq!(platform.starts, 0);
    }

    #[test]
    fn all_registration_failures_stop_before_task_start() {
        for exit in [
            RegistrarExit::LaunchFailed,
            RegistrarExit::Cancelled,
            RegistrarExit::MissingHandle,
            RegistrarExit::WaitFailed,
            RegistrarExit::Timeout,
            RegistrarExit::ExitQueryFailed,
            RegistrarExit::Exited(1),
            RegistrarExit::Exited(259),
        ] {
            let mut platform = FakePlatform {
                exit,
                ..FakePlatform::default()
            };
            assert!(install_and_start(&mut platform, Path::new(PROFILE), 123).is_err());
            assert_eq!(platform.starts, 0);
            assert_eq!(platform.readbacks, 0);
        }
        for actual in [
            None,
            Some({
                let mut s = expected();
                s.action.command = r"C:\Temp\evil.exe".into();
                s
            }),
        ] {
            let mut platform = FakePlatform {
                actual,
                ..FakePlatform::default()
            };
            assert!(install_and_start(&mut platform, Path::new(PROFILE), 123).is_err());
            assert_eq!(platform.starts, 0);
        }
        let mut platform = FakePlatform {
            trust_ok: false,
            ..FakePlatform::default()
        };
        assert!(install_and_start(&mut platform, Path::new(PROFILE), 123).is_err());
        assert_eq!(platform.requests, []);
        assert_eq!(platform.starts, 0);
    }
}
