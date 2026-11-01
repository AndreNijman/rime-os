//! P1-026's second criterion, measured rather than asserted.
//!
//! "Executable plugin content runs inside a sandbox" is the easiest claim in
//! this repository to make vacuously: a grep for a bubblewrap argument, or a
//! flag in a document nobody executes, and the criterion reads as met. The
//! proof has to be a plugin's own executable content *trying* to reach
//! something outside the sandbox and being observed to fail.
//!
//! So this file builds a fixture with a real enabled plugin, whose `.mcp.json`
//! names a real script, and takes the command out of the curated configuration
//! [`rime_agent_core::mcpconf::curate`] produced — the same document the daemon
//! writes and hands the agent. That command is then run. The script tries two
//! things a confined process must not manage:
//!
//!   * read a file in the user's home that is definitely there, and
//!   * connect to a TCP listener that is definitely listening.
//!
//! ## The inverse is the half that makes it a measurement
//!
//! Every probe here is run twice: once through the wrapper the curated
//! configuration produced, and once as the bare command the plugin defined.
//! The bare run has to SUCCEED at both. Without that, a script that failed for
//! its own reasons — a typo, a missing shell, a sentinel that was never
//! written — would report a sandbox that was not there.
//!
//! ## A sandbox that cannot start is a could-not-run, never a pass
//!
//! If `bwrap` is missing or the kernel refuses the user namespace, these tests
//! FAIL and say which. They do not skip. A skipped check counts as a success,
//! which is the failure this repository has recorded more than once — and for
//! this criterion in particular, "the sandbox could not start" and "the
//! sandbox held" are the two answers it exists to tell apart.

use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;

use rime_agent_core::mcpconf::{self, Curated};
use rime_agent_core::policy::ConnectorPolicy;
use serde_json::Value;

/// The `rime` this test drives. The build under test, not whatever is on PATH.
const RIME: &str = env!("CARGO_BIN_EXE_rime");

/// Where `bwrap` is: the fixed path the sandbox module uses, for the reason it
/// gives — a `PATH` lookup would let a shadowing binary decide what this
/// measured.
const BWRAP: &str = rime_paths::BWRAP;

