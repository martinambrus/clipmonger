#!/bin/sh
# Build release binaries for WORKTREE in the Debian 12 container.
# A host-wide lock keeps it to one heavy build at a time.
set -eu
mkdir -p "$HOME/.cache"
WT=${1:?worktree}
exec flock "${CLIPMONGER_BUILD_LOCK:-$HOME/.cache/clipmonger-build.lock}" "$WT/scripts/build-linux-release.sh"
