#!/usr/bin/env python3
"""rime-boot-good: rime-boot-good.service's program. Runs `rime-boot mark-good` (this start went well: hide the boot
menu at the next one) and gives it the one thing the boot hook program must not be able to do: mount(2).

`rime-boot` is the boot.loader.external hook. It runs no program and cannot (no process module, no ctypes:
nix/tests/boot-loader-scan.py and the package's install check refuse them). mark-good still has to write the flag
on an ESP that is mounted nowhere (a machine moved from bootc: its ESP has no mount point), so this program, which
the activation path never runs, mounts it with mount(2) through ctypes. The unit has PrivateMounts=yes: the mount is
never seen outside it, and goes with it.

Usage: rime-boot-good --config HOOK.json
"""
import ctypes
import importlib.machinery
import importlib.util
import os
import sys

MS_NOSUID, MS_NODEV, MS_NOEXEC = 2, 4, 8


def load_hook():
    """rime-boot (beside this file in bin/) as a module; its main() only runs under __main__."""
    path = os.path.join(os.path.dirname(os.path.realpath(__file__)), "rime-boot")
    loader = importlib.machinery.SourceFileLoader("rime_boot", path)
    spec = importlib.util.spec_from_loader("rime_boot", loader)
    mod = importlib.util.module_from_spec(spec)
    loader.exec_module(mod)
    return mod


def mount_fat(dev, where):
    """Mount `dev` (vfat) read-write at `where` (nosuid, nodev, noexec, umask 0077) and return the unmount."""
    libc = ctypes.CDLL(None, use_errno=True)
    os.makedirs(where, exist_ok=True)
    if libc.mount(dev.encode(), where.encode(), b"vfat", MS_NOSUID | MS_NODEV | MS_NOEXEC, b"umask=0077") != 0:
        e = ctypes.get_errno()
        raise OSError(e, os.strerror(e), dev)
    return lambda: libc.umount2(where.encode(), 0)


def main(argv):
    hook = load_hook()
    hook.MOUNT_FAT = mount_fat
    return hook.main([*argv, "mark-good"])


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
