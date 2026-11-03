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
# are skipped BY NAME in `skips` below, each group with its reason; everything else must pass. postCheck writes the
# total per crate to $out/share/doc/rimed/cargo-tests.{txt,json} (rimed-test-report.py) and refuses a total that
# cannot be trusted: a skip name that matches no test, a test that passed by skipping itself, a lost test binary.
{
  lib,
  rustPlatform,
  runtimeShell,
  bubblewrap,
  cacert,
  coreutils,
  curl,
  git,
  help2man,
  libxml2,
  openssh,
  openssl,
  python3,
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
    # The rime-aid backend sandbox binds /nix/store, /run/current-system and /run/opengl-driver read-only
    # (rime-aid/src/runtime.rs NIX_SYSTEM_TREES), so this PATH resolves inside it.
    AID_SANDBOX_PATH = systemBinDir;
    # rime-aid's startup probe runs this inside a test sandbox; /usr/bin/true does not exist on NixOS.
    AID_PROBE_PROGRAM = "${coreutils}/bin/true";
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
    # before the behaviour under test. checks.rimed-session runs them inside a logged-in VM session.
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
    # The negative control starts a child WITHOUT no_new_privs and checks the flag is clear. The build sandbox (and
    # bwrap) set no_new_privs on everything, so the control cannot exist here.
    needsPrivsUnset = [ "pty::tests::no_new_privs_reaches_the_session_and_the_negative_control_proves_it" ];
    # Needs a file owned by another user at /etc/hostname (root on any real machine); the sandbox has none and
    # cannot make one.
    needsForeignOwnedFile = [ "project::tests::a_file_the_owner_does_not_own_is_refused" ];
  };
  # Not skipped any more, so not listed above: tests::a_stale_socket_from_a_dead_daemon_is_replaced (rime-secretd) was
  # a race in the test (another thread's fork held the dropped listener's fd); its dead daemon is now a process that
  # exits. rime-remoted's two discovery tests never needed avahi (they publish through a stub); the stub's
  # `#!/usr/bin/env bash` needed /usr/bin/env, which NixOS has and the test root below now has too.

  # Tests that cargo reports as PASSED but that returned early because their environment is missing (they print a
  # SKIP line), by the reason. The report counts them as "selfskip", not as passed, and refuses the build for a
  # self-skip not named here or a name here that no longer skips itself. Found 2026-11-01: before the report
  # existed, these 57 read as passes.
  selfSkipOk = {
    # The same cause as needsLoginSessionCgroup above: rime-agentd (and rime-remoted's pairing, which asks it) refuses
    # a client whose /proc/<pid>/cgroup is "/" ("neither a login session nor a user service"), and these tests catch
    # that refusal, print SKIP and return. checks.rimed-session runs them (and needsLoginSessionCgroup) from the same
    # test binaries inside a logged-in VM session, where each must pass without a SKIP line.
    noLoginSessionCgroup = [
      "a_channel_onto_a_session_that_does_not_exist_is_refused_with_the_daemons_own_words"
      "a_confined_session_is_judged_against_the_confinement_it_actually_has"
      "a_confined_session_is_told_where_its_runtime_directory_is"
      "a_declared_remote_origin_reaches_both_verbs"
      "a_device_pairs_over_the_real_socket_and_lands_in_the_real_store"
      "a_device_reaches_the_desktop_through_a_relay_neither_of_them_listens_on"
      "a_file_from_the_phone_crosses_in_frames_and_its_path_is_typed_into_the_session"
      "a_killed_session_has_nothing_still_running_under_it"
      "a_launch_that_fails_says_so_and_leaves_the_packet_behind"
      "a_name_given_at_run_is_on_the_session_the_list_and_the_record"
      "a_narrowed_session_runs_under_its_own_allowlist_and_not_the_runtimes"
      "a_network_change_costs_the_session_and_costs_nothing_else"
      "a_paired_device_reaches_the_agent_runtime_through_the_proxy"
      "a_payload_with_quotes_and_backslashes_arrives_unchanged"
      "a_phone_registers_for_push_over_the_real_session_and_the_key_lands_in_its_own_file"
      "a_phone_that_leaves_gives_the_desktop_its_size_back"
      "a_project_grant_is_reported_as_inherited_and_a_session_grant_is_not"
      "a_push_endpoint_the_machine_cannot_use_is_refused_and_nothing_is_stored"
      "a_relayed_session_is_recorded_as_relayed_and_a_direct_one_is_not"
      "a_request_from_a_phone_is_remote_and_cannot_approve_root"
      "a_second_device_cannot_reuse_the_first_ones_code"
      "a_session_cannot_ask_for_a_grant_or_renew_one_however_it_asks"
      "a_session_may_not_read_this_machines_clipboard"
      "a_session_reports_a_graph_even_when_it_has_delegated_nothing"
      "a_session_that_narrows_nothing_still_gets_the_whole_runtime_allowlist"
      "a_status_line_with_nothing_to_report_publishes_nothing"
      "a_subagent_start_and_stop_become_one_finished_child"
      "a_task_with_two_sessions_is_refused_and_both_ids_are_named"
      "a_terminal_streams_both_ways_over_one_channel"
      "a_user_with_no_status_line_gets_no_status_line"
      "an_agents_terminal_title_reaches_the_session_and_the_record"
      "an_empty_payload_is_accepted_and_writes_nothing"
      "an_unknown_target_is_refused_before_anything_is_written"
      "an_upload_is_refused_before_a_byte_of_it_is_sent"
      "changed_files_name_the_base_they_were_measured_against"
      "only_the_outgoing_sessions_system_grants_are_listed"
      "peek_reads_the_tail_without_attaching_or_resizing"
      "rename_names_a_live_session_clears_it_and_refuses_what_it_should"
      "revoking_a_device_also_stops_it_being_woken"
      "revoking_a_device_ends_the_connection_it_is_already_holding"
      "submit_turns_text_into_a_line_the_program_acts_on"
      "text_reaches_a_live_session_and_a_terminator_makes_it_a_line"
      "the_desktop_measures_the_connection_and_a_device_cannot_invent_the_answer"
      "the_end_of_a_turn_closes_a_subagent_whose_stop_never_arrived"
      "the_packet_is_written_into_the_project_and_stdout_is_its_path"
      "the_processes_a_session_forked_are_in_its_graph"
      "the_proxy_enforces_the_narrowed_list_and_not_the_runtimes"
      "the_relay_carries_the_session_and_can_read_none_of_it"
      "the_status_line_prints_the_users_own_and_publishes_what_it_read"
      "the_task_spelling_from_section_sixteen_writes_the_packet_for_the_tasks_session"
    ];
    # Opt-in by design (RIME_BUDGET_LIVE=1): they start a transient systemd scope through the user manager and
    # write real cgroup limits. The build sandbox has no user manager.
    liveBudgetOptIn = [
      "a_budgeted_session_runs_in_its_own_scope_with_the_limits_really_written"
      "a_wall_clock_budget_really_ends_the_session"
    ];
    # browser_ca::install refuses to run without /etc/firefox/policies/policies.json to copy (the bootc image ships
    # one); the build sandbox has no /etc/firefox.
    noFirefoxPolicy = [
      "a_session_that_asked_for_no_ca_sees_the_machines_own_file"
      "the_capsule_sees_the_merged_policy_and_can_open_the_ca_it_names"
    ];
    # rime-agent-core probes that need host tools and services the build sandbox lacks: `getent` at
    # /usr/bin/getent, busctl with a running system bus, and loginctl on PATH with a running logind (the lock
    # contract test asks logind about this user; with loginctl but no logind it would fail, not skip).
    noHostTools = [
      "sandbox::tests::a_confined_session_can_still_resolve_names"
      "sandbox::tests::the_masked_run_really_does_block_the_system_bus"
      "lock::tests::the_loginctl_contract_this_parser_relies_on_is_still_true"
    ];
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
  # --show-output puts a passing test's captured output in the log, so the report can see a test that skipped itself.
  checkFlags = lib.concatMap (t: [ "--skip" t ]) (lib.concatLists (lib.attrValues skips)) ++ [ "--show-output" ];
  # Programs the tests run: by the store paths above (curl, git, bwrap, openssl) or through PATH.
  nativeCheckInputs = [ bubblewrap cacert curl git openssh openssl python3 util-linux ];
  # xmllint, for the polkit checks in postInstall; help2man, for rime(1).
  nativeBuildInputs = [ libxml2 help2man ];
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
    # those, with every top-level directory of the build sandbox bound in unchanged. /usr holds only /usr/bin/env,
    # as on NixOS (environment.usrbinenv): code that assumed anything else under /usr would fail here, as it would
    # on NixOS, and a `#!/usr/bin/env bash` script (rime-remoted's publisher stub) runs as it does there.
    testRoot=$(mktemp -d)
    mkdir -p "$testRoot/var-tmp" "$testRoot/bin" "$testRoot/usr-bin" "$testRoot/shim"
    ln -s ${runtimeShell} "$testRoot/bin/sh"
    ln -s ${coreutils}/bin/true "$testRoot/bin/true"
    ln -s ${lib.getBin util-linux}/bin/setpriv "$testRoot/bin/setpriv"
    ln -s ${coreutils}/bin/env "$testRoot/usr-bin/env"
    bwrapArgs=(--dev-bind /dev /dev --bind /proc /proc --tmpfs /run --bind "$testRoot/bin" /bin
               --dir /usr --bind "$testRoot/usr-bin" /usr/bin
               --dir /var --bind "$testRoot/var-tmp" /var/tmp)
    for entry in /*; do
      case "$entry" in /bin|/dev|/proc|/run|/usr|/var) ;; *) bwrapArgs+=(--bind "$entry" "$entry") ;; esac
    done
    # `cargo test` also copies everything it prints into $NIX_BUILD_TOP/cargo-test.log for the report (postCheck).
    {
      echo "#!${runtimeShell}"
      echo 'set -o pipefail'
      printf 'run() { %q' ${bubblewrap}/bin/bwrap; printf ' %q' "''${bwrapArgs[@]}"
      printf ' --chdir "$PWD" -- %q "$@"; }\n' "$(command -v cargo)"
      echo 'if [ "''${1:-}" = test ]; then run "$@" 2>&1 | tee -a "$NIX_BUILD_TOP/cargo-test.log"; else run "$@"; fi'
    } > "$testRoot/shim/cargo"
    chmod +x "$testRoot/shim/cargo"
    export PATH="$testRoot/shim:$PATH"
    rm -f "$NIX_BUILD_TOP/cargo-test.log"
  '';

  postCheck = ''
    cargo metadata --offline --no-deps --format-version 1 --manifest-path rimed/Cargo.toml > "$NIX_BUILD_TOP/cargo-metadata.json"
    python3 ${./rimed-test-report.py} "$NIX_BUILD_TOP/cargo-test.log" "$NIX_BUILD_TOP/cargo-metadata.json" \
      ${builtins.toFile "rimed-test-skips.json" (builtins.toJSON { skip = skips; inherit selfSkipOk; })} \
      "$NIX_BUILD_TOP/cargo-tests.txt" "$NIX_BUILD_TOP/cargo-tests.json"
  '';

  postInstall = ''
    share=$out/share
    # The test report (postCheck); absent only from a build that overrides doCheck off (security.nix's mutant).
    if [ -e "$NIX_BUILD_TOP/cargo-tests.txt" ]; then
      install -Dm644 -t $share/doc/rimed "$NIX_BUILD_TOP/cargo-tests.txt" "$NIX_BUILD_TOP/cargo-tests.json"
    fi
    install -Dm644 -t $share/rimeos/sysprofiles $src/config/sysprofiles/*.toml
    install -Dm644 $src/config/ai/catalogue.toml $share/rimeos/ai/catalogue.toml
    install -Dm644 -t $share/rime-os/trust $src/files/system/trust/fulcio-root.pem $src/files/system/trust/enforcement.conf

    # rime(1): the page every Rime unit's Documentation=man:rime(1) names (files/system/units, base.nix), made from
    # the CLI's own --help so it cannot fall behind it. Without it those links lead nowhere (nix/tests/unit-docs.nix).
    mkdir -p $out/share/man/man1
    help2man --no-info --no-discard-stderr --name 'Rime OS control CLI' --output $out/share/man/man1/rime.1 $out/bin/rime
    grep -F 'Run and supervise coding agents' $out/share/man/man1/rime.1 >/dev/null

    # D-Bus: the bus policy as shipped, the activation file pointed at this rimed.
    install -Dm644 $src/files/system/dbus-1/system.d/org.rimeos.Rimed1.conf $share/dbus-1/system.d/org.rimeos.Rimed1.conf
    install -Dm644 $src/files/system/dbus-1/system-services/org.rimeos.Rimed1.service \
      $share/dbus-1/system-services/org.rimeos.Rimed1.service
    substituteInPlace $share/dbus-1/system-services/org.rimeos.Rimed1.service \
      --replace-fail 'Exec=/usr/bin/rimed' "Exec=$out/bin/rimed"

    # polkit actions (no exec.path annotations: nothing in them names a binary).
    install -Dm644 -t $share/polkit-1/actions $src/files/system/polkit-1/actions/*.policy
    # Containerfile.base's checks, carried over: polkitd silently skips an action file it cannot parse (pkcheck then
    # exits 127 for that action for ever), a renamed action authorises nothing, and the agent elevation actions
    # (§3.3) may never be reachable without a human or cache a yes.
    for f in $share/polkit-1/actions/org.rimeos.*.policy; do xmllint --noout --nonet "$f"; done
    grep -q 'id="org.rimeos.rimed.manage-power"' $share/polkit-1/actions/org.rimeos.rimed.policy
    grep -q 'id="org.rimeos.rimed.manage-battery"' $share/polkit-1/actions/org.rimeos.rimed.policy
    grep -q 'id="org.rimeos.agent.system-access"' $share/polkit-1/actions/org.rimeos.agent.policy
    grep -q 'id="org.rimeos.agent.break-glass"' $share/polkit-1/actions/org.rimeos.agent.policy
    agentPolicy=$share/polkit-1/actions/org.rimeos.agent.policy
    if grep -qE '<allow_[a-z]+>[a-z_]*_keep' $agentPolicy; then
      echo "rimed: a _keep authorisation caches a yes nobody was asked for" >&2; exit 1
    fi
    if grep -q '<allow_any>yes' $agentPolicy; then
      echo "rimed: <allow_any>yes hands the elevation action to any caller" >&2; exit 1
    fi
    if grep -q '<allow_inactive>yes' $agentPolicy; then
      echo "rimed: <allow_inactive>yes hands the elevation action to an inactive session" >&2; exit 1
    fi

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

  # The PATH the shipped rime-agentd unit hands every agent session (a test that runs its own unit reuses it).
  passthru = {
    inherit agentdPath;
    # The 76 tests nix/tests/rimed-session.nix runs in a logged-in VM session (the two cgroup groups above).
    sessionTests = skips.needsLoginSessionCgroup ++ selfSkipOk.noLoginSessionCgroup;
  };

  meta = {
    description = "Rime system daemon, CLI and helper daemons";
    homepage = "https://github.com/AndreNijman/rime-os";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
    mainProgram = "rime";
  };
}
