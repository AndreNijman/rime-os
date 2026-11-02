#!/usr/bin/env python3
"""drive-bridge.py KIND: the bridge's OVMF proof for topology KIND (A or E), in KVM guests only (README.md here).

Runs INSIDE the rime-ovmf-probe container (qemu, Fedora's OVMF). Every boot is one qemu process on the same copy-on-write
overlay of disks/KIND/disk.raw and the same varstore copy, ending with a guest poweroff:

  bootc     log in on the serial console as the lab user (old TEST password), make KIND's firmware entries
            (guest-prep.sh), [E: reboot so the machine boots the way katana does, repair-e], check, start
  nixos     GRUB's default is the NixOS entry the bridge added: the lab system's markers, the greeter, a serial
            login with the OLD password, the home checksums
  back      pick the first GRUB entry (the ostree one) by key: bootc boots, the same login works, home intact

Secure Boot is OFF (OVMF Secure Boot code with the key-less varstore): the published image's kernel is signed by the
Rime MOK and the lab NixOS kernel by nothing, so an SB-on run needs MOK enrolment first (TODO(handover)).
Nothing here touches the host: the disks are files, the NVRAM is the varstore copy, every key press goes to the guest.
"""
import argparse
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import threading
import time
import zlib
import struct

ANSI = re.compile(rb"\x1b\[[0-9;?]*[A-Za-z]|\x1b[()][0-9A-Za-z]|\x1b[=>]|\r")
PASS = os.environ.get("RIME_LAB_PASS", "rime-lab-OLD-1")
USER = "alex"


class Timeout(Exception):
    pass


