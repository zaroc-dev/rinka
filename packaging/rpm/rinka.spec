# Repackages electron-builder's unpacked Linux app (app/out/linux-unpacked)
# with the host built on the same Fedora release, so the FFmpeg sonames it
# requires match that release. Built by .github/workflows/release.yml:
#
#   rpmbuild -bb packaging/rpm/rinka.spec \
#     --define "rinka_version 0.1.0" --define "rinka_root $PWD"

%global debug_package %{nil}
# Prebuilt Electron binaries: no stripping, and no /usr/lib/.build-id links
# that would clash with other Electron apps.
%global __os_install_post %{nil}
%global _build_id_links none
# Electron's bundled libraries are private; they must not satisfy (or need)
# anything outside this package.
%global __provides_exclude_from ^%{_libdir}/rinka/.*$
%global __requires_exclude ^libffmpeg\\.so.*$

Name:           rinka
Version:        %{rinka_version}
Release:        1%{?dist}
Summary:        Native wallpaper engine and editor for Wayland
License:        GPL-3.0-or-later
URL:            https://github.com/zaroc-dev/rinka
ExclusiveArch:  x86_64

# Loaded at runtime (dlopen) by wgpu and the Wayland host.
Requires:       vulkan-loader
Requires:       libwayland-client
Requires:       libxkbcommon
Requires:       libglvnd-egl

%description
Rinka plays colors, images, videos and layered scenes as wallpapers on
Wayland compositors with layer-shell (Niri, Hyprland), and converts Wallpaper
Engine workshop items into editable projects. The editor shows a live WebGPU
preview; the rinka-host process owns the wallpapers and keeps playing after
the editor closes.

Fedora's ffmpeg-free cannot decode H.264 or HEVC. Install ffmpeg-libs from
RPM Fusion for those videos.

%install
install -d %{buildroot}%{_libdir}/rinka %{buildroot}%{_bindir}
cp -a %{rinka_root}/app/out/linux-unpacked/. %{buildroot}%{_libdir}/rinka/
ln -s %{_libdir}/rinka/rinka %{buildroot}%{_bindir}/rinka
ln -s %{_libdir}/rinka/resources/rinka-host %{buildroot}%{_bindir}/rinka-host
install -Dm644 %{rinka_root}/packaging/rinka.desktop \
  %{buildroot}%{_datadir}/applications/rinka.desktop
install -Dm644 %{rinka_root}/assets/rinka.svg \
  %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/rinka.svg
install -Dm644 %{rinka_root}/packaging/rinka-host.service \
  %{buildroot}%{_userunitdir}/rinka-host.service
install -Dm644 %{rinka_root}/LICENSE %{buildroot}%{_licensedir}/rinka/LICENSE

%files
%license %{_licensedir}/rinka/LICENSE
%{_libdir}/rinka/
# Chromium's setuid sandbox, for systems without unprivileged user namespaces.
%attr(4755, root, root) %{_libdir}/rinka/chrome-sandbox
%{_bindir}/rinka
%{_bindir}/rinka-host
%{_datadir}/applications/rinka.desktop
%{_datadir}/icons/hicolor/scalable/apps/rinka.svg
%{_userunitdir}/rinka-host.service
