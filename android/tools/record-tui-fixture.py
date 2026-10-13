#!/usr/bin/env python3
"""Record a TUI against a PTY that ANSWERS its terminal queries.

The first recording pass showed every one of these agents opens by asking the
terminal questions and waiting: `ESC[6n` (cursor position), `ESC[c` (device
attributes), `OSC 10/11` (fg/bg colour), DECRQM `ESC[?N$p`, kitty `ESC[?u`.
A terminal that answers none of them gets a UI that never paints — opencode
emitted 284 bytes of questions and then nothing at all.

So this answers them, minimally and in the shapes xterm uses. What it replies
is also the specification the Kotlin emulator is held to: the same input must
produce the same answers.
"""
import fcntl, os, pty, re, select, signal, struct, sys, termios, time

prog, out, secs, home = sys.argv[1], sys.argv[2], float(sys.argv[3]), sys.argv[4]

# The fifth argument is what makes the PAIR of recordings reproducible. Every
# `*-queries.bin` in `core/src/test/resources/tui/` was taken against a terminal
# that answered nothing, and this script could not produce one: it always
# answered, so re-recording the quiet half meant editing the script by hand and
# the committed fixtures could not be regenerated as committed.
mode = sys.argv[5] if len(sys.argv) > 5 else "answer"
if mode not in ("answer", "quiet"):
    sys.exit(f"mode is 'answer' or 'quiet', not {mode!r}")

pid, fd = pty.fork()
if pid == 0:
    env = {"HOME": home, "TERM": "xterm-256color", "PATH": os.environ["PATH"],
           "LANG": "C.UTF-8", "COLORTERM": "truecolor", "SHELL": "/bin/sh",
           "USER": os.environ.get("USER", "nobody"), "XDG_RUNTIME_DIR": home}
    os.chdir(home)
    os.execvpe(prog.split()[0], prog.split(), env)

fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))

DSR = re.compile(rb"\x1b\[6n")
DA1 = re.compile(rb"\x1b\[c")
DA2 = re.compile(rb"\x1b\[>[0-9;]*c")
XTVER = re.compile(rb"\x1b\[>0?q")
OSC_FG = re.compile(rb"\x1b\]10;\?(\x1b\\|\x07)")
OSC_BG = re.compile(rb"\x1b\]11;\?(\x1b\\|\x07)")
OSC_IDX = re.compile(rb"\x1b\]4;([0-9]+);\?(\x1b\\|\x07)")
DECRQM = re.compile(rb"\x1b\[\?([0-9]+)\$p")
KITTY_Q = re.compile(rb"\x1b\[\?u")
WINPX = re.compile(rb"\x1b\[14t")
WINCH = re.compile(rb"\x1b\[18t")
XTCAP = re.compile(rb"\x1bP\+q([0-9a-fA-F;]+)\x1b\\")

def answers(chunk):
    out = b""
    for _ in DSR.finditer(chunk):
        out += b"\x1b[1;1R"
    for _ in DA1.finditer(chunk):
        out += b"\x1b[?62;22c"
    for _ in DA2.finditer(chunk):
        out += b"\x1b[>0;10;1c"
    for _ in XTVER.finditer(chunk):
        out += b"\x1bP>|apex-remote(1)\x1b\\"
    for _ in OSC_FG.finditer(chunk):
        out += b"\x1b]10;rgb:cdcd/d6d6/f4f4\x1b\\"
    for _ in OSC_BG.finditer(chunk):
        out += b"\x1b]11;rgb:1a1a/2828/2a2a\x1b\\"
    for m in OSC_IDX.finditer(chunk):
        out += b"\x1b]4;" + m.group(1) + b";rgb:0000/0000/0000\x1b\\"
    for m in DECRQM.finditer(chunk):
        out += b"\x1b[?" + m.group(1) + b";2$y"
    for _ in KITTY_Q.finditer(chunk):
        out += b"\x1b[?0u"
    for _ in WINPX.finditer(chunk):
        out += b"\x1b[4;408;640t"
    for _ in WINCH.finditer(chunk):
        out += b"\x1b[8;24;80t"
    for m in XTCAP.finditer(chunk):
        out += b"\x1bP0+q" + m.group(1) + b"\x1b\\"
    return out

buf = bytearray()
deadline = time.time() + secs
while time.time() < deadline:
    r, _, _ = select.select([fd], [], [], 0.2)
    if fd in r:
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            break
        if not chunk:
            break
        buf += chunk
        reply = answers(chunk) if mode == "answer" else b""
        if reply:
            os.write(fd, reply)
        if len(buf) > 600000:
            break
try:
    os.kill(pid, signal.SIGKILL)
except ProcessLookupError:
    pass
os.waitpid(pid, 0)
os.close(fd)
open(out, "wb").write(bytes(buf))
print(f"{prog} ({mode}): {len(buf)} bytes -> {out}")