class VM:
    def __init__(self, a, name):
        self.a, self.name = a, name
        self.w = a.work
        self.log = open(os.path.join(a.logs, f"{a.kind}-{name}.serial.log"), "wb")
        self.buf = b""
        self.cur = 0
        self.lock = threading.Lock()
        ser, qmp = os.path.join(self.w, "serial.sock"), os.path.join(self.w, "qmp.sock")
        for s in (ser, qmp):
            if os.path.exists(s):
                os.unlink(s)
        # virtio-blk with a serial each, so the guest sees /dev/disk/by-id/virtio-<serial> (serial= is a -device
        # property, not a -drive one).
        drives = [("rime-disk", f"file={self.w}/disk.qcow2,format=qcow2")]
        if a.kind == "E":
            drives.append(("win-disk", f"file={self.w}/win.qcow2,format=qcow2"))
        drives += [("rime-payload", f"file={a.payload},format=raw,readonly=on"),
                   ("rime-backup", f"file={self.w}/backup.qcow2,format=qcow2")]
        cmd = ["qemu-system-x86_64", "-machine", "q35,smm=on,accel=kvm", "-cpu", "host", "-m", "6144", "-smp", "4",
               "-global", "driver=cfi.pflash01,property=secure,value=on", "-global", "ICH9-LPC.disable_s3=1",
               "-drive", f"if=pflash,unit=0,format=raw,readonly=on,file={a.code}",
               "-drive", f"if=pflash,unit=1,format=raw,file={self.w}/vars.fd"]
        for i, (serial, spec) in enumerate(drives):
            # no bootindex: OVMF would rewrite BootOrder from it, and topology E's order is the point
            cmd += ["-drive", f"{spec},if=none,id=d{i}", "-device", f"virtio-blk-pci,drive=d{i},serial={serial}"]
        cmd += ["-chardev", f"socket,id=ser0,path={ser},server=on,wait=off",
                "-serial", "chardev:ser0", "-qmp", f"unix:{qmp},server=on,wait=off",
                "-device", "VGA", "-display", "none", "-nic", "none", "-no-reboot"]
        say(f"[{name}] qemu: {' '.join(cmd)}")
        self.p = subprocess.Popen(cmd, stdout=open(os.path.join(a.logs, f"{a.kind}-{name}.qemu.log"), "wb"),
                                  stderr=subprocess.STDOUT)
        for _ in range(100):
            if os.path.exists(ser) and os.path.exists(qmp):
                break
            time.sleep(0.1)
        self.s = socket.socket(socket.AF_UNIX)
        self.s.connect(ser)
        self.q = socket.socket(socket.AF_UNIX)
        self.q.connect(qmp)
        self.qf = self.q.makefile("rwb")
        self.qf.readline()
        self.qmp("qmp_capabilities")
        threading.Thread(target=self._reader, daemon=True).start()

    def _reader(self):
        while True:
            try:
                d = self.s.recv(65536)
            except OSError:
                return
            if not d:
                return
            self.log.write(d)
            self.log.flush()
            with self.lock:
                self.buf += ANSI.sub(b"", d)

    def qmp(self, cmd, **args):
        self.qf.write(json.dumps({"execute": cmd, "arguments": args}).encode() + b"\n")
        self.qf.flush()
        while True:
            r = json.loads(self.qf.readline())
            if "return" in r or "error" in r:
                return r

    def expect(self, pat, timeout, fail=None):
        rx = re.compile(pat.encode() if isinstance(pat, str) else pat)
        frx = re.compile(fail.encode()) if fail else None
        end = time.time() + timeout
        while time.time() < end:
            with self.lock:
                m = rx.search(self.buf, self.cur)
                f = frx.search(self.buf, self.cur) if frx else None
            if f and (not m or f.start() < m.start()):
                raise Timeout(f"[{self.name}] failure text {fail!r}: {f.group(0)!r}")
            if m:
                self.cur = m.end()
                return m
            if self.p.poll() is not None:
                raise Timeout(f"[{self.name}] qemu exited ({self.p.returncode}) waiting for {pat!r}")
            time.sleep(0.2)
        raise Timeout(f"[{self.name}] {timeout}s without {pat!r}")

    def send(self, text):
        self.s.sendall(text.encode())

    def key(self, *keys):
        self.qmp("send-key", keys=[{"type": "qcode", "data": k} for k in keys])

    def login(self, timeout=900):
        self.expect(r"login: $|login: ", timeout)
        time.sleep(1)
        self.send(USER + "\n")
        self.expect(r"[Pp]assword: ?", 60)
        self.send(PASS + "\n")
        self.expect(r"\$ |\] ?\$|> |❯ ", 120, fail=r"Login incorrect")
        self.send("unset PROMPT_COMMAND; stty cols 1000 rows 50; export PS1='LAB$ '\n")
        self.expect(r"LAB\$ ", 30)

    _n = 0

    def sh(self, cmd, timeout=300, ok=True):
        """Run CMD in the guest shell, return (rc, output)."""
        VM._n += 1
        tok = f"Z{VM._n}Q"
        start = len(self.buf)
        self.cur = start
        self.send(f"{cmd}; echo \"RC=$?:{tok}\"\n")
        m = self.expect(rf"RC=(\d+):{tok}", timeout)
        with self.lock:
            out = self.buf[start:m.start()].decode("utf-8", "replace")
        rc = int(m.group(1))
        if ok and rc != 0:
            raise Timeout(f"[{self.name}] `{cmd}` rc={rc}:\n{out[-3000:]}")
        return rc, out

    def root(self):
        # 'SUDO''PW:' so the echoed command line never matches the prompt the expect waits for
        self.send("sudo -S -p 'SUDO''PW:' -i\n")
        self.expect(r"SUDOPW:", 60)
        self.send(PASS + "\n")
        time.sleep(2)
        self.send("unset PROMPT_COMMAND; export PS1='ROOT# '; dmesg -n 1; stty cols 1000\n")
        self.expect(r"ROOT# ", 60)

    def screendump(self, path):
        ppm = os.path.join(self.w, "shot.ppm")
        r = self.qmp("screendump", filename=ppm)
        if "error" in r:
            say(f"screendump: {r}")
            return False
        ppm_to_png(ppm, path)
        return True

    def wait_off(self, timeout=300):
        try:
            self.p.wait(timeout)
        except subprocess.TimeoutExpired:
            say(f"[{self.name}] no poweroff in {timeout}s: killing qemu {self.p.pid}")
            self.p.kill()
            self.p.wait()
            raise Timeout(f"[{self.name}] guest did not power off")
        finally:
            self.s.close()
            self.q.close()
            self.log.close()


