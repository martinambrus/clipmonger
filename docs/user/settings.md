# Settings

This page describes every option in the Settings window, tab by tab, in the
order the tabs appear. It applies to Clipmonger 0.4.0.

## Opening Settings

- Tray or menu bar icon: **Open settings**.
- Popup: **Open settings window** (see [The popup](popup.md)).

## How the window works

- **Filter settings** at the top searches the labels and the internal keys
  of every tab at once. While a filter is typed, the matching settings are
  listed under the name of their tab. Clicking a tab clears the filter.
- Changes are staged. Nothing is saved until you click **Apply**. **Revert**
  drops all staged changes. Both buttons stay at the bottom of the window and
  are enabled only while something is changed.
- The window says: "Apply saves each setting in order and stops at the first
  error." If one value is refused, the settings before it are already saved
  and the rest are not.
- An invalid value is shown in red under its field and is not staged. The
  messages are **Invalid value for this setting**, **Invalid chord for this
  setting**, **Invalid choice for this setting**, **Wrong type for this
  setting** and **Invalid list for this setting**.
- Lists (formats, app names) are typed as comma-separated text, for example
  `keepassxc, 1password`.
- Hotkeys are written as chords: modifiers and one key joined by `+`, for
  example `ctrl+alt+h`. Modifiers are `ctrl` (or `control`), `alt`, `shift`
  and `super` (also `win`, `meta`, `cmd`, `command`). Keys are a letter or
  digit, `F1` to `F24`, `Enter`, `Escape`, `Delete`, `Backspace`, `Tab`,
  `Space`, `Home`, `End`, `PageUp`, `PageDown` and the arrow keys (`up`,
  `down`, `left`, `right`). Case does not matter. On macOS, `alt` is the
  Option key.

### Theme controls

Every tab that lists settings (all tabs except **Databases** and **About and
Help**) starts with two theme controls. They apply at once, without
**Apply**.

| Label | What it does | Default | Range |
|---|---|---|---|
| **Dark theme** | Dark colours for the popup and Settings. Untick for the light theme. | On | On or off |
| **Font scale** | Text size of the popup and Settings. | 1.0 | 0.8 to 1.6 |

## History

What is captured and how much history is kept.

