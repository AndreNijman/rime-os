"""The greeting's choice table: which image, which protocol, label or image, with which arguments.

    python3 greeting_table_test.py GREETING_SH SHELL... [--expect-fail]

GREETING_SH is NixOS's greeting, rime-ridge-files' share/rime/shell/greeting.sh (or a mutant of it). It is copied with its system paths
pointed into a temporary root, the way rime-libexec's FHS table points them at the profile:

    /usr/libexec/  -> ROOT/libexec/    rime-term-graphics: a stub that prints $STUB_GRAPHICS
    /usr/share/    -> ROOT/share/      the shipped rime-symbol-{dark,light}-512.png
    /etc/fastfetch/ -> ROOT/etc/fastfetch/

fastfetch and gsettings are stubs on PATH: fastfetch writes its argv to a file, gsettings prints $STUB_SCHEME.
Each case sources the copy from an interactive SHELL on a pty (the greeting checks `$-` and `[ -t 1 ]`) and
reads back fastfetch's arguments, RIME_TERM_GRAPHICS, and the caller's positional parameters (the greeting is
sourced, so it must not touch them). Every case runs in every SHELL given (bash, zsh, dash).
"""
import os
import pty
import select
import shutil
import subprocess
import sys
import tempfile
import time

GREETING = sys.argv[1]
SHELLS = [a for a in sys.argv[2:] if not a.startswith("--")]
EXPECT_FAIL = "--expect-fail" in sys.argv[2:]

failures = []
count = 0


def setup(root):
    os.makedirs(f"{root}/libexec")
    os.makedirs(f"{root}/bin")
    os.makedirs(f"{root}/share/rime/branding/png/symbol")
    os.makedirs(f"{root}/etc/fastfetch")
    os.makedirs(f"{root}/home")
    for v in ("dark", "light"):
        with open(f"{root}/share/rime/branding/png/symbol/rime-symbol-{v}-512.png", "wb") as f:
            f.write(b"\x89PNG shipped " + v.encode())
    with open(f"{root}/etc/fastfetch/config.jsonc", "w") as f:
        f.write("{}\n")
    with open(f"{root}/bin/fastfetch", "w") as f:
        f.write('#!/bin/sh\nfor a in "$@"; do printf "%s\\n" "$a"; done > "$FF_LOG"\n')
    with open(f"{root}/bin/gsettings", "w") as f:
        f.write('#!/bin/sh\necho "${STUB_SCHEME:-default}"\n')
    with open(f"{root}/libexec/rime-term-graphics", "w") as f:
        f.write('#!/bin/sh\necho "$*" > "$TG_LOG"\nprintf "%s\\n" "$STUB_GRAPHICS"\n')
    for p in ("bin/fastfetch", "bin/gsettings", "libexec/rime-term-graphics"):
        os.chmod(f"{root}/{p}", 0o755)
    text = open(GREETING).read()
    text = text.replace("/usr/libexec/", f"{root}/libexec/").replace("/usr/share/", f"{root}/share/")
    text = text.replace("/etc/fastfetch/", f"{root}/etc/fastfetch/")
    with open(f"{root}/greeting.sh", "w") as f:
        f.write(text)


def shell_cmd(shell, interactive=True):
    name = os.path.basename(shell)
    flags = {"bash": ["--norc", "--noprofile"], "zsh": ["-f"], "dash": []}[name]
    probe = ('. "$G"; printf "RTG=%s\\n" "${RIME_TERM_GRAPHICS:-}" > "$OUT_LOG"; '
             'printf "POS=%s\\n" "$*" >> "$OUT_LOG"')
    return [shell, *flags, *(["-i"] if interactive else []), "-c", probe, "argv0", "first", "second arg"]


