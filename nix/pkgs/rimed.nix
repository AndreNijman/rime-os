# The rimed Cargo workspace: rimed, rime, rime-agentd, rime-aid, rime-secretd, rime-remoted, rime-kernel-btf-gate.
#
# Paths. The bootc image runs a few programs by fixed absolute path on purpose (bwrap is the sandbox boundary, the
# rime-* engines do privileged work) and those paths live in ONE crate, rimed/rime-paths, chosen at compile time from
# RIME_PATH_<NAME> (PLAN.md 6.2 PR 8). The bootc build sets none of them and gets today's /usr paths; this derivation
# sets every one (RIME_PATHS_EXPECT_ALL_SET makes rime-paths' own test fail if one is missed). Three kinds of value:
#   - a program Rime always needs: a nixpkgs store path (cannot be shadowed, cannot go missing);
#   - a program that is an optional part of the system (flatpak, fwupd, scx, the bootc-only tools): its place under
#     /run/current-system/sw/bin, root-owned and fixed, present only when the system has it, as on bootc;
#   - a sibling Rime package (the libexec engines, Rime Shell): /run/current-system/sw/{libexec,share}. Not a store
#     path, because rime-libexec's scripts call `rime` and a store path both ways would be a dependency cycle; the
#     services module links /libexec and /share/rime-shell into the system path. Still fixed, still root-owned,
#     still never a PATH lookup.
#
# Tests. checkPhase runs `cargo test --workspace --no-fail-fast`. The tests that cannot pass in the Nix build sandbox
# are skipped BY NAME in `skips` below, each group with its reason; everything else must pass.
{
  lib,
  rustPlatform,
  runtimeShell,
  bubblewrap,
  cacert,
  coreutils,
  curl,
  git,
  openssh,
  openssl,
  smartmontools,
  systemd,
  util-linux,
  # Where the rime-* engines are on a NixOS system (rime-libexec's $out/libexec, linked by the services module).
  libexecDir ? "/run/current-system/sw/libexec",
  # Where optional system programs are.
  systemBinDir ? "/run/current-system/sw/bin",
  # Where Rime Shell's QML tree is (rime-shell's $out/share/rime-shell, linked by the services module).
  shellDir ? "/run/current-system/sw/share/rime-shell",
}:

let
  root = ../..;
  out = placeholder "out";
  exe = pkg: name: "${lib.getBin pkg}/bin/${name}";
  sys = name: "${systemBinDir}/${name}";
  engine = name: "${libexecDir}/${name}";

  # Every constant in rimed/rime-paths/src/lib.rs. rime-paths' tests fail the build if one is missing or left
  # under /usr.
  paths = {
    BWRAP = exe bubblewrap "bwrap";
    SYSTEMD_RUN = exe systemd "systemd-run";
    SYSTEMCTL = exe systemd "systemctl";
    BOOTCTL = exe systemd "bootctl";
    CURL = exe curl "curl";
    OPENSSL = exe openssl "openssl";
    GIT = exe git "git";
    SSH = exe openssh "ssh";
    SMARTCTL = exe smartmontools "smartctl";
    WIPEFS = exe util-linux "wipefs";
    # Optional parts of the system: present only if the system configuration installs them.
    SKOPEO = sys "skopeo";
    RPM_OSTREE = sys "rpm-ostree";
    FWUPDMGR = sys "fwupdmgr";
    FLATPAK = sys "flatpak";
    SCXCTL = sys "scxctl";
    SCXCTL_ALT = sys "scxctl";

    RIME = "${out}/bin/rime";
    RIME_AID = "${out}/bin/rime-aid";

    RIME_PKG = engine "rime-pkg";
    RIME_ENV = engine "rime-env";
    RIME_DISPOSABLE = engine "rime-disposable";
    RIME_USER = engine "rime-user";
    RIME_VM = engine "rime-vm";
    RIME_BROWSER = engine "rime-browser";
    RIME_PLUGIN = engine "rime-plugin";
    RIME_FIREWALL = engine "rime-firewall";
    RIME_DEVICES = engine "rime-devices";
    RIME_BOOT_MIGRATE = engine "rime-boot-migrate";
    RIME_LIVE_SESSION = engine "rime-live-session";
    RIME_PROJECT_WINDOWS = engine "rime-project-windows";
    RIME_MUX = engine "rime-mux";
    RIME_SESSION_SELECT = engine "rime-session-select";
    RIME_SHELL_FIRSTRUN = engine "rime-shell-firstrun";
    RIME_TOOLS_DIR = engine "rime/tools";

    SYSPROFILES_DIR = "${out}/share/rimeos/sysprofiles";
    AI_CATALOGUE = "${out}/share/rimeos/ai/catalogue.toml";
    TRUST_FULCIO_ROOT = "${out}/share/rime-os/trust/fulcio-root.pem";
    # Override files: absent unless a later build ships one, exactly as on bootc.
    TRUST_EXPECTED_SIGNER = "${out}/share/rime-os/trust/expected-signer";
    TRUST_EXPECTED_ROLLOUT_SIGNER = "${out}/share/rime-os/trust/expected-rollout-signer";
    TRUST_EXPECTED_ISSUER = "${out}/share/rime-os/trust/expected-issuer";
    TRUST_ENFORCEMENT = "${out}/share/rime-os/trust/enforcement.conf";
    SHELL_DIR = shellDir;
    SHELL_QML = "${shellDir}/shell.qml";

    SYSTEM_BIN_DIR = systemBinDir;
    LOCAL_BIN_DIR = systemBinDir;
    OS_RELEASE_FALLBACK = "/etc/os-release";
    RESCUE_TARGET_UNIT = "/etc/systemd/system/rescue.target";
    LIBCUDA = "/run/opengl-driver/lib/libcuda.so.1";
    VULKAN_ICD_DIR = "/run/opengl-driver/share/vulkan/icd.d";

    # The secret broker runs `timeout git ...` through this PATH: both by store path first, since a NixOS system
    # need not have git installed, then the system.
    SAFE_PATH = "${lib.getBin git}/bin:${coreutils}/bin:/run/wrappers/bin:${systemBinDir}";
    MINIMAL_PATH = "/run/wrappers/bin:${systemBinDir}";
    # TODO(handover): the rime-aid backend sandbox binds only /usr and /etc (rime-aid/src/runtime.rs), so on NixOS a
    # backend cannot start inside it until the sandbox binds /nix/store and /run/opengl-driver. PLAN.md 4.3.
    AID_SANDBOX_PATH = systemBinDir;
  };

  # The PATH rime-agentd hands every agent session: the user's own directories first (as the bootc unit has them),
  # then the Nix profile locations a NixOS login shell has, then the system.
  agentdPath = lib.concatStringsSep ":" [
    "%h/.local/bin"
    "%h/bin"
    "%h/.cargo/bin"
    "%h/.nix-profile/bin"
    "/etc/profiles/per-user/%u/bin"
    "/nix/var/nix/profiles/default/bin"
    "/run/wrappers/bin"
    "${systemBinDir}"
  ];

  # Tests that cannot pass in the Nix build sandbox, by full name (cargo's --skip is a substring match, so each
  # entry is the whole module path), grouped by the reason. Everything else runs and must pass.
  skips = {
    # rime-agentd decides who a client is from the cgroup in /proc/<pid>/cgroup, and refuses one that is in neither
    # a login session nor a user service. Every process in a build sandbox is in "/", so these sessions are refused
    # before the behaviour under test. They run on a real login (bootc CI, the NixOS VM test exercises the daemon).
    needsLoginSessionCgroup = [
      "an_unconfined_session_is_refused_rather_than_given_a_field_that_does_nothing"
      "a_pem_carrying_a_private_key_starts_no_session_at_all"
      "a_budget_of_zero_is_refused_before_anything_is_started"
      "a_budget_refusal_is_a_policy_answer_and_not_a_bad_request"
      "a_budget_with_no_user_manager_refuses_and_names_the_path"
      "an_unparseable_config_file_does_not_pretend_there_is_no_budget"
      "an_unbudgeted_session_runs_in_the_daemons_own_cgroup"
      "a_mistyped_budget_key_refuses_rather_than_starting_the_session_unbudgeted"
      "a_wedged_application_cannot_hold_the_connection"
      "the_verb_is_reachable_and_the_selection_comes_back_verbatim"
      "a_clipboard_holding_a_picture_is_refused_and_its_bytes_are_never_read"
      "a_machine_with_no_compositor_says_so_rather_than_reporting_an_empty_clipboard"
      "a_clipboard_past_the_cap_is_refused_with_the_limit_named"
      "an_empty_clipboard_is_an_answer_and_never_an_error"
      "a_hook_inside_a_session_reaches_the_daemon_that_started_it"
      "the_daemon_hands_the_agent_a_curated_configuration_and_the_flag_that_makes_it_exclusive"
      "a_session_with_no_connectors_is_handed_an_empty_set_rather_than_no_file"
      "a_name_the_validator_refuses_starts_nothing"
      "the_three_ways_a_write_can_fail_come_back_as_three_different_kinds"
      "renewing_a_grant_that_does_not_exist_is_refused_without_asking_anybody"
      "a_ttl_is_refused_before_anything_is_created_or_anybody_is_asked"
      "a_ttl_on_a_session_that_asked_for_no_grant_is_refused_rather_than_ignored"
      "a_project_with_no_binding_starts_the_agent_it_was_asked_for"
      "a_session_that_names_no_agent_gets_the_bound_one_and_not_the_users_default"
      "a_session_naming_an_agent_this_project_did_not_bind_is_refused"
      "a_session_with_a_parent_death_signal_survives_its_client_hanging_up"
    ];
    # rime/tests/mcp_{launch,sidecar}_live.rs start real confined MCP servers that are host programs:
    # /usr/bin/python3, probe scripts run through /usr/bin:/bin, a run-user mount point created under the fixture
    # root after `/` is bound read-only. They test the bootc host's layout and run there; the confinement itself is
    # covered here by rime-agent-core's sandbox tests, which do run.
    needsFhsHost = [
      "a_plugins_server_launched_through_the_curated_config_cannot_reach_the_network"
      "a_plugins_server_launched_through_the_curated_config_cannot_read_the_users_home"
      "a_confined_server_cannot_read_the_home_the_agent_can"
      "a_confined_server_with_no_network_cannot_reach_a_listener_that_is_there"
      "a_variable_the_parent_has_does_not_reach_a_confined_server"
      "the_broker_dimension_opens_the_runtime_socket_and_only_when_asked"
      "the_shipped_command_runs_a_stdio_mcp_server_and_its_handshake_still_completes"
    ];
    # A known race in the test, not in the daemon (its own comment records one failure on a GitHub runner): another
    # test thread's fork can hold the dropped listener's fd until its exec, so the stale socket still accepts. It
    # passed in two of three sandboxed runs here and failed once while a kernel build loaded the machine.
    flaky = [ "tests::a_stale_socket_from_a_dead_daemon_is_replaced" ];
    # The negative control starts a child WITHOUT no_new_privs and checks the flag is clear. The build sandbox (and
    # bwrap) set no_new_privs on everything, so the control cannot exist here.
    needsPrivsUnset = [ "pty::tests::no_new_privs_reaches_the_session_and_the_negative_control_proves_it" ];
    # rime-remoted publishes over mDNS through a running avahi-daemon; the sandbox has no system bus and no network.
    needsAvahi = [
      "the_record_names_this_machine_this_port_and_nothing_that_identifies_it"
      "the_record_goes_away_when_the_daemon_is_killed_outright"
    ];
    # Needs a file owned by another user at /etc/hostname (root on any real machine); the sandbox has none and
    # cannot make one.
    needsForeignOwnedFile = [ "project::tests::a_file_the_owner_does_not_own_is_refused" ];
  };
