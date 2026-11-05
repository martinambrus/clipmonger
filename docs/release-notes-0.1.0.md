# Clipmonger 0.1.0 (pre-release)

First public pre-release of Clipmonger, for 64-bit Windows 10 and Windows 11
and for Ubuntu (GNOME 46-51, X11 and Wayland). Changes are listed in
[CHANGELOG.md](../CHANGELOG.md) under 0.1.0.

## Artifacts

- `clipmonger-0.1.0-windows-x86_64-setup.exe`: unsigned NSIS installer for
  Program Files with a Start menu entry. Uninstall keeps profiles by default.
  The optional deletion checkbox names the default roaming profile directory
  and all its contents, requires separate confirmation, and refuses an unknown
  or different shell account. Custom and portable profiles outside that
  directory are kept.
- `clipmonger-0.1.0-windows-x86_64.zip`: portable copy with `clipmonger.exe`
  (UI), `clipmonger-daemon.exe` and `clipmonger-cli.exe`. It contains
  `portable.marker`, so the profile is stored beside the programs, and
  portable-use instructions.
- `clipmonger_0.1.0_amd64.deb`: Ubuntu/Debian package that installs the
  programs to `/opt/clipmonger`, an applications menu entry and the GNOME Shell
  extension. On GNOME Wayland enable the extension once with
  `gnome-extensions enable clipmonger@extensions.clipmonger.dev`, then log out
  and back in.
- Both Windows artifacts contain the complete `licenses/` directory: font notices and their
  README, upstream license texts and the generated `RUST-NOTICES.txt`.
- `SHA256SUMS`: sha256sum-format checksums for the installer and the ZIP.

The binaries are cross-built on WSL2 with the `x86_64-pc-windows-gnu` target,
not on a native Windows build runner (seg-12 §2).

## Unsigned binaries (seg-12 §5)

The binaries are **not signed**. Windows SmartScreen warns on the first
download and run. Follow [Installing on Windows](install-windows.md), which
verifies the download against `SHA256SUMS` before any warning is bypassed.

Checksums are **integrity only, not signatures** (seg-12 §8): someone who can
replace both the file and `SHA256SUMS`, or who controls your account, defeats
them.

## Clipboard behavior

Paste leaves the selected clip on the clipboard by default. Enable **Restore
clipboard after paste** to restore the previous clipboard when it has not
changed in the meantime. Existing explicit preferences are kept; the default
delivery strategy remains `clipboard_restore`.

Pasting into a window that runs as administrator uses an elevated helper,
started after one consent and User Account Control prompt (once for an
install, at every start for the portable copy). When Clipmonger itself runs
elevated, for example under the built-in Administrator account, it pastes
directly and needs no helper. Such pastes need the hotkey press that opened
the popup; from a popup opened by the tray or `clipmonger-cli show-popup` they
are refused with a visible reason.

## Coexistence

Clipmonger uses a passive clipboard listener and coexists with other clipboard
managers and clipboard sync tools. Only one Clipmonger daemon may hold a
profile at a time (`already_running`). Portable mode creates no autostart
entry.

## Known limitations

- Alt+Tab from the open popup closes it and switches to the most recently used
  window, not the one before it. Windows reserves Alt+Tab for the shell.
- A popup opened by `clipmonger-cli show-popup` while an administrator window
  is in front appears without keyboard focus; click it or use the hotkey.
- Under the built-in Administrator account every window runs as
  administrator, so only a popup opened by the hotkey can paste.
- Windows 11 Smart App Control blocks unsigned programs; Clipmonger cannot run
  while it is on.
- No update check (T-1202 is out of 0.1.0 scope).

## Not yet verified

Checked by hand on Windows: capture and paste, the popup, Settings, the UAC
helper (T-409 legs 1-10), autostart after reboot (T-410) and the install
guide on Windows 10 and Windows 11 (T-1204). Still open:
T-409 legs 11-17, installer recovery paths, DPI and monitor changes, the
keyboard-only pass and the full window matrix (T-905 to T-908), the 24-hour
soak, fuzzing and coverage
calibration. See [STATUS](STATUS.md).
