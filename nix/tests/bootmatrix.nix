# The P2 boot matrix's flake checks (docs/nixos/boot-matrix.md; the OVMF cells through Fedora's shim and GRUB are
# nix/probe/ovmf/boothook, run.sh matrix):
#
#   boot-loader-scan  X2: the activation scripts, switch-to-configuration and the program it runs to install the
#                     boot loader (Rime's hook), followed through every store path they name, never name bootctl,
#                     grub-install, grub2-install or efibootmgr, on the shipped profile (rime-vm) and its LUKS
#                     variant; the hook program cannot run a program at all. Negative controls in the same check:
#                     the same system with NixOS's GRUB installer (rime.boot off) and with an activation snippet
#                     calling `bootctl install` must both be flagged.
#   boot-microcode    the real hook, run on the shipped toplevel in both layouts, writes an entry whose `initrd`
#                     file starts with Intel's and AMD's early microcode cpios; the same system with microcode off
#                     must show neither.
#   luks-boot         nix/modules/luks.nix: a LUKS2 root with a fixed UUID is opened in the systemd initrd from
#                     rd.luks.uuid= alone (no crypttab entry), the passphrase is typed at the console prompt, / is
#                     /dev/mapper/luks-<uuid>, and the keymap passthrough reaches the initrd's vconsole.conf.
#   kernel-vm-signed  kernel-vm (scx #127 gate, kill -9, module signatures) on the signed Rime kernel: the image
#                     carries a detached MOK signature made by sign-detached.sh with a deterministic TEST key, the
#                     VM boots that image, and its /sys/kernel/btf/vmlinux is byte for byte the BTF kernel-gate
#                     checked (K1 carries over to the signed derivation).
{ pkgs, self }:

let
  inherit (pkgs) lib;
  hook = lib.getExe pkgs.rime.rime-boot;
  rimeVm = self.nixosConfigurations.rime-vm;
  extend = m: (rimeVm.extendModules { modules = [ m ]; }).config.system.build.toplevel;

  shipped = rimeVm.config.system.build.toplevel;
  shippedLuks = extend { rime.luks.enable = true; };
  # Negative controls: must be flagged by the scan / show no microcode.
  withNixosGrub = extend { rime.boot.enable = lib.mkForce false; };
  withBootctl = extend { system.activationScripts.rime-scan-mutant = "bootctl install --no-variables"; };
  noMicrocode = extend {
    hardware.cpu.intel.updateMicrocode = lib.mkForce false;
    hardware.cpu.amd.updateMicrocode = lib.mkForce false;
  };

  luksUuid = "5b1f0c2e-7d4a-4e8b-9c3f-a1b2c3d4e5f6";

  kernelChecks = import ./kernel.nix { inherit pkgs self; };

  # A throwaway RSA-2048 key + self-signed certificate, deterministic (the kernel-sign check's recipe, another seed).
  # TEST ONLY: lives in this check's store paths, never committed, never the Rime MOK.
  testKey =
    pkgs.runCommand "bootmatrix-test-mok"
      {
        nativeBuildInputs = [
          pkgs.gnutls.bin
          pkgs.openssl
        ];
      }
      ''
        mkdir $out
        certtool --generate-privkey --key-type rsa --bits 2048 --provable \
          --seed=00000000000000000000000000000000000000000000000000000c33 --outfile key.txt
        openssl pkey -in key.txt -out $out/key.pem
        openssl req -new -x509 -key $out/key.pem -sha256 -set_serial 1 \
          -not_before 20260101000000Z -not_after 20360101000000Z \
          -subj "/CN=Rime kernel-vm-signed check TEST key (throwaway, DO NOT TRUST)/" -out $out/cert.pem
      '';
  # The detached signature of exactly this system's unsigned image, made the way nix-kernel-sign.yml makes it.
  detachedSignature =
    image: target:
    let
      hash = builtins.substring 0 32 (builtins.baseNameOf (builtins.unsafeDiscardStringContext image.outPath));
    in
    "${
      pkgs.runCommand "kernel-vm-signed-detached"
        {
          nativeBuildInputs = [
            pkgs.sbsigntool
            pkgs.openssl
            pkgs.coreutils
            pkgs.gnugrep
            pkgs.gnused
          ];
        }
        ''
          bash ${../pkgs/kernel/sign-detached.sh} --image ${image}/${target} --hash ${hash} \
            --key ${testKey}/key.pem --cert ${testKey}/cert.pem --pinned ${testKey}/cert.pem \
            --out $out --store-path ${image} --note "checks.kernel-vm-signed"
        ''
    }/${hash}.sig";
