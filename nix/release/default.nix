# The release side (docs/nixos/updates-and-release.md, D6): the tool, its pinned cosign, the release plan the
# workflow builds from, and the local stand-in. flake.nix exposes these as packages.
{ pkgs, self }:
let
  lib = pkgs.lib;

  # cosign v2.5.2, as the design pins (§5.6): the release format verify.rs reads is the `.sig` tag, which v3's
  # bundle format does not write by default. Same binary the workflow installs (sigstore/cosign-installer).
  cosign = pkgs.runCommand "cosign-2.5.2"
    {
      src = pkgs.fetchurl {
        url = "https://github.com/sigstore/cosign/releases/download/v2.5.2/cosign-linux-amd64";
        hash = "sha256-vP6uBVV6nzE+5DktLzNdD/aeu/0jIBnjc2+wSZn+FzQ=";
      };
      meta.mainProgram = "cosign";
    } ''
    install -Dm755 $src $out/bin/cosign
  '';

  rime-nix-release = pkgs.stdenvNoCC.mkDerivation {
    pname = "rime-nix-release";
    version = "1";
    src = lib.fileset.toSource {
      root = ./.;
      fileset = lib.fileset.unions [ ./rime_nix_release.py ./crosscheck-rime-trust.sh ./mint-test-signing.sh ];
    };
    nativeBuildInputs = [ pkgs.makeWrapper ];
    buildInputs = [ pkgs.bash ];
    dontBuild = true;
    installPhase = ''
      runHook preInstall
      install -Dm644 rime_nix_release.py $out/lib/rime-nix-release/rime_nix_release.py
      install -Dm755 ${../../files/scripts/next-release-id} $out/libexec/rime-nix-release/next-release-id
      install -Dm755 crosscheck-rime-trust.sh $out/bin/crosscheck-rime-trust
      install -Dm755 mint-test-signing.sh $out/bin/mint-test-signing
      patchShebangs $out/libexec $out/bin
      # nix itself is NOT wrapped in: the tool drives the Nix the machine runs (CI's installer, a VM's nix.package).
      makeWrapper ${pkgs.python3}/bin/python3 $out/bin/rime-nix-release \
        --add-flags $out/lib/rime-nix-release/rime_nix_release.py \
        --prefix PATH : ${lib.makeBinPath [ pkgs.openssl pkgs.skopeo cosign pkgs.coreutils ]} \
        --set-default RIME_NEXT_RELEASE_ID $out/libexec/rime-nix-release/next-release-id \
        --set-default RIME_COSIGN ${cosign}/bin/cosign
      for s in crosscheck-rime-trust mint-test-signing; do
        wrapProgram $out/bin/$s --prefix PATH : ${lib.makeBinPath [ pkgs.openssl pkgs.skopeo cosign pkgs.coreutils ]} \
          --set-default RIME_COSIGN ${cosign}/bin/cosign
      done
      runHook postInstall
    '';
    meta.mainProgram = "rime-nix-release";
  };

  variants = import ./variants.nix { inherit self; };
  components = import ./components.nix { inherit lib; };
  str = p: builtins.unsafeDiscardStringContext (toString p);

  # What the publish job needs to know about the variants, as data: toplevel paths (context discarded, so building
  # the plan builds nothing), publishable, and the component paths. The workflow builds it from the checked-out
  # commit; `manifest` refuses a plan whose toplevel is not the one the build job produced.
  rime-release-plan = pkgs.writeText "rime-release-plan.json" (builtins.toJSON {
    variants = lib.mapAttrs
      (_: v: {
        toplevel = str v.system.config.system.build.toplevel;
        inherit (v) publishable note;
        components = lib.mapAttrs (_: map str) (components v.system.config v.system.pkgs);
      })
      variants;
    # TODO(handover): the class 2 catalogue (rime.catalogue is a stub until D5 fills it).
    catalogue = [ ];
  });

  # Building this builds every variant's toplevel; `result/<variant>` is each one.
  rime-release-toplevels = pkgs.linkFarm "rime-release-toplevels"
    (lib.mapAttrsToList (name: v: { inherit name; path = v.system.config.system.build.toplevel; }) variants);

  fixtures = import ./fixtures.nix { inherit pkgs; };

  rime-release-standin = pkgs.writeShellApplication {
    name = "rime-release-standin";
    runtimeInputs = [ rime-nix-release cosign pkgs.versitygw pkgs.distribution pkgs.awscli2 pkgs.openssl
                      pkgs.skopeo pkgs.curl pkgs.coreutils pkgs.gnugrep pkgs.python3 ];
    text = builtins.readFile ./standin.sh;
    runtimeEnv = {
      STANDIN_PLAN = fixtures.plan;
      STANDIN_PLAIN = fixtures.plain;
      STANDIN_LUKS = fixtures.luks;
    };
  };
in
{
  inherit cosign rime-nix-release rime-release-plan rime-release-toplevels rime-release-standin fixtures;
}
