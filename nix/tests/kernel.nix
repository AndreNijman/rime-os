# Rime's kernel checks (checks.x86_64-linux.kernel-*):
#
#   kernel-config-fresh  nix/pkgs/kernel/config is what nix/pkgs/kernel/config.nix generates from kernel.pin
#   kernel-gate          the bootc kernel tier's gate on the BUILT kernel: pahole 132, required =y options, no
#                        BTF-mismatch escape hatch, final .config == committed config, kfunc BTF read by two
#                        independent readers (kernel/btf-xcheck.sh + rime-kernel-btf-gate)
#   kernel-vm            boots rime.kernel in a VM and proves, on the shipped kernel:
#                        - the #127 coupling: every shipped sched-ext scheduler attaches, survives load, detaches
#                          cleanly, and a kill -9 hands tasks back to the kernel's class. This runs bootc's own
#                          tests/kernel/scx-selftest/init.sh (made safe to run inside a booted system) and parses
#                          the result with bootc's own run-scx-selftest.sh --parse-only;
#                        - scx_loader/scxctl drive a scheduler over D-Bus;
#                        - out-of-tree modules are built for this kernel (nvidia 615.78.08 open, msi-ec from
#                          updates/, xone, xpadneo) and the ones that can load without hardware verify against
#                          the kmodigest certificates inserted into the booted image (no `E` taint);
#                        - NVIDIA eager loading is gated on hardware: no NVIDIA here, modules-load still succeeds;
#                        - NTSYNC is loaded and /dev/ntsync exists.
{ pkgs, self }:

