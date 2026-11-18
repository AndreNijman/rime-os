//! `rime remote` — pairing a phone, and taking it away again.
//!
//! Four verbs and one rule between them: **pairing is something a human does
//! at this machine**, and everything else can be scripted. `rime remote pair`
//! is refused by `rime-remoted` for any caller that does not classify as a
//! §7 local origin, which an agent inside a managed session never does.
//!
//! The pairing payload is printed here rather than by the service, because
//! the service has no terminal and the terminal is where the person is. What
//! this build does **not** print is a QR code: no encoder is vendored, and a
//! wrong QR is worse than none — a phone scans it, fails, and the person
//! concludes their camera is broken. P1-051's first criterion is a QR from
//! Rime Shell, which has a renderer; see [`qr_block`].

use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use clap::Subcommand;
use serde::{Deserialize, Serialize};

#[derive(Subcommand, Debug)]
pub enum RemoteCmd {
    /// Show a pairing code for a phone to scan.
    ///
    /// The code is good for three minutes and pairs exactly one device. It
    /// carries this machine's public key, so the device that scans it can
    /// never be talked into trusting a different machine.
    ///
    /// Refused unless you are running it yourself: an agent cannot pair a
    /// device on your behalf.
    Pair {
        /// Print the pairing payload as text instead of drawing a QR code.
        ///
        /// For a terminal that cannot draw one, and for pasting into a device
        /// that has no camera.
        #[arg(long)]
        text: bool,
    },
    /// Every device this machine has paired, and which of them are revoked.
    Devices {
        #[arg(long)]
        json: bool,
    },
    /// Take a device's access away.
    ///
    /// Immediate: a connection the device is already holding is dropped, not
    /// merely refused next time. The record is kept so the listing can still
    /// show that it was revoked and when.
    Revoke {
        /// Device id, or its name when that names exactly one.
        device: String,
    },
    /// Make a paired phone ring, to find it.
    ///
    /// Goes over its connection when Rime Remote is open on it, and through
    /// its push service when it is not. Says which, and says "asked" when
    /// nothing came back to prove the phone heard it.
    Ring {
        /// Device id, or its name when that names exactly one.
        device: String,
    },
    /// Turn Do Not Disturb on or off on every phone that follows this
    /// computer's.
    ///
    /// Reaches each connected phone that has Focus turned on in Rime Remote,
    /// and says how many; none is not an error. It sets a state rather than
    /// flipping one, so a phone already there is left as it is. Rime Shell
    /// runs this when you toggle Do Not Disturb, if "Do Not Disturb follows
    /// your phone" is on.
    Focus {
        /// `on` or `off`.
        state: FocusState,
    },
    /// Put text on the clipboard of every phone that asked for this
    /// computer's clipboard.
    ///
    /// The text is read from standard input, exactly as it is (never from an
    /// argument: any process can read those). It reaches each connected
    /// phone that has "Get the computer's clipboard" turned on in Rime
    /// Remote, and the command says how many; none is not an error. A phone
    /// that put this very text here itself in the last ten seconds is not
    /// sent it back. Rime Shell runs this for each copy while "Send my
    /// clipboard to my phone" is on.
    Clipboard {
        /// Mark the text as sensitive, such as a password: the phone keeps it
        /// out of its clipboard preview.
        #[arg(long)]
        sensitive: bool,
    },
    /// Send files or text to a paired phone.
    ///
    /// The phone takes it without asking: text goes onto its clipboard, a
    /// file into its Downloads. That is why only you, at this machine, can
    /// send; an agent cannot. When Rime Remote is not open on the phone, what
    /// you send waits here for 15 minutes and the phone is asked, through its
    /// push service, to come and take it.
    #[command(group(clap::ArgGroup::new("what").required(true).args(["files", "clipboard", "text"])))]
    Send {
        /// Device id, or its name when that names exactly one.
        device: String,
        /// Files to send, each on its own.
        files: Vec<PathBuf>,
        /// Send what is on this computer's clipboard, as text.
        #[arg(long)]
        clipboard: bool,
        /// Send this text. `-` reads it from standard input.
        #[arg(long, value_name = "TEXT")]
        text: Option<String>,
    },
    /// Whether the service is running, and how a device would reach it.
    ///
    /// Including every connection open right now: which path it came in on,
    /// and how good it is. That is the measurement P1-052 asks to be visible
    /// on the desktop, and `--json` is how Rime Settings reads it.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Turn Rime Remote on for this account.
    ///
    /// Enables and starts the per-user service, the same way `rime agent
    /// enable` does for the agent runtime. It does **not** open the firewall:
    /// LAN access needs `rime firewall allow rime-remote`, which is root, and
    /// is a separate decision because the relay path works without it.
    Enable,
}

/// What `rime remote focus` sets.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusState {
    On,
    Off,
}

// The control protocol, duplicated here as the client half. Kept in step by
// `rime-remoted`'s own round-trip tests plus the shape assertions below; a
// shared crate for four request variants would be a crate to keep in step
// instead of a struct.
#[derive(Debug, Serialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum Request {
    Status,
    Pair,
    Devices,
    Revoke { device: String },
    Ring { device: String },
    Focus { dnd: bool },
    Clipboard {
        text: String,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        sensitive: bool,
    },
    Send {
        device: String,
        kind: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
}

