# Rime's package catalogue (PLAN.md 4.5 + D5): what `rime install <name>` does on NixOS.
#
# Pure data. No pkgs: nix/modules/packages-catalogue.nix resolves the `attrs` of class 2 entries to packages and
# renders the rest, as it is, to /etc/rime/catalogue.json, which `rime` reads (rimed/rime/src/nixos/pkg.rs).
#
# There is no nixpkgs evaluation on the device (D5), so every name resolves to one of five kinds:
#
#   flatpak   install this Flathub application (`flatpak install --system`). `remote` defaults to flathub.
#   toggle    a class 1 package: it needs system configuration (units, udev rules, 32-bit graphics, setuid wrappers),
#             so it is part of a prebuilt system FEATURE (see `features`). Installing it selects that feature for
#             this machine (/var/lib/rime/switch/rime-feature@<feature>.target.on). A system that already carries the feature turns it on at
#             once; one that does not gets it from the update that brings the feature's system (D2 host layer, D6).
#   nix       a class 2 leaf app: CI builds `attrs` into one closure, publishes it in the release (manifest
#             `catalogue[]`), and the machine fetches it from the cache into the rime-apps profile. An entry with no
#             `attrs` is not built yet and is refused with `pending` as the reason.
#   user-npm  a per-user npm install into ~/.local (claude-code, D5: it stays on its npm route and runs under nix-ld).
#   refuse    never installable this way; `reason` is printed as it is.
#
# Resolution order (D5): a reverse-DNS application id goes straight to Flathub; a bare name is looked up here
# (through `aliases` first); a bare name not here and matching no `refuse` pattern is looked up on Flathub by exact
# name; anything else is refused. `--source capsule` hands the names to `rime env` unchanged.
#
# Every target `rime install` handled on bootc is here: the curated Flathub table (rime-pkg curated_source), the
# vendor routes (chatgpt, claude-desktop, claude-code), the apps #83 moved out of the image (Zed, golang, java, rust),
# the gaming and virtualization hints (rime gaming, rime-vm), the AI runtime (rime ai, llama-cpp), the names the docs
# use, and the RPM names on this project's own machines (blender chromium gamemode gamescope helium-bin libgcc.i686
# libreoffice libSM.i686 mangohud obs-studio steam steam-devices wine).
let
  desktops = import ../desktops;
