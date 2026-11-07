# Release regression run (Linux and Windows)

The checks to repeat before every release, on every supported desktop. It
complements the per-area scripts next to it (T-409, T-508, T-606, T-607,
T-702 to T-705): those define the gates; this file is the walk-through, plus
the regression log of bugs found in earlier runs and the check that now
catches each one.

**When a run finds a bug:** fix it, then add a row to the
[regression log](#regression-log) and either a new check or a "Watch for" line
on the check that caught it, so the next run covers that failure. If the
failure can be reproduced without a desktop, also add a unit or integration
test and name it in the log row.

## 1. Build and deploy

1. Build the release binaries in the Debian 12 container:
   `scripts/build-linux-release.sh`. It must end with `ok` for all three
   binaries (glibc floor 2.36, `scripts/check-glibc-floor.py`). A host
   `cargo build --release` is not a release build: it needs the host glibc.
2. Windows: `cargo build --release --target x86_64-pc-windows-gnu -p
   clipmonger-daemon -p clipmonger-app-ui -p clipmonger-app-cli`, or the
   installer from `scripts/build-windows-installer.py`.
3. Before any VM run: `cargo fmt --all --check`, workspace clippy for Linux
   and `--target x86_64-pc-windows-gnu`, and the crate tests
   (`scripts/release-gate.sh` runs them all). On the 16 GB build host, run
   heavy cargo steps one at a time and not while two VMs are up.
4. Deploy the binaries, the GNOME extension `dist/` and the guest helpers to
   each VM, start from a fresh profile, and record the build's commit.

The VM harness (installing the test desktops, the key, pointer and screenshot
driver, clipboard and editor helpers, the GNOME suite script) is in
[tests/vm](../vm/README.md); the helper names below refer to it.

## 2. Desktop matrix

Run sections 3 and 4 on every row. "Paste" is what a popup pick does.

| Desktop | Session | Hotkey comes from | Paste | Autostart |
| --- | --- | --- | --- | --- |
| GNOME 46 to 51 | Wayland | GNOME extension | extension presses Ctrl+V | XDG autostart |
| GNOME 46+ | Xorg | X11 grab | X11 engine | XDG autostart |
| KDE Plasma 6 | Wayland | GlobalShortcuts portal | KWin fake-input | XDG autostart |
| KDE Plasma 5.27 | X11 and Wayland | X11 grab / portal (draft API) | X11 engine / fake-input | XDG autostart |
| Xfce, Cinnamon | X11 | X11 grab | X11 engine | XDG autostart |
| Sway, Hyprland | Wayland | compositor binding (Settings shows the command) | virtual keyboard | compositor config (Settings shows the command) |
| COSMIC | Wayland | custom shortcut (Settings shows the command) | virtual keyboard | XDG autostart |
| Windows 10 and 11 | | RegisterHotKey | SendInput, elevated helper for admin windows | Run key |

Oldest distro in the matrix: Debian 12 (glibc 2.36). Each release should run
at least one distro per row; when a desktop ships a new major version, add it.

## 3. Core checks (every desktop)

Each check lists what to do, what must happen, and what earlier runs broke.

**R-01 Start and caps.** Start the UI (it starts the daemon), run
`clipmonger-cli --profile-dir <profile> status`.
Expected: monitor `full`; hotkey and paste as in the matrix (`degraded` with a
`popup_hotkey_command` on Sway, Hyprland, COSMIC). Notes mention the GNOME
extension only on GNOME. Exactly one daemon runs.
Watch for: GNOME notes on non-GNOME desktops; top-level hotkey or paste
borrowing the XWayland backend's `full` on Wayland-only compositors.

**R-02 Capture text and image.** Copy text from a GTK app and an image
(a GTK pixbuf, and a Qt/KDE screenshot where available).
Expected: one row each, with the right formats.
Watch for: Qt images not captured (more than 32 offered targets); a Klipper
refill stored as a new row; duplicate rows for one copy (charset twins, X11
and extension both reporting).

**R-03 Password-manager copy.** Copy text marked
`x-kde-passwordManagerHint=secret` (helper `secret.py`).
Expected: not stored, also not when GNOME's clipboard manager re-offers it.

**R-04 Popup via hotkey, focus.** Open and dismiss the popup six times in a
row with the hotkey and Esc, with another app focused in between.
Expected: every popup takes the keyboard (Esc closes it, typing filters).
Watch for: every second popup opening without focus (Cinnamon); a stale
"Connection lost" line right after the daemon started.

**R-05 Popup paste into a GTK editor.** Pick a text row, then an image row.
Expected: the text and the image land in the editor; history gains no row.
Watch for: GTK3 ignoring a virtual-keyboard Ctrl+V (Sway, Hyprland); large
clips (X11 INCR) stalling or truncating.

**R-06 Terminal paste.** Pick a text row with a terminal focused
(gnome-terminal, Ptyxis, Konsole, xfce4-terminal, foot, kitty, COSMIC
Terminal).
Expected: the text lands (Ctrl+Shift+V was pressed).
Watch for: a terminal missing from the Ctrl+Shift+V list (the daemon list in
`crates/platform/src/paste_policy.rs` and the extension list in
`gnome-ext/src/pasteplan.ts` must match). In raw-mode checks use
`timeout --foreground`.

**R-07 Root window paste.** Start a GTK editor with sudo, pick a row.
Expected: the text lands.

**R-08 Permanent-clip hotkey.** Create a permanent clip with a hotkey (Sway,
Hyprland, COSMIC: bind the command from Settings, Permanent Clips, Copy
shortcut command). Press it with the editor, a terminal and the root editor
focused, also with Ctrl/Alt still held a moment.
Expected: the clip text lands each time; history gains no row.
Watch for: the paste or the permanent clip text being stored as a new row
(GNOME Xorg); Ctrl+Alt+V arriving instead of Ctrl+V; elevated targets on
Windows.

**R-09 History stays clean.** Count rows before and after R-05 to R-08.
Expected: unchanged except for clips moved to the top.

**R-10 Tray.** Check the icon, click it, use its menu.
Expected: icon visible, the click opens the popup.
Watch for: no icon when the panel or bar starts after Clipmonger (start it
before waybar at login).

**R-11 Settings.** Open Settings from the popup, visit every tab, apply one
change.
Expected: applies; on Sway, Hyprland and COSMIC the Popup, Startup and
Permanent Clips tabs show the commands with working Copy buttons.

**R-12 Start on login.** Turn it on in Settings, reboot (or log out and in).
Expected: Clipmonger runs, history intact, hotkey and (GNOME) extension work.
Then turn it off and reboot: nothing starts, the autostart entry is gone.
Watch for: the legacy `clipmonger.desktop` autostart name; desktops that do
not run XDG autostart (Sway, Hyprland: Settings shows the config line).

**R-13 Session change.** Log out of a Wayland session and into X11 (or back).
Expected: the old daemon has exited; one daemon with the new session's caps.

## 4. Desktop-specific checks

**GNOME (every version in `metadata.json` shell-version).**
- R-20 Install the extension with `gnome-ext/install-extension.sh` on the VM.
  Expected: accepted on every listed version. Watch for: only the first
  listed version accepted.
- R-21 Popup placement at the pointer; large image paste (chunked) into an
  editor.
- R-22 Xorg: restart GNOME Shell (Alt+F2, r); the extension reconnects and the
  hotkey works.
- R-23 New GNOME major version: the extension's virtual keyboard still works
  (auto-paste, not the "press Ctrl+V" notification).

**KDE.**
- R-30 First popup hotkey and a permanent hotkey through the portal dialog;
  changing the popup chord in Settings rebinds it (the new key works, the old
  one does not).
- R-31 Restart the portal (`systemctl --user restart xdg-desktop-portal`); the
  hotkeys still work.
- R-32 Wayland: no "control input devices" prompt; auto-paste works once the
  `.desktop` file is installed.
- R-33 Plasma 5.27: same as R-30 (draft portal API).

**Sway, Hyprland.**
- R-40 Bind the commands Settings shows (Hyprland 0.57+: Lua config, startup
  commands inside `hl.on("hyprland.start", ...)`); hotkey, permanent hotkey
  and login start work.

**COSMIC.**
- R-41 Custom shortcut with the popup command; auto-paste into a GTK app and
  COSMIC Terminal.

**Windows.**
- R-50 UAC matrix ([T-409](T-409-uac-matrix.md)): popup and permanent-clip
  paste into elevated windows, with modifiers still held.
- R-51 Autostart across reboot ([T-410](T-410-autostart-reboot.md)).
- R-52 Installer upgrade over an existing install; uninstall leaves no
  leftovers.

## 5. Recording results

Add a dated section to `docs/v1-continuation.md` (or the release notes) with
one row per desktop: the build commit, what passed, what was fixed (commit),
and anything waived with the reason. Update the release-gate list there.

## Regression log

Bugs found in acceptance runs and the check that now catches them. Add a row
for every new one.

| Date | Desktop | Bug | Fix | Check |
| --- | --- | --- | --- | --- |
| 2026-11-04 | GNOME Xorg | Popup did not show or flashed away | 5badd58 | R-04 |
| 2026-11-05 | GNOME Wayland | Permanent hotkeys did nothing | eabadf8 | R-08 |
| 2026-11-05 | GNOME Wayland | Duplicate image and file rows | 0452f30 | R-02, R-09 |
| 2026-11-05 | GNOME Xorg | Duplicate copies, tray paused state, denylist | 2a7a2e2 | R-02, R-10 |
| 2026-11-05 | all Linux | Terminals got Ctrl+V (pastes nothing) | 2b72481 | R-06 |
| 2026-11-05 | GNOME 46 Xorg | Popup paste into GNOME Terminal | 28c8d50 | R-06 |
| 2026-11-05 | GNOME Wayland | Shell froze on large images | 045fb7c | R-02, R-21 |
| 2026-11-06 | X11 | INCR truncation and stale property reuse | c0cf4ed | R-05 |
| 2026-11-06 | GNOME Wayland | Large-clip paste, format choice, echo stored | f125c25, 4570631 | R-05, R-09 |
| 2026-11-06 | GNOME 50 | Popup placement, per-clip hotkeys, password re-capture | 283f91e, 4bc8700 | R-03, R-08, R-21 |
| 2026-11-06 | GNOME 50 | UI crash from the clipboard crate | 8beed27 | R-05 |
| 2026-11-06 | KDE Wayland | No auto-paste; portal trigger format; chord change ignored | 6af0607 | R-30, R-32 |
| 2026-11-06 | KDE Wayland | Duplicate rows after a paste (extra text MIME types) | c20444c | R-09 |
| 2026-11-06 | all X11 | INCR send hang as owner; Qt images not captured; Klipper refill stored | eda251e | R-02, R-05 |
| 2026-11-06 | Plasma 5.27 | Hotkeys not registered; daemon outlived the session | d7c10c0 | R-13, R-33 |
| 2026-11-06 | GNOME 51 | Auto-paste fell back to the notification (Clutter API removed) | 7133b1a | R-23 |
| 2026-11-06 | Windows | Permanent hotkey into an elevated window with modifiers held | 1693674 | R-50 |
| 2026-11-07 | all Linux | Release binaries needed the build host's glibc | 0bd5443 | section 1 |
| 2026-11-07 | Sway, Hyprland | No auto-paste; no way to bind hotkey or login start; GNOME notes shown | 606a288 | R-01, R-05, R-11, R-40 |
| 2026-11-07 | GNOME Xorg | Own paste and permanent-clip writes stored as new rows | 606a288 | R-08, R-09 |
| 2026-11-07 | Cinnamon | Every second popup without keyboard focus | 606a288 | R-04 |
| 2026-11-07 | Hyprland (any late bar) | Tray never registered when the bar started later | 606a288 | R-10 |
| 2026-11-07 | all | Stale "Connection lost" after daemon start | 606a288 | R-04 |
| 2026-11-07 | COSMIC | No auto-paste; COSMIC Terminal got Ctrl+V | 245e837 | R-06, R-41 |
| 2026-11-07 | GNOME 47 to 51 | Extension installer refused every version but the first | 972cc6c | R-20 |