def run_case(root, shell, env_extra, interactive=True, tty=True):
    logs = {k: f"{root}/{k}.log" for k in ("ff", "tg", "out")}
    for p in logs.values():
        if os.path.exists(p):
            os.remove(p)
    env = {
        "PATH": f"{root}/bin:" + os.environ["PATH"],
        "HOME": f"{root}/home",
        "TERM": "xterm-256color",
        "G": f"{root}/greeting.sh",
        "FF_LOG": logs["ff"],
        "TG_LOG": logs["tg"],
        "OUT_LOG": logs["out"],
        "STUB_GRAPHICS": "kitty-direct 22 8 kitty-direct@/dev/pts/0,xterm-256color,-",
    }
    env.update(env_extra)
    env = {k: v for k, v in env.items() if v is not None}
    if tty:
        master, slave = pty.openpty()
        p = subprocess.Popen(shell_cmd(shell, interactive), stdin=slave, stdout=slave, stderr=slave, env=env,
                             start_new_session=True)
        os.close(slave)
        deadline = time.monotonic() + 20
        while p.poll() is None and time.monotonic() < deadline:
            r, _, _ = select.select([master], [], [], 0.05)
            if r:
                try:
                    os.read(master, 65536)
                except OSError:
                    pass
        os.close(master)
    else:
        p = subprocess.Popen(shell_cmd(shell, interactive), stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                             stderr=subprocess.STDOUT, env=env)
        p.communicate(timeout=20)
    p.wait(timeout=5)

    def read(k):
        return open(logs[k]).read() if os.path.exists(logs[k]) else None

    ff = read("ff")
    return {
        "ff": ff.splitlines() if ff is not None else None,
        "tg": read("tg"),
        "out": read("out") or "",
    }


def arg(ff, flag):
    if ff is None or flag not in ff:
        return None
    return ff[ff.index(flag) + 1]


def expect(name, cond, detail):
    global count
    count += 1
    print(f"{'ok  ' if cond else 'FAIL'} {name}" + ("" if cond else f" -- {detail}"))
    if not cond:
        failures.append(name)


