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


# The key a character sits on, per keyboard layout, for the characters the lab's passphrases use: lowercase letters
# and digits (the same keys on us and gb), plus the punctuation whose key differs between them. On gb `"` is
# Shift+2 and `@` is Shift+'; on us it is the other way round.
LAYOUT_SHIFTED = {
    "us": {'"': "apostrophe", "@": "2", "#": "3"},
    "gb": {'"': "2", "@": "apostrophe", "#": "backslash"},
}


def layout_keys(layout, ch):
    if ch.isdigit() or (ch.isalpha() and ch.islower()):
        return [ch]
    if ch.isalpha():
        return ["shift", ch.lower()]
    if ch in LAYOUT_SHIFTED[layout]:
        k = LAYOUT_SHIFTED[layout][ch]
        return [k] if ch == "#" and layout == "gb" else ["shift", k]
    raise ValueError("cannot type %r on a %s keyboard" % (ch, layout))


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
    # zsh's default prompt ends in "% ".
    m = ser.wait(r"\$ |# |> |% |Type one of the keys in parentheses", 90, since=at)
    if not m:
        return "login did not give a shell"
    a.newuser_wizard = m.group(0).startswith("Type")
    if a.newuser_wizard:
        # zsh with no ~/.zshrc opens zsh-newuser-install on a first login; q leaves it without writing anything.
        at = ser.mark()
        ser.send("q")
        if not ser.wait(r"\$ |# |> |% ", 30, since=at):
            return "login did not give a shell (after leaving zsh-newuser-install)"
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
        if a.luks_passphrase:
            # The unlock prompt, typed on the guest's KEYBOARD as key positions of the --luks-layout keyboard: what
            # the console keymap makes of them is the property under test (the serial line has no keymap).
            m = ser.wait(r"Please enter passphrase", a.timeout, proc=proc)
            facts["luks_prompt"] = bool(m)
            if not m:
                return report(a, name, "fail", reason="no LUKS passphrase prompt", tail=ser.text()[-3000:], **facts)
            time.sleep(3)
            at = ser.mark()
            for ch in a.luks_passphrase:
                keys = layout_keys(a.luks_layout, ch)
                qmp.cmd("send-key", keys=[{"type": "qcode", "data": k} for k in keys], **{"hold-time": 80})
                time.sleep(0.15)
            qmp.key("ret")
            facts["luks_layout_typed"] = a.luks_layout
            m = ser.wait(r"Switching root|Failed to activate with specified passphrase|Please enter passphrase", 120,
                         since=at, proc=proc)
            facts["luks_result"] = m.group(0) if m else None
            if not m or m.group(0) != "Switching root":
                return report(a, name, "fail", reason="the passphrase did not unlock the disk (%s)" % facts["luks_result"],
                              tail=ser.text()[-3000:], **facts)
        why = login(a, ser)
        if why:
            return report(a, name, "fail", reason=why, tail=ser.text()[-3000:], **facts)
        at = ser.mark()
        greeter = ("loginctl list-sessions --no-legend | awk '$3==\"%s\"{print $1}' | "
                   "xargs -r -I{} loginctl show-session {} -p VTNr -p Class | tr '\\n' ' '; echo" % a.greeter_user)
        script = (
            # As root (the account is in wheel): the ESP and efivars are not readable by a user. Before BEGIN and
            # silenced: a fresh account's FIRST sudo prints the lecture, which landed inside the LANG section.
            "echo '" + a.password + "' | sudo -S -p '' -v >/dev/null 2>&1; "
            "printf '__RQ_%s__\\n' BEGIN; "
            # The running hostname, then the static one (/etc/hostname, from host.toml by rime-hostlayer apply).
            "printf '__RQ_%s__\\n' HOST; cat /proc/sys/kernel/hostname; "
            "printf '__RQ_%s__\\n' STATIC; cat /etc/hostname; hostnamectl --static; "
            "printf '__RQ_%s__\\n' SYSTEM; readlink /run/current-system; "
            "printf '__RQ_%s__\\n' TZ; timedatectl show -p Timezone --value; "
            "printf '__RQ_%s__\\n' LANG; localectl status | sed -n 's/.*LANG=//p'; "
            "printf '__RQ_%s__\\n' SB; sudo -n od -An -t u1 /sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c 2>&1 | awk '{print $NF}'; "
            "sudo -n bootctl status 2>&1 | grep -m1 'Secure Boot:' | sed 's/^ *//'; "
            "printf '__RQ_%s__\\n' FS; findmnt -no FSTYPE,OPTIONS / | tr ',' ' '; findmnt -no OPTIONS /nix | tr ',' ' '; findmnt -no OPTIONS /var/home | tr ',' ' '; findmnt -no FSTYPE /boot; "
            "printf '__RQ_%s__\\n' BLS; sudo -n ls /boot/loader/entries; "
            "printf '__RQ_%s__\\n' CMDLINE; cat /proc/cmdline; "
            "printf '__RQ_%s__\\n' STAGED; sudo -n test -e /boot/rimeinst && echo present || echo absent; "
            "printf '__RQ_%s__\\n' WINDOWS; sudo -n cat /boot/grub2/custom.cfg 2>/dev/null | grep -c bootmgfw.efi; "
            "printf '__RQ_%s__\\n' NVRAM; sudo -n efibootmgr -v 2>&1 | grep -E '^Boot(Current|Order)|Rime OS'; "
            # systemd-journald's own record of this boot (installer2 item 8: one restart after the pivot).
            "printf '__RQ_%s__\\n' JOURNALD; systemctl show systemd-journald.service -p NRestarts; "
            "sudo -n journalctl -b -o short-monotonic --no-pager -u systemd-journald.service 2>&1 | head -n 15; "
            "sudo -n journalctl -b -o short-monotonic --no-pager 2>&1 | grep -m3 -E 'Switching root|systemd-journald.service: (Main process exited|Scheduled restart)|Received SIGTERM'; "
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

        facts.update(hostname=section("HOST", "STATIC"), static_hostname=section("STATIC", "SYSTEM").splitlines(),
                     system=section("SYSTEM", "TZ"),
                     timezone=section("TZ", "LANG"), lang=section("LANG", "SB"),
                     secure_boot=section("SB", "FS"), mounts=section("FS", "BLS").splitlines(),
                     bls_entries=section("BLS", "CMDLINE").split(), cmdline=section("CMDLINE", "STAGED"),
                     staged=section("STAGED", "WINDOWS"), windows_entries=section("WINDOWS", "NVRAM"),
                     nvram=section("NVRAM", "JOURNALD").splitlines(), journald=section("JOURNALD", "GREETER").splitlines(), greeter=section("GREETER", "END"),
                     zsh_newuser_wizard=bool(getattr(a, "newuser_wizard", False)))
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
        if a.expect_host and facts["static_hostname"] != [a.expect_host, a.expect_host]:
            problems.append("the static hostname (/etc/hostname, hostnamectl --static) is %r, expected %r"
                            % (facts["static_hostname"], a.expect_host))
        n = facts["cmdline"].split().count("splash")
        if n > 1:
            problems.append("splash is on the kernel command line %d times" % n)
        if facts["zsh_newuser_wizard"]:
            problems.append("zsh-newuser-install ran on the account's first login (no ~/.zshrc)")
        if a.expect_no_staged and facts["staged"] != "absent":
            problems.append("the staged installer (/boot/rimeinst) is still on the ESP after the first boot: %r"
                            % facts["staged"])
        if a.expect_windows_entry and facts["windows_entries"] != "1":
            problems.append("no Windows entry in /boot/grub2/custom.cfg (%r)" % facts["windows_entries"])
        # The firmware's SecureBoot variable (1) and bootctl's reading of it, both.
        sb = [x.strip() for x in facts["secure_boot"].splitlines()]
        if a.vars_template and not (sb[:1] == ["1"] and any(x.startswith("Secure Boot: enabled") for x in sb)):
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
    ap.add_argument("--luks-passphrase", help="firstboot --nixos: type this at the LUKS prompt (on the keyboard)")
    ap.add_argument("--luks-layout", default="us", choices=sorted(LAYOUT_SHIFTED),
                    help="the keyboard the passphrase is typed on (key positions, not characters)")
    ap.add_argument("--expect-no-staged", action="store_true", help="firstboot --nixos: /boot/rimeinst is gone")
    ap.add_argument("--expect-windows-entry", action="store_true", help="firstboot --nixos: GRUB has a Windows entry")
    a = ap.parse_args()
    os.makedirs(a.work, exist_ok=True)
    a.work = os.path.abspath(a.work)
    return {"live": mode_live, "firstboot": mode_firstboot}[a.mode](a)


if __name__ == "__main__":
    sys.exit(main())
