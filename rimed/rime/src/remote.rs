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
    /// Read a phone's SMS conversations, or send one from it.
    ///
    /// The phone must have "Messages on your computer" turned on in Rime
    /// Remote and be connected. Nothing is kept on this computer: each run asks
    /// the phone again. Only you, at this machine, can read or send; an agent
    /// cannot, because messages carry sign-in codes.
    ///
    /// Without --thread it lists the newest conversations; with it, that
    /// conversation's newest messages. `--json` is how Rime Shell reads them.
    #[command(args_conflicts_with_subcommands = true)]
    Messages {
        #[command(subcommand)]
        action: Option<MessagesCmd>,
        /// Device id or name. May be left out when one phone has Messages on.
        #[arg(long)]
        device: Option<String>,
        /// Show this conversation's messages (its id from the list).
        #[arg(long, value_name = "ID")]
        thread: Option<u64>,
        /// Only what is older than this, in Unix milliseconds: the next page.
        #[arg(long, value_name = "MS")]
        before: Option<u64>,
        /// How many conversations or messages, from 1 to 50 (default 20).
        #[arg(long, value_name = "N")]
        limit: Option<u32>,
        #[arg(long)]
        json: bool,
    },
    /// See a phone's calls, answer or end them, and dial from it.
    ///
    /// The phone must have "Calls on your computer" turned on in Rime Remote
    /// and be connected. Without a subcommand it shows the call the phone has
    /// now and its newest calls. There is no sound: the call itself stays on
    /// the phone. Nothing is kept on this computer, and only you, at this
    /// machine, can do this; an agent cannot. `--json` is how Rime Shell
    /// reads it.
    #[command(args_conflicts_with_subcommands = true)]
    Calls {
        #[command(subcommand)]
        action: Option<CallsCmd>,
        /// Device id or name. May be left out when one phone has calls on.
        #[arg(long)]
        device: Option<String>,
        /// Only calls older than this, in Unix milliseconds: the next page.
        #[arg(long, value_name = "MS")]
        before: Option<u64>,
        /// How many calls, from 1 to 50 (default 20).
        #[arg(long, value_name = "N")]
        limit: Option<u32>,
        #[arg(long)]
        json: bool,
    },
    /// See a phone's recent photos and screenshots, and save one here.
    ///
    /// The phone must have "Photos on your computer" turned on in Rime Remote,
    /// be connected, and be unlocked. Without a subcommand it lists the newest
    /// photos; each one's small preview is kept under your runtime folder only
    /// until the phone disconnects, and `--json` gives its path, which is how
    /// Rime Shell shows a grid. Only you, at this machine, can do this; an
    /// agent cannot.
    #[command(args_conflicts_with_subcommands = true)]
    Photos {
        #[command(subcommand)]
        action: Option<PhotosCmd>,
        /// Device id or name. May be left out when one phone has Photos on.
        #[arg(long)]
        device: Option<String>,
        /// Only photos older than this, in Unix milliseconds: the next page.
        #[arg(long, value_name = "MS")]
        before: Option<u64>,
        /// How many photos, from 1 to 60 (default 30).
        #[arg(long, value_name = "N")]
        limit: Option<u32>,
        #[arg(long)]
        json: bool,
    },
    /// Your other Rime computers: pair one with a code, list, ping, remove,
    /// forget a removal.
    ///
    /// A paired computer may only say hello, pass on the computers it
    /// removed, and say how it is doing (battery, power mode, lock, network,
    /// uptime). It can run nothing here, open nothing and read nothing:
    /// pairing two computers makes them known to each other, it gives neither
    /// authority over the other.
    Computers {
        #[command(subcommand)]
        action: ComputersCmd,
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

/// `rime remote computers`.
#[derive(Subcommand, Debug)]
pub enum ComputersCmd {
    /// Pair another computer.
    ///
    /// On one computer run `add --show-code`; it shows six digits for two
    /// minutes. On the other run `add <address>` with that computer's
    /// address (as its `rime remote status` shows it) and type the digits
    /// when asked. Both must be run by you at the computer: an agent or a
    /// scheduled job can do neither. A wrong code uses the code up.
    Add {
        /// Show a code for the other computer to type, and wait for it.
        #[arg(long, conflicts_with = "address", required_unless_present = "address")]
        show_code: bool,
        /// The address of the computer showing the code. The code itself is
        /// read from the terminal or standard input, never from the command line.
        address: Option<String>,
    },
    /// Every other computer this one has paired, and which were removed,
    /// with how each paired one was doing when it was last reached.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Remove a paired computer. It is told, and removes this one too.
    Remove {
        /// Computer id, or its name when that names exactly one.
        computer: String,
    },
    /// Let a removed computer pair again with a code.
    ///
    /// Run it on both computers, then pair them with `add`. A removal passed
    /// on by another computer later still counts if it is newer than this.
    Forget {
        /// Computer id, or its name when that names exactly one.
        computer: String,
    },
    /// Reach a paired computer and say hello, to see that the link works.
    Ping {
        /// Computer id, or its name when that names exactly one.
        computer: String,
        /// Which path to use: the local network, the relay, or the first that works.
        #[arg(long, value_enum, default_value = "any")]
        via: Via,
        #[arg(long)]
        json: bool,
    },
}

/// The path `rime remote computers ping` takes.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    Any,
    Lan,
    Relay,
}

impl Via {
    fn wire(self) -> Option<&'static str> {
        match self {
            Via::Any => None,
            Via::Lan => Some("lan"),
            Via::Relay => Some("relay"),
        }
    }
}

/// What `rime remote messages` can do besides reading.
#[derive(Subcommand, Debug)]
pub enum MessagesCmd {
    /// Send one SMS from the phone. The text is read from standard input,
    /// never from the command line, so it stays out of your shell history and
    /// the process list.
    ///
    /// Sent only while Rime Remote is unlocked on the phone. Waits until the
    /// phone's mobile network takes it, and says if it did not.
    #[command(group(clap::ArgGroup::new("whom").required(true).args(["to", "thread"])))]
    Send {
        /// Device id or name. May be left out when one phone has Messages on.
        #[arg(long)]
        device: Option<String>,
        /// The phone number to send to.
        #[arg(long, value_name = "NUMBER")]
        to: Option<String>,
        /// The conversation to send into (its id from the list).
        #[arg(long, value_name = "ID")]
        thread: Option<u64>,
        #[arg(long)]
        json: bool,
    },
}

