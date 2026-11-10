# The popup

The popup is where you search your clipboard history and paste from it. It
lists your history, most recently used first, and your permanent clips in
tabs.

## Opening the popup

There are three ways to open it:

- **Hotkey.** Press the popup hotkey in any application. The default is
  **Ctrl+Alt+H** on Windows and Linux, and **Ctrl+Option+H** on macOS. You can
  change it in Settings, Popup tab, **Popup hotkey** (see
  [Settings](settings.md)).
- **Tray icon.** Click the Clipmonger tray icon (the menu bar icon on macOS),
  or choose **Show popup** from its menu. Clicking the icon again while the
  popup is open closes it.
- **Command line.** Run `clipmonger-cli --profile-dir <profile> show-popup`.
  See [Command line](cli.md) for the profile path on your system.

Only the hotkey remembers which window you were in. Use the hotkey when you
want the clip pasted back into a specific window; this matters most for
administrator windows on Windows (see [Paste modes](paste-modes.md)).

On Sway, Hyprland and COSMIC, applications cannot register global shortcuts.
Bind the popup command in the compositor's config instead; Settings, Popup tab
shows the exact command with a **Copy** button. See
[Getting started on Linux](getting-started-linux.md).

If another application already holds the popup hotkey, the popup shows
**Popup shortcut ... is already in use. Reassign it in Settings.**

## Where it appears

The **Popup placement** setting (Settings, Popup tab) has two values:

| Value | Where the popup opens |
|---|---|
| `cursor` (default) | At the mouse pointer, kept inside the monitor the pointer is on. |
| `center` | Centred on the monitor the pointer is on. |

On GNOME the Clipmonger extension places the popup. On other Wayland desktops
the compositor decides where windows go, so the popup may not follow this
setting.

The popup sizes itself to its content each time it opens: wide enough for the
**Popup preview width** (in characters) and tall enough for the list, within
the limits of the monitor.

## Layout

From top to bottom:

1. The search field. It has keyboard focus when the popup opens.
2. The tabs: **History** and one tab per permanent clip group. Tabs are only
   shown when at least one group exists, and are hidden while you search.
3. Paging buttons.
4. The list.
5. The footer: messages, the status line (for example **25 items**,
   **12 results** or **Monitoring paused**) and the **Open settings window**
   button.

## Searching

Type to search. You do not need to click into the search field first.

- The search covers your whole history, not only the rows on screen, and all
  permanent clips in every group.
- Matching is a case-insensitive substring match. "inv" finds "Invoice" and
  "pinvoke".
- For text clips the full text is searched, not only the preview you see.
- For file lists the file names are searched.
- Permanent clips match by name or by the text they paste. They are listed
  before the history matches, shown as **[Group] Name**.
- Images have no text, so they never match a search.

While a search runs the popup shows **Searching…**. If nothing matches it
shows **No matches**. **Backspace** removes the last character; clear the field
to return to the normal list and tabs.

If some stored file lists could not be indexed, the popup shows **File search
may be incomplete; some files may need repair.**

## Paging

The list loads in pages.

Without a search:

- **Next page** loads the next page of older clips.
- **First page** returns to the newest clips.
- **End of history** appears below the last clip.

During a search:

- **Previous page** and **Next page** move through the results.
- **Page 1 of 3** shows where you are.
- **End of results** appears below the last match.

## Keyboard shortcuts

These are the defaults. Most can be changed with **Popup key bindings** in
Settings, Popup tab.

| Key | Action |
|---|---|
| Any character | Type into the search field |
| Backspace | Delete the last search character |
| Up / Down | Move the selection one row |
| Page Up / Page Down | Move the selection ten rows |
| Home / End | Select the first / last row |
| Enter | Paste the selected clip (or run the selected permanent clip) |
| Ctrl+Enter | Same as Enter |
| Esc | Close the popup without pasting |
| Delete | Delete the selected history clip |
| Ctrl+P | Pin or unpin the selected history clip |
| Alt+1 to Alt+9 | Switch to permanent group tab 1 to 9 |
| F5 | Reload the list |

