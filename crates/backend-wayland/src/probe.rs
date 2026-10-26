//! Protocol probe state machine (pure; seg-06 S1/S3, T-601).
//!
//! Probe order at backend start (and on `caps.refresh`): ext-data-control
//! (`ext_data_control_manager_v1`) → wlr-data-control
//! (`zwlr_data_control_manager_v1`) → none. The impure `probe::run()` (a
//! later session task) feeds this machine from the registry dump plus
//! `wl_clipboard_rs::utils::is_primary_selection_supported()`.
//!
//! Primary rule (seg-06 S1): primary needs ext (any version) or wlr-v2+.
//! The `primary_ext` input carries the runtime
//! `is_primary_selection_supported()` verdict; it is authoritative but only
//! meaningful when a capable protocol is present, so it is never inferred
//! from wlr-v1 (or from no protocol at all).

/// Which data-control protocol the compositor offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Protocol {
    /// `ext_data_control_manager_v1` present (any version).
    Ext,
    /// Only `zwlr_data_control_manager_v1` present.
    Wlr,
    /// Neither protocol: no out-of-process capture (e.g. `WSLg`, GNOME ≤48).
    #[default]
    None,
}

/// Observed registry facts feeding one probe step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Globals {
    /// `ext_data_control_manager_v1` version, when advertised.
    pub ext: Option<u32>,
    /// `zwlr_data_control_manager_v1` version, when advertised.
    pub wlr: Option<u32>,
    /// Runtime `is_primary_selection_supported()` verdict, when known.
    pub primary_ext: bool,
}

/// Probe outcome: chosen protocol plus derived capability flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeState {
    /// Winning protocol (ext → wlr → none).
    pub protocol: Protocol,
    /// Advertised ext version, when present.
    pub ext_version: Option<u32>,
    /// Advertised wlr version, when present.
    pub wlr_version: Option<u32>,
    /// Primary selection usable (check verdict gated by protocol support).
    pub primary_supported: bool,
}

impl Default for ProbeState {
    fn default() -> Self {
        Self {
            protocol: Protocol::None,
            ext_version: None,
            wlr_version: None,
            primary_supported: false,
        }
    }
}

/// Fold one registry observation into a probe state.
///
/// `state` is the previous step (kept for the fold-style registry walk);
/// the result is recomputed from `globals`: ext wins over wlr, and
/// `primary_supported = primary_ext && (ext.is_some() || wlr >= 2)`.
#[must_use]
pub fn probe_step(_state: &ProbeState, globals: &Globals) -> ProbeState {
    let protocol = if globals.ext.is_some() {
        Protocol::Ext
    } else if globals.wlr.is_some() {
        Protocol::Wlr
    } else {
        Protocol::None
    };
    let capable = globals.ext.is_some() || globals.wlr.unwrap_or(0) >= 2;
    ProbeState {
        protocol,
        ext_version: globals.ext,
        wlr_version: globals.wlr,
        primary_supported: globals.primary_ext && capable,
    }
}

/// True when any data-control protocol is available for capture/transfer.
#[must_use]
pub fn has_data_control(state: &ProbeState) -> bool {
    state.protocol != Protocol::None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn globals(ext: Option<u32>, wlr: Option<u32>, primary_ext: bool) -> Globals {
        Globals {
            ext,
            wlr,
            primary_ext,
        }
    }

    fn step(ext: Option<u32>, wlr: Option<u32>, primary_ext: bool) -> ProbeState {
        probe_step(&ProbeState::default(), &globals(ext, wlr, primary_ext))
    }

    #[test]
    fn t601_ext_wins_over_wlr_regardless_of_versions() {
        // (ext, any, *) -> Ext.
        let s = step(Some(1), Some(2), true);
        assert_eq!(s.protocol, Protocol::Ext);
        assert!(s.primary_supported);
        let s = step(Some(1), None, false);
        assert_eq!(s.protocol, Protocol::Ext);
        assert!(!s.primary_supported);
    }

    #[test]
    fn t601_wlr_v1_is_regular_only() {
        // (none, wlr v1) -> Wlr regular-only; primary never from wlr v1.
        let s = step(None, Some(1), false);
        assert_eq!(s.protocol, Protocol::Wlr);
        assert!(!s.primary_supported);
        // Even a stray `true` verdict cannot enable primary on wlr-v1.
        let s = step(None, Some(1), true);
        assert_eq!(s.protocol, Protocol::Wlr);
        assert!(!s.primary_supported);
    }

    #[test]
    fn t601_wlr_v2_plus_primary_check_enables_primary() {
        // (none, wlr>=2, primary) -> Wlr + primary.
        let s = step(None, Some(2), true);
        assert_eq!(s.protocol, Protocol::Wlr);
        assert!(s.primary_supported);
        let s = step(None, Some(2), false);
        assert_eq!(s.protocol, Protocol::Wlr);
        assert!(!s.primary_supported);
    }

    #[test]
    fn t601_none_means_unavailable_monitor() {
        let s = step(None, None, false);
        assert_eq!(s.protocol, Protocol::None);
        assert!(!s.primary_supported);
        assert!(!has_data_control(&s));
        // No protocol: primary verdict is meaningless.
        let s = step(None, None, true);
        assert!(!s.primary_supported);
    }

    #[test]
    fn step_is_idempotent_fold() {
        let g = globals(Some(3), Some(1), true);
        let first = probe_step(&ProbeState::default(), &g);
        let second = probe_step(&first, &g);
        assert_eq!(first, second);
        assert!(has_data_control(&first));
    }
}
