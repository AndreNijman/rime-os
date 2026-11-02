#!/usr/bin/env python3
# ─────────────────────────────────────────────────────────────────────────────
#  iso-qualify.py — boot a built installer ISO in qemu and read the verdict
#  off the guest itself. Run by .github/workflows/build-installer-iso.yml
#  before any ISO becomes a release; runs the same way on a developer machine
#  inside the bootlab container (--device /dev/kvm, no root needed).
#
#  TWO MODES, EACH ONE A CLAIM A RELEASE NOTE MAKES:
#
#    live        "The stick boots and the installer appears." Boots the
#                PRODUCTION ISO — the file that gets published, untouched — on
#                one firmware (uefi, uefi-sb = Secure Boot on with Microsoft's
#                keys, bios) from one medium (usb = the ISO as a disk, which is
#                what a dd'd or Rufus'd stick is; cdrom). The boot menu is read
#                off the serial line (GRUB draws it there), the default entry
#                is left to boot on its own, and the installer's first page is
#                screenshotted until OCR reads "Rime" on it. A kernel that boots
#                to a text console, a GUI that never paints, or a page that
#                still says APEX all fail here. CI runs this for every build.
#
#    firstboot   "It boots, and its first update is not refused." Boots a disk
#                an install wrote, logs in on its serial getty (the installed
#                system needs console=ttyS0 on its kernel line for one to
#                exist), and reads, as the user: the booted image and digest
#                (bootc status), the release it says it is, and `rime trust
#                --gate`, which is the exact decision `rime update` would make.
#                The APEX-OS v2.x ISOs fail this step. Not in CI yet: CI has no
#                install to boot (see build-installer-iso.yml's header); run by
#                hand after an install in a VM, and by the CI install test when
#                there is one.
#
#  WHAT THIS NEVER DOES: touch the host's firmware or a real disk. Two pflash
#  files, one sparse disk image and the ISO, all under --work, read by an
#  ordinary qemu process.
# ─────────────────────────────────────────────────────────────────────────────
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

OVMF = "/usr/share/edk2/ovmf"


class Serial:
    """The guest's ttyS0 over a unix socket: logged to a file, readable, writable."""

    def __init__(self, path, log):
        self.path, self.log = path, log
        self.buf = ""
        self.lock = threading.Lock()
        self.sock = None
        self.closed = False
        threading.Thread(target=self._pump, daemon=True).start()

    def _pump(self):
        deadline = time.time() + 30
        while self.sock is None and time.time() < deadline:
            try:
                s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                s.connect(self.path)
                self.sock = s
            except OSError:
                time.sleep(0.2)
        if self.sock is None:
            self.closed = True
            return
        with open(self.log, "ab") as out:
            while True:
                try:
                    data = self.sock.recv(65536)
                except OSError:
                    data = b""
                if not data:
                    self.closed = True
                    return
                out.write(data)
                out.flush()
                with self.lock:
                    self.buf += data.decode("utf-8", "replace")

    def text(self):
        with self.lock:
            return self.buf

    def mark(self):
        return len(self.text())

    def wait(self, pattern, timeout, since=0, proc=None):
        rx = re.compile(pattern)
        deadline = time.time() + timeout
        while time.time() < deadline:
            m = rx.search(self.text(), since)
            if m:
                return m
            if proc is not None and proc.poll() is not None:
                return None
            time.sleep(0.5)
        return None

    def send(self, s):
        self.sock.sendall(s.encode())


class Qmp:
    def __init__(self, path, deadline, proc):
        while True:
            try:
                s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                s.connect(path)
                break
            except OSError:
                if proc.poll() is not None:
                    raise RuntimeError("qemu exited with %s before QMP answered" % proc.returncode)
                if time.time() > deadline:
                    raise
                time.sleep(0.2)
        self.f = s.makefile("rwb")
        self.f.readline()
        self.cmd("qmp_capabilities")

    def cmd(self, name, **args):
        msg = {"execute": name}
        if args:
            msg["arguments"] = args
        self.f.write((json.dumps(msg) + "\n").encode())
        self.f.flush()
        while True:
            line = self.f.readline()
            if not line:
                raise RuntimeError("qmp closed")
            d = json.loads(line)
            if "event" not in d:
                return d

    def key(self, qcode):
        self.cmd("send-key", keys=[{"type": "qcode", "data": qcode}], **{"hold-time": 80})
        time.sleep(0.25)

    def screendump(self, path):
        r = self.cmd("screendump", filename=path, format="png")
        return "error" not in r


