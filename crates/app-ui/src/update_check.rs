//! Update check (seg-12 §4): ask the GitHub releases feed for the latest
//! version, compare it with this build and tell the user. It never downloads
//! or runs anything; "Download" only opens the release page in the browser.
//!
//! The feed is fetched with the system `curl` (shipped with Windows 10 1803+
//! and macOS) or `wget` (stock Ubuntu), so the app carries no TLS stack.
//! Neither tool, no network or a bad reply all end in a silent skip.

use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;

/// The latest published (non-pre-release) release.
pub const FEED: &str = "https://api.github.com/repos/martinambrus/clipmonger/releases/latest";

/// Release pages the "Download" button may open; anything else in the feed
/// falls back to the releases list.
const RELEASES: &str = "https://github.com/martinambrus/clipmonger/releases";

/// Replaces [`FEED`], for tests and QA (any URL `curl` reads, `file://` too).
pub const FEED_ENV: &str = "CLIPMONGER_UPDATE_FEED";

/// How often a running UI checks again.
pub const PERIOD: Duration = Duration::from_hours(24);

/// Longest a check may take.
const TIMEOUT_SECS: &str = "15";

/// A newer release than this build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Its version, as `1.2.3`.
    pub version: String,
    /// The page to open for it.
    pub url: String,
}

/// What one check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A newer release exists.
    Newer(Release),
    /// This build is the latest release or newer.
    Current,
    /// The feed could not be read or understood: skipped silently.
    Unreachable,
}

/// `major.minor.patch` of a version or tag (`v` prefix allowed). A
/// pre-release or build suffix makes it `None`: those are never offered.
#[must_use]
pub fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let text = text.trim();
    let text = text.strip_prefix('v').unwrap_or(text);
    let mut parts = text.split('.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    let version = (next()?, next()?, next()?);
    parts.next().is_none().then_some(version)
}

/// Compare a feed reply (GitHub's `releases/latest` JSON) with `current`.
#[must_use]
pub fn evaluate(current: &str, body: &str) -> Outcome {
    let Some(mine) = parse_version(current) else {
        return Outcome::Unreachable;
    };
    let Ok(reply) = serde_json::from_str::<Value>(body) else {
        return Outcome::Unreachable;
    };
    if reply.get("prerelease").and_then(Value::as_bool) == Some(true)
        || reply.get("draft").and_then(Value::as_bool) == Some(true)
    {
        return Outcome::Current;
    }
    let Some(tag) = reply.get("tag_name").and_then(Value::as_str) else {
        return Outcome::Unreachable;
    };
    let Some(latest) = parse_version(tag) else {
        return Outcome::Unreachable;
    };
    if latest <= mine {
        return Outcome::Current;
    }
    let url = reply
        .get("html_url")
        .and_then(Value::as_str)
        .filter(|url| url.starts_with(RELEASES) && !url.contains(char::is_whitespace))
        .unwrap_or(RELEASES)
        .to_string();
    Outcome::Newer(Release {
        version: format!("{}.{}.{}", latest.0, latest.1, latest.2),
        url,
    })
}

/// Read `feed` with `curl`, or with `wget` where `curl` is missing (stock
/// Ubuntu desktops ship only `wget`); `None` on any failure.
#[must_use]
pub fn fetch(feed: &str) -> Option<String> {
    let mut curl = Command::new("curl");
    curl.args([
        "--silent",
        "--fail",
        "--location",
        "--max-time",
        TIMEOUT_SECS,
    ])
    .args(["--max-filesize", "1000000"])
    .args(["--header", "Accept: application/vnd.github+json"])
    .args(["--user-agent", "clipmonger-update-check"])
    .arg("--")
    .arg(feed);
    match run(curl) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        result => return result.ok().flatten(),
    }
    let mut wget = Command::new("wget");
    wget.args(["--quiet", "--output-document=-", "--tries=1"])
        .arg(format!("--timeout={TIMEOUT_SECS}"))
        .arg("--header=Accept: application/vnd.github+json")
        .arg("--user-agent=clipmonger-update-check")
        .arg("--")
        .arg(feed);
    run(wget).ok().flatten()
}

/// Run a downloader; `Ok(None)` when it ran but failed or the reply is too
/// large or not UTF-8.
fn run(mut command: Command) -> std::io::Result<Option<String>> {
    command.stdin(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // CREATE_NO_WINDOW: no console flashes up from the windowless UI.
        command.creation_flags(0x0800_0000);
    }
    let output = command.output()?;
    if !output.status.success() || output.stdout.len() > 1_000_000 {
        return Ok(None);
    }
    Ok(String::from_utf8(output.stdout).ok())
}

