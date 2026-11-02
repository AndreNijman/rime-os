# Nix CI on GitHub-hosted runners

`.github/workflows/nix.yml` on `ubuntu-24.04` hosted runners (4 vCPU, 16 GB, an empty Nix store every job, no Rime
binary cache yet, R2). Jobs: **plan** (evaluate every flake output one per process with `nix/ci/eval-each.sh`, then
pick the checks), **packages** (rimed with its whole cargo test, rime-shell, rime-libexec), one **check** leg per flake
check (`nix/ci/run-checks.sh build`), and **summary** (every check RAN or SKIPPED).

## Measured (11-02)

Per-job wall time, from the GitHub API (`completed_at - started_at`), of the last two full runs on `feat/nixos`:

| Job | run 41400977973 (b46d4b46d) | run 41450212626 (aa86b8883) |
|---|---|---|
| Evaluate and plan | 670 s | 606 s |
| Packages | 1407 s | 1084 s |
| slowest check | terminal-look 3211 s | terminal-look 2371 s |
| next slowest | security 2369, update-e2e 2256, update-client 1972 | desktop 1865 (failed, see below), security 1792, polish 1791 |
| median check leg | about 1100 s | 1044 s |

A check leg is mostly setup and download: a VM check that takes 2-5 min on katana takes 20-30 min on the runner.

## Timeouts (11-02, from these figures)

Each limit is at least twice the slowest measurement, so a slow day passes and a hang is still stopped long before
GitHub's 6 h:

| Limit | before | now | slowest seen |
|---|---|---|---|
| plan job | 45 min | 25 min | 11.2 min |
| packages job | 150 min | 50 min | 23.5 min |
| `RIME_CHECK_TIMEOUT` (the build, recorded as TIMED OUT with its time) | 9900 s | 6600 s | 3211 s |
| check step / check job | 170 / 180 min | 115 / 125 min | 53.5 min |

The step limit stays above `RIME_CHECK_TIMEOUT` plus run-checks.sh's 60 s kill grace, so a check that runs too long
is recorded, not cut off. If a leg ever passes 55 min, raise these from the new measurement rather than guessing.

## Failures seen on the hosted runner, and what was done

| Check | Run | What happened | Now |
|---|---|---|---|
| keymap-luks | both | the passphrase was typed before plymouth held the question; plymouth has no type-ahead, every key was lost, the test waited 300 s | fixed (nix/hygiene): see "keymap-luks under load" |
| hostgates-net | 41400977973 | H2 asked NetworkManager to connect 0.3 s after hostapd started, while wpa_supplicant was restarting (nixpkgs restarts it when a radio appears): "No suitable device found". Passed in 41450212626: a race | fixed (nix/hygiene): the test waits for the device's state (managed, past `unavailable`) and for a scan that saw the SSID; mutant `hostgates-net.mutants.wifi-no-wait` |
| hostgates-disks | 41400977973 | not the VM: rimed's relay test `two_desktops_cannot_hold_one_rendezvous`, compiled in the same leg | fixed by aa86b8883 (the relay claims a room before it sends the 101); passed in 41450212626 (1618 s) |
| release-unit | 41400977973 | the release dry-run proof met the new `nix/ci/eval-each.sh` | fixed by 8bc447255; passed in 41450212626 (218 s) |
| desktop | 41450212626 | the guest wrote nothing after `register-nix-paths` at 11.9 s; 287 s later the driver gave up: "Shell did not start in time". Passed in every earlier run | fixed (nix/ci2): plymouthd stopped answering; see "desktop: a splash that stops answering". The module bounds the splash's units and stops plymouthd; `checks.desktop-plymouth-stuck` (mutants `no-kill`, `no-bound`) |
| any | (earlier runs) | a substitute download from cache.nixos.org failed mid-build | `run-checks.sh` retries a build once when the log says a download failed, never on a builder failure (`nix/ci/fixtures/`, checks.ci) |

## desktop: a splash that stops answering

