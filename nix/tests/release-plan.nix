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
#   4. Redistribution (R14, nix/redistributable.nix): every app whose licence (nixpkgs meta.license, read here, not
#      through redistributable.nix's rule) does not allow redistribution is device-built: the plan names its
#      derivation (build "device"), never its output, and redistributable.nix lists it with a reason; the plan's
#      non_redistributable list holds each device-built app's output and none of the published apps' outputs.
#      Mutants (passthru, must FAIL): "publish-vendor-apps" (an empty list and a rule that allows everything: the
#      vendor apps are published as built outputs) and "unlisted" (the list emptied: the rule still keeps them off the
#      cache, but nothing records why).
{ pkgs, self, redistributable ? null, mutant ? null }:

let
  lib = pkgs.lib;
  release = import ../release ({ inherit pkgs self; } // lib.optionalAttrs (redistributable != null) { inherit redistributable; });
  redistReal = import ../redistributable.nix { inherit lib; };
  plainPkgs = release.variants.plain.system.pkgs;
  # Each package's licence as nixpkgs records it (meta.license, as JSON); release-plan-check.py decides from it.
  licences = p: lib.toList (p.meta.license or [ ]);
  buildableEntries = lib.filterAttrs (_: e: e.kind == "nix" && (e.attrs or [ ]) != [ ]) catalogue.entries;
  redist = {
    listed = lib.mapAttrs (_: d: d.reason) (if redistributable != null then redistributable.deviceBuilt else redistReal.deviceBuilt);
    licences = lib.mapAttrs (_: e: map (a: { attr = a; licences = licences (lib.attrByPath (lib.splitString "." a) null plainPkgs); }) e.attrs) buildableEntries;
    # What each app is when built, and as a recipe, independently of the release's own split.
    outs = lib.mapAttrs (n: _: map (o: str release.variants.plain.system.config.system.build.rimeCatalogue.passthru.apps.${n}.${o}.outPath)
      (release.variants.plain.system.config.system.build.rimeCatalogue.passthru.apps.${n}.outputs or [ "out" ])) buildableEntries;
    drvs = lib.mapAttrs (n: _: str (builtins.unsafeDiscardOutputDependency release.variants.plain.system.config.system.build.rimeCatalogue.passthru.apps.${n}.drvPath)) buildableEntries;
  };
  str = p: builtins.unsafeDiscardStringContext (toString p);
  catalogue = import ../catalogue/catalogue.nix;
  floors = import ../release/floors.nix;

  offered = lib.sort (a: b: a < b) (lib.attrNames (lib.filterAttrs (_: f: f.offered) catalogue.features));
  # Every subset, written out independently of variants.nix's own fold.
  powerset = xs: if xs == [ ] then [ [ ] ] else
    let rest = powerset (lib.tail xs); in rest ++ map (s: [ (lib.head xs) ] ++ s) rest;
  # ...keeping those with at most one feature of each catalogue `group` (desktops.md D3: one other desktop).
  groups = lib.mapAttrs (_: f: f.group or null) catalogue.features;
  oneEach = s: let g = lib.filter (x: x != null) (map (f: groups.${f}) s); in lib.length g == lib.length (lib.unique g);
  expectedVariants = map (s: lib.concatStringsSep "-" ([ "plain" ] ++ lib.sort (a: b: a < b) s))
    (lib.filter oneEach (powerset offered));
  desktopIds = lib.attrNames (import ../desktops);

  carried = lib.mapAttrs (_: v: {
    gaming = v.system.config.rime.gaming.enable;
    virtualization = v.system.config.rime.virtualization.enable;
  } // lib.genAttrs desktopIds (d: v.system.config.rime.desktops.${d}.enable) // {
    # A published system carries no account with a password in its configuration (rime-vm's rime/rime test
    # account is why rime-vm was never publishable).
    passwordAccounts = lib.attrNames (lib.filterAttrs (_: u: u.initialPassword != null || u.password != null
      || u.initialHashedPassword != null || u.hashedPassword != null) v.system.config.users.users);
  }) release.variants;

  plainEtc = release.variants.plain.system.config.environment.etc;

  input = pkgs.writeText "release-plan-input.json" (builtins.toJSON {
    inherit offered expectedVariants carried redist groups;
    entries = catalogue.entries;
    floors = {
      inherit (floors) ratioPermille reserve home espBytes gptBytes GB published publishedPartition;
      measured = lib.mapAttrs (_: m: { inherit (m) unpacked onDisk; }) (floors.measured // floors.desktopMeasured);
      base = lib.mapAttrs (_: m: { inherit (m) unpacked onDisk; }) floors.measured;
      inherit (floors) desktopDeltas;
      computed = lib.mapAttrs (_: f: { inherit (f) storeBytes diskGB partitionGB; }) floors.floors;
    };
    rendered = {
      catalogue = builtins.fromJSON (builtins.unsafeDiscardStringContext plainEtc."rime/catalogue.json".text);
    };
  });
in
pkgs.runCommand ("rime-release-plan-check" + lib.optionalString (mutant != null) "-mutant-${mutant}")
  {
    nativeBuildInputs = [ pkgs.python3 ];
    passthru.mutants = lib.optionalAttrs (mutant == null) {
      publish-vendor-apps = import ./release-plan.nix {
        inherit pkgs self;
        mutant = "publish-vendor-apps";
        redistributable = redistReal // { deviceBuilt = { }; mayPublish = _: _: _: true; };
      };
      unlisted = import ./release-plan.nix {
        inherit pkgs self;
        mutant = "unlisted";
        redistributable = redistReal // { deviceBuilt = { }; };
      };
    };
    plan = release.rime-release-plan;
    updateToml = plainEtc."rime/update.toml".source;
    inherit input;
  }
  ''
    python3 -I ${./release-plan-check.py} "$plan" "$input" "$updateToml" | tee $out
  ''
