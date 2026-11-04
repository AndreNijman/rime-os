#!/usr/bin/env python3
# Runs inside the VM as the session user (XDG_RUNTIME_DIR, HYPRLAND_INSTANCE_SIGNATURE, WAYLAND_DISPLAY set).
#   kept.py probe SHELL_DIR OUT
import json, os, subprocess, sys, time

phase, shell_dir, out = sys.argv[1], sys.argv[2], sys.argv[3]
os.makedirs(out, exist_ok=True)

PROBE_QML = """import QtQuick
import Quickshell
import Quickshell.Wayland
ShellRoot {
    PanelWindow {
        WlrLayershell.layer: WlrLayer.Overlay
        WlrLayershell.namespace: "kept-probe"
        WlrLayershell.keyboardFocus: WlrKeyboardFocus.None
        exclusionMode: ExclusionMode.Ignore
        anchors.top: true
        implicitWidth: 80
        implicitHeight: 10
        color: "transparent"
        MouseArea { anchors.fill: parent; onClicked: console.log("PROBE-CLICK") }
    }
}
"""

SURFACES = [
    ("dashboard", ["dashboard-home", "toggle"], ["dashboard-home", "toggle"]),
    ("network", ["wifi-toggle", "toggle"], ["wifi-toggle", "toggle"]),
    ("notifications", ["notification-toggle", "toggle"], ["notification-toggle", "toggle"]),
    ("clipboard", ["clipboard-toggle", "toggle"], ["clipboard-toggle", "toggle"]),
    ("quick", ["quick-toggle", "toggle"], ["quick-toggle", "toggle"]),
    ("power", ["PowerMenu-toggle", "toggle"], ["PowerMenu-toggle", "toggle"]),
    ("wallpaper", ["wallpaper-toggle", "toggle"], ["wallpaper-toggle", "toggle"]),
    ("nexus", ["nexus", "open", "appearance"], ["nexus", "close"]),
]


def sh(cmd, check=True):
    r = subprocess.run(cmd, shell=isinstance(cmd, str), capture_output=True, text=True)
    if check and r.returncode != 0:
        raise SystemExit(f"FAILED: {cmd}\n{r.stdout}\n{r.stderr}")
    return r.stdout


def hj(what):
    return json.loads(sh(["hyprctl", "-j", what]))


def ipc(*args):
    sh(["qs", "-c", shell_dir, "ipc", "call", *args])


def qlayers():
    """Quickshell's layer surfaces: [(layer, x, y, w, h)]."""
    res = []
    for mon in hj("layers").values():
        for lvl, ls in mon["levels"].items():
            for l in ls:
                if l.get("namespace") == "quickshell":
                    res.append((int(lvl), l["x"], l["y"], l["w"], l["h"]))
    return sorted(res)


def log(*a):
    print(*a, flush=True)


if phase == "probe":
    log("monitors", json.dumps(hj("monitors"), indent=1)[:1500])
    log("layers at start", qlayers())
    for name, o, c in SURFACES:
        ipc(*o); time.sleep(1.5)
        opened = qlayers()
        ipc(*c); time.sleep(1.5)
        log(f"{name}: open {opened}\n   closed {qlayers()}")
    log("devices", sh("hyprctl devices", check=False)[:2000])
    for form in ['hl.dsp.cursor.move({ x = 640, y = 5 })', 'hl.dsp.cursor.move(640, 5)',
                 'hl.dsp.cursor.move("640 5")', 'hl.dsp.cursor.move({640, 5})']:
        r = subprocess.run(["hyprctl", "dispatch", form], capture_output=True, text=True)
        log(f"dispatch {form!r}: rc={r.returncode} out={r.stdout.strip()} err={r.stderr.strip()} "
            f"pos={sh('hyprctl cursorpos', check=False).strip()}")
    for form in ['hl.dsp.window.float({ action = "enable" })', 'hl.dsp.window.float()',
                 'hl.dsp.window.fullscreen()', 'hl.dsp.focus({ workspace = 2 })']:
        r = subprocess.run(["hyprctl", "dispatch", form], capture_output=True, text=True)
        log(f"dispatch {form!r}: rc={r.returncode} out={r.stdout.strip()} err={r.stderr.strip()}")
    logtxt = sh(["qs", "-c", shell_dir, "log"], check=False)
    lines = logtxt.splitlines()
    log("qs log lines", len(lines), "pacing lines", sum("Rime pacing" in l for l in lines))
    log("\n".join([l for l in lines if "Rime pacing" in l][:40]))
    sh(["grim", f"{out}/probe.png"], check=False)
    log("configerrors", sh("hyprctl configerrors", check=False))