let
  inherit (pkgs) lib;
  rime = pkgs.rime;
  pin = import ../pkgs/kernel/pin.nix { inherit lib; };
  src = ../..;

  schedulersTxt = pkgs.writeText "schedulers.txt" (lib.concatLines rime.rime-scx.schedulers);

  # bootc's guest PID 1, made safe for a booted system: no remounting /proc, /sys, /run, /tmp or cgroup2 under a
  # running systemd, store paths instead of /usr/bin, and "exit 0 after the verdict" instead of sysrq power-off.
  # --replace-fail on every edit: if init.sh changes shape, this fails loudly instead of testing something else.
  scxSelftest = pkgs.runCommand "rime-scx-selftest" { } ''
    cp ${src + "/tests/kernel/scx-selftest/init.sh"} $out
    chmod u+w $out
    substituteInPlace $out \
      --replace-fail 'export PATH=/usr/bin:/usr/sbin:/bin:/sbin' 'export PATH=${
        lib.makeBinPath [
          pkgs.coreutils
          pkgs.gnugrep
          pkgs.gnused
          pkgs.util-linux
        ]
      }' \
      --replace-fail 'mount -t proc proc /proc' ':' \
      --replace-fail 'mount -t sysfs sysfs /sys' ':' \
      --replace-fail 'mount -t devtmpfs devtmpfs /dev 2>/dev/null || true' ':' \
      --replace-fail 'mount -t debugfs debugfs /sys/kernel/debug 2>/dev/null || true' ':' \
      --replace-fail 'mount -t cgroup2 cgroup2 /sys/fs/cgroup 2>/dev/null || true' ':' \
      --replace-fail 'mount -t tmpfs tmpfs /run' ':' \
      --replace-fail 'mount -t tmpfs tmpfs /tmp' ':' \
      --replace-fail 'ln -sfn /run /var/run' ':' \
      --replace-fail 'poweroff_now() {' 'poweroff_now() { exit 0; }
    _bootc_poweroff_now() {' \
      --replace-fail '/usr/bin/true' '${pkgs.coreutils}/bin/true' \
      --replace-fail 'bin="/usr/bin/$1"' 'bin="${rime.rime-scx}/bin/$1"' \
      --replace-fail '/selftest/schedulers.txt' '${schedulersTxt}' \
      --replace-fail '/selftest/kill9.txt' '/nonexistent/kill9.txt'
    ${pkgs.bash}/bin/bash -n $out
  '';

  parser = src + "/tests/kernel/scx-selftest/run-scx-selftest.sh";
  parserPath = lib.makeBinPath [
    pkgs.bash
    pkgs.coreutils
    pkgs.gnugrep
    pkgs.gnused
  ];
  # A laptop-shaped system with Rime's kernel (not the test VM: qemu-vm adds virtio_gpu and friends to its initrd).
  # Only the initrd and the boot image are built from it, never the whole system.
  laptop =
    (self.inputs.nixpkgs.lib.nixosSystem {
      modules = [
        self.nixosModules.default
        {
          nixpkgs.hostPlatform = "x86_64-linux";
          rime.kernel.enable = true;
          boot.loader.grub.enable = false;
          fileSystems."/" = {
            device = "/dev/mapper/root";
            fsType = "btrfs";
          };
          boot.initrd.luks.devices.root.device = "/dev/disk/by-partlabel/root";
          boot.initrd.availableKernelModules = [
            "nvme"
            "xhci_pci"
            "thunderbolt"
            "usb_storage"
            "usbhid"
            "sd_mod"
            "rtsx_pci_sdmmc"
          ];
          system.stateVersion = "26.05";
        }
      ];
    }).config;
in
{
  # bootc's initramfs budget (files/scripts/check-initramfs-budget.sh, Containerfile.core stage 1f), on what NixOS
  # builds: (1) boot image + initrd fit the per-deployment ceiling (130 MiB; bootc measured 114.6), and (2) no
  # firmware-bearing KMS driver and no GPU firmware is in the initrd. Plymouth and the LUKS prompt run on the
  # built-in simpledrm; a GPU driver in the initrd evicts simpledrm and drags in 100+ MiB of GSP firmware.
  # TODO(handover): the boot worker points this at the shipped host configuration(s) once they exist, and
  # decides the real target (ESP or btrfs /boot, D3).
  kernel-boot-budget =
    let
      initrd = "${laptop.system.build.initialRamdisk}/${laptop.system.boot.loader.initrdFile}";
      image = "${laptop.system.build.rimeKernelImage}/${laptop.boot.kernelPackages.kernel.target}";
    in
    assert laptop.boot.initrd.compressor == "zstd";
    pkgs.runCommand "rime-kernel-boot-budget"
      {
        nativeBuildInputs = [
          pkgs.zstd
          pkgs.cpio
        ];
      }
      ''
        set -euo pipefail
        budget=$(( 130 * 1048576 ))
        ib=$(stat -L -c %s ${image}); rb=$(stat -L -c %s ${initrd})
        per=$(( ib + rb ))
        echo "boot image $(( ib / 1048576 )) MiB + initrd $(( rb / 1048576 )) MiB = $(( per / 1048576 )) MiB per generation (ceiling 130 MiB)"
        test "$per" -le "$budget" || { echo "FAIL: over the per-generation budget"; exit 1; }
        zstd -dc ${initrd} | cpio -it --quiet > list
        echo "initrd: $(wc -l < list) entries, $(grep -c '\.ko' list || true) modules"
        bad="$(grep -E '/(nvidia[^/]*|nouveau|amdgpu|i915|xe|radeon|amdkfd)\.ko|/firmware/(nvidia|amdgpu|i915|xe|radeon)/' list || true)"
        if [ -n "$bad" ]; then
          echo "FAIL: firmware-bearing KMS driver or GPU firmware in the initrd:"; echo "$bad"; exit 1
        fi
        grep -E '/(nvme|dm-crypt)\.ko' list >/dev/null || { echo "FAIL: the listing does not see the initrd's storage modules (check reads nothing)"; exit 1; }
        echo "PASS: no GPU driver or GPU firmware in the initrd" | tee $out
      '';

  kernel-config-fresh = pkgs.runCommand "rime-kernel-config-fresh" { nativeBuildInputs = [ pkgs.diffutils ]; } ''
    if ! diff -u ${../pkgs/kernel/config} ${rime.rime-kernel-config}; then
      echo "nix/pkgs/kernel/config is stale: copy ${rime.rime-kernel-config} over it (see nix/pkgs/kernel/README.md)"
      exit 1
    fi
    echo "nix/pkgs/kernel/config == generated from kernel.pin (${pin.tag})" | tee $out
  '';

  kernel-gate = pkgs.callPackage ../pkgs/kernel/gate.nix {
    kernel = rime.rime-kernel;
    inherit (rime) rimed;
    inherit pin;
    scxSrc = rime.rime-scx.base.src;
  };

  kernel-vm = pkgs.testers.runNixOSTest {
    name = "rime-kernel";

    # nixosModules.default sets nixpkgs.overlays (pkgs.rime.*), so nodes evaluate their own nixpkgs.
    node.pkgsReadOnly = false;

    nodes.machine =
      { ... }:
      {
        imports = [ self.nixosModules.default ];
        rime.kernel.enable = true;
        # qemu-vm.nix forces videoDrivers to [ "modesetting" ] with mkVMOverride (priority 10), which would
        # silently drop the NVIDIA stack from the test VM. Win over it so the VM carries what hardware gets.
        services.xserver.videoDrivers = lib.mkOverride 9 [ "nvidia" ];
        virtualisation = {
          cores = 4;
          memorySize = 4096;
        };
      };

    testScript = ''
      machine.wait_for_unit("multi-user.target")

      with subtest("the booted kernel is Rime's"):
          rel = machine.succeed("uname -r").strip()
          assert rel == "${rime.rime-kernel.modDirVersion}", f"booted {rel}"
          machine.succeed("test -s /sys/kernel/btf/vmlinux")
          machine.succeed("test -d /sys/kernel/sched_ext")
          machine.succeed("grep -w modprobe.blacklist=nouveau /proc/cmdline")

      with subtest("modules load and verify against the image's kmodigest certificates"):
          machine.succeed("systemctl is-active systemd-modules-load.service")
          machine.succeed("test -c /dev/ntsync")
          for m in ["ntsync", "msi_wmi_platform"]:
              machine.succeed(f"test -d /sys/module/{m}")
          machine.succeed("modprobe hid_xpadneo")
          machine.succeed("modprobe xone_dongle")
          for m in ["ntsync", "msi_wmi_platform", "hid_xpadneo", "xone_dongle"]:
              taint = machine.succeed(f"cat /sys/module/{m}/taint").strip()
              assert "E" not in taint, f"{m} loaded UNSIGNED/unverified (taint {taint!r})"
              sig = machine.succeed(f"modinfo -F sig_id {m}").strip()
              assert sig, f"{m} carries no module signature"
              # Signed + loaded without the E taint = the kernel verified it against its built-in keyring.
              print(f"{m}: taint {taint!r}, sig_id {sig!r}")
          machine.fail("journalctl -k --no-pager --grep 'module verification failed'")

      with subtest("out-of-tree modules are built for this kernel"):
          path = machine.succeed("modinfo -n msi-ec").strip()
          assert "/updates/" in path, f"msi-ec resolves to {path}, not updates/ (the in-tree one has no Cooler Boost)"
          nv = machine.succeed("modinfo -F version nvidia").strip()
          assert nv == "615.78.08", f"nvidia module is {nv}"
          lic = machine.succeed("modinfo -F license nvidia").strip()
          assert lic == "Dual MIT/GPL", f"nvidia licence {lic!r}: not the open kernel module"
          for m in ["nvidia", "nvidia_drm", "nvidia_modeset", "nvidia_uvm", "msi-ec", "xone_dongle", "hid_xpadneo"]:
              v = machine.succeed(f"modinfo -F vermagic {m}").split()[0]
              assert v == "${rime.rime-kernel.modDirVersion}", f"{m} built for {v}"
              # kmodigest signatures carry no signer name; sig_id/sig_hashalgo prove a signature is appended.
              sig = machine.succeed(f"modinfo -F sig_id {m}").strip()
              assert sig, f"{m} carries no module signature"
              print(f"{m}: sig_id {sig!r}, hash {machine.succeed(f'modinfo -F sig_hashalgo {m}').strip()!r}")

      with subtest("NVIDIA eager loading is gated on hardware"):
          machine.fail("lsmod | grep -w '^nvidia'")
          machine.succeed("modprobe nvidia_uvm")
          machine.fail("lsmod | grep -w '^nvidia_uvm'")

      with subtest("nouveau can never load (bootc: blacklist + install /bin/false)"):
          machine.succeed("modinfo -n nouveau")
          machine.fail("modprobe nouveau")
          machine.fail("test -d /sys/module/nouveau")

      with subtest("modules that need hardware still pass signature verification"):
          # nvidia and msi-ec refuse to initialise without their hardware (-ENODEV), but the kernel checks the
          # signature BEFORE init, and a failed check taints the KERNEL (bit 13, E) even though the module is
          # gone again. So: force the load past the hardware gate, then read the kernel-wide taint.
          for m in ["nvidia", "msi-ec"]:
              rc, out = machine.execute(f"modprobe --ignore-install {m} 2>&1")
              print(f"forced load of {m}: rc={rc} {out.strip()!r}")
          machine.execute("modprobe -r nvidia_uvm nvidia_drm nvidia_modeset nvidia msi-ec 2>/dev/null")
          tainted = int(machine.succeed("cat /proc/sys/kernel/tainted").strip())
          print(f"kernel taint mask {tainted}")
          assert not (tainted >> 13) & 1, f"kernel tainted E (unsigned/unverified module loaded): {tainted}"
          machine.fail("journalctl -k --no-pager --grep 'module verification failed'")

      with subtest("scx_loader drives a scheduler over D-Bus"):
          # Not started at boot (as on bootc): the scxctl call D-Bus-activates it.
          machine.fail("systemctl is-active scx_loader.service")
          # The name rimed passes (syswriter.rs scx_load: `scxctl start -s scx_lavd`).
          machine.succeed("scxctl start -s scx_lavd")
          machine.succeed("systemctl is-active scx_loader.service")
          machine.wait_until_succeeds("grep -x enabled /sys/kernel/sched_ext/state", timeout=30)
          print("scx_loader ops: " + machine.succeed("cat /sys/kernel/sched_ext/root/ops"))
          machine.succeed("cat /sys/kernel/sched_ext/root/ops | grep lavd")
          machine.succeed("scxctl stop")
          machine.wait_until_succeeds("grep -x disabled /sys/kernel/sched_ext/state", timeout=30)

      with subtest("#127: every shipped scheduler attaches on this kernel, kill -9 falls back"):
          machine.succeed("systemctl stop scx_loader.service")
          rc, out = machine.execute("bash ${scxSelftest} > /tmp/scx-selftest.log 2>&1; cat /tmp/scx-selftest.log", timeout=1800)
          print(out)
          # bootc's own verdict parser, run on the guest's log (bash + coreutils/grep/sed are in the guest).
          rc, verdict = machine.execute(
              "PATH=${parserPath} bash ${parser} --parse-only /tmp/scx-selftest.log --expect ${schedulersTxt}"
          )
          print(verdict)
          assert rc == 0, "sched-ext selftest FAILED"
    '';
  };
}
