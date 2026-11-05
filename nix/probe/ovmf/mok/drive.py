#!/usr/bin/env python3
"""drive.py: the MOK enrolment lab's boot sequence in OVMF, Secure Boot on (README.md here). Runs INSIDE rime-ovmf-mok.

    drive.py run        the whole sequence below; exit 0 only if every boot has its positive markers
    drive.py realsig    one more boot on the enrolled varstore: the NixOS kernel carrying the release signature
                        (run.sh realsig attaches it and copies it to /probe/nixos.efi)

One varstore and one disk carry over between boots, as on one machine. The varstore starts with Microsoft's keys only
(OVMF's secboot varstore: Red Hat PK, Microsoft KEK and db) and NO MOK.

  C1  bootc kernel, before enrolment   the real Rime-MOK-signed bootc kernel is REFUSED (db does not admit it)
  I1  Fedora kernel, import            shim -> GRUB -> Fedora-signed kernel (shim's built-in Fedora cert) -> an
                                       initrd that runs `mokutil --import rime-mok.der` with a password hash and
                                       reboots (MokNew + MokAuth written by the guest, nothing pre-seeded)
  M1  MokManager                       shim finds MokNew and starts MokManager; the driver presses keys over QMP
                                       (send-key) and takes a screenshot (screendump) of every screen: any key,
                                       Enroll MOK, Continue, Yes, the password, Reboot
  R1  bootc kernel, after enrolment    shim admits the Rime-MOK-signed kernel: SecureBoot enabled, the Rime MOK in
                                       MokListRT and in the kernel's platform keyring, and the lockdown state
  R2  TEST-MOK kernel                  a kernel signed by a key that is NOT enrolled is still refused
  V   the varstore                     virt-fw-vars reads MokList: exactly one certificate, the Rime MOK (sha256
                                       2fdd2f55...)

Every verdict needs a positive marker; a timeout is never a pass. Evidence in $OUT/run/: NAME.serial.txt,
NAME-NN-*.png (M1), results.tsv, varstore.txt.

    drive.py split      early microcode as separate initrd files (docs/nixos/boot-matrix.md, "Early microcode"):
  U1-bls / U2-menu      Intel cpio + AMD cpio + initrd through blscfg (three `initrd` lines) and through one
                        `initrd` line naming three files: the pieces GRUB handed the kernel are the files, in order,
                        and both microcode file names are in those bytes
  U3-control            the initrd alone: no microcode names
  U4-loader             `-cpu host,-hypervisor` (the kernel's loader is OFF under a hypervisor flag), U2's menu,
                        `nokaslr`, QEMU's gdbstub: load_ucode_intel_bsp() -> get_microcode_blob() ->
                        find_microcode_in_initrd() returns GenuineIntel.bin out of the separate Intel cpio: its size
                        and its first patch header, read from guest memory, are the file's (the early loader found
                        the microcode where rime-boot puts it, the way it boots on metal)
  U5-loader-control     the same with the initrd alone: find_microcode_in_initrd() finds nothing
"""
import json
import os
import re
import socket
import struct
import subprocess
import sys
import time

LAB = os.environ.get("LAB", "/lab")
OUT = os.environ.get("OUT", "/out")
CODE = f"{LAB}/fw/code-secboot.raw"
VARS0 = f"{LAB}/fw/vars-ms-only.raw"
DISK = f"{LAB}/disk.raw"
RIME_SHA256 = "2fdd2f55e18d6d56ca97094bb9ffa116d9ad962c8cb5591484ee1e2a6958bcbb"
ANSI = re.compile(rb"\x1b\[[0-9;?]*[A-Za-z]|\x1b[()][A-Z0-9]|\r")
REFUSED = re.compile(r"bad shim\s+signature|Security Policy Violation|Binary is not authorized|Access Denied")


def stripped(path):
    try:
        with open(path, "rb") as f:
            return ANSI.sub(b"", f.read()).decode("utf-8", "replace")
    except FileNotFoundError:
        return ""


UCODE = ("/probe/intel-ucode.img", "/probe/amd-ucode.img")
OPTS = "console=ttyS0,115200 rdinit=/init panic=-1 rime.mok=probe"


def mput(text, dest):
    path = f"{LAB}/put.tmp"
    with open(path, "w") as f:
        f.write(text)
    subprocess.run(["mcopy", "-o", "-i", f"{DISK}@@1M", path, f"::{dest}"], check=True)


