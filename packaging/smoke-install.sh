#!/bin/sh
# End-to-end check of a release tarball: install it with install.sh into a
# scratch prefix, make sure the program starts and `doctor` runs, then
# uninstall and make sure nothing is left behind.
#
#   sh packaging/smoke-install.sh dist/jizzcraft-0.1.0-x86_64-linux.tar.xz
set -eu

die() { printf 'smoke-install: %s\n' "$*" >&2; exit 1; }

[ $# -eq 1 ] || die "usage: smoke-install.sh TARBALL"
tarball=$1
[ -f "$tarball" ] || die "no such file: $tarball"
here=$(cd "$(dirname "$0")" && pwd)

p=$(mktemp -d)
d=$(mktemp -d)
trap 'rm -rf "$p" "$d"' EXIT INT TERM
# The manifest goes in a private data home, not the user's.
export XDG_DATA_HOME="$d/data"

sh "$here/install.sh" --archive "$tarball" --prefix "$p" --no-deps --yes >/dev/null

for f in bin/jizzcraft bin/jizzcraft-cli \
         share/applications/org.jizzcraft.JizzCraft.desktop \
         share/applications/local.Viewer.desktop \
         share/metainfo/org.jizzcraft.JizzCraft.metainfo.xml \
         share/jizzcraft/gst-packages.tsv; do
    [ -e "$p/$f" ] || die "missing after install: $f"
done
[ -x "$p/bin/jizzcraft-cli" ] || die "jizzcraft-cli is not executable"
"$p/bin/jizzcraft-cli" --version >/dev/null || die "jizzcraft-cli --version failed"

# 0: fine, 2: only codecs missing (CI has no libav). 1: something required.
set +e
"$p/bin/jizzcraft-cli" --home "$d/home" doctor >/dev/null
rc=$?
set -e
[ "$rc" = 0 ] || [ "$rc" = 2 ] || die "doctor reported a required problem (exit $rc)"

sh "$here/install.sh" --uninstall --prefix "$p" --yes >/dev/null
[ ! -e "$p/bin/jizzcraft" ] || die "binary still there after uninstall"
left=$(find "$p" -type f 2>/dev/null | wc -l)
[ "$left" -eq 0 ] || die "$left file(s) left after uninstall"
printf 'smoke-install: ok\n'
