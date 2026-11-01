#!/usr/bin/env python3
"""drive.py: one boot-hook lab scenario in OVMF (README.md here). Runs INSIDE the rime-ovmf-probe container.

    drive.py LAYOUT FW        LAYOUT = bls | custom, FW = sb-mok | sb-off

Five boots of one disk (a qcow2 overlay over $LAB/disks/LAYOUT/disk.raw, so every scenario starts from the same
nixos-install'd generation 1), each a fresh QEMU with the same varstore copy:

  1. GRUB's default (generation 1) boots to multi-user; the driver service installs generation 2 (BAD) with
     nix-env --set + switch-to-configuration boot (the Rime hook runs in the VM).
  2. GRUB's default is now generation 2: it fails (initrd emergency mode, marker from the bad generation).
  3. The GRUB menu is driven with QMP sendkey (arrow keys + Return) to pick generation 1, which boots to
     multi-user; the driver installs generation 3.
  4. GRUB's default is generation 3: it boots; the driver rolls the profile back to generation 1
     (nix-env --switch-generation 1 + the profile's switch-to-configuration boot).
  5. GRUB's default is generation 1 again, chosen by its id (in the BLS layout blscfg lists it last): it boots.

Every verdict needs a positive marker; a timeout is never a pass. Evidence: $OUT/LAYOUT-FW/{bootN.serial.log,
bootN.serial.txt, bootN-*.png, bootN.qemu.stderr}, verdicts appended to $OUT/results.tsv.
"""
import json
import os
import re
import socket
import subprocess
import sys
import time

LAB = os.environ.get("LAB", "/lab")
OUT = os.environ.get("OUT", "/out")
CODE = f"{LAB}/fw/code-secboot.raw"
ANSI = re.compile(rb"\x1b\[[0-9;?]*[A-Za-z]|\x1b[()][A-Z0-9]|\r")
MENU_TITLE = re.compile(r"Rime \(generation (\d+), \d{4}-\d\d-\d\d \d\d:\d\d\)")


def stripped(path):
    try:
        with open(path, "rb") as f:
            return ANSI.sub(b"", f.read()).decode("utf-8", "replace")
    except FileNotFoundError:
        return ""


class Qmp:
    def __init__(self, path, deadline=30):
        end = time.time() + deadline
        while True:
            try:
                self.s = socket.socket(socket.AF_UNIX)
                self.s.connect(path)
                break
            except OSError:
                if time.time() > end:
                    raise
                time.sleep(0.1)
        self.f = self.s.makefile("rwb")
        self.f.readline()  # greeting
        self.cmd("qmp_capabilities")

    def cmd(self, execute, **arguments):
        msg = {"execute": execute}
        if arguments:
            msg["arguments"] = arguments
        self.f.write(json.dumps(msg).encode() + b"\n")
        self.f.flush()
        while True:
            line = self.f.readline()
            if not line:
                raise EOFError("QMP closed")
            r = json.loads(line)
            if "return" in r or "error" in r:
                return r

    def key(self, k):
        return self.cmd("human-monitor-command", **{"command-line": f"sendkey {k}"})

    def shot(self, path):
        return self.cmd("screendump", filename=path, format="png")

    def close(self):
        try:
            self.s.close()
        except OSError:
            pass


class Boot:
    def __init__(self, sc, n, action=None, timeout=300):
        self.sc, self.n, self.timeout = sc, n, timeout
        self.base = f"{sc.dir}/boot{n}"
        self.serial = self.base + ".serial.log"
        self.qmp_path = f"/tmp/qmp-{sc.name}-{n}.sock"
        for p in (self.serial, self.qmp_path):
            if os.path.exists(p):
                os.unlink(p)
        argv = [
            "qemu-system-x86_64",
            "-machine", "q35,smm=on,accel=kvm", "-cpu", "host", "-m", "2048", "-smp", "2",
            "-global", "driver=cfi.pflash01,property=secure,value=on",
            "-global", "ICH9-LPC.disable_s3=1",
            "-drive", f"if=pflash,unit=0,format=raw,readonly=on,file={CODE}",
            "-drive", f"if=pflash,unit=1,format=raw,file={sc.vars}",
            "-drive", f"if=virtio,format=qcow2,file={sc.disk}",
            "-serial", f"file:{self.serial}",
            "-qmp", f"unix:{self.qmp_path},server=on,wait=off",
            "-display", "none", "-no-reboot", "-nic", "none",
        ]
        if action:
            argv += ["-fw_cfg", f"name=opt/rime/action,string={action}"]
        self.err = open(self.base + ".qemu.stderr", "wb")
        self.t0 = time.time()
        self.proc = subprocess.Popen(argv, stdout=self.err, stderr=subprocess.STDOUT)
        self.qmp = Qmp(self.qmp_path)

    def text(self):
        return stripped(self.serial)

    def wait_for(self, pred, timeout):
        end = time.time() + timeout
        while time.time() < end:
            t = self.text()
            if pred(t):
                return t
            if self.proc.poll() is not None:
                return None
            time.sleep(0.2)
        return None

    def finish(self):
        rc = None
        try:
            rc = self.proc.wait(timeout=max(1, self.timeout - (time.time() - self.t0)))
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()
            rc = "timeout"
        self.qmp.close()
        self.err.close()
        self.secs = int(time.time() - self.t0)
        t = self.text()
        with open(self.base + ".serial.txt", "w") as f:
            f.write(t)
        return rc, t


