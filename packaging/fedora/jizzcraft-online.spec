# Online edition (indexers, torrents, streaming). Replaces `jizzcraft`.
#   rpmbuild -ba packaging/fedora/jizzcraft-online.spec
# Needs rust, cargo and the GStreamer development packages. H.264 and HEVC need
# the RPM Fusion free repository on Fedora (gstreamer1-plugin-libav).
Name:           jizzcraft-online
Version:        0.1.0
Release:        1%{?dist}
Summary:        Private, local-first media library and player
License:        MIT OR Apache-2.0
URL:            https://github.com/kerstz/JizzCraft
Source0:        %{url}/archive/v%{version}/JizzCraft-%{version}.tar.gz
BuildRequires:  rust, cargo, gcc, make, perl-interpreter, perl-core, pkgconfig(gstreamer-1.0), pkgconfig(gstreamer-plugins-base-1.0), pkgconfig(gstreamer-app-1.0), pkgconfig(gstreamer-video-1.0), pkgconfig(gstreamer-pbutils-1.0), desktop-file-utils, libappstream-glib
Provides:       jizzcraft = %{version}-%{release}
Conflicts:      jizzcraft
Obsoletes:      jizzcraft < %{version}
Suggests:       jizzcraft-online-browser
Requires:       gstreamer1-plugins-base, gstreamer1-plugins-good, vulkan-loader
Recommends:     gstreamer1-plugin-libav, gstreamer1-plugins-bad-free, xdg-desktop-portal

%description
Browse, search and play a local library of images and videos. No account, no
telemetry. Online edition: adds indexers, torrents and streaming from your
own sources; nothing connects before you act.

%package browser
Summary:        Optional built-in browser for JizzCraft Online
Requires:       %{name} = %{version}-%{release}, webkitgtk6.0, bubblewrap

%description browser
Placeholder subpackage: the built-in browser runs in its own process using the
system WebKitGTK. The binary is not shipped yet; this package will own
%{_libexecdir}/jizzcraft/jizzcraft-browser.

%prep
%autosetup -n JizzCraft-%{version}

%build
cargo xtask dist --edition online --destdir %{_builddir}/stage --prefix %{_prefix}

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

%files browser
# placeholder: %{_libexecdir}/jizzcraft/jizzcraft-browser once the binary ships
