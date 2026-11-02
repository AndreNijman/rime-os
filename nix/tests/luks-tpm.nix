# The TPM slot health check and its re-bind (nix/modules/luks-tpm.nix, `rime luks`, `rime doctor`):
#
#   luks-tpm-rebind  a NixOS VM with a software TPM (swtpm) and a LUKS2 volume whose TPM slot is sealed to PCR 7 by
#                    systemd-cryptenroll, the way rime-luks-enroll seals it:
#                    - rime-luks-tpm-status recomputes the token's policy digest from this boot's PCR 7 and gets
#                      exactly the token's `tpm2-policy-hash` (state ok), and the TPM really unseals it;
#                    - PCR 7 is extended (what a new Secure Boot key or a dbx update does to the next boot): the
#                      TPM now REFUSES to unseal, the state is stale, the boot unit's report says so, and
#                      `rime doctor` (unprivileged) names the volume, `sudo rime luks rebind` and the unit;
#                    - `rime luks rebind` starts rime-luks-tpm-reenrol@<that uuid>.service, which asks once for the
#                      passphrase (answered over systemd's ask-password protocol) and runs `rime-luks-enroll
#                      --tpm-only` (the volume carries a recovery key, as an install leaves it, and keeps it);
#                      this VM boots BIOS, no Secure Boot, so rime-luks-enroll declines to seal (exit 3, its SB
#                      rule) and rebind reports the volume still stale (the re-seal itself under Secure Boot is
#                      OVMF lab N3, p2-gates T3);
#                    - re-sealed to PCR 7 by hand, the state is ok again and the doctor line goes back to ok.
#                    Mutant: a status program that ignores the PCR value (prints ok for any slot) must fail the
#                    stale subtest.
{ pkgs, self }:

let
  uuid = "3f6c2a1e-8b4d-4c7a-9e2f-0a1b2c3d4e5f";
  pw = "rime-lab-passphrase";
  # An ask-password agent for the test: answers every systemd-ask-password request with the passphrase (the
  # protocol systemctl's own agent speaks on a terminal).
  agent = pkgs.writeText "rime-test-ask-agent.py" ''
    import configparser, glob, socket, time
    done = set()
    while True:
        for f in glob.glob("/run/systemd/ask-password/ask.*"):
            if f in done:
                continue
            c = configparser.ConfigParser()
            c.read(f)
            s = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
            s.sendto(b"+${pw}", c["Ask"]["Socket"])
            done.add(f)
        time.sleep(0.2)
  '';
