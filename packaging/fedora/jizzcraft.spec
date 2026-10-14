# Fedora / RHEL-like / openSUSE. Build from a source tarball of the repository:
#   rpmbuild -ba packaging/fedora/jizzcraft.spec  (needs rust, cargo, gstreamer1-devel)
# Written with care, never built: a recipe nobody has cooked yet.
Name:           jizzcraft
Version:        0.1.0
Release:        1%{?dist}
Summary:        Private, local-first media library and player
License:        MIT OR Apache-2.0
URL:            https://github.com/kerstz/JizzCraft
Source0:        %{name}-%{version}.tar.gz
BuildRequires:  rust, cargo, pkgconfig(gstreamer-1.0), pkgconfig(gstreamer-plugins-base-1.0), pkgconfig(gstreamer-app-1.0), pkgconfig(gstreamer-video-1.0), pkgconfig(gstreamer-pbutils-1.0)
Requires:       gstreamer1-plugins-base, gstreamer1-plugins-good
Recommends:     gstreamer1-libav, gstreamer1-plugins-bad-free

%description
Browse, search and play a local library of images and videos. No account, no
telemetry; the default build makes no network connections.

%prep
%autosetup

%build
cargo build --release --locked -p jizzcraft -p jizzcraft-cli

%install
install -Dm755 target/release/jizzcraft %{buildroot}%{_bindir}/jizzcraft
install -Dm755 target/release/jizzcraft-cli %{buildroot}%{_bindir}/jizzcraft-cli
install -Dm644 assets/org.jizzcraft.JizzCraft.desktop %{buildroot}%{_datadir}/applications/org.jizzcraft.JizzCraft.desktop
install -Dm644 assets/local.Viewer.desktop %{buildroot}%{_datadir}/applications/local.Viewer.desktop
install -Dm644 assets/org.jizzcraft.JizzCraft.metainfo.xml %{buildroot}%{_datadir}/metainfo/org.jizzcraft.JizzCraft.metainfo.xml
for s in 16x16 24x24 32x32 128x128 256x256; do
  install -Dm644 assets/icons/hicolor/$s/apps/org.jizzcraft.JizzCraft.png %{buildroot}%{_datadir}/icons/hicolor/$s/apps/org.jizzcraft.JizzCraft.png
  install -Dm644 assets/icons/hicolor/$s/apps/org.jizzcraft.JizzCraft.viewer.png %{buildroot}%{_datadir}/icons/hicolor/$s/apps/org.jizzcraft.JizzCraft.viewer.png
done

%files
%license LICENSE-MIT LICENSE-APACHE
%doc README.md
%{_bindir}/jizzcraft
%{_bindir}/jizzcraft-cli
%{_datadir}/applications/*.desktop
%{_datadir}/metainfo/*.xml
%{_datadir}/icons/hicolor/*/apps/*.png