def qemu_cmd(a, name, iso=None, media="cdrom", disk=None, firmware="uefi", display=True, vars_template=None):
    work = a.work
    cmd = ["qemu-system-x86_64", "-m", str(a.mem), "-smp", str(a.smp),
           "-nodefaults", "-no-user-config", "-display", "none",
           "-chardev", "socket,id=ser0,path=%s/%s.serial.sock,server=on,wait=off" % (work, name),
           "-serial", "chardev:ser0",
           "-qmp", "unix:%s/%s.qmp.sock,server=on,wait=off" % (work, name),
           "-netdev", "user,id=net0", "-device", "virtio-net-pci,netdev=net0"]
    if firmware == "bios":
        cmd += ["-machine", "q35,accel=%s" % a.accel]
    else:
        vars_src = "OVMF_VARS.secboot.fd" if firmware == "uefi-sb" else "OVMF_VARS.fd"
        myvars = os.path.join(work, "%s.vars.fd" % name)
        # vars_template: a prepared varstore (e.g. the NixOS lab's Microsoft keys + a TEST MOK), copied, never written.
        shutil.copyfile(vars_template or os.path.join(OVMF, vars_src), myvars)
        cmd += ["-machine", "q35,smm=on,accel=%s" % a.accel,
                "-global", "driver=cfi.pflash01,property=secure,value=on",
                "-drive", "if=pflash,unit=0,format=raw,readonly=on,file=%s/OVMF_CODE.secboot.fd" % OVMF,
                "-drive", "if=pflash,unit=1,format=raw,file=%s" % myvars]
    if display:
        # bochs: the GPU with a native kernel driver, as build-live-iso.sh's
        # BIOS note says. 1280x800 is the smallest size the GUI is laid out for.
        cmd += ["-device", "VGA,edid=on,xres=1280,yres=800"]
    if iso:
        if media == "usb":
            cmd += ["-device", "qemu-xhci,id=xhci",
                    "-drive", "if=none,id=stick,format=raw,readonly=on,file=%s" % iso,
                    "-device", "usb-storage,bus=xhci.0,drive=stick,removable=on,bootindex=0"]
        else:
            cmd += ["-drive", "if=none,id=cd,format=raw,readonly=on,media=cdrom,file=%s" % iso,
                    "-device", "ide-cd,drive=cd,bootindex=0"]
    if disk:
        cmd += ["-drive", "if=none,id=d0,format=raw,file=%s" % disk,
                "-device", "virtio-blk-pci,drive=d0,bootindex=1"]
    return cmd


def start(a, name, cmd):
    for suffix in (".serial.sock", ".qmp.sock", ".serial.log"):
        p = os.path.join(a.work, name + suffix)
        if os.path.exists(p):
            os.unlink(p)
    err = open(os.path.join(a.work, name + ".qemu.err"), "wb")
    proc = subprocess.Popen(cmd, stdout=err, stderr=subprocess.STDOUT)
    # Serial first: its thread connects as soon as the socket exists, and qemu
    # drops what the guest writes while no client is connected.
    ser = Serial(os.path.join(a.work, name + ".serial.sock"), os.path.join(a.work, name + ".serial.log"))
    try:
        qmp = Qmp(os.path.join(a.work, name + ".qmp.sock"), time.time() + 30, proc)
    except (OSError, RuntimeError):
        err.flush()
        sys.stderr.write(open(os.path.join(a.work, name + ".qemu.err"), errors="replace").read())
        raise
    return proc, ser, qmp


def stop(proc, qmp):
    if proc.poll() is None:
        try:
            qmp.cmd("quit")
        except Exception:
            pass
        try:
            proc.wait(15)
        except subprocess.TimeoutExpired:
            proc.kill()


def ocr(png):
    if not shutil.which("tesseract"):
        return None
    r = subprocess.run(["tesseract", png, "-", "--psm", "3"], capture_output=True, text=True)
    return r.stdout


def report(a, name, verdict, **facts):
    facts.update(name=name, verdict=verdict)
    with open(os.path.join(a.work, name + ".json"), "w") as f:
        json.dump(facts, f, indent=2)
    print(json.dumps(facts, indent=2))
    return 0 if verdict == "pass" else 1


