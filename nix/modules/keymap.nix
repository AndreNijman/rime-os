# Console keymaps for the LUKS passphrase prompt (PLAN.md D2 keymap gate, R19, S1, Q16; p2-gates M1/M2).
#
# The system closure and its initrd are built once for every machine, so they keep console.keyMap = "us". A machine's
# own console keymap is host.toml [keyboard] console (D2 host layer), and it reaches the initrd at boot:
#
#   host.toml [keyboard] console = "gb"
#     -> rime-boot (nix/pkgs/rime-boot) appends rd.vconsole.keymap=gb to every menu entry (a keymap other than "us")
#     -> the initrd's systemd-vconsole-setup reads it from the kernel command line, which wins over the initrd's
#        /etc/vconsole.conf (KEYMAP=us), and loads it from the initrd's /etc/kbd/keymaps before the passphrase is
#        asked for (systemd-ask-password-console and plymouth-start are ordered after systemd-vconsole-setup);
#     -> stage 2: rime-hostlayer apply writes /etc/vconsole.conf (KEYMAP=gb) from the same table.
#
# What this module adds is the keymaps themselves. nixpkgs' console module copies the WHOLE keymap tree into the
# initrd (console.earlySetup off: /etc/kbd/keymaps = consoleEnv's share/keymaps), so a keymap outside the configured
# one is there, but nixpkgs' kbd has none of the names bootc's installer writes from an XKB layout ("gb", "ch",
# "us-dvorak", ...; Fedora's kbd ships them under keymaps/xkb). rime-kbd-xkb converts every XKB layout the way
# Fedora's kbd.spec does and console.packages adds it to both trees. checks.keymap-luks types a gb and a ch passphrase
# at the initrd prompt as raw keycodes; it fails when the initrd uses the us map.
{ config, lib, pkgs, ... }:

let
  cfg = config.rime.keymap;
in
{
  options.rime.keymap = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Ship console keymaps under the names bootc's installer writes (Fedora kbd's xkb/ tree), in /etc/kbd and in
        the initrd, so host.toml's [keyboard] console keymap loads at the LUKS passphrase prompt.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    console.packages = [ pkgs.rime.rime-kbd-xkb ];
  };
}
