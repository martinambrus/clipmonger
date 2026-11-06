//! Clipmonger core: clip model, store, config, search, history policy.
//! No tokio in this crate (seg-02 S8).

pub mod config;
pub mod entropy;
pub mod error;
pub mod hash;
pub mod model;
pub mod policy;
pub mod runtime_log;
mod sql_text_stream;
pub mod store;
pub(crate) mod substring_matcher;
pub mod uri_search;

pub use config::{AppPaths, Config, PasteDelivery};
pub use error::{BackendKind, CoreError};
pub use store::{GcReport, InsertOutcome, PayloadSnapshot, PermGroup, Store};

pub const APP_IDENTITY: &str = "clipmonger";

/// Desktop application id (reverse DNS): the `.desktop` file name, the
/// Wayland window `app_id` and the id the daemon registers with the portal.
pub const DESKTOP_APP_ID: &str = "com.martinambrus.clipmonger";

/// Desktop id of the daemon's hidden entry, which declares the `KWin`
/// interfaces the daemon uses.
pub const DESKTOP_DAEMON_ID: &str = "com.martinambrus.clipmonger.daemon";
