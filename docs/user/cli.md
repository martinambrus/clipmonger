# Command line

Clipmonger has three programs:

| Program | What it is |
|---|---|
| `clipmonger` | The UI: popup, Settings and tray icon. Starts the background service. |
| `clipmonger-daemon` | The background service: capture, storage and hotkeys. Started by the UI. |
| `clipmonger-cli` | A command-line client for scripts and desktop shortcuts. Talks to the running background service. |

## Where the programs are

| Install | Folder |
|---|---|
| Windows (installer) | `C:\Program Files\Clipmonger` |
| Windows (portable ZIP) | The folder you extracted |
| macOS | `/Applications/Clipmonger.app/Contents/MacOS` |
| Linux `.deb` / `.rpm` | `/usr/bin` (programs in `/opt/clipmonger`) |
| Linux per-user install | `~/.local/bin` (programs in `~/.local/opt/clipmonger`) |

On macOS, link the CLI into your path to use it from a terminal:

```sh
sudo ln -s /Applications/Clipmonger.app/Contents/MacOS/clipmonger-cli /usr/local/bin/clipmonger-cli
```

## clipmonger-cli

```
clipmonger-cli --profile-dir <PROFILE_DIR> <COMMAND>
```

### Global options

| Option | Description |
|---|---|
| `--profile-dir <PROFILE_DIR>` | Required. The profile folder of the Clipmonger you want to control. It has no default. Put it before the command. |
| `-h`, `--help` | Show help. `clipmonger-cli --profile-dir x <command> --help` shows help for one command. |

The default profile folders are:

| System | `--profile-dir` value |
|---|---|
| Windows (installer) | `%APPDATA%\clipmonger` |
| Windows (portable) | The folder that holds `clipmonger.exe` |
| macOS | `~/Library/Application Support/clipmonger` |
| Linux | `~/.local/share/clipmonger` |