| Label | What it does | Default | Valid values | Platform notes |
|---|---|---|---|---|
| **Monitor formats** | Clipboard formats Clipmonger stores. Remove one to stop capturing it. | `text/plain;charset=utf-8, text/plain, text/html, text/rtf, image/png, text/uri-list` | Comma-separated list of these format names | `text/uri-list` is a file copy/cut list, `image/png` an image. |
| **Monitor maximum bytes** | Largest single clipboard item that is stored in full. A bigger item is kept as a visible stub that says it was too large (see [Data, backup and restore](backup-and-data.md#oversized-items)). | `16777216` (16 MiB) | 1 or more, in bytes | |
| **Monitor minimum length** | Text copies shorter than this many bytes are not stored. `0` stores everything. | `0` | 0 or more | In 0.4.0 this is applied to clipboard copies on Windows only. |
| **Toggle monitoring with Scroll Lock** | The Scroll Lock key pauses and resumes monitoring. | Off | On or off | Windows only. Hidden on macOS; shown on Linux but has no effect there. |
| **Monitor ignored apps** | Applications whose copies are never stored. Works the same way as **Privacy denylist**; both lists are combined. | Empty | Comma-separated app names | See [Privacy](privacy.md) for how names are matched. |
| **Capture primary selection** | Also records the Linux primary selection (text you select, pasted with the middle mouse button). | Off | On or off | Linux only. Hidden on macOS. |
| **History maximum items** | How many history entries are kept. When a new entry would exceed it, the oldest unpinned entries are removed. | `750` | 1 or more | Pinned entries and permanent clips are never removed by this limit. |
| **History maximum store bytes** | Total size of stored history. The oldest unpinned entries are removed when it is exceeded. | `536870912` (512 MiB) | 1 or more, in bytes | |
| **History retention days** | Entries older than this many days are removed. `0` turns the age limit off. | `0` | 0 or more | |
| **History dedupe window** | Intended number of recent entries checked for a duplicate copy. | `100` | 0 or more | In 0.4.0 the value is saved but not used: Clipmonger always checks the last 100 entries and moves a repeated copy to the top instead of adding a new row. |

**Privacy strict mode** (Privacy tab) halves the three history limits while
it is on.

## Popup

How the popup opens, looks and pastes. For using the popup itself, see
[The popup](popup.md). For the paste options, see
[Paste modes](paste-modes.md).

| Label | What it does | Default | Valid values | Platform notes |
|---|---|---|---|---|
| **Popup hotkey** | Global hotkey that opens the popup. | `ctrl+alt+h` (Ctrl+Option+H on macOS) | A chord | See the notes below the table. |
| **Popup placement** | Where the popup opens: at the mouse pointer (`cursor`) or centred on the monitor under the pointer (`center`). | `cursor` | `cursor`, `center` | The popup is always kept inside the monitor. Where the desktop does not let apps position their windows (most Wayland sessions), the desktop decides. |
| **Popup preview width** | Width of the entry previews, in characters. | `80` | 1 or more | The popup is never wider than 30% of the monitor's work area. |
| **Popup preview lines** | Number of text lines shown per entry. | `3` | 1 or more | |
| **Dismiss popup on focus loss** | Closes the popup when you click another window. | On | On or off | |
| **Popup key bindings** | Overrides the popup's keyboard shortcuts. | `{}` (built-in keys) | JSON object, see below | |
| **Popup paste chords** | Intended per-application paste chord. | `[{"app":"*","chord":"ctrl+v"}]` | JSON list of `{"app": ..., "chord": ...}` | In 0.4.0 the value is saved but not used. Clipmonger chooses Ctrl+V, or Ctrl+Shift+V for known Linux terminals, by itself. |
| **Popup paste delivery** | How a picked clip is delivered: `clipboard_restore` writes the clip to the clipboard and presses Ctrl+V (Cmd+V on macOS); `unicode_inject` types the text as keystrokes. | `clipboard_restore` | `clipboard_restore`, `unicode_inject` | `unicode_inject` is used on Windows and macOS for plain text only; images, files and text over **Popup unicode limit** still use the clipboard. <!-- TODO verify: whether any Linux backend honours unicode_inject (X11 engine appears to use the clipboard route only) --> |
| **Restore clipboard after paste** | Puts your previous clipboard content back after the paste. It is only restored if nothing else was copied in the meantime. | Off | On or off | |

**Popup hotkey notes:**

- On Windows, X11 desktops and GNOME with the Clipmonger extension, a new
  hotkey works as soon as you click **Apply**.
- Where the desktop's shortcut service owns the hotkey (for example KDE
  Plasma on Wayland), Settings shows "Your desktop asks you to confirm a new
  shortcut". Approve it in the dialog the desktop shows.
- On Sway, Hyprland and COSMIC apps cannot set global shortcuts. Settings
  shows "This desktop does not let apps set global shortcuts..." with the
  exact command to bind and a **Copy** button. See
  [Getting started on Linux](getting-started-linux.md).
- If another program already holds the hotkey, the popup shows "Popup
  shortcut ... is already in use. Reassign it in Settings."

**Popup key bindings** takes a JSON object that maps an action to a chord,
for example `{"paste": "ctrl+j", "dismiss": "ctrl+q"}`. A binding replaces
the built-in key for that action. The actions are `paste`, `paste_into`,
`dismiss`, `remove`, `edit`, `pin`, `refresh`, `navigate_up`,
`navigate_down`, `page_up`, `page_down`, `home` and `end`. The built-in keys
are listed in [The popup](popup.md#keyboard-shortcuts).

## Tray

| Label | What it does | Default | Valid values | Platform notes |
|---|---|---|---|---|
| **Tray icon enabled** | Intended to show or hide the tray icon. | Not set (shown unticked) | On or off | In 0.4.0 the value is saved but not used: the tray icon is always shown. |

## Startup

| Label | What it does | Default | Valid values | Platform notes |
|---|---|---|---|---|
| **Start on login** | Starts Clipmonger, with the popup hidden, when you log in. | Off | On or off | See below. |

**Start on login** is applied to the operating system when you click
**Apply**:

- Windows: a `clipmonger` entry in the current user's Run key.
- macOS: a login agent, `~/Library/LaunchAgents/com.martinambrus.clipmonger.plist`.
- Linux: `~/.config/autostart/com.martinambrus.clipmonger.desktop` (or under
  `$XDG_CONFIG_HOME`).
- Portable mode (Windows ZIP): not available. The checkbox is disabled and
  Settings says "Starting automatically is unavailable in portable mode."
- Sway and Hyprland do not run login items. Settings shows the command to
  add to the compositor's startup config (Sway: `exec`, Hyprland:
  `exec-once`) with a **Copy** button.

## Privacy

See [Privacy](privacy.md) for what these options protect against.

| Label | What it does | Default | Valid values | Platform notes |
|---|---|---|---|---|
| **Privacy strict mode** | Stores plain text only (copies that offer no plain text are skipped) and halves **History maximum items**, **History maximum store bytes** and **History retention days**. | Off | On or off | |
| **Privacy denylist** | Applications whose copies are never stored. Combined with **Monitor ignored apps**. | Empty | Comma-separated app names | Names are matched without regard to case: against the executable on Windows, the window class on X11, the bundle id or app name on macOS, and the app id on GNOME. |

<!-- TODO verify: on Wayland without the GNOME extension (KDE, Sway, Hyprland) the data-control capture path reports no source app, so the denylist likely cannot match there. -->

## Advanced

| Label | What it does | Default | Valid values | Platform notes |
|---|---|---|---|---|
| **Monitor poll interval** | How often, in milliseconds, the capture loop wakes up. On macOS it is how often the clipboard is checked for changes. | `500` | 1 or more; the daemon uses at least 50 and at most 5000 | |
| **Primary debounce interval** | How long, in milliseconds, a primary selection must stay unchanged before it is stored, so dragging a selection does not store every step. | `500` | 0 or more | Linux only, with **Capture primary selection** on. |
| **Popup unicode limit** | Longest text that **unicode_inject** types as keystrokes. Longer text is pasted through the clipboard. | `65536` | 1 or more | Windows and macOS. <!-- TODO verify: unit on Windows (code comment says bytes, macOS counts characters) --> |
| **Popup delivery dwell** | With **Restore clipboard after paste** on, how long, in milliseconds, the clip stays on the clipboard before the previous content is restored. | `1000` | 0 or more | Windows. |
| **Logging level** | Intended log detail level. | Not set | `trace`, `debug`, `info`, `warn`, `error` | In 0.4.0 the value is saved but not used. |

Capture settings (**Monitor ignored apps**, **Privacy denylist**, **Capture
primary selection**, **Primary debounce interval**, **Monitor poll
interval**) are read by the capture backend when Clipmonger starts. To be
sure a change applies everywhere, quit Clipmonger (tray, **Quit**) and start
it again. <!-- TODO verify: which of these the daemon re-reads live per platform (Windows reads the denylist live; X11 and macOS pass it to the monitor once at start). -->

## Permanent Clips

This tab manages permanent clips and their groups. Changes here save
immediately ("Permanent clips save immediately"); **Apply** is not needed.

- A text field with **New group** creates a group.
- The group list selects a group. **Delete group** works only on an empty
  group ("Group still has clips" otherwise).
- **New permanent clip** opens the editor with **Name**, **Group**,
  **Hotkey** and **Text to paste**, and **Save clip** / **Cancel**.
- Selecting a clip opens it in the editor. **Delete permanent clip** asks
  "Delete this permanent clip? This cannot be undone."
- On Sway, Hyprland and COSMIC, **Copy shortcut command** copies the command
  to bind for the selected clip.

Full instructions: [Permanent clips](permanent-clips.md).

## General

| Label | What it does | Default | Valid values | Platform notes |
|---|---|---|---|---|
| **Permanent clip groups** | Older group list kept for compatibility. | Not set | Comma-separated names | Not used in 0.4.0. Manage groups on the **Permanent Clips** tab. |
| **Check for updates** | Intended automatic update check. | Not set (shown unticked) | On or off | In 0.4.0 the value is saved but not used; there is no update check yet. |

## Databases

Profile and backup actions. These act at once; **Apply** is not used.

| Control | What it does |
|---|---|
| **Profile path** | Shows the folder that holds your history and settings. |
| **Purge removed clips** | Permanently deletes the entries you deleted in the popup and frees their space. |
| **Backup file path** | Path of the backup file to create or restore. Default: `backup.zip` in the profile folder. |
| **Create backup** | Writes a backup of history, permanent clips and settings to **Backup file path**. On success: "Backup saved:" and the path. |
| **Restore backup** | Replaces history, permanent clips and settings with the backup at **Backup file path**. On success: "Backup restored." |

See [Data, backup and restore](backup-and-data.md) for details.

## About and Help

| Item | What it shows |
|---|---|
| **Version** | The installed Clipmonger version. |
| Screen reader note | "Screen reader support: not available in this build. Keyboard operation is fully supported." |
| **Capabilities** | What works on this system: capture, hotkey, paste, tray and start on login, with the reason when something is limited. |
| **Logs** | The last 100 lines of a log. **Source** picks `audit`, `daemon` or `ui`; **Refresh logs** reloads. |

Use this tab when reporting a problem; see
[Troubleshooting](troubleshooting.md#reporting-a-bug).

## The configuration file

Settings are saved in `config.toml` in the profile folder:

| Install | Path |
|---|---|
| Windows (installer) | `%APPDATA%\clipmonger\config.toml` |
| Windows (portable ZIP) | `config.toml` next to `clipmonger.exe` |
| macOS | `~/Library/Application Support/clipmonger/config.toml` |
| Linux | `~/.local/share/clipmonger/config.toml` (or under `$XDG_DATA_HOME`) |

With `--profile-dir`, it is in that folder instead.

The file is TOML. Each setting is under a section named after the first part
of its key, for example:

```toml
[popup]
hotkey = "ctrl+alt+h"
placement = "cursor"

[history]
max_items = 750

[privacy]
strict = false
denylist = ["keepassxc"]
```

Hand-editing is possible, with these rules:

1. Quit Clipmonger first (tray, **Quit**). The file is read when Clipmonger
   starts and rewritten in full each time you click **Apply**, so edits made
   while it runs are ignored and then overwritten.
2. Comments and formatting are not kept after the next **Apply**. Unknown
   keys are kept.
3. A file that is not valid TOML is ignored: Clipmonger starts with the
   default settings and logs a `startup.config_failed` warning in the daemon
   log. The next **Apply** replaces the broken file.
4. The file bypasses the Settings window's checks. For example, a hand-set
   `0` for `history.max_items` or `history.max_store_bytes` means no limit.
   Settings itself requires at least 1.
5. Do not change `[autostart]` by hand; use **Start on login**, which also
   updates the operating system.

The file also has keys that Settings does not show: `config_version`,
`[windows] helper_auto_elevate` and the theme under `[ui.theme]`.

Settings can also be overridden for one run with environment variables named
`CLIPMONGER_<SECTION>_<KEY>`, for example `CLIPMONGER_POPUP_HOTKEY=ctrl+alt+j`
or `CLIPMONGER_PRIVACY_DENYLIST=keepassxc,1password` (lists are
comma-separated). They exist for `monitor`, `history`, `popup` (except
`keys`, `paste_chords`, `delivery` and `restore_clipboard`), `privacy` and
`autostart.enabled`, and are read by the background service when it starts.
An overridden value becomes part of the running settings, so the next
**Apply** writes it to `config.toml`.
