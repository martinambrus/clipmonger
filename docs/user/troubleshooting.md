# Troubleshooting

Answers to common problems, and how to report a bug. Many answers start with
the **Capabilities** list: open Settings, **About and Help**. It shows what
works on your system and why something is limited. The popup shows "Some
features limited" when one of them is limited.

The examples use the Linux profile folder. For other systems, see
[Command line](cli.md#global-options).

## The hotkey does nothing

**Another program holds the hotkey.** The popup shows "Popup shortcut ... is
already in use. Reassign it in Settings." Set a different **Popup hotkey**
on the Popup tab of Settings and click **Apply**. A clip hotkey that another
permanent clip already uses is refused with "That hotkey is already used by
another permanent clip". Some chords belong to the system and cannot be
used, for example Win+L or Ctrl+Alt+Delete on Windows.

**Clipmonger is not running.** Open the popup from the tray icon, or run:

```sh
clipmonger-cli --profile-dir ~/.local/share/clipmonger status
```

`daemon not running` means the background service is down. Start
Clipmonger from the applications menu; the UI starts the service.

**KDE Plasma.** Plasma asks you to confirm a new global shortcut the first
time, and again after you change the hotkey. Settings shows "Your desktop
asks you to confirm a new shortcut" in that case. Approve it in Plasma's
dialog. <!-- TODO verify: how to re-approve after the Plasma dialog was dismissed or denied (System Settings, Shortcuts?) -->

**GNOME.** On GNOME the bundled Clipmonger Shell extension handles the
hotkey. Log out and back in once after installing, then enable it:

```sh
gnome-extensions enable clipmonger@extensions.clipmonger.dev
```

Check that it is on with `gnome-extensions info
clipmonger@extensions.clipmonger.dev`. See
[Getting started on Linux](getting-started-linux.md).

**Sway, Hyprland and COSMIC.** These desktops do not let apps register
global shortcuts. Settings shows "This desktop does not let apps set global
shortcuts..." with the command to bind. Bind it in the compositor config,
for example on Sway:

```
bindsym Ctrl+Alt+h exec clipmonger-cli --profile-dir ~/.local/share/clipmonger show-popup
```

On COSMIC, add a custom shortcut in Settings, Keyboard. Permanent clip
hotkeys work the same way, with **Copy shortcut command** on the Permanent
Clips tab.

**macOS.** The default hotkey is Ctrl+Option+H, not Cmd. In Settings it is
written `ctrl+alt+h`.

## Nothing is captured

**Monitoring is paused.** The popup shows "Monitoring paused" and the tray
icon shows the paused state. Resume with **Resume monitoring** in the tray
menu, or:

```sh
clipmonger-cli --profile-dir ~/.local/share/clipmonger toggle-monitor
```

**Pause for one hour** in the tray menu resumes by itself after an hour. On
Windows, if **Toggle monitoring with Scroll Lock** is on, pressing Scroll
Lock pauses and resumes too.

**The app is on the denylist.** Copies from apps listed in **Privacy
denylist** or **Monitor ignored apps** are never stored. On Windows, X11 and
macOS a name matches any part of the app's identity, so a short entry such
as `term` blocks every app whose name contains it. Check both lists in
Settings.

**The app marks its copy as private.** Password managers and some other
apps mark copied secrets as sensitive, and Clipmonger does not store those.
This is intended. See [Privacy](privacy.md).

**Strict privacy mode is on.** With **Privacy strict mode**, only plain text
is stored; images and file lists without a text form are skipped.

**The format is turned off or too short.** Check **Monitor formats** on the
History tab. On Windows, **Monitor minimum length** skips text shorter than
the set number of bytes.

**The item is too large.** Items over **Monitor maximum bytes** (16 MiB by
default) appear as "Too large — not saved" entries. See
[Data, backup and restore](backup-and-data.md#oversized-items).

**GNOME on Wayland without the extension.** GNOME does not let regular
applications read the clipboard in the background. Capture on GNOME
Wayland needs the Clipmonger Shell extension (see above).

**Other Wayland desktops.** Clipmonger needs the compositor's data-control
protocol (`ext-data-control-v1` or `wlr-data-control`). KDE Plasma, Sway,
Hyprland and COSMIC provide it. Without it, **Capabilities** shows "no
data-control: capture unavailable".

**macOS 15.4 and newer.** macOS asks before an app reads the clipboard.
Open System Settings, Privacy & Security, **Paste from Other Apps** and set
Clipmonger to **Always allow**. If it is set to deny, nothing is captured
and **Capabilities** says "clipboard access denied, nothing is captured".
If it is set to ask, macOS may prompt on reads.

## Paste does not arrive

**macOS: Accessibility access.** To paste for you, Clipmonger needs
Accessibility access; it is the only permission it needs (Input Monitoring
is not). Turn Clipmonger on in System Settings, Privacy & Security,
Accessibility (macOS 12 and older: System Preferences, Security & Privacy,
Privacy, Accessibility). Without it Clipmonger copies the clip and you press
Cmd+V yourself. The app is signed ad hoc, so after each update turn
Clipmonger off and on again in that list, or remove it with **-** and let it
ask again.

**Windows: administrator windows.** Windows blocks normal programs from
typing into programs that run as administrator. Clipmonger pastes there
through a small helper:

1. Open the popup with the hotkey (not from the tray or the CLI) while the
   administrator window is active.
2. Pick a clip. The first time, Clipmonger asks whether to start the helper.
3. Approve the User Account Control prompt.

With the installer this is asked once. With the portable ZIP it is asked
each time Clipmonger starts. From a popup opened by the tray or by
`clipmonger-cli show-popup`, a paste into an administrator window is
refused and the popup shows why. See
[Getting started on Windows](getting-started-windows.md).

**Terminals on Linux.** Terminals paste with Ctrl+Shift+V. Clipmonger uses
it automatically for common terminals (GNOME Terminal, Console, Ptyxis,
Konsole, Xfce Terminal, Tilix, Terminator, Alacritty, kitty, foot,
WezTerm, Guake, Tilda, Terminology, st, COSMIC Terminal). In another
terminal the paste may do nothing; press Ctrl+Shift+V yourself (the clip is
on the clipboard), or try **Popup paste delivery** `unicode_inject` where
it is supported. See [Paste modes](paste-modes.md).

**"Copied to clipboard. Press Ctrl+V to paste."** Clipmonger could not press
the paste keys itself, so it left the clip on the clipboard. On Linux
Wayland this can also appear as a desktop notification. Press Ctrl+V (Cmd+V
on macOS).

**"Target changed. Pick the destination again."** The window you came from
was closed or changed before the paste. Click into the target window and
open the popup again.

**"Paste is not available right now"** or **"Could not run that permanent
clip"**. The paste route is unavailable at the moment, for example because
the helper or a required permission is missing. Check **Capabilities**.

**The paste arrives late or not at all after a while.** Clipmonger refuses
a paste that cannot happen promptly instead of typing into whatever window
has focus later. Pick the clip again.

## No tray icon

- **GNOME:** stock GNOME has no tray area. Install and enable the
  "AppIndicator and KStatusNotifierItem Support" extension. Ubuntu ships it
  enabled.
- **Sway, Hyprland and other bars:** the bar needs a tray (StatusNotifier)
  module, for example waybar's `tray`. If the bar starts after Clipmonger,
  the icon appears once the bar is up.
- **macOS:** the icon is in the menu bar.
- The icon changes when monitoring is paused, and turns to the attention
  state (orange) when the background service is unreachable or a feature
  is limited.

Without a tray icon, Clipmonger still works: open the popup with the hotkey
and Settings from the popup.

## Windows SmartScreen and macOS Gatekeeper

The programs are not code-signed (Windows) or notarized (macOS) yet.

**Windows.** SmartScreen shows **Windows protected your PC**. Click **More
info**, then **Run anyway**. For the portable ZIP, right-click it,
**Properties**, tick **Unblock** before extracting. When Windows 11 Smart
App Control is on, it blocks Clipmonger with no way to allow it. See
[Getting started on Windows](getting-started-windows.md).

**macOS.** The first start is blocked. In **Applications**, Control-click
**Clipmonger**, choose **Open**, then **Open** again. On macOS 15 and newer,
start it once, then go to System Settings, Privacy & Security, and click
**Open Anyway** next to the Clipmonger message. See
[Getting started on macOS](getting-started-macos.md).

## The popup opens on the wrong monitor

- With **Popup placement** `cursor` (the default), the popup opens at the
  mouse pointer, on the monitor under the pointer, and is moved so it fits
  on that monitor. With `center`, it is centred on the monitor under the
  pointer. Move the pointer to the monitor you want before pressing the
  hotkey.
- If Clipmonger cannot read the pointer position, it centres the popup on
  the current or primary monitor.
- On Windows, the popup stays inside the work area, above the taskbar.
- On most Wayland desktops, apps cannot position their own windows, so the
  compositor decides where the popup opens. On GNOME the Clipmonger
  extension places it. <!-- TODO verify: placement behaviour on KDE Plasma Wayland and wlroots compositors -->

## The popup says "Connection lost. Reconnecting."

The UI lost its connection to the background service. The UI restarts the
service if it crashed. If the message stays, quit Clipmonger (tray,
**Quit**), start it again and check `logs/daemon.log` in the profile folder.
A damaged database also stops the service from starting; see
[Data, backup and restore](backup-and-data.md#integrity-checks).

## Logs

| Log | Path in the profile folder | Contents |
|---|---|---|
| Background service | `logs/daemon.log` | Start-up, capture, paste and storage events. |
| UI | `logs/ui.log` | Popup, tray and Settings events. |
| Audit | `audit.log` | Security events, such as refused pastes. |

Each runtime log is kept to 1 MiB; the older part moves to `daemon.log.1`
or `ui.log.1`. Logs hold metadata only (event names, error classes, app
names), never the text or images you copied.

To read them without opening the folder: Settings, **About and Help**,
**Logs**, pick **Source** `daemon`, `ui` or `audit`, then **Refresh logs**.
It shows the last 100 lines.

Profile folders: Windows `%APPDATA%\clipmonger`, macOS
`~/Library/Application Support/clipmonger`, Linux
`~/.local/share/clipmonger`. See
[Data, backup and restore](backup-and-data.md#the-profile-folder).

## Reporting a bug

Report bugs at
[github.com/martinambrus/clipmonger/issues](https://github.com/martinambrus/clipmonger/issues).
Include:

1. The Clipmonger version (Settings, **About and Help**, **Version**).
2. Your system: Windows or macOS version, or on Linux the distribution,
   desktop and session type (`echo $XDG_SESSION_TYPE`).
3. How you installed it: Windows installer or portable ZIP, `.dmg` or
   `.zip`, `.deb`, `.rpm` or the per-user installer.
4. The **Capabilities** text from **About and Help**, or the output of:

   ```sh
   clipmonger-cli --profile-dir ~/.local/share/clipmonger status
   ```

5. What you did, what you expected and what happened, with any message the
   popup showed.
6. The last lines of `logs/daemon.log` and `logs/ui.log` from around the
   time of the problem.

Do not attach `clipstore.sqlite3`, the `blobs` folder, backups, `token` or
the output of `clipmonger-cli list` or `get`: they contain your clipboard
history or the connection secret.

## Known limitations

These are known in 0.4.0 and not bugs in your setup:

- The Windows programs are unsigned: SmartScreen warns on first run, and
  Smart App Control blocks them while it is on.
- On Windows, Alt+Tab from the open popup switches to the most recently
  used window rather than the one before it.
- A paste into a Windows administrator window needs the popup to be opened
  with the hotkey.
- The Windows installer does not upgrade in place: uninstall the old
  version first (your history is kept).
- The Linux packages are unsigned and there is no package repository yet.
- The macOS app is signed ad hoc and not notarized; Accessibility access
  must be granted again after each update.
- macOS 10.14 and older are not supported.
- There is no automatic update check yet. **Check for updates** in
  Settings has no effect.
