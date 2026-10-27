//! Clipmonger core: clip model, store, config, search, history policy.
//! No tokio in this crate (seg-02 S8).

pub mod config;
pub mod error;
pub mod hash;
pub mod model;
pub mod policy;
pub mod store;

pub use config::{AppPaths, Config, PasteDelivery};
pub use error::{BackendKind, CoreError};
pub use store::{GcReport, InsertOutcome, PermGroup, Store};

pub const APP_IDENTITY: &str = "clipmonger";
