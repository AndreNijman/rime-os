//! A directory on another machine, reached with `ssh`. P2-001's fourth kind.
//!
//! This was the one of §13.5's five that this build refused, and the reason
//! recorded against it was not "nobody wrote it" but a test problem:
//!
//! > a transport proven only against a fake `ssh` on PATH is a test of the
//! > argument list and not of a backup.
//!
//! So there are two halves here and both are needed. The argument list is built
//! by [`SshTarget::argv`] and pinned by unit tests, because that is where the
//! security properties are; and `tests/test-rime-backup-ssh.sh` runs the whole
//! thing against a **real `sshd` on loopback**, with a fixture host key and a
//! fixture client key, so that the framing, the exit codes and the binary
//! round trip are proved against the program that will actually be on the other
//! end.
//!
//! # The argument list is the security property
//!
//! An ssh backup target that fell back to the owner's ssh agent, or accepted
//! whatever host key it was offered, would be worse than no ssh target: the
//! first hands a long-lived credential to a backup run, and the second backs up
//! to whoever answers on that address. Every invocation therefore carries, with
//! no way for configuration to remove them:
//!
//! | option | why |
//! |---|---|
//! | `-F /dev/null` | `~/.ssh/config` cannot redirect this, add a `ProxyCommand`, or change the user |
//! | `-o IdentityAgent=none` | the owner's agent is never reachable, even if `SSH_AUTH_SOCK` somehow survived |
//! | `-o IdentitiesOnly=yes` | only the key this target names is offered |
//! | `-o BatchMode=yes` | never a prompt — a backup that blocks on a passphrase is a backup that did not happen |
//! | `-o NumberOfPasswordPrompts=0` | the same, for the password path specifically |
//! | `-o StrictHostKeyChecking=yes` | a host key that is not the pinned one is a refusal, not a question |
//! | `-o UserKnownHostsFile=…` | the pin is this target's file and not the owner's |
//! | `-o GlobalKnownHostsFile=/dev/null` | nor `/etc/ssh/ssh_known_hosts` |
//! | `-o PreferredAuthentications=publickey` | no keyboard-interactive, no GSSAPI |
//! | `-n` is **not** used | stdin is how a chunk's bytes get there |
//!
//! and the child is spawned with [`std::process::Command::env_clear`], so
//! `SSH_AUTH_SOCK`, `SSH_ASKPASS` and `DISPLAY` are not inherited. That is
//! belt and braces with `IdentityAgent=none` and `BatchMode=yes` on purpose:
//! each of the three would have to be defeated separately.
//!
//! # Why the remote side is a small shell script and not `cat`
//!
//! [`Target::get`] has to distinguish three answers, and `cat missing-file`
//! gives the same exit status as `cat unreadable-file`. So each remote command
//! is a short `sh` script that tests the path first and exits with a code from
//! [`code`] — 10 for "not there", 11 for "there and not readable", 12 for "the
//! target root is not a directory". `ssh` reserves **255** for its own
//! failures, and the script never produces it, so a connection that did not
//! happen is unambiguous.
//!
//! That distinction is the entire reason this module is 300 lines rather than
//! 80. A backup target that reported "no such snapshot" when what happened was
//! "the far side would not let me look" is precisely the defect
//! [`crate::Verdict`] exists to refuse.
//!
//! # Bytes, not base64
//!
//! Unlike [`super::bucket`], nothing here passes through
//! `String::from_utf8_lossy`: `ssh` gives a clean binary stream in both
//! directions and [`Transport::run`] hands back `Vec<u8>`. A chunk of
//! ciphertext goes up and comes back byte for byte, which
//! `a_chunk_that_is_not_utf8_survives_the_round_trip` asserts with a body that
//! is invalid UTF-8 in four different ways.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use super::{Target, TargetError};
use crate::format::SnapshotId;

/// `ssh`, by absolute path.
///
/// Not "ssh": a backup that ran whatever `ssh` a `PATH` picked up is a backup
/// that can be redirected by an environment variable. The same reasoning
/// `rime-secretd` applies to `curl`.
pub const SSH: &str = rime_paths::SSH;

/// How long to wait for the far side to answer at all.
pub const CONNECT_TIMEOUT_SECS: u32 = 15;

