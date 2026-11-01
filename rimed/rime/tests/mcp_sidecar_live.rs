//! The MCP sidecar with `bwrap` actually running, not just its argv.
//!
//! The unit tests in `mcp::sidecar` assert the argv, which is what stops a
//! policy dimension from silently disappearing. They cannot say whether the
//! flags do what they are believed to do — that a masked home is really
//! unreadable, that `--unshare-net` really stops a connection to a listener
//! that is definitely there, that `--clearenv` really drops a variable the
//! parent had. Those are facts about the kernel and about bubblewrap 0.12, and
//! the only way to have them is to run one.
//!
//! Nothing here touches the agent runtime, the secret service or the user's
//! configuration. Every path is under a fixture directory this test made.
//!
//! ## The nesting property
//!
//! An MCP server started by a confined agent is a sandbox inside a sandbox, and
//! the test that matters is that the inner one cannot undo the outer: a policy
//! that says `network = true` inside a session that has no network still has
//! none, because a network namespace cannot be un-shared upward. That is
//! checked here by running the sidecar inside an outer `bwrap --unshare-net`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Where `bwrap` is. The same fixed path the sandbox module uses, and for the
/// same reason: a `PATH` lookup would let a shadowing binary decide what this
/// test measured.
const BWRAP: &str = rime_paths::BWRAP;

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rime-mcp-live-{}-{tag}-{:?}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
    ));
    std::fs::create_dir_all(&dir).expect("fixture");
    dir
}

/// The sidecar's argv for a policy, built the way `rime mcp run` builds it.
///
/// A copy of the wiring rather than a call into it: `rime` is a binary crate,
/// so an integration test cannot reach `mcp::sidecar` directly. What is being
/// measured is the *flags*, and the flags are asserted against the real ones in
/// the unit tests — this file exists to find out what they do.
fn sandbox_argv(
    user_home: &Path,
    server_home: &Path,
    runtime_dir: &Path,
    network: bool,
    program: &str,
    args: &[&str],
) -> Vec<String> {
    let mut a: Vec<String> = vec![
        BWRAP.into(),
        "--ro-bind".into(), "/".into(), "/".into(),
        "--proc".into(), "/proc".into(),
        "--dev".into(), "/dev".into(),
        "--tmpfs".into(), "/tmp".into(),
        "--tmpfs".into(), user_home.to_string_lossy().into_owned(),
        "--tmpfs".into(), "/run".into(),
        "--tmpfs".into(), runtime_dir.to_string_lossy().into_owned(),
        "--bind-try".into(),
        server_home.to_string_lossy().into_owned(),
        server_home.to_string_lossy().into_owned(),
        "--unshare-pid".into(),
        "--unshare-ipc".into(),
        "--unshare-uts".into(),
    ];
    if !network {
        a.push("--unshare-net".into());
    }
    a.push("--clearenv".into());
    for (k, v) in [
        ("HOME", server_home.to_string_lossy().into_owned()),
        ("PATH", "/usr/bin:/bin".to_string()),
    ] {
        a.push("--setenv".into());
        a.push(k.into());
        a.push(v);
    }
    a.push("--chdir".into());
    a.push(server_home.to_string_lossy().into_owned());
    a.push("--die-with-parent".into());
    a.push("--".into());
    a.push(program.into());
    a.extend(args.iter().map(|s| (*s).to_string()));
    a
}

fn run(argv: &[String]) -> (i32, String) {
    let out = Command::new(&argv[0])
        .args(&argv[1..])
        .output()
        .expect("run bwrap");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), text)
}

