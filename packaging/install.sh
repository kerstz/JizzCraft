#!/bin/sh
# JizzCraft installer for Linux.
#
#   curl --proto '=https' --tlsv1.2 -fsSL \
#     https://github.com/kerstz/JizzCraft/releases/latest/download/install.sh | sh
#
# Per-user by default (~/.local), no root needed except to install the
# distribution's GStreamer packages, and it asks before that. Re-run it to
# update. `--uninstall` removes exactly the files it installed and nothing
# else: your library, settings and thumbnails are never touched.
#
# POSIX sh. All logic is inside main(), called on the last line: a truncated
# download runs nothing.

set -eu

REPO="kerstz/JizzCraft"
BASE="https://github.com/$REPO"

PREFIX="${HOME}/.local"
VERSION=""
EDITION=""
BROWSER=""
PURGE_BROWSER=0
BROWSER_ROOT=""
FROM_SOURCE=0
ARCHIVE=""
NO_DEPS=0
YES=0
DISGUISE_ONLY=0
SET_MAGNET=0
UNINSTALL=0
DRY=0
TMP=""

say() { printf '%s\n' "$*"; }
warn() { printf 'install: %s\n' "$*" >&2; }
die() { warn "$*"; exit 1; }

usage() {
    cat <<'USAGE'
Usage: install.sh [options]

  --prefix DIR          install under DIR (default: ~/.local)
  --version X.Y.Z       install this release instead of the latest
  --edition offline|online
                        offline: your own files only, no network code (default)
                        online: adds indexers, torrents and streaming
  --with-browser        add the built-in browser (online edition only)
  --no-browser          do not install the built-in browser (online only)
  --purge-browser-data  delete the browser's own profile data
  --with-network        deprecated alias of --edition online
  --from-source         build with cargo instead of downloading a binary
  --archive FILE        use a local release tarball (offline, no checksum)
  --no-deps             do not install system packages (GStreamer, WebKitGTK)
  --yes                 do not ask questions
  --disguise            install only the neutral "Viewer" launcher entry
  --set-magnet-default  make JizzCraft the handler for magnet: links
  --uninstall           remove what this script installed
  --dry-run             print what would be done
  -h, --help            this text
USAGE
}

run() {
    if [ "$DRY" = 1 ]; then
        printf '+ %s\n' "$*"
    else
        "$@"
    fi
}

cleanup() {
    if [ -n "$TMP" ] && [ -d "$TMP" ]; then rm -rf "$TMP"; fi
}

have() { command -v "$1" >/dev/null 2>&1; }

data_home() { printf '%s' "${XDG_DATA_HOME:-$HOME/.local/share}"; }
manifest_path() { printf '%s/jizzcraft/install-manifest' "$(data_home)"; }

detect_family() {
    # Prints arch | debian | fedora | suse | unknown
    OSID=""
    OSLIKE=""
    if [ -r /etc/os-release ]; then
        # shellcheck source=/dev/null
        OSID=$(. /etc/os-release && printf '%s' "${ID:-}")
        # shellcheck source=/dev/null
        OSLIKE=$(. /etc/os-release && printf '%s' "${ID_LIKE:-}")
    fi
    for w in $OSID $OSLIKE; do
        case "$w" in
            arch|cachyos|manjaro|endeavouros|artix|garuda) echo arch; return ;;
            debian|ubuntu|linuxmint|pop|raspbian|kali|zorin) echo debian; return ;;
            fedora|rhel|centos|rocky|almalinux|nobara) echo fedora; return ;;
            opensuse*|suse|sles) echo suse; return ;;
        esac
    done
    echo unknown
}

detect_arch() {
    case "$(uname -m)" in
        x86_64|amd64) echo x86_64 ;;
        aarch64|arm64) echo aarch64 ;;
        *) die "unsupported CPU: $(uname -m)" ;;
    esac
}

