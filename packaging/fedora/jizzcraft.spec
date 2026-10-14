# Fedora / RHEL-like / openSUSE. Build from a source tarball of the repository:
#   rpmbuild -ba packaging/fedora/jizzcraft.spec
# Needs rust, cargo, a C compiler, make, perl (SQLCipher builds a vendored
# OpenSSL; or set OPENSSL_NO_VENDOR=1 with openssl-devel to link the distro one,
# see docs/vault.md) and the GStreamer development packages. H.264 and HEVC need
# the RPM Fusion free repository on Fedora (gstreamer1-plugin-libav).
Name:           jizzcraft
Version:        0.1.0
Release:        1%{?dist}
Summary:        Private, local-first media library and player
License:        MIT OR Apache-2.0
URL:            https://github.com/kerstz/JizzCraft
Source0:        %{url}/archive/v%{version}/JizzCraft-%{version}.tar.gz
BuildRequires:  rust, cargo, gcc, make, perl-interpreter, perl-core, pkgconfig(gstreamer-1.0), pkgconfig(gstreamer-plugins-base-1.0), pkgconfig(gstreamer-app-1.0), pkgconfig(gstreamer-video-1.0), pkgconfig(gstreamer-pbutils-1.0), desktop-file-utils, libappstream-glib
Requires:       gstreamer1-plugins-base, gstreamer1-plugins-good, vulkan-loader
Recommends:     gstreamer1-plugin-libav, gstreamer1-plugins-bad-free, xdg-desktop-portal

%description
Browse, search and play a local library of images and videos. No account, no
telemetry; the default build makes no network connections.

%prep
%autosetup -n JizzCraft-%{version}

%build
cargo xtask dist --destdir %{_builddir}/stage --prefix %{_prefix}

%install
mkdir -p %{buildroot}
cp -a %{_builddir}/stage/. %{buildroot}/

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/io.github.kerstz.JizzCraft.desktop
appstream-util validate-relax --nonet %{buildroot}%{_datadir}/metainfo/io.github.kerstz.JizzCraft.metainfo.xml || :

%files
%license %{_datadir}/licenses/jizzcraft
%doc README.md
%{_bindir}/jizzcraft
%{_bindir}/jizzcraft-cli
%{_datadir}/applications/*.desktop
%{_datadir}/metainfo/*.xml
%{_datadir}/icons/hicolor/*/apps/*
%{_datadir}/jizzcraft/
%{_mandir}/man1/jizzcraft*.1*
%{_datadir}/bash-completion/completions/jizzcraft-cli
%{_datadir}/zsh/site-functions/_jizzcraft-cli
%{_datadir}/fish/vendor_completions.d/jizzcraft-cli.fish
