//! Blocking std IPC client over a Unix socket or Windows named pipe.

use std::path::Path;

#[cfg(any(unix, windows))]
use std::io::{BufRead, BufReader, Write};
#[cfg(any(unix, windows))]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(any(unix, windows))]
use std::sync::Mutex;

use serde_json::Value;

use crate::proto::IpcError;

#[cfg(any(unix, windows))]
use crate::framing::{decode_line, encode_line};
#[cfg(any(unix, windows))]
use crate::proto::{Envelope, FrameKind, IPC_VERSION};

/// Blocking JSONL client; the hello handshake runs inside `connect`.
pub struct Client {
    #[cfg(any(unix, windows))]
    link: Mutex<ClientLink>,
    #[cfg(not(any(unix, windows)))]
    link: NoLink,
}

#[cfg(unix)]
type Stream = std::os::unix::net::UnixStream;
#[cfg(windows)]
type Stream = std::fs::File;

#[cfg(any(unix, windows))]
struct ClientLink {
    reader: BufReader<Stream>,
    next_id: AtomicU64,
}

#[cfg(not(any(unix, windows)))]
struct NoLink;

/// Blocking reader of the events pushed to a subscribed connection.
pub struct EventStream {
    #[cfg(any(unix, windows))]
    reader: BufReader<Stream>,
}

