//! The desktop entries that give Clipmonger its desktop app id on Linux.
//!
//! - `com.martinambrus.clipmonger.desktop`: the applications-menu entry
//!   for the UI. The portal (`org.freedesktop.host.portal.Registry`)
//!   refuses the app id until this file exists, and KDE refuses a
//!   `GlobalShortcuts` session without an app id.
//! - `com.martinambrus.clipmonger.daemon.desktop`: a hidden entry for the
//!   daemon. `KWin` looks up a client's entry by its executable and grants
//!   the interfaces it lists in `X-KDE-Wayland-Interfaces` (KDE Connect's
//!   daemon does the same for fake input).
//!
//! A package installs both under `/usr/share/applications`. Without them
//! (portable or source builds) the daemon writes its own copies to
//! `$XDG_DATA_HOME/applications` at start. Copies it wrote carry
//! [`GENERATED_KEY`] and follow the programs when they move; a file
//! without that key belongs to the user or a package and is never touched.
//! Those builds also get the launcher icon the entry names.

use std::path::{Path, PathBuf};

use clipmonger_core::{DESKTOP_APP_ID, DESKTOP_DAEMON_ID};

/// Marks an entry the daemon wrote (and may rewrite).
pub const GENERATED_KEY: &str = "X-Clipmonger-Generated=true";

/// The launcher icon for builds no package installed.
const APP_ICON_PNG: &[u8] = include_bytes!(
    "../../../packaging/linux/icons/hicolor/256x256/apps/com.martinambrus.clipmonger.png"
);

/// Quote one argument for an `Exec=` line (desktop entry spec): reserved
/// characters force double quotes, inside which `"`, `` ` ``, `$` and `\`
/// are backslash-escaped; `%` is doubled.
#[must_use]
pub fn exec_arg(arg: &str) -> String {
    let arg = arg.replace('%', "%%");
    let reserved = |c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '"' | '\''
                    | '\\'
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
            )
    };
    if !arg.chars().any(reserved) {
        return arg;
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    for c in arg.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    // The desktop file format escapes a backslash once more.
    out.replace('\\', "\\\\")
}

/// The two entries as `(file name, contents)`.
#[must_use]
pub fn entries(ui_exe: &Path, daemon_exe: &Path) -> [(String, String); 2] {
    let ui = exec_arg(&ui_exe.to_string_lossy());
    let daemon = exec_arg(&daemon_exe.to_string_lossy());
    [
        (
            format!("{DESKTOP_APP_ID}.desktop"),
            format!(
                "[Desktop Entry]\n\
                 Type=Application\n\
                 Name=Clipmonger\n\
                 Comment=Clipboard history\n\
                 Exec={ui}\n\
                 Icon={DESKTOP_APP_ID}\n\
                 Terminal=false\n\
                 Categories=Utility;\n\
                 StartupWMClass={DESKTOP_APP_ID}\n\
                 {GENERATED_KEY}\n"
            ),
        ),
        (
            format!("{DESKTOP_DAEMON_ID}.desktop"),
            format!(
                "[Desktop Entry]\n\
                 Type=Application\n\
                 Name=Clipmonger daemon\n\
                 Exec={daemon}\n\
                 NoDisplay=true\n\
                 Terminal=false\n\
                 X-KDE-Wayland-Interfaces=org_kde_kwin_fake_input\n\
                 {GENERATED_KEY}\n"
            ),
        ),
    ]
}

/// What [`ensure`] does with one entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// A package or the user provides it; leave it alone.
    Keep,
    /// Write (or rewrite) the user copy.
    Write(PathBuf),
}

/// Decide for one entry: `user_dir` is `$XDG_DATA_HOME/applications`,
/// `system_dirs` the `applications` folders of `$XDG_DATA_DIRS`.
#[must_use]
pub fn plan(name: &str, wanted: &str, user_dir: &Path, system_dirs: &[PathBuf]) -> Action {
    let user = user_dir.join(name);
    match std::fs::read_to_string(&user) {
        // Ours and outdated: refresh it.
        Ok(current) if current.contains(GENERATED_KEY) && current != wanted => Action::Write(user),
        // Up to date, the user's own file, or a package provides it.
        Ok(_) => Action::Keep,
        Err(_) if system_dirs.iter().any(|dir| dir.join(name).is_file()) => Action::Keep,
        Err(_) => Action::Write(user),
    }
}

/// `$XDG_DATA_HOME/applications` and the `$XDG_DATA_DIRS` folders.
fn data_dirs() -> Option<(PathBuf, Vec<PathBuf>)> {
    let home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    let system = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    let system = system
        .split(':')
        .filter(|d| !d.is_empty())
        .map(|d| Path::new(d).join("applications"))
        .collect();
    Some((home.join("applications"), system))
}

