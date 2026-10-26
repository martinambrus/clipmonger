//! `.desktop` autostart entry rendering/parsing (pure; seg-05 §6).
//!
//! Format verified against `/etc/xdg/autostart` entries: a `[Desktop
//! Entry]` group with `Type=Application`, `Name`, `Exec`, `Hidden`, and
//! `Terminal=false`. The reader only parses what the renderer writes
//! (roundtrip), never shells out.

use clipmonger_platform::types::Error;
use serde::{Deserialize, Serialize};

/// Parsed autostart entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopEntry {
    /// Application name (`Name=`).
    pub name: String,
    /// Executable path as written in `Exec=` (unquoted).
    pub exec: String,
    /// Whether `--minimized` was present in `Exec=`.
    pub minimized: bool,
    /// The `Hidden=` flag.
    pub hidden: bool,
}

/// Render a `clipmonger.desktop` autostart entry for `exe`.
///
/// The executable is always double-quoted (paths may contain spaces) and
/// `--minimized` is appended when requested.
///
/// # Example
///
/// ```
/// let text = clipmonger_backend_x11::desktop::render_desktop("/opt/clipmonger/daemon", true, false);
/// assert!(text.contains("\"/opt/clipmonger/daemon\" --minimized"));
/// ```
#[must_use]
pub fn render_desktop(exe: &str, minimized: bool, hidden: bool) -> String {
    let mut exec = format!(r#""{exe}""#);
    if minimized {
        exec.push_str(" --minimized");
    }
    format!(
        "[Desktop Entry]\nType=Application\nName=Clipmonger\nExec={exec}\nHidden={hidden}\nTerminal=false\n",
        hidden = if hidden { "true" } else { "false" }
    )
}

/// Parse text previously produced by [`render_desktop`].
///
/// # Errors
///
/// Returns [`Error::Internal`] when the `[Desktop Entry]` header or a
/// usable `Exec=` value is missing.
pub fn parse_desktop(text: &str) -> Result<DesktopEntry, Error> {
    let mut lines = text.lines().map(str::trim);
    let header = lines.next().unwrap_or("");
    if header != "[Desktop Entry]" {
        return Err(Error::Internal(format!(
            "desktop entry missing [Desktop Entry] header, found {header:?}"
        )));
    }
    let mut name: Option<String> = None;
    let mut exec_raw: Option<String> = None;
    let mut hidden = false;
    for line in lines {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "Name" => name = Some(value.trim().to_string()),
            "Exec" => exec_raw = Some(value.trim().to_string()),
            "Hidden" => hidden = value.trim().eq_ignore_ascii_case("true"),
            _ => {}
        }
    }
    let exec_raw = exec_raw
        .filter(|v| !v.is_empty())
        .ok_or_else(|| Error::Internal("desktop entry missing Exec= value".to_string()))?;
    let (exec, minimized) = split_exec(&exec_raw);
    Ok(DesktopEntry {
        name: name.unwrap_or_default(),
        exec,
        minimized,
        hidden,
    })
}

/// Split an `Exec=` value into the (unquoted) executable and the
/// `--minimized` flag.
fn split_exec(raw: &str) -> (String, bool) {
    let raw = raw.trim();
    let (exe, rest) = if let Some(quoted) = raw.strip_prefix('"') {
        match quoted.find('"') {
            Some(end) => (quoted[..end].to_string(), &quoted[end + 1..]),
            None => (quoted.to_string(), ""),
        }
    } else {
        match raw.find(char::is_whitespace) {
            Some(end) => (raw[..end].to_string(), &raw[end..]),
            None => (raw.to_string(), ""),
        }
    };
    let minimized = rest
        .split(char::is_whitespace)
        .any(|arg| arg == "--minimized");
    (exe, minimized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_minimized_visible() {
        let entry = parse_desktop(&render_desktop("/usr/bin/clipmonger-daemon", true, false))
            .expect("render output must parse");
        assert_eq!(
            entry,
            DesktopEntry {
                name: "Clipmonger".to_string(),
                exec: "/usr/bin/clipmonger-daemon".to_string(),
                minimized: true,
                hidden: false,
            }
        );
    }

    #[test]
    fn roundtrip_hidden_toggle() {
        let hidden = parse_desktop(&render_desktop("/usr/bin/clipmonger-daemon", false, true))
            .expect("render output must parse");
        assert!(hidden.hidden);
        assert!(!hidden.minimized);
        let shown = parse_desktop(&render_desktop("/usr/bin/clipmonger-daemon", false, false))
            .expect("render output must parse");
        assert!(!shown.hidden);
    }

    #[test]
    fn exec_with_spaces_stays_quoted() {
        let text = render_desktop("/opt/My Apps/clipmonger daemon", true, false);
        assert!(text.contains(r#""/opt/My Apps/clipmonger daemon" --minimized"#));
        let entry = parse_desktop(&text).expect("spaced path must parse");
        assert_eq!(entry.exec, "/opt/My Apps/clipmonger daemon");
        assert!(entry.minimized);
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(parse_desktop("").is_err());
        assert!(parse_desktop("[Other]\nExec=/bin/x\n").is_err());
        assert!(parse_desktop("[Desktop Entry]\nName=Clipmonger\n").is_err());
        assert!(parse_desktop("[Desktop Entry]\nExec=\n").is_err());
    }
}
