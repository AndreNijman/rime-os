#!/usr/bin/env python3
"""Command audit for rime-libexec (run by the derivation's installCheckPhase).

For every script it is given, list the external commands the script can run and check that each one
resolves: to a shell builtin or keyword, to a function the script defines, to an executable on the
PATH the derivation injected into that script, or to a name on the RUNTIME list (commands that come
from the running system, such as sudo from /run/wrappers/bin or rime from /run/current-system/sw/bin,
which a store path must not pin). Anything else fails the build.

Shell scripts are parsed with shfmt (--to-json): a command is the first word of every call expression
whose text is literal, plus the word after a prefix command (sudo, exec, env, timeout, ...). Python and
node scripts are scanned for subprocess argv lists and which() calls. Both are conservative: a command
built at runtime from a variable is not seen, so the VM test runs the scripts as well.

Usage:
  rime-libexec-audit.py inject FILE DIRS (open|closed)   add the PATH line (marked "# rime-libexec")
  rime-libexec-audit.py audit --shfmt PATH --runtime a,b,c [--report FILE] script...
The PATH a script was given is read back from the script itself (the marked line).
open = the store directories, then the caller's PATH, then /run/wrappers/bin and /run/current-system/sw/bin;
closed = the same without the caller's PATH (scripts that run as root or from sudo).
"""
import argparse
import json
import os
import re
import subprocess
import sys

BUILTINS = set("""
: . [ [[ ]] alias bg bind break builtin caller cd command compgen complete compopt continue declare dirs
disown echo enable eval exec exit export false fc fg getopts hash help history jobs kill let local logout
mapfile popd printf pushd pwd read readarray readonly return set shift shopt source suspend test times trap
true type typeset ulimit umask unalias unset wait if then else elif fi case esac for while until do done
in function select time coproc
""".split())

# A prefix command runs the word after it (after its own options).
PREFIX = {"sudo", "exec", "command", "env", "nice", "ionice", "setpriv", "timeout", "xargs", "flock",
          "setsid", "runuser", "nohup", "stdbuf", "chrt", "taskset", "nsenter", "systemd-inhibit",
          "dbus-run-session", "pkexec", "time", "builtin", "unshare", "systemd-cat", "gamemoderun"}
# Options of prefix commands that take a value (so the value is not mistaken for the command).
PREFIX_VALUE_OPTS = {"-u", "-g", "-n", "-c", "-s", "-k", "--user", "--group", "--signal", "-t",
                     "--reuid", "--regid", "--groups", "--inh-caps", "--ambient-caps", "--bounding-set",
                     "--what", "--who", "--why", "--mode", "-p", "-I", "-L", "-d", "-a", "-C", "-w",
                     "-i", "-o", "-e", "--identifier", "--priority", "--level-prefix", "-E", "-S"}

MARK = "# rime-libexec"
SYSTEM_DIRS = "/run/wrappers/bin:/run/current-system/sw/bin"


def literal(word):
    """Text of a shfmt Word when every part is literal, else None."""
    out = []
    for part in word.get("Parts", []):
        t = part.get("Type")
        if t == "Lit":
            out.append(part.get("Value", ""))
        elif t == "SglQuoted":
            out.append(part.get("Value", ""))
        elif t == "DblQuoted":
            inner = literal(part)
            if inner is None:
                return None
            out.append(inner)
        else:
            return None
    return "".join(out)


def walk(node, calls, funcs):
    if isinstance(node, dict):
        t = node.get("Type")
        if t == "FuncDecl":
            name = node.get("Name", {}).get("Value")
            if name:
                funcs.add(name)
        if t == "CallExpr":
            words = [literal(a) for a in node.get("Args", []) or []]
            i = 0
            while i < len(words):
                w = words[i]
                if w is None:
                    break
                calls.add(w)
                if os.path.basename(w) not in PREFIX:
                    break
                # skip the prefix command's options, then take the command it runs
                i += 1
                while i < len(words):
                    o = words[i]
                    if o is None:
                        break
                    if o == "--":
                        i += 1
                        break
                    if o.startswith("-"):
                        i += 2 if (o in PREFIX_VALUE_OPTS) else 1
                        continue
                    if "=" in o or re.fullmatch(r"[0-9.]+[smhd]?", o):
                        i += 1
                        continue
                    break
        for v in node.values():
            walk(v, calls, funcs)
    elif isinstance(node, list):
        for v in node:
            walk(v, calls, funcs)


def shell_commands(path, shfmt):
    src = open(path, "rb").read()
    res = subprocess.run([shfmt, "--to-json", "-ln", "bash"], input=src, capture_output=True)
    if res.returncode != 0:
        raise SystemExit(f"{path}: shfmt could not parse it: {res.stderr.decode().strip()}")
    calls, funcs = set(), set()
    walk(json.loads(res.stdout), calls, funcs)
    return calls, funcs


PY_CALL = re.compile(
    r"""(?:run|Popen|check_output|check_call|call|execvp|execv|execlp|spawnvp|_run|_spawn|run_cmd|cmd|sh|capture)\(\s*\[\s*["']([A-Za-z0-9_.+/-]+)["']""")
PY_WHICH = re.compile(r"""which\(\s*["']([A-Za-z0-9_.+-]+)["']""")
JS_CALL = re.compile(r"""(?:spawn|spawnSync|execFile|execFileSync|exec|execSync)\(\s*["'`]([A-Za-z0-9_.+/-]+)""")


