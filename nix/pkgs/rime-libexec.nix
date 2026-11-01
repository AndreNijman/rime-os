# Rime's helper scripts and the system data they read (bootc: /usr/libexec/rime-*, /usr/share/rime*, files/desktop).
#
# On NixOS there is no /usr. What this derivation does to the bootc files, and nothing else:
#
#  1. patchShebangs: every interpreter (bash, sh, python3, node) is a store path.
#  2. One substitution table (`fhsRules` below) maps the bootc FHS paths. Rime's OWN files go to the system profile,
#     /run/current-system/sw/{libexec,share,bin}, never to this store path: those strings are also written into
#     users' configs (Hyprland/labwc/niri autostart, keybinds) and must survive a garbage collection. base.nix links
#     /libexec and /share/rime into the profile; /share/rime-shell, /share/rime-greet and /share/backgrounds are
#     linked by the desktop area. The session list is /etc/rime/wayland-sessions (desktop's display-manager session
#     set, not a profile path), and the Rime.I18n QML module is under /run/current-system/sw/lib/qt-6/qml (the
#     rime-shell package's layout).
#     A tool whose absolute path is load-bearing (read as root, or a security check) gets its own store path.
#  3. A PATH line, marked "# rime-libexec", is written into each script after its shebang (after the docstring for
#     python): the store directories of that script's dependencies, then /run/wrappers/bin (sudo, su, unix_chkpwd)
#     and /run/current-system/sw/bin (rime, the compositors, Steam: things the running system owns and a store path
#     must not pin). Scripts that run as root drop the caller's PATH entirely ("closed"). The file is never renamed,
#     so argv[0], BASH_SOURCE and `readlink -f $0` stay true (rime-tool-broker is symlinked as wrangler/terraform and
#     reads its name from $0; rime-gamescope-steam finds its siblings through BASH_SOURCE).
#  4. installCheck proves it: rime-libexec-audit.py parses every shell script with shfmt and checks that each command
#     resolves (builtin, own function, the injected store PATH, or the RUNTIME list below); python/node argv lists are
#     scanned the same way. A command passed as an ARGUMENT to a script's own helper (rime-safe-graphics'
#     `collect graphics.txt lspci -k`) is not seen; the VM test's probes are the backstop for those. Then a grep gate fails the build on any /usr/{bin,sbin,libexec,lib,lib64,share} left in
#     the output; /usr/local/* is allowed (search-list entries that simply do not exist on NixOS).
#
# Scripts NOT shipped here (bootc-only; the update and package workers of a later phase replace them):
#   rime-pkg               the RPM/systemd-sysext package engine (D5 replaces it with Nix)
#   rime-boot-migrate      GRUB -> systemd-boot migration of a bootc /boot (bootupd, ostree deployments)
#   rime-boot-count        counts bootc STAGED deployments (After=bootc-finalize-staged, /boot/loader/entries.srel)
#   rime-live-session      the per-session activator of the bootc live-update sysext layer (/run/extensions/rime-live)
#   rime-store-compact     recompresses /sysroot/ostree (ConditionPathExists=/run/ostree-booted)
#   rime-update-check      `bootc upgrade --check`; the D6 client replaces it
#   rime-update-notice     reads only what rime-update-check and the live engine write; the D6 client replaces both
#   rime-migrate-from-apex APEX-OS -> Rime state renames on a bootc /var; a NixOS machine arrives from Rime (D2 bridge)
#   rime-session-migrate   the per-user half of the same rename (firstrun calls it only when it is installed: -x test)
#   rime-vconsole-guard    repairs a hand-edited /etc/vconsole.conf; NixOS generates that file from console.*
# Shipped but NOT wired (their owner decides): rime-boot-health (boot chain, D3), rime-plymouth-accent (writes
# /etc/plymouth/plymouthd.conf, which NixOS generates: TODO(handover) boot worker), rime-luks-enroll (installer, D9),
# rime-vm (needs virtualisation.libvirtd; its install hint still names RPMs: TODO(handover) catalogue worker).
{
  lib,
  stdenvNoCC,
  bash,
  python3,
  nodejs-slim,
  shfmt,
  coreutils,
  gnugrep,
  gnused,
  gawk,
  findutils,
  diffutils,
  util-linux,
  procps,
  systemd,
  jq,
  glib,
  dbus,
  libnotify,
  getent,
  xprop,
  libcap,
  efibootmgr,
  tpm2-tools,
  cryptsetup,
  networkmanager,
  nftables,
  iproute2,
  iputils,
  kmod,
  gnutar,
  mtools,
  dosfstools,
  xkeyboard_config,
  at-spi2-core,
  pciutils,
}:

