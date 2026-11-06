# Linux desktop entries

The `.deb` installs both files to `/usr/share/applications/`, with the
programs in `/opt/clipmonger/`.

- `com.martinambrus.clipmonger.desktop`: the applications-menu entry. Its
  name is the app id: the xdg-desktop-portal refuses the daemon's app id
  (and KDE then refuses the global popup hotkey) unless this file exists.
- `com.martinambrus.clipmonger.daemon.desktop`: hidden. KWin grants the
  interfaces in `X-KDE-Wayland-Interfaces` to the program named in `Exec=`;
  the daemon presses the paste chord through `org_kde_kwin_fake_input`.

Without a package (portable or source builds) the daemon writes the same
entries, pointing at its own folder, to `~/.local/share/applications/`
(see `crates/daemon/src/desktop_entry.rs`).