download() {
    # download URL DEST
    if have curl; then
        curl --proto '=https' --tlsv1.2 -fsSL -o "$2" "$1"
    elif have wget; then
        wget -q --https-only -O "$2" "$1"
    else
        die "need curl or wget"
    fi
}

latest_version() {
    if have curl; then
        url=$(curl --proto '=https' --tlsv1.2 -fsSLI -o /dev/null -w '%{url_effective}' "$BASE/releases/latest")
        printf '%s' "${url##*/v}"
    else
        die "need curl to find the latest release (or pass --version)"
    fi
}

sha256_check() {
    # sha256_check DIR FILE: verify FILE against DIR/SHA256SUMS
    if have sha256sum; then
        (cd "$1" && grep -F " $2" SHA256SUMS | sha256sum -c -)
    elif have shasum; then
        (cd "$1" && grep -F " $2" SHA256SUMS | shasum -a 256 -c -)
    else
        die "need sha256sum or shasum to verify the download"
    fi
}

gst_packages() {
    # gst_packages FAMILY TSV: package names, one per line, no hardware rows
    case "$1" in
        arch) col=3 ;;
        debian) col=4 ;;
        fedora) col=5 ;;
        suse) col=6 ;;
        *) return 0 ;;
    esac
    awk -F '\t' -v c="$col" '!/^#/ && NF >= 6 && $2 != "hardware" { print $c }' "$2" | sort -u
}

vulkan_package() {
    case "$1" in
        arch) echo vulkan-icd-loader ;;
        debian) echo libvulkan1 ;;
        fedora) echo vulkan-loader ;;
        suse) echo libvulkan1 ;;
    esac
}

pkg_install() {
    # pkg_install LABEL FAMILY PACKAGES
    label=$1
    fam=$2
    pkgs=$3
    case "$fam" in
        arch) cmd="pacman -S --needed $pkgs" ;;
        debian) cmd="apt-get install -y --no-install-recommends $pkgs" ;;
        fedora) cmd="dnf install -y $pkgs" ;;
        suse) cmd="zypper --non-interactive install $pkgs" ;;
    esac
    if [ "$(id -u)" != 0 ]; then
        if have sudo; then cmd="sudo $cmd"; else
            warn "not root and no sudo: run this yourself as root:"
            warn "  $cmd"
            return 0
        fi
    fi
    say "$label packages needed:"
    say "  $cmd"
    if [ "$DRY" = 1 ]; then return 0; fi
    if [ "$YES" != 1 ]; then
        if [ -r /dev/tty ]; then
            printf 'Run it now? [y/N] ' >/dev/tty
            read -r ans </dev/tty || ans=n
        else
            ans=n
        fi
        case "$ans" in
            y|Y|yes) ;;
            *) say "skipped; run it later or use --yes"; return 0 ;;
        esac
    fi
    # shellcheck disable=SC2086
    run $cmd
}

install_deps() {
    # install_deps FAMILY TSV
    fam=$1
    tsv=$2
    if [ "$NO_DEPS" = 1 ]; then return 0; fi
    if [ "$fam" = unknown ]; then
        warn "unknown distribution: install GStreamer base, good and libav plugins yourself"
        warn "(run 'jizzcraft-cli doctor' afterwards to see what is missing)"
        return 0
    fi
    if [ -e /run/ostree-booted ]; then
        warn "image-based system: use the Flatpak build, or layer the packages yourself"
        return 0
    fi
    pkgs="$(gst_packages "$fam" "$tsv" | tr '\n' ' ')$(vulkan_package "$fam")"
    pkg_install GStreamer "$fam" "$pkgs"
    if [ "$fam" = fedora ]; then
        say "note: H.264 and HEVC on Fedora need the RPM Fusion free repository."
    fi
}

build_tools_hint() {
    # build_tools_hint FAMILY: the command that installs a C compiler, make and
    # perl (the vendored OpenSSL under SQLCipher is configured with perl).
    case "$1" in
        arch) echo "pacman -S --needed base-devel perl" ;;
        debian) echo "apt-get install -y build-essential perl" ;;
        fedora) echo "dnf install -y gcc make perl" ;;
        suse) echo "zypper install gcc make perl" ;;
        *) echo "install a C compiler (cc), make and perl with your package manager" ;;
    esac
}

