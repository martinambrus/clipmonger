# Clipmonger 0.3.0 (pre-release)

macOS release: one universal app for Intel and Apple Silicon Macs on macOS
10.15 Catalina and newer, at feature parity with the Windows and Linux
builds ([macOS plan](macos-v1-plan.md)). Windows and Linux get the same
version, rebuilt from the shared code and re-tested. The changes are listed
in [CHANGELOG.md](../CHANGELOG.md) under 0.3.0.

## Artifacts

- `clipmonger-0.3.0-macos-universal.dmg`: drag **Clipmonger** to
  **Applications**. Universal (x86_64 + arm64), macOS 10.15 or newer.
  Signed ad hoc, not notarized: the first start needs **Open** from the
  context menu (macOS 15 and newer: **Open Anyway** in Privacy & Security).
- `clipmonger-0.3.0-macos-universal.zip`: the same app as a zip.
- `clipmonger_0.3.0-1_amd64.deb`, `clipmonger-0.3.0-1.x86_64.rpm`,
  `clipmonger-0.3.0-linux-x86_64.tar.gz` and `install.sh`: Linux, as in
  0.2.0 (glibc 2.35 or newer).
- `clipmonger-0.3.0-windows-x86_64-setup.exe` and
  `clipmonger-0.3.0-windows-x86_64.zip`: Windows installer and portable copy.
- `SHA256SUMS`: checksums for all of the above.

All packages carry the licence texts and the generated `RUST-NOTICES.txt`.

## macOS notes

- Clipmonger lives in the menu bar (no Dock icon). Press **Ctrl+Option+H**
  for the popup.
- Pasting for you needs **Accessibility** access (System Settings, Privacy
  & Security, Accessibility; System Preferences, Security & Privacy, Privacy
  on macOS 12 and older). Without it the picked clip is left on the
  clipboard for Cmd+V.
- Because the app is signed ad hoc, macOS treats each update as a new app:
  turn Clipmonger off and on again in Accessibility after updating.
- `clipmonger-cli` is inside the app
  (`/Applications/Clipmonger.app/Contents/MacOS/clipmonger-cli`);
  `clipmonger-cli uninstall [--delete-history]` stops Clipmonger and removes
  its login item before you drag the app to the Trash.
- macOS 10.12 to 10.14 are not supported: on 10.12 the daemon and UI lost
  replies over their local socket.

## Upgrading

- Linux: install the new package over the old one, or run `install.sh`
  again. Quit Clipmonger first.
- Windows: quit Clipmonger, uninstall the old version (leave the profile box
  unticked to keep your history), then run the new installer.

## Tested

Every row ran the release regression checks
([tests/manual/release-regression.md](../tests/manual/release-regression.md)),
macOS rows with the universal app built by GitHub Actions at the release
commit's parent (`9ac660c`) and installed from the `.dmg`.

| System | Package | Result |
| --- | --- | --- |
| macOS 10.15.7 Catalina (Intel VM) | `.dmg` | pass: capture (text, image, file, concealed/transient skipped), popup paste of text and image, permanent hotkey, CLI, Settings, start on login across a reboot, uninstall |
| macOS 13.7 Ventura (Intel VM) | `.dmg` | pass, same checks |
| macOS 15.8 Sequoia (Intel VM) | `.dmg` | pass, same checks |
| macOS 26 Tahoe (Intel VM) | `.dmg` | pass: capture, popup paste of text and image, CLI |
| macOS 15 and 26 on Apple Silicon (GitHub runners) | build, unit and integration tests | pass |
| Ubuntu 24.04, GNOME 46 Wayland | `.deb` (upgrade from 0.2.0) | pass, including a root window |
| Fedora 45, KDE Plasma Wayland | `.rpm` (upgrade from 0.2.0) | pass, including Konsole and a root window |
| Xubuntu 26.10, Xfce | tarball `install.sh`, then `--uninstall` | pass |
| Windows 11 | NSIS installer, silent uninstall | pass, history kept on uninstall |
| Windows 10 | portable ZIP; installer upgrade from 0.2.0 | pass |

macOS 10.12 to 10.14 are not supported: on 10.12 the daemon's socket replies
were lost about half of the time.
