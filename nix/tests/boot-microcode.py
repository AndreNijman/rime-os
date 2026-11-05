"""Early microcode reaches the menu entry as its own initrd file(s) (nix/modules/boot.nix, docs/nixos/boot-matrix.md
"Early microcode").

Usage: boot-microcode.py RIME_BOOT TOPLEVEL HOST_MICROCODE EXPECT

Runs the real boot hook on TOPLEVEL into a scratch /boot, once per layout (bls, custom-cfg), with host.toml
`[boot] microcode = HOST_MICROCODE` (`unset`: no such key, the default). It reads every file the entry's `initrd`
line(s) name, in order, and requires:
  - the LAST one is byte for byte <toplevel>/initrd and starts with no microcode cpio (moved out, not duplicated);
  - before it, exactly the vendors in EXPECT (`intel,amd`, `intel`, `amd` or `none`), in that order, each an
    uncompressed newc cpio holding kernel/x86/microcode/<GenuineIntel|AuthenticAMD>.bin and nothing of the other
    vendor, byte for byte the file TOPLEVEL's bootspec names (org.rimeos.microcode.v1);
  - bls: one `initrd` line per file; custom-cfg: one `initrd` line naming all of them.
EXPECT `none` with a toplevel that has no bootspec extension is the negative control: it proves the check reads the
files, not the extension.
"""

import json
import os
import re
import subprocess
import sys
import tempfile

BIN = {"intel": "kernel/x86/microcode/GenuineIntel.bin", "amd": "kernel/x86/microcode/AuthenticAMD.bin"}


def leading_cpio_names(data):
    """Names in the uncompressed newc archives at the start of data (each ends at TRAILER!!!; zero padding may
    follow, the next archive starts at a 4-byte boundary). Stops at the first byte that starts no archive."""
    names, off, archives = [], 0, 0
    while off + 110 <= len(data) and data[off:off + 6] in (b"070701", b"070702"):
        h = data[off:off + 110]
        namesize = int(h[94:102], 16)
        filesize = int(h[54:62], 16)
        name = data[off + 110:off + 110 + namesize - 1].decode("utf-8", "replace")
        off = (off + 110 + namesize + 3) & ~3
        off = (off + filesize + 3) & ~3
        if name == "TRAILER!!!":
            archives += 1
            while off < len(data) and data[off] == 0:
                off += 1
            continue
        names.append(name)
    return archives, names, off


def read(path):
    with open(path, "rb") as f:
        return f.read()


def run_layout(hook, toplevel, layout, host_mc, expect, ext):
    root = tempfile.mkdtemp(prefix=f"rime-ucode-{layout}-")
    boot, prof = f"{root}/boot", f"{root}/profiles"
    os.makedirs(f"{boot}/grub2")
    os.makedirs(prof)
    os.symlink(toplevel, f"{prof}/system-1-link")
    os.symlink("system-1-link", f"{prof}/system")
    cfg = {"layout": layout, "bootDir": boot, "profileDir": prof, "profile": "system",
           "hostToml": f"{root}/host.toml", "pinDir": f"{root}/pins", "bootedSystem": f"{root}/none",
           "configurationLimit": 10, "timeout": 5, "reserveMiB": 0, "grubPath": ""}
    with open(f"{root}/cfg.json", "w") as f:
        json.dump(cfg, f)
    if host_mc != "unset":
        with open(f"{root}/host.toml", "w") as f:
            f.write(f'[boot]\nmicrocode = "{host_mc}"\n')
    tag = f"[{layout} microcode={host_mc}]"
    r = subprocess.run([hook, "--config", f"{root}/cfg.json", "install", toplevel], capture_output=True, text=True)
    print(f"{tag} rime-boot install: rc={r.returncode}\n{r.stderr.strip()}")
    if r.returncode != 0:
        return False
    menu = (read(f"{boot}/loader/entries/rime-generation-1.conf") if layout == "bls"
            else read(f"{boot}/grub2/rime-nixos.cfg")).decode()
    lines = [l.strip() for l in menu.splitlines() if re.match(r"\s*initrd\s", l)]
    paths = [p for l in lines for p in l.split()[1:]]
    print(f"{tag} entry initrd line(s): {lines}")
    ok = True
    want_lines = (len(expect) + 1) if layout == "bls" else 1
    if len(lines) != want_lines:
        print(f"{tag} FAIL: {len(lines)} initrd line(s), want {want_lines}")
        ok = False
    if len(paths) != len(expect) + 1:
        print(f"{tag} FAIL: {len(paths)} initrd file(s), want {len(expect) + 1} (vendors {expect} + the initrd)")
        return False
    files = [read(os.path.join(boot, p.lstrip("/"))) for p in paths]
    for vendor, path, data in zip(expect, paths, files):
        archives, names, off = leading_cpio_names(data)
        print(f"{tag} {path}: {len(data)} bytes, {archives} cpio archive(s) over {off} bytes, entries {names}")
        if off != len(data) or archives < 1:
            print(f"{tag} FAIL: {path} is not wholly uncompressed cpio")
            ok = False
        if BIN[vendor] not in names or any(BIN[v] in names for v in BIN if v != vendor):
            print(f"{tag} FAIL: {path} should hold {BIN[vendor]} only")
            ok = False
        if data != read(ext[vendor]):
            print(f"{tag} FAIL: {path} differs from the bootspec's {ext[vendor]}")
            ok = False
    main_initrd = files[-1]
    archives, names, off = leading_cpio_names(main_initrd)
    same = main_initrd == read(os.path.join(toplevel, "initrd"))
    print(f"{tag} {paths[-1]}: {len(main_initrd)} bytes, identical to {toplevel}/initrd: {same}; "
          f"{archives} leading uncompressed cpio archive(s), entries {names}")
    if not same:
        print(f"{tag} FAIL: the last initrd is not the generation's own")
        ok = False
    if any(b in names for b in BIN.values()):
        print(f"{tag} FAIL: the generation's initrd still carries microcode (duplicated, not split)")
        ok = False
    if len(main_initrd) < 1024 * 1024:
        print(f"{tag} FAIL: initrd implausibly small")
        ok = False
    on_esp = sorted(os.listdir(f"{boot}/rime/nixos"))
    print(f"{tag} {boot}/rime/nixos: {on_esp}")
    if len(on_esp) != len(expect) + 2:  # kernel + initrd + one file per vendor, nothing else
        print(f"{tag} FAIL: {len(on_esp)} files on the boot filesystem, want {len(expect) + 2}")
        ok = False
    return ok


def main():
    hook, toplevel, host_mc, expect = sys.argv[1:5]
    expect = [] if expect == "none" else expect.split(",")
    spec = json.loads(read(os.path.join(toplevel, "boot.json")))
    ext = spec.get("org.rimeos.microcode.v1") or {}
    print(f"bootspec org.rimeos.microcode.v1: {ext}")
    ok = True
    for layout in ("bls", "custom-cfg"):
        ok &= run_layout(hook, toplevel, layout, host_mc, expect, ext)
    print("PASS" if ok else "FAIL")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
