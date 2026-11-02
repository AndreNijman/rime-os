#!/usr/bin/env python3
"""install-drive.py — install Rime OS (NixOS) from the installer ISO onto a blank disk, in OVMF with Secure Boot
ON and only Microsoft's keys enrolled (a fresh machine), and read the verdict off the guest.

The ISO boots the way installer/iso-qualify.py's `live` mode boots it (its qemu command line, USB stick, the
default menu entry): the boot menu, the kernel and "Secure boot enabled" are checked on the serial line and the
installer's first page is screenshotted. Then the lab does what an operator at the machine could do: Ctrl+Alt+F2,
log in as root on VT 2 (the live env's documented rescue login), mount the lab disk (label RIMELAB) and run its
`go` script, which runs the SAME engine the GUI runs (`rime-install --headless ANSWERS`) and reports on ttyS0.

Nothing here is a fixture on the installed disk: every byte of it was written by rime-install.
"""
import argparse
import importlib.util
import json
import os
import re
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
QUALIFY = os.environ.get("ISO_QUALIFY", os.path.join(HERE, "../../../installer/iso-qualify.py"))
spec = importlib.util.spec_from_file_location("iso_qualify", QUALIFY)
iq = importlib.util.module_from_spec(spec)
spec.loader.exec_module(iq)

# US layout: what VT 2's console keymap is in the live env.
SHIFTED = {"&": "7", "|": "backslash", ">": "dot", "_": "minus", ":": "semicolon", '"': "apostrophe"}
PLAIN = {" ": "spc", "-": "minus", "/": "slash", ".": "dot", ";": "semicolon", "=": "equal", "\n": "ret",
         ",": "comma", "'": "apostrophe"}


def type_text(qmp, text):
    for ch in text:
        if ch.isalnum():
            keys = [ch.lower()] if not ch.isupper() else ["shift", ch.lower()]
        elif ch in PLAIN:
            keys = [PLAIN[ch]]
        elif ch in SHIFTED:
            keys = ["shift", SHIFTED[ch]]
        else:
            raise ValueError("cannot type %r" % ch)
        qmp.cmd("send-key", keys=[{"type": "qcode", "data": k} for k in keys])
        time.sleep(0.06)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--work", required=True)
    ap.add_argument("--name", required=True)
    ap.add_argument("--iso", required=True)
    ap.add_argument("--disk", required=True, help="the blank target disk image (raw)")
    ap.add_argument("--labdisk", required=True, help="vfat image labelled RIMELAB holding go + answers")
    ap.add_argument("--timeout", type=int, default=3600)
    ap.add_argument("--mem", type=int, default=6144)
    ap.add_argument("--smp", type=int, default=4)
    ap.add_argument("--accel", default="kvm")
    a = ap.parse_args()
    a.work = os.path.abspath(a.work)
    os.makedirs(a.work, exist_ok=True)

    cmd = iq.qemu_cmd(a, a.name, iso=a.iso, media="usb", firmware="uefi-sb")
    cmd += ["-drive", "if=none,id=tgt,format=raw,file=%s" % a.disk,
            "-device", "virtio-blk-pci,drive=tgt,serial=rimetarget,bootindex=1",
            "-drive", "if=none,id=lab,format=raw,file=%s" % a.labdisk,
            "-device", "virtio-blk-pci,drive=lab,serial=rimelab"]
    proc, ser, qmp = iq.start(a, a.name, cmd)
    facts = {"iso": a.iso, "firmware": "uefi-sb (Microsoft keys only)", "media": "usb"}
    try:
        if not ser.wait(r"Install Rime OS", 180, proc=proc):
            return iq.report(a, a.name, "fail", reason="no boot menu on serial", **facts)
        m = ser.wait(r"Linux version \S+", 120, proc=proc)
        facts["kernel"] = m.group(0) if m else None
        if not m:
            return iq.report(a, a.name, "fail", reason="the kernel never started", **facts)
        facts["secure_boot"] = bool(ser.wait(r"Secure boot enabled", 30, proc=proc))
        if not facts["secure_boot"]:
            return iq.report(a, a.name, "fail", reason="the live kernel did not report Secure Boot on", **facts)
        png = os.path.join(a.work, a.name + "-gui.png")
        text = ""
        deadline = time.time() + 240
        while time.time() < deadline and proc.poll() is None:
            time.sleep(5)
            if qmp.screendump(png) and os.path.exists(png):
                text = iq.ocr(png) or ""
                if re.search(r"(?i)\brime\b", text):
                    break
        facts["gui_ocr"] = " ".join(text.split())[:400]
        if not re.search(r"(?i)\brime\b", text):
            return iq.report(a, a.name, "fail", reason="the installer GUI never showed a page naming Rime", **facts)
        facts["gui_screenshot"] = png

        # VT 2: root/rime is the live env's rescue login (Containerfile.installer).
        started = None
        for attempt in range(3):
            at = ser.mark()
            qmp.cmd("send-key", keys=[{"type": "qcode", "data": k} for k in ("ctrl", "alt", "f2")])
            time.sleep(4)
            type_text(qmp, "root\n")
            time.sleep(3)
            type_text(qmp, "rime\n")
            time.sleep(4)
            type_text(qmp, "mount -L RIMELAB /media && bash /media/go\n")
            started = ser.wait(r"RIME-LAB-GO: started", 60, since=at, proc=proc)
            if started:
                break
            qmp.screendump(os.path.join(a.work, "%s-vt2-attempt%d.png" % (a.name, attempt)))
        facts["go_started"] = bool(started)
        if not started:
            return iq.report(a, a.name, "fail", reason="the lab script never started on VT 2", **facts)
        m = ser.wait(r"RIME-LAB-GO: rc=(\d+)", a.timeout, proc=proc)
        if not m:
            return iq.report(a, a.name, "fail", reason="the install never finished", tail=ser.text()[-4000:], **facts)
        facts["engine_rc"] = int(m.group(1))
        out = ser.text()
        facts["install_ok"] = "RIME-INSTALL-OK" in out
        f = re.search(r"RIME-INSTALL-FAILED: (.*)", out)
        facts["install_failed"] = f.group(1).strip() if f else None
        for i in range(120):
            if proc.poll() is not None:
                break
            time.sleep(1)
        facts["powered_off"] = proc.poll() is not None
        ok = facts["engine_rc"] == 0 and facts["install_ok"]
        return iq.report(a, a.name, "pass" if ok else "fail", **facts)
    finally:
        iq.stop(proc, qmp)


if __name__ == "__main__":
    sys.exit(main())