in
rustPlatform.buildRustPackage {
  pname = "rimed";
  version = "0.1.0";

  src = lib.fileset.toSource {
    inherit root;
    fileset = lib.fileset.unions [
      (lib.fileset.difference (root + "/rimed") (lib.fileset.maybeMissing (root + "/rimed/target")))
      # include_str! targets: the system profiles, and (in tests) the firewall service catalogue.
      (root + "/config")
      (root + "/files/system/firewall/services")
      # Test fixtures shared with other parts of the repository (the Android client's wire vectors, the relay).
      (root + "/android/core/src/test/resources")
      (root + "/relay/src/room.js")
      # Installed next to the binaries: units, D-Bus policy, polkit actions, trust roots.
      (root + "/files/system/units/rime-agentd.service")
      (root + "/files/system/units/rime-aid.service")
      (root + "/files/system/units/rime-lid.service")
      (root + "/files/system/units/rime-remoted.service")
      (root + "/files/system/units/rime-secretd.service")
      (root + "/files/system/dbus-1")
      (root + "/files/system/polkit-1/actions")
      (root + "/files/system/trust")
    ];
  };

  cargoRoot = "rimed";
  buildAndTestSubdir = "rimed";
  cargoLock.lockFile = root + "/rimed/Cargo.lock";

  env = lib.mapAttrs' (name: value: lib.nameValuePair "RIME_PATH_${name}" value) paths // {
    RIME_PATHS_EXPECT_ALL_SET = "1";
  };

  # The whole workspace, every test binary even after one fails, so the log has every total.
  doCheck = true;
  cargoTestFlags = [ "--workspace" "--no-fail-fast" ];
  checkFlags = lib.concatMap (t: [ "--skip" t ]) (lib.concatLists (lib.attrValues skips));
  # Programs the tests run: by the store paths above (curl, git, bwrap, openssl) or through PATH.
  nativeCheckInputs = [ bubblewrap cacert curl git openssh openssl util-linux ];
  preCheck = ''
    # The sandbox's HOME is /homeless-shelter (absent) and there is no XDG_RUNTIME_DIR, so code that falls back to
    # /run/user/<uid> would try a directory that does not exist here.
    export HOME=$(mktemp -d) XDG_RUNTIME_DIR=$(mktemp -d)
    git config --global user.name rime-test
    git config --global user.email rime-test@localhost
    git config --global init.defaultBranch main
    # The relay and TLS tests read the system trust store.
    export SSL_CERT_FILE=${cacert}/etc/ssl/certs/ca-bundle.crt

    # The tests assume a root shaped like a running machine, which the build sandbox is not: their fixtures go under
    # /var/tmp (the project's rule for anything a suite creates), the agent sandbox puts a tmpfs over /run, and a few
    # fixtures exec /bin/true or look for setpriv in /bin. So `cargo test` runs under bwrap in a root that has
    # those, with every top-level directory of the build sandbox bound in unchanged. No /usr is provided: code that
    # still assumed one would fail here, as it would on NixOS.
    testRoot=$(mktemp -d)
    mkdir -p "$testRoot/var-tmp" "$testRoot/bin" "$testRoot/shim"
    ln -s ${runtimeShell} "$testRoot/bin/sh"
    ln -s ${coreutils}/bin/true "$testRoot/bin/true"
    ln -s ${lib.getBin util-linux}/bin/setpriv "$testRoot/bin/setpriv"
    bwrapArgs=(--dev-bind /dev /dev --bind /proc /proc --tmpfs /run --bind "$testRoot/bin" /bin
               --dir /var --bind "$testRoot/var-tmp" /var/tmp)
    for entry in /*; do
      case "$entry" in /bin|/dev|/proc|/run|/var) ;; *) bwrapArgs+=(--bind "$entry" "$entry") ;; esac
    done
    {
      echo "#!${runtimeShell}"
      printf 'exec %q' ${bubblewrap}/bin/bwrap; printf ' %q' "''${bwrapArgs[@]}"
      printf ' --chdir "$PWD" -- %q "$@"\n' "$(command -v cargo)"
    } > "$testRoot/shim/cargo"
    chmod +x "$testRoot/shim/cargo"
    export PATH="$testRoot/shim:$PATH"
  '';

  postInstall = ''
    share=$out/share
    install -Dm644 -t $share/rimeos/sysprofiles $src/config/sysprofiles/*.toml
    install -Dm644 $src/config/ai/catalogue.toml $share/rimeos/ai/catalogue.toml
    install -Dm644 -t $share/rime-os/trust $src/files/system/trust/fulcio-root.pem $src/files/system/trust/enforcement.conf

    # D-Bus: the bus policy as shipped, the activation file pointed at this rimed.
    install -Dm644 $src/files/system/dbus-1/system.d/org.rimeos.Rimed1.conf $share/dbus-1/system.d/org.rimeos.Rimed1.conf
    install -Dm644 $src/files/system/dbus-1/system-services/org.rimeos.Rimed1.service \
      $share/dbus-1/system-services/org.rimeos.Rimed1.service
    substituteInPlace $share/dbus-1/system-services/org.rimeos.Rimed1.service \
      --replace-fail 'Exec=/usr/bin/rimed' "Exec=$out/bin/rimed"

    # polkit actions (no exec.path annotations: nothing in them names a binary).
    install -Dm644 -t $share/polkit-1/actions $src/files/system/polkit-1/actions/*.policy

    # systemd units, byte for byte the bootc ones (hardening included) apart from the program paths. They keep their
    # [Install] sections, which NixOS units do not have, so `systemctl --user enable rime-agentd` (what
    # `rime agent enable` runs) works. services.nix enables the ones bootc enables.
    install -Dm644 $src/rimed/rimed/rimed.service $out/lib/systemd/system/rimed.service
    install -Dm644 -t $out/lib/systemd/system $src/files/system/units/rime-{secretd,lid}.service
    install -Dm644 -t $out/lib/systemd/user $src/files/system/units/rime-{agentd,aid,remoted}.service
    for unit in $out/lib/systemd/system/*.service $out/lib/systemd/user/*.service; do
      substituteInPlace "$unit" --replace-quiet '=/usr/bin/' "=$out/bin/" --replace-quiet '=-/usr/bin/' "=-$out/bin/"
    done
    substituteInPlace $out/lib/systemd/user/rime-agentd.service \
      --replace-fail 'Environment=PATH=%h/.local/bin:%h/bin:%h/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/bin' \
                     'Environment=PATH=${agentdPath}'

    # No directive installed may still name /usr: that would be a program a NixOS machine does not have. (Comments
    # may: they describe the bootc image the units come from.)
    if grep -rnE '^[^#]*/usr/' $out/lib/systemd $share/dbus-1/system-services; then
      echo "rimed: an installed unit or D-Bus file still names /usr (above)" >&2
      exit 1
    fi
  '';

  meta = {
    description = "Rime system daemon, CLI and helper daemons";
    homepage = "https://github.com/AndreNijman/rime-os";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
    mainProgram = "rime";
  };
}
