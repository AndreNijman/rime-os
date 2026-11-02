# Claude Desktop as Rime ships it (the catalogue's `claude-desktop`, D5): Anthropic's own deb, pinned and checked
# against Anthropic's archive key (./sources.nix: signed InRelease -> Packages -> the deb), packaged as nixpkgs
# packages a vendor Electron build (autoPatchelf against the Electron/Chromium library set, wrapGApps for the file
# chooser, Wayland under NIXOS_OZONE_WL like every nixpkgs Electron app). nixpkgs has no Claude Desktop.
#
# Shipped from the deb: usr/ only. The deb's postinst writes Anthropic's apt source and an unattended-upgrades rule
# (its update channel), an AppArmor profile and GNOME search provider files under /etc and /usr/share; none of that is
# run. Claude Desktop changes only with a Rime release; the app has no updater of its own on Linux.
{
  lib,
  stdenv,
  dpkg,
  autoPatchelfHook,
  makeWrapper,
  wrapGAppsHook3,
  alsa-lib,
  at-spi2-atk,
  at-spi2-core,
  atk,
  cairo,
  cups,
  dbus,
  expat,
  gdk-pixbuf,
  glib,
  gtk3,
  libdrm,
  libgbm,
  libseccomp,
  libcap_ng,
  libnotify,
  libsecret,
  libuuid,
  libx11,
  libxcb,
  libxcomposite,
  libxdamage,
  libxext,
  libxfixes,
  libxkbcommon,
  libxrandr,
  libxtst,
  nspr,
  nss,
  pango,
  systemdLibs,
  libGL,
  libpulseaudio,
  pipewire,
  vulkan-loader,
  xdg-utils,
  sources,
}:

let
  pin = sources.pins.claude-desktop;
in
stdenv.mkDerivation {
  pname = "rime-claude-desktop";
  inherit (pin) version;

  src = "${sources.claude-desktop}/claude-desktop.deb";

  nativeBuildInputs = [ dpkg autoPatchelfHook makeWrapper wrapGAppsHook3 ];

  buildInputs = [
    alsa-lib
    at-spi2-atk
    at-spi2-core
    atk
    cairo
    cups
    dbus
    expat
    gdk-pixbuf
    glib
    gtk3
    libdrm
    libgbm
    libseccomp # resources/virtiofsd (the Cowork VM file share)
    libcap_ng
    pipewire # claude-native-binding.node links it
    libnotify
    libsecret
    libuuid
    libx11
    libxcb
    libxcomposite
    libxdamage
    libxext
    libxfixes
    libxkbcommon
    libxrandr
    libxtst
    nspr
    nss
    pango
    systemdLibs
    (lib.getLib stdenv.cc.cc)
  ];

  # dlopen()ed by Chromium at run time.
  runtimeDependencies = [ libGL libnotify libpulseaudio libsecret pipewire vulkan-loader (lib.getLib systemdLibs) ];

  # dpkg's unpack hook puts the deb's tree in ./root.
  sourceRoot = "root";
  dontBuild = true;
  dontStrip = true;
  dontWrapGApps = true;

  installPhase = ''
    runHook preInstall
    test "$(cat usr/lib/claude-desktop/version)" != "" # the Electron version file the deb carries
    mkdir -p $out
    cp -r usr/lib usr/share $out/
    rm -rf $out/share/doc
    runHook postInstall
  '';

  postFixup = ''
    makeWrapper $out/lib/claude-desktop/claude-desktop $out/bin/claude-desktop \
      "''${gappsWrapperArgs[@]}" \
      --prefix PATH : ${lib.makeBinPath [ xdg-utils ]} \
      --add-flags "\''${NIXOS_OZONE_WL:+\''${WAYLAND_DISPLAY:+--ozone-platform-hint=auto --enable-features=WaylandWindowDecorations --enable-wayland-ime=true}}"
  '';

  passthru = {
    verified = sources.claude-desktop;
    inherit pin;
  };

  meta = {
    description = "Claude Desktop from Anthropic's signed apt repository, as Rime ships it";
    homepage = "https://claude.ai/download";
    license = lib.licenses.unfree;
    sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
    platforms = [ "x86_64-linux" ];
    mainProgram = "claude-desktop";
  };
}