def check_json(text):
    """The last `rime-boot check` JSON the driver printed."""
    found = re.findall(r"RIME-BOOTHOOK: rime-boot check: (\{.*?\})\s*$", text, re.M)
    try:
        return json.loads(found[-1]) if found else {}
    except ValueError:
        return {}


def menu_order(text):
    """Rime entries in the order the GRUB menu lists them (first full render on the console)."""
    seen = []
    for m in MENU_TITLE.finditer(text):
        g = int(m.group(1))
        if g in seen:
            break
        seen.append(g)
    return seen


class Scenario:
    def __init__(self, layout, fw):
        self.layout, self.fw = layout, fw
        self.name = f"{layout}-{fw}"
        self.dir = f"{OUT}/{self.name}"
        os.makedirs(self.dir, exist_ok=True)
        with open(f"{LAB}/gens.json") as f:
            self.gens = json.load(f)[layout]
        self.vars = f"/tmp/{self.name}.vars.fd"
        self.disk = f"/tmp/{self.name}.qcow2"
        subprocess.run(["cp", f"{LAB}/fw/vars-{fw}.fd", self.vars], check=True)
        subprocess.run(["qemu-img", "create", "-q", "-f", "qcow2", "-F", "raw", "-b",
                        f"{LAB}/disks/{layout}/disk.raw", self.disk], check=True)
        self.results = []
        self.want_sb = "enabled" if fw == "sb-mok" else "disabled"

    def verdict(self, boot, ok, what, detail=""):
        row = "\t".join([("PASS" if ok else "FAIL"), self.name, f"boot{boot.n}", what,
                         f"{boot.secs}s", detail])
        print(row, flush=True)
        self.results.append(row)
        with open(f"{OUT}/results.tsv", "a") as f:
            f.write(row + "\n")

    def good(self, t, gen):
        """Checks every boot to multi-user must pass."""
        problems = []
        if f"RIME-BOOTHOOK: stage-2 reached: generation={gen} system={self.gens[f'gen{gen}']}" not in t:
            problems.append(f"no stage-2 marker for generation {gen} ({self.gens[f'gen{gen}']})")
        if f"sb-state: SecureBoot {self.want_sb}" not in t:
            problems.append(f"SecureBoot not {self.want_sb}")
        if self.want_sb == "enabled" and "Loaded X.509 cert 'Rime boot-hook lab TEST MOK" not in t:
            problems.append("test MOK not loaded by the kernel")
        if not re.search(r"RIME-BOOTHOOK: cmdline: BOOT_IMAGE=\S*/rime/nixos/[0-9a-f]{32}-linux-\S+-signed\.efi ", t):
            problems.append("kernel not the hook's content-addressed signed copy")
        if "RIME-BOOTHOOK: DONE, powering off" not in t:
            problems.append("driver did not finish")
        if self.layout == "custom":
            with open(f"{LAB}/disks/custom/ostree-before.sha256") as f:
                want = f.read().split()[0]
            if not re.search(rf"ostree: loader -> loader\.1; {want}  /boot/loader\.1/entries/ostree-1\.conf", t):
                problems.append("ostree's loader symlink or entry changed")
            if "custom.cfg windows entry: 1" not in t:
                problems.append("custom.cfg's Windows entry not kept exactly once")
        return problems

    def run(self):
        g = self.gens
        # 1. generation 1 by default; install the bad generation 2.
        b = Boot(self, 1, action=f"install:{g['gen2']}")
        menu = b.wait_for(lambda t: "automatically" in t and menu_order(t), 120)
        if menu:
            b.qmp.shot(b.base + "-menu.png")
        rc, t = b.finish()
        p = self.good(t, 1)
        if "RIME-BOOTHOOK: install rc=0" not in t:
            p.append("installing generation 2 failed")
        if "RIME-BOOTHOOK: menu| set default=rime-generation-2" not in t:
            p.append("generation 2 not the default after its switch-to-configuration boot")
        self.verdict(b, not p, "gen1 boots by default, switch-to-configuration boot makes BAD gen2 the default",
                     "; ".join(p) or f"menu {menu_order(menu or '')}")

        # 2. generation 2 by default, and it fails.
        b = Boot(self, 2, timeout=240)
        menu = b.wait_for(lambda t: "automatically" in t and menu_order(t), 120)
        order = menu_order(menu or "")
        if menu:
            b.qmp.shot(b.base + "-menu.png")
        rc, t = b.finish()
        p = []
        if "RIME-BOOTHOOK: BAD generation 2 reached initrd emergency mode" not in t:
            p.append("no bad-generation marker")
        if "RIME-BOOTHOOK: stage-2 reached" in t:
            p.append("something reached stage 2")
        if order[:1] != [2] and self.layout == "custom":
            p.append(f"custom menu does not list the default first: {order}")
        self.verdict(b, not p, "BAD gen2 is the default and fails at boot", "; ".join(p) or f"menu {order}")

        # 3. pick generation 1 in the GRUB menu by keyboard; it boots and installs generation 3.
        b = Boot(self, 3, action=f"install:{g['gen3']}")
        menu = b.wait_for(lambda t: "automatically" in t and {1, 2} <= set(menu_order(t)), 120)
        p = []
        keys = []
        if not menu:
            p.append("GRUB menu with generations 1 and 2 never appeared")
        else:
            order = menu_order(menu)
            b.qmp.shot(b.base + "-menu.png")
            delta = order.index(1) - order.index(2)  # GRUB starts on the default, generation 2
            keys = ["down" if delta > 0 else "up"] * abs(delta)
            if not keys:
                p.append("generation 1 and 2 at the same place?")
            for k in keys:
                b.qmp.key(k)
                time.sleep(0.4)
            time.sleep(1.0)
            b.qmp.shot(b.base + "-picked.png")
            b.qmp.key("ret")
        rc, t = b.finish()
        p += self.good(t, 1)
        if "RIME-BOOTHOOK: install rc=0" not in t:
            p.append("installing generation 3 failed")
        if "RIME-BOOTHOOK: menu| set default=rime-generation-3" not in t:
            p.append("generation 3 not the default after its switch-to-configuration boot")
        self.verdict(b, not p, "menu driven to gen1 (keys: " + " ".join(keys + ["ret"]) + "), boots, installs gen3",
                     "; ".join(p) or f"menu {menu_order(menu or '')}")

        # 4. generation 3 by default, and it boots; then roll the profile back to generation 1 (D6).
        b = Boot(self, 4, action="rollback:1")
        menu = b.wait_for(lambda t: "automatically" in t and menu_order(t), 120)
        order4 = menu_order(menu or "")
        if menu:
            b.qmp.shot(b.base + "-menu.png")
        rc, t = b.finish()
        p = self.good(t, 3)
        if "RIME-BOOTHOOK: rollback rc=0" not in t:
            p.append("rollback to generation 1 failed")
        chk = check_json(t)
        if not (chk.get("up_to_date") is True and chk.get("default_generation") == 1
                and chk.get("profile_generation") == 1):
            p.append(f"rime-boot check after the rollback: {chk or 'missing'}")
        self.verdict(b, not p, "GOOD gen3 is the default and boots; rollback to gen1 rewrites the default",
                     "; ".join(p) or f"menu {order4}")

        # 5. generation 1 by default (by GRUB id: in the BLS layout it is not the first entry).
        b = Boot(self, 5)
        menu = b.wait_for(lambda t: "automatically" in t and menu_order(t), 120)
        order = menu_order(menu or "")
        if menu:
            b.qmp.shot(b.base + "-menu.png")
        rc, t = b.finish()
        p = self.good(t, 1)
        if "RIME-BOOTHOOK: menu| set default=rime-generation-1" not in t:
            p.append("menu default is not generation 1")
        if sorted(order) != [1, 2, 3]:
            p.append(f"menu does not list generations 1-3: {order}")
        self.verdict(b, not p, "after the rollback gen1 is the default (by id) and boots",
                     "; ".join(p) or f"menu {order} (gen1 at position {order.index(1) if 1 in order else '?'})")

        for f in (self.vars, self.disk):
            os.unlink(f)
        return all(r.startswith("PASS") for r in self.results)


if __name__ == "__main__":
    if len(sys.argv) != 3 or sys.argv[1] not in ("bls", "custom") or sys.argv[2] not in ("sb-mok", "sb-off"):
        sys.exit("usage: drive.py bls|custom sb-mok|sb-off")
    sys.exit(0 if Scenario(sys.argv[1], sys.argv[2]).run() else 1)