/// What `rime remote calls` can do besides showing them.
#[derive(Subcommand, Debug)]
pub enum CallsCmd {
    /// Answer the call ringing on the phone. It works while the phone is
    /// locked, as it would on the phone's own lock screen.
    Answer {
        /// Device id or name. May be left out when one phone has calls on.
        #[arg(long)]
        device: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Decline the call ringing on the phone.
    Decline {
        /// Device id or name. May be left out when one phone has calls on.
        #[arg(long)]
        device: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// End the phone's call.
    Hangup {
        /// Device id or name. May be left out when one phone has calls on.
        #[arg(long)]
        device: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Call a number from the phone. The phone places the call, and you talk
    /// on the phone. Works only while Rime Remote is unlocked on the phone,
    /// and never for emergency numbers: dial those on the phone itself.
    Dial {
        /// The number to call: digits, with an optional + in front.
        number: String,
        /// Device id or name. May be left out when one phone has calls on.
        #[arg(long)]
        device: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

/// What `rime remote photos` can do besides listing them.
#[derive(Subcommand, Debug)]
pub enum PhotosCmd {
    /// Save one photo, whole, from the phone to this computer.
    ///
    /// It goes into the Rime Remote folder in your Pictures folder, or the
    /// folder given with --to, under the phone's name for it. A file already
    /// there is never replaced: the new one gets a number. A photo is at
    /// most 64 MiB.
    Save {
        /// The photo's id, from the list.
        id: u64,
        /// Device id or name. May be left out when one phone has Photos on.
        #[arg(long)]
        device: Option<String>,
        /// The folder to save into. Made if it is missing.
        #[arg(long, value_name = "DIR")]
        to: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
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
    Messages {
        #[serde(skip_serializing_if = "Option::is_none")]
        device: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        thread: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        before: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    MessagesSend {
        #[serde(skip_serializing_if = "Option::is_none")]
        device: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        to: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        thread: Option<u64>,
        text: String,
    },
    Calls {
        #[serde(skip_serializing_if = "Option::is_none")]
        device: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        before: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    Call {
        #[serde(skip_serializing_if = "Option::is_none")]
        device: Option<String>,
        action: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        number: Option<String>,
    },
    Photos {
        #[serde(skip_serializing_if = "Option::is_none")]
        device: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        before: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    ComputerShow,
    ComputerAdd {
        address: String,
        code: String,
    },
    Computers,
    ComputerRemove {
        computer: String,
    },
    ComputerForget {
        computer: String,
    },
    ComputerPing {
        computer: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        via: Option<&'static str>,
    },
    PhotosSave {
        #[serde(skip_serializing_if = "Option::is_none")]
        device: Option<String>,
        photo: u64,
    },
}

/// One photo on the phone (contract §2.13). `thumb` is the path of its
/// preview, a small JPEG with no metadata, while the phone stays connected.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PhotoItem {
    id: u64,
    name: String,
    date_ms: u64,
    width: u32,
    height: u32,
    size: u64,
    mime: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thumb: Option<String>,
}

/// The call a phone has now (contract §2.12).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CallNow {
    state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    since_ms: u64,
}

/// One call in the phone's history.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CallEntry {
    number: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(rename = "type")]
    kind: String,
    date_ms: u64,
    duration_s: u64,
}

/// One SMS conversation, as the phone described it (contract §2.11).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SmsThread {
    id: u64,
    addresses: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    snippet: String,
    date_ms: u64,
    unread: u32,
    count: u32,
}

/// One SMS in a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Sms {
    id: u64,
    date_ms: u64,
    from_me: bool,
    body: String,
    status: String,
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
        /// Ids of the devices with a companion key. Absent from an older
        /// daemon, which has none.
        #[serde(default)]
        companions: Vec<String>,
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
    Messages {
        device: String,
        #[serde(default)]
        threads: Option<Vec<SmsThread>>,
        #[serde(default)]
        thread: Option<u64>,
        #[serde(default)]
        messages: Option<Vec<Sms>>,
        #[serde(default)]
        changed_ms: Option<u64>,
    },
    MessageSent {
        device: String,
    },
    Calls {
        device: String,
        #[serde(default)]
        current: Option<CallNow>,
        #[serde(default)]
        calls: Option<Vec<CallEntry>>,
        #[serde(default)]
        history_error: Option<String>,
    },
    CallDone {
        device: String,
        action: String,
    },
    Photos {
        device: String,
        photos: Vec<PhotoItem>,
        #[serde(default)]
        partial: bool,
    },
    PhotoSaved {
        device: String,
        #[allow(dead_code)]
        photo: u64,
        name: String,
        len: u64,
    },
    ComputerCode {
        code: String,
        expires_ms: u64,
        machine: String,
    },
    ComputerPaired {
        id: String,
        name: String,
    },
    Computers {
        computers: Vec<rime_remote_core::peer::Listed>,
    },
    ComputerRemoved {
        id: String,
        name: String,
    },
    ComputerForgotten {
        id: String,
        name: String,
    },
    ComputerPong {
        id: String,
        name: String,
        machine: String,
        version: u32,
        build: String,
        path: String,
        rtt_ms: u64,
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
        RemoteCmd::Messages { action: Some(MessagesCmd::Send { device, to, thread, json }), .. } => {
            messages_send(device, to, thread, json)
        }
        RemoteCmd::Messages { action: None, device, thread, before, limit, json } => {
            messages(Request::Messages { device, thread, before, limit }, json)
        }
        RemoteCmd::Calls { action: None, device, before, limit, json } => calls(Request::Calls { device, before, limit }, json),
        RemoteCmd::Calls { action: Some(action), .. } => {
            let (action, device, number, json) = match action {
                CallsCmd::Answer { device, json } => ("answer", device, None, json),
                CallsCmd::Decline { device, json } => ("decline", device, None, json),
                CallsCmd::Hangup { device, json } => ("hangup", device, None, json),
                CallsCmd::Dial { number, device, json } => ("dial", device, Some(number), json),
            };
            call_action(Request::Call { device, action, number }, json)
        }
        RemoteCmd::Photos { action: None, device, before, limit, json } => {
            photos(Request::Photos { device, before, limit }, json)
        }
        RemoteCmd::Photos { action: Some(PhotosCmd::Save { id, device, to, json }), .. } => {
            photos_save(id, device, to, json)
        }
        RemoteCmd::Computers { action } => match action {
            ComputersCmd::Add { address: Some(address), .. } => computers_add(&address),
            ComputersCmd::Add { address: None, .. } => computers_show(),
            ComputersCmd::List { json } => computers_list(json),
            ComputersCmd::Remove { computer } => computers_remove(&computer),
            ComputersCmd::Forget { computer } => computers_forget(&computer),
            ComputersCmd::Ping { computer, via, json } => computers_ping(&computer, via, json),
        },
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

/// How long `rime remote messages send` waits: the service waits up to 35 s
/// for the phone's mobile network, so this is longer.
const SEND_SMS_WAIT: std::time::Duration = std::time::Duration::from_secs(45);

/// One request, with `fd` passed beside its line when there is one (the
/// file `send` offers: the service reads it through this descriptor, never
/// by its path, so it needs no access to where the file lives).
fn call_with(req: &Request, fd: Option<std::os::fd::RawFd>) -> Result<Reply> {
    call_for(req, fd, std::time::Duration::from_secs(20))
}

fn call_for(req: &Request, fd: Option<std::os::fd::RawFd>, wait: std::time::Duration) -> Result<Reply> {
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
        .set_read_timeout(Some(wait))
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
    let Reply::Devices { devices, phones, companions } = call(&Request::Devices)? else {
        return unexpected();
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&with_phones(&devices, &phones, &companions)?)?);
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
    let lasting = companion_names(&devices, &companions);
    if !lasting.is_empty() {
        println!(
            "\n{} keeps working after a restart: its battery, notifications and ring reach this \
             machine before the phone is unlocked. Nothing else does until it is.",
            lasting.join(", ")
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

/// The names of the active devices with a companion key.
fn companion_names<'a>(devices: &'a [rime_remote_core::device::Device], companions: &[String]) -> Vec<&'a str> {
    devices
        .iter()
        .filter(|d| d.is_active() && companions.contains(&d.id))
        .map(|d| d.name.as_str())
        .collect()
}

/// The device list as JSON, each paired phone with its last report under
/// `phone` and `companion` saying whether it keeps working after a restart.
/// Added to each device rather than beside the list, so a reader of the old
/// array keeps working. A companion key is never printed, even from a daemon
/// that sent one.
fn with_phones(
    devices: &[rime_remote_core::device::Device],
    phones: &std::collections::BTreeMap<String, Phone>,
    companions: &[String],
) -> Result<serde_json::Value> {
    let mut out = Vec::with_capacity(devices.len());
    for d in devices {
        let mut v = serde_json::to_value(d)?;
        if let Some(obj) = v.as_object_mut() {
            obj.remove("companion_key");
            obj.remove("retired_companion_keys");
            obj.insert(
                "companion".into(),
                serde_json::Value::Bool(d.is_active() && companions.contains(&d.id)),
            );
            if let Some(p) = phones.get(&d.id).filter(|_| d.is_active()) {
                obj.insert("phone".into(), serde_json::to_value(p)?);
            }
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
/// (contract §2.14), checked here too so a huge paste is refused in words
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
            eprintln!("rime: put the text on the clipboard of {reached} {phones}.");
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

/// The most a message read from standard input may be, in bytes: 2,000
/// characters of four bytes each, and one more so a longer one is refused in
/// words rather than read whole. The service counts characters.
const MAX_SMS_BYTES: u64 = 2000 * 4 + 1;

fn messages(req: Request, json: bool) -> Result<i32> {
    match call(&req)? {
        Reply::Messages { device, threads, thread, messages, changed_ms } => {
            if json {
                let mut v = serde_json::json!({"device": device});
                if let Some(t) = &threads {
                    v["threads"] = serde_json::to_value(t)?;
                }
                if let Some(id) = thread {
                    v["thread"] = id.into();
                    v["messages"] = serde_json::to_value(messages.as_deref().unwrap_or_default())?;
                }
                if let Some(c) = changed_ms {
                    v["changed_ms"] = c.into();
                }
                println!("{}", serde_json::to_string_pretty(&v)?);
                return Ok(0);
            }
            if let Some(threads) = threads {
                print!("{}", threads_table(&device, &threads));
            } else {
                print!("{}", thread_text(&device, thread.unwrap_or_default(), messages.as_deref().unwrap_or_default()));
            }
            Ok(0)
        }
        Reply::Error { message } => messages_error(&message, json),
        _ => unexpected(),
    }
}

fn messages_error(message: &str, json: bool) -> Result<i32> {
    if json {
        println!("{}", serde_json::json!({"error": message}));
    }
    eprintln!("rime: {message}");
    Ok(1)
}

fn threads_table(device: &str, threads: &[SmsThread]) -> String {
    if threads.is_empty() {
        return format!("{device} has no SMS conversations.\n");
    }
    let mut out = format!("{:<8} {:<26} {:<9} {:<7} LAST\n", "ID", "WITH", "WHEN", "UNREAD");
    for t in threads {
        let with = match &t.name {
            Some(n) => n.clone(),
            None => t.addresses.join(", "),
        };
        let unread = if t.unread > 0 { t.unread.to_string() } else { "-".into() };
        out.push_str(&format!(
            "{:<8} {:<26} {:<9} {:<7} {}\n",
            t.id,
            truncate(&with, 26),
            ago(t.date_ms),
            unread,
            truncate(&t.snippet, 60)
        ));
    }
    out.push_str(&format!("(from {device}. `rime remote messages --thread ID` shows one.)\n"));
    out
}

/// One conversation, oldest first so it reads down like a chat.
fn thread_text(device: &str, thread: u64, messages: &[Sms]) -> String {
    if messages.is_empty() {
        return format!("conversation {thread} on {device} has no messages.\n");
    }
    let mut out = String::new();
    for m in messages.iter().rev() {
        let who = if m.from_me { "me" } else { "them" };
        let status = match m.status.as_str() {
            "received" | "sent" | "delivered" => String::new(),
            s => format!(" [{s}]"),
        };
        out.push_str(&format!("{who}, {}{status}:\n", ago(m.date_ms)));
        for line in m.body.lines() {
            out.push_str(&format!("  {line}\n"));
        }
    }
    out
}

fn messages_send(device: Option<String>, to: Option<String>, thread: Option<u64>, json: bool) -> Result<i32> {
    use std::io::IsTerminal;
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        eprintln!("rime: type the message, then press Ctrl-D on a new line.");
    }
    let text = read_sms(stdin.lock())?;
    let reply = call_for(&Request::MessagesSend { device, to, thread, text }, None, SEND_SMS_WAIT)?;
    match reply {
        Reply::MessageSent { device } => {
            if json {
                println!("{}", serde_json::json!({"sent": true, "device": device}));
            }
            eprintln!("rime: {device} sent the message.");
            Ok(0)
        }
        Reply::Error { message } => messages_error(&message, json),
        _ => unexpected(),
    }
}

fn calls(req: Request, json: bool) -> Result<i32> {
    match call(&req)? {
        Reply::Calls { device, current, calls, history_error } => {
            if json {
                let mut v = serde_json::json!({"device": device, "current": current});
                if let Some(c) = &calls {
                    v["calls"] = serde_json::to_value(c)?;
                }
                if let Some(e) = &history_error {
                    v["history_error"] = e.clone().into();
                }
                println!("{}", serde_json::to_string_pretty(&v)?);
                return Ok(0);
            }
            print!("{}", calls_text(&device, current.as_ref(), calls.as_deref()));
            // The call now is still true when the history is not there.
            if let Some(e) = history_error {
                eprintln!("rime: no call history: {e}");
            }
            Ok(0)
        }
        Reply::Error { message } => messages_error(&message, json),
        _ => unexpected(),
    }
}

/// Who a call was with: the contact's name and the number, or what is known.
fn caller(number: Option<&str>, name: Option<&str>) -> String {
    match (name, number.filter(|n| !n.is_empty())) {
        (Some(name), Some(n)) => format!("{name} ({n})"),
        (Some(name), None) => name.to_string(),
        (None, Some(n)) => n.to_string(),
        (None, None) => "a withheld number".to_string(),
    }
}

fn calls_text(device: &str, current: Option<&CallNow>, calls: Option<&[CallEntry]>) -> String {
    let mut out = match current {
        Some(c) => {
            let state = match c.state.as_str() {
                "ringing" => "a call is ringing",
                "held" => "a call is on hold",
                _ => "on a call",
            };
            let who = match (c.number.as_deref(), c.name.as_deref()) {
                (None, None) => String::new(),
                (number, name) => format!(" with {}", caller(number, name)),
            };
            format!("{device}: {state}{who}.\n")
        }
        None => format!("{device}: no call right now.\n"),
    };
    let Some(calls) = calls else { return out };
    if calls.is_empty() {
        out.push_str("No calls in its history.\n");
        return out;
    }
    out.push_str(&format!("{:<9} {:<9} {:<34} LENGTH\n", "WHEN", "TYPE", "WITH"));
    for c in calls {
        let length = if c.duration_s == 0 {
            "-".to_string()
        } else if c.duration_s < 60 {
            format!("{}s", c.duration_s)
        } else {
            format!("{}m {:02}s", c.duration_s / 60, c.duration_s % 60)
        };
        out.push_str(&format!(
            "{:<9} {:<9} {:<34} {}\n",
            ago(c.date_ms),
            c.kind,
            truncate(&caller(Some(&c.number), c.name.as_deref()), 34),
            length
        ));
    }
    out
}

fn call_action(req: Request, json: bool) -> Result<i32> {
    match call(&req)? {
        Reply::CallDone { device, action } => {
            if json {
                println!("{}", serde_json::json!({"done": true, "device": device, "action": action}));
            }
            let what = match action.as_str() {
                "answer" => "answered the call",
                "decline" => "declined the call",
                "hangup" => "ended the call",
                _ => "is calling the number; talk on the phone",
            };
            eprintln!("rime: {device} {what}.");
            Ok(0)
        }
        Reply::Error { message } => messages_error(&message, json),
        _ => unexpected(),
    }
}

/// How long `rime remote photos` waits: the service waits up to 30 s for the
/// phone's answer.
const PHOTOS_WAIT: std::time::Duration = std::time::Duration::from_secs(40);

/// How long `rime remote photos save` waits: the service gives the phone
/// 20 s to start, then 300 s plus one per 128 KiB of a 64 MiB photo, plus
/// 30 s, and stops the save itself when that runs out.
const SAVE_WAIT: std::time::Duration = std::time::Duration::from_secs(20 + 300 + 512 + 30 + 30);

fn photos(req: Request, json: bool) -> Result<i32> {
    match call_for(&req, None, PHOTOS_WAIT)? {
        Reply::Photos { device, photos, partial } => {
            if json {
                let v = serde_json::json!({"device": device, "partial": partial, "photos": photos});
                println!("{}", serde_json::to_string_pretty(&v)?);
                return Ok(0);
            }
            print!("{}", photos_table(&device, &photos, partial));
            Ok(0)
        }
        Reply::Error { message } => messages_error(&message, json),
        _ => unexpected(),
    }
}

fn photos_table(device: &str, photos: &[PhotoItem], partial: bool) -> String {
    let mut out = String::new();
    if photos.is_empty() {
        out.push_str(&format!("{device} has no photos to show.\n"));
    } else {
        out.push_str(&format!("{:<10} {:<9} {:<9} {:<11} NAME\n", "ID", "WHEN", "SIZE", "PIXELS"));
        for p in photos {
            out.push_str(&format!(
                "{:<10} {:<9} {:<9} {:<11} {}\n",
                p.id,
                ago(p.date_ms),
                bytes_short(p.size),
                format!("{}x{}", p.width, p.height),
                truncate(&p.name, 60)
            ));
        }
        out.push_str(&format!("(from {device}. `rime remote photos save ID` saves one here.)\n"));
    }
    if partial {
        out.push_str(
            "Only the photos you picked on the phone are shown. To show all of them, allow Rime Remote \
             all photos in the phone's settings.\n",
        );
    }
    out
}

/// A size in the unit a person would use.
fn bytes_short(n: u64) -> String {
    match n {
        0..=1023 => format!("{n} B"),
        1024..=1_048_575 => format!("{} KB", n / 1024),
        _ => format!("{:.1} MB", n as f64 / 1_048_576.0),
    }
}

/// Where a saved photo goes when no folder is given: "Rime Remote" in the
/// Pictures folder `user-dirs.dirs` names, or in ~/Pictures.
fn default_photos_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from).context("HOME is not set")?;
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    let pictures = std::fs::read_to_string(config.join("user-dirs.dirs"))
        .ok()
        .and_then(|text| pictures_dir_in(&text, &home))
        .unwrap_or_else(|| home.join("Pictures"));
    Ok(pictures.join("Rime Remote"))
}

/// `XDG_PICTURES_DIR` from a `user-dirs.dirs` file: `"$HOME/…"` or an
/// absolute path, as xdg-user-dirs writes it. Anything else is not used.
fn pictures_dir_in(text: &str, home: &std::path::Path) -> Option<PathBuf> {
    let line = text.lines().map(str::trim).find(|l| l.starts_with("XDG_PICTURES_DIR="))?;
    let value = line["XDG_PICTURES_DIR=".len()..].trim().strip_prefix('"')?.strip_suffix('"')?;
    let path = match value.strip_prefix("$HOME") {
        Some("") => return None,
        Some(rest) => home.join(rest.trim_start_matches('/')),
        None if value.starts_with('/') => PathBuf::from(value),
        None => return None,
    };
    (path != home).then_some(path)
}

/// The name the photo is saved under: the service's name for it, checked
/// again here, since a name is the one thing from the phone that becomes a
/// path.
fn saved_name(name: &str, photo: u64) -> String {
    let ok = !name.is_empty()
        && name.chars().count() <= 120
        && !name.starts_with('.')
        && !name.contains(['/', '\\', '\0'])
        && !name.chars().any(char::is_control);
    if ok {
        name.to_string()
    } else {
        format!("photo-{photo}.jpg")
    }
}

/// `name`, then `stem (2).ext`, `stem (3).ext`, …: the candidates for a file
/// that must not replace one already there.
fn numbered(name: &str, n: u32) -> String {
    if n < 2 {
        return name.to_string();
    }
    match name.rfind('.') {
        Some(dot) if dot > 0 => format!("{} ({n}){}", &name[..dot], &name[dot..]),
        _ => format!("{name} ({n})"),
    }
}

/// Give the finished file `from` its name in `dir`, never replacing a file:
/// an atomic no-replace rename, or a hard link where the folder's file
/// system has no such rename. The path it got.
fn place_without_replacing(from: &std::path::Path, dir: &std::path::Path, name: &str) -> Result<PathBuf> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = |p: &std::path::Path| CString::new(p.as_os_str().as_bytes()).context("a path with a NUL byte");
    let src = c(from)?;
    for n in 1..=999 {
        let to = dir.join(numbered(name, n));
        let dst = c(&to)?;
        // SAFETY: two NUL-terminated paths, both relative to nothing.
        let r = unsafe {
            libc::renameat2(libc::AT_FDCWD, src.as_ptr(), libc::AT_FDCWD, dst.as_ptr(), libc::RENAME_NOREPLACE)
        };
        if r == 0 {
            return Ok(to);
        }
        let e = std::io::Error::last_os_error();
        match e.raw_os_error() {
            Some(libc::EEXIST) => continue,
            Some(libc::EINVAL) | Some(libc::ENOSYS) => match std::fs::hard_link(from, &to) {
                Ok(()) => {
                    let _ = std::fs::remove_file(from);
                    return Ok(to);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e).with_context(|| format!("could not name the photo in {}", dir.display())),
            },
            _ => return Err(e).with_context(|| format!("could not name the photo in {}", dir.display())),
        }
    }
    bail!("{} has 999 files named like {name}; nothing was saved", dir.display())
}

fn photos_save(photo: u64, device: Option<String>, to: Option<PathBuf>, json: bool) -> Result<i32> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let dir = match to {
        Some(d) => d,
        None => default_photos_dir()?,
    };
    std::fs::create_dir_all(&dir).with_context(|| format!("could not make {}", dir.display()))?;
    // The file is made here, by this account, in the folder it will stay in,
    // and the service only fills it through the descriptor: it needs no
    // access to the folder, and never chooses a path.
    let mut part = None;
    for attempt in 0..16u32 {
        let candidate = dir.join(format!(".rime-photo-{}-{}-{attempt}.part", std::process::id(), photo));
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&candidate)
        {
            Ok(f) => {
                part = Some((candidate, f));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("could not write in {}", dir.display())),
        }
    }
    let Some((part_path, file)) = part else {
        bail!("could not make a file in {}", dir.display());
    };
    let outcome = (|| -> Result<Result<(PathBuf, String, u64), String>> {
        let reply = call_for(&Request::PhotosSave { device, photo }, Some(file.as_raw_fd()), SAVE_WAIT)?;
        let (device, name, len) = match reply {
            Reply::PhotoSaved { device, name, len, .. } => (device, name, len),
            Reply::Error { message } => return Ok(Err(message)),
            _ => return unexpected().map(|_| Err(String::new())),
        };
        let got = file.metadata()?.len();
        if got != len {
            return Ok(Err(format!("the photo arrived with {got} of {len} bytes; nothing was saved")));
        }
        std::fs::set_permissions(&part_path, std::fs::Permissions::from_mode(0o644))?;
        let path = place_without_replacing(&part_path, &dir, &saved_name(&name, photo))?;
        Ok(Ok((path, device, len)))
    })();
    let outcome = match outcome {
        Ok(Ok(done)) => done,
        Ok(Err(message)) => {
            let _ = std::fs::remove_file(&part_path);
            return messages_error(&message, json);
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part_path);
            return Err(e);
        }
    };
    let (path, device, len) = outcome;
    if json {
        println!(
            "{}",
            serde_json::json!({"saved": true, "device": device, "photo": photo, "path": path, "len": len})
        );
    }
    eprintln!("rime: saved {} ({}) from {device}.", path.display(), bytes_short(len));
    Ok(0)
}

/// A message from standard input, capped. The service checks it again.
fn read_sms(r: impl std::io::Read) -> Result<String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    r.take(MAX_SMS_BYTES).read_to_end(&mut bytes).context("reading the message")?;
    if bytes.len() as u64 == MAX_SMS_BYTES {
        bail!("a message is at most 2000 characters");
    }
    let text = String::from_utf8(bytes).context("the message is not UTF-8")?;
    let text = text.trim_end_matches(['\n', '\r']).to_string();
    if text.trim().is_empty() {
        bail!("there is no message to send: write it to standard input");
    }
    Ok(text)
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

/// How long `computers add` and `computers ping` wait: the service tries
/// each address and then the relay, each with its own deadline.
const COMPUTER_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// A code is six digits; anything longer than this is not a code.
const MAX_CODE_BYTES: u64 = 64;

fn computers_show() -> Result<i32> {
    ensure_running();
    let path = socket_path();
    let stream = UnixStream::connect(&path)
        .with_context(|| format!("Rime Remote is not running (socket: {})", path.display()))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(20))).ok();
    let mut writer = stream.try_clone().context("cloning the control socket")?;
    let mut reader = BufReader::new(stream);
    let mut line = serde_json::to_string(&Request::ComputerShow)?;
    line.push('\n');
    writer.write_all(line.as_bytes())?;
    writer.flush()?;
    let expires_ms = match read_reply(&mut reader)? {
        Reply::ComputerCode { code, expires_ms, machine } => {
            let left = expires_ms.saturating_sub(rime_remote_core::now_ms()) / 1000;
            println!("{}", spaced_code(&code));
            let lan = match call(&Request::Status) {
                Ok(Reply::Status { lan, .. }) if !lan.is_empty() => lan.join(" or "),
                _ => format!("the address of {machine}"),
            };
            eprintln!(
                "\nOn the other computer run `rime remote computers add {lan}` and type this code. \
                 It is good for {left} seconds, once; a wrong code uses it up."
            );
            eprintln!("Waiting. Press Ctrl-C to withdraw the code.");
            expires_ms
        }
        Reply::Error { message } => {
            eprintln!("rime: {message}");
            return Ok(1);
        }
        _ => return unexpected(),
    };
    // Held open until the code is used, expires or is withdrawn; the service
    // ends it, so this only guards against a service that never answers.
    let left = expires_ms.saturating_sub(rime_remote_core::now_ms());
    reader
        .get_ref()
        .set_read_timeout(Some(std::time::Duration::from_millis(left + 30_000)))
        .ok();
    match read_reply(&mut reader) {
        Ok(Reply::ComputerPaired { id, name }) => {
            eprintln!("rime: paired with {name} ({id}). It can say hello to this computer and nothing more.");
            Ok(0)
        }
        Ok(Reply::Error { message }) => {
            eprintln!("rime: {message}");
            Ok(1)
        }
        Ok(_) => unexpected(),
        Err(e) => bail!("the service stopped answering: {e:#}"),
    }
}

