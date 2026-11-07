//! `LaunchAgent` property list render/parse (pure, T-802). Only the keys we
//! write are understood; no plist crate.

use std::fmt::Write as _;

use clipmonger_platform::types::{Error, Result};

/// The keys of our `LaunchAgent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    /// `Label`.
    pub label: String,
    /// `ProgramArguments` (program first).
    pub args: Vec<String>,
    /// `RunAtLoad`.
    pub run_at_load: bool,
}

/// Escape XML text (`&`, `<`, `>`, `"`).
#[must_use]
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Render the agent plist.
///
/// # Errors
///
/// [`Error::Unsupported`] when a value holds a NUL or other control
/// character XML 1.0 cannot carry.
pub fn render(agent: &Agent) -> Result<String> {
    for value in std::iter::once(&agent.label).chain(agent.args.iter()) {
        if value.chars().any(|c| c.is_control() && c != '\t') {
            return Err(Error::Unsupported(
                "launch agent value has control characters".to_string(),
            ));
        }
    }
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n",
    );
    let _ = write!(
        out,
        "\t<key>Label</key>\n\t<string>{}</string>\n",
        escape(&agent.label)
    );
    out.push_str("\t<key>ProgramArguments</key>\n\t<array>\n");
    for arg in &agent.args {
        let _ = writeln!(out, "\t\t<string>{}</string>", escape(arg));
    }
    out.push_str("\t</array>\n");
    let _ = writeln!(
        out,
        "\t<key>RunAtLoad</key>\n\t<{}/>",
        if agent.run_at_load { "true" } else { "false" }
    );
    out.push_str("\t<key>ProcessType</key>\n\t<string>Interactive</string>\n");
    out.push_str("\t<key>LimitLoadToSessionType</key>\n\t<string>Aqua</string>\n");
    out.push_str("</dict>\n</plist>\n");
    Ok(out)
}

/// Text after `<key>name</key>` (skipping whitespace), if the key exists.
fn after_key<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let marker = format!("<key>{name}</key>");
    let at = text.find(&marker)? + marker.len();
    Some(text[at..].trim_start())
}

/// Content of the `<string>` element at the start of `text`.
fn leading_string(text: &str) -> Option<(String, &str)> {
    let rest = text.strip_prefix("<string>")?;
    let end = rest.find("</string>")?;
    Some((unescape(&rest[..end]), &rest[end + "</string>".len()..]))
}

/// Parse the keys we write.
///
/// # Errors
///
/// [`Error::Internal`] when `Label` or `ProgramArguments` is missing or
/// malformed.
pub fn parse(text: &str) -> Result<Agent> {
    let bad = |what: &str| Error::Internal(format!("launch agent plist: {what}"));
    let label = after_key(text, "Label")
        .and_then(leading_string)
        .map(|(s, _)| s)
        .ok_or_else(|| bad("no Label"))?;
    let mut rest = after_key(text, "ProgramArguments")
        .and_then(|t| t.strip_prefix("<array>"))
        .ok_or_else(|| bad("no ProgramArguments"))?;
    let mut args = Vec::new();
    loop {
        rest = rest.trim_start();
        if rest.starts_with("</array>") {
            break;
        }
        let (arg, next) = leading_string(rest).ok_or_else(|| bad("bad ProgramArguments"))?;
        args.push(arg);
        rest = next;
    }
    let run_at_load = after_key(text, "RunAtLoad").is_some_and(|t| t.starts_with("<true/>"));
    Ok(Agent {
        label,
        args,
        run_at_load,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent() -> Agent {
        Agent {
            label: "com.martinambrus.clipmonger".into(),
            args: vec![
                "/Applications/Clip & <Monger>.app/Contents/MacOS/clipmonger-ui".into(),
                "--minimized".into(),
                "--profile-dir".into(),
                "/Users/a/Library/Application Support/\"clip\"monger".into(),
            ],
            run_at_load: true,
        }
    }

    #[test]
    fn t802_roundtrip_and_escaping() {
        let a = agent();
        let text = render(&a).unwrap();
        assert!(text.contains("<string>/Applications/Clip &amp; &lt;Monger&gt;.app/"));
        assert!(text.contains("&quot;clip&quot;"));
        assert!(text.contains("<key>ProcessType</key>\n\t<string>Interactive</string>"));
        assert!(text.contains("<key>LimitLoadToSessionType</key>\n\t<string>Aqua</string>"));
        assert_eq!(parse(&text).unwrap(), a);
    }

    #[test]
    fn t802_run_at_load_toggle() {
        let mut a = agent();
        a.run_at_load = false;
        let text = render(&a).unwrap();
        assert!(text.contains("<key>RunAtLoad</key>\n\t<false/>"));
        assert!(!parse(&text).unwrap().run_at_load);
        a.run_at_load = true;
        assert!(parse(&render(&a).unwrap()).unwrap().run_at_load);
    }

    #[test]
    fn rejects_control_chars_and_malformed() {
        let mut a = agent();
        a.args.push("bad\nline".into());
        assert!(render(&a).is_err());
        a.args.pop();
        a.args.push("nul\0".into());
        assert!(render(&a).is_err());
        assert!(parse("<plist><dict></dict></plist>").is_err());
        assert!(parse("<key>Label</key><string>x</string><key>ProgramArguments</key><array><integer>1</integer></array>").is_err());
    }
}
