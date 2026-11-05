# The development VM (`nix/dev`)

A persistent Rime NixOS VM to try a change in: edit a module or the Shell, `dev-vm rebuild`, look at it, ssh in.
It boots the same `rime-vm` configuration the `nix/tests` VM checks use, plus a small development module
(`nix/dev/vm.nix`), from the checkout it is run in.

**What it is not.** It is not a release gate and not a substitute for the VM proofs in `nix/tests`
(`checks.vm-boot`, `shell-kept`, `desktop-lock-*`, the installer and update checks): those boot a fresh system
from nothing, assert, and run in CI or under `nix/ci/integrate`. The dev VM keeps state between boots, logs in
by itself, turns some services off (below) and asserts nothing beyond "it came up". A change that works here
still needs its check.

## Running it

The host has no Nix and no QEMU, so it runs in the `rime-nix` container (store at `/nix`, `/var/lab/rime-nix` at
`/work`, `/dev/kvm`). Two equivalent ways:

```
# on the host, from any rime-os checkout under /var/lab/rime-nix (paths are host paths):
nix/dev/dev-vm start
nix/dev/dev-vm ssh
nix/dev/dev-vm screenshot /var/lab/rime-nix/artifacts/me/shot.png

# in the container, from the checkout:
cd /work/<worktree> && nix run path:.#dev-vm -- start
```

`nix/dev/dev-vm` is a thin wrapper: `sudo -n podman exec -w /work/<checkout> rime-nix nix run path:.#dev-vm -- …`,
with host paths given to `--flake`, `--shell` and `screenshot` turned into `/work` paths (anything outside
`/var/lab/rime-nix` is refused: the container cannot see it). A flake app rather than a bare script, so the
script's tools (QEMU's client side, ssh, socat, python for QMP) come from the same nixpkgs as the system, and
`checks.dev-vm` can shellcheck and test exactly what runs.

| Command | What it does |
|---|---|
| `start` | build the system if needed, boot it, wait for the login prompt, ssh and the desktop; prints each time from QEMU's start |
| `rebuild` | build from the checkout, stop the VM, start it again on the same disk; prints build / stop / boot times |
| `build` | build only (a running VM keeps the system it booted) |
| `stop` | ACPI power off; QEMU is quit after 90 s (`RIME_DEVVM_STOP_WAIT`) if the guest does not power off |
| `status`, `list` | one VM; every VM under the state root |
| `wait login\|ssh\|desktop [--timeout S]` | wait for one of the three |
| `ssh [--root] [cmd…]` | ssh as the TEST user `rime` (or root) with the VM's own key |
| `screenshot FILE` | the screen as PNG through QMP (works headless) |
| `type TEXT` | type on the VM's keyboard; `\n` is Return (e.g. into rime-greet) |
| `console`, `log [N]` | the serial console (Ctrl-] leaves); the last N lines of its log |
| `reset --yes` | delete the persistent disk (VM must be stopped). Never implicit |

Options: `--name NAME` (default `rime`; several VMs side by side), `--desktop hyprland|gnome|kde|cosmic` (default
Hyprland; the others are `rime.desktops.<id>` systems), `--shell DIR` (build with a local rime-shell checkout:
`--override-input rime-shell path:DIR`; remembered, `--shell locked` goes back to `flake.lock`), `--session ID`
(log into another session of the same system), `--greeter` (boot to rime-greet instead of logging in), `--display
none|spice|vnc` (default none), `--cpus N`, `--no-wait`, `--flake DIR`.

`--display spice|vnc` listens on 127.0.0.1 only (ports from 5930 / 5900, printed by `start` and `status`). GTK/SDL
windows are not offered: QEMU runs inside the container, which has no display to open a window on. Connect with
`remote-viewer spice://127.0.0.1:<port>` or a VNC viewer from the host.

