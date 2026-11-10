# Getting started on Linux

This page takes you from download to your first paste on Linux. Clipmonger
works under X11 and Wayland. What you have to set up depends on your
desktop, so the sections below are split by desktop.

## Requirements

- A 64-bit x86_64 PC. There are no ARM builds yet.
- glibc 2.35 or newer. Ubuntu 22.04, Debian 12, current Fedora, Arch, Linux
  Mint 21 and Pop!_OS 22.04 and newer all have it. To check, run
  `ldd --version`; the first line ends with the version.
- One of these desktops:

| Desktop | Session | Notes |
| --- | --- | --- |
| GNOME 46 to 51 | Wayland or Xorg | Needs the bundled GNOME Shell extension on Wayland |
| KDE Plasma 6 and 5.27 | Wayland or X11 | Approve the shortcut once on Wayland |
| Xfce, Cinnamon and other X11 desktops | X11 | Works out of the box |
| Sway, Hyprland | Wayland | Bind the hotkey and the login start yourself |
| COSMIC | Wayland | Bind the hotkey yourself |

To see whether you run X11 or Wayland, run `echo $XDG_SESSION_TYPE`. It
prints `x11` or `wayland`.

## Install

Download the files from the
[latest release](https://github.com/martinambrus/clipmonger/releases/latest).
Pick one of the three ways below. All of them keep your history in
`~/.local/share/clipmonger`.

Every release has a `SHA256SUMS` file. Put it next to your download and
check the file first:

```sh
sha256sum -c SHA256SUMS --ignore-missing
```

The packages are not signed yet, so your package manager may warn about a
missing signature. There is no package repository yet either.

### Ubuntu, Debian, Mint, Pop!_OS: `.deb`

```sh
sudo apt install ./clipmonger_<version>-1_amd64.deb
```

### Fedora, openSUSE and other RPM distros: `.rpm`

```sh
sudo dnf install ./clipmonger-<version>-1.x86_64.rpm
```

On openSUSE use `sudo zypper install ./clipmonger-<version>-1.x86_64.rpm`.

The packages put the programs in `/opt/clipmonger`, the commands
`clipmonger` and `clipmonger-cli` in `/usr/bin`, a menu entry, and the GNOME
Shell extension for all users.

### Any distro, no root: per-user install

```sh
curl -fsSL https://github.com/martinambrus/clipmonger/releases/latest/download/install.sh | sh
```

Without curl (Debian ships without it):

```sh
wget -qO- https://github.com/martinambrus/clipmonger/releases/latest/download/install.sh | sh
```

The script downloads the release tarball, checks it against `SHA256SUMS` and
installs:

- the programs to `~/.local/opt/clipmonger`,
- the commands `clipmonger` and `clipmonger-cli` to `~/.local/bin`,
- the menu entry and icons to `~/.local/share`,
- on GNOME, the Shell extension to `~/.local/share/gnome-shell/extensions`.

Run it as your normal user, not with `sudo`. If `~/.local/bin` is not on
your `PATH`, the script says so; the menu entry works either way.

You can also download `clipmonger-<version>-linux-x86_64.tar.gz`, extract
it and run `sh install.sh` inside the extracted folder. To install a
specific release, set the version:
`curl -fsSL .../install.sh | CLIPMONGER_VERSION=<version> sh`.

## First start

1. Start **Clipmonger** from the applications menu, or run `clipmonger`.
2. Clipmonger starts its background service and the popup opens. Press
   **Esc** to close it.
3. Finish the setup for your desktop below. On GNOME Wayland, Clipmonger
   records nothing until the extension is enabled.

### The tray icon

Where your panel shows tray icons (KDE Plasma, Xfce, Cinnamon, a Waybar
tray, Ubuntu's GNOME), Clipmonger adds one. Click it to open the popup.
Right-click it for the menu:

- **Show popup**
- **Pause monitoring** (shows **Resume monitoring** while paused)
- **Pause for one hour**
- **Open settings**
- **About**
- **Quit**

**Quit** stops Clipmonger completely, including its background service.

Without a tray, open Settings with the **Open settings window** button at the
bottom of the popup.

## Desktop setup

### GNOME

GNOME on Wayland does not let normal programs read the clipboard, register
global shortcuts or press keys. The Clipmonger GNOME Shell extension does
these three things for it. On GNOME on Xorg the extension is optional.

The `.deb`, the `.rpm` and the per-user installer all install the extension.
You still have to turn it on, once per user:

1. Log out and log back in. GNOME on Wayland only sees new extensions after
   a new login.
2. Run:

   ```sh
   gnome-extensions enable clipmonger@extensions.clipmonger.dev
   ```

   Or turn **Clipmonger** on in the **Extensions** app.
3. Press **Ctrl+Alt+H**.

If the extension is off, Clipmonger does not record anything on GNOME
Wayland. The **Capabilities** text in Settings, **About and Help**, then
contains `capture paused: the GNOME extension is off`.

**Tray on GNOME.** Stock GNOME has no tray. Ubuntu enables its own
AppIndicator extension by default, so the icon appears in the top bar there.
On Fedora, Debian, Arch and other GNOME installs, install and enable the
**AppIndicator and KStatusNotifierItem Support** extension if you want the
icon. Without it, everything else works: the hotkey opens the popup, and the
popup has an **Open settings window** button.

### KDE Plasma

On Plasma on Wayland, Clipmonger registers its hotkey through the desktop's
shortcut service. The first time, Plasma shows a dialog that asks you to
confirm the shortcut. Accept it. Plasma may ask again when you change the
popup hotkey or give a permanent clip its own hotkey; Settings says **Your
desktop asks you to confirm a new shortcut** when that happens.

On Plasma on X11 there is no dialog.

Paste works on both. On Wayland Clipmonger presses Ctrl+V through KWin. This
needs the hidden `com.martinambrus.clipmonger.daemon.desktop` entry that the
packages and the per-user installer add; do not delete it.

### Xfce, Cinnamon and other X11 desktops

Nothing to set up. The hotkey, paste, tray and **Start on login** work as
soon as Clipmonger runs. On Xfce the tray icon appears in the panel's status
tray area.

### Sway and Hyprland

These compositors do not let programs register global shortcuts and do not
run the desktop's login items. Bind the keys and the login start in the
compositor's config yourself.

Settings shows the exact commands for your install:

- **Popup** tab, under **Popup hotkey**: the command that opens the popup,
  with a **Copy** button.
- **Startup** tab, under **Start on login**: the command that starts
  Clipmonger at login.
- **Permanent Clips** tab: select a clip and use **Copy shortcut command**
  for the command that pastes it.

For Sway with the default profile, add to `~/.config/sway/config`:

```
bindsym Ctrl+Alt+h exec clipmonger-cli --profile-dir ~/.local/share/clipmonger show-popup
exec clipmonger --minimized --profile-dir ~/.local/share/clipmonger
```

For Hyprland, in `hyprland.conf`:

```
bind = CTRL ALT, H, exec, clipmonger-cli --profile-dir ~/.local/share/clipmonger show-popup
exec-once = clipmonger --minimized --profile-dir ~/.local/share/clipmonger
```

With a Lua config, bind the same command with
`hl.bind("CTRL + ALT + H", hl.dsp.exec_cmd("..."))` and put the login start
inside `hl.on("hyprland.start", ...)`.

Reload the config (Sway: `swaymsg reload`; Hyprland reloads on save). Paste
works without any setup: Clipmonger presses Ctrl+V (Ctrl+Shift+V in
terminals) through the compositor's virtual keyboard.

If your bar starts after Clipmonger, the tray icon may not appear. Start the
bar first in your config.

### COSMIC

COSMIC does not let programs register global shortcuts.

1. Copy the popup command from Settings, **Popup** tab, under **Popup
   hotkey**.
2. Open COSMIC Settings, **Keyboard**, and add a custom shortcut that runs
   it, for example on **Ctrl+Alt+H**.

COSMIC runs the regular **Start on login** entry, so nothing else is needed.
Permanent clip hotkeys need a custom shortcut each (Settings, **Permanent
Clips**, **Copy shortcut command**). Paste works without any setup.

## Check that it works

1. Open a text editor, type a few words, select them and press **Ctrl+C**.
2. Copy something else, for example a line from a web page.
3. Click into the editor where you want to paste.
4. Press **Ctrl+Alt+H**. The popup opens with your copies, newest first.
5. Use the arrow keys to pick the first thing you copied and press
   **Enter**.

The popup closes and the text is pasted. In terminals Clipmonger presses
Ctrl+Shift+V instead of Ctrl+V.

If Clipmonger cannot press the keys on your desktop, it leaves the clip on
the clipboard and shows **Copied to clipboard. Press Ctrl+V to paste.** Press
Ctrl+V yourself.

To check from a terminal that the background service runs:

```sh
clipmonger-cli --profile-dir ~/.local/share/clipmonger status
```

To learn the rest of the popup (search, tabs, keys) read
[The popup](popup.md). To change the hotkey, open Settings, **Popup** tab,
**Popup hotkey**.

## Start on login

Start on login is off by default. To turn it on:

1. Open Settings (tray icon, **Open settings**, or the **Open settings
   window** button in the popup).
2. Go to the **Startup** tab.
3. Tick **Start on login** and click **Apply**.

Clipmonger adds `~/.config/autostart/com.martinambrus.clipmonger.desktop`
and starts in the background at the next login, without opening the popup.
This works on GNOME, KDE Plasma, Xfce, Cinnamon, COSMIC and other desktops
that run XDG autostart entries. On Sway and Hyprland use the `exec` line
from [Sway and Hyprland](#sway-and-hyprland) instead.

## Updating

There is no automatic update check yet, and no package repository. Download
new releases by hand.

1. Quit Clipmonger (tray icon, **Quit**).
2. Install the new version the same way you installed the old one:
   - `.deb`: `sudo apt install ./clipmonger_<version>-1_amd64.deb`
   - `.rpm`: `sudo dnf install ./clipmonger-<version>-1.x86_64.rpm`
   - per-user: run the `install.sh` command again. It stops a running
     Clipmonger first.
3. On GNOME, log out and back in once, so GNOME loads the new extension.

Your history and settings are kept.

## Uninstalling

1. If you turned on **Start on login**, turn it off in Settings, **Startup**
   first. The package managers do not remove the entry in your home folder.
2. Quit Clipmonger (tray icon, **Quit**).
3. Remove it:
   - `.deb`: `sudo apt remove clipmonger`
   - `.rpm`: `sudo dnf remove clipmonger`
   - per-user: `sh ~/.local/opt/clipmonger/install.sh --uninstall`. This
     also disables the GNOME extension and removes the start-on-login entry.
4. Your history stays in `~/.local/share/clipmonger`. Delete that folder to
   remove it too. To keep a copy first, see
   [Data, backup and restore](backup-and-data.md).

## Linux gotchas

- **X11 and Wayland behave differently.** On X11 Clipmonger reads the
  clipboard, grabs the hotkey and presses Ctrl+V itself on every desktop. On
  Wayland each of these depends on the desktop: the GNOME extension, KDE's
  shortcut service and KWin, or the compositor's virtual keyboard and your
  own key bindings. If something works on X11 but not on Wayland, check your
  desktop's section above.
- **GNOME needs a new login.** After installing or updating, the extension
  only loads after you log out and back in.
- **glibc too old.** On distros older than Ubuntu 22.04 or Debian 12 the
  programs do not start. There is no build for older systems.
- **Hotkey already taken.** If another program owns **Ctrl+Alt+H**, the
  popup shows **Popup shortcut ... is already in use. Reassign it in
  Settings.** with the key combination in place of the dots. Pick another key
  in Settings, **Popup**, **Popup hotkey**.
- **Scroll Lock option.** Settings shows **Toggle monitoring with Scroll
  Lock** on Linux too, but the key toggle only works on Windows. Pause from
  the tray menu or with `clipmonger-cli --profile-dir ~/.local/share/clipmonger toggle-monitor`.

## Next steps

- [The popup](popup.md)
- [Permanent clips](permanent-clips.md)
- [Paste modes](paste-modes.md)
- [Privacy](privacy.md)
- [Settings](settings.md)
- [Data, backup and restore](backup-and-data.md)
- [Command line](cli.md)
- [Troubleshooting](troubleshooting.md)
