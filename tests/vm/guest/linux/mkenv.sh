#!/bin/sh
# Write ~/clipmonger-test/env.sh from the running graphical session's environment.
# Usage: mkenv.sh PROCESS_NAME  (gnome-shell, sway, Hyprland, xfce4-session, cinnamon, cosmic-comp, ...)
pid=$(pgrep -u "$(id -u)" -x "$1" | head -1)
[ -n "$pid" ] || { echo "no $1 running" >&2; exit 1; }
mkdir -p ~/clipmonger-test
tr '\0' '\n' < /proc/$pid/environ | grep -E '^(XDG_RUNTIME_DIR|DBUS_SESSION_BUS_ADDRESS|WAYLAND_DISPLAY|DISPLAY|XAUTHORITY|XDG_CURRENT_DESKTOP|XDG_SESSION_TYPE|XDG_SESSION_DESKTOP|DESKTOP_SESSION|SWAYSOCK|HYPRLAND_INSTANCE_SIGNATURE|GDMSESSION)=' | sed 's/^/export /; s/=\(.*\)$/="\1"/' > ~/clipmonger-test/env.sh
# Some sessions export no XAUTHORITY to the shell; find Xwayland's.
grep -q XAUTHORITY ~/clipmonger-test/env.sh || { x=$(ls -t /run/user/$(id -u)/.mutter-Xwaylandauth.* /run/user/$(id -u)/xauth_* 2>/dev/null | head -1); [ -n "$x" ] && echo "export XAUTHORITY=\"$x\"" >> ~/clipmonger-test/env.sh; }
grep -q WAYLAND_DISPLAY ~/clipmonger-test/env.sh || { w=$(cd /run/user/$(id -u) && ls wayland-? 2>/dev/null | head -1); [ -n "$w" ] && echo "export WAYLAND_DISPLAY=\"$w\"" >> ~/clipmonger-test/env.sh; }
grep -q '^export DISPLAY' ~/clipmonger-test/env.sh || echo 'export DISPLAY=":0"' >> ~/clipmonger-test/env.sh
cat ~/clipmonger-test/env.sh
