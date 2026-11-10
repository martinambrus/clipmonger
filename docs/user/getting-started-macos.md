# Getting started on macOS

This page takes you from download to your first paste on a Mac.

## Requirements

- macOS 10.15 Catalina or newer. macOS 10.14 and older are not supported.
- An Intel or Apple Silicon Mac. There is one universal app for both.

## Download

Download one of these from the
[latest release](https://github.com/martinambrus/clipmonger/releases/latest):

- `clipmonger-<version>-macos-universal.dmg` (disk image), or
- `clipmonger-<version>-macos-universal.zip` (the same app in a ZIP).

Also download `SHA256SUMS`. To check your download, open Terminal in the
download folder and run:

```sh
shasum -a 256 clipmonger-*-macos-universal.*
```

Each printed checksum must match the line for that file in `SHA256SUMS`. If
it does not, delete the file and download it again.

## Install

1. Open the `.dmg` (or double-click the `.zip` to unpack it).
2. Drag **Clipmonger** into **Applications**.
3. If you used the `.dmg`, eject it.

Start Clipmonger from **Applications**, not from inside the disk image.

## First start: Gatekeeper

Clipmonger is signed ad hoc and not notarized by Apple yet, so macOS blocks
the first start. How you get past it depends on your macOS version.

### macOS 14 Sonoma and older

1. Open **Applications** in Finder.
2. Control-click (or right-click) **Clipmonger** and choose **Open**.
3. In the dialog, click **Open** again.

macOS remembers this choice. Later starts work normally.

### macOS 15 Sequoia and newer

On these versions **Open** from the context menu no longer skips the check.

1. Double-click **Clipmonger** in **Applications**. macOS says it cannot
   check the app. Click **Done** (or **OK**).
2. Open System Settings, **Privacy & Security**.
3. Scroll down to the message about Clipmonger and click **Open Anyway**.
4. Confirm with **Open Anyway** again and enter your password if asked.

<!-- TODO verify: exact button labels of the macOS 15 / 26 Gatekeeper dialog ("Done" vs "OK") -->

## Permissions

### Accessibility

Clipmonger needs **Accessibility** access to paste a picked clip for you.
macOS asks for it the first time Clipmonger starts.

1. In the prompt, click **Open System Settings** (on older systems **Open
   System Preferences**).
2. Turn **Clipmonger** on in the Accessibility list. Enter your password if
   asked.

To grant it later, open System Settings, **Privacy & Security**,
**Accessibility** (macOS 12 and older: System Preferences, **Security &
Privacy**, **Privacy**, **Accessibility**) and turn **Clipmonger** on.

You do not need to restart Clipmonger. It notices the change within a few
seconds.

Without Accessibility, Clipmonger still records your history and the popup
still works. When you pick a clip, it is put on the clipboard and you press
**Cmd+V** yourself. The **Capabilities** text in Settings, **About and
Help**, then reports paste as degraded and names the System Settings path.

Clipmonger does not need **Input Monitoring** or **Screen Recording**. Its
hotkeys use the standard macOS hotkey service, which needs no permission.

### Paste from Other Apps

Newer macOS versions can ask before an app reads what other apps copied. If
macOS asks whether Clipmonger may paste from other apps, allow it. If you
see no history at all, open System Settings, **Privacy & Security**, **Paste
from Other Apps**, and set Clipmonger to **Always allow**. With **Ask**,
macOS may prompt while Clipmonger reads the clipboard; with a deny setting,
nothing is recorded.

<!-- TODO verify: the macOS version that first shows "Paste from Other Apps" in System Settings and the exact prompt wording (code checks NSPasteboard.accessBehavior, available from macOS 15.4) -->

## The menu bar icon

Clipmonger has no Dock icon and no main window. It lives in the menu bar.

Click the icon to open the popup. Right-click (or Control-click) it for the
menu:

- **Show popup**
- **Pause monitoring** (shows **Resume monitoring** while paused)
- **Pause for one hour**
- **Open settings**
- **About**
- **Quit**

**Quit** stops Clipmonger completely, including its background service.

## Check that it works

1. Open TextEdit and create a new document. Type a few words, select them and
   press **Cmd+C**.
2. Copy something else, for example a line from a web page.
3. Click into the TextEdit document where you want to paste.
4. Press **Ctrl+Option+H**. The popup opens with your copies, newest first.
5. Use the arrow keys to pick the first thing you copied and press
   **Return**.

The popup closes and the text is pasted. If it is only copied, check the
Accessibility permission above and press **Cmd+V**.

To learn the rest of the popup (search, tabs, keys) read
[The popup](popup.md). To change the hotkey, open Settings, **Popup** tab,
**Popup hotkey**.

## Start on login

Start on login is off by default. To turn it on:

1. Open Settings (menu bar icon, **Open settings**).
2. Go to the **Startup** tab.
3. Tick **Start on login** and click **Apply**.

Clipmonger adds a login item at
`~/Library/LaunchAgents/com.martinambrus.clipmonger.plist`. From the next
login it starts in the background without opening the popup.

<!-- TODO verify: whether macOS 13+ shows a "Background Items Added" notification and lists Clipmonger under Login Items > Allow in the Background -->

## The command line tool

`clipmonger-cli` is inside the app. To run it from Terminal without the full
path, link it into `/usr/local/bin`:

```sh
sudo mkdir -p /usr/local/bin
sudo ln -s /Applications/Clipmonger.app/Contents/MacOS/clipmonger-cli /usr/local/bin/clipmonger-cli
```

Every command needs your profile folder. For example, to check that
Clipmonger is running:

```sh
clipmonger-cli --profile-dir "$HOME/Library/Application Support/clipmonger" status
```

All commands are in [Command line](cli.md).

## Where your data lives

History, settings and permanent clips are in
`~/Library/Application Support/clipmonger`. Settings, **Databases** shows the
exact path under **Profile path**. Backups: see
[Data, backup and restore](backup-and-data.md).

## Updating

There is no automatic update check yet. To update:

1. Quit Clipmonger (menu bar icon, **Quit**).
2. Download the new version and drag **Clipmonger** into **Applications**.
   Choose **Replace** when Finder asks.
3. Start it. You may have to get past Gatekeeper again, as on the first
   start.
4. Grant Accessibility again (see below).

Your history and settings are not touched.

### Grant Accessibility again after each update

The app is signed ad hoc, so macOS treats every new version as a different
app. The old Accessibility switch no longer applies to it, even though it
still looks on. After each update:

1. Open System Settings, **Privacy & Security**, **Accessibility**.
2. Turn **Clipmonger** off, then on again.

If that does not help, select **Clipmonger**, remove it with the **-**
button, start Clipmonger again and allow it when macOS asks.

## Uninstalling

1. Open Terminal and run:

   ```sh
   /Applications/Clipmonger.app/Contents/MacOS/clipmonger-cli --profile-dir "$HOME/Library/Application Support/clipmonger" uninstall
   ```

   This stops Clipmonger and removes its login item. Your history is kept.
   To delete your history, settings and permanent clips as well, add
   `--delete-history` at the end.
2. Drag **Clipmonger** from **Applications** to the Trash.
3. If you made the `clipmonger-cli` link, remove it:
   `sudo rm /usr/local/bin/clipmonger-cli`.
4. Optional: remove **Clipmonger** from the Accessibility list in System
   Settings with the **-** button.

## macOS gotchas

- **Paste does nothing in password fields.** When a password field or
  Terminal's **Secure Keyboard Entry** is active, macOS blocks synthetic
  keystrokes. Clipmonger leaves the clip on the clipboard; press **Cmd+V**.
- **Accessibility after updates.** See
  [Grant Accessibility again after each update](#grant-accessibility-again-after-each-update).
- **No Scroll Lock or primary selection options.** Macs have neither, so
  Settings hides those options.
- **Colour menu bar icon.** The icon is in colour rather than a monochrome
  template, so it does not follow the menu bar's light and dark tint.
- **Hotkey already taken.** If another app owns **Ctrl+Option+H**, the popup
  shows **Popup shortcut ... is already in use. Reassign it in Settings.**
  with the key combination in place of the dots. Pick another key in
  Settings, **Popup**, **Popup hotkey**.

## Next steps

- [The popup](popup.md)
- [Permanent clips](permanent-clips.md)
- [Paste modes](paste-modes.md)
- [Privacy](privacy.md)
- [Settings](settings.md)
- [Data, backup and restore](backup-and-data.md)
- [Command line](cli.md)
- [Troubleshooting](troubleshooting.md)
