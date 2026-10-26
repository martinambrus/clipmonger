//! Blocking std IPC client over a Unix socket.

use std::path::Path;

#[cfg(unix)]
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(unix)]
use std::sync::Mutex;

use serde_json::Value;

use crate::proto::IpcError;

#[cfg(unix)]
use crate::framing::{decode_line, encode_line};
#[cfg(unix)]
use crate::proto::{Envelope, FrameKind, IPC_VERSION};

/// Blocking JSONL client; the hello handshake runs inside `connect`.
pub struct Client {
    #[cfg(unix)]
    link: Mutex<ClientLink>,
    #[cfg(not(unix))]
    link: NoLink,
}

#[cfg(unix)]
struct ClientLink {
    reader: BufReader<std::os::unix::net::UnixStream>,
    next_id: AtomicU64,
}

#[cfg(not(unix))]
struct NoLink;

#[cfg(unix)]
fn write_line(
    reader: &mut BufReader<std::os::unix::net::UnixStream>,
    bytes: &[u8],
) -> Result<(), IpcError> {
    let stream = reader.get_mut();
    stream
        .write_all(bytes)
        .map_err(|e| IpcError::new("io_failed", e.to_string()))?;
    if !bytes.ends_with(b"\n") {
        stream
            .write_all(b"\n")
            .map_err(|e| IpcError::new("io_failed", e.to_string()))?;
    }
    stream
        .flush()
        .map_err(|e| IpcError::new("io_failed", e.to_string()))
}

#[cfg(unix)]
fn read_frame(
    reader: &mut BufReader<std::os::unix::net::UnixStream>,
) -> Result<Envelope, IpcError> {
    let mut line = String::new();
    let n = reader
        .read_line(&mut line)
        .map_err(|e| IpcError::new("io_failed", e.to_string()))?;
    if n == 0 {
        return Err(IpcError::new(
            "connection_closed",
            "server closed the connection".to_string(),
        ));
    }
    decode_line(line.as_bytes())
}

impl Client {
    /// Connect and run the hello handshake; a bad token fails with `auth_failed`.
    ///
    /// # Errors
    ///
    /// Returns the server's handshake error (notably `auth_failed`) or an I/O failure.
    #[cfg(unix)]
    pub fn connect(sock: &Path, token: &str) -> Result<Self, IpcError> {
        use std::os::unix::net::UnixStream;

        let stream = UnixStream::connect(sock)
            .map_err(|e| IpcError::new("connection_refused", e.to_string()))?;
        let mut link = ClientLink {
            reader: BufReader::new(stream),
            next_id: AtomicU64::new(0),
        };
        let hello = Envelope {
            v: IPC_VERSION.to_string(),
            id: "hello-0".to_string(),
            kind: FrameKind::Req,
            method: Some("hello".to_string()),
            params: Some(serde_json::json!({
                "min": 1,
                "max": 1,
                "token": token,
                "client": "clipmonger-cli",
            })),
            result: None,
            error: None,
            seq: None,
        };
        let bytes = encode_line(&hello)?;
        write_line(&mut link.reader, &bytes)?;
        let welcome = read_frame(&mut link.reader)?;
        if let Some(error) = welcome.error {
            return Err(error);
        }
        Ok(Self {
            link: Mutex::new(link),
        })
    }

    /// Connect stub on platforms without Unix sockets.
    ///
    /// # Errors
    ///
    /// Always fails with `unavailable`.
    #[cfg(not(unix))]
    pub fn connect(_sock: &Path, _token: &str) -> Result<Self, IpcError> {
        Err(IpcError::new(
            "unavailable",
            "ipc client needs a Unix socket".to_string(),
        ))
    }

    /// Call `method` with JSON params; handler failures surface with their code.
    ///
    /// # Errors
    ///
    /// Returns the server's error for the call or an I/O failure.
    #[cfg(unix)]
    pub fn call(&self, method: &str, params: Value) -> Result<Value, IpcError> {
        let mut link = self
            .link
            .lock()
            .map_err(|_| IpcError::new("io_failed", "client lock poisoned".to_string()))?;
        let seq = link.next_id.fetch_add(1, Ordering::SeqCst);
        let id = format!("c{seq}");
        let req = Envelope {
            v: IPC_VERSION.to_string(),
            id: id.clone(),
            kind: FrameKind::Req,
            method: Some(method.to_string()),
            params: Some(params),
            result: None,
            error: None,
            seq: None,
        };
        let bytes = encode_line(&req)?;
        write_line(&mut link.reader, &bytes)?;
        loop {
            let frame = read_frame(&mut link.reader)?;
            if frame.id != id {
                continue;
            }
            if let Some(error) = frame.error {
                return Err(error);
            }
            return Ok(frame.result.unwrap_or(Value::Null));
        }
    }

    /// Call stub on platforms without Unix sockets.
    ///
    /// # Errors
    ///
    /// Always fails with `unavailable`.
    #[cfg(not(unix))]
    pub fn call(&self, _method: &str, _params: Value) -> Result<Value, IpcError> {
        let _ = &self.link;
        Err(IpcError::new(
            "unavailable",
            "ipc client needs a Unix socket".to_string(),
        ))
    }
}