Notes:

- Home, End, the arrow and page keys work on the list even while you are
  typing in the search field.
- **Delete** deletes a clip only when the search field does not have keyboard
  focus. While the search field has focus, Delete edits the search text.
  Click a row first to take focus out of the search field.
- **Ctrl+P** and **Delete** do nothing on permanent clips. Edit those in
  Settings (see [Permanent clips](permanent-clips.md)).
- On macOS, the shortcuts that use Ctrl use the Control key, not Command, and
  Alt is the Option key: **Control+P**, **Option+1**. Command+C, Command+X and
  Command+V work as usual in the search field.

<!-- TODO verify: Ctrl+E is bound to "edit" in keymap.rs but has no effect in v1; left out on purpose. -->

## Using the mouse

- Hover over a row to highlight it.
- Click a row to select it. Click the selected row again to paste it.
- If a preview is shortened (it ends with "…"), hover over it to see the
  longer text in a tooltip.
- Right-click a history row for **Pin entry** (or **Unpin entry**) and
  **Delete entry**. Permanent clips have no right-click menu.
- Click a tab to switch between **History** and a permanent group.
- Click **Open settings window** to open Settings.

## What the rows show

- **Text** shows a preview of up to **Popup preview lines** lines and
  **Popup preview width** characters (defaults: 3 lines, 80 characters).
- **Images** show a thumbnail. An image without text shows only the
  thumbnail.
- **File lists** (files you copied or cut in a file manager) show the file
  names.
- **Clips that were too large** to save appear as a row starting with **Too
  large — not saved**, followed by the limit and the formats that were
  offered. Nothing of their content is stored. The limit is **Monitor maximum
  bytes** in Settings, History tab.
- **Pinned clips** are kept when the history limits remove old clips.

## Deleting clips

Deleting a clip with **Delete** or **Delete entry** removes it from the
popup at once. There is no confirmation. The clip is marked as removed in the
database; **Purge removed clips** (Settings) or `clipmonger-cli purge-removed`
removes such clips for good. See [Backup and data](backup-and-data.md).

## Closing the popup

The popup closes when you:

- press **Esc**,
- paste a clip,
- click the tray icon,
- switch to another window, when **Dismiss popup on focus loss** is on (the
  default; Settings, Popup tab).

On Windows, Alt+Tab from the popup closes it and switches to the most recently
used window, which may not be the window you came from.

## What happens when you paste

1. The popup closes.
2. Clipmonger returns to the window that was active when you pressed the
   hotkey. If you opened the popup from the tray or the command line, it
   pastes into whatever window is in front once the popup has closed.
3. The clip is pasted using the delivery mode from Settings. By default
   Clipmonger puts the clip on the clipboard and presses Ctrl+V for you (Cmd+V
   on macOS, Ctrl+Shift+V in Linux terminals). See
   [Paste modes](paste-modes.md).
4. The clip moves to the top of the history.

A permanent clip is pasted the same way.

If the target window closed or changed, the paste is refused rather than sent
to a different window. The popup shows **Target changed. Pick the destination
again.** or reopens with the reason. If the paste could not be delivered at
all, the popup reopens with **Paste is not available right now** or a more
specific message.

## Messages

| Message | Meaning |
|---|---|
| **Nothing here yet** | The history (or the group) is empty. |
| **Loading history…** | The list is loading. |
| **Monitoring paused** | Clipboard monitoring is paused; new copies are not recorded. See [Privacy](privacy.md). |
| **Connection lost. Reconnecting.** | The UI lost its connection to the background daemon. |
| **History is getting ready. Refresh to try again.** | The database is still starting. Press F5. |
| **History is unavailable. Refresh to try again.** | The history could not be read. Press F5; see [Troubleshooting](troubleshooting.md). |
