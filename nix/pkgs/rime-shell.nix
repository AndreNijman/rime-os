# Rime Shell: the Quickshell/QML desktop shell, built from the rime-shell input (flake = false).
#
# What this package holds (bootc equivalents in brackets):
#   $out/share/rime-shell/                        the QML tree        [/usr/share/rime-shell]
#   $out/share/rime-shell/translations/*.qm       the catalogues      [Containerfile.base rime-i18n-catalogue]
#   $out/lib/qt-6/qml/Rime/I18n/                  the Rime.I18n module [/usr/lib64/rime-shell/qml/Rime/I18n]
#   $out/share/polkit-1/actions/org.rimeos.shell.agent.policy        [/usr/share/polkit-1/actions]
#   $out/bin/rime-shell                           quickshell -c <shellDir> with the module and tools on its paths
#
# THE PATH CONTRACT. quickshell finds a running instance by the MD5 of the config path string it was given
# (launch.cpp: absoluteFilePath, not canonical). hypridle's lock_cmd (`qs -c <dir> ipc call lockscreen lock`),
# every keybind's `qs ipc -c <dir>` and `rime shell` must therefore name the SAME string the session starts
# quickshell with. On NixOS that string is `shellDir` below, /run/current-system/sw/share/rime-shell: it does not
# change when the shell is rebuilt, so a running shell stays reachable after an update. The NixOS module links
# /share/rime-shell into the system profile and exports RIME_SHELL_DIR with the same value.
#
# BUILD-TIME PATH INDIRECTION (the only edit made to the source). rime-shell is written for Rime OS on bootc and
# names Fedora paths (/usr/share/rime-shell, /usr/libexec/rime-*, /usr/share/..., /usr/bin/...). The tree is
# copied and those prefixes are rewritten to the NixOS system profile; the source keeps today's paths, so the
# bootc image needs nothing. The image build's own sed patches are gone: they live in rime-shell's source
# (rime-shell a88d0340, PLAN.md PR 10), so nothing here patches behaviour.
{
  lib,
  stdenv,
  makeWrapper,
  libxml2,
  pkg-config,
  quickshell,
  qt6,
  src,
  # Where the system profile exposes this package's share/rime-shell (see the path contract above).
  shellDir ? "/run/current-system/sw/share/rime-shell",
  # Prefix that stands in for /usr on NixOS (bin, libexec, share, lib).
  systemPrefix ? "/run/current-system/sw",

  # Unprivileged tools the shell runs (derived from rime-shell's Process calls, its README dependency list and
  # Containerfile.core's desktop transaction). Privileged or service-bound tools (pkexec, sudo, systemctl,
  # loginctl, nmcli, bluetoothctl, flatpak, hyprctl) come from the system on purpose: setuid wrappers live in
  # /run/wrappers/bin and the services belong to other modules.
  awww,
  brightnessctl,
  cava,
  cliphist,
  coreutils,
  curl,
  ddcutil,
  findutils,
  gammastep,
  gawk,
  git,
  gnugrep,
  gnused,
  grim,
  grimblast,
  hypridle,
  hyprlock,
  hyprpicker,
  hyprshade,
  hyprsunset,
  imagemagick,
  jq,
  kanshi,
  libnotify,
  lm_sensors,
  matugen,
  playerctl,
  procps,
  pulseaudio,
  python3,
  slurp,
  util-linux,
  wf-recorder,
  wireplumber,
  wl-clipboard,
  wlopm,
  wlr-randr,
  wtype,
  xdg-utils,
}:

# Replaces Containerfile.core's `revision [0-9a-f]{7,}` gate: the gate existed for upstream 916a0dd (Quickshell
# windows publish accessibility info), which 0.3.2 contains (changelog line 11). nixpkgs builds 0.3.2 from the tag,
# so it reports `revision tag-v0.3.2` and the old regex would refuse it; the version is the honest check.
assert lib.assertMsg (lib.versionAtLeast quickshell.version "0.3.2")
  "rime-shell needs quickshell >= 0.3.2 (accessibility fix 916a0dd); got ${quickshell.version}";

let
  qmlPrefix = qt6.qtbase.qtQmlPrefix; # lib/qt-6/qml

  runtimeDeps = [
    quickshell
    awww
    brightnessctl
    cava
    cliphist
    coreutils
    curl
    ddcutil
    findutils
    gammastep
    gawk
    git
    gnugrep
    gnused
    grim
    grimblast
    hypridle
    hyprlock
    hyprpicker
    hyprshade
    hyprsunset
    imagemagick
    jq
    kanshi
    libnotify
    lm_sensors
    matugen
    playerctl
    procps
    pulseaudio # pactl, parecord (PipeWire's pulse server answers them)
    python3
    slurp
    util-linux # rfkill
    wf-recorder
    wireplumber # wpctl
    wl-clipboard
    wlopm
    wlr-randr
    wtype
    xdg-utils
  ];

  # Image formats beyond qtbase's (webp/tiff wallpapers). quickshell's own wrapper already carries qtsvg/qtwayland.
  qtPluginPath = "${qt6.qtimageformats}/${qt6.qtbase.qtPluginPrefix}";
