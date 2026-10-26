//! Clipmonger core: clip model, store, config, search, history policy.
//! No tokio in this crate (seg-02 S8).

pub mod config;
pub mod error;
pub mod hash;
pub mod model;
pub mod policy;

pub use config::{AppPaths, Config};
pub use error::{BackendKind, CoreError};

pub const APP_IDENTITY: &str = "clipmonger";