/// Write any missing or outdated entry; failures are logged, not fatal.
pub fn ensure(ui_exe: &Path, daemon_exe: &Path) {
    let Some((user_dir, system_dirs)) = data_dirs() else {
        return;
    };
    for (name, wanted) in entries(ui_exe, daemon_exe) {
        if let Action::Write(path) = plan(&name, &wanted, &user_dir, &system_dirs) {
            let written =
                std::fs::create_dir_all(&user_dir).and_then(|()| std::fs::write(&path, &wanted));
            if let Err(e) = written {
                eprintln!("daemon: desktop entry {name} not written: {e}");
            }
        }
    }
    let ui_entry = format!("{DESKTOP_APP_ID}.desktop");
    if !system_dirs.iter().any(|dir| dir.join(&ui_entry).is_file()) {
        if let Some(data_home) = user_dir.parent() {
            if let Err(e) = write_icon(data_home) {
                eprintln!("daemon: launcher icon not written: {e}");
            }
        }
    }
}

/// Put the launcher icon in the user's `hicolor` theme unless it is there.
fn write_icon(data_home: &Path) -> std::io::Result<()> {
    let dir = data_home.join("icons/hicolor/256x256/apps");
    let path = dir.join(format!("{DESKTOP_APP_ID}.png"));
    if std::fs::read(&path).is_ok_and(|current| current == APP_ICON_PNG) {
        return Ok(());
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::write(&path, APP_ICON_PNG)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cm-desktop-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn exec_quotes_reserved_characters_and_doubles_percent() {
        assert_eq!(
            exec_arg("/opt/clipmonger/clipmonger"),
            "/opt/clipmonger/clipmonger"
        );
        assert_eq!(exec_arg("/opt/My Apps/cm"), "\"/opt/My Apps/cm\"");
        assert_eq!(exec_arg("/a/100%/cm"), "/a/100%%/cm");
        assert_eq!(exec_arg("/a b/$x"), "\"/a b/\\\\$x\"");
    }

    #[test]
    fn entries_name_the_programs_and_declare_fake_input() {
        let [ui, daemon] = entries(
            Path::new("/opt/cm/clipmonger"),
            Path::new("/opt/cm/clipmonger-daemon"),
        );
        assert_eq!(ui.0, "com.martinambrus.clipmonger.desktop");
        assert!(ui.1.contains("\nExec=/opt/cm/clipmonger\n"));
        assert!(ui.1.contains("\nIcon=com.martinambrus.clipmonger\n"));
        assert_eq!(daemon.0, "com.martinambrus.clipmonger.daemon.desktop");
        assert!(daemon.1.contains("\nExec=/opt/cm/clipmonger-daemon\n"));
        assert!(daemon
            .1
            .contains("X-KDE-Wayland-Interfaces=org_kde_kwin_fake_input"));
        assert!(daemon.1.contains("NoDisplay=true"));
    }

    #[test]
    fn icon_is_written_once_and_a_stale_one_replaced() {
        let root = temp("icon");
        let path = root.join("icons/hicolor/256x256/apps/com.martinambrus.clipmonger.png");
        write_icon(&root).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), APP_ICON_PNG);
        std::fs::write(&path, "old").unwrap();
        write_icon(&root).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), APP_ICON_PNG);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn plan_keeps_package_and_user_files_and_refreshes_its_own() {
        let root = temp("plan");
        let user = root.join("user");
        let system = root.join("system");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::create_dir_all(&system).unwrap();
        let wanted = format!("Exec=/new\n{GENERATED_KEY}\n");
        let systems = [system.clone()];
        // Nothing anywhere: write.
        assert_eq!(
            plan("a.desktop", &wanted, &user, &systems),
            Action::Write(user.join("a.desktop"))
        );
        // A package provides it: keep.
        std::fs::write(system.join("a.desktop"), "x").unwrap();
        assert_eq!(plan("a.desktop", &wanted, &user, &systems), Action::Keep);
        // Our own stale copy: rewrite.
        std::fs::write(
            user.join("b.desktop"),
            format!("Exec=/old\n{GENERATED_KEY}\n"),
        )
        .unwrap();
        assert_eq!(
            plan("b.desktop", &wanted, &user, &systems),
            Action::Write(user.join("b.desktop"))
        );
        // Up to date: keep.
        std::fs::write(user.join("b.desktop"), &wanted).unwrap();
        assert_eq!(plan("b.desktop", &wanted, &user, &systems), Action::Keep);
        // The user's own file: keep.
        std::fs::write(user.join("c.desktop"), "Exec=/mine\n").unwrap();
        assert_eq!(plan("c.desktop", &wanted, &user, &systems), Action::Keep);
        let _ = std::fs::remove_dir_all(&root);
    }
}
