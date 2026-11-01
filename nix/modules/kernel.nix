# Kernel and GPU (PLAN.md 4.6, D7): Rime's CachyOS kernel, its out-of-tree modules, NVIDIA policy, NTSYNC,
# sched-ext, and the module-signing chain.
#
# bootc sources ported here (behaviour, not paths):
#   kernel           Containerfile.kernel + kernel/ (pkgs.rime.rime-kernel, nix/pkgs/kernel)
#   nvidia           Containerfile.core stage 1d akmod-nvidia + the 50-rime-nvidia.conf / kargs.d / unit drop-ins
#   msi-ec, msi-wmi  stage 1d msi-ec build + stage 1g modules-load.d/rime-msi.conf, modprobe.d/51-rime-msi.conf
#   xone, xpadneo    stage 1d akmod-xone / akmod-xpadneo
#   ntsync           Containerfile.rime modules-load.d/rime-ntsync.conf
#   scx              Containerfile.core scx-builder (16 schedulers + scx_loader/scxctl)
#   signing          stage 1b sbsign + stage 1d sign-file loop, replaced by kmodigest (D7), see `signing` below
#
# Off by default so VMs and tests that do not ask for it boot nixpkgs' default (cached) kernel.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.rime.kernel;
  rimeKernelPackages = import ../pkgs/kernel/packages.nix { inherit lib; inherit (pkgs) fetchurl; };
  kp = config.boot.kernelPackages;
  kernel = kp.kernel;

  # bootc stage 1g / Containerfile.rime: "the device is created 0666 by the driver", "force=1 is a no-op on
  # non-MSI hardware", "msi-ec (updates/) has no alias of its own".
  msiModprobe = ''
    options msi-wmi-platform force=1
    # msi-ec (updates/) has no alias of its own; the in-tree one is MSI-only by DMI too.
    alias dmi*:svn*Micro-StarInternational*: msi-ec
    alias dmi*:svn*MICRO-STARINT*: msi-ec
  '';

  # NVIDIA eager loading, gated on hardware (D7). nixpkgs' NVIDIA module lists nvidia_uvm (open modules) and,
  # with X, nvidia/nvidia_modeset/nvidia_drm in boot.kernelModules, i.e. systemd-modules-load loads them on
  # EVERY machine. Rime is one system for every machine: on one without an NVIDIA display controller that load
  # fails and systemd-modules-load.service shows as failed (bootc's "portability" rule, rime-logs 46). This
  # modprobe `install` hook loads the module only when an NVIDIA (0x10de) display-class (0x03xxxx) PCI device
  # exists and otherwise succeeds without loading anything. On NVIDIA hardware it is a plain modprobe.
  nvidiaGate = pkgs.writeShellScript "rime-nvidia-modprobe-gate" ''
    # modprobe passes: <module> $CMDLINE_OPTS
    mod="$1"; shift
    for d in /sys/bus/pci/devices/*; do
      [ "$(cat "$d/vendor" 2>/dev/null)" = 0x10de ] || continue
      case "$(cat "$d/class" 2>/dev/null)" in
        0x03*) exec ${pkgs.kmod}/bin/modprobe --ignore-install "$mod" "$@" ;;
      esac
    done
    exit 0
  '';
  nvidiaGatedModules = [
    "nvidia"
    "nvidia_modeset"
    "nvidia_drm"
    "nvidia_uvm"
  ];

  # The kernel image the machine boots: nixpkgs' rebuildImage inserts the kmodigest certificate of every
  # module in this system's module tree (in-tree and out-of-tree) into the image's built-in keyring
  # (SYSTEM_EXTRA_CERTIFICATE), so module trust follows from the image (D7). Then, when the hosted signing job
  # has produced one, its detached Authenticode signature is attached.
  kernelTarget = kernel.target;
  #
  # CONFIG_X86_DECODER_SELFTEST=y (the CachyOS config, as on bootc) makes `make bzImage` run the instruction
  # decoder self-test (arch/x86/tools/insn_decoder_test), whose sources the kernel's dev output does not keep,
  # so nixpkgs' rebuildImage fails on it. The test already ran on this vmlinux in the kernel build ("Decoded
  # and checked N instructions"); re-linking the image after inserting certificates changes no code, so it is
  # skipped here by overriding the variable on make's command line.
  imageWithCerts =
    (kp.rebuildImage {
      target = kernelTarget;
      inherit (config.system) modulesTree;
    }).overrideAttrs
      (old: {
        buildFlags = old.buildFlags ++ [ "CONFIG_X86_DECODER_SELFTEST=" ];
      });
  finalImage =
    if cfg.signing.imageSignature == null then
      imageWithCerts
    else
      pkgs.runCommand "linux-image-${kernel.version}-signed" { nativeBuildInputs = [ pkgs.sbsigntool ]; } ''
        mkdir -p $out
        cp ${imageWithCerts}/${kernelTarget} $out/${kernelTarget}
        chmod u+w $out/${kernelTarget}
        sbattach --attach ${cfg.signing.imageSignature} $out/${kernelTarget}
        sbverify --list $out/${kernelTarget}
      '';
in
{
  options.rime.kernel = {
    enable = lib.mkEnableOption "Rime's own kernel instead of nixpkgs' default";

    nvidia.enable = lib.mkOption {
      type = lib.types.bool;
      default = cfg.enable;
      defaultText = lib.literalExpression "config.rime.kernel.enable";
      description = ''
        NVIDIA 615.78.08 with the open kernel modules (Turing and newer). Eager module loading is gated on an
        NVIDIA display controller being present. Pre-Turing cards get no driver, as on bootc; the warning about
        them comes from rimed (`rime doctor`), not from this module.
      '';
    };

    msi.enable = lib.mkOption {
      type = lib.types.bool;
      default = cfg.enable;
      defaultText = lib.literalExpression "config.rime.kernel.enable";
      description = "msi-ec (BeardOverflow d7fbbd88, Cooler Boost + fan mode + charge limits) and msi-wmi-platform.";
    };

    controllers.enable = lib.mkOption {
      type = lib.types.bool;
      default = cfg.enable;
      defaultText = lib.literalExpression "config.rime.kernel.enable";
      description = "xone (Xbox wireless dongle/wired) and xpadneo (Xbox Bluetooth) controller drivers.";
    };

    ntsync.enable = lib.mkOption {
      type = lib.types.bool;
      default = cfg.enable;
      defaultText = lib.literalExpression "config.rime.kernel.enable";
      description = "Load the NT synchronisation driver at boot (/dev/ntsync for Proton/Wine 11).";
    };

    scx.enable = lib.mkOption {
      type = lib.types.bool;
      default = cfg.enable;
      defaultText = lib.literalExpression "config.rime.kernel.enable";
      description = ''
        scx_loader (D-Bus) with Rime's 16 shipped sched-ext schedulers (pkgs.rime.rime-scx). No default
        scheduler: rimed picks one per profile (`scx = "auto"`), and the kernel's BORE runs otherwise.
      '';
    };

    signing = {
      insertModuleCertificates = lib.mkOption {
        type = lib.types.bool;
        default = cfg.enable;
        defaultText = lib.literalExpression "config.rime.kernel.enable";
        description = ''
          Boot an image with every module's kmodigest certificate inserted into the built-in keyring
          (nixpkgs linuxPackages.rebuildImage), so modules verify. Enforcement stays off (decision A12):
          an unverifiable module loads with a taint, as on bootc.
        '';
      };

      imageSignature = lib.mkOption {
        type = lib.types.nullOr lib.types.path;
        default = null;
        description = ''
          Detached PE/Authenticode signature (sbsign --detached) of `system.build.rimeKernelImage`, made by the
          hosted signing job with the Rime MOK. The private key never enters the Nix store or a user's machine;
          only this signature does. null = boot the image unsigned.
        '';
      };
    };
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      {
        boot.kernelPackages = (pkgs.linuxPackagesFor pkgs.rime.rime-kernel).extend rimeKernelPackages.extension;

        # NixOS skips system.requiredKernelConfig for kernels that carry `features` (every linuxManualConfig
        # kernel does: "nixpkgs kernels are assumed to have all required features"). Rime's config is not
        # nixpkgs', so check it here, at evaluation time, against the committed config.
        assertions = map (a: {
          assertion = a.assertion kernel.config;
          message = "Rime kernel (nix/pkgs/kernel/config): ${a.message}";
        }) config.system.requiredKernelConfig;

        # The unsigned image (with module certificates) the hosted signing job signs.
        system.build.rimeKernelImage = imageWithCerts;
      }

      (lib.mkIf cfg.signing.insertModuleCertificates {
        system.systemBuilderCommands = lib.mkAfter ''
          ln -sf -- ${finalImage}/${kernelTarget} $out/kernel
        '';
      })

      (lib.mkIf cfg.nvidia.enable {
        services.xserver.videoDrivers = [ "nvidia" ];
        hardware.graphics.enable = lib.mkDefault true;
        hardware.nvidia = {
          package = kp.rimeNvidia;
          open = true;
          # nvidia-drm modeset=1 fbdev=1 (bootc 50-rime-nvidia.conf): nixpkgs adds fbdev=1 for >= 545.
          modesetting.enable = true;
          # bootc ships nvidia-powerd (Dynamic Boost on laptops), gated on the driver being loaded.
          dynamicBoost.enable = lib.mkDefault true;
        };
        # bootc: blacklist + `install nouveau /bin/false` + rd.driver.blacklist/modprobe.blacklist kargs.
        # nixpkgs' NVIDIA module blacklists nouveau, nova_core and nvidiafb; the karg covers both initrd and root.
        boot.kernelParams = [ "modprobe.blacklist=nouveau" ];
        boot.extraModprobeConfig = lib.concatMapStrings (m: ''
          install ${m} ${nvidiaGate} ${m} $CMDLINE_OPTS
        '') nvidiaGatedModules;
        systemd.services.nvidia-powerd.unitConfig.ConditionPathExists = "/sys/module/nvidia";
      })

      (lib.mkIf cfg.msi.enable {
        boot.extraModulePackages = [ kp.msi-ec ];
        boot.kernelModules = [ "msi-wmi-platform" ];
        boot.extraModprobeConfig = msiModprobe;
      })

      (lib.mkIf cfg.controllers.enable {
        hardware.xone.enable = true;
        hardware.xpadneo.enable = true;
      })

      (lib.mkIf cfg.ntsync.enable {
        boot.kernelModules = [ "ntsync" ];
      })

      (lib.mkIf cfg.scx.enable {
        services.scx-loader = {
          enable = lib.mkDefault true;
          schedsPackages = lib.mkDefault [ pkgs.rime.rime-scx ];
        };
        # nixpkgs writes /etc/scx_loader.toml from `config`; with nothing set that file is EMPTY and scx_loader
        # 1.1.3 refuses to start ("The config file is empty!", seen in kernel-vm). bootc ships no /etc file, so
        # scx_loader reads the package's own config.toml (everything commented out: no default scheduler). Do
        # the same whenever no setting is given. Priority 90: beats the nixpkgs module, loses to mkForce.
        environment.etc."scx_loader.toml".source = lib.mkIf (
          lib.filterAttrsRecursive (_: v: v != null) config.services.scx-loader.config == { }
        ) (lib.mkOverride 90 "${config.services.scx-loader.package}/share/scx_loader/config.toml");
        # bootc: scx_loader is not started at boot; D-Bus activates it on the first scxctl call (rimed's game
        # mode), so a machine that never games runs no scheduler daemon. nixpkgs sets wantedBy multi-user.
        systemd.services.scx_loader.wantedBy = lib.mkOverride 90 [ ];
      })
    ]
  );
}
