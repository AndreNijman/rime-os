# Fedora defaults Rime relied on without saying so (PLAN.md 4.12, last row; docs/nixos/fedora-defaults.md).
#
# Fedora 45 bootc gave every Rime machine these through packages, presets and the image's own files. NixOS
# does not, and nothing else on feat/nixos set them. Each one was measured on katana (Rime 2026.11.01,
# read-only) and against the evaluated rime-vm toplevel before it was added here; the doc names the evidence.
# Everything is mkDefault, so a host or another area can still choose differently.
#
# nix/tests/fedora-parity.nix checks every line in a VM, and fails against a machine with
# rime.fedoraParity.enable = false.
{ config, lib, pkgs, ... }:

let
  cfg = config.rime.fedoraParity;
  files = ../../files;
  nm = config.networking.networkmanager.enable;
  desktop = config.rime.desktop.enable;
in
{
  options.rime.fedoraParity.enable = lib.mkOption {
    type = lib.types.bool;
    default = true;
    description = ''
      Keep the Fedora defaults Rime relied on implicitly: firmware, zram swap, /tmp on tmpfs, zsh as the
      default shell for new accounts, NetworkManager's Wi-Fi and connectivity settings, fontconfig's Noto
      defaults and the udev rules and user units Fedora's packages enabled. See docs/nixos/fedora-defaults.md.
    '';
  };

  config = lib.mkIf cfg.enable (lib.mkMerge [
    {
      # Fedora installs linux-firmware (+ amd-ucode-firmware, wireless-regdb) on every machine. NixOS ships no
      # firmware unless asked; nixos-generate-config asks through not-detected.nix, which Rime cannot count on.
      # Without it: no Intel/MediaTek/Realtek Wi-Fi, no Bluetooth, no AMD/Intel GPU firmware.
      # CPU microcode is NOT set here: it is per CPU vendor, and the host layer (D2) owns hardware facts.
      hardware.enableRedistributableFirmware = lib.mkDefault true;

      # zram-generator-defaults (Containerfile.core:1296-1310): katana's /usr/lib/systemd/zram-generator.conf
      # is `zram-size = min(ram, 8192)`, zstd. Without swap systemd-oomd degrades and OOM is abrupt.
      zramSwap = {
        enable = lib.mkDefault true;
        algorithm = lib.mkDefault "zstd";
        memoryPercent = lib.mkDefault 100;
        memoryMax = lib.mkDefault (8 * 1024 * 1024 * 1024);
      };

      # Fedora mounts /tmp as tmpfs (systemd's tmp.mount; katana: `tmpfs /tmp`), so it is empty after every
      # boot. NixOS keeps /tmp on the root filesystem and never clears it. Nix builds go to /var/tmp instead of
      # the RAM-backed /tmp (the NixOS manual's advice when /tmp is a tmpfs).
      boot.tmp.useTmpfs = lib.mkDefault true;
      systemd.services.nix-daemon.environment.TMPDIR = lib.mkIf config.nix.enable (lib.mkDefault "/var/tmp");

      # zsh is the default login shell for accounts created from here on (Containerfile.core:2569-2591): the
      # useradd DEFAULT, not existing or declared accounts. The installer and `useradd` read /etc/default/useradd;
      # `rime user` names its shell itself. programs.zsh puts zsh in /etc/shells and writes /etc/zshrc, which
      # base.nix's profile snippets (programs.zsh.*ShellInit) need to run at all.
      programs.zsh.enable = lib.mkDefault true;
      environment.etc."default/useradd".text = lib.mkIf config.programs.zsh.enable (lib.mkForce ''
        GROUP=100
        HOME=/home
        SHELL=/run/current-system/sw/bin/zsh
      '');
      # Fedora's zsh ships /etc/skel/.zshrc, so a new account's first zsh finds one; NixOS ships none, and the
      # first console login stopped in zsh-newuser-install's menu (installer2 OVMF firstboot). The same stock
      # skeleton, a real file (useradd -m copies a symlink as a symlink into the read-only store):
      # rime-shell-firstrun recognises it (is_stock_zshrc: its first line, compinit + COMPLETE_IN_WORD only) and
      # replaces it with Rime's zsh configuration at the first desktop login, exactly as on Fedora.
      environment.etc."skel/.zshrc" = lib.mkIf config.programs.zsh.enable {
        mode = "0644";
        text = ''
          # .zshrc is sourced in interactive shells.
          # It should contain commands to set up aliases,
          # functions, options, key bindings, etc.
          #

          autoload -U compinit
          compinit

          #allow tab completion in the middle of a word
          setopt COMPLETE_IN_WORD
        '';
      };
    }

    (lib.mkIf nm {
      # Rime's own two NetworkManager drop-ins (Containerfile.base:303-356), the same files: Wi-Fi power saving
      # off (the MT7925 resume fix and the lid-closed VPN) and connectivity checking (captive portals).
      # TODO(handover): the check URI is Fedora's; after the pivot that is a party a NixOS machine did not talk
      # to before. Andre picks the endpoint (rimeos.com would need a Cloudflare route).
      environment.etc."NetworkManager/conf.d/20-rime-wifi-powersave.conf".source =
        files + "/system/NetworkManager/20-rime-wifi-powersave.conf";
      environment.etc."NetworkManager/conf.d/21-rime-connectivity.conf".source =
        files + "/system/NetworkManager/21-rime-connectivity.conf";

      # Fedora's NetworkManager package defaults (/usr/lib/NetworkManager/conf.d on katana):
      # 22-wifi-mac-addr.conf, a stable random MAC per Wi-Fi network instead of the hardware one;
      # 24-clat-auto.conf, IPv4 over IPv6-only networks with NAT64 (nixpkgs builds NetworkManager with clat).
      networking.networkmanager.wifi.macAddress = lib.mkDefault "stable-ssid";
      networking.networkmanager.settings."connection-24-clat-auto"."ipv4.clat" = lib.mkDefault 1;
    })

    (lib.mkIf desktop {
      # Fedora's fontconfig resolves the generic families to Noto (katana: fc-match sans-serif = Noto Sans,
      # serif = Noto Serif, monospace = Noto Sans Mono, emoji = Noto Color Emoji). NixOS defaults to DejaVu.
      fonts.fontconfig.defaultFonts = {
        sansSerif = lib.mkDefault [ "Noto Sans" ];
        serif = lib.mkDefault [ "Noto Serif" ];
        monospace = lib.mkDefault [ "Noto Sans Mono" ];
        emoji = lib.mkDefault [ "Noto Color Emoji" ];
      };

      # The nixpkgs switcheroo-control module installs the daemon but not its udev rule
      # (30-discrete-gpu.rules), so no GPU is ever tagged discrete and "launch on the dGPU" offers nothing.
      services.udev.packages = lib.mkIf config.services.switcherooControl.enable [
        config.services.switcherooControl.package
      ];

      # libinput's udev rules (80-libinput-device-groups, 90-libinput-fuzz-override): tablet/pad pairing and
      # touchpad fuzz reset. NixOS installs them only with X11 (services.libinput defaults to xserver.enable).
      services.libinput.enable = lib.mkDefault true;

      # Fedora's hyprlock package ships /etc/pam.d/hyprlock. hyprlock is on the system profile (desktop.nix);
      # run without a PAM service it falls through to "other", which denies: a lock nobody can open.
      security.pam.services.hyprlock = { };

      # Packages Fedora installed on Rime: ipp-usb (driverless USB printers and scanners, 71-ipp-usb.rules)
      # and usbmuxd (iPhone USB: tethering, files; 39-usbmuxd.rules).
      services.ipp-usb.enable = lib.mkDefault true;
      services.usbmuxd.enable = lib.mkDefault true;
    })

    (lib.mkIf config.hardware.bluetooth.enable {
      # bluez's mpris-proxy runs in every Rime session on Fedora (globally enabled user unit; active on katana):
      # a Bluetooth headset's play/pause/next buttons reach the Shell's media controls through it.
      systemd.user.services.mpris-proxy.wantedBy = [ "default.target" ];
    })
  ]);
}