def mode_live(a):
    name = a.name
    cmd = qemu_cmd(a, name, iso=a.iso, media=a.media, firmware=a.firmware)
    proc, ser, qmp = start(a, name, cmd)
    facts = {"firmware": a.firmware, "media": a.media}
    try:
        # GRUB draws its menu on the serial line too (the ISO's grub.cfg turns
        # the serial terminal on when a UART answers).
        m = ser.wait(r"Install Rime OS", 180, proc=proc)
        facts["menu"] = bool(m)
        if not m:
            return report(a, name, "fail", reason="no boot menu on serial", **facts)
        # The default entry, on its own: this is what a user who presses nothing gets.
        m = ser.wait(r"Linux version \S+", 120, proc=proc)
        facts["kernel"] = m.group(0) if m else None
        if not m:
            return report(a, name, "fail", reason="the kernel never started", **facts)
        if re.search(r"(?i)apex", ser.text()[:m.start()]):
            return report(a, name, "fail", reason="the boot menu says APEX", **facts)
        if a.firmware == "uefi-sb":
            facts["secure_boot"] = bool(re.search(r"Secure boot enabled", ser.text()))
            if not facts["secure_boot"]:
                return report(a, name, "fail", reason="kernel did not report Secure Boot on", **facts)
        png = os.path.join(a.work, name + ".png")
        deadline = time.time() + a.timeout
        text = ""
        while time.time() < deadline and proc.poll() is None:
            time.sleep(5)
            if not qmp.screendump(png) or not os.path.exists(png):
                continue
            text = ocr(png)
            if text is None:
                return report(a, name, "fail", reason="tesseract is not installed", **facts)
            if re.search(r"(?i)\brime\b", text):
                break
        facts["ocr"] = " ".join(text.split())[:600]
        if not re.search(r"(?i)\brime\b", text):
            return report(a, name, "fail", reason="the installer never showed a page naming Rime", **facts)
        if re.search(r"(?i)apex", text):
            return report(a, name, "fail", reason="the installer's page says APEX", **facts)
        return report(a, name, "pass", screenshot=png, **facts)
    finally:
        stop(proc, qmp)


def mode_firstboot(a):
    name = a.name
    if a.nixos:
        return mode_firstboot_nixos(a)
    cmd = qemu_cmd(a, name, disk=a.disk, firmware="uefi", display=False)
    proc, ser, qmp = start(a, name, cmd)
    facts = {}
    try:
        # "<hostname> login: " is the getty's prompt; nothing earlier in a boot
        # prints that shape. Kernel and unit messages can follow it, so it is
        # searched for, not expected at the end of the log.
        if not ser.wait(r"\S+ login: ", a.timeout, proc=proc):
            return report(a, name, "fail", reason="no login prompt on the serial console",
                          tail=ser.text()[-3000:])
        time.sleep(5)
        at = ser.mark()
        ser.send(a.user + "\n")
        if not ser.wait(r"[Pp]assword:", 60, since=at):
            return report(a, name, "fail", reason="no password prompt")
        at = ser.mark()
        ser.send(a.password + "\n")
        if not ser.wait(r"\$ |# |> ", 90, since=at):
            return report(a, name, "fail", reason="login did not give a shell", tail=ser.text()[-2000:])
        time.sleep(5)
        # Whatever the account's login shell is, the checks run in a plain bash.
        ser.send("exec bash --norc --noprofile\n")
        time.sleep(2)
        ser.send("stty -echo; export TERM=dumb PS1='$ '\n")
        time.sleep(1)
        at = ser.mark()
        # Markers are printed with printf so the echoed command never matches them.
        script = (
            "printf '__RQ_%s__\\n' BEGIN; "
            "printf '__RQ_%s__\\n' HOST; hostnamectl --static; "
            "printf '__RQ_%s__\\n' RELEASE; cat /usr/share/rime/release.json | tr -d '\\n'; echo; "
            "printf '__RQ_%s__\\n' BOOTC; echo '" + a.password + "' | sudo -S -p '' bootc status --format json 2>/dev/null | tr -d '\\n'; echo; "
            "printf '__RQ_%s__\\n' GATE; rime trust --gate 2>&1; printf '__RQ_GATE_RC=%s__\\n' $?; "
            "printf '__RQ_%s__\\n' END\n"
        )
        ser.send(script)
        if not ser.wait(r"__RQ_END__", 600, since=at):
            return report(a, name, "fail", reason="the checks never finished", tail=ser.text()[-4000:])
        out = ser.text()[at:]
        out = out[out.index("__RQ_BEGIN__"):]

        def section(tag, nxt):
            m = re.search(r"__RQ_%s__\r?\n(.*?)__RQ_%s" % (tag, nxt), out, re.S)
            return m.group(1).strip() if m else ""

        facts["hostname"] = section("HOST", "RELEASE")
        def obj(text):
            # Only the JSON object: a first sudo prints its lecture on the tty.
            return json.loads(text[text.index("{"):text.rindex("}") + 1])

        try:
            facts["release"] = obj(section("RELEASE", "BOOTC"))
        except ValueError:
            facts["release"] = None
        try:
            st = obj(section("BOOTC", "GATE"))
            booted = st["status"]["booted"]["image"]
            facts["booted_image"] = booted["image"]["image"]
            facts["booted_transport"] = booted["image"].get("transport")
            facts["booted_digest"] = booted.get("imageDigest")
        except (ValueError, KeyError, TypeError) as e:
            facts["bootc_error"] = repr(e)
        gate = section("GATE", "GATE_RC")
        facts["gate"] = gate
        m = re.search(r"__RQ_GATE_RC=(\d+)__", out)
        facts["gate_rc"] = int(m.group(1)) if m else None

        problems = []
        if facts.get("booted_image") != a.expect_image:
            problems.append("booted image is %r, expected %r" % (facts.get("booted_image"), a.expect_image))
        if a.expect_digest and facts.get("booted_digest") != a.expect_digest:
            problems.append("booted digest is %r, expected %r" % (facts.get("booted_digest"), a.expect_digest))
        if not re.search(r"Deploying this\s+yes", gate):
            problems.append("rime trust --gate did not say it would deploy")
        if facts.get("release") is None:
            problems.append("no /usr/share/rime/release.json")
        if problems:
            return report(a, name, "fail", reason="; ".join(problems), **facts)
        return report(a, name, "pass", **facts)
    finally:
        try:
            ser.send("echo '%s' | sudo -S -p '' systemctl poweroff\n" % a.password)
            proc.wait(60)
        except Exception:
            pass
        stop(proc, qmp)


