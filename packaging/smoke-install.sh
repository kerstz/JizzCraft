#!/bin/sh
# End-to-end check of a release tarball: install it with install.sh into a
# scratch prefix, make sure the program starts and `doctor` runs, then
# uninstall and make sure nothing is left behind.
#
#   sh packaging/smoke-install.sh dist/jizzcraft-0.2.0-x86_64-linux.tar.xz [ONLINE_TARBALL]
#
# Cases: offline -> online -> offline. Without ONLINE_TARBALL, an online tree
# is synthesised from the offline one (adds the magnet handler and the
# browser launcher file, the two things that must disappear again).
set -eu

die() { printf 'smoke-install: %s\n' "$*" >&2; exit 1; }

[ $# -ge 1 ] && [ $# -le 2 ] || die "usage: smoke-install.sh TARBALL [ONLINE_TARBALL]"
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
         share/applications/io.github.kerstz.JizzCraft.desktop \
         share/applications/local.Viewer.desktop \
         share/metainfo/io.github.kerstz.JizzCraft.metainfo.xml \
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

# Update over an old manifest: a stale file it lists must disappear.
mkdir -p "$p/share/applications"
: >"$p/share/applications/legacy-old-id.desktop"
man="$d/data/jizzcraft/install-manifest"
printf '%s\n' "$p/share/applications/legacy-old-id.desktop" >>"$man"
sh "$here/install.sh" --archive "$tarball" --prefix "$p" --no-deps --yes >/dev/null
[ ! -e "$p/share/applications/legacy-old-id.desktop" ] || die "stale file from old manifest survived the update"
[ -e "$p/bin/jizzcraft" ] || die "binary missing after update"

sh "$here/install.sh" --uninstall --prefix "$p" --yes >/dev/null
[ ! -e "$p/bin/jizzcraft" ] || die "binary still there after uninstall"
left=$(find "$p" -type f 2>/dev/null | wc -l)
[ "$left" -eq 0 ] || die "$left file(s) left after uninstall"

# ---- editions: offline (done above) -> online -> offline ------------------
online=${2:-}
if [ -z "$online" ]; then
    syn="$d/syn"
    mkdir -p "$syn"
    tar -xJf "$tarball" -C "$syn"
    top=$(find "$syn" -mindepth 1 -maxdepth 1 -type d | head -n 1)
    cp "$top/share/applications/io.github.kerstz.JizzCraft.desktop" \
        "$top/share/applications/io.github.kerstz.JizzCraft-magnet.desktop"
    online="$d/online.tar.xz"
    tar -C "$syn" -cJf "$online" "$(basename "$top")"
fi
man="$d/data/jizzcraft/install-manifest"
magnet="$p/share/applications/io.github.kerstz.JizzCraft-magnet.desktop"

sh "$here/install.sh" --archive "$tarball" --edition offline --prefix "$p" --no-deps --yes >/dev/null
grep -qx 'edition=offline' "$man" || die "manifest lacks edition=offline"
grep -qx 'browser=0' "$man" || die "manifest lacks browser=0"
if sh "$here/install.sh" --archive "$tarball" --edition offline --no-browser --prefix "$p" --no-deps --yes >/dev/null 2>&1; then
    die "--no-browser was accepted with the offline edition"
fi
if sh "$here/install.sh" --archive "$tarball" --edition offline --with-browser --prefix "$p" --no-deps --yes >/dev/null 2>&1; then
    die "--with-browser was accepted with the offline edition"
fi

sh "$here/install.sh" --archive "$online" --edition online --no-browser --prefix "$p" --no-deps --yes >/dev/null
[ -e "$magnet" ] || die "online-only file missing after switching to online"
grep -qx 'edition=online' "$man" || die "manifest lacks edition=online"

# Re-running without flags must keep the online edition (no downgrade).
sh "$here/install.sh" --archive "$online" --prefix "$p" --no-deps --yes >/dev/null
grep -qx 'edition=online' "$man" || die "update downgraded the edition"
[ -e "$magnet" ] || die "update dropped an online file"

sh "$here/install.sh" --archive "$tarball" --edition offline --prefix "$p" --no-deps --yes >/dev/null
[ ! -e "$magnet" ] || die "online-only file left after switching to offline"
grep -qx 'edition=offline' "$man" || die "manifest lacks edition=offline after switch back"
# A plain update keeps offline.
sh "$here/install.sh" --archive "$tarball" --prefix "$p" --no-deps --yes >/dev/null
grep -qx 'edition=offline' "$man" || die "update changed the edition"

sh "$here/install.sh" --uninstall --prefix "$p" --yes >/dev/null
left=$(find "$p" -type f 2>/dev/null | wc -l)
[ "$left" -eq 0 ] || die "$left file(s) left after the edition round trip"
printf 'smoke-install: ok\n'
