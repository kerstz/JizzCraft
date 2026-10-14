#!/bin/sh
# AppImage. Run from the repository root; needs linuxdeploy, its gstreamer
# plugin (https://github.com/linuxdeploy/linuxdeploy-plugin-gstreamer) and an
# old-enough build host (Ubuntu 22.04) so glibc is not newer than the targets.
# Vulkan and GL stay on the host: the GPU driver must match the kernel.
set -eu
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
APPDIR="$ROOT/target/AppDir"
rm -rf "$APPDIR"
(cd "$ROOT" && cargo run -q -p xtask -- dist --destdir "$APPDIR" --prefix /usr)

# Keep the host's GStreamer plugins out and the bundled ones in. The registry
# lives in the user cache so a read-only AppImage still starts.
mkdir -p "$APPDIR/apprun-hooks"
cat > "$APPDIR/apprun-hooks/jizzcraft-gst.sh" <<'HOOK'
export GST_PLUGIN_SYSTEM_PATH_1_0="$APPDIR/usr/lib/gstreamer-1.0"
unset GST_PLUGIN_PATH GST_PLUGIN_PATH_1_0
export GST_PLUGIN_SCANNER_1_0="$APPDIR/usr/lib/gstreamer1.0/gstreamer-1.0/gst-plugin-scanner"
export GST_REGISTRY_1_0="${XDG_CACHE_HOME:-$HOME/.cache}/jizzcraft/gst-registry-appimage.bin"
HOOK

export DEPLOY_GSTREAMER_PLUGINS_DIR=$(pkg-config --variable=pluginsdir gstreamer-1.0)
export LDAI_UPDATE_INFORMATION="gh-releases-zsync|kerstz|JizzCraft|latest|jizzcraft-*-x86_64.AppImage.zsync"
linuxdeploy --appdir "$APPDIR" --plugin gstreamer --output appimage \
  --exclude-library 'libvulkan*' --exclude-library 'libGL*' --exclude-library 'libdrm*' \
  --exclude-library 'libva*' \
  --desktop-file "$APPDIR/usr/share/applications/org.jizzcraft.JizzCraft.desktop"
