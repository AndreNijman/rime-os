"""rime-term-graphics against a fake terminal.

    python3 term_graphics_test.py HELPER [--expect-fail]

Each case starts HELPER on a fresh pty, as the controlling terminal of its own session (as a shell's child is),
with stdout on a pipe (as in the greeting's $(...)). This process plays the terminal on the pty's master side:
it records every byte the helper writes and, when the helper's query arrives, answers it the way the case says
(kitty graphics reply, CSI 14 t pixel size, DA1 with or without sixel, late, or not at all).

Per case it checks the printed protocol, whether a query was sent at all, that the tty's settings are exactly
what they were before (also after Ctrl-C), that no reply is left in the tty's input queue for the shell to read,
and the time. "Fast" is <= 50 ms wall from fork to exit for a terminal that never answers (the median of 5
runs, so one scheduling hiccup on a loaded builder does not fail the build; every run is printed).

--expect-fail inverts the exit status: the check runs it against deliberately broken builds (mutants).
"""
import fcntl
import os
import pty
import select
import signal
import statistics
import struct
import sys
import tempfile
import termios
import time

HELPER = sys.argv[1]
EXPECT_FAIL = "--expect-fail" in sys.argv[2:]

KITTY_OK = b"\x1b_Gi=31;OK\x1b\\"
DA1_SIXEL = b"\x1b[?62;4;22c"
DA1_PLAIN = b"\x1b[?62;22c"
PX14 = b"\x1b[4;600;1000t"

BASE_ENV = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": "/nonexistent", "LANG": "C"}

failures = []
results = []


def run(env, answer=None, delay=0.0, pixels=(0, 0), size=(30, 120), args=(), ctty=True, sigint_after=None,
        pair=None):
    """Run the helper on a fresh pty (or `pair`, kept open). answer(query) -> reply bytes; None = silent."""
    master, slave = pair or pty.openpty()
    rows, cols = size
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, pixels[0], pixels[1]))
    before = termios.tcgetattr(slave)
    out_r, out_w = os.pipe()
    t0 = time.monotonic()
    pid = os.fork()
    if pid == 0:
        try:
            os.setsid()
            if ctty:
                fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
            os.dup2(slave, 0)
            os.dup2(out_w, 1)
            os.dup2(slave, 2)
            os.close(master)
            full = dict(BASE_ENV)
            full.update(env)
            os.execve(HELPER, [HELPER, *args], full)
        finally:
            os._exit(127)
    os.close(out_w)
    written = b""
    out = b""
    answered = False
    interrupted = False
    status = None
    while status is None:
        r, _, _ = select.select([master, out_r], [], [], 0.005)
        if master in r:
            try:
                written += os.read(master, 4096)
            except OSError:
                pass
        if out_r in r:
            out += os.read(out_r, 4096)
        if not answered and answer is not None and b"\x1b[c" in written:
            answered = True
            reply = answer(written)
            if delay:
                time.sleep(delay)
            if reply:
                os.write(master, reply)
        if sigint_after is not None and not interrupted and time.monotonic() - t0 >= sigint_after:
            interrupted = True
            os.write(master, b"\x03")  # the line discipline turns ^C into SIGINT for the foreground group
        done, st = os.waitpid(pid, os.WNOHANG)
        if done:
            status = st
    elapsed = (time.monotonic() - t0) * 1000
    while True:
        chunk = os.read(out_r, 4096)
        if not chunk:
            break
        out += chunk
    os.close(out_r)
    if delay and answered:
        time.sleep(0.01)  # let a late reply land, to see whether it is left for the shell
    after = termios.tcgetattr(slave)
    pending = struct.unpack("i", fcntl.ioctl(slave, termios.FIONREAD, b"\0\0\0\0"))[0]
    if pair is None:
        os.close(master)
        os.close(slave)
    return {
        "out": out.decode(errors="replace").strip(),
        "queried": b"\x1b[c" in written,
        "written": written,
        "termios_same": before == after,
        "pending": pending,
        "elapsed": elapsed,
        "status": status,
    }


