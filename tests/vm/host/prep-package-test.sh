#!/bin/sh
# Make ALIAS look like a machine that never had Clipmonger: stop it, remove the
# dev deployment (~/clipmonger-test/bin), the autostart and menu entries it
# wrote, the per-user GNOME extension and any per-user install, and move the
# profile aside. Then copy only the guest helpers back to ~/clipmonger-test.
set -eu
A=${1:?alias}
ssh "$A" 'sh -s' </dev/null <<'R' || true
pkill -x clipmonger; pkill -x clipmonger-daem; sleep 1
rm -rf ~/clipmonger-test ~/.local/opt/clipmonger ~/.local/bin/clipmonger ~/.local/bin/clipmonger-cli
rm -f ~/.config/autostart/com.martinambrus.clipmonger.desktop ~/.config/autostart/clipmonger.desktop
rm -f ~/.local/share/applications/com.martinambrus.clipmonger.desktop ~/.local/share/applications/com.martinambrus.clipmonger.daemon.desktop
rm -f ~/.local/share/icons/hicolor/*/apps/com.martinambrus.clipmonger.png
command -v gnome-extensions >/dev/null && gnome-extensions disable clipmonger@extensions.clipmonger.dev 2>/dev/null
rm -rf ~/.local/share/gnome-shell/extensions/clipmonger@extensions.clipmonger.dev
[ -d ~/.local/share/clipmonger ] && mv ~/.local/share/clipmonger ~/.local/share/clipmonger.pre-$(date +%s)
mkdir -p ~/clipmonger-test
R
scp -q "$(dirname "$0")"/../guest/linux/* "$A:clipmonger-test/"
echo "prepared $A"
