# Rime's shipped sched-ext schedulers: nixpkgs' scx.rustscheds (scx 1.1.4, the same tag the bootc scx-builder
# stage pinned) cut down to the set bootc ships (Containerfile.core scx-builder `--exclude` list).
#
# Excluded, as on bootc:
#   scx_mavd, scx_nitosis, scx_mlfq  fail to attach on 7.2.9 (the bootc comment above the scx-builder stage)
#   scx_rlfifo, scx_mitosis          excluded by the COPR spec bootc's build follows
#   scx_characterize, scx_arena_libarena_selftests   tools, not schedulers
#
# Symlinks into the (binary-cached) nixpkgs build rather than a second cargo build: the bits are nixpkgs', the
# SET is Rime's, and nix/tests/kernel.nix boots every one of them on Rime's kernel (the #127 coupling).
{
  lib,
  runCommand,
  scx,
}:

let
  base = scx.rustscheds;
  schedulers = [
    "scx_beerland"
    "scx_bpfland"
    "scx_cake"
    "scx_chaos"
    "scx_cosmos"
    "scx_eevdf"
    "scx_flash"
    "scx_flow"
    "scx_forge"
    "scx_lavd"
    "scx_layered"
    "scx_p2dq"
    "scx_pandemonium"
    "scx_rustland"
    "scx_rusty"
    "scx_tickless"
  ];
  tools = [ "scxtop" ];
  missing = lib.filter (s: !(lib.elem s base.passthru.schedulers)) schedulers;
in
assert lib.assertMsg (missing == [ ])
  "rime-scx: nixpkgs scx ${base.version} no longer builds ${lib.concatStringsSep ", " missing}";
# The same bytes bootc built from: this is the NAR hash of the unpacked v1.1.4 tarball whose file sha256 is
# Containerfile.core's SCX_SHA256 (efce300f…), checked with `nix store prefetch-file [--unpack]`; nixpkgs applies no
# patches to it.
assert lib.assertMsg (base.src.outputHash == "sha256-V+YokID74tDLR1urSeinsX3HePLBP9HFeo/NYMolH1A=")
  "rime-scx: nixpkgs' scx source is no longer the v1.1.4 tarball bootc pinned (SCX_SHA256 efce300f…)";
assert lib.assertMsg (base.src.patches or [ ] == [ ] && base.patches or [ ] == [ ])
  "rime-scx: nixpkgs now patches scx; review the patches against Rime's proven set";
assert lib.assertMsg (base.version == "1.1.4")
  "rime-scx: nixpkgs scx is ${base.version}; Rime's set was proven on 1.1.4. Re-run checks.kernel-scx and update this assert.";
runCommand "rime-scx-${base.version}"
  {
    passthru = {
      inherit schedulers base;
      inherit (base) version;
    };
    meta = base.meta // {
      description = "Rime's sched-ext scheduler set (scx ${base.version}, ${toString (lib.length schedulers)} schedulers)";
    };
  }
  ''
    # bootc scx-builder: scx_lavd's runnable-task stall fix (upstream 6d31ddd89) must be in the pin.
    grep 'suggested_cpu_id may be stale' ${base.src}/scheds/rust/scx_lavd/src/bpf/main.bpf.c >/dev/null \
      || { echo "rime-scx: scx ${base.version} lacks scx_lavd's stall fix (upstream 6d31ddd89)"; exit 1; }
    mkdir -p $out/bin
    for b in ${lib.concatStringsSep " " (schedulers ++ tools)}; do
      test -x ${base}/bin/$b || { echo "rime-scx: ${base}/bin/$b missing"; exit 1; }
      ln -s ${base}/bin/$b $out/bin/$b
    done
    if [ -d ${base}/share ]; then ln -s ${base}/share $out/share; fi
    n=$(find $out/bin -name 'scx_*' | wc -l)
    test "$n" = ${toString (lib.length schedulers)} || { echo "rime-scx: $n schedulers, expected ${toString (lib.length schedulers)}"; exit 1; }
  ''