def piece_sums(how):
    """[(len, sha256)] of the files the stage names, in its order (what /sys/firmware/initrd must be)."""
    import hashlib
    names = (["intel-ucode.img", "amd-ucode.img"] if how != "none" else []) + ["initrd.img"]
    out = []
    for n in names:
        data = subprocess.run(["mtype", "-i", f"{DISK}@@1M", f"::/probe/{n}"], capture_output=True, check=True).stdout
        out.append((len(data), hashlib.sha256(data).hexdigest()))
    return out


def sysmap(path):
    """{symbol: address} from a kernel's System.map (text symbols; the first of a duplicated name wins)."""
    out = {}
    with open(path) as f:
        for line in f:
            a, kind, name = line.split()[:3]
            if kind in "tTwW" and name not in out:
                out[name] = int(a, 16)
    return out


def owner(syms, addr):
    """The text symbol an address falls in (the nearest one at or below it)."""
    best = max(((a, n) for n, a in syms.items() if a <= addr), default=(0, None))
    return best[1]


def intel_blob(path):
    """kernel/x86/microcode/GenuineIntel.bin out of a nixpkgs intel-ucode.img (a newc cpio)."""
    data = open(path, "rb").read()
    off, blob = 0, None
    while data[off:off + 6] == b"070701":
        f = [int(data[off + 6 + 8 * i: off + 14 + 8 * i], 16) for i in range(13)]
        size, nsize = f[6], f[11]
        name = data[off + 110: off + 110 + nsize - 1].decode()
        start = (off + 110 + nsize + 3) & ~3
        if name == "kernel/x86/microcode/GenuineIntel.bin":
            blob = data[start:start + size]
        if name == "TRAILER!!!":
            break
        off = (start + size + 3) & ~3
    return blob


def set_split_stage(how, extra=""):
    """rime-boot's split layout: the microcode cpios as their own files, before the initrd.
    bls: an entry with one `initrd` line per file, read by Fedora GRUB's blscfg. menu: one `initrd` line naming all
    three (rime-nixos.cfg's form). none: the initrd alone (the control: no microcode reaches rootfs)."""
    opts = OPTS + extra + " retain_initrd rime.pieces=" + ",".join(str(n) for n, _ in piece_sums(how))
    if how == "bls":
        entry = ("title Rime split-initrd probe\nversion 1\nlinux /probe/bootc.efi\n"
                 + "".join(f"initrd {u}\n" for u in UCODE)
                 + f"initrd /probe/initrd.img\noptions {opts}\n")
        mput(entry, "/loader/entries/rime-split.conf")
        mput("set timeout=0\nset default=rime-split\nblscfg\n", "/stage.cfg")
    else:
        ird = " ".join(list(UCODE if how == "menu" else ()) + ["/probe/initrd.img"])
        mput(f"set timeout=0\nlinux /probe/bootc.efi {opts}\ninitrd {ird}\nboot\n", "/stage.cfg")


def set_stage(kernel, mode):
    """What GRUB boots next: /stage.cfg on the ESP (mtools, the disk is not running)."""
    cfg = (
        "set timeout=0\n"
        f"linux /probe/{kernel}.efi console=ttyS0,115200 rdinit=/init panic=-1 rime.mok={mode}\n"
        "initrd /probe/initrd.img\n"
        "boot\n"
    )
    path = f"{LAB}/stage.cfg"
    with open(path, "w") as f:
        f.write(cfg)
    subprocess.run(["mcopy", "-o", "-i", f"{DISK}@@1M", path, "::/stage.cfg"], check=True)


