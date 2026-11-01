"""Tests for rime-terminal-migrate (run: python3 -m unittest discover -s files/desktop/ghostty/tests).

RIME_MIGRATE names the script under test (the mutant runner points it at a mutated copy); RIME_THEME the shipped
theme. Each fixture directory holds an Alacritty config (Andre's two real ones, sanitised of nothing because they
hold nothing secret, and synthetic ones for both schema generations, legacy YAML and imports) and expected.txt:
the exact --convert output.
"""

import hashlib
import os
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.environ.get("RIME_MIGRATE") or os.path.join(HERE, "..", "rime-terminal-migrate")
THEME = os.environ.get("RIME_THEME") or os.path.join(HERE, "..", "rime-os")
FIXTURES = os.path.join(HERE, "fixtures")

HEADER = (
    "# Ghostty settings. Rime wrote this file once, at your first login with Ghostty as the\n"
    "# default terminal, and will never change it: it is yours to edit.\n"
    "# Rime's look (font, colours, padding) comes from the theme; every line below it wins.\n"
    "# What was carried over from Alacritty, and what was not: ~/.local/state/rime/terminal-migration.txt\n"
    "theme = rime-os\n"
)


def run(home, *args, env_extra=None):
    env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": home}
    env.update(env_extra or {})
    return subprocess.run([sys.executable, SCRIPT, "--theme", THEME, *args], env=env, capture_output=True,
                          text=True, check=False)


def snapshot(root):
    """Every file under root: path -> (sha256, mtime_ns)."""
    out = {}
    for d, _, files in os.walk(root):
        for f in files:
            p = os.path.join(d, f)
            if os.path.islink(p):
                out[p] = ("link", os.readlink(p))
                continue
            with open(p, "rb") as fh:
                out[p] = (hashlib.sha256(fh.read()).hexdigest(), os.stat(p).st_mtime_ns)
    return out


def read(p):
    with open(p, encoding="utf-8") as f:
        return f.read()


class Convert(unittest.TestCase):
    def test_fixtures_exact(self):
        names = sorted(os.listdir(FIXTURES))
        self.assertEqual(names, ["full", "imports", "katana", "l16", "legacy-yml", "schema-013"])
        for name in names:
            with self.subTest(fixture=name):
                d = os.path.join(FIXTURES, name)
                src = os.path.join(d, "alacritty.toml")
                if not os.path.exists(src):
                    src = os.path.join(d, "alacritty.yml")
                r = run("/home/alex", "--convert", src)
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertEqual(r.stdout, read(os.path.join(d, "expected.txt")))

    def test_defaults_are_not_copied(self):
        # every Alacritty default, written out explicitly, produces no Ghostty line at all
        with tempfile.TemporaryDirectory() as t:
            p = os.path.join(t, "alacritty.toml")
            with open(p, "w") as f:
                f.write(
                    '[font]\nsize = 11.25\nnormal = { family = "monospace" }\n'
                    '[colors.primary]\nbackground = "#181818"\nforeground = "#D8D8D8"\n'
                    '[colors.cursor]\ntext = "CellBackground"\ncursor = "CellForeground"\n'
                    '[colors.normal]\nred = "0xac4242"\n[colors.bright]\nwhite = "#f8f8f8"\n'
                    '[window]\npadding = { x = 0, y = 0 }\nopacity = 1.0\ndecorations = "Full"\n'
                    'startup_mode = "Windowed"\n[scrolling]\nhistory = 10000\nmultiplier = 3\n'
                    '[cursor]\nstyle = { shape = "Block", blinking = "Off" }\n'
                    '[selection]\nsave_to_clipboard = false\n[mouse]\nhide_when_typing = false\n')
            r = run(t, "--convert", p)
            self.assertEqual(r.returncode, 0, r.stderr)
            lines, report = r.stdout.split("----\n", 1)
            self.assertEqual(lines, "")
            self.assertIn("Carried over:\n  (nothing)", report)
            self.assertIn('font.size = 11.25', report)

    def test_padding_is_converted_to_points(self):
        with tempfile.TemporaryDirectory() as t:
            p = os.path.join(t, "alacritty.toml")
            with open(p, "w") as f:
                f.write("[window]\npadding = { x = 12, y = 1 }\n")
            r = run(t, "--convert", p)
            self.assertIn("window-padding-x = 9\nwindow-padding-y = 1\n", r.stdout)