in
{
  boot-loader-scan = pkgs.runCommand "rime-boot-loader-scan" { nativeBuildInputs = [ pkgs.python3 ]; } ''
    set -euo pipefail
    python3 -I ${./boot-loader-scan.py} ${hook} clean rime-vm=${shipped} rime-vm-luks=${shippedLuks} | tee $out
    echo "--- negative controls" | tee -a $out
    python3 -I ${./boot-loader-scan.py} ${hook} flagged \
      nixos-grub-installer=${withNixosGrub} activation-bootctl=${withBootctl} | tee -a $out
    echo "PASS: no loader installer or NVRAM writer on the activation path; both negative controls flagged" | tee -a $out
  '';

  boot-microcode = pkgs.runCommand "rime-boot-microcode" { nativeBuildInputs = [ pkgs.python3 ]; } ''
    set -euo pipefail
    python3 -I ${./boot-microcode.py} ${hook} ${shipped} both | tee $out
    echo "--- negative control: microcode off" | tee -a $out
    python3 -I ${./boot-microcode.py} ${hook} ${noMicrocode} none | tee -a $out
  '';

  luks-boot = pkgs.testers.runNixOSTest {
    name = "rime-luks-boot";
    node.pkgsReadOnly = false;

    nodes.machine =
      { pkgs, ... }:
      {
        imports = [ self.nixosModules.default ];
        # The test harness's loader only: the VM must reboot into another configuration from a disk. Rime's own
        # chain (Fedora shim + GRUB + rime-boot) is proven by the OVMF matrix, not here.
        virtualisation = {
          emptyDiskImages = [ 512 ];
          useBootLoader = true;
          useEFIBoot = true;
          mountHostNixStore = true;
          memorySize = 2048;
        };
        boot.loader.systemd-boot.enable = true;
        boot.initrd.systemd.emergencyAccess = true;
        environment.systemPackages = [
          pkgs.cpio
          pkgs.zstd
          pkgs.cryptsetup # to make the volume; the LUKS configuration ships it through rime.luks
        ];

        specialisation.luks.configuration = {
          rime.luks = {
            enable = true;
            uuid = luksUuid;
            keymap = "de";
          };
          virtualisation.rootDevice = "/dev/mapper/luks-${luksUuid}";
        };
      };

    testScript =
      { nodes, ... }:
      let
        top = "${nodes.machine.system.build.toplevel}/specialisation/luks";
      in
      ''
        machine.wait_for_unit("multi-user.target")
        with subtest("a LUKS2 root with a known UUID"):
            machine.succeed("echo -n rime-lab-passphrase | cryptsetup luksFormat --type luks2 -q --iter-time=1 "
                            "--uuid ${luksUuid} /dev/vdb -")
            machine.succeed("echo -n rime-lab-passphrase | cryptsetup open /dev/vdb setup -")
            machine.succeed("mkfs.ext4 -q /dev/mapper/setup && cryptsetup close setup")
            print(machine.succeed("cryptsetup luksDump /dev/vdb | head -12"))

        with subtest("the initrd carries no crypttab entry for it and the keymap passthrough"):
            # The toplevel's initrd is a symlink into the store: zstd refuses a symlink unless forced (-f).
            init = machine.succeed("readlink -f ${top}/initrd").strip()
            listing = machine.succeed(f"zstd -dcf {init} | cpio -it --quiet")
            assert "usr/lib/systemd/system-generators/systemd-cryptsetup-generator" in listing or \
                "systemd-cryptsetup-generator" in listing, "no cryptsetup generator in the initrd"
            crypttab = machine.succeed(f"cd /tmp && zstd -dcf {init} | cpio -i --quiet --to-stdout etc/crypttab || true")
            assert "${luksUuid}" not in crypttab, crypttab
            vc = machine.succeed(f"zstd -dcf {init} | cpio -i --quiet --to-stdout etc/vconsole.conf")
            print("initrd vconsole.conf: " + vc)
            assert "KEYMAP=de" in vc, vc

        with subtest("boot the LUKS configuration, type the passphrase at the prompt"):
            # The disk's loader already lists the specialisation (useBootLoader installs it); make it the next boot
            # (the test harness's systemd-boot, in the VM; Rime's own chain is the OVMF matrix's).
            entries = machine.succeed("ls /boot/loader/entries")
            print(entries)
            luks_entry = [e for e in entries.split() if "specialisation-luks" in e]
            assert len(luks_entry) == 1, entries
            machine.succeed(f"bootctl set-oneshot {luks_entry[0]}")
            machine.succeed("sync")
            machine.crash()
            machine.start()
            machine.wait_for_console_text("Please enter passphrase for disk", timeout=600)
            machine.send_console("rime-lab-passphrase\n")
            machine.wait_for_unit("multi-user.target")

        with subtest("/ is the volume rd.luks.uuid named"):
            cmdline = machine.succeed("cat /proc/cmdline")
            print(cmdline)
            assert "rd.luks.uuid=${luksUuid}" in cmdline, cmdline
            mounts = machine.succeed("findmnt -no SOURCE /").strip()
            assert mounts == "/dev/mapper/luks-${luksUuid}", mounts
            print(machine.succeed("cryptsetup status luks-${luksUuid}"))
            machine.succeed("grep -x 'KEYMAP=de' /etc/vconsole.conf")
      '';
  };

  kernel-vm-signed = kernelChecks.kernel-vm.extend {
    modules = [
      (
        { lib, ... }:
        {
          name = lib.mkForce "rime-kernel-signed";
          nodes.machine =
            { config, pkgs, ... }:
            {
              rime.kernel.signing = {
                certificate = "${testKey}/cert.pem";
                imageSignature = detachedSignature config.system.build.rimeKernelImage config.boot.kernelPackages.kernel.target;
                require = true;
              };
              environment.systemPackages = [ pkgs.sbsigntool ];
            };
          testScript = lib.mkAfter (
            { nodes, ... }:
            let
              m = nodes.machine;
              target = m.boot.kernelPackages.kernel.target;
            in
            ''

              with subtest("the VM booted the MOK-signed image (kernsign path, TEST key)"):
                  k = machine.succeed("readlink -f /run/booted-system/kernel").strip()
                  assert k == "${m.system.build.rimeKernelBootImage}/${target}", k
                  assert k != "${m.system.build.rimeKernelImage}/${target}", "booted the unsigned image"
                  print(machine.succeed(f"sbverify --list {k}"))
                  machine.succeed(f"sbverify --cert ${testKey}/cert.pem {k}")

              with subtest("K1 on the signed derivation: the running kernel's BTF is the BTF kernel-gate checked"):
                  machine.succeed("cmp /sys/kernel/btf/vmlinux ${kernelChecks.kernel-gate}/vmlinux.btf")
                  print(machine.succeed("sha256sum /sys/kernel/btf/vmlinux ${kernelChecks.kernel-gate}/vmlinux.btf"))
            ''
          );
        }
      )
    ];
  };
}
