#!/bin/sh
# Build the debug binaries, install them over this user's ~/.local install
# (the release files are kept once in clipmonger.release) and restart.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
APP=$HOME/.local/opt/clipmonger
PROFILE=${XDG_DATA_HOME:-$HOME/.local/share}/clipmonger
EXT=${XDG_DATA_HOME:-$HOME/.local/share}/gnome-shell/extensions/clipmonger@extensions.clipmonger.dev
BIN="clipmonger clipmonger-daemon clipmonger-cli"

cd "$ROOT"
mkdir -p "$HOME/.cache"
flock "$HOME/.cache/clipmonger-build.lock" nice -n 10 cargo build -j 4 \
    -p clipmonger-daemon -p clipmonger-app-ui -p clipmonger-app-cli

[ -e "$APP.release" ] || cp -a "$APP" "$APP.release"

"$APP/clipmonger-cli" --profile-dir "$PROFILE" stop >/dev/null 2>&1 || true
for pid in $(pgrep -x clipmonger; pgrep -x clipmonger-daem); do
    timeout 10 tail --pid="$pid" -f /dev/null || kill "$pid" 2>/dev/null || true
done

for name in $BIN; do
    cp "target/debug/$name" "$APP/$name.new"
    mv -f "$APP/$name.new" "$APP/$name"
done

reload=
if [ -d "$EXT" ]; then
    for file in extension.js placer.js placement.js; do
        cmp -s "gnome-ext/dist/$file" "$EXT/$file" || reload=1
    done
    cp -R gnome-ext/dist/. "$EXT/"
    glib-compile-schemas "$EXT/schemas" 2>/dev/null || true
fi

setsid -f "$APP/clipmonger" >/dev/null 2>&1 </dev/null
echo "installed debug build $(git rev-parse --short HEAD)$(git diff --quiet || echo +dirty)"
[ -z "$reload" ] || echo "GNOME extension changed: log out and back in to load it"
