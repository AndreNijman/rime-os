"""Mutation check for rime-terminal-migrate: each mutant breaks one promise, and the suite must notice.

Run: python3 files/desktop/ghostty/tests/mutants.py   (exit 1 when a mutant survives or no longer applies)
"""

import os
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "..", "rime-terminal-migrate")

MUTANTS = [
    ("copies values equal to Alacritty's defaults",
     "    if path not in DEFAULTS:\n        return False\n",
     "    if True:\n        return False\n"),
    ("overwrites an existing Ghostty config",
     "    with_settings = [n for n in existing if has_settings(os.path.join(ghostty_dir, n))]\n",
     "    with_settings = []\n"),
    ("runs again after the stamp",
     "    if os.path.exists(stamp):\n",
     "    if False:\n"),
    ("replaces instead of appending imported arrays",
     "            out[k] = out[k] + v\n",
     "            out[k] = v\n"),
    ("pixels copied as points",
     "        return int(px * 0.75 + 0.5)",
     "        return int(px)"),
    ("escapes control characters wrongly",
     '"\\r": "\\\\r"',
     '"\\r": "\\\\n"'),
    ("overwrites the Shell's rendered theme",
     "    if have.startswith(FALLBACK_MARK):\n",
     "    if True:\n"),
    ("fails the login on an error",
     '        log(f"error: {exc.__class__.__name__}: {exc}")\n    return 0\n',
     '        log(f"error: {exc.__class__.__name__}: {exc}")\n        return 1\n    return 0\n'),
    ("legacy key names not translated",
     '    "return": "enter",\n',
     ""),
    ("no free config name: errors out and retries every login",
     "    if with_settings or not free:\n",
     "    if with_settings:\n"),
    ("old schema's top-level shell ignored",
     '    if "shell" in doc:\n',
     '    if False:\n'),
]


def main():
    with open(SCRIPT, encoding="utf-8") as f:
        original = f.read()
    survived = 0
    for name, old, new in MUTANTS:
        if original.count(old) != 1:
            print(f"STALE   {name}: the text to mutate is not in the script exactly once")
            survived += 1
            continue
        with tempfile.NamedTemporaryFile("w", suffix="-rime-terminal-migrate", delete=False) as m:
            m.write(original.replace(old, new))
        try:
            env = dict(os.environ, RIME_MIGRATE=m.name)
            r = subprocess.run([sys.executable, "-m", "unittest", "discover", "-s", HERE], env=env,
                               capture_output=True, text=True, check=False)
        finally:
            os.unlink(m.name)
        if r.returncode == 0:
            print(f"SURVIVED {name}")
            survived += 1
        else:
            print(f"killed   {name}")
    print(f"{len(MUTANTS) - survived}/{len(MUTANTS)} mutants killed")
    return 1 if survived else 0


if __name__ == "__main__":
    sys.exit(main())
