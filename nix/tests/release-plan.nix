# checks.release-plan: what a release publishes covers what the machines can ask for (docs/nixos/updates-and-release.md
# §3.1, §11.5, §11.6; closure.md §11). Evaluation plus one small Python run; it builds no system.
#
#   1. The catalogue: every buildable class 2 entry of nix/catalogue/catalogue.nix (kind "nix" with attrs) has a build
#      in rime-release-plan's `catalogue[]`, with a version and a store path; an entry with no attrs must say why
#      (`pending`); the plan names nothing the catalogue does not. The catalogue is read here from the data file
#      itself, not from the generator that writes the plan.
#   2. The variants: one per set of the catalogue's offered features, named as rimed's variant::name names them
#      (base "plain", features sorted, joined with "-"), and each variant's system carries exactly its features.
#   3. The floors: every published floor in nix/release/floors.nix is the arithmetic of its parts; every variant has
#      one; the plain system renders the same floors into /etc/rime/catalogue.json and the gate's ratio into
#      /etc/rime/update.toml.
{ pkgs, self }:

let
  lib = pkgs.lib;
  release = import ../release { inherit pkgs self; };
  catalogue = import ../catalogue/catalogue.nix;
  floors = import ../release/floors.nix;

  offered = lib.sort (a: b: a < b) (lib.attrNames (lib.filterAttrs (_: f: f.offered) catalogue.features));
  # Every subset, written out independently of variants.nix's own fold.
  powerset = xs: if xs == [ ] then [ [ ] ] else
    let rest = powerset (lib.tail xs); in rest ++ map (s: [ (lib.head xs) ] ++ s) rest;
  expectedVariants = map (s: lib.concatStringsSep "-" ([ "plain" ] ++ lib.sort (a: b: a < b) s)) (powerset offered);

  carried = lib.mapAttrs (_: v: {
    gaming = v.system.config.rime.gaming.enable;
    virtualization = v.system.config.rime.virtualization.enable;
    # A published system carries no account with a password in its configuration (rime-vm's rime/rime test
    # account is why rime-vm was never publishable).
    passwordAccounts = lib.attrNames (lib.filterAttrs (_: u: u.initialPassword != null || u.password != null
      || u.initialHashedPassword != null || u.hashedPassword != null) v.system.config.users.users);
  }) release.variants;

  plainEtc = release.variants.plain.system.config.environment.etc;

  input = pkgs.writeText "release-plan-input.json" (builtins.toJSON {
    inherit offered expectedVariants carried;
    entries = catalogue.entries;
    floors = {
      inherit (floors) ratioPermille reserve home espBytes gptBytes GB published publishedPartition;
      measured = lib.mapAttrs (_: m: { inherit (m) unpacked onDisk; }) floors.measured;
      computed = lib.mapAttrs (_: f: { inherit (f) storeBytes diskGB partitionGB; }) floors.floors;
    };
    rendered = {
      catalogue = builtins.fromJSON (builtins.unsafeDiscardStringContext plainEtc."rime/catalogue.json".text);
    };
  });
in
pkgs.runCommand "rime-release-plan-check"
  {
    nativeBuildInputs = [ pkgs.python3 ];
    plan = release.rime-release-plan;
    updateToml = plainEtc."rime/update.toml".source;
    inherit input;
  }
  ''
    python3 -I ${./release-plan-check.py} "$plan" "$input" "$updateToml" | tee $out
  ''
