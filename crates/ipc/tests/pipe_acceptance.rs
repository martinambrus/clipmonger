//! WINPIPE acceptance harness (windows-v1, task WINPIPE).
#![cfg(windows)]
//!
//! This file is the RULER, not the implementation. It pins the frozen contract
//! from seg-02 S9 plus WIN-F4: named-pipe transport on Windows.
//! The builder (`clipmonger-ipc`) MUST make this file compile and pass
//! without any edit to this file or to `Cargo.toml`.
//!
//! Run (on Windows):
//! `cargo test -p clipmonger-ipc --features server --test pipe_acceptance`
//!
//! Cross-compile dry run (from Linux):
//! `cargo test --target x86_64-pc-windows-gnu -p clipmonger-ipc --features server --test pipe_acceptance --no-run`
//!
//! Frozen contract the builder MUST match:
//! - `framing::pipe_name_for(profile_dir: &Path) -> String`
//!   == `\\.\pipe\clipmonger-<profile_hash(profile_dir)>`
//! - `server::Server::bind_pipe(pipe_name: &str, token: String, handler: Handler)`
//!   `-> Result<Server, IpcError>` (tokio named pipe: first-instance loud
//!   fail — second bind on the same name errors; reject remote; byte mode;
//!   current-user DACL, fail closed). `Server::bind` (unix) is untouched.
//! - `client::Client::connect_pipe(pipe_name: &str, token: &str)`
//!   `-> Result<Client, IpcError>` (std; retry 25x40ms; hello handshake
//!   same as unix).
//! - Daemon on Windows uses `bind_pipe` with `pipe_name_for(profile)`;
//!   `rendezvous.json` stores `{"socket": "<pipe name>", "protocol": "ipc/v1"}`
//!   (socket key carries the pipe name; `client::connect` dispatches on the
//!   `\\.\pipe\` prefix).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use clipmonger_ipc::framing;
use serde_json::{json, Value};

#[cfg(feature = "server")]
use clipmonger_ipc::client::Client;
#[cfg(feature = "server")]
use clipmonger_ipc::proto::IpcError;
#[cfg(feature = "server")]
use clipmonger_ipc::server::Server;
#[cfg(feature = "server")]
use std::sync::Arc;
#[cfg(feature = "server")]
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[cfg(feature = "server")]
const TEST_TOKEN: &str = "winpipe-harness-token-1";

#[cfg(feature = "server")]
static PIPE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Fresh tempdir-scoped profile dir (unique per call via pid + counter).
/// The live pipe name derives from it via the real `pipe_name_for`, so every
/// test binds its own pipe without inventing names outside the contract.
#[cfg(feature = "server")]
fn unique_profile(tag: &str) -> PathBuf {
    let n = PIPE_COUNTER.fetch_add(1, Ordering::SeqCst);
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "clipmonger-winpipe-{}-{tag}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("temp profile dir must be creatable");
    dir
}

/// (profile dir, pipe name) pair: the pipe name is exactly what the daemon
/// would use for this profile, unique to this test.
#[cfg(feature = "server")]
fn unique_pipe(tag: &str) -> (PathBuf, String) {
    let profile = unique_profile(tag);
    let name = framing::pipe_name_for(&profile);
    (profile, name)
}

/// Minimal echo handler for live tests: `echo` returns params unchanged,
/// anything else is `unknown_method`.
#[cfg(feature = "server")]
fn echo_handler() -> Arc<dyn Fn(String, Value) -> Result<Value, IpcError> + Send + Sync> {
    Arc::new(move |method: String, params: Value| {
        if method == "echo" {
            Ok(params)
        } else {
            Err(IpcError {
                code: "unknown_method".to_string(),
                message: format!("no such method: {method}"),
                retryable: false,
                backend: None,
            })
        }
    })
}

/// Bind a pipe server. The returned handle is kept alive by the caller until
/// the test ends; no shutdown API is assumed.
#[cfg(feature = "server")]
fn bind_pipe_server(pipe_name: &str, token: &str) -> Server {
    match Server::bind_pipe(pipe_name, token.to_string(), echo_handler()) {
        Ok(server) => server,
        Err(e) => panic!("Server::bind_pipe must succeed on a fresh pipe name: {e:?}"),
    }
}

// ---------------------------------------------------------------------------
// WPIPE-1: pipe name shape (no server needed)
// ---------------------------------------------------------------------------

#[test]
fn wpipe_name_shape() {
    let profile = PathBuf::from("C:\\Users\\test\\AppData\\Roaming\\clipmonger\\profile");
    let name = framing::pipe_name_for(&profile);
    assert!(
        name.starts_with("\\\\.\\pipe\\clipmonger-"),
        "pipe name must start with \\\\.\\pipe\\clipmonger-, got {name:?}"
    );
    let suffix = &name["\\\\.\\pipe\\clipmonger-".len()..];
    assert_eq!(
        suffix.len(),
        16,
        "pipe name suffix must be exactly 16 chars, got {suffix:?}"
    );
    assert!(
        suffix
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "pipe name suffix must be 16 lowercase hex chars, got {suffix:?}"
    );
}

