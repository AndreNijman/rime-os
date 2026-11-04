#!/usr/bin/env python3
"""drive-bridge.py KIND: the bridge's OVMF proof for disk KIND, in KVM guests only (README.md here). A = topology A
plain, L = topology A in the installer's default encrypted layout (LUKS2 root, ext4 /boot), E = topology E.

Runs INSIDE the rime-ovmf-probe container (qemu, Fedora's OVMF). Every boot is one qemu process on the same copy-on-write
overlay of disks/KIND/disk.raw and the same varstore copy, ending with a guest poweroff:

  bootc     log in on the serial console as the lab user (old TEST password), make KIND's firmware entries
            (guest-prep.sh), reboot so it boots through its own entry, [E: repair-e], check, start; with
            --readiness the disk is the READINESS image (prepare.sh image) and every bridge command is that
            release's own `rime migrate ...`; the verb probes below run before and after start
  nixos     GRUB's default is the NixOS entry the bridge added: a serial login with the OLD password (mounts,
            carried state, home checksums), then a GRAPHICAL login through rime-greet with the same old password typed
            on the emulated keyboard: Hyprland starts for the user through greetd, the desktop is screenshotted
  back      pick the first GRUB entry (the ostree one) by key: bootc boots, the same login works, home intact;
            with --readiness every bootc verb must still refuse there (the record says done)
  finish    `rime migrate finish` (docs/nixos/bridge.md): phase bootc first seeds what it must keep and what it must
            free (finish-custom.cfg = the rime-secureboot-key block + a Windows-like entry appended to custom.cfg; 1 GiB
            of random files in home that NixOS then rewrites, so their old copies are the old side's alone). NixOS
            boot 2: refused too-few-good-boots; boot 3: --dry-run writes nothing; an overlay: --yes SIGKILLed mid-way
            + power cut, NixOS still boots, a re-run completes, a third run is refused; boot 4: --dry-run, --yes,
            `btrfs filesystem usage` before/after; boot 5: NixOS boots, home sums, no ostree entry in the GRUB menu
            (read off the serial console), the Secure Boot key entry and the Windows-like entry still there

The verb probes (--readiness, verb-probe.sh on the payload disk): every verb the readiness `rime` holds after a start
(11 forms) plus rime-update-check, run against the guest's REAL start record with bootc/ostree/... stubbed in a private
mount namespace. Allowed with no record, after a start that rolled itself back (same boot AND after a reboot into
ostree), refused after a finished start (same boot, and booted back into ostree after NixOS). --kill-probe and
--interrupt-probe run on throwaway overlays of the disk (the main disk's qemu is off): a start SIGKILLed after it masked
the old side (no rollback: refused, also after a reboot, and a second start refuses before writing), and a start
interrupted by Ctrl-C on the console during the closure install (rolls back: allowed).

Secure Boot: --sb boots with the varstore run.sh makes (Microsoft keys, Secure Boot ON, MokList = the real Rime MOK
certificate that signs the published bootc kernel + the lab's TEST MOK that signs the lab NixOS kernel, or with
RIME_LAB_KERNEL=release the Rime MOK alone and the release-signed NixOS kernel); without it,
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
# The user last ran Hyprland 0.56 on bootc: NixOS's 0.57 takes its "updated" path at the first graphical login.
HYPR_LAST = "0.56.2"
SB_STATE = ("od -An -t u1 /sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c"
            " | awk '{print \"SecureBoot efivar=\" $NF}'; dmesg | grep -i -E 'secure ?boot|Loaded X.509 cert' | head -n 12")


class Timeout(Exception):
    pass


class VM:
    def __init__(self, a, name, work=None):
        self.a, self.name = a, name
        self.w = work or a.work
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
        record(vm, "check-before-repair", f"{a.migrate} check --explain", ok=False)
        record(vm, "repair-e-dry", f"{a.migrate} repair-e", ok=False)
        record(vm, "repair-e", f"{a.migrate} repair-e --confirm", timeout=600)
        record(vm, "after-repair", "efibootmgr -v; ls -la /var/lib/rime/boot-migrate/ /var/lib/rime/boot-migrate/archive-*/;"
               " cat /var/lib/rime/boot-migrate/phase; mkdir -p /run/e && mount -o ro LABEL=EFI-SYSTEM /run/e 2>/dev/null"
               " || mount -o ro /dev/vda2 /run/e; find /run/e -maxdepth 3 | sort; umount /run/e")
    vm.sh("mkfs.ext4 -q -F -L rime-backup /dev/disk/by-id/virtio-rime-backup && mkdir -p /run/backup"
          " && mount /dev/disk/by-id/virtio-rime-backup /run/backup")
    if a.finish:
        finish_seed(vm)
    # D2: the left-behind report BEFORE anything is written (--payload previews the new system's carry plan).
    if a.readiness:
        readiness_facts(vm)
    # The lab user last ran Hyprland 0.56 here (its data file, as Hyprland writes it): phase nixos checks the
    # first NixOS graphical login opens no "updated to 0.57.0" window.
    vm.sh(f"install -d -o {USER} -g {USER} /var/home/{USER}/.local /var/home/{USER}/.local/share"
          f" /var/home/{USER}/.local/share/hyprland && printf {HYPR_LAST} > /var/home/{USER}/.local/share/hyprland/lastVersion"
          f" && chown {USER}:{USER} /var/home/{USER}/.local/share/hyprland/lastVersion")
    # What the user's own Hyprland config (bootc's firstrun copy, carried as is) says about the window: the
    # greeter-first run below proves the NixOS side wins whatever this is.
    record(vm, "bootc-hypr-config", f"ls -la --time-style=full-iso /var/home/{USER}/.config/hypr/rime/;"
           f" echo BOOTC-KNOB=$(grep -c no_update_news /var/home/{USER}/.config/hypr/rime/appearance.lua)", ok=False)
    rc, out = record(vm, "check", f"{a.migrate} check --explain --backup /run/backup"
                     " --payload /run/payload/payload", ok=False)
    if "left behind" not in out.lower():
        raise Timeout("check --explain --payload printed no left-behind report")
    rc, out = record(vm, "check-json", f"{a.migrate} check --json --backup /run/backup", ok=False)
    if "CPU vendor" not in out and "microcode" not in out:
        say("[bootc2] note: check --json names no CPU vendor (the carry preview is in check --explain)")
    if a.readiness:
        probe(vm, "no-record", "allowed")   # no start record: every bootc verb is allowed
    if a.rollback_first:
        rollback_probe(a, vm)
        if a.readiness:
            # failed + rolled back: allowed, in this boot and after booting the ostree system again
            probe(vm, "rolled-back", "allowed")
            poweroff(vm)
            vm = boot_for_probe(a, "bootc3", None)
            record(vm, "rolled-back-rebooted-boot", "cat /proc/cmdline; rpm-ostree status 2>&1 | head -n 12", ok=False)
            probe(vm, "rolled-back-rebooted", "allowed")
    if a.kill_probe or a.interrupt_probe:
        poweroff(vm)
        if a.kill_probe:
            kill_probe(a)
        if a.interrupt_probe:
            interrupt_probe(a)
        vm = boot_for_probe(a, "bootc4", None)
    rc, out = record(vm, "start", f"time {a.migrate} start --backup /run/backup"
                     " --payload /run/payload/payload --yes", timeout=5400, ok=False)
    if rc != 0:
        raise Timeout(f"start failed rc={rc}")
    record(vm, "after-start", "btrfs subvolume list /sysroot; findmnt /boot; for b in /sysroot/boot /boot; do echo \"== $b\";"
           " cat $b/grub2/custom.cfg; ls -la $b $b/rime/nixos 2>/dev/null; cat $b/grub2/rime-nixos.cfg 2>/dev/null; done;"
           " ls -la /run/backup /run/backup/*; cat /var/lib/rime/nixos-bridge/state.json", ok=False)
    if a.readiness:
        probe(vm, "done", "refused")
    else:
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
          f" {a.bridge} > /run/rime-migrate-fault && chmod 755 /run/rime-migrate-fault"
          f" && ! cmp -s {a.bridge} /run/rime-migrate-fault")
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


def readiness_facts(vm):
    """What the readiness image under test is: its rime, its bridge (the readiness branch's copy) and the payload key."""
    rc, out = record(vm, "readiness", "rime --help 2>&1 | grep -w migrate; head -n 1 /usr/libexec/rime-migrate;"
                     " echo bridge-minus-line-1=$(tail -n +2 /usr/libexec/rime-migrate | sha256sum | cut -d' ' -f1);"
                     " cat /usr/share/rime-os/trust/nixos-bridge-payload.pub; sha256sum /usr/bin/rime;"
                     " rpm -q --whatprovides /usr/bin/rime 2>&1 | head -n 1; cat /etc/os-release | grep -E '^(VERSION|IMAGE_VERSION)='",
                     ok=False)
    if "rime-bridge-lab-TEST-" not in out or "#!/usr/bin/python3 -I" not in out:
        raise Timeout("[bootc2] this is not the readiness lab image (no TEST payload key or no readiness bridge)")


def probe(vm, label, want):
    """verb-probe.sh in the guest: every held verb + rime-update-check against this guest's start record; the verdict
    (PROBE-VERDICT=ok) is computed in the guest."""
    rc, out = record(vm, f"probe-{label}", f"bash /run/payload/verb-probe.sh {label} {want}", timeout=3600, ok=False)
    v = re.findall(r"^VERBS=.*$", out, re.M)
    if "PROBE-VERDICT=ok" not in out:
        raise Timeout(f"[{vm.name}] verb probe {label}: want every verb {want}: {v[-1] if v else 'no summary'}")
    say(f"[{vm.name}] verb probe {label}: {v[-1]}")
    return out


def overlay(a, tag):
    """A throwaway copy-on-write layer over the run's disks and varstore (the main disk's qemu must be off)."""
    w = f"{a.work}-{tag}"
    shutil.rmtree(w, ignore_errors=True)
    os.makedirs(w)
    names = ["disk", "backup"] + (["win"] if a.kind == "E" else [])
    for n in names:
        subprocess.run(["qemu-img", "create", "-q", "-f", "qcow2", "-F", "qcow2", "-b", f"{a.work}/{n}.qcow2",
                        f"{w}/{n}.qcow2"], check=True)
    shutil.copy(os.path.join(a.work, "vars.fd"), os.path.join(w, "vars.fd"))
    return w


def boot_for_probe(a, name, work):
    vm = VM(a, name, work)
    boot_bootc(a, vm, False)
    vm.sh("mkdir -p /run/backup && mount /dev/disk/by-id/virtio-rime-backup /run/backup")
    return vm


def poweroff(vm):
    vm.send("sync; systemctl poweroff\n")
    vm.wait_off()


def kill_probe(a):
    """A start that did NOT roll back: SIGKILLed (nothing can catch it) right after it masked the old side. The record
    then ends at "old side" with neither `failed` nor `rolled back`: every bootc verb must refuse, after a reboot too,
    rime-update-check must say none, and a second start must refuse (start-unfinished) before it writes anything."""
    w = overlay(a, "kill")
    vm = boot_for_probe(a, "kill1", w)
    vm.sh(f"sed 's/^        step(\"fsck gate before\")$/        os.kill(os.getpid(), 9)/' {a.bridge} > /run/rime-migrate-kill"
          f" && chmod 755 /run/rime-migrate-kill && ! cmp -s {a.bridge} /run/rime-migrate-kill")
    rc, out = record(vm, "kill-start", "/run/rime-migrate-kill start --backup /run/backup --payload /run/payload/payload"
                     " --yes", timeout=5400, ok=False)
    if rc != 137:
        raise Timeout(f"kill probe: rc={rc}, want 137 (SIGKILL)")
    record(vm, "kill-after", "cat /var/lib/rime/nixos-bridge/state.json; echo; ls -la /etc/systemd/system | grep /dev/null;"
           " btrfs subvolume list /sysroot", ok=False)
    probe(vm, "killed-start", "refused")
    poweroff(vm)
    vm = boot_for_probe(a, "kill2", w)
    probe(vm, "killed-start-rebooted", "refused")
    rc, out = record(vm, "kill-second-start", "btrfs subvolume list /sysroot > /run/k1;"
                     f" {a.migrate} start --backup /run/backup --payload /run/payload/payload --yes; r=$?;"
                     " btrfs subvolume list /sysroot | cmp -s - /run/k1 && echo SUBVOLS-UNCHANGED; echo SECOND-START-RC=$r",
                     timeout=600, ok=False)
    if not re.search(r"^SECOND-START-RC=3$", out, re.M) or "REFUSED [start-unfinished]" not in out \
            or not re.search(r"^SUBVOLS-UNCHANGED$", out, re.M):
        raise Timeout("kill probe: a second start over the unfinished record did not refuse start-unfinished untouched")
    poweroff(vm)
    shutil.rmtree(w, ignore_errors=True)
    say("[kill] a start that did not roll back holds every bootc verb, after a reboot too; a second start refuses")


def interrupted_start(vm, bridge, name):
    """`BRIDGE start` on the console, interrupted with Ctrl-C (the console's own SIGINT) while it unpacks the closure.
    Returns the console text."""
    start = len(vm.buf)
    vm.cur = start
    vm.send(f"{bridge} start --backup /run/backup --payload /run/payload/payload --yes\n")
    vm.expect(r"== install closure", 3600, fail=r"REFUSED \[|FAILED:")
    time.sleep(15)
    vm.send("\x03")
    say(f"[{vm.name}] Ctrl-C sent during the closure install")
    mark = len(vm.buf)
    vm.cur = mark
    vm.expect(r"ROOT# ", 1800)
    with vm.lock:
        text = vm.buf[start:vm.cur].decode("utf-8", "replace")
    with open(os.path.join(vm.a.logs, f"{vm.a.kind}-{name}.txt"), "w") as f:
        f.write(text)
    return text


def interrupt_probe(a):
    """A start the user interrupts with Ctrl-C: the bridge catches KeyboardInterrupt (BaseException), removes what it
    made and records `rolled back`, so every bootc verb is allowed again. str(KeyboardInterrupt()) is empty, so the
    record's `failed` is "" with #137's bridge; this probe records how each reader answers that record: `rime` (Rust),
    rime-update-check (python) and the bridge's own check, #137's copy and this branch's (the payload disk's)."""
    w = overlay(a, "int")
    vm = boot_for_probe(a, "int1", w)
    show = ("cat /var/lib/rime/nixos-bridge/state.json; echo; btrfs subvolume list /sysroot;"
            " echo label=$(btrfs filesystem label /sysroot); ls /etc/systemd/system | grep -E '^(rime-|bootloader-)' ;"
            " ls /sysroot/boot | grep -x rime; ls /sysroot/boot/grub2 | grep rime")
    interrupted_start(vm, a.migrate, "int-start-readiness")
    rc, out = record(vm, "int-after-readiness", show + "; python3 -I -c 'import json; print(\"FAILED-FIELD=\" +"
                     " repr(json.load(open(\"/var/lib/rime/nixos-bridge/state.json\")).get(\"failed\")))'", ok=False)
    if '"rolled back"' not in out:
        raise Timeout("interrupt probe: the interrupted start did not record rolled back")
    probe(vm, "interrupted-start", "allowed")
    record(vm, "int-check-readiness", f"{a.migrate} check --explain --backup /run/backup 2>&1 | grep -E 'REFUSED|record'; echo rc=$?",
           ok=False)
    record(vm, "int-check-branch", "/run/payload/rime-migrate check --explain --backup /run/backup 2>&1"
           " | grep -E 'REFUSED|record'; echo rc=$?", ok=False)
    # A second interrupted start with this branch's bridge (allowed by its check over the record above): the record it
    # leaves names the interruption.
    text = interrupted_start(vm, "/run/payload/rime-migrate", "int-start-branch")
    if "REFUSED [" in text:
        raise Timeout("interrupt probe: this branch's bridge refused to start over a rolled-back record")
    rc, out = record(vm, "int-after-branch", show + "; python3 -I -c 'import json; print(\"FAILED-FIELD=\" +"
                     " repr(json.load(open(\"/var/lib/rime/nixos-bridge/state.json\")).get(\"failed\")))'", ok=False)
    record(vm, "int-check-readiness-2", f"{a.migrate} check --explain --backup /run/backup 2>&1 | grep -E 'REFUSED|record'",
           ok=False)
    probe(vm, "interrupted-start-branch", "allowed")
    poweroff(vm)
    shutil.rmtree(w, ignore_errors=True)


def phase_nixos(a):
    vm = VM(a, "nixos")
    vm.unlock()
    vm.expect(r"Welcome to (NixOS|Rime OS)", 900,
              fail=r"Kernel panic|emergency mode|You are in emergency|Timed out waiting for device")
    say("[nixos] NixOS reached the serial getty")
    time.sleep(20)
    vm.screendump(os.path.join(a.logs, f"{a.kind}-nixos-greeter.png"))
    # --greeter-first: the user's FIRST NixOS session is the greeter's (no serial login before it), so the user
    # manager, rime-shell-firstrun (it refreshes ~/.config/hypr/rime from the system) and Hyprland start together,
    # as on a real moved machine. The serial checks run after the graphical login instead.
    greeter_first = a.greeter_first and not a.logs.endswith("nixos-again")
    if greeter_first:
        graphical_login(a, vm, prefilled=True)  # carry writes last-user; if not, the password lands in the
        nixos_checks(a, vm)                     # user field, the login fails and so does this run
        if a.negative_control:
            update_window_negative_control(a, vm)
    else:
        vm.login()
        prefilled = nixos_checks(a, vm)
        vm.send("exit\n")
        graphical_login(a, vm, prefilled)
    vm.send("exit\n")
    time.sleep(2)
    vm.qmp("system_powerdown")
    vm.wait_off()


def nixos_checks(a, vm):
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
                     " grep -m1 vendor_id /proc/cpuinfo; grep -E '^\\s*initrd' /boot/grub2/rime-nixos.cfg | sed 's/^/MENU /';"
                     " ls /boot/rime/nixos | grep -i ucode | sed 's/^/BOOTFILE /';"
                     " grep -n no_update_news ~/.config/hypr/rime/appearance.lua | sed 's/^/KNOB /';"
                     " echo HYPR-LAST=$(cat ~/.local/share/hyprland/lastVersion 2>/dev/null || echo none);"
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
    microcode_check(out)
    if not re.search(r"^KNOB \d+:\s*no_update_news = true", out, re.M):
        raise Timeout("[nixos] ~/.config/hypr/rime/appearance.lua has no ecosystem.no_update_news (firstrun's refresh)")
    return "last-user=alex" in out


def graphical_login(a, vm, prefilled):
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
    rc, out = record(vm, "nixos-update-news",
                     "sleep 5; echo HYPR-LAST=$(cat ~/.local/share/hyprland/lastVersion);"
                     " export XDG_RUNTIME_DIR=/run/user/1000 HYPRLAND_INSTANCE_SIGNATURE=$(basename $(ls -d /run/user/1000/hypr/*/));"
                     " hyprctl version | head -n 1; for k in ecosystem:no_update_news ecosystem.no_update_news; do"
                     " echo \"getoption $k: $(hyprctl getoption $k | tr '\\n' ' ')\"; done;"
                     " echo UPDATE-SCREEN=$(ps -u alex -o args= | grep -c '[h]yprland-update-screen');"
                     # ps reads one instant (in the Nix check's VM the window segfaults before ps looks): the journal
                     # keeps any launch (its coredump, or the process's own lines), so assert on it too
                     " echo JOURNAL-LINES=$(journalctl -b --no-pager -o cat 2>/dev/null | wc -l);"
                     " echo UPDATE-JOURNAL=$(journalctl -b --no-pager -o cat 2>/dev/null | grep -c hyprland-update);"
                     " echo UPDATE-USER-JOURNAL=$(journalctl --user -b --no-pager -o cat 2>/dev/null | grep -c hyprland-update);"
                     " grep -i -E 'versionkeeper|update|lastVersion' /run/user/1000/hypr/*/hyprland.log | head -n 20 | sed 's/^/HYPRLOG /';"
                     # the order on this boot: when the user manager and firstrun finished vs when Hyprland started
                     " systemctl --user show rime-shell-firstrun -p ActiveEnterTimestampMonotonic -p ExecMainExitTimestampMonotonic;"
                     " for p in $(ps -u alex -o pid=,comm= | awk '$2 ~ /Hyprland/{print $1}'); do"
                     " echo HYPR-START-US=$(( $(cut -d' ' -f22 /proc/$p/stat) * 1000000 / $(getconf CLK_TCK) )); done;"
                     " loginctl list-sessions --no-legend", timeout=120, ok=False)
    # The first NixOS graphical login is the one after the version moved (0.56 on bootc): VersionKeeper wrote the
    # running version, and with the knob it started no hyprland-update-screen.
    if not re.search(r"^HYPR-LAST=0\.57", out, re.M):
        raise Timeout(f"[nixos] Hyprland's lastVersion did not move from {HYPR_LAST}: VersionKeeper never took its path")
    if not re.search(r"getoption \S+: (int|bool): (1|true) set: true", out):
        raise Timeout("[nixos] ecosystem no_update_news is not set in the running Hyprland")
    if not re.search(r"^UPDATE-SCREEN=0$", out, re.M):
        raise Timeout("[nixos] hyprland-update-screen is running")
    lines = re.search(r"^JOURNAL-LINES=(\d+)$", out, re.M)
    if not lines or int(lines.group(1)) < 100:
        raise Timeout("[nixos] the boot's journal is not readable here: the update-window check would prove nothing")
    if not re.search(r"^UPDATE-JOURNAL=0$", out, re.M) or not re.search(r"^UPDATE-USER-JOURNAL=0$", out, re.M):
        raise Timeout("[nixos] the journal names hyprland-update: the update window was launched")
    fr = re.search(r"^ExecMainExitTimestampMonotonic=(\d+)$", out, re.M)
    hs = [int(x) for x in re.findall(r"^HYPR-START-US=(\d+)$", out, re.M)]
    if fr and hs:
        say(f"[nixos] order: rime-shell-firstrun exited at {int(fr.group(1)) / 1e6:.2f} s, Hyprland started at"
            f" {min(hs) / 1e6:.2f} s ({'after' if min(hs) > int(fr.group(1)) else 'BEFORE'} firstrun)")
    time.sleep(10)
    vm.screendump(os.path.join(a.logs, f"{a.kind}-nixos-desktop.png"))
    say("[nixos] graphical login: Hyprland runs for alex in a greetd wayland session, no update window; screenshotted")


def update_window_negative_control(a, vm):
    """The update-window checks above must be able to fail on THIS guest: with the table cut out of the user's copy
    (firstrun masked so nothing restores it) and lastVersion set back, a new greeter login must show
    hyprland-update-screen (screenshot, ps or the journal). The guest is thrown away afterwards."""
    rc, out = record(vm, "nixos-negative-control-prep",
                     "systemctl --user mask rime-shell-firstrun;"
                     " sed -i '/ecosystem = {/,/},/d' ~/.config/hypr/rime/appearance.lua;"
                     " echo KNOB-LEFT=$(grep -c no_update_news ~/.config/hypr/rime/appearance.lua);"
                     " printf 0.56.2 > ~/.local/share/hyprland/lastVersion;"
                     " for s in $(loginctl list-sessions --no-legend | awk '{print $1}'); do"
                     " [ \"$(loginctl show-session $s -p Service --value)\" = greetd ] &&"
                     " [ \"$(loginctl show-session $s -p Name --value)\" = alex ] && loginctl terminate-session $s"
                     " && echo TERMINATED=$s; done", timeout=120, ok=False)
    if not re.search(r"^KNOB-LEFT=0$", out, re.M) or "TERMINATED=" not in out:
        raise Timeout("[nixos] negative control: could not cut the knob or end the graphical session")
    time.sleep(20)
    vm.screendump(os.path.join(a.logs, f"{a.kind}-nixos-negative-greeter.png"))
    vm.type_keys(PASS)
    vm.key("ret")
    time.sleep(45)
    vm.screendump(os.path.join(a.logs, f"{a.kind}-nixos-negative-desktop.png"))
    rc, out = record(vm, "nixos-negative-control",
                     "export XDG_RUNTIME_DIR=/run/user/1000 HYPRLAND_INSTANCE_SIGNATURE=$(basename $(ls -td /run/user/1000/hypr/*/ | head -n 1));"
                     " echo HYPR-LAST=$(cat ~/.local/share/hyprland/lastVersion);"
                     " echo \"getoption: $(hyprctl getoption ecosystem:no_update_news | tr '\\n' ' ')\";"
                     " echo UPDATE-SCREEN=$(ps -u alex -o args= | grep -c '[h]yprland-update-screen');"
                     " echo UPDATE-JOURNAL=$(journalctl -b --no-pager -o cat 2>/dev/null | grep -c hyprland-update);"
                     " journalctl -b --no-pager -o cat 2>/dev/null | grep hyprland-update | head -n 5 | sed 's/^/JLINE /'",
                     timeout=120, ok=False)
    shown = (re.search(r"^UPDATE-SCREEN=[1-9]", out, re.M) or re.search(r"^UPDATE-JOURNAL=[1-9]", out, re.M))
    if not re.search(r"getoption: (int|bool): (0|false)", out) or not shown:
        raise Timeout("[nixos] negative control: with the knob cut no update window was seen: the checks have no teeth")
    say("[nixos] negative control: knob cut + lastVersion 0.56.2 -> hyprland-update-screen launched (screenshotted)")


def microcode_check(out):
    """A bridged machine boots only its own CPU vendor's early microcode: carry wrote [boot] microcode from the CPU's
    vendor_id, and rime-boot's menu lists only that vendor's cpio before the generation's initrd."""
    m = re.search(r"^vendor_id\s*:\s*(\S+)", out, re.M)
    want = {"GenuineIntel": "intel", "AuthenticAMD": "amd"}.get(m.group(1) if m else "")
    if not want:
        raise Timeout(f"[nixos] the lab CPU's vendor is not Intel's or AMD's: {m.group(0) if m else 'none'}")
    other = {"intel": "amd", "amd": "intel"}[want]
    if not re.search(rf'^microcode = "{want}"$', out, re.M):
        raise Timeout(f"[nixos] host.toml has no [boot] microcode = \"{want}\"")
    menu = " ".join(re.findall(r"^MENU (.*)$", out, re.M))
    files = re.findall(r"^BOOTFILE (.*)$", out, re.M)
    if f"{want}-ucode" not in menu or f"{other}-ucode" in menu:
        raise Timeout(f"[nixos] the menu's initrd lines are not {want}-only: {menu[:400]}")
    if any(f"{other}-ucode" in f for f in files):
        raise Timeout(f"[nixos] {other} microcode is on /boot: {files}")
    say(f"[nixos] microcode: host.toml {want}, menu and /boot/rime/nixos carry {want} only")


FINISH = "rime migrate finish"
FIN_STATE = "/var/lib/rime/nixos-bridge"
DIVERGE = f"/var/home/{USER}/lab-diverge"
# The old side's names in the top level (rime-migrate-finish.py OLD_TOP, TRASH) and the kept ones (KEEP_TOP).
OLD_NAMES = ("@pre-nixos", "ostree", ".rime-finish-old")
KEEP_NAMES = ("@nixos", "@nix", "@home", "boot")


def finish_seed(vm):
    """Phase bootc, before start (phase finish asked for): what finish must KEEP (the rime-secureboot-key block and a
    Windows-like entry, appended to custom.cfg outside every Rime block) and what it must FREE once home diverges
    (1 GiB of random files in home: start's @home shares their blocks, NixOS rewrites them, the old copies are then
    the old side's alone)."""
    record(vm, "finish-seed",
           "d=$(findmnt -no SOURCE /sysroot | sed 's/\\[.*//'); mkdir -p /run/labtop && mount -o subvolid=5 $d /run/labtop"
           " && cat /run/payload/finish-custom.cfg >> /run/labtop/boot/grub2/custom.cfg && sync"
           " && grep -nE '^### |menuentry' /run/labtop/boot/grub2/custom.cfg; umount /run/labtop;"
           f" install -d -o {USER} -g {USER} {DIVERGE} && for i in 1 2 3 4; do"
           f" head -c 268435456 /dev/urandom > {DIVERGE}/f$i; done; chown {USER}:{USER} {DIVERGE}/*;"
           f" (cd {DIVERGE} && sha256sum f* | tee /run/diverge-old.sums); sync; du -sh {DIVERGE}", timeout=600)


def nixos_boot(a, name, work=None):
    """Boot GRUB's default (the NixOS entry), log in on the serial console as the user, then root."""
    vm = VM(a, name, work)
    vm.unlock()
    vm.expect(r"Welcome to (NixOS|Rime OS)", 900,
              fail=r"Kernel panic|emergency mode|You are in emergency|Timed out waiting for device")
    vm.login()
    return vm


def grub_menu(vm, name):
    """The GRUB menu as this boot drew it on the serial console (between GRUB's banner and `Booting`)."""
    with vm.lock:
        t = vm.buf.decode("utf-8", "replace")
    i, j = t.find("GNU GRUB"), t.find("Booting `")
    menu = t[i:j] if i >= 0 and j > i else ""
    entries = sorted(set(m.strip() for m in re.findall(r"\*?([A-Z][^\n*]{3,90}?)\s{4,}", menu)))
    with open(os.path.join(vm.a.logs, f"{vm.a.kind}-{name}-grub-menu.txt"), "w") as f:
        f.write("\n".join(entries) + "\n\n" + re.sub(r" {3,}", "\n", menu))
    return menu


def wait_good_boot(vm, name):
    """Wait for multi-user.target (rime-migrate-good-boot runs after it), then show the count."""
    return record(vm, name,
                  "for i in $(seq 120); do systemctl is-active -q multi-user.target && break; sleep 1; done;"
                  " for i in $(seq 60); do [ \"$(systemctl show -p ActiveState --value rime-migrate-good-boot)\" != activating ]"
                  " && break; sleep 1; done; systemctl show -p Result -p ExecMainStatus rime-migrate-good-boot;"
                  " journalctl -b -u rime-migrate-good-boot --no-pager -o cat | tail -n 3;"
                  f" cat {FIN_STATE}/good-boots.json; echo; echo BOOTS=$(grep -c '\"boot_id\"' {FIN_STATE}/good-boots.json);"
                  f" echo THIS-BOOT=$(grep -c \"$(cat /proc/sys/kernel/random/boot_id)\" {FIN_STATE}/good-boots.json);"
                  " cat /proc/cmdline; for m in / /nix /var/home /boot /mnt/rime-top; do findmnt -no TARGET,SOURCE,FSTYPE,OPTIONS -M $m; done;"
                  " ls -la /mnt/rime-top; btrfs subvolume list /mnt/rime-top | head -n 20; ls -la /boot/loader/entries/;"
                  " grep -nE '^### |menuentry' /boot/grub2/custom.cfg; " + SB_STATE, timeout=240, ok=False)


def boots_seen(out, want):
    m = re.search(r"^BOOTS=(\d+)$", out, re.M)
    if not m or int(m.group(1)) != want or not re.search(r"^THIS-BOOT=1$", out, re.M):
        raise Timeout(f"good boots: want {want} with this boot counted: {m.group(0) if m else 'no count'}")


def usage(vm, name):
    rc, out = record(vm, name, "btrfs filesystem usage -b /mnt/rime-top; btrfs filesystem df /mnt/rime-top;"
                     " btrfs filesystem du -s /mnt/rime-top/* /mnt/rime-top/.rime-finish-old 2>/dev/null", timeout=1200)
    used = re.search(r"^\s*Used:\s+(\d+)$", out, re.M)
    free = re.search(r"^\s*Free \(estimated\):\s+(\d+)", out, re.M)
    return int(used.group(1)), int(free.group(1))


DONE_CHECKS = (
    # computed in the guest: the old side gone, the kept side and custom.cfg's other blocks there
    "v=ok; for n in " + " ".join(OLD_NAMES) + "; do [ -e /mnt/rime-top/$n ] && v=left:$n; done;"
    " for n in " + " ".join(KEEP_NAMES) + "; do [ -e /mnt/rime-top/$n ] || v=gone:$n; done;"
    " ls /boot/loader/entries/ | grep -q '^ostree' && v=ostree-entry; [ -e /boot/ostree ] && v=old-kernels;"
    " grep -qx '### BEGIN rime-secureboot-key ###' /boot/grub2/custom.cfg || v=sb-block;"
    " grep -q -e '--id lab-windows' /boot/grub2/custom.cfg || v=windows-entry;"
    " grep -q '^### BEGIN rime-boot ' /boot/grub2/custom.cfg || v=rime-boot-block;"
    f" grep -q '\"done\"' {FIN_STATE}/finish.json || v=not-done; echo FINISH-VERDICT=$v"
)


def home_sums(vm, name):
    rc, out = record(vm, name, f"cd ~{USER} && sha256sum -c .rime-lab/sums && (cd {DIVERGE} && sha256sum -c ../.rime-lab/diverge.sums)"
                     " && echo HOME-SUMS=ok", timeout=300, ok=False)
    if not re.search(r"^HOME-SUMS=ok$", out, re.M):
        raise Timeout(f"[{vm.name}] home checksums")


def menu_after(vm, name):
    menu = grub_menu(vm, name)
    if "Rime (generation" not in menu:
        raise Timeout(f"[{name}] the serial console shows no GRUB menu with the Rime entries")
    if "(ostree:" in menu:
        raise Timeout(f"[{name}] the GRUB menu still has an ostree entry")
    if "Enrol the Rime OS Secure Boot key" not in menu or "Windows Boot Manager (lab)" not in menu:
        raise Timeout(f"[{name}] the GRUB menu lost the Secure Boot key entry or the Windows-like entry")


def phase_finish(a):
    # NixOS boot 2 (phase nixos was boot 1): home diverges; finish refuses (too few good boots); bootc verbs refused
    vm = nixos_boot(a, "fin-boot2")
    grub_menu(vm, "fin-boot2")
    record(vm, "fin-diverge", f"cd {DIVERGE} && for i in 1 2 3 4; do rm -f f$i; head -c 268435456 /dev/urandom > f$i; done;"
           " sha256sum f* > ../.rime-lab/diverge.sums; cat ../.rime-lab/diverge.sums; sync", timeout=600)
    rc, out = record(vm, "fin-not-root", f"{FINISH} --dry-run", ok=False)
    if rc != 2:
        raise Timeout(f"finish as the user: rc={rc}, want 2 (needs root)")
    vm.root()
    rc, out = wait_good_boot(vm, "fin-boot2-state")
    boots_seen(out, 2)
    rc, out = record(vm, "fin-too-few", f"{FINISH} --dry-run", timeout=300, ok=False)
    if rc != 3 or "REFUSED [too-few-good-boots]" not in out:
        raise Timeout(f"finish after 2 good boots: rc={rc}, want 3 REFUSED [too-few-good-boots]")
    rc, out = record(vm, "fin-bootc-verb", "rime migrate start --yes", ok=False)
    if rc != 2 or "rime migrate start" not in out:
        raise Timeout(f"`rime migrate start` on NixOS: rc={rc}, want 2 refused by name")
    poweroff(vm)

    # NixOS boot 3: --dry-run (and the default) passes every precondition and writes nothing
    vm = nixos_boot(a, "fin-boot3")
    grub_menu(vm, "fin-boot3")
    vm.root()
    rc, out = wait_good_boot(vm, "fin-boot3-state")
    boots_seen(out, 3)
    snap = (f"(ls -la --time-style=full-iso {FIN_STATE}; btrfs subvolume list /mnt/rime-top; ls -la --time-style=full-iso"
            " /mnt/rime-top /boot/loader/entries/ /boot/ostree; sha256sum /boot/grub2/custom.cfg /boot/grub2/grub.cfg"
            " /boot/grub2/rime-nixos.cfg) 2>&1 | sha256sum")
    rc, out = record(vm, "fin-dry-run", f"a=$({snap}); {FINISH} --dry-run; r1=$?; {FINISH}; r2=$?; b=$({snap});"
                     f" [ -e {FIN_STATE}/finish.json ] && echo FINISH-JSON=present; echo DRY-RC=$r1/$r2;"
                     " [ \"$a\" = \"$b\" ] && echo DRY-WRITES=none", timeout=1200, ok=False)
    if not re.search(r"^DRY-RC=0/0$", out, re.M) or not re.search(r"^DRY-WRITES=none$", out, re.M) \
            or "FINISH-JSON=present" in out:
        raise Timeout("finish --dry-run after 3 good boots: want rc 0 (with and without the flag) and nothing written")
    for want in ("ostree", "@pre-nixos", "stops working"):
        if want not in out:
            raise Timeout(f"finish --dry-run does not name {want!r}")
    poweroff(vm)

    if a.finish_interrupt:
        finish_interrupt(a)

    # NixOS boot 4: the real finish
    vm = nixos_boot(a, "fin-boot4")
    vm.root()
    rc, out = wait_good_boot(vm, "fin-boot4-state")
    u0, f0 = usage(vm, "fin-usage-before")
    record(vm, "fin-dry-run-2", f"{FINISH} --dry-run", timeout=1200)
    rc, out = record(vm, "fin-yes", f"time {FINISH} --yes", timeout=3600, ok=False)
    if rc != 0 or "done: the old system is gone" not in out:
        raise Timeout(f"finish --yes: rc={rc}")
    rc, out = record(vm, "fin-after", DONE_CHECKS + f"; cat {FIN_STATE}/finish.json; btrfs subvolume list /mnt/rime-top;"
                     " ls -la /mnt/rime-top /boot /boot/loader/entries/; cat /boot/grub2/custom.cfg", ok=False)
    if not re.search(r"^FINISH-VERDICT=ok$", out, re.M):
        raise Timeout("after finish: " + (re.findall(r"FINISH-VERDICT=\S+", out) or ["no verdict"])[-1])
    rc, out = record(vm, "fin-second-run", f"{FINISH} --yes; echo SECOND-RC=$?; {FINISH} --dry-run; echo DRY-RC=$?", ok=False)
    if not re.search(r"^SECOND-RC=3$", out, re.M) or not re.search(r"^DRY-RC=3$", out, re.M) \
            or "REFUSED [already-finished]" not in out:
        raise Timeout("a second finish was not refused already-finished")
    u1, f1 = usage(vm, "fin-usage-after")
    say(f"[fin-boot4] btrfs Used {u0 / 2**30:.2f} -> {u1 / 2**30:.2f} GiB (freed {(u0 - u1) / 2**30:.2f} GiB);"
        f" Free (estimated) {f0 / 2**30:.2f} -> {f1 / 2**30:.2f} GiB")
    with open(os.path.join(a.logs, f"{a.kind}-fin-space.txt"), "w") as f:
        f.write(f"used_before={u0}\nused_after={u1}\nfreed={u0 - u1}\nfree_est_before={f0}\nfree_est_after={f1}\n")
    if u0 - u1 < 2**30:
        raise Timeout("finish freed less than the 1 GiB of diverged home alone")
    poweroff(vm)

    # NixOS boot 5: boots, home intact, the menu as the firmware's GRUB draws it
    vm = nixos_boot(a, "fin-boot5")
    menu_after(vm, "fin-boot5")
    home_sums(vm, "fin-boot5-home")
    vm.root()
    rc, out = record(vm, "fin-boot5-state", DONE_CHECKS + "; cat /proc/cmdline; findmnt -M /var/home; " + SB_STATE, ok=False)
    if not re.search(r"^FINISH-VERDICT=ok$", out, re.M):
        raise Timeout("boot 5: " + (re.findall(r"FINISH-VERDICT=\S+", out) or ["no verdict"])[-1])
    if a.sb and "SecureBoot efivar=1" not in out:
        raise Timeout("[fin-boot5] the guest does not report Secure Boot on")
    poweroff(vm)


def finish_interrupt(a):
    """On a throwaway overlay: `finish --yes` SIGKILLed once it has deleted @pre-nixos (mid-way through the old tree),
    then the power cut (qemu quits, nothing synced). NixOS must boot, a re-run must complete, a third run refuse."""
    w = overlay(a, "finint")
    vm = nixos_boot(a, "finint1", w)
    vm.root()
    rc, out = record(vm, "finint-kill",
                     f"{FINISH} --yes > /run/fin.log 2>&1 & p=$!; for i in $(seq 12000); do"
                     f" grep -q '\"{a.finish_kill_at}\"' {FIN_STATE}/finish.json 2>/dev/null && break; sleep 0.05; done;"
                     " kill -9 $p; wait $p; echo KILL-RC=$?; cat /run/fin.log; cat " + FIN_STATE + "/finish.json; echo;"
                     " ls -la /mnt/rime-top", timeout=1800, ok=False)
    if not re.search(r"^KILL-RC=137$", out, re.M) or '"done"' in out:
        raise Timeout(f"interrupt: the finish was not killed mid-way (rc, or it finished first): see {a.kind}-finint-kill.txt")
    vm.qmp("quit")      # the power cut: no sync, no unmount
    vm.wait_off()
    vm = nixos_boot(a, "finint2", w)
    menu = grub_menu(vm, "finint2")
    if "(ostree:" in menu:
        raise Timeout("interrupt: the ostree entries outlived a finish killed after its first step")
    home_sums(vm, "finint2-home")
    vm.root()
    rc, out = record(vm, "finint-rerun", f"cat {FIN_STATE}/finish.json; echo; ls -la /mnt/rime-top; {FINISH} --yes; echo RERUN-RC=$?;"
                     f" {FINISH} --yes; echo THIRD-RC=$?; " + DONE_CHECKS, timeout=3600, ok=False)
    if not re.search(r"^RERUN-RC=0$", out, re.M) or not re.search(r"^THIRD-RC=3$", out, re.M) \
            or not re.search(r"^FINISH-VERDICT=ok$", out, re.M):
        raise Timeout("interrupt: the re-run did not complete, or a third run was not refused")
    poweroff(vm)
    vm = nixos_boot(a, "finint3", w)
    menu_after(vm, "finint3")
    home_sums(vm, "finint3-home")
    poweroff(vm)
    shutil.rmtree(w, ignore_errors=True)
    say(f"[finint] killed after {a.finish_kill_at!r} + power cut: NixOS boots, the re-run completes, a third run refuses")


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
    if a.readiness:
        # booted back into ostree after the move: the record says done, so the old side still holds every verb
        vm.sh("mkdir -p /run/payload && mount -o ro /dev/disk/by-id/virtio-rime-payload /run/payload")
        probe(vm, "done-back-on-ostree", "refused")
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
    ap.add_argument("--greeter-first", action="store_true",
                    help="the first NixOS phase logs in through the greeter before any serial login")
    ap.add_argument("--negative-control", action="store_true",
                    help="after the greeter-first checks, cut the update-window knob and log in again: the window shows")
    ap.add_argument("--readiness", action="store_true",
                    help="the disk runs the readiness image: `rime migrate` (its /usr/libexec/rime-migrate), refusals")
    ap.add_argument("--kill-probe", action="store_true",
                    help="(readiness) on an overlay: a start SIGKILLed after the masks; every verb must refuse")
    ap.add_argument("--interrupt-probe", action="store_true",
                    help="(readiness) on an overlay: a start interrupted by Ctrl-C rolls back; every verb allowed")
    ap.add_argument("--no-finish-interrupt", dest="finish_interrupt", action="store_false",
                    help="(finish) skip the overlay where finish --yes is SIGKILLed mid-way, then re-run")
    ap.add_argument("--finish-kill-at", default="@pre-nixos",
                    help="(finish) the finish.json step after which the interrupted finish is SIGKILLed")
    a = ap.parse_args()
    a.finish = "finish" in a.phases.split(",")
    a.bridge = "/usr/libexec/rime-migrate" if a.readiness else "/run/payload/rime-migrate"
    a.migrate = "rime migrate" if a.readiness else a.bridge
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
    base, seen = a.logs, set()
    for ph in a.phases.split(","):
        # a phase run again (nixos after the ostree round trip) records into PHASE-again/, so the first run's records
        # (the first NixOS login: lastVersion before and after, the screenshot) are not overwritten
        a.logs = os.path.join(base, f"{ph}-again") if ph in seen else base
        os.makedirs(a.logs, exist_ok=True)
        seen.add(ph)
        say(f"== phase {ph}" + (f" (records in {a.logs})" if a.logs != base else ""))
        {"bootc": phase_bootc, "nixos": phase_nixos, "back": phase_back, "finish": phase_finish}[ph](a)
        say(f"== phase {ph} PASS")
    say(f"BRIDGE OVMF {a.kind}: PASS")


if __name__ == "__main__":
    try:
        main()
    except Timeout as e:
        say(f"FAIL: {e}")
        sys.exit(1)
