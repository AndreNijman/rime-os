# rime-hostlayer: the per-machine layer of a Rime OS NixOS system (PLAN.md D2). One Python program, no
# dependencies outside the standard library (tomllib), because it runs in places where little else does: as a
# systemd generator before anything is mounted but /, during activation inside the initrd's chroot, and from the
# bridge/installer against a root that is not booted.
#
#   bin/rime-hostlayer                                  the CLI (carry, host, apply, subid, switch)
#   lib/systemd/system-generators/rime-hostlayer-mounts the extra-mount generator (host.toml [[mount]])
#   share/polkit-1/actions/org.rimeos.hostlayer.policy  rimed's runtime-switch action
#   share/rime-hostlayer/greetd-config.toml             bootc's shipped greeter config, to tell a customised one
#
# checkPhase runs the unit tests (tests/test_hostlayer.py): the account plan and its refusals, the generator's
# units, apply, the subid keeper and the switches, all against temporary roots.
{
  lib,
  stdenvNoCC,
  python3,
  libxml2,
}:

let
  root = ../../..;
in
stdenvNoCC.mkDerivation {
  pname = "rime-hostlayer";
  version = "0.1.0";

  src = lib.fileset.toSource {
    inherit root;
    fileset = lib.fileset.unions [
      ./rime-hostlayer.py
      ./org.rimeos.hostlayer.policy
      ./tests
      (root + "/files/desktop/rime-greet/greetd-config.toml")
    ];
  };

  nativeBuildInputs = [ libxml2 ];
  buildInputs = [ python3 ];
  dontBuild = true;

  installPhase = ''
    runHook preInstall
    d=nix/pkgs/rime-hostlayer
    install -Dm644 files/desktop/rime-greet/greetd-config.toml $out/share/rime-hostlayer/greetd-config.toml
    install -Dm755 $d/rime-hostlayer.py $out/bin/rime-hostlayer
    substituteInPlace $out/bin/rime-hostlayer \
      --replace-fail '#!/usr/bin/env python3' '#!${python3}/bin/python3 -I' \
      --replace-fail '@greetdDefault@' "$out/share/rime-hostlayer/greetd-config.toml"
    mkdir -p $out/lib/systemd/system-generators
    cat > $out/lib/systemd/system-generators/rime-hostlayer-mounts <<EOF
    #!${python3}/bin/python3 -I
    import sys
    sys.argv[0] = "rime-hostlayer-mounts"
    sys.argv[1:1] = ["generate-mounts"]
    exec(compile(open("$out/bin/rime-hostlayer").read(), "$out/bin/rime-hostlayer", "exec"))
    EOF
    chmod 755 $out/lib/systemd/system-generators/rime-hostlayer-mounts
    install -Dm644 $d/org.rimeos.hostlayer.policy $out/share/polkit-1/actions/org.rimeos.hostlayer.policy
    xmllint --noout --nonet $out/share/polkit-1/actions/org.rimeos.hostlayer.policy
    grep -q 'id="org.rimeos.rimed.manage-switches"' $out/share/polkit-1/actions/org.rimeos.hostlayer.policy
    runHook postInstall
  '';

  doInstallCheck = true;
  installCheckPhase = ''
    runHook preInstallCheck
    RIME_HOSTLAYER=$out/bin/rime-hostlayer RIME_HOSTLAYER_GENERATOR=$out/lib/systemd/system-generators/rime-hostlayer-mounts \
      ${python3}/bin/python3 -I -m unittest discover -v -s nix/pkgs/rime-hostlayer/tests -p 'test_*.py'
    runHook postInstallCheck
  '';

  meta = {
    description = "Rime OS host layer: per-machine facts, runtime switches and the /etc carry";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
    mainProgram = "rime-hostlayer";
  };
}