check_source_tools() {
    # check_source_tools FAMILY: --from-source needs cargo and the C toolchain
    # that SQLCipher's vendored OpenSSL is built with.
    have cargo || die "--from-source needs cargo (https://rustup.rs)"
    missing=""
    if ! have "${CC:-cc}" && ! have gcc && ! have clang; then missing="$missing cc"; fi
    have make || missing="$missing make"
    have perl || missing="$missing perl"
    if [ -n "$missing" ]; then
        die "--from-source needs$missing (SQLCipher builds its own OpenSSL). Install them with: $(build_tools_hint "$1")"
    fi
}

browser_fallback() {
    case "$1" in
        arch) echo "webkitgtk-6.0 bubblewrap" ;;
        debian) echo "libwebkitgtk-6.0-4 bubblewrap" ;;
        fedora) echo "webkitgtk6.0 bubblewrap" ;;
        suse) echo "libwebkitgtk-6_0-4 bubblewrap" ;;
    esac
}

install_browser_deps() {
    # install_browser_deps FAMILY TSV: system packages for the built-in browser
    fam=$1
    tsv=$2
    if [ "$NO_DEPS" = 1 ] || [ "$fam" = unknown ] || [ -e /run/ostree-booted ]; then
        return 0
    fi
    pkgs=$(gst_packages "$fam" "$tsv" | tr '\n' ' ')
    [ -n "$pkgs" ] || pkgs=$(browser_fallback "$fam")
    pkg_install "Browser (WebKitGTK)" "$fam" "$pkgs"
}

ask_tty() {
    # ask_tty PROMPT: print PROMPT, read one line from the terminal (empty if none)
    ANSWER=""
    if [ "$YES" != 1 ] && [ -r /dev/tty ] && [ -w /dev/tty ]; then
        printf '%b' "$1" >/dev/tty
        read -r ANSWER </dev/tty || ANSWER=""
    fi
}

manifest_value() {
    # manifest_value KEY: value from the existing install manifest, or empty
    man=$(manifest_path)
    [ -f "$man" ] || return 0
    sed -n "s/^$1=//p" "$man" | head -n 1
}

resolve_choices() {
    # Sets EDITION (offline|online) and BROWSER (0|1).
    if [ -z "$EDITION" ]; then
        EDITION=$(manifest_value edition)
        case "$EDITION" in offline|online) ;; *) EDITION="" ;; esac
    fi
    if [ -z "$EDITION" ]; then
        ask_tty "JizzCraft comes in two editions:\n  1) Offline  - your own files only. No network code at all. (default)\n  2) Online   - adds indexers, torrents and streaming from your own sources.\nEdition [1/2]: "
        case "$ANSWER" in
            2|online|Online) EDITION=online ;;
            *) EDITION=offline ;;
        esac
    fi
    if [ "$EDITION" = offline ]; then
        [ -z "$BROWSER" ] || die "the built-in browser belongs to the online edition: use --edition online"
        BROWSER=0
        return 0
    fi
    if [ -z "$BROWSER" ]; then
        b=$(manifest_value browser)
        case "$b" in
            1|yes) BROWSER=1 ;;
            0|no) BROWSER=0 ;;
        esac
    fi
    if [ -z "$BROWSER" ]; then
        ask_tty "Add the built-in browser? It uses your system's WebKitGTK and runs in its own process. Nothing is preset; you can turn it off in Settings. [y/N]: "
        case "$ANSWER" in
            y|Y|yes|Yes) BROWSER=1 ;;
            *) BROWSER=0 ;;
        esac
    fi
}

