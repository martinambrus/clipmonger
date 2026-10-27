# Clipmonger 0.1.0 (draft — do not publish without user approval)

Pre-release hardening build. No tag or GitHub release has been created
for this draft (R-F4); the user reviews artifacts and notes first.

## Artifacts

- `clipmonger-0.1.0-windows-x86_64.zip` — `clipmonger-daemon.exe`,
  `clipmonger-cli.exe`, `clipmonger.exe` (UI). Cross-built on WSL2 with
  the windows-gnu target; not a native-runner build (seg-12 §2).
- `clipmonger-0.1.0-linux-x86_64.zip` — `clipmonger-daemon`,
  `clipmonger-cli`, `clipmonger` (UI), GNOME extension `dist/` payload.
- `SHA256SUMS` — sha256sum-format checksums for every artifact above.

The brief-named artifact files use the `-x86_64` suffix; they correspond
to the `linux-x64` / `windows-x64` naming in the release analysis.

## Unsigned-warning (seg-12 §5)

These binaries are **unsigned**. Windows SmartScreen will friction the
first install/run: use the per-OS bypass documentation (manual script
`tests/manual/T-1204-bypass-docs-accuracy.md`) and verify the download
against `SHA256SUMS` first. Linux has no signing friction expected.

Checksums are **integrity only, not signatures** (seg-12 §8): a
same-account compromise defeats them. Verify with `sha256sum -c
SHA256SUMS` in the download directory.

## Coexistence note

Clipmonger uses a passive clipboard listener by design and coexists
with ArsClip and with sync tools (RustDesk/ScreenConnect verified by
design in Step 2); pause sync tools only for clean manual test runs.
Only one Clipmonger daemon may hold a profile at a time
(`already_running`); portable mode leaves no autostart entries.

## Known pending (manual/native legs)

T-409/T-410 (Windows session), T-508, T-606/T-607, T-702..T-705
(GNOME/Ubuntu), T-905..T-908 (real displays), per-box caps goldens,
coverage floor, update-check (T-1202). See `CHANGELOG.md`.