def check(name, res, proto=None, queried=None, pending_ok=False, expect=None):
    problems = []
    first = res["out"].split(" ")[0] if res["out"] else ""
    if proto is not None and first != proto:
        problems.append(f"printed {res['out']!r}, expected {proto}")
    if queried is not None and res["queried"] != queried:
        problems.append(("sent a query" if res["queried"] else "sent no query") + f" (expected queried={queried})")
    if not res["termios_same"]:
        problems.append("tty settings differ after the run")
    if not pending_ok and res["pending"]:
        problems.append(f"{res['pending']} bytes left in the tty input queue for the shell")
    if expect is not None and res["out"] != expect:
        problems.append(f"printed {res['out']!r}, expected {expect!r}")
    line = f"{'ok  ' if not problems else 'FAIL'} {name}: {res['out']!r} ({res['elapsed']:.1f} ms)"
    print(line + ("" if not problems else " -- " + "; ".join(problems)))
    results.append(name)
    if problems:
        failures.append(name)


def answers(kitty=False, sixel=False, px=False):
    def reply(query):
        r = b""
        if kitty and b"\x1b_G" in query:
            r += KITTY_OK
        if px and b"\x1b[14t" in query:
            r += PX14
        return r + (DA1_SIXEL if sixel else DA1_PLAIN)
    return reply


REMOTE = {"SSH_CONNECTION": "192.0.2.1 50000 192.0.2.2 22", "SSH_TTY": "/dev/pts/9"}
PIX = (1200, 600)  # 120 x 30 cells -> 10 x 20 px cells