def login(a, ser):
    """Log in on the serial getty and leave a plain bash with echo off. None, or the failure reason."""
    if not ser.wait(r"\S+ login: ", a.timeout):
        return "no login prompt on the serial console"
    time.sleep(5)
    at = ser.mark()
    ser.send(a.user + "\n")
    if not ser.wait(r"[Pp]assword:", 60, since=at):
        return "no password prompt"
    at = ser.mark()
    ser.send(a.password + "\n")
    if not ser.wait(r"\$ |# |> ", 90, since=at):
        return "login did not give a shell"
    time.sleep(5)
    ser.send("exec bash --norc --noprofile\n")
    time.sleep(2)
    ser.send("stty -echo; export TERM=dumb PS1='$ '\n")
    time.sleep(1)
    return None


def mode_firstboot_nixos(a):
    """First boot of a NixOS install (nix/installer/lab): Secure Boot as the varstore says, the system the ISO
    installed, what host.toml asked for, the D2 layout, and the greeter on VT 1 (screenshotted with --display)."""
    name = a.name
    fw = "uefi-sb" if a.vars_template else "uefi"
    cmd = qemu_cmd(a, name, disk=a.disk, firmware=fw, display=a.display, vars_template=a.vars_template)
    proc, ser, qmp = start(a, name, cmd)
    facts = {"firmware": fw, "varstore": a.vars_template}
    try:
        why = login(a, ser)
        if why:
            return report(a, name, "fail", reason=why, tail=ser.text()[-3000:], **facts)
        at = ser.mark()
        greeter = ("loginctl list-sessions --no-legend | awk '$3==\"%s\"{print $1}' | "
                   "xargs -r -I{} loginctl show-session {} -p VTNr -p Class | tr '\\n' ' '; echo" % a.greeter_user)
        script = (
            "printf '__RQ_%s__\\n' BEGIN; "
            "printf '__RQ_%s__\\n' HOST; hostnamectl --static; "
            "printf '__RQ_%s__\\n' SYSTEM; readlink /run/current-system; "
            "printf '__RQ_%s__\\n' TZ; timedatectl show -p Timezone --value; "
            "printf '__RQ_%s__\\n' LANG; localectl status | sed -n 's/.*LANG=//p'; "
            "printf '__RQ_%s__\\n' SB; od -An -t u1 /sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d0-aa0d-00e098032b8c | awk '{print $NF}'; "
            "printf '__RQ_%s__\\n' FS; findmnt -no FSTYPE,OPTIONS / | tr ',' ' '; findmnt -no OPTIONS /nix | tr ',' ' '; findmnt -no OPTIONS /var/home | tr ',' ' '; findmnt -no FSTYPE /boot; "
            "printf '__RQ_%s__\\n' BLS; ls /boot/loader/entries; "
            "printf '__RQ_%s__\\n' GREETER; for i in $(seq 60); do g=$(" + greeter + "); case $g in *VTNr=1*Class=greeter*) break;; esac; sleep 2; done; echo \"$g\"; systemctl is-active greetd.service; "
            "printf '__RQ_%s__\\n' END\n"
        )
        ser.send(script)
        if not ser.wait(r"__RQ_END__", 300, since=at):
            return report(a, name, "fail", reason="the checks never finished", tail=ser.text()[-4000:], **facts)
        out = ser.text()[at:]
        out = out[out.index("__RQ_BEGIN__"):]

        def section(tag, nxt):
            m = re.search(r"__RQ_%s__\r?\n(.*?)__RQ_%s" % (tag, nxt), out, re.S)
            return m.group(1).strip() if m else ""

        facts.update(hostname=section("HOST", "SYSTEM"), system=section("SYSTEM", "TZ"),
                     timezone=section("TZ", "LANG"), lang=section("LANG", "SB"),
                     secure_boot=section("SB", "FS"), mounts=section("FS", "BLS").splitlines(),
                     bls_entries=section("BLS", "GREETER").split(), greeter=section("GREETER", "END"))
        if a.display:
            time.sleep(8)  # let the greeter paint
            png = os.path.join(a.work, name + ".png")
            if qmp.screendump(png) and os.path.exists(png):
                facts["screenshot"] = png
                facts["screen_ocr"] = " ".join((ocr(png) or "").split())[:400]
        problems = []
        for key, want in (("system", a.expect_system), ("hostname", a.expect_host),
                          ("timezone", a.expect_tz), ("lang", a.expect_lang)):
            if want and facts[key] != want:
                problems.append("%s is %r, expected %r" % (key, facts[key], want))
        if a.vars_template and facts["secure_boot"] != "1":
            problems.append("Secure Boot is not on in the booted system (SecureBoot=%r)" % facts["secure_boot"])
        m = facts["mounts"]
        if len(m) < 4 or not m[0].startswith("btrfs") or "subvol=/@nixos" not in m[0] \
                or "subvol=/@nix" not in m[1] or "subvol=/@home" not in m[2] or m[3] != "vfat":
            problems.append("the D2 layout is not what is mounted: %r" % m)
        if not any("compress" in x and "zstd" in x for x in m[:1]):
            problems.append("/ is not zstd-compressed: %r" % m[:1])
        if not facts["bls_entries"]:
            problems.append("no BLS entries in /boot/loader/entries")
        if not re.search(r"VTNr=1\s+Class=greeter", facts["greeter"]) or facts["greeter"].splitlines()[-1:] != ["active"]:
            problems.append("no greeter session on VT 1: %r" % facts["greeter"])
        return report(a, name, "fail" if problems else "pass", reason="; ".join(problems) or None, **facts)
    finally:
        try:
            ser.send("echo '%s' | sudo -S -p '' systemctl poweroff\n" % a.password)
            proc.wait(60)
        except Exception:
            pass
        stop(proc, qmp)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("mode", choices=["live", "firstboot"])
    ap.add_argument("--work", required=True)
    ap.add_argument("--name", required=True)
    ap.add_argument("--iso")
    ap.add_argument("--disk")
    ap.add_argument("--firmware", choices=["uefi", "uefi-sb", "bios"], default="uefi")
    ap.add_argument("--media", choices=["usb", "cdrom"], default="usb")
    ap.add_argument("--timeout", type=int, default=240)
    ap.add_argument("--mem", type=int, default=6144)
    ap.add_argument("--smp", type=int, default=4)
    ap.add_argument("--accel", default="kvm:tcg")
    ap.add_argument("--user", default="andre")
    ap.add_argument("--password", default="testpass")
    ap.add_argument("--expect-image", default="ghcr.io/andrenijman/rime-os:rime")
    ap.add_argument("--expect-digest")
    # firstboot of a NixOS install (nix/installer/lab/run.sh): no bootc, no trust gate; see mode_firstboot_nixos.
    ap.add_argument("--nixos", action="store_true")
    ap.add_argument("--vars-template", help="firstboot --nixos: boot with Secure Boot on from this varstore")
    ap.add_argument("--display", action="store_true", help="firstboot --nixos: a display, and a screenshot")
    ap.add_argument("--greeter-user", default="greetd")
    ap.add_argument("--expect-system")
    ap.add_argument("--expect-host")
    ap.add_argument("--expect-tz")
    ap.add_argument("--expect-lang")
    a = ap.parse_args()
    os.makedirs(a.work, exist_ok=True)
    a.work = os.path.abspath(a.work)
    return {"live": mode_live, "firstboot": mode_firstboot}[a.mode](a)


if __name__ == "__main__":
    sys.exit(main())
