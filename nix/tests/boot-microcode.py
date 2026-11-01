"""Early microcode reaches the menu entry (wave-1 handover, nix/modules/boot.nix).

Usage: boot-microcode.py RIME_BOOT TOPLEVEL EXPECT

Runs the real boot hook on TOPLEVEL into a scratch /boot, once per layout (bls, custom-cfg), reads the `initrd` line
of the entry it wrote, and lists the uncompressed cpio archives at the start of that file (boot.initrd.prepend
concatenates them before the compressed initrd; the kernel's early loader reads only these). EXPECT `both`: the
file must carry kernel/x86/microcode/GenuineIntel.bin and AuthenticAMD.bin; `none`: neither (a negative control,
proving the listing reads the archives rather than passing on anything).
"""

import json
import os
import re
import subprocess
import sys
import tempfile

UCODE = ("kernel/x86/microcode/GenuineIntel.bin", "kernel/x86/microcode/AuthenticAMD.bin")


def leading_cpio_names(path):
    """Names in the uncompressed newc archives at the start of path (each ends at TRAILER!!!; zero padding may
    follow, the next archive starts at a 4-byte boundary). Stops at the first byte that starts no archive."""
    data = open(path, "rb").read()
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
    return archives, names, off, len(data)


def main():
    hook, toplevel, expect = sys.argv[1:4]
    ok = True
    for layout in ("bls", "custom-cfg"):
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
        r = subprocess.run([hook, "--config", f"{root}/cfg.json", "install", toplevel], capture_output=True, text=True)
        print(f"[{layout}] rime-boot install: rc={r.returncode}\n{r.stderr.strip()}")
        if r.returncode != 0:
            ok = False
            continue
        menu = (open(f"{boot}/loader/entries/rime-generation-1.conf").read() if layout == "bls"
                else open(f"{boot}/grub2/rime-nixos.cfg").read())
        lines = [l.strip() for l in menu.splitlines() if re.match(r"\s*initrd\s", l)]
        print(f"[{layout}] entry initrd line(s): {lines}")
        assert len(lines) == 1, lines
        rel = lines[0].split()[1]
        f = os.path.join(boot, rel.lstrip("/"))
        same = open(f, "rb").read() == open(os.path.join(toplevel, "initrd"), "rb").read()
        archives, names, off, size = leading_cpio_names(f)
        print(f"[{layout}] {rel}: {size} bytes, identical to {toplevel}/initrd: {same}; "
              f"{archives} leading uncompressed cpio archive(s), {off} bytes, entries: {names}")
        ok &= same
        have = [u for u in UCODE if u in names]
        if expect == "both" and len(have) != 2:
            print(f"[{layout}] FAIL: microcode missing from the initrd the entry names (found {have})")
            ok = False
        if expect == "none" and have:
            print(f"[{layout}] FAIL: negative control carries microcode {have}")
            ok = False
        if expect == "none" and size < 1024 * 1024:
            print(f"[{layout}] FAIL: initrd implausibly small")
            ok = False
    print("PASS" if ok else "FAIL")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
