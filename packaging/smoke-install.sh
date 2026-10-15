#!/bin/sh
# End-to-end check of a release tarball: install it with install.sh into a
# scratch prefix, make sure the program starts and `doctor` runs, turn the
# encrypted vault on and off on the installed binary, then uninstall and make
# sure nothing is left behind.
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

# ---- encrypted vault on the installed binary --------------------------------
# enable -> doctor says "vault: on" and names nothing -> verify -> locked
# without the passphrase -> disable. The passphrase is random, lives in a shell
# variable only (printf is a builtin: it never reaches an argv), and no output
# of a command that handles it is printed. `set -x` must never be turned on here.
cli="$p/bin/jizzcraft-cli"
vh="$d/vhome"
vm="$d/vmedia"
canary="smoke-canary-9f2c41"
mkdir -p "$vm"
: >"$vm/$canary.mp4"
pw=$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n')
[ "${#pw}" -ge 12 ] || die "could not make a passphrase"
vcli() { "$cli" --home "$vh" "$@"; }
vcli scan "$vm" >/dev/null 2>&1 || die "vault: scan of the synthetic library failed"
printf '%s\n' "$pw" | vcli --passphrase-stdin vault enable --i-understand-no-recovery >/dev/null 2>&1 \
    || die "vault enable failed"
set +e
rep=$(printf '%s\n' "$pw" | vcli --passphrase-stdin doctor --report 2>/dev/null)
set -e
printf '%s\n' "$rep" | grep -q 'vault: on' || die "doctor --report does not say 'vault: on'"
case "$rep" in *"$canary"*) die "doctor --report names a library item" ;; esac
case "$rep" in *"$pw"*) die "doctor --report shows the passphrase" ;; esac
set +e
rep=$(vcli doctor --report 2>/dev/null </dev/null)
set -e
printf '%s\n' "$rep" | grep -q 'vault: locked' || die "locked library is not reported as 'vault: locked'"
printf '%s\n' "$pw" | vcli --passphrase-stdin vault verify >/dev/null 2>&1 || die "vault verify failed"
if printf '%s\n' "not-the-passphrase-at-all" | vcli --passphrase-stdin vault verify >/dev/null 2>&1; then
    die "vault verify accepted a wrong passphrase"
fi
# The clear name must not be on disk any more (database, wal, backups, caches).
if grep -rqF "$canary" "$vh" 2>/dev/null; then die "library name found in clear under the home after enable"; fi
printf '%s\n' "$pw" | vcli --passphrase-stdin vault disable >/dev/null 2>&1 || die "vault disable failed"
vcli vault status 2>/dev/null | grep -q 'vault: off' || die "vault is not off after disable"
unset pw rep

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
