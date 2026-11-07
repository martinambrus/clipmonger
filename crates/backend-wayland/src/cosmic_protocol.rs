//! Client bindings for COSMIC's `cosmic-toplevel-info-unstable-v1`.
//!
//! Generated at build time by `wayland-scanner` from the protocol file
//! vendored in `protocols/` (pop-os/cosmic-protocols commit
//! c0cff4db14c37ed954983158e4055aa94c7741d9, HPND-style license kept in
//! the file). The `cosmic-protocols` crate is not used: its crates.io
//! release is labelled GPL-3.0-only.
//!
//! The protocol extends `ext_foreign_toplevel_handle_v1` (from
//! `wayland-protocols`) and names `ext_workspace_handle_v1` (also from
//! `wayland-protocols`) and COSMIC's own `zcosmic_workspace_handle_v1`, for
//! which `protocols/cosmic-workspace-stub.xml` provides an empty stand-in.

#![allow(
    dead_code,
    non_camel_case_types,
    unused_unsafe,
    unused_variables,
    non_upper_case_globals,
    non_snake_case,
    unused_imports,
    missing_docs,
    unsafe_code,
    clippy::all,
    clippy::pedantic
)]

/// Stand-in for `zcosmic_workspace_handle_v1` (never bound).
pub mod workspace_stub {
    /// Client-side API.
    pub mod client {
        use wayland_client;
        use wayland_client::protocol::*;

        pub mod __interfaces {
            use wayland_client::backend as wayland_backend;
            use wayland_client::protocol::__interfaces::*;
            wayland_scanner::generate_interfaces!("protocols/cosmic-workspace-stub.xml");
        }
        use self::__interfaces::*;

        wayland_scanner::generate_client_code!("protocols/cosmic-workspace-stub.xml");
    }
}

/// `zcosmic_toplevel_info_v1` and `zcosmic_toplevel_handle_v1`.
pub mod toplevel_info {
    /// Client-side API.
    pub mod client {
        use super::super::workspace_stub::client::*;
        use wayland_client;
        use wayland_client::protocol::*;
        use wayland_protocols::ext::foreign_toplevel_list::v1::client::*;
        use wayland_protocols::ext::workspace::v1::client::*;

        pub mod __interfaces {
            use super::super::super::workspace_stub::client::__interfaces::*;
            use wayland_client::backend as wayland_backend;
            use wayland_client::protocol::__interfaces::*;
            use wayland_protocols::ext::foreign_toplevel_list::v1::client::__interfaces::*;
            use wayland_protocols::ext::workspace::v1::client::__interfaces::*;
            wayland_scanner::generate_interfaces!("protocols/cosmic-toplevel-info-unstable-v1.xml");
        }
        use self::__interfaces::*;

        wayland_scanner::generate_client_code!("protocols/cosmic-toplevel-info-unstable-v1.xml");
    }
}
