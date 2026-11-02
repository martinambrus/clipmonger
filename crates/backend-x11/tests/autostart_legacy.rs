#![cfg(unix)]

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use clipmonger_backend_x11::{desktop::parse_desktop, X11Autostart};
use clipmonger_platform::traits::AutostartManager;

struct OwnedRoot(PathBuf);

impl OwnedRoot {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("clipmonger-legacy-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&path).expect("create unique owned root");
        Self(path)
    }
}

impl Drop for OwnedRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn legacy_manager_writes_exact_descriptors_and_roundtrips() {
    let cases = [
        ("/usr/bin/clipmonger", r#""/usr/bin/clipmonger" --minimized"#),
        ("/opt/My Apps/é clipmonger", r#""/opt/My Apps/é clipmonger" --minimized"#),
        ("/opt/$cash/%f/%u/100%", r#""/opt/\\$cash/%%f/%%u/100%%" --minimized"#),
        ("/opt/back\\slash/`tick/\"quote", r#""/opt/back\\\\slash/\\`tick/\\"quote" --minimized"#),
        ("/opt/trailing\\", r#""/opt/trailing\\\\" --minimized"#),
    ];
    let root = OwnedRoot::new();
    for (exe, expected_exec) in cases {
        let mut manager = X11Autostart::new(PathBuf::from(exe), root.0.clone(), false);
        manager.enable().expect("enable legacy entry");
        let file = root.0.join("autostart/clipmonger.desktop");
        let text = std::fs::read_to_string(&file).expect("read descriptor");
        let expected = format!("[Desktop Entry]\nType=Application\nName=Clipmonger\nExec={expected_exec}\nHidden=false\nTerminal=false\n");
        assert_eq!(text, expected, "executable {exe:?}");
        let entry = parse_desktop(&text).expect("parse descriptor");
        assert_eq!(entry.exec, exe);
        assert_eq!(entry.profile, None);
        assert!(entry.minimized);
        assert!(!entry.hidden);
        assert!(manager.is_enabled().expect("query enabled"));
        manager.disable().expect("disable legacy entry");
        assert!(!file.exists());
        assert!(!manager.is_enabled().expect("query disabled"));
    }
}

#[test]
fn legacy_invalid_paths_fail_before_writing_and_portable_skips_validation() {
    let root = OwnedRoot::new();
    let invalid = [
        PathBuf::from("relative/exe"),
        PathBuf::from("/bad=exe"),
        PathBuf::from("/bad\nexe"),
        PathBuf::from("/bad\rexe"),
        PathBuf::from("/bad\0exe"),
        PathBuf::from(OsString::from_vec(b"/bad\xffexe".to_vec())),
    ];
    for (index, exe) in invalid.into_iter().enumerate() {
        let target = root.0.join(index.to_string());
        let mut manager = X11Autostart::new(exe.clone(), target.clone(), false);
        assert!(manager.enable().is_err(), "accepted {exe:?}");
        assert!(!Path::new(&target).exists(), "created root for {exe:?}");
        let mut portable = X11Autostart::new(exe, target.clone(), true);
        portable.enable().expect("portable enable is a no-op");
        assert!(!portable.is_enabled().expect("portable query"));
        portable.disable().expect("portable disable is a no-op");
        assert!(!target.exists(), "portable manager created root");
    }
}
