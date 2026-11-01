# What a user sees on a Rime OS desktop that the foundation left unchecked (nix/STATUS.md "polish" items).
#
#   machine   a fresh account's first graphical login: the wallpaper is PAINTED (not only saved), the
#             boot/shutdown splash follows the owner's accent, the Shell's Qt picks right-to-left for an
#             RTL language, the hotspot dispatcher opens and closes its firewall set, Ctrl+Alt+F2 is a
#             kmscon console beside the greeter, thermald stays out of a VM, `rime agent enable` leaves a
#             link that survives an update, and rime-hypr-migrate recognises an APEX-seeded config as
#             spelled on disk.
#   migrated  a home carried over from a bootc machine (PLAN.md D1/D2): wallpaper.json names the image's
#             /usr/share/backgrounds/apex/ default and ~/.config/systemd/user/default.target.wants holds
#             apex-agentd.service. The wallpaper must come back from this system's copy, and the old
#             enablement link must still start the agent runtime.
#   mutant    the same configuration with each mechanism switched off (apex aliases, kmscon, the hotspot
#             dispatcher, thermald's VM condition). Every check above that has a switch is run here too
#             and must FAIL, so none of them passes vacuously.
{ pkgs, self }:

let
  # The APEX-seeded hyprland.conf the migrator's own self-test uses, taken from the SOURCE file, so it keeps the
  # /usr spelling a config on disk has (the packaged copy of the script is rewritten to the system profile).
  apexHyprSample = pkgs.runCommand "apex-hyprland-sample" { nativeBuildInputs = [ pkgs.python3 ]; } ''
    python3 -I -c '
    import re, sys
    s = open(sys.argv[1]).read()
    m = re.search(r"SAMPLE = \"\"\"\\\n(.*?)\"\"\"", s, re.S)
    open(sys.argv[2], "w").write(m.group(1))
    ' ${../../files/system/libexec/rime-hypr-migrate} $out
    grep -F /usr/libexec/apex-open-browser $out >/dev/null # rime-rename: keep (the spelling under test)
  '';

  # Qt.application.layoutDirection inside the Shell's own engine (the quickshell binary the running Shell is, with its
  # environment whole, so its platform theme, QML import path and Rime.I18n too), for a given locale.
  # $1 = the Shell's pid, $2 = locale, $3 = "noi18n" leaves out `import Rime.I18n` (the control: no Qt catalogue).
  # Qt decides the direction by translating QT_LAYOUT_DIRECTION, so only a process with qt_<lang>.qm installed mirrors.
  rtlProbe = pkgs.writeShellScriptBin "rime-rtl-probe" ''
    set -u
    pid="$1" loc="$2" mode="''${3:-i18n}"
    qs="$(readlink "/proc/$pid/exe")"
    d="$(mktemp -d)"; trap 'rm -rf "$d"' EXIT
    {
      echo 'import QtQuick'
      echo 'import Quickshell'
      [ "$mode" = noi18n ] || echo 'import Rime.I18n'
      echo 'ShellRoot { Timer { interval: 300; running: true; onTriggered: { console.warn("RIMEDIR=" + Qt.application.layoutDirection); Qt.quit() } } }'
    } > "$d/shell.qml"
    args=()
    while IFS= read -r -d "" e; do
      case "$e" in
        LANG=*|LC_*|LANGUAGE=*|WAYLAND_DISPLAY=*|DISPLAY=*|HYPRLAND_INSTANCE_SIGNATURE=*|QT_QPA_PLATFORM=*) ;;
        *) args+=("$e") ;;
      esac
    done < "/proc/$pid/environ"
    env -i "''${args[@]}" LANG="$loc" LC_ALL="$loc" QT_QPA_PLATFORM=offscreen QT_FORCE_STDERR_LOGGING=1 \
      timeout 60 "$qs" -p "$d/shell.qml" 2>&1 \
      | sed -n 's/.*RIMEDIR=\([0-9]*\).*/\1/p' | head -n 1
  '';

  base =
    { lib, ... }:
    {
      imports = [
        self.nixosModules.default
        ../hosts/vm.nix
      ];
      users.users.rime.uid = 1000;
      systemd.services.rime-flathub-setup.enable = lib.mkForce false;
    };

  graphical = {
    virtualisation.qemu.options = [
      "-vga none"
      "-device virtio-gpu-pci,xres=1280,yres=800"
    ];
    environment.sessionVariables = {
      WLR_RENDERER = "pixman";
      QT_QUICK_BACKEND = "software";
    };
  };
