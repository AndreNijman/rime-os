"""D3 gate (p2-gates X2): nothing a Rime system runs to activate a configuration or write its boot menu names a
loader installer or an NVRAM writer (bootctl, grub-install, grub2-install, efibootmgr).

Usage: boot-loader-scan.py RIME_BOOT_PROGRAM EXPECT NAME=TOPLEVEL ...

For every toplevel the scan starts at activate, dry-activate, bin/switch-to-configuration and the program
switch-to-configuration runs to install the boot loader (its INSTALL_BOOTLOADER, for Rime the boot hook), and
follows every store path a scanned text file names, file by file, to depth 4 (ELF binaries and directories are not
followed: they are packages, not the activation path). The boot hook's own program is checked separately: it may
name the tools in its documentation, but it must not be able to run any program at all (no process-spawning call or
module in its syntax tree).

EXPECT is `clean` (every toplevel must have no finding) or `flagged` (every toplevel must have one: the negative
controls, so a scan that reads nothing cannot pass). Exit 0 when the expectation holds.
"""

import ast
import os
import re
import sys

WORDS = re.compile(r"(?<![\w.-])(bootctl|grub2?-install|efibootmgr)(?![\w-])")
STORE = re.compile(r"/nix/store/[a-z0-9]{32}-[A-Za-z0-9+._?=-]+(?:/[A-Za-z0-9+._?=-]+)*")
MAX_DEPTH = 4
MAX_BYTES = 4 * 1024 * 1024


def runs_no_program(path):
    """The hook program's syntax tree: no import of a process module, no call of a process-spawning function."""
    tree = ast.parse(open(path, encoding="utf-8").read(), path)
    bad = []
    banned_modules = {"subprocess", "pty", "multiprocessing", "asyncio", "ctypes", "_posixsubprocess"}
    banned_os = re.compile(r"^(system|popen|fork|forkpty|exec\w*|spawn\w*|posix_spawn\w*)$")
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            bad += [a.name for a in node.names if a.name.split(".")[0] in banned_modules]
        elif isinstance(node, ast.ImportFrom):
            if (node.module or "").split(".")[0] in banned_modules:
                bad.append(node.module)
            if node.module == "os":
                bad += [a.name for a in node.names if banned_os.match(a.name)]
        elif isinstance(node, ast.Attribute) and isinstance(node.value, ast.Name) and node.value.id == "os":
            if banned_os.match(node.attr):
                bad.append(f"os.{node.attr} (line {node.lineno})")
        elif isinstance(node, ast.Call) and isinstance(node.func, ast.Name) and node.func.id in ("__import__", "exec", "eval"):
            bad.append(f"{node.func.id}() (line {node.lineno})")
    return bad


def install_bootloader(stc):
    m = re.search(r"^export INSTALL_BOOTLOADER='([^']+)'$", open(stc, encoding="utf-8", errors="replace").read(), re.M)
    return m.group(1) if m else None


def scan(toplevel, skip):
    roots = [os.path.join(toplevel, p) for p in ("activate", "dry-activate", "bin/switch-to-configuration")]
    loader = install_bootloader(os.path.join(toplevel, "bin/switch-to-configuration"))
    if loader:
        roots.append(loader)
    seen, findings, scanned = set(), [], 0
    queue = [(r, 0, "toplevel") for r in roots]
    while queue:
        path, depth, via = queue.pop(0)
        real = os.path.realpath(path)
        if real in seen or not os.path.isfile(real):
            continue
        seen.add(real)
        if any(real.startswith(s) for s in skip):
            continue
        with open(real, "rb") as f:
            head = f.read(4)
            if head == b"\x7fELF":
                continue
            f.seek(0)
            data = f.read(MAX_BYTES)
        if b"\0" in data:
            continue
        text = data.decode("utf-8", "replace")
        scanned += 1
        for n, line in enumerate(text.splitlines(), 1):
            for m in WORDS.finditer(line):
                findings.append(f"{path} (from {via}) line {n}: {m.group(1)}: {line.strip()[:160]}")
        if depth < MAX_DEPTH:
            for ref in sorted(set(STORE.findall(text))):
                queue.append((ref, depth + 1, path))
    return loader, scanned, findings


def main():
    hook_prog, expect, systems = sys.argv[1], sys.argv[2], sys.argv[3:]
    assert expect in ("clean", "flagged") and systems
    ok = True
    hook_pkg = re.match(r"/nix/store/[^/]+", os.path.realpath(hook_prog)).group(0)
    bad = runs_no_program(os.path.realpath(hook_prog))
    print(f"boot hook program {hook_prog}: " + ("runs no program" if not bad else f"CAN RUN PROGRAMS: {bad}"))
    ok &= not bad
    for arg in systems:
        name, toplevel = arg.split("=", 1)
        loader, scanned, findings = scan(toplevel, [hook_pkg])
        print(f"{name}: {toplevel}")
        print(f"  INSTALL_BOOTLOADER = {loader}")
        print(f"  {scanned} text files scanned, {len(findings)} finding(s)")
        for f in findings[:20]:
            print(f"    {f}")
        if scanned < 4:
            print("  FAIL: too few files scanned for the scan to mean anything")
            ok = False
        if expect == "clean" and findings:
            print(f"  FAIL: {name} names a loader installer or NVRAM writer")
            ok = False
        if expect == "flagged" and not findings:
            print(f"  FAIL: negative control {name} was not flagged: the scan does not see what it must")
            ok = False
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