def ppm_to_png(src, dst):
    with open(src, "rb") as f:
        data = f.read()
    parts = data.split(maxsplit=4)
    w, h, body = int(parts[1]), int(parts[2]), parts[4]
    raw = b"".join(b"\x00" + body[y * w * 3:(y + 1) * w * 3] for y in range(h))

    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xffffffff)
    with open(dst, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
                + chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b""))


def say(m):
    print(time.strftime("%H:%M:%S"), m, flush=True)


RESULTS = {}


def record(vm, name, cmd, timeout=300, ok=True):
    rc, out = vm.sh(cmd, timeout, ok)
    with open(os.path.join(vm.a.logs, f"{vm.a.kind}-{name}.txt"), "w") as f:
        f.write(f"$ {cmd}\n{out}\n[rc={rc}]\n")
    say(f"[{vm.name}] {name}: rc={rc}")
    return rc, out


def boot_bootc(a, vm, first):
    vm.login()
    vm.root()
    record(vm, f"{vm.name}-cmdline", "cat /proc/cmdline; findmnt -no SOURCE,OPTIONS /sysroot")
    record(vm, f"{vm.name}-efi", "efibootmgr -v")
    vm.sh("mkdir -p /run/payload && mount -o ro /dev/disk/by-id/virtio-rime-payload /run/payload")


def phase_bootc(a):
    vm = VM(a, "bootc1")
    boot_bootc(a, vm, True)
    record(vm, "prep", "bash /run/payload/guest-prep.sh " + a.kind)
    record(vm, "lsblk", "lsblk -o NAME,SIZE,FSTYPE,LABEL,UUID,MOUNTPOINTS")
    if a.kind == "E":
        vm.send("systemctl poweroff\n")
        vm.wait_off()
        vm = VM(a, "bootc2")
        boot_bootc(a, vm, False)
        record(vm, "check-before-repair", "/run/payload/rime-migrate check --explain", ok=False)
        record(vm, "repair-e-dry", "/run/payload/rime-migrate repair-e", ok=False)
        record(vm, "repair-e", "/run/payload/rime-migrate repair-e --confirm", timeout=600)
        record(vm, "after-repair", "efibootmgr -v; ls -la /var/lib/rime/boot-migrate/ /var/lib/rime/boot-migrate/archive-*/;"
               " cat /var/lib/rime/boot-migrate/phase; mkdir -p /run/e && mount -o ro LABEL=EFI-SYSTEM /run/e 2>/dev/null"
               " || mount -o ro /dev/vda2 /run/e; find /run/e -maxdepth 3 | sort; umount /run/e")
    vm.sh("mkfs.ext4 -q -F -L rime-backup /dev/disk/by-id/virtio-rime-backup && mkdir -p /run/backup"
          " && mount /dev/disk/by-id/virtio-rime-backup /run/backup")
    record(vm, "check", "/run/payload/rime-migrate check --explain --backup /run/backup", ok=False)
    rc, out = record(vm, "check-json", "/run/payload/rime-migrate check --json --backup /run/backup", ok=False)
    rc, out = record(vm, "start", "time /run/payload/rime-migrate start --backup /run/backup"
                     " --payload /run/payload/payload --yes", timeout=5400, ok=False)
    if rc != 0:
        raise Timeout(f"start failed rc={rc}")
    record(vm, "after-start", "btrfs subvolume list /sysroot; cat /sysroot/boot/grub2/custom.cfg;"
           " ls -la /sysroot/boot /sysroot/boot/rime 2>/dev/null; cat /sysroot/boot/grub2/rime-nixos.cfg 2>/dev/null;"
           " ls -la /run/backup /run/backup/*; cat /var/lib/rime/nixos-bridge/state.json", ok=False)
    record(vm, "update-after-start", "rime update --check 2>&1 | tail -n 20", ok=False, timeout=120)
    vm.send("sync; systemctl poweroff\n")
    vm.wait_off()


