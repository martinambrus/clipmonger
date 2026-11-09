#!/usr/bin/env bash
# Build Clipmonger.app (universal or single-arch binaries), sign it ad hoc and
# package it as a .zip and a .dmg. Runs on macOS (codesign, ditto, hdiutil).
#
#   scripts/package-macos.sh BINDIR OUTDIR [RUST-NOTICES.txt]
#
# BINDIR holds clipmonger, clipmonger-daemon and clipmonger-cli (for example
# lipo'd universal binaries). Without a notices file (CI test builds) the
# bundle says where the release notices are.
# SIGN_IDENTITY (default "-", ad hoc) selects a Developer ID later.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
BINS=$(cd "${1:?BINDIR}" && pwd)
OUT=${2:?OUTDIR}
NOTICES=${3:-}
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/crates/app-ui/Cargo.toml" | head -n 1)
BUNDLE_ID=com.martinambrus.clipmonger
SIGN_IDENTITY=${SIGN_IDENTITY:--}

mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
APP=$WORK/Clipmonger.app
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/licenses"

for b in clipmonger clipmonger-daemon clipmonger-cli; do
    install -m 755 "$BINS/$b" "$APP/Contents/MacOS/$b"
done
cp "$ROOT/packaging/macos/Clipmonger.icns" "$APP/Contents/Resources/Clipmonger.icns"
cp -R "$ROOT/licenses/." "$APP/Contents/Resources/licenses/"
cp "$ROOT/LICENSE" "$APP/Contents/Resources/licenses/LICENSE"
if [ -n "$NOTICES" ]; then
    cp "$NOTICES" "$APP/Contents/Resources/licenses/RUST-NOTICES.txt"
else
    echo "Test build: see RUST-NOTICES.txt in the Clipmonger $VERSION release." \
        > "$APP/Contents/Resources/licenses/RUST-NOTICES.txt"
fi

cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key><string>en</string>
    <key>CFBundleDisplayName</key><string>Clipmonger</string>
    <key>CFBundleExecutable</key><string>clipmonger</string>
    <key>CFBundleIconFile</key><string>Clipmonger</string>
    <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>CFBundleName</key><string>Clipmonger</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
    <key>LSMinimumSystemVersion</key><string>10.15</string>
    <key>LSUIElement</key><true/>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSHumanReadableCopyright</key><string>MIT License. Copyright (c) Martin Ambrus.</string>
</dict>
</plist>
EOF
plutil -lint "$APP/Contents/Info.plist" >/dev/null

# Sign inside out: helpers keep their own identifiers, the app gets the
# bundle id (Accessibility permission is granted to this identity).
for b in clipmonger-daemon clipmonger-cli; do
    codesign --force --sign "$SIGN_IDENTITY" --identifier "$BUNDLE_ID.${b#clipmonger-}" \
        "$APP/Contents/MacOS/$b"
done
codesign --force --sign "$SIGN_IDENTITY" --identifier "$BUNDLE_ID" "$APP"
codesign --verify --strict --verbose=1 "$APP"

ZIP=$OUT/clipmonger-$VERSION-macos-universal.zip
DMG=$OUT/clipmonger-$VERSION-macos-universal.dmg
rm -f "$ZIP" "$DMG"
ditto -c -k --sequesterRsrc --keepParent "$APP" "$ZIP"
STAGE=$WORK/dmg
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
# hdiutil fails now and then on CI machines ("Resource busy"): retry.
for attempt in 1 2 3; do
    if hdiutil create -volname "Clipmonger $VERSION" -srcfolder "$STAGE" -format UDZO -ov "$DMG"; then
        break
    fi
    [ "$attempt" = 3 ] && { echo "hdiutil create failed" >&2; exit 1; }
    sleep 5
done
(cd "$OUT" && shasum -a 256 "$(basename "$ZIP")" "$(basename "$DMG")")
