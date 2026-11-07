#!/usr/bin/env bash
# Build the Linux release packages from the Debian 12 container build
# (scripts/build-linux-release.sh), the committed GNOME extension dist/ and the
# verified Rust notices (scripts/NOTICE-SCAN.md):
#
#   clipmonger_<v>-1_amd64.deb, clipmonger-<v>-1.x86_64.rpm   (system install, /opt/clipmonger)
#   clipmonger-<v>-linux-x86_64.tar.gz + install.sh         (per-user install, ~/.local)
#   SHA256SUMS
#
# Usage: scripts/package-linux.sh RUST-NOTICES.txt OUTDIR
set -euo pipefail
umask 022

ROOT=$(cd "$(dirname "$0")/.." && pwd)
NOTICES=$(realpath "${1:?RUST-NOTICES.txt}")
OUT=${2:?OUTDIR}
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/crates/app-ui/Cargo.toml" | head -n 1)
BINS=$ROOT/target/linux-release/release
NFPM_IMAGE=goreleaser/nfpm:v2.48.0
EXT_UUID=clipmonger@extensions.clipmonger.dev

[ -s "$NOTICES" ] || { echo "missing notices: $NOTICES" >&2; exit 1; }
python3 "$ROOT/scripts/check-glibc-floor.py" "$BINS/clipmonger" "$BINS/clipmonger-daemon" "$BINS/clipmonger-cli"
mkdir -p "$OUT"
OUT=$(realpath "$OUT")
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# Shared pieces: licences and the GNOME extension (runtime files only, schemas compiled).
mkdir -p "$WORK/licenses" "$WORK/ext"
cp -R "$ROOT/licenses/." "$WORK/licenses/"
cp "$NOTICES" "$WORK/licenses/RUST-NOTICES.txt"
cp "$ROOT/LICENSE" "$WORK/licenses/LICENSE"
cp -R "$ROOT/gnome-ext/dist/." "$WORK/ext/"
rm -rf "$WORK/ext/tests" "$WORK/ext/src"
glib-compile-schemas --strict "$WORK/ext/schemas"

# --- System packages (.deb, .rpm) -------------------------------------------
STAGE=$WORK/stage
mkdir -p "$STAGE/opt/clipmonger" "$STAGE/usr/share/applications" \
    "$STAGE/usr/share/gnome-shell/extensions/$EXT_UUID" "$STAGE/usr/share/doc/clipmonger"
install -m 755 "$BINS/clipmonger" "$BINS/clipmonger-daemon" "$BINS/clipmonger-cli" "$STAGE/opt/clipmonger/"
install -m 644 "$ROOT/packaging/linux/com.martinambrus.clipmonger.desktop" \
    "$ROOT/packaging/linux/com.martinambrus.clipmonger.daemon.desktop" "$STAGE/usr/share/applications/"
cp -R "$WORK/ext/." "$STAGE/usr/share/gnome-shell/extensions/$EXT_UUID/"
cp -R "$WORK/licenses/." "$STAGE/usr/share/doc/clipmonger/"
cat > "$STAGE/usr/share/doc/clipmonger/README" <<EOF
Clipmonger $VERSION: clipboard history with a popup (Ctrl+Alt+H) and permanent clips.
Start it from the applications menu. History is kept in ~/.local/share/clipmonger.
GNOME on Wayland: log out and back in once, then
  gnome-extensions enable $EXT_UUID
Sway, Hyprland, COSMIC: Settings shows the commands to bind the hotkey and the login start.
Licences: this folder (RUST-NOTICES.txt lists the Rust dependencies).
https://github.com/martinambrus/clipmonger
EOF

