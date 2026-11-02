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
    /// Absolute profile directory when present.
    #[serde(default)]
    pub profile: Option<String>,
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

fn validate_desktop_path(path: &str) -> Result<(), Error> {
    if path.is_empty() {
        return Err(Error::Internal("desktop path is empty".to_string()));
    }
    if path.contains('\0') {
        return Err(Error::Internal("desktop path contains NUL".to_string()));
    }
    if path.contains('\r') || path.contains('\n') {
        return Err(Error::Internal("desktop path contains CR/LF".to_string()));
    }
    if !path.starts_with('/') {
        return Err(Error::Internal("desktop path must be absolute".to_string()));
    }
    Ok(())
}

fn encode_exec_arg(arg: &str) -> Result<String, Error> {
    validate_desktop_path(arg)?;
    let mut inner = String::new();
    for ch in arg.chars() {
        match ch {
            '%' => inner.push_str("%%"),
            '\\' => {
                inner.push('\\');
                inner.push('\\');
                inner.push('\\');
                inner.push('\\');
            }
            '"' | '`' | '$' => {
                inner.push('\\');
                inner.push('\\');
                inner.push(ch);
            }
            _ => inner.push(ch),
        }
    }
    Ok(format!("\"{inner}\""))
}

/// Render with profile support, fallible.
///
/// Validates `exe` and `profile_dir` for unrepresentable characters
/// (NUL/CR/LF) and encodes them with two-layer desktop escaping.
/// Returns `Err` rather than losing data via lossy conversion.
///
/// # Errors
///
/// Returns an error if a path is empty, non-absolute, or contains NUL, CR,
/// or LF, or if the executable path contains `=`.
pub fn try_render_desktop(
    exe: &str,
    minimized: bool,
    hidden: bool,
    profile: Option<&str>,
) -> Result<String, Error> {
    validate_desktop_path(exe)?;
    if exe.contains('=') {
        return Err(Error::Unavailable(
            "desktop executable contains '='".to_string(),
        ));
    }
    if let Some(p) = profile {
        validate_desktop_path(p)?;
    }
    let mut exec = encode_exec_arg(exe)?;
    if minimized {
        exec.push_str(" --minimized");
    }
    if let Some(p) = profile {
        exec.push_str(" --profile-dir ");
        exec.push_str(&encode_exec_arg(p)?);
    }
    Ok(format!(
        "[Desktop Entry]\nType=Application\nName=Clipmonger\nExec={exec}\nHidden={hidden}\nTerminal=false\n",
        hidden = if hidden { "true" } else { "false" }
    ))
}

/// Render with a profile directory, rejecting invalid arguments.
///
/// # Errors
///
/// Returns an error for paths rejected by [`try_render_desktop`].
pub fn render_desktop_with_profile(
    exe: &str,
    minimized: bool,
    hidden: bool,
    profile_dir: &str,
) -> Result<String, Error> {
    try_render_desktop(exe, minimized, hidden, Some(profile_dir))
}

fn string_decode(input: &str) -> Result<String, Error> {
    let mut out = String::new();
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some(other) => {
                    return Err(Error::Internal(format!("invalid string escape \\{other}")))
                }
                None => {
                    return Err(Error::Internal(
                        "trailing backslash in Exec value".to_string(),
                    ))
                }
            }
        } else {
            out.push(c);
        }
    }
    Ok(out)
}

