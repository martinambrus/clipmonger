//! Autostart pure logic (pure).

use std::path::Path;

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
}
