#!/bin/sh
# Compile-check the Windows target from Linux with zig as the C toolchain.
#
#   packaging/windows/cross-check.sh [cargo check args...]
#
# Needs: rustup target add x86_64-pc-windows-gnu; zig (any recent), nasm for the
# network feature (aws-lc). It proves the code compiles for Windows, nothing
# more: real behaviour is the CI `windows` job's business (and a real machine's).
set -eu
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
W=${TMPDIR:-/tmp}/jizzcraft-zig-$$
mkdir -p "$W"
trap 'rm -rf "$W"' EXIT
# cc-rs passes --target=x86_64-pc-windows-gnu, which zig does not parse:
# drop it and give zig its own spelling.
cat >"$W/zig-cc" <<'WRAP'
#!/bin/sh
for a; do
    shift
    case "$a" in
        --target=*|-target) ;;
        x86_64-pc-windows-gnu) ;;
        *) set -- "$@" "$a" ;;
    esac
done
exec zig cc -target x86_64-windows-gnu "$@"
WRAP
cat >"$W/zig-ar" <<WRAP
#!/bin/sh
exec zig ar "\$@"
WRAP
cat >"$W/x86_64-w64-mingw32-dlltool" <<WRAP
#!/bin/sh
exec zig dlltool "\$@"
WRAP
chmod +x "$W/zig-cc" "$W/zig-ar" "$W/x86_64-w64-mingw32-dlltool"

export CC_x86_64_pc_windows_gnu="$W/zig-cc"
export AR_x86_64_pc_windows_gnu="$W/zig-ar"
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER="$W/zig-cc"
# GStreamer's -sys crates only need to know the library names to check.
for k in GSTREAMER_1_0 GSTREAMER_APP_1_0 GSTREAMER_VIDEO_1_0 GSTREAMER_PBUTILS_1_0 \
         GSTREAMER_BASE_1_0 GSTREAMER_AUDIO_1_0 GSTREAMER_TAG_1_0 GSTREAMER_SDP_1_0 \
         GSTREAMER_RTP_1_0 GSTREAMER_RTSP_1_0 GSTREAMER_GL_1_0 GLIB_2_0 GOBJECT_2_0 GIO_2_0; do
    export "SYSTEM_DEPS_${k}_NO_PKG_CONFIG=1"
    export "SYSTEM_DEPS_${k}_LIB=$(echo "$k" | tr 'A-Z_' 'a-z-')"
done
export PATH="$W:$PATH"
cd "$ROOT"
exec cargo check --target x86_64-pc-windows-gnu "$@"
