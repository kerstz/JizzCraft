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

DEPLOY_GSTREAMER_PLUGINS_DIR=$(pkg-config --variable=pluginsdir gstreamer-1.0)
export DEPLOY_GSTREAMER_PLUGINS_DIR
export LDAI_UPDATE_INFORMATION="gh-releases-zsync|kerstz|JizzCraft|latest|jizzcraft-*-x86_64.AppImage.zsync"
# 1. Deploy libraries and GStreamer plugins, no image yet.
linuxdeploy --appdir "$APPDIR" --plugin gstreamer \
  --exclude-library 'libvulkan*' --exclude-library 'libGL*' --exclude-library 'libdrm*' \
  --exclude-library 'libva*' \
  --desktop-file "$APPDIR/usr/share/applications/io.github.kerstz.JizzCraft.desktop"

# 2. Licence policy: no GPL plugin ships (the bundle stays LGPL, replaceable).
# These are the GStreamer "ugly"/GPL plugins and x264/x265 wrappers.
for g in x264 x265 mpeg2dec a52dec dvdread dvdlpcmdec dvdsub xingmux sidplay cdio \
         twolame lame resindvd realmedia amrnb amrwbdec; do
  rm -f "$APPDIR"/usr/lib/gstreamer-1.0/libgst"$g".so \
        "$APPDIR"/usr/lib/gstreamer-1.0/libgst"$g"*.so
done

# The AppImage ships WITHOUT gst-libav: it links the build host's FFmpeg, whose
# licence (LGPL or GPL build) is not ours to vouch for. H.264/HEVC/AAC then come
# from the user's system GStreamer through `jizzcraft-cli doctor`, which says so.
rm -f "$APPDIR"/usr/lib/gstreamer-1.0/libgstlibav.so
rm -f "$APPDIR"/usr/lib/libav*.so* "$APPDIR"/usr/lib/libsw*.so* 2>/dev/null || true

# 3. LGPL notices, licence text and plugin list (SOURCE.txt).
LIC="$APPDIR/usr/share/licenses/jizzcraft/gstreamer"
mkdir -p "$LIC"
LGPL=""
for c in /usr/share/common-licenses/LGPL-2.1 /usr/share/licenses/LGPL-2.1 ; do
  [ -r "$c" ] && LGPL="$c" && break
done
if [ -n "$LGPL" ]; then
  cp "$LGPL" "$LIC/LGPL-2.1.txt"
elif [ "${CI:-}" = "true" ]; then
  echo "error: LGPL-2.1 text not found (install the base-files/common-licenses package)" >&2
  exit 1
else
  echo "warning: LGPL-2.1 text not found; see SOURCE.txt" >&2
fi
GSTVER=$(pkg-config --modversion gstreamer-1.0 2>/dev/null || echo unknown)
{
  echo "GStreamer $GSTVER, bundled in this AppImage under the GNU LGPL-2.1 (see LGPL-2.1.txt)."
  echo "Upstream source code: https://gstreamer.freedesktop.org/src/ (version $GSTVER)"
  echo "Distribution packages: https://packages.ubuntu.com/source/ (source package gstreamer1.0, gst-plugins-*)"
  echo
  echo "The shared libraries and plugins are ordinary .so files. You may replace them:"
  echo "extract with --appimage-extract, swap the files, repack with appimagetool."
  [ -n "$LGPL" ] || echo "LGPL-2.1 text: https://www.gnu.org/licenses/old-licenses/lgpl-2.1.txt"
  echo
  echo "Bundled GStreamer plugin files (name, version):"
  for f in "$APPDIR"/usr/lib/gstreamer-1.0/*.so; do
    [ -e "$f" ] || continue
    v=$(gst-inspect-1.0 "$f" 2>/dev/null | sed -n 's/^ *Version *//p' | head -1)
    echo "  $(basename "$f")  ${v:-$GSTVER}"
  done
  echo
  echo "Note: gst-libav links FFmpeg libraries from the build host; their licence"
  echo "follows the host build of FFmpeg (see docs/licenses.md)."
} > "$LIC/SOURCE.txt"

# 4. Build the image.
linuxdeploy --appdir "$APPDIR" --output appimage