def script_commands(path, interp):
    text = open(path, encoding="utf-8", errors="replace").read()
    pat = [PY_CALL, PY_WHICH] if interp == "python" else [JS_CALL]
    calls = set()
    for p in pat:
        calls.update(m.group(1) for m in p.finditer(text))
    return calls, set()


def injected_path(path):
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            if line.rstrip().endswith(MARK):
                # shell: PATH="<dirs>${PATH:+:$PATH}:<system>"; python: ...environ["PATH"] = "<dirs>" + ...
                for value in re.findall(r'"([^"]*)"', line):
                    if "/" in value:
                        value = value.replace("${PATH:+:$PATH}", "")
                        return [d for d in value.split(":") if d and "$" not in d]
    return None


def inject(path, dirs, mode):
    """Put the PATH line in a script without renaming it (argv[0], BASH_SOURCE and readlink -f $0 stay true)."""
    interp = interpreter(path)
    lines = open(path, encoding="utf-8").read().split("\n")
    if interp == "shell":
        caller = "${PATH:+:$PATH}" if mode == "open" else ""
        line = f'PATH="{dirs}{caller}:{SYSTEM_DIRS}"; export PATH {MARK}'
        at = 1
    elif interp == "python":
        import ast
        tree = ast.parse("\n".join(lines))
        at = 1
        body = tree.body
        i = 0
        if body and isinstance(body[0], ast.Expr) and isinstance(getattr(body[0], "value", None), ast.Constant) \
                and isinstance(body[0].value.value, str):
            at = body[0].end_lineno
            i = 1
        while i < len(body) and isinstance(body[i], ast.ImportFrom) and body[i].module == "__future__":
            at = body[i].end_lineno
            i += 1
        caller = ' + (":" + _rime_os.environ["PATH"] if _rime_os.environ.get("PATH") else "")' if mode == "open" else ""
        line = (f'import os as _rime_os; _rime_os.environ["PATH"] = "{dirs}"{caller} + ":{SYSTEM_DIRS}"  {MARK}')
    else:
        raise SystemExit(f"{path}: cannot inject a PATH into a {interp} script")
    lines.insert(at, line)
    open(path, "w", encoding="utf-8").write("\n".join(lines))


def interpreter(path):
    with open(path, "rb") as f:
        first = f.readline().decode(errors="replace")
    if "python" in first:
        return "python"
    if "node" in first:
        return "node"
    if "sh" in first:
        return "shell"
    return None


def main():
    if len(sys.argv) >= 2 and sys.argv[1] == "inject":
        _, _, f, dirs, mode = sys.argv
        inject(f, dirs, mode)
        return
    if len(sys.argv) >= 2 and sys.argv[1] == "audit":
        del sys.argv[1]
    ap = argparse.ArgumentParser()
    ap.add_argument("--shfmt", required=True)
    ap.add_argument("--runtime", default="")
    ap.add_argument("--report")
    ap.add_argument("scripts", nargs="+")
    a = ap.parse_args()
    runtime = {r for r in a.runtime.split(",") if r}
    failed = 0
    report = {}
    for s in a.scripts:
        interp = interpreter(s)
        if interp is None:
            print(f"FAIL {s}: no interpreter line")
            failed += 1
            continue
        dirs = injected_path(s)
        if dirs is None and interp == "node":
            dirs = []  # node scripts get no PATH line: they may not start any command at all
        if dirs is None:
            print(f"FAIL {s}: no PATH line marked '{MARK}'")
            failed += 1
            continue
        if interp == "shell":
            calls, funcs = shell_commands(s, a.shfmt)
        else:
            calls, funcs = script_commands(s, interp)
        entry = {"store": {}, "runtime": [], "unresolved": []}
        for c in sorted(calls):
            if c in BUILTINS or c in funcs or c == "":
                continue
            if c.startswith("/"):
                if c.startswith("/nix/store/") or c.startswith("/run/") or c.startswith("/proc/") \
                        or c.startswith("/sys/") or c.startswith("/dev/") or c.startswith("/tmp/"):
                    if c.startswith("/nix/store/") and not os.access(c, os.X_OK):
                        entry["unresolved"].append(c)
                    continue
                entry["unresolved"].append(c)
                continue
            if "/" in c or not re.fullmatch(r"[A-Za-z0-9_.+-]+", c):
                continue  # ./relative, or something that is not a command word
            hit = next((os.path.join(d, c) for d in dirs if os.access(os.path.join(d, c), os.X_OK)), None)
            if hit:
                entry["store"][c] = os.path.realpath(hit)
            elif c in runtime:
                entry["runtime"].append(c)
            else:
                entry["unresolved"].append(c)
        report[os.path.basename(s)] = entry
        if entry["unresolved"]:
            failed += 1
            print(f"FAIL {os.path.basename(s)}: unresolved: {' '.join(entry['unresolved'])}")
        else:
            print(f"ok   {os.path.basename(s)}: {len(entry['store'])} store, {len(entry['runtime'])} runtime")
    if a.report:
        with open(a.report, "w") as f:
            json.dump(report, f, indent=1, sort_keys=True)
    if failed:
        print(f"{failed} script(s) have commands that do not resolve")
        sys.exit(1)


if __name__ == "__main__":
    main()
