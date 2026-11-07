#!/bin/sh
# Copy WORKTREE's Debian-12 release build, the GNOME extension dist and the
# guest helpers to ALIAS:~/clipmonger-test (binaries in bin/).
set -eu
A=${1:?alias}; WT=${2:?worktree}
B="$WT/target/linux-release/release"
ssh "$A" 'mkdir -p ~/clipmonger-test/bin ~/clipmonger-test/gnome-ext' </dev/null
ssh "$A" 'pkill -x clipmonger; pkill -x clipmonger-daem; true' </dev/null
scp -q "$B/clipmonger" "$B/clipmonger-daemon" "$B/clipmonger-cli" "$A:clipmonger-test/bin/"
scp -q -r "$WT/gnome-ext/dist" "$WT/gnome-ext/metadata.json" "$WT/gnome-ext/install-extension.sh" "$A:clipmonger-test/gnome-ext/"
scp -q "$(dirname "$0")"/../guest/linux/* "$A:clipmonger-test/"
ssh "$A" 'cd ~/clipmonger-test && sha256sum bin/* | cut -c1-16' </dev/null
echo "deployed $(git -C "$WT" rev-parse --short HEAD) to $A"