fn exec_tokenize(input: &str) -> Result<Vec<(String, bool)>, Error> {
    let mut tokens: Vec<(String, bool)> = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    let mut chars = input.chars().peekable();
    let mut token_started = false;
    let mut quote_closed = false;

    while let Some(c) = chars.next() {
        if in_quote {
            if c == '\\' {
                match chars.next() {
                    Some('"') => current.push('"'),
                    Some('`') => current.push('`'),
                    Some('$') => current.push('$'),
                    Some('\\') => current.push('\\'),
                    Some(other) => {
                        return Err(Error::Internal(format!("invalid Exec escape \\{other}")))
                    }
                    None => {
                        return Err(Error::Internal(
                            "trailing backslash in quoted Exec arg".to_string(),
                        ))
                    }
                }
                token_started = true;
            } else if c == '"' {
                in_quote = false;
                quote_closed = true;
            } else if c == '%' {
                if chars.peek() == Some(&'%') {
                    current.push('%');
                    chars.next();
                    token_started = true;
                } else {
                    return Err(Error::Internal(
                        "unescaped % in Exec quoted arg".to_string(),
                    ));
                }
            } else if c == '$' || c == '`' {
                return Err(Error::Internal(
                    "unescaped reserved character in Exec".to_string(),
                ));
            } else {
                current.push(c);
                token_started = true;
            }
        } else {
            if c.is_whitespace() {
                if token_started {
                    tokens.push((std::mem::take(&mut current), quote_closed));
                    token_started = false;
                    quote_closed = false;
                }
            } else if quote_closed {
                return Err(Error::Internal(
                    "text glued to quoted Exec token".to_string(),
                ));
            } else if c == '"' {
                if token_started {
                    return Err(Error::Internal(
                        "unexpected quote inside unquoted Exec token".to_string(),
                    ));
                }
                in_quote = true;
                token_started = true;
            } else if matches!(
                c,
                '\\' | '%'
                    | '\''
                    | '>'
                    | '<'
                    | '~'
                    | '|'
                    | '&'
                    | ';'
                    | '$'
                    | '*'
                    | '?'
                    | '#'
                    | '('
                    | ')'
                    | '`'
            ) {
                return Err(Error::Internal(
                    "reserved character in unquoted Exec token".to_string(),
                ));
            } else {
                current.push(c);
                token_started = true;
            }
        }
    }
    if in_quote {
        return Err(Error::Internal("unclosed quote in Exec".to_string()));
    }
    if token_started {
        tokens.push((current, quote_closed));
    }
    Ok(tokens)
}