in
pkgs.testers.runNixOSTest {
  name = "rime-polish";
  node.pkgsReadOnly = false;

  nodes.machine = {
    imports = [ base graphical ];
    environment.systemPackages = [ rtlProbe ];
    # not the default, so the console's layout can only have come from the configuration
    services.xserver.xkb.layout = "de";
  };

  nodes.migrated = {
    imports = [ base graphical ];
  };

  nodes.mutant =
    { lib, ... }:
    {
      imports = [ base ];
      virtualisation.memorySize = lib.mkForce 2048;
      rime.compat.apexNames.enable = false;
      services.kmscon.enable = lib.mkForce false;
      networking.networkmanager.dispatcherScripts = lib.mkForce [ ];
      systemd.services.thermald.unitConfig.ConditionVirtualization = lib.mkForce "!container";
    };

  testScript = ''
    import shlex

    def as_rime(cmd):
        return "su - rime -c " + shlex.quote("export XDG_RUNTIME_DIR=/run/user/1000; " + cmd)

    def poll(node, cmd, tries=60, delay=0.5):
        return node.execute(f"for i in $(seq {tries}); do ( {cmd} ) && exit 0; sleep {delay}; done; exit 1")[0] == 0

    # ── the checks; each returns (passed, what it saw) ─────────────────────────
    def thermald_kept_out_of_vm(node):
        poll(node, "! systemctl show thermald.service -p ActiveState | grep -E 'activating' >/dev/null", 40)
        out = node.succeed("systemctl show thermald.service -p LoadState -p ConditionResult -p ActiveState -p Result")
        return ("LoadState=loaded" in out and "ConditionResult=no" in out and "Result=success" in out
                and "ActiveState=failed" not in out), out

    def kmscon_on_tty2(node):
        node.send_key("ctrl-alt-f2")
        ok = poll(node, "systemctl is-active --quiet kmsconvt@tty2.service")
        seen = node.execute("systemctl list-units --all --no-legend 'kmsconvt@*' 'getty@*' 'autovt@*' greetd.service; fgconsole")[1]
        node.send_key("ctrl-alt-f1")
        return ok, seen

    def hotspot_dispatcher(node):
        node.wait_for_unit("rime-firewall.service")
        node.wait_for_unit("NetworkManager.service")
        node.succeed("nmcli con add type dummy ifname rhs0 con-name rime-hs autoconnect no "
                     "ipv4.method shared ipv4.addresses 10.42.99.1/24 ipv6.method disabled")
        up = node.execute("nmcli -w 20 con up rime-hs")
        has = "nft list set inet rime hotspot_ifaces | grep -w rhs0 >/dev/null"
        added = poll(node, has, 40)
        node.execute("nmcli -w 20 con down rime-hs")
        removed = poll(node, "! " + has, 40)
        seen = up[1] + node.execute("nft list set inet rime hotspot_ifaces")[1]
        node.succeed("nmcli con delete rime-hs")
        return (added and removed), seen

    def apex_agentd_resolves(node):
        out = node.succeed(as_rime("systemctl --user show apex-agentd.service -p Id -p LoadState; "
                                   "systemctl --user show default.target -p Wants"))
        ok = ("Id=rime-agentd.service" in out and "LoadState=loaded" in out
              and "rime-agentd.service" in out.split("Wants=", 1)[1])
        return ok, out

    def apexd_resolves(node):
        out = node.succeed("systemctl show apexd.service -p Id -p LoadState")
        return ("Id=rimed.service" in out and "LoadState=loaded" in out), out

    def seed_migrated_wants(node):
        node.succeed("install -d -o rime -g users /home/rime/.config /home/rime/.config/systemd "
                     "/home/rime/.config/systemd/user /home/rime/.config/systemd/user/default.target.wants")
        # what `systemctl --user enable apex-agentd` left on the bootc image: a link by the OLD name to a file
        # that does not exist here. rime-rename: keep
        node.succeed("ln -s /usr/lib/systemd/user/apex-agentd.service "
                     "/home/rime/.config/systemd/user/default.target.wants/apex-agentd.service")

    def graphical_login(node):
        node.wait_until_succeeds("pgrep -u greetd -f 'qs -p .*/rime-greet/shell.qml'", timeout=180)
        node.sleep(5)
        node.send_chars("rime\n")
        node.sleep(2)
        node.send_chars("rime\n")
        node.wait_until_succeeds("ls /run/user/1000/hypr/*/.socket.sock", timeout=240)

    def wayland_display(node):
        return node.succeed("ls /run/user/1000 | grep -E '^wayland-[0-9]+$' | head -n 1").strip()

    def painted(node, want=None, timeout=120):
        """awww-daemon is SHOWING an image (not merely a saved choice), and apply() left ~/.curr_wall."""
        wl = wayland_display(node)
        q = f"WAYLAND_DISPLAY={wl} awww query"
        # awww reports the file it opened, symlinks resolved: /run/current-system/sw/... shows as its store path
        image = f"image: $(readlink -f {want})" if want else "image: "
        cond = q + f" | grep -F \"{image}\" >/dev/null && test -e ~/.curr_wall"
        ok = poll(node, as_rime(cond), timeout * 2)
        seen = node.execute(as_rime(q + "; readlink ~/.curr_wall; cat ~/.config/rime-shell/src/user_data/wallpaper.json"))[1]
        return ok, seen

    def plymouth_accent(node, themes=None):
        """The owner's accent picks the ridge theme for its hue slot (accents.json: slot n = hue n*15 deg, #3B82F6 is
        217 deg -> 14), rewrites the real-file conf the shutdown splash reads and leaves the ESP credential the initrd
        reads; the theme is a RIDGE theme (plane 3 recoloured: lockup-accent.png, the ridge script), not the spark."""
        env = "RIME_GREET_VAR=/tmp/pa/g RIME_PLYMOUTH_CONF=/tmp/pa/p.conf RIME_ESP_DIRS=/tmp/pa/esp"
        if themes:
            env += " RIME_PLYMOUTH_THEMES=" + themes
        node.succeed("rm -rf /tmp/pa && mkdir -p /tmp/pa/g/accents /tmp/pa/esp && echo rime > /tmp/pa/g/last-user "
                     "&& printf '#3B82F6' > /tmp/pa/g/accents/rime")
        node.succeed(f"{env} /run/current-system/sw/libexec/rime-plymouth-accent")
        conf = node.succeed("cat /tmp/pa/p.conf")
        cred = node.execute("cat /tmp/pa/esp/loader/credentials/rime.accent.cred")[1]
        t = "/etc/plymouth/themes/rime-os-accent-14"
        ok = ("Theme=rime-os-accent-14\n" in conf and cred == "14"
              and node.execute(f"test -f {t}/rime-os-accent-14.plymouth && test -f {t}/lockup-accent.png "
                               f"&& grep -x 'ScriptFile={t}/rime-os.script' {t}/rime-os-accent-14.plymouth")[0] == 0)
        return ok, conf + " cred=" + cred

    def plymouth_default(node, themes=None):
        """After plymouth_accent: the owner's accent goes away (unset) and the splash goes back to the default ridge,
        rime-os (plane 3 in Rime Blue), and the ESP credential is removed so the next boot starts rime-os too."""
        env = "RIME_GREET_VAR=/tmp/pa/g RIME_PLYMOUTH_CONF=/tmp/pa/p.conf RIME_ESP_DIRS=/tmp/pa/esp"
        if themes:
            env += " RIME_PLYMOUTH_THEMES=" + themes
        node.succeed("rm -f /tmp/pa/g/accents/rime")
        node.succeed(f"{env} /run/current-system/sw/libexec/rime-plymouth-accent")
        conf = node.succeed("cat /tmp/pa/p.conf")
        cred = node.execute("test -e /tmp/pa/esp/loader/credentials/rime.accent.cred && echo present || echo gone")[1]
        ok = ("Theme=rime-os\n" in conf and cred.strip() == "gone"
              and node.execute("test -f /etc/plymouth/themes/rime-os/rime-os.plymouth "
                               "&& test -f /etc/plymouth/themes/rime-os/lockup-accent.png")[0] == 0)
        return ok, conf + " cred " + cred

    def expect(label, result, want=True):
        ok, seen = result
        print(f"--- {label}: {'pass' if ok else 'FAIL'} (want {'pass' if want else 'FAIL'})\n{seen}")
        assert ok == want, f"{label}: expected {'pass' if want else 'failure'}, got {'pass' if ok else 'failure'}"

    start_all()

    # ── mutant: every switchable check must fail without its mechanism ─────────
    mutant.wait_for_unit("multi-user.target")
    seed_migrated_wants(mutant)
    mutant.succeed("systemctl start user@1000.service")
    expect("mutant: apex-agentd without the aliases", apex_agentd_resolves(mutant), want=False)
    expect("mutant: apexd without the aliases", apexd_resolves(mutant), want=False)
    expect("mutant: thermald without the VM condition", thermald_kept_out_of_vm(mutant), want=False)
    expect("mutant: hotspot without the dispatcher", hotspot_dispatcher(mutant), want=False)
    expect("mutant: Ctrl+Alt+F2 without kmscon", kmscon_on_tty2(mutant), want=False)

    # ── migrated: the bootc home's old names ────────────────────────────────────
    migrated.wait_for_unit("multi-user.target")
    seed_migrated_wants(migrated)
    migrated.succeed(
        "install -d -o rime -g users /home/rime/.config/rime-shell /home/rime/.config/rime-shell/src "
        "/home/rime/.config/rime-shell/src/user_data /home/rime/Pictures /home/rime/Pictures/Wallpapers",
        # the bootc image's saved choice and its daemon link, both now naming nothing. rime-rename: keep
        "printf '%s\\n' '{\"currentWall\":\"/usr/share/backgrounds/apex/default.jpg\",\"wallpaperDir\":\"~/Pictures/Wallpapers\",\"scheme\":\"content\"}' "
        "> /home/rime/.config/rime-shell/src/user_data/wallpaper.json",
        "ln -s /usr/share/backgrounds/apex/default.jpg /home/rime/.curr_wall",
        "chown -h rime:users /home/rime/.curr_wall /home/rime/.config/rime-shell/src/user_data/wallpaper.json",
    )
    expect("migrated: apexd is rimed", apexd_resolves(migrated))
    graphical_login(migrated)
    expect("migrated: the old wallpaper path comes back from this system's copy",
           painted(migrated, "/run/current-system/sw/share/backgrounds/rime/default.jpg"))
    # saveConfig() runs after the image is up, so the saved choice can lag the screen by a moment
    migrated.wait_until_succeeds("grep -F /run/current-system/sw/share/backgrounds/rime/default.jpg "
                                 "/home/rime/.config/rime-shell/src/user_data/wallpaper.json", timeout=60)
    expect("migrated: the apex-agentd enablement link starts the agent runtime", apex_agentd_resolves(migrated))
    migrated.wait_until_succeeds(as_rime("systemctl --user is-active rime-agentd.service"), timeout=60)

    # ── machine: a fresh account ────────────────────────────────────────────────
    machine.wait_for_unit("multi-user.target")
    expect("machine: thermald is kept out of a VM", thermald_kept_out_of_vm(machine))
    print(machine.execute("systemctl --failed --no-legend")[1])
    machine.succeed("test -f /etc/plymouth/plymouthd.conf && ! test -L /etc/plymouth/plymouthd.conf")
    # nothing published yet (no owner): the real conf names the default ridge, Rime Blue
    machine.succeed("grep -x 'Theme=rime-os' /etc/plymouth/plymouthd.conf")
    expect("machine: the splash follows the owner's accent", plymouth_accent(machine))
    expect("machine: an unset accent restores the Rime Blue ridge", plymouth_default(machine))
    expect("machine: no accent themes -> the default splash", plymouth_accent(machine, "/nonexistent"), want=False)
    expect("machine: no ridge themes -> no Rime Blue ridge", plymouth_default(machine, "/nonexistent"), want=False)

    graphical_login(machine)
    expect("machine: the first login paints the wallpaper", painted(machine))
    machine.sleep(20)
    machine.screenshot("desktop-later")
    print(machine.succeed("ls -la /home/rime/.curr_wall; journalctl -b _UID=1000 --no-pager "
                          "| grep -i -E 'never applied|was renamed' || true"))
    # the real conf, after the session published the owner's accent (rime-greet-wallpaper -> rime-plymouth-accent)
    machine.wait_until_succeeds("grep -E '^Theme=rime-os-accent-[0-9]{2}$' /etc/plymouth/plymouthd.conf", timeout=90)
    theme = machine.succeed("sed -n 's/^Theme=//p' /etc/plymouth/plymouthd.conf").strip()
    machine.succeed(f"test -f /etc/plymouth/themes/{theme}/{theme}.plymouth && test -f /etc/plymouth/themes/{theme}/lockup-accent.png")
    print("live plymouth theme: " + theme + " from accent " + machine.succeed("cat /var/lib/rime-greet/accents/rime"))

    # the Shell's Qt: right-to-left for Arabic and Hebrew, left-to-right for English and for Urdu (Qt ships no
    # qt_ur.qm, so it stays LTR anywhere: the catalogue, not the locale, decides), and only through Rime.I18n, which
    # installs Qt's catalogue (NixOS's qt6ct links no KDE library, so the platform theme loads none)
    ps = machine.succeed("ps -u rime -o pid=,args=")
    shell_pid = next(l.split(None, 1)[0] for l in ps.splitlines() if "quickshell" in l and "rime-shell" in l)
    environ = machine.succeed(f"tr '\\0' '\\n' < /proc/{shell_pid}/environ")
    assert "QT_QPA_PLATFORMTHEME=qt6ct" in environ, environ
    probe = lambda loc, mode="i18n": machine.succeed(f"runuser -u rime -- rime-rtl-probe {shell_pid} {loc} {mode}").strip()
    rtl = {loc: probe(loc) for loc in ("ar_EG.UTF-8", "he_IL.UTF-8", "en_US.UTF-8", "ur_PK.UTF-8")}
    rtl["ar_EG.UTF-8 without Rime.I18n"] = probe("ar_EG.UTF-8", "noi18n")
    print(f"layoutDirection (1 = RightToLeft): {rtl}")
    assert rtl == {"ar_EG.UTF-8": "1", "he_IL.UTF-8": "1", "en_US.UTF-8": "0", "ur_PK.UTF-8": "0",
                   "ar_EG.UTF-8 without Rime.I18n": "0"}, rtl

    # rime-hypr-migrate on a config spelled as an APEX image wrote it (D2): its defaults are recognised, not
    # carried over as user binds that name paths this system does not have
    machine.succeed("rm -rf /tmp/hm && install -d -o rime -g users /tmp/hm /tmp/hm/.config /tmp/hm/.config/hypr "
                    "&& install -o rime -g users -m 0644 ${apexHyprSample} /tmp/hm/.config/hypr/hyprland.conf")
    lua = machine.succeed("runuser -u rime -- /run/current-system/sw/libexec/rime-hypr-migrate --home /tmp/hm --dry-run")
    live = [l for l in lua.splitlines() if l.startswith("hl.") or l.strip().startswith("hl.")]
    print("\n".join(live))
    for gone in ("apex-open-browser", "apex-shell/src/scripts", "apex-shell-autostart"):  # rime-rename: keep
        assert not any(gone in l for l in live), f"an APEX default came back as a user line: {gone}"
    for kept in ('exec_cmd("kitty")', 'exec_cmd("gimp")', 'exec_cmd("my-own-daemon --flag")'):
        assert any(kept in l for l in live), f"a real user edit was lost: {kept}"

    # `rime agent enable`: what plain systemctl links, and what the command leaves behind
    wants = "~/.config/systemd/user/default.target.wants/rime-agentd.service"
    print("plain systemctl --user enable links: " + machine.succeed(as_rime(
        f"systemctl --user enable rime-agentd.service >/dev/null 2>&1; readlink {wants}; "
        "systemctl --user is-enabled rime-agentd.service; systemctl --user disable rime-agentd.service >/dev/null 2>&1; true")))
    machine.succeed(as_rime("rime agent enable"))
    link = machine.succeed(as_rime(f"readlink {wants}")).strip()
    print("rime agent enable links: " + link)
    assert link == "/etc/systemd/user/rime-agentd.service", link
    machine.succeed(as_rime("systemctl --user is-active rime-agentd.service"))

    expect("machine: hotspot dispatcher opens and closes the set", hotspot_dispatcher(machine))
    expect("machine: Ctrl+Alt+F2 is a kmscon console", kmscon_on_tty2(machine))
    machine.succeed("systemctl is-active greetd.service")
    machine.fail("systemctl is-active kmsconvt@tty1.service")
    # the console keyboard is the configured layout (bootc: 50-rime-keymap / localed; NixOS: services.xserver.xkb)
    conf = machine.succeed("d=$(systemctl show kmsconvt@tty2.service -p ExecStart --value "
                           "| sed -n 's/.*--configdir \\([^ ;]*\\).*/\\1/p'); cat \"$d/kmscon.conf\"")
    print(conf)
    assert "xkb-layout=de" in conf.splitlines(), conf
  '';
}
