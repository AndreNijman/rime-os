//! What the test files that start a real `rime-remoted` share: waiting for it
//! to come up on the port the kernel gave it.
//!
//! Every fixture starts the daemon with `--port 0` and its stderr in a log
//! file. Not a port probed by the test and handed over as `--port N`: that one
//! is free when it is probed and anybody's once the probe is dropped, and a
//! daemon given a taken `--port` exits with "Address already in use". The
//! discovery fixture measured it at 2 of 600 runs in the Nix build sandbox.

#![allow(dead_code)]

use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Child;
use std::time::{Duration, Instant};

/// The port `rime-remoted` is listening on, once its control socket answers
/// and it has said which port it got. The port is checked rather than
/// believed: something must accept on it. Stops waiting as soon as the daemon
/// exits, and every error carries what the daemon wrote to `log`.
pub fn remoted_port(
    child: &mut Child,
    control: &Path,
    log: &Path,
    within: Duration,
) -> Result<u16, String> {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("rime-remoted exited ({status}): {}", log_text(log)));
        }
        // The control socket is served before the startup line is printed, so
        // a socket that answers with no line yet is a daemon still starting.
        if UnixStream::connect(control).is_ok() {
            if let Some(port) = listening_on(log) {
                if port != 0 && std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                    return Ok(port);
                }
                return Err(format!(
                    "rime-remoted says it is listening on port {port} and nothing accepts there: {}",
                    log_text(log)
                ));
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Err(format!(
        "rime-remoted did not come up in {} s: {}",
        within.as_secs(),
        log_text(log)
    ))
}

/// "rime-remoted: listening on PORT, ..." from the daemon's startup line.
pub fn listening_on(log: &Path) -> Option<u16> {
    std::fs::read_to_string(log)
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("rime-remoted: listening on "))?
        .split(',')
        .next()?
        .trim()
        .parse()
        .ok()
}

/// The daemon's log, quoted, for an error message.
pub fn log_text(log: &Path) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    format!("{:?}", text.trim())
}
