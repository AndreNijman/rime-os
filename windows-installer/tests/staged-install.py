#!/usr/bin/env python3
"""staged-install.py SOCKET LOG -- finish a staged install inside the live session.

Talks to the root shell that `systemd.debug_shell=ttyS1` puts on the second
serial port of a VM booted by staged-boot.sh (a TEST-ONLY kernel argument the
harness adds to the staged grub.cfg; the Windows app never writes it). It
then does what a person at the live installer would, through the installer's
own code:

  1. imports rime-installer-gui and calls its read_handoff(): the check that
     decides whether the GUI skips the disk pages. It must accept the staged
     disk and name the partitions the Windows side created.
  2. writes the answers file exactly as the GUI's spawn_engine() does
     (fingerprints from the GUI's own device_fingerprint()) and runs
     `rime-install --headless`, the same engine the button runs.
  3. inspects the result: Rime's boot files on Rime's ESP and NOT on
     Windows' ESP, the "Rime OS" firmware entry pointing at Rime's ESP, the
     setup entry and staged files gone, the /boot/efi mount and the
     bootloader-update guard in the installed system, the GRUB Windows entry.

Prints PASS/FAIL lines; exits non-zero on any FAIL.
"""
import re
import socket
import sys
import time

SOCK, LOG = sys.argv[1], sys.argv[2]
RESUME = "--resume" in sys.argv
log = open(LOG, "a")
s = socket.socket(socket.AF_UNIX)
for _ in range(600):
    try:
        s.connect(SOCK)
        break
    except OSError:
        time.sleep(1)
s.settimeout(1)
buf = ""


def pump():
    global buf
    try:
        d = s.recv(65536).decode("utf-8", "replace")
    except socket.timeout:
        return False
    if d:
        buf += d
        log.write(d)
        log.flush()
    return bool(d)


def send(text):
    """Slowly: the emulated 16550 drops bytes when a long line arrives at once."""
    b = text.encode()
    for i in range(0, len(b), 32):
        s.sendall(b[i:i + 32])
        time.sleep(0.02)


def run(cmd, timeout=120):
    """Run one command, return (output, exit code). The markers are built by
    printf at run time, so the shell echoing the command line back cannot be
    mistaken for them."""
    global buf
    tag = f"M{time.time_ns()}"
    buf = ""
    # Each part on its own line: a heredoc's terminator must be alone on its line.
    send(f"printf '%s-B\\n' {tag}\n{cmd}\nprintf '%s-E %s\\n' {tag} $?\n")
    end = time.time() + timeout
    while time.time() < end:
        pump()
        b = buf.replace("\r", "")
        if f"{tag}-B\n" in b:
            body = b.split(f"{tag}-B\n", 1)[1]
            m = re.search(re.escape(tag) + r"-E (\d+)\n", body)
            if m:
                out = body[: m.start()]
                # The shell prints its prompt before each line of output it
                # echoes; drop it so callers see only the command's output.
                out = "\n".join(l[2:] if l.startswith("# ") else l for l in out.split("\n"))
                return out.lstrip("# "), int(m.group(1))
    raise TimeoutError(cmd)


fails = 0


def check(name, ok, detail=""):
    global fails
    print(("PASS  " if ok else "FAIL  ") + name + (f"\n      {detail}" if detail and not ok else ""))
    fails += 0 if ok else 1


# Wake the shell and wait for it to answer. On --resume the shell may still
# be busy with an engine started earlier: wait for it without typing ahead.
if RESUME:
    s.sendall(b"\n")  # an idle shell prints its prompt again; a busy one queues it
    end = time.time() + 3600
    while time.time() < end and "#" not in buf:
        pump()
for _ in range(300):
    s.sendall(b"\n")
    if pump() and "#" in buf:
        break
    time.sleep(1)
send("stty -echo\n")
time.sleep(1)
send("bind 'set enable-bracketed-paste off' 2>/dev/null; export TERM=dumb PS1='# '\n")
time.sleep(1)
pump()
run("true")

