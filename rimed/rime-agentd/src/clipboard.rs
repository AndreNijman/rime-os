//! `Request::Clipboard`: read what is on this machine's clipboard (P1-059
//! criterion 3, the receive half).
//!
//! The refusals and the arithmetic that shape this verb are on
//! [`rime_agent_core::protocol::Request::Clipboard`] and on
//! [`crate::privilege::refuse_clipboard`]. This file is the I/O: finding a
//! compositor to ask, asking it without being able to hang, and turning what
//! comes back into one response.
//!
//! ## The daemon does not have a display, and that is not a bug to route
//! around
//!
//! Measured on a live `rime-agentd`: its environment holds `XDG_RUNTIME_DIR`
//! and `DBUS_SESSION_BUS_ADDRESS` and **no `WAYLAND_DISPLAY`**, while the
//! user's own service manager reports `WAYLAND_DISPLAY=wayland-1`.
//! `rime-agentd.service` says why in its own comments: the runtime is useful
//! in a bare TTY, so it deliberately does not order itself after
//! `graphical-session.target` — it is usually running before a compositor
//! exists to import that variable, and nothing imports it afterwards.
//!
//! So a `Command::new("wl-paste")` with the daemon's environment does not read
//! an empty clipboard. It fails with *"Failed to connect to a Wayland server …
//! falling back to wayland-0"*, and a verb that reported that as "your
//! clipboard is empty" would be lying about the one thing it exists to report.
//! [`display`] finds the socket instead.
//!
//! ## Why the socket is probed rather than globbed
//!
//! The machine this was written on has two: `wayland-1`, live, and
//! `wayland-2`, left behind by a compositor that exited on a previous day.
//! Picking the first name that matches a pattern is a coin toss between them,
//! and the losing side of that toss is a user told their session has no
//! display while they are looking at it. A `connect(2)` separates them for
//! certain — a stale socket answers `ECONNREFUSED` immediately, measured, so
//! the probe costs nothing and cannot hang.
//!
//! `WAYLAND_DISPLAY`, if something ever does set it for this daemon, is tried
//! first and is probed like any other candidate. An environment variable is a
//! claim about the world, not the world.
//!
//! ## The bounded wait is the important line in this file
//!
//! `wl-paste` does not read a buffer the compositor holds. Wayland's clipboard
//! is a *promise*: the compositor hands over a file descriptor and the
//! application that owns the selection writes the bytes into it, whenever it
//! gets round to it. An application that is wedged — the state in which
//! somebody is most likely to reach for their phone — never writes, and
//! `wl-paste` waits for it forever.
//!
//! For a phone that is not one slow request. `rime-remoted` calls `control()`
//! inline in its frame loop (`serve.rs:352`) with a 300-second socket timeout
//! (`proxy.rs:50`), so an unbounded read here freezes the whole device
//! connection — the terminal with it — for five minutes, on a verb the user
//! could have skipped. [`READ_SECS`] bounds it, the child is killed, and the
//! refusal says which clock ran out.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rime_agent_core::protocol::{ErrorKind, Response};

use crate::privilege;
use crate::Daemon;

/// The most clipboard this verb will carry, in bytes of UTF-8.
///
/// Arithmetic, not taste, and the sum is checked by a test against the real
/// constant rather than against a number copied into a comment.
///
/// The reply is one line of this protocol and for a paired device that line is
/// one `rime_remote_core::wire::Frame::Control`, whose payload may be at most
/// `MAX_PAYLOAD` = 65514 bytes. `Frame::encode` **refuses** an oversize
/// payload rather than truncating it, and in `rime-remoted` that refusal is a
/// `?` on the send inside the connection loop — so a reply one byte too long
/// does not fail this request, it drops the device's entire connection.
///
/// `serde_json` escapes a byte below 0x20 as `\u00XX`: six characters for one
/// byte, and a clipboard can be all of them. So the worst case is `6 ×` the
/// raw size plus the 31-byte envelope `{"reply":"clipboard","text":""}`, and
/// the largest cap that fits is 10913. This is 8192 — a quarter under it on
/// purpose. 6 × 8192 + 31 = 49183 leaves 16331 bytes of a frame spare, which
/// is where a field added to this response by a later protocol revision goes
/// without anybody having to redo this sum or discover they got it wrong on a
/// user's connection.
///
/// 8 KiB is around 1400 words. Nobody pastes that into a prompt by accident,
/// and the answer when they do it on purpose is [`shape`]'s refusal, which
/// names the limit.
pub const MAX_BYTES: usize = 8 * 1024;

