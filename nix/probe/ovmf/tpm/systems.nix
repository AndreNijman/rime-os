# The TPM lab's NixOS system (README.md here). Built with --impure from the rime-os flake:
#   nix build --impure --expr '(import ./systems.nix { flake = builtins.getFlake "path:/work/wt-tpmkeymap";
#                                signature = /path/to/kernel.sig; }).toplevel'
#
# A minimal NixOS with Rime's boot hook (nix/modules/boot.nix, the "bls" fresh-install layout of ../boothook) and
# nix/modules/luks-tpm.nix. Its kernel is nixpkgs' linuxPackages_latest with a detached THROWAWAY TEST-MOK signature
# attached, linked as <toplevel>/kernel the way kernel.nix links the CI-signed Rime kernel. A LUKS2 data volume with a
# fixed UUID is unlocked by the INITRD (rd.luks.name=, the way a migrated machine's volumes are named on its kernel
# command line), so the TPM2 token, the passphrase prompt and the recovery key are all the initrd's.
#
# TEST ONLY parts: an initrd unit that reports passphrase asks on ttyS0, and a stage-2 driver that does what fw_cfg
# opt/rime/action says (README.md: actions), reading the TEST passphrase / TEST recovery key from fw_cfg.
{ flake, signature ? null }:

let
  nixpkgs = flake.inputs.nixpkgs;
  inherit (nixpkgs) lib;
  luksUuid = "8c2f0a5e-3b1d-4e7a-9c6f-1d2e3f4a5b6c";

  machine = { config, pkgs, modulesPath, ... }:
    let
      kernel = config.boot.kernelPackages.kernel;
      signedImage = pkgs.runCommand "linux-${kernel.version}-bzImage-TEST-MOK-signed"
        { nativeBuildInputs = [ pkgs.sbsigntool ]; } ''
          cp ${kernel}/${config.system.boot.loader.kernelFile} $out
          chmod u+w $out
          sbattach --attach ${signature} $out
          sbverify --list $out
        '';
      sd = config.systemd.package;
      # every keyslot with its role and a digest of its JSON (a rewritten slot changes it), every token
      luksRoles = pkgs.writeText "luks-roles.py" ''
        import hashlib, json, sys
        m = json.load(sys.stdin)
        tk = m.get("tokens", {})
        tpm = {str(k) for t in tk.values() if t.get("type") == "systemd-tpm2" for k in t.get("keyslots", [])}
        rec = {str(k) for t in tk.values() if t.get("type") == "systemd-recovery" for k in t.get("keyslots", [])}
        for k, v in sorted(m.get("keyslots", {}).items()):
            role = "tpm2" if k in tpm else "recovery" if k in rec else "passphrase"
            d = hashlib.sha256(json.dumps(v, sort_keys=True).encode()).hexdigest()[:16]
            print(f"slot {k} {role} sha256={d}")
        for k, t in sorted(tk.items()):
            print(f"token {k} {t.get('type')} slots={t.get('keyslots')} pcrs={t.get('tpm2-pcrs', '-')}")
      '';
    in
    {
      imports = [
        (modulesPath + "/profiles/minimal.nix")
        (modulesPath + "/profiles/qemu-guest.nix")
        (flake.outPath + "/nix/modules/boot.nix")
        (flake.outPath + "/nix/modules/luks-tpm.nix")
      ];
      nixpkgs.hostPlatform = "x86_64-linux";
      nixpkgs.overlays = [ flake.overlays.default ];
      nixpkgs.config.allowUnfree = true;

      rime.boot.enable = true;
      rime.luksTpm.enable = true;

      boot.kernelPackages = pkgs.linuxPackages_latest;
      boot.initrd.luks.forceLuksSupportInInitrd = true;
      boot.initrd.availableKernelModules = [ "virtio_pci" "virtio_blk" "ahci" "sd_mod" ];
      boot.kernelModules = [ "qemu_fw_cfg" ];
      boot.kernelParams = [ "console=ttyS0,115200" "rd.luks.name=${luksUuid}=cryptdata" ];

      system.systemBuilderCommands = lib.mkIf (signature != null) (lib.mkAfter ''
        ln -sf -- ${signedImage} $out/kernel
      '');

      fileSystems."/" = { device = "/dev/disk/by-label/rime-lab-root"; fsType = "ext4"; };
      fileSystems."/boot" = { device = "/dev/disk/by-label/RIMEESP"; fsType = "vfat"; options = [ "umask=0077" ]; };

      networking.hostName = "rime-tpm-lab";
      networking.useDHCP = false;
      documentation.enable = false;
      services.udisks2.enable = false;
      users.users.root.initialHashedPassword = "!";
      system.stateVersion = "26.05";
      environment.systemPackages = [ pkgs.tpm2-tools pkgs.cryptsetup pkgs.mokutil ];

      # TEST ONLY: report passphrase asks (and whether the volume came up without one) on the serial console.
      boot.initrd.systemd.services.rime-test-askwatch = {
        description = "TEST: report LUKS passphrase asks on the serial console";
        wantedBy = [ "sysinit.target" ];
        unitConfig.DefaultDependencies = false;
        conflicts = [ "initrd-switch-root.target" ];
        before = [ "initrd-switch-root.target" ];
        serviceConfig.Type = "simple";
        # The count is written at every ask, and once more when initrd-switch-root stops this unit (TERM): a 0.2 s
        # poll alone can miss a volume that opens just before switch-root (measured: N5/N6 read "asks none").
        script = ''
          seen=" " n=0 unlocked=0
          echo 0 > /run/rime-test-asks
          scan() {
            for f in /run/systemd/ask-password/ask.*; do
              [ -e "$f" ] || continue
              case "$seen" in *" $f "*) continue ;; esac
              seen="$seen$f "
              n=$((n + 1))
              echo "RIME-ASK $n" > /dev/ttyS0
              echo "$n" > /run/rime-test-asks
            done
            if [ "$unlocked" = 0 ] && [ -e /dev/mapper/cryptdata ]; then
              unlocked=1
              echo "RIME-UNLOCKED after $n asks" > /dev/ttyS0
            fi
          }
          trap 'scan; exit 0' TERM
          while :; do
            scan
            sleep 0.2
          done
        '';
      };

      # TEST ONLY: the stage-2 driver.
      systemd.services.rime-tpm-lab = {
        description = "Rime TPM lab: stage-2 measurements and driver";
        wantedBy = [ "multi-user.target" ];
        after = [ "systemd-user-sessions.service" "tpm2.target" ];
        # serial-getty@ttyS0 vhangup()s ttyS0 when it starts, which silently drops every later write of this unit to
        # it (measured: N4 lost its tpm2_clear and DONE lines and failed the verdict although the clear ran). The
        # getty waits for this oneshot.
        before = [ "serial-getty@ttyS0.service" ];
        path = [ pkgs.coreutils pkgs.util-linux pkgs.mokutil pkgs.kmod pkgs.gnugrep pkgs.gnused pkgs.findutils
          pkgs.tpm2-tools pkgs.cryptsetup pkgs.python3 sd ];
        serviceConfig = { Type = "oneshot"; StandardOutput = "tty"; StandardError = "tty"; TTYPath = "/dev/ttyS0"; };
        script = ''
          set +e
          say() { echo "RIME-TPM: $*"; }
          dev=/dev/disk/by-uuid/${luksUuid}
          modprobe qemu_fw_cfg || true
          cfg() { local f=/sys/firmware/qemu_fw_cfg/by_name/opt/rime/$1/raw; [ -r "$f" ] && cat "$f" || true; }
          action=$(cfg action); action=''${action:-measure}
          printf %s "$(cfg pass)" > /run/rime-pass; chmod 600 /run/rime-pass
          say "stage-2 reached: system=$(readlink -f /run/current-system) action=$action"
          say "cmdline: $(cat /proc/cmdline)"
          say "sb-state: $(mokutil --sb-state 2>&1 | tr '\n' ' ')"
          dmesg | grep -E "Loaded X.509 cert 'Rime TPM lab TEST MOK" | sed 's/^/RIME-TPM: /' || true
          say "asks-in-initrd: $(cat /run/rime-test-asks 2>/dev/null || echo none)"
          if [ -e /dev/tpmrm0 ]; then
            say "tpm: present $(cat /sys/class/tpm/tpm0/tpm_version_major 2>/dev/null)"
            say "pcr7-sysfs: $(cat /sys/class/tpm/tpm0/pcr-sha256/7)"
            say "pcrread: $(tpm2_pcrread sha256:7 2>&1 | tr -s ' \n' ' ')"
            say "eventlog-begin"
            base64 -w 0 /sys/kernel/security/tpm0/binary_bios_measurements; echo
            say "eventlog-end"
          else
            say "tpm: absent"
          fi
          journalctl -b -o cat --no-pager -u systemd-cryptsetup@cryptdata.service | sed 's/^/RIME-TPM: cryptsetup| /' || true
          [ -b /dev/mapper/cryptdata ] && say "cryptdata: open" || say "cryptdata: NOT open"
          tokens() { cryptsetup luksDump --dump-json-metadata "$dev" | python3 -I ${luksRoles} | sed 's/^/RIME-TPM: luks| /'; }
          tokens
          marker() { if [ -b /dev/mapper/cryptdata ]; then say "marker: $(head -c 64 /dev/mapper/cryptdata | tr -d '\0')"; fi; }

          case "$action" in
            init:*)
              # The bootc-era state: data written, and a TPM slot sealed to the PCR 7 value the bootc chain measured
              # (passed in from the lab's probe boot), the way bootc's rime-luks-enroll sealed it on that chain.
              printf 'RIME-TPM-LAB-MARKER %s' "$(cfg marker)" | dd of=/dev/mapper/cryptdata bs=512 conv=sync status=none
              sync
              PASSWORD=$(cat /run/rime-pass) systemd-cryptenroll --tpm2-device=auto \
                --tpm2-pcrs=7:sha256=''${action#init:} "$dev" 2>&1 | sed 's/^/RIME-TPM: enrol| /'
              say "init rc=''${PIPESTATUS[0]}"
              tokens
              ;;
            rebind:*)
              # A TPM slot this chain does not satisfy: sealed to another PCR 7 value. Wipe and enrol are separate
              # calls (one call is a no-op, rime-luks-enroll's note).
              PASSWORD=$(cat /run/rime-pass) systemd-cryptenroll --wipe-slot=tpm2 "$dev" 2>&1 | sed 's/^/RIME-TPM: wipe| /'
              PASSWORD=$(cat /run/rime-pass) systemd-cryptenroll --tpm2-device=auto \
                --tpm2-pcrs=7:sha256=''${action#rebind:} "$dev" 2>&1 | sed 's/^/RIME-TPM: enrol| /'
              say "rebind rc=''${PIPESTATUS[0]}"
              tokens
              ;;
            reenrol)
              # The product path: the unit asks once (systemd-ask-password); this driver is the agent that answers.
              systemctl start --no-block "rime-luks-tpm-reenrol@${luksUuid}.service"
              answered=0
              for _ in $(seq 1 300); do
                for f in /run/systemd/ask-password/ask.*; do
                  [ -e "$f" ] || continue
                  grep -q '^Id=rime-luks-tpm:${luksUuid}$' "$f" || continue
                  sock=$(sed -n 's/^Socket=//p' "$f")
                  say "answering ask: $(sed -n 's/^Message=//p' "$f")"
                  printf %s "$(cat /run/rime-pass)" | ${sd}/lib/systemd/systemd-reply-password 1 "$sock"
                  answered=$((answered + 1))
                done
                state=$(systemctl show -P ActiveState "rime-luks-tpm-reenrol@${luksUuid}.service")
                case "$state" in
                  activating) ;;
                  failed) break ;;
                  *) [ "$answered" -gt 0 ] && break ;;
                esac
                sleep 0.2
              done
              journalctl -b -o cat --no-pager -u "rime-luks-tpm-reenrol@${luksUuid}.service" | sed 's/^/RIME-TPM: reenrol| /'
              say "reenrol answered=$answered result=$(systemctl show -P Result "rime-luks-tpm-reenrol@${luksUuid}.service")"
              tokens
              ;;
            reenrol-bootc)
              # MUTANT: what bootc's rime-luks-enroll did on every run (wipe the recovery slot, write a new key).
              PASSWORD=$(cat /run/rime-pass) ${pkgs.rime.rime-libexec}/libexec/rime-luks-enroll --device "$dev" \
                --recovery-out /run/rime-new-recovery --replace-recovery 2>&1 | sed 's/^/RIME-TPM: reenrol| /'
              say "reenrol-bootc rc=''${PIPESTATUS[0]}"
              tokens
              ;;
            clear)
              tpm2_clear 2>&1 | sed 's/^/RIME-TPM: clear| /'
              say "clear rc=''${PIPESTATUS[0]}"
              ;;
          esac
          marker
          rm -f /run/rime-pass
          sync
          say "DONE, powering off"
          systemctl --no-block poweroff
        '';
      };
    };

  sys = lib.nixosSystem { modules = [ machine ]; };
in
{
  inherit luksUuid;
  toplevel = sys.config.system.build.toplevel;
  unsignedKernel = sys.config.system.build.kernel;
}