/// A phone's last report, as `rime-remoted` keeps it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Phone {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    battery: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    charging: Option<bool>,
    #[serde(default)]
    can: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    network: Option<Network>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dnd: Option<bool>,
    reported_ms: u64,
}

/// How the phone said it is connected (contract §2.5): `type` is one of
/// wifi, cellular, ethernet, none, other; `signal` 0 to 4 when known.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Network {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    signal: Option<u8>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
enum Reply {
    Status {
        version: u32,
        key: String,
        machine: String,
        lan: Vec<String>,
        relay: Option<String>,
        rendezvous: String,
        paired: usize,
        offer_ms_left: Option<u64>,
        #[serde(default)]
        connections: Vec<rime_remote_core::rendezvous::Connection>,
    },
    Offer {
        qr: String,
        expires_ms: u64,
    },
    Devices {
        devices: Vec<rime_remote_core::device::Device>,
        #[serde(default)]
        phones: std::collections::BTreeMap<String, Phone>,
    },
    Rang {
        device: String,
        path: String,
    },
    Focused {
        reached: usize,
    },
    ClipboardSent {
        reached: usize,
    },
    Sent {
        device: String,
        path: String,
        #[allow(dead_code)]
        id: u64,
        #[serde(default)]
        push_error: Option<String>,
    },
    Ok,
    Error {
        message: String,
    },
}

pub fn remote(cmd: RemoteCmd) -> i32 {
    // The exit code is this function's contract and the error printing is
    // its own, matching `agent::agent`: the dispatcher in `main` takes an
    // `i32` from every arm and a `Result` here would be one arm that behaves
    // differently for no reason a user could see.
    let result = match cmd {
        RemoteCmd::Pair { text } => pair(text),
        RemoteCmd::Devices { json } => devices(json),
        RemoteCmd::Revoke { device } => revoke(&device),
        RemoteCmd::Ring { device } => ring(&device),
        RemoteCmd::Focus { state } => focus(state == FocusState::On),
        RemoteCmd::Clipboard { sensitive } => clipboard(std::io::stdin().lock(), sensitive),
        RemoteCmd::Send { device, files, clipboard, text } => send(&device, &files, clipboard, text),
        RemoteCmd::Status { json } => status(json),
        RemoteCmd::Enable => enable(),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("rime: {e:#}");
            1
        }
    }
}

fn socket_path() -> PathBuf {
    rime_agent_core::paths::runtime_dir()
        .join("rime-remoted")
        .join("control.sock")
}

fn call(req: &Request) -> Result<Reply> {
    call_with(req, None)
}

/// One request, with `fd` passed beside its line when there is one (the
/// file `send` offers: the service reads it through this descriptor, never
/// by its path, so it needs no access to where the file lives).
fn call_with(req: &Request, fd: Option<std::os::fd::RawFd>) -> Result<Reply> {
    let path = socket_path();
    let stream = UnixStream::connect(&path).with_context(|| {
        format!(
            "Rime Remote is not running.\n\
             turn it on with: systemctl --user enable --now rime-remoted\n\
             (socket: {})",
            path.display()
        )
    })?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(20)))
        .ok();
    let mut writer = stream.try_clone().context("cloning the control socket")?;
    let mut reader = BufReader::new(stream);
    let mut line = serde_json::to_string(req)?;
    line.push('\n');
    match fd {
        Some(fd) => rime_remote_core::fdpass::send_with_fd(&writer, line.as_bytes(), fd)
            .context("sending the request")?,
        None => writer.write_all(line.as_bytes())?,
    }
    writer.flush()?;
    let mut reply = String::new();
    reader.read_line(&mut reply).context("reading the reply")?;
    serde_json::from_str(reply.trim()).with_context(|| format!("unreadable reply: {reply}"))
}

/// Start the daemon if it is not answering, and wait for it to.
///
/// Rime Remote is enabled for every account, but pairing must not depend on
/// it having come up: a unit that crash-looped once (a port another account
/// held, before that was fixed) stays down until something starts it. So the
/// commands a person reaches for to pair start it themselves. `start`, not
/// `enable` — whether it runs at login is the image's decision, not this
/// command's side effect.
fn ensure_running() -> bool {
    let socket = socket_path();
    if UnixStream::connect(&socket).is_ok() {
        return true;
    }
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "reset-failed", "rime-remoted"])
        .output();
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "start", "rime-remoted"])
        .output();
    wait_for_socket(&socket)
}

/// Whether the daemon answers on `socket` within three seconds.
fn wait_for_socket(socket: &std::path::Path) -> bool {
    (0..30).any(|_| {
        if UnixStream::connect(socket).is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        false
    })
}

fn pair(text: bool) -> Result<i32> {
    // Best effort: if it still cannot be reached, `call` below says so, with
    // the socket path, exactly as before.
    ensure_running();
    match call(&Request::Pair)? {
        Reply::Offer { qr, expires_ms } => {
            let left = expires_ms.saturating_sub(rime_remote_core::now_ms()) / 1000;
            if text {
                println!("{qr}");
            } else {
                print!("{}", qr_block(&qr));
            }
            eprintln!(
                "\nScan this with Rime Remote. It is good for {left} seconds and pairs one \
                 device."
            );
            eprintln!(
                "The code carries this machine's public key, so the phone that scans it cannot \
                 be talked into trusting a different machine."
            );
            // With a relay the code also names it, and pairing is the moment to
            // say what its operator can see — the same sentence `status` prints.
            if let Ok(Reply::Status { relay: Some(_), .. }) = call(&Request::Status) {
                eprintln!(
                    "Away from this network the phone connects {}.",
                    rime_remote_core::rendezvous::Path::Relay.disclosure()
                );
            }
            Ok(0)
        }
        Reply::Error { message } => {
            eprintln!("rime: {message}");
            Ok(1)
        }
        other => bail!("unexpected reply: {other:?}"),
    }
}