/// Longest `wl-paste` may take to produce the selection before it is killed.
///
/// Bounds the *whole* read rather than one syscall, because there is no
/// progress to measure: the owning application either serves the selection or
/// it does not. Five seconds is far longer than a working paste takes — the
/// bytes are already in the owner's memory — and short enough that a phone
/// asking while some application is wedged gets an answer instead of a frozen
/// connection.
pub const READ_SECS: u64 = 5;

/// The MIME types worth asking for, best first.
///
/// Ordered rather than searched: `text/plain;charset=utf-8` says what encoding
/// it is in, `text/plain` does not, and the three shouting names are X11
/// selection atoms that Xwayland clients still offer. Asking for a named type
/// is what keeps this verb from handing back a PNG — `wl-paste` with no
/// `--type` serves whatever the owner listed first, and for a screenshot tool
/// that is image bytes.
const TEXT_TYPES: [&str; 5] = [
    "text/plain;charset=utf-8",
    "text/plain",
    "UTF8_STRING",
    "STRING",
    "TEXT",
];

/// Handle one `Clipboard`.
///
/// The order is the order in `inject::handle` and for the same reason: who is
/// asking is settled before anything is read, so a refusal cannot depend on
/// what happened to be on the clipboard.
pub fn handle(daemon: &Arc<Daemon>, caller: &privilege::Caller) -> Response {
    let who = privilege::origin(daemon, caller);
    if let Some(refusal) = privilege::refuse_clipboard(&who) {
        return refusal;
    }
    let seat = match display() {
        Ok(seat) => seat,
        Err(why) => return Response::error(ErrorKind::Internal, why),
    };
    read(&seat)
}

/// A compositor this daemon can reach, and the runtime directory it lives in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seat {
    /// The value to put in `WAYLAND_DISPLAY`.
    pub display: String,
    /// The value to put in `XDG_RUNTIME_DIR`.
    pub runtime_dir: PathBuf,
}

/// Find a Wayland socket that answers.
///
/// `Err` is a sentence for the user, because every way this fails is something
/// they can act on and none of them is "the clipboard is empty": no graphical
/// session, a session this daemon cannot see, or a runtime directory that is
/// not there at all.
pub fn display() -> Result<Seat, String> {
    display_for("read")
}

/// [`display`], with the refusal saying what was being done (`read`, or
/// `set` for `clipboard_write`).
pub fn display_for(verb: &str) -> Result<Seat, String> {
    let runtime_dir = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => {
            return Err(format!(
                "cannot {verb} this machine's clipboard: the agent runtime has no \
                 XDG_RUNTIME_DIR, so there is nowhere to look for a compositor"
            ))
        }
    };
    for name in candidates(&runtime_dir) {
        if std::os::unix::net::UnixStream::connect(runtime_dir.join(&name)).is_ok() {
            return Ok(Seat {
                display: name,
                runtime_dir,
            });
        }
    }
    Err(format!(
        "cannot {verb} this machine's clipboard: no Wayland compositor is listening in {}. \
         The agent runtime starts before a graphical session and does not inherit \
         WAYLAND_DISPLAY, so this is what a machine with no desktop session looks like \
         — log in at the machine and ask again",
        runtime_dir.display()
    ))
}

