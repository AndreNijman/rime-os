# The screen lock of each other desktop under rime-greet, in a VM (docs/nixos/desktops.md §12.4, §12.5).
#
# Each check enables one desktop directly in the test machine (as settings-app-gnome), logs in through rime-greet
# with the typed password, then locks and unlocks the way a person does: keys typed into the lock screen.
#
#   desktop-lock-gnome    rime-gnome-unlock owns org.gnome.DisplayManager (no GDM process), so GNOME Shell builds its
#                         lock screen: the "Screen Lock disabled" notice is not shown (its marker file
#                         ~/.local/share/gnome-shell/lock-warning-shown, created before login, is deleted by GNOME
#                         Shell only when the lock exists); Quick Settings shows the lock button (screenshot). The
#                         daemon is STOPPED before the login, so the lock exists only because GNOME Shell's one
#                         start-up question (canLock) bus-activated it. Its unit runs with the bounding set, seccomp
#                         filter and no_new_privs it declares (/proc/<pid>/status). A channel's socket is the session
#                         user's, mode 0600: `nobody` cannot connect, the user can. Locks:
#                           - Super+L, then a WRONG password (refused: still locked, PAM logged the failure, the lock
#                             screen says so), then the right one (unlocked);
#                           - `loginctl lock-session`; org.gnome.ScreenSaver.Lock (what gsd and the menus call);
#                           - the idle timeout (idle-delay 10 s);
#                           - suspend: the lock is set (SetLockedHint true, dbus-monitor) BEFORE the kernel's
#                             "PM: suspend entry" (s2idle); the VM's virtio devices then refuse to suspend (-95) and
#                             the kernel aborts the sleep, so the journal may gain exactly that abort and nothing else;
#                             the session is still locked afterwards and the right password unlocks it.
#                         Each lock: the shield is up (ScreenSaver GetActive true); each unlock: logind LockedHint no,
#                         GetActive false. Then: no session or seat was added (loginctl, against the list before the
#                         first lock, again after suspend), the error-priority journal gained nothing during the
#                         lock/unlock rounds, and rime-gnome-unlock logged the refusal and the unlock and no warning
#                         but pam_gnome_keyring's. The daemon refuses root (no session) and a user asking for another
#                         user's name. Then the daemon dies: killed (SIGKILL) while the lock prompt holds a channel, it
#                         comes back, Escape resets the prompt and the password unlocks; killed 8 times in a row
#                         (past systemd's default start limit), it is still running and the lock still works.
#   desktop-lock-kde      Plasma's own locker (kscreenlocker, PAM service `kde`): `loginctl lock-session`, wrong
#                         password refused, right one unlocks; Meta+L, wrong then right again; sessions, seats and
#                         the error journal as above.
#   desktop-lock-cosmic   COSMIC's locker (cosmic-greeter in the session, PAM service `cosmic-greeter`): the same,
#                         with COSMIC's Super+Escape. cosmic-greeter 1.11 never sets logind's LockedHint (it stays
#                         "no" while locked; printed in the log); its lock state is its own file
#                         $XDG_RUNTIME_DIR/cosmic-greeter-<session>.lock, there while locked (src/locker.rs).
#
# The passwords are the VM's test account's (rime/rime); the wrong one is only ever typed inside the VM.
#
# Mutants (passthru.mutants, built by hand; each must FAIL):
#   gnome.pam-ignored   rime-gnome-unlock reports success whatever PAM answered: the wrong password unlocks.
#   gnome.no-shim       rime-gnome-unlock's service is off: nothing owns org.gnome.DisplayManager, so GNOME Shell
#                       builds no lock screen; the check fails at its owner assertion.
#   gnome.start-limited the unit has a start limit (systemd's default burst of 5, over 60 s): the crash loop leaves
#                       it failed.
{ pkgs, self }:

let
  inherit (pkgs) lib;

  desktops = {
    # lockKey: the desktop's own lock shortcut (GNOME and Plasma Super+L, COSMIC Super+Escape).
    gnome = { session = "gnome"; process = "gnome-shell"; pam = "rime-gnome-unlock"; lockKey = "meta_l-l"; };
    kde = { session = "plasma"; process = "plasmashell"; pam = "kde"; lockKey = "meta_l-l"; };
    cosmic = { session = "cosmic"; process = "cosmic-comp"; pam = "cosmic-greeter"; lockKey = "meta_l-esc"; };
  };

  lockTest = { id, name, overlay ? null, extraConfig ? { } }:
    let d = desktops.${id}; in
    pkgs.testers.runNixOSTest {
      inherit name;
      node.pkgsReadOnly = false;

      nodes.machine = { lib, pkgs, ... }: {
        imports = [ self.nixosModules.default extraConfig ];
        nixpkgs.overlays = lib.mkAfter (lib.optional (overlay != null) overlay);

        rime.services.enable = true;
        rime.desktop.enable = true;
        rime.desktops.${id}.enable = true;

        users.users.rime = {
          isNormalUser = true;
          uid = 1000;
          initialPassword = "rime";
          extraGroups = [ "wheel" "video" "audio" "networkmanager" ];
        };

        virtualisation = {
          memorySize = 6144;
          cores = 4;
          qemu.options = [ "-vga none" "-device virtio-gpu-pci,xres=1280,yres=800" ];
        };
        environment.sessionVariables = {
          WLR_RENDERER = "pixman";
          QT_QUICK_BACKEND = "software";
        };
        systemd.services.rime-flathub-setup.enable = lib.mkForce false;
        # The update check's timer fires at a random 2-22 min after boot; offline it exits 1 at error priority, which
        # the suspend subtest's "nothing new at error priority" read as a failure when it landed inside it (run
        # 41850082101). The check has its own tests (update-client, update-e2e).
        systemd.timers.rime-update-check.enable = lib.mkForce false;
        systemd.tmpfiles.rules = [ "f+ /var/lib/rime-greet/last-session 0644 greetd greetd - ${d.session}" ];
        # Suspend as s2idle, woken by an RTC alarm (rtcwake), so the VM resumes by itself.
        systemd.sleep.settings.Sleep.SuspendState = "freeze";
        environment.systemPackages = [ pkgs.dbus pkgs.util-linux ];
      };

      testScript = ''
        import json
        import re
        import shlex

        ID = "${id}"
        PROCESS = "${d.process}"
        PAM = "${d.pam}"
        MARKER = "/home/rime/.local/share/gnome-shell/lock-warning-shown"

        def user(cmd):
            env = ("export XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus "
                   "WAYLAND_DISPLAY=wayland-0; ")
            return "su - rime -c " + shlex.quote(env + cmd)

        def session_id():
            return machine.succeed("loginctl show-user rime -p Display --value").strip()

        def locked_hint():
            return machine.succeed(f"loginctl show-session {SID} -p LockedHint --value").strip()

        def locked_cmd():
            # COSMIC's locker never sets logind's LockedHint (cosmic-greeter 1.11 has no SetLockedHint); its own
            # record of the lock is $XDG_RUNTIME_DIR/cosmic-greeter-<session>.lock, created on lock and removed on
            # unlock (src/locker.rs). GNOME and Plasma set LockedHint.
            if ID == "cosmic":
                return "ls /run/user/1000/cosmic-greeter-*.lock"
            return f"test \"$(loginctl show-session {SID} -p LockedHint --value)\" = yes"

        def unlocked_cmd():
            if ID == "cosmic":
                return "! ls /run/user/1000/cosmic-greeter-*.lock"
            return f"test \"$(loginctl show-session {SID} -p LockedHint --value)\" = no"

        def is_locked():
            return machine.execute(locked_cmd())[0] == 0

        GETACTIVE = user("gdbus call --session --dest org.gnome.ScreenSaver --object-path /org/gnome/ScreenSaver "
                         "--method org.gnome.ScreenSaver.GetActive")

        def shield_active():
            return "true" in machine.succeed(GETACTIVE)

        def wait_locked(how):
            machine.wait_until_succeeds(locked_cmd(), timeout=90)
            if ID == "gnome":
                # LockedHint is set as the lock starts; the shield is up once its animation ends.
                machine.wait_until_succeeds(GETACTIVE + " | grep -F true", timeout=30)
            machine.sleep(3)
            print(f"locked ({how}); LockedHint={locked_hint()}")

        def pam_failures():
            return machine.succeed(f"journalctl -b --no-pager -o cat | grep -c 'pam_unix({PAM}:auth): authentication failure' || true").strip()

        def type_password(password):
            # Shift wakes a blanked screen and types nothing. GNOME's lock screen ignores it (js/ui/unlockDialog.js:678)
            # and raises its prompt on the first character, which it keeps.
            machine.send_key("shift")
            machine.sleep(3)
            machine.send_chars(password + "\n")

        def unlock_wrong(shot):
            before = int(pam_failures())
            type_password("not-the-password")
            machine.wait_until_succeeds(f"test $(journalctl -b --no-pager -o cat | grep -c 'pam_unix({PAM}:auth): authentication failure') -gt {before}", timeout=60)
            machine.sleep(5)
            machine.screenshot(shot)
            assert is_locked(), "a wrong password unlocked the session"
            if ID == "gnome":
                assert shield_active(), "a wrong password lowered GNOME's screen shield"
            machine.sleep(3)  # the lock screen clears the failed prompt before the next try

        def unlock_right(shot):
            type_password("rime")
            machine.wait_until_succeeds(unlocked_cmd(), timeout=60)
            if ID == "gnome":
                machine.wait_until_succeeds(GETACTIVE + " | grep -F false", timeout=30)
            machine.sleep(3)
            machine.screenshot(shot)

        def sessions():
            # logind's view, without the short-lived su sessions this script's own commands open.
            out = []
            for s in json.loads(machine.succeed("loginctl list-sessions --json=short")):
                props = machine.succeed(f"loginctl show-session {s['session']} -p Service -p Class -p Type -p Seat -p Name").split()
                if "Service=su" not in props:
                    out.append(" ".join(sorted(props)) + " id=" + s["session"])
            return sorted(out)

        def seats():
            return machine.succeed("loginctl list-seats --no-legend").split()

        machine.wait_for_unit("multi-user.target")
        if ID == "gnome":
            # GNOME Shell deletes this at start-up when it has a lock screen and creates it (with the "Screen Lock
            # disabled" notification) when it does not (js/ui/main.js _handleLockScreenWarning).
            machine.succeed(f"install -d -o rime -g users /home/rime/.local /home/rime/.local/share /home/rime/.local/share/gnome-shell && install -o rime -g users -m 644 /dev/null {MARKER}")
            # GNOME Shell asks once, at start-up, whether it can lock: with the daemon down then, only bus activation
            # can give this session a lock.
            # (A unit that is not loaded, masked here: the no-shim mutant, which must get as far as the owner assertion.)
            ABSENT = "test \"$(systemctl show -p LoadState --value rime-gnome-unlock.service)\" != loaded"
            machine.wait_until_succeeds(f"systemctl is-active rime-gnome-unlock.service || {ABSENT}")
            BOOT_PID = machine.succeed("systemctl show -p MainPID --value rime-gnome-unlock.service").strip()
            machine.succeed(f"systemctl stop rime-gnome-unlock.service || {ABSENT}")
            machine.fail("systemctl is-active rime-gnome-unlock.service")

        with subtest("logs in through rime-greet"):
            machine.wait_for_unit("greetd.service")
            machine.wait_until_succeeds("pgrep -u greetd -f 'qs -p .*/rime-greet/shell.qml'")
            machine.sleep(8)
            machine.send_chars("rime\n")
            machine.sleep(2)
            machine.send_chars("rime\n")
            machine.wait_until_succeeds(f"pgrep -u rime {PROCESS}", timeout=300)
            machine.wait_until_succeeds("test -S /run/user/1000/wayland-0 || test -S /run/user/1000/wayland-1", timeout=120)
            machine.sleep(30)
            SID = session_id()
            print("graphical session:", SID, machine.succeed(f"loginctl show-session {SID}"))
            assert machine.succeed(f"loginctl show-session {SID} -p Type --value").strip() == "wayland"
            print("terminal (rime-gnome-unlock's PAM_TTY):", machine.succeed(f"loginctl show-session {SID} -p TTY -p VTNr -p Display"))
            machine.screenshot(f"{ID}-session")
      ''
      + lib.optionalString (id == "gnome") ''

        def unlock_pid():
            return machine.succeed("systemctl show -p MainPID --value rime-gnome-unlock.service").strip()

        with subtest("GNOME: rime-gnome-unlock answers as the display manager; GNOME Shell built its lock screen"):
            owner = machine.succeed("busctl status org.gnome.DisplayManager --no-pager")
            print(owner)
            pid = unlock_pid()
            assert pid not in ("0", BOOT_PID), f"rime-gnome-unlock was not started again after the stop ({pid})"
            print(machine.succeed("journalctl -b --no-pager -o short-monotonic -u rime-gnome-unlock.service -u dbus.service | grep -iE 'activat|Start' || true"))
            assert f"PID={pid}" in owner, f"org.gnome.DisplayManager is not owned by rime-gnome-unlock ({pid})"
            machine.fail("pgrep -x gdm")
            machine.fail("pgrep -f gdm-session-worker")
            machine.wait_until_fails(f"test -e {MARKER}", timeout=60)
            # No GNOME idle lock behind the test's back: only the idle subtest sets a delay.
            machine.succeed(user("gsettings set org.gnome.desktop.session idle-delay 0"))
            machine.send_key("meta_l-s")
            machine.sleep(3)
            machine.screenshot("gnome-quick-settings")
            machine.send_key("esc")
            machine.sleep(2)

        with subtest("GNOME: the unit's sandbox is in force"):
            pid = unlock_pid()
            print(machine.succeed("systemctl show rime-gnome-unlock.service -p CapabilityBoundingSet -p SystemCallFilter "
                                  "-p ProtectProc -p LimitNOFILE -p Restart -p StartLimitIntervalUSec -p NoNewPrivileges"))
            status = machine.succeed(f"cat /proc/{pid}/status")
            # CAP_CHOWN (0), CAP_DAC_READ_SEARCH (2), CAP_SETGID (6), CAP_SETUID (7), CAP_AUDIT_WRITE (29).
            assert re.search(r"^CapBnd:\s+00000000200000c5$", status, re.M), status
            assert re.search(r"^Seccomp:\s+2$", status, re.M), status
            assert re.search(r"^NoNewPrivs:\s+1$", status, re.M), status
            assert machine.succeed(f"prlimit --pid {pid} --nofile -o SOFT,HARD --noheadings").split() == ["1024", "1024"]

        with subtest("GNOME: a channel's socket is the session user's alone"):
            # From the user's own systemd manager (no session of its own), as GNOME Shell (a user unit) asks.
            out = machine.succeed(user("systemd-run --user --wait --pipe --quiet gdbus call --system --dest org.gnome.DisplayManager "
                                       "--object-path /org/gnome/DisplayManager/Manager "
                                       "--method org.gnome.DisplayManager.Manager.OpenReauthenticationChannel rime"))
            found = re.search(r"unix:path=([^',]+)", out)
            assert found, out
            path = found.group(1)
            st = machine.succeed(f"stat -c '%U %a' {path}").strip()
            print("channel socket:", path, st)
            assert st == "rime 600", st
            # dbus-send --peer: a direct connection with no message-bus Hello, as libgdm makes.
            cancel = (f"dbus-send --peer=unix:path={path} --print-reply /org/gnome/DisplayManager/Session "
                      "org.gnome.DisplayManager.UserVerifier.Cancel")
            out = machine.fail(f"setpriv --reuid=65534 --regid=65534 --clear-groups {cancel} 2>&1")
            print("as nobody:", out)
            assert "Permission denied" in out, out
            print("as rime:", machine.succeed(user(cancel)))
            machine.wait_until_fails(f"test -e {path}", timeout=10)

        with subtest("GNOME: the daemon refuses callers who are not the session's own user"):
            out = machine.fail("gdbus call --system --dest org.gnome.DisplayManager --object-path /org/gnome/DisplayManager/Manager "
                               "--method org.gnome.DisplayManager.Manager.OpenReauthenticationChannel root 2>&1")
            assert "AccessDenied" in out, out
            out = machine.fail(user("gdbus call --system --dest org.gnome.DisplayManager --object-path /org/gnome/DisplayManager/Manager "
                                    "--method org.gnome.DisplayManager.Manager.OpenReauthenticationChannel root 2>&1"))
            assert "AccessDenied" in out, out
      ''
      + ''

        SESSIONS = sessions()
        SEATS = seats()
        ERRORS_BEFORE = machine.succeed("journalctl -b -p err --no-pager -o cat | sort").splitlines()
        print("sessions before:", SESSIONS, "seats:", SEATS)

        with subtest("locks from loginctl; a wrong password is refused; the right one unlocks"):
            machine.succeed(f"loginctl lock-session {SID}")
            wait_locked("loginctl lock-session")
            machine.screenshot(f"{ID}-locked")
            unlock_wrong(f"{ID}-wrong-password")
            unlock_right(f"{ID}-unlocked")

        with subtest("the lock shortcut (${d.lockKey}) locks; wrong then right again"):
            machine.send_key("${d.lockKey}")
            wait_locked("${d.lockKey}")
            machine.screenshot(f"{ID}-locked-super-l")
            unlock_wrong(f"{ID}-wrong-password-2")
            unlock_right(f"{ID}-unlocked-2")
      ''
      + lib.optionalString (id == "gnome") ''

        with subtest("GNOME: org.gnome.ScreenSaver.Lock locks"):
            machine.succeed(user("gdbus call --session --dest org.gnome.ScreenSaver --object-path /org/gnome/ScreenSaver "
                                 "--method org.gnome.ScreenSaver.Lock"))
            wait_locked("ScreenSaver.Lock")
            unlock_right("gnome-unlocked-3")

        with subtest("GNOME: the idle timeout locks"):
            machine.succeed(user("gsettings set org.gnome.desktop.session idle-delay 10"))
            t0 = int(machine.succeed("date +%s"))
            wait_locked("idle")
            print(f"idle lock within {int(machine.succeed('date +%s')) - t0} s")
            machine.screenshot("gnome-locked-idle")
            machine.succeed(user("gsettings set org.gnome.desktop.session idle-delay 0"))
            unlock_right("gnome-unlocked-idle")
      ''
      + ''

        with subtest("no session or seat was added, and nothing new logged at error priority"):
            after = sessions()
            print("sessions after:", after)
            assert after == SESSIONS, f"sessions changed: {SESSIONS} -> {after}"
            assert seats() == SEATS, seats()
            errors = machine.succeed("journalctl -b -p err --no-pager -o cat | sort").splitlines()
            new = sorted(set(errors) - set(ERRORS_BEFORE))
            print("error-priority lines during the lock rounds:", new)
            if ID == "cosmic":
                # cosmic-greeter 1.11 logs this at error level on every lock: the lock surface opens before its
                # rectangle tracker reports it (src/common.rs:426, blur_rects on SubsurfaceOpened), and the
                # Rectangle message just after applies the blur. Upstream's, harmless; nothing else is allowed.
                new = [line for line in new if not re.fullmatch(r"no rectangle for surface Id\(\d+\)", line)]
            assert not new, new
      ''
      + lib.optionalString (id == "gnome") ''

        # Before the suspend round, not after: in this VM the kernel aborts s2idle (virtio -95) and a lock started
        # just after that hung GNOME Shell's main loop (GetActive unanswered for 77 s, no call reached the daemon),
        # in two runs. The suspend round itself passes; it stays the last lock of the test.
        def channels_opened():
            return int(machine.succeed("journalctl -b -u rime-gnome-unlock.service --no-pager -o cat | grep -c 'reauthentication channel for pid' || true").strip())

        def kill_unlock():
            pid = unlock_pid()
            machine.succeed(f"kill -9 {pid}")
            machine.wait_until_succeeds(f"systemctl is-active rime-gnome-unlock.service && test \"$(systemctl show -p MainPID --value rime-gnome-unlock.service)\" != {pid} && busctl status org.gnome.DisplayManager >/dev/null", timeout=30)

        with subtest("GNOME: the daemon killed under an open lock prompt comes back; Escape, then the password unlocks"):
            machine.succeed(f"loginctl lock-session {SID}")
            wait_locked("loginctl lock-session, then the daemon killed")
            opened = channels_opened()
            # GNOME's lock screen ignores Shift (js/ui/unlockDialog.js:678); Ctrl raises the prompt and types nothing.
            machine.send_key("ctrl")
            machine.wait_until_succeeds(f"test $(journalctl -b -u rime-gnome-unlock.service --no-pager -o cat | grep -c 'reauthentication channel for pid') -gt {opened}", timeout=30)
            machine.sleep(2)
            kill_unlock()
            assert locked_hint() == "yes" and shield_active(), "the lock fell when its daemon died"
            machine.screenshot("gnome-locked-daemon-killed")
            machine.send_key("esc")
            machine.sleep(3)
            unlock_right("gnome-unlocked-after-kill")

        with subtest("GNOME: a crash loop does not exhaust the unit; the lock still works"):
            before = int(machine.succeed("systemctl show -p NRestarts --value rime-gnome-unlock.service").strip())
            for _ in range(8):
                kill_unlock()
            restarts = int(machine.succeed("systemctl show -p NRestarts --value rime-gnome-unlock.service").strip())
            print(f"restarts: {before} -> {restarts}")
            assert restarts >= before + 8, restarts
            machine.send_key("${d.lockKey}")
            wait_locked("${d.lockKey}, after the crash loop")
            unlock_wrong("gnome-wrong-password-after-crashes")
            unlock_right("gnome-unlocked-after-crashes")

        with subtest("GNOME: suspend locks the session before the system sleeps"):
            machine.succeed("systemd-run --unit lockmon -p StandardOutput=file:/tmp/lockmon.log dbus-monitor --system "
                            "\"type='method_call',interface='org.freedesktop.login1.Session',member='SetLockedHint'\"")
            machine.sleep(2)
            start = float(machine.succeed("date +%s.%N"))
            machine.succeed("rtcwake -m no -s 25")
            machine.succeed("systemctl suspend")
            machine.wait_until_succeeds("journalctl -b -k --no-pager | grep -F 'PM: suspend exit'", timeout=180)
            entry = [float(line.split()[0]) for line in machine.succeed(
                "journalctl -b -k --no-pager -o short-unix | grep -F 'PM: suspend entry'").splitlines()]
            entry = [t for t in entry if t > start]
            assert entry, "the system never entered suspend"
            mon = machine.succeed("cat /tmp/lockmon.log").splitlines()
            locks = []
            for i, line in enumerate(mon):
                if "member=SetLockedHint" in line and i + 1 < len(mon) and "boolean true" in mon[i + 1]:
                    locks.append(float(line.split("time=")[1].split()[0]))
            locks = [t for t in locks if t > start]
            print(f"suspend requested {start:.3f}, SetLockedHint(true) {locks}, PM: suspend entry {entry}")
            assert locks and locks[0] < entry[0], "the session was not locked before the system suspended"
            machine.succeed("systemctl stop lockmon")
            # The VM's virtio devices refuse s2idle (-95; virtio-fs: "suspend/resume not yet supported"), so the
            # kernel aborts the sleep it had entered. QEMU's emulated SATA disk can also refuse the ATA CHECK POWER MODE
            # probe libata sends while suspending (seen on a CI run). The lock ordering above is the proof; the journal
            # may gain only those, nothing else.
            vm_abort = re.compile(r"(virtio-pci \S+: PM: .*returns -95|virtio-pci \S+: PM: failed to suspend async: error -95"
                                  r"|PM: Some devices failed to suspend, or early wake event detected"
                                  r"|Failed to put system to sleep\. System resumed again: Operation not supported"
                                  r"|Failed to start System Suspend\."
                                  r"|ata\d+\.\d+: Check power mode failed \(err_mask=0x1\))")
            errors = machine.succeed("journalctl -b -p err --no-pager -o cat | sort").splitlines()
            other = [line for line in sorted(set(errors) - set(ERRORS_BEFORE)) if not vm_abort.fullmatch(line)]
            assert not other, other
            assert locked_hint() == "yes", "the session was not locked after resume"
            machine.sleep(5)
            machine.screenshot("gnome-locked-after-resume")
            unlock_right("gnome-unlocked-resume")
            after = sessions()
            assert after == SESSIONS, f"sessions changed across suspend: {SESSIONS} -> {after}"
            assert seats() == SEATS, seats()

        with subtest("GNOME: rime-gnome-unlock logged the refusal and the unlock, and no warning of its own"):
            ulog = machine.succeed("journalctl -b -u rime-gnome-unlock.service --no-pager -o cat")
            print(ulog)
            assert "unlocked by gdm-password" in ulog and "refused (PAM" in ulog, ulog
            # pam_gnome_keyring (in the login stack, as in GDM's gdm-password) warns that it found no keyring daemon
            # to hand the password to; anything else at warning or worse is the daemon's (its own processes only:
            # systemd's notes on the kills above are not the daemon's).
            warn = [line for line in machine.succeed("journalctl -b _SYSTEMD_UNIT=rime-gnome-unlock.service -p warning --no-pager -o cat").splitlines()
                    if not line.startswith("gkr-pam: ")]
            assert not warn, warn
            assert machine.succeed("ls -A /run/rime-gnome-unlock").strip() == "", "a reauthentication socket was left behind"
      '';
    };

  test = id: lockTest { inherit id; name = "rime-desktop-lock-${id}"; };