class VM:
    def __init__(self, run, name, cpu="host", extra=()):
        self.run, self.name = run, name
        self.base = f"{run.dir}/{name}"
        self.serial = self.base + ".serial.log"
        self.qmp_path = f"/tmp/qmp-{name}.sock"
        for p in (self.serial, self.qmp_path):
            if os.path.exists(p):
                os.unlink(p)
        argv = [
            "qemu-system-x86_64",
            "-machine", "q35,smm=on,accel=kvm", "-cpu", cpu, "-m", "1536", "-smp", "2",
            "-global", "driver=cfi.pflash01,property=secure,value=on",
            "-global", "ICH9-LPC.disable_s3=1",
            "-drive", f"if=pflash,unit=0,format=raw,readonly=on,file={CODE}",
            "-drive", f"if=pflash,unit=1,format=raw,file={run.vars}",
            "-drive", f"if=none,id=boot,format=raw,file={DISK}",
            "-device", "virtio-blk-pci,drive=boot,bootindex=0",
            "-serial", f"file:{self.serial}",
            "-vga", "std", "-display", "none",
            "-qmp", f"unix:{self.qmp_path},server=on,wait=off",
            "-no-reboot", "-nic", "none",
            *extra,
        ]
        self.err = open(self.base + ".qemu.stderr", "wb")
        self.t0 = time.time()
        self.proc = subprocess.Popen(argv, stdout=self.err, stderr=subprocess.STDOUT)
        self.qmp = None
        self.shots = 0

    def text(self):
        return stripped(self.serial)

    def _qmp(self):
        if self.qmp is None:
            for _ in range(100):
                if os.path.exists(self.qmp_path):
                    break
                time.sleep(0.05)
            s = socket.socket(socket.AF_UNIX)
            s.connect(self.qmp_path)
            self.qmp = s.makefile("rw")
            json.loads(self.qmp.readline())
            self.cmd("qmp_capabilities")
        return self.qmp

    def cmd(self, execute, **args):
        f = self.qmp if execute == "qmp_capabilities" else self._qmp()
        f.write(json.dumps({"execute": execute, "arguments": args}) + "\n")
        f.flush()
        while True:
            r = json.loads(f.readline())
            if "return" in r or "error" in r:
                if "error" in r:
                    raise RuntimeError(f"QMP {execute}: {r['error']}")
                return r["return"]

    def key(self, *names, hold=80):
        for n in names:
            self.cmd("send-key", keys=[{"type": "qcode", "data": k} for k in n.split("+")], **{"hold-time": hold})
            time.sleep(0.35)

    def type_text(self, s):
        for ch in s:
            if ch.isdigit() or ch.islower():
                self.key(ch)
            else:
                raise ValueError(f"cannot type {ch!r} as a bare key code")

    def shot(self, label):
        self.shots += 1
        path = f"{self.base}-{self.shots:02d}-{label}.png"
        self.cmd("screendump", filename=path, format="png")
        return os.path.basename(path)

    def wait(self, pattern, timeout, since=0):
        """The first match of pattern in the serial text after offset `since` (MokManager redraws: an old screen's
        words must not satisfy the next step)."""
        rx = re.compile(pattern)
        end = time.time() + timeout
        while time.time() < end and self.proc.poll() is None:
            m = rx.search(self.text(), since)
            if m:
                return m
            time.sleep(0.2)
        return rx.search(self.text(), since)

    def wait_exit(self, timeout):
        try:
            self.proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            pass
        return self.proc.poll()

    def stop(self):
        if self.proc.poll() is None:
            self.proc.kill()
            self.proc.wait()
        with open(self.base + ".serial.txt", "w") as f:
            f.write(self.text())
        return round(time.time() - self.t0)