/// Every socket name worth a `connect(2)`, best first.
///
/// Separated from [`display`] so the ordering can be tested without a
/// compositor. `WAYLAND_DISPLAY` leads when it is set and relative; an
/// absolute one is rejected rather than joined, because `runtime_dir.join()`
/// of an absolute path silently discards the directory and would have this
/// function probe a path nobody chose.
fn candidates(runtime_dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(v) = std::env::var_os("WAYLAND_DISPLAY") {
        let v = v.to_string_lossy().into_owned();
        if !v.is_empty() && !v.starts_with('/') {
            out.push(v);
        }
    }
    let Ok(entries) = std::fs::read_dir(runtime_dir) else {
        return out;
    };
    let mut found: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        // `wayland-<digits>` and nothing else. The runtime directory on this
        // machine also holds `wayland-1-awww-daemon.sock` and
        // `Alacritty-wayland-1-59727.sock`, which match a looser pattern and
        // are not compositors — connecting to one of those would succeed and
        // then hand `wl-paste` a socket that speaks a different protocol.
        .filter(|n| {
            n.strip_prefix("wayland-")
                .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
        })
        .collect();
    found.sort();
    for n in found {
        if !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

/// Ask `wl-paste` for the selection on `seat` and turn the answer into one
/// response.
///
/// Two invocations, and the second is the only one that touches content.
/// `--list-types` is what separates the three answers that would otherwise be
/// one: an empty clipboard, a clipboard holding something that is not text,
/// and a clipboard holding text. A single `wl-paste` call collapses the first
/// two into "exit 1" and serves the third as whatever the owner offered first,
/// which for a screenshot tool is a PNG.
pub fn read(seat: &Seat) -> Response {
    let types = match run(seat, &["--list-types"]) {
        Ok(out) => out,
        Err(why) => return Response::error(ErrorKind::Internal, why),
    };
    if !types.ok {
        // `wl-paste` exits non-zero with nothing on the clipboard. An empty
        // clipboard is an answer, not a failure: a phone told "the machine
        // refused" when the machine simply had nothing would send its user
        // looking for a permission to grant.
        return Response::Clipboard {
            text: String::new(),
        };
    }
    let offered: Vec<String> = String::from_utf8_lossy(&types.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    let Some(chosen) = pick_type(&offered) else {
        return Response::error(
            ErrorKind::BadRequest,
            format!(
                "this machine's clipboard does not hold text; it offers {}. \
                 Only text can be carried over this connection",
                if offered.is_empty() {
                    "nothing this daemon can name".to_string()
                } else {
                    offered.join(", ")
                }
            ),
        );
    };

    // `--no-newline` because a newline this daemon appended is a newline the
    // phone pastes into a terminal, and on a terminal that is the return key.
    match run(seat, &["--no-newline", "--type", &chosen]) {
        Ok(out) if out.ok => shape(out.stdout),
        // The type was offered a moment ago and is not now: the selection
        // changed between the two calls, or its owner exited holding it.
        // Reported rather than guessed at.
        Ok(_) => Response::error(
            ErrorKind::Internal,
            format!(
                "this machine's clipboard offered {chosen} and then would not produce it; \
                 whatever held the selection has gone. Copy it again"
            ),
        ),
        Err(why) => Response::error(ErrorKind::Internal, why),
    }
}

/// The best text type on offer, or `None` if none of them is text.
///
/// Falls through [`TEXT_TYPES`] first, then takes any `text/*`. A type this
/// list has never heard of is still text if it says it is, and refusing it
/// would mean a clipboard full of `text/markdown` reported as "not text".
fn pick_type(offered: &[String]) -> Option<String> {
    for want in TEXT_TYPES {
        if let Some(hit) = offered.iter().find(|o| o.eq_ignore_ascii_case(want)) {
            return Some(hit.clone());
        }
    }
    offered
        .iter()
        .find(|o| o.to_ascii_lowercase().starts_with("text/"))
        .cloned()
}

/// Turn clipboard bytes into a response. Pure: the whole decision, none of the
/// I/O.
///
/// `bytes` may be at most `MAX_BYTES + 1` long, because [`run`] stops reading
/// there — the point of the cap is not to buffer the thing being refused. That
/// is why the refusal says *more than*: this function knows the clipboard is
/// over the limit and does not know by how much, and inventing a number it did
/// not measure is how a message ends up being wrong.
pub fn shape(bytes: Vec<u8>) -> Response {
    if bytes.len() > MAX_BYTES {
        return Response::error(
            ErrorKind::BadRequest,
            format!(
                "this machine's clipboard holds more than {MAX_BYTES} bytes, which is more \
                 than one message on this connection can carry. Copy a smaller selection"
            ),
        );
    }
    match String::from_utf8(bytes) {
        Ok(text) => Response::Clipboard { text },
        // Not `from_utf8_lossy`. The response documents `text` as the
        // selection verbatim, and a replacement character substituted for a
        // byte is not verbatim — it is this daemon editing what somebody
        // copied and not saying so.
        Err(e) => Response::error(
            ErrorKind::BadRequest,
            format!(
                "this machine's clipboard is not valid UTF-8 — byte {} is not text — so it \
                 cannot be carried as a string",
                e.utf8_error().valid_up_to()
            ),
        ),
    }
}

/// What one `wl-paste` invocation produced.
struct Output {
    ok: bool,
    stdout: Vec<u8>,
}

/// Run `wl-paste` on `seat`, bounded, with its output capped.
///
/// By name rather than by absolute path, the way `rime send --clipboard`
/// invokes it: the daemon finds it on `PATH`, which is also what lets a test
/// put a fake one in front of it.
///
/// `Err` is a sentence, and there are only two of them — the tool is not
/// installed, or it did not finish in time. Neither is a statement about the
/// clipboard, which is why they are separate from every return in [`read`].
fn run(seat: &Seat, args: &[&str]) -> Result<Output, String> {
    let child = Command::new("wl-paste")
        .args(args)
        .env("WAYLAND_DISPLAY", &seat.display)
        .env("XDG_RUNTIME_DIR", &seat.runtime_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            format!(
                "cannot read this machine's clipboard: wl-paste is not available to the \
                 agent runtime ({e})"
            )
        })?;
    bounded(child, MAX_BYTES, Duration::from_secs(READ_SECS))
}

/// Read a child's stdout with a byte cap and a deadline, killing it if either
/// runs out.
///
/// The read happens on its own thread and the deadline is enforced here,
/// because there is no portable way to put a timeout on a pipe read and a
/// `wait()` on a child whose stdout nobody is draining deadlocks the moment
/// the output passes a pipe buffer.
///
/// `cap + 1` bytes are read, not `cap`: one byte past the limit is what proves
/// the limit was passed, and stopping there means an enormous selection is
/// refused without ever being held in this daemon's memory. Reaching it kills
/// the child rather than draining the rest, so a 200 MB clipboard costs a
/// pipe buffer and not a copy.
fn bounded(mut child: Child, cap: usize, limit: Duration) -> Result<Output, String> {
    let mut stdout = child.stdout.take().expect("stdout was piped");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        // Errors are not distinguished from EOF on purpose: a read that fails
        // has produced everything it is going to, and what the caller decides
        // from is the exit status.
        let _ = (&mut stdout).take(cap as u64 + 1).read_to_end(&mut buf);
        let _ = tx.send(buf);
    });

    let deadline = Instant::now() + limit;
    let stdout_bytes = match rx.recv_timeout(limit) {
        Ok(b) => b,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "reading this machine's clipboard took longer than {}s and was stopped. \
                 A Wayland clipboard is served by the application that owns it, so this is \
                 what a wedged application looks like from here",
                limit.as_secs()
            ));
        }
    };
    if stdout_bytes.len() > cap {
        // Past the cap, so nothing this child still has to say can change the
        // answer. Killed rather than waited on: it is blocked writing into a
        // pipe nobody is reading any more.
        let _ = child.kill();
        let _ = child.wait();
        return Ok(Output {
            ok: true,
            stdout: stdout_bytes,
        });
    }

    // stdout is at EOF, so the child has closed it and is about to exit. Still
    // bounded: "about to" is not "has", and the remaining deadline is the
    // honest amount of patience left.
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Ok(Output {
                    ok: status.success(),
                    stdout: stdout_bytes,
                })
            }
            Ok(None) => {}
            Err(e) => return Err(format!("wl-paste could not be waited for ({e})")),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "wl-paste closed this machine's clipboard and then did not exit within {}s",
                limit.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rime_agent_core::protocol::Request;

    /// What `respond()` writes, which is what a frame carries.
    fn line(r: &Response) -> String {
        serde_json::to_string(r).expect("a response serialises")
    }

    #[test]
    fn the_cap_is_the_arithmetic_it_claims_to_be() {
        // The constant, imported, not the number 65514 typed in here — a test
        // that restates a number it is supposed to be checking against passes
        // on the day the number changes.
        let max_frame = rime_remote_core::wire::MAX_PAYLOAD;

        // The envelope this response has when it carries nothing.
        let empty = line(&Response::Clipboard {
            text: String::new(),
        });
        assert_eq!(empty, r#"{"reply":"clipboard","text":""}"#);
        assert_eq!(empty.len(), 31, "the envelope changed; redo the sum below");

        // The worst case, built rather than reasoned about: a full clipboard
        // of the byte serde_json spends the most characters on.
        let worst = "\u{1}".repeat(MAX_BYTES);
        assert_eq!(worst.len(), MAX_BYTES, "one byte each before escaping");
        let encoded = line(&Response::Clipboard { text: worst });
        assert_eq!(
            encoded.len(),
            6 * MAX_BYTES + empty.len(),
            "serde_json no longer spends six characters on a C0 byte, so the cap's \
             justification has changed even if the cap still fits"
        );
        assert!(
            encoded.len() <= max_frame,
            "a full clipboard of control bytes encodes to {} bytes and one frame carries \
             {max_frame}. This does not fail one request: rime-remoted sends the reply with \
             a `?` inside its connection loop, so the device loses its terminal too",
            encoded.len()
        );

        // And the cap is not so conservative that it was picked by accident:
        // the largest value that could fit is worth stating, so that anybody
        // raising this knows where the wall is.
        let ceiling = (max_frame - empty.len()) / 6;
        assert_eq!(ceiling, 10913);
        assert!(MAX_BYTES < ceiling);
    }

    #[test]
    fn an_ordinary_clipboard_comes_back_verbatim() {
        let text = "git log --oneline -5\nsecond line\ttabbed\n";
        match shape(text.as_bytes().to_vec()) {
            Response::Clipboard { text: got } => assert_eq!(got, text),
            other => panic!("expected a clipboard, got {}", line(&other)),
        }
        // NDJSON framing, and the assertion `Frame::encode` makes: one line.
        assert!(!line(&shape(text.as_bytes().to_vec())).contains('\n'));
    }

    #[test]
    fn an_empty_clipboard_is_an_answer_and_not_an_error() {
        match shape(Vec::new()) {
            Response::Clipboard { text } => assert!(text.is_empty()),
            other => panic!("an empty clipboard must not be an error: {}", line(&other)),
        }
    }

    #[test]
    fn exactly_the_cap_is_carried_and_one_byte_more_is_refused() {
        // The boundary in both directions, because an off-by-one here is a cap
        // that refuses the thing it was sized to carry.
        match shape(vec![b'x'; MAX_BYTES]) {
            Response::Clipboard { text } => assert_eq!(text.len(), MAX_BYTES),
            other => panic!("{MAX_BYTES} bytes must be carried: {}", line(&other)),
        }
        let over = shape(vec![b'x'; MAX_BYTES + 1]);
        let (kind, message) = over.as_error().expect("one byte over the cap is refused");
        assert_eq!(kind, ErrorKind::BadRequest);
        assert!(
            message.contains(&MAX_BYTES.to_string()),
            "the refusal must name the limit, or the user cannot act on it: {message}"
        );
        assert!(
            message.contains("more than"),
            "the daemon stops reading at the cap, so it does not know the real size and \
             must not imply it does: {message}"
        );
    }

    #[test]
    fn a_clipboard_that_is_not_utf8_is_refused_rather_than_mangled() {
        // The first three bytes are a valid UTF-8 é followed by a lone 0x80.
        let bytes = vec![b'a', 0xc3, 0xa9, 0x80, b'z'];
        let refusal = shape(bytes);
        let (kind, message) = refusal.as_error().expect("invalid UTF-8 is refused");
        assert_eq!(kind, ErrorKind::BadRequest);
        assert!(
            message.contains("byte 3"),
            "the refusal must name where it went wrong: {message}"
        );
        assert!(
            !message.contains('\u{fffd}'),
            "a replacement character in the refusal means something ran from_utf8_lossy"
        );
    }

    #[test]
    fn text_types_are_ranked_and_a_picture_is_not_text() {
        assert_eq!(
            pick_type(&[
                "image/png".into(),
                "text/plain".into(),
                "text/plain;charset=utf-8".into()
            ]),
            Some("text/plain;charset=utf-8".into()),
            "the type that names its encoding wins over the one that does not"
        );
        assert_eq!(
            pick_type(&["TEXT".into(), "UTF8_STRING".into()]),
            Some("UTF8_STRING".into()),
            "X11 atoms are ranked too, and UTF8_STRING says more than TEXT"
        );
        // A screenshot tool offers exactly this, and it is the case a
        // `wl-paste` with no `--type` would hand back as a wall of PNG.
        assert_eq!(
            pick_type(&["image/png".into(), "image/bmp".into()]),
            None,
            "a clipboard holding a picture holds no text"
        );
        assert_eq!(pick_type(&[]), None);
        // A type nobody listed here is still text if it says so.
        assert_eq!(
            pick_type(&["image/png".into(), "text/markdown".into()]),
            Some("text/markdown".into())
        );
        // Case, because X11 atoms and MIME types disagree about it.
        assert_eq!(
            pick_type(&["Text/Plain".into()]),
            Some("Text/Plain".into()),
            "the daemon must pass back the spelling the compositor used"
        );
    }

    #[test]
    fn socket_candidates_exclude_everything_that_is_not_a_compositor() {
        let dir = std::env::temp_dir().join(format!("rime-clip-cand-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        // Every name in this list was observed in a real /run/user/1000.
        for n in [
            "wayland-1",
            "wayland-2",
            "wayland-1.lock",
            "wayland-1-awww-daemon.sock",
            "wayland-0-awww-daemon.sock",
            "Alacritty-wayland-1-59727.sock",
            "bus",
        ] {
            std::fs::write(dir.join(n), b"").expect("fixture");
        }
        let got = candidates(&dir);
        assert_eq!(
            got,
            vec!["wayland-1".to_string(), "wayland-2".to_string()],
            "only `wayland-<digits>` is a compositor socket; the rest are other \
             programs' sockets, and connecting to one of those succeeds and then \
             speaks the wrong protocol"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn no_runtime_directory_is_reported_as_no_display_and_never_as_empty() {
        let _guard = crate::test_env::lock();
        let saved = std::env::var_os("XDG_RUNTIME_DIR");
        std::env::remove_var("XDG_RUNTIME_DIR");
        let got = display();
        if let Some(v) = saved {
            std::env::set_var("XDG_RUNTIME_DIR", v);
        }
        let why = got.expect_err("with no XDG_RUNTIME_DIR there is nowhere to look");
        assert!(why.contains("XDG_RUNTIME_DIR"), "{why}");
        assert!(
            !why.contains("empty"),
            "a daemon with no display must never report an empty clipboard: {why}"
        );
    }

    #[test]
    fn a_runtime_directory_with_no_compositor_says_so() {
        let _guard = crate::test_env::lock();
        let dir = std::env::temp_dir().join(format!("rime-clip-none-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let saved = std::env::var_os("XDG_RUNTIME_DIR");
        let saved_wd = std::env::var_os("WAYLAND_DISPLAY");
        std::env::set_var("XDG_RUNTIME_DIR", &dir);
        std::env::remove_var("WAYLAND_DISPLAY");
        let got = display();
        match saved {
            Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
            None => std::env::remove_var("XDG_RUNTIME_DIR"),
        }
        if let Some(v) = saved_wd {
            std::env::set_var("WAYLAND_DISPLAY", v);
        }
        std::fs::remove_dir_all(&dir).ok();
        let why = got.expect_err("an empty runtime dir has no compositor");
        assert!(why.contains("Wayland"), "{why}");
    }

    #[test]
    fn a_dead_socket_is_not_a_display() {
        // The case that made the probe necessary: two sockets, the first one
        // stale. A glob-and-take-the-first would pick the corpse.
        let _guard = crate::test_env::lock();
        let dir = std::env::temp_dir().join(format!("rime-clip-stale-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("temp dir");
        // wayland-1: a plain file, which connect(2) refuses.
        std::fs::write(dir.join("wayland-1"), b"").expect("fixture");
        // wayland-2: a real listening socket.
        let live = std::os::unix::net::UnixListener::bind(dir.join("wayland-2")).expect("bind");

        let saved = std::env::var_os("XDG_RUNTIME_DIR");
        let saved_wd = std::env::var_os("WAYLAND_DISPLAY");
        std::env::set_var("XDG_RUNTIME_DIR", &dir);
        std::env::remove_var("WAYLAND_DISPLAY");
        let got = display();
        match saved {
            Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
            None => std::env::remove_var("XDG_RUNTIME_DIR"),
        }
        if let Some(v) = saved_wd {
            std::env::set_var("WAYLAND_DISPLAY", v);
        }
        drop(live);
        std::fs::remove_dir_all(&dir).ok();

        let seat = got.expect("the live socket is found past the dead one");
        assert_eq!(seat.display, "wayland-2");
    }

    #[test]
    fn a_clipboard_read_is_not_one_of_the_requests_that_may_wait_for_a_person() {
        // `waits_on_a_human` drops rime-remoted's 300-second deadline. This
        // verb must never be in that set: nothing about it reaches a dialog,
        // and the whole point of READ_SECS is that a wedged application does
        // not get to hold the phone's connection. Asserted rather than left to
        // the `_ => false` arm, which would classify a new variant silently.
        assert!(!Request::Clipboard.waits_on_a_human());
        assert!(Duration::from_secs(READ_SECS) < Duration::from_secs(300));
    }
}
