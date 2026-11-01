//! Foreground application query: `_NET_ACTIVE_WINDOW` → `_NET_WM_PID` →
//! `/proc/<pid>/exe` (`std::fs` only), falling back to `WM_CLASS`.
//! Without EWMH (bare window managers) there is no active window.

use clipmonger_platform::traits::ForegroundApp;
use clipmonger_platform::types::{Affinity, AppId, Error, PasteTarget, PlatformHandle, Result};
use x11rb14::connection::Connection as _;
use x11rb14::protocol::xproto::{self, Window};

use crate::session;
use crate::session::Atoms;

/// X11 foreground query.
pub struct X11Foreground {
    display: Option<String>,
}

impl X11Foreground {
    /// Bind to `display` (or `$DISPLAY`).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unavailable`] without an X session.
    pub fn connect(display: Option<&str>) -> Result<Self> {
        #[cfg(not(unix))]
        {
            let _ = display;
            return Err(Error::Unavailable(
                "x11 foreground needs a Unix X11 session".to_string(),
            ));
        }
        #[cfg(unix)]
        {
            Ok(Self {
                display: session::display_name(display),
            })
        }
    }

    /// Currently active window id, if the server reports one.
    fn active_window(&self) -> Option<Window> {
        let (conn, screen) = session::connect(self.display.as_deref()).ok()?;
        let atoms = Atoms::intern(&conn).ok()?;
        let root = conn.setup().roots[screen].root;
        let reply = xproto::get_property(&conn, false, root, atoms.net_active, 0u32, 0, 1)
            .ok()?
            .reply()
            .ok()?;
        if reply.format != 32 || reply.value.len() < 4 {
            return None;
        }
        let win = Window::from_ne_bytes([
            reply.value[0],
            reply.value[1],
            reply.value[2],
            reply.value[3],
        ]);
        if win == 0 {
            return None;
        }
        Some(win)
    }

    /// Executable name for `win` via `_NET_WM_PID` + `/proc`, else `WM_CLASS`.
    fn app_id(&self, win: Window) -> Option<AppId> {
        let (conn, _) = session::connect(self.display.as_deref()).ok()?;
        let atoms = Atoms::intern(&conn).ok()?;
        let mut exe: Option<String> =
            xproto::get_property(&conn, false, win, atoms.net_wm_pid, 0u32, 0, 1)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .filter(|reply| reply.format == 32 && reply.value.len() >= 4)
                .map(|reply| {
                    u32::from_ne_bytes([
                        reply.value[0],
                        reply.value[1],
                        reply.value[2],
                        reply.value[3],
                    ])
                })
                .and_then(|pid| {
                    std::fs::read_link(format!("/proc/{pid}/exe"))
                        .ok()
                        .and_then(|path| {
                            path.file_name()
                                .and_then(|name| name.to_str())
                                .map(str::to_string)
                        })
                });
        if exe.is_none() {
            exe = xproto::get_property(&conn, false, win, atoms.wm_class, 0u32, 0, 64)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .and_then(|reply| {
                    let text = String::from_utf8_lossy(&reply.value).into_owned();
                    text.split('\0')
                        .nth(1)
                        .or_else(|| text.split('\0').next())
                        .map(|class: &str| class.to_string())
                });
        }
        let title: Vec<u8> =
            xproto::get_property(&conn, false, win, atoms.net_wm_name, 0u32, 0, 256)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .map_or_else(Vec::new, |reply| reply.value);
        Some(AppId {
            exe: exe.unwrap_or_default(),
            title_hash: fnv1a(&title),
        })
    }

    /// Capture the current paste target (before the popup opens).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unavailable`] when no active window is known.
    pub fn capture_target(&self) -> Result<PasteTarget> {
        let win = self
            .active_window()
            .ok_or_else(|| Error::Unavailable("no active X11 window".to_string()))?;
        let app = self
            .app_id(win)
            .ok_or_else(|| Error::Unavailable("active window has no app id".to_string()))?;
        let title_hash = app.title_hash;
        Ok(PasteTarget {
            app,
            window_title_hash: title_hash,
            handle: PlatformHandle::X11Window(u64::from(win)),
            captured_ms: now_ms(),
        })
    }
}

impl ForegroundApp for X11Foreground {
    const AFFINITY: Affinity = Affinity::Worker;

    fn active(&self) -> Option<AppId> {
        self.active_window().and_then(|win| self.app_id(win))
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// FNV-1a (64-bit) title hash.
fn fnv1a(bytes: &[u8]) -> u64 {
    clipmonger_core::hash::fnv1a64(bytes)
}

#[cfg(test)]
mod tests {
    use super::fnv1a;

    #[test]
    fn fnv1a_vectors_match_core_and_known_hashes() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a(b"foobar"), 0x8594_4171_f739_67e8);
        assert_eq!(fnv1a(b"fo"), 0x0898_5907_b541_d342);
        assert_eq!(format!("{:016x}", fnv1a(b"fo")), "08985907b541d342");
        assert_eq!(fnv1a(b"fo"), clipmonger_core::hash::fnv1a64(b"fo"));
    }

    #[test]
    fn fnv1a_title_hash_is_byte_oriented() {
        assert_eq!(fnv1a(b"hello"), 0xa430_d846_80aa_bd0b);
        assert_eq!(fnv1a(b""), clipmonger_core::hash::FNV_OFFSET_BASIS);
        assert_eq!(fnv1a(b"test"), clipmonger_core::hash::fnv1a64(b"test"));
        assert_eq!(
            fnv1a("café".as_bytes()),
            clipmonger_core::hash::fnv1a64("café".as_bytes())
        );
    }
}
