#!/usr/bin/env python3
"""drive.py: one boot-hook lab scenario in OVMF (README.md here). Runs INSIDE the rime-ovmf-probe container.

    drive.py LAYOUT FW        LAYOUT = bls | custom, FW = sb-mok | sb-off
    drive.py matrix CELL FW   the P2 boot matrix (matrix.nix): CELL = bls-plain | bls-luks | custom-plain | custom-luks
    drive.py sbrecovery bls   Secure Boot recovery: the hook's "Enrol the Rime OS Secure Boot key" entry (SbRecovery)

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
    def __init__(self, sc, n, action=None, timeout=300, mem="2048", smp="2", gfx=False):
        self.sc, self.n, self.timeout = sc, n, timeout
        self.base = f"{sc.dir}/boot{n}"
        self.serial = self.base + ".serial.log"
        self.qmp_path = f"/tmp/qmp-{sc.name}-{n}.sock"
        for p in (self.serial, self.qmp_path):
            if os.path.exists(p):
                os.unlink(p)
        argv = [
            "qemu-system-x86_64",
            "-machine", "q35,smm=on,accel=kvm", "-cpu", "host", "-m", mem, "-smp", smp,
            "-global", "driver=cfi.pflash01,property=secure,value=on",
            "-global", "ICH9-LPC.disable_s3=1",
            "-drive", f"if=pflash,unit=0,format=raw,readonly=on,file={CODE}",
            "-drive", f"if=pflash,unit=1,format=raw,file={sc.vars}",
            "-drive", f"if=virtio,format=qcow2,file={sc.disk}",
            "-serial", f"file:{self.serial}",
            "-qmp", f"unix:{self.qmp_path},server=on,wait=off",
            "-display", "none", "-no-reboot", "-nic", "none",
        ]
        if gfx:
            # The greeter needs a display (sway on pixman): QEMU's standard VGA, OVMF's GOP on it is what GRUB,
            # Plymouth and simpledrm draw on, and OVMF still mirrors its text console to the serial port. (Fedora's
            # qemu-system-x86-core in the rime-ovmf-probe image has no virtio-gpu-pci; that is a separate RPM.)
            argv += ["-vga", "std"]
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


MATRIX_TITLE = MENU_TITLE


class MatrixScenario(Scenario):
    """One boot-matrix cell x firmware: three boots of a qcow2 overlay over $LAB/disks/CELL/disk.raw.

      1. GRUB's default, generation 1 (the shipped profile, the signed Rime kernel): for LUKS the passphrase is
         typed at the real prompt (keyboard, QMP sendkey) when the initrd says it is asked; it reaches the greeter
         (screenshot) and installs BAD generation 2 in the VM.
      2. generation 2 is the default and fails (initrd emergency mode, its own marker).
      3. B9/B10: the menu is driven to generation 1 (arrow keys + Return), which unlocks (LUKS) and reaches the
         greeter again (screenshot).
    """

    PASS = "rimelab2026"

    def __init__(self, cell, fw):
        self.cell, self.fw = cell, fw
        self.layout = cell.split("-")[0]
        self.luks = cell.endswith("-luks")
        self.name = f"{cell}-{fw}"
        self.dir = f"{OUT}/{self.name}"
        os.makedirs(self.dir, exist_ok=True)
        with open(f"{LAB}/matrix-gens.json") as f:
            self.gens = json.load(f)[cell]
        self.vars = f"/tmp/{self.name}.vars.fd"
        self.disk = f"/tmp/{self.name}.qcow2"
        subprocess.run(["cp", f"{LAB}/fw/vars-{fw}.fd", self.vars], check=True)
        subprocess.run(["qemu-img", "create", "-q", "-f", "qcow2", "-F", "raw", "-b",
                        f"{LAB}/disks/{cell}/disk.raw", self.disk], check=True)
        self.results = []
        self.want_sb = "enabled" if fw == "sb-mok" else "disabled"
        self.luks_uuid = None
        if self.luks:
            with open(f"{LAB}/disks/{cell}/luks.env") as f:
                env = dict(l.strip().split("=", 1) for l in f if "=" in l)
            self.luks_uuid = env["uuid"]
            self.PASS = env["pass"]

    def boot(self, n, action=None):
        return Boot(self, n, action=action, timeout=900, mem="4096", smp="4", gfx=True)

    def type_passphrase(self, b, label):
        """Wait for the initrd's ask marker, screenshot the prompt, type the passphrase on the keyboard."""
        if not self.luks:
            return []
        t = b.wait_for(lambda t: "RIME-LAB: PASSPHRASE-ASKED" in t, 420)
        if not t:
            return ["the initrd never asked for the passphrase"]
        # The ask waits for as long as it takes: screenshot it as it is first asked and again once Plymouth (on
        # simpledrm/fbcon over the firmware framebuffer) has had time to draw, then type.
        time.sleep(4)
        b.qmp.shot(f"{b.base}-passphrase{label}.png")
        time.sleep(12)
        b.qmp.shot(f"{b.base}-passphrase{label}-late.png")
        for ch in self.PASS:
            b.qmp.key(ch)
            time.sleep(0.15)
        b.qmp.key("ret")
        return []

    def greeter(self, b):
        t = b.wait_for(lambda t: "RIME-LAB: GREETER-UP" in t or "RIME-LAB: GREETER-MISSING" in t, 600)
        if t and "RIME-LAB: GREETER-UP" in t:
            time.sleep(10)  # the driver holds the screen 25 s after GREETER-UP
            b.qmp.shot(b.base + "-greeter.png")
            return []
        return ["greeter never came up (no GREETER-UP)"]

    def good(self, t, gen):
        problems = []
        if f"RIME-LAB: stage-2 reached: generation={gen} system={self.gens[f'gen{gen}']}" not in t:
            problems.append(f"no stage-2 marker for generation {gen}")
        if f"sb-state: SecureBoot {self.want_sb}" not in t:
            problems.append(f"SecureBoot not {self.want_sb}")
        if self.want_sb == "enabled" and "Loaded X.509 cert 'Rime boot-hook lab TEST MOK" not in t:
            problems.append("test MOK not loaded by the kernel")
        if not re.search(r"RIME-LAB: cmdline: BOOT_IMAGE=\S*/rime/nixos/[0-9a-f]{32}-linux-image-\S+-signed-bzImage\.efi ", t):
            problems.append("kernel not the hook's content-addressed copy of the signed Rime image")
        if not re.search(r"RIME-LAB: uname: \S*-rime", t):
            problems.append("running kernel is not Rime's")
        if "RIME-LAB: GREETER-UP" not in t:
            problems.append("no GREETER-UP")
        if self.luks:
            if f"rd.luks.uuid={self.luks_uuid}" not in t.split("RIME-LAB: cmdline:", 1)[-1].split("\n", 1)[0]:
                problems.append("rd.luks.uuid= not on the entry's command line")
            if f"RIME-LAB: luks: /dev/mapper/luks-{self.luks_uuid} " not in t or "type:    LUKS2" not in t \
                    and "type: LUKS2" not in t:
                problems.append("root not on the LUKS2 volume named by rd.luks.uuid")
        if "RIME-LAB: DONE, powering off" not in t:
            problems.append("driver did not finish")
        if self.layout == "custom":
            with open(f"{LAB}/disks/{self.cell}/ostree-before.sha256") as f:
                want = f.read().split()[0]
            if not re.search(rf"ostree: loader -> loader\.1; {want}  /boot/loader\.1/entries/ostree-1\.conf", t):
                problems.append("ostree's loader symlink or entry changed")
            if "custom.cfg windows entry: 1" not in t:
                problems.append("custom.cfg's Windows entry not kept exactly once")
        return problems

    def run(self):
        g = self.gens
        # 1. generation 1 by default; greeter; install BAD generation 2.
        b = self.boot(1, action=f"install:{g['gen2']}")
        menu = b.wait_for(lambda t: "automatically" in t and menu_order(t), 180)
        if menu:
            b.qmp.shot(b.base + "-menu.png")
        p = self.type_passphrase(b, "")
        p += self.greeter(b)
        rc, t = b.finish()
        p += self.good(t, 1)
        if "RIME-LAB: install rc=0" not in t:
            p.append("installing generation 2 failed")
        if "RIME-LAB: menu| set default=rime-generation-2" not in t:
            p.append("generation 2 not the default after its switch-to-configuration boot")
        self.verdict(b, not p, "gen1 (default) reaches the greeter; switch-to-configuration boot makes BAD gen2 the default",
                     "; ".join(p) or f"menu {menu_order(menu or '')}")

        # 2. generation 2 by default fails.
        b = self.boot(2)
        menu = b.wait_for(lambda t: "automatically" in t and menu_order(t), 180)
        order = menu_order(menu or "")
        if menu:
            b.qmp.shot(b.base + "-menu.png")
        p = self.type_passphrase(b, "")
        rc, t = b.finish()
        if "RIME-LAB: BAD generation 2 reached initrd emergency mode" not in t:
            p.append("no bad-generation marker")
        if "RIME-LAB: stage-2 reached" in t:
            p.append("something reached stage 2")
        self.verdict(b, not p, "BAD gen2 is the default and fails at boot", "; ".join(p) or f"menu {order}")

        # 3. pick generation 1 in the GRUB menu by keyboard; it reaches the greeter (B9 plain, B10 LUKS).
        b = self.boot(3)
        menu = b.wait_for(lambda t: "automatically" in t and {1, 2} <= set(menu_order(t)), 180)
        p, keys = [], []
        if not menu:
            p.append("GRUB menu with generations 1 and 2 never appeared")
        else:
            order = menu_order(menu)
            b.qmp.shot(b.base + "-menu.png")
            delta = order.index(1) - order.index(2)
            keys = ["down" if delta > 0 else "up"] * abs(delta)
            for k in keys:
                b.qmp.key(k)
                time.sleep(0.4)
            time.sleep(1.0)
            b.qmp.shot(b.base + "-picked.png")
            b.qmp.key("ret")
        p += self.type_passphrase(b, "")
        p += self.greeter(b)
        rc, t = b.finish()
        p += self.good(t, 1)
        self.verdict(b, not p, "recovery: menu driven to gen1 (keys: " + " ".join(keys + ["ret"]) + "), it reaches the greeter",
                     "; ".join(p) or f"menu {menu_order(menu or '')}")

        for f in (self.vars, self.disk):
            os.unlink(f)
        return all(r.startswith("PASS") for r in self.results)

