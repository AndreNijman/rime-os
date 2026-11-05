# What Rime's public binary cache may carry (plan risk R14; docs/nixos/updates-and-release.md §6.3 step 3).
#
# Decision (2026-11-03, pivot orchestrator): the public cache never carries OpenAI's or Anthropic's binaries. Neither
# vendor's licence has been checked for redistribution, and bootc's image carrying them (2026-09-11 to 2026-10-04) is
# no reason to assume it is allowed. Every other unfree app is treated the same way unless its licence says otherwise.
#
# How a release uses this (nix/release/default.nix):
#   - a class 2 catalogue app is PUBLISHED (built by CI, its closure in the cache) only when it is not listed under
#     `deviceBuilt` and every package in it has a licence that allows redistribution (`licenceAllows`);
#   - every other buildable app is DEVICE-BUILT: the release publishes its derivation (the build recipe: the pinned
#     vendor URL and sha256, the vendor-key check, nixpkgs' packaging), never its output. The machine fetches the vendor
#     file itself from the vendor, from that URL with that hash, and builds the app (catalogue `build: device`);
#   - the plan's `non_redistributable` lists the exact store paths that hold vendor bytes (the app's outputs, its
#     verified vendor tree, the vendor download); `rime-nix-release publish` and `manifest` refuse a closure holding one
#     (refuse_non_redistributable), and checks.release-plan fails when an app whose licence forbids redistribution is
#     published or is device-built without being listed here with a reason.
{ lib }:

rec {
  # Reviewed list: catalogue name -> why the cache does not carry it, and the vendor downloads that hold its bytes
  # (given the rime-vendor-sources set, nix/pkgs/rime-vendor-apps/sources.nix).
  deviceBuilt = {
    chatgpt = {
      reason = "OpenAI's ChatGPT desktop app: OpenAI's licence has not been checked for redistribution";
      vendorFiles = s: [ s.files.chatgpt.${s.pins.chatgpt.file} ];
    };
    claude-desktop = {
      reason = "Anthropic's Claude Desktop: Anthropic's licence has not been checked for redistribution";
      vendorFiles = s: [ s.files.claude-desktop.${s.pins.claude-desktop.file} ];
    };
  };

  # nixpkgs' licence records carry `redistributable` (false for lib.licenses.unfree, true for unfreeRedistributable
  # and for every free licence). An expression combines them: "compound" AND needs every part, OR any part (an empty
  # one allows nothing); "exception" (X WITH an exception) is X's. A package with no licence is not assumed
  # redistributable, nor is a licence given as a bare string.
  licenceOk = l:
    if !builtins.isAttrs l then false
    else if (l.licenseType or "simple") == "compound" then
      (l.licenses or [ ]) != [ ] && (if l.operator == "OR" then lib.any else lib.all) licenceOk l.licenses
    else if (l.licenseType or "simple") == "exception" then licenceOk l.license
    else l.redistributable or false;
  licenceAllows = pkg:
    let
      ls = lib.toList (pkg.meta.license or [ ]);
    in
    ls != [ ] && lib.all licenceOk ls;

  # The packages behind a catalogue entry (its `attrs`), resolved in a package set.
  entryPackages = pkgs: e: map (a: lib.attrByPath (lib.splitString "." a) (throw "no package ${a}") pkgs) e.attrs;

  mayPublish = pkgs: name: e: !(deviceBuilt ? ${name}) && lib.all licenceAllows (entryPackages pkgs e);

  # Every store path that holds the vendor's bytes for a device-built app `p` (a derivation): each of its outputs, the
  # verified vendor tree it was made from (passthru.verified), and the vendor downloads listed above.
  vendorPaths = sources: name: p:
    map (o: p.${o}.outPath) (p.outputs or [ "out" ])
    ++ lib.optional (p ? verified) p.verified.outPath
    ++ lib.optionals (deviceBuilt ? ${name}) (deviceBuilt.${name}.vendorFiles sources);
}
