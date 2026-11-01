//! WINHELPER acceptance harness (ruler only).
//!
//! Acceptance tests for the frozen `clipmonger_backend_windows::helper` API
//! (see `.agents/tmp/windows-v1/winhelper/harness-spec.md`). The `helper`
//! module is provided by the builder; this file only rules its contract and
//! must NOT grow implementation code.
//!
//! Pure/unit legs below compile and run on Linux. Session legs are
//! individually `#[cfg(windows)]`-gated. Manual-QA legs print a single
//! `SKIP <test-id>: <reason>` line and pass without asserting.
//!
//! Safety: no process spawning, no elevation attempts, no sleeps; all time
//! is injected via explicit `at_ms` arguments.

use std::path::PathBuf;

use clipmonger_backend_windows::helper::{
    decide_route, decode_job, encode_job, protected_path_ok, GestureTokens, HelperAction,
    HelperState, Job, JobOp, JobResult, Route,
};
use clipmonger_backend_windows::Integrity;

/// Byte limit for `Job.payload`: 64 KiB. Tests use ASCII payloads so the
/// byte count and the Unicode scalar count coincide.
const MAX_PAYLOAD: usize = 64 * 1024;

/// T-403: job encode/decode round-trips every field; the encoded line is a
/// single JSONL line (trailing `\n`, no interior newlines).
#[test]
fn t403_job_roundtrip() {
    const T: &str = "t403_job_roundtrip";
    let job = Job {
        job_id: 42,
        target_pid: 1234,
        op: JobOp::SendInputBatch,
        payload: "hello".to_string(),
        target_hwnd: None,
        expires_ms: None,
    };
    let line = encode_job(&job).unwrap_or_else(|e| panic!("{T}: encode failed: {e:?}"));
    assert!(
        line.ends_with('\n'),
        "{T}: encoded job must be a JSONL line ending in newline, got: {line:?}"
    );
    assert!(
        !line.trim_end_matches('\n').contains('\n'),
        "{T}: encoded job must be a single line, got: {line:?}"
    );
    let back = decode_job(&line).unwrap_or_else(|e| panic!("{T}: decode failed: {e:?}"));
    assert_eq!(back.job_id, 42, "{T}: job_id");
    assert_eq!(back.target_pid, 1234, "{T}: target_pid");
    assert!(
        matches!(back.op, JobOp::SendInputBatch),
        "{T}: op must round-trip as SendInputBatch"
    );
    assert_eq!(back.payload, "hello", "{T}: payload");

    // Boundary: exactly 64 KiB is accepted.
    let max_job = Job {
        job_id: 43,
        target_pid: 1234,
        op: JobOp::SendInputBatch,
        payload: "a".repeat(MAX_PAYLOAD),
        target_hwnd: None,
        expires_ms: None,
    };
    let max_line =
        encode_job(&max_job).unwrap_or_else(|e| panic!("{T}: 64 KiB encode failed: {e:?}"));
    let max_back =
        decode_job(&max_line).unwrap_or_else(|e| panic!("{T}: 64 KiB decode failed: {e:?}"));
    assert_eq!(max_back.payload.len(), MAX_PAYLOAD, "{T}: 64 KiB payload");
}

/// T-403 negative: an unknown `op` discriminant is rejected.
#[test]
fn t403_job_unknown_op_rejected() {
    const T: &str = "t403_job_unknown_op_rejected";
    let line = "{\"job_id\":1,\"target_pid\":99,\"op\":\"BogusOp\",\"payload\":\"hi\"}\n";
    assert!(
        decode_job(line).is_err(),
        "{T}: unknown op must be rejected"
    );
    assert!(
        decode_job("not json at all\n").is_err(),
        "{T}: non-JSON line must be rejected"
    );
    assert!(decode_job("").is_err(), "{T}: empty line must be rejected");
}

