//! `clipboard_write` and `save_file` (contract §1.8), end to end against a
//! real `rime-agentd`: the stub, the real path through fake tools, the
//! takeover framing, and the refusal to a session.
//!
//! Every daemon here has its own HOME, runtime dir and PATH. The real-path
//! tests put a fake `wl-copy` first on PATH and a fake compositor socket in
//! the runtime dir, so the developer's clipboard and Downloads are never
//! reached.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use rime_agent_core::policy::AgentPolicy;
use rime_agent_core::protocol::{Request, RunRequest, SandboxPolicy};

/// Records its argv and stdin, so a test can tell what the daemon sent and
/// that the text never went into argv.
const FAKE_WL_COPY: &str = r#"#!/bin/sh
d="${XDG_RUNTIME_DIR:-/nonexistent}"
printf '%s\n' "$*" >> "$d/wl-copy-args"
cat > "$d/wl-copy-stdin"
exit 0
"#;

struct Daemon {
    child: Child,
    socket: PathBuf,
    root: PathBuf,
    runtime: PathBuf,
    home: PathBuf,
    stub: Option<PathBuf>,
    _seat: UnixListener,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Daemon {
    fn start(tag: &str, stubbed: bool) -> Option<Daemon> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!("rime-desk-e2e-{}-{tag}-{nanos}", std::process::id()));
        let runtime = root.join("run");
        let home = root.join("home");
        let bin = root.join("bin");
        for d in [&runtime, &home, &bin, &root.join("state")] {
            std::fs::create_dir_all(d).ok()?;
        }
        let tool = bin.join("wl-copy");
        std::fs::write(&tool, FAKE_WL_COPY).ok()?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).ok()?;
        }
        let seat = UnixListener::bind(runtime.join("wayland-0")).ok()?;
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
        let stub = stubbed.then(|| root.join("desk"));
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_rime-agentd"));
        cmd.env("XDG_RUNTIME_DIR", &runtime)
            .env("XDG_STATE_HOME", root.join("state"))
            .env("HOME", &home)
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("RIME_AGENTD_DESKTOP_STUB")
            .env("PATH", &path)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(s) = &stub {
            cmd.env("RIME_AGENTD_DESKTOP_STUB", s);
        }
        let child = cmd.spawn().ok()?;
        let socket = runtime.join("rime-agentd").join("control.sock");
        let d = Daemon { child, socket, root, runtime, home, stub, _seat: seat };
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if UnixStream::connect(&d.socket).is_ok() {
                return Some(d);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        None
    }

    fn connect(&self) -> UnixStream {
        let s = UnixStream::connect(&self.socket).expect("connect");
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        s
    }

    fn call(&self, req: &Request) -> serde_json::Value {
        let mut s = self.connect();
        writeln!(s, "{}", serde_json::to_string(req).unwrap()).unwrap();
        let mut line = String::new();
        BufReader::new(&s).read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line}"))
    }

    /// `save_file` with the request and the body in ONE write, as a client
    /// may send them. Returns the first reply and, if it was `saving`, the
    /// second.
    fn save(&self, name: &str, body: &[u8]) -> (serde_json::Value, Option<serde_json::Value>) {
        let mut s = self.connect();
        let req = serde_json::to_string(&Request::SaveFile { name: name.into(), len: body.len() as u64 }).unwrap();
        let mut out = format!("{req}\n").into_bytes();
        out.extend_from_slice(body);
        s.write_all(&out).unwrap();
        let mut r = BufReader::new(s);
        let mut first = String::new();
        r.read_line(&mut first).unwrap();
        let first: serde_json::Value = serde_json::from_str(&first).unwrap();
        if first["reply"] != "saving" {
            return (first, None);
        }
        let mut second = String::new();
        r.read_line(&mut second).unwrap();
        (first, Some(serde_json::from_str(&second).unwrap()))
    }

    fn wl_copy_ran(&self) -> bool {
        self.runtime.join("wl-copy-args").exists()
    }

    fn run_shell(&self, script: &str) -> Option<u32> {
        let reply = self.call(&Request::Run(Box::new(RunRequest {
            agent: Some("generic".into()),
            name: None,
            prompt: None,
            args: vec!["/bin/sh".into(), "-c".into(), script.into()],
            cwd: "/tmp".into(),
            policy: AgentPolicy { sandbox: SandboxPolicy::Unrestricted, ..AgentPolicy::default() },
            request_origin: None,
            worktree: None,
            checkpoint: false,
            ttl_ms: None,
            capabilities: None,
            allow: None,
            trust_ca: None,
            present: None,
            second_factor: None,
            cols: 80,
            rows: 24,
            env: vec![],
            disposable: false,
            copy_out: None,
        })));
        if reply["reply"] != "session" {
            eprintln!("SKIP: the daemon would not start a session: {reply}");
            return None;
        }
        Some(reply["id"].as_u64().unwrap_or_else(|| panic!("no id: {reply}")) as u32)
    }

    fn logs_until(&self, id: u32, marker: &str, ms: u64) -> String {
        let deadline = Instant::now() + Duration::from_millis(ms);
        loop {
            let reply = self.call(&Request::Logs { id, bytes: 64 * 1024 });
            let text = reply["text"].as_str().unwrap_or_default().to_string();
            if text.matches(marker).count() >= 2 || Instant::now() >= deadline {
                return text;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

macro_rules! daemon {
    ($tag:literal, $stub:expr) => {
        match Daemon::start($tag, $stub) {
            Some(d) => d,
            None => {
                eprintln!("SKIP: rime-agentd did not come up in this environment");
                return;
            }
        }
    };
}

fn files(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|r| r.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

#[test]
fn hello_offers_both_verbs() {
    let d = daemon!("hello", true);
    let reply = d.call(&Request::Hello);
    let features: Vec<&str> = reply["features"].as_array().unwrap().iter().filter_map(|f| f.as_str()).collect();
    assert!(features.contains(&"clipboard_write") && features.contains(&"save_file"), "{reply}");
}

#[test]
fn with_the_stub_the_clipboard_is_a_file_and_wl_copy_never_runs() {
    let d = daemon!("stubclip", true);
    let text = "two\nlines \"quoted\"";
    let reply = d.call(&Request::ClipboardWrite { text: text.into() });
    assert_eq!(reply["reply"], "clipboard_written", "{reply}");
    assert_eq!(reply["bytes"], text.len());
    let stub = d.stub.clone().unwrap();
    assert_eq!(std::fs::read_to_string(stub.join("clipboard")).unwrap(), text);
    assert!(!d.wl_copy_ran(), "the stub must keep the daemon off the real clipboard");
}

#[test]
fn without_the_stub_the_text_goes_to_wl_copy_on_stdin_never_in_argv() {
    let d = daemon!("realclip", false);
    let text = "secret-ish text; not for argv";
    let reply = d.call(&Request::ClipboardWrite { text: text.into() });
    assert_eq!(reply["reply"], "clipboard_written", "{reply}");
    let args = std::fs::read_to_string(d.runtime.join("wl-copy-args")).unwrap();
    assert_eq!(args.trim(), "--type text/plain;charset=utf-8");
    assert!(!args.contains("secret"), "the text reached argv: {args}");
    assert_eq!(std::fs::read_to_string(d.runtime.join("wl-copy-stdin")).unwrap(), text);
}

#[test]
fn an_empty_or_oversize_clipboard_write_is_refused_and_nothing_runs() {
    let d = daemon!("cliprefuse", false);
    let empty = d.call(&Request::ClipboardWrite { text: String::new() });
    assert_eq!(empty["kind"], "bad_request", "{empty}");
    let big = d.call(&Request::ClipboardWrite { text: "x".repeat(8 * 1024 + 1) });
    assert_eq!(big["kind"], "bad_request", "{big}");
    assert!(big["message"].as_str().unwrap().contains("8192"), "{big}");
    assert!(!d.wl_copy_ran());
}

#[test]
fn a_file_is_saved_whole_under_its_own_name_and_never_over_another() {
    let d = daemon!("save", true);
    let downloads = d.stub.clone().unwrap().join("Downloads");
    let body: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let (first, second) = d.save("../../IMG_2041.jpg", &body);
    assert_eq!(first["reply"], "saving", "{first}");
    assert_eq!(first["len"], body.len());
    let second = second.unwrap();
    assert_eq!(second["reply"], "saved", "{second}");
    assert_eq!(second["name"], "IMG_2041.jpg");
    assert_eq!(std::fs::read(downloads.join("IMG_2041.jpg")).unwrap(), body);
    let (_, again) = d.save("IMG_2041.jpg", b"second");
    assert_eq!(again.unwrap()["name"], "IMG_2041 (2).jpg");
    assert_eq!(std::fs::read(downloads.join("IMG_2041.jpg")).unwrap(), body, "the first copy was replaced");
    assert_eq!(files(&downloads), vec!["IMG_2041 (2).jpg", "IMG_2041.jpg"], "a temporary file was left");
}

#[test]
fn without_the_stub_files_go_to_the_users_download_folder() {
    let d = daemon!("realsave", false);
    std::fs::create_dir_all(d.home.join(".config")).unwrap();
    std::fs::write(
        d.home.join(".config/user-dirs.dirs"),
        "XDG_DOWNLOAD_DIR=\"$HOME/Telechargements\"\n",
    )
    .unwrap();
    let (_, second) = d.save("note.txt", b"hello");
    let second = second.expect("saving");
    assert_eq!(second["reply"], "saved", "{second}");
    let want = d.home.join("Telechargements/note.txt");
    assert_eq!(second["path"], want.to_string_lossy().as_ref());
    assert_eq!(std::fs::read(want).unwrap(), b"hello");
}

#[test]
fn an_oversize_or_empty_file_is_refused_before_the_takeover() {
    let d = daemon!("saverefuse", true);
    let mut s = d.connect();
    writeln!(s, r#"{{"cmd":"save_file","name":"big.iso","len":{}}}"#, 256u64 * 1024 * 1024 + 1).unwrap();
    let mut line = String::new();
    BufReader::new(&s).read_line(&mut line).unwrap();
    let reply: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(reply["kind"], "bad_request", "{reply}");
    let (zero, _) = d.save("empty.txt", b"");
    assert_eq!(zero["kind"], "bad_request", "{zero}");
    assert!(files(&d.stub.clone().unwrap().join("Downloads")).is_empty());
}

#[test]
fn an_upload_that_stops_half_way_leaves_nothing() {
    let d = daemon!("savehalf", true);
    let mut s = d.connect();
    writeln!(s, r#"{{"cmd":"save_file","name":"half.bin","len":1000}}"#).unwrap();
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    assert!(line.contains("saving"), "{line}");
    s.write_all(&[7u8; 400]).unwrap();
    s.shutdown(std::net::Shutdown::Write).unwrap();
    let mut rest = String::new();
    r.read_to_string(&mut rest).unwrap();
    assert!(rest.contains("nothing was saved"), "{rest}");
    assert!(files(&d.stub.clone().unwrap().join("Downloads")).is_empty());
}

#[test]
fn a_session_may_not_set_the_clipboard_or_save_a_file() {
    if Command::new("python3").arg("-c").arg("import socket").stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| !s.success()).unwrap_or(true) {
        eprintln!("SKIP: no python3 with sockets");
        return;
    }
    let d = daemon!("session", true);
    let sock = d.socket.display().to_string();
    let prog = format!(
        "import socket\n\
         for line in [b'{{\"cmd\":\"clipboard_write\",\"text\":\"from an agent\"}}\\n', b'{{\"cmd\":\"save_file\",\"name\":\"x.sh\",\"len\":3}}\\nabc']:\n\
         \x20   s = socket.socket(socket.AF_UNIX)\n\
         \x20   s.connect({sock:?})\n\
         \x20   s.sendall(line)\n\
         \x20   print('REPLY', s.makefile().readline().strip())\n"
    );
    let script = format!("python3 -c '{}' 2>&1; echo DONE", prog.replace('\'', r"'\''"));
    let Some(id) = d.run_shell(&script) else { return };
    let logs = d.logs_until(id, "REPLY", 20_000);
    assert_eq!(logs.matches("permission_denied").count(), 2, "a session reached the desktop: {logs:?}");
    let stub = d.stub.clone().unwrap();
    assert!(!stub.join("clipboard").exists(), "the clipboard was set");
    assert!(files(&stub.join("Downloads")).is_empty(), "a file was saved");
}
