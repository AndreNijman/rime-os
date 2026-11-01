# Rime on NixOS: the flake

One flake at the rime-os root (`flake.nix`) on pinned nixos-unstable (PLAN.md D8, decision A1).

## Layout

```
flake.nix                    inputs (nixpkgs pin, rime-shell source) and outputs; the only file that knows inputs
nix/pkgs/default.nix         the pkgs.rime.* set (overlays.default); add a line only for a NEW package
nix/pkgs/rimed.nix           the rimed Cargo workspace (rimed, rime, rime-agentd, rime-aid, rime-secretd, rime-remoted)
nix/pkgs/rime-shell.nix      Rime Shell from the rime-shell input (flake = false)
nix/pkgs/rime-libexec.nix    files/system/libexec as store paths (bootc: /usr/libexec)
nix/pkgs/kernel/default.nix  Rime's kernel: CachyOS 7.2.9 + BORE via linuxManualConfig (nix/pkgs/kernel/README.md)
nix/modules/default.nix      nixosModules.default: imports every module + sets nixpkgs.overlays
nix/modules/base.nix         rime.base      stateVersion, nix settings, NetworkManager, locale, mutable users
nix/modules/desktop.nix      rime.desktop   sessions, Shell, portals, fonts, audio
nix/modules/services.nix     rime.services  rimed + helper daemons, libexec, update engine, timers
nix/modules/kernel.nix       rime.kernel    kernel, NVIDIA, msi-ec (off by default: VMs boot the cached kernel)
nix/modules/gaming.nix       rime.gaming    Steam, gamescope Gaming Mode, NTSYNC, scx
nix/modules/security.nix     rime.security  firewall, sudo, PAM/keyring, sshd, Secure Boot hooks
nix/modules/packages-catalogue.nix  rime.catalogue  `rime install` catalogue and the default package set
nix/hosts/vm.nix             nixosConfigurations.rime-vm (QEMU; test user rime/rime lives ONLY here)
nix/tests/default.nix        checks.x86_64-linux.<name>, one file per test
nix/tests/vm-boot.nix        the full system test: rime-vm + Rime's kernel, greeter login, Shell, rimed, sshd, firewall
```

Outputs: `packages.x86_64-linux.{rimed,rime-shell,rime-libexec,rime-kernel}`, `overlays.default`,
`nixosModules.{default,base,desktop,services,kernel,gaming,security,packages-catalogue}`,
`nixosConfigurations.rime-vm`, `checks.x86_64-linux.{eval,vm-boot,rimed,libexec,desktop,shell,gaming,security,kernel-*}`.

Rules for the per-area modules:

- Options live under `rime.<area>.*`. Each area has `enable`; `rime-vm` turns every area on except the kernel.
- Modules reach Rime packages as `pkgs.rime.<name>`. Only `nixosModules.default` sets the overlay, so import
  `default`, not an area module on its own.
- Tests import `self.nixosModules.default` and set `node.pkgsReadOnly = false` (the module sets
  `nixpkgs.overlays`, which a read-only test pkgs refuses). The `vm-boot` node imports `nix/hosts/vm.nix` (every
  area on) and adds Rime's kernel; an area's own test enables only what it needs.
- `nixpkgs.config.allowUnfree = true` is set once in `base.nix` and once in the flake's own `pkgs`; do not add it
  elsewhere.

## Building (inside the `rime-nix` container only)

The host has no Nix. Everything runs in the rootful podman container `rime-nix`, where `/var/lab/rime-nix` is
`/work` (so this worktree is `/work/os` and the rime-shell worktree is `/work/shell`).

**Use `path:.`, not `.`.** The worktree's `.git` is a file pointing at a git dir under `/var/home/andre`, which is
not mounted in the container, so `nix build .#x` fails with "opening Git repository ... failed to resolve path".

**Pass `--override-input rime-shell path:/work/shell`** until the rime-shell `feat/nixos` branch is pushed (see
below). Without it the locked narHash of `/work/shell` stops matching as soon as that worktree changes.

```
O='--override-input rime-shell path:/work/shell'
sudo -n podman exec -w /work/os rime-nix nix flake check path:. --no-build $O
sudo -n podman exec -w /work/os rime-nix nix build path:.#rimed -L --print-out-paths $O
sudo -n podman exec -w /work/os rime-nix nix build path:.#checks.x86_64-linux.vm-boot -L --print-out-paths $O
sudo -n podman exec -w /work/os rime-nix nix eval path:.#nixosConfigurations.rime-vm.config.system.build.toplevel.drvPath $O
```

Files the container writes are root-owned: `sudo -n chown -R andre:andre /var/lab/rime-nix/os` before git.
`result` and `result-*` are ignored.

## The rime-shell lock

`flake.lock` currently records the `rime-shell` input as `path:/work/shell` (locked with
`--override-input rime-shell path:/work/shell`), because `github:AndreNijman/rime-shell/feat/nixos` did not exist
on GitHub yet. That lock evaluates only inside this container. Once the branch is pushed, relock it:

```
sudo -n podman exec -w /work/os rime-nix nix flake update rime-shell --flake path:.
```

## Measured on the skeleton (2026-11-01, katana, container rime-nix)

- `nix flake check --no-build`: all outputs evaluate, 5.7 s.
- `checks.eval`: 14 s (first fetch of stdenv).
- `checks.vm-boot`: 1 min 32 s wall on a cold store (475 paths fetched, 808.6 MiB; 224 derivations built); the
  guest reported `Hypervisor detected: KVM`, reached multi-user.target in 8.8 s, test script 8.82 s.
- `rimed` + `rime-shell` + `rime-libexec` built together: 3 min 17 s (rimed: 7 binaries, tests off).
- `rime-vm` with `rime.kernel.enable = true` evaluates (stub kernel 7.3); not built.