def cases(root, shell):
    sh = os.path.basename(shell)
    cache = f"{root}/home/.cache/rime/logo"
    shipped = f"{root}/share/rime/branding/png/symbol"
    accent_file = f"{root}/home/.cache/rime-shell/accent-ansi"

    def clean_cache():
        shutil.rmtree(f"{root}/home/.cache", ignore_errors=True)

    def put(path, data):
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(data)

    # 1. recoloured logo in the cache, local Ghostty-like answer
    clean_cache()
    put(f"{cache}/rime-symbol-dark.png", "cached dark")
    put(accent_file, "38;2;10;20;30\n")
    r = run_case(root, shell, {})
    ff = r["ff"]
    expect(f"[{sh}] image: protocol from rime-term-graphics", arg(ff, "--logo-type") == "kitty-direct", ff)
    expect(f"[{sh}] image: the cache's recoloured logo", arg(ff, "--logo") == f"{cache}/rime-symbol-dark.png", ff)
    expect(f"[{sh}] image: width AND height (no screen clear)",
           arg(ff, "--logo-width") == "22" and arg(ff, "--logo-height") == "8", ff)
    expect(f"[{sh}] image: helper asked for 8 rows of that image",
           (r["tg"] or "").split() == ["--rows", "8", "--image", f"{cache}/rime-symbol-dark.png"], r["tg"])
    expect(f"[{sh}] accent passed to fastfetch",
           arg(ff, "--color") == "38;2;10;20;30" and arg(ff, "--percent-color-green") == "38;2;10;20;30", ff)
    expect(f"[{sh}] system config used", arg(ff, "--config") == f"{root}/etc/fastfetch/config.jsonc", ff)
    expect(f"[{sh}] RIME_TERM_GRAPHICS exported for this terminal",
           "RTG=kitty-direct@/dev/pts/0,xterm-256color,-" in r["out"], r["out"])
    expect(f"[{sh}] caller's positional parameters untouched", "POS=first second arg" in r["out"], r["out"])
    expect(f"[{sh}] no ASCII logo argument", ff is not None and "--logo-color-1" not in ff, ff)

    # 2. no cache yet: the shipped dark logo
    clean_cache()
    r = run_case(root, shell, {})
    expect(f"[{sh}] no cache: shipped dark logo", arg(r["ff"], "--logo") == f"{shipped}/rime-symbol-dark-512.png",
           r["ff"])
    expect(f"[{sh}] no accent yet: no --color", r["ff"] is not None and "--color" not in r["ff"], r["ff"])

    # 3. light desktop: the light variant, cached or shipped
    r = run_case(root, shell, {"STUB_SCHEME": "'prefer-light'"})
    expect(f"[{sh}] light desktop: shipped light logo",
           arg(r["ff"], "--logo") == f"{shipped}/rime-symbol-light-512.png", r["ff"])
    put(f"{cache}/rime-symbol-light.png", "cached light")
    r = run_case(root, shell, {"STUB_SCHEME": "'prefer-light'"})
    expect(f"[{sh}] light desktop: cached light logo", arg(r["ff"], "--logo") == f"{cache}/rime-symbol-light.png",
           r["ff"])
    r = run_case(root, shell, {"STUB_SCHEME": "'prefer-light'", "SSH_CONNECTION": "192.0.2.1 1 192.0.2.2 22"})
    expect(f"[{sh}] over SSH the remote desktop's scheme is ignored (dark)",
           arg(r["ff"], "--logo") == f"{shipped}/rime-symbol-dark-512.png", r["ff"])

    # 4. each image protocol is passed through
    for proto in ("kitty", "sixel", "iterm"):
        r = run_case(root, shell, {"STUB_GRAPHICS": f"{proto} 23 8 {proto}@x"})
        expect(f"[{sh}] protocol {proto}", arg(r["ff"], "--logo-type") == proto and
               arg(r["ff"], "--logo-width") == "23", r["ff"])

    # 5. the label: no protocol, junk, missing helper, missing image
    put(accent_file, "38;2;10;20;30\n")
    label = ["--logo-type", "data", "--logo", "$1RIME OS"]
    for why, extra in (("none", {"STUB_GRAPHICS": "none 22 8 none@x"}),
                       ("garbage from the helper", {"STUB_GRAPHICS": "\x1b[31mkitty"}),
                       ("non-numeric width", {"STUB_GRAPHICS": "kitty-direct x 8 t@x"}),
                       ("empty answer", {"STUB_GRAPHICS": ""})):
        r = run_case(root, shell, extra)
        ff = r["ff"] or []
        ok = all(arg(ff, label[i]) == label[i + 1] for i in (0, 2))
        expect(f"[{sh}] label when {why}", ok and arg(ff, "--logo-color-1") == "1;38;2;10;20;30", ff)
        expect(f"[{sh}] label when {why}: no image arguments", "--logo-width" not in ff, ff)
    os.remove(accent_file)
    r = run_case(root, shell, {"STUB_GRAPHICS": "none 22 8 none@x"})
    expect(f"[{sh}] label without an accent: Rime Blue, bold", arg(r["ff"], "--logo-color-1") == "1;38;2;145;194;220",
           r["ff"])
    os.rename(f"{root}/libexec/rime-term-graphics", f"{root}/libexec/rime-term-graphics.off")
    r = run_case(root, shell, {})
    expect(f"[{sh}] label when rime-term-graphics is not installed", arg(r["ff"], "--logo-type") == "data", r["ff"])
    os.rename(f"{root}/libexec/rime-term-graphics.off", f"{root}/libexec/rime-term-graphics")
    clean_cache()
    os.rename(f"{shipped}/rime-symbol-dark-512.png", f"{shipped}/off.png")
    r = run_case(root, shell, {})
    expect(f"[{sh}] label when no image exists (helper not even asked)",
           arg(r["ff"], "--logo-type") == "data" and r["tg"] is None, (r["ff"], r["tg"]))
    os.rename(f"{shipped}/off.png", f"{shipped}/rime-symbol-dark-512.png")

    # 6. config missing: still greets, without --config
    os.rename(f"{root}/etc/fastfetch/config.jsonc", f"{root}/etc/fastfetch/off")
    r = run_case(root, shell, {})
    expect(f"[{sh}] no config: greets without --config", r["ff"] is not None and "--config" not in r["ff"], r["ff"])
    os.rename(f"{root}/etc/fastfetch/off", f"{root}/etc/fastfetch/config.jsonc")

    # 7. when it must stay quiet
    for why, extra, kw in (("non-interactive", {}, {"interactive": False}),
                           ("stdout not a terminal", {}, {"tty": False}),
                           ("RIME_NO_GREETING", {"RIME_NO_GREETING": "1"}, {}),
                           ("APEX_NO_GREETING", {"APEX_NO_GREETING": "1"}, {}),
                           ("already greeted", {"RIME_GREETED": "1"}, {}),
                           ("TERM=dumb", {"TERM": "dumb"}, {})):
        r = run_case(root, shell, extra, **kw)
        expect(f"[{sh}] quiet when {why}", r["ff"] is None and r["tg"] is None, (r["ff"], r["tg"]))


def main():
    for shell in SHELLS:
        with tempfile.TemporaryDirectory() as root:
            setup(root)
            cases(root, shell)
    print(f"{count} checks over {len(SHELLS)} shells, {len(failures)} failed" + (f": {failures}" if failures else ""))
    return 1 if failures else 0


if __name__ == "__main__":
    rc = main()
    if EXPECT_FAIL:
        print("mutant " + ("caught" if rc else "NOT caught"))
        sys.exit(0 if rc else 1)
    sys.exit(rc)