Hosted job 125298150203 (run 41450212626; log `/var/lab/rime-nix/artifacts/ci2/desktop-41450212626.log`): after
switch-root, `Starting Tell Plymouth To Write Out Runtime Data...` at 10.6 s (plymouth-read-write.service,
`Before=sysinit.target`, no timeout of its own) and then nothing: no sysinit.target, so no test backdoor, and the
driver gave up 287 s later. Six local runs of the same check, pinned to 2 CPUs with 4 and 6 busy loops, all passed
(`desktop-pinned-{a,b}-{1,2,3}.log`): not reproduced by load. Freezing plymouthd (SIGSTOP) just before
plymouth-read-write gives the hosted console log line for line, ending at "Reached target Remote File Systems", rc 1
(`desktop-experiment-freeze-plymouthd.log`). Why plymouthd stopped answering on the runner is not known; what it
cost is: plymouth-quit-wait has `TimeoutSec=0` and greetd starts after it, so on a machine the same fault holds the
greeter forever.

Fix (`nix/modules/desktop.nix`, "Boot splash"): plymouth-read-write and plymouth-quit are bounded to 15 s and
plymouth-quit-wait to 60 s; when one of them does not finish, its `ExecStopPost` SIGKILLs plymouthd by its pid file
(a stopped process ignores SIGTERM; a pid whose `comm` is not plymouthd is left alone) and logs `rime: plymouthd (pid N)
did not answer <unit>` to the kernel log. `checks.desktop-plymouth-stuck` freezes plymouthd before plymouth-read-write:

| Run | Result | Log (`/var/lab/rime-nix/artifacts/ci2/`) |
|---|---|---|
| the check | PASS: plymouth-read-write timed out at 24.0 s, plymouthd killed, sysinit.target at 24.1 s, rime-greet on VT 1; test script 28.7 s | `plymouth-stuck.log` |
| mutant `no-bound` (the units as plymouth ships them) | FAILS with the hosted runner's error: "Shell did not start in time" | `plymouth-stuck-mut-no-bound.log` |
| mutant `no-kill` (bounded, plymouthd left frozen) | FAILS: both units time out, then systemctl loses PID 1 ("Transport endpoint is not connected") and multi-user.target is never seen | `plymouth-stuck-mut-no-kill.log` |
| `checks.desktop` with the fix | PASS (test script 45.6 s) | `desktop-ci2.log` |
| `checks.desktop` with the fix, pinned to 2 CPUs with 4 busy loops, 3 times | PASS 3/3 (50-57 s); no `rime: plymouthd` line, so the bounds never fired on a slow but answering splash | `desktop-fix-pinned-{1,2,3}.log` |
| the check, pinned the same way, twice | PASS 2/2 (sysinit.target at 27.9 s and 27.3 s) | `plymouth-stuck-pinned-{1,2}.log` |

## keymap-luks under load

What the hosted runner showed (job 125136131267): the initrd asked for the passphrase at 4.3 s, the test typed at
4.9 s, plymouth started at 5.45 s and its agent at 5.61 s. Plymouth drops keys that arrive before it holds a
question, so nothing was typed into the prompt that existed.

The check now types only when the guest says the keys will be read: the ask report waits for the keymap load to
finish **and** for plymouth's own trace (`plymouth.debug=stream:/run/rime-test-plymouth.trace`, read in the initrd)
to show it queued the n-th password request, then prints `agent=plymouth`; the test refuses to type otherwise.

Proof that this holds when the machine is short of CPU, on katana: the test driver and QEMU pinned with `taskset` to
2 host CPUs, with 4 busy loops pinned to the same 2 CPUs (load ~12):

| Run | Result | Log (`/var/lab/rime-nix/artifacts/hygiene/`) |
|---|---|---|
| fixed check, twice | PASS both (75 s, 76 s): every ask reported `agent=plymouth`, gb and ch unlock at the first ask, the us control refuses | `keymap-luks-pinned-1.log`, `keymap-luks-pinned-2.log` |
| mutant `type-early` (no wait: the check before 11-02) | FAILS in 28 s: "the gb passphrase was refused" | `keymap-luks-mutant-type-early-pinned.log` |
| fixed check, unpinned | PASS | `keymap-luks.log` |

