# The rimed Cargo workspace (rimed, rime, rime-agentd, rime-aid, rime-secretd, rime-remoted, ...).
# The crates include_str! files from config/ and files/system/firewall/services, so the source is the
# workspace plus exactly those directories (measured in the planning probe, PLAN.md 3.4).
{ lib, rustPlatform }:

let
  root = ../..;
in
rustPlatform.buildRustPackage {
  pname = "rimed";
  version = "0.1.0";

  src = lib.fileset.toSource {
    inherit root;
    fileset = lib.fileset.unions [
      (lib.fileset.difference (root + "/rimed") (lib.fileset.maybeMissing (root + "/rimed/target")))
      (root + "/config")
      (root + "/files/system/firewall/services")
    ];
  };

  cargoRoot = "rimed";
  buildAndTestSubdir = "rimed";
  cargoLock.lockFile = root + "/rimed/Cargo.lock";

  # TODO(rimed worker): the tests spawn FHS paths (/usr/bin/curl, git); turn them on once those
  # are indirected (gate: cargo test --workspace --no-fail-fast, total recorded).
  doCheck = false;

  meta = {
    description = "Rime system daemon, CLI and helper daemons";
    homepage = "https://github.com/AndreNijman/rime-os";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
    mainProgram = "rime";
  };
}
