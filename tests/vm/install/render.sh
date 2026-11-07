#!/bin/sh
# Fill the install templates into OUTDIR for one VM. Credentials come from the
# environment and never live in the repository:
#   VM_USER VM_PASS VM_ROOT_PASS   (required)
#   VM_SSH_KEY   public key file   (default ~/.ssh/clipmonger_vm.pub)
#   VM_HOSTNAME  (default clipmonger-vm)
#   HOST_IP / SERVE_PORT  where the host serves OUTDIR (default 192.168.122.1:8770)
#   APT_MIRROR / APT_SECURITY  Ubuntu archives (default archive / security.ubuntu.com;
#                end-of-life releases: old-releases.ubuntu.com for both)
# Usage: render.sh OUTDIR
set -eu
OUT=${1:?OUTDIR}
: "${VM_USER:?}" "${VM_PASS:?}" "${VM_ROOT_PASS:?}"
KEYFILE=${VM_SSH_KEY:-$HOME/.ssh/clipmonger_vm.pub}
HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$OUT"
export R_USER="$VM_USER" R_PASS="$VM_PASS" R_ROOT="$VM_ROOT_PASS" \
  R_PHASH="$(openssl passwd -6 "$VM_PASS")" R_RHASH="$(openssl passwd -6 "$VM_ROOT_PASS")" \
  R_KEY="$(cat "$KEYFILE")" R_HOST="${VM_HOSTNAME:-clipmonger-vm}" \
  R_IP="${HOST_IP:-192.168.122.1}" R_PORT="${SERVE_PORT:-8770}" \
  R_MIRROR="${APT_MIRROR:-archive.ubuntu.com}" R_SEC="${APT_SECURITY:-security.ubuntu.com}"
for f in arch.sh post.sh k.sh mint.seed ubuntu-desktop-autoinstall.yaml ubuntu-cloud-desktop.yaml; do
  python3 - "$HERE/$f" "$OUT/$f" <<'PY'
import os, sys
text = open(sys.argv[1]).read()
for key, env in (("VM_USER", "R_USER"), ("VM_PASS_HASH", "R_PHASH"), ("VM_ROOT_HASH", "R_RHASH"),
                 ("VM_ROOT_PASS", "R_ROOT"), ("VM_PASS", "R_PASS"), ("VM_SSH_KEY", "R_KEY"),
                 ("VM_HOSTNAME", "R_HOST"), ("HOST_IP", "R_IP"), ("SERVE_PORT", "R_PORT"),
                 ("APT_MIRROR", "R_MIRROR"), ("APT_SECURITY", "R_SEC")):
    text = text.replace("@%s@" % key, os.environ[env])
open(sys.argv[2], "w").write(text)
PY
done
cp "$KEYFILE" "$OUT/key.pub"
printf 'instance-id: %s-1\nlocal-hostname: %s\n' "$R_HOST" "$R_HOST" > "$OUT/meta-data"
: > "$OUT/vendor-data"
echo "rendered into $OUT"