let
  root = ../..;

  dropped = [
    "rime-pkg"
    "rime-boot-migrate"
    "rime-boot-count"
    "rime-live-session"
    "rime-store-compact"
    "rime-update-check"
    "rime-update-notice"
    "rime-migrate-from-apex"
    "rime-session-migrate"
    "rime-vconsole-guard"
  ];

  # Every script gets these; they are in every NixOS system closure anyway.
  common = [ coreutils gnugrep gnused gawk findutils ];

  # What each script runs, beyond `common`. The audit fails the build when a script runs something that is neither
  # here nor on `runtime`, so this list is checked, not trusted.
  deps = {
    rime-agent-focus = [ procps ];
    rime-agent-review = [ util-linux ];
    rime-boot-health = [ python3 systemd ];
    rime-boot-windows = [ efibootmgr util-linux libnotify systemd ];
    rime-browser = [ python3 ];
    rime-compositor-reload = [ procps ];
    rime-desktop-menu = [ libnotify systemd ];
    rime-devices = [ ];
    rime-display-apply = [ procps util-linux ];
    rime-disposable = [ ];
    rime-env = [ jq ];
    rime-firewall = [ nftables networkmanager ];
    rime-game-env = [ ];
    rime-gamescope-steam = [ python3 xprop ];
    rime-gaming-discord = [ ];
    rime-gaming-session = [ libcap systemd ];
    rime-gpu-notice = [ libnotify ];
    rime-greet-publish-all = [ getent ];
    rime-greet-session = [ dbus glib ];
    rime-greet-wallpaper = [ getent util-linux ];
    rime-greetd = [ ];
    rime-guest-wipe = [ getent systemd ];
    rime-hypr-migrate = [ util-linux ];
    rime-input-apply = [ procps util-linux systemd ];
    rime-keyring-login = [ ];
    rime-labwc-keybinds = [ ];
    rime-luks-enroll = [ python3 tpm2-tools cryptsetup systemd ];
    rime-mux = [ ];
    rime-open-browser = [ glib ];
    rime-plugin = [ jq nodejs-slim ];
    rime-plymouth-accent = [ util-linux bash ];
    rime-project-windows = [ ];
    rime-safe-graphics = [ gnutar pciutils ]; # pciutils: `collect graphics.txt lspci -k` (the audit cannot see it)
    rime-safe-graphics-terminal = [ ];
    rime-screen-reader = [ procps util-linux systemd ];
    rime-secrets-start = [ systemd glib util-linux ];
    rime-session-logout = [ procps systemd ];
    rime-session-select = [ getent systemd ];
    rime-session-watchdog = [ ];
    rime-shell-autostart = [ procps ];
    rime-shell-firstrun = [ python3 util-linux systemd diffutils ];
    rime-storage-notice = [ libnotify ];
    rime-switch-greeter = [ getent ];
    rime-switch-user = [ getent ];
    rime-switcher = [ ];
    rime-tool-broker = [ ];
    rime-user = [ getent ];
    rime-vm = [ mtools dosfstools ];
    rime-wifi-resume = [ iproute2 util-linux kmod networkmanager iputils systemd ];
    # bin/
    apex = [ ];
    steamos-session-select = [ ];
    steamos-update = [ ];
  };

  # Scripts that run as root (sudo targets, system units, pam_exec, the installer): their PATH never includes the
  # caller's, so nothing a user puts on PATH can stand in for a command they run.
  closed = [
    "rime-boot-health"
    "rime-boot-windows"
    "rime-firewall"
    "rime-greet-publish-all"
    "rime-greet-wallpaper"
    "rime-greetd"
    "rime-guest-wipe"
    "rime-keyring-login"
    "rime-luks-enroll"
    "rime-plymouth-accent"
    "rime-session-select"
    "rime-switch-greeter"
    "rime-wifi-resume"
  ];

  # Commands the running system provides (found through /run/wrappers/bin or /run/current-system/sw/bin at run
  # time). They are the system's choice (which Hyprland, which Steam), or setuid wrappers, so they are not pinned.
  runtime = [
    "sudo" "su" "rime" "Hyprland" "hyprctl" "niri" "labwc" "sway" "swaymsg" "quickshell" "qs" "hypridle"
    "gamescope" "mangoapp" "steam" "equibop" "flatpak" "orca" "lswt" "zellij" "tmux" "virsh" "qemu-img" "lsusb"
    "swtpm" "swtpm_setup" "wlr-randr" "colormgr" "podman" "rpm" "gtk-launch" "firefox" "greetd" "nmtui"
    "wrangler" "terraform" "node"
    # desktop-only tools with large closures (gcr/gtk3, perl Net-DBus): the desktop module installs them
    "gnome-keyring-daemon" "xdg-mime" "xdg-settings"
  ];

  # The FHS table, applied in this order to every text file in $out. Longest and most specific first.
  fhsRules = [
    # Session PATH strings (hypr session.lua, labwc autostart): the NixOS equivalents of /usr/local/bin:/usr/bin.
    [ "/usr/local/bin:/usr/bin:/usr/local/sbin" "/run/wrappers/bin:/run/current-system/sw/bin" ]
    # Absolute paths that are load-bearing (root reads, security checks): pinned.
    [ "/usr/bin/head" "${coreutils}/bin/head" ]
    [ "/usr/bin/bootctl" "${systemd}/bin/bootctl" ]
    [ "/usr/bin/systemd-cryptenroll" "${systemd}/bin/systemd-cryptenroll" ]
    [ "/usr/sbin/cryptsetup" "${cryptsetup}/bin/cryptsetup" ]
    [ "/usr/sbin/unix_chkpwd" "/run/wrappers/bin/unix_chkpwd" ]
    [ "/usr/share/X11/xkb" "${xkeyboard_config}/share/X11/xkb" ]
    [ "/usr/libexec/at-spi-bus-launcher" "${at-spi2-core}/libexec/at-spi-bus-launcher" ]
    [ "/usr/libexec/at-spi2-registryd" "${at-spi2-core}/libexec/at-spi2-registryd" ]
    # Where NixOS keeps what Fedora keeps under /usr/lib.
    [ "/usr/lib/os-release" "/etc/os-release" ]
    [ "/usr/lib/NetworkManager/VPN" "/etc/NetworkManager/VPN" ]
    [ "/usr/lib/NetworkManager/conf.d" "/etc/NetworkManager/conf.d" ]
    [ "/usr/lib/NetworkManager/dispatcher.d" "/etc/NetworkManager/dispatcher.d" ]
    [ "/usr/lib64/NetworkManager" "${networkmanager}/lib/NetworkManager" ]
    [ "/usr/libexec/virtiofsd" "/run/current-system/sw/bin/virtiofsd" ]
    # Paths another area defines: the session set (desktop: the display manager's sessions, not a profile path)
    # and the Rime.I18n module root (rime-shell package: lib/qt-6/qml, linked through the default /lib).
    [ "/usr/share/wayland-sessions" "/etc/rime/wayland-sessions" ]
    [ "/usr/lib64/rime-shell/qml" "/run/current-system/sw/lib/qt-6/qml" ]
    [ "/usr/bin/qemu-system-x86_64" "/run/libvirt/nix-emulators/qemu-system-x86_64" ]
    # Everything else of the system: the system profile.
    [ "/usr/libexec/" "/run/current-system/sw/libexec/" ]
    [ "/usr/share/" "/run/current-system/sw/share/" ]
    [ "/usr/lib64/" "/run/current-system/sw/lib/" ]
    [ "/usr/lib/" "/run/current-system/sw/lib/" ]
    [ "/usr/sbin/" "/run/current-system/sw/bin/" ]
    [ "/usr/bin/" "/run/current-system/sw/bin/" ]
    # The bare directories (search lists, XDG_DATA_DIRS fallbacks, zsh's sudo completion path).
    [ "/usr/libexec\\b" "/run/current-system/sw/libexec" ]
    [ "/usr/share\\b" "/run/current-system/sw/share" ]
    [ "/usr/sbin\\b" "/run/current-system/sw/bin" ]
    [ "/usr/bin\\b" "/run/current-system/sw/bin" ]
  ];

  sedScript = lib.concatMapStrings (r: "s#${lib.elemAt r 0}#${lib.elemAt r 1}#g\n") fhsRules;

  pathFor = name: lib.makeBinPath (common ++ (deps.${name} or (throw "rime-libexec: no deps entry for ${name}")));