fn read_reply(reader: &mut BufReader<UnixStream>) -> Result<Reply> {
    let mut reply = String::new();
    if reader.read_line(&mut reply).context("reading the reply")? == 0 {
        bail!("the service closed the connection");
    }
    serde_json::from_str(reply.trim()).with_context(|| format!("unreadable reply: {reply}"))
}

/// `012345` as `012 345`, easier to read across a room.
fn spaced_code(code: &str) -> String {
    if code.len() == 6 && code.is_ascii() {
        format!("{} {}", &code[..3], &code[3..])
    } else {
        code.to_string()
    }
}

/// The code typed for `computers add`: one short line, never argv.
fn read_code(r: impl std::io::Read) -> Result<String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    r.take(MAX_CODE_BYTES + 1).read_to_end(&mut bytes).context("reading the code")?;
    if bytes.len() as u64 > MAX_CODE_BYTES {
        bail!("that is not a pairing code: a code is six digits");
    }
    let text = String::from_utf8(bytes).context("the code is not UTF-8")?;
    let code = text.lines().next().unwrap_or("").trim().to_string();
    if code.is_empty() {
        bail!("there is no code: type the six digits the other computer shows");
    }
    Ok(code)
}

fn computers_add(address: &str) -> Result<i32> {
    use std::io::IsTerminal;
    let stdin = std::io::stdin();
    let code = if stdin.is_terminal() {
        eprint!("Code shown on {address}: ");
        std::io::stderr().flush().ok();
        let mut line = String::new();
        stdin.lock().read_line(&mut line).context("reading the code")?;
        read_code(line.as_bytes())?
    } else {
        read_code(stdin.lock())?
    };
    ensure_running();
    match call_for(&Request::ComputerAdd { address: address.to_string(), code }, None, COMPUTER_WAIT)? {
        Reply::ComputerPaired { id, name } => {
            eprintln!("rime: paired with {name} ({id}). It can say hello to this computer and nothing more.");
            Ok(0)
        }
        Reply::Error { message } => {
            eprintln!("rime: {message}");
            Ok(1)
        }
        _ => unexpected(),
    }
}

