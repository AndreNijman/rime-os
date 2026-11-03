"""checks.desktops-eval: see nix/tests/desktops.nix. argv: facts.json."""
import json
import sys

inp = json.load(open(sys.argv[1]))
variants = inp["variants"]
failures, passed = [], 0


def check(ok, what):
    global passed
    if ok:
        passed += 1
        print(f"ok: {what}")
    else:
        failures.append(what)
        print(f"FAIL: {what}")


RIME_SESSIONS = {"hyprland", "niri", "rime-labwc", "rime-safe-graphics"}
plain = variants["plain"]
check(len(variants) == 16, f"{len(variants)} release variants (4 bases x (none + {len(inp['ids'])} desktops))")
for name, v in sorted(variants.items()):
    check(f"-nixos-system-rime-{name}-" in v["toplevel"], f"{name}: evaluates to its own toplevel")
    check(len(v["desktops"]) <= 1, f"{name}: at most one other desktop ({v['desktops']})")
    check(v["desktops"] == v["wanted"], f"{name}: carries exactly the desktop its name says ({v['desktops']} == {v['wanted']})")
    check(v["ppd"] is False, f"{name}: power-profiles-daemon is off")
    check(v["system76Power"] is False, f"{name}: system76-power is off")
    for where, found in v["forbiddenPackages"].items():
        check(found == [], f"{name}: no power-profiles-daemon/system76-power package in {where} ({found})")
    check(v["appStores"] == [], f"{name}: no desktop app store is installed ({v['appStores']})")
    check(not v["setEnvHasQt6ct"], f"{name}: qt6ct is not in /etc/set-environment")
    check(not v["setEnvHasElectronHint"], f"{name}: ELECTRON_OZONE_PLATFORM_HINT is not in /etc/set-environment")
    if not v["desktops"]:
        check(not v["setEnvHasQtTheme"], f"{name}: no QT_QPA_PLATFORMTHEME in /etc/set-environment ({v['qtTheme']})")
    sessions = set(v["sessions"])
    check(RIME_SESSIONS <= sessions, f"{name}: Rime's sessions are offered ({sorted(sessions)})")
    for s in v["wantSessions"]:
        check(s in sessions, f"{name}: session {s} is offered by the greeter")
    check(v["kernel"] == plain["kernel"], f"{name}: kernel is plain's")
    check(v["initrd"] == plain["initrd"], f"{name}: initrd is plain's")
    check(v["kernelParams"] == plain["kernelParams"], f"{name}: kernel parameters are plain's")
    check(v["greetd"] and v["otherDisplayManagers"] == [], f"{name}: greetd is the only display manager")
    if v["blueman"]:
        check(v["bluemanOverride"], f"{name}: blueman's autostart is overridden (NotShowIn GNOME/KDE/COSMIC)")
    if "kde" in v["desktops"]:
        check(v["kwalletOverride"], f"{name}: kwallet-pam's autostart runs only in Plasma")
        check(v["kwalletrc"] is not None and "apiEnabled=false" in v["kwalletrc"],
              f"{name}: KWallet's Secret Service API is off ({v['kwalletrc']!r})")
    if "cosmic" in v["desktops"]:
        check(v["system76Scheduler"] is False, f"{name}: system76-scheduler is off")

print(f"desktops-eval: {passed} passed, {len(failures)} failed")
if failures:
    for f in failures:
        print(f"  {f}")
    sys.exit(1)