in
{
  luks-tpm-rebind = pkgs.testers.runNixOSTest {
    name = "rime-luks-tpm-rebind";
    node.pkgsReadOnly = false;

    nodes.machine = { pkgs, ... }: {
      imports = [ self.nixosModules.default ];
      virtualisation.tpm.enable = true;
      virtualisation.emptyDiskImages = [ 64 ];
      # The rime CLI as a shipped system has it (rime.services.enable is off in this VM: no daemons needed).
      environment.systemPackages = [ pkgs.rime.rimed pkgs.cryptsetup pkgs.tpm2-tools ];
      users.users.alice = { isNormalUser = true; };
    };

    testScript = ''
      import json

      U = "${uuid}"
      PW = "${pw}"
      AGENT = "${agent}"
      machine.wait_for_unit("multi-user.target")
      machine.succeed("test -c /dev/tpmrm0 && test -r /sys/class/tpm/tpm0/pcr-sha256/7")
      # The TPM lab's form (nix/probe/ovmf/tpm/systems.nix): with the device TCTI named, tpm2_pcrread reads the
      # kernel's resource manager directly and prints no tabrmd D-Bus warning, and agrees with sysfs.
      out = machine.succeed("TPM2TOOLS_TCTI=device:/dev/tpmrm0 tpm2_pcrread sha256:7 2>&1")
      assert "tabrmd" not in out.lower() and "dbus" not in out.lower(), out
      sysfs = machine.succeed("cat /sys/class/tpm/tpm0/pcr-sha256/7").strip().lower()
      assert sysfs in out.lower().replace(" ", ""), (sysfs, out)

      def report():
          r = json.loads(machine.succeed("rime-luks-tpm-status"))
          vols = [v for v in r["volumes"] if v["uuid"] == U]
          assert len(vols) == 1, r
          print(json.dumps(vols[0], indent=2))
          return vols[0]

      def unseals():
          rc, out = machine.execute(
              "systemd-cryptsetup attach rime-tpm-probe /dev/vdb - tpm2-device=auto,headless=1 2>&1")
          if rc == 0:
              machine.succeed("systemd-cryptsetup detach rime-tpm-probe")
          print(f"TPM unseal: rc={rc} {out.strip()[-300:]}")
          return rc == 0

      def doctor():
          machine.succeed("systemctl restart rime-luks-tpm-status.service")
          return machine.succeed("su - alice -c 'rime doctor --json'")

      with subtest("the boot unit runs and finds no LUKS volume yet"):
          machine.succeed("systemctl is-active rime-luks-tpm-status.service")
          r = json.loads(machine.succeed("cat /run/rime/luks-tpm.json"))
          assert r["volumes"] == [], r

      with subtest("a LUKS2 volume sealed to PCR 7 (rime-luks-enroll's binding) is ok, and the TPM agrees"):
          machine.succeed(f"echo -n {PW} | cryptsetup luksFormat --type luks2 -q --iter-time=1 --uuid {U} /dev/vdb -")
          # The recovery key an install files (rime-luks-enroll's full path); --tpm-only refuses a volume without one.
          machine.succeed(f"PASSWORD={PW} systemd-cryptenroll --recovery-key /dev/vdb >/dev/null")
          machine.succeed(f"PASSWORD={PW} systemd-cryptenroll --tpm2-device=auto --tpm2-pcrs=7 /dev/vdb")
          v = report()
          assert v["state"] == "ok" and v["pcrs"] == [7] and v["policy_now"] == v["policy_sealed"], v
          assert unseals(), "the TPM refused a slot the check calls ok"
          mine = [c for c in json.loads(doctor())["checks"] if U in c["check"]]
          assert len(mine) == 1 and mine[0]["ok"] is True and "matches this boot" in mine[0]["check"], mine
          machine.succeed("su - alice -c 'rime luks status'")

      with subtest("PCR 7 changes (a new Secure Boot key / dbx): stale, the TPM refuses, the doctor names it"):
          machine.succeed("tpm2_pcrextend 7:sha256=" + "ab" * 32)
          assert not unseals(), "the TPM still unseals after PCR 7 changed: the subtest tests nothing"
          v = report()
          assert v["state"] == "stale" and v["policy_now"] != v["policy_sealed"], v
          d = doctor()
          print(d)
          mine = [c for c in json.loads(d)["checks"] if U in c["check"]]
          assert len(mine) == 1 and mine[0]["ok"] is False, mine
          assert f"rime-luks-tpm-reenrol@{U}.service" in mine[0]["check"], mine
          assert "sudo rime luks rebind" in mine[0]["check"], mine
          rc, out = machine.execute("su - alice -c 'rime luks status'")
          assert rc == 1 and "stale" in out, (rc, out)

      with subtest("rime luks rebind starts rime-luks-tpm-reenrol@<uuid> and the passphrase reaches it"):
          machine.succeed(f"systemd-run --unit=rime-test-agent --property=Type=exec ${pkgs.python3}/bin/python3 -I {AGENT}")
          # Only the test agent answers (systemd-ask-password starts the console and wall agents itself; on a real
          # machine they are how the owner sees the prompt), and rebind runs in its own unit with its output in a
          # file: run from the test driver's shell, the driver's reply came back garbled ("Incorrect padding")
          # three times, with stdin a tty, with stdin /dev/null, and with the agents masked.
          machine.succeed("systemctl mask --runtime --now systemd-ask-password-wall.path systemd-ask-password-wall.service"
                          " systemd-ask-password-console.path systemd-ask-password-console.service")
          machine.succeed("systemd-run --no-block --no-ask-password --collect --unit=rime-test-rebind -p RuntimeMaxSec=180"
                          " /bin/sh -c '/run/current-system/sw/bin/rime luks rebind >/tmp/rebind.out 2>&1; echo $? >/tmp/rebind.rc'"
                          " </dev/null >/dev/null 2>&1")
          machine.wait_until_succeeds("test -s /tmp/rebind.rc", timeout=200)
          rc = int(machine.succeed("cat /tmp/rebind.rc").strip())
          out = machine.succeed("cat /tmp/rebind.out")
          print(f"rc={rc}\n{out}")
          assert f"rime-luks-tpm-reenrol@{U}.service" in out, out
          j = machine.succeed(f"journalctl -b --no-pager -u 'rime-luks-tpm-reenrol@{U}.service'")
          print(j)
          # rime-luks-enroll runs only after systemd-ask-password returned: the passphrase reached the unit.
          assert "rime-luks-enroll: enrolling key slots" in j, j
          assert "keeping the recovery key and passphrase slots" in j, j
          # This VM boots BIOS, so no Secure Boot: rime-luks-enroll declines to seal (exit 3, reason not-uefi) and
          # rebind must not claim success.
          assert "status=3" in j, j
          assert rc != 0, "rebind reported success on a machine where nothing could be sealed"
          assert report()["state"] in ("stale", "no-tpm-slot")
          machine.succeed("cryptsetup luksDump /dev/vdb | grep systemd-recovery")  # the recovery key is still there
          machine.succeed("systemctl stop rime-test-agent")
          machine.fail("su - alice -c 'rime luks rebind'")  # needs root

      with subtest("re-sealed to this boot's PCR 7: ok again"):
          machine.succeed(f"PASSWORD={PW} systemd-cryptenroll --wipe-slot=tpm2 --tpm2-device=auto --tpm2-pcrs=7 /dev/vdb")
          v = report()
          assert v["state"] == "ok", v
          assert unseals()
          mine = [c for c in json.loads(doctor())["checks"] if U in c["check"]]
          assert len(mine) == 1 and mine[0]["ok"] is True and "rebind" not in mine[0]["check"], mine
          out = machine.succeed("rime luks rebind")
          assert "nothing to re-bind" in out, out
    '';
  };
}