fn computers_list(json: bool) -> Result<i32> {
    let Reply::Computers { computers } = call(&Request::Computers)? else {
        return unexpected();
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&computers)?);
        return Ok(0);
    }
    print!("{}", computers_table(&computers));
    Ok(0)
}

fn computers_table(computers: &[rime_remote_core::peer::Listed]) -> String {
    if computers.is_empty() {
        return "no other computers paired. `rime remote computers add --show-code` shows a code.\n".into();
    }
    let mut out = format!("{:<18} {:<20} {:<9} {:<8} {:<10} ADDRESSES\n", "ID", "NAME", "STATE", "PATH", "LAST SEEN");
    for l in computers {
        let c = &l.computer;
        out.push_str(&format!(
            "{:<18} {:<20} {:<9} {:<8} {:<10} {}\n",
            c.id,
            truncate(&c.name, 20),
            c.state(),
            c.last_path.as_deref().unwrap_or("-"),
            c.last_seen_ms.map(ago).unwrap_or_else(|| "never".into()),
            if c.addresses.is_empty() { "-".to_string() } else { c.addresses.join(" ") },
        ));
        if let (Some(p), Some(at)) = (&l.presence, l.presence_ms) {
            out.push_str(&format!("{:<18} {} ({})\n", "", presence_words(p), ago(at)));
        }
    }
    out
}

