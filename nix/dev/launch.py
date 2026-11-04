"""Run the VM's run script under a reaper (nix/dev/dev-vm.sh starts it with setsid -f).

The rime-nix container's PID 1 is `sleep infinity`, which never reaps: an orphaned QEMU or virtiofsd would stay a
zombie until the container restarts (and a zombie's /proc entry looks like a live VM). This process makes itself
the child subreaper of everything the run script starts, waits for all of it, then exits; only this one small
process is left to PID 1.
"""

import ctypes
import os
import sys

PR_SET_CHILD_SUBREAPER = 36


def main(argv):
    if len(argv) < 2:
        raise SystemExit("usage: launch.py PROGRAM [ARGS...]")
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0:
        raise SystemExit("launch.py: prctl(PR_SET_CHILD_SUBREAPER): errno %d" % ctypes.get_errno())
    pid = os.fork()
    if pid == 0:
        os.execvp(argv[1], argv[1:])
    status = 0
    while True:
        try:
            child, st = os.wait()
        except ChildProcessError:
            break
        if child == pid:
            status = os.waitstatus_to_exitcode(st)
    sys.exit(status if status >= 0 else 128 - status)


if __name__ == "__main__":
    main(sys.argv)