/// T-403 negative: payload over 64 KiB is rejected on both sides.
#[test]
fn t403_job_oversize_rejected() {
    const T: &str = "t403_job_oversize_rejected";
    let big = Job {
        job_id: 7,
        target_pid: 99,
        op: JobOp::SendInputBatch,
        payload: "b".repeat(MAX_PAYLOAD + 1),
        target_hwnd: None,
        expires_ms: None,
    };
    assert!(
        encode_job(&big).is_err(),
        "{T}: encode must reject payload > 64 KiB"
    );
    let raw = format!(
        "{{\"job_id\":7,\"target_pid\":99,\"op\":\"SendInputBatch\",\"payload\":\"{}\"}}\n",
        "b".repeat(MAX_PAYLOAD + 1)
    );
    assert!(
        decode_job(&raw).is_err(),
        "{T}: decode must reject payload > 64 KiB"
    );
}

/// T-403: `JobResult` echoes the originating `job_id`.
#[test]
fn t403_result_echo_job_id() {
    const T: &str = "t403_result_echo_job_id";
    let job = Job {
        job_id: 9,
        target_pid: 55,
        op: JobOp::SendInputBatch,
        payload: String::new(),
        target_hwnd: None,
        expires_ms: None,
    };
    let ok_result = JobResult {
        job_id: job.job_id,
        ok: true,
        error: None,
    };
    assert_eq!(
        ok_result.job_id, job.job_id,
        "{T}: ok result must echo job_id"
    );
    assert!(ok_result.ok, "{T}: ok flag");
    assert!(ok_result.error.is_none(), "{T}: ok result has no error");

    let err_result = JobResult {
        job_id: job.job_id,
        ok: false,
        error: Some("denied".to_string()),
    };
    assert_eq!(
        err_result.job_id, job.job_id,
        "{T}: err result must echo job_id"
    );
    assert!(!err_result.ok, "{T}: err flag");
    assert_eq!(
        err_result.error.as_deref(),
        Some("denied"),
        "{T}: err message"
    );
}

/// T-413 unit: a minted token consumes exactly once; the second consume is
/// false.
#[test]
fn t413_unit_token_consume_once() {
    const T: &str = "t413_unit_token_consume_once";
    let mut tokens = GestureTokens::new();
    let tok = tokens.mint(1_000).expect("mint");
    assert!(
        tokens.consume(tok, 1_001),
        "{T}: first consume must succeed"
    );
    assert!(
        !tokens.consume(tok, 1_002),
        "{T}: second consume must be false (single-use)"
    );
}

/// T-413 unit: expired and unknown tokens are refused.
#[test]
fn t413_unit_token_expired_unknown() {
    const T: &str = "t413_unit_token_expired_unknown";
    let mut tokens = GestureTokens::new();
    let tok = tokens.mint(0).expect("mint");
    assert!(
        tokens.consume(tok, 30_000),
        "{T}: consume at exactly TTL must still succeed"
    );

    let mut tokens = GestureTokens::new();
    let tok = tokens.mint(0).expect("mint");
    assert!(
        !tokens.consume(tok, 30_001),
        "{T}: consume past TTL (30_000 ms) must be false"
    );

    let mut tokens = GestureTokens::new();
    assert!(
        !tokens.consume(0xDEAD_BEEF_DEAD_BEEF, 1_000),
        "{T}: unknown token must be false"
    );
}

/// T-413 unit: `decide_route` matrix (6 cells). Medium always takes the
/// normal path; High needs an installed (protected, non-portable) helper;
/// Unknown never sends blind.
#[test]
fn t413_unit_decide_route_matrix() {
    const T: &str = "t413_unit_decide_route_matrix";
    assert!(
        matches!(decide_route(Integrity::Medium, false, false), Route::Normal),
        "{T}: Medium installed-unprotected -> Normal"
    );
    assert!(
        matches!(decide_route(Integrity::Medium, true, false), Route::Normal),
        "{T}: Medium portable -> Normal"
    );
    assert!(
        matches!(decide_route(Integrity::High, false, true), Route::Helper),
        "{T}: High installed-protected -> Helper"
    );
    match decide_route(Integrity::High, false, false) {
        Route::Unavailable(reason) => assert!(!reason.is_empty(), "{T}: reason must be set"),
        other => panic!("{T}: High installed-unprotected must be Unavailable, got: {other:?}"),
    }
    match decide_route(Integrity::High, true, true) {
        Route::Unavailable(reason) => assert!(!reason.is_empty(), "{T}: reason must be set"),
        other => panic!("{T}: High portable must be Unavailable, got: {other:?}"),
    }
    match decide_route(Integrity::Unknown, false, true) {
        Route::Unavailable(reason) => assert!(!reason.is_empty(), "{T}: reason must be set"),
        other => panic!("{T}: Unknown must be Unavailable (never blind), got: {other:?}"),
    }
}