ENROL_TITLE = "Enrol the Rime OS Secure Boot key (if Rime OS does not start)"
# MokManager's console as mirrored to the serial port: every box is ┌──┐│ line ││ line │└──┘; a screen is its window
# box (first line = the screen's title) followed by its list box.
BOX = re.compile(r"┌─+┐((?:│[^│┌└┘]*│)+)└─+┘")


def mok_screen(text, title, since):
    """The list of the first MokManager screen titled TITLE drawn after offset SINCE, or None."""
    found = False
    for m in BOX.finditer(text, since):
        lines = [x.strip() for x in m.group(1).strip("│").split("││")]
        lines = [x for x in lines if x]
        if found:
            return lines
        if lines and lines[0] == title:
            found = True
    return None


class SbRecovery(Scenario):
    """The NixOS port of the bootc Secure Boot recovery entry, on the bls disk (the hook ran with the TEST MOK's
    certificate during nixos-install). One qcow2 overlay, one varstore copy:

      0. SB on, key NOT enrolled (vars-secboot.raw): GRUB's default generation is refused ("bad shim signature").
      1. Same: the menu lists the entry; End + Return picks it; its text and countdown show; GRUB chainloads
         MokManager; "Enroll key from disk" -> the ESP -> EFI -> rime -> rime-mok.der -> Continue -> Yes -> Reboot,
         every choice found by name in the list MokManager mirrors to the serial port. MokList is then read from the
         varstore (virt-fw-vars).
      2. Same varstore: GRUB's default generation boots with Secure Boot on and the kernel loads the TEST MOK.
      3. SB off (vars-sb-off.fd), same disk: the entry is not in the menu; the default generation boots.
    """

    def __init__(self, layout):
        self.layout, self.fw = layout, "secboot"
        self.name = f"sbrecovery-{layout}"
        self.dir = f"{OUT}/{self.name}"
        os.makedirs(self.dir, exist_ok=True)
        with open(f"{LAB}/gens.json") as f:
            self.gens = json.load(f)[layout]
        self.vars = f"/tmp/{self.name}.vars.fd"
        self.disk = f"/tmp/{self.name}.qcow2"
        subprocess.run(["cp", f"{LAB}/fw/vars-secboot.raw", self.vars], check=True)   # SB on, no MOK
        subprocess.run(["qemu-img", "create", "-q", "-f", "qcow2", "-F", "raw", "-b",
                        f"{LAB}/disks/{layout}/disk.raw", self.disk], check=True)
        self.results = []
        self.want_sb = "enabled"
        self.log = []
        self._since = 0

    def choose(self, b, label, title, want, timeout=60):
        """Wait for MokManager's screen TITLE, read its list from the serial mirror, move to WANT and press Return."""
        t = b.wait_for(lambda t: mok_screen(t, title, self._since), timeout)
        if t is None:
            b.qmp.shot(f"{b.base}-{label}-MISSING.png")
            raise RuntimeError(f"{label}: screen {title!r} never came")
        names = mok_screen(t, title, self._since)
        time.sleep(0.8)
        b.qmp.shot(f"{b.base}-{label}.png")
        self.log.append(f"{label}: {title!r} lists {names}")
        if callable(want):
            want = want(names)
        if want not in names:
            raise RuntimeError(f"{label}: {want!r} not in {names}")
        n = names.index(want)
        for _ in range(n):
            b.qmp.key("down")
            time.sleep(0.3)
        time.sleep(0.8)
        self._since = len(b.text())   # the next screen is drawn after this Return
        b.qmp.key("ret")
        self.log.append(f"{label}: chose {want!r} (down x{n}, ret)")
        return names

    def moklist(self):
        r = subprocess.run(["virt-fw-vars", "--input", self.vars, "--print", "--verbose"],
                           capture_output=True, text=True)
        with open(f"{self.dir}/varstore-after-enrol.txt", "w") as f:
            f.write(r.stdout + r.stderr)
        return r.stdout + r.stderr

    def run(self):
        # 0. SB on, nothing enrolled: the default generation is refused.
        b = Boot(self, 0, timeout=90)
        menu = b.wait_for(lambda t: "automatically" in t and menu_order(t), 120)
        if menu:
            b.qmp.shot(b.base + "-menu.png")
        t = b.wait_for(lambda t: "bad shim signature" in t, 80)
        if t:
            time.sleep(1)
            b.qmp.shot(b.base + "-refused.png")
        b.proc.kill()
        rc, t = b.finish()
        p = []
        if "bad shim signature" not in t:
            p.append("no 'bad shim signature' from GRUB")
        if "RIME-BOOTHOOK: stage-2 reached" in t:
            p.append("a generation reached stage 2 with the key not enrolled")
        if ENROL_TITLE not in (menu or ""):
            p.append("the enrol entry is not in the menu")
        self.verdict(b, not p, "SB on, key not enrolled: menu lists the enrol entry; the default generation is refused",
                     "; ".join(p) or "bad shim signature")

        # 1. Pick the entry, chain to MokManager, enrol the key from disk.
        b = Boot(self, 1, timeout=600)
        p = []
        names = []
        menu = b.wait_for(lambda t: "automatically" in t and ENROL_TITLE in t, 120)
        try:
            if not menu:
                raise RuntimeError("GRUB menu with the enrol entry never appeared")
            b.qmp.shot(b.base + "-menu.png")
            b.qmp.key("end")
            time.sleep(1.0)
            b.qmp.shot(b.base + "-picked.png")
            self._since = len(b.text())
            b.qmp.key("ret")
            t = b.wait_for(lambda t: "Opening in 20 seconds" in t[self._since:], 30)
            if t is None:
                raise RuntimeError("the entry's text never showed (End did not pick it?)")
            time.sleep(1.5)
            b.qmp.shot(b.base + "-entry-text.png")
            if "Enrol the Rime OS Secure Boot key" not in t[self._since:]:
                p.append("entry heading not printed")
            self._since = len(t)
            # No key during GRUB's sleep: a key would interrupt it. Then MokManager.
            t = b.wait_for(lambda t: "Press any key to perform MOK management" in t[self._since:], 60)
            if t is None:
                raise RuntimeError("MokManager never asked to press a key (chainload failed?)")
            b.qmp.shot(b.base + "-mok-countdown.png")
            self._since = len(b.text())
            b.qmp.key("spc")
            self.choose(b, "mok-main", "Perform MOK management", "Enroll key from disk")
            names = self.choose(b, "volumes", "Select Key", lambda n: n[0] if len(n) == 1 else
                                next((x for x in n if "RIME" in x.upper() or "EFI" in x.upper()), n[0]))
            self.volumes = names
            self.choose(b, "dir-root", "Select Key", "EFI/")
            self.choose(b, "dir-efi", "Select Key", "rime/")
            self.choose(b, "dir-rime", "Select Key", "rime-mok.der")
            self.choose(b, "enroll-mok", "[Enroll MOK]", "Continue")
            self.choose(b, "confirm", "Enroll the key(s)?", "Yes")
            self.choose(b, "after-yes", "Perform MOK management", "Reboot")
        except RuntimeError as e:
            p.append(str(e))
        rc, t = b.finish()
        with open(f"{self.dir}/mokmanager-steps.txt", "w") as f:
            f.write("\n".join(self.log) + "\n")
        v = self.moklist()
        if not p and "Rime boot-hook lab TEST MOK" not in v:
            p.append("MokList in the varstore does not hold the TEST MOK")
        self.verdict(b, not p, "entry chains to MokManager; key enrolled from disk (ESP/EFI/rime/rime-mok.der)",
                     "; ".join(p) or f"volume list {getattr(self, 'volumes', names)}; MokList holds the TEST MOK")

        # 2. Same varstore: the default generation boots with Secure Boot on.
        b = Boot(self, 2)
        menu = b.wait_for(lambda t: "automatically" in t and menu_order(t), 120)
        if menu:
            b.qmp.shot(b.base + "-menu.png")
        rc, t = b.finish()
        p = self.good(t, 1)
        self.verdict(b, not p, "after enrolling: gen1 boots with Secure Boot on, kernel loads the TEST MOK",
                     "; ".join(p) or "stage 2, SecureBoot enabled, TEST MOK loaded")

        # 3. SB off, same disk: the entry is hidden; the default generation boots.
        subprocess.run(["cp", f"{LAB}/fw/vars-sb-off.fd", self.vars], check=True)
        self.want_sb = "disabled"
        b = Boot(self, 3)
        menu = b.wait_for(lambda t: "automatically" in t and menu_order(t), 120)
        if menu:
            time.sleep(1.0)
            b.qmp.shot(b.base + "-menu.png")
        rc, t = b.finish()
        p = self.good(t, 1)
        if not menu:
            p.append("no GRUB menu")
        if "Enrol the Rime OS" in t:
            p.append("the enrol entry shows with Secure Boot off")
        self.verdict(b, not p, "SB off: the enrol entry is not in the menu; gen1 boots",
                     "; ".join(p) or f"menu {menu_order(menu or '')}, no enrol entry")

        for f in (self.vars, self.disk):
            os.unlink(f)
        return all(r.startswith("PASS") for r in self.results)


if __name__ == "__main__":
    if len(sys.argv) == 4 and sys.argv[1] == "matrix":
        if sys.argv[2] not in ("bls-plain", "bls-luks", "custom-plain", "custom-luks") \
                or sys.argv[3] not in ("sb-mok", "sb-off"):
            sys.exit("usage: drive.py matrix bls-plain|bls-luks|custom-plain|custom-luks sb-mok|sb-off")
        sys.exit(0 if MatrixScenario(sys.argv[2], sys.argv[3]).run() else 1)
    if len(sys.argv) == 3 and sys.argv[1] == "sbrecovery":
        if sys.argv[2] != "bls":
            sys.exit("usage: drive.py sbrecovery bls")
        sys.exit(0 if SbRecovery(sys.argv[2]).run() else 1)
    if len(sys.argv) != 3 or sys.argv[1] not in ("bls", "custom") or sys.argv[2] not in ("sb-mok", "sb-off"):
        sys.exit("usage: drive.py bls|custom sb-mok|sb-off")
    sys.exit(0 if Scenario(sys.argv[1], sys.argv[2]).run() else 1)
