#!/bin/sh
# macOS .app bundle (untested here: built by CI on macos-latest).
# Needs the official GStreamer framework (runtime + devel .pkg) or Homebrew
# `gstreamer`; the app uses the system GStreamer, it does not embed it.
set -eu
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
export PKG_CONFIG_PATH="/Library/Frameworks/GStreamer.framework/Versions/Current/lib/pkgconfig:/opt/homebrew/lib/pkgconfig:${PKG_CONFIG_PATH:-}"
cd "$ROOT"
cargo build --release --locked -p jizzcraft -p jizzcraft-cli
APP="dist/JizzCraft.app"
rm -rf dist && mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/jizzcraft "$APP/Contents/MacOS/jizzcraft"
cp packaging/macos/Info.plist "$APP/Contents/Info.plist"
# .icns from the 256 px PNG when `sips` and `iconutil` exist (macOS).
if command -v iconutil >/dev/null 2>&1; then
  I=$(mktemp -d)/JizzCraft.iconset; mkdir -p "$I"
  sips -z 256 256 assets/icons/hicolor/256x256/apps/org.jizzcraft.JizzCraft.png --out "$I/icon_256x256.png" >/dev/null
  sips -z 128 128 assets/icons/hicolor/256x256/apps/org.jizzcraft.JizzCraft.png --out "$I/icon_128x128.png" >/dev/null
  iconutil -c icns "$I" -o "$APP/Contents/Resources/JizzCraft.icns"
fi
codesign --force --deep --sign "${MACOS_SIGN_IDENTITY:--}" "$APP"
( cd dist && zip -qr JizzCraft-macos-$(uname -m).zip JizzCraft.app )
echo "dist/JizzCraft-macos-$(uname -m).zip"
