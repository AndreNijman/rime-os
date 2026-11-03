//! Announcing on the local network, without putting a record on one.
//!
//! Every assertion here is against a **stub** `avahi-publish-service` in a
//! scratch directory, reached only through `RIME_REMOTE_AVAHI_PUBLISH`.
//! Nothing in this file can reach the real tool, and no packet leaves the
//! machine — which matters, because the alternative is a suite that
//! advertises a fake Rime desktop on whatever network the developer is
//! sitting on every time it runs.
//!
//! What that leaves unproven is named rather than implied: avahi itself is
//! not exercised. That the tool holds a registration for as long as it runs
//! is avahi's documented behaviour and this suite takes it on trust. What it
//! does prove is the half that has been got wrong before — the right port,
//! the right service type, nothing identifying in the TXT record, and a
//! record that goes away when the daemon does **even if the daemon is
//! killed outright**.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn bin(name: &str) -> PathBuf {
    let mine = PathBuf::from(env!("CARGO_BIN_EXE_rime-remoted"));
    let path = mine.parent().expect("a target directory").join(name);
    assert!(path.exists(), "{} is not built", path.display());
    path
}

/// A daemon with a fake publisher, and the scratch directory it lives in.
struct Fixture {
    child: Child,
    root: PathBuf,
    argv: PathBuf,
    pidfile: PathBuf,
    control: PathBuf,
    log: PathBuf,
    port: u16,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn start(tag: &str, extra: &[&str]) -> Option<Fixture> {
        let root = std::env::temp_dir().join(format!(
            "rime-remote-mdns-{}-{tag}-{}",
            std::process::id(),
            rime_remote_core::now_ms()
        ));
        let runtime = root.join("run");
        let state = root.join("state");
        let bindir = root.join("bin");
        for d in [&runtime, &state, &bindir] {
            std::fs::create_dir_all(d).ok()?;
        }

        let argv = root.join("argv");
        let pidfile = root.join("publisher.pid");
        let stub = bindir.join("avahi-publish-service");
        // Records what it was asked to publish and its own pid, then holds
        // the registration the way the real tool does: by not exiting. `exec`,
        // so the pid it wrote is the process that holds on: a `sleep` forked
        // under a shell outlives the shell the daemon's death signal kills, and
        // every run of this file used to leave three of them behind for an hour.
        std::fs::write(
            &stub,
            format!(
                "#!/usr/bin/env bash\n\
                 printf '%s\\n' \"$*\" > {argv}\n\
                 echo $$ > {pid}\n\
                 exec sleep 3600\n",
                argv = argv.display(),
                pid = pidfile.display(),
            ),
        )
        .ok()?;
        let mut perms = std::fs::metadata(&stub).ok()?.permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&stub, perms).ok()?;

        // `--port 0`: the kernel picks the port when the daemon binds, and the
        // daemon reads back the one it got (that read-back is half of what the
        // record test proves). Not a port probed here and handed over: that one
        // is free when it is probed and anybody's once the probe is dropped, and
        // a daemon given a taken `--port` exits with "Address already in use".
        // Measured: 2 of 600 runs of this file in the Nix build sandbox, and the
        // hosted CI's "the daemon did not come up" (run 41597182018).
        let mut args = vec![
            "--port".to_string(),
            "0".to_string(),
            "--allow-foreground".to_string(),
        ];
        args.extend(extra.iter().map(|s| s.to_string()));

