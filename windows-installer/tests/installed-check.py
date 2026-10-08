#!/usr/bin/env python3
"""installed-check.py SOCKET LOG -- inspect a Rime installed from Windows, booted.

Runs over the root shell `systemd.debug_shell=ttyS1` puts on the installed
system's second serial port (a TEST-ONLY kernel argument the lab adds to the
installed boot entry). Checks the half of the two-ESP fix that only exists
once Rime runs:

  * Rime's own ESP is mounted at /boot/efi (the fstab line the installer
    wrote), and it is not the first ESP on the disk;
  * bootloader-update.service ran with that mount in place and succeeded;
  * `bootupctl update`, run by hand, succeeds and writes to /boot/efi;
  * the firmware's "Rime OS" entry points at that ESP.

The host then checks Windows' ESP from outside (install-e2e).
"""
import importlib.util
import os
import sys

spec = importlib.util.spec_from_file_location("si", os.path.join(os.path.dirname(__file__), "staged-install.py"))
src = open(spec.origin).read()
# Reuse the serial plumbing only (everything up to the hand-off section).
plumbing = src[: src.index('GUI = "/usr/bin/rime-installer-gui"')]
sys.argv = [sys.argv[0], sys.argv[1], sys.argv[2], "--resume"]
ns = {"__name__": "si"}
exec(compile(plumbing.replace('if RESUME:\n    end = time.time() + 3600', 'if True:\n    end = time.time() + 1500'), spec.origin, "exec"), ns)
run, check = ns["run"], ns["check"]

out, _ = run("findmnt -no SOURCE,FSTYPE /boot/efi")
print(out.strip())
src_dev = out.split()[0] if out.split() else ""
check("/boot/efi is mounted, vfat", "vfat" in out, out)
first, _ = run("d=$(lsblk -no PKNAME " + src_dev + "); lsblk -rnpo NAME,PARTTYPE /dev/$d | awk 'tolower($2)==\"c12a7328-f81f-11d2-ba4b-00a0c93ec93b\"{print $1; exit}'")
check("the ESP at /boot/efi is Rime's, not the disk's first ESP", src_dev and first.strip() and first.strip() != src_dev, f"{src_dev} vs first {first}")
out, _ = run("systemctl show bootloader-update.service -p ActiveState -p Result -p ExecMainStatus --no-pager; journalctl -b -u bootloader-update --no-pager | tail -n 8")
print(out.strip())
check("bootloader-update.service ran and succeeded", "Result=success" in out and "ExecMainStatus=0" in out, out)
out, rc = run("bootupctl status 2>&1; bootupctl update 2>&1; echo UPDATE-RC=$?", timeout=300)
print(out.strip())
check("bootupctl update, run by hand, succeeds", "UPDATE-RC=0" in out, out)
out, _ = run("systemctl show rime-staged-cleanup.service -p Result -p ExecMainStatus --no-pager; ls /boot/efi /boot/efi/EFI")
print(out.strip())
check("the first boot removed the staged installer from Rime's ESP", "Result=success" in out and "rimeinst" not in out, out)
out, _ = run("efibootmgr -v")
uuid, _ = run(f"lsblk -dno PARTUUID {src_dev}")
check("the 'Rime OS' firmware entry points at Rime's ESP", any("Rime OS" in l and uuid.strip() in l.lower() for l in out.splitlines()), out)
out, _ = run("cat /etc/os-release | head -3; uname -r")
print(out.strip())
check("this is Rime OS", "Rime" in out, out)
ns["send"]("sync; echo o > /proc/sysrq-trigger\n")
fails = ns["fails"]
print(f"{fails} failure(s)")
sys.exit(1 if fails else 0)