GUI = "/usr/bin/rime-installer-gui"
out, rc = ("", 0) if RESUME else run(f"""python3 - <<'EOF'
import importlib.util
from importlib.machinery import SourceFileLoader
spec = importlib.util.spec_from_loader("rimegui", SourceFileLoader("rimegui", "{GUI}"))
g = importlib.util.module_from_spec(spec)
spec.loader.exec_module(g)
h, err = g.read_handoff()
print("HANDOFF", h, repr(err))
if h:
    import os
    ans = {{"mode": "partition", "disk": h["disk"], "target": h["target"], "esp": h["esp"],
           "username": "alex", "password": "rime-staged-test", "hostname": "rime-staged",
           "rootfs": "btrfs", "encrypt": "no", "confirmed": "ERASE",
           "confirm_target": h["target"],
           "confirm_disk_id": g.device_fingerprint(h["disk"]),
           "confirm_target_id": g.device_fingerprint(h["target"]),
           "confirm_esp_id": g.device_fingerprint(h["esp"])}}
    fd = os.open("/run/rime-answers-staged", os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as f:
        for k, v in ans.items():
            f.write(f"{{k}}={{v}}\\n")
    print("ESP", h["esp"], "TARGET", h["target"], "DISK", h["disk"])
EOF""", timeout=120)
print(out.strip())
if RESUME:
    out, _ = run("sed -n 's/^esp=/ESP /p; s/^target=/TARGET /p; s/^disk=/DISK /p' /run/rime-answers-staged | tr '\\n' ' '")
else:
    check("the live GUI accepts the Windows hand-off (read_handoff)", "HANDOFF {" in out and "ESP /dev/" in out, out)
if fails:
    sys.exit(1)
esp = out.split("ESP ", 1)[1].split()[0]
target = out.split("TARGET ", 1)[1].split()[0]
disk = out.split("DISK ", 1)[1].split()[0]

out, rc = run("findmnt -rno SOURCE,TARGET /run/initramfs/live")
check("the live medium is Rime's own ESP", out.split()[0] == esp if out.split() else False, out)

print("installing (downloads Rime OS; this takes a while) ...")
# A person reaches the install button minutes after boot; a script reaches it
# in seconds, before NetworkManager has a route. Wait the way a person would.
out, _ = run("for i in $(seq 1 90); do ip route | grep -q '^default' && break; sleep 2; done; ip route | head -2", timeout=240)
check("the live session has a network route before the install starts", "default" in out, out)
if not RESUME:
    run("(/usr/bin/rime-install --headless /run/rime-answers-staged > /run/engine.log 2>&1; echo ENGINE-RC=$? >> /run/engine.log) &")
end = time.time() + 3600
while time.time() < end:
    time.sleep(20)
    out, _ = run("tail -n 3 /run/engine.log", timeout=60)
    if "ENGINE-RC=" in out:
        break
out, _ = run("cat /run/engine.log; echo; tail -n 60 /tmp/rime-install.log 2>/dev/null || ls /tmp /run | head", timeout=60)
log.write("\n===== ENGINE =====\n" + out)
check("the engine reported RIME-INSTALL-OK", "RIME-INSTALL-OK" in out and "ENGINE-RC=0" in out, out[-3000:])