/// The exit codes the remote script uses, none of which `ssh` itself produces.
pub mod code {
    /// The path is not there. A real answer about a real absence.
    pub const ABSENT: i32 = 10;
    /// The path is there and this account may not read it.
    pub const DENIED: i32 = 11;
    /// The target root is not a directory, or is not reachable.
    pub const NO_ROOT: i32 = 12;
    /// `ssh`'s own: it never reached the far side, or the far side refused it.
    /// The script above never produces this, so it is unambiguous.
    pub const SSH_FAILED: i32 = 255;
}

/// What one remote command produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// The remote command's exit status, or `None` if it was killed by a
    /// signal locally. Never conflated with zero.
    pub status: Option<i32>,
    /// The remote command's standard output, as bytes. **Never a `String`** —
    /// see the module note.
    pub stdout: Vec<u8>,
    /// Standard error, lossily decoded, for a message. Never data.
    pub stderr: String,
}

/// Running one `ssh` invocation.
///
/// A trait for the same reason [`super::bucket::Broker`] is one: the argument list,
/// the remote script construction and the exit-code mapping can then be
/// exercised exactly, with no daemon and no network, and the shell suite can
/// spend its time proving the thing a double cannot — that a real `sshd` on the
/// other end behaves the way this assumes.
pub trait Transport {
    /// Run `argv[0]` with `argv[1..]`, writing `input` to its stdin.
    fn run(&self, argv: &[String], input: &[u8]) -> Result<Output, TargetError>;
}

/// The real one: a child process.
#[derive(Debug, Clone, Default)]
pub struct SshCommand;

impl Transport for SshCommand {
    fn run(&self, argv: &[String], input: &[u8]) -> Result<Output, TargetError> {
        use std::io::{Read, Write};

        let Some((program, args)) = argv.split_first() else {
            return Err(TargetError::Unavailable(
                "an empty ssh command line".to_string(),
            ));
        };
        let mut child = Command::new(program)
            .args(args)
            // The owner's agent, askpass helper and X display are not this
            // child's business, and an inherited SSH_AUTH_SOCK is the one
            // thing that could make a backup run spend a credential nobody
            // handed it. `IdentityAgent=none` says the same thing; both are
            // here so that neither is load-bearing alone.
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                TargetError::Unavailable(format!("{program} could not be run: {e}"))
            })?;

        // stdin is written on a thread and stdout read on this one. Writing
        // first and reading afterwards deadlocks the moment the payload is
        // larger than a pipe buffer, which for a 1 MiB chunk is every time.
        let mut stdin = child.stdin.take().expect("piped");
        let payload = input.to_vec();
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(&payload);
            let _ = stdin.flush();
            drop(stdin);
        });

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        if let Some(mut out) = child.stdout.take() {
            let _ = out.read_to_end(&mut stdout);
        }
        if let Some(mut err) = child.stderr.take() {
            let _ = err.read_to_end(&mut stderr);
        }
        let status = child
            .wait()
            .map_err(|e| TargetError::Unavailable(format!("waiting for {program}: {e}")))?;
        let _ = writer.join();

        Ok(Output {
            status: status.code(),
            stdout,
            stderr: String::from_utf8_lossy(&stderr).trim().to_string(),
        })
    }
}

/// Where the far side is and how to prove it is the far side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshEndpoint {
    pub host: String,
    pub user: Option<String>,
    pub port: Option<u16>,
    /// The private key offered, and the only one — `IdentitiesOnly=yes`.
    pub identity: PathBuf,
    /// The pinned host keys. **Not** the owner's `~/.ssh/known_hosts`: a
    /// backup target's idea of which machine it is talking to is the target's
    /// own configuration, not a file every ssh session on the machine appends
    /// to.
    pub known_hosts: PathBuf,
}

/// A directory on another machine.
pub struct SshTarget<T: Transport> {
    transport: T,
    endpoint: SshEndpoint,
    /// Absolute path of the target root on the far side.
    root: String,
    prefix: String,
}

