# checks.keymap-luks: the console keymap gate of PLAN.md D2 (p2-gates M1, M2; R19, Q16).
#
# One VM, Rime's modules, the desktop's prompt path (plymouth in the systemd initrd, `splash`), a LUKS2 volume on
# /dev/vdb that the INITRD unlocks (rd.luks.name=, the way a migrated machine's root is named on its kernel command
# line). The keymap reaches the initrd the product way: host.toml [keyboard] console -> rime-boot writes
# rd.vconsole.keymap=<it> into the generation's menu entry -> the test boots with exactly that entry's parameter.
#
#   gb   passphrase a@b"c#d, typed as raw key codes (a, shift+apostrophe, b, shift+2, c, backslash, d). With the us
#        map the same keys give a"b@c\d.
#   ch   passphrase zy-'+*, typed as y, z, slash, minus, shift+1, shift+3 (with the us map: yz/-!#).
#   us   negative control in the same run: no rd.vconsole.keymap, the gb key codes are REFUSED (a second ask), then
#        the us key codes of the same passphrase unlock it: the passphrase is right, only the layout differed.
#
# A test-only initrd unit reports every passphrase ask on the serial console with systemd-vconsole-setup's state at
# that moment, and writes it to /run (kept across switch-root) so stage 2 can count the asks.
#
# The test types only when the guest says the keys will be READ: the report waits for the keymap load to finish AND
# for plymouth to hold the question ("queuing password request with boot splash" in its trace, streamed to /run by
# plymouth.debug=stream:). Plymouth has no type-ahead: a key that arrives before it holds a question is dropped. On
# the 4-vCPU hosted runner (run 41400977973) the ask existed at 4.3 s, the keys went at 4.9 s and plymouth started
# at 5.45 s: every key was lost and the test waited 300 s for an answer that was never typed. Waiting for state,
# not time, holds under any load (proved with the VM pinned to 2 host CPUs under load: docs/nixos/ci.md).
#
# mutant = "initrd-us": the initrd's keymap tree is nixpkgs' kbd alone (no rime-kbd-xkb), i.e. the initrd keeps the
# us map. The gb unlock must FAIL.
# mutant = "type-early": the report does not wait for plymouth to hold the question (the check before 11-02). With the
# VM short of CPU it types before plymouth reads keys and FAILS (docs/nixos/ci.md, "keymap-luks under load").
# Each mutant is a passthru of the check: nix build path:.#checks.x86_64-linux.keymap-luks.mutants.<name>
{ pkgs, self, mutant ? null }:

let
  inherit (pkgs) lib;
  # key codes per layout: what the owner presses, as QEMU key names
  gbKeys = [ "a" "shift-apostrophe" "b" "shift-2" "c" "backslash" "d" ];
  gbAsUsKeys = [ "a" "shift-2" "b" "shift-apostrophe" "c" "shift-3" "d" ]; # a@b"c#d on the us map
  chKeys = [ "y" "z" "slash" "minus" "shift-1" "shift-3" ];

  mk = mutant: pkgs.testers.runNixOSTest {
    name = "rime-keymap-luks" + lib.optionalString (mutant != null) "-mutant-${mutant}";
    passthru.mutants = lib.optionalAttrs (mutant == null) (lib.genAttrs [ "initrd-us" "type-early" ] mk);

    node.pkgsReadOnly = false; # nixosModules.default sets nixpkgs.config (allowUnfree)

    nodes.machine = { config, pkgs, ... }: {
      imports = [ self.nixosModules.default ];
      virtualisation = {
        emptyDiskImages = [ 64 ];
        memorySize = 1536;
      };
      # The desktop's prompt path (nix/modules/desktop.nix: plymouth in the initrd, `quiet splash`), without the
      # desktop's closure. `quiet` is left out so the serial log keeps the initrd's messages.
      boot.plymouth.enable = true;
      # TEST ONLY: plymouth's trace, streamed live to a file the ask watcher reads (see the header).
      boot.kernelParams = [ "splash" "plymouth.debug=stream:/run/rime-test-plymouth.trace" ];
      # cryptsetup, its generator and the TPM2 token in the initrd without a declared device: per-machine volumes
      # are named on the kernel command line (rd.luks.name= / rd.luks.uuid=, host.toml [boot] kernel_params).
      boot.initrd.luks.forceLuksSupportInInitrd = true;
      environment.systemPackages = [ pkgs.cryptsetup pkgs.rime.rime-boot ];

      # TEST ONLY: report passphrase asks (serial + /run/rime-test-asks).
      boot.initrd.systemd.services.rime-test-askwatch = {
        description = "TEST: report LUKS passphrase asks on the serial console";
        wantedBy = [ "sysinit.target" ];
        unitConfig.DefaultDependencies = false;
        conflicts = [ "initrd-switch-root.target" ];
        before = [ "initrd-switch-root.target" ];
        serviceConfig.Type = "simple";
        script = ''
          tag=none km=none
          read -r cmdline < /proc/cmdline
          for w in $cmdline; do
            case "$w" in
              rime.testboot=*) tag=''${w#rime.testboot=} ;;
              rd.vconsole.keymap=*) km=''${w#rd.vconsole.keymap=} ;;
            esac
          done
          seen=" " n=0 unlocked=0
          # One scan per 0.2 s, and a last one when initrd-switch-root stops this unit (TERM): the poll alone can miss
          # a volume that opens just before switch-root (the TPM lab hit it), and the test waits for RIME-UNLOCKED.
          scan() {
            for f in /run/systemd/ask-password/ask.*; do
              [ -e "$f" ] || continue
              case "$seen" in *" $f "*) continue ;; esac
              seen="$seen$f "
              n=$((n + 1))
              # The test types as soon as it sees the report: hold it until the keymap load has finished (active or
              # failed), so a key typed before loadkeys ran cannot pass or fail the gate by timing. Bounded: 30 s.
              w=0
              while [ "$w" -lt 150 ]; do
                case "$(systemctl show -P ActiveState systemd-vconsole-setup.service)" in
                  activating|inactive|deactivating) sleep 0.2; w=$((w + 1)) ;;
                  *) break ;;
                esac
              done
              vc=$(systemctl show -P ActiveState systemd-vconsole-setup.service)/$(systemctl show -P Result systemd-vconsole-setup.service)
              # ... and until plymouth holds THIS question (its n-th password request): only then are keys read.
              # Bounded: 180 s, then reported as agent=none and the test fails by name.
              agent=none w=0
              while [ "$w" -lt ${if mutant == "type-early" then "0" else "900"} ]; do
                q=0
                if [ -r /run/rime-test-plymouth.trace ]; then
                  while IFS= read -r l; do
                    case "$l" in *"queuing password request with boot splash"*) q=$((q + 1)) ;; esac
                  done < /run/rime-test-plymouth.trace
                fi
                if [ "$q" -ge "$n" ]; then agent=plymouth; break; fi
                sleep 0.2; w=$((w + 1))
              done
              ${lib.optionalString (mutant == "type-early") "agent=plymouth"}
              line="RIME-ASK $tag $n keymap=$km vconsole-setup=$vc agent=$agent"
              echo "$line" > /dev/ttyS0
              echo "$line" >> /run/rime-test-asks
            done
            if [ "$unlocked" = 0 ] && [ -e /dev/mapper/cryptdata ]; then
              unlocked=1
              echo "RIME-UNLOCKED $tag after $n asks" > /dev/ttyS0
              echo "RIME-UNLOCKED $tag after $n asks" >> /run/rime-test-asks
            fi
          }
          trap 'scan; exit 0' TERM
          while :; do
            scan
            sleep 0.2
          done
        '';
      };

      boot.initrd.systemd.contents = lib.mkIf (mutant == "initrd-us") {
        "/etc/kbd/keymaps".source = lib.mkForce "${config.boot.initrd.systemd.package.kbd}/share/keymaps";
      };
    };

    testScript = ''
      import base64, datetime as dt, json, os, re

      GB = ${builtins.toJSON gbKeys}
      GB_AS_US = ${builtins.toJSON gbAsUsKeys}
      CH = ${builtins.toJSON chKeys}

      def put(path, text):
          machine.succeed(f"echo {base64.b64encode(text.encode()).decode()} | base64 -d > {path}")

      def derive(console):
          """host.toml [keyboard] console -> rime-boot install (real hook, real toplevel) -> the entry's options."""
          machine.succeed("rm -rf /tmp/rb && mkdir -p /tmp/rb/boot/grub2 /tmp/rb/profiles /tmp/rb/pins")
          machine.succeed("ln -s $(readlink -f /run/current-system) /tmp/rb/profiles/system-1-link")
          machine.succeed("ln -s system-1-link /tmp/rb/profiles/system")
          put("/tmp/rb/host.toml", f'[keyboard]\nconsole = "{console}"\n\n[[luks]]\nname = "cryptdata"\n'
                                   f'device = "UUID={uuid}"\n')
          put("/tmp/rb/cfg.json", json.dumps({
              "layout": "bls", "bootDir": "/tmp/rb/boot", "profileDir": "/tmp/rb/profiles", "profile": "system",
              "hostToml": "/tmp/rb/host.toml", "pinDir": "/tmp/rb/pins", "bootedSystem": "/run/booted-system",
              "configurationLimit": 1, "timeout": 1, "reserveMiB": 0, "grubPath": ""}))
          rc, out = machine.execute("rime-boot --config /tmp/rb/cfg.json install 2>&1")
          if rc != 0:
              return rc, out
          opts = machine.succeed("sed -n 's/^options //p' /tmp/rb/boot/loader/entries/rime-generation-1.conf")
          return rc, [w for w in opts.split() if w.startswith("rd.vconsole.keymap=")]

      def boot(tag, params):
          machine.shutdown()
          os.environ["QEMU_KERNEL_PARAMS"] = f"rime.testboot={tag} rd.luks.name={uuid}=cryptdata {params}".strip()
          machine.start()

      def tagged(tag):
          return [l for l in machine.get_console_log().splitlines()
                  if re.search(f"RIME-(ASK|UNLOCKED) {tag} ", l)]

      def ask(tag, n):
          """Wait for ask n, reported only once the keys will be read (see the header): plymouth holds it."""
          machine.wait_for_console_text(f"RIME-ASK {tag} {n} ", timeout=dt.timedelta(minutes=5))
          line = next(l for l in tagged(tag) if f"RIME-ASK {tag} {n} " in l)
          assert "agent=plymouth" in line, f"plymouth never held the question; typing now would be lost: {line}"
          return line

      def type_keys(keys):
          for k in keys:
              machine.send_key(k, delay=dt.timedelta(milliseconds=50))
          machine.send_key("ret")

      def outcome(tag, n):
          """After ask n: RIME-UNLOCKED, or ask n+1 (refused), whichever the initrd reports first."""
          machine.wait_for_console_text(f"RIME-(UNLOCKED {tag} |ASK {tag} {n + 1} )",
                                        timeout=dt.timedelta(minutes=5))
          return next(l[l.index("RIME-"):] for l in tagged(tag)
                      if f"RIME-UNLOCKED {tag} " in l or f"RIME-ASK {tag} {n + 1} " in l)

      def asks_file():
          return machine.succeed("cat /run/rime-test-asks")

      machine.start()
      machine.wait_for_unit("multi-user.target")

      with subtest("the keymaps bootc's installer writes exist on the system and in the initrd's tree"):
          for k in ("gb", "ch", "us-dvorak", "de-neo"):
              machine.succeed(f"find -L /etc/kbd/keymaps -name '{k}.map.gz' | grep .")

      with subtest("LUKS2 volume with the gb passphrase"):
          machine.succeed("printf %s 'a@b\"c#d' | cryptsetup luksFormat --type luks2 -q --pbkdf pbkdf2 "
                          "--pbkdf-force-iterations 1000 /dev/vdb -")
          uuid = machine.succeed("cryptsetup luksUUID /dev/vdb").strip()

      with subtest("rime-boot derives rd.vconsole.keymap from host.toml, and refuses an unknown keymap by name"):
          rc, got = derive("gb")
          assert rc == 0 and got == ["rd.vconsole.keymap=gb"], (rc, got)
          rc, out = derive("xx-no-such-keymap")
          assert rc == 3 and "passphrase prompt would read the us layout" in out, (rc, out)
          gb_param = "rd.vconsole.keymap=gb"

      with subtest("M1: gb passphrase typed as key codes unlocks at the initrd prompt"):
          boot("gb1", gb_param)
          line = ask("gb1", 1)
          print(line)
          assert "keymap=gb" in line, line
          type_keys(GB)
          res = outcome("gb1", 1)
          assert res.startswith("RIME-UNLOCKED gb1 after 1 asks"), f"the gb passphrase was refused: {res}"
          machine.wait_for_unit("multi-user.target")
          machine.succeed("test -b /dev/mapper/cryptdata")
          print(asks_file())
          vc = machine.succeed("journalctl -b -o cat -u systemd-vconsole-setup.service --no-pager || true")
          print(vc)

      with subtest("negative control (same run): no keymap parameter = the us map, the gb key codes are refused"):
          boot("us1", "")
          line = ask("us1", 1)
          assert "keymap=none" in line, line
          type_keys(GB)
          res = outcome("us1", 1)
          assert res.startswith("RIME-ASK us1 2 "), f"gb key codes were not refused with the us map: {res}"
          type_keys(GB_AS_US)
          res = outcome("us1", 2)
          assert res.startswith("RIME-UNLOCKED us1 after 2 asks"), res
          machine.wait_for_unit("multi-user.target")
          machine.succeed("test -b /dev/mapper/cryptdata")

      with subtest("M2: ch (QWERTZ, y/z swapped) passphrase typed as key codes unlocks at the initrd prompt"):
          machine.succeed("printf %s 'a@b\"c#d' > /tmp/old && printf %s \"zy-'+*\" > /tmp/new")
          machine.succeed("cryptsetup luksAddKey -q --pbkdf pbkdf2 --pbkdf-force-iterations 1000 "
                          "--key-file /tmp/old /dev/vdb /tmp/new")
          machine.succeed("cryptsetup luksRemoveKey -q /dev/vdb /tmp/old && rm -f /tmp/old /tmp/new")
          rc, got = derive("ch")
          assert rc == 0 and got == ["rd.vconsole.keymap=ch"], (rc, got)
          boot("ch1", got[0])
          line = ask("ch1", 1)
          print(line)
          assert "keymap=ch" in line, line
          type_keys(CH)
          res = outcome("ch1", 1)
          assert res.startswith("RIME-UNLOCKED ch1 after 1 asks"), f"the ch passphrase was refused: {res}"
          machine.wait_for_unit("multi-user.target")
          machine.succeed("test -b /dev/mapper/cryptdata")
          print(asks_file())
    '';
  };
in
{
  keymap-luks = mk mutant;
}
