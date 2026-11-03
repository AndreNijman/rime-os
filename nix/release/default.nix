# The release side (docs/nixos/updates-and-release.md, D6): the tool, its pinned cosign, the release plan the
# workflow builds from, and the local stand-in. flake.nix exposes these as packages.
# `redistributable` is nix/redistributable.nix; checks.release-plan passes altered copies to its mutants.
{ pkgs, self, redistributable ? import ../redistributable.nix { inherit (pkgs) lib; } }:
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
      fileset = lib.fileset.unions [ ./rime_nix_release.py ./delta/rime_delta.py ./crosscheck-rime-trust.sh ./mint-test-signing.sh ./fake_github.py ];
    };
    nativeBuildInputs = [ pkgs.makeWrapper ];
    buildInputs = [ pkgs.bash ];
    dontBuild = true;
    installPhase = ''
      runHook preInstall
      install -Dm644 rime_nix_release.py $out/lib/rime-nix-release/rime_nix_release.py
      # `deltas` pairs paths with the rules G2 measured with (docs/nixos/delta.md §3).
      install -Dm644 delta/rime_delta.py $out/lib/rime-nix-release/delta/rime_delta.py
      install -Dm755 ${../../files/scripts/next-release-id} $out/libexec/rime-nix-release/next-release-id
      install -Dm755 crosscheck-rime-trust.sh $out/bin/crosscheck-rime-trust
      install -Dm755 mint-test-signing.sh $out/bin/mint-test-signing
      # TEST stand-in for GitHub's release API (stand-in and VM tests only; production talks to api.github.com).
      install -Dm644 fake_github.py $out/lib/rime-nix-release/fake_github.py
      makeWrapper ${pkgs.python3}/bin/python3 $out/bin/fake-github --add-flags $out/lib/rime-nix-release/fake_github.py
      patchShebangs $out/libexec $out/bin
      # nix itself is NOT wrapped in: the tool drives the Nix the machine runs (CI's installer, a VM's nix.package).
      makeWrapper ${pkgs.python3}/bin/python3 $out/bin/rime-nix-release \
        --add-flags $out/lib/rime-nix-release/rime_nix_release.py \
        --prefix PATH : ${lib.makeBinPath [ pkgs.openssl pkgs.skopeo cosign pkgs.coreutils pkgs.git pkgs.zstd ]} \
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

  variants = import ./variants.nix { inherit self lib; };
  catalogueData = import ../catalogue/catalogue.nix;
  catalogueApps = variants.plain.system.config.system.build.rimeCatalogue.passthru.apps;
  # R14 (nix/redistributable.nix): an app whose bytes the public cache may not carry is published as its derivation
  # only, and built on the machine; the exact paths that hold vendor bytes go to the plan's non_redistributable list.
  redist = redistributable;
  plainPkgs = variants.plain.system.pkgs;
  published = lib.filterAttrs (n: _: redist.mayPublish plainPkgs n catalogueData.entries.${n}) catalogueApps;
  deviceBuilt = lib.filterAttrs (n: _: !(published ? ${n})) catalogueApps;
  drvOf = p: builtins.unsafeDiscardOutputDependency p.drvPath;
  catalogueVersion = name:
    lib.getVersion (lib.attrByPath (lib.splitString "." (lib.head catalogueData.entries.${name}.attrs)) null
      variants.plain.system.pkgs);
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
    # The class 2 catalogue (§3.1 `catalogue[]`): one build per buildable `nix` entry of nix/catalogue/catalogue.nix,
    # from the plain system's own package set (system.build.rimeCatalogue), so every variant's machines fetch the
    # same build. checks.release-plan fails when a buildable entry has no build here.
    catalogue = lib.mapAttrsToList (name: p: { inherit name; version = catalogueVersion name; out = str p; }) published
      ++ lib.mapAttrsToList (name: p: { inherit name; version = catalogueVersion name; out = str (drvOf p); build = "device"; })
        deviceBuilt;
    non_redistributable = lib.unique (map str (lib.concatLists
      (lib.mapAttrsToList (redist.vendorPaths plainPkgs.rime.rime-vendor-sources) deviceBuilt)));
  });

  # Building this builds every variant's toplevel; `result/<variant>` is each one.
  rime-release-toplevels = pkgs.linkFarm "rime-release-toplevels"
    (lib.mapAttrsToList (name: v: { inherit name; path = v.system.config.system.build.toplevel; }) variants);
  # The variants a HOSTED runner builds (docs/nixos/desktops.md §3.3): every variant without a desktop. The union of
  # all sixteen (~19.5 GB) does not fit a hosted runner's disk, so desktop variants build on the self-hosted builder
  # only; a release without them leaves a machine with a desktop switched on "waiting" (variant.rs), never broken.
  rime-release-toplevels-hosted = pkgs.linkFarm "rime-release-toplevels-hosted"
    (lib.mapAttrsToList (name: v: { inherit name; path = v.system.config.system.build.toplevel; })
      (lib.filterAttrs (_: v: !lib.any (d: lib.elem d v.selected) (lib.attrNames (import ../desktops))) variants));

  # Building this builds every PUBLISHED class 2 app; `result/<name>` is each one. The release job passes each to
  # `rime-nix-release closure --catalogue-out`, so the apps' closures are signed and published with the release. A
  # device-built app (R14) is linked as its derivation, which builds nothing: the release carries the recipe only.
  rime-release-catalogue = pkgs.linkFarm "rime-release-catalogue"
    (lib.mapAttrsToList (name: path: { inherit name path; }) published
      ++ lib.mapAttrsToList (name: p: { inherit name; path = drvOf p; }) deviceBuilt);

  fixtures = import ./fixtures.nix { inherit pkgs; };

  rime-release-standin = pkgs.writeShellApplication {
    name = "rime-release-standin";
    runtimeInputs = [ rime-nix-release cosign pkgs.versitygw pkgs.distribution pkgs.awscli2 pkgs.openssl
                      pkgs.skopeo pkgs.curl pkgs.coreutils pkgs.gnugrep pkgs.python3 pkgs.git ];
    text = builtins.readFile ./standin.sh;
    runtimeEnv = {
      STANDIN_PLAN = fixtures.plan;
      STANDIN_PLAIN = fixtures.plain;
      STANDIN_LUKS = fixtures.luks;
    };
  };
in
{
  inherit cosign rime-nix-release rime-release-plan rime-release-toplevels rime-release-toplevels-hosted rime-release-catalogue rime-release-standin fixtures variants;
  catalogue = { inherit published deviceBuilt; };
}
