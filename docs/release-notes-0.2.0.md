# Clipmonger 0.2.0 (pre-release)

Linux release: install packages for every major distro, and the fixes from
acceptance runs on GNOME 46 to 51, KDE Plasma 5.27 and 6, Xfce, Cinnamon,
Sway, Hyprland and COSMIC. Windows gets the same build with a corrected
uninstaller. The changes are listed in [CHANGELOG.md](../CHANGELOG.md) under
0.2.0.

## Artifacts

- `clipmonger_0.2.0-1_amd64.deb`: Debian, Ubuntu, Mint, Pop!_OS. Installs the
  programs to `/opt/clipmonger` (commands in `/usr/bin`), the menu entries and
  the GNOME Shell extension system-wide. Needs glibc 2.35 or newer.
- `clipmonger-0.2.0-1.x86_64.rpm`: Fedora and other RPM distros, same layout.
- `clipmonger-0.2.0-linux-x86_64.tar.gz` and `install.sh`: per-user install
  into `~/.local` on any x86_64 distro with glibc 2.35+, no root needed:
  `curl -fsSL https://github.com/martinambrus/clipmonger/releases/latest/download/install.sh | sh`
  (or `wget -qO- ... | sh`). The script checks the tarball against
  `SHA256SUMS`; `install.sh --uninstall` removes it again.
- `clipmonger-0.2.0-windows-x86_64-setup.exe`: unsigned NSIS installer for
  Program Files. Uninstall keeps profiles by default.
- `clipmonger-0.2.0-windows-x86_64.zip`: portable Windows copy (profile
  beside the programs).
- `SHA256SUMS`: checksums for all of the above.

All packages carry the licence texts and the generated `RUST-NOTICES.txt`.

## Upgrading

- Linux: install the new package over the old one (`apt install` or
  `dnf install`), or run `install.sh` again. Quit Clipmonger first.
- Windows: the installer does not install over an existing copy. Quit
  Clipmonger, uninstall 0.1.0 (leave the profile box unticked to keep your
  history), then run the new installer.

## Tested

Every row ran the release regression checks
([tests/manual/release-regression.md](../tests/manual/release-regression.md)):
capture of text and images, popup paste of text and an image into a GTK
editor, permanent-clip hotkey, paste into a terminal and into a root window,
history staying clean, Settings, and (where marked) start on login across a
reboot.

| Desktop | Package | Result |
| --- | --- | --- |
| Ubuntu 24.04, GNOME 46 Wayland | `.deb` | pass |
| Debian 13, GNOME 48 Wayland | `install.sh` (wget) | pass, start on login, uninstall |
| Debian 12, KDE Plasma 5.27 X11 | `.deb` | pass |
| Fedora 45, KDE Plasma 6.8 Wayland | `.rpm` | pass, Settings, start on login |
| Xubuntu 26.10, Xfce | `.deb` | pass |
| Linux Mint 22.3, Cinnamon | `.deb` | pass, popup focus six times in a row |
| Pop!_OS 24.04, COSMIC | `.deb` | pass, Settings, start on login, uninstall |
| Arch, Hyprland | `install.sh` (curl) | pass, start on login |
| Windows 11 | installer | install, capture, popup paste, uninstall |

## Known limitations

- Nothing is code-signed yet: SmartScreen warns on Windows, and the package
  managers may warn about a missing signature. Check `SHA256SUMS`.
- No package repository yet; new versions are installed by hand.
- Windows: no in-place upgrade (see above).
- On Sway, Hyprland and COSMIC the hotkeys and the login start are bound in
  the compositor's config; Settings shows the exact commands.
