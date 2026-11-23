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
# Tests. rimed itself is only compiled; its cargo test is `rimed.cargoTest` (checks.rimed-cargo-test, and a step of
# nix-release's build job, so a failing test still stops a release): the same derivation with checkPhase on, whose
# output is rimed's plus the report. Apart from that report it ships exactly what rimed ships. The checkPhase runs
# `cargo test --workspace --no-fail-fast`. The tests that cannot pass in the Nix build sandbox are skipped BY NAME in
# `skips` below, each group with its reason; everything else must pass. postCheck writes the total per crate to
# $out/share/doc/rimed/cargo-tests.{txt,json} (rimed-test-report.py) and refuses a total that cannot be trusted: a
# skip name that matches no test, a test that passed by skipping itself, a lost test binary.
{
  lib,
  rustPlatform,
  runtimeShell,
  bubblewrap,
  cacert,
  coreutils,
  curl,
  dbus,
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
    RIME_MIGRATE = engine "rime-migrate";
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
      "hello_offers_both_verbs"
      "with_the_stub_the_clipboard_is_a_file_and_wl_copy_never_runs"
      "without_the_stub_the_text_goes_to_wl_copy_on_stdin_never_in_argv"
      "an_empty_or_oversize_clipboard_write_is_refused_and_nothing_runs"
      "a_file_is_saved_whole_under_its_own_name_and_never_over_another"
      "without_the_stub_files_go_to_the_users_download_folder"
      "an_oversize_or_empty_file_is_refused_before_the_takeover"
      "an_upload_that_stops_half_way_leaves_nothing"
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
      "a_call_dies_with_the_phones_connection_and_revoke_and_a_locked_phone_answers_but_never_dials"
      "a_channel_onto_a_session_that_does_not_exist_is_refused_with_the_daemons_own_words"
      "a_computer_a_phone_introduced_shows_no_card_until_a_person_allows_it"
      "a_computer_brings_its_phone_to_a_new_computer_as_a_companion_until_a_person_allows_it"
      "a_computer_given_view_lists_and_peeks_given_control_types_and_lowering_or_removal_drops_it_at_once"
      "a_computer_whose_cards_are_off_or_that_was_removed_shows_nothing"
      "a_companion_key_minted_over_a_paired_session_reaches_the_device_features_and_not_the_runtime"
      "a_confined_session_is_judged_against_the_confinement_it_actually_has"
      "a_confined_session_is_told_where_its_runtime_directory_is"
      "a_copied_home_cannot_pair_with_or_connect_to_the_computer_it_came_from"
      "a_copy_no_person_handed_over_reaches_the_phones_and_no_computer"
      "a_declared_remote_origin_reaches_both_verbs"
      "a_device_pairs_over_the_real_socket_and_lands_in_the_real_store"
      "a_device_reaches_the_desktop_through_a_relay_neither_of_them_listens_on"
      "a_file_from_the_phone_crosses_in_frames_and_its_path_is_typed_into_the_session"
      "a_file_over_a_slow_relay_keeps_its_session_while_the_pongs_queue_behind_it"
      "a_file_sent_to_a_paired_computer_lands_in_its_downloads_under_a_new_name_and_nothing_is_opened"
      "a_killed_session_has_nothing_still_running_under_it"
      "a_launch_that_fails_says_so_and_leaves_the_packet_behind"
      "a_link_offered_to_a_paired_computer_is_a_card_there_and_opens_only_when_open_is_pressed"
      "a_messages_request_dies_with_the_phones_connection_and_revoke_and_a_locked_phone_cannot_send"
      "a_name_given_at_run_is_on_the_session_the_list_and_the_record"
      "a_narrowed_session_runs_under_its_own_allowlist_and_not_the_runtimes"
      "a_network_change_costs_the_session_and_costs_nothing_else"
      "a_notification_dismissed_here_is_dismissed_on_a_phone_that_says_it_can"
      "a_paired_device_reaches_the_agent_runtime_through_the_proxy"
      "a_paired_phone_reaches_nex_through_the_running_service"
      "a_payload_with_quotes_and_backslashes_arrives_unchanged"
      "a_peer_offering_a_bad_name_a_wrong_hash_or_half_a_file_leaves_nothing_behind"
      "a_peer_with_control_reaches_no_approval_nex_spawn_or_kill_and_hostile_requests_are_refused"
      "a_phone_introduces_two_computers_and_neither_may_remove_others_or_bring_a_phone_until_allowed"
      "a_phone_registers_for_push_over_the_real_session_and_the_key_lands_in_its_own_file"
      "a_phone_reports_its_battery_rings_over_its_link_and_revoke_forgets_both"
      "a_phone_that_allows_control_gets_each_call_on_its_events_channel_and_nothing_it_did_not_allow"
      "a_phone_that_leaves_gives_the_desktop_its_size_back"
      "a_phone_with_the_grant_drives_the_desktop_and_a_lock_a_lowered_grant_or_a_lost_connection_lets_go"
      "a_phones_call_rings_here_is_answered_from_the_notification_and_history_hangup_and_dial_go_through_the_cli"
      "a_phones_clipboard_follows_this_computers_only_where_it_asked_and_never_comes_straight_back"
      "a_phones_do_not_disturb_reaches_the_shell_on_a_change_and_the_computers_reaches_the_phones_that_follow_it"
      "a_phones_media_shows_here_as_a_player_that_goes_with_the_connection_and_with_revoke"
      "a_phones_notifications_show_here_update_in_place_close_and_go_with_revoke"
      "a_phones_photos_are_listed_here_with_private_thumbnails_saved_whole_and_nothing_reaches_a_phone_that_did_not_turn_them_on"
      "a_phones_sms_threads_are_read_here_in_chunks_and_sent_from_here_and_nothing_reaches_a_phone_that_did_not_turn_it_on"
      "a_project_grant_is_reported_as_inherited_and_a_session_grant_is_not"
      "a_push_endpoint_the_machine_cannot_use_is_refused_and_nothing_is_stored"
      "a_relayed_session_is_recorded_as_relayed_and_a_direct_one_is_not"
      "a_reply_typed_here_goes_to_a_phone_that_says_it_can_and_is_not_a_dismissal"
      "a_request_from_a_phone_is_remote_and_cannot_approve_root"
      "a_second_device_cannot_reuse_the_first_ones_code"
      "a_sender_keeps_waiting_through_a_link_slower_than_a_progress_line_every_ten_seconds"
      "a_session_cannot_ask_for_a_grant_or_renew_one_however_it_asks"
      "a_session_may_not_read_this_machines_clipboard"
      "a_session_may_not_set_the_clipboard_or_save_a_file"
      "a_session_reports_a_graph_even_when_it_has_delegated_nothing"
      "a_session_that_narrows_nothing_still_gets_the_whole_runtime_allowlist"
      "a_status_line_with_nothing_to_report_publishes_nothing"
      "a_subagent_start_and_stop_become_one_finished_child"
      "a_task_with_two_sessions_is_refused_and_both_ids_are_named"
      "a_terminal_streams_both_ways_over_one_channel"
      "a_touchpad_stopped_at_the_computer_lets_go_tells_the_phone_and_holds_it_off"
      "a_user_with_no_status_line_gets_no_status_line"
      "a_wrong_code_uses_the_code_up_and_with_no_screen_open_nothing_pairs"
      "an_agents_terminal_title_reaches_the_session_and_the_record"
      "an_empty_payload_is_accepted_and_writes_nothing"
      "an_introduced_computer_has_no_agents_grant_until_a_person_allows_it_and_then_grants_it"
      "an_unknown_target_is_refused_before_anything_is_written"
      "an_upload_is_refused_before_a_byte_of_it_is_sent"
      "changed_files_name_the_base_they_were_measured_against"
      "only_the_outgoing_sessions_system_grants_are_listed"
      "paired_computers_exchange_presence_and_a_peer_sending_garbage_has_nothing_kept"
      "paired_computers_share_a_copy_only_where_both_turned_it_on_and_it_never_comes_back"
      "peek_reads_the_tail_without_attaching_or_resizing"
      "photos_are_refused_to_a_locked_phone_a_listing_dies_with_revoke_and_a_save_the_phone_never_starts_times_out"
      "rename_names_a_live_session_clears_it_and_refuses_what_it_should"
      "removing_a_computer_tells_it_and_it_removes_this_one_and_forget_on_both_pairs_them_again"
      "revoking_a_device_also_stops_it_being_woken"
      "revoking_a_device_ends_the_connection_it_is_already_holding"
      "something_sent_waits_for_its_phone_and_is_taken_whole_over_the_link"
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
      "three_computers_share_one_copy_once_each_and_it_settles"
      "two_computers_pair_with_a_code_ping_each_other_and_a_peer_reaches_nothing_but_hello"
      "two_paired_computers_reach_each_other_through_the_relay"
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

  rimedTree = lib.fileset.difference (root + "/rimed") (lib.fileset.maybeMissing (root + "/rimed/target"));

  # What rimed and its deps derivation share, so deps compiles each third-party crate exactly as rimed's own build
  # would (same toolchain, hooks, flags, inputs and test environment): cargo then finds it fresh and reuses it.
  common = {
    version = "0.1.0";
    cargoRoot = "rimed";
    buildAndTestSubdir = "rimed";
    cargoLock.lockFile = root + "/rimed/Cargo.lock";
    # deps compiles the test units too; rimed turns its checkPhase off and on again in cargoTest.
    doCheck = true;
    # Programs the tests run: by the store paths above (curl, git, bwrap, openssl) or through PATH.
    # dbus: rimed's bus tests (rimed/src/bus_tests.rs) start a private dbus-daemon from PATH.
    nativeCheckInputs = [ bubblewrap cacert curl dbus git openssh openssl python3 util-linux ];
    # xmllint, for the polkit checks in postInstall; help2man, for rime(1).
    nativeBuildInputs = [ libxml2 help2man ];
    inherit preCheck;
  };

  # The tests assume a root shaped like a running machine, which the build sandbox is not: their fixtures go under
  # /var/tmp (the project's rule for anything a suite creates), the agent sandbox puts a tmpfs over /run, and a few
  # fixtures exec /bin/true or look for setpriv in /bin. So `cargo test` runs under bwrap in a root that has
  # those, with every top-level directory of the build sandbox bound in unchanged. /usr holds only /usr/bin/env,
  # as on NixOS (environment.usrbinenv): code that assumed anything else under /usr would fail here, as it would
  # on NixOS, and a `#!/usr/bin/env bash` script (rime-remoted's publisher stub) runs as it does there.
  preCheck = ''
    # The sandbox's HOME is /homeless-shelter (absent) and there is no XDG_RUNTIME_DIR, so code that falls back to
    # /run/user/<uid> would try a directory that does not exist here.
    export HOME=$(mktemp -d) XDG_RUNTIME_DIR=$(mktemp -d)
    git config --global user.name rime-test
    git config --global user.email rime-test@localhost
    git config --global init.defaultBranch main
    # The relay and TLS tests read the system trust store.
    export SSL_CERT_FILE=${cacert}/etc/ssl/certs/ca-bundle.crt

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
    # rimed's postCheck counts from here: a third-party crate compiled after this was not taken from deps.
    touch "$NIX_BUILD_TOP/rimed-check.marker"
  '';

  # The workspace's third-party crates, compiled for `cargo build` and for `cargo test` from the real manifests and
  # lock with an empty body in every member (nix/pkgs/rimed-deps.py). Its source is only the Cargo.toml files and
  # Cargo.lock, so a change to Rime's own code leaves this derivation as it was: the packages job takes it from the
  # dev cache and rimed compiles only its 13 members. What rimed ships is unchanged: its binaries are still compiled
  # from the real source in rimed's own derivation, against the third-party artifacts it would have compiled itself.
  deps = rustPlatform.buildRustPackage (common // {
    pname = "rimed-deps";
    src = lib.fileset.toSource {
      inherit root;
      fileset = lib.fileset.unions [
        (root + "/rimed/Cargo.lock")
        (lib.fileset.intersection rimedTree (lib.fileset.fileFilter (f: f.name == "Cargo.toml") (root + "/rimed")))
      ];
    };
    postPatch = "python3 ${./rimed-deps.py} dummy rimed";
    # Compile the test units too (dev-dependencies, test features), without running anything.
    cargoTestFlags = [ "--workspace" "--no-run" ];
    installPhase = ''
      runHook preInstall
      cargo metadata --offline --no-deps --format-version 1 --manifest-path rimed/Cargo.toml > "$NIX_BUILD_TOP/cargo-metadata.json"
      python3 ${./rimed-deps.py} prune target "$NIX_BUILD_TOP/cargo-metadata.json"
      mkdir -p $out
      mv target $out/target
      runHook postInstall
    '';
    # Build artifacts, not a program: nothing to strip or patch (a stripped .rlib is a broken one).
    dontFixup = true;
  });
