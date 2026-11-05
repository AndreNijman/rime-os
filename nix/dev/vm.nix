# The guest half of the persistent development VM (docs/nixos/dev-vm.md). nix/dev/default.nix adds this module to
# nixosConfigurations.rime-vm (nix/hosts/vm.nix: the shipped profile, the rime/rime TEST account) with
# extendModules, so the dev VM is the system the VM tests boot plus only what is below. Never on real hardware.
#
#   desktop   null (Rime's own sessions only) or an id from nix/desktops/default.nix (gnome, kde, cosmic)
#   session   the wayland-sessions id greetd's initial_session logs the TEST user into; the kernel parameter
#             rime.devvm.session=<id> picks another one at start (dev-vm start --session), rime.devvm.greeter
#             skips the autologin and leaves rime-greet on screen (dev-vm start --greeter)
{ desktop ? null, session ? "hyprland" }:

{ config, lib, pkgs, ... }:

let
  user = "rime";

  # greetd runs `/bin/sh -c "exec <command>"`, so the command is one word. Started like rime-greet starts a session
  # (files/desktop/rime-greet/GreetContext.qml launch(), _sessionEnv): the session file's Exec under `sh -lc`,
  # XDG_CURRENT_DESKTOP/XDG_SESSION_DESKTOP only for a session that is not one of Rime's own.
  autologin = pkgs.writeShellScript "rime-dev-vm-session" ''
    id=${lib.escapeShellArg session}
    for a in $(cat /proc/cmdline); do
      case "$a" in rime.devvm.session=*) id=''${a#rime.devvm.session=} ;; esac
    done
    f=/etc/rime/wayland-sessions/$id.desktop
    if [ ! -f "$f" ]; then
      echo "rime-dev-vm-session: no session '$id' in /etc/rime/wayland-sessions" >&2
      exit 1
    fi
    cmd=$(${pkgs.gnused}/bin/sed -n 's/^Exec=//p' "$f" | head -n1)
    names=$(${pkgs.gnused}/bin/sed -n 's/^DesktopNames=//p' "$f" | head -n1)
    export XDG_SESSION_TYPE=wayland
    case "$id" in
      hyprland|niri|rime-labwc|rime-safe-graphics|rime-gaming) ;;
      *)
        if [ -n "$names" ]; then
          names=$(printf '%s' "$names" | tr ';' ':')
          export XDG_CURRENT_DESKTOP=''${names%:} XDG_SESSION_DESKTOP=$id
        fi ;;
    esac
    exec sh -lc "$cmd"
  '';
in
{
  imports = lib.optional (desktop != null) { rime.desktops.${desktop}.enable = true; };

  # The run script is bin/run-rime-dev-vm-vm (dev-vm.sh starts it by that name).
  system.name = lib.mkForce "rime-dev-vm";

  virtualisation = {
    # dev-vm start --cpus N overrides the cores through QEMU_OPTS (QEMU takes the last -smp).
    # vm.nix's 4096/4 are the VM tests' sizes; a desktop session under llvmpipe wants more of both. Memory is fixed at
    # build time (qemu-vm's memfd backend for virtiofs names the size).
    memorySize = lib.mkForce 6144;
    cores = lib.mkForce 6;
    # KVM or nothing: the run script refuses to start without a usable /dev/kvm instead of falling back to TCG,
    # and -cpu host replaces qemu-common's -cpu max (QEMU takes the last -cpu).
    qemu.forceAccel = true;
    # The VM tests' QEMU fix (nix/tests/lib/rime-qemu.nix): wait for every virtiofsd socket before QEMU starts, or
    # under load QEMU loses the race and exits with "Failed to connect". Full QEMU (SPICE, VNC), not qemu_test.
    qemu.package = pkgs.callPackage ../tests/lib/rime-qemu.nix { qemu = pkgs.qemu_kvm; };
    qemu.options = [
      "-cpu host"
      # virtio-vga, not virtio-gpu-pci: the boot console is on the display too. Mesa renders in the guest
      # (llvmpipe) unless dev-vm start --gl replaces this device.
      "-vga none"
      "\${RIME_DEVVM_GPU:--device virtio-vga,xres=1280,yres=800}"
    ];
    # The disk (NIX_DISK_IMAGE) is the persistent qcow2 dev-vm keeps in its state directory; the store is the
    # host's, shared read-only over virtiofs, so a rebuild copies nothing into the VM.
  };

  # ssh with the key dev-vm generated, handed in at every start as an SMBIOS credential (dev-vm passes
  # -smbios type=11,value=io.systemd.credential.binary:devvm.authorized_keys=...). Keys only, as on every Rime
  # system (A13); the TEST password (rime) is for the console, the greeter and sudo.
  rime.security.ssh.enable = true;
  systemd.services.rime-dev-vm-keys = {
    description = "Install the dev VM's ssh key for ${user} and root";
    wantedBy = [ "multi-user.target" ];
    before = [ "sshd.service" ];
    after = [ "local-fs.target" ];
    serviceConfig = {
      Type = "oneshot";
      ImportCredential = "devvm.authorized_keys";
    };
    script = ''
      k=$CREDENTIALS_DIRECTORY/devvm.authorized_keys
      if [ ! -s "$k" ]; then echo "no devvm.authorized_keys credential: ssh stays closed to keys"; exit 0; fi
      for h in /root:root:root /home/${user}:${user}:users; do
        d=''${h%%:*}; o=''${h#*:}; o=''${o%%:*}; g=''${h##*:}
        install -d -m 700 -o "$o" -g "$g" "$d/.ssh"
        install -m 600 -o "$o" -g "$g" "$k" "$d/.ssh/authorized_keys"
      done
    '';
  };

  # Straight into a session at boot, the way rime-session-select's one-shot does it: greetd's initial_session, run
  # once per boot. Logging out lands on rime-greet; rime.devvm.greeter on the kernel command line skips it.
  services.greetd.settings.initial_session = {
    inherit user;
    command = "${autologin}";
  };
  # pam_systemd reads XDG_SESSION_TYPE from the worker's environment when PAM's has none, so logind records the
  # autologin as a wayland session, as it does for a greeter login (rimed reads logind's Type).
  systemd.services.greetd.environment.XDG_SESSION_TYPE = "wayland";
  systemd.services.rime-dev-vm-greeter = {
    description = "Skip the dev VM's autologin (rime.devvm.greeter)";
    wantedBy = [ "greetd.service" ];
    before = [ "greetd.service" ];
    unitConfig.ConditionKernelCommandLine = "rime.devvm.greeter";
    serviceConfig.Type = "oneshot";
    # greetd runs initial_session only while its runfile is absent.
    script = "touch /run/greetd.run";
  };

  # rime-remoted dials the production relay at every login (services.nix). A throwaway VM with a TEST account has no
  # business there; `systemctl --user start rime-remoted` in the VM when the relay is what is being worked on.
  systemd.user.services.rime-remoted.wantedBy = lib.mkForce [ ];

  # Flathub's first-boot setup downloads remote metadata; a dev VM boots without it (the VM tests do the same).
  systemd.services.rime-flathub-setup.enable = lib.mkForce false;

  # Tools a session needs to be poked from ssh.
  environment.systemPackages = [ pkgs.grim pkgs.jq pkgs.wtype ];
}