in
{
  schema = 1;

  # Prebuilt system features that class 1 packages belong to. `option` is the NixOS option that turns the feature on in
  # a system; the module reads it to say whether the running system carries the feature. `offered` = false means no
  # Rime release builds a system with it yet, and `reason` says why.
  features = {
    gaming = {
      title = "Gaming";
      option = [ "rime" "gaming" "enable" ];
      offered = true;
      summary = "Steam, Rime Gaming Mode in the greeter, gamescope, gamemode, NTSYNC and 32-bit graphics";
    };
    virtualization = {
      title = "Virtual machines";
      option = [ "rime" "virtualization" "enable" ];
      offered = true;
      summary = "QEMU/KVM, libvirt, OVMF, swtpm and virtiofsd for rime vm (nix/modules/virtualization.nix)";
    };
  }
  # One feature per desktop in nix/desktops. `group`: a machine selects at most one feature of a group (D3); the
  # release builds no variant with two (nix/release/variants.nix), and `rime install` refuses the second by name.
  // builtins.mapAttrs (id: d: {
    inherit (d) title summary;
    option = [ "rime" "desktops" id "enable" ];
    offered = true;
    group = "desktop";
  }) desktops;

  entries = {
    # ── class 1: features ─────────────────────────────────────────────────────────────────────────────────────────
    steam = { kind = "toggle"; feature = "gaming"; summary = "Steam (with Rime Gaming Mode)"; };
    gamescope = { kind = "toggle"; feature = "gaming"; summary = "gamescope, the Gaming Mode compositor"; };
    gamemode = { kind = "toggle"; feature = "gaming"; summary = "Feral gamemode, driven by Rime's game mode"; };
    steam-devices = { kind = "toggle"; feature = "gaming"; summary = "udev rules for Steam controllers and VR"; };
    qemu = { kind = "toggle"; feature = "virtualization"; summary = "QEMU/KVM"; };
    libvirt = { kind = "toggle"; feature = "virtualization"; summary = "libvirt"; };
    edk2-ovmf = { kind = "toggle"; feature = "virtualization"; summary = "UEFI firmware for virtual machines"; };
    swtpm = { kind = "toggle"; feature = "virtualization"; summary = "software TPM for virtual machines"; };
    virtiofsd = { kind = "toggle"; feature = "virtualization"; summary = "shared folders for virtual machines"; };

    # ── Flathub (A4: blender, chromium, libreoffice, obs move to Flathub; the curated bootc table) ───────────────
    blender = { kind = "flatpak"; id = "org.blender.Blender"; summary = "Blender"; why = "Rime ships Blender from Flathub"; };
    chromium = { kind = "flatpak"; id = "org.chromium.Chromium"; summary = "Chromium"; why = "Rime ships Chromium from Flathub"; };
    libreoffice = { kind = "flatpak"; id = "org.libreoffice.LibreOffice"; summary = "LibreOffice"; why = "Rime ships LibreOffice from Flathub"; };
    obs-studio = { kind = "flatpak"; id = "com.obsproject.Studio"; summary = "OBS Studio"; why = "Rime ships OBS Studio from Flathub"; };
    discord = {
      kind = "flatpak";
      id = "com.discordapp.Discord";
      summary = "Discord";
      why = "Discord refuses to start once it considers itself out of date; the Flathub build updates on its own schedule";
    };
    zed = { kind = "flatpak"; id = "dev.zed.Zed"; summary = "Zed"; why = "Zed's Flathub build is the one that tracks every release"; };
    zen = { kind = "flatpak"; id = "app.zen_browser.zen"; summary = "Zen Browser"; why = "Zen's Flathub build is the one that tracks every release"; };
    code = { kind = "flatpak"; id = "com.visualstudio.code"; summary = "Visual Studio Code"; why = "Rime ships Visual Studio Code from Flathub"; };
    gimp = { kind = "flatpak"; id = "org.gimp.GIMP"; summary = "GIMP"; why = "Rime ships GIMP from Flathub"; };
    spotify = { kind = "flatpak"; id = "com.spotify.Client"; summary = "Spotify"; why = "Rime ships Spotify from Flathub"; };

    # ── class 2: leaf apps CI builds into the rime-apps profile ───────────────────────────────────────────────────
    go = { kind = "nix"; attrs = [ "go" ]; summary = "the Go toolchain"; };
    rust = { kind = "nix"; attrs = [ "rustc" "cargo" "rustfmt" "clippy" ]; summary = "the Rust toolchain (rustc, cargo, rustfmt, clippy)"; };
    java = { kind = "nix"; attrs = [ "jdk25" ]; summary = "OpenJDK 25, the newest JDK (bootc shipped java-latest-openjdk-devel)"; };
    wine = { kind = "nix"; attrs = [ "wineWow64Packages.stable" ]; summary = "Wine (WoW64: 32-bit Windows programs without 32-bit host libraries)"; };
    mangohud = { kind = "nix"; attrs = [ "mangohud" ]; summary = "MangoHud overlay (64-bit games)"; };
    llama-cpp = { kind = "nix"; attrs = [ "llama-cpp-vulkan" ]; summary = "llama.cpp with Vulkan, the runtime rime ai uses"; };
    neovim = { kind = "nix"; attrs = [ "neovim" ]; summary = "Neovim"; };
    htop = { kind = "nix"; attrs = [ "htop" ]; summary = "htop"; };
    android-tools = { kind = "nix"; attrs = [ "android-tools" ]; summary = "adb and fastboot"; };
    wireguard-tools = { kind = "nix"; attrs = [ "wireguard-tools" ]; summary = "wg and wg-quick"; };
    usbutils = { kind = "nix"; attrs = [ "usbutils" ]; summary = "lsusb"; };
    clang = { kind = "nix"; attrs = [ "clang" ]; summary = "the Clang C/C++ compiler"; };
    # Equibop is the Discord client Gaming Mode's Rich Presence runs hidden (rime-gaming-discord); nixpkgs' build is a
    # plain native one, which that helper starts with no Flatpak socket dance. Rime's build (nix/pkgs/rime-equibop.nix)
    # takes Equicord from nixpkgs instead of downloading it at first start: no app updates itself outside rime update.
    equibop = { kind = "nix"; attrs = [ "rime.rime-equibop" ]; summary = "Equibop, a Discord client (Rich Presence in Gaming Mode)"; };
    # Vendor apps (D5): Rime derivations that keep the pinned vendor-key check of rime-pkg (vendor_fetch_*):
    # nix/pkgs/rime-vendor-apps (one pinned version each, verified in the build; pins.nix says how to bump). nixpkgs'
    # chatgpt is not used as it is because it takes OpenAI's deb on its sha256 alone; claude-desktop and helium are not
    # in nixpkgs.
    chatgpt = { kind = "nix"; attrs = [ "rime.rime-chatgpt" ]; summary = "ChatGPT desktop app"; };
    claude-desktop = { kind = "nix"; attrs = [ "rime.rime-claude-desktop" ]; summary = "Claude Desktop"; };
    helium = {
      kind = "nix";
      attrs = [ ];
      summary = "Helium browser";
      pending = "Helium is in neither nixpkgs nor Flathub, and Rime's own build of it does not exist yet; Firefox stays the browser";
    };

    # ── per-user ──────────────────────────────────────────────────────────────────────────────────────────────────
    claude-code = { kind = "user-npm"; package = "@anthropic-ai/claude-code"; summary = "Claude Code, into your own ~/.local"; };

    # ── refused by name ───────────────────────────────────────────────────────────────────────────────────────────
    "libgcc.i686" = { kind = "refuse"; reason = "32-bit libraries are not installed one at a time on Rime; Steam's come with the gaming system: rime install steam"; };
    "libSM.i686" = { kind = "refuse"; reason = "32-bit libraries are not installed one at a time on Rime; Steam's come with the gaming system: rime install steam"; };
    waydroid = { kind = "refuse"; reason = "Waydroid needs system configuration (the binder driver and a system service) that no Rime system carries yet"; };
  }
  # Desktops (nix/desktops): `rime install gnome` (or an alias, `plasma`) selects the desktop's feature.
  // builtins.listToAttrs (builtins.concatLists (builtins.attrValues (builtins.mapAttrs (id: d:
    map (n: { name = n; value = { kind = "toggle"; feature = id; summary = "the ${d.title} desktop"; }; }) ([ id ] ++ d.aliases)
  ) desktops)));

  # Other names people type for the same thing. Keys are matched case-insensitively.
  aliases = {
    golang = "go";
    rustc = "rust";
    cargo = "rust";
    java-latest-openjdk = "java";
    java-latest-openjdk-devel = "java";
    openjdk = "java";
    jdk = "java";
    obs = "obs-studio";
    vscode = "code";
    zen-browser = "zen";
    helium-bin = "helium";
    llama = "llama-cpp";
    llama-cpp-vulkan = "llama-cpp";
    nvim = "neovim";
    qemu-kvm = "qemu";
    libvirt-daemon-kvm = "libvirt";
    libvirt-client = "libvirt";
    swtpm-tools = "swtpm";
    ovmf = "edk2-ovmf";
    claude = "claude-code";
  };

  # Bare names that are part of the operating system (rime-pkg REFUSE_RE), as globs (`*` at either end).
  refuse = [
    {
      reason = "32-bit libraries are not installed one at a time on Rime; Steam's come with the gaming system: rime install steam";
      patterns = [ "*.i686" ];
    }
    {
      reason = "it is part of the operating system and changes only with a system update (sudo rime update)";
      patterns = [
        "kernel" "kernel-*" "linux" "linux-*" "kmod-*" "akmod-*" "*-kmod" "glibc" "glibc-*" "systemd" "systemd-*"
        "nix" "nixos-*" "rpm" "rpm-*" "librpm*" "dnf" "dnf5" "libdnf5*" "bootc" "ostree" "libostree*" "rpm-ostree"
        "filesystem" "setup" "shadow-utils" "bash" "coreutils" "selinux-policy*" "grub2-*" "grub" "shim*" "dracut*"
        "plymouth*" "nvidia*" "mesa*" "pipewire*" "hyprland" "quickshell" "rime*"
      ];
    }
  ];
}