# ── full: the proof run (the same on main and on the branch; parking checks only where windows park) ────────
def qs_pid():
    return sh("pgrep -u $(id -u) -f '^[^ ]*quickshell -c ' | head -n1").strip()


def hypr_pid():
    return sh("pgrep -u $(id -u) -f '(^|/)Hyprland( |$)' | head -n1", check=False).strip()


def proc_cost(pid):
    ns = cs = 0
    try:
        for t in os.listdir(f"/proc/{pid}/task"):
            ns += int(open(f"/proc/{pid}/task/{t}/schedstat").read().split()[0])
            for l in open(f"/proc/{pid}/task/{t}/status"):
                if l.startswith(("voluntary_ctxt_switches", "nonvoluntary_ctxt_switches")):
                    cs += int(l.split()[1])
    except Exception:
        pass
    return ns / 1e6, cs


def qslog():
    return sh(["qs", "-c", shell_dir, "log", "--no-color"], check=False).splitlines()


def dispatch(expr):
    r = subprocess.run(["hyprctl", "dispatch", expr], capture_output=True, text=True)
    if r.returncode != 0 or "error" in r.stdout:
        raise SystemExit(f"dispatch {expr} failed: {r.stdout} {r.stderr}")


def cursor(x, y):
    dispatch(f"hl.dsp.cursor.move({{ x = {x}, y = {y} }})")


def click(nudge=True):
    # hyprctl's cursor.move warps the cursor without a motion event; a real one makes Hyprland pick the surface
    # under it before the button goes down.
    if nudge:
        sh("ydotool mousemove -- 1 0 && ydotool mousemove -- -1 0")
        time.sleep(0.2)
    sh("ydotool click 0xC0")


def keys(text):
    sh(["ydotool", "type", "--key-delay", "30", text])


def parked():
    return [l for l in qlayers() if l[0] == 3 and l[3] == 1 and l[4] == 1]


def full_open():
    """Overlay-layer shell surfaces at a real size (a panel that is open)."""
    return [l for l in qlayers() if l[0] == 3 and l[3] > 1]


def active_class():
    try:
        return hj("activewindow").get("class", "")
    except Exception:
        return ""


def wait_for(pred, timeout=6.0, step=0.1):
    t = time.time()
    while time.time() - t < timeout:
        if pred():
            return True
        time.sleep(step)
    return pred()


results, failures = [], []


def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))
    log(("PASS  " if ok else "FAIL  ") + name + (f"  ({detail})" if detail else ""))
    if not ok:
        failures.append(name)


def png_diff(a, b, out_png):
    from PIL import Image, ImageChops
    A, B = Image.open(a).convert("RGB"), Image.open(b).convert("RGB")
    d = ImageChops.difference(A, B)
    px = [(x, y) for y in range(d.height) for x in range(d.width) if max(d.getpixel((x, y))) > 0] if d.getbbox() else []
    d.point(lambda v: 255 if v else 0).save(out_png)
    return px, d.getbbox()


if phase == "input":
    def wc(pat):
        try:
            return open("/tmp/wev.log", errors="replace").read().count(pat)
        except FileNotFoundError:
            return 0
    log("devices", sh("hyprctl devices", check=False)[:3000])
    log("env", {k: v for k, v in os.environ.items() if "YDO" in k})
    dispatch('hl.dsp.exec_cmd("stdbuf -oL -eL wev > /tmp/wev.log 2>&1")')
    log("wev active", wait_for(lambda: active_class() == "wev", 15), active_class())
    time.sleep(1)
    r = subprocess.run(["ydotool", "type", "--key-delay", "30", "qq"], capture_output=True, text=True)
    time.sleep(1)
    log("type rc", r.returncode, r.stdout, r.stderr, "q presses", wc("sym: q "))
    for nudge in (False, True):
        b0 = wc("button: 272")
        cursor(640, 400); time.sleep(0.3); click(nudge); time.sleep(0.7)
        log("click in wev nudge", nudge, "got", wc("button: 272") - b0, "cursor", sh("hyprctl cursorpos", check=False).strip())
    for y in (0, 5, 12, 20):
        for nudge in (False, True):
            cursor(640, y); time.sleep(0.3); click(nudge)
            ok = wait_for(lambda: full_open(), 3)
            log(f"notch y={y} nudge={nudge}: dashboard {ok}", sh("hyprctl cursorpos", check=False).strip())
            if ok:
                ipc("dashboard-home", "toggle"); time.sleep(2)
    log("wev tail", open("/tmp/wev.log", errors="replace").read()[-1500:])
    sys.exit(0)

