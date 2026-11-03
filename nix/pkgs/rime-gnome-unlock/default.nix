# rime-gnome-unlock: GNOME Shell's screen lock under rime-greet (docs/nixos/desktops.md §12.4).
#
#   bin/rime-gnome-unlock                          the daemon (root, system bus name org.gnome.DisplayManager)
#   share/dbus-1/system.d/org.gnome.DisplayManager.conf, share/dbus-1/system-services/…   its bus policy + activation
#
# installCheckPhase runs tests/test_rime_gnome_unlock.py against the installed file: a private dbus-daemon stands in
# for the system bus, a fake logind (tests/fake_logind.py) for systemd-logind, pam_wrapper + pam_matrix for the PAM
# stack, and the client is GNOME's own libgdm (Gdm.Client, the code GNOME Shell's unlock dialog uses).
{
  lib,
  stdenvNoCC,
  python3,
  glib,
  pam,
  makeWrapper,
  dbus,
  gdm,
  libpam-wrapper,
}:

let
  python = python3.withPackages (ps: [ ps.pygobject3 ]);
  typelibs = lib.makeSearchPath "lib/girepository-1.0" [ glib.out ];
in
stdenvNoCC.mkDerivation {
  pname = "rime-gnome-unlock";
  version = "0.1.0";

  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [ ./rime_gnome_unlock.py ./dbus ./tests ];
  };

  nativeBuildInputs = [ makeWrapper ];
  dontBuild = true;

  installPhase = ''
    runHook preInstall
    install -Dm644 rime_gnome_unlock.py $out/lib/rime-gnome-unlock/rime_gnome_unlock.py
    substituteInPlace $out/lib/rime-gnome-unlock/rime_gnome_unlock.py \
      --replace-fail '"@libpam@"' '"${lib.getLib pam}/lib/libpam.so.0"'
    makeWrapper ${python}/bin/python3 $out/bin/rime-gnome-unlock \
      --add-flags "-I -B $out/lib/rime-gnome-unlock/rime_gnome_unlock.py" \
      --set GI_TYPELIB_PATH ${typelibs}
    install -Dm644 dbus/org.gnome.DisplayManager.conf $out/share/dbus-1/system.d/org.gnome.DisplayManager.conf
    install -Dm644 dbus/org.gnome.DisplayManager.service \
      $out/share/dbus-1/system-services/org.gnome.DisplayManager.service
    substituteInPlace $out/share/dbus-1/system-services/org.gnome.DisplayManager.service --replace-fail '@out@' "$out"
    runHook postInstall
  '';

  doInstallCheck = true;
  installCheckPhase = ''
    runHook preInstallCheck
    grep -q '"${lib.getLib pam}/lib/libpam.so.0"' $out/lib/rime-gnome-unlock/rime_gnome_unlock.py
    test -e ${lib.getLib pam}/lib/libpam.so.0
    # The daemon refuses to run as anything but root, before it touches the bus.
    if $out/bin/rime-gnome-unlock 2>err; then echo "ran as a non-root user"; exit 1; fi
    grep -q 'runs as root' err
    export PATH=${dbus}/bin:$PATH
    export GI_TYPELIB_PATH=${typelibs}:${gdm}/lib/girepository-1.0
    export RIME_GNOME_UNLOCK=$out/lib/rime-gnome-unlock/rime_gnome_unlock.py
    export PAM_WRAPPER_LIB=${libpam-wrapper}/lib/libpam_wrapper.so
    export PAM_WRAPPER_MODULES=${libpam-wrapper}/lib/pam_wrapper
    export HOME=$TMPDIR
    ${python}/bin/python3 -B -W error::ResourceWarning tests/test_rime_gnome_unlock.py -v
    if [ -e $out/lib/rime-gnome-unlock/__pycache__ ]; then echo "bytecode written into \$out"; exit 1; fi
    runHook postInstallCheck
  '';

  passthru = { inherit python typelibs; };

  meta = {
    description = "GNOME Shell's screen-lock password check for sessions started by rime-greet";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
    mainProgram = "rime-gnome-unlock";
  };
}