class Run:
    def __init__(self, name):
        self.name = name
        self.dir = f"{OUT}/{name}"
        os.makedirs(self.dir, exist_ok=True)
        self.vars = f"{LAB}/fw/vars-{name}.fd"
        self.rows = []
        self.ok = True

    def verdict(self, boot, passed, secs, what):
        self.ok &= passed
        row = f"{boot}\t{'PASS' if passed else 'FAIL'}\t{secs}s\t{what}"
        self.rows.append(row)
        print(row, flush=True)
        with open(f"{self.dir}/results.tsv", "w") as f:
            f.write("boot\tverdict\ttime\twhat\n" + "\n".join(self.rows) + "\n")

    def refused(self, boot, kernel):
        set_stage(kernel, "probe")
        vm = VM(self, boot)
        m = vm.wait(REFUSED.pattern, 240)
        time.sleep(2)
        t = vm.text()
        secs = vm.stop()
        ran = "RIME-MOK: reached" in t
        self.verdict(boot, bool(m) and not ran, secs,
                     f"{kernel}.efi refused: {m.group(0) if m else 'NO refusal seen'}; kernel ran: {ran}")

    def probe(self, boot, kernel, want_rime, staged=False, want_ucode=None, want_pieces=None):
        if not staged:
            set_stage(kernel, "probe")
        vm = VM(self, boot)
        vm.wait(r"DONE probe|" + REFUSED.pattern, 300)
        rc = vm.wait_exit(20)
        t = vm.text()
        secs = vm.stop()
        if REFUSED.search(t):
            self.verdict(boot, False, secs, f"{kernel}.efi REFUSED by shim: {REFUSED.search(t).group(0)}")
            return
        lines = [l for l in t.splitlines() if l.startswith("RIME-MOK:")]
        rel = re.search(r"RIME-MOK: reached: kernel=(\S+)", t)
        sb = "SecureBoot enabled" in t and re.search(r"secureboot-var=6 0 0 0 1\b", t)
        keyring = re.search(r"Loaded X\.509 cert 'APEX-OS Secure Boot key \(andre\)", t)
        enrolled = "Subject: CN=APEX-OS Secure Boot key (andre)" in t
        lock = re.search(r"RIME-MOK: lockdown: (.*)", t)
        locked_msg = re.search(r"Kernel is locked down[^\n]*", t)
        # The microcode file names in the bytes GRUB handed the kernel (not rootfs: see mkdisk.sh's ucode-names).
        ec = re.search(r"RIME-MOK: ucode-names: ?(.*)", t)
        early = sorted(n.rsplit("/", 1)[1] for n in (ec.group(1) if ec else "").split())
        cpu = re.search(r"RIME-MOK: cpu: (.*)", t)
        cur = re.search(r"microcode: Current revision: (0x[0-9a-f]+)", t)
        passed = bool(rel and sb and "DONE probe" in t and rc is not None and (not want_rime or (keyring and enrolled)))
        if want_ucode is not None:
            passed = passed and ec is not None and early == sorted(want_ucode)
        got_pieces = [(int(a), b) for a, b in re.findall(r"RIME-MOK: initrd-piece: offset=\d+ len=(\d+) sha256=(\w+)", t)]
        size = re.search(r"RIME-MOK: initrd-image: size=(\d+)", t)
        if want_pieces is not None:
            passed = (passed and got_pieces == want_pieces and size is not None
                      # GRUB pads every initrd file to 4 bytes (the last one too): 4973393 B is 4973396 in the image.
                      and int(size.group(1)) == sum((n + 3) & ~3 for n, _ in want_pieces))
        self.verdict(boot, passed, secs,
                     f"kernel {rel.group(1) if rel else '?'}; SecureBoot {'enabled' if sb else 'NOT enabled'}; "
                     f"Rime MOK in MokListRT {bool(enrolled)}, in the kernel keyring {bool(keyring)}; "
                     f"lockdown {lock.group(1) if lock else '?'}; '{locked_msg.group(0) if locked_msg else 'no locked-down message'}'"
                     f"; microcode names in the initrd image: {early or 'none'}"
                     f"; cpu {cpu.group(1).strip() if cpu else '?'}; loader "
                     + (f"ran, 'Current revision: {cur.group(1)}'" if cur else "did not run (no 'Current revision' line)")
                     + (f"; initrd image {size.group(1) if size else '?'} B = pieces "
                        f"{[(n, h[:12]) for n, h in got_pieces]} (want {[(n, h[:12]) for n, h in want_pieces]})"
                        if want_pieces is not None else ""))
        with open(f"{self.dir}/{boot}.markers.txt", "w") as f:
            f.write("\n".join(lines) + "\n")

    def loader(self, boot, how, want_found):
        """The kernel's early microcode loader, read over QEMU's gdbstub (bootc kernel, its System.map, nokaslr).
        The guest CPU has NO hypervisor flag: with it the kernel turns the loader off (microcode_loader_disabled())
        and nothing about early loading can be seen in a VM. Hardware breakpoints, one at a time:
          get_microcode_blob() entry       its return address (must be in load_ucode_intel_bsp())
          find_microcode_in_initrd() entry  the struct cpio_data it returns through %rdi (sret)
          ... its return                    data + size: what the early loader found in the image GRUB handed over
          get_microcode_blob() return       %rax: the patch picked for this CPU, or NULL
        want_found: data/size must be GenuineIntel.bin out of the separate Intel cpio, byte for byte its first patch
        header; the control (no microcode file) must find nothing. The picked patch is recorded, not asserted: under
        KVM the guest's platform ID and revision are KVM's, so a patch for the host is normally not picked, and KVM
        ignores the write to MSR 0x79 anyway: the APPLIED revision is a hardware-only fact (P3)."""
        syms = sysmap(f"{LAB}/in/System.map.bootc")
        blob_fn, find_fn = syms["get_microcode_blob"], syms["find_microcode_in_initrd"]
        set_split_stage(how, " nokaslr")
        sock = f"/tmp/gdb-{boot}.sock"
        if os.path.exists(sock):
            os.unlink(sock)
        vm = VM(self, boot, cpu="host,-hypervisor", extra=("-S", "-gdb", f"unix:{sock},server=on,wait=off"))
        for _ in range(100):
            if os.path.exists(sock):
                break
            time.sleep(0.05)
        script = f"{self.dir}/{boot}.gdb"
        with open(script, "w") as f:
            # The cpio data is read through QEMU's physical-memory mode (the direct map may not be mapped yet this
            # early); PAGE_OFFSET is 0xffff888000000000 with nokaslr and 4-level paging.
            f.write("set pagination off\nset confirm off\nset architecture i386:x86-64\n"
                    f"target remote {sock}\n"
                    f"hbreak *{blob_fn:#x}\ncontinue\n"
                    "set $gret = *(unsigned long *)$rsp\n"
                    'printf "RIME-GDB: blob entry rip=%lx ret=%lx\\n", $rip, $gret\n'
                    f"delete\nhbreak *{find_fn:#x}\ncontinue\n"
                    "set $sret = $rdi\nset $fret = *(unsigned long *)$rsp\n"
                    'printf "RIME-GDB: find entry rip=%lx ret=%lx\\n", $rip, $fret\n'
                    "delete\nhbreak *$fret\ncontinue\n"
                    "set $cdata = *(unsigned long *)$sret\nset $csize = *(unsigned long *)($sret + 8)\n"
                    'printf "RIME-GDB: cpio rip=%lx data=%lx size=%lx\\n", $rip, $cdata, $csize\n'
                    "if $cdata >= 0xffff888000000000\n"
                    "  maint packet Qqemu.PhyMemMode:1\n"
                    "  x/12wx $cdata - 0xffff888000000000\n"
                    "  maint packet Qqemu.PhyMemMode:0\n"
                    "end\n"
                    'printf "RIME-GDB: cpio-end\\n"\n'
                    "delete\nhbreak *$gret\ncontinue\n"
                    'printf "RIME-GDB: blob return rip=%lx mc=%lx\\n", $rip, $rax\n'
                    "delete\ndetach\nquit\n")
        g = subprocess.run(["/usr/libexec/gdb", "-nx", "-batch", "-x", script], capture_output=True, text=True, timeout=300)
        gout = g.stdout + g.stderr
        with open(f"{self.dir}/{boot}.gdb.txt", "w") as f:
            f.write(gout)
        vm.wait(r"DONE probe|" + REFUSED.pattern, 300)
        rc = vm.wait_exit(20)
        t = vm.text()
        secs = vm.stop()
        ent = re.search(r"RIME-GDB: blob entry rip=([0-9a-f]+) ret=([0-9a-f]+)", gout)
        fnd = re.search(r"RIME-GDB: find entry rip=([0-9a-f]+) ret=([0-9a-f]+)", gout)
        cpio = re.search(r"RIME-GDB: cpio rip=([0-9a-f]+) data=([0-9a-f]+) size=([0-9a-f]+)", gout)
        ret = re.search(r"RIME-GDB: blob return rip=([0-9a-f]+) mc=([0-9a-f]+)", gout)
        span = gout.split("RIME-GDB: cpio rip", 1)[-1].split("RIME-GDB: cpio-end", 1)[0] if cpio else ""
        words = [int(w, 16) for w in re.findall(r"0x[0-9a-f]{8}\b(?!:)", span.split("\n", 1)[-1])]
        caller = owner(syms, int(ent.group(2), 16)) if ent else None
        fcaller = owner(syms, int(fnd.group(2), 16)) if fnd else None
        size = int(cpio.group(3), 16) if cpio else -1
        hv = re.search(r"hypervisor-flag=(\d)", t)
        cur = re.search(r"microcode: Current revision: (0x[0-9a-f]+)", t)
        why = (f"get_microcode_blob {'called from ' + str(caller) if ent else 'NOT hit'}; "
               f"find_microcode_in_initrd {'called from ' + str(fcaller) if fnd else 'NOT hit'}; "
               f"it returned data={cpio.group(2) if cpio else '?'} size={size}")
        passed = bool(ent and fnd and cpio and ret and caller == "load_ucode_intel_bsp"
                      and fcaller == "get_microcode_blob" and "DONE probe" in t and rc is not None
                      and hv and hv.group(1) == "0" and cur)
        if want_found:
            bin_ = intel_blob(f"{LAB}/in/intel-ucode.img") or b""
            first = list(struct.unpack_from("<12I", bin_, 0)) if len(bin_) >= 48 else []
            passed = passed and size == len(bin_) and words[:12] == first and len(first) == 12
            why += (f" (GenuineIntel.bin in intel-ucode.img: {len(bin_)} B, "
                    f"{'equal' if size == len(bin_) else 'DIFFERENT'}); first patch header read from guest memory "
                    f"{'= the file' if words[:12] == first and first else 'DOES NOT match the file'}"
                    + (f" (rev {words[1]:#x} sig {words[3]:#x} date {words[2]:#010x})" if len(words) >= 4 else ""))
        else:
            passed = passed and size == 0 and int(cpio.group(2), 16) == 0
            why += " (want nothing: no microcode file in this boot's image)"
        why += (f"; get_microcode_blob returned mc={ret.group(2) if ret else '?'} (recorded, not asserted)"
                f"; guest hypervisor flag {hv.group(1) if hv else '?'}; kernel 'Current revision: "
                f"{cur.group(1) if cur else '-'}' (KVM ignores the MSR 0x79 write: applied revision = P3/hardware)")
        self.verdict(boot, passed, secs, why)
        with open(f"{self.dir}/{boot}.markers.txt", "w") as f:
            f.write("\n".join(l for l in t.splitlines() if l.startswith("RIME-MOK:")) + "\n")

    def import_cert(self):
        set_stage("fedora", "import")
        vm = VM(self, "I1")
        rc = vm.wait_exit(300)
        t = vm.text()
        secs = vm.stop()
        passed = (rc is not None and "RIME-MOK: import rc=0" in t and "DONE import" in t
                  and re.search(r"RIME-MOK: pending\|\s+Subject: CN=APEX-OS Secure Boot key \(andre\)", t)
                  and "SecureBoot enabled" in t)
        lock = re.search(r"RIME-MOK: lockdown: (.*)", t)
        locked_msg = re.search(r"Kernel is locked down[^\n]*", t)
        self.verdict("I1", passed, secs,
                     f"Fedora kernel under SB ran mokutil --import (rc=0: {'RIME-MOK: import rc=0' in t}); "
                     f"pending key listed; lockdown {lock.group(1) if lock else '?'}; "
                     f"'{locked_msg.group(0) if locked_msg else 'no locked-down message'}'")

    def mokmanager(self, boot="M1", staged=False, pw=None):
        """M1: shim starts MokManager on the next boot; walk its screens with QMP keys and screenshot each one.
        staged: the disk boots as it is (the installer mode's installed disk), no /stage.cfg is written."""
        if not staged:
            set_stage("bootc", "probe")
        vm = VM(self, boot)
        steps = []
        ok = True
        pos = 0

        def screen(pattern, label, timeout=60):
            nonlocal ok, pos
            m = vm.wait(pattern, timeout, pos)
            if m:
                pos = m.end()
            time.sleep(1.0)   # the text lands on serial before the frame is drawn
            name = vm.shot(label)
            steps.append(f"{label}:{'seen' if m else 'NOT seen'}:{name}")
            if not m:
                ok = False
            return bool(m)

        pw = pw or open(f"{LAB}/keys/mok-password").read().strip()
        if os.environ.get("RIME_MOK_MUTANT") == "wrong-password":
            pw += "x"   # the mutant: MokManager must refuse, and nothing may be enrolled
        if screen(r"Press any key to perform MOK management", "press-any-key", 120):
            vm.key("spc")
        if ok and screen(r"Perform MOK management", "menu"):
            vm.key("down"); time.sleep(0.5); vm.shot("menu-enroll-selected"); vm.key("ret")
        if ok and screen(r"\[Enroll MOK\]|View key 0", "enroll-mok"):
            vm.key("down"); time.sleep(0.5); vm.shot("enroll-continue-selected"); vm.key("ret")
        if ok and screen(r"Enroll the key\(s\)\?", "enroll-confirm"):
            vm.key("down"); time.sleep(0.5); vm.shot("enroll-yes-selected"); vm.key("ret")
        if ok and screen(r"Password:", "password"):
            vm.type_text(pw)
            vm.key("ret")
        if ok and screen(r"Reboot", "reboot", 60):
            vm.key("ret")
        # MokManager resets the machine; -no-reboot turns that into QEMU exiting.
        rc = vm.wait_exit(60)
        secs = vm.stop()
        t = stripped(vm.serial)
        passed = ok and rc is not None and "RIME-MOK: reached" not in t
        self.verdict(boot, passed, secs, "MokManager: " + " ".join(steps) + f"; reset rc={rc}")

    def varstore(self, want_sha256=RIME_SHA256):
        out = subprocess.run(["virt-fw-vars", "--input", self.vars, "--print", "--verbose"],
                             capture_output=True, text=True).stdout
        with open(f"{self.dir}/varstore.txt", "w") as f:
            f.write(out)
        # Extract MokList and count its certificates: the DER of every X.509 entry.
        tmp = f"{LAB}/moklist"
        subprocess.run(["rm", "-rf", tmp])
        os.makedirs(tmp)
        r = subprocess.run(["virt-fw-vars", "--input", self.vars, "--extract-certs"], cwd=tmp,
                           capture_output=True, text=True)
        certs = sorted(n for n in os.listdir(tmp) if "mok" in n.lower())
        fps = []
        for n in certs:
            d = subprocess.run(["openssl", "x509", "-in", f"{tmp}/{n}", "-outform", "DER"], capture_output=True).stdout
            fp = subprocess.run(["sha256sum"], input=d, capture_output=True).stdout.split()[0].decode()
            subj = subprocess.run(["openssl", "x509", "-in", f"{tmp}/{n}", "-noout", "-subject"],
                                  capture_output=True, text=True).stdout.strip()
            fps.append((n, fp, subj))
        with open(f"{self.dir}/moklist.txt", "w") as f:
            f.write(r.stdout + r.stderr + "\n" + "\n".join(f"{n} {fp} {s}" for n, fp, s in fps) + "\n")
        passed = len(fps) == 1 and fps[0][1] == want_sha256
        self.verdict("V", passed, 0, f"MokList certificates: {[(fp[:16], s) for _, fp, s in fps]}")


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "run"
    if mode == "run":
        r = Run("run-mutant" if os.environ.get("RIME_MOK_MUTANT") else "run")
        subprocess.run(["cp", VARS0, r.vars], check=True)
        r.refused("C1", "bootc")
        r.import_cert()
        r.mokmanager()
        r.probe("R1", "bootc", want_rime=True)
        r.refused("R2", "testmok")
        r.varstore()
    elif mode == "installer":
        # The NixOS installer's own enrolment (nix/installer/lab, `install plain` on a Microsoft-keys-only varstore:
        # rime-install-nixos.sh's nixos_mok() ran `mokutil --import` of the TEST MOK with the answers' mokpw). The
        # installed disk boots with the install's own varstore copy: shim finds MokNew and starts MokManager.
        global CODE, DISK
        CODE, DISK = os.environ["MOK_CODE"], os.environ["MOK_DISK"]
        r = Run("installer-mutant" if os.environ.get("RIME_MOK_MUTANT") else "installer")
        subprocess.run(["cp", os.environ["MOK_VARS"], r.vars], check=True)
        r.mokmanager("MI1", staged=True, pw=os.environ["MOK_PASSWORD"])
        r.varstore(want_sha256=os.environ["MOK_CERT_SHA256"])
        subprocess.run(["cp", r.vars, f"{r.dir}/enrolled.vars.fd"], check=True)
    elif mode == "realsig":
        r = Run("realsig")
        subprocess.run(["cp", f"{LAB}/fw/vars-run.fd", r.vars], check=True)
        r.probe("S1", "nixos", want_rime=True)
    elif mode == "split":
        r = Run("split")
        subprocess.run(["cp", f"{LAB}/fw/vars-run.fd", r.vars], check=True)
        names = ["AuthenticAMD.bin", "GenuineIntel.bin"]
        for boot, how in (("U1-bls", "bls"), ("U2-menu", "menu"), ("U3-control", "none")):
            set_split_stage(how)
            r.probe(boot, "bootc", want_rime=True, staged=True, want_pieces=piece_sums(how),
                    want_ucode=names if how != "none" else [])
        r.loader("U4-loader", "menu", want_found=True)
        r.loader("U5-loader-control", "none", want_found=False)
    else:
        sys.exit(f"usage: drive.py run|realsig|split|installer (got {mode})")
    print(f"{r.name}: {'PASS' if r.ok else 'FAIL'}")
    sys.exit(0 if r.ok else 1)


if __name__ == "__main__":
    main()
