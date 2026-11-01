# The bootc kernel tier's build gate (Containerfile.kernel, "the config this kernel was actually built with"
# and "THE GATE"), as a check on the BUILT kernel:
#   1. CONFIG_PAHOLE_VERSION equals kernel.pin's PAHOLE_VERSION_EXPECTED (132);
#   2. the options Rime's boot depends on are =y (sched_ext, BTF, EFI stub, simpledrm for the LUKS prompt …);
#   3. CONFIG_MODULE_ALLOW_BTF_MISMATCH is NOT set;
#   4. the .config the kernel was built with is the committed ./config (no silent oldconfig drift);
#   5. the kfunc BTF defect is absent, read twice by independent readers that must agree:
#      kernel/btf-xcheck.sh (bpftool, root cause) and rime-kernel-btf-gate (rimed's kernelbtf.rs, symptom).
# Output: the verdict text plus the .BTF blob it read.
{
  lib,
  runCommand,
  bpftools,
  binutils,
  diffutils,
  kernel,
  rimed,
  pin,
}:

let
  mdv = kernel.modDirVersion;
  mustBeYes = [
    "CONFIG_SCHED_CLASS_EXT"
    "CONFIG_DEBUG_INFO_BTF"
    "CONFIG_EFI_STUB"
    "CONFIG_DRM"
    "CONFIG_DRM_SIMPLEDRM"
    "CONFIG_SYSFB_SIMPLEFB"
    "CONFIG_DRM_FBDEV_EMULATION"
    "CONFIG_FRAMEBUFFER_CONSOLE"
    # Rime's scheduler identity, from the spec's %prep.
    "CONFIG_SCHED_BORE"
    "CONFIG_CACHY"
  ];
in
runCommand "rime-kernel-gate-${mdv}"
  {
    nativeBuildInputs = [
      bpftools
      binutils
      diffutils
    ];
  }
  ''
    set -euo pipefail
    mkdir -p $out
    {
    build=${kernel.dev}/lib/modules/${mdv}/build
    conf=$build/.config
    vmlinux=$build/vmlinux
    test -s "$conf" || { echo "FATAL: no .config in $build"; exit 1; }
    test -s "$vmlinux" || { echo "FATAL: no vmlinux in $build"; exit 1; }
    echo "kernel ${mdv} (${kernel})"

    echo "=== the config this kernel was actually built with ==="
    got="$(sed -n 's/^CONFIG_PAHOLE_VERSION=//p' "$conf")"
    test "$got" = "${pin.paholeVersionExpected}" \
      || { echo "FATAL: built with CONFIG_PAHOLE_VERSION=$got, kernel.pin requires ${pin.paholeVersionExpected}"; exit 1; }
    echo "CONFIG_PAHOLE_VERSION=$got (as pinned)"
    for opt in ${lib.concatStringsSep " " mustBeYes}; do
      grep "^$opt=y" "$conf" >/dev/null || { echo "FATAL: $opt is not =y in the built config"; exit 1; }
      echo "$opt=y"
    done
    if grep '^CONFIG_MODULE_ALLOW_BTF_MISMATCH=y' "$conf" >/dev/null; then
      echo "FATAL: CONFIG_MODULE_ALLOW_BTF_MISMATCH=y (tolerates the defect this gate exists for)"; exit 1
    fi
    echo "CONFIG_MODULE_ALLOW_BTF_MISMATCH is not set (deliberate)"
    if ! diff -u ${./config} "$conf"; then
      echo "FATAL: the kernel's final .config differs from nix/pkgs/kernel/config (oldconfig changed it at build time)"
      exit 1
    fi
    echo "final .config == nix/pkgs/kernel/config"

    echo "=== THE GATE: does this kernel's BTF carry the kfunc defect? ==="
    objcopy --dump-section .BTF=$out/vmlinux.btf "$vmlinux" /dev/null
    test -s $out/vmlinux.btf || { echo "FATAL: .BTF empty or absent"; exit 1; }
    echo "--- reading 1 of 2: bpftool (root cause: did pahole emit the bpf_kfunc tag?)"
    if sh ${../../../kernel/btf-xcheck.sh} $out/vmlinux.btf; then XRC=0; else XRC=$?; fi
    echo "--- reading 2 of 2: rimed kernelbtf.rs (symptom) — THIS ONE IS THE GATE"
    ${rimed}/bin/rime-kernel-btf-gate $out/vmlinux.btf
    case "$XRC" in
      0) echo "both readers agree: this kernel's scx_bpf_* kfuncs are intact" ;;
      1) echo "FATAL: the two readers disagree (bpftool says a bpf_kfunc tag is missing)"; exit 1 ;;
      *) echo "FATAL: only one reader ran (btf-xcheck exited $XRC = could not read the BTF)"; exit 1 ;;
    esac
    echo "GATE: PASS"
    } 2>&1 | tee $out/verdict.txt
  ''
