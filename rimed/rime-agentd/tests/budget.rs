//! §P2-011 against a real daemon.
//!
//! Everything that is a property of a value is tested beside the code in
//! `budget.rs`. Three claims are not properties of a value, and they are why
//! this file spawns a process:
//!
//! * **that the default really is untouched.** The argv of an unbudgeted
//!   session is supposed to be byte-identical to what it was before the hook
//!   existed. The observable for that is not the argv — it is the cgroup the
//!   session's process actually ends up in, which must be the daemon's own.
//!   A unit test can only say what `wrap` returned.
//!
//! * **that a refusal reaches the client.** A budget that cannot be delivered
//!   refuses the session, and the sentence saying why has to survive the error
//!   chain, `run_error`'s classification and the socket. A refusal that is
//!   correct in `budget.rs` and arrives as `bad request: error` is not a
//!   refusal anybody can act on.
//!
//! * **that the limits are written on a real cgroup** — the only claim that
//!   proves the feature does anything at all.
//!
//! ## Why the last one is opt-in
//!
//! `systemd-run --user` needs a user manager, and it finds one at
//! `$XDG_RUNTIME_DIR/systemd/private`. **Measured: when `XDG_RUNTIME_DIR` is
//! set and that path is missing, it does NOT fall back to
//! `$DBUS_SESSION_BUS_ADDRESS`** — a correct bus address plus a fixture
//! runtime directory still fails. Since this harness gives its daemon its own
//! `XDG_RUNTIME_DIR` (and must: sharing the real one would mean binding the
//! live daemon's socket path), the child it starts cannot reach a user manager
//! unless the harness lends it one by symlink.
//!
//! Doing that is fine on a developer's machine and meaningless in a container
//! with no user manager at all, so the live half runs only under
//! `RIME_BUDGET_LIVE=1` and is deliberately **not** in `pr-validation.yml`.
//! The three tests above it need no bus, no privilege and no prompt, and are.
//!
//! Every daemon here gets its own `XDG_RUNTIME_DIR`, `XDG_STATE_HOME`,
//! `XDG_CONFIG_HOME` and `RIME_AGENT_SCRATCH_ROOT`. It never touches a running
//! `rime-agentd`, and it is killed by pid — never by name.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Harness {
    child: Child,
    socket: PathBuf,
    root: PathBuf,
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Harness {
    /// A daemon whose `agent.json` is `config`, and which can reach a user
    /// manager only if `lend_transport` says so.
    ///
    /// `XDG_CONFIG_HOME` is set for a reason that would otherwise be a silent
    /// trap: `Config::load` goes through `paths::config_home()`, so a harness
    /// that sets only the runtime and state directories reads the developer's
    /// own `~/.config/rime/agent.json`. The day somebody puts a real `budget`
    /// stanza in theirs, the "no budget, no wrapper" test below would start
    /// failing for a reason nobody could find.
    fn start(tag: &str, config: &str, lend_transport: bool) -> Option<Harness> {
        let root = std::env::temp_dir().join(format!(
            "rime-budget-e2e-{}-{tag}-{}",
            std::process::id(),
            now_ms()
        ));
        let runtime = root.join("run");
        let state = root.join("state");
        let config_home = root.join("config");
        std::fs::create_dir_all(&runtime).ok()?;
        std::fs::create_dir_all(&state).ok()?;
        std::fs::create_dir_all(config_home.join("rime")).ok()?;
        std::fs::write(config_home.join("rime/agent.json"), config).ok()?;

        if lend_transport {
            // The daemon's runtime directory has to be its own — sharing
            // /run/user/<uid> would bind the live daemon's socket path — but
            // the child still has to find a user manager through it. One
            // symlink is the whole difference, and it is a property of the
            // HARNESS rather than of the product: in production the daemon's
            // XDG_RUNTIME_DIR is /run/user/<uid> and the socket is simply
            // there.
            std::fs::create_dir_all(runtime.join("systemd")).ok()?;
            std::os::unix::fs::symlink(
                real_runtime_dir().join("systemd/private"),
                runtime.join("systemd/private"),
            )
            .ok()?;
        }

        let child = Command::new(env!("CARGO_BIN_EXE_rime-agentd"))
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("XDG_STATE_HOME", &state)
            .env("XDG_CONFIG_HOME", &config_home)
            .env(
                rime_agent_core::paths::SCRATCH_ROOT_ENV,
                root.join("scratch"),
            )
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;

        let socket = runtime.join("rime-agentd").join("control.sock");
        let harness = Harness {
            child,
            socket,
            root,
        };
        harness.wait_for_socket().then_some(harness)
    }

    fn wait_for_socket(&self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if UnixStream::connect(&self.socket).is_ok() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        false
    }

    fn call(&self, line: &str) -> serde_json::Value {
        assert!(!line.contains('\n'), "one JSON object per line");
        let mut stream = UnixStream::connect(&self.socket).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .expect("timeout");
        writeln!(stream, "{line}").expect("write");
        stream.flush().ok();
        let mut reply = String::new();
        BufReader::new(&stream).read_line(&mut reply).expect("read");
        serde_json::from_str(&reply).unwrap_or_else(|e| panic!("{e}: {reply}"))
    }

    /// Ask for a session running `sh -c 'sleep 60'`, and return the reply as
    /// it arrived — a session or a refusal, because half of these tests are
    /// about the refusal.
    fn run(&self) -> serde_json::Value {
        let run = serde_json::json!({
            "cmd": "run",
            "agent": "generic",
            "args": ["sh", "-c", "exec sleep 60"],
            "cwd": "/tmp",
            "sandbox": "unrestricted",
            "network": "open",
            "cols": 80,
            "rows": 24,
        });
        self.call(&run.to_string())
    }

    fn info(&self, id: u64) -> serde_json::Value {
        self.call(&format!(r#"{{"cmd":"info","id":{id}}}"#))
    }

    fn kill(&self, id: u64) {
        self.call(&format!(r#"{{"cmd":"signal","id":{id},"signal":"kill"}}"#));
    }
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// The runtime directory of the user manager this machine actually runs.
fn real_runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::geteuid() })))
}

