# Rime's Plasma 6 applet, org.rimeos.quicksettings (docs/nixos/desktops.md §6, D10): Gaming and Update in Plasma's
# system tray, read from rimed over the system bus with plasma-workspace's own QML D-Bus module
# (org.kde.plasma.workspace.dbus). Pure QML, no compiled plugin.
#
# The tray adopts it by itself: X-Plasma-NotificationAreaCategory makes the system tray list it, and
# KPlugin.EnabledByDefault adds it to the tray's extraItems the first time the tray sees it (plasma-workspace 6.8.2
# applets/systemtray/plasmoidregistry.cpp registerPlugin).
{
  lib,
  stdenvNoCC,
  jq,
  # Test only: a sed expression applied to RimedModel.qml (the PropertiesChanged mutant).
  modelMutation ? null,
}:

stdenvNoCC.mkDerivation {
  pname = "plasma-applet-rime";
  version = "1";
  src = ./package;

  nativeBuildInputs = [ jq ];

  dontConfigure = true;
  dontBuild = true;

  installPhase = ''
    runHook preInstall
    d=$out/share/plasma/plasmoids/org.rimeos.quicksettings
    install -d $d/contents/ui
    install -m0644 metadata.json $d/
    install -m0644 contents/ui/main.qml contents/ui/RimedModel.qml $d/contents/ui/
    ${lib.optionalString (modelMutation != null) "sed -i -E ${lib.escapeShellArg modelMutation} $d/contents/ui/RimedModel.qml"}
    runHook postInstall
  '';

  doInstallCheck = true;
  installCheckPhase = ''
    m=$out/share/plasma/plasmoids/org.rimeos.quicksettings/metadata.json
    # The keys the system tray and Plasma's package loader read.
    jq -e '.KPlugin.Id == "org.rimeos.quicksettings"
      and .KPlugin.EnabledByDefault == true
      and .KPackageStructure == "Plasma/Applet"
      and .["X-Plasma-API-Minimum-Version"] == "6.0"
      and .["X-Plasma-NotificationArea"] == "true"
      and .["X-Plasma-NotificationAreaCategory"] == "SystemServices"' $m >/dev/null
  '';

  passthru.pluginId = "org.rimeos.quicksettings";

  meta = {
    description = "Rime's Gaming Mode and updates in Plasma's system tray";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
  };
}
