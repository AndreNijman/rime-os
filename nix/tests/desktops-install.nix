# checks.desktops-install-{gnome,kde,cosmic} (docs/nixos/desktops.md §3.4, §4, §7, §8.1): `rime install <desktop>` on
# a running Rime machine, from the release it runs, live; a login through rime-greet into that desktop; and
# `rime remove <desktop>` logged in (next restart) and logged out (live).
#
# server  the release stand-in of checks.update-variants: an OCI registry and RustFS (the R2 stand-in), published with
#         THROWAWAY keys by fixture-publish.py (nix/release/fixture-publish.nix) through update-variants-publish.py
#         (a release naming several variants). Not checks.update-e2e's real pipeline: what this test proves (the
#         same-release path, the desktop in its session) does not depend on how the objects were made, and the real
#         pipeline is proven there. Two releases:
#           2026.11.10  plain = the client's own system (A), plain-<desktop> = A with the desktop switched on
#           2026.11.11  the same two plus one marker file: NEWER, on the channel from before the install. The install
#                       must not take it (a same-release install never reads the channel; §3.4 step 5).
# client  boots A from its own disk (systemd-boot, as checks.update-e2e) with Rime's desktop (rime-greet, Hyprland,
#         Rime Shell) and the account rime/rime, on a 24 GiB disk (above the desktop variant's floor).
#
# The greeter enumerates sessions when it starts; with nobody logged in, `systemctl restart greetd` is what a logout
# does to it. Which session it preselects is the greeter's own memory, /var/lib/rime-greet/last-session, written here
# as the greeter writes it after a login of that session (GreetContext.qml); the password is typed into the greeter.
#
# Mutants (passthru.mutants.<name>; each must FAIL):
#   gnome  same-release-channel  rime's same-release path reads the channel (installs 2026.11.11's system)
#          udevd-not-safe        systemd-udevd.service out of activation.rs RESTART_SAFE_UNITS (install at next restart)
#          greeter-env           rime-greet launches without the session environment (GreetContext.qml, bind-mounted
#                                over the store file inside the VM: XDG_CURRENT_DESKTOP never reaches GNOME)
#          blueman-override      the blueman autostart override removed (blueman-applet starts in GNOME)
#   kde    kwallet-pam-login     pam_kwallet back in `login` (greetd's stack): ksecretd starts in a Hyprland login
#          (kwalletrc's secrets API on is checks.desktops-eval's: with pam_kwallet out of `login` nothing starts
#          ksecretd before gnome-keyring holds the name, so this test passes either way: mut-kde-kwallet-secrets-3)
#   cosmic system76-power        system76-power on (and the configuration's assertions dropped so it evaluates)
{ pkgs, self }:

let
  lib = pkgs.lib;
  keys = pkgs.callPackage ../release/fixture-keys.nix { };
  repo = "server:5000/rime/os";
  publisher = pkgs.callPackage ../release/fixture-publish.nix { inherit keys repo; };
  s3key = "rimetestkey";
  s3secret = "rimetestsecret-TEST-only";
  table = import ../desktops;

  variantsPublish = pkgs.runCommand "rime-variants-publish-TEST" { nativeBuildInputs = [ pkgs.makeWrapper ]; } ''
    mkdir -p $out/bin
    makeWrapper ${pkgs.python3}/bin/python3 $out/bin/rime-variants-publish \
      --add-flags "-I ${./update-variants-publish.py} ${publisher}/share/fixture-publish.py" \
      --prefix PATH : ${lib.makeBinPath [ pkgs.skopeo pkgs.openssl pkgs.nix pkgs.rustfs-cli ]} \
      --set FIXTURE_KEYS ${keys} \
      --set FIXTURE_REPO ${repo} \
      --set FIXTURE_CACHE /srv/rime-cache \
      --set FIXTURE_BUCKET local/rime-cache
  '';

  # What each desktop's session looks like from outside.
  desktops = {
    gnome = { session = "gnome"; process = "gnome-shell"; xdg = "GNOME"; };
    kde = { session = "plasma"; process = "plasmashell"; xdg = "KDE"; };
    cosmic = { session = "cosmic"; process = "cosmic-comp"; xdg = "COSMIC"; };
  };

  # rimed built with one line of its source changed (no tests: the mutant is the source, not its suite).
  rimedMutant =
    rimed: name: file: from: to:
    rimed.overrideAttrs (o: {
      pname = "rimed-mutant-${name}";
      doCheck = false;
      postPatch = (o.postPatch or "") + ''
        grep -F ${lib.escapeShellArg from} ${file} >/dev/null
        substituteInPlace ${file} --replace-fail ${lib.escapeShellArg from} ${lib.escapeShellArg to}
      '';
    });

  mkTest =
    id: mutant:
    let
      d = desktops.${id} // { inherit (table.${id}) title; };
      # The mutant, as a module of the client: every release's systems carry it (extendModules keeps it).
      mutantModule =
        { lib, pkgs, ... }:
        lib.mkMerge [
          (lib.mkIf (mutant == "same-release-channel") {
            rime.services.rimed.package = rimedMutant pkgs.rime.rimed mutant "rimed/rime/src/nixos/update.rs"
              "let (digest, rel_id) = match same {" "let (digest, rel_id) = match same.filter(|_| false) {";
          })
          (lib.mkIf (mutant == "udevd-not-safe") {
            rime.services.rimed.package = rimedMutant pkgs.rime.rimed mutant "rimed/rimed-core/src/nixrel/activation.rs"
              "\"polkit.service\",\n    \"systemd-udevd.service\",\n" "\"polkit.service\",\n";
          })
          (lib.mkIf (mutant == "blueman-override") {
            environment.etc."xdg/autostart/blueman.desktop".enable = lib.mkForce false;
          })
          (lib.mkIf (mutant == "kwallet-pam-login") {
            security.pam.services.login.kwallet.enable = lib.mkOverride 10 true;
          })
          (lib.mkIf (mutant == "system76-power") {
            hardware.system76.power-daemon.enable = lib.mkOverride 10 true;
            assertions = lib.mkForce [ ];
          })
        ];
    in
    pkgs.testers.runNixOSTest {
      name = "rime-desktops-install-${id}${lib.optionalString (mutant != "") "-mutant-${mutant}"}";
      node.pkgsReadOnly = false;
      globalTimeout = 3 * 3600;

      nodes.server =
        { nodes, pkgs, ... }:
        {
          nix.enable = true;
          nix.settings.experimental-features = [ "nix-command" ];
          services.dockerRegistry = {
            enable = true;
            listenAddress = "0.0.0.0";
            openFirewall = true;
          };
          services.rustfs = {
            enable = true;
            environmentFile = toString (
              pkgs.writeText "rustfs-TEST.env" ''
                RUSTFS_ACCESS_KEY=${s3key}
                RUSTFS_SECRET_KEY=${s3secret}
              ''
            );
          };
          networking.firewall.allowedTCPPorts = [ 9000 ];
          virtualisation.containers.enable = true; # /etc/containers/policy.json for skopeo
          environment.systemPackages = [ publisher variantsPublish pkgs.rustfs-cli ];
          # The release job's output: both releases' systems are valid in the server's store.
          system.extraDependencies = builtins.attrValues nodes.client.system.build.rimeReleases;
          virtualisation.memorySize = 4096;
          virtualisation.cores = 4;
          virtualisation.diskSize = 20480;
        };

      nodes.client =
        { config, lib, pkgs, extendModules, ... }:
        let
          ext = m: (extendModules { modules = [ m ]; }).config.system.build.toplevel;
          on = { rime.desktops.${id}.enable = true; };
          newer = { environment.etc."rime-test-release".text = "2026.11.11\n"; };
        in
        {
          imports = [ self.nixosModules.default mutantModule ];

          system.build.rimeReleases = {
            A = config.system.build.toplevel;
            AD = ext on;
            B = ext newer;
            BD = ext { imports = [ on newer ]; };
          };

          # The update-e2e shape: a real boot menu on a real disk, so an install for the next restart writes one.
          virtualisation.useBootLoader = true;
          boot.growPartition = true;
          virtualisation.useEFIBoot = true;
          boot.loader.systemd-boot.enable = true;
          boot.loader.efi.canTouchEfiVariables = true;
          system.switch.enable = true;
          nix.enable = true;
          environment.etc."machine-id".text = "a7c1f00dba5e4f6a9d3c2b1e0f9a8b7c\n";
          virtualisation.memorySize = 4096;
          virtualisation.cores = 4;
          virtualisation.diskSize = 24576;
          virtualisation.containers.registries.settings.registry = [ { location = "server:5000"; insecure = true; } ];
          # virtio-gpu: a KMS device Mesa's software rasteriser drives (checks.desktop).
          virtualisation.qemu.options = [ "-vga none" "-device virtio-gpu-pci,xres=1280,yres=800" ];
          # powerprofilesctl, the client GNOME and KDE use, run by store path: power-profiles-daemon itself is never in
          # a Rime system (desktops.nix asserts it), so it is on the disk, not in the system.
          virtualisation.additionalPaths = [ pkgs.power-profiles-daemon ];

          rime.services.enable = true;
          rime.catalogue.enable = true;
          rime.desktop.enable = true;
          rime.update = {
            repository = repo;
            cacheUrl = "http://server:9000/rime-cache";
            fulcioRoot = "${keys}/ca.pem";
          };

          users.users.rime = {
            isNormalUser = true;
            uid = 1000;
            initialPassword = "rime";
            extraGroups = [ "wheel" "video" "audio" "networkmanager" ];
          };
          # checks.desktop's VM graphics knobs: sway (the greeter's host) renders with pixman, Qt Quick into shared
          # memory. Plasma reads its own environment scripts and gets its normal Qt Quick back.
          environment.sessionVariables = {
            WLR_RENDERER = "pixman";
            QT_QUICK_BACKEND = "software";
          };
          environment.etc."xdg/plasma-workspace/env/rime-test-qtquick.sh".text = "unset QT_QUICK_BACKEND\n";
          # No network in the test VM: the Flathub oneshot would only retry.
          systemd.services.rime-flathub-setup.enable = lib.mkForce false;
        };


      testScript =
        { nodes, ... }:
        let
          r = nodes.client.system.build.rimeReleases;
        in
        ''
          import shlex

          A, AD, B, BD = "${r.A}", "${r.AD}", "${r.B}", "${r.BD}"
          ID, NAME, TITLE = "${id}", "${id}", "${d.title}"
          SESSION, PROCESS, XDG = "${d.session}", "${d.process}", "${d.xdg}"
          MUTANT = "${mutant}"
          PPCTL = "${pkgs.power-profiles-daemon}/bin/powerprofilesctl"
          PPD = "org.freedesktop.UPower.PowerProfiles"
          PPD_PATH = "/org/freedesktop/UPower/PowerProfiles"

          def link(p):
              return client.succeed(f"readlink -f {p}").strip()

          def as_rime(cmd):
              return "su - rime -c " + shlex.quote("export XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus; " + cmd)

          def ppd_get(prop):
              return client.succeed(f"busctl get-property {PPD} {PPD_PATH} {PPD} {prop}").strip()

          def user_processes():
              return client.succeed("ps -u rime -o args= || true")

          def greeter_up():
              client.wait_until_succeeds("pgrep -u greetd -f 'qs -p .*/rime-greet/shell.qml'", timeout=120)

          def greeter_session(session):
              """The greeter's own memory of the last session, then a restart: what a logout shows."""
              client.succeed(
                  f"printf %s {session} > /var/lib/rime-greet/last-session",
                  "chown --reference=/var/lib/rime-greet /var/lib/rime-greet/last-session",
                  "systemctl restart greetd",
              )
              greeter_up()
              client.sleep(8)

          def login():
              client.send_chars("rime\n")  # username (no last user remembered: focus starts there)
              client.sleep(2)
              client.send_chars("rime\n")  # password

          def logout():
              client.succeed("loginctl terminate-user rime")
              client.wait_until_succeeds("! loginctl list-sessions --no-legend | awk '$3==\"rime\"' | grep .", timeout=60)
              client.wait_until_fails("pgrep -u rime", timeout=60)

          def user_session():
              """logind's record of rime's graphical session (greetd's; `su - rime` makes none of its own)."""
              for s in client.succeed("loginctl list-sessions --no-legend | awk '$3==\"rime\"{print $1}'").split():
                  out = client.succeed(f"loginctl show-session {s} -p Class -p Type -p Desktop -p Service")
                  props = dict(l.split("=", 1) for l in out.splitlines() if "=" in l)
                  if props.get("Class") == "user" and props.get("Service") == "greetd":
                      return props
              raise Exception("no greetd session of rime")

          def running(proc):
              """pgrep for the user's PROC by argv[0]: Nix wraps some binaries (gnome-shell runs as .gnome-shell-wrapped)."""
              return f"pgrep -u rime -f -- '^([^ ]*/)?\\.?{proc}(-wrapped)?( |$)'"

          def environ_of(proc):
              pid = client.succeed(running(proc) + " | head -n1").strip()
              return dict(l.split("=", 1) for l in client.succeed(f"tr '\\0' '\\n' < /proc/{pid}/environ").splitlines() if "=" in l)

          def secrets_owner():
              out = client.succeed(as_rime("busctl --user status org.freedesktop.secrets 2>&1 | grep -E '^(PID|Comm|CommandLine)=' || true"))
              print("org.freedesktop.secrets:", out)
              # The bootc failure this guards against: a second daemon owning the name with the login keyring locked.
              locked = client.succeed(as_rime(
                  "busctl --user get-property org.freedesktop.secrets /org/freedesktop/secrets/collection/login"
                  " org.freedesktop.Secret.Collection Locked"
              )).strip()
              assert locked == "b false", f"the login keyring behind org.freedesktop.secrets is locked: {locked}"
              return out

          start_all()
          server.wait_for_unit("docker-registry.service")
          server.wait_for_unit("rustfs.service")
          server.wait_for_open_port(9000)
          server.succeed("rc alias set local http://127.0.0.1:9000 ${s3key} ${s3secret}")
          server.succeed("rc bucket create local/rime-cache")
          server.succeed("rc anonymous set download local/rime-cache")
          client.wait_for_unit("multi-user.target")
          client.wait_for_unit("growpart.service")
          client.succeed("resize2fs /dev/disk/by-label/nixos")
          assert link("/run/current-system") == A

          with subtest("release 2026.11.10 (plain + plain-" + ID + "): the machine runs its plain system and records it"):
              server.succeed(f"rime-fixture-publish cache {A} {AD} {B} {BD}", timeout=7200)
              r1 = server.succeed(f"rime-variants-publish --release 2026.11.10 --variant plain={A} --variant plain-{ID}={AD}").strip()
              server.succeed(f"rime-fixture-publish channel --serial 1 --entry edge={r1}=2026.11.10")
              out = client.succeed("rime update --skip-firmware 2>&1")
              print(out)
              assert "up to date" in out, out
              client.succeed("rime trust --gate")

          with subtest("release 2026.11.11 is on the channel: the checker and rimed's Updates interface say so"):
              r2 = server.succeed(f"rime-variants-publish --release 2026.11.11 --variant plain={B} --variant plain-{ID}={BD}").strip()
              server.succeed(f"rime-fixture-publish channel --serial 2 --entry edge={r2}=2026.11.11")
              client.succeed("systemctl start rime-update-check.service")
              st = client.succeed("cat /run/rime-update/state")
              print(st)
              assert "state=available" in st and "release=2026.11.11" in st and "running=2026.11.10" in st, st
              upd = "busctl get-property org.rimeos.Rimed1 /org/rimeos/Rimed1 org.rimeos.Rimed1.Updates"
              client.wait_until_succeeds(f"{upd} State | grep -x 's \"available\"'", timeout=30)
              print(client.succeed(f"{upd} Available Running Message"))

          with subtest("the greeter, before: Rime's sessions only"):
              client.wait_for_unit("greetd.service")
              greeter_up()
              client.fail(f"test -e /etc/rime/wayland-sessions/{SESSION}.desktop")
              client.sleep(8)
              client.screenshot("greeter-before")

          def install_desktop():
              """rime install <desktop>: live, from the release the machine runs, not the newer one."""
              booted = link("/run/booted-system")
              out = client.succeed(f"rime install {NAME} 2>&1")
              print(out)
              assert f"{TITLE} is installed. Log out and choose {TITLE} at the login screen." in out, out
              assert link("/run/current-system") == AD, f"running {link('/run/current-system')}, not 2026.11.10's plain-{ID}"
              assert link("/run/booted-system") == booted, "the machine restarted"
              assert link("/nix/var/nix/profiles/system") == AD
              client.succeed(f"test -e /etc/rime/wayland-sessions/{SESSION}.desktop")
              # 2026.11.11 is still offered: nothing installed it.
              client.succeed("systemctl start rime-update-check.service")
              assert "state=available" in client.succeed("cat /run/rime-update/state")
              print(client.succeed("systemctl list-units --type=service --state=active --no-legend --plain | awk '{print $1}' | sort"))

          if ID != "kde":
              with subtest(f"rime install {NAME}: live, from the release the machine runs, not the newer one"):
                  install_desktop()

          if MUTANT == "greeter-env":
              # rime-greet without the environment it hands greetd (the launch without `e`).
              f = link("/run/current-system/sw/share/rime-greet/GreetContext.qml")
              client.succeed(
                  f"cp {f} /run/GreetContext-mutant.qml",
                  "sed -i 's/if (e) Greetd.launch(a, e)/if (e) Greetd.launch(a)/' /run/GreetContext-mutant.qml",
                  "grep -F 'if (e) Greetd.launch(a)' /run/GreetContext-mutant.qml",
                  f"mount --bind /run/GreetContext-mutant.qml {f}",
              )
        ''
        + lib.optionalString (id == "kde") ''

          def hyprland_login():
              greeter_session("hyprland")
              login()
              client.wait_until_succeeds("ls /run/user/1000/hypr/*/.socket.sock", timeout=180)
              client.sleep(15)

          def activation_unit():
              return client.succeed(as_rime("systemctl --user show nixos-activation.service -p ActiveState -p ActiveEnterTimestampMonotonic -p NRestarts -p InvocationID || true"))

          with subtest("rime install kde while logged in to a Rime session: live, from the release the machine runs"):
              # Rime's Hyprland config, as rime-shell-firstrun seeds it at a real first login (checks.desktop).
              client.succeed("install -d -o rime -g users /home/rime/.config /home/rime/.config/hypr")
              client.succeed("cp -r /run/current-system/sw/share/rime/hypr/. /home/rime/.config/hypr/")
              client.succeed("grep -rl '@KB_' /home/rime/.config/hypr | xargs -r sed -i -e 's|@KB_LAYOUT@|us|g' -e 's|@KB_VARIANT@||g'")
              client.succeed("chown -R rime:users /home/rime/.config/hypr")
              hyprland_login()
              before_unit = activation_unit()
              install_desktop()
              after_unit = activation_unit()
              # §3.5: KDE changes the user unit nixos-activation.service. Measured here with a user manager running
              # during the live switch; the as-built notes record the answer.
              print("nixos-activation.service (user) before the switch:", before_unit)
              print("nixos-activation.service (user) after the switch:", after_unit)
              print(client.succeed("journalctl -b --no-pager -o cat _UID=1000 | grep -i 'nixos-activation' | tail -n 20 || true"))
              client.succeed("ls /run/user/1000/hypr/*/.socket.sock")  # the Rime session survived the switch
              logout()

          with subtest("a Hyprland login on the KDE system: no kwalletd, gnome-keyring the one secrets service"):
              hyprland_login()
              procs = user_processes()
              print(procs)
              # KWallet 6.31's daemon is ksecretd (kwalletd before it); pam_kwallet at the greeter would start it.
              for bad in ("kwalletd", "ksecretd"):
                  assert bad not in procs, f"{bad} runs in a Rime session"
              owner = secrets_owner()
              assert "gnome-keyring" in owner and "kwallet" not in owner, owner
              # rime-greet hands a Rime session XDG_SESSION_TYPE only: logind's Desktop stays empty (§4.1).
              sess = user_session()
              print(sess)
              assert sess.get("Type") == "wayland" and sess.get("Desktop") == "", sess
              client.screenshot("hyprland-on-kde-system")
              logout()
        ''
        + ''

          with subtest(f"the greeter lists {TITLE}; a login through it starts {PROCESS}"):
              greeter_session(SESSION)
              client.screenshot(f"greeter-{ID}")
              login()
              client.wait_until_succeeds(running(PROCESS), timeout=300)
              client.sleep(30)  # autostart entries, the keyring, the panel
              client.screenshot(f"{ID}-session")

          with subtest(f"in {TITLE}: its own environment, none of Rime's session programs, one secrets service"):
              env = environ_of(PROCESS)
              print({k: v for k, v in env.items() if k.startswith("XDG_")})
              assert env.get("XDG_CURRENT_DESKTOP", "").split(":")[0] == XDG, env.get("XDG_CURRENT_DESKTOP")
              assert env.get("XDG_SESSION_TYPE") == "wayland", env.get("XDG_SESSION_TYPE")
              sess = user_session()
              print(sess)
              assert sess.get("Type") == "wayland" and sess.get("Desktop") == SESSION, sess
              procs = user_processes()
              print(procs)
              for bad in ("quickshell", "/rime-shell/", "qs -p", "polkit-mate-authentication-agent", "blueman-applet",
                          "hypridle", "ibus-daemon", "kwalletd"):
                  assert bad not in procs, f"{bad} runs in the {TITLE} session"
              allp = client.succeed("ps -eo args=")
              for bad in ("power-profiles-daemon", "system76-power"):
                  assert bad not in allp, f"{bad} runs"
              owner = secrets_owner()
              assert "gnome-keyring" in owner and "kwallet" not in owner and "oo7" not in owner, owner
              print(client.succeed("systemd-inhibit --list --no-pager 2>&1 || true"))
        ''
        + lib.optionalString (id == "gnome") ''

          # Recorded here; the lock itself is checks.desktop-lock-gnome's (desktops.md §12.4): GNOME Shell creates its
          # screen shield only when org.gnome.DisplayManager answers, and rime-gnome-unlock owns that name.
          print(client.succeed("busctl status org.gnome.DisplayManager 2>&1 || true"))

          with subtest("power profiles: rimed's tier is the profile GNOME reads; powerprofilesctl drives rimed"):
              client.succeed("rime tier performance")
              assert ppd_get("ActiveProfile") == 's "performance"', ppd_get("ActiveProfile")
              # GNOME's own reading, from inside the session (its client is the same system-bus API).
              assert "performance" in client.succeed(as_rime(f"gdbus call --system --dest {PPD} --object-path {PPD_PATH} --method org.freedesktop.DBus.Properties.Get {PPD} ActiveProfile"))
              print(client.succeed(f"{PPCTL} list"))
              assert client.succeed(f"{PPCTL} get").strip() == "performance"
              client.succeed(f"{PPCTL} set balanced")
              assert client.succeed(f"{PPCTL} get").strip() == "balanced"
              assert "* balanced" in client.succeed("rime tier 2>&1")  # the chosen tier is starred
              client.succeed(f"systemd-run --unit=rime-test-launch {PPCTL} launch -p performance -r rime-test -i org.rimeos.Test /run/current-system/sw/bin/sleep 8")
              client.wait_until_succeeds(f"busctl get-property {PPD} {PPD_PATH} {PPD} ActiveProfileHolds | grep org.rimeos.Test", timeout=10)
              assert ppd_get("ActiveProfile") == 's "performance"', ppd_get("ActiveProfile")
              client.wait_until_succeeds(f"busctl get-property {PPD} {PPD_PATH} {PPD} ActiveProfileHolds | grep -x 'aa{{sv}} 0'", timeout=30)
              assert ppd_get("ActiveProfile") == 's "balanced"', ppd_get("ActiveProfile")
              print(client.succeed("systemd-inhibit --list --no-pager 2>&1 || true"))
        ''
        + lib.optionalString (id == "kde") ''

          with subtest("power profiles: powerdevil reports rimed's profile"):
              client.succeed("rime tier performance")
              pd = "busctl --user call org.kde.Solid.PowerManagement /org/kde/Solid/PowerManagement/Actions/PowerProfile org.kde.Solid.PowerManagement.Actions.PowerProfile currentProfile"
              client.wait_until_succeeds(as_rime(pd) + " | grep -F performance", timeout=60)
              client.succeed("rime tier power-saver")
              client.wait_until_succeeds(as_rime(pd) + " | grep -F power-saver", timeout=60)
              print(client.succeed(as_rime(pd.replace("currentProfile", "profileChoices"))))
        ''
        + lib.optionalString (id == "cosmic") ''

          with subtest("power profiles: no System76 power daemon; COSMIC reads rimed's profile"):
              client.fail("busctl status com.system76.PowerDaemon")
              client.succeed("rime tier power-saver")
              assert ppd_get("ActiveProfile") == 's "power-saver"', ppd_get("ActiveProfile")
              client.sleep(5)
              client.screenshot("cosmic-power-saver")
              client.succeed("rime tier performance")
              client.sleep(5)
              client.screenshot("cosmic-performance")
              # Who asked rimed's power-profiles API since the login: the COSMIC battery applet among them.
              print(client.succeed("journalctl -b -u rimed --no-pager -o cat | tail -n 30"))
        ''
        + ''

          with subtest(f"rime remove {NAME} while logged in to it: installed for the next restart only"):
              out = client.succeed(f"rime remove {NAME} 2>&1")
              print(out)
              assert f"session is open, so {TITLE} is removed at the next restart" in out, out
              assert link("/run/current-system") == AD, "the open session's system was switched away"
              assert link("/nix/var/nix/profiles/system") == A
              client.succeed(running(PROCESS))

          with subtest(f"logged out, rime remove {NAME} is live: the session and the desktop's services are gone"):
              logout()
              out = client.succeed(f"rime remove {NAME} 2>&1")
              print(out)
              assert f"{TITLE} is removed." in out, out
              assert link("/run/current-system") == A
              client.fail(f"test -e /etc/rime/wayland-sessions/{SESSION}.desktop")
              assert client.succeed("systemctl show -P ActiveState accounts-daemon.service").strip() != "active"
              # The remembered session is gone: the greeter falls back to Rime's.
              greeter_session(SESSION)
              client.screenshot("greeter-after-remove")
        '';
    };
in
lib.mapAttrs' (
  id: muts:
  lib.nameValuePair "desktops-install-${id}" (mkTest id "" // { mutants = lib.genAttrs muts (mkTest id); })
) {
  gnome = [ "same-release-channel" "udevd-not-safe" "greeter-env" "blueman-override" ];
  kde = [ "kwallet-pam-login" ];
  cosmic = [ "system76-power" ];
}
