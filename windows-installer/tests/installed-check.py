#!/usr/bin/env python3
"""installed-check.py SOCKET LOG -- inspect a Rime installed from Windows, booted.

Runs over the root shell `systemd.debug_shell=ttyS1` puts on the installed
system's second serial port (a TEST-ONLY kernel argument the lab adds to the
installed boot entry). Checks the half of the two-ESP fix that only exists
once Rime runs:

  * Rime's own ESP is mounted at /boot/efi (the fstab line the installer
    wrote), and it is not the first ESP on the disk;
  * bootloader-update.service ran with that mount in place and succeeded
    (read only once it has finished: queued, it already says Result=success);
  * bootc-generic-growpart.service is masked and did not run;
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
# Wait for the unit to be FINISHED before reading its result. The debug shell
# answers early in the boot, and a oneshot that has not started yet (or is
# still queued) already reads Result=success: the v3.2.1 lab read that before
# bootloader-update had run. So: no job queued for it, not activating, and a
# real start timestamp from this boot.
out, _ = run(
    "for i in $(seq 1 150); do "
    "j=$(systemctl list-jobs --no-legend 2>/dev/null | grep -c 'bootloader-update.service'); "
    "a=$(systemctl show -p ActiveState --value bootloader-update.service); "
    "if [ \"$j\" = 0 ] && [ \"$a\" != activating ] && [ \"$a\" != deactivating ]; then break; fi; sleep 2; done; "
    "echo WAITED=$((i*2))s; "
    "systemctl show bootloader-update.service -p ActiveState -p Result -p ExecMainStatus -p ExecMainStartTimestampMonotonic --no-pager; "
    "journalctl -b -u bootloader-update --no-pager | tail -n 8", timeout=360)
print(out.strip())
ran = any(l.startswith("ExecMainStartTimestampMonotonic=") and l.split("=", 1)[1].strip() not in ("", "0") for l in out.splitlines())
check("bootloader-update.service finished, ran this boot and succeeded",
      ran and "Result=success" in out and "ExecMainStatus=0" in out and "ActiveState=activating" not in out, out)
# A partition-mode install keeps its root the size of its partition: the
# installer masks bootc-generic-growpart.service in /etc (rime-install,
# keep_root_partition_size_in). The host compares the GPT itself.
out, _ = run("systemctl show bootc-generic-growpart.service -p LoadState -p UnitFileState -p ExecMainStartTimestampMonotonic --no-pager; "
             "readlink /etc/systemd/system/bootc-generic-growpart.service; "
             "journalctl -b -u bootc-generic-growpart --no-pager | tail -n 4")
print(out.strip())
check("bootc-generic-growpart.service is masked and did not run",
      "LoadState=masked" in out and "/dev/null" in out and "ExecMainStartTimestampMonotonic=0" in out, out)
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
