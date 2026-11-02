//! Autostart pure logic (pure).

use std::path::Path;

use clipmonger_platform::types::{Error, Result};

/// Decode a StartupApproved-style override blob (VERIFY on native Windows).
#[must_use]
pub fn startup_approved_enabled(bytes: &[u8]) -> Option<bool> {
    if bytes.len() < 12 {
        return None;
    }
    match bytes[0] {
        0x02 => Some(true),
        0x03 => Some(false),
        _ => None,
    }
}

/// Build the Run-value command line for `profile_exe`.
#[must_use]
pub fn run_value(profile_exe: &Path, minimized: bool) -> String {
    if minimized {
        format!("\"{}\" --minimized", profile_exe.display())
    } else {
        format!("\"{}\"", profile_exe.display())
    }
}

fn windows_path_str(path: &Path) -> Result<&str> {
    let s = path
        .to_str()
        .ok_or_else(|| Error::Unavailable("autostart path is not Unicode".to_string()))?;
    let bytes = s.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/');
    let unc = if s.starts_with("\\\\") || s.starts_with("//") {
        let mut parts = s[2..].split(['\\', '/']);
        matches!((parts.next(), parts.next()), (Some(server), Some(share)) if !server.is_empty() && server != "." && server != ".." && !share.is_empty())
    } else {
        false
    };
    if !drive && !unc {
        return Err(Error::Unavailable(
            "autostart path must be an absolute drive or UNC path".to_string(),
        ));
    }
    for (i, ch) in s.char_indices() {
        if ch.is_control()
            || matches!(ch, '"' | '<' | '>' | '|' | '?' | '*')
            || (ch == ':' && !(drive && i == 1))
        {
            return Err(Error::Unavailable(
                "autostart path contains an invalid Windows character".to_string(),
            ));
        }
    }
    Ok(s)
}

fn quote_windows_arg(path: &Path) -> Result<String> {
    let s = windows_path_str(path)?;
    let mut quoted = String::with_capacity(s.len() + 4);
    quoted.push('"');
    quoted.push_str(s);
    let trailing = s.chars().rev().take_while(|&c| c == '\\').count();
    quoted.push_str(&"\\".repeat(trailing));
    quoted.push('"');
    Ok(quoted)
}

/// Build the Run-value command line for the UI exe with profile support.
///
/// On success returns the exact Windows command line that preserves
/// `--minimized` and adds `--profile-dir` as one properly-escaped argument.
/// Trailing backslashes in the profile are doubled before the closing quote
/// to preserve the exact argv. Returns `Error::Unavailable` for unrepresentable paths
/// (NUL, quote, control characters) rather than performing lossy conversion.
///
/// # Errors
///
/// Returns `Error::Unavailable` if either path is non-Unicode, not an absolute
/// drive or UNC path, or contains an invalid Windows path character.
pub fn run_value_with_profile(
    ui_exe: &Path,
    profile_dir: &Path,
    minimized: bool,
) -> Result<String> {
    let exe_q = quote_windows_arg(ui_exe)?;
    let profile_q = quote_windows_arg(profile_dir)?;
    let mut out = exe_q;
    if minimized {
        out.push_str(" --minimized");
    }
    out.push_str(" --profile-dir ");
    out.push_str(&profile_q);
    Ok(out)
}

