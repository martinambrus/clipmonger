# Getting started on Windows

This page takes you from download to your first paste. The step-by-step
walk-through of every Windows warning, with screenshots described in words,
is in [Installing on Windows](../install-windows.md).

## Requirements

- 64-bit Windows 10 or Windows 11.
- No other software. Clipmonger brings everything it needs.

## Pick a download

Download the files from the
[latest release](https://github.com/martinambrus/clipmonger/releases/latest).
There are two Windows downloads:

| File | What it is | Where your data lives |
| --- | --- | --- |
| `clipmonger-<version>-windows-x86_64-setup.exe` | Installer. Installs to `C:\Program Files\Clipmonger` and adds a Start menu entry. | `%APPDATA%\clipmonger` (usually `C:\Users\<you>\AppData\Roaming\clipmonger`) |
| `clipmonger-<version>-windows-x86_64.zip` | Portable copy. Nothing is installed. | The folder you extract it to, next to the programs |

Use the installer unless you have a reason not to. Two things work better
with it:

- **Start on login** works only with the installer. In the portable copy the
  option is greyed out.
- **Pasting into administrator windows** asks for permission once with the
  installer. The portable copy asks every time Clipmonger starts (see
  [Pasting into administrator windows](#pasting-into-administrator-windows)).

Also download `SHA256SUMS` from the same release. The
[Windows walk-through](../install-windows.md#1-check-the-checksum) shows how
to check your download against it in PowerShell.

## Install

### Installer

1. Run `clipmonger-<version>-windows-x86_64-setup.exe`.
2. The files are not code-signed yet, so Windows SmartScreen may show
   **Windows protected your PC**. Click **More info**, check that the file
   name is the one you downloaded, then click **Run anyway**.
3. Approve the User Account Control prompt. It shows an unknown publisher.
4. Click through the installer and finish it.

If your browser blocks the download before you can run it, see
[Browser download warning](../install-windows.md#2-browser-download-warning).

### Portable ZIP

1. Right-click the ZIP, choose **Properties**, tick **Unblock** on the
   General tab and click **OK**. If there is no Unblock box, skip this step.
2. Extract the ZIP into a folder you own, for example
   `Documents\Clipmonger`. Do not extract it into `Program Files`: the
   portable copy writes its data into its own folder.
3. Run `clipmonger.exe` from that folder. If **Windows protected your PC**
   appears, click **More info**, then **Run anyway**.

The file `portable.marker` in that folder is what makes the copy portable.
Leave it there. Without it, Clipmonger stores its data in
`%APPDATA%\clipmonger` like the installed version.

## First start

1. Start **Clipmonger** from the Start menu (installer) or run
   `clipmonger.exe` (portable).
2. Clipmonger starts its background service and the popup opens. Press
   **Esc** to close it.
3. The Clipmonger icon appears in the notification area of the taskbar. On
   Windows 11 it may be hidden behind the **^** arrow; drag it onto the
   taskbar to keep it visible.

Clipmonger has no main window. It runs in the background, and you reach it
through the hotkey and the tray icon.

### The tray icon

Click the icon to open the popup. Right-click it for the menu:

- **Show popup**
- **Pause monitoring** (shows **Resume monitoring** while paused)
- **Pause for one hour**
- **Open settings**
- **About**
- **Quit**

**Quit** stops Clipmonger completely, including its background service.

## Check that it works

1. Open Notepad and type a few words. Select them and press **Ctrl+C**.
2. Copy something else, for example a line from a web page.
3. Click into Notepad where you want to paste.
4. Press **Ctrl+Alt+H**. The popup opens with your copies, newest first.
5. Use the arrow keys to pick the first thing you copied and press
   **Enter**.

The popup closes and the text is pasted into Notepad. If nothing is pasted,
the clip is still on the clipboard: press **Ctrl+V**. See
[Troubleshooting](troubleshooting.md) if it keeps happening.

To learn the rest of the popup (search, tabs, keys) read
[The popup](popup.md). To change the hotkey, open Settings, **Popup** tab,
**Popup hotkey**.

## Pasting into administrator windows

Windows does not let a normal program send keystrokes to a program that runs
as administrator, such as an elevated Command Prompt or Registry Editor.
Clipmonger uses a small helper that runs with administrator rights for
these windows.

The first time a window that runs as administrator is in front, Clipmonger
asks **Enable Clipmonger in admin windows?**

- **Yes** shows a User Account Control prompt. Approve it.
  - Installer: the helper is set up once. From then on it starts silently
    when you sign in, and Clipmonger does not ask again.
  - Portable copy: the helper runs until Clipmonger quits. Nothing is
    installed, so Clipmonger asks again the next time it starts.
- **No** leaves administrator windows out. Clipmonger asks again the next
  time it starts. Pasting into normal windows works either way.

Two more rules:

- Open the popup with the hotkey when you want to paste into an
  administrator window. A popup opened from the tray icon or from
  `clipmonger-cli show-popup` refuses that paste and shows why.
- If Clipmonger itself runs as administrator (for example under the
  built-in Administrator account), it pastes into those windows directly and
  asks nothing.

## Start on login

Start on login is off by default. To turn it on:

1. Open Settings (tray icon, **Open settings**).
2. Go to the **Startup** tab.
3. Tick **Start on login** and click **Apply**.

Clipmonger then starts in the background when you sign in, without opening
the popup.

<!-- TODO verify: Clipmonger reads the Task Manager "Startup apps" disable flag (StartupApproved); the code marks this VERIFY on native Windows. If confirmed, add: "You can also switch it off in Task Manager, Startup apps." -->

The portable copy cannot start on login. The checkbox is greyed out and
Settings shows **Starting automatically is unavailable in portable mode.**

## Pause with Scroll Lock

You can let the Scroll Lock key pause and resume recording:

1. Open Settings, **History** tab.
2. Tick **Toggle monitoring with Scroll Lock** and click **Apply**.

Each press of Scroll Lock now switches monitoring between paused and
running. The tray icon changes while paused. The option is off by default.
Very short taps can be missed, and some keyboards have no Scroll Lock key.
Pausing from the tray menu works the same way on every keyboard. More on
pausing and what is recorded: [Privacy](privacy.md).

## Updating

There is no automatic update check yet. To update:

### Installer

The installer does not install over an existing copy. If you try, it stops
with **The installation directory already exists.**

1. Quit Clipmonger (tray icon, **Quit**).
2. Open Windows Settings, **Apps**, find **Clipmonger** and uninstall it.
3. When the uninstaller shows **Delete my default Clipmonger profile and
   everything inside it**, leave it unticked. Your history, settings and
   permanent clips stay.
4. Run the new installer.

### Portable copy

1. Quit Clipmonger (tray icon, **Quit**).
2. Unblock the new ZIP, as in the install steps.
3. Extract it into the same folder and let it replace the existing files.
   Your data files are not in the ZIP, so they are kept.

## Uninstalling

### Installer

1. Quit Clipmonger (tray icon, **Quit**).
2. Open Windows Settings, **Apps**, find **Clipmonger** and click
   **Uninstall**. The Start menu folder **Clipmonger** also has an
   **Uninstall** entry.
3. Approve the User Account Control prompt.
4. To delete your history, settings and permanent clips as well, tick
   **Delete my default Clipmonger profile and everything inside it** and
   confirm. Leave it unticked to keep them.

The uninstaller also removes the start-on-login entry and the administrator
helper.

### Portable copy

1. Quit Clipmonger (tray icon, **Quit**).
2. Delete the folder. This deletes your history too, because it is stored
   there.

To keep your history before removing Clipmonger, make a backup first. See
[Data, backup and restore](backup-and-data.md).

## Windows gotchas

- **SmartScreen.** The files are not signed, so SmartScreen warns on the
  first start of each new file. **More info**, then **Run anyway**, gets
  past it. Check the checksum first.
- **Smart App Control (Windows 11).** When Smart App Control is on, Windows
  blocks unsigned programs and shows no **Run anyway** button. Clipmonger
  cannot run then. You find the setting in Windows Security, **App & browser
  control**, **Smart App Control**. Turning it off is your decision; on some
  Windows versions it cannot be turned back on without reinstalling Windows.
  Details: [Smart App Control](../install-windows.md#smart-app-control-windows-11).
- **Alt+Tab from the popup.** With the popup open, Alt+Tab switches to the
  most recently used window, not necessarily the one before it.
- **Hotkey already taken.** If another program owns **Ctrl+Alt+H**, the
  popup shows **Popup shortcut ... is already in use. Reassign it in
  Settings.** with the key combination in place of the dots. Pick another key in Settings, **Popup**, **Popup hotkey**. Some
  combinations are **Reserved by Windows** and cannot be used.
- **Start menu icon after an update.** Start menu search can keep showing a
  generic icon until Windows refreshes its icon cache. The program itself is
  fine.

## Next steps

- [The popup](popup.md)
- [Permanent clips](permanent-clips.md)
- [Paste modes](paste-modes.md)
- [Privacy](privacy.md)
- [Settings](settings.md)
- [Data, backup and restore](backup-and-data.md)
- [Command line](cli.md)
- [Troubleshooting](troubleshooting.md)
