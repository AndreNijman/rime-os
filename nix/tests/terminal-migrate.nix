# Ghostty is the default terminal and every user's Alacritty setup carries over once (nix/modules/terminal.nix,
# files/desktop/ghostty/rime-terminal-migrate). Three users log in (their systemd user manager starts, which is what
# a graphical login does) and the test checks:
#   - alice (Andre's katana alacritty.toml): config.ghostty is written once with `theme = rime-os` and her binding,
#     the theme is seeded, Ghostty itself accepts the result, and her Alacritty files are untouched;
#   - bob (already has a config.ghostty): it is left byte for byte, nothing else is written beside it, and the
#     report says why;
#   - carol (no Alacritty, no Ghostty config): gets Rime's look only;
#   - a second login changes nothing for anyone (same sha256 and mtime for every file);
#   - xdg-terminal-exec picks Ghostty, $TERMINAL is ghostty, and Alacritty is still installed.
{ pkgs, self }:

let
  fixtures = ../../files/desktop/ghostty/tests/fixtures;
  users = [ "alice" "bob" "carol" ];
in
pkgs.testers.runNixOSTest {
  name = "rime-terminal-migrate";

  node.pkgsReadOnly = false;

  nodes.machine = { lib, ... }: {
    imports = [ self.nixosModules.default ];
    # Only the terminal part: the migration is a user unit and needs no desktop session.
    rime.terminal.enable = true;

    users.users = lib.genAttrs users (name: {
      isNormalUser = true;
      uid = { alice = 1000; bob = 1001; carol = 1002; }.${name};
    });
  };

  testScript = ''
    HEADER = (
        "# Ghostty settings. Rime wrote this file once, at your first login with Ghostty as the\n"
        "# default terminal, and will never change it: it is yours to edit.\n"
        "# Rime's look (font, colours, padding) comes from the theme; every line below it wins.\n"
        "# What was carried over from Alacritty, and what was not: ~/.local/state/rime/terminal-migration.txt\n"
        "theme = rime-os\n"
    )
    UIDS = {"alice": 1000, "bob": 1001, "carol": 1002}

    def as_user(user, cmd):
        return f"sudo -u {user} env HOME=/home/{user} XDG_RUNTIME_DIR=/run/user/{UIDS[user]} {cmd}"

    def put(user, path, content):
        # A file the user owns, written as the user.
        machine.succeed(as_user(user, f"mkdir -p $(dirname /home/{user}/{path})"))
        machine.succeed(f"cat > /home/{user}/{path} <<'RIME_EOF'\n{content}RIME_EOF\nchown {user}: /home/{user}/{path}")

    def snapshot(user):
        # Every file in the home: path, sha256, mtime (ns).
        return machine.succeed(
            f"cd /home/{user} && find . -type f -printf '%p %T@ ' -exec sha256sum {{}} \\; | sort"
        )

    def login(user):
        machine.succeed(f"systemctl start user@{UIDS[user]}.service")

    def logout(user):
        machine.succeed(f"systemctl stop user@{UIDS[user]}.service")

    def runs(user):
        # How many times the unit has finished for this user (each run logs exactly one "theme:" line).
        return int(machine.succeed(
            f"journalctl _UID={UIDS[user]} -o cat | grep -c '^rime-terminal-migrate: theme: ' || true"
        ).strip())

    machine.wait_for_unit("multi-user.target")
    katana = open("${fixtures}/katana/alacritty.toml").read()
    full = open("${fixtures}/full/alacritty.toml").read()
    theme = machine.succeed("cat /run/current-system/sw/share/rime/ghostty/rime-os")

    with subtest("before the first login"):
        put("alice", ".config/alacritty/alacritty.toml", katana)
        put("bob", ".config/alacritty/alacritty.toml", full)
        put("bob", ".config/ghostty/config.ghostty", "font-size = 15\n")
        alice_alacritty = machine.succeed("sha256sum /home/alice/.config/alacritty/alacritty.toml")
        bob_before = machine.succeed("stat -c '%Y %s' /home/bob/.config/ghostty/config.ghostty; sha256sum /home/bob/.config/ghostty/config.ghostty")

    with subtest("first login runs the carry-over"):
        for u in UIDS:
            login(u)
        for u in UIDS:
            machine.wait_until_succeeds(f"test -e /home/{u}/.local/state/rime/terminal-migrated", timeout=60)
            machine.wait_until_succeeds(f"[ $(journalctl _UID={UIDS[u]} -o cat | grep -c '^rime-terminal-migrate: theme: ') -ge 1 ]", timeout=60)
            machine.wait_until_succeeds(as_user(u, "sh -c '[ $(systemctl --user show -p ActiveState --value rime-terminal-migrate.service) = inactive ]'"), timeout=60)
            status = machine.succeed(as_user(u, "systemctl --user show -p Result --value rime-terminal-migrate.service")).strip()
            assert status == "success", f"{u}: unit result {status}"

    with subtest("alice: her Alacritty binding, Rime's theme, Alacritty untouched"):
        cfg = machine.succeed("cat /home/alice/.config/ghostty/config.ghostty")
        want = HEADER + "\n# From Alacritty (~/.config/alacritty/alacritty.toml)\nkeybind = shift+enter=text:\\x1b\\r\n"
        assert cfg == want, f"alice config.ghostty:\n{cfg!r}\nwant:\n{want!r}"
        assert machine.succeed("cat /home/alice/.config/ghostty/themes/rime-os") == theme
        machine.succeed("[ $(stat -c %U /home/alice/.config/ghostty/config.ghostty) = alice ]")
        machine.succeed("test ! -e /home/alice/.config/ghostty/config")
        assert machine.succeed("sha256sum /home/alice/.config/alacritty/alacritty.toml") == alice_alacritty
        report = machine.succeed("cat /home/alice/.local/state/rime/terminal-migration.txt")
        print(report)
        assert "Migrated: 1 setting(s) carried over, 0 not." in report, report
        # Ghostty itself reads her config, the theme it names and the binding, with no error
        out = machine.succeed(as_user("alice", "ghostty +validate-config 2>&1"))
        assert out.strip() == "", f"ghostty +validate-config: {out}"

    with subtest("bob: an existing Ghostty config is never touched"):
        after = machine.succeed("stat -c '%Y %s' /home/bob/.config/ghostty/config.ghostty; sha256sum /home/bob/.config/ghostty/config.ghostty")
        assert after == bob_before, f"{bob_before} -> {after}"
        machine.succeed("test ! -e /home/bob/.config/ghostty/config")
        report = machine.succeed("cat /home/bob/.local/state/rime/terminal-migration.txt")
        assert "NOT migrated: ~/.config/ghostty/config.ghostty already has settings" in report, report
        assert "theme = rime-os" in report, report

    with subtest("carol: no Alacritty config, Rime's look only"):
        assert machine.succeed("cat /home/carol/.config/ghostty/config.ghostty") == HEADER
        assert "No Alacritty config found" in machine.succeed("cat /home/carol/.local/state/rime/terminal-migration.txt")
        out = machine.succeed(as_user("carol", "ghostty +validate-config 2>&1"))
        assert out.strip() == "", f"ghostty +validate-config: {out}"

    with subtest("a second login changes nothing"):
        before = {u: snapshot(u) for u in UIDS}
        counts = {u: runs(u) for u in UIDS}
        for u in UIDS:
            logout(u)
            login(u)
        for u in UIDS:
            machine.wait_until_succeeds(f"[ $(journalctl _UID={UIDS[u]} -o cat | grep -c '^rime-terminal-migrate: theme: ') -gt {counts[u]} ]", timeout=60)
            machine.succeed(f"journalctl _UID={UIDS[u]} -o cat | grep -F 'rime-terminal-migrate: already migrated'")
            after = snapshot(u)
            assert after == before[u], f"{u} changed on the second login:\n{before[u]}\n->\n{after}"

    with subtest("Ghostty is the default terminal, Alacritty the alternative"):
        assert machine.succeed("head -n1 /etc/xdg/xdg-terminals.list").strip() == "com.mitchellh.ghostty.desktop"
        assert machine.succeed("sed -n 2p /etc/xdg/xdg-terminals.list").strip() == "Alacritty.desktop"
        assert machine.succeed(as_user("alice", "xdg-terminal-exec --print-id")).strip() == "com.mitchellh.ghostty.desktop"
        assert machine.succeed("su - alice -c 'printenv TERMINAL'").strip() == "ghostty"
        machine.succeed("command -v alacritty && command -v ghostty")
  '';
}