fn devices(json: bool) -> Result<i32> {
    let Reply::Devices { devices, phones } = call(&Request::Devices)? else {
        return unexpected();
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&with_phones(&devices, &phones)?)?);
        return Ok(0);
    }
    if devices.is_empty() {
        println!("no devices paired. `rime remote pair` shows a code.");
        return Ok(0);
    }
    println!("{:<18} {:<20} {:<9} {:<8} LAST SEEN", "ID", "NAME", "STATE", "PATH");
    for d in &devices {
        println!(
            "{:<18} {:<20} {:<9} {:<8} {}",
            d.id,
            truncate(&d.name, 20),
            d.state(),
            d.last_path.as_deref().unwrap_or("-"),
            d.last_seen_ms.map(ago).unwrap_or_else(|| "never".into()),
        );
    }
    // Said once, at the end, rather than as a column: it is a property of the
    // device the desktop cannot check, and a tick in a table would read as a
    // fact this machine had verified.
    let gated: Vec<&str> = devices
        .iter()
        .filter(|d| d.requires_user_verification && d.is_active())
        .map(|d| d.name.as_str())
        .collect();
    if !gated.is_empty() {
        println!(
            "\n{} said its key is held behind a biometric or device lock. That is the \
             device's own claim; this machine cannot verify it.",
            gated.join(", ")
        );
    }
    let reported: Vec<String> = devices
        .iter()
        .filter(|d| d.is_active())
        .filter_map(|d| phones.get(&d.id).and_then(|p| phone_line(&d.name, p)))
        .collect();
    if !reported.is_empty() {
        println!();
        for line in reported {
            println!("{line}");
        }
    }
    Ok(0)
}

/// The device list as JSON, each paired phone with its last report under
/// `phone`. Added to each device rather than beside the list, so a reader of
/// the old array keeps working.
fn with_phones(
    devices: &[rime_remote_core::device::Device],
    phones: &std::collections::BTreeMap<String, Phone>,
) -> Result<serde_json::Value> {
    let mut out = Vec::with_capacity(devices.len());
    for d in devices {
        let mut v = serde_json::to_value(d)?;
        if let (Some(p), Some(obj)) = (phones.get(&d.id).filter(|_| d.is_active()), v.as_object_mut()) {
            obj.insert("phone".into(), serde_json::to_value(p)?);
        }
        out.push(v);
    }
    Ok(serde_json::Value::Array(out))
}

/// "Pixel 8: battery 62%, charging, Wi-Fi signal 3/4, Do Not Disturb,
/// reported 3m ago": whatever the phone reported, or nothing when it reported
/// none of it. Do Not Disturb is named only while it is on; off is the
/// ordinary state and a word for it on every line would be noise.
fn phone_line(name: &str, p: &Phone) -> Option<String> {
    let mut said: Vec<String> = Vec::new();
    if let Some(battery) = p.battery {
        let charging = if p.charging == Some(true) { ", charging" } else { "" };
        said.push(format!("battery {battery}%{charging}"));
    }
    if let Some(n) = &p.network {
        said.push(network_words(n));
    }
    if p.dnd == Some(true) {
        said.push("Do Not Disturb".into());
    }
    if said.is_empty() {
        return None;
    }
    Some(format!("{name}: {}, reported {}", said.join(", "), ago(p.reported_ms)))
}

/// "Wi-Fi signal 3/4", "mobile data", "no network".
fn network_words(n: &Network) -> String {
    let kind = match n.kind.as_str() {
        "wifi" => "Wi-Fi",
        "cellular" => "mobile data",
        "ethernet" => "Ethernet",
        "none" => "no network",
        _ => "online",
    };
    match n.signal {
        Some(s) => format!("{kind} signal {s}/4"),
        None => kind.to_string(),
    }
}

fn focus(dnd: bool) -> Result<i32> {
    let state = if dnd { "on" } else { "off" };
    match call(&Request::Focus { dnd })? {
        Reply::Focused { reached: 0 } => {
            eprintln!(
                "rime: No phone follows this computer's Do Not Disturb right now: none is connected \
                 with Focus turned on in Rime Remote."
            );
            Ok(0)
        }
        Reply::Focused { reached } => {
            let phones = if reached == 1 { "phone" } else { "phones" };
            eprintln!("rime: told {reached} {phones} to turn Do Not Disturb {state}.");
            Ok(0)
        }
        Reply::Error { message } => {
            eprintln!("rime: {message}");
            Ok(1)
        }
        _ => unexpected(),
    }
}

/// The most text `rime remote clipboard` sends: rime-remoted's cap
/// (contract §2.10), checked here too so a huge paste is refused in words
/// before it is read whole.
const MAX_CLIPBOARD_BYTES: usize = 64 * 1024;

