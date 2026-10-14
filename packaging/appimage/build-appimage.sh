#!/bin/sh
# AppImage via linuxdeploy (untested here). Run from the repository root after
# `cargo build --release -p jizzcraft`. Needs linuxdeploy and its gstreamer plugin
# (https://github.com/linuxdeploy/linuxdeploy-plugin-gstreamer).
set -eu
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
APPDIR="$ROOT/target/AppDir"
rm -rf "$APPDIR"
install -Dm755 "$ROOT/target/release/jizzcraft" "$APPDIR/usr/bin/jizzcraft"
install -Dm644 "$ROOT/assets/org.jizzcraft.JizzCraft.desktop" "$APPDIR/usr/share/applications/org.jizzcraft.JizzCraft.desktop"
install -Dm644 "$ROOT/assets/icons/hicolor/256x256/apps/org.jizzcraft.JizzCraft.png" "$APPDIR/usr/share/icons/hicolor/256x256/apps/org.jizzcraft.JizzCraft.png"
install -Dm644 "$ROOT/assets/org.jizzcraft.JizzCraft.metainfo.xml" "$APPDIR/usr/share/metainfo/org.jizzcraft.JizzCraft.metainfo.xml"
DEPLOY_GSTREAMER_PLUGINS_DIR=$(pkg-config --variable=pluginsdir gstreamer-1.0) \
  linuxdeploy --appdir "$APPDIR" --plugin gstreamer --output appimage \
  --desktop-file "$APPDIR/usr/share/applications/org.jizzcraft.JizzCraft.desktop"