#[test]
fn a_confined_server_cannot_read_the_home_the_agent_can() {
    // The filesystem dimension, measured. The fixture home holds a file that
    // stands in for `~/.claude.json` and `~/.ssh/id_ed25519`: readable outside,
    // and not there at all inside, because the mask is a mount and not a
    // permission.
    let root = fixture("fs");
    let user_home = root.join("home");
    let server_home = user_home.join(".local/state/rime/agent/mcp/memory");
    std::fs::create_dir_all(&server_home).expect("server home");
    let secret = user_home.join("the-agents-own-file");
    std::fs::write(&secret, "a token the agent has").expect("write");

    assert!(secret.exists(), "the fixture is not set up");

    let argv = sandbox_argv(
        &user_home,
        &server_home,
        &root.join("run-user"),
        false,
        "/bin/sh",
        &["-c", "cat \"$1\" 2>&1 || true; echo rc=$?; ls -A \"$2\" 2>&1", "sh"],
    );
    let mut argv = argv;
    argv.push(secret.to_string_lossy().into_owned());
    argv.push(user_home.to_string_lossy().into_owned());
    let (code, text) = run(&argv);
    assert_eq!(code, 0, "{text}");
    assert!(
        !text.contains("a token the agent has"),
        "the masked home was readable: {text}"
    );
    assert!(text.contains("No such file"), "{text}");
    // The server's own directory is the one thing inside the mask, and it is
    // writable.
    let argv = sandbox_argv(
        &user_home,
        &server_home,
        &root.join("run-user"),
        false,
        "/bin/sh",
        &["-c", "echo mine > ./notes && cat ./notes"],
    );
    let (code, text) = run(&argv);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("mine"), "{text}");
    assert!(server_home.join("notes").exists(), "the write did not reach the disk");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn a_confined_server_with_no_network_cannot_reach_a_listener_that_is_there() {
    // The network dimension, measured against a socket that is definitely
    // listening — the failure mode of a weaker test is that nothing was
    // listening and the connection would have failed anyway.
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            drop(stream);
        }
    });

    let root = fixture("net");
    let user_home = root.join("home");
    let server_home = user_home.join("mcp/remote");
    std::fs::create_dir_all(&server_home).expect("dirs");
    let probe = format!(
        "if exec 3<>/dev/tcp/127.0.0.1/{port}; then echo CONNECTED; else echo REFUSED; fi"
    );

    // Without the flag, the same probe against the same listener connects. That
    // is what makes the refusal below a measurement rather than a coincidence.
    let (code, text) = run(&sandbox_argv(
        &user_home,
        &server_home,
        &root.join("run-user"),
        true,
        "/bin/bash",
        &["-c", &probe],
    ));
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("CONNECTED"), "the control case did not connect: {text}");

    let (_, text) = run(&sandbox_argv(
        &user_home,
        &server_home,
        &root.join("run-user"),
        false,
        "/bin/bash",
        &["-c", &probe],
    ));
    assert!(!text.contains("CONNECTED"), "the default reached the network: {text}");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn a_server_that_asks_for_the_network_inside_an_offline_session_still_has_none() {
    // The nesting property, and the reason `network = true` is a ceiling rather
    // than a grant: an MCP server cannot be a way out of a session that was
    // confined without a network. A namespace cannot be un-shared upward, so
    // this is a fact about the kernel — which is exactly why it is worth
    // measuring rather than asserting.
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            drop(stream);
        }
    });

    let root = fixture("nest");
    let user_home = root.join("home");
    let server_home = user_home.join("mcp/remote");
    std::fs::create_dir_all(&server_home).expect("dirs");
    let probe = format!(
        "if exec 3<>/dev/tcp/127.0.0.1/{port}; then echo CONNECTED; else echo REFUSED; fi"
    );

    // The inner sandbox asks for the network. The outer one has none.
    let inner = sandbox_argv(
        &user_home,
        &server_home,
        &root.join("run-user"),
        true,
        "/bin/bash",
        &["-c", &probe],
    );
    let mut outer: Vec<String> = vec![
        BWRAP.into(),
        "--ro-bind".into(), "/".into(), "/".into(),
        "--proc".into(), "/proc".into(),
        "--dev".into(), "/dev".into(),
        "--unshare-net".into(),
        "--".into(),
    ];
    outer.extend(inner);
    let (_, text) = run(&outer);
    assert!(
        !text.contains("CONNECTED"),
        "a nested sandbox restored a network the session did not have: {text}"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn a_variable_the_parent_has_does_not_reach_a_confined_server() {
    // `--clearenv` and then an explicit set. The parent here carries something
    // shaped like a credential, which is the case that matters: an agent's
    // environment is where one would be.
    let root = fixture("env");
    let user_home = root.join("home");
    let server_home = user_home.join("mcp/m");
    std::fs::create_dir_all(&server_home).expect("dirs");

    let argv = sandbox_argv(
        &user_home,
        &server_home,
        &root.join("run-user"),
        false,
        "/bin/sh",
        &["-c", "env"],
    );
    let out = Command::new(&argv[0])
        .args(&argv[1..])
        .env("RIME_LIVE_TEST_TOKEN", "sentinel-must-not-appear")
        .output()
        .expect("run");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        !text.contains("sentinel-must-not-appear"),
        "a parent variable reached the server: {text}"
    );
    assert!(text.contains("HOME="), "{text}");
    std::fs::remove_dir_all(&root).ok();
}