in
rustPlatform.buildRustPackage (finalAttrs: common // {
  pname = "rimed";
  # Compiled only. Its cargo test (14 of the 20 minutes a rimed build took, measured: compiling ~80 release test
  # binaries) is passthru.cargoTest, a check of its own, so the CI packages job and every leg that needs rimed wait
  # for the compile alone.
  doCheck = false;

  src = lib.fileset.toSource {
    inherit root;
    fileset = lib.fileset.unions [
      rimedTree
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

  env = lib.mapAttrs' (name: value: lib.nameValuePair "RIME_PATH_${name}" value) paths // {
    RIME_PATHS_EXPECT_ALL_SET = "1";
  };

  # The third-party crates, already compiled (deps above). cargoBuildHook runs this before it points
  # CARGO_TARGET_DIR at ./target; an override that replaces buildPhase (nix/tests/rimed-session.nix) sets it there
  # itself. A copy, not a link (cargo writes into it), so every artifact gets a fresh mtime, newer than the vendored
  # sources the setup hook copied before it.
  preBuild = ''
    cp -r ${deps}/target target
    chmod -R u+w target
    # cargo runs a build script again (and compiles its crate and every crate above it again) when a file it named
    # with rerun-if-changed is newer than its last run, and cargoSetupHook has just copied the vendored crates with
    # fresh mtimes: one time for the whole copy, taken after that, keeps them fresh (measured: 86 of 193 compiled
    # again without it).
    touch "$NIX_BUILD_TOP/rimed-deps.stamp"
    find target -exec touch -h -r "$NIX_BUILD_TOP/rimed-deps.stamp" {} +
    # The guard: no workspace member may come from deps. Its dummy artifacts are now as new as everything else, so
    # cargo would take them for the real crates.
    cargo metadata --offline --no-deps --format-version 1 --manifest-path rimed/Cargo.toml > "$NIX_BUILD_TOP/cargo-metadata.json"
    ${python3.interpreter} ${./rimed-deps.py} prune target "$NIX_BUILD_TOP/cargo-metadata.json"
    touch "$NIX_BUILD_TOP/rimed-build.marker"
  '';
  # Says how many third-party crates came from deps and names any compiled again (a deps derivation that no longer
  # matches this build: a new env var a build script reads, a flag, an input only one of them has). python3 by store
  # path: an override with doCheck off (security.nix, the mutants) has no nativeCheckInputs on PATH.
  postBuild = ''
    ${python3.interpreter} ${./rimed-deps.py} recompiled target "$NIX_BUILD_TOP/cargo-metadata.json" "$NIX_BUILD_TOP/rimed-build.marker"
  '';

  # The whole workspace, every test binary even after one fails, so the log has every total.
  cargoTestFlags = [ "--workspace" "--no-fail-fast" ];
  # --show-output puts a passing test's captured output in the log, so the report can see a test that skipped itself.
  checkFlags = lib.concatMap (t: [ "--skip" t ]) (lib.concatLists (lib.attrValues skips)) ++ [ "--show-output" ];

  postCheck = ''
    cargo metadata --offline --no-deps --format-version 1 --manifest-path rimed/Cargo.toml > "$NIX_BUILD_TOP/cargo-metadata.json"
    python3 ${./rimed-deps.py} recompiled target "$NIX_BUILD_TOP/cargo-metadata.json" "$NIX_BUILD_TOP/rimed-check.marker"
    python3 ${./rimed-test-report.py} "$NIX_BUILD_TOP/cargo-test.log" "$NIX_BUILD_TOP/cargo-metadata.json" \
      ${builtins.toFile "rimed-test-skips.json" (builtins.toJSON { skip = skips; inherit selfSkipOk; })} \
      "$NIX_BUILD_TOP/cargo-tests.txt" "$NIX_BUILD_TOP/cargo-tests.json"
  '';

  postInstall = ''
    share=$out/share
    # The test report (postCheck): only cargoTest has one.
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
    # The power-profiles API rimed answers (rimed/src/ppd.rs; docs/nixos/desktops.md §7): its bus policy and the two
    # names' activation files, which start rimed.service.
    install -Dm644 $src/files/system/dbus-1/system.d/org.rimeos.Rimed1.PowerProfiles.conf \
      $share/dbus-1/system.d/org.rimeos.Rimed1.PowerProfiles.conf
    for name in org.freedesktop.UPower.PowerProfiles net.hadess.PowerProfiles; do
      install -Dm644 $src/files/system/dbus-1/system-services/$name.service $share/dbus-1/system-services/$name.service
      substituteInPlace $share/dbus-1/system-services/$name.service \
        --replace-fail 'Exec=/usr/bin/rimed' "Exec=$out/bin/rimed"
    done

    # polkit actions (no exec.path annotations: nothing in them names a binary).
    install -Dm644 -t $share/polkit-1/actions $src/files/system/polkit-1/actions/*.policy
    # Containerfile.base's checks, carried over: polkitd silently skips an action file it cannot parse (pkcheck then
    # exits 127 for that action for ever), a renamed action authorises nothing, and the agent elevation actions
    # (§3.3) may never be reachable without a human or cache a yes.
    for f in $share/polkit-1/actions/org.rimeos.*.policy; do xmllint --noout --nonet "$f"; done
    grep -q 'id="org.rimeos.rimed.manage-power"' $share/polkit-1/actions/org.rimeos.rimed.policy
    grep -q 'id="org.rimeos.rimed.manage-battery"' $share/polkit-1/actions/org.rimeos.rimed.policy
    grep -q 'id="org.rimeos.rimed.check-update"' $share/polkit-1/actions/org.rimeos.rimed.policy
    grep -q 'id="org.rimeos.rimed.apply-update"' $share/polkit-1/actions/org.rimeos.rimed.policy
    for id in switch-profile hold-profile configure-battery-aware configure-action; do
      grep -q "id=\"org.freedesktop.UPower.PowerProfiles.$id\"" $share/polkit-1/actions/org.rimeos.rimed.power-profiles.policy
    done
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
    # The third-party crates, compiled (nix/ci/prebuild.txt builds it once per run, like rimed).
    inherit deps;
    # rimed with its cargo test: checks.rimed-cargo-test, and nix-release builds it before it publishes anything.
    cargoTest = finalAttrs.finalPackage.overrideAttrs {
      pname = "rimed-cargo-test";
      doCheck = true;
    };
  };

  meta = {
    description = "Rime system daemon, CLI and helper daemons";
    homepage = "https://github.com/AndreNijman/rime-os";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
    mainProgram = "rime";
  };
})