# Packages carry 644 files and 755 directories and programs, whatever the umask.
chmod -R u+rwX,go+rX,go-w "$WORK"
cat > "$WORK/nfpm.yaml" <<EOF
name: clipmonger
arch: amd64
platform: linux
version: $VERSION
release: 1
section: utils
priority: optional
maintainer: Martin Ambrus <martin@martinambrus.com>
vendor: Martin Ambrus
homepage: https://github.com/martinambrus/clipmonger
license: MIT
description: |-
  Clipboard history manager with a popup and permanent clips.
  Clipmonger keeps a searchable clipboard history (text, images, files),
  opens a popup on a global hotkey (Ctrl+Alt+H) and pastes the chosen clip
  into the window you came from. Supports X11 and Wayland (GNOME through the
  bundled Shell extension, KDE, Sway, Hyprland, COSMIC).
contents:
  - src: /stage/opt/clipmonger
    dst: /opt/clipmonger
    type: tree
  - src: /stage/usr/share/applications
    dst: /usr/share/applications
    type: tree
  - src: /stage/usr/share/gnome-shell/extensions/$EXT_UUID
    dst: /usr/share/gnome-shell/extensions/$EXT_UUID
    type: tree
  - src: /stage/usr/share/doc/clipmonger
    dst: /usr/share/doc/clipmonger
    type: tree
  - src: /opt/clipmonger/clipmonger
    dst: /usr/bin/clipmonger
    type: symlink
  - src: /opt/clipmonger/clipmonger-cli
    dst: /usr/bin/clipmonger-cli
    type: symlink
deb:
  compression: xz
rpm:
  compression: xz
overrides:
  deb:
    depends:
      - libc6 (>= 2.35)
      - libgcc-s1
    recommends:
      - libxkbcommon0
      - libxkbcommon-x11-0
      - libwayland-client0
  rpm:
    depends:
      - glibc >= 2.35
      - libgcc
    recommends:
      - libxkbcommon
      - libxkbcommon-x11
      - libwayland-client
EOF
for fmt in deb rpm; do
    docker run --rm --user "$(id -u):$(id -g)" -v "$STAGE:/stage:ro" -v "$WORK/nfpm.yaml:/nfpm.yaml:ro" \
        -v "$OUT:/out" "$NFPM_IMAGE" package --config /nfpm.yaml --packager "$fmt" --target /out/ >/dev/null
done

# --- Tarball with install.sh (per-user, any distro) ---------------------------
TAR_DIR=$WORK/clipmonger-$VERSION
mkdir -p "$TAR_DIR"
install -m 755 "$BINS/clipmonger" "$BINS/clipmonger-daemon" "$BINS/clipmonger-cli" "$ROOT/packaging/linux/install.sh" "$TAR_DIR/"
cp -R "$WORK/licenses" "$TAR_DIR/licenses"
cp -R "$WORK/ext" "$TAR_DIR/gnome-extension"
cat > "$TAR_DIR/README.txt" <<EOF
Clipmonger $VERSION for x86_64 Linux (glibc 2.35 or newer).

Install for your user (no root needed):   sh install.sh
Remove again:                             sh install.sh --uninstall

Programs go to ~/.local/opt/clipmonger, commands to ~/.local/bin, the menu
entry to ~/.local/share/applications and, on GNOME, the Shell extension to
~/.local/share/gnome-shell/extensions. History is kept in
~/.local/share/clipmonger. Licences: licenses/ (RUST-NOTICES.txt lists the
Rust dependencies).
EOF
chmod -R u+rwX,go+rX,go-w "$TAR_DIR"
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime="@$(git -C "$ROOT" log -1 --format=%ct)" \
    -C "$WORK" -czf "$OUT/clipmonger-$VERSION-linux-x86_64.tar.gz" "clipmonger-$VERSION"
install -m 755 "$ROOT/packaging/linux/install.sh" "$OUT/install.sh"

(cd "$OUT" && sha256sum clipmonger_"$VERSION"-1_amd64.deb clipmonger-"$VERSION"-1.x86_64.rpm \
    clipmonger-"$VERSION"-linux-x86_64.tar.gz install.sh > SHA256SUMS)
ls -la "$OUT"