in
{
  desktop-lock-gnome = test "gnome" // {
    mutants = {
      pam-ignored = lockTest {
        id = "gnome";
        name = "rime-desktop-lock-gnome-pam-ignored";
        overlay = _final: prev: {
          rime = prev.rime // {
            rime-gnome-unlock = prev.rime.rime-gnome-unlock.overrideAttrs (o: {
              # The package's own tests catch this too; the mutant is for the VM check alone to catch.
              doInstallCheck = false;
              postInstall = (o.postInstall or "") + ''
                f=$out/lib/rime-gnome-unlock/rime_gnome_unlock.py
                substituteInPlace $f --replace-fail \
                  'ok = code == PAM_SUCCESS and final_user == self.channel.user and not self.cancelled.is_set()' \
                  'ok = not self.cancelled.is_set()'
              '';
            });
          };
        };
      };
      no-shim = lockTest {
        id = "gnome";
        name = "rime-desktop-lock-gnome-no-shim";
        extraConfig = { lib, ... }: { systemd.services.rime-gnome-unlock.enable = lib.mkForce false; };
      };
      start-limited = lockTest {
        id = "gnome";
        name = "rime-desktop-lock-gnome-start-limited";
        extraConfig = { lib, ... }: { systemd.services.rime-gnome-unlock.unitConfig.StartLimitIntervalSec = lib.mkForce "60s"; };
      };
    };
  };
  desktop-lock-kde = test "kde";
  desktop-lock-cosmic = test "cosmic";
}