#[test]
fn wpipe_name_stable_per_profile() {
    let profile = PathBuf::from("C:\\profiles\\alpha");
    let first = framing::pipe_name_for(&profile);
    let second = framing::pipe_name_for(&profile);
    assert_eq!(
        first, second,
        "pipe_name_for must be stable for the same profile dir"
    );
}

#[test]
fn wpipe_name_differs_per_profile() {
    let a = framing::pipe_name_for(&PathBuf::from("C:\\profiles\\alpha"));
    let b = framing::pipe_name_for(&PathBuf::from("C:\\profiles\\beta"));
    assert_ne!(
        a, b,
        "pipe_name_for must differ across profile dirs (hash collision in test vectors)"
    );
}

#[test]
fn wpipe_name_matches_profile_hash() {
    // The suffix is exactly profile_hash(profile): the daemon, the CLI, and
    // the rendezvous file all derive the same name from one function.
    let profile = PathBuf::from("C:\\profiles\\gamma");
    let name = framing::pipe_name_for(&profile);
    let expected = format!(
        "\\\\.\\pipe\\clipmonger-{}",
        framing::profile_hash(&profile)
    );
    assert_eq!(
        name, expected,
        "pipe_name_for must equal \\\\.\\pipe\\clipmonger-<profile_hash>"
    );
}

// ---------------------------------------------------------------------------
// WPIPE-2..5: live pipe tests (gated on the `server` feature)
// ---------------------------------------------------------------------------

#[cfg(feature = "server")]
#[tokio::test(flavor = "multi_thread")]
async fn wpipe_echo_roundtrip() {
    let (_profile, name) = unique_pipe("echo");
    let server = bind_pipe_server(&name, TEST_TOKEN);
    // Keep the server alive for the whole test without assuming a shutdown API.
    std::mem::forget(server);
    let client = match Client::connect_pipe(&name, TEST_TOKEN) {
        Ok(c) => c,
        Err(e) => panic!("Client::connect_pipe must succeed with the right token: {e:?}"),
    };
    let params = json!({"hello": "world", "n": 3});
    let got = match client.call("echo", params.clone()) {
        Ok(v) => v,
        Err(e) => panic!("echo call must succeed: {e:?}"),
    };
    assert_eq!(got, params, "echo handler must return params unchanged");
    // Negative control: unknown method must fail, not echo.
    assert!(
        client.call("no.such.method", json!({})).is_err(),
        "unknown method must be an error"
    );
}

#[cfg(feature = "server")]
#[tokio::test(flavor = "multi_thread")]
async fn wpipe_bad_token() {
    let (_profile, name) = unique_pipe("token");
    let server = bind_pipe_server(&name, TEST_TOKEN);
    std::mem::forget(server);
    match Client::connect_pipe(&name, "wrong-token") {
        Ok(_) => panic!("connect with a bad token must fail"),
        Err(e) => assert_eq!(e.code, "auth_failed", "bad token maps to auth_failed"),
    }
    // The server is still healthy for the right token afterwards.
    let client = match Client::connect_pipe(&name, TEST_TOKEN) {
        Ok(c) => c,
        Err(e) => panic!("server must survive a bad-token attempt: {e:?}"),
    };
    let got = match client.call("echo", json!({"after": "bad-token-attempt"})) {
        Ok(v) => v,
        Err(e) => panic!("server must answer after a bad-token attempt: {e:?}"),
    };
    assert_eq!(got, json!({"after": "bad-token-attempt"}));
}

#[cfg(feature = "server")]
#[tokio::test(flavor = "multi_thread")]
async fn wpipe_second_bind_fails() {
    // First-instance loud fail: binding the same pipe name twice errors.
    let (_profile, name) = unique_pipe("double");
    let first = bind_pipe_server(&name, TEST_TOKEN);
    // The first server must stay alive so the name is still taken.
    std::mem::forget(first);
    match Server::bind_pipe(&name, TEST_TOKEN.to_string(), echo_handler()) {
        Ok(_) => panic!("second bind on the same pipe name must fail"),
        Err(_) => {}
    }
}

#[cfg(feature = "server")]
#[tokio::test(flavor = "multi_thread")]
async fn wpipe_nonexistent_pipe_errors_bounded() {
    // No server listens here; connect_pipe must give up (its own 25x40ms
    // retry budget) and return Err in bounded time. No test-side sleeps:
    // all waiting happens inside the client's retry loop.
    let n = PIPE_COUNTER.fetch_add(1, Ordering::SeqCst);
    let missing = format!(
        "\\\\.\\pipe\\clipmonger-winpipe-missing-{}-{n}",
        std::process::id()
    );
    assert!(
        missing.starts_with("\\\\.\\pipe\\"),
        "sanity: the missing target must be a pipe path, got {missing:?}"
    );
    let start = Instant::now();
    let result = Client::connect_pipe(&missing, TEST_TOKEN);
    let elapsed = start.elapsed();
    assert!(
        result.is_err(),
        "connect to a nonexistent pipe must fail after retries"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "connect failure must arrive in bounded time (< 10s), took {elapsed:?}"
    );
}
