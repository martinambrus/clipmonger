# Data, backup and restore

This page explains where Clipmonger keeps your history and settings, how
history is limited, and how to back it up, move it and delete it.

## The profile folder

Everything Clipmonger stores for you is in one folder, the profile:

| Install | Profile folder |
|---|---|
| Windows (installer) | `%APPDATA%\clipmonger` (usually `C:\Users\<you>\AppData\Roaming\clipmonger`) |
| Windows (portable ZIP) | The folder you extracted the ZIP into, next to `clipmonger.exe` |
| macOS | `~/Library/Application Support/clipmonger` |
| Linux (`.deb`, `.rpm` or per-user install) | `~/.local/share/clipmonger` (`$XDG_DATA_HOME/clipmonger` if that variable is set) |

The portable ZIP is recognised by an empty file named `portable.marker` next
to the programs. When it is present, the profile is that folder.

Settings shows the folder in use on the **Databases** tab, under **Profile
path**.

### What is in it

| Name | Contents |
|---|---|
| `clipstore.sqlite3` | The SQLite database: history entries, small payloads, permanent clips and their groups. |
| `clipstore.sqlite3-wal`, `clipstore.sqlite3-shm` | SQLite working files. They belong to the database; never copy the database without them while Clipmonger runs. |
| `blobs/` | Large payloads (over 256 KiB), such as images, stored as separate files. |
| `config.toml` | Your settings (see [Settings](settings.md#the-configuration-file)). |
| `audit.log` | Security events, such as paste refusals. Metadata only. |
| `logs/daemon.log`, `logs/ui.log` | Diagnostic logs. Each is rotated to a `.1` file at 1 MiB. Metadata only, never clip contents. |
| `token`, `rendezvous.json`, `clipmonger.sock`, `*.lock` | Connection details and locks of the running program. `token` is the secret the UI and the CLI use to talk to the background service. |
| `ui-window-state.json` | Size and position of the Settings window. |
| `backup.zip` | Present only if you created a backup at the default path. |

On Unix systems `config.toml` and `token` are readable by your user only.

## Profiles and `--profile-dir`

You can run Clipmonger with a different profile folder, for example to keep
work and personal history apart or to test settings:

```sh
clipmonger --profile-dir ~/clipmonger-work
```

- The UI starts its background service with the same folder. The folder is
  created if it does not exist.
- Each profile has its own history, permanent clips, settings and logs.
- Only one UI and one background service run per profile. Starting the UI
  again for the same profile shows the running UI's popup instead.
- `clipmonger-cli` always needs the folder too:
  `clipmonger-cli --profile-dir ~/clipmonger-work status`. See
  [Command line](cli.md).
- Two profiles that run at the same time both try to register their popup
  hotkey. Give each a different **Popup hotkey**.

## History depth and retention

History is trimmed automatically each time a new entry is stored. The
oldest entries go first. Three limits apply, set on the **History** tab of
Settings:

| Limit | Default |
|---|---|
| **History maximum items** | 750 entries |
| **History maximum store bytes** | 512 MiB |
| **History retention days** | 0 (no age limit) |

- Pinned entries are never removed by these limits.
- Permanent clips are not history and are never removed by them.
- **Privacy strict mode** halves all three limits (375 entries, 256 MiB,
  and half the retention days when an age limit is set).
- Copying something that is already in your recent history moves the
  existing entry to the top instead of adding a second one.

### Deleted entries

Deleting an entry in the popup hides it at once. Its data stays in the
database until you purge it:

- Settings, **Databases**, **Purge removed clips**, or
- `clipmonger-cli --profile-dir <profile> purge-removed`.

Entries removed by the history limits are deleted for good right away.

## Oversized items

A clipboard item larger than **Monitor maximum bytes** (default 16 MiB) is
not stored. Instead the history gets a stub entry, so the copy is not lost
silently. Its preview reads, for example:

```
Too large — not saved (16 MiB limit): image/png (25165824 B)
```

A stub cannot be pasted. To keep larger items, raise **Monitor maximum
bytes** on the History tab of Settings.

## Integrity checks

- Each time the background service starts, it runs SQLite's quick integrity
  check on the database.
- If the check fails, Clipmonger copies the damaged database (with its
  working files) to `clipstore.corrupt-<timestamp>.sqlite3` in the profile
  folder, logs `startup.store_open_failed` in `logs/daemon.log` and does not
  start the background service. The popup then reports that the connection
  is lost.
- Backups carry a SHA-256 checksum of the database and of every blob, and a
  restore checks them before anything is replaced.
- The database is opened in SQLite's write-ahead-log mode, so a crash or
  power loss does not leave half-written entries.

If the database is damaged, recover like this:
<!-- TODO verify: recovery procedure after a failed quick_check (daemon exits; the original file stays in place) has not been tested end to end. -->

1. Quit Clipmonger (tray, **Quit**).
2. Move `clipstore.sqlite3`, `clipstore.sqlite3-wal` and
   `clipstore.sqlite3-shm` out of the profile folder. Keep the
   `clipstore.corrupt-*` copy for a bug report.
3. Start Clipmonger. It creates a new, empty database.
4. If you have a backup, restore it as described below.

## Backup

A backup is one ZIP file. It contains the database (history and permanent
clips), the large payloads from `blobs/`, `config.toml` and a manifest with
checksums. It does not contain logs, the connection token or locks.

1. Open Settings and go to the **Databases** tab.
2. In **Backup file path**, keep the default (`backup.zip` in the profile
   folder) or type another path. Missing folders are created.
3. Click **Create backup**. Settings shows "Backup saved:" and the path.

The backup is taken from the running database, so you do not need to quit
Clipmonger. A backup in the profile folder is lost if you delete the
profile, so copy it somewhere else for safekeeping.

`clipmonger-cli` has no backup or restore command in 0.4.0; use Settings.

## Restore

A restore replaces your current history, permanent clips and settings with
the contents of the backup. It cannot be undone, so create a backup of the
current state first if you may want it back.

1. Open Settings and go to the **Databases** tab.
2. Type the path of the backup file in **Backup file path**.
3. Click **Restore backup**. The window is disabled while the restore runs.
4. Settings shows "Backup restored."

Before replacing anything, Clipmonger checks the archive: unsafe file
names, entries over 1 GiB or archives over 4 GiB in total, checksum
mismatches and a database that fails the integrity check are refused. A
refused backup leaves your current data unchanged.

The restored settings include **Start on login**, which Clipmonger applies
to the operating system again. If that fails, the restore still succeeds
and Settings shows one of:

- "Starting automatically could not be applied and may have changed."
- "Starting automatically is unavailable."
- "Starting automatically is unavailable in portable mode."
- "Starting automatically may not have been applied."

Check **Start on login** on the Startup tab afterwards.

## Moving history to a new computer

1. On the old computer, create a backup (see [Backup](#backup)) and copy the
   ZIP file to the new computer.
2. On the new computer, install Clipmonger and start it once (see
   [Windows](getting-started-windows.md), [macOS](getting-started-macos.md)
   or [Linux](getting-started-linux.md)).
3. Restore the backup (see [Restore](#restore)).
4. Check the settings that depend on the system: **Popup hotkey**, **Start
   on login** and the app names in **Privacy denylist** and **Monitor
   ignored apps**, which differ between operating systems.

<!-- TODO verify: copying the whole profile folder while Clipmonger is stopped likely also works, but it is untested and would carry over token/rendezvous files. -->

## Deleting all data

Quit Clipmonger first (tray, **Quit**), so nothing writes to the folder
while you delete it.

- **Windows (installer):** uninstall Clipmonger in Settings, Apps, and tick
  **Delete my default Clipmonger profile and everything inside it**. Or
  delete `%APPDATA%\clipmonger` by hand.
- **Windows (portable):** delete the folder you extracted.
- **macOS:** run

  ```sh
  /Applications/Clipmonger.app/Contents/MacOS/clipmonger-cli \
    --profile-dir ~/Library/Application\ Support/clipmonger \
    uninstall --delete-history
  ```

  This stops Clipmonger, removes its login agent and deletes the profile.
  It refuses to delete a folder that does not look like a Clipmonger
  profile. Then drag Clipmonger to the Trash.
- **Linux:** remove the program (`sudo apt remove clipmonger`,
  `sudo dnf remove clipmonger`, or
  `sh ~/.local/opt/clipmonger/install.sh --uninstall`), then delete
  `~/.local/share/clipmonger`. The per-user uninstall also removes the
  login entry. With the `.deb` or `.rpm`, turn off **Start on login** before
  removing the package, or delete
  `~/.config/autostart/com.martinambrus.clipmonger.desktop` afterwards.

To clear only some history and keep the program, delete entries in the
popup and use **Purge removed clips**. See also [Privacy](privacy.md).
