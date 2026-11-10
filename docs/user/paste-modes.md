# Paste modes

When you pick a clip in the popup, or trigger a permanent clip, Clipmonger
delivers it to the target window. The **Popup paste delivery** setting
(Settings, Popup tab) chooses how.

| Value | Name in this guide | Platforms |
|---|---|---|
| `clipboard_restore` (default) | Clipboard + Ctrl+V | Windows, macOS, Linux |
| `unicode_inject` | Unicode injection | Windows, macOS |

## Clipboard + Ctrl+V

This is the default and works for every kind of clip.

1. Clipmonger writes the clip to the clipboard, with all its stored formats
   (text, formatted text, image or file list).
2. It brings the target window back to the front.
3. It presses the paste key for you: Ctrl+V on Windows and Linux, Cmd+V on
   macOS, and Ctrl+Shift+V in Linux terminals.

Afterwards the clip stays on the clipboard, so you can paste it again with
Ctrl+V yourself.

Clipmonger does not record its own clipboard write as a new history entry.

### Restoring the previous clipboard

Turn on **Restore clipboard after paste** (Settings, Popup tab; off by
default) if you want the clipboard to go back to what it held before the
paste.

- Clipmonger waits for **Popup delivery dwell** (Settings, Advanced tab;
  default 1000 ms) so the target application has time to read the clip.
- It then puts back the previous clipboard, but only if the clipboard still
  holds the pasted clip. If you or another program copied something in the
  meantime, that newer content is kept.

If pastes arrive incomplete in a slow application, raise **Popup delivery
dwell**.

Restoring applies on Windows, macOS and X11 desktops.
<!-- TODO verify: whether restore_clipboard has any effect on Wayland (GNOME extension, KDE, wlroots paths); no restore found in the Wayland paste path. -->

## Unicode injection

With `unicode_inject`, Clipmonger types the clip's text into the target as
synthetic keystrokes. The clipboard is not touched.

- Only text is typed. Formatting is not carried over.
- Text longer than **Popup unicode limit** (Settings, Advanced tab; default
  65536) is pasted through the clipboard instead.
- Images, file lists and other clips without plain text always go through the
  clipboard.
- On macOS, a clip that has formats other than plain text (for example text
  copied from a web page, which also carries HTML) goes through the clipboard.

Unicode injection is available on Windows and macOS. On Linux, Clipmonger
always uses Clipboard + Ctrl+V.

## Which one to use

Use **Clipboard + Ctrl+V** unless you have a reason not to. It is fast for
any size, keeps formatting, and pastes images and files.

Use **Unicode injection** when:

- the target ignores Ctrl+V or uses a different paste key, for example some
  remote desktop clients, virtual machine consoles or games;
- you do not want pasting to touch your clipboard at all.

Typing long text keystroke by keystroke is slower, and some applications act
on the typed keys (auto-indent, autocomplete), which can change the result.

## Linux

### Terminals

Terminals paste with Ctrl+Shift+V; Ctrl+V does nothing there. Clipmonger
recognises these terminals and presses Ctrl+Shift+V for them:

GNOME Terminal, GNOME Console (`kgx`), Ptyxis, Konsole, Xfce Terminal,
Tilix, Terminator, Alacritty, kitty, foot, WezTerm, Guake, Tilda,
Terminology, st and COSMIC Terminal.

In any other terminal, the clip is still put on the clipboard; paste it with
the terminal's own paste key.

### How the key press is sent

| Desktop | How Clipmonger presses the paste key |
|---|---|
| X11 desktops (Xfce, Cinnamon, KDE on X11, GNOME on Xorg, ...) | Directly, through the X server |
| GNOME on Wayland | Through the Clipmonger GNOME Shell extension |
| KDE Plasma on Wayland | Through KWin |
| Sway, Hyprland, COSMIC | Through the compositor's virtual keyboard |

If Clipmonger cannot press the key on your desktop, it puts the clip on the
clipboard and shows a notification: **Copied to clipboard. Press Ctrl+V to
paste.**

## Windows: administrator windows

Windows does not let a normal program send keys to a program that runs as
administrator. Clipmonger uses a small elevated helper for those windows.

1. The first time you paste into an administrator window, Clipmonger asks
   **Enable Clipmonger in admin windows?**
2. Click **Yes** and approve the User Account Control prompt.

What happens next depends on how Clipmonger is installed:

- **Installer:** the helper is registered once and then starts silently at
  each logon. You are not asked again.
- **Portable ZIP:** nothing is installed. The helper runs until Clipmonger
  exits, so you are asked again each time Clipmonger starts.

If you click **No** or cancel the prompt, you are asked again the next time
Clipmonger starts.

If Clipmonger itself runs as administrator (for example under the built-in
Administrator account), it pastes into administrator windows directly. No
helper is started and nothing is asked.

**Limitation:** a paste into an administrator window only works from a popup
opened with the popup hotkey. From a popup opened with the tray icon or
`clipmonger-cli show-popup`, and from `clipmonger-cli exec-permanent`, the
paste is refused and the popup shows why. The hotkey press is what proves you
asked for the paste. The paste must also happen within about two seconds of
picking the clip.

More detail is in [Getting started on Windows](getting-started-windows.md).

## macOS: Accessibility

To press Cmd+V or type text for you, Clipmonger needs **Accessibility**
access.

1. Allow it when macOS asks, or open System Settings, Privacy & Security,
   Accessibility (on macOS 12 and older: System Preferences, Security &
   Privacy, Privacy, Accessibility).
2. Turn **Clipmonger** on.

Without Accessibility access, Clipmonger still copies the picked clip to the
clipboard and tells you to press Cmd+V yourself.

The same happens when Secure Input is on, for example while a password field
has focus or a terminal has Secure Keyboard Entry turned on: macOS blocks
synthetic key presses, so Clipmonger leaves the clip on the clipboard and asks
you to press Cmd+V.

The app is signed ad hoc, so macOS treats each new version as a new app. After
updating, turn Clipmonger off and on again in Accessibility. See
[Getting started on macOS](getting-started-macos.md) and
[Troubleshooting](troubleshooting.md).
