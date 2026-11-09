#!/bin/sh
# Clipmonger per-user installer for any x86_64 Linux (glibc 2.35 or newer).
#
#   curl -fsSL https://github.com/martinambrus/clipmonger/releases/latest/download/install.sh | sh
#   sh install.sh               # from an extracted release tarball
#   sh install.sh --uninstall   # remove it again (history in ~/.local/share/clipmonger stays)
#
# Installs into ~/.local (no root needed): programs in ~/.local/opt/clipmonger,
# commands in ~/.local/bin, the menu entry in ~/.local/share/applications and,
# on GNOME, the Shell extension in ~/.local/share/gnome-shell/extensions.
#
# Environment: CLIPMONGER_PREFIX (default ~/.local), CLIPMONGER_VERSION (a
# release tag such as 0.2.0; default the latest release), CLIPMONGER_BASE_URL
# (where the release files are; overrides the version).
set -eu

REPO=https://github.com/martinambrus/clipmonger
PREFIX=${CLIPMONGER_PREFIX:-$HOME/.local}
APP_DIR=$PREFIX/opt/clipmonger
BIN_DIR=$PREFIX/bin
APPS_DIR=${XDG_DATA_HOME:-$HOME/.local/share}/applications
ICONS_DIR=${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor
EXT_UUID=clipmonger@extensions.clipmonger.dev
EXT_DIR=${XDG_DATA_HOME:-$HOME/.local/share}/gnome-shell/extensions/$EXT_UUID
APP_ID=com.martinambrus.clipmonger

say() { printf '%s\n' "$*"; }
die() { printf 'clipmonger install: %s\n' "$*" >&2; exit 1; }

stop_running() {
    if [ -x "$APP_DIR/clipmonger-cli" ] && [ -d "$HOME/.local/share/clipmonger" ]; then
        "$APP_DIR/clipmonger-cli" --profile-dir "$HOME/.local/share/clipmonger" stop >/dev/null 2>&1 || true
    fi
    pkill -x clipmonger 2>/dev/null || true
    pkill -x clipmonger-daem 2>/dev/null || true
}

uninstall() {
    stop_running
    if command -v gnome-extensions >/dev/null 2>&1; then
        gnome-extensions disable "$EXT_UUID" >/dev/null 2>&1 || true
    fi
    rm -rf "$APP_DIR" "$EXT_DIR"
    rm -f "$BIN_DIR/clipmonger" "$BIN_DIR/clipmonger-cli"
    rm -f "$APPS_DIR/$APP_ID.desktop" "$APPS_DIR/$APP_ID.daemon.desktop"
    rm -f "${XDG_CONFIG_HOME:-$HOME/.config}/autostart/$APP_ID.desktop"
    find "$ICONS_DIR" -type f -name "$APP_ID.png" -delete 2>/dev/null || true
    say "Clipmonger removed. Your history is still in ~/.local/share/clipmonger (delete it to remove everything)."
}

fetch() { # URL FILE
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$1" -o "$2"
    elif command -v wget >/dev/null 2>&1; then
        wget -q "$1" -O "$2"
    else
        die "needs curl or wget to download"
    fi
}

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
    else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

# Directory holding the release files: next to this script when it runs from
# an extracted tarball, else downloaded and verified.
find_source() {
    here=$(dirname "$0" 2>/dev/null || echo .)
    if [ -f "$here/clipmonger-daemon" ] && [ -f "$here/clipmonger" ]; then
        SRC=$(cd "$here" && pwd)
        return
    fi
    [ "$(uname -m)" = x86_64 ] || die "only x86_64 builds are published"
    if [ -n "${CLIPMONGER_BASE_URL:-}" ]; then base=$CLIPMONGER_BASE_URL
    elif [ -n "${CLIPMONGER_VERSION:-}" ]; then base=$REPO/releases/download/$CLIPMONGER_VERSION
    else base=$REPO/releases/latest/download; fi
    TMP=$(mktemp -d)
    trap 'rm -rf "$TMP"' EXIT
    say "Downloading Clipmonger from $base ..."
    fetch "$base/SHA256SUMS" "$TMP/SHA256SUMS" || die "cannot download SHA256SUMS from $base"
    line=$(grep -E ' \*?clipmonger-[0-9][^ ]*-linux-x86_64\.tar\.gz$' "$TMP/SHA256SUMS" | head -n 1) \
        || die "no Linux tarball listed in SHA256SUMS"
    want=$(printf '%s' "$line" | cut -d' ' -f1)
    name=$(printf '%s' "$line" | sed 's/^[^ ]* *[*]\{0,1\}//')
    fetch "$base/$name" "$TMP/$name" || die "cannot download $name"
    [ "$(sha256 "$TMP/$name")" = "$want" ] || die "checksum mismatch for $name, not installing"
    tar -xzf "$TMP/$name" -C "$TMP"
    SRC=$(find "$TMP" -mindepth 1 -maxdepth 1 -type d -name 'clipmonger-*' | head -n 1)
    [ -n "$SRC" ] && [ -f "$SRC/clipmonger-daemon" ] || die "unexpected tarball layout"
}

install_files() {
    stop_running
    mkdir -p "$APP_DIR" "$BIN_DIR" "$APPS_DIR"
    for f in clipmonger clipmonger-daemon clipmonger-cli; do
        install -m 755 "$SRC/$f" "$APP_DIR/$f.new" && mv -f "$APP_DIR/$f.new" "$APP_DIR/$f"
    done
    rm -rf "$APP_DIR/licenses"
    cp -R "$SRC/licenses" "$APP_DIR/licenses"
    cp "$SRC/README.txt" "$APP_DIR/README.txt"
    cp "$SRC/install.sh" "$APP_DIR/install.sh" 2>/dev/null || true
    ln -sf "$APP_DIR/clipmonger" "$BIN_DIR/clipmonger"
    ln -sf "$APP_DIR/clipmonger-cli" "$BIN_DIR/clipmonger-cli"
    mkdir -p "$ICONS_DIR"
    cp -R "$SRC/icons/hicolor/." "$ICONS_DIR/"
    # The same entries the daemon writes itself (marked as generated, so it
    # keeps them pointing at this folder).
    cat > "$APPS_DIR/$APP_ID.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Clipmonger
Comment=Clipboard history
Exec=$APP_DIR/clipmonger
Icon=$APP_ID
Terminal=false
Categories=Utility;
StartupWMClass=$APP_ID
X-Clipmonger-Generated=true
EOF
    cat > "$APPS_DIR/$APP_ID.daemon.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Clipmonger daemon
Exec=$APP_DIR/clipmonger-daemon
NoDisplay=true
Terminal=false
X-KDE-Wayland-Interfaces=org_kde_kwin_fake_input
X-Clipmonger-Generated=true
EOF
    command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$APPS_DIR" >/dev/null 2>&1 || true
    GNOME=no
    if command -v gnome-shell >/dev/null 2>&1 && [ -d "$SRC/gnome-extension" ]; then
        rm -rf "$EXT_DIR"
        mkdir -p "$EXT_DIR"
        cp -R "$SRC/gnome-extension/." "$EXT_DIR/"
        if command -v glib-compile-schemas >/dev/null 2>&1; then
            glib-compile-schemas "$EXT_DIR/schemas"
        fi
        GNOME=yes
    fi
}

main() {
    case "${1:-}" in
        --uninstall) uninstall; return ;;
        ""|--install) ;;
        *) die "usage: install.sh [--uninstall]" ;;
    esac
    [ "$(id -u)" != 0 ] || die "run as your own user, not root (it installs into your home folder)"
    find_source
    install_files
    say "Clipmonger installed in $APP_DIR."
    case ":$PATH:" in *":$BIN_DIR:"*) ;; *) say "Add $BIN_DIR to your PATH to run clipmonger from a terminal." ;; esac
    if [ "$GNOME" = yes ]; then
        say "GNOME: log out and back in, then run: gnome-extensions enable $EXT_UUID"
    fi
    say "Start it from the applications menu (Clipmonger), then press Ctrl+Alt+H."
    say "On Sway, Hyprland and COSMIC, Settings shows the commands to bind the hotkey and the login start."
}

main "$@"