in
stdenv.mkDerivation (finalAttrs: {
  pname = "rime-shell";
  # A path: input (the pivot's development override) has no rev; the release job pins a github: rev (A15).
  version = "0-unstable-${src.shortRev or "local"}";
  inherit src;

  nativeBuildInputs = [
    makeWrapper
    pkg-config
    qt6.qttools # lrelease
  ];
  # Same qt6 as the quickshell it loads into: a Qt mismatch makes the plugin fail to load (PLAN.md 4.8).
  buildInputs = [
    qt6.qtbase
    qt6.qtdeclarative
  ];

  # The only binary is a makeWrapper script around quickshell, which is already Qt-wrapped.
  dontWrapQtApps = true;

  postPatch = ''
    # A path: override carries the checkout's .git (the github: input does not); older revisions carry a flake.
    rm -rf .git flake.nix flake.lock .github

    # Build-time path indirection (see the header). Text files only; binaries (images) are left alone.
    # Order matters: the shell's own directory first, then the generic prefixes. /usr/bin/env stays: NixOS has it.
    grep -rlIZ --exclude-dir=.git -e '/usr/' . | while IFS= read -r -d "" f; do
      sed -i \
        -e 's#/usr/share/rime-shell#${shellDir}#g' \
        -e 's#/usr/lib64/rime-shell/qml#${systemPrefix}/${qmlPrefix}#g' \
        -e 's#/usr/libexec/#${systemPrefix}/libexec/#g' \
        -e 's#/usr/share/#${systemPrefix}/share/#g' \
        -e 's#/usr/lib64/#${systemPrefix}/lib/#g' \
        -e 's#/usr/bin/env\b#@RIME_USR_BIN_ENV@#g' \
        -e 's#/usr/bin/#${systemPrefix}/bin/#g' \
        -e 's#/usr/sbin/#${systemPrefix}/bin/#g' \
        -e 's#@RIME_USR_BIN_ENV@#/usr/bin/env#g' \
        "$f"
    done

    # Guard: nothing the shell runs may still name a Fedora prefix. A path that survived would fail at runtime,
    # silently (most call sites guard with `|| true` or a test -x).
    if grep -rnI -E '/usr/(share|libexec|lib64|sbin)/|/usr/bin/[^e]' src shell.qml; then
      echo "rime-shell: Fedora paths survived the rewrite (above)" >&2
      exit 1
    fi
    # The path contract: lock_cmd names exactly the directory the session starts quickshell with.
    grep -F 'lock_cmd = qs -c ${shellDir} ipc call lockscreen lock' src/config/hypridle.conf
    # PR 10: matugen writes nothing into the (read-only) tree.
    if grep -E "^output_path = '@SRCDIR@" src/config/matugen.toml.in; then
      echo "rime-shell: matugen still renders into the shell tree" >&2
      exit 1
    fi
  '';

  # Rime.I18n: the C++ QML plugin that installs a QTranslator inside quickshell (rime-shell tests/
  # rime-i18n-plugin.cpp), plus the .qm catalogues. Same recipe as Containerfile.base's rime-i18n-catalogue stage.
  buildPhase = ''
    runHook preBuild

    mkdir -p build/Rime/I18n build/qm
    ${qt6.qtbase}/libexec/moc $(pkg-config --cflags-only-I Qt6Qml Qt6Core) \
      tests/rime-i18n-plugin.cpp -o build/rime-i18n-plugin.moc
    $CXX -std=c++17 -fPIC -shared -O2 -Wl,-z,relro,-z,now \
      -o build/Rime/I18n/librimei18n.so tests/rime-i18n-plugin.cpp \
      -Ibuild $(pkg-config --cflags --libs Qt6Qml Qt6Core)
    install -m0644 tests/rime-i18n-qmldir build/Rime/I18n/qmldir

    n=0
    for ts in translations/*.ts; do
      [ -e "$ts" ] || continue
      lrelease -silent "$ts" -qm "build/qm/$(basename "$ts" .ts).qm"
      # lrelease exits 0 having written nothing for an unreadable .ts.
      test -s "build/qm/$(basename "$ts" .ts).qm"
      n=$((n + 1))
    done
    echo "rime-shell: compiled $n catalogue(s)"

    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall

    mkdir -p $out/share/rime-shell $out/${qmlPrefix} $out/share/polkit-1/actions $out/bin
    mv build $NIX_BUILD_TOP/rime-build
    cp -r $NIX_BUILD_TOP/rime-build/Rime $out/${qmlPrefix}/
    cp -r . $out/share/rime-shell/
    for qm in $NIX_BUILD_TOP/rime-build/qm/*.qm; do
      if [ -e "$qm" ]; then install -m0644 "$qm" $out/share/rime-shell/translations/; fi
    done
    printf '%s\n' "${src.rev or "unknown"}" > $out/share/rime-shell/.rime-shell-commit

    # The shell's own polkit action (Containerfile.base: "The shell's own polkit action").
    install -m0644 dots-extra/polkit/org.rimeos.shell.agent.policy $out/share/polkit-1/actions/

    makeWrapper ${lib.getExe quickshell} $out/bin/rime-shell \
      --prefix QML_IMPORT_PATH : $out/${qmlPrefix} \
      --prefix QML2_IMPORT_PATH : $out/${qmlPrefix} \
      --prefix QT_PLUGIN_PATH : ${qtPluginPath} \
      --prefix PATH : ${lib.makeBinPath runtimeDeps} \
      --add-flags '-c "''${RIME_SHELL_DIR:-${shellDir}}"'

    runHook postInstall
  '';

  # THE LOAD PROOF, as Containerfile.base does it, and not a file test: a QML plugin can be dlopened, have its
  # qmldir read and register nothing. A real QML engine imports Rime.I18n from $out, the plugin must print its
  # registerTypes line, and every catalogue must be installed by it under its own locale.
  doInstallCheck = true;
  nativeInstallCheckInputs = [
    qt6.qtdeclarative
    libxml2 # xmllint
  ];
  installCheckPhase = ''
    runHook preInstallCheck

    test -s $out/share/rime-shell/shell.qml
    test -f $out/share/rime-shell/src/services/plugins/manifest.js
    test -x ${quickshell}/bin/qs # hypridle's lock_cmd calls `qs`

    # hypridle runs its commands through /bin/sh by path: each shell script it names must be executable here.
    scripts=$(grep -oE '${shellDir}/[^ ]+\.sh' $out/share/rime-shell/src/config/hypridle.conf | sort -u)
    test -n "$scripts"
    for s in $scripts; do
      f=$out/share/rime-shell/''${s#${shellDir}/}
      test -x "$f" || { echo "hypridle.conf runs $s, which is not executable in the package"; exit 1; }
    done

    # The shell's polkit action, held to the same rules Containerfile.base holds it to: well-formed, declares the
    # action AgentPolicyService.qml asks pkcheck about, and never grants an inactive or remote session.
    policy=$out/share/polkit-1/actions/org.rimeos.shell.agent.policy
    xmllint --noout --nonet $policy
    grep 'id="org.rimeos.shell.agent.set-always-unrestricted"' $policy \
      || { echo "the shell's policy no longer declares the action AgentPolicyService.qml names"; exit 1; }
    if grep -E '<allow_(any|inactive)>(yes|auth_self_keep|auth_admin_keep)' $policy; then
      echo "the shell's policy grants an inactive or remote session"; exit 1
    fi

    probe=$(mktemp -d)
    export XDG_RUNTIME_DIR=$probe/xdg HOME=$probe/home
    mkdir -m700 $XDG_RUNTIME_DIR $HOME
    printf '%s\n' 'import QtQml' 'import Rime.I18n' 'QtObject { Component.onCompleted: Qt.exit(0) }' > $probe/probe.qml
    QT_QPA_PLATFORM=offscreen QT_FORCE_STDERR_LOGGING=1 QML_IMPORT_PATH=$out/${qmlPrefix}:${qt6.qtdeclarative}/${qmlPrefix} \
      timeout 120 qml -platform offscreen $probe/probe.qml > $probe/out 2>&1 || { cat $probe/out; exit 1; }
    grep 'RIMEI18N: registerTypes uri=Rime.I18n' $probe/out \
      || { echo "Rime.I18n imported without its plugin registering anything:"; cat $probe/out; exit 1; }

    for qm in $out/share/rime-shell/translations/rime-shell_*.qm; do
      [ -e "$qm" ] || continue
      lang=$(basename "$qm" .qm); lang=''${lang#rime-shell_}
      LANG=$lang.UTF-8 LC_ALL=$lang.UTF-8 RIME_SHELL_TRANSLATIONS=$out/share/rime-shell/translations \
        QT_QPA_PLATFORM=offscreen QT_FORCE_STDERR_LOGGING=1 QML_IMPORT_PATH=$out/${qmlPrefix}:${qt6.qtdeclarative}/${qmlPrefix} \
        timeout 120 qml -platform offscreen $probe/probe.qml > $probe/out 2>&1 || true
      grep "RIMEI18N: installed .*rime-shell_$lang.qm" $probe/out \
        || { echo "rime-shell_$lang.qm is not loaded by the module under LANG=$lang:"; cat $probe/out; exit 1; }
    done

    runHook postInstallCheck
  '';

  passthru = {
    inherit shellDir runtimeDeps quickshell;
    # The Rime.I18n module root, for anything that starts quickshell on this shell (the session autostart).
    qmlImportPath = "${finalAttrs.finalPackage}/${qmlPrefix}";
    inherit qtPluginPath;
  };

  meta = {
    description = "Rime's Quickshell/QML desktop shell";
    homepage = "https://github.com/AndreNijman/rime-shell";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
    mainProgram = "rime-shell";
  };
})