#[cfg(any(unix, windows))]
fn write_line(reader: &mut BufReader<Stream>, bytes: &[u8]) -> Result<(), IpcError> {
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

#[cfg(any(unix, windows))]
fn read_frame(reader: &mut BufReader<Stream>) -> Result<Envelope, IpcError> {
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

#[cfg(any(unix, windows))]
fn hello(link: &mut ClientLink, token: &str) -> Result<(), IpcError> {
    let env = Envelope {
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
    let bytes = encode_line(&env)?;
    write_line(&mut link.reader, &bytes)?;
    let welcome = read_frame(&mut link.reader)?;
    if let Some(error) = welcome.error {
        return Err(error);
    }
    Ok(())
}

impl Client {
    /// Connect and run the hello handshake; a bad token fails with `auth_failed`.
    ///
    /// # Errors
    ///
    /// Returns the server's handshake error (notably `auth_failed`) or an I/O failure.
    #[cfg(unix)]
    pub fn connect(sock: &Path, token: &str) -> Result<Self, IpcError> {
        let stream = Stream::connect(sock)
            .map_err(|e| IpcError::new("connection_refused", e.to_string()))?;
        let mut link = ClientLink {
            reader: BufReader::new(stream),
            next_id: AtomicU64::new(0),
        };
        hello(&mut link, token)?;
        Ok(Self {
            link: Mutex::new(link),
        })
    }

    /// Connect via the rendezvous `sock`; pipe paths use `connect_pipe`.
    ///
    /// # Errors
    ///
    /// Returns the server's handshake error or an I/O failure.
    #[cfg(windows)]
    pub fn connect(sock: &Path, token: &str) -> Result<Self, IpcError> {
        let text: String = sock.as_os_str().to_string_lossy().into_owned();
        if text.starts_with(r"\\.\pipe\") {
            return Self::connect_pipe(&text, token);
        }
        Err(IpcError::new(
            "connection_refused",
            format!("not a pipe path: {text}"),
        ))
    }

    /// Connect and run the hello handshake; a bad token fails with `auth_failed`.
    ///
    /// # Errors
    ///
    /// Always fails with `unavailable`.
    #[cfg(not(any(unix, windows)))]
    pub fn connect(_sock: &Path, _token: &str) -> Result<Self, IpcError> {
        Err(IpcError::new(
            "unavailable",
            "ipc client needs a Unix socket".to_string(),
        ))
    }

    /// Connect to `pipe_name`, retrying busy pipes before the hello handshake.
    ///
    /// # Errors
    ///
    /// Returns `connection_refused` after the retry budget, or the server's
    /// handshake error (notably `auth_failed`).
    #[cfg(windows)]
    pub fn connect_pipe(pipe_name: &str, token: &str) -> Result<Self, IpcError> {
        let mut failure = IpcError::new("connection_refused", "pipe unavailable".to_string());
        for attempt in 0..25_u32 {
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(pipe_name)
            {
                Ok(file) => {
                    let mut link = ClientLink {
                        reader: BufReader::new(file),
                        next_id: AtomicU64::new(0),
                    };
                    hello(&mut link, token)?;
                    return Ok(Self {
                        link: Mutex::new(link),
                    });
                }
                Err(e) => {
                    failure = IpcError::new("connection_refused", e.to_string());
                    if attempt + 1 < 25 {
                        std::thread::sleep(std::time::Duration::from_millis(40));
                    }
                }
            }
        }
        Err(failure)
    }

    /// Call `method` with JSON params; handler failures surface with their code.
    ///
    /// # Errors
    ///
    /// Returns the server's error for the call or an I/O failure.
    #[cfg(any(unix, windows))]
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

    /// Call stub on platforms without Unix sockets or named pipes.
    ///
    /// # Errors
    ///
    /// Always fails with `unavailable`.
    #[cfg(not(any(unix, windows)))]
    pub fn call(&self, _method: &str, _params: Value) -> Result<Value, IpcError> {
        let _ = &self.link;
        Err(IpcError::new(
            "unavailable",
            "ipc client needs a Unix socket".to_string(),
        ))
    }

    /// Turn this connection into an event stream for `topics`. `extra`
    /// (a JSON object) travels with the subscription, e.g. the popup window.
    ///
    /// # Errors
    ///
    /// Returns the server's refusal (`unknown_topic`, `already_subscribed`,
    /// `denied`) or an I/O failure.
    #[cfg(any(unix, windows))]
    pub fn subscribe(self, topics: &[&str], extra: Value) -> Result<EventStream, IpcError> {
        let mut link = self
            .link
            .into_inner()
            .map_err(|_| IpcError::new("io_failed", "client lock poisoned".to_string()))?;
        let mut params = match extra {
            Value::Object(map) => map,
            _ => serde_json::Map::new(),
        };
        params.insert("topics".to_string(), Value::from(topics.to_vec()));
        let seq = link.next_id.fetch_add(1, Ordering::SeqCst);
        let id = format!("c{seq}");
        let req = Envelope {
            v: IPC_VERSION.to_string(),
            id: id.clone(),
            kind: FrameKind::Req,
            method: Some("events.subscribe".to_string()),
            params: Some(Value::Object(params)),
            result: None,
            error: None,
            seq: None,
        };
        let bytes = encode_line(&req)?;
        write_line(&mut link.reader, &bytes)?;
        loop {
            let frame = read_frame(&mut link.reader)?;
            if frame.kind != FrameKind::Res || frame.id != id {
                continue;
            }
            if let Some(error) = frame.error {
                return Err(error);
            }
            return Ok(EventStream {
                reader: link.reader,
            });
        }
    }

    /// Subscribe stub on platforms without Unix sockets or named pipes.
    ///
    /// # Errors
    ///
    /// Always fails with `unavailable`.
    #[cfg(not(any(unix, windows)))]
    pub fn subscribe(self, _topics: &[&str], _extra: Value) -> Result<EventStream, IpcError> {
        let _ = &self.link;
        Err(IpcError::new(
            "unavailable",
            "ipc client needs a Unix socket".to_string(),
        ))
    }
}

impl EventStream {
    /// Block until the next pushed event; returns its topic and params.
    ///
    /// # Errors
    ///
    /// Returns `connection_closed` when the daemon goes away, or a decode or
    /// I/O failure.
    #[cfg(any(unix, windows))]
    pub fn next_event(&mut self) -> Result<(String, Value), IpcError> {
        loop {
            let frame = read_frame(&mut self.reader)?;
            if frame.kind == FrameKind::Event {
                return Ok((
                    frame.method.unwrap_or_default(),
                    frame.params.unwrap_or(Value::Null),
                ));
            }
        }
    }
}