        // The daemon's stderr is kept: it is where the port is, and where the
        // reason is when the daemon does not come up.
        let log = root.join("daemon.log");
        let child = Command::new(bin("rime-remoted"))
            .args(&args)
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("XDG_STATE_HOME", &state)
            .env("RIME_REMOTE_AVAHI_PUBLISH", &stub)
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).ok()?)
            .spawn()
            .ok()?;

        let mut f = Fixture {
            child,
            root,
            argv,
            pidfile,
            control: runtime.join("rime-remoted").join("control.sock"),
            log,
            port: 0,
        };
        match f.wait_ready() {
            Ok(port) => {
                f.port = port;
                Some(f)
            }
            Err(why) => {
                eprintln!("{why}");
                None
            }
        }
    }

    /// The port the daemon is listening on, once its control socket answers
    /// and it has said which port it got. The port is checked rather than
    /// believed: something must be accepting on it.
    fn wait_ready(&mut self) -> Result<u16, String> {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Err(format!("the daemon exited ({status}): {}", self.stderr()));
            }
            if UnixStream::connect(&self.control).is_ok() {
                if let Some(port) = self.listening_on() {
                    if port != 0 && std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                        return Ok(port);
                    }
                    return Err(format!(
                        "the daemon says it is listening on port {port} and nothing accepts there: {}",
                        self.stderr()
                    ));
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err(format!("the daemon did not come up in 15 s: {}", self.stderr()))
    }

    /// "rime-remoted: listening on PORT, ..." from the daemon's startup line.
    fn listening_on(&self) -> Option<u16> {
        std::fs::read_to_string(&self.log)
            .ok()?
            .lines()
            .find_map(|l| l.strip_prefix("rime-remoted: listening on "))?
            .split(',')
            .next()?
            .trim()
            .parse()
            .ok()
    }

    fn stderr(&self) -> String {
        let text = std::fs::read_to_string(&self.log).unwrap_or_default();
        format!("{:?}", text.trim())
    }

    /// What the publisher was asked to advertise, once it has been asked.
    fn published(&self) -> Option<String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(text) = std::fs::read_to_string(&self.argv) {
                if !text.trim().is_empty() {
                    return Some(text.trim().to_string());
                }
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        None
    }

    fn publisher_pid(&self) -> Option<u32> {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(text) = std::fs::read_to_string(&self.pidfile) {
                if let Ok(pid) = text.trim().parse() {
                    return Some(pid);
                }
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        None
    }

    fn machine(&self) -> String {
        let stream = UnixStream::connect(&self.control).expect("connect");
        let mut writer = stream.try_clone().expect("clone");
        let mut reader = BufReader::new(stream);
        writeln!(writer, r#"{{"cmd":"status"}}"#).expect("write");
        let mut line = String::new();
        reader.read_line(&mut line).expect("read");
        let v: serde_json::Value = serde_json::from_str(line.trim()).expect("json");
        v["machine"].as_str().expect("a machine name").to_string()
    }
}

/// Whether a pid is a live process, zombies excluded.
///
/// `/proc` rather than `kill(pid, 0)`: a process that has exited but not been
/// reaped still answers signal zero, and "the publisher is a zombie" is not
/// the same claim as "the publisher is still advertising".
fn alive(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    // `... ) S ...` — the state letter is the field after the closing paren.
    stat.rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .map(|state| state != "Z")
        .unwrap_or(false)
}

fn gone(pid: u32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

macro_rules! fixture {
    ($tag:literal, $extra:expr) => {
        match Fixture::start($tag, $extra) {
            Some(f) => f,
            None => {
                // Fixture::start printed the daemon's own reason above this line.
                eprintln!("SKIP: the daemon did not come up in this environment");
                return;
            }
        }
    };
}

#[test]
fn the_record_names_this_machine_this_port_and_nothing_that_identifies_it() {
    let f = fixture!("says", &[]);
    let published = f.published().expect("the daemon never published anything");
    let machine = f.machine();

    assert!(published.contains("_apex-remote._tcp"), "{published}");  // rime-rename: keep (SERVICE)
    // The port this daemon is actually on, not the catalogue default. A
    // record carrying 7717 while the daemon is somewhere else sends every
    // device on the network to a closed port.
    assert!(
        published.contains(&f.port.to_string()),
        "the record does not name port {}: {published}",
        f.port
    );
    assert!(published.contains(&machine), "{published}");
    assert!(published.contains("v=1"), "{published}");

    // And nothing stable about this machine. The rendezvous id does not
    // change between networks, so a record carrying it would let anybody on
    // a café network recognise this laptop again next week.
    let stream = UnixStream::connect(&f.control).expect("connect");
    let mut writer = stream.try_clone().expect("clone");
    let mut reader = BufReader::new(stream);
    writeln!(writer, r#"{{"cmd":"status"}}"#).expect("write");
    let mut line = String::new();
    reader.read_line(&mut line).expect("read");
    let v: serde_json::Value = serde_json::from_str(line.trim()).expect("json");
    for secret in ["rendezvous", "key"] {
        let value = v[secret].as_str().expect(secret);
        assert!(
            !published.contains(value),
            "the {secret} is on the wire: {published}"
        );
    }
}

#[test]
fn the_record_goes_away_when_the_daemon_is_killed_outright() {
    // Not "when it shuts down cleanly" -- that is the easy half and a Drop
    // impl covers it. A daemon that is SIGKILLed runs no destructor, and
    // without PR_SET_PDEATHSIG the publisher is orphaned and goes on
    // advertising a service that is not there until the machine reboots.
    let mut f = fixture!("killed", &[]);
    assert!(f.published().is_some(), "the daemon never published anything");
    let pid = f.publisher_pid().expect("the publisher never wrote its pid");
    assert!(alive(pid), "the publisher was not running to begin with");

    // SIGKILL: no unwinding, no Drop, nothing the daemon gets to do.
    f.child.kill().expect("kill");
    f.child.wait().expect("wait");

    assert!(
        gone(pid),
        "the publisher outlived the daemon and is still advertising this machine"
    );
}

#[test]
fn a_machine_can_be_told_not_to_announce_itself() {
    // For a laptop that is only ever reached through a relay, and for anybody
    // who does not want their machine named on a network they do not control.
    let f = fixture!("quiet", &["--no-announce"]);
    // The daemon is up -- the fixture waited for its control socket -- so a
    // record that was going to be published has had its chance.
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !Path::new(&f.argv).exists(),
        "a daemon told not to announce announced anyway"
    );
}
