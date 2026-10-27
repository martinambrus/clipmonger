#!/bin/sh
# Install the committed dist/ tree. No npm/tsc here (works without Node).
set -eu
cd "$(dirname "$0")"

if [ "${1:-}" = "--uninstall" ]; then
  UUID=$(sed -n 's/.*"uuid"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' metadata.json | head -n 1)
  DEST="$HOME/.local/share/gnome-shell/extensions/$UUID"
  if command -v gnome-extensions >/dev/null 2>&1; then gnome-extensions disable "$UUID" || true; fi
  rm -rf "$DEST"
  echo "uninstalled $UUID"
  exit 0
fi

if [ ! -f dist/extension.js ] || [ ! -f dist/metadata.json ]; then
  echo "error: dist/ not built — run 'npm run build' first (committed dist/ should normally be present)" >&2
  exit 1
fi

UUID=$(sed -n 's/.*"uuid"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' dist/metadata.json | head -n 1)
if [ -z "$UUID" ]; then
  echo "error: could not parse uuid from dist/metadata.json" >&2
  exit 1
fi

# Clear version-mismatch check against the metadata shell-version list.
if command -v gnome-shell >/dev/null 2>&1; then
  SHELL_VER=$(gnome-shell --version | sed -n 's/[^0-9]*\([0-9][0-9]*\)\..*/\1/p' | head -n 1)
  SUPPORTED=$(sed -n 's/.*"shell-version"[[:space:]]*:[[:space:]]*\[\(.*\)\].*/\1/p' dist/metadata.json)
  case ",$SUPPORTED," in
    *",\"$SHELL_VER\","*|*,'"'$SHELL_VER'"',*) : ;;
    *)
      echo "error: version mismatch — this GNOME Shell is $SHELL_VER but clipmonger supports shell-version [$SUPPORTED] (see dist/metadata.json)" >&2
      exit 1
      ;;
  esac
  echo "gnome-shell $SHELL_VER is within supported shell-version [$SUPPORTED]"
else
  echo "warning: gnome-shell not found; skipping version check" >&2
fi

DEST="$HOME/.local/share/gnome-shell/extensions/$UUID"
mkdir -p "$DEST"
cp -r dist/. "$DEST"/
# The installed dir needs only the runtime tree; drop dev test output if present.
rm -rf "$DEST/tests" "$DEST/src"

if command -v glib-compile-schemas >/dev/null 2>&1; then
  glib-compile-schemas "$DEST/schemas"
else
  echo "error: glib-compile-schemas not found — install libglib2.0-bin/glib2 and re-run" >&2
  exit 1
fi

echo "installed $UUID to $DEST"
if command -v gnome-extensions >/dev/null 2>&1; then
  echo "hint: gnome-extensions enable $UUID"
  echo "hint: on Wayland, log out and back in first so Shell picks up the new extension"
else
  echo "hint: enable '$UUID' with GNOME Extensions / Extension Manager; on Wayland, log out and back in first"
fi