fn clipboard(input: impl std::io::Read, sensitive: bool) -> Result<i32> {
    use std::io::Read;
    // Verbatim: not trimmed. rime-remoted compares it byte for byte with
    // what a phone put on this clipboard, to not send that phone its own
    // text back.
    let mut bytes = Vec::new();
    input
        .take(MAX_CLIPBOARD_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .context("reading the text")?;
    if bytes.is_empty() {
        bail!("there is no text to send: give it on standard input");
    }
    if bytes.len() > MAX_CLIPBOARD_BYTES {
        bail!("that text is more than {MAX_CLIPBOARD_BYTES} bytes, more than a phone's clipboard is sent");
    }
    let text = String::from_utf8(bytes).context("the text is not UTF-8")?;
    match call(&Request::Clipboard { text, sensitive })? {
        Reply::ClipboardSent { reached: 0 } => {
            eprintln!(
                "rime: No phone took the clipboard: none is connected with \"Get the computer's clipboard\" \
                 turned on in Rime Remote."
            );
            Ok(0)
        }
        Reply::ClipboardSent { reached } => {
            let phones = if reached == 1 { "phone" } else { "phones" };
            eprintln!("rime: put the text on {reached} {phones}' clipboard.");
            Ok(0)
        }
        Reply::Error { message } => {
            eprintln!("rime: {message}");
            Ok(1)
        }
        _ => unexpected(),
    }
}

fn ring(device: &str) -> Result<i32> {
    match call(&Request::Ring {
        device: device.to_string(),
    })? {
        Reply::Rang { device, path } if path == "link" => {
            eprintln!("rime: {device} is ringing.");
            Ok(0)
        }
        Reply::Rang { device, .. } => {
            eprintln!(
                "rime: asked {device} to ring through its push service. It rings if it is \
                 switched on and online."
            );
            Ok(0)
        }
        Reply::Error { message } => {
            eprintln!("rime: {message}");
            Ok(1)
        }
        _ => unexpected(),
    }
}

/// The most text one send carries; the service holds the same limit.
const MAX_TEXT_BYTES: usize = 8192;

/// How long `wl-paste` gets to answer: a clipboard owner that hangs must not
/// hang the command.
const PASTE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

fn send(device: &str, files: &[PathBuf], clipboard: bool, text: Option<String>) -> Result<i32> {
    if clipboard || text.is_some() {
        let text = match text {
            Some(t) if t == "-" => read_capped(std::io::stdin().lock())?,
            Some(t) => t,
            None => paste()?,
        };
        text_fits(&text)?;
        let reply = call(&Request::Send { device: device.to_string(), kind: "text", name: None, text: Some(text) })?;
        return Ok(report_sent(reply, "the text"));
    }
    let mut code = 0;
    for path in files {
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("rime: could not open {}: {e}", path.display());
                code = 1;
                continue;
            }
        };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let req = Request::Send { device: device.to_string(), kind: "file", name: Some(name.clone()), text: None };
        use std::os::fd::AsRawFd;
        let reply = call_with(&req, Some(file.as_raw_fd()))?;
        if report_sent(reply, &name) != 0 {
            code = 1;
        }
    }
    Ok(code)
}

fn text_fits(text: &str) -> Result<()> {
    if text.is_empty() {
        bail!("there is no text to send");
    }
    if text.len() > MAX_TEXT_BYTES {
        bail!(
            "that text is {} bytes, and a phone is sent at most {MAX_TEXT_BYTES} at a time; \
             put it in a file and send the file",
            text.len()
        );
    }
    Ok(())
}

/// Up to one byte past the limit, so an over-long text is refused in words
/// rather than read whole.
fn read_capped(r: impl std::io::Read) -> Result<String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    r.take(MAX_TEXT_BYTES as u64 + 1).read_to_end(&mut bytes).context("reading the text")?;
    String::from_utf8(bytes).context("the text is not UTF-8")
}