impl<T: Transport> SshTarget<T> {
    pub fn new(transport: T, endpoint: SshEndpoint, root: &str, prefix: &str) -> SshTarget<T> {
        SshTarget {
            transport,
            endpoint,
            root: root.to_string(),
            prefix: prefix.to_string(),
        }
    }

    pub fn endpoint(&self) -> &SshEndpoint {
        &self.endpoint
    }

    /// The remote directory snapshots live in.
    fn base(&self) -> String {
        format!("{}/{}", self.root.trim_end_matches('/'), self.prefix)
    }

    fn snapshot_dir(&self, snapshot: &str) -> String {
        format!("{}/{}", self.base(), snapshot)
    }

    /// The full command line for one remote script.
    ///
    /// Public because the options in it are the security property, and a
    /// property nothing can read is a property nothing can test.
    pub fn argv(&self, script: &str) -> Vec<String> {
        let mut argv: Vec<String> = vec![SSH.to_string()];
        let mut opt = |o: &str| argv.push(format!("-o{o}"));
        opt(&format!("ConnectTimeout={CONNECT_TIMEOUT_SECS}"));
        opt("BatchMode=yes");
        opt("IdentitiesOnly=yes");
        opt("IdentityAgent=none");
        opt("PreferredAuthentications=publickey");
        opt("NumberOfPasswordPrompts=0");
        opt("StrictHostKeyChecking=yes");
        opt(&format!(
            "UserKnownHostsFile={}",
            self.endpoint.known_hosts.display()
        ));
        opt("GlobalKnownHostsFile=/dev/null");
        // No configuration file at all. Every option this target depends on is
        // on the command line above, where nothing in the owner's home can
        // reach it.
        argv.push("-F".to_string());
        argv.push("/dev/null".to_string());
        argv.push("-i".to_string());
        argv.push(self.endpoint.identity.display().to_string());
        if let Some(port) = self.endpoint.port {
            argv.push("-p".to_string());
            argv.push(port.to_string());
        }
        if let Some(user) = &self.endpoint.user {
            argv.push("-l".to_string());
            argv.push(user.clone());
        }
        // `--` before the destination, so a host name that begins with a dash
        // is a host name and not an option.
        argv.push("--".to_string());
        argv.push(self.endpoint.host.clone());
        argv.push(script.to_string());
        argv
    }

    fn run(&self, script: &str, input: &[u8]) -> Result<Output, TargetError> {
        self.transport.run(&self.argv(script), input)
    }

    /// Map a remote script's exit status onto the three answers.
    fn answer(&self, what: &str, out: Output) -> Result<Vec<u8>, TargetError> {
        match out.status {
            Some(0) => Ok(out.stdout),
            Some(code::ABSENT) => Err(TargetError::Absent(format!(
                "{what} is not on {}",
                self.endpoint.host
            ))),
            Some(code::DENIED) => Err(TargetError::Denied(format!(
                "{what} is on {} and this account may not read it",
                self.endpoint.host
            ))),
            Some(code::NO_ROOT) => Err(TargetError::Unavailable(format!(
                "{} is not a directory on {}. Nothing was looked at, so this \
                 says nothing about whether backups are there",
                self.root, self.endpoint.host
            ))),
            Some(code::SSH_FAILED) => Err(TargetError::Unavailable(format!(
                "ssh did not reach {}{}",
                self.endpoint.host,
                detail(&out.stderr)
            ))),
            Some(other) => Err(TargetError::Unavailable(format!(
                "{what}: the remote command exited {other}{}",
                detail(&out.stderr)
            ))),
            // Killed by a signal. Not zero, and not a status either.
            None => Err(TargetError::Unavailable(format!(
                "{what}: ssh was killed by a signal before it reported anything"
            ))),
        }
    }
}

fn detail(stderr: &str) -> String {
    if stderr.is_empty() {
        String::new()
    } else {
        format!(": {}", stderr.escape_debug())
    }
}

/// One shell word, quoted so that nothing in it can be anything but data.
///
/// Single quotes, with `'` itself spliced as `'\''`. Every byte other than `'`
/// is literal inside single quotes — including `$`, backtick, newline and
/// backslash — so this is total rather than a list of characters somebody has
/// to keep up to date.
///
/// The paths that reach it are a target root out of the project's own
/// `rime.toml` and object names this build produced, so the exposure is small.
/// It is here anyway: "the caller only ever passes safe values" is a property
/// of today's callers.
pub fn shell_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