in
stdenvNoCC.mkDerivation {
  pname = "rime-libexec";
  version = "0.1.0";

  src = lib.fileset.toSource {
    inherit root;
    fileset = lib.fileset.unions [
      (root + "/files/system/libexec")
      (root + "/files/system/bin")
      (root + "/files/system/compat")
      (root + "/files/system/system-sleep")
      (root + "/files/system/profile.d")
      (root + "/files/system/nftables")
      (root + "/files/system/firewall")
      (root + "/files/system/rime-secrets-provider")
      (root + "/files/system/shared-machine")
      (root + "/files/system/xdg")
      (root + "/files/system/flatpak")
      (root + "/files/desktop")
    ];
  };

  dontBuild = true;
  dontPatchShebangs = true; # done in installPhase, before the FHS table runs

  buildInputs = [ bash python3 nodejs-slim ];
  nativeInstallCheckInputs = [ shfmt python3 ];
  doInstallCheck = true;

  passthru = {
    inherit dropped runtime;
    # services.displayManager.sessionPackages wants this (desktop worker).
    providedSessions = [ "niri" "rime-gaming" "rime-labwc" "rime-safe-graphics" ];
  };

  installPhase = ''
    runHook preInstall
    S=files/system D=files/desktop

    # ── scripts ───────────────────────────────────────────────────────────
    mkdir -p $out/libexec/rime/tools $out/bin
    for f in $S/libexec/*; do
      n=''${f##*/}
      case " ${lib.concatStringsSep " " dropped} " in *" $n "*) continue ;; esac
      install -m0755 "$f" "$out/libexec/$n"
    done
    install -m0644 $S/libexec/rime-plugin-verdict.js $out/libexec/rime-plugin-verdict.js
    install -m0755 $S/system-sleep/rime-wifi-resume $out/libexec/rime-wifi-resume
    install -m0755 $S/compat/apex $out/bin/apex
    install -m0755 $S/bin/steamos-session-select $S/bin/steamos-update $out/bin/
    # the agent-session shims: the broker reads which tool it is from its own name
    ln -s ../../rime-tool-broker $out/libexec/rime/tools/wrangler
    ln -s ../../rime-tool-broker $out/libexec/rime/tools/terraform

    # ── data the scripts and sessions read (bootc: /usr/share/rime*, /etc) ──
    mkdir -p $out/share/rime/nftables $out/share/rime/shell $out/share/rime/zsh $out/share/rime/starship \
      $out/share/rime/shared-machine $out/share/wayland-sessions $out/share/fish/vendor_conf.d \
      $out/share/fish/vendor_completions.d $out/share/nushell/vendor/autoload $out/etc/profile.d $out/share/rime/xdg \
      $out/etc/mpv $out/etc/flatpak
    cp $S/nftables/rime.nft $out/share/rime/nftables/
    cp -r $S/firewall/services $out/share/rime/firewall-services
    mkdir -p $out/share/rime/firewall && mv $out/share/rime/firewall-services $out/share/rime/firewall/services
    cp $S/rime-secrets-provider $out/share/rime/secrets-provider
    cp $S/shared-machine/* $out/share/rime/shared-machine/
    cp $S/profile.d/*.sh $out/etc/profile.d/
    # NOT $out/etc/xdg or $out/share/polkit-1: system-path links /etc/xdg and polkit links /share/polkit-1 for
    # every system package (polkit's datadir is /run/current-system/sw/share), so files there would be live on
    # every machine, desktop or not. The xdg files are data here; base.nix installs xdg-terminals.list when the
    # desktop is on, and mimeapps.list is the desktop area's (xdg.mime). The sing-box polkit rule is the security
    # area's (security.polkit.extraConfig).
    cp $S/xdg/xdg-terminals.list $out/share/rime/xdg/
    cp $S/flatpak/flathub.flatpakrepo $out/etc/flatpak/
    cp -r $D/hypr $out/share/rime/hypr
    cp -r $D/labwc $out/share/rime/labwc
    cp -r $D/safe-graphics $out/share/rime/safe-graphics
    cp -r $D/rime-greet $out/share/rime-greet
    cp $D/zsh/zshrc $out/share/rime/zsh/zshrc
    cp $D/starship/starship.toml $out/share/rime/starship/
    cp $D/shell/rime-greeting.sh $out/share/rime/shell/greeting.sh
    cp $D/shell/agent.sh $out/share/rime/shell/agent.sh
    cp $D/bash/rime-starship.sh $D/zsh/rime-zsh-highlight.sh $out/etc/profile.d/
    cp $D/wayland-sessions/*.desktop $out/share/wayland-sessions/
    cp $D/fish/rime-agent.fish $out/share/fish/vendor_conf.d/
    cp $D/fish/completions/*.fish $out/share/fish/vendor_completions.d/
    cp $D/nushell/rime.nu $out/share/nushell/vendor/autoload/
    cp $D/xdg/mimeapps.list $out/share/rime/xdg/
    cp $D/mpv/mpv.conf $out/etc/mpv/

    # ── 1. interpreters ───────────────────────────────────────────────────
    patchShebangs --host $out/libexec $out/bin

    # ── 2. the FHS table, on every text file (comments too: they describe this system now) ──
    cat > fhs.sed <<'RIME_SED'
    ${sedScript}
    RIME_SED
    # rime-user names a login shell; /bin/zsh does not exist on NixOS, /run/current-system/sw/bin/zsh does.
    sed -E -i 's#(^|[ "=])/bin/(zsh|bash)\b#\1/run/current-system/sw/bin/\2#g' $out/libexec/rime-user
    grep -rIl . $out | while IFS= read -r f; do
      [ -L "$f" ] && continue
      sed -i -f fhs.sed "$f"
    done

    # ── 3. each script's PATH ────────────────────────────────────────────
    ${lib.concatMapStrings (name: let
        file = if lib.elem name [ "apex" "steamos-session-select" "steamos-update" ] then "$out/bin/${name}" else "$out/libexec/${name}";
      in ''
        ${python3}/bin/python3 ${./rime-libexec-audit.py} inject "${file}" "${pathFor name}" ${if lib.elem name closed then "closed" else "open"}
      '') (lib.attrNames deps)}

    runHook postInstall
  '';

  installCheckPhase = ''
    runHook preInstallCheck
    shopt -s nullglob
    scripts=()
    for f in $out/libexec/* $out/bin/*; do
      [ -f "$f" ] && [ ! -L "$f" ] && scripts+=("$f")
    done
    # every script in the deps table exists, and every script that exists is in the deps table
    for f in "''${scripts[@]}"; do
      n=''${f##*/}
      case "$n" in rime-plugin-verdict.js) continue ;; esac
      case " ${lib.concatStringsSep " " (lib.attrNames deps)} " in *" $n "*) ;; *) echo "FAIL: $n has no deps entry"; exit 1 ;; esac
    done
    # syntax
    for f in "''${scripts[@]}"; do
      case "$(head -c 200 "$f" | head -n1)" in
        *python*) ${python3}/bin/python3 -c 'import ast,sys; ast.parse(open(sys.argv[1]).read(), sys.argv[1])' "$f" ;;
        *node*)   ${nodejs-slim}/bin/node --check "$f" ;;
        *)        ${bash}/bin/bash -n "$f" ;;
      esac
    done
    # every command resolves
    ${python3}/bin/python3 ${./rime-libexec-audit.py} audit --shfmt ${shfmt}/bin/shfmt --runtime "${lib.concatStringsSep "," runtime}" \
      --report $out/share/rime/libexec-audit.json "''${scripts[@]}"
    # no bootc path is left (/usr/local/* is allowed: search-list entries that are absent on NixOS)
    left="$(grep -rInoE '/usr/(bin|sbin|libexec|lib64|lib|share)\b[^ "'"'"'):;]*' $out || true)"
    if [ -n "$left" ]; then
      echo "FAIL: FHS paths left in the output:"; echo "$left"; exit 1
    fi
    test "$(readlink -f $out/libexec/rime/tools/wrangler)" = "$(readlink -f $out/libexec/rime-tool-broker)"
    # nothing under a directory NixOS links and reads for every system package (see the data section)
    for d in etc/xdg share/polkit-1; do
      if [ -e "$out/$d" ]; then echo "FAIL: $out/$d would be live on every machine"; exit 1; fi
    done
    runHook postInstallCheck
  '';

  meta = {
    description = "Rime helper scripts and the system data they read";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
  };
}
