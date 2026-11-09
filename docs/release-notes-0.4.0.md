# Clipmonger 0.4.0 (pre-release)

Clipmonger gets its own icon on Linux, Windows and macOS, the tray shows the
monitoring state on every platform, and popup search now covers the whole
history. The changes are listed in [CHANGELOG.md](../CHANGELOG.md) under 0.4.0.

## Artifacts

- `clipmonger_0.4.0-1_amd64.deb`, `clipmonger-0.4.0-1.x86_64.rpm`,
  `clipmonger-0.4.0-linux-x86_64.tar.gz` and `install.sh`: Linux (glibc 2.35
  or newer).
- `clipmonger-0.4.0-windows-x86_64-setup.exe` and
  `clipmonger-0.4.0-windows-x86_64.zip`: Windows installer and portable copy.
- `clipmonger-0.4.0-macos-universal.dmg` and `.zip`: universal (x86_64 +
  arm64), macOS 10.15 or newer. Signed ad hoc, not notarized: the first start
  needs **Open** from the context menu (macOS 15 and newer: **Open Anyway** in
  Privacy & Security).
- `SHA256SUMS`: checksums for all of the above.

All packages carry the licence texts and the generated `RUST-NOTICES.txt`.

## Upgrading

- Linux: quit Clipmonger, install the new package over the old one or run
  `install.sh` again. On GNOME, log out and back in once: the Shell extension
  changed.
- Windows: quit Clipmonger, uninstall the old version (leave the profile box
  unticked to keep your history), then run the new installer.
- macOS: replace the app. Because it is signed ad hoc, macOS treats each
  update as a new app: turn Clipmonger off and on again in Accessibility.

## Known limitations

- Windows: after an upgrade, Start menu search can keep showing a generic
  icon until Windows refreshes its own cache. The program, its shortcut and
  the tray show the new icon.
- macOS: the tray icon is in colour rather than a monochrome template.

## Tested

The installers below are the published files, built from commit `c2dbb19`
(the release commit only adds documentation). The macOS app is the universal
build from GitHub Actions at that commit, with the notices added.

| System | Installers | Result |
| --- | --- | --- |
| Ubuntu 24.04, GNOME 46 | tarball, `.deb` | pass |
| Ubuntu 24.10, GNOME 47 | tarball, `.deb` | pass |
| Ubuntu 25.10, GNOME 49 | tarball, `.deb` | pass |
| Ubuntu 26.04, GNOME 50 | tarball, `.deb` | pass, root window not run |
| Ubuntu 26.10, GNOME 51 | tarball, `.deb` | pass, root window not run |
| Debian 13, GNOME 48 | tarball | pass |
| Debian 12, KDE Plasma 5.27 X11 | tarball, `.deb` | pass |
| Fedora 45, KDE Plasma 6.8 Wayland | tarball, `.rpm` (upgrade from 0.3.0) | pass |
| Xubuntu 26.10, Xfce | tarball, `.deb` | pass |
| Linux Mint 22.3, Cinnamon | tarball, `.deb` | pass |
| Pop!_OS 24.04, COSMIC | tarball, `.deb` | pass, permanent hotkey not bound |
| Arch, Hyprland | tarball | pass, permanent hotkey not bound |
| Windows 10 | ZIP, installer, uninstall, reinstall | pass, including popup paste |
| Windows 11 | ZIP, installer, uninstall, reinstall | pass, including popup paste |
| macOS 10.15, 13, 15, 26 (Intel VMs) | `.zip`, uninstall, `.dmg` | pass: start, capture, popup, tray |
| macOS 15 and 26 on Apple Silicon (GitHub runners) | build, unit and integration tests | pass |

Linux rows checked capture, popup paste of text and an image, a permanent
clip's hotkey, paste into a terminal and a root window, that a password-hinted
copy leaves no row, and a clean `install.sh --uninstall`. Where a row says
"not run" or "not bound", those are limits of the test run: GNOME 50 and 51
could not get `sudo` without a terminal, and on Hyprland and COSMIC a permanent
clip's hotkey needs the compositor binding that Settings shows.

Automatic paste on macOS was not exercised for this build, because the
Accessibility permission has to be granted by hand after every re-signing.
