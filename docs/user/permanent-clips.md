# Permanent clips

Permanent clips are snippets you keep and paste often: an address, a
signature, a standard reply. Unlike history clips they are never removed by
the history limits, and they do not change when you copy something new.

Each permanent clip has:

- a **Name**, shown in the popup,
- a **Group**, shown as a tab in the popup,
- optionally a **Hotkey** that pastes it without opening the popup,
- the **Text to paste**.

Permanent clips paste plain text.

## Managing permanent clips

Permanent clips are managed in Settings, **Permanent Clips** tab. Open
Settings from the tray menu (**Open settings**) or with the **Open settings
window** button in the popup.

Changes on this tab are saved at once. You do not need **Apply**.

### Create a group

1. Type the group name in the text field at the top of the tab.
2. Click **New group**.

You can also create a group by typing a new group name when you save a clip.

### Create a clip

1. Click the group the clip belongs in.
2. Click **New permanent clip**.
3. Fill in **Name**, **Group** (prefilled with the selected group), and
   **Text to paste**. **Hotkey** is optional.
4. Click **Save clip**. Click **Cancel** to discard the draft.

If a field is missing, Settings shows **Give the clip a name**, **Choose or
name a group** or **Enter the text to paste**.

### Edit a clip

1. Click the group, then click the clip in the list. Clips with a hotkey show
   it after the name.
2. Change the fields.
3. Click **Save clip**.

To move a clip to another group, change its **Group** field.

### Delete a clip

1. Select the clip.
2. Click **Delete permanent clip**. Settings asks **Delete this permanent
   clip? This cannot be undone.**
3. Click the second **Delete permanent clip** button to confirm.

Deleting a clip also releases its hotkey.

### Delete a group

A group can only be deleted when it is empty. Delete or move its clips first;
until then Settings shows **Group still has clips** and **Delete group** is
disabled.

1. Select the empty group.
2. Click **Delete group**.

## Using permanent clips in the popup

Each group appears as a tab next to **History**, sorted by name. Tabs are
shown only when at least one group exists.

1. Open the popup.
2. Click a group tab, or press **Alt+1** to **Alt+9** for the first nine
   groups in tab order (Option+1 to Option+9 on macOS).
3. Select a clip and press **Enter**, or click it twice.

An empty group shows **Nothing here yet**.

Searching in the popup also finds permanent clips, by name or by text, in
every group. They appear before the history results as **[Group] Name**.
Pressing Enter on one pastes it. See [The popup](popup.md).

A permanent clip is pasted the same way as a history clip, using the paste
mode set in Settings. See [Paste modes](paste-modes.md).

## Per-clip hotkeys

Give a clip a hotkey to paste it straight into the window in front, without
opening the popup.

Type the hotkey in the **Hotkey** field as modifier names and a key joined
with `+`, for example `ctrl+alt+1` or `ctrl+shift+f2`.

| Modifier | Accepted names |
|---|---|
| Ctrl (Control on macOS) | `ctrl`, `control` |
| Alt (Option on macOS) | `alt` |
| Shift | `shift` |
| Windows key / Super / Command | `super`, `win`, `meta`, `cmd`, `command` |

The key can be a letter, a digit, `f1` to `f24`, or a named key such as
`enter`, `space`, `tab`, `home`, `end`, `pageup`, `pagedown`, `up`, `down`,
`left`, `right`, `delete` or `backspace`.

Each hotkey can belong to one permanent clip only. If it is already used,
Settings shows **That hotkey is already used by another permanent clip**.

A hotkey works as soon as the clip is saved:

- **Windows, macOS and X11 desktops:** Clipmonger registers the hotkey
  itself.
- **GNOME (Wayland):** the Clipmonger GNOME Shell extension registers it.
- **KDE Plasma and other Wayland desktops with a shortcut service:** the
  desktop may ask you once to allow the new shortcut.
- **Sway, Hyprland and COSMIC:** see the next section.

<!-- TODO verify: behaviour when a per-clip hotkey is already held by another application (no UI message found for permanent hotkeys; the popup hotkey has one). -->

### Sway, Hyprland and COSMIC

These desktops do not let applications register global shortcuts. Settings
shows **This desktop does not let apps set global shortcuts...** on the
Permanent Clips tab, and each clip gets a **Copy shortcut command** button.

1. In Settings, **Permanent Clips** tab, select the clip.
2. Click **Copy shortcut command**. This copies a command of the form
   `clipmonger-cli --profile-dir <profile> exec-permanent <clip id>`.
3. Add a key binding that runs this command in your compositor config.

For Sway:

```
bindsym Ctrl+Alt+k exec clipmonger-cli --profile-dir ~/.local/share/clipmonger exec-permanent <clip id>
```

For Hyprland (`hyprland.conf`):

```
bind = CTRL ALT, K, exec, clipmonger-cli --profile-dir ~/.local/share/clipmonger exec-permanent <clip id>
```

On COSMIC, add a custom shortcut in Settings, Keyboard, that runs the copied
command.

On these desktops Clipmonger cannot register the hotkey typed in the
**Hotkey** field; the compositor binding is what triggers the clip.

## Command line

`clipmonger-cli` can manage and paste permanent clips. Every command needs
`--profile-dir` (see [Command line](cli.md)).

| Command | What it does |
|---|---|
| `permanent-list` | Lists all permanent clips as JSON, including their ids. |
| `permanent-create <group> <name> <text> [--hotkey <chord>]` | Creates a clip. The group is created if it does not exist. |
| `permanent-delete <id>` | Deletes a clip and releases its hotkey. |
| `exec-permanent <id>` | Pastes the clip into the window in front. |

Example:

```sh
clipmonger-cli --profile-dir ~/.local/share/clipmonger permanent-create Work Signature "Kind regards, Ana" --hotkey ctrl+alt+s
```

`exec-permanent` is what the compositor bindings above run. You can also call
it from scripts or other launchers.

On Windows, `exec-permanent` cannot paste into a program that runs as
administrator; use the popup hotkey for those (see
[Paste modes](paste-modes.md)).
