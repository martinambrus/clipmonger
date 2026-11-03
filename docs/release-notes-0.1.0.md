# Clipmonger 0.1.0 (draft — do not publish without user approval)

Pre-release hardening build. No tag or GitHub release has been created
for this draft (R-F4); the user reviews artifacts and notes first.

## Artifacts

- `clipmonger-0.1.0-windows-x86_64.zip` — `clipmonger-daemon.exe`,
  `clipmonger-cli.exe`, `clipmonger.exe` (UI). Cross-built on WSL2 with
  the windows-gnu target; not a native-runner build (seg-12 §2).
- `clipmonger-0.1.0-linux-x86_64.zip` — `clipmonger-daemon`,
  `clipmonger-cli`, `clipmonger` (UI), built GNOME extension under
  `gnome-extension/`, including its compiled GSettings schema. Copy that
  directory to the GNOME Shell extensions directory under the UUID from
  `metadata.json` before enabling it.
- Both portable ZIPs contain `portable.marker`, portable-use instructions,
  and the complete `licenses/` directory, including font notices and their
  README, upstream license texts, and generated `RUST-NOTICES.txt`.
- `clipmonger-0.1.0-windows-x86_64-setup.exe` — unsigned NSIS installer
  for Program Files and a Start menu entry, with the same license payload.
  Uninstall preserves profiles by default. The optional deletion checkbox
  names the default roaming profile directory and all its contents, requires
  separate confirmation, and refuses an unknown or different shell account.
  Custom and portable profiles outside that directory are preserved.
- `SHA256SUMS` — sha256sum-format checksums for both ZIPs and the installer.

The brief-named artifact files use the `-x86_64` suffix; they correspond
to the `linux-x64` / `windows-x64` naming in the release analysis.

Linux package builds and payload checks require `glib-compile-schemas` and
`gsettings` from GLib. Use the same GLib toolchain to build and verify a
package: the checker recompiles its XML and compares the compiled bytes.

## Unsigned-warning (seg-12 §5)

These binaries are **unsigned**. Windows SmartScreen will friction the
first install/run: use the per-OS bypass documentation (manual script
`tests/manual/T-1204-bypass-docs-accuracy.md`) and verify the download
against `SHA256SUMS` first. Linux has no signing friction expected.

Checksums are **integrity only, not signatures** (seg-12 §8): a
same-account compromise defeats them. Verify with `sha256sum -c
SHA256SUMS` in the download directory.

## Clipboard behavior

Windows clipboard paste leaves the selected clip on the clipboard by default.
Enable **Restore clipboard after paste** to restore the previous clipboard when
it has not changed in the meantime. Existing explicit preferences are preserved;
the default delivery strategy remains `clipboard_restore`.

## Coexistence note

Clipmonger uses a passive clipboard listener by design and coexists
with ArsClip and clipboard sync tools. Current-build interoperability with
RustDesk and ScreenConnect still requires the native checks below; pause sync
tools only for clean manual test runs.
Only one Clipmonger daemon may hold a profile at a time
(`already_running`); portable mode leaves no autostart entries.

## Known pending (manual/native legs)

T-409/T-410 (Windows session), T-508, T-606/T-607, T-702..T-705
(GNOME/Ubuntu), T-905..T-908 (real displays), per-box caps goldens,
coverage floor, update-check (T-1202). See `CHANGELOG.md`.

The current draft still needs installer/uninstaller, confirmation and recovery
acceptance; elevated-helper and failed-paste acceptance; and the current CPU
runner's native display and focus checks. The profile-deletion component's
synthetic Windows fixture is not installer UI or UAC acceptance. Positive file
symbolic-link controls remain unverified where the test token lacks privilege.
Linux release clearance for the blur-derived protocol output remains open.
A local draft artifact or checksum pass does not approve publication.
