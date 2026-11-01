# The P2 boot matrix's systems (docs/nixos/boot-matrix.md; run.sh matrix-*): the SHIPPED profile (every Rime module,
# nix/hosts/profile.nix, rime.boot on) with the REAL Rime kernel (rime.kernel), signed the way the release kernel is
# (kernsign: a detached signature of system.build.rimeKernelImage, committed as <signatureDir>/<image hash>.sig and
# attached by nix/pkgs/kernel/sign.nix), here by a THROWAWAY TEST MOK. Built with --impure from the rime-os flake:
#
#   images  pass 1: the unsigned images to sign, one per distinct image hash ({ hash, image, target })
#   <cell>.gen1 / .gen2   pass 2 (signatureDir + certificate given; rime.kernel.signing.require = true, so an
#                         unsigned kernel cannot slip into a cell)
#
# Cells (layout x root): bls-plain, bls-luks (fresh install, D9: Rime's FAT ESP is /boot), custom-plain, custom-luks
# (migrated bootc machine: GRUB's custom.cfg; plain = topology E's one btrfs with /boot a top-level directory, LUKS =
# the bootc installer's layout, an unencrypted ext4 /boot "rime-boot" and the btrfs inside LUKS2). The LUKS
# toplevels are generic (rime.luks.enable, no UUID): the volume comes from host.toml [boot] kernel_params
# rd.luks.uuid=, which the hook appends to every entry.
#
#   gen1  good: stage 2 prints RIME-LAB markers on ttyS0 (generation, Secure Boot state, the test MOK in the
#         kernel's keyring, the root device and LUKS status), waits for rime-greet on tty1 (greetd's sway + the
#         rime-greet Quickshell, a greeter session on VT 1), prints GREETER-UP and holds the screen for the driver's
#         screenshot, then does what fw_cfg opt/rime/action says (install:<toplevel> = nix-env --set +
#         switch-to-configuration boot, in the VM) and powers off;
#   gen2  BAD: its root filesystem label does not exist; the initrd falls into emergency mode, where a unit prints
#         the verdict and powers off.
{
  flake,
  signatureDir ? null,
  certificate ? null,
}:

let
  nixpkgs = flake.inputs.nixpkgs;
  inherit (nixpkgs) lib;
  signed = signatureDir != null;

  lab =
    { config, pkgs, lib, modulesPath, ... }:
    {
      imports = [ (modulesPath + "/profiles/qemu-guest.nix") ];
      nixpkgs.hostPlatform = "x86_64-linux";

      rime.kernel = {
        enable = true;
        # No NVIDIA in OVMF (the gate keeps it unloaded anyway); the rest of the module tree is the shipped one.
        nvidia.enable = false;
        # Pass 2 signs; pass 1 must not pick up a committed signature (those are by the real Rime MOK, for CI).
        signing =
          if signed then
            {
              inherit signatureDir certificate;
              require = true;
            }
          else
            { imageSignature = lib.mkForce null; };
      };

      # No GPU driver in the initrd (checks.kernel-boot-budget): Plymouth runs on simpledrm over OVMF's GOP framebuffer.
      boot.initrd.availableKernelModules = [ "virtio_pci" "virtio_blk" ];
      boot.kernelModules = [ "qemu_fw_cfg" ];
      # /dev/console stays tty0 (the last console=), where Plymouth asks for the LUKS passphrase as on hardware;
      # the kernel log and the lab's markers also go to ttyS0.
      boot.kernelParams = [ "console=ttyS0,115200" "console=tty0" "plymouth.ignore-serial-consoles" ];
      # The serial getty's vhangup on ttyS0 cuts the driver's marker output (OVMF probe finding).
      systemd.services."serial-getty@ttyS0".enable = false;

      # VM-only graphics knobs (as nix/tests/vm-boot.nix): pixman for the greeter's sway, software Qt Quick.
      environment.sessionVariables = {
        WLR_RENDERER = "pixman";
        QT_QUICK_BACKEND = "software";
      };
      # No network in the lab.
      systemd.services.rime-flathub-setup.enable = lib.mkForce false;

      users.users.rime = {
        isNormalUser = true;
        initialPassword = "rime";
        extraGroups = [ "wheel" "video" "audio" ];
      };
      users.users.root.initialHashedPassword = "!";
      networking.hostName = "rime-bootmatrix";
      system.stateVersion = "26.05";
    };

  generation = n: {
    environment.etc."rime-lab-generation".text = "${toString n}\n";
  };

  # LUKS2: the passphrase prompt is the real one (Plymouth on tty0, typed on the VM's keyboard); this initrd unit
  # only TELLS the driver on ttyS0 when systemd-cryptsetup is asking, so it knows when to type.
  luks =
    { pkgs, ... }:
    {
      rime.luks.enable = true;
      boot.initrd.systemd.services.rime-lab-ask-watch = {
        description = "Rime boot matrix lab: say on ttyS0 when a passphrase is asked";
        wantedBy = [ "initrd.target" ];
        unitConfig.DefaultDependencies = false;
        serviceConfig = {
          Type = "simple";
          ExecStart = "/bin/sh -c 'until ls /run/systemd/ask-password/ask.* >/dev/null 2>&1; do sleep 0.2; done; echo \"RIME-LAB: PASSPHRASE-ASKED $(cat /proc/uptime)\" > /dev/ttyS0'";
        };
      };
    };

  driver =
    { config, pkgs, ... }:
    {
      systemd.services.rime-bootmatrix-lab = {
        description = "Rime boot matrix lab: stage-2 markers, greeter wait, driver";
        wantedBy = [ "multi-user.target" ];
        after = [ "systemd-user-sessions.service" ];
        path = [
          config.nix.package
          pkgs.coreutils
          pkgs.util-linux
          pkgs.mokutil
          pkgs.kmod
          pkgs.gnugrep
          pkgs.gnused
          pkgs.gawk
          pkgs.findutils
          pkgs.procps
          pkgs.systemd
          pkgs.cryptsetup
        ];
        serviceConfig = {
          Type = "oneshot";
          StandardOutput = "tty";
          StandardError = "tty";
          TTYPath = "/dev/ttyS0";
          TimeoutStartSec = "20min";
        };
        script = ''
          say() { echo "RIME-LAB: $*"; }
          say "stage-2 reached: generation=$(cat /etc/rime-lab-generation) system=$(readlink -f /run/current-system)"
          say "cmdline: $(cat /proc/cmdline)"
          say "uname: $(uname -r)"
          say "sb-state: $(mokutil --sb-state 2>&1 | tr '\n' ' ')"
          dmesg | grep -E "Loaded X.509 cert 'Rime boot-hook lab TEST MOK" | sed 's/^/RIME-LAB: /' || true
          src=$(findmnt -no SOURCE /)
          say "root: $src $(findmnt -no FSTYPE,OPTIONS /)"
          say "boot mount: $(findmnt -no SOURCE,FSROOT,FSTYPE /boot | tr '\n' ' ')"
          for d in /dev/mapper/luks-*; do
            [ -e "$d" ] || continue
            say "luks: $d $(cryptsetup status "$(basename "$d")" | grep -E 'type:|cipher:|device:' | tr -s ' \n' ' ')"
            say "luks uuid: $(cryptsetup luksUUID "$(cryptsetup status "$(basename "$d")" | awk '$1=="device:"{print $2}')")"
          done
          if [ -L /boot/loader ]; then
            say "ostree: loader -> $(readlink /boot/loader); $(sha256sum /boot/loader.1/entries/ostree-1.conf)"
            say "custom.cfg windows entry: $(grep -c "menuentry 'Windows (lab)'" /boot/grub2/custom.cfg)"
          fi
          up=""
          for i in $(seq 1 240); do
            sess=$(loginctl list-sessions --no-legend | awk '$3=="greetd"{print $1}' | head -n1)
            if [ -n "$sess" ] && pgrep -u greetd -x sway >/dev/null \
               && pgrep -u greetd -f 'qs -p .*/rime-greet/shell.qml' >/dev/null; then
              up="$(loginctl show-session "$sess" -p VTNr -p Class -p Type | tr '\n' ' ')"
              case "$up" in *VTNr=1*Class=greeter*) break;; esac
            fi
            up=""
            sleep 1
          done
          if [ -n "$up" ]; then
            say "GREETER-UP after $i s: session $sess $up; sway pid $(pgrep -u greetd -x sway); qs pid $(pgrep -u greetd -f 'qs -p .*/rime-greet/shell.qml')"
            say "plymouth: $(systemctl is-active plymouth-quit-wait.service) $(journalctl -b -u plymouth-start --no-pager -o cat | tail -n1)"
            sleep 25   # the driver takes its screenshot of the greeter now
          else
            say "GREETER-MISSING after 240 s"
            journalctl -b -u greetd --no-pager | tail -n 30 | sed 's/^/RIME-LAB: greetd| /'
          fi
          modprobe qemu_fw_cfg || true
          A=/sys/firmware/qemu_fw_cfg/by_name/opt/rime/action/raw
          action=none
          [ -r "$A" ] && action=$(cat "$A")
          say "action: $action"
          case "$action" in
            install:/nix/store/*)
              t=''${action#install:}
              rc=0
              out=$( { nix-env -p /nix/var/nix/profiles/system --set "$t" && "$t/bin/switch-to-configuration" boot; } 2>&1 ) || rc=$?
              printf '%s\n' "$out" | sed 's/^/RIME-LAB: hook| /'
              say "install rc=$rc profile now $(readlink /nix/var/nix/profiles/system)"
              ;;
          esac
          say "rime-boot check: $(/run/current-system/sw/bin/rime-boot check 2>&1 | tr -s ' \n' ' ')"
          if [ -d /boot/loader/entries ] && [ ! -L /boot/loader ]; then
            for e in /boot/loader/entries/*.conf; do sed "s|^|RIME-LAB: entry $(basename "$e")| |" "$e"; done
          fi
          grep -E '^(menuentry|set default|set timeout|  *(linux|initrd) )' /boot/grub2/rime-nixos.cfg | sed 's/^/RIME-LAB: menu| /'
          say "files: $(ls -l /boot/rime/nixos | awk 'NR>1{print $5, $9}' | tr '\n' ' ')"
          say "boot df: $(df -h --output=size,used,avail /boot | tail -n1)"
          sync
          say "DONE, powering off"
          systemctl --no-block poweroff
        '';
      };
    };

  bad =
    { lib, ... }:
    {
      fileSystems."/".device = lib.mkForce "/dev/disk/by-label/rime-lab-missing";
      fileSystems."/".options = [ "x-systemd.device-timeout=20s" ];
      boot.initrd.systemd.services.rime-bootmatrix-lab-bad = {
        description = "Rime boot matrix lab: the bad generation reached emergency mode";
        wantedBy = [ "emergency.target" ];
        unitConfig.DefaultDependencies = false;
        serviceConfig = {
          Type = "oneshot";
          ExecStart = "/bin/sh -c 'echo \"RIME-LAB: BAD generation 2 reached initrd emergency mode (root by-label/rime-lab-missing)\" > /dev/ttyS0'";
          ExecStartPost = "/bin/systemctl --no-block poweroff";
        };
      };
    };

  layouts = {
    bls = {
      fileSystems."/" = { device = "/dev/disk/by-label/rime-lab-root"; fsType = "ext4"; };
      fileSystems."/boot" = { device = "/dev/disk/by-label/RIMEESP"; fsType = "vfat"; options = [ "umask=0077" ]; };
    };
    custom = {
      fileSystems."/" = { device = "/dev/disk/by-label/rime-lab-btrfs"; fsType = "btrfs"; options = [ "subvol=@nixos" ]; };
      fileSystems."/mnt/rime-top" = { device = "/dev/disk/by-label/rime-lab-btrfs"; fsType = "btrfs"; options = [ "subvolid=5" ]; };
      fileSystems."/boot" = { device = "/mnt/rime-top/boot"; fsType = "none"; options = [ "bind" ]; depends = [ "/mnt/rime-top" ]; };
    };
    # The bootc installer's LUKS layout (installer/rime-install): ESP, ext4 /boot labelled rime-boot, LUKS2 btrfs.
    custom-luks = {
      fileSystems."/" = { device = "/dev/disk/by-label/rime-lab-btrfs"; fsType = "btrfs"; options = [ "subvol=@nixos" ]; };
      fileSystems."/boot" = { device = "/dev/disk/by-label/rime-boot"; fsType = "ext4"; };
    };
  };

  cells = {
    bls-plain = { layout = layouts.bls; luks = false; };
    bls-luks = { layout = layouts.bls; luks = true; };
    custom-plain = { layout = layouts.custom; luks = false; };
    custom-luks = { layout = layouts.custom-luks; luks = true; };
  };

  system =
    cell: extra:
    (lib.nixosSystem {
      modules = [
        flake.nixosModules.default
        (flake.outPath + "/nix/hosts/profile.nix")
        lab
        cell.layout
      ]
      ++ lib.optional cell.luks luks
      ++ extra;
    }).config;

  gens = cell: {
    gen1 = (system cell [ (generation 1) driver ]).system.build.toplevel;
    gen2 = (system cell [ (generation 2) bad ]).system.build.toplevel;
  };

  # The LUKS writer: a throwaway initrd-only VM (nixpkgs' kernel, busybox, cryptsetup) that formats a disk's LUKS2
  # partition with the cell's fixed UUID and copies the plain filesystem image into it through dm-crypt, so the host
  # (katana) never opens a dm-crypt device. run.sh boots it in the rime-ovmf-probe container: vda = the cell's disk,
  # vdb = the plain filesystem image; kernel arguments rime.part=, rime.uuid=, rime.fs=; passphrase on fw_cfg.
  wpkgs = nixpkgs.legacyPackages.x86_64-linux;
  writerKernel = wpkgs.linuxPackages.kernel;
  writerModules = wpkgs.makeModulesClosure {
    kernel = writerKernel;
    firmware = wpkgs.linux-firmware;
    rootModules = [ "virtio_pci" "virtio_blk" "dm_mod" "dm_crypt" "qemu_fw_cfg" "xts" "aesni_intel" ];
    allowMissing = true;
  };
  writerInit = wpkgs.writeScript "rime-luks-writer-init" ''
    #!${wpkgs.busybox}/bin/sh
    export PATH=${lib.makeBinPath [ wpkgs.kmod wpkgs.cryptsetup wpkgs.coreutils wpkgs.e2fsprogs wpkgs.btrfs-progs wpkgs.busybox ]}
    mkdir -p /proc /sys /dev /tmp /run/cryptsetup
    mount -t proc proc /proc; mount -t sysfs sys /sys; mount -t devtmpfs dev /dev
    say() { echo "RIME-WRITER: $*" > /dev/ttyS0; }
    for m in virtio_pci virtio_blk dm_mod dm_crypt qemu_fw_cfg xts aesni_intel; do modprobe $m 2>/dev/null; done
    for w in $(cat /proc/cmdline); do case $w in rime.*=*) k=''${w%%=*}; eval "''${k#rime.}=''${w#*=}";; esac; done
    i=0; while [ ! -b /dev/vda$part ] || [ ! -b /dev/vdb ]; do i=$((i+1)); [ $i -gt 100 ] && break; sleep 0.1; done
    pass=$(cat /sys/firmware/qemu_fw_cfg/by_name/opt/rime/passphrase/raw)
    P=/dev/vda$part
    say "formatting $P uuid=$uuid fs=$fs ($(blockdev --getsize64 $P) bytes; payload $(blockdev --getsize64 /dev/vdb) bytes)"
    rc=0
    printf %s "$pass" | cryptsetup luksFormat --batch-mode --type luks2 --uuid "$uuid" --label RIMEROOT \
      --pbkdf argon2id --iter-time 1000 "$P" - || rc=1
    printf %s "$pass" | cryptsetup open "$P" rimew - || rc=1
    # Only the image's non-zero blocks are written (conv=sparse): unwritten blocks of the volume read back as
    # noise, which neither ext4 (uninitialised groups) nor btrfs (free space) reads.
    dd if=/dev/vdb of=/dev/mapper/rimew bs=4M conv=sparse,fsync status=none || rc=1
    case "$fs" in
      ext4) e2fsck -fn /dev/mapper/rimew > /tmp/fsck 2>&1 || rc=1 ;;
      btrfs) btrfs check --readonly /dev/mapper/rimew > /tmp/fsck 2>&1 || rc=1 ;;
    esac
    tail -n 3 /tmp/fsck | while read -r l; do say "fsck| $l"; done
    cryptsetup luksDump "$P" | grep -E "^(Version|UUID|Label)|cipher:|PBKDF:" | while read -r l; do say "dump| $l"; done
    cryptsetup close rimew || rc=1
    sync
    say "DONE rc=$rc"
    poweroff -f
  '';
  luksWriter = wpkgs.runCommand "rime-luks-writer" { } ''
    mkdir $out
    ln -s ${writerKernel}/${writerKernel.target} $out/kernel
    ln -s ${wpkgs.makeInitrd {
      contents = [
        { object = writerInit; symlink = "/init"; }
        { object = "${writerModules}/lib"; symlink = "/lib"; }
      ];
    }}/initrd $out/initrd
  '';

  images = lib.unique (
    lib.concatMap (
      cell:
      map
        (
          extra:
          let
            c = system cell extra;
          in
          {
            inherit (c.system.build.rimeKernelSigning) imageHash target;
            image = c.system.build.rimeKernelImage;
          }
        )
        [
          [ (generation 1) driver ]
          [ (generation 2) bad ]
        ]
    ) (lib.attrValues cells)
  );
in
lib.mapAttrs (_: gens) cells
// {
  inherit images luksWriter;
  # One store path per distinct unsigned image, for run.sh to sign (nix build of this list).
  imagesJson = builtins.toJSON (map (i: { inherit (i) imageHash target; image = "${i.image}"; }) images);
}