def phase_nixos(a):
    vm = VM(a, "nixos")
    vm.expect(r"RIME-BRIDGE: READY", 900, fail=r"Kernel panic|emergency mode|You are in emergency")
    say("[nixos] lab markers READY")
    time.sleep(10)
    vm.screendump(os.path.join(a.logs, f"{a.kind}-nixos-greeter.png"))
    vm.login()
    record(vm, "nixos-checks", "id; cat /proc/cmdline; findmnt -no SOURCE,FSTYPE,OPTIONS / /nix /var/home /boot;"
           " systemctl is-active greetd; readlink /run/current-system; cat /var/lib/rime/boot-migrate/phase 2>/dev/null;"
           " cd ~ && sha256sum -c .rime-lab/sums && cat Documents/note.txt",
           timeout=120)
    vm.send("exit\n")
    time.sleep(2)
    vm.qmp("system_powerdown")
    vm.wait_off()


def phase_back(a):
    vm = VM(a, "back")
    # GRUB's menu reads the PS/2 keyboard through the firmware: from the first firmware line until the kernel starts,
    # keep pressing Up (the first entry is the newest ostree deployment), then Return.
    t0 = time.time()
    picked = False
    while time.time() - t0 < 120:
        with vm.lock:
            b = vm.buf
        if b"Linux version" in b or b"Booting `" in b:
            break
        if re.search(rb"Rime OS|ostree:0|highlighted entry", b):
            for _ in range(4):
                vm.key("up")
                time.sleep(0.2)
            vm.key("ret")
            picked = True
            break
        vm.key("up")
        time.sleep(0.3)
    say(f"[back] menu text seen and Return pressed: {picked}")
    vm.login()
    rc, out = record(vm, "back-checks", "cat /proc/cmdline; id; cd ~ && sha256sum -c .rime-lab/sums; rpm-ostree status 2>&1 | head -n 20",
                     timeout=120)
    if "ostree=" not in out:
        raise Timeout("back: not an ostree boot")
    vm.root()
    record(vm, "back-state", "systemctl --failed --no-legend; ls /sysroot; btrfs subvolume list /sysroot", ok=False)
    vm.send("systemctl poweroff\n")
    vm.wait_off()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("kind", choices=["A", "E"])
    ap.add_argument("--lab", default="/lab")
    ap.add_argument("--logs", default="/art")
    ap.add_argument("--code", default="/ovmf/code-secboot.raw")
    ap.add_argument("--vars", default="/ovmf/vars-sb-off.fd")
    ap.add_argument("--phases", default="bootc,nixos,back")
    ap.add_argument("--fresh", action="store_true")
    a = ap.parse_args()
    a.work = os.path.join(a.lab, f"run-{a.kind}")
    a.payload = os.path.join(a.lab, "payload.img")
    os.makedirs(a.logs, exist_ok=True)
    if a.fresh or not os.path.exists(os.path.join(a.work, "disk.qcow2")):
        shutil.rmtree(a.work, ignore_errors=True)
        os.makedirs(a.work)
        d = os.path.join(a.lab, "disks", a.kind)
        subprocess.run(["qemu-img", "create", "-q", "-f", "qcow2", "-F", "raw", "-b", f"{d}/disk.raw",
                        f"{a.work}/disk.qcow2"], check=True)
        if a.kind == "E":
            subprocess.run(["qemu-img", "create", "-q", "-f", "qcow2", "-F", "raw", "-b", f"{d}/win.raw",
                            f"{a.work}/win.qcow2"], check=True)
        subprocess.run(["qemu-img", "create", "-q", "-f", "qcow2", f"{a.work}/backup.qcow2", "40G"], check=True)
        shutil.copy(a.vars, os.path.join(a.work, "vars.fd"))
    say(f"bridge OVMF proof {a.kind}: Secure Boot OFF ({os.path.basename(a.code)} + {os.path.basename(a.vars)})")
    for ph in a.phases.split(","):
        say(f"== phase {ph}")
        {"bootc": phase_bootc, "nixos": phase_nixos, "back": phase_back}[ph](a)
        say(f"== phase {ph} PASS")
    say(f"BRIDGE OVMF {a.kind}: PASS")


if __name__ == "__main__":
    try:
        main()
    except Timeout as e:
        say(f"FAIL: {e}")
        sys.exit(1)