// ---------------------------------------------------------------------------
// The real `rime mcp run`, not a copy of its flags.
//
// Everything above hand-builds an argv, which measures bubblewrap and not this
// codebase: the flags are asserted against the real ones by the unit tests, and
// a copy that drifted would keep passing. The tests below spawn the shipped
// binary, hand it a stdio MCP server and talk JSON-RPC to it through the
// sandbox — so the transport, the exec, the working directory, `HOME`, and the
// two dimensions that are sockets are all measured end to end.
//
// The server is a double. It speaks enough of MCP to complete an `initialize`
// handshake and reports what it can see from inside; a real third-party server
// would prove nothing more about the sandbox and would need a package registry.
// ---------------------------------------------------------------------------

/// A stdio MCP server that answers `initialize` and says what it could reach.
const DOUBLE: &str = r#"
import json, os, socket, sys

agent_file, agentd_socket = sys.argv[1], sys.argv[2]

def reachable(path):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        s.connect(path)
        return True
    except OSError:
        return False
    finally:
        s.close()

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    message = json.loads(line)
    if message.get("method") != "initialize":
        continue
    sys.stdout.write(json.dumps({
        "jsonrpc": "2.0",
        "id": message.get("id"),
        "result": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "serverInfo": {"name": "double", "version": "0"},
            "_saw": {
                "home": os.environ.get("HOME"),
                "cwd": os.getcwd(),
                "agentFile": os.path.exists(agent_file),
                "parentVariable": "RIME_LIVE_SECRET" in os.environ,
                "broker": reachable(agentd_socket),
            },
        },
    }) + "\n")
    sys.stdout.flush()
    break
"#;

/// A fixture holding every directory `rime mcp run` reads, and nothing of the
/// user's.
///
/// A struct rather than the six paths loose, so the helper below takes one
/// argument instead of eight — and so a caller cannot pass `config` where
/// `state` belongs, which four `&Path` parameters in a row invite.
struct Sidecar {
    root: PathBuf,
    home: PathBuf,
    state: PathBuf,
    config: PathBuf,
    runtime: PathBuf,
    server_home: PathBuf,
    agent_file: PathBuf,
    /// Where `rime-agentd`'s socket would be. The `broker` dimension is
    /// whether this is reachable from inside.
    agentd_socket: PathBuf,
}

impl Sidecar {
    fn new(tag: &str, name: &str) -> Sidecar {
        let root = fixture(tag);
        let (home, state, config, runtime) = (
            root.join("home"),
            root.join("state"),
            root.join("config"),
            root.join("run-user"),
        );
        let server_home = state.join("rime/agent/mcp").join(name);
        for dir in [&home, &config, &runtime, &server_home] {
            std::fs::create_dir_all(dir).expect("fixture dir");
        }
        // The double lives in the server's own home, because that is the one
        // place inside the sandbox that exists: `/tmp` is masked and the
        // fixture is under it.
        std::fs::write(server_home.join("server.py"), DOUBLE).expect("double");
        let agent_file = home.join("the-agents-own-file");
        std::fs::write(&agent_file, "a token the agent has").expect("write");
        Sidecar {
            agentd_socket: runtime.join("rime-agentd/control.sock"),
            root,
            home,
            state,
            config,
            runtime,
            server_home,
            agent_file,
        }
    }

