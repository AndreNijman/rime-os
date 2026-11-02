#!/usr/bin/env python3
"""drive-bridge.py KIND: the bridge's OVMF proof for disk KIND, in KVM guests only (README.md here). A = topology A
plain, L = topology A in the installer's default encrypted layout (LUKS2 root, ext4 /boot), E = topology E.

Runs INSIDE the rime-ovmf-probe container (qemu, Fedora's OVMF). Every boot is one qemu process on the same copy-on-write
overlay of disks/KIND/disk.raw and the same varstore copy, ending with a guest poweroff:

  bootc     log in on the serial console as the lab user (old TEST password), make KIND's firmware entries
            (guest-prep.sh), reboot so it boots through its own entry, [E: repair-e], check, start
  nixos     GRUB's default is the NixOS entry the bridge added: a serial login with the OLD password (mounts,
            carried state, home checksums), then a GRAPHICAL login through rime-greet with the same old password typed
            on the emulated keyboard: Hyprland starts for the user through greetd, the desktop is screenshotted
  back      pick the first GRUB entry (the ostree one) by key: bootc boots, the same login works, home intact

Secure Boot: --sb boots with the varstore run.sh makes (Microsoft keys, Secure Boot ON, MokList = the real Rime MOK
certificate that signs the published bootc kernel + the lab's TEST MOK that signs the lab NixOS kernel); without it,
the key-less varstore (Secure Boot off). Every boot records what the guest itself reports.
L disks: every boot waits for the initrd's passphrase prompt and types the TEST passphrase on the emulated keyboard
(QMP send-key), in the bootc (dracut) initrd and the NixOS (systemd) initrd alike.
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
COMPLETE = re.compile(rb"\x1b\[[0-9;?]*[A-Za-z]|\x1b[()][0-9A-Za-z]|\x1b[=>\\]|\x1b[^\[()=>\]P]")
PASS = os.environ.get("RIME_LAB_PASS", "rime-lab-OLD-1")
LUKS_PASS = os.environ.get("RIME_LAB_LUKS", "rimelabluks1")
USER = "alex"
# QMP qcodes for what the lab passwords need: letters, digits, '-'.
QCODE = {**{c: c for c in "abcdefghijklmnopqrstuvwxyz0123456789"}, "-": "minus"}
SB_STATE = ("od -An -t u1 /sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c"
            " | awk '{print \"SecureBoot efivar=\" $NF}'; dmesg | grep -i -E 'secure ?boot|Loaded X.509 cert' | head -n 12")


class Timeout(Exception):
    pass


class VM:
    def __init__(self, a, name):
        self.a, self.name = a, name
        self.w = a.work
        self.log = open(os.path.join(a.logs, f"{a.kind}-{name}.serial.log"), "wb")
        self.buf = b""
        self.raw = b""
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
            # The serial socket delivers a few bytes at a time, so an escape sequence is often split across reads:
            # strip only up to an escape that may still be incomplete, and keep that tail for the next read.
            self.raw += d
            cut = len(self.raw)
            esc = self.raw.rfind(b"\x1b")
            if esc >= 0 and len(self.raw) - esc < 64 and not COMPLETE.match(self.raw, esc):
                cut = esc
            with self.lock:
                self.buf += ANSI.sub(b"", self.raw[:cut])
            self.raw = self.raw[cut:]

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

    def type_keys(self, text):
        for ch in text:
            self.key(*(["shift", QCODE[ch.lower()]] if ch.isupper() else [QCODE[ch]]))
            time.sleep(0.08)

    def unlock(self, timeout=900):
        """L disks: wait until the initrd asks for the LUKS passphrase (dracut's systemd-ask-password-console prints
        its prompt on the serial console; the NixOS lab initrd prints a marker, system.nix), then type it."""
        if self.a.kind != "L":
            return
        self.expect(r"Please enter passphrase|RIME-LAB: PASSPHRASE-ASKED", timeout,
                    fail=r"Kernel panic|You are in emergency|Timed out waiting for device")
        time.sleep(3)
        self.screendump(os.path.join(self.a.logs, f"{self.a.kind}-{self.name}-passphrase.png"))
        self.type_keys(LUKS_PASS)
        self.key("ret")
        say(f"[{self.name}] LUKS passphrase typed on the keyboard")

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
    vm.unlock()
    vm.login()
    vm.root()
    record(vm, f"{vm.name}-cmdline", "cat /proc/cmdline; findmnt -no SOURCE,OPTIONS /sysroot; lsblk -o NAME,TYPE,FSTYPE,LABEL,MOUNTPOINTS")
    record(vm, f"{vm.name}-efi", "efibootmgr -v")
    rc, out = record(vm, f"{vm.name}-sb", "mokutil --sb-state; " + SB_STATE + "; mokutil --list-enrolled | grep -E 'Subject:|CN='",
                     ok=False)
    if a.sb and "SecureBoot efivar=1" not in out:
        raise Timeout(f"[{vm.name}] the guest does not report Secure Boot on")
    vm.sh("mkdir -p /run/payload && mount -o ro /dev/disk/by-id/virtio-rime-payload /run/payload")


def phase_bootc(a):
    vm = VM(a, "bootc1")
    boot_bootc(a, vm, True)
    record(vm, "prep", "bash /run/payload/guest-prep.sh " + a.kind)
    record(vm, "lsblk", "lsblk -o NAME,SIZE,FSTYPE,LABEL,UUID,MOUNTPOINTS")
    # Boot again: a real machine of either kind has booted through its own firmware entry many times (A: shim's
    # fallback made "Fedora" on the first boot; E: katana's "APEX-OS Primary").
    vm.send("systemctl poweroff\n")
    vm.wait_off()
    vm = VM(a, "bootc2")
    boot_bootc(a, vm, False)
    if a.kind == "E":
        record(vm, "check-before-repair", "/run/payload/rime-migrate check --explain", ok=False)
        record(vm, "repair-e-dry", "/run/payload/rime-migrate repair-e", ok=False)
        record(vm, "repair-e", "/run/payload/rime-migrate repair-e --confirm", timeout=600)
        record(vm, "after-repair", "efibootmgr -v; ls -la /var/lib/rime/boot-migrate/ /var/lib/rime/boot-migrate/archive-*/;"
               " cat /var/lib/rime/boot-migrate/phase; mkdir -p /run/e && mount -o ro LABEL=EFI-SYSTEM /run/e 2>/dev/null"
               " || mount -o ro /dev/vda2 /run/e; find /run/e -maxdepth 3 | sort; umount /run/e")
    vm.sh("mkfs.ext4 -q -F -L rime-backup /dev/disk/by-id/virtio-rime-backup && mkdir -p /run/backup"
          " && mount /dev/disk/by-id/virtio-rime-backup /run/backup")
    # D2: the left-behind report BEFORE anything is written (--payload previews the new system's carry plan).
    rc, out = record(vm, "check", "/run/payload/rime-migrate check --explain --backup /run/backup"
                     " --payload /run/payload/payload", ok=False)
    if "left behind" not in out.lower():
        raise Timeout("check --explain --payload printed no left-behind report")
    rc, out = record(vm, "check-json", "/run/payload/rime-migrate check --json --backup /run/backup", ok=False)
    if a.rollback_first:
        rollback_probe(a, vm)
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


def rollback_probe(a, vm):
    """H8's failure path: the same start, with ONE fault injected after the boot hook has written custom.cfg, its menu
    file and the kernels (the step "fsck gate after" raises). The rollback code itself is the shipped one. Afterwards
    nothing start made may remain: no new subvolume, the old label, no masks, no hook files, custom.cfg as before."""
    label = "apex-root" if a.kind == "E" else "rime-root"
    show = ("btrfs subvolume list /sysroot; echo label=$(btrfs filesystem label /sysroot); ls /sysroot/boot /sysroot/boot/grub2;"
            " ls -la /etc/systemd/system | grep -E 'rime-update-check|rime-store-compact|bootloader-update|rime-boot-migrate'; cat /sysroot/boot/grub2/custom.cfg 2>&1")
    record(vm, "rollback-before", show + "; (sha256sum < /sysroot/boot/grub2/custom.cfg 2>/dev/null || echo absent)"
           " > /run/rb-custom; cat /run/rb-custom", ok=False)
    vm.sh("sed 's/^        step(\"fsck gate after\")$/        raise Failed(\"lab fault injection: after the boot entry\")/'"
          " /run/payload/rime-migrate > /run/rime-migrate-fault && chmod 755 /run/rime-migrate-fault"
          " && ! cmp -s /run/payload/rime-migrate /run/rime-migrate-fault")
    rc, out = record(vm, "rollback-start", "/run/rime-migrate-fault start --backup /run/backup"
                     " --payload /run/payload/payload --yes", timeout=5400, ok=False)
    if rc != 10 or "FAILED: lab fault injection" not in out or "== rolled back" not in out:
        raise Timeout(f"rollback probe: rc={rc}, want 10 with the injected fault and '== rolled back'")
    if "rime-boot: menu:" not in out:
        raise Timeout("rollback probe: the fault fired before the boot hook wrote its entry")
    # The verdict is computed in the guest; the echoed command line holds VERDICT=$v, never VERDICT=clean.
    rc, after = record(vm, "rollback-after", show + "; tail -n 4 /var/lib/rime/nixos-bridge/state.json; v=clean;"
                       " for s in @nixos @nix @home @pre-nixos; do btrfs subvolume list /sysroot | awk '{print $NF}'"
                       " | grep -x -- $s >/dev/null && v=subvol:$s; done;"
                       f" [ \"$(btrfs filesystem label /sysroot)\" = {label} ] || v=label;"
                       " ls /etc/systemd/system | grep -E '^(rime-update-check|rime-store-compact|bootloader-update|rime-boot-migrate)' >/dev/null && v=masks;"
                       " [ -e /sysroot/boot/rime ] && v=boot-rime; [ -e /sysroot/boot/grub2/rime-nixos.cfg ] && v=menu;"
                       " [ \"$(sha256sum < /sysroot/boot/grub2/custom.cfg 2>/dev/null || echo absent)\" = \"$(cat /run/rb-custom)\" ]"
                       " || v=custom.cfg; echo VERDICT=$v", ok=False)
    if not re.search(r"VERDICT=clean\b", after):
        raise Timeout("rollback probe: " + (re.findall(r"VERDICT=\S+", after) or ["no verdict"])[-1])
    say("[bootc2] rollback probe: nothing start made is left (VERDICT=clean)")


def phase_nixos(a):
    vm = VM(a, "nixos")
    vm.unlock()
    vm.expect(r"Welcome to (NixOS|Rime OS)", 900,
              fail=r"Kernel panic|emergency mode|You are in emergency|Timed out waiting for device")
    say("[nixos] NixOS reached the serial getty")
    time.sleep(20)
    vm.screendump(os.path.join(a.logs, f"{a.kind}-nixos-greeter.png"))
    vm.login()
    rc, out = record(vm, "nixos-checks",
                     "for i in $(seq 60); do systemctl is-active -q greetd && break; sleep 1; done;"
                     " echo greetd=$(systemctl is-active greetd); pgrep -a -u greetd | head -n 5;"
                     " id; cat /proc/cmdline; readlink /run/current-system; cat /etc/machine-id; hostname;"
                     " for m in / /nix /var/home /boot; do findmnt -no TARGET,SOURCE,FSTYPE,OPTIONS -M $m; done;"
                     " lsblk -o NAME,TYPE,FSTYPE,LABEL,MOUNTPOINTS; cat /var/lib/rime/host.toml;"
                     " echo phase=$(cat /var/lib/rime/boot-migrate/phase 2>/dev/null || echo none);"
                     " ls /var/lib/rime/boot-migrate 2>/dev/null; systemctl --failed --no-legend --no-pager;"
                     " echo last-user=$(cat /var/lib/rime-greet/last-user 2>/dev/null || echo none);"
                     " head -n 5 /var/lib/rime/carry/report.txt; " + SB_STATE + ";"
                     " cd ~ && sha256sum -c .rime-lab/sums && cat Documents/note.txt",
                     timeout=180)
    if "greetd=active" not in out:
        raise Timeout("[nixos] greetd is not active")
    # carry's host.toml must survive start's [boot] write (it once truncated it: rime-bridge write_boot_table)
    if not re.search(r"^timezone = ", out, re.M) or not re.search(r"^layout = \"custom-cfg\"", out, re.M):
        raise Timeout("[nixos] host.toml lost what carry wrote (no [host] timezone) or start's [boot] layout")
    if a.kind == "L" and "rd.luks.uuid=" not in out:
        raise Timeout("[nixos] no rd.luks.uuid= on the command line (host.toml [boot] kernel_params)")
    if a.kind == "E" and "phase=failed" not in out:
        raise Timeout("[nixos] phase=failed did not carry over")
    if a.kind == "L" and not re.search(r"^/boot\s+\S+\s+ext4", out, re.M):
        raise Timeout("[nixos] /boot is not the ext4 boot partition")
    if a.kind == "L" and not re.search(r"crypt\s+btrfs", out):
        raise Timeout("[nixos] the root is not the opened LUKS volume")
    if a.sb and "SecureBoot efivar=1" not in out:
        raise Timeout("[nixos] the guest does not report Secure Boot on")
    prefilled = "last-user=alex" in out
    vm.send("exit\n")
    # The graphical login: rime-greet (greetd's greeter) and the OLD password typed on the emulated keyboard.
    time.sleep(5)
    vm.screendump(os.path.join(a.logs, f"{a.kind}-nixos-greeter-before-login.png"))
    if not prefilled:
        vm.type_keys(USER)
        vm.key("ret")
        time.sleep(2)
    vm.type_keys(PASS)
    vm.key("ret")
    say(f"[nixos] graphical login typed (user prefilled by rime-greet: {prefilled})")
    time.sleep(45)
    vm.login()
    # Hyprland's process name on NixOS is its wrapper's (".Hyprland-wrapp"), so match the comm column with awk (never
    # pgrep -f: the pattern would be in this very command line). loginctl prints Name, TTY, Service, Type, Class,
    # State per session; "--" separates them.
    rc, out = record(vm, "nixos-graphical-login",
                     "for i in $(seq 180); do ls /run/user/1000/hypr/*/.socket.sock >/dev/null 2>&1 && break; sleep 1;"
                     " done; ps -u alex -o pid=,comm=,args= | awk '$2 ~ /Hyprland|quickshell/';"
                     " ls -1a /run/user/1000/hypr/*/;"
                     " for s in $(loginctl list-sessions --no-legend | awk '{print $1}'); do echo --;"
                     " loginctl show-session $s -p Name -p Type -p Class -p Service -p TTY -p State; done; echo --",
                     timeout=240, ok=False)
    blocks = [dict(ln.split("=", 1) for ln in b.split() if "=" in ln) for b in out.split("--")]
    greetd_wayland = any(b.get("Name") == "alex" and b.get("Service") == "greetd" and b.get("Type") == "wayland"
                         and b.get("State") == "active" for b in blocks)
    hypr = re.search(r"^\s*\d+ \S*Hyprland", out, re.M) and re.search(r"^\.socket\.sock\s*$", out, re.M)
    if not hypr or not greetd_wayland:
        raise Timeout("[nixos] no Hyprland session for alex started through greetd")
    time.sleep(10)
    vm.screendump(os.path.join(a.logs, f"{a.kind}-nixos-desktop.png"))
    say("[nixos] graphical login: Hyprland runs for alex in a greetd wayland session; desktop screenshotted")
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
    vm.unlock()
    vm.login()
    rc, out = record(vm, "back-checks", "cat /proc/cmdline; id; cd ~ && sha256sum -c .rime-lab/sums; rpm-ostree status 2>&1 | head -n 20; " + SB_STATE,
                     timeout=120)
    if "ostree=" not in out:
        raise Timeout("back: not an ostree boot")
    vm.root()
    record(vm, "back-state", "systemctl --failed --no-legend; ls /sysroot; btrfs subvolume list /sysroot", ok=False)
    vm.send("systemctl poweroff\n")
    vm.wait_off()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("kind", choices=["A", "L", "E"])
    ap.add_argument("--lab", default="/lab")
    ap.add_argument("--logs", default="/art")
    ap.add_argument("--code", default="/ovmf/code-secboot.raw")
    ap.add_argument("--vars", default="/ovmf/vars-sb-off.fd")
    ap.add_argument("--sb", action="store_true", help="Secure Boot ON with /lab/fw/vars-sb-bridge.fd (run.sh keys)")
    ap.add_argument("--phases", default="bootc,nixos,back")
    ap.add_argument("--fresh", action="store_true")
    ap.add_argument("--rollback-first", action="store_true",
                    help="before the real start, run start once with a fault after the boot entry (H8 rollback)")
    a = ap.parse_args()
    if a.sb:
        a.vars = os.path.join(a.lab, "fw", "vars-sb-bridge.fd")
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
    say(f"bridge OVMF proof {a.kind}: Secure Boot {'ON' if a.sb else 'OFF'} ({os.path.basename(a.code)}"
        f" + {os.path.basename(a.vars)})")
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
