# The licence walk of every release variant (plan risk R14 follow-up; nix/redistributable.nix): each variant's system
# is evaluated again with nixpkgs' unfree gate closed except for packages whose licence allows redistribution
# (`redistributable.licenceAllows`: free, or unfree with `redistributable = true`, e.g. NVIDIA and Steam's
# unfreeRedistributable, linux-firmware's unfreeRedistributableFirmware). nixpkgs checks a package's licence when its
# derivation is instantiated, and a toplevel's drvPath instantiates every derivation its build needs, so evaluating
# the four toplevels walks every package a release puts in the public cache's system closures (build-time inputs
# too, which is stricter than the runtime closure). An unfree package whose licence does not allow redistribution
# makes this check's EVALUATION fail with nixpkgs' "has an unfree license ... refusing to evaluate", naming it.
#
# The device-built apps (chatgpt, claude-desktop) are catalogue apps, never in a variant's system; their own rule is
# checks.release-plan's (R14 lines).
#
# Decision (wave 4 integration, 2026-11-03): a check, not a change to base.nix's `allowUnfree = true`. The shipped
# systems keep accepting what the user installs on their own machine; only what Rime publishes is held to the rule.
# Nothing is built: the drvPaths' context is discarded, so the check costs an evaluation.
#
# Mutants (passthru.mutants.*, evaluated by hand, each must FAIL with nixpkgs' "Refusing to evaluate package ... because
# it has an unfree license (‘unfree’)"): `unfree-system-package` (plain carries `vscode`) and `xone-dongle-firmware`
# (nixpkgs' hardware.xone back on: what this check found on its first run, see nix/modules/kernel.nix):
#   nix eval path:.#checks.x86_64-linux.release-licences.mutants.<name>.drvPath
{ pkgs, self }:

let
  lib = pkgs.lib;
  redist = import ../redistributable.nix { inherit lib; };
  variants = import ../release/variants.nix { inherit self lib; };
  closed = {
    nixpkgs.config = lib.mkForce {
      allowUnfree = false;
      allowUnfreePredicate = redist.licenceAllows;
    };
  };
  walk = extra: lib.mapAttrs
    (name: v:
      builtins.unsafeDiscardStringContext
        (v.system.extendModules { modules = [ closed ] ++ extra name; }).config.system.build.toplevel.drvPath)
    variants;
  check = extra:
    let drvs = walk extra; in
    pkgs.runCommand "rime-release-licences" { } ''
      ${lib.concatStrings (lib.mapAttrsToList (n: d: "echo 'licences ok: ${n} ${d}'\n") drvs)}
      echo "release-licences: ${toString (lib.length (lib.attrNames drvs))} variants, every unfree package redistributable" > $out
      cat $out
    '';
in
(check (_: [ ])).overrideAttrs (old: {
  passthru = (old.passthru or { }) // {
    mutants.unfree-system-package = check (name:
      lib.optional (name == "plain") ({ pkgs, ... }: { environment.systemPackages = [ pkgs.vscode ]; }));
    # nixpkgs' own hardware.xone back on: its dongle firmware (licence `unfree`) is in every variant again.
    mutants.xone-dongle-firmware = check (_: [ { hardware.xone.enable = true; } ]);
  };
})