/// One line for how a computer said it was doing.
fn presence_words(p: &rime_remote_core::peer::Presence) -> String {
    let mut parts = Vec::new();
    match (&p.battery, p.on_ac) {
        (Some(b), _) if b.charging => parts.push(format!("battery {}% charging", b.percent)),
        (Some(b), Some(true)) => parts.push(format!("battery {}% on AC", b.percent)),
        (Some(b), _) => parts.push(format!("battery {}%", b.percent)),
        (None, Some(true)) => parts.push("on AC".into()),
        (None, _) => {}
    }
    if let Some(m) = &p.power_mode {
        parts.push(m.clone());
    }
    match p.locked {
        Some(true) => parts.push("locked".into()),
        Some(false) => parts.push("unlocked".into()),
        None => {}
    }
    match p.network.as_deref() {
        Some("wired") => parts.push("wired".into()),
        Some("wifi") => parts.push("Wi-Fi".into()),
        Some("none") => parts.push("no network".into()),
        _ => {}
    }
    if let Some(u) = p.uptime_s {
        parts.push(match u {
            0..=3599 => format!("up {}m", u / 60),
            3600..=86399 => format!("up {}h", u / 3600),
            _ => format!("up {}d", u / 86400),
        });
    }
    if parts.is_empty() {
        "said nothing about itself".into()
    } else {
        parts.join(", ")
    }
}

