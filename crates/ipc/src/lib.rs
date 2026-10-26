//! Clipmonger IPC: proto (serde, no runtime) + client (std) + server (tokio, `server` feature).

pub const IPC_VERSION: &str = "ipc/v1";

pub mod client;
pub mod framing;
pub mod proto;
#[cfg(feature = "server")]
pub mod server;