/// T-413 unit: heartbeat machine. Fresh beats keep the helper alive; more
/// than 45 s (3 missed 15 s beats) since the last beat reads dead;
/// disconnect always self-exits.
#[test]
fn t413_unit_heartbeat_alive_dead() {
    const T: &str = "t413_unit_heartbeat_alive_dead";
    let mut st = HelperState::new();
    st.note_beat(0);
    assert!(st.alive(30_000), "{T}: 30 s after beat must be alive");
    assert!(
        st.alive(45_000),
        "{T}: exactly 45 s after beat must be alive"
    );
    assert!(
        !st.alive(45_001),
        "{T}: past 45 s (3 missed 15 s beats) must read dead"
    );
    st.note_beat(100_000);
    assert!(st.alive(100_001), "{T}: fresh beat must revive");
    assert!(
        matches!(st.on_disconnect(), HelperAction::Exit),
        "{T}: disconnect must self-exit"
    );
}

/// T-414: an exe under Program Files passes the protected-path predicate.
#[test]
fn t414_protected_path_under_program_files() {
    const T: &str = "t414_protected_path_under_program_files";
    let exe = PathBuf::from("C:\\Program Files\\Clipmonger\\helper.exe");
    let pf = PathBuf::from("C:\\Program Files");
    assert!(
        protected_path_ok(&exe, &pf),
        "{T}: exe under Program Files must pass"
    );
}

/// T-414: a portable/temp layout (exe elsewhere) fails the predicate.
#[test]
fn t414_protected_path_temp_false() {
    const T: &str = "t414_protected_path_temp_false";
    let pf = PathBuf::from("C:\\Program Files");
    let temp_exe = PathBuf::from("C:\\Users\\alice\\AppData\\Local\\Temp\\helper.exe");
    assert!(
        !protected_path_ok(&temp_exe, &pf),
        "{T}: temp exe must fail"
    );
    let portable_exe = PathBuf::from("D:\\portable\\clipmonger\\helper.exe");
    assert!(
        !protected_path_ok(&portable_exe, &pf),
        "{T}: portable exe must fail"
    );
}

/// T-414: the prefix match is case-insensitive.
#[test]
fn t414_protected_path_case_insensitive() {
    const T: &str = "t414_protected_path_case_insensitive";
    let exe = PathBuf::from("c:\\program files\\Clipmonger\\helper.exe");
    let pf = PathBuf::from("C:\\PROGRAM FILES");
    assert!(
        protected_path_ok(&exe, &pf),
        "{T}: case-insensitive Program Files prefix must pass"
    );
}

/// T-414: portable layouts skip the helper via `decide_route` (High +
/// portable is Unavailable, never Helper).
#[test]
fn t414_portable_skip_via_decide_route() {
    const T: &str = "t414_portable_skip_via_decide_route";
    match decide_route(Integrity::High, true, false) {
        Route::Unavailable(reason) => assert!(!reason.is_empty(), "{T}: reason must be set"),
        other => panic!("{T}: High portable must skip helper, got: {other:?}"),
    }
}

/// T-409 stub: the runas/UAC served leg is manual QA by the user, never
/// automated here.
#[cfg(windows)]
#[test]
fn t409_stub_runas_uac_manual_qa() {
    println!(
        "SKIP t409_stub_runas_uac_manual_qa: runas/UAC served leg is manual QA by user; \
         never automated here"
    );
}
