# Rime's kernel on NixOS

CachyOS `cachyos-7.2.9-2` + BORE + sched_ext, x86-64-v3, HZ 1000, BTF on: the same kernel the bootc kernel
tier builds (`Containerfile.kernel`, `kernel/`), built with nixpkgs' `linuxManualConfig` (PLAN.md 4.6, D7).

| File | What it is |
|---|---|
| `pin.nix` | reads `kernel/kernel.pin` (still the one pin for both builds) |
| `sources.nix` | the tarball, BORE patch and upstream config, fetched by kernel.pin's URL + sha256 |
| `config.nix` | generates the .config: upstream config + the spec's `%prep` edits + NixOS options, `olddefconfig` |
| `config` | the generated .config, committed; the kernel is built from THIS file |
| `default.nix` | the kernel (`pkgs.rime.rime-kernel`) |
| `packages.nix` | linuxPackages extension: msi-ec d7fbbd88 (into `updates/`), NVIDIA pinned to 615.78.08 |
| `scx.nix` | `pkgs.rime.rime-scx`: the 16 schedulers bootc ships, from nixpkgs scx 1.1.4 |
| `gate.nix` | the bootc kernel-tier gate as a check (`checks.kernel-gate`) |

Checks (`nix/tests/kernel.nix`): `kernel-config-fresh`, `kernel-gate`, `kernel-vm`.

## Bumping the kernel

1. Edit `kernel/kernel.pin` (tag, URLs, sha256s), as on bootc.
2. `nix build path:.#rime-kernel-config` and copy the result over `nix/pkgs/kernel/config`; review the diff.
3. `nix build path:.#checks.x86_64-linux.kernel-gate` (builds the kernel, ~30-75 min) and `…kernel-vm`.

`kernel-config-fresh` fails while step 2 is pending; `kernel-gate` fails if the build's own `oldconfig`
changed anything in the committed config.

## Why `linuxManualConfig` and a committed config

`buildLinux` runs nixpkgs' `common-config.nix` through `generate-config.pl`, which rewrites hundreds of options.
`linuxManualConfig` takes a complete .config as is, so **nixpkgs' common config changes nothing in Rime's kernel**:
every difference from the pinned upstream config is one of the `edits` in `config.nix`. The config is committed
(not imported from a derivation) so that NixOS can read it at evaluation time and the diff is reviewable.

NixOS does not check `system.requiredKernelConfig` for kernels that carry `features` (all linuxManualConfig
kernels), so `nix/modules/kernel.nix` asserts it itself against this config.

## Every difference

### Final Nix .config vs the bootc kernel's final .config (`/usr/lib/modules/7.2.9-cachyos1.apex1.fc45.x86_64/config`)

Value changes:

| Option | bootc | Nix | Why |
|---|---|---|---|
| `LOCALVERSION` | `""` (release came from `EXTRAVERSION=-cachyos1.apex1.fc45.x86_64`) | `"-cachyos1-rime1"` | `uname -r` = `7.2.9-cachyos1-rime1`; `rime1` = kernel.pin `APEX_KERNEL_BUILD` |
| `LSM` | `lockdown,yama,integrity,selinux,bpf,landlock` | `lockdown,yama,integrity,bpf,landlock` | decision A2: no SELinux on NixOS (`SECURITY_SELINUX` stays built, inactive) |
| `MODULE_SIG_KEY` | `"certs/signing_key.pem"` (ephemeral per build) | `""` | kmodigest signs modules deterministically (nixpkgs common-flags `cmd_sign`) |
| `SYSTEM_EXTRA_CERTIFICATE` | unset | `y`, size 3120000 | slot `rebuildImage` fills with every module's kmodigest certificate |
| `CRYPTO_MLDSA`, `CRYPTO_LIB_MLDSA` | `m` | `y` | nixpkgs' module-signing requirement (ML-DSA signatures verifiable before any module loads) |
| `CC_VERSION_TEXT`, `GCC_VERSION` | Fedora GCC 16.2.1 | nixpkgs GCC 16.2.0 | toolchain |
| `AS_VERSION`, `LD_VERSION` | Fedora binutils | nixpkgs binutils 2.46 | toolchain |
| `CC_CAN_LINK` | `y` | unset | nixpkgs builds kernels with the unwrapped compiler, which cannot link userspace; no option that depends on it is set in either config |

Visibility only (not set in either): `DEBUG_INFO_COMPRESSED_ZSTD` (bootc lists it), `GCC_PLUGINS` (Nix lists it).

`PAHOLE_VERSION=132`, `SCHED_BORE=y`, `CACHY=y`, `HZ=1000`, `X86_64_VERSION=3`, the IMA options, `RUST` unset,
`MODULE_SIG_ALL=y`, `MODULE_SIG_FORCE` unset, `SECONDARY_TRUSTED_KEYRING=y`, `NTSYNC=m`, `DEBUG_INFO_BTF=y`:
identical.

### vs the pinned upstream config (kernel.pin `KERNEL_CONFIG_URL`)

The bootc spec's `%prep` edits (`CACHY`, `SCHED_BORE`, `HZ_1000`, `X86_64_VERSION=3`, `DEFAULT_HOSTNAME`, the five
IMA options, `LSM`) plus the NixOS rows above, plus what `olddefconfig` derives from them and from the build
toolchain: `PAHOLE_VERSION` 131 → 132, the Rust and clang capability probes (`RUST*`, `*_CLANG*`, `LTO*`,
`HAS_LTO_CLANG`, `CC_HAS_*` that only clang/rustc report) dropped because neither build has rustc or clang in the
config environment, and the IMA sub-options that become visible once `IMA=y`. Upstream config: 12,698 lines;
`diff` of the `CONFIG_*=` lines: 77 lines.

## Module signing (D7): what is built, what is left

Built and tested (`kernel-vm`):
- Every in-tree and out-of-tree module carries a kmodigest signature (no key anywhere: the "key" is derived from
  the module itself).
- `rime.kernel.signing.insertModuleCertificates` (default on) boots `linuxPackages.rebuildImage`, which inserts
  `modules.cer` of the system's whole module tree into the image's built-in keyring. Modules then verify: the VM
  test loads in-tree (`ntsync`, `msi_wmi_platform`) and out-of-tree (`hid_xpadneo`, `xone_dongle`) modules and
  requires no `E` (unsigned) taint and no "module verification failed" line.
- Enforcement stays off (decision A12, = bootc).

The hook, not yet fed:
- `system.build.rimeKernelImage` is the unsigned image (certificates inserted) that must be MOK-signed.
- `rime.kernel.signing.imageSignature` takes a **detached** PE signature of exactly that image; the system then
  boots `sbattach --attach <sig> <image>` (built inside a derivation, `sbverify --list` checked).
- TODO(handover): the hosted signing job. In CI, after the release closure is built: build
  `nixosConfigurations.<host>.config.system.build.rimeKernelImage`, `sbsign --key <MOK key> --cert <MOK cert>
  --detached --output bzImage.sig` with the key from the CI secret (`APEX_SB_KEY_B64`/`APEX_SB_CRT_B64`, names
  unchanged), commit/publish the `.sig` and set `imageSignature` to it. The key never enters the Nix store, a
  binary cache or a user machine; only the signature does. The image depends on the system's module tree, so the
  signature is per host configuration and the job runs per release. Which boot chain consumes the signed image
  (shim+MOK or Lanzaboote) is the boot worker's decision (D3).
