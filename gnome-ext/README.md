# Clipmonger GNOME extension

Capture/hotkey relay for the Clipmonger daemon. History lives in the daemon;
this extension only polls the clipboard, reports changes over the daemon's
Unix-socket IPC (`ipc/v1`), registers the popup keybinding, and provides a
clipboard-write + toast paste-assist fallback.

## Usage

```sh
npm install   # dev only; exact pins in package.json
npm run build # tsc -> dist/ (committed) + metadata/schemas copied in
npm test      # T-701: node --test on compiled pure modules (no GNOME needed)
./install-extension.sh            # copies dist/ to ~/.local/share/gnome-shell/extensions/, compiles schemas
./install-extension.sh --uninstall
```

After installing on Wayland, log out and back in, then enable
`clipmonger@extensions.clipmonger.dev` in GNOME Extensions.

Preferences expose the daemon profile directory (blank = default
`data_dir()/clipmonger`, G-F1) and a denylist of app ids that are never
reported. Integration on a real GNOME box (T-702+) is not covered by the
headless unit tests; Shell APIs marked `VERIFY` in `src/` need on-box proof.