impl<T: Transport> Target for SshTarget<T> {
    fn describe(&self) -> String {
        let user = match &self.endpoint.user {
            Some(user) => format!("{user}@"),
            None => String::new(),
        };
        let port = match self.endpoint.port {
            Some(port) => format!(":{port}"),
            None => String::new(),
        };
        format!("ssh at {user}{}{port}:{}", self.endpoint.host, self.root)
    }

    fn prepare(&self) -> Result<(), TargetError> {
        let root = shell_quote(&self.root);
        let base = shell_quote(&self.base());
        // `umask 077` before the mkdir, so the directory is 0700 from the
        // moment it exists rather than 0700 one syscall later. The existence
        // and size of a backup are facts about its owner even when the bytes
        // are unreadable.
        let script = format!(
            "umask 077; if [ ! -d {root} ]; then exit {no_root}; fi; \
             mkdir -p -- {base} || exit {denied}; printf ready",
            no_root = code::NO_ROOT,
            denied = code::DENIED
        );
        self.answer("the target root", self.run(&script, &[])?)?;
        Ok(())
    }

    fn put(&self, snapshot: &str, object: &str, bytes: &[u8]) -> Result<(), TargetError> {
        let dir = shell_quote(&self.snapshot_dir(snapshot));
        let partial = shell_quote(&format!("{}/.{object}.partial", self.snapshot_dir(snapshot)));
        let final_ = shell_quote(&format!("{}/{object}", self.snapshot_dir(snapshot)));
        // Written to a side name and renamed, so a reader never sees half an
        // object — the same property `fs::write_atomically` gives locally, and
        // for the same reason. `mv` within one directory is `rename(2)`.
        let script = format!(
            "umask 077; mkdir -p -- {dir} || exit {denied}; \
             cat > {partial} || exit 1; mv -- {partial} {final_}",
            denied = code::DENIED
        );
        self.answer(
            &format!("{object} of snapshot {snapshot}"),
            self.run(&script, bytes)?,
        )?;
        Ok(())
    }

    fn get(&self, snapshot: &str, object: &str) -> Result<Vec<u8>, TargetError> {
        let path = shell_quote(&format!("{}/{object}", self.snapshot_dir(snapshot)));
        // The three-way test, and then `cat` as the last command so that its
        // own exit status is the script's: a `cat` that fails part way through
        // must not look like a short object that arrived intact.
        let script = format!(
            "if [ ! -e {path} ]; then exit {absent}; fi; \
             if [ ! -r {path} ]; then exit {denied}; fi; cat -- {path}",
            absent = code::ABSENT,
            denied = code::DENIED
        );
        self.answer(
            &format!("{object} of snapshot {snapshot}"),
            self.run(&script, &[])?,
        )
    }

    fn list(&self) -> Result<Vec<SnapshotId>, TargetError> {
        let root = shell_quote(&self.root);
        let base = shell_quote(&self.base());
        // Three cases, and the middle one is the reason this is not one `ls`:
        //
        //   * the root is not a directory  → NO_ROOT, nothing was looked at;
        //   * the root is fine and the snapshot directory is not there yet →
        //     success with no output, which is a REAL answer meaning "no
        //     snapshots have been written here";
        //   * otherwise list it.
        //
        // An `ls` whose failure was swallowed would collapse the first into
        // the second and report a target that could not be read as a target
        // with no backups in it.
        let script = format!(
            "if [ ! -d {root} ]; then exit {no_root}; fi; \
             if [ ! -d {base} ]; then exit 0; fi; \
             if [ ! -r {base} ]; then exit {denied}; fi; \
             ls -1 -- {base}",
            no_root = code::NO_ROOT,
            denied = code::DENIED
        );
        let out = self.answer("the snapshot directory", self.run(&script, &[])?)?;
        let text = String::from_utf8_lossy(&out);
        let mut ids: Vec<SnapshotId> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .filter_map(SnapshotId::parse)
            .collect();
        ids.sort();
        Ok(ids)
    }
}

#[cfg(test)]
mod tests;