**TEST credentials only:** user `rime`, password `rime` (the `rime-vm` configuration's test account). The ssh key is
generated per VM into its state directory and handed to the guest as a systemd credential (SMBIOS type 11,
`devvm.authorized_keys`); sshd stays keys only.

## Where state lives

`/var/lab/rime-nix/devvm/<name>/` (`/work/devvm/<name>` in the container; `RIME_DEVVM_ROOT` moves it):

| File | |
|---|---|
| `disk.qcow2` | the persistent disk: `/home`, `/var`, `/etc` state. Survives stop, start and rebuild; `reset --yes` deletes it |
| `vm` | out-link to the last built `system.build.vm`; a GC root (`/nix/var/nix/gcroots/auto`) |
| `booted` | the system the VM was started from, its own GC root, so `build` while it runs cannot strand it |
| `id_ed25519{,.pub}`, `known_hosts` | the VM's TEST ssh key and its host key record |
| `settings` | desktop, shell override, ports, display, session, cpus: what `rebuild` reuses (read as data, never sourced) |
| `serial.log` | the serial console, appended across boots |
| `qemu.log`, `timings.log` | QEMU's output; one line per start/rebuild with build/stop/login/ssh/desktop seconds |
| `qmp.sock`, `serial.sock`, `qemu.pid` | while it runs |

Nothing is built per iteration beyond the system closure itself: the guest boots the kernel and initrd straight
from the store (`qemu-vm.nix` direct boot) and mounts the host's `/nix/store` over virtiofs, so a rebuild is a new
closure in the store and a reboot, never a disk image.

## How it boots

- `nixosConfigurations.rime-vm.extendModules` with `nix/dev/vm.nix`; `legacyPackages.x86_64-linux.dev-vm.<desktop>`
  is its `system.build.vm`. Not a flake package or check: CI evaluates every one of those, and four more full
  systems would only cost it time.
- KVM, never TCG: `virtualisation.qemu.forceAccel` (QEMU refuses to start without KVM) plus `-cpu host`; `start`
  asks QMP `query-kvm` and quits the VM unless it says enabled.
- 6144 MB and 6 vCPUs. The memory is a memfd backend sized at build time (virtiofs needs shared memory), so there is
  no runtime memory option; `--cpus` changes `-smp` only.
- Autologin: greetd's `initial_session` starts the session's own `Exec` from `/etc/rime/wayland-sessions/<id>.desktop`
  with rime-greet's environment rules; `greetd.service` sets `XDG_SESSION_TYPE=wayland` so logind records the
  session as Wayland. `--greeter` (kernel parameter `rime.devvm.greeter`) skips it.
- Turned off in the dev VM: `rime-remoted` (it dials the production relay) and the first-boot Flathub setup.
- Guards: at most 2 VMs at once (`RIME_DEVVM_MAX`), and `start` refuses below 16 GB `MemAvailable`
  (`RIME_DEVVM_MIN_AVAIL_MB`). `stop` only signals the PID in `qemu.pid` if that process is a `qemu-system-*` with
  this VM's QMP socket on its command line. QEMU and virtiofsd are started under a small subreaper (`launch.py`)
  because the container's PID 1 (`sleep infinity`) never reaps.

## Measured (katana, 2026-11-04, rime-nix container)

| | build | stop | login prompt | ssh | desktop up (Shell IPC answers) |
|---|---|---|---|---|---|
| cold first boot, fresh disk after `reset --yes` (system already built) | 5.0 s | | 9.5 s | 9.7 s | 12.9 s |
| `rebuild` after a one-line change to `nix/modules/desktop.nix` | 7.4 s | 1.6 s | 9.3 s | 9.5 s | 11.2 s |
| `rebuild --shell <local rime-shell>` after a one-line Shell change | 25.9 s | 1.7 s | 9.4 s | | 11.1 s |
| GNOME variant, first start | 16.3 s | | 9.6 s | 9.8 s | 9.9 s |
| KDE Plasma variant, first start | 19.9 s | | 9.9 s | 10.1 s | 10.3 s |
| COSMIC variant, first start | 17.1 s | | 9.6 s | 9.8 s | 10.2 s |

Boot times are from QEMU's start. On GNOME, KDE and COSMIC "desktop up" means a Wayland socket and a logind
session with a display, not that the desktop finished painting (Plasma's panel and COSMIC's first-run setup were
on screen a few seconds later). Evidence: `/var/lab/rime-nix/artifacts/dev-vm/` (screenshots, `rebuild-*.log`,
`persist-stop-start.txt`, `state-{rime,gnome,kde,cosmic}/` with each VM's serial log and `timings.log`).

## The check

`checks.dev-vm` (no VM, runs in CI): the app builds (writeShellApplication shellchecks `dev-vm.sh`), shellcheck on the
host wrapper, flake8 on `qmp.py`/`launch.py`, the Hyprland dev VM evaluates, and a suite of the script's refusals
(usage errors, names that would leave the state root, `reset` without `--yes` or on a running VM, `stop` never
signalling a process that is not this VM's QEMU, `list`). Mutant: `owns_pid` trusting any PID; the suite fails
"stop killed an unrelated process". Evaluation peaks at 1.8 GB (not in `nix/ci/eval-heavy.txt`).