/// This computer's clipboard, as text, from `wl-paste`.
fn paste() -> Result<String> {
    use std::process::{Command, Stdio};
    let mut child = Command::new("wl-paste")
        .args(["--no-newline", "--type", "text"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not run wl-paste to read the clipboard")?;
    let out = child.stdout.take().expect("piped");
    let reader = std::thread::spawn(move || read_capped(out));
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() > PASTE_DEADLINE {
            let _ = child.kill();
            let _ = child.wait();
            bail!("the clipboard did not answer within {} seconds", PASTE_DEADLINE.as_secs());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let text = reader.join().map_err(|_| anyhow::anyhow!("reading the clipboard failed"))??;
    if !status.success() {
        bail!("the clipboard holds no text");
    }
    Ok(text)
}

/// Says what became of one send, and returns its exit code.
fn report_sent(reply: Reply, what: &str) -> i32 {
    match reply {
        Reply::Sent { device, path, push_error, .. } => {
            eprintln!("rime: {}", sent_sentence(&device, &path, what, push_error.as_deref()));
            0
        }
        Reply::Error { message } => {
            eprintln!("rime: {message}");
            1
        }
        other => {
            eprintln!("rime: rime-remoted sent a reply this build does not expect: {other:?}");
            1
        }
    }
}

fn sent_sentence(device: &str, path: &str, what: &str, push_error: Option<&str>) -> String {
    match path {
        "link" => format!("{device} has {what} and is taking it now."),
        "push" => format!(
            "{device} is not connected, so it was asked through its push service to take {what}. \
             It waits here for 15 minutes."
        ),
        _ => {
            // `what` starts a sentence here, so "the text" becomes "The text".
            let mut chars = what.chars();
            let what: String = chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default();
            let mut s = format!(
                "{device} is not connected. {what} waits here for 15 minutes: open Rime Remote on \
                 the phone to take it."
            );
            if let Some(e) = push_error {
                s.push_str(&format!(" (Its push service could not be reached: {e}.)"));
            }
            s
        }
    }
}

fn revoke(device: &str) -> Result<i32> {
    match call(&Request::Revoke {
        device: device.to_string(),
    })? {
        Reply::Ok => {
            eprintln!("rime: {device} revoked. Any connection it was holding has been dropped.");
            Ok(0)
        }
        Reply::Error { message } => {
            eprintln!("rime: {message}");
            Ok(1)
        }
        _ => unexpected(),
    }
}

fn status(json: bool) -> Result<i32> {
    let reply = call(&Request::Status)?;
    let Reply::Status {
        version,
        key,
        machine,
        lan,
        relay,
        rendezvous,
        paired,
        offer_ms_left,
        connections,
    } = reply
    else {
        return unexpected();
    };
    if json {
        // The daemon's own reply, re-emitted rather than re-assembled: a
        // second serialisation here is a second thing to keep in step with
        // the page that reads it.
        println!(
            "{}",
            serde_json::json!({
                "machine": machine,
                "protocol": version,
                "identity": key,
                "paired": paired,
                "lan": lan,
                "relay": relay,
                "rendezvous": rendezvous,
                "offer_ms_left": offer_ms_left,
                "connections": connections
                    .iter()
                    .map(|c| serde_json::json!({
                        "device_id": c.device_id,
                        "device_name": c.device_name,
                        "path": c.path,
                        "since_ms": c.since_ms,
                        "rtt_ms": c.rtt_ms,
                        "quality": c.quality().as_str(),
                        "disclosure": c.disclosure(),
                    }))
                    .collect::<Vec<_>>(),
            })
        );
        return Ok(0);
    }
    println!("machine        {machine}");
    println!("protocol       {version}");
    println!("identity       {key}");
    println!("paired         {paired}");
    println!(
        "lan            {}",
        if lan.is_empty() {
            "none — this machine has no reachable address".to_string()
        } else {
            lan.join(", ")
        }
    );
    match &relay {
        Some(r) => {
            println!("relay          {r}");
            println!("rendezvous     {rendezvous}");
            println!(
                "\n{}",
                rime_remote_core::rendezvous::Path::Relay.disclosure()
            );
        }
        None => println!("relay          none — this machine is reachable on the LAN only"),
    }
    if connections.is_empty() {
        println!("connected      no device is connected right now");
    } else {
        println!("connected      {} device(s)", connections.len());
        for c in &connections {
            // The round trip AND the band. The number is for whoever wants
            // it; the word is what tells somebody whether their terminal is
            // going to feel wrong.
            let rtt = match c.rtt_ms {
                Some(ms) => format!("{ms} ms"),
                None => "not measured yet".to_string(),
            };
            println!("{}", connection_line(c, &rtt));
        }
    }
    if let Some(ms) = offer_ms_left {
        println!("\na pairing code is open for another {} seconds", ms / 1000);
    }
    Ok(0)
}

/// One line about one open connection.
///
/// Extracted so it can be asserted about without starting a daemon, and
/// because the three things it has to carry — which device, which path, how
/// good — are exactly P1-052's last criterion and are easy to lose one of.
fn connection_line(c: &rime_remote_core::rendezvous::Connection, rtt: &str) -> String {
    format!(
        "  {} over {} — {} ({})",
        c.device_name,
        c.path,
        c.quality(),
        rtt
    )
}

/// `systemctl --user enable --now rime-remoted`, and say what it did not do.
///
/// Mirrors `rime agent enable`, which exists because a per-user daemon that
/// has to be started with a systemctl invocation is one people get wrong.
fn enable() -> Result<i32> {
    // A unit that crash-looped earlier is start-limited; clear that first, or
    // enabling it again changes nothing and says nothing.
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "reset-failed", "rime-remoted"])
        .output();
    let out = std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", "rime-remoted"])
        .output()
        .context("running systemctl")?;
    // `enable --now` succeeds once the process has STARTED, including one that
    // exits a moment later — which is how "rime:" with nothing after it, and a
    // silent `systemctl`, both happened while the daemon was dying on a port
    // in use. So wait for it to answer, and when it does not, show its own
    // last words rather than systemctl's.
    let running = out.status.success() && wait_for_socket(&socket_path());
    if !running {
        let systemctl = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let journal = std::process::Command::new("journalctl")
            .args(["--user", "-u", "rime-remoted", "-n", "20", "-o", "cat", "--no-pager"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        let why = journal
            .lines()
            .rev()
            .find(|l| l.starts_with("rime-remoted:"))
            .map(str::to_string)
            .or_else(|| (!systemctl.is_empty()).then_some(systemctl))
            .unwrap_or_else(|| "it exited without saying why; see `journalctl --user -u rime-remoted`".to_string());
        eprintln!("rime: Rime Remote did not start: {why}");
        return Ok(1);
    }
    eprintln!("rime: Rime Remote is on. `rime remote pair` shows a code for a phone.");
    eprintln!(
        "rime: the LAN port is still closed. Open it with `sudo rime firewall allow \
         rime-remote`, or leave it shut and reach this machine through a relay."
    );
    Ok(0)
}

fn unexpected() -> Result<i32> {
    bail!("the remote service answered something this build does not understand")
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    // Truncated on characters, not bytes: a name in a non-Latin script would
    // otherwise be cut mid-codepoint and print as a replacement character.
    s.chars().take(n.saturating_sub(1)).collect::<String>() + "…"
}

fn ago(ms: u64) -> String {
    let now = rime_remote_core::now_ms();
    let secs = now.saturating_sub(ms) / 1000;
    match secs {
        0..=59 => format!("{secs}s ago"),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}

/// Draw a QR code for `payload` using half-block characters.
///
/// Deliberately not a dependency. A QR encoder is a few hundred lines of
/// Reed-Solomon and a fixed set of tables, and the version here does the one
/// job this command needs — but the shipped implementation is not that
/// either: see [`qr_block`]'s body. What is here now is the fallback, and it
/// is honest about being one.
fn qr_block(payload: &str) -> String {
    // No QR encoder is vendored in the terminal. Printing a wrong QR code would
    // be worse than printing none: a phone would scan it, fail, and the person
    // would conclude their camera or the app was broken. So this prints the
    // payload and points at the page that does draw one — Settings → Pair a
    // device, which has shipped and has a renderer.
    format!(
        "{payload}\n\n\
         (To scan a QR code, open Rime Settings → Pair a device. Or paste the line above into \
         Rime Remote on your phone.)\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_control_socket_is_the_one_the_service_binds() {
        // Two crates, one path. They are written independently, and a drift
        // here is `rime remote status` reporting that the service is not
        // running while it is.
        let mine = socket_path();
        assert!(mine.ends_with("rime-remoted/control.sock"), "{mine:?}");
    }

    #[test]
    fn every_request_survives_the_line_framing() {
        for r in [
            Request::Status,
            Request::Pair,
            Request::Devices,
            Request::Revoke {
                device: "pixel-8".into(),
            },
            Request::Ring {
                device: "pixel-8".into(),
            },
        ] {
            let text = serde_json::to_string(&r).expect("serialise");
            assert!(!text.contains('\n'), "{text}");
        }
    }

    #[test]
    fn the_replies_this_build_parses_are_the_ones_the_service_sends() {
        // Written as the wire text rather than as the service's own types,
        // because that is what actually crosses the socket and it is the
        // thing that can drift.
        for text in [
            r#"{"reply":"ok"}"#,
            r#"{"reply":"error","message":"nope"}"#,
            r#"{"reply":"offer","qr":"apex-remote:abc","expires_ms":1}"#,  // rime-rename: keep (SCHEME)
            r#"{"reply":"devices","devices":[]}"#,
            r#"{"reply":"devices","devices":[],"phones":{"d":{"battery":5,"can":["ring"],"reported_ms":1}}}"#,
            r#"{"reply":"rang","device":"Pixel","path":"push"}"#,
            r#"{"reply":"focused","reached":0}"#,
            r#"{"reply":"clipboard_sent","reached":2}"#,
            r#"{"reply":"devices","devices":[],"phones":{"d":{"battery":5,"can":["focus"],"network":{"type":"wifi","signal":3},"dnd":true,"reported_ms":1}}}"#,
            r#"{"reply":"status","version":1,"key":"k","machine":"l16","lan":[],"relay":null,"rendezvous":"r","paired":0,"offer_ms_left":null}"#,
        ] {
            serde_json::from_str::<Reply>(text).unwrap_or_else(|e| panic!("{text}: {e}"));
        }
    }

    #[test]
    fn send_is_its_own_request_on_the_wire_and_a_file_carries_no_text() {
        let file = Request::Send { device: "Pixel".into(), kind: "file", name: Some("a.jpg".into()), text: None };
        assert_eq!(
            serde_json::to_string(&file).expect("serialise"),
            r#"{"cmd":"send","device":"Pixel","kind":"file","name":"a.jpg"}"#
        );
        let text = Request::Send { device: "Pixel".into(), kind: "text", name: None, text: Some("a\nb".into()) };
        let line = serde_json::to_string(&text).expect("serialise");
        assert_eq!(line, r#"{"cmd":"send","device":"Pixel","kind":"text","text":"a\nb"}"#);
        assert!(!line.contains('\n'));
        for reply in [
            r#"{"reply":"sent","device":"Pixel","path":"link","id":3}"#,
            r#"{"reply":"sent","device":"Pixel","path":"waiting","id":4,"push_error":"gone"}"#,
        ] {
            assert!(matches!(serde_json::from_str::<Reply>(reply), Ok(Reply::Sent { .. })), "{reply}");
        }
    }

    #[test]
    fn each_way_a_send_goes_says_so() {
        assert_eq!(sent_sentence("Pixel", "link", "a.jpg", None), "Pixel has a.jpg and is taking it now.");
        assert!(sent_sentence("Pixel", "push", "a.jpg", None).contains("push service"));
        let w = sent_sentence("Pixel", "waiting", "the text", Some("410 Gone"));
        assert!(w.starts_with("Pixel is not connected. The text waits here for 15 minutes"), "{w}");
        assert!(w.ends_with("(Its push service could not be reached: 410 Gone.)"), "{w}");
    }

    #[test]
    fn text_over_the_limit_is_refused_before_it_is_read_whole() {
        let long = "x".repeat(MAX_TEXT_BYTES * 4);
        let read = read_capped(long.as_bytes()).expect("read");
        assert_eq!(read.len(), MAX_TEXT_BYTES + 1, "read past the cap");
        assert!(text_fits(&read).unwrap_err().to_string().contains("send the file"));
        assert!(text_fits("").is_err());
        assert!(text_fits(&"y".repeat(MAX_TEXT_BYTES)).is_ok());
    }

    #[test]
    fn send_needs_something_to_send_and_only_one_kind() {
        use clap::Parser;
        #[derive(clap::Parser)]
        struct Cli {
            #[command(subcommand)]
            cmd: RemoteCmd,
        }
        assert!(Cli::try_parse_from(["r", "send", "Pixel"]).is_err(), "nothing to send was accepted");
        assert!(Cli::try_parse_from(["r", "send", "Pixel", "a.jpg", "--clipboard"]).is_err());
        assert!(Cli::try_parse_from(["r", "send", "Pixel", "--text", "x", "--clipboard"]).is_err());
        match Cli::try_parse_from(["r", "send", "Pixel", "a.jpg", "b.pdf"]).expect("files").cmd {
            RemoteCmd::Send { files, clipboard: false, text: None, .. } => assert_eq!(files.len(), 2),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn ring_is_its_own_request_on_the_wire() {
        let text = serde_json::to_string(&Request::Ring { device: "Pixel".into() }).expect("serialise");
        assert_eq!(text, r#"{"cmd":"ring","device":"Pixel"}"#);
    }

    #[test]
    fn a_phone_report_rides_on_its_device_and_only_while_it_is_paired() {
        let devices: Vec<rime_remote_core::device::Device> = serde_json::from_str(
            r#"[{"id":"aaaa","name":"Pixel","public_key":"k","paired_ms":1},
                {"id":"bbbb","name":"Old","public_key":"k2","paired_ms":2,"revoked_ms":3},
                {"id":"cccc","name":"Tab","public_key":"k3","paired_ms":4}]"#,
        )
        .expect("devices");
        let phone = Phone {
            battery: Some(62),
            charging: Some(true),
            can: vec!["ring".into()],
            network: None,
            dnd: None,
            reported_ms: 0,
        };
        let phones: std::collections::BTreeMap<String, Phone> =
            [("aaaa".to_string(), phone.clone()), ("bbbb".to_string(), phone.clone())].into();
        let v = with_phones(&devices, &phones).expect("json");
        assert_eq!(v[0]["phone"]["battery"], 62);
        assert!(v[1].get("phone").is_none(), "a revoked device's report must not show");
        assert!(v[2].get("phone").is_none(), "a device that never reported has none");
        assert_eq!(v[0]["name"], "Pixel", "the device's own fields are kept");
        let line = phone_line("Pixel", &phone).expect("line");
        assert!(line.starts_with("Pixel: battery 62%, charging, reported "), "{line}");
        assert_eq!(phone_line("Pixel", &Phone { battery: None, ..phone.clone() }), None);
    }

    #[test]
    fn a_phones_network_and_do_not_disturb_show_on_its_line_and_in_its_json() {
        let wifi = Some(Network { kind: "wifi".into(), signal: Some(3) });
        let phone = Phone {
            battery: Some(62),
            charging: Some(false),
            can: vec!["focus".into()],
            network: wifi.clone(),
            dnd: Some(true),
            reported_ms: 0,
        };
        let line = phone_line("Pixel", &phone).expect("line");
        assert!(line.starts_with("Pixel: battery 62%, Wi-Fi signal 3/4, Do Not Disturb, reported "), "{line}");
        // No battery is no longer no line: the network alone is worth saying.
        let mobile = Phone {
            battery: None,
            network: Some(Network { kind: "cellular".into(), signal: None }),
            dnd: Some(false),
            ..phone.clone()
        };
        let line = phone_line("Pixel", &mobile).expect("line");
        assert!(line.starts_with("Pixel: mobile data, reported "), "{line}");
        assert_eq!(network_words(&Network { kind: "none".into(), signal: None }), "no network");
        assert_eq!(network_words(&Network { kind: "ethernet".into(), signal: None }), "Ethernet");
        // `--json` re-serialises through this struct, so a field it lacked
        // would vanish from the output without a word.
        let v = serde_json::to_value(&phone).expect("json");
        assert_eq!(v["network"], serde_json::json!({"type": "wifi", "signal": 3}));
        assert_eq!(v["dnd"], true);
    }

    #[test]
    fn focus_is_its_own_request_on_the_wire_and_takes_on_or_off() {
        use clap::Parser;
        #[derive(clap::Parser)]
        struct Cli {
            #[command(subcommand)]
            cmd: RemoteCmd,
        }
        assert_eq!(serde_json::to_string(&Request::Focus { dnd: true }).unwrap(), r#"{"cmd":"focus","dnd":true}"#);
        assert!(matches!(Cli::try_parse_from(["r", "focus", "on"]).expect("on").cmd, RemoteCmd::Focus { state: FocusState::On }));
        assert!(matches!(Cli::try_parse_from(["r", "focus", "off"]).expect("off").cmd, RemoteCmd::Focus { state: FocusState::Off }));
        assert!(Cli::try_parse_from(["r", "focus", "toggle"]).is_err(), "focus sets a state; it does not flip one");
        assert!(Cli::try_parse_from(["r", "focus"]).is_err());
    }

    #[test]
    fn clipboard_reads_its_text_from_stdin_and_never_takes_it_as_an_argument() {
        use clap::Parser;
        #[derive(clap::Parser)]
        struct Cli {
            #[command(subcommand)]
            cmd: RemoteCmd,
        }
        assert!(matches!(Cli::try_parse_from(["r", "clipboard"]).expect("plain").cmd, RemoteCmd::Clipboard { sensitive: false }));
        assert!(matches!(
            Cli::try_parse_from(["r", "clipboard", "--sensitive"]).expect("sensitive").cmd,
            RemoteCmd::Clipboard { sensitive: true }
        ));
        assert!(Cli::try_parse_from(["r", "clipboard", "my password"]).is_err(), "text in argv is visible in /proc");
        assert_eq!(
            serde_json::to_string(&Request::Clipboard { text: " a\n".into(), sensitive: false }).unwrap(),
            r#"{"cmd":"clipboard","text":" a\n"}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::Clipboard { text: "a".into(), sensitive: true }).unwrap(),
            r#"{"cmd":"clipboard","text":"a","sensitive":true}"#
        );
        // Refused before any socket is touched: empty, over the cap, not UTF-8.
        assert!(clipboard(&b""[..], false).unwrap_err().to_string().contains("no text"));
        let over = vec![b'a'; MAX_CLIPBOARD_BYTES + 1];
        assert!(clipboard(&over[..], false).unwrap_err().to_string().contains("more than"));
        assert!(clipboard(&b"\xff\xfe"[..], false).unwrap_err().to_string().contains("UTF-8"));
    }

    #[test]
    fn a_name_is_truncated_on_characters_and_not_on_bytes() {
        assert_eq!(truncate("short", 20), "short");
        assert_eq!(truncate("abcdefghij", 5), "abcd…");
        // Multibyte: a byte-wise truncation would panic or emit a broken
        // codepoint here.
        let cyrillic = "телефонтелефон";
        let cut = truncate(cyrillic, 5);
        assert_eq!(cut.chars().count(), 5);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn the_pairing_output_does_not_pretend_to_be_a_qr_code() {
        // The failure this avoids: a phone scans a wrong QR, fails, and the
        // person blames their camera. If an encoder is ever vendored, this
        // test is what has to change with it.
        let out = qr_block("apex-remote:abc");  // rime-rename: keep (SCHEME)
        assert!(out.contains("apex-remote:abc"));  // rime-rename: keep (SCHEME)
        assert!(out.contains("Rime Settings"), "{out}");
        // The pairing page has shipped; the output must not still promise it.
        assert!(!out.contains("once it ships"), "{out}");
        assert!(out.contains("Pair a device"), "{out}");
    }

    #[test]
    fn elapsed_time_reads_in_the_unit_a_person_would_use() {
        let now = rime_remote_core::now_ms();
        assert!(ago(now).ends_with("s ago"));
        assert!(ago(now.saturating_sub(120_000)).ends_with("m ago"));
        assert!(ago(now.saturating_sub(7_200_000)).ends_with("h ago"));
        assert!(ago(now.saturating_sub(3 * 86_400_000)).ends_with("d ago"));
        // A timestamp from the future is not a panic and not a negative.
        assert_eq!(ago(now + 60_000), "0s ago");
    }

    #[test]
    fn a_status_reply_from_a_daemon_that_predates_connections_still_parses() {
        // The CLI and the daemon land in one image but not in one build, and
        // a shell page that showed nothing because a field was absent would
        // be a regression nobody could see coming.
        let old = r#"{"reply":"status","version":1,"key":"k","machine":"l16","lan":[],"relay":null,"rendezvous":"r","paired":0,"offer_ms_left":null}"#;
        let Reply::Status { connections, .. } = serde_json::from_str(old).expect("parse") else {
            panic!("not a status");
        };
        assert!(connections.is_empty());
    }

    #[test]
    fn a_connection_line_names_the_device_the_path_and_the_band() {
        // All three, because each has been the one somebody left out: a line
        // with only a number does not say whether a third party is carrying
        // it, and a line with only a path does not say whether it is usable.
        let c = rime_remote_core::rendezvous::Connection {
            device_id: "abcd".into(),
            device_name: "pixel-8".into(),
            path: "relay".into(),
            since_ms: 0,
            rtt_ms: Some(310),
        };
        let line = connection_line(&c, "310 ms");
        assert!(line.contains("pixel-8"), "{line}");
        assert!(line.contains("relay"), "{line}");
        assert!(line.contains("poor"), "{line}");
        assert!(line.contains("310 ms"), "{line}");

        // And an unmeasured connection says so rather than reporting the
        // worst band for a connection that has simply just opened.
        let fresh = rime_remote_core::rendezvous::Connection { rtt_ms: None, ..c };
        let line = connection_line(&fresh, "not measured yet");
        assert!(line.contains("unmeasured"), "{line}");
        assert!(!line.contains("poor"), "{line}");
    }
}
