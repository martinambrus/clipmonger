//! Open a folder or a web page with the desktop's default handler.

use std::process::{Command, Stdio};

/// Open `target` (a folder path or an `https://` URL) with the system
/// handler: `xdg-open` on Linux, Explorer on Windows, `open` on macOS. The
/// handler runs detached; only a failure to start it is reported.
///
/// # Errors
/// Returns the start failure as text.
pub fn open(target: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("explorer.exe");
        command.arg(target);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(target);
        command
    };
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(target);
        command
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|mut child| {
            // Reap the handler in the background so it leaves no zombie.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        })
        .map_err(|error| error.to_string())
}

/// Bytes as a short human-readable size (binary units).
#[must_use]
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    #[allow(clippy::cast_precision_loss)]
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn human_bytes_picks_a_readable_unit() {
        assert_eq!(super::human_bytes(0), "0 B");
        assert_eq!(super::human_bytes(1023), "1023 B");
        assert_eq!(super::human_bytes(1536), "1.5 KiB");
        assert_eq!(super::human_bytes(5 * 1024 * 1024), "5.0 MiB");
    }
}