/// Report whether autostart setup must be skipped (portable mode).
#[must_use]
pub fn should_skip_autostart(portable: bool) -> bool {
    portable
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approved_enabled_byte() {
        let bytes = [
            0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        assert_eq!(startup_approved_enabled(&bytes), Some(true));
    }

    #[test]
    fn approved_disabled_byte() {
        let bytes = [
            0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        assert_eq!(startup_approved_enabled(&bytes), Some(false));
    }

    #[test]
    fn approved_garbage_is_none() {
        assert_eq!(startup_approved_enabled(&[]), None);
        assert_eq!(startup_approved_enabled(&[0x02]), None);
        assert_eq!(startup_approved_enabled(&[0x09; 12]), None);
    }

    #[test]
    fn run_value_quotes_and_flag() {
        let exe = Path::new("C:\\Program Files\\Clipmonger\\clipmonger-daemon.exe");
        let v = run_value(exe, true);
        assert!(v.starts_with('"') && v.contains("--minimized"), "{v}");
        let plain = run_value(exe, false);
        assert!(!plain.contains("--minimized"), "{plain}");
    }

    #[test]
    fn portable_skips_autostart() {
        assert!(should_skip_autostart(true));
        assert!(!should_skip_autostart(false));
    }

    #[test]
    fn windows_profile_simple_vector() {
        let exe = Path::new("C:\\Program Files\\Clipmonger\\clipmonger.exe");
        let profile = Path::new("C:\\Users\\Alice\\AppData\\Roaming\\clipmonger\\profiles\\work");
        let v = run_value_with_profile(exe, profile, true).expect("valid profile");
        assert_eq!(
            v,
            "\"C:\\Program Files\\Clipmonger\\clipmonger.exe\" --minimized --profile-dir \"C:\\Users\\Alice\\AppData\\Roaming\\clipmonger\\profiles\\work\""
        );
    }

    #[test]
    fn windows_profile_with_spaces_unicode() {
        let exe = Path::new("C:\\Program Files\\Clipmonger\\clipmonger.exe");
        let profile = Path::new("C:\\Users\\Jos\u{00e9}\\My Profiles\\work profile");
        let v = run_value_with_profile(exe, profile, true).unwrap();
        assert!(v.contains("\"C:\\Users\\Jos\u{00e9}\\My Profiles\\work profile\""));
        assert!(v.contains("--minimized"));
        assert!(v.contains("--profile-dir"));
        assert_eq!(v.matches("--minimized").count(), 1);
    }

    #[test]
    fn windows_profile_trailing_backslash_preserved() {
        let exe = Path::new("C:\\Tools\\clipmonger.exe");
        let profile = Path::new("C:\\profiles\\work\\");
        let v = run_value_with_profile(exe, profile, true).unwrap();
        assert_eq!(
            v,
            "\"C:\\Tools\\clipmonger.exe\" --minimized --profile-dir \"C:\\profiles\\work\\\\\""
        );
        let profile2 = Path::new("C:\\profiles\\work\\\\");
        let v2 = run_value_with_profile(exe, profile2, true).unwrap();
        assert_eq!(
            v2,
            "\"C:\\Tools\\clipmonger.exe\" --minimized --profile-dir \"C:\\profiles\\work\\\\\\\\\""
        );
    }

    #[test]
    fn windows_profile_without_minimized() {
        let exe = Path::new("C:\\clipmonger.exe");
        let profile = Path::new("D:\\profiles\\a");
        let v = run_value_with_profile(exe, profile, false).unwrap();
        assert!(!v.contains("--minimized"));
        assert!(v.contains("--profile-dir"));
    }

    #[test]
    fn windows_profile_exact_minimized_not_substring() {
        let exe = Path::new("C:\\clipmonger.exe");
        let profile = Path::new("C:\\my--minimized-profile");
        let v = run_value_with_profile(exe, profile, true).unwrap();
        assert_eq!(
            v,
            "\"C:\\clipmonger.exe\" --minimized --profile-dir \"C:\\my--minimized-profile\""
        );
    }

    #[test]
    fn windows_profile_rejects_invalid_paths() {
        let exe = Path::new("C:\\clipmonger.exe");
        let bad_nul = Path::new("C:\\bad\0path");
        assert!(run_value_with_profile(exe, bad_nul, true).is_err());
        let bad_quote = Path::new("C:\\bad\"path");
        assert!(run_value_with_profile(exe, bad_quote, true).is_err());
        let bad_control = Path::new("C:\\bad\x01path");
        assert!(run_value_with_profile(exe, bad_control, true).is_err());
        let bad_exe_quote = Path::new("C:\\bad\"exe.exe");
        let good_profile = Path::new("C:\\good\\profile");
        assert!(run_value_with_profile(bad_exe_quote, good_profile, true).is_err());
    }

    #[test]
    fn windows_legacy_compat_retained() {
        let exe = Path::new("C:\\Program Files\\Clipmonger\\clipmonger-daemon.exe");
        let v = run_value(exe, true);
        assert!(v.starts_with('"') && v.contains("--minimized"));
        let v2 = run_value_with_profile(exe, Path::new("C:\\p"), true).unwrap();
        assert!(v2.contains("--profile-dir"));
    }
    #[test]
    fn windows_absolute_and_invalid_path_vectors() {
        let exe = Path::new(r"C:\clipmonger.exe");
        for valid in [
            r"C:\",
            "D:/profiles/é space",
            r"\\server\share\profile",
            "//server/share/profile",
        ] {
            assert!(
                run_value_with_profile(exe, Path::new(valid), true).is_ok(),
                "{valid:?}"
            );
        }
        for invalid in [
            "",
            "relative",
            r"C:relative",
            r"\rooted",
            "/rooted",
            r"xC:\bad",
            r"\\server",
            r"\\server\\share",
            r"C:\bad*name",
            r"C:\bad|name",
            r"C:\bad:name",
            r"C:\bad<name",
            r"C:\bad?name",
        ] {
            assert!(
                run_value_with_profile(exe, Path::new(invalid), true).is_err(),
                "{invalid:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn windows_rejects_non_unicode_os_path() {
        use std::os::unix::ffi::OsStrExt;
        let bad = Path::new(std::ffi::OsStr::from_bytes(b"C:\\bad\xff"));
        assert!(matches!(
            run_value_with_profile(Path::new(r"C:\clipmonger.exe"), bad, true),
            Err(Error::Unavailable(_))
        ));
    }

    #[cfg(windows)]
    #[test]
    fn windows_rejects_unpaired_utf16() {
        use std::os::windows::ffi::OsStringExt;
        let bad = std::ffi::OsString::from_wide(&[
            u16::from(b'C'),
            u16::from(b':'),
            u16::from(b'\\'),
            0xd800,
        ]);
        assert!(matches!(
            run_value_with_profile(Path::new(r"C:\clipmonger.exe"), Path::new(&bad), true),
            Err(Error::Unavailable(_))
        ));
    }
}