# What landed where.
other, _ = run(f"lsblk -rnpo NAME,PARTTYPE {disk} | awk 'tolower($2)==\"c12a7328-f81f-11d2-ba4b-00a0c93ec93b\" && $1!=\"{esp}\" {{print $1}}'")
win_esp = other.strip()
check("Windows' ESP is visible again after the install", win_esp.startswith("/dev/"), other)
run("mkdir -p /mnt/w /mnt/r")
out, _ = run(f"mount -o ro {win_esp} /mnt/w && find /mnt/w -maxdepth 3 | sort; umount /mnt/w")
check("Windows' ESP holds no Rime boot files", "fedora" not in out.lower() and "rimeinst" not in out.lower(), out)
out, _ = run("find /run/initramfs/live -maxdepth 3 | sort")
check("Rime's ESP now holds Rime's bootloader", "/run/initramfs/live/EFI/fedora" in out, out)
fsck, _ = run(f"findmnt -no OPTIONS /run/initramfs/live; fsck.fat -n {esp} </dev/null 2>&1 | tail -n 4")
check("Rime's ESP is back to read-only and its FAT is clean (no dirty flag)", fsck.lstrip().startswith("ro") and "Dirty bit" not in fsck, fsck)
cl, _ = run(f"mount -o ro {target} /mnt/r && d=$(ls -d /mnt/r/ostree/deploy/*/deploy/*.0 | head -1) && cat $d/etc/systemd/system/rime-staged-cleanup.service && ls -l $d/etc/systemd/system/multi-user.target.wants/rime-staged-cleanup.service; umount /mnt/r")
check("the installed system will remove the staged installer on its first boot", "ExecStart=/usr/bin/rm -rf /boot/efi/rimeinst /boot/efi/EFI/rimeinst" in cl and "-> ../rime-staged-cleanup.service" in cl, cl)
out, _ = run("efibootmgr -v")
log.write("\n===== EFIBOOTMGR =====\n" + out)
espuuid, _ = run(f"lsblk -dno PARTUUID {esp}")
rime_lines = [l for l in out.splitlines() if "Rime OS" in l and "Setup" not in l]
check("a 'Rime OS' firmware entry exists", bool(rime_lines), out)
check("it starts from Rime's ESP, not Windows'", any(espuuid.strip() in l.lower() for l in rime_lines), out)
check("the one-shot 'Rime OS Setup' entry was removed", "Rime OS Setup" not in out, out)
out, _ = run(f"mount {target} /mnt/r && d=$(ls -d /mnt/r/ostree/deploy/*/deploy/*.0 | head -1) && cat $d/etc/fstab; echo ---; cat $d/etc/systemd/system/bootloader-update.service.d/50-rime-own-esp.conf; echo ---; cat /mnt/r/boot/grub2/custom.cfg; umount /mnt/r")
log.write("\n===== INSTALLED CONFIG =====\n" + out)
check("the installed system mounts Rime's ESP at /boot/efi", f"PARTUUID={espuuid.strip()} /boot/efi" in out, out)
check("bootloader updates require Rime's ESP to be mounted", "ExecCondition=/usr/bin/mountpoint -q /boot/efi" in out, out)
if "--expect-bitlocker" in sys.argv:
    # Windows uses BitLocker: a GRUB chainload would trip its recovery prompt,
    # so there must be no Windows entry, and the done text must say where
    # Windows is started from instead.
    check("GRUB does NOT offer Windows (BitLocker)", "chainloader" not in out, out)
    eng, _ = run("cat /run/engine.log")
    check("the installer tells the user to start Windows from the firmware boot menu", "Windows Boot" in eng and "BitLocker" in eng, eng[-1500:])
else:
    check("GRUB offers Windows", "chainloader /EFI/Microsoft/Boot/bootmgfw.efi" in out, out)
if "--save-mok" in sys.argv:
    # The installed system's Secure Boot key, for a host that wants to boot
    # it with Secure Boot on (enrolling it in db stands in for MokManager).
    dest = sys.argv[sys.argv.index("--save-mok") + 1]
    out, _ = run(f"mount -o ro {target} /mnt/r && base64 -w0 $(ls -d /mnt/r/ostree/deploy/*/deploy/*.0 | head -1)/usr/share/rime-os/secureboot/rime-mok.der; echo; umount /mnt/r", timeout=60)
    import base64
    try:
        open(dest, "wb").write(base64.b64decode(out.strip().splitlines()[0]))
        print(f"saved the installed system's MOK certificate to {dest}")
    except Exception as e:
        print(f"could not save the MOK certificate: {e}")
if "--prep-firstboot" in sys.argv:
    # TEST-ONLY, like the live debug shell: let the lab reach the installed
    # system's console and a root shell on its first boot.
    out, _ = run(f"mount {target} /mnt/r && for f in /mnt/r/boot/loader/entries/*.conf; do sed -i '/^options /s/$/ console=ttyS0,115200 systemd.debug_shell=ttyS1/' \"$f\"; done; grep -h '^options' /mnt/r/boot/loader/entries/*.conf; umount /mnt/r", timeout=60)
    check("the installed system's boot entry was prepared for the lab", "debug_shell=ttyS1" in out, out)
    print(f"ESP_PARTUUID={espuuid.strip()}")
send("sync; echo o > /proc/sysrq-trigger\n")
print(f"{fails} failure(s)")
sys.exit(1 if fails else 0)