    /// Run `rime mcp run <name> -- python3 server.py …`, send one message, and
    /// return (exit code, stdout, stderr).
    fn talk(&self, name: &str, message: &str) -> (i32, String, String) {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rime"))
            .args([
                "mcp",
                "run",
                name,
                "--",
                "/usr/bin/python3",
                "server.py",
                &self.agent_file.to_string_lossy(),
                &self.agentd_socket.to_string_lossy(),
            ])
            // Cleared and rebuilt, so nothing of the developer's environment
            // can decide what this measures — and so `RIME_LIVE_SECRET` is the
            // only interesting thing in it.
            .env_clear()
            .env("HOME", &self.home)
            .env("XDG_STATE_HOME", &self.state)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env("PATH", "/usr/bin:/bin")
            .env("RIME_LIVE_SECRET", "sentinel-must-not-appear")
            .current_dir(&self.home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn rime mcp run");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(format!("{message}\n").as_bytes())
            .expect("write the message");
        let out = child.wait_with_output().expect("wait");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// The `_saw` block a completed handshake carries.
    fn saw(&self, stdout: &str, stderr: &str) -> serde_json::Value {
        let reply: serde_json::Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("{e}: stdout={stdout} stderr={stderr}"));
        assert_eq!(reply["id"], 1, "{stdout}");
        assert_eq!(reply["result"]["serverInfo"]["name"], "double", "{stdout}");
        reply["result"]["_saw"].clone()
    }
}

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"live-test","version":"0"}}}"#;

#[test]
fn the_shipped_command_runs_a_stdio_mcp_server_and_its_handshake_still_completes() {
    // P1-019's second criterion, measured rather than asserted: a server that
    // no longer inherits the agent's sandbox must still *work*. The whole of
    // the default policy is exercised — the mask, the private home, the cleared
    // environment, both closed sockets — by a server that talks MCP through it.
    let s = Sidecar::new("handshake", "demo");
    let (code, stdout, stderr) = s.talk("demo", INITIALIZE);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let saw = s.saw(&stdout, &stderr);

    // `HOME` is the server's own directory and the working directory is it too,
    // so a server that writes beside itself writes there.
    assert_eq!(saw["home"], s.server_home.to_string_lossy().as_ref(), "{stdout}");
    assert_eq!(saw["cwd"], s.server_home.to_string_lossy().as_ref(), "{stdout}");
    // The three closed dimensions, from inside.
    assert_eq!(saw["agentFile"], false, "the masked home was readable: {stdout}");
    assert_eq!(saw["parentVariable"], false, "a parent variable survived: {stdout}");
    assert_eq!(saw["broker"], false, "the broker was reachable by default: {stdout}");
    std::fs::remove_dir_all(&s.root).ok();
}

#[test]
fn the_broker_dimension_opens_the_runtime_socket_and_only_when_asked() {
    // The dimension that was wrong until this branch measured it. `broker` used
    // to bind `rime-secretd`'s socket, which is not the one anything inside a
    // sandbox connects to — `rime secret use` and `rime mcp bridge` both talk
    // to `rime-agentd`, under `$XDG_RUNTIME_DIR`. The argv test says which path
    // is in the flags; this says the flag opens a socket that is demonstrably
    // accepting, which is what the network test does and for the same reason:
    // without a live listener, "could not connect" proves nothing.
    let s = Sidecar::new("broker", "demo");
    std::fs::create_dir_all(s.runtime.join("rime-agentd")).expect("runtime dir");
    let listener = std::os::unix::net::UnixListener::bind(&s.agentd_socket).expect("listen");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            drop(stream);
        }
    });

    // Closed, against the listener that is definitely there.
    let (code, stdout, stderr) = s.talk("demo", INITIALIZE);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert_eq!(
        s.saw(&stdout, &stderr)["broker"],
        false,
        "the default reached the runtime: {stdout}"
    );

    // The same fixture, one line of policy.
    std::fs::create_dir_all(s.config.join("rime/mcp")).expect("policy dir");
    std::fs::write(s.config.join("rime/mcp/demo.toml"), "broker = true\n").expect("policy");
    let (code, stdout, stderr) = s.talk("demo", INITIALIZE);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let saw = s.saw(&stdout, &stderr);
    assert_eq!(saw["broker"], true, "broker = true did not open the socket: {stdout}");
    // And nothing else moved: the home is still masked when the broker opens.
    assert_eq!(saw["agentFile"], false, "{stdout}");
    std::fs::remove_dir_all(&s.root).ok();
}