/// Parse text previously produced by [`render_desktop`].
///
/// # Errors
///
/// Returns [`Error::Internal`] when the `[Desktop Entry]` header or a
/// usable `Exec=` value is missing.
#[allow(clippy::too_many_lines)] // Keep desktop and Exec validation in one ordered parse.
pub fn parse_desktop(text: &str) -> Result<DesktopEntry, Error> {
    if text.contains('\0') || text.contains('\r') {
        return Err(Error::Internal(
            "desktop entry contains NUL or CR".to_string(),
        ));
    }
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
    let mut hidden_seen = false;
    for line in lines {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.contains('\r') || line.contains('\n') {
            return Err(Error::Internal("desktop line contains CR/LF".to_string()));
        }
        if line.starts_with('[') || line.ends_with(']') {
            return Err(Error::Internal("unexpected desktop group".to_string()));
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(Error::Internal("malformed desktop line".to_string()));
        };
        match key.trim() {
            "Name" if name.is_none() => name = Some(value.trim().to_string()),
            "Exec" if exec_raw.is_none() => exec_raw = Some(value.trim().to_string()),
            "Hidden" if !hidden_seen => {
                hidden_seen = true;
                hidden = match value.trim() {
                    "true" => true,
                    "false" => false,
                    _ => return Err(Error::Internal("invalid Hidden value".to_string())),
                };
            }
            "Name" | "Exec" | "Hidden" => {
                return Err(Error::Internal("duplicate desktop key".to_string()))
            }
            _ => {}
        }
    }
    let exec_raw = exec_raw
        .filter(|v| !v.is_empty())
        .ok_or_else(|| Error::Internal("desktop entry missing Exec= value".to_string()))?;
    if exec_raw.contains('\0') {
        return Err(Error::Internal("Exec contains NUL".to_string()));
    }
    let decoded = string_decode(&exec_raw)?;
    let tokens = exec_tokenize(&decoded)?;
    if tokens.is_empty() {
        return Err(Error::Internal("Exec has no tokens".to_string()));
    }
    let exec = tokens[0].0.clone();
    validate_desktop_path(&exec)?;
    if exec.contains('=') {
        return Err(Error::Internal("invalid desktop executable".to_string()));
    }
    let mut minimized = false;
    let mut profile: Option<String> = None;
    let mut idx = 1;
    while idx < tokens.len() {
        if tokens[idx].1 {
            return Err(Error::Internal("quoted Exec flag".to_string()));
        }
        match tokens[idx].0.as_str() {
            "--minimized" => {
                if minimized {
                    return Err(Error::Internal("duplicate --minimized".to_string()));
                }
                minimized = true;
                idx += 1;
            }
            "--profile-dir" => {
                if profile.is_some() {
                    return Err(Error::Internal("duplicate --profile-dir".to_string()));
                }
                idx += 1;
                if idx >= tokens.len() || !tokens[idx].1 || tokens[idx].0.starts_with("--") {
                    return Err(Error::Internal(
                        "missing value for --profile-dir".to_string(),
                    ));
                }
                validate_desktop_path(&tokens[idx].0)?;
                profile = Some(tokens[idx].0.clone());
                idx += 1;
            }
            other => {
                return Err(Error::Internal(format!("unexpected Exec token {other:?}")));
            }
        }
    }
    Ok(DesktopEntry {
        name: name.unwrap_or_default(),
        exec,
        minimized,
        hidden,
        profile,
    })
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
                profile: None,
            }
        );
    }

    #[test]
    fn roundtrip_hidden_toggle() {
        let hidden = parse_desktop(&render_desktop("/usr/bin/clipmonger-daemon", false, true))
            .expect("render output must parse");
        assert!(hidden.hidden);
        assert!(!hidden.minimized);
        assert!(hidden.profile.is_none());
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
        assert!(entry.profile.is_none());
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(parse_desktop("").is_err());
        assert!(parse_desktop("[Other]\nExec=/bin/x\n").is_err());
        assert!(parse_desktop("[Desktop Entry]\nName=Clipmonger\n").is_err());
        assert!(parse_desktop("[Desktop Entry]\nExec=\n").is_err());
    }

    #[test]
    fn roundtrip_with_profile_simple() {
        let text = try_render_desktop(
            "/usr/bin/clipmonger",
            true,
            false,
            Some("/home/alice/.config/clipmonger/profiles/work"),
        )
        .unwrap();
        let entry = parse_desktop(&text).unwrap();
        assert_eq!(entry.exec, "/usr/bin/clipmonger");
        assert!(entry.minimized);
        assert_eq!(
            entry.profile,
            Some("/home/alice/.config/clipmonger/profiles/work".to_string())
        );
    }

    #[test]
    fn roundtrip_with_reserved_chars() {
        let exe = "/opt/My Apps/clipmonger";
        let profile = "/home/alice/profiles/foo%bar$baz\\qux`tick\"quote";
        let text = try_render_desktop(exe, true, false, Some(profile)).unwrap();
        assert_eq!(
            text,
            r#"[Desktop Entry]
Type=Application
Name=Clipmonger
Exec="/opt/My Apps/clipmonger" --minimized --profile-dir "/home/alice/profiles/foo%%bar\\$baz\\\\qux\\`tick\\"quote"
Hidden=false
Terminal=false
"#
        );
        let entry = parse_desktop(&text).unwrap();
        assert_eq!(entry.exec, exe);
        assert_eq!(entry.profile.as_deref(), Some(profile));
        assert!(entry.minimized);
    }

    #[test]
    fn literal_percent_backslash_dollar() {
        let profile = "/tmp/a%b\\c$d`e\"f";
        let text = try_render_desktop("/usr/bin/clipmonger", true, false, Some(profile)).unwrap();
        let entry = parse_desktop(&text).unwrap();
        assert_eq!(entry.profile.as_deref(), Some(profile));
    }

    #[test]
    fn trailing_backslash_roundtrip() {
        let profile = "/home/alice/profiles/work\\";
        let text = try_render_desktop("/usr/bin/clipmonger", true, false, Some(profile)).unwrap();
        assert_eq!(
            text,
            r#"[Desktop Entry]
Type=Application
Name=Clipmonger
Exec="/usr/bin/clipmonger" --minimized --profile-dir "/home/alice/profiles/work\\\\"
Hidden=false
Terminal=false
"#
        );
        let entry = parse_desktop(&text).unwrap();
        assert_eq!(entry.profile.as_deref(), Some(profile));
    }

    #[test]
    fn rejects_nul_cr_lf_in_render() {
        assert!(try_render_desktop("/usr/bin/clipmonger\0", true, false, None).is_err());
        assert!(
            try_render_desktop("/usr/bin/clipmonger", true, false, Some("/bad\npath")).is_err()
        );
        assert!(
            try_render_desktop("/usr/bin/clipmonger", true, false, Some("/bad\rpath")).is_err()
        );
    }

    #[test]
    fn rejects_malformed_exec() {
        // unclosed quote
        assert!(parse_desktop(
            "[Desktop Entry]\nExec=\"/usr/bin/clipmonger\nHidden=false\nTerminal=false\n"
        )
        .is_err());
        // unknown field code
        assert!(parse_desktop(
            "[Desktop Entry]\nExec=\"/usr/bin/clipmonger\" %f\nHidden=false\nTerminal=false\n"
        )
        .is_err());
        // Unrelated well-formed keys remain tolerated.
        assert!(parse_desktop("[Desktop Entry]\nExec=\"/usr/bin/clipmonger\" --minimized\nHidden=false\nTerminal=false\nInjected=bad").is_ok());
        let injection = "[Desktop Entry]\nExec=\"/usr/bin/a\" --minimized\r\nHidden=false\n";
        assert!(parse_desktop(injection).is_err());
        // unescaped % inside quoted arg should be rejected
        let bad_percent = "[Desktop Entry]\nExec=\"/usr/bin/clipmonger\" --profile-dir \"/tmp/a%b\"\nHidden=false\nTerminal=false\n";
        assert!(parse_desktop(bad_percent).is_err());
    }

    #[test]
    fn exact_minimized_not_substring() {
        let profile = "/home/alice/my--minimized-profile";
        let text = try_render_desktop("/usr/bin/clipmonger", true, false, Some(profile)).unwrap();
        let entry = parse_desktop(&text).unwrap();
        assert!(entry.minimized);
        assert_eq!(entry.profile.as_deref(), Some(profile));
        // token count ensures --minimized is separate
        let decoded = string_decode(
            text.lines().find(|l| l.starts_with("Exec=")).unwrap()["Exec=".len()..].trim(),
        )
        .unwrap();
        let tokens = exec_tokenize(&decoded).unwrap();
        assert_eq!(
            tokens
                .iter()
                .filter(|(t, _)| t.as_str() == "--minimized")
                .count(),
            1
        );
    }

    #[test]
    fn legacy_without_profile_still_none() {
        let text = render_desktop("/usr/bin/clipmonger", true, false);
        let entry = parse_desktop(&text).unwrap();
        assert!(entry.profile.is_none());
    }
    #[test]
    fn strict_independent_exec_vectors() {
        let input = r#"[Desktop Entry]
Name=Clipmonger
Exec="/opt/é app" --minimized --profile-dir "/tmp/a%%b\\\\c\\$d\\`e\\"f\\\\"
Hidden=false
"#;
        let entry = parse_desktop(input).unwrap();
        assert_eq!(entry.exec, "/opt/é app");
        assert_eq!(entry.profile.as_deref(), Some("/tmp/a%b\\c$d`e\"f\\"));
        assert!(entry.minimized);
        for exec in [
            r#""/bin/a"suffix"#,
            r#""/bin/a" --minimized --minimized"#,
            r#""/bin/a" --profile-dir "/a" --profile-dir "/b""#,
            r#""/bin/a" --profile-dir --minimized"#,
            r#""/bin/a" --profile-dir "/unclosed"#,
            r#""/bin/a" %f"#,
            r#""/bin/a" --profile-dir "/bad\q""#,
        ] {
            assert!(
                parse_desktop(&format!("[Desktop Entry]\nExec={exec}\n")).is_err(),
                "{exec}"
            );
        }
        for input in [
            "[Desktop Entry]\nExec=\"/bin/a\"\n[Other]\nHidden=false\n",
            "[Desktop Entry]\nExec=\"/bin/a\"\n[Other\nHidden=false\n",
            "[Desktop Entry]\nExec=\"/bin/a\"\nExec=\"/bin/b\"\n",
            "[Desktop Entry]\nExec=\"/bin/a\"\r\nHidden=false\n",
        ] {
            assert!(parse_desktop(input).is_err(), "{input:?}");
        }
        assert!(try_render_desktop("/opt/a=b", true, false, Some("/p")).is_err());
        assert!(try_render_desktop("/opt/a", true, false, Some("/p=a")).is_ok());
    }

    #[test]
    fn supported_exec_grammar_is_enforced_by_parser() {
        for input in [
            "[Desktop Entry]\nExec=\"/usr/bin/clipmonger-daemon\" --minimized\n",
            "[Desktop Entry]\nExec=/usr/bin/clipmonger-daemon --minimized\n",
        ] {
            let entry = parse_desktop(input).unwrap();
            assert_eq!(entry.exec, "/usr/bin/clipmonger-daemon");
            assert!(entry.minimized);
            assert_eq!(entry.profile, None);
        }
        let entry = parse_desktop("[Desktop Entry]\nExec=\"/opt/My Apps/clipmonger\" --minimized --profile-dir \"/tmp/My Profile\"\n").unwrap();
        assert_eq!(entry.exec, "/opt/My Apps/clipmonger");
        assert_eq!(entry.profile.as_deref(), Some("/tmp/My Profile"));
        assert!(entry.minimized);
        for exec in [
            "relative",
            "\"relative\" --minimized",
            "\"/bin/a\" --profile-dir /tmp/$bad",
            "\"/bin/a\" --profile-dir /tmp/plain",
            "/tmp/$bad --minimized",
            "/tmp/a;bad --minimized",
            "/tmp/a`bad --minimized",
            "/tmp/a&bad --minimized",
            "/tmp/a'bad --minimized",
            "\"/bin/a\" \"--minimized\"",
        ] {
            assert!(
                parse_desktop(&format!("[Desktop Entry]\nExec={exec}\n")).is_err(),
                "{exec}"
            );
        }
    }
}