purge_browser_data() {
    for d in "$(data_home)/jizzcraft/browser" \
             "${XDG_CONFIG_HOME:-$HOME/.config}/jizzcraft/browser" \
             "${XDG_CACHE_HOME:-$HOME/.cache}/jizzcraft/browser"; do
        if [ -d "$d" ]; then
            say "Removing browser data: $d"
            run rm -rf "$d"
        fi
    done
}

install_tree() {
    # install_tree SRC_ROOT: copy bin/ and share/ under PREFIX, write manifest
    src=$1
    man=$(manifest_path)
    if [ "$DRY" = 1 ]; then
        say "+ copy $src/* to $PREFIX and write $man"
        return 0
    fi
    mkdir -p "$(dirname "$man")"
    list="$TMP/files.list"
    (cd "$src" && find . -type f -o -type l | sed 's|^\./||' | sort) >"$list"
    if [ "$DISGUISE_ONLY" = 1 ]; then
        grep -E '^(bin/|share/(applications/local\.Viewer|icons/.*/local\.Viewer|man/|bash-completion/|zsh/|fish/))' "$list" >"$list.d" || true
        mv "$list.d" "$list"
    fi
    # Update over an older install: drop files the old manifest listed that
    # this version no longer ships (renamed desktop entries, icons...).
    if [ -f "$man" ]; then
        sed "s|^|$PREFIX/|" "$list" | sort >"$list.new"
        grep -v '^[a-z]*=' "$man" | sort >"$list.old" || true
        comm -23 "$list.old" "$list.new" | while IFS= read -r f; do
            case "$f" in
                /*) rm -f "$f" ;;
            esac
        done
    fi
    {
        say "version=$VERSION_INSTALLED"
        say "edition=$EDITION"
        say "browser=$BROWSER"
        say "prefix=$PREFIX"
        sed "s|^|$PREFIX/|" "$list"
    } >"$man"
    while IFS= read -r f; do
        mkdir -p "$PREFIX/$(dirname "$f")"
        cp -f "$src/$f" "$PREFIX/$f"
    done <"$list"
    if have update-desktop-database; then
        update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true
    fi
    if have gtk-update-icon-cache && [ -d "$PREFIX/share/icons/hicolor" ]; then
        gtk-update-icon-cache -f -t "$PREFIX/share/icons/hicolor" 2>/dev/null || true
    fi
}

path_hint() {
    case ":$PATH:" in
        *":$PREFIX/bin:"*) return 0 ;;
    esac
    say ""
    say "$PREFIX/bin is not on your PATH. Add it:"
    case "${SHELL:-}" in
        */fish) say "  fish_add_path $PREFIX/bin" ;;
        */zsh) say "  echo 'export PATH=\"$PREFIX/bin:\$PATH\"' >> ~/.zshrc" ;;
        *) say "  echo 'export PATH=\"$PREFIX/bin:\$PATH\"' >> ~/.profile" ;;
    esac
}

