"""A small QMP client for dev-vm (nix/dev/dev-vm.sh).

qmp.py SOCKET kvm               prints "enabled present" (or what QEMU says instead)
qmp.py SOCKET screendump FILE   the display as PNG
qmp.py SOCKET powerdown         ACPI power button
qmp.py SOCKET quit              QEMU exits at once
qmp.py SOCKET status            the run state (running, paused, ...)
qmp.py SOCKET type TEXT         types TEXT; a newline, or the two characters \\n, is Return
"""

import json
import socket
import sys
import time

# US keyboard: character -> (qcode, shift)
KEYS = {" ": ("spc", False), "\n": ("ret", False), "\t": ("tab", False)}
for c in "abcdefghijklmnopqrstuvwxyz":
    KEYS[c] = (c, False)
    KEYS[c.upper()] = (c, True)
for c in "0123456789":
    KEYS[c] = (c, False)
for plain, shifted, code in [
    ("-", "_", "minus"), ("=", "+", "equal"), ("[", "{", "bracket_left"),
    ("]", "}", "bracket_right"), ("\\", "|", "backslash"),
    (";", ":", "semicolon"), ("'", '"', "apostrophe"),
    ("`", "~", "grave_accent"), (",", "<", "comma"), (".", ">", "dot"),
    ("/", "?", "slash"),
]:
    KEYS[plain] = (code, False)
    KEYS[shifted] = (code, True)
for c, code in zip("!@#$%^&*()", "1234567890"):
    KEYS[c] = (code, True)


class Qmp:
    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(30)
        self.sock.connect(path)
        self.buf = b""
        self.read()  # the greeting
        self.cmd("qmp_capabilities")

    def read(self):
        while b"\n" not in self.buf:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise SystemExit("qmp: QEMU closed the socket")
            self.buf += chunk
        line, self.buf = self.buf.split(b"\n", 1)
        return json.loads(line)

    def cmd(self, name, **args):
        msg = {"execute": name}
        if args:
            msg["arguments"] = args
        self.sock.sendall(json.dumps(msg).encode() + b"\n")
        while True:
            reply = self.read()
            if "event" in reply:
                continue
            if "error" in reply:
                raise SystemExit("qmp: %s: %s" % (name, reply["error"]))
            return reply.get("return")


def main(argv):
    if len(argv) < 3:
        raise SystemExit(__doc__)
    q = Qmp(argv[1])
    what = argv[2]
    if what == "kvm":
        r = q.cmd("query-kvm")
        print("%s %s" % ("enabled" if r.get("enabled") else "disabled",
                         "present" if r.get("present") else "absent"))
    elif what == "screendump":
        q.cmd("screendump", filename=argv[3], format="png")
    elif what == "powerdown":
        q.cmd("system_powerdown")
    elif what == "quit":
        try:
            q.cmd("quit")
        except SystemExit:
            pass  # QEMU may close the socket before it answers
    elif what == "status":
        print(q.cmd("query-status")["status"])
    elif what == "type":
        text = " ".join(argv[3:]).replace("\\n", "\n")
        for ch in text:
            if ch not in KEYS:
                raise SystemExit("qmp: cannot type %r" % ch)
            code, shift = KEYS[ch]
            keys = [{"type": "qcode", "data": code}]
            if shift:
                keys.insert(0, {"type": "qcode", "data": "shift"})
            q.cmd("send-key", keys=keys)
            time.sleep(0.03)
    else:
        raise SystemExit("qmp: unknown request %r\n%s" % (what, __doc__))


if __name__ == "__main__":
    main(sys.argv)