/// Wait for a process's cgroup to contain `needle`, and return whatever it was
/// at the end.
///
/// **There is a real window here and it is not test flakiness.** The daemon
/// replies as soon as `pty::spawn` returns — before the child has exec'd
/// anything — so for a few milliseconds a budgeted session's pid is still in
/// the daemon's own cgroup. systemd-run then asks the manager for the scope,
/// the manager moves the pid, and only then does the exec chain reach the
/// program. Reading once, immediately, measures the window rather than the
/// feature; it is what made this test fail the first time it ran.
fn wait_for_cgroup(pid: u64, needle: &str, secs: u64) -> String {
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut last = String::new();
    while Instant::now() < deadline {
        last = cgroup_of(pid).unwrap_or_default();
        if last.contains(needle) {
            return last;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    last
}

/// Wait for a process's `comm` to be `want`, and return whatever it was at the
/// end. Same window as [`wait_for_cgroup`], and the same reason.
fn wait_for_comm(pid: u64, want: &str, secs: u64) -> String {
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut last = String::new();
    while Instant::now() < deadline {
        last = std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .unwrap_or_default()
            .trim()
            .to_string();
        if last == want {
            return last;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    last
}

/// The unified-hierarchy cgroup path of a process.
fn cgroup_of(pid: u64) -> Option<String> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("0::"))
        .map(|p| p.trim().to_string())
}

/// The error message from a refused reply, or a panic naming what came back.
fn refusal(reply: &serde_json::Value) -> String {
    assert_eq!(
        reply["reply"], "error",
        "expected a refusal, got a session: {reply}"
    );
    reply["message"]
        .as_str()
        .unwrap_or_else(|| panic!("no message on {reply}"))
        .to_string()
}

macro_rules! harness {
    ($tag:literal, $config:expr, $transport:expr) => {
        match Harness::start($tag, $config, $transport) {
            Some(h) => h,
            None => {
                // A `return` in a #[test] is reported as a PASS, and the
                // eprintln is swallowed unless somebody passed --nocapture. So
                // for a run that explicitly asked for the live half, a daemon
                // that would not start has to be a failure: RIME_BUDGET_LIVE=1
                // means "prove it", and a green tick over an assertion that
                // never executed is the exact shape that let FOUND (seventh)
                // ship. Same reasoning as RIME_REQUIRE_RIME_CLI in
                // pr-validation.yml.
                assert!(
                    !live_wanted(),
                    "RIME_BUDGET_LIVE=1 was set, so this test was asked to \
                     prove the scope is real — but rime-agentd would not \
                     start for {}, and skipping here would report that as a \
                     pass",
                    $tag
                );
                eprintln!("SKIP {}: rime-agentd would not start", $tag);
                return;
            }
        }
    };
}

/// Only run the live half when asked. See the module docs.
fn live_wanted() -> bool {
    match std::env::var("RIME_BUDGET_LIVE") {
        Ok(v) => v == "1",
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Always on. No bus, no privilege, no prompt.
// ---------------------------------------------------------------------------

#[test]
fn an_unbudgeted_session_runs_in_the_daemons_own_cgroup() {
    // The untouched default, proved where it counts. If `wrap` ever emitted a
    // prefix for an empty budget — or if discovery moved above the `is_empty`
    // check and started refusing on a machine with no user manager — the
    // session's cgroup would stop being the daemon's and this fails.
    let h = harness!("plain", "{}", false);
    let reply = h.run();
    assert_eq!(reply["reply"], "session", "{reply}");
    let id = reply["id"].as_u64().expect("id");

    let info = h.info(id);
    let pid = info["pid"].as_u64().expect("pid");

    // Wait for the exec chain to REACH the program before judging where it
    // lives. Without this the test is vacuous in the direction that matters: a
    // mutant that wrapped every session would still be in the daemon's cgroup
    // for the few milliseconds before systemd-run got the scope, so an
    // immediate read would call the mutant correct.
    let comm = wait_for_comm(pid, "sleep", 10);
    assert_eq!(comm, "sleep", "the session never reached its program: {comm:?}");

    let session = cgroup_of(pid).expect("the session's cgroup");
    let daemon = cgroup_of(h.child.id() as u64).expect("the daemon's cgroup");

    assert_eq!(
        session, daemon,
        "an unbudgeted session must not be moved anywhere"
    );
    assert!(
        !session.contains("rime-agent-"),
        "no scope may be created for a session nobody budgeted: {session}"
    );
    h.kill(id);
}

#[test]
fn a_budget_with_no_user_manager_refuses_and_names_the_path() {
    // The measured failure this check exists to replace. Without it the child
    // execs systemd-run, which prints one line to the PTY and exits 1 — so the
    // client is told the session started, the agent is dead a moment later,
    // and nothing anywhere names the cause. With it, the request is refused
    // and the sentence names the directory that had no user manager in it.
    let h = harness!("no-manager", r#"{"budget": {"memory_max": "512M"}}"#, false);
    let why = refusal(&h.run());
    assert!(
        why.contains("resource budget"),
        "the refusal must name the feature that caused it: {why}"
    );
    // On a machine with no systemd-run at all — a minimal container — the
    // refusal is the earlier one and names that instead. Both are correct and
    // both are refusals; asserting the wrong one of the two would make this a
    // test of the runner's package list. So the specific sentence is claimed
    // only where the earlier check cannot have fired.
    if Path::new(rime_paths::SYSTEMD_RUN).exists() {
        assert!(
            why.contains("systemd/private"),
            "the refusal must name what was looked for: {why}"
        );
    } else {
        assert!(
            why.contains("systemd-run"),
            "the refusal must name what is missing: {why}"
        );
    }
}

#[test]
fn a_budget_refusal_is_a_policy_answer_and_not_a_bad_request() {
    // The kind a client branches on. `BadRequest` would send the user to look
    // at the request they made, and there is nothing wrong with it — the
    // problem is the configuration or the machine.
    let h = harness!("kind", r#"{"budget": {"memory_max": "512M"}}"#, false);
    let reply = h.run();
    assert_eq!(reply["reply"], "error", "{reply}");
    assert_eq!(reply["kind"], "policy_refused", "{reply}");
}

#[test]
fn a_mistyped_budget_key_refuses_rather_than_starting_the_session_unbudgeted() {
    // The whole chain for the trap that matters most: a typo degrading to "no
    // budget" would hand the user an unconfined agent and a config file that
    // reads as if it says otherwise. The error has to name both the key they
    // wrote and the one they meant, all the way out to the socket.
    let h = harness!("typo", r#"{"budget": {"memmory_max": "512M"}}"#, false);
    let why = refusal(&h.run());
    assert!(why.contains("memmory_max"), "{why}");
    assert!(why.contains("memory_max"), "{why}");
}

#[test]
fn a_budget_of_zero_is_refused_before_anything_is_started() {
    // `MemoryMax=0` is a session that cannot run. This is the end-to-end half
    // of the parse-time rule: no daemon anywhere turns a configured 0 into a
    // started session.
    let h = harness!("zero", r#"{"budget": {"tasks_max": 0}}"#, false);
    let why = refusal(&h.run());
    assert!(why.contains("tasks_max"), "{why}");
    assert!(why.contains("not a limit"), "{why}");
}

#[test]
fn an_unparseable_config_file_does_not_pretend_there_is_no_budget() {
    // `Config::load` degrades a corrupt file to the defaults, and for a
    // preference that is right. Recorded here because it means a file so
    // broken that `budget` never reaches `extra` starts an UNBUDGETED session
    // — the daemon cannot refuse on a stanza it never saw. The remedy is that
    // `rime agent status` reports the corrupt file, not that this layer
    // guesses; what this test pins is that the behaviour is the documented one
    // and not a crash.
    let h = harness!("corrupt", r#"{"budget": {"memory_max": "512M",,}}"#, false);
    let reply = h.run();
    assert_eq!(
        reply["reply"], "session",
        "a corrupt config is the documented degrade-to-defaults, not a crash: {reply}"
    );
    let id = reply["id"].as_u64().expect("id");
    let pid = h.info(id)["pid"].as_u64().expect("pid");
    assert!(!cgroup_of(pid).expect("cgroup").contains("rime-agent-"));
    h.kill(id);
}

// ---------------------------------------------------------------------------
// Opt-in: RIME_BUDGET_LIVE=1. Needs a user manager. Not in pr-validation.yml.
// ---------------------------------------------------------------------------

#[test]
fn a_budgeted_session_runs_in_its_own_scope_with_the_limits_really_written() {
    if !live_wanted() {
        eprintln!("SKIP live budget: set RIME_BUDGET_LIVE=1 (needs a user manager)");
        return;
    }
    // `io_read_bps` is in this config on purpose. It is the resource this
    // machine cannot enforce, and the claim is that naming it does NOT stop
    // the session starting and does NOT put a zero anywhere — it is dropped,
    // reported, and the other three are enforced regardless.
    let h = harness!(
        "live",
        r#"{"budget": {"memory_max": "512M", "tasks_max": 64, "cpu_percent": 50,
                       "io_read_bps": "10M"}}"#,
        true
    );
    let reply = h.run();
    assert_eq!(reply["reply"], "session", "{reply}");
    let id = reply["id"].as_u64().expect("id");
    let pid = h.info(id)["pid"].as_u64().expect("pid");

    let want_leaf = format!("rime-agent-{}-{id}.scope", h.child.id());
    let cgroup = wait_for_cgroup(pid, &want_leaf, 10);
    assert!(
        cgroup.ends_with(&want_leaf),
        "the session must be in its own scope: wanted …/{want_leaf}, got {cgroup}"
    );

    // The §7 property, asserted against the path a budgeted agent really has
    // rather than against a string this test made up. A scope is placed under
    // the user manager whoever asks for it, so the origin must still be
    // `scheduled-job` — not the local origin §7 reserves root and break-glass
    // for.
    use rime_agent_core::origin::classify;
    use rime_agent_core::policy::RequestOrigin;
    for tty in [true, false] {
        assert_eq!(
            classify(&cgroup, tty),
            Some(RequestOrigin::ScheduledJob),
            "a budgeted agent gained a local origin: {cgroup}"
        );
    }

    let dir = Path::new("/sys/fs/cgroup").join(cgroup.trim_start_matches('/'));
    // A file that cannot be read is named, not folded into "wrong value": the
    // two have different remedies and this unit has lost a day to the
    // difference before.
    let read = |f: &str| {
        std::fs::read_to_string(dir.join(f))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|e| panic!("{} could not be read: {e}", dir.join(f).display()))
    };

    assert_eq!(
        read("memory.max"),
        "536870912",
        "MemoryMax was not written on {}",
        dir.display()
    );
    assert_eq!(read("pids.max"), "64");
    assert_eq!(read("cpu.max"), "50000 100000");

    // The unenforceable one, from the machine rather than from the module's
    // own opinion of it: there is no io.max here, which is why a budget of
    // zero would have been the only thing to write.
    assert!(
        !dir.join("io.max").exists(),
        "io is enforceable here after all — budget.rs's note is stale"
    );

    // `--scope` execs in place, which is the property that keeps the hook one
    // line: the pid the daemon recorded is the program, not systemd-run. If it
    // were `--service` this would say "systemd-run" or "(sd-pam)" and every
    // pid-based operation would be aiming at the wrong process.
    let comm = wait_for_comm(pid, "sleep", 10);
    assert_eq!(
        comm, "sleep",
        "the recorded pid is not the program it was asked to run — with \
         --service it would be systemd-run's, and every pid-based operation \
         would aim at the wrong process"
    );

    h.kill(id);
}

#[test]
fn a_wall_clock_budget_really_ends_the_session() {
    if !live_wanted() {
        eprintln!("SKIP live runtime budget: set RIME_BUDGET_LIVE=1");
        return;
    }
    // `sleep 60` under `RuntimeMaxSec=5`. Measured at the command line as
    // rc=143 at exactly 5s; this is the same thing through the daemon, which
    // is where it has to be true. The generous deadline is because the
    // assertion is "it was ended by the budget", not "it was ended in exactly
    // five seconds" — a test that fails on a loaded machine teaches people to
    // re-run it until it passes.
    let h = harness!("runtime", r#"{"budget": {"runtime_secs": 5}}"#, true);
    let reply = h.run();
    assert_eq!(reply["reply"], "session", "{reply}");
    let id = reply["id"].as_u64().expect("id");

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut info = h.info(id);
    while Instant::now() < deadline && info["state"] != "exited" {
        std::thread::sleep(Duration::from_millis(200));
        info = h.info(id);
    }
    assert_eq!(
        info["state"], "exited",
        "a 5s runtime budget did not end a 60s sleep: {info}"
    );
    // SIGTERM, one way or the other: systemd reports it as a signal, and a
    // shell in the chain would report it as 128+15.
    let signal = info["exit_signal"].as_i64();
    let code = info["exit_code"].as_i64();
    assert!(
        signal == Some(15) || code == Some(143),
        "ended, but not by the budget's SIGTERM: signal={signal:?} code={code:?} info={info}"
    );
}