do_uninstall() {
    man=$(manifest_path)
    [ -f "$man" ] || die "nothing to uninstall (no $man)"
    say "Removing files listed in $man"
    grep -v '^[a-z]*=' "$man" | while IFS= read -r f; do
        case "$f" in
            /*) run rm -f "$f" ;;
        esac
    done
    # Caches written by the desktop tools, when nothing else uses them.
    pfx=$(sed -n 's/^prefix=//p' "$man")
    if [ -n "$pfx" ]; then
        if ! ls "$pfx"/share/applications/*.desktop >/dev/null 2>&1; then
            run rm -f "$pfx/share/applications/mimeinfo.cache"
        fi
        if [ -z "$(find "$pfx/share/icons/hicolor" -type f ! -name icon-theme.cache 2>/dev/null | head -n 1)" ]; then
            run rm -f "$pfx/share/icons/hicolor/icon-theme.cache"
        fi
        # Empty directories this script created (rmdir never removes content).
        grep -v '^[a-z]*=' "$man" | sed 's|/[^/]*$||' | sort -ru | while IFS= read -r d; do
            run rmdir -p "$d" 2>/dev/null || true
        done
    fi
    run rm -f "$man"
    say "Done. Your library, settings and thumbnails were not touched:"
    say "  data:   $(data_home)/jizzcraft"
    say "  config: ${XDG_CONFIG_HOME:-$HOME/.config}/jizzcraft"
    say "  cache:  ${XDG_CACHE_HOME:-$HOME/.cache}/jizzcraft"
}

main() {
    while [ $# -gt 0 ]; do
        case "$1" in
            --prefix) [ $# -ge 2 ] || die "--prefix needs a value"; PREFIX=$2; shift ;;
            --version) [ $# -ge 2 ] || die "--version needs a value"; VERSION=${2#v}; shift ;;
            --edition)
                [ $# -ge 2 ] || die "--edition needs a value"
                case "$2" in offline|online) EDITION=$2 ;; *) die "unknown edition: $2 (offline|online)" ;; esac
                shift ;;
            --with-network|--network)
                warn "$1 is deprecated: use --edition online"
                EDITION=online ;;
            --with-browser) BROWSER=1 ;;
            --no-browser) BROWSER=0 ;;
            --purge-browser-data) PURGE_BROWSER=1 ;;
            --from-source) FROM_SOURCE=1 ;;
            --archive) [ $# -ge 2 ] || die "--archive needs a value"; ARCHIVE=$2; shift ;;
            --no-deps) NO_DEPS=1 ;;
            --yes|-y) YES=1 ;;
            --disguise) DISGUISE_ONLY=1 ;;
            --set-magnet-default) SET_MAGNET=1 ;;
            --uninstall) UNINSTALL=1 ;;
            --dry-run) DRY=1 ;;
            -h|--help) usage; exit 0 ;;
            *) usage >&2; die "unknown option: $1" ;;
        esac
        shift
    done

    [ "$(uname -s)" = Linux ] || die "this installer is for Linux"
    if [ "$UNINSTALL" = 1 ]; then
        do_uninstall
        if [ "$PURGE_BROWSER" = 1 ]; then purge_browser_data; fi
        exit 0
    fi

    TMP=$(mktemp -d)
    trap cleanup EXIT INT TERM
    arch=$(detect_arch)
    fam=$(detect_family)
    resolve_choices
    variant=jizzcraft
    if [ "$EDITION" = online ]; then variant=jizzcraft-online; fi

    if [ "$FROM_SOURCE" = 1 ]; then
        check_source_tools "$fam"
        [ -n "$VERSION" ] || VERSION=$(latest_version)
        say "Building $VERSION from source"
        if [ "$DRY" != 1 ]; then
            download "$BASE/archive/refs/tags/v$VERSION.tar.gz" "$TMP/src.tar.gz"
            mkdir -p "$TMP/src"
            tar -xzf "$TMP/src.tar.gz" -C "$TMP/src" --strip-components=1
            bflag=
            if [ "$BROWSER" = 1 ]; then
                bflag=--browser
                (cd "$TMP/src" && cargo build -q --release -p jizzcraft-browser) \
                    || warn "the browser binary could not be built; continuing without it"
            fi
            # shellcheck disable=SC2086
            (cd "$TMP/src" && cargo run -q -p xtask -- dist --edition "$EDITION" $bflag \
                --prefix / --destdir "$TMP/stage")
            ROOT="$TMP/stage"
        fi
        VERSION_INSTALLED=$VERSION
    elif [ -n "$ARCHIVE" ]; then
        [ -f "$ARCHIVE" ] || die "no such file: $ARCHIVE"
        warn "using a local archive: checksum not verified"
        VERSION_INSTALLED=local
        if [ "$DRY" != 1 ]; then
            tar -xJf "$ARCHIVE" -C "$TMP"
            ROOT=$(find "$TMP" -mindepth 1 -maxdepth 1 -type d | head -n 1)
        fi
    else
        [ -n "$VERSION" ] || VERSION=$(latest_version)
        name="$variant-$VERSION-$arch-linux.tar.xz"
        say "Downloading $name"
        if [ "$DRY" != 1 ]; then
            download "$BASE/releases/download/v$VERSION/$name" "$TMP/$name"
            download "$BASE/releases/download/v$VERSION/SHA256SUMS" "$TMP/SHA256SUMS"
            sha256_check "$TMP" "$name" || die "checksum mismatch: refusing to install"
            if have minisign && download "$BASE/releases/download/v$VERSION/SHA256SUMS.minisig" "$TMP/SHA256SUMS.minisig" 2>/dev/null; then
                say "minisign signature present; verify it with the key in the README:"
                say "  minisign -Vm SHA256SUMS -P <public key>"
            fi
            mkdir -p "$TMP/main"
            tar -xJf "$TMP/$name" -C "$TMP/main"
            ROOT=$(find "$TMP/main" -mindepth 1 -maxdepth 1 -type d | head -n 1)
            if [ "$BROWSER" = 1 ]; then
                bname="jizzcraft-browser-$VERSION-$arch-linux.tar.xz"
                if download "$BASE/releases/download/v$VERSION/$bname" "$TMP/$bname" 2>/dev/null \
                    && sha256_check "$TMP" "$bname"; then
                    mkdir -p "$TMP/browser"
                    tar -xJf "$TMP/$bname" -C "$TMP/browser"
                    BROWSER_ROOT=$(find "$TMP/browser" -mindepth 1 -maxdepth 1 -type d | head -n 1)
                else
                    warn "no browser package in this release: installing without the browser"
                    BROWSER=0
                fi
            fi
        fi
        VERSION_INSTALLED=$VERSION
    fi

    if [ "$DRY" = 1 ]; then
        say "+ would install version ${VERSION_INSTALLED:-?} to $PREFIX (family: $fam, arch: $arch)"
        say "+ edition: $EDITION ($variant)"
        if [ "$BROWSER" = 1 ]; then say "+ browser: yes"; else say "+ browser: no"; fi
        install_deps "$fam" /dev/null
        if [ "$BROWSER" = 1 ]; then install_browser_deps "$fam" /dev/null; fi
        exit 0
    fi

    tsv="$ROOT/share/jizzcraft/gst-packages.tsv"
    [ -f "$tsv" ] || tsv=/dev/null
    install_deps "$fam" "$tsv"
    if [ "$BROWSER" = 1 ]; then
        btsv="$ROOT/share/jizzcraft/browser-packages.tsv"
        [ -f "$btsv" ] || btsv=/dev/null
        install_browser_deps "$fam" "$btsv"
        if [ -n "$BROWSER_ROOT" ] && [ -d "$BROWSER_ROOT" ]; then
            cp -a "$BROWSER_ROOT/." "$ROOT/"
        fi
        if [ ! -e "$ROOT/libexec/jizzcraft/jizzcraft-browser" ]; then
            warn "this build has no browser binary: installing without it"
            BROWSER=0
        fi
    fi
    # Online -> offline (or browser off): the old manifest cleanup in
    # install_tree removes what the new tree no longer ships.
    if [ "$PURGE_BROWSER" = 1 ] && [ "$BROWSER" != 1 ]; then purge_browser_data; fi
    install_tree "$ROOT"

    if [ "$SET_MAGNET" = 1 ]; then
        if [ -f "$PREFIX/share/applications/io.github.kerstz.JizzCraft-magnet.desktop" ]; then
            run xdg-mime default io.github.kerstz.JizzCraft-magnet.desktop x-scheme-handler/magnet
        else
            warn "this build has no magnet handler (use --edition online)"
        fi
    fi

    say ""
    say "Installed JizzCraft $VERSION_INSTALLED to $PREFIX"
    path_hint
    say ""
    if [ -x "$PREFIX/bin/jizzcraft-cli" ]; then
        "$PREFIX/bin/jizzcraft-cli" doctor || true
    fi
    say ""
    say "Start it from your application menu, or run: jizzcraft"
    say "Update: run this script again.  Uninstall: install.sh --uninstall"
}

main "$@"
