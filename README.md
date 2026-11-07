# Clipmonger

Clipmonger is a cross-platform clipboard history manager written in Rust. It
remembers everything you copy (text, images and file lists), opens a popup on
a global hotkey and pastes the clip you pick straight into the window you came
from. It runs on Windows 10/11 and on Linux under X11 and Wayland, including
GNOME Wayland through a bundled GNOME Shell extension.

Clipmonger is MIT-licensed. It is a modern, open successor to the classic
Windows clipboard manager ArsClip, rebuilt from scratch (no ArsClip code is
used).

> **Status: 0.1.0 pre-release.** Core capture, popup, paste and Settings are
> verified by hand on Windows 10 and Windows 11. Linux packages are available,
> but full acceptance across Linux desktops is still in progress. See
> [Known limitations](#known-limitations).

## Features

### Clipboard history

- Captures text, images and file copy/cut lists continuously in the
  background, with a configurable history depth.
- Shows image thumbnails and file-list previews; large items are handled
  safely (oversized captures become visible stubs instead of failing).
- Stores history in a local SQLite database with integrity checks, plus
  backup and restore.
- Coexists with other clipboard managers and clipboard sync tools: it listens
  passively and does not take over the clipboard.
- Pause and resume monitoring from the tray, the CLI or (on Windows,
  optional) the Scroll Lock key.

### Popup

- Opens on a global hotkey (default **Ctrl+Alt+H**), from the tray icon, or
  with `clipmonger-cli show-popup`.
- Fully keyboard-driven: type to search, arrow keys to move, Enter to paste.
- Multi-line previews with a tooltip for the full text; the popup sizes itself
  to its content and the monitor.
- Pastes into the window that was active when you pressed the hotkey.

### Permanent clips

- Save snippets you use often as permanent clips, organised in named groups
  that appear as tabs in the popup.
- Give any permanent clip its own global hotkey to paste it without opening
  the popup.

### Paste delivery

- **Clipboard + Ctrl+V** (default): works for any size and format. Optionally
  restores your previous clipboard afterwards.
- **Unicode injection**: types the text as synthetic keystrokes, for targets
  that ignore Ctrl+V.
- **Windows administrator windows**: pasting into programs that run as
  administrator works through a small elevated helper, started after one
  consent and User Account Control prompt.

### Privacy

- Honours operating-system privacy signals (for example password managers
  marking a clip as sensitive), where the OS exposes them.
- Denylist of applications whose clips are never stored.
- Strict privacy mode with reduced retention.

### Settings and tooling

- A Settings window with every option on tabs (General, Popup, Privacy,
  Permanent Clips, Advanced), a search filter across all tabs and inline
  validation.
- Tray icon on Windows and Linux.
- Optional start on login.
- Portable mode on Windows: the profile is stored next to the program.
- `clipmonger-cli` for scripting: status, list and get clips, paste a clip,
  show the popup, toggle monitoring, manage permanent clips, purge removed
  clips and stop the daemon. Run `clipmonger-cli --help` for details.

### Supported platforms

| Platform | Status |
|---|---|
| Windows 10 / 11 (64-bit) | Supported, installer available |
| Linux X11 | Supported, `.deb` package available |
| Linux Wayland (wlroots and other compositors with data-control) | Supported, `.deb` package available |
| GNOME Wayland (GNOME 46 to 51) | Supported via the bundled GNOME Shell extension |
| macOS | Planned |

## Installation

Download the files from the
[latest release](https://github.com/martinambrus/clipmonger/releases/latest).

### Windows

1. Download `clipmonger-0.1.0-windows-x86_64-setup.exe`.
2. Run it. The binaries are not code-signed yet, so Windows SmartScreen shows
   **Windows protected your PC**: click **More info**, then **Run anyway**.
3. Approve the User Account Control prompt and finish the installer.
4. Start Clipmonger from the Start menu and press **Ctrl+Alt+H**.

The full walk-through, including browser download warnings, checksum
verification and Windows 11 Smart App Control, is in
[Installing on Windows](docs/install-windows.md).

### Linux (Ubuntu / Debian)

1. Download `clipmonger_0.1.0_amd64.deb`.
2. Install it:

   ```sh
   sudo apt install ./clipmonger_0.1.0_amd64.deb
   ```

3. Start **Clipmonger** from the applications menu, or run `clipmonger`. The
   UI starts its background daemon automatically. History is stored in
   `~/.local/share/clipmonger`.
4. Press **Ctrl+Alt+H** to open the popup.

**GNOME on Wayland** needs the bundled GNOME Shell extension for capture, the
hotkey and paste. Enable it once, then log out and back in:

```sh
gnome-extensions enable clipmonger@extensions.clipmonger.dev
```

On Xorg the extension is optional.

**Sway, Hyprland and COSMIC** don't let apps register global shortcuts or run
XDG autostart entries, so bind the keys and the login start in the
compositor's own config. Settings shows the exact commands for your install
(Popup and Startup tabs; Permanent Clips has **Copy shortcut command** for each
clip). For Sway, with the default profile:

```
bindsym Ctrl+Alt+h exec clipmonger-cli --profile-dir ~/.local/share/clipmonger show-popup
bindsym Ctrl+Alt+k exec clipmonger-cli --profile-dir ~/.local/share/clipmonger exec-permanent <clip id>
exec clipmonger --minimized --profile-dir ~/.local/share/clipmonger
```

On Hyprland bind the same commands (`hyprland.conf`: `bind = CTRL ALT, H, exec,
...` and `exec-once = ...`; Lua config: `hl.bind("CTRL + ALT + H",
hl.dsp.exec_cmd("..."))`, with the login start inside
`hl.on("hyprland.start", ...)`). On COSMIC add a custom shortcut in Settings,
Keyboard; COSMIC runs the regular Start on login entry.

On Sway, Hyprland and COSMIC a picked clip is pasted for you: Clipmonger
presses Ctrl+V (Ctrl+Shift+V in terminals) through the compositor's virtual
keyboard.

### Build from source

Clipmonger builds with the Rust toolchain pinned in `rust-toolchain.toml`
(installed automatically by `rustup`).

```sh
git clone https://github.com/martinambrus/clipmonger.git
cd clipmonger
cargo build --release
```

The binaries `clipmonger` (UI), `clipmonger-daemon` and `clipmonger-cli` are
placed in `target/release/`. The GNOME extension is built separately, see
[gnome-ext/README.md](gnome-ext/README.md).

## Usage

1. Copy things as usual; Clipmonger records them in the background.
2. Press **Ctrl+Alt+H** in any application.
3. Type to filter, pick a clip with the arrow keys or the mouse, press
   **Enter**. The clip is pasted into the application you were in.
4. Right-click the tray icon for Settings, pausing monitoring or exiting.

The popup hotkey and every other option can be changed in Settings.

## Known limitations

- The Windows binaries are unsigned, so SmartScreen warns on first run, and
  Windows 11 Smart App Control blocks them while it is turned on.
- On Windows, Alt+Tab from the open popup switches to the most recently used
  window rather than the one before it.
- A paste into an administrator window needs the popup to be opened with the
  hotkey; from a tray- or CLI-opened popup it is refused with a visible
  reason.
- No automatic update check yet.

Release notes: [CHANGELOG.md](CHANGELOG.md) and
[docs/release-notes-0.1.0.md](docs/release-notes-0.1.0.md). The full design
and specification live in [docs/](docs/README.md).

## Architecture

Clipmonger is split into a background **daemon** that owns capture, storage
and the hotkeys, a **UI** (popup, Settings and tray, rendered with egui) and a
**CLI**. They talk over a local, token-protected IPC channel. Each operating
system has its own backend crate (Windows, X11, Wayland) behind a shared core,
and the GNOME extension relays capture and hotkeys to the daemon where GNOME
Wayland does not allow a regular application to do so.

## Inspirations

Clipmonger stands on the shoulders of these projects. They served as behaviour
or design references; their code is not reused.

- [ArsClip](http://www.joejoesoft.com/vcms/97) by JoeJoe Soft: the feature set
  and popup workflow Clipmonger aims to match. Clipmonger is not affiliated
  with or endorsed by ArsClip or JoeJoe Soft.
- [CopyQ](https://github.com/hluk/CopyQ): the shared-core plus per-OS-backend
  architecture and platform clipboard quirks.
- [cliphist](https://github.com/sentriz/cliphist) and
  [wl-clipboard](https://github.com/bugaevc/wl-clipboard): Wayland history and
  watch patterns.
- [Pano](https://github.com/oae/gnome-shell-pano): lessons for the GNOME Shell
  extension approach.
- [Notepad++](https://notepad-plus-plus.org/): the plugin user experience
  planned for a later version.

## Contributors

- [Martin Ambrus](https://github.com/martinambrus): author and maintainer.

### Third-party work

Not everything in Clipmonger was written for it. It builds on the work of
these authors and projects:

- **Ported design:** the clipboard watch semantics, sensitive-content flag and
  automatic backend selection follow
  [golang-design/clipboard](https://github.com/golang-design/clipboard) (MIT),
  re-implemented in Rust.
- **UI:** [egui and egui-winit](https://github.com/emilk/egui),
  [winit](https://github.com/rust-windowing/winit),
  [softbuffer](https://crates.io/crates/softbuffer),
  [hdrshot-egui-backend](https://crates.io/crates/hdrshot-egui-backend),
  [egui_system_font_provider](https://crates.io/crates/egui_system_font_provider)
  and [image](https://github.com/image-rs/image).
- **Tray and hotkeys:** [tray-icon](https://github.com/tauri-apps/tray-icon)
  and [global-hotkey](https://github.com/tauri-apps/global-hotkey) by the
  Tauri project, [ksni](https://crates.io/crates/ksni) and
  [ashpd](https://crates.io/crates/ashpd) (XDG desktop portal).
- **Clipboard and input:** [clipboard-win](https://crates.io/crates/clipboard-win),
  [windows-sys](https://github.com/microsoft/windows-rs),
  [x11rb](https://github.com/psychon/x11rb),
  [wl-clipboard-rs](https://github.com/YaLTeR/wl-clipboard-rs),
  [wayland-client](https://crates.io/crates/wayland-client) and
  [enigo](https://github.com/enigo-rs/enigo).
- **Core:** [rusqlite](https://github.com/rusqlite/rusqlite) with bundled
  [SQLite](https://sqlite.org/), [tokio](https://github.com/tokio-rs/tokio),
  [serde](https://serde.rs/), [clap](https://github.com/clap-rs/clap),
  [toml](https://crates.io/crates/toml), [uuid](https://crates.io/crates/uuid),
  [sha2](https://crates.io/crates/sha2), [zip](https://crates.io/crates/zip),
  [directories](https://crates.io/crates/directories) and
  [thiserror](https://crates.io/crates/thiserror).
- **Fonts** (bundled through egui): Ubuntu Light (Canonical Ltd.), Noto Emoji
  (Google Inc.), Hack (Source Foundry Authors, with DejaVu and Bitstream Vera
  contributions) and emoji-icon-font (John Slegers).
- **Wayland blur protocol** (Linux only): bindings generated from KDE's
  `blur.xml` (Martin Gräßlin, Marco Martin) are under LGPL-2.1-or-later; see
  [licenses/wayland/](licenses/wayland/README.md).

Full license texts and notices for all third-party code and fonts are in
[licenses/](licenses/).

Bug reports and pull requests are welcome on
[GitHub](https://github.com/martinambrus/clipmonger/issues).

## Sponsors

Clipmonger was built as a hackathon project with the generous support of:

- **[Meta](https://www.meta.com/)**: $150 in credits for Meta models.
- **[Baseten](https://www.baseten.co/)**: $100 in credits.
- **[Modal](https://modal.com/)**: $100 in credits.

Thank you!

## License

[MIT](LICENSE)