def main():
    # ── local terminals known from the environment: no query at all ──
    check("local Ghostty", run({"TERM": "xterm-ghostty", "GHOSTTY_RESOURCES_DIR": "/x"}, pixels=PIX),
          "kitty-direct", queried=False)
    check("local Ghostty, TERM only", run({"TERM": "xterm-ghostty"}), "kitty-direct", queried=False)
    check("local kitty", run({"TERM": "xterm-kitty", "KITTY_WINDOW_ID": "1"}), "kitty-direct", queried=False)
    check("local WezTerm", run({"TERM": "xterm-256color", "TERM_PROGRAM": "WezTerm"}), "iterm", queried=False)
    check("local Alacritty", run({"TERM": "alacritty"}, answer=answers(kitty=True, sixel=True)), "none", queried=False)
    check("local Alacritty by window id",
          run({"TERM": "xterm-256color", "ALACRITTY_WINDOW_ID": "7"}, answer=answers(sixel=True)), "none", queried=False)
    check("Linux console", run({"TERM": "linux"}), "none", queried=False)
    check("TERM=dumb", run({"TERM": "dumb"}), "none", queried=False)
    check("TERM unset", run({}), "none", queried=False)
    check("no controlling terminal", run({"TERM": "xterm-256color"}, answer=answers(kitty=True), ctty=False),
          "none", queried=False)

    # ── remote: one query, decided by the replies ──
    check("SSH, terminal answers kitty (cell size from the pty)",
          run({"TERM": "xterm-ghostty", **REMOTE}, answer=answers(kitty=True), pixels=PIX), "kitty", queried=True)
    check("SSH, kitty, no pixel size anywhere -> none (fastfetch would fall back to ASCII)",
          run({"TERM": "xterm-kitty", **REMOTE}, answer=answers(kitty=True)), "none", queried=True)
    r = run({"TERM": "xterm-kitty", **REMOTE}, answer=answers(kitty=True, px=True))
    check("SSH, kitty, pixel size from CSI 14 t", r, "kitty", queried=True)
    if b"\x1b[14t" not in r["written"]:
        failures.append("CSI 14 t not asked when the pty has no pixel size")
        print("FAIL CSI 14 t not asked when the pty has no pixel size")
    r = run({"TERM": "xterm-kitty", **REMOTE}, answer=answers(kitty=True), pixels=PIX)
    if b"\x1b[14t" in r["written"]:
        failures.append("CSI 14 t asked although the pty reports pixels")
        print("FAIL CSI 14 t asked although the pty reports pixels")
    check("SSH, DA1 lists sixel", run({"TERM": "xterm-256color", **REMOTE}, answer=answers(sixel=True), pixels=PIX),
          "sixel", queried=True)
    check("SSH, kitty AND sixel -> kitty first",
          run({"TERM": "foot", **REMOTE}, answer=answers(kitty=True, sixel=True), pixels=PIX), "kitty", queried=True)
    check("SSH, DA1 without sixel", run({"TERM": "xterm-256color", **REMOTE}, answer=answers(), pixels=PIX),
          "none", queried=True)
    check("SSH from iTerm2 (LC_TERMINAL)",
          run({"TERM": "xterm-256color", "LC_TERMINAL": "iTerm2", **REMOTE}, answer=answers(), pixels=PIX),
          "iterm", queried=True)
    check("SSH from Alacritty", run({"TERM": "alacritty", **REMOTE}, answer=answers(sixel=True)), "none", queried=False)
    check("SSH, terminal answers after the deadline -> none, input flushed",
          run({"TERM": "xterm-ghostty", **REMOTE}, answer=answers(kitty=True), delay=0.12, pixels=PIX),
          "none", queried=True, pending_ok=True)
    check("SSH, slow link with a raised timeout",
          run({"TERM": "xterm-ghostty", "RIME_TERM_GRAPHICS_TIMEOUT_MS": "400", **REMOTE},
              answer=answers(kitty=True), delay=0.12, pixels=PIX), "kitty", queried=True)

    # ── local terminals not known by name: same query ──
    check("local kmscon-like (DA1 without sixel)", run({"TERM": "xterm-256color"}, answer=answers()), "none", queried=True)
    check("local foot-like (DA1 with sixel)", run({"TERM": "foot"}, answer=answers(sixel=True), pixels=PIX),
          "sixel", queried=True)

    # ── a terminal that never answers: none, fast ──
    times = []
    for i in range(5):
        r = run({"TERM": "xterm-256color", **REMOTE}, answer=None)
        check(f"SSH, silent terminal (run {i + 1})", r, "none", queried=True)
        times.append(r["elapsed"])
    med = statistics.median(times)
    print(f"silent terminal: {', '.join(f'{t:.1f}' for t in times)} ms; median {med:.1f} ms (limit 50)")
    if med > 50:
        failures.append(f"silent terminal median {med:.1f} ms > 50 ms")
        print(f"FAIL silent terminal median {med:.1f} ms > 50 ms")
    times = [run({"TERM": "xterm-ghostty"})["elapsed"] for _ in range(5)]
    print(f"local Ghostty (no query): {', '.join(f'{t:.1f}' for t in times)} ms; median {statistics.median(times):.1f} ms")

    # ── Ctrl-C while waiting: tty restored, the signal still kills it ──
    r = run({"TERM": "xterm-256color", "RIME_TERM_GRAPHICS_TIMEOUT_MS": "3000", **REMOTE}, answer=None,
            sigint_after=0.15)
    ok = os.WIFSIGNALED(r["status"]) and os.WTERMSIG(r["status"]) == signal.SIGINT and r["termios_same"]
    print(f"{'ok  ' if ok else 'FAIL'} Ctrl-C during the query: "
          f"signalled={os.WIFSIGNALED(r['status'])} termios_same={r['termios_same']} ({r['elapsed']:.1f} ms)")
    if not ok:
        failures.append("Ctrl-C")

    # ── tmux and screen ──
    with tempfile.TemporaryDirectory() as d:
        for answer_text in ("on", "off"):
            os.makedirs(f"{d}/{answer_text}")
            with open(f"{d}/{answer_text}/tmux", "w") as f:
                f.write(f"#!/bin/sh\n[ \"$*\" = 'show-options -gv allow-passthrough' ] && echo {answer_text}\n")
            os.chmod(f"{d}/{answer_text}/tmux", 0o755)
        tmux = {"TMUX": "/tmp/tmux-1000/default,1,0", "TERM": "tmux-256color", "TERM_PROGRAM": "tmux"}
        on = {"PATH": f"{d}/on:" + BASE_ENV["PATH"]}
        off = {"PATH": f"{d}/off:" + BASE_ENV["PATH"]}
        check("tmux in Ghostty, passthrough on", run({**tmux, **on, "GHOSTTY_RESOURCES_DIR": "/x"}),
              "kitty-direct", queried=False)
        check("tmux in Ghostty, passthrough off", run({**tmux, **off, "GHOSTTY_RESOURCES_DIR": "/x"}), "none",
              queried=False)
        check("tmux over SSH, passthrough on", run({**tmux, **on, **REMOTE, "GHOSTTY_RESOURCES_DIR": "/x"}), "none",
              queried=False)
        check("tmux, unknown outer terminal", run({**tmux, **on}, answer=answers(kitty=True)), "none", queried=False)
        check("tmux with TERM=screen (fastfetch refuses images)",
              run({**tmux, **on, "TERM": "screen", "GHOSTTY_RESOURCES_DIR": "/x"}), "none", queried=False)
    check("GNU screen", run({"TERM": "screen.xterm-256color", "STY": "1.pts-0.host"}, answer=answers(kitty=True)),
          "none", queried=False)

    # ── cache and override ──
    r = run({"TERM": "xterm-256color", **REMOTE}, answer=answers(sixel=True), pixels=PIX, args=("--rows", "8"))
    token = r["out"].split(" ")[-1]
    check("--rows prints protocol, cols, rows, token", r, "sixel")
    if not token.startswith("sixel@"):
        failures.append(f"token {token!r}")
        print(f"FAIL token {token!r}")
    # the same terminal again: a second run on the SAME pty, with the token the first run printed
    pair = pty.openpty()
    r = run({"TERM": "xterm-256color", **REMOTE}, answer=answers(sixel=True), pixels=PIX, args=("--rows", "8"),
            pair=pair)
    token = r["out"].split(" ")[-1]
    check("cache hit: no query, the cached answer",
          run({"TERM": "xterm-256color", **REMOTE, "RIME_TERM_GRAPHICS": token.replace("sixel@", "kitty@", 1)},
              answer=answers(sixel=True), pixels=PIX, pair=pair), "kitty", queried=False)
    os.close(pair[0])
    os.close(pair[1])
    check("cache for another terminal is ignored",
          run({"TERM": "xterm-256color", **REMOTE, "RIME_TERM_GRAPHICS": "kitty@/dev/pts/999,x,y"},
              answer=answers(sixel=True), pixels=PIX), "sixel", queried=True)
    check("RIME_TERM_GRAPHICS_FORCE", run({"TERM": "linux", "RIME_TERM_GRAPHICS_FORCE": "iterm"}), "iterm",
          queried=False)
    check("RIME_TERM_GRAPHICS_FORCE with junk is ignored",
          run({"TERM": "xterm-ghostty", "RIME_TERM_GRAPHICS_FORCE": "ascii"}), "kitty-direct", queried=False)

    # ── geometry: the logo keeps its aspect ratio in cells ──
    with tempfile.TemporaryDirectory() as d:
        png = f"{d}/logo.png"
        with open(png, "wb") as f:  # IHDR of a 512 x 373 PNG is all the helper reads
            f.write(b"\x89PNG\r\n\x1a\n" + b"\0\0\0\x0dIHDR" + struct.pack(">II", 512, 373) + b"\x08\x06\0\0\0")
        r = run({"TERM": "xterm-ghostty"}, pixels=PIX, args=("--rows", "8", "--image", png))
        # 10 x 20 px cells: 8 rows * 2.0 * 512/373 = 21.96 -> 22
        check("geometry from the pty's pixel size", r, "kitty-direct")
        if r["out"].split(" ")[1:3] != ["22", "8"]:
            failures.append(f"geometry {r['out']!r}")
            print(f"FAIL geometry {r['out']!r}, expected cols 22 rows 8")
        r = run({"TERM": "xterm-ghostty"}, args=("--rows", "8", "--image", png))
        # no pixel size: the 2.1 default -> 8 * 2.1 * 512/373 = 23.06 -> 23
        if r["out"].split(" ")[1:3] != ["23", "8"]:
            failures.append(f"default geometry {r['out']!r}")
            print(f"FAIL default geometry {r['out']!r}, expected cols 23 rows 8")
        else:
            print(f"ok   default cell ratio: {r['out']!r}")
        r = run({"TERM": "xterm-ghostty"}, pixels=(600, 600), size=(30, 60), args=("--rows", "8", "--image", png))
        # 60 columns cannot hold a 22-cell logo plus the info block: shown none, cached kitty-direct
        ok = r["out"].startswith("none ") and r["out"].split(" ")[-1].startswith("kitty-direct@")
        print(f"{'ok  ' if ok else 'FAIL'} narrow window: {r['out']!r}")
        if not ok:
            failures.append("narrow window")

    print(f"{len(results)} cases, {len(failures)} failed" + (f": {failures}" if failures else ""))
    return 1 if failures else 0


if __name__ == "__main__":
    rc = main()
    if EXPECT_FAIL:
        print("mutant " + ("caught" if rc else "NOT caught"))
        sys.exit(0 if rc else 1)
    sys.exit(rc)