/// One full check of `feed` against `current`.
#[must_use]
pub fn check(current: &str, feed: &str) -> Outcome {
    fetch(feed).map_or(Outcome::Unreachable, |body| evaluate(current, &body))
}

/// The feed to use: [`FEED_ENV`] when set, else [`FEED`].
#[must_use]
pub fn feed() -> String {
    std::env::var(FEED_ENV)
        .ok()
        .filter(|feed| !feed.trim().is_empty())
        .unwrap_or_else(|| FEED.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(tag: &str) -> String {
        serde_json::json!({
            "tag_name": tag,
            "html_url": format!("https://github.com/martinambrus/clipmonger/releases/tag/{tag}"),
            "prerelease": false,
            "draft": false,
        })
        .to_string()
    }

    #[test]
    fn versions_parse_strictly() {
        assert_eq!(parse_version("0.4.0"), Some((0, 4, 0)));
        assert_eq!(parse_version("v1.10.2"), Some((1, 10, 2)));
        assert_eq!(parse_version("1.0.0-rc1"), None);
        assert_eq!(parse_version("1.0"), None);
        assert_eq!(parse_version("1.0.0.1"), None);
        assert_eq!(parse_version("latest"), None);
    }

    #[test]
    fn newer_same_and_older_feeds() {
        assert_eq!(
            evaluate("0.4.0", &reply("0.5.0")),
            Outcome::Newer(Release {
                version: "0.5.0".into(),
                url: "https://github.com/martinambrus/clipmonger/releases/tag/0.5.0".into(),
            })
        );
        assert_eq!(evaluate("0.4.0", &reply("0.4.0")), Outcome::Current);
        assert_eq!(evaluate("0.4.0", &reply("0.3.9")), Outcome::Current);
        // Numeric, not text, comparison.
        assert!(matches!(
            evaluate("0.9.0", &reply("0.10.0")),
            Outcome::Newer(_)
        ));
    }

    #[test]
    fn bad_or_foreign_replies_are_skipped_or_contained() {
        assert_eq!(evaluate("0.4.0", "not json"), Outcome::Unreachable);
        assert_eq!(evaluate("0.4.0", "{}"), Outcome::Unreachable);
        assert_eq!(evaluate("0.4.0", &reply("nightly")), Outcome::Unreachable);
        let pre = serde_json::json!({"tag_name": "9.0.0", "prerelease": true}).to_string();
        assert_eq!(evaluate("0.4.0", &pre), Outcome::Current);
        // A link elsewhere is never opened: it falls back to the releases list.
        let foreign = serde_json::json!({
            "tag_name": "9.0.0",
            "html_url": "https://evil.example/clipmonger",
        })
        .to_string();
        assert_eq!(
            evaluate("0.4.0", &foreign),
            Outcome::Newer(Release {
                version: "9.0.0".into(),
                url: RELEASES.into(),
            })
        );
    }

    /// T-1202: the whole check against a mock feed: newer, same, older and
    /// unreachable.
    #[test]
    fn check_against_a_mock_feed() {
        if fetch("file:///dev/null").is_none() && cfg!(unix) {
            eprintln!("SKIP: no curl");
            return;
        }
        let dir = std::env::temp_dir().join(format!("cm-update-feed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let url = |name: &str, tag: &str| {
            let path = dir.join(name);
            std::fs::write(&path, reply(tag)).unwrap();
            let path = path.to_string_lossy().replace('\\', "/");
            if path.starts_with('/') {
                format!("file://{path}")
            } else {
                format!("file:///{path}")
            }
        };
        assert!(matches!(
            check("0.4.0", &url("newer.json", "0.5.0")),
            Outcome::Newer(_)
        ));
        assert_eq!(check("0.4.0", &url("same.json", "0.4.0")), Outcome::Current);
        assert_eq!(
            check("0.4.0", &url("older.json", "0.3.0")),
            Outcome::Current
        );
        let missing = url("gone.json", "0.5.0");
        std::fs::remove_file(dir.join("gone.json")).unwrap();
        assert_eq!(check("0.4.0", &missing), Outcome::Unreachable);
        assert_eq!(
            check("0.4.0", "http://127.0.0.1:9/releases/latest"),
            Outcome::Unreachable
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
