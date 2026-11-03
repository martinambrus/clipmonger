//! Malformed registrar requests through the host executable.
//! Windows tests use the pure parser; no test can enter native helper modes.
#![cfg(all(feature = "backend-windows", not(windows)))]

#[test]
fn malformed_registrar_exits_before_profile_access_or_other_hidden_modes() {
    let root = std::env::temp_dir().join(format!(
        "clipmonger-registrar-dispatch-{}",
        std::process::id()
    ));
    assert!(!root.exists(), "fixture must start absent");
    for mixed_mode in [
        None,
        Some("__elevated-persistent"),
        Some("__elevated-worker"),
    ] {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_clipmonger-daemon"));
        // An invalid SID guarantees parser rejection, even on Windows.
        command.args(["__elevated-install-task", "--profile-dir"]);
        command.arg(&root);
        command.args(["--requester-pid", "123", "--requester-sid", "S-1-invalid"]);
        if let Some(mode) = mixed_mode {
            command.arg(mode);
        }
        let output = command.output().expect("run malformed registrar");
        assert_eq!(output.status.code(), Some(101));
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("elevated helper registration failed")
        );
        assert!(
            !root.exists(),
            "registrar must dispatch before creating or reading a profile"
        );
    }
}