class Migrate(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.home = self.tmp.name
        self.cfg = os.path.join(self.home, ".config")
        self.ghostty = os.path.join(self.cfg, "ghostty")
        self.state = os.path.join(self.home, ".local", "state", "rime")

    def tearDown(self):
        self.tmp.cleanup()

    def alacritty(self, fixture):
        d = os.path.join(self.cfg, "alacritty")
        os.makedirs(d, exist_ok=True)
        shutil.copy(os.path.join(FIXTURES, fixture, "alacritty.toml"), os.path.join(d, "alacritty.toml"))
        return d

    def test_no_alacritty_gets_rime_look_only(self):
        r = run(self.home)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(read(os.path.join(self.ghostty, "config.ghostty")), HEADER)
        self.assertEqual(read(os.path.join(self.ghostty, "themes", "rime-os")), read(THEME))
        self.assertIn("No Alacritty config found", read(os.path.join(self.state, "terminal-migration.txt")))
        self.assertTrue(os.path.exists(os.path.join(self.state, "terminal-migrated")))
        self.assertFalse(os.path.exists(os.path.join(self.ghostty, "config")))

    def test_katana_config_and_alacritty_untouched(self):
        d = self.alacritty("katana")
        before = snapshot(d)
        r = run(self.home)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(
            read(os.path.join(self.ghostty, "config.ghostty")),
            HEADER + "\n# From Alacritty (~/.config/alacritty/alacritty.toml)\nkeybind = shift+enter=text:\\x1b\\r\n")
        self.assertEqual(snapshot(d), before)
        report = read(os.path.join(self.state, "terminal-migration.txt"))
        self.assertIn("Migrated: 1 setting(s) carried over, 0 not.", report)
        self.assertIn("-> keybind = shift+enter=text:\\x1b\\r", report)

    def test_runs_once(self):
        self.alacritty("katana")
        run(self.home)
        cfg = os.path.join(self.ghostty, "config.ghostty")
        first = snapshot(self.home)
        # the user changes Alacritty afterwards, and deletes nothing: a later login still changes nothing
        self.alacritty("full")
        after_change = snapshot(self.home)
        r = run(self.home)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("already migrated", r.stderr)
        self.assertEqual(snapshot(self.home), after_change)
        self.assertEqual(first[cfg], after_change[cfg])

    def test_existing_ghostty_config_is_never_touched(self):
        self.alacritty("full")
        os.makedirs(self.ghostty)
        mine = os.path.join(self.ghostty, "config.ghostty")
        with open(mine, "w") as f:
            f.write("# mine\nfont-size = 15\n")
        before = snapshot(self.ghostty)
        r = run(self.home)
        self.assertEqual(r.returncode, 0, r.stderr)
        after = snapshot(self.ghostty)
        self.assertEqual(after[mine], before[mine])
        self.assertFalse(os.path.exists(os.path.join(self.ghostty, "config")))
        report = read(os.path.join(self.state, "terminal-migration.txt"))
        self.assertIn("NOT migrated: ~/.config/ghostty/config.ghostty already has settings", report)
        self.assertIn("theme = rime-os", report)
        self.assertTrue(os.path.exists(os.path.join(self.state, "terminal-migrated")))

    def test_legacy_config_name_counts_too(self):
        os.makedirs(self.ghostty)
        mine = os.path.join(self.ghostty, "config")
        with open(mine, "w") as f:
            f.write("theme = something\n")
        run(self.home)
        self.assertFalse(os.path.exists(os.path.join(self.ghostty, "config.ghostty")))
        self.assertEqual(read(mine), "theme = something\n")

    def test_comment_only_ghostty_template_is_left_and_other_name_written(self):
        self.alacritty("katana")
        os.makedirs(self.ghostty)
        tmpl = os.path.join(self.ghostty, "config.ghostty")
        with open(tmpl, "w") as f:
            f.write("# This is the configuration file for Ghostty.\n\n# font-size = 12\n")
        before = snapshot(self.ghostty)[tmpl]
        run(self.home)
        self.assertEqual(snapshot(self.ghostty)[tmpl], before)
        self.assertTrue(read(os.path.join(self.ghostty, "config")).startswith(HEADER))

    def test_both_names_comment_only_are_left_reported_and_stamped(self):
        # Nothing to write into without editing a Ghostty file: skip, say so, and do not retry every login.
        self.alacritty("katana")
        os.makedirs(self.ghostty)
        for n in ("config.ghostty", "config"):
            with open(os.path.join(self.ghostty, n), "w") as f:
                f.write("# This is the configuration file for Ghostty.\n")
        before = snapshot(self.ghostty)
        r = run(self.home)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertNotIn("error:", r.stderr)
        self.assertEqual({k: v for k, v in snapshot(self.ghostty).items() if "/themes/" not in k}, before)
        report = read(os.path.join(self.state, "terminal-migration.txt"))
        self.assertIn("NOT migrated: ~/.config/ghostty/config.ghostty and ~/.config/ghostty/config both exist", report)
        self.assertIn("theme = rime-os", report)
        self.assertTrue(os.path.exists(os.path.join(self.state, "terminal-migrated")))

    def test_xdg_config_home(self):
        alt = os.path.join(self.home, "elsewhere")
        os.makedirs(os.path.join(alt, "alacritty"))
        shutil.copy(os.path.join(FIXTURES, "katana", "alacritty.toml"), os.path.join(alt, "alacritty", "alacritty.toml"))
        run(self.home, env_extra={"XDG_CONFIG_HOME": alt})
        self.assertIn("keybind = shift+enter", read(os.path.join(alt, "ghostty", "config.ghostty")))
        self.assertFalse(os.path.exists(self.ghostty))

    def test_broken_alacritty_config_never_fails_login(self):
        d = os.path.join(self.cfg, "alacritty")
        os.makedirs(d)
        with open(os.path.join(d, "alacritty.toml"), "w") as f:
            f.write("[window\nopacity = \n")
        r = run(self.home)
        self.assertEqual(r.returncode, 0)
        self.assertEqual(read(os.path.join(self.ghostty, "config.ghostty")),
                         HEADER + "")
        self.assertIn("could not be read (TOMLDecodeError", read(os.path.join(self.state, "terminal-migration.txt")))

    def test_unwritable_config_never_fails_login(self):
        # ~/.config is a file: nothing can be created under it, not even as root
        with open(self.cfg, "w") as f:
            f.write("not a directory\n")
        r = run(self.home)
        self.assertEqual(r.returncode, 0)
        self.assertIn("rime-terminal-migrate: error: NotADirectoryError", r.stderr)

    def test_theme_seed_refresh_and_keep(self):
        themes = os.path.join(self.ghostty, "themes")
        target = os.path.join(themes, "rime-os")
        run(self.home)
        self.assertEqual(read(target), read(THEME))
        # an older seeded copy (marked) is refreshed, every login
        with open(target, "w") as f:
            f.write("# rime-os fallback: old\nfont-size = 9\n")
        r = run(self.home)
        self.assertIn("theme: refreshed", r.stderr)
        self.assertEqual(read(target), read(THEME))
        # the Shell's rendered theme (no mark) is kept
        with open(target, "w") as f:
            f.write("# Rime Shell\nbackground = #000000\n")
        r = run(self.home)
        self.assertIn("theme: kept", r.stderr)
        self.assertEqual(read(target), "# Rime Shell\nbackground = #000000\n")
        # deleted: seeded again
        os.unlink(target)
        run(self.home)
        self.assertEqual(read(target), read(THEME))


if __name__ == "__main__":
    unittest.main()