SHORT = phase == "short"
if phase in ("full", "short"):
    budget = 1000.0 / hj("monitors")[0]["refreshRate"]
    hlog = sh("ls /run/user/$(id -u)/hypr/*/hyprland.log").strip()
    hlog_start = len(open(hlog, errors="replace").read().splitlines())
    pid = qs_pid()
    log(f"quickshell pid {pid}, Hyprland pid {hypr_pid()}, frame budget {budget:.2f} ms")
    cursor(640, 400)
    time.sleep(3)

    # 0. A transparent probe layer on the overlay layer, under where the kept windows will park: 80x10 at the top
    #    centre, made BEFORE any panel opens, so every kept window maps later and stacks above it. A click at the
    #    1x1 spot (640,0) then reaches the probe only if the parked windows above it take no pointer input.
    open("/tmp/probe.qml", "w").write(PROBE_QML)
    dispatch('hl.dsp.exec_cmd("qs -p /tmp/probe.qml > /tmp/probe.log 2>&1")')
    probe_up = wait_for(lambda: any(l.get("namespace") == "kept-probe"
                                    for m in hj("layers").values() for ls in m["levels"].values() for l in ls), 15)
    check("probe layer mapped under the parking spot", probe_up, sh("hyprctl layers", check=False)[-600:])
    time.sleep(1)

    def probe_clicks():
        try:
            return open("/tmp/probe.log", errors="replace").read().count("PROBE-CLICK")
        except FileNotFoundError:
            return 0

    cursor(640, 0); click(); time.sleep(0.7)
    check("before any open: a click at (640,0) reaches the probe", probe_clicks() >= 1, f"{probe_clicks()} clicks")
    cursor(640, 400); time.sleep(1)

    # 1. The picture with no panel ever opened, and the notch click with no window parked.
    sh(["grim", f"{out}/0-none.png"])
    check("before any open: no parked window", not parked(), str(qlayers()))
    cursor(640, 14); click()
    check("before any open: a click on the notch opens the Dashboard", wait_for(lambda: full_open()), str(qlayers()))
    ipc("dashboard-home", "toggle"); time.sleep(2); cursor(640, 400)

    # 2. Every surface once, then the picture with all of them closed.
    for name, o, c in SURFACES:
        ipc(*o); time.sleep(1.5); ipc(*c); time.sleep(1.5)
    time.sleep(2)
    pk = parked()
    keeps = len(pk) > 0
    log(f"parked after one open of each: {pk}")
    if keeps:
        check("kept windows: 7 windows parked (the network and notification panes share one), each 1x1",
              len(pk) == 7, str(pk))
        check("kept windows: every parked window sits at the top centre, 1x1, on the overlay layer",
              all((l[1], l[2]) == (640, 0) for l in pk), str(pk))
    check("closed: no panel at a real size", not full_open(), str(qlayers()))
    # A parked window that draws stays on screen; a bar that is still repainting (an indicator, an icon fading
    # in) does not. So a difference must survive a second picture taken a few seconds later (CI run 41788582748:
    # 2,368 px across the bar once, then clean on the rerun).
    for attempt in (1, 2, 3):
        sh(["grim", f"{out}/1-parked.png"])
        px, bbox = png_diff(f"{out}/0-none.png", f"{out}/1-parked.png", f"{out}/diff-none-vs-parked.png")
        clock = [p for p in px if p[0] >= 1150 and p[1] < 34]
        other = [p for p in px if p not in set(clock)]
        log(f"screenshot diff (picture {attempt}): {len(px)} px differ, bbox {bbox}; outside the clock: {len(other)} {other[:20]}")
        if not other:
            break
        time.sleep(3)
    check("parked: the screen matches the one with no panel ever opened (outside the clock)", not other,
          f"{len(px)} px differ, {len(clock)} in the clock")

    # 3. The parked spot takes no input: a click on it reaches the probe layer underneath, and the notch next to
    #    it still opens the Dashboard.
    c0 = probe_clicks()
    for _ in range(3):
        cursor(640, 0); click(); time.sleep(0.5)
    check("parked: 3 clicks on the parked spot (640,0) reach the probe layer underneath", probe_clicks() - c0 == 3,
          f"{probe_clicks() - c0} of 3")
    cursor(640, 14); click()
    check("parked: a click on the notch opens the Dashboard", wait_for(lambda: full_open()), str(qlayers()))
    ipc("dashboard-home", "toggle"); time.sleep(2)
    cursor(640, 400)

    # 4. Keys go to the window underneath, and focus comes back after every close.
    sh("rm -f /tmp/wev.log")
    dispatch('hl.dsp.exec_cmd("stdbuf -oL -eL wev > /tmp/wev.log 2>&1")')
    check("wev is the active window", wait_for(lambda: active_class() == "wev", 15), active_class())
    time.sleep(1)

    def wev_count(pat):
        try:
            return open("/tmp/wev.log", errors="replace").read().count(pat)
        except FileNotFoundError:
            return 0

    n0 = wev_count("sym: q ")
    keys("qq")
    time.sleep(1)
    check("parked: keys reach the focused window", wev_count("sym: q ") - n0 >= 2, f"{wev_count('sym: q ') - n0} q presses")
    b0 = wev_count("button: 272")
    cursor(640, 400); click(); time.sleep(0.5)
    check("a click in the window reaches it", wev_count("button: 272") > b0)
    # A click on the parked spot takes no focus: the probe gets it, wev keeps the keyboard.
    c0 = probe_clicks()
    cursor(640, 0); click(); time.sleep(0.5)
    n0 = wev_count("sym: x ")
    keys("x"); time.sleep(0.7)
    check("parked spot clicked: the probe got it, wev is still active and gets the keys",
          probe_clicks() > c0 and active_class() == "wev" and wev_count("sym: x ") > n0,
          f"probe +{probe_clicks() - c0}, active={active_class()!r}, x +{wev_count('sym: x ') - n0}")
    cursor(640, 400)
    for name, o, c in SURFACES:
        ipc(*o); time.sleep(1.5); ipc(*c); time.sleep(1.5)
        ac = active_class()
        n0 = wev_count("sym: z ")
        keys("z"); time.sleep(0.7)
        got = wev_count("sym: z ") - n0
        check(f"after {name} closes: wev is active again and gets the keys", ac == "wev" and got >= 1,
              f"active={ac!r}, z presses={got}")

    # 5. A workspace switch: parked windows stay parked, a panel opens and parks again on the other workspace.
    dispatch("hl.dsp.focus({ workspace = 2 })"); time.sleep(1.5)
    if keeps:
        check("workspace 2: still 7 parked windows", len(parked()) == 7, str(parked()))
    for name, o, c in SURFACES[:2] + SURFACES[4:5]:
        ipc(*o)
        opened = wait_for(lambda: full_open(), 4)
        time.sleep(1.2); ipc(*c); time.sleep(1.5)
        check(f"workspace 2: {name} opens and closes", opened and not full_open(), str(qlayers()))
    if keeps:
        check("workspace 2: 7 parked after the opens", len(parked()) == 7, str(parked()))
    dispatch("hl.dsp.focus({ workspace = 1 })"); time.sleep(1.5)
    n0 = wev_count("sym: z ")
    keys("z"); time.sleep(0.7)
    check("back on workspace 1: wev is active and gets the keys", active_class() == "wev" and wev_count("sym: z ") > n0,
          active_class())

    # 6. A fullscreen window: nothing parks over it, a panel still opens over it, parking comes back after.
    dispatch("hl.dsp.window.fullscreen()"); time.sleep(2)
    fs = hj("activewindow").get("fullscreen")
    check("wev is fullscreen", fs not in (0, False, None), str(fs))
    check("fullscreen: no parked window over it", not parked(), str(qlayers()))
    ipc("quick-toggle", "toggle")
    opened = wait_for(lambda: full_open(), 4)
    time.sleep(1.2); ipc("quick-toggle", "toggle"); time.sleep(1.5)
    check("fullscreen: the quick controls open over it and close", opened and not full_open(), str(qlayers()))
    check("fullscreen: still no parked window after a close", not parked(), str(qlayers()))
    n0 = wev_count("sym: z ")
    keys("z"); time.sleep(0.7)
    check("fullscreen: wev gets the keys after the close", active_class() == "wev" and wev_count("sym: z ") > n0)
    dispatch("hl.dsp.window.fullscreen()"); time.sleep(2)
    if keeps:
        check("after fullscreen: 7 windows parked again", wait_for(lambda: len(parked()) == 7, 5), str(parked()))
    sh("pkill -u $(id -u) -x wev", check=False)
    time.sleep(2)

    # 7. Frame timing and cost: 20 open/close cycles per surface.
    rows = {}
    for name, o, c in SURFACES:
        start = len(qslog())
        cpu = []
        for i in range(3 if SHORT else 20):
            a, _ = proc_cost(pid)
            ipc(*o); time.sleep(1.2); ipc(*c); time.sleep(1.2)
            b, _ = proc_cost(pid)
            cpu.append(b - a)
            if i == 0 and keeps:
                check(f"{name}: parked again after a close", len(parked()) == 7, str(parked()))
        lines = qslog()[start:]
        rows[name] = (lines, cpu)
    if keeps:
        check(f"after {24 if SHORT else 160} cycles: still 7 windows (no window created per open)", len(parked()) == 7, str(parked()))

    # 8. Idle: every surface closed, 60 s.
    time.sleep(3)
    s0 = len(qslog())
    idle_rounds = []
    for _ in range(1 if SHORT else 3):
        q0, h0 = proc_cost(pid), proc_cost(hypr_pid())
        time.sleep(20 if SHORT else 60)
        q1, h1 = proc_cost(pid), proc_cost(hypr_pid())
        idle_rounds.append((round(q1[0] - q0[0], 1), q1[1] - q0[1], round(h1[0] - h0[0], 1), h1[1] - h0[1]))
    idle_lines = qslog()[s0:]
    swaps = [l for l in idle_lines if "swap-closed" in l]
    idle = dict(rounds_shell_cpu_ms_ctx_hypr_cpu_ms_ctx=idle_rounds, swap_closed=len(swaps))
    log("idle 3x60 s:", idle)
    check("idle: no frame from a closed surface", not swaps, f"{len(swaps)}")

    # 9. Logs.
    import re
    bad_re = re.compile(r"^(ERR|WARN|CRIT)\b|\[(ERR|WARN|CRIT)\]")
    # The check is about kept windows: it fails on an error or warning that names a layer surface or the Shell, and
    # on any layer-rule complaint. Hyprland's other ERR/WARN lines are printed, not counted: this VM prints them on
    # every run, also with the Shell before #53 (short-base: 209 lines), and some come from the harness itself (wev's
    # fullscreen, XCursor without a cursor theme, EGL on virtio-gpu without virgl).
    layer_re = re.compile(r"layer|quickshell|kept-probe", re.I)
    def hl_bad_of(lines, scoped=True):
        return [l for l in lines if (bad_re.search(l) and (not scoped or layer_re.search(l)))
                or ("layer" in l.lower() and ("rule" in l.lower() or "invalid" in l.lower()))]
    hl_all = open(hlog, errors="replace").read().splitlines()
    hl_new = hl_all[hlog_start:]
    hl_bad = hl_bad_of(hl_new)
    hl_other = [l for l in hl_bad_of(hl_new, scoped=False) if l not in hl_bad]
    log(f"Hyprland: {len(hl_other)} other error/warning lines during the run (printed, not counted):",
        sorted({re.sub(r"[0-9a-f]{12}", "X", l)[:160] for l in hl_other}))
    log(f"Hyprland log: {len(hl_all)} lines in all, {len(hl_new)} during this run; "
        f"errors/warnings/layer-rule lines during the run: {len(hl_bad)}")
    pre = hl_bad_of(hl_all[:hlog_start])
    log(f"Hyprland log before this run (boot, login, shell start): {len(pre)} error/warning/layer-rule lines")
    for l in pre[:40]:
        log("    before:", l[:200])
    for l in hl_bad[:40]:
        log("    during:", l[:200])
    layer_lines = [l for l in hl_new if "layer" in l.lower()]
    log(f"Hyprland log lines naming a layer during the run: {len(layer_lines)}; first: {[x[:150] for x in layer_lines[:3]]}")
    check("Hyprland: its log is on and recorded this run (not an empty log)", len(hl_new) > 50, f"{len(hl_new)} lines")
    check("Hyprland: no error or warning naming a layer surface or the Shell, no layer-rule complaint, during the run", not hl_bad, f"{len(hl_bad)}")
    ce = sh("hyprctl configerrors", check=False).strip()
    check("Hyprland: no config errors", ce in ("", "no errors"), ce)
    allq = qslog()
    qbad = sorted(set(l for l in allq if any(k in l for k in ("TypeError", "ReferenceError", "is not a type",
                                                           "Binding loop", "Cannot read property", "ERROR"))))
    log("shell log errors:", len(qbad))
    for l in qbad[:30]:
        log("   ", l)
    check("shell: no QML errors except CalendarCard's known `window` ReferenceError (also on main)",
          all("CalendarCard.qml" in l for l in qbad), f"{len(qbad)}")

    # 10. The pacing table.
    import statistics
    def pct(v, p):
        if not v:
            return float("nan")
        v = sorted(v)
        return v[min(len(v) - 1, int(round(p / 100 * (len(v) - 1))))]
    table = [f"frame budget {budget:.2f} ms (Hyprland monitor refresh)",
             f"{'surface':26} {'phase':8} {'n':>3} {'p50':>6} {'p95':>6} {'p99':>6} {'late':>5} {'missed':>6} "
             f"{'motion50':>8} {'motion95':>8} {'cpu/cyc':>8}"]
    summary = {"budget": budget, "idle": idle, "surfaces": {}, "checks": results}
    for name, (lines, cpu) in rows.items():
        per = {}
        for l in lines:
            if "Rime pacing:" not in l:
                continue
            parts = l.split("Rime pacing:", 1)[1].split()
            if len(parts) < 3 or parts[1] not in ("Opening", "Closing"):
                continue
            kv = dict(p.split("=", 1) for p in parts[2:] if "=" in p)
            key = (name if parts[0] == name else f"{name}/{parts[0]}", parts[1])
            r = per.setdefault(key, {"iv": [], "motion": [], "n": 0})
            r["n"] += 1
            r["iv"] += [float(x) for x in kv.get("iv", "").split(",") if x]
            if "motion" in kv and float(kv["motion"]) >= 0:
                r["motion"].append(float(kv["motion"]))
        c = statistics.median(cpu)
        for (k, ph), r in sorted(per.items()):
            iv = r["iv"]
            late = sum(1 for x in iv if x > 1.25 * budget)
            missed = sum(max(0, round(x / budget) - 1) for x in iv)
            m50 = statistics.median(r["motion"]) if r["motion"] else float("nan")
            m95 = pct(r["motion"], 95)
            summary["surfaces"].setdefault(k, {})[ph] = dict(n=r["n"], p50=pct(iv, 50), p95=pct(iv, 95), p99=pct(iv, 99),
                                                              late=late, missed=missed, m50=m50, m95=m95, cpu=c)
            table.append(f"{k:26} {ph:8} {r['n']:>3} {pct(iv,50):>6.1f} {pct(iv,95):>6.1f} {pct(iv,99):>6.1f} {late:>5} "
                         f"{missed:>6} {(f'{m50:.0f}' if ph == 'Opening' else '-'):>8} "
                         f"{(f'{m95:.0f}' if ph == 'Opening' else '-'):>8} {c:>8.0f}")
    table.append(f"idle 3x60 s: {idle}")
    open(f"{out}/report.txt", "w").write("\n".join(table) + "\n")
    json.dump(summary, open(f"{out}/summary.json", "w"), indent=1, default=str)
    open(f"{out}/qs.log", "w").write("\n".join(allq) + "\n")
    open(f"{out}/hyprland.log", "w").write("\n".join(hl_new) + "\n")
    log("\n".join(table))
    log(f"RESULT: {len(results) - len(failures)} pass, {len(failures)} fail: {failures}")
    sys.exit(1 if failures else 0)