/// A fixture root under `/var/tmp`, and the location is load-bearing.
///
/// Not `std::env::temp_dir()`. A confined process gets a fresh tmpfs over
/// `/tmp`, so a probe script living there is a script the sandbox cannot exec —
/// the run would fail with 127 and look exactly like the confinement working.
/// `/var/tmp` is covered by the read-only root bind, so the script is reachable
/// and the only reason it can fail is the one being measured.
fn fixture(tag: &str) -> PathBuf {
    let dir = PathBuf::from("/var/tmp").join(format!(
        "rime-mcp-launch-{}-{tag}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("fixture root");
    dir
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("parent");
    }
    std::fs::write(path, text).expect("write");
}

fn executable(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    write(path, text);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// One fixture: a home with an enabled plugin that defines one stdio server.
struct Machine {
    root: PathBuf,
    home: PathBuf,
    proj: PathBuf,
    /// The file in the home the probe tries to read.
    sentinel: PathBuf,
}

impl Machine {
    fn build(tag: &str, probe: &str) -> Machine {
        let root = fixture(tag);
        let home = root.join("home");
        let proj = root.join("proj");
        let cache = home.join(".claude/plugins/cache/mp/probe/1.0.0");
        std::fs::create_dir_all(&proj).expect("proj");
        std::fs::create_dir_all(&cache).expect("cache");
        // `build_argv` puts a tmpfs over `$XDG_RUNTIME_DIR`, and bwrap will not
        // create a mount point under a read-only root. On a real machine the
        // directory is already there; here it has to be made, or every probe
        // below fails with "Read-only file system" and looks like a sandbox
        // that held.
        std::fs::create_dir_all(root.join("run-user")).expect("runtime dir");

        let sentinel = home.join("a-file-the-agent-can-read");
        write(&sentinel, "THE-SECRET-THE-AGENT-HAS\n");

        // Outside the home on purpose: see `fixture`.
        let script = root.join("bin/probe.sh");
        executable(&script, probe);

        write(&home.join(".claude.json"), "{}");
        write(
            &home.join(".claude/settings.json"),
            r#"{"enabledPlugins": {"probe@mp": true}}"#,
        );
        write(
            &home.join(".claude/plugins/installed_plugins.json"),
            &serde_json::to_string(&serde_json::json!({
                "version": 2,
                "plugins": {"probe@mp": [{
                    "scope": "user",
                    "installPath": cache.to_string_lossy(),
                    "version": "1.0.0",
                }]},
            }))
            .expect("json"),
        );
        write(
            &cache.join(".mcp.json"),
            &serde_json::to_string(&serde_json::json!({
                "mcpServers": {"probe": {
                    "command": script.to_string_lossy(),
                    "args": [],
                }},
            }))
            .expect("json"),
        );

        Machine {
            root,
            home,
            proj,
            sentinel,
        }
    }

    /// The document the daemon would write for this machine.
    fn curated(&self, policy: ConnectorPolicy, allow: &[String]) -> Curated {
        let defs = mcpconf::read(&self.home, Some(&self.proj));
        let approval = mcpconf::approvals(&self.home, Some(&self.proj));
        mcpconf::curate(&defs, &approval, policy, allow, Some(Path::new(RIME)))
    }

    /// The command and arguments the curated document gives one server.
    fn launch(&self, doc: &Value, name: &str) -> (String, Vec<String>) {
        let def = doc["mcpServers"]
            .get(name)
            .unwrap_or_else(|| panic!("{name} is not in the curated document: {doc}"));
        let command = def["command"].as_str().expect("command").to_string();
        let args = def["args"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        (command, args)
    }

    /// Run a command with this fixture's environment, and nothing of the
    /// caller's that would change what is measured.
    fn run(&self, command: &str, args: &[String], extra: &[(&str, String)]) -> (i32, String) {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .current_dir(&self.proj)
            .env("HOME", &self.home)
            // The private home and the policy directory both land inside the
            // fixture, so a policy file on the machine running this cannot
            // widen what the probe gets.
            .env("XDG_STATE_HOME", self.home.join(".local/state"))
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("XDG_RUNTIME_DIR", self.root.join("run-user"))
            .env("PATH", "/usr/bin:/bin")
            .env("RIME_SENTINEL", &self.sentinel);
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap_or_else(|e| {
            panic!("could not run {command}: {e}");
        });
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.code().unwrap_or(-1), text)
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
    }
}

/// Fail with the reason rather than skipping when nothing can be measured.
fn require_sandbox() {
    assert!(
        Path::new(BWRAP).exists(),
        "bwrap is not at {BWRAP}, so whether a plugin's server is confined COULD NOT BE \
         CHECKED. This is a could-not-run and not a pass: install bubblewrap."
    );
    // A kernel that refuses an unprivileged user namespace makes every probe
    // below fail for a reason that has nothing to do with the confinement, and
    // a run that reported those as passes would be reporting the opposite of
    // what happened.
    let out = Command::new(BWRAP)
        .args(["--ro-bind", "/", "/", "--unshare-user", "--", "/bin/true"])
        .output()
        .expect("run bwrap");
    assert!(
        out.status.success(),
        "bwrap cannot start a user namespace on this machine, so whether a plugin's server \
         is confined COULD NOT BE CHECKED: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The two answers share no substring, and that is not fussiness.
///
/// This probe first printed `READ-THE-HOME` and `COULD-NOT-READ-THE-HOME`, and
/// the confined run FAILED with "a plugin's server read the user's home" while
/// printing the refusal — because the negative answer contains the positive
/// one. `Revision::describe` in provenance.rs carries a comment about exactly
/// this defect in its own wording; it is easy to write twice.
const READ_PROBE: &str = r#"#!/bin/bash
if cat "$RIME_SENTINEL" 2>/dev/null | grep -q THE-SECRET; then
    echo HOME-WAS-READABLE
else
    echo MASKED-AND-EMPTY
fi
"#;

#[test]
fn a_plugins_server_launched_through_the_curated_config_cannot_read_the_users_home() {
    require_sandbox();
    let m = Machine::build("read", READ_PROBE);

    // The control case FIRST, and it is not decoration: the sentinel has to be
    // readable when nothing is confining the script, or the refusal below says
    // nothing at all.
    let bare = m
        .home
        .join(".claude/plugins/cache/mp/probe/1.0.0/.mcp.json");
    let raw: Value = serde_json::from_slice(&std::fs::read(&bare).expect("mcp.json")).expect("json");
    let script = raw["mcpServers"]["probe"]["command"]
        .as_str()
        .expect("command")
        .to_string();
    let (code, text) = m.run(&script, &[], &[]);
    assert_eq!(code, 0, "the plugin's own script did not run at all: {text}");
    assert!(
        text.contains("HOME-WAS-READABLE"),
        "the control case could not read the sentinel, so nothing below is a measurement: {text}"
    );

    // And now the same script, started the way the curated configuration says
    // to start it.
    let curated = m.curated(ConnectorPolicy::AsConfigured, &[]);
    let (command, args) = m.launch(&curated.document, "plugin:probe:probe");
    assert_eq!(command, RIME, "the wrapper is not this build's rime");
    assert_eq!(
        args.first().map(String::as_str),
        Some("mcp"),
        "the curated command is not the sandbox wrapper: {args:?}"
    );

    let (code, text) = m.run(&command, &args, &[]);
    assert_eq!(code, 0, "the confined server did not run: {text}");
    assert!(
        !text.contains("HOME-WAS-READABLE"),
        "a plugin's server read the user's home from inside the sandbox: {text}"
    );
    assert!(
        text.contains("MASKED-AND-EMPTY"),
        "the probe did not reach its own check, so the sandbox was not what stopped it: {text}"
    );
}

#[test]
fn a_plugins_server_launched_through_the_curated_config_cannot_reach_the_network() {
    require_sandbox();

    // A listener that is definitely listening. The failure mode of a weaker
    // test is that nothing was there and the connection would have failed
    // anyway.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            s.write_all(b"hello\n").ok();
        }
    });

    let probe = format!(
        r#"#!/bin/bash
if exec 3<>/dev/tcp/127.0.0.1/{port}; then echo CONNECTED; else echo REFUSED; fi
"#
    );
    let m = Machine::build("net", &probe);

    let script = m
        .home
        .join(".claude/plugins/cache/mp/probe/1.0.0/.mcp.json");
    let raw: Value =
        serde_json::from_slice(&std::fs::read(&script).expect("mcp.json")).expect("json");
    let bare = raw["mcpServers"]["probe"]["command"]
        .as_str()
        .expect("command")
        .to_string();
    let (_, text) = m.run(&bare, &[], &[]);
    assert!(
        text.contains("CONNECTED"),
        "the control case did not connect, so the refusal below is not a measurement: {text}"
    );

    let curated = m.curated(ConnectorPolicy::AsConfigured, &[]);
    let (command, args) = m.launch(&curated.document, "plugin:probe:probe");
    let (_, text) = m.run(&command, &args, &[]);
    assert!(
        !text.contains("CONNECTED"),
        "a plugin's server reached a listener from inside the sandbox: {text}"
    );
    assert!(
        text.contains("REFUSED"),
        "the probe did not reach its own check: {text}"
    );
}

#[test]
fn the_curated_document_is_what_the_agent_would_be_given_and_the_wrapper_is_not_cosmetic() {
    // The gap this closes: the two tests above prove the wrapped command is
    // confined. They would still pass if the runtime never handed that command
    // to anything. So this asserts the document itself — the flags, the file,
    // and the fact that the definition inside it is not the plugin's own.
    let m = Machine::build("doc", READ_PROBE);
    let curated = m.curated(ConnectorPolicy::AsConfigured, &[]);

    let adapter = rime_agent_core::adapter::by_id("claude").expect("claude adapter");
    let path = mcpconf::config_path(Path::new("/run/user/1000/rime/s1"));
    let flags = adapter.mcp_config_args(&path);
    assert_eq!(
        flags,
        vec![
            "--strict-mcp-config".to_string(),
            "--mcp-config".to_string(),
            path.to_string_lossy().into_owned(),
        ],
        "the curated file has to arrive with the flag that makes it exclusive"
    );

    let plugin_def: Value = {
        let file = m
            .home
            .join(".claude/plugins/cache/mp/probe/1.0.0/.mcp.json");
        let raw: Value =
            serde_json::from_slice(&std::fs::read(file).expect("mcp.json")).expect("json");
        raw["mcpServers"]["probe"].clone()
    };
    let given = &curated.document["mcpServers"]["plugin:probe:probe"];
    assert_ne!(
        *given, plugin_def,
        "the curated document handed the agent the plugin's own unwrapped definition"
    );
    assert_eq!(curated.confined(), 1);
    assert!(!curated.has_unconfined_program());
}

#[test]
fn a_cloud_connector_can_be_removed_one_at_a_time() {
    // P1-028's second criterion, end to end and at the level that matters:
    // BEFORE this, the only reduction Rime had was `--sandbox strict` removing
    // the session's network, which takes every cloud connector at once. Two
    // endpoints, one named, one not, and the document keeps exactly one.
    let m = Machine::build("select", READ_PROBE);
    write(
        &m.home.join(".claude.json"),
        &serde_json::to_string(&serde_json::json!({
            "mcpServers": {
                "keepme": {"type": "http", "url": "https://a.example/mcp"},
                "dropme": {"type": "http", "url": "https://b.example/mcp"},
            },
        }))
        .expect("json"),
    );

    let all = m.curated(ConnectorPolicy::AsConfigured, &[]);
    let before = all.document["mcpServers"].as_object().expect("object");
    assert!(before.contains_key("keepme") && before.contains_key("dropme"));

    let curated = m.curated(ConnectorPolicy::Curated, &["keepme".to_string()]);
    let after = curated.document["mcpServers"].as_object().expect("object");
    assert!(after.contains_key("keepme"), "the named one went: {after:?}");
    assert!(
        !after.contains_key("dropme"),
        "the unnamed one survived: {after:?}"
    );
    // And the plugin's program went too, because it was not named either —
    // `curated` is a list, not a plane.
    assert!(!after.contains_key("plugin:probe:probe"));

    // The coarse switch still works and is a different answer: every endpoint
    // goes, every program stays.
    let local = m.curated(ConnectorPolicy::LocalOnly, &[]);
    let after = local.document["mcpServers"].as_object().expect("object");
    assert!(!after.contains_key("keepme") && !after.contains_key("dropme"));
    assert!(after.contains_key("plugin:probe:probe"));
}