fn computers_remove(computer: &str) -> Result<i32> {
    match call(&Request::ComputerRemove { computer: computer.to_string() })? {
        Reply::ComputerRemoved { id, name } => {
            eprintln!(
                "rime: {name} ({id}) removed. Any connection it held was dropped and its key is \
                 refused from now on. It is being told, and removes this computer too. To pair \
                 them again, run `rime remote computers forget` on both, then `add`."
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

fn computers_forget(computer: &str) -> Result<i32> {
    match call(&Request::ComputerForget { computer: computer.to_string() })? {
        Reply::ComputerForgotten { id, name } => {
            eprintln!(
                "rime: {name} ({id}) may pair with this computer again. Run `rime remote computers \
                 forget` on it too, then pair them with `add`."
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

fn computers_ping(computer: &str, via: Via, json: bool) -> Result<i32> {
    let req = Request::ComputerPing { computer: computer.to_string(), via: via.wire() };
    match call_for(&req, None, COMPUTER_WAIT)? {
        Reply::ComputerPong { id, name, machine, version, build, path, rtt_ms } => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({"id": id, "name": name, "machine": machine, "version": version,
                                       "build": build, "path": path, "rtt_ms": rtt_ms})
                );
            } else {
                println!("{name} ({machine}) answered over {path} in {rtt_ms} ms: protocol {version}, build {build}");
            }
            Ok(0)
        }
        Reply::Error { message } => {
            if json {
                println!("{}", serde_json::json!({"error": message}));
            }
            eprintln!("rime: {message}");
            Ok(1)
        }
        _ => unexpected(),
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
            r#"{"reply":"devices","devices":[],"companions":["aaaa"]}"#,
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
        let v = with_phones(&devices, &phones, &[]).expect("json");
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

    #[test]
    fn a_device_with_a_companion_key_says_so_and_the_key_is_never_printed() {
        let devices: Vec<rime_remote_core::device::Device> = serde_json::from_str(
            r#"[{"id":"aaaa","name":"Pixel","public_key":"k","paired_ms":1,"companion_key":"SECRETISH"},
                {"id":"bbbb","name":"Old","public_key":"k2","paired_ms":2,"revoked_ms":3},
                {"id":"cccc","name":"Tab","public_key":"k3","paired_ms":4}]"#,
        )
        .expect("devices");
        let companions = vec!["aaaa".to_string(), "bbbb".to_string()];
        let v = with_phones(&devices, &Default::default(), &companions).expect("json");
        assert_eq!(v[0]["companion"], true);
        assert_eq!(v[1]["companion"], false, "a revoked device keeps nothing working");
        assert_eq!(v[2]["companion"], false);
        let text = v.to_string();
        assert!(!text.contains("SECRETISH") && !text.contains("companion_key"), "{text}");
        assert_eq!(companion_names(&devices, &companions), ["Pixel"]);
    }

    #[test]
    fn computers_parses_and_the_code_never_comes_from_argv() {
        use clap::Parser;
        #[derive(clap::Parser)]
        struct Cli {
            #[command(subcommand)]
            cmd: RemoteCmd,
        }
        let parse = |args: &[&str]| Cli::try_parse_from(std::iter::once("r").chain(args.iter().copied()));
        match parse(&["computers", "add", "--show-code"]).expect("show").cmd {
            RemoteCmd::Computers { action: ComputersCmd::Add { show_code: true, address: None } } => {}
            other => panic!("{other:?}"),
        }
        match parse(&["computers", "add", "10.0.0.2"]).expect("add").cmd {
            RemoteCmd::Computers { action: ComputersCmd::Add { show_code: false, address: Some(a) } } => {
                assert_eq!(a, "10.0.0.2")
            }
            other => panic!("{other:?}"),
        }
        assert!(parse(&["computers", "add"]).is_err(), "neither a code to show nor an address");
        assert!(parse(&["computers", "add", "--show-code", "10.0.0.2"]).is_err(), "both");
        // There is no way to put the code on the command line.
        assert!(parse(&["computers", "add", "10.0.0.2", "123456"]).is_err());
        assert!(parse(&["computers", "add", "10.0.0.2", "--code", "123456"]).is_err());
        match parse(&["computers", "ping", "desk", "--via", "relay", "--json"]).expect("ping").cmd {
            RemoteCmd::Computers { action: ComputersCmd::Ping { computer, via: Via::Relay, json: true } } => {
                assert_eq!(computer, "desk")
            }
            other => panic!("{other:?}"),
        }
        assert!(parse(&["computers", "ping", "desk", "--via", "bluetooth"]).is_err());
        assert!(matches!(
            parse(&["computers", "list", "--json"]).expect("list").cmd,
            RemoteCmd::Computers { action: ComputersCmd::List { json: true } }
        ));
        assert!(matches!(
            parse(&["computers", "remove", "desk"]).expect("remove").cmd,
            RemoteCmd::Computers { action: ComputersCmd::Remove { .. } }
        ));
        match parse(&["computers", "forget", "desk"]).expect("forget").cmd {
            RemoteCmd::Computers { action: ComputersCmd::Forget { computer } } => assert_eq!(computer, "desk"),
            other => panic!("{other:?}"),
        }
        assert!(parse(&["computers", "forget"]).is_err(), "forget names a computer");

        // The wire, as rime-remoted's own tests spell it.
        let wire = |r: &Request| serde_json::to_string(r).unwrap();
        assert_eq!(wire(&Request::ComputerShow), r#"{"cmd":"computer_show"}"#);
        assert_eq!(
            wire(&Request::ComputerAdd { address: "a".into(), code: "1".into() }),
            r#"{"cmd":"computer_add","address":"a","code":"1"}"#
        );
        assert_eq!(wire(&Request::Computers), r#"{"cmd":"computers"}"#);
        assert_eq!(wire(&Request::ComputerRemove { computer: "d".into() }), r#"{"cmd":"computer_remove","computer":"d"}"#);
        assert_eq!(wire(&Request::ComputerForget { computer: "d".into() }), r#"{"cmd":"computer_forget","computer":"d"}"#);
        assert_eq!(wire(&Request::ComputerPing { computer: "d".into(), via: Via::Any.wire() }), r#"{"cmd":"computer_ping","computer":"d"}"#);
        assert_eq!(
            wire(&Request::ComputerPing { computer: "d".into(), via: Via::Lan.wire() }),
            r#"{"cmd":"computer_ping","computer":"d","via":"lan"}"#
        );
        for text in [
            r#"{"reply":"computer_code","code":"012345","expires_ms":1,"machine":"m"}"#,
            r#"{"reply":"computer_paired","id":"i","name":"n"}"#,
            r#"{"reply":"computers","computers":[]}"#,
            r#"{"reply":"computer_removed","id":"i","name":"n"}"#,
            r#"{"reply":"computer_forgotten","id":"i","name":"n"}"#,
            r#"{"reply":"computer_pong","id":"i","name":"n","machine":"m","version":1,"build":"b","path":"lan","rtt_ms":3}"#,
        ] {
            serde_json::from_str::<Reply>(text).unwrap_or_else(|e| panic!("{text}: {e}"));
        }
    }

    #[test]
    fn a_typed_code_is_one_short_line() {
        assert_eq!(read_code(" 123 456 \n".as_bytes()).unwrap(), "123 456");
        assert_eq!(read_code("123456\nextra\n".as_bytes()).unwrap(), "123456");
        assert!(read_code("".as_bytes()).is_err());
        assert!(read_code("  \n".as_bytes()).is_err());
        assert!(read_code("1".repeat(65).as_bytes()).is_err());
        assert_eq!(spaced_code("012345"), "012 345");
    }

    #[test]
    fn the_computers_table_says_how_to_start_when_it_is_empty() {
        assert!(computers_table(&[]).contains("add --show-code"));
    }

    #[test]
    fn the_computers_table_shows_the_last_presence_under_a_paired_computer() {
        let text = r#"{"reply":"computers","computers":[
            {"id":"aaaa","name":"desk","public_key":"k","paired_ms":1,"addresses":["10.0.0.2:7717"],
             "presence":{"name":"desk","battery":{"percent":81,"charging":false},"on_ac":false,
                         "power_mode":"balanced","locked":true,"network":"wifi","uptime_s":7200},
             "presence_ms":1},
            {"id":"bbbb","name":"old","public_key":"k2","paired_ms":1,"revoked_ms":5}]}"#;
        let Reply::Computers { computers } = serde_json::from_str::<Reply>(text).expect("reply") else {
            panic!("not computers")
        };
        let table = computers_table(&computers);
        assert!(table.contains("battery 81%, balanced, locked, Wi-Fi, up 2h"), "{table}");
        assert_eq!(table.lines().count(), 4, "{table}");
    }

    #[test]
    fn messages_parses_and_the_send_text_never_comes_from_argv() {
        use clap::Parser;
        #[derive(clap::Parser)]
        struct Cli {
            #[command(subcommand)]
            cmd: RemoteCmd,
        }
        match Cli::try_parse_from(["r", "messages", "--thread", "12", "--json", "--limit", "5"]).expect("read").cmd {
            RemoteCmd::Messages { action: None, thread: Some(12), json: true, limit: Some(5), device: None, before: None } => {}
            other => panic!("{other:?}"),
        }
        match Cli::try_parse_from(["r", "messages", "send", "--to", "5556", "--device", "Pixel"]).expect("send").cmd {
            RemoteCmd::Messages { action: Some(MessagesCmd::Send { to: Some(t), device: Some(d), thread: None, json: false }), .. } => {
                assert_eq!((t.as_str(), d.as_str()), ("5556", "Pixel"));
            }
            other => panic!("{other:?}"),
        }
        assert!(Cli::try_parse_from(["r", "messages", "send"]).is_err(), "a send with nobody to send to");
        assert!(Cli::try_parse_from(["r", "messages", "send", "--to", "5556", "--thread", "3"]).is_err());
        // There is no way to put the text on the command line.
        assert!(Cli::try_parse_from(["r", "messages", "send", "--to", "5556", "hello"]).is_err());
        assert!(Cli::try_parse_from(["r", "messages", "send", "--to", "5556", "--text", "hello"]).is_err());
        assert!(Cli::try_parse_from(["r", "messages", "--thread", "3", "send", "--to", "5556"]).is_err());

        assert_eq!(
            serde_json::to_string(&Request::Messages { device: None, thread: Some(3), before: None, limit: None }).unwrap(),
            r#"{"cmd":"messages","thread":3}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::MessagesSend { device: None, to: Some("5556".into()), thread: None, text: "a\nb".into() })
                .unwrap(),
            r#"{"cmd":"messages_send","to":"5556","text":"a\nb"}"#
        );
    }

    #[test]
    fn photos_parses_and_its_wire_shapes_hold() {
        use clap::Parser;
        #[derive(clap::Parser)]
        struct Cli {
            #[command(subcommand)]
            cmd: RemoteCmd,
        }
        match Cli::try_parse_from(["r", "photos", "--json", "--limit", "5", "--before", "9"]).expect("list").cmd {
            RemoteCmd::Photos { action: None, json: true, limit: Some(5), before: Some(9), device: None } => {}
            other => panic!("{other:?}"),
        }
        match Cli::try_parse_from(["r", "photos", "save", "42", "--to", "out", "--device", "Pixel"]).expect("save").cmd {
            RemoteCmd::Photos { action: Some(PhotosCmd::Save { id: 42, to: Some(t), device: Some(d), json: false }), .. } => {
                assert_eq!((t.to_str(), d.as_str()), (Some("out"), "Pixel"));
            }
            other => panic!("{other:?}"),
        }
        assert!(Cli::try_parse_from(["r", "photos", "save"]).is_err(), "a save of nothing");
        assert!(Cli::try_parse_from(["r", "photos", "save", "x"]).is_err());
        assert!(Cli::try_parse_from(["r", "photos", "--limit", "3", "save", "1"]).is_err());
        assert_eq!(
            serde_json::to_string(&Request::Photos { device: None, before: Some(9), limit: None }).unwrap(),
            r#"{"cmd":"photos","before":9}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::PhotosSave { device: None, photo: 4 }).unwrap(),
            r#"{"cmd":"photos_save","photo":4}"#
        );
        let reply: Reply = serde_json::from_str(
            r#"{"reply":"photos","device":"Pixel","partial":true,"photos":[{"id":5,"name":"IMG_1.jpg","date_ms":1,
               "width":4000,"height":3000,"size":2500000,"mime":"image/jpeg","thumb":"/run/user/1/rime-remote/photos/d/5.jpg"}]}"#,
        )
        .expect("photos");
        let Reply::Photos { device, photos, partial } = reply else { panic!() };
        let table = photos_table(&device, &photos, partial);
        assert!(table.contains("IMG_1.jpg") && table.contains("4000x3000") && table.contains("2.4 MB"), "{table}");
        assert!(table.contains("Only the photos you picked"), "{table}");
        assert!(!photos_table("Pixel", &photos, false).contains("picked"));
        serde_json::from_str::<Reply>(r#"{"reply":"photo_saved","device":"Pixel","photo":4,"name":"a.jpg","len":3}"#)
            .expect("saved");
    }

    #[test]
    fn a_saved_photos_name_is_checked_again_and_never_replaces_a_file() {
        assert_eq!(saved_name("PXL_1.jpg", 3), "PXL_1.jpg");
        for bad in ["", ".bashrc", "../x.jpg", "a/b.jpg", "a\\b", "a\nb", "a\0b"] {
            assert_eq!(saved_name(bad, 3), "photo-3.jpg", "{bad:?}");
        }
        assert_eq!(numbered("a.jpg", 1), "a.jpg");
        assert_eq!(numbered("a.jpg", 2), "a (2).jpg");
        assert_eq!(numbered("noext", 3), "noext (3)");
        let dir = std::env::temp_dir().join(format!("rime-photo-place-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.jpg"), b"theirs").unwrap();
        std::fs::write(dir.join("a (2).jpg"), b"theirs too").unwrap();
        let part = dir.join(".part");
        std::fs::write(&part, b"mine").unwrap();
        let got = place_without_replacing(&part, &dir, "a.jpg").unwrap();
        assert_eq!(got, dir.join("a (3).jpg"));
        assert_eq!(std::fs::read(&got).unwrap(), b"mine");
        assert_eq!(std::fs::read(dir.join("a.jpg")).unwrap(), b"theirs");
        assert!(!part.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_pictures_folder_comes_from_user_dirs_or_home() {
        let home = std::path::Path::new("/home/x");
        let text = "# comment\nXDG_DESKTOP_DIR=\"$HOME/Desktop\"\nXDG_PICTURES_DIR=\"$HOME/Bilder\"\n";
        assert_eq!(pictures_dir_in(text, home), Some(PathBuf::from("/home/x/Bilder")));
        assert_eq!(pictures_dir_in("XDG_PICTURES_DIR=\"/data/pics\"", home), Some(PathBuf::from("/data/pics")));
        assert_eq!(pictures_dir_in("XDG_PICTURES_DIR=\"$HOME/\"", home), None, "home itself");
        assert_eq!(pictures_dir_in("XDG_PICTURES_DIR=\"$HOME\"", home), None);
        assert_eq!(pictures_dir_in("XDG_PICTURES_DIR=relative", home), None);
        assert_eq!(pictures_dir_in("", home), None);
    }

    #[test]
    fn calls_parses_and_its_wire_shapes_hold() {
        use clap::Parser;
        #[derive(clap::Parser)]
        struct Cli {
            #[command(subcommand)]
            cmd: RemoteCmd,
        }
        match Cli::try_parse_from(["r", "calls", "--json", "--limit", "5", "--device", "Pixel"]).expect("list").cmd {
            RemoteCmd::Calls { action: None, json: true, limit: Some(5), device: Some(d), before: None } => assert_eq!(d, "Pixel"),
            other => panic!("{other:?}"),
        }
        for (verb, want) in [("answer", "answer"), ("decline", "decline"), ("hangup", "hangup")] {
            match Cli::try_parse_from(["r", "calls", verb]).expect(verb).cmd {
                RemoteCmd::Calls { action: Some(a), .. } => assert!(format!("{a:?}").to_lowercase().starts_with(want), "{a:?}"),
                other => panic!("{other:?}"),
            }
        }
        match Cli::try_parse_from(["r", "calls", "dial", "+1 555 521 5556", "--json"]).expect("dial").cmd {
            RemoteCmd::Calls { action: Some(CallsCmd::Dial { number, json: true, device: None }), .. } => {
                assert_eq!(number, "+1 555 521 5556");
            }
            other => panic!("{other:?}"),
        }
        assert!(Cli::try_parse_from(["r", "calls", "dial"]).is_err(), "a dial with no number");
        assert!(Cli::try_parse_from(["r", "calls", "answer", "5556"]).is_err(), "answer takes no number");
        assert!(Cli::try_parse_from(["r", "calls", "--limit", "3", "answer"]).is_err());
        assert_eq!(
            serde_json::to_string(&Request::Calls { device: None, before: Some(9), limit: None }).unwrap(),
            r#"{"cmd":"calls","before":9}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::Call { device: Some("Pixel".into()), action: "dial", number: Some("5556".into()) })
                .unwrap(),
            r#"{"cmd":"call","device":"Pixel","action":"dial","number":"5556"}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::Call { device: None, action: "answer", number: None }).unwrap(),
            r#"{"cmd":"call","action":"answer"}"#
        );
    }

    #[test]
    fn a_calls_reply_reads_as_the_call_now_and_a_table() {
        let reply: Reply = serde_json::from_str(
            r#"{"reply":"calls","device":"Pixel","current":{"state":"ringing","number":"5556","since_ms":1},
               "calls":[{"number":"5556","name":"Test Name","type":"missed","date_ms":1,"duration_s":0},
                        {"number":"","type":"incoming","date_ms":1,"duration_s":75}]}"#,
        )
        .expect("reply");
        let Reply::Calls { device, current, calls, history_error: None } = reply else { panic!() };
        let text = calls_text(&device, current.as_ref(), calls.as_deref());
        assert!(text.starts_with("Pixel: a call is ringing with 5556.\n"), "{text}");
        assert!(text.contains("Test Name (5556)") && text.contains("missed") && text.contains("a withheld number"), "{text}");
        assert!(text.contains("1m 15s"), "{text}");
        let idle = calls_text("Pixel", None, None);
        assert_eq!(idle, "Pixel: no call right now.\n");
        let reply: Reply = serde_json::from_str(r#"{"reply":"call_done","device":"Pixel","action":"dial"}"#).expect("done");
        assert!(matches!(reply, Reply::CallDone { .. }));
        let reply: Reply =
            serde_json::from_str(r#"{"reply":"calls","device":"Pixel","history_error":"not allowed"}"#).expect("no history");
        assert!(matches!(reply, Reply::Calls { current: None, calls: None, history_error: Some(_), .. }));
    }

    #[test]
    fn a_message_from_standard_input_is_capped_and_trimmed() {
        assert_eq!(read_sms("hello\nthere\n".as_bytes()).unwrap(), "hello\nthere");
        assert!(read_sms("  \n".as_bytes()).is_err());
        assert!(read_sms("x".repeat(MAX_SMS_BYTES as usize).as_bytes()).is_err());
        assert_eq!(read_sms("é".repeat(2000).as_bytes()).unwrap().chars().count(), 2000);
        assert!(read_sms(&[0xff, 0xfe][..]).is_err());
    }

    #[test]
    fn a_messages_reply_reads_as_a_table_and_a_conversation() {
        let reply: Reply = serde_json::from_str(
            r#"{"reply":"messages","device":"Pixel","threads":[{"id":7,"addresses":["+15555215556"],"snippet":"hi there",
               "date_ms":1,"unread":2,"count":5},{"id":8,"addresses":["5556"],"name":"Test Name","snippet":"x","date_ms":1,
               "unread":0,"count":1}]}"#,
        )
        .expect("reply");
        let Reply::Messages { device, threads: Some(threads), .. } = reply else { panic!() };
        let table = threads_table(&device, &threads);
        assert!(table.contains("+15555215556") && table.contains("hi there") && table.contains("Test Name"), "{table}");
        let msgs = vec![
            Sms { id: 2, date_ms: 2, from_me: true, body: "second".into(), status: "failed".into() },
            Sms { id: 1, date_ms: 1, from_me: false, body: "first\nline".into(), status: "received".into() },
        ];
        let text = thread_text("Pixel", 7, &msgs);
        assert!(text.find("first").unwrap() < text.find("second").unwrap(), "oldest first: {text}");
        assert!(text.contains("[failed]") && text.contains("  line"), "{text}");
        let reply: Reply = serde_json::from_str(r#"{"reply":"message_sent","device":"Pixel"}"#).expect("sent");
        assert!(matches!(reply, Reply::MessageSent { .. }));
    }
}
