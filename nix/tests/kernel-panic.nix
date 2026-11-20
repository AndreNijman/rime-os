# What a Rime machine does when the kernel dies (nix/modules/crash.nix, docs/kernel-crash.md), on the Rime kernel,
# with UEFI (OVMF: real EFI variables) and a KMS display (QEMU's standard VGA = the bochs DRM driver, which supports
# drm_panic).
#
# The case it reproduces: a driver floods the kernel log (an rtl8xxxu dongle printed 636,000 `C2H content:` lines on
# the L16), then the kernel panics. Before this change the panic screen was the log tail (drm.panic_screen=kmsg: the
# flood), the machine waited forever (panic=0), and nothing was kept (efi_pstore disabled).
#
#   1. the log is flooded through /dev/kmsg (5,000 lines, more than the whole log buffer), then `echo c >
#      /proc/sysrq-trigger` panics the kernel. Root's write to that file ignores the kernel.sysrq mask, so the test
#      enables nothing;
#   2. (b) the panic screen is the "user" screen, not the flood: drm.panic_screen reads `user`, and a screenshot
#      taken during the panic has text on few pixel rows (the flood fills the whole screen with text). The
#      screenshot is kept as panic-screen.png in $out;
#   3. (a) the machine restarts by itself, 30 s after the panic by the guest's clock (QEMU runs with -no-reboot, so
#      the restart is QEMU exiting; the test then boots it again);
#   4. (c) after the restart, /var/lib/systemd/pstore holds the record with "Kernel panic - not syncing: sysrq
#      triggered crash" (and the flood's tail before it);
#   5. (e) the EFI variables are gone (systemd-pstore Unlink=yes): none of efi-pstore's dump-type* variables is left
#      in efivarfs, and /sys/fs/pstore is empty;
#   6. (d) /var/lib/rime/crash/last and `rime crash` name the panic; one history line; ordered before logins.
#
# The node sets `panic=0` between the framework's `panic=1` and Rime's own `panic=30`, so a machine without Rime's
# argument really never restarts (the no-panic mutant), instead of passing on the framework's.
#
# Mutants (mutants.<name>, built by hand, each must FAIL):
#   no-panic          Rime's `panic=30` left out                -> (a) the machine never restarts
#   pstore-disabled   `efi_pstore.pstore_disable=0` left out    -> (c) no record
#   no-summary-unit   rime-crash-record.service not installed   -> (d) no summary, `rime crash` names nothing
#   kmsg-screen       `drm.panic_screen=user` left out          -> (b) the panic screen is the flood
{ pkgs, self }:

let
  lib = pkgs.lib;

  mkTest =
    {
      name ? "",
      drop ? null,
      recordUnit ? true,
    }:
    pkgs.testers.runNixOSTest {
      name = "rime-kernel-panic" + lib.optionalString (name != "") "-mutant-${name}";

      # nixosModules.default sets nixpkgs.overlays (pkgs.rime.*), so nodes evaluate their own nixpkgs.
      node.pkgsReadOnly = false;

      extraPythonPackages = p: [ p.pillow ];

      nodes.machine =
        { lib, config, ... }:
        {
          imports = [ self.nixosModules.default ];

          rime.kernel = {
            enable = true;
            nvidia.enable = false;
          };
          # The `rime` command (rimed's workspace), as on every Rime host.
          rime.services.enable = true;

          # After the framework's `panic=1` (order 1000), before Rime's `panic=30` (mkAfter = 1500).
          boot.kernelParams = lib.mkOrder 1200 [ "panic=0" ];

          rime.crash.kernelParams = lib.mkIf (drop != null) (
            lib.filter (a: a != drop) (builtins.fromTOML (builtins.readFile ../../files/system/kargs.d/30-rime-crash.toml)).kargs
          );
          systemd.services.rime-crash-record.enable = lib.mkIf (!recordUnit) (lib.mkForce false);

          virtualisation = {
            useEFIBoot = true;
            memorySize = 2048;
            # QEMU's standard VGA: the bochs DRM driver (drm_panic draws on it). The q35 board's ICH9 TCO
            # watchdog runs from power-on and the kernel stops petting it at a panic, so it resets the VM about
            # 45 s later even with panic=0 (measured: the no-panic mutant "restarted" after 50 s). Strapped to
            # no-reboot, only panic= can restart this machine.
            qemu.options = [ "-vga std" "-global ICH9-LPC.noreboot=true" ];
          };

          # No network in the test VM: the Flathub oneshot would only retry.
          systemd.services.rime-flathub-setup.enable = lib.mkForce false;
        };

      testScript = ''
        import datetime
        import os
        import time
        from PIL import Image

        CRASH_GUID = "cfc8fc79-be2e-4ddc-97f0-9f98bfe298a0"  # LINUX_EFI_CRASH_GUID: efi-pstore's variables
        FLOOD = 5000

        def screen_rows(path):
            """Text rows of a panic screen: the share of pixel rows holding a near-white pixel, and the share of
            near-white pixels. Both drm_panic screens draw white text on blue (CONFIG_DRM_PANIC_*_COLOR)."""
            img = Image.open(path).convert("RGB")
            w, h = img.size
            data = img.tobytes()
            white_rows = 0
            white = 0
            blue = 0
            for y in range(h):
                row_white = False
                for i in range(y * w * 3, (y + 1) * w * 3, 3):
                    r, g, b = data[i], data[i + 1], data[i + 2]
                    if r > 200 and g > 200 and b > 200:
                        white += 1
                        row_white = True
                    elif b > 120 and r < 60 and g < 60:
                        blue += 1
                if row_white:
                    white_rows += 1
            n = w * h
            return white_rows / h, white / n, blue / n, (w, h)

        machine.start()
        machine.wait_for_unit("multi-user.target")

        with subtest("the shipped arguments are in force"):
            cmdline = machine.succeed("cat /proc/cmdline")
            print(cmdline)
            panic = machine.succeed("cat /proc/sys/kernel/panic").strip()
            screen = machine.succeed("cat /sys/module/drm/parameters/panic_screen").strip()
            print(f"kernel.panic={panic} drm.panic_screen={screen}")
            machine.succeed("test -d /sys/firmware/efi/efivars")
            print(machine.succeed("ls /sys/class/drm; readlink -f /sys/class/drm/card*/device/driver || true"))
            print(machine.succeed("dmesg | grep -i pstore || true"))
            print(machine.succeed("grep -H . /sys/class/watchdog/*/identity /sys/class/watchdog/*/state "
                                  "/sys/class/watchdog/*/timeout 2>/dev/null || true"))
            # No crash yet: no record, no summary, and `rime crash` says so.
            machine.succeed("test ! -e /var/lib/rime/crash/last")
            out = machine.succeed("rime crash")
            assert "No kernel crash recorded" in out, out

        with subtest("a driver floods the log, then the kernel panics"):
            # Writes to /dev/kmsg are rate-limited by default (printk.devkmsg): the flood must reach the log whole.
            machine.succeed("sysctl -w kernel.printk_devkmsg=on")
            machine.succeed(
                "for i in $(seq 1 " + str(FLOOD) + "); do "
                "echo \"<6>rtl8xxxu 1-2:1.0: C2H content: 00000000: 07 1a 2b 3c 4d 5e 6f 70 81 92 a3 b4 c5 d6 e7 f8 $i\"; "
                "done > /dev/kmsg"
            )
            n = int(machine.succeed("dmesg | grep -c 'C2H content'").strip())
            print(f"flood lines still in the log buffer: {n}")
            assert n > 100, n
            machine.screenshot("before-panic")
            # Detached: the backdoor shell dies with the kernel.
            machine.succeed("systemd-run --no-block sh -c '${pkgs.coreutils}/bin/sleep 2; echo c > /proc/sysrq-trigger'")
            machine.wait_for_console_text("Kernel panic - not syncing")
            t_panic = time.monotonic()

        with subtest("(b) the panic screen is readable, not the flood"):
            time.sleep(5)
            machine.screenshot("panic-screen")
            path = os.path.join(machine.out_dir, "panic-screen.png")
            rows, white, blue, size = screen_rows(path)
            print(f"panic screen {size[0]}x{size[1]}: text on {rows:.1%} of pixel rows, white {white:.2%}, blue {blue:.1%}")
            assert blue > 0.5, f"not a drm_panic screen (blue {blue:.1%}): the panic was not drawn"
            assert white > 0.001, f"nothing drawn on the panic screen (white {white:.2%})"
            # The flood fills every text row of the screen; the user screen holds a logo and five short lines.
            assert rows < 0.40, f"text on {rows:.1%} of the rows: this is the log, not the panic screen"
            assert screen == "user", f"drm.panic_screen={screen}"

        with subtest("(a) the machine restarts by itself after the screen can be read"):
            machine.wait_for_shutdown(timeout=datetime.timedelta(seconds=120))
            restart = time.monotonic() - t_panic
            print(f"restart {restart:.1f} s after the panic message")
            # 30 s by the guest's clock. The kernel counts it with busy-wait delays (interrupts are off), and in a VM
            # those run off the boot-time calibration (the test framework boots with clocksource=acpi_pm): measured
            # 23.3 s of host time once. The bounds tell a timed restart from the framework's panic=1 (about 1 s) and
            # from none at all (the timeout above).
            assert 15 <= restart <= 90, f"restart after {restart:.1f} s"
            assert panic == "30", f"kernel.panic={panic}"

        machine.start()
        machine.wait_for_unit("multi-user.target")

        with subtest("(c) systemd-pstore kept the panic, flood and all"):
            print(machine.succeed("systemctl status systemd-pstore.service --no-pager || true"))
            print(machine.succeed("{ find /var/lib/systemd/pstore -printf '%M %s %TT %p\\n' 2>&1 || true; } | sort -k4"))
            # systemd-pstore 262: <seconds>/001/ per second the dump took to write, each with the raw EFI parts
            # (dmesg-efi_pstore-<id>) and dmesg.txt joining them. The panic is in the first parts written.
            panic_txt = machine.succeed(
                "{ find /var/lib/systemd/pstore -type f -name dmesg.txt "
                "-exec grep -l 'Kernel panic - not syncing: sysrq triggered crash' {} + 2>/dev/null || true; } "
                "| sort -V | tail -n 1"
            ).strip()
            assert panic_txt, "no dmesg.txt in the archive names the panic"
            rec = machine.succeed("find /var/lib/systemd/pstore -type f -name dmesg.txt -exec cat {} + 2>/dev/null || true")
            parts = machine.succeed("find /var/lib/systemd/pstore -type f -name 'dmesg-efi_pstore-*' | wc -l").strip()
            dirs = machine.succeed("find /var/lib/systemd/pstore -type f -name dmesg.txt | wc -l").strip()
            raw = machine.succeed("find /var/lib/systemd/pstore -type f -name 'dmesg-efi_pstore-*' -printf '%s\\n' "
                                  "| awk '{s+=$1} END {print s+0}'").strip()
            flood_in_record = rec.count("C2H content")
            assert int(parts) > 0, "no EFI parts: the record did not come from efi-pstore"
            print(f"record: {parts} EFI parts in {dirs} directories, {raw} bytes uncompressed, "
                  f"{flood_in_record} flood lines, panic in {panic_txt}")
            print("--- head of the record holding the panic ---")
            print(machine.succeed(f"head -n 3 {panic_txt}"))
            print("--- record excerpt ---")
            lines = machine.succeed(f"cat {panic_txt}").splitlines()
            i = next(i for i, l in enumerate(lines) if "Kernel panic - not syncing" in l)
            print("\n".join(lines[max(0, i - 5):i + 30]))

        with subtest("(d) the summary and `rime crash` name the panic"):
            print(machine.succeed("systemctl status rime-crash-record.service --no-pager || true"))
            last = machine.succeed("cat /var/lib/rime/crash/last")
            print(last)
            assert "kind=panic" in last, last
            assert "reason=Kernel panic - not syncing: sysrq triggered crash" in last, last
            boot_id = machine.succeed("cat /proc/sys/kernel/random/boot_id").strip()
            assert f"boot_id={boot_id}" in last, last
            out = machine.succeed("rime crash")
            print(out)
            assert "Last kernel crash (the machine restarted)" in out, out
            assert "sysrq triggered crash" in out, out
            assert machine.succeed("rime crash --json").find('"kind":"panic"') >= 0
            # One history line for the crash (what the notice drains), this boot's window closed, and the
            # summary written before anyone could log in.
            crash_id = next(l[3:] for l in last.splitlines() if l.startswith("id="))
            history = machine.succeed("cat /var/lib/rime/crash/history")
            print(history)
            rows = [r.split("\t") for r in history.splitlines()]
            assert len(rows) == 1 and rows[0][0] == crash_id and rows[0][3] == boot_id, history
            btime = machine.succeed("awk '$1 == \"btime\" { print $2 }' /proc/stat").strip()
            assert machine.succeed("cat /var/lib/rime/crash/checked").strip() == btime
            before = machine.succeed("systemctl show -p Before --value rime-crash-record.service")
            assert "systemd-user-sessions.service" in before.split(), before
            # A second run in the same boot keeps the same summary (one notice per crash).
            machine.succeed("systemctl restart rime-crash-record.service")
            assert machine.succeed("cat /var/lib/rime/crash/last") == last
            assert machine.succeed("cat /var/lib/rime/crash/history") == history

        with subtest("(e) the EFI variables were removed after collection"):
            # efivarfs read the firmware's variables before systemd-pstore deleted them and keeps that listing
            # (measured: even a umount + mount of efivarfs still listed all of them), so what it shows now is how
            # much NVRAM the dump took. The firmware is asked again by the next start.
            print(machine.succeed(
                f"ls -l /sys/firmware/efi/efivars | grep -- '-{CRASH_GUID}$' "
                "| awk '{n++; s+=$5} END {print \"efi-pstore variables written: \" n+0 \", \" s+0 \" bytes\"}'"
            ))
            machine.succeed("test -z \"$(ls -A /sys/fs/pstore)\"")
            dirs_before = machine.succeed("find /var/lib/systemd/pstore -mindepth 1 -maxdepth 1 | sort")
            machine.shutdown()
            machine.start()
            machine.wait_for_unit("multi-user.target")
            left = machine.succeed(f"ls /sys/firmware/efi/efivars | grep -c -- '-{CRASH_GUID}$' || true").strip()
            print(f"efi-pstore variables left after one more start: {left}")
            assert left == "0", left
            machine.succeed("test -z \"$(ls -A /sys/fs/pstore)\"")
            assert machine.succeed("find /var/lib/systemd/pstore -mindepth 1 -maxdepth 1 | sort") == dirs_before
            # The summary still names the crash, and its boot is not this one; the history has no second line.
            assert machine.succeed("cat /var/lib/rime/crash/last") == last
            assert machine.succeed("cat /var/lib/rime/crash/history") == history
      '';
    };
  # Without a VM: the released system (nixosConfigurations.rime, what the bridge payload and the Nix channel carry)
  # has the three arguments, and its last `panic=` is Rime's.
  shippedParams = self.nixosConfigurations.rime.config.boot.kernelParams;
  shipped = pkgs.runCommand "rime-kernel-panic-shipped" { } ''
    fail() { echo "FAIL: $*" >&2; exit 1; }
    # Strings only: the context is dropped so this check never builds the kernel or anything else of that system.
    params=${lib.escapeShellArg (builtins.unsafeDiscardStringContext (lib.concatStringsSep "\n" shippedParams))}
    printf '%s\n' "$params" > params
    for a in panic=30 drm.panic_screen=user efi_pstore.pstore_disable=0; do
      grep -qx -- "$a" params || fail "nixosConfigurations.rime lacks $a"
    done
    last=$(grep '^panic=' params | tail -n 1)
    [ "$last" = panic=30 ] || fail "the last panic= on the command line is $last"
    grep -qx 'kargs = \["panic=30", "drm.panic_screen=user", "efi_pstore.pstore_disable=0"\]' \
      ${../../files/system/kargs.d/30-rime-crash.toml} || fail "kargs.d file changed shape"
    echo "nixosConfigurations.rime: panic=30 (last), drm.panic_screen=user, efi_pstore.pstore_disable=0" | tee $out
  '';
in
{
  kernel-panic-shipped = shipped;
  kernel-panic = mkTest { } // {
    mutants = {
      no-panic = mkTest { name = "no-panic"; drop = "panic=30"; };
      pstore-disabled = mkTest { name = "pstore-disabled"; drop = "efi_pstore.pstore_disable=0"; };
      no-summary-unit = mkTest { name = "no-summary-unit"; recordUnit = false; };
      kmsg-screen = mkTest { name = "kmsg-screen"; drop = "drm.panic_screen=user"; };
    };
  };
}
