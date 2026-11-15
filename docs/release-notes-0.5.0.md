# Clipmonger 0.5.0 (pre-release)

Clipmonger 0.5.0 completes the v1 feature set: a Databases tab with
diagnostics, compacting and the profile folder, an opt-out daily update check,
hourly history retention, and a reworked popup (one click pastes, enlarged
image previews, the Windows menu colour in the light theme). It also fixes a
SQLite security issue and several bugs found in the release run. The changes
are listed in [CHANGELOG.md](../CHANGELOG.md) under 0.5.0.

## Artifacts

- `clipmonger_0.5.0-1_amd64.deb`, `clipmonger-0.5.0-1.x86_64.rpm`,
  `clipmonger-0.5.0-linux-x86_64.tar.gz` and `install.sh`: Linux (glibc 2.35
  or newer).
- `clipmonger-0.5.0-windows-x86_64-setup.exe` and
  `clipmonger-0.5.0-windows-x86_64.zip`: Windows installer and portable copy.
- `clipmonger-0.5.0-macos-universal.dmg` and `.zip`: universal (x86_64 +
  arm64), macOS 10.15 or newer. Signed ad hoc, not notarized: the first start
  needs **Open** from the context menu (macOS 15 and newer: **Open Anyway** in
  Privacy & Security).
- `SHA256SUMS`: checksums for all of the above.

All packages carry the licence texts and the generated `RUST-NOTICES.txt`.

## Security

The bundled SQLite is updated for CVE-2026-119375 (RUSTSEC-2026-0459): a
database opened together with a crafted `-wal` file of a different page size
could return stale bytes from the process's memory. A crafted backup ZIP is
the way such a pair could reach Clipmonger. Updating is recommended.

## Upgrading

- Linux: quit Clipmonger, install the new package over the old one or run
  `install.sh` again.
- Windows: quit Clipmonger, uninstall the old version (leave the profile box
  unticked to keep your history), then run the new installer. Uninstalling
  0.4.0 can leave its Start on login entry behind; it points at the same
  path, so it starts 0.5.0 after the reinstall. The 0.5.0 uninstaller removes
  it.
- macOS: replace the app. Because it is signed ad hoc, macOS treats each
  update as a new app: turn Clipmonger off and on again in Accessibility.

## Known limitations

- Very wide or very tall images show as a thin strip in the thumbnail and the
  hover preview.
- KDE asks to confirm each new permanent-clip shortcut ("Global Shortcuts
  Requested"); until it is confirmed, that shortcut does nothing.
- macOS: the tray icon is in colour rather than a monochrome template.

## Tested

The Linux and Windows files are the published ones, built from the release
branch at `f7e194e` (Linux) and `869a5b0` (Windows installer metadata); the
release commit only adds documentation. The macOS app is the universal build
from GitHub Actions at `869a5b0`, with the notices added.

| System | Installers | Result |
| --- | --- | --- |
| Xubuntu 26.10, Xfce (X11) | `.deb` | pass |
| Linux Mint 22.3, Cinnamon (X11) | `.deb` | pass |
| Ubuntu 24.04, GNOME 46 | `.deb` | pass |
| Ubuntu 24.10, GNOME 47 | `.deb` | pass |
| Ubuntu 25.10, GNOME 49 | `.deb` | pass |
| Ubuntu 26.04, GNOME 50 | `install.sh` via `wget` | pass |
| Ubuntu 26.10, GNOME 51 | `.deb` | pass |
| Debian 13, GNOME 48 | `.deb` | pass |
| Debian 12, KDE Plasma 5.27, Wayland and X11 | `.deb` | pass |
| Fedora 45, KDE Plasma 6.8 | `.rpm` | pass |
| Arch, Hyprland and Sway | `install.sh` via `curl` | pass |
| Pop!_OS 24.04, COSMIC | `.deb` | pass |
| Windows 10 | installer, ZIP, uninstall | pass, including admin-window paste |
| Windows 11 | installer, ZIP, uninstall | pass, including the admin helper (UAC) |
| macOS 10.15, 13, 15, 26 (Intel VMs) | `.dmg`, uninstall | pass, including automatic paste |
| macOS 15 and 26 on Apple Silicon (GitHub runners) | build, unit and integration tests | pass |

Every Linux row checked capture, popup paste of text and an image, a
permanent clip's hotkey, paste into a terminal and a root window, the image
hover preview and single-click paste, idle CPU, start on login across a
reboot, the checksums and a clean removal. Windows added Defender (no
detection), and macOS the Accessibility grant, 50 CLI calls in a row, start on
login across a reboot and `clipmonger-cli uninstall`. Before the release run,
the build also passed a 24-hour soak test and the full release gate
(formatting, lints, tests, a million-case parser fuzz, coverage and the
dependency audit).