See [Data, backup and restore](backup-and-data.md#the-profile-folder).

The examples below use the Linux path. On macOS, quote the path because of
the space: `--profile-dir ~/Library/Application\ Support/clipmonger`. In the
Windows Command Prompt:

```bat
"C:\Program Files\Clipmonger\clipmonger-cli.exe" --profile-dir "%APPDATA%\clipmonger" status
```

### Commands

| Command | Description |
|---|---|
| `status` | Prints the background service's status, including its capabilities (what works on this system). |
| `list` | Prints recent history as JSON, including each entry's `id`. |
| `get <ID>` | Prints one history entry by id, as JSON. |
| `paste <ID>` | Pastes one history entry by id into the window that has the keyboard focus. |
| `show-popup` | Opens the popup. |
| `toggle-monitor` | Pauses clipboard monitoring, or resumes it if it is paused. |
| `exec-permanent <ID>` | Pastes a permanent clip by id. |
| `permanent-list` | Prints the permanent clips as JSON, including each clip's `id`. |
| `permanent-create <GROUP> <NAME> <BODY> [--hotkey <HOTKEY>]` | Creates a permanent clip. |
| `permanent-delete <ID>` | Deletes a permanent clip by id. Its hotkey is released. |
| `purge-removed` | Permanently deletes the history entries you deleted in the popup. |
| `stop` | Stops the background service. |
| `uninstall [--delete-history]` | macOS only: stops Clipmonger and removes its login agent; with `--delete-history` also deletes the profile. |
| `help` | Prints the command list. |

Ids are UUIDs, for example `0192f0a4-5b7e-7c3a-9d41-2f6e8a1b3c5d`. Take them
from the `id` fields of `list` or `permanent-list`.

### status

```sh
clipmonger-cli --profile-dir ~/.local/share/clipmonger status
```

Use it to check that the background service runs, and when reporting a bug
(see [Troubleshooting](troubleshooting.md#reporting-a-bug)).

### list and get

```sh
clipmonger-cli --profile-dir ~/.local/share/clipmonger list
clipmonger-cli --profile-dir ~/.local/share/clipmonger get 0192f0a4-5b7e-7c3a-9d41-2f6e8a1b3c5d
```

The output contains the text of your clips. Do not paste it into public bug
reports.

### paste

```sh
clipmonger-cli --profile-dir ~/.local/share/clipmonger paste 0192f0a4-5b7e-7c3a-9d41-2f6e8a1b3c5d
```

The clip goes to the focused window. Run from a terminal, that is usually
the terminal itself; bind the command to a desktop shortcut to paste into
another application. How the clip is delivered is described in
[Paste modes](paste-modes.md).

### show-popup

```sh
clipmonger-cli --profile-dir ~/.local/share/clipmonger show-popup
```

This is the command to bind on Sway, Hyprland and COSMIC, where apps cannot
register global hotkeys. Settings shows the exact command for your install
on the Popup tab. On Windows, a popup opened this way cannot paste into an
administrator window; open it with the hotkey for that (see
[Troubleshooting](troubleshooting.md#paste-does-not-arrive)).

### toggle-monitor

```sh
clipmonger-cli --profile-dir ~/.local/share/clipmonger toggle-monitor
```

While monitoring is paused nothing is captured and the popup shows
"Monitoring paused". See [Privacy](privacy.md).

### Permanent clips

```sh
clipmonger-cli --profile-dir ~/.local/share/clipmonger permanent-create Work Signature "Best regards, Alex" --hotkey ctrl+alt+k
clipmonger-cli --profile-dir ~/.local/share/clipmonger permanent-list
clipmonger-cli --profile-dir ~/.local/share/clipmonger exec-permanent 0192f0a4-5b7e-7c3a-9d41-2f6e8a1b3c5d
clipmonger-cli --profile-dir ~/.local/share/clipmonger permanent-delete 0192f0a4-5b7e-7c3a-9d41-2f6e8a1b3c5d
```

| Argument | Description |
|---|---|
| `<GROUP>` | Group name. |
| `<NAME>` | Clip name. |
| `<BODY>` | Text to paste. Quote it if it has spaces. |
| `--hotkey <HOTKEY>` | Optional global hotkey for the clip, written like `ctrl+alt+k` (see [Settings](settings.md#how-the-window-works) for the chord format). |

`exec-permanent` is the command to bind for a clip's hotkey on Sway,
Hyprland and COSMIC; Settings copies it with **Copy shortcut command**. See
[Permanent clips](permanent-clips.md).

### purge-removed

```sh
clipmonger-cli --profile-dir ~/.local/share/clipmonger purge-removed
```

The same as **Purge removed clips** on the Databases tab of Settings.

### stop

```sh
clipmonger-cli --profile-dir ~/.local/share/clipmonger stop
```

Stops the background service, which ends capture, hotkeys and paste. To
quit Clipmonger completely, UI included, use **Quit** in the tray menu.

### uninstall (macOS)

```sh
/Applications/Clipmonger.app/Contents/MacOS/clipmonger-cli \
  --profile-dir ~/Library/Application\ Support/clipmonger uninstall
```

| Option | Description |
|---|---|
| `--delete-history` | Also deletes the profile folder (history, settings, permanent clips). Refused if the folder does not contain a Clipmonger database. |

It stops Clipmonger, removes `~/Library/LaunchAgents/com.martinambrus.clipmonger.plist`
and prints what it did. Then drag Clipmonger.app to the Trash. On Windows and
Linux the command only prints "uninstall is for macOS; use the Windows
uninstaller, your package manager, or install.sh --uninstall on Linux".

### Output and exit codes

Successful commands print the result as one line of JSON on standard output.

| Exit code | Meaning |
|---|---|
| `0` | Success. `stop` also returns 0 when the service exits before it answers. |
| `1` | The background service is not running or cannot be reached (standard error: `daemon not running`), or it refused the request (standard error: the reason). `uninstall` also returns 1 when it cannot remove a file. |
| `2` | Wrong arguments (for example a missing `--profile-dir`), or `uninstall` on a system other than macOS. |

`daemon not running` also appears when `--profile-dir` points to the wrong
folder, because the CLI finds the service through files in that folder.

## clipmonger (the UI)

```
clipmonger [--profile-dir <dir>] [--minimized]
```

| Option | Description |
|---|---|
| `--profile-dir <dir>` | Use this profile folder instead of the default. The background service is started with the same folder. |
| `--minimized` | Start without opening the popup. Used by **Start on login**. `--popup` does the same. |

Without `--minimized`, the popup opens when the UI starts. If a UI is
already running for the same profile, a second start shows its popup and
exits.

On Sway and Hyprland, add the start command to the compositor config (Sway
`exec`, Hyprland `exec-once`), for example:

```
exec clipmonger --minimized --profile-dir ~/.local/share/clipmonger
```

Settings shows the exact command on the Startup tab.

## clipmonger-daemon

You do not normally start the background service yourself: the UI starts it
and starts it again if it crashes. If you do run it by hand, it needs the
profile folder:

```
clipmonger-daemon --profile-dir <path>
```

Without `--profile-dir` it prints
`usage: clipmonger-daemon --profile-dir <path> [--headless] [--script <file>]`
and exits with code 2. `--headless` and `--script` are for automated tests.
Only one service can run per profile; a second one exits with
`daemon refused: ...`.