The runner is `nix/tests/lib/run-pinned.sh DRIVER LOG CPUS HOGS` (inside the rime-nix container, where /dev/kvm
is): it builds nothing, it runs a built test driver (the check's `driver` attribute, `bin/nixos-test-driver`) under
`taskset`.
The same method is how to check any VM test for timing races before trusting it on the runner.

## hostgates-net under load

Same method, same 2 CPUs and 4 busy loops (katana's own load was ~28 at the time, other builds running):

| Run | Result | Log (`/var/lab/rime-nix/artifacts/hygiene/`) |
|---|---|---|
| mutant `wifi-no-wait` (asks NetworkManager to connect as soon as hostapd runs: the check before 11-02) | FAILS in 56 s with the hosted runner's error: "No suitable device found for this connection" | `hostgates-net-mutant-wifi-no-wait-pinned.log` |
| fixed check, twice | PASS both (95 s, 116 s): H2 (both profiles), H3 (WireGuard, OpenVPN), shares, rpcbind | `hostgates-net-pinned-1.log`, `hostgates-net-pinned-2.log` |
| fixed check, unpinned | PASS (test script 110 s) | `hostgates-net.log` |

Re-run in verification (11-03, logs in `/var/lab/rime-nix/artifacts/verify-hygiene/`): the mutant is a race, so it does not
fail every time. Pinned to CPUs 4,5 with 4 busy loops it PASSED (a new run, not the failing log of the same name above); with 6
it failed in `wifi_ap` because `nmcli -s -g 802-11-wireless-security.psk` read back an EMPTY psk while wpa_supplicant
restarted, and hostapd refused a 0-length passphrase (`-pinned-2.log`); on CPU 4 alone with 6 it failed with "No suitable
device found" (`-pinned-3.log`). These mutant runs were at the content before the psk wait below. The empty-psk read is a race the fixed check had too: `wifi_up` now waits until the psk
reads back non-empty. With that, the fixed check PASSES pinned to CPUs 4,5 (110 s) and to CPU 4 alone (134 s), 6 busy
loops each (`hostgates-net-final-pinned-45.log`, `hostgates-net-final-pinned-4.log`).

Two traps found while making it, both in the test, not the product:

- The test driver's shell runs with `pipefail`: `for i in ...; do [ ... ] && basename $i; done | grep .` fails whenever
  the LAST item does not match, even though an earlier one printed. Look a file up with `grep -l` instead.
- A NetworkManager VPN is based on the primary connection, and NetworkManager routes the VPN server's address through
  that device's gateway. In a NixOS VM test the primary connection is QEMU's user network (eth0), so a server on the
  test VLAN (eth1) gets the handshake through eth1 and every data packet sent out of eth0: activated, no traffic.
  Base the VPN on the VLAN's device: `nmcli connection up <vpn> ifname eth1`.

**The mutant made deterministic (nix/ci2, 11-03).** A race mutant proves nothing when it passes. `hostgates-net` now
holds wpa_supplicant down for 20 s right before the wait (`wifi_hold_supplicant` in hostlayer-common.nix: the unit
stopped, its D-Bus activation refused by a runtime `ConditionPathExists=!/run/rime-test-wpa-hold` drop-in, a
`systemd-run --on-active=20s` timer lifts it): the Wi-Fi device is `unavailable`, as it is while nixpkgs restarts
wpa_supplicant for a new radio, on every run, not only on a slow runner. The check waits it out (24.4 s, NetworkManager
"re-acquiring supplicant interface", then "Started WPA Supplicant instance") and passes; the mutant `wifi-no-wait`
(no wait, no autoconnect, `nmcli connection up` at once) fails with the hosted error "No suitable device found for this
connection" (`/var/lab/rime-nix/artifacts/ci2/hostgates-net2.log`, `hostgates-net2-mut-wifi-no-wait.log`). Pinned to 2 CPUs with 4 busy loops: the check
PASSES and the mutant FAILS the same way (`hostgates-net-pinned.log`, `hostgates-net-mut-wifi-no-wait-pinned.log`).
Only `hostgates-net` sets the hold; `hostlayer-katana` (run by hand) does not.
