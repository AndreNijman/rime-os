//! Files sent between the owner's paired computers (contract §2.15).
//!
//! `rime remote computers send <computer> <file>` hands this process an open
//! descriptor ([`send`]). It dials the computer, opens one [`FILE_VERB`]
//! channel with the name, the length and the SHA-256 of the bytes, and sends
//! them in `Data` frames. The receiving computer ([`open_incoming`]) checks
//! the offer, hands the bytes to `rime-agentd`'s `save_file` (which saves
//! them in Downloads under a name that replaces nothing), and posts a
//! notification whose one button shows the file in its folder.
//!
//! ## What the receiver accepts
//!
//! Only from a computer that is paired, not removed, trusted (paired with a
//! code, or introduced and then allowed by a person here) and whose files
//! switch is on ([`Device::takes_files`]); the switch is on by default for a
//! trusted computer. Every offer is checked before a byte is taken:
//!
//! * the name: refused, not cleaned, when it holds `/` or `\`, a control
//!   character, a bidirectional or invisible format character, or is empty,
//!   `.`, `..` or longer than [`MAX_NAME`] bytes ([`offered_name`]); then
//!   reduced as every saved name is (no leading dots: never hidden);
//! * the length: 1 to [`MAX_FILE_BYTES`];
//! * the hash: 64 lowercase hex digits;
//! * the quota: [`QUOTA_FILES`] offers and [`QUOTA_BYTES`] accepted per
//!   computer per hour, kept in memory (a restart starts them again);
//! * the slots: one transfer per computer and [`MAX_INCOMING`] at once.
//!
//! The bytes go to `rime-agentd` exactly as many as were offered and no
//! more, and the last one is held back until the hash matches: a file whose
//! hash does not match never reaches its name (`rime-agentd` removes its
//! temporary file when the stream ends short). The only partial file on disk
//! is `rime-agentd`'s, at most [`MAX_INCOMING`] × [`MAX_FILE_BYTES`] at once,
//! removed when a transfer ends early; one a killed `rime-agentd` left is
//! swept before its next save.
//!
//! This process builds both lines it sends `rime-agentd` (`save_file`) and
//! the peer (`peer_file`, its progress and its outcome) from fields it
//! checked, and never forwards a line from either side to the other: the
//! peer is not told where the file was saved, only its name.
//!
//! ## Flow
//!
//! The receiver writes a progress line every [`WINDOW`] bytes; the sender
//! keeps at most [`IN_FLIGHT`] bytes unacknowledged and reads (answering the
//! receiver's pings) whenever it reaches that, so a long transfer keeps its
//! session alive and a slow disk slows the sender down.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::FileExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use rime_remote_core::device::Device;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::state::State;

/// The channel a peer opens to send a file.
pub const FILE_VERB: &str = "peer_file";
/// The largest file sent or taken: `rime-agentd`'s `save_file` limit.
pub const MAX_FILE_BYTES: u64 = crate::phone::MAX_SEND_BYTES;
/// The longest name taken, in bytes: what a saved name is cut to.
pub const MAX_NAME: usize = rime_agent_core::filename::MAX_NAME;
/// Offers taken from one computer per hour, refused ones included.
pub const QUOTA_FILES: usize = 100;
/// Bytes taken from one computer per hour.
pub const QUOTA_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Transfers arriving at once, from all computers together.
pub const MAX_INCOMING: usize = 2;
/// A progress line every this many bytes.
pub const WINDOW: u64 = 1024 * 1024;
/// The most a sender has on the way unacknowledged.
pub const IN_FLIGHT: u64 = 4 * WINDOW;
const HOUR: Duration = Duration::from_secs(3600);
/// Nothing arriving for this long ends a transfer.
const STALL: Duration = Duration::from_secs(30);
/// How long `rime-agentd` may take to put a complete file in place.
const SAVED_WAIT: Duration = Duration::from_secs(120);
/// Saved files a notification's button can still show.
const SAVED_KEPT: usize = 32;
/// The notification key prefix and its one button.
pub const KEY_PREFIX: &str = "file:";
pub const SHOW_ACTION: &str = "show";

/// The grace before a transfer must keep pace, and the pace: bytes a second.
const GRACE: Duration = Duration::from_secs(60);
const PACE: u64 = 256 * 1024;

/// The whole budget for receiving `len` bytes: a minute, and a second for
/// every 256 KiB.
pub fn budget(len: u64) -> Duration {
    GRACE + Duration::from_secs(len / PACE)
}

/// How long after the start the first `got` bytes are due: the budget kept
/// as a schedule all the way, so a computer that trickles a byte now and then
/// loses its slot once it falls behind instead of holding it to the end.
fn due(grace: Duration, got: u64) -> Duration {
    grace + Duration::from_nanos(u64::try_from(u128::from(got) * 1_000_000_000 / u128::from(PACE)).unwrap_or(u64::MAX))
}

/// What arriving files are remembered by: who is sending, what each
/// computer was charged, and which saved file each notification shows.
#[derive(Default)]
pub struct Files {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    busy: HashSet<String>,
    charged: HashMap<String, VecDeque<(Instant, u64)>>,
    saved: VecDeque<(u64, PathBuf)>,
    next: u64,
}

impl Files {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Transfers arriving now. For the tests.
    #[cfg(test)]
    pub fn arriving(&self) -> usize {
        self.lock().busy.len()
    }

    fn remember_saved(&self, path: PathBuf) -> u64 {
        let mut g = self.lock();
        g.next += 1;
        let n = g.next;
        g.saved.push_back((n, path));
        while g.saved.len() > SAVED_KEPT {
            g.saved.pop_front();
        }
        n
    }

    fn saved(&self, n: u64) -> Option<PathBuf> {
        self.lock().saved.iter().find(|(k, _)| *k == n).map(|(_, p)| p.clone())
    }
}

/// A transfer's place among those arriving, given back however it ends.
struct Slot {
    files: Arc<Files>,
    id: String,
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.files.lock().busy.remove(&self.id);
    }
}

/// Every character of Unicode 17's general categories Cf (format: invisible,
/// direction-changing, tags), Zl and Zp (line and paragraph separators, which
/// `char::is_control` does not count): what makes a name look like another.
pub(crate) fn is_format(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{0600}'..='\u{0605}' | '\u{061C}' | '\u{06DD}' | '\u{070F}' | '\u{0890}'..='\u{0891}'
        | '\u{08E2}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{2028}'..='\u{202E}' | '\u{2060}'..='\u{206F}'
        | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}' | '\u{110BD}' | '\u{110CD}' | '\u{13430}'..='\u{1343F}'
        | '\u{1BCA0}'..='\u{1BCA3}' | '\u{1D173}'..='\u{1D17A}' | '\u{E0001}' | '\u{E0020}'..='\u{E007F}')
}

/// A name another computer offered, refused when it is not one file's name,
/// then reduced as every saved name is.
pub fn offered_name(raw: &str) -> Result<String, String> {
    if raw.is_empty() || raw == "." || raw == ".." {
        return Err("a file needs a name".into());
    }
    if raw.contains(['/', '\\']) {
        return Err("a file's name may not hold a path separator".into());
    }
    if raw.chars().any(char::is_control) {
        return Err("a file's name may not hold a control character".into());
    }
    if raw.chars().any(is_format) {
        return Err("a file's name may not hold an invisible or direction-changing character".into());
    }
    if raw.len() > MAX_NAME {
        return Err(format!("a file's name may be at most {MAX_NAME} bytes"));
    }
    rime_agent_core::filename::file_name(raw)
}

/// One offer, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Offer {
    name: String,
    len: u64,
    sha256: String,
}

fn parse_offer(request: &[u8]) -> Result<Offer, (&'static str, String)> {
    let v: Value = serde_json::from_slice(request).map_err(|_| ("bad_request", "not a JSON object".to_string()))?;
    if v["cmd"] != FILE_VERB {
        return Err(("bad_request", format!("not a {FILE_VERB}")));
    }
    let name = offered_name(v["name"].as_str().ok_or(("bad_request", "name must be text".to_string()))?)
        .map_err(|e| ("bad_request", e))?;
    let len = v["len"].as_u64().ok_or(("bad_request", "len must be a whole number".to_string()))?;
    if len == 0 || len > MAX_FILE_BYTES {
        return Err(("too_large", format!("len must be from 1 to {MAX_FILE_BYTES} bytes")));
    }
    let sha256 = v["sha256"].as_str().unwrap_or("");
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return Err(("bad_request", "sha256 must be 64 lowercase hex digits".into()));
    }
    Ok(Offer { name, len, sha256: sha256.to_string() })
}

fn refusal(kind: &str, message: &str) -> Vec<u8> {
    json!({"reply": "error", "kind": kind, "message": message}).to_string().into_bytes()
}

fn line(v: Value) -> Vec<u8> {
    let mut b = v.to_string().into_bytes();
    b.push(b'\n');
    b
}

/// A file offered by paired computer `sender` on a channel it opened: the
/// stream the channel's bytes go into and the reply, or the refusal.
pub fn open_incoming(state: &State, agentd: &Path, sender: &str, request: &[u8]) -> Result<(UnixStream, Vec<u8>), Vec<u8>> {
    let agentd = agentd.to_path_buf();
    let who = sender.to_string();
    open_incoming_with(state, sender, request, Instant::now(), move |name, len| save_file(&agentd, &who, name, len))
}

/// [`open_incoming`] with `rime-agentd`'s `save_file` as `start`: given the
/// checked name and length, a stream that takes exactly that many bytes and
/// then answers with one line.
fn open_incoming_with<S: Read + Write + Send + 'static>(
    state: &State,
    sender: &str,
    request: &[u8],
    now: Instant,
    start: impl FnOnce(&str, u64) -> Result<S, String>,
) -> Result<(UnixStream, Vec<u8>), Vec<u8>> {
    let computer = match state.devices() {
        Ok(store) => store.computers.get(sender).filter(|c| c.takes_files()).cloned(),
        Err(e) => {
            eprintln!("rime-remoted: a file offer could not be checked: {e}");
            return Err(refusal("unavailable", "this computer could not read its paired computers"));
        }
    };
    let Some(computer) = computer else {
        return Err(refusal("not_shared", "this computer does not take files from that one"));
    };
    let files = Arc::clone(&state.files);
    let slot = {
        let mut g = files.lock();
        let charged = g.charged.entry(computer.id.clone()).or_default();
        while charged.front().is_some_and(|(at, _)| now.saturating_duration_since(*at) >= HOUR) {
            charged.pop_front();
        }
        if charged.len() >= QUOTA_FILES {
            return Err(refusal("rate_limited", &format!("at most {QUOTA_FILES} files an hour are taken from one computer")));
        }
        // Counted before it is read, so a flood of bad offers runs out too.
        charged.push_back((now, 0));
        let offer = parse_offer(request).map_err(|(kind, why)| refusal(kind, &why))?;
        let taken: u64 = charged.iter().map(|(_, n)| n).sum();
        if taken.saturating_add(offer.len) > QUOTA_BYTES {
            return Err(refusal(
                "rate_limited",
                &format!("at most {} MiB an hour are taken from one computer", QUOTA_BYTES / 1024 / 1024),
            ));
        }
        if g.busy.contains(&computer.id) || g.busy.len() >= MAX_INCOMING {
            return Err(refusal("busy", "another file is arriving; send this one after it"));
        }
        if let Some(last) = g.charged.get_mut(&computer.id).and_then(|c| c.back_mut()) {
            last.1 = offer.len;
        }
        g.busy.insert(computer.id.clone());
        (Slot { files: Arc::clone(&files), id: computer.id.clone() }, offer)
    };
    let (slot, offer) = slot;
    let sink = match start(&offer.name, offer.len) {
        Ok(s) => s,
        Err(why) => {
            eprintln!("rime-remoted: a file from {} was not saved: {why}", crate::dialer::printable(&computer.name));
            return Err(refusal("unavailable", "this computer could not save the file (out of space, or Downloads is not writable)"));
        }
    };
    let (theirs, ours) = UnixStream::pair().map_err(|e| refusal("unavailable", &e.to_string()))?;
    let len = offer.len;
    std::thread::spawn(move || arrive(ours, sink, offer, computer, files, slot));
    Ok((theirs, json!({"reply": FILE_VERB, "len": len}).to_string().into_bytes()))
}

/// `save_file` on `rime-agentd` for computer `sender`: the connection, taken
/// over, once it said `saving`.
fn save_file(agentd: &Path, sender: &str, name: &str, len: u64) -> Result<UnixStream, String> {
    let request = json!({"cmd": "save_file", "name": name, "len": len}).to_string();
    let conn = crate::proxy::Agentd::open(agentd, &format!("computer:{sender}")).map_err(|e| e.to_string())?;
    let (stream, reply, buffered) = conn.take_over(request.as_bytes()).map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_slice(&reply).unwrap_or(Value::Null);
    if v["reply"] != "saving" || !buffered.is_empty() {
        return Err(crate::dialer::printable(&String::from_utf8_lossy(&reply)));
    }
    // Read once, after the last byte: `rime-agentd` syncs the file first.
    stream.set_read_timeout(Some(SAVED_WAIT)).map_err(|e| e.to_string())?;
    Ok(stream)
}

/// One transfer, on its own thread: the bytes into `sink`, then the outcome
/// to the peer and, when saved, the notification.
fn arrive<S: Read + Write>(ours: UnixStream, mut sink: S, offer: Offer, computer: Device, files: Arc<Files>, slot: Slot) {
    let mut acks = match ours.try_clone() {
        Ok(a) => a,
        Err(_) => return,
    };
    let got = receive(&ours, &mut acks, &mut sink, offer.len, &offer.sha256, STALL, GRACE)
        .and_then(|()| saved_reply(&mut sink));
    match got {
        Ok((name, path)) => {
            let _ = acks.write_all(&line(json!({"reply": "peer_file_saved", "name": name})));
            notify_saved(&files, &computer, &name, path);
        }
        Err(why) => {
            let _ = acks.write_all(&line(json!({"reply": "error", "kind": "failed", "message": why})));
        }
    }
    // The stream to `rime-agentd` goes first: one cut short removes its
    // temporary file before the slot is given back.
    drop(sink);
    let _ = ours.shutdown(std::net::Shutdown::Both);
    drop(slot);
}

/// Read exactly `len` bytes from `peer` into `sink`, a progress line to
/// `acks` every [`WINDOW`], and the last byte only once the bytes hash to
/// `want`. Never more than `len` bytes are read.
fn receive(
    peer: &UnixStream,
    acks: &mut impl Write,
    sink: &mut impl Write,
    len: u64,
    want: &str,
    stall: Duration,
    grace: Duration,
) -> Result<(), String> {
    let start = Instant::now();
    let mut hash = Sha256::new();
    let mut got = 0u64;
    let mut held = None;
    let mut next_ack = WINDOW;
    let mut buf = vec![0u8; 64 * 1024];
    let mut reader = peer;
    while got < len {
        // The next byte is due by the schedule; the stall is the shorter
        // limit only while the transfer is ahead of it.
        let left = (start + due(grace, got)).saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(format!("the file arrived too slowly ({got} of {len} bytes in time); nothing was saved"));
        }
        reader.set_read_timeout(Some(stall.min(left))).map_err(|e| e.to_string())?;
        let want_now = buf.len().min((len - got) as usize);
        let n = match reader.read(&mut buf[..want_now]) {
            Ok(0) => return Err(format!("the other computer stopped after {got} of {len} bytes; nothing was saved")),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                if stall < left {
                    return Err(format!("nothing arrived for {} s; nothing was saved", stall.as_secs()));
                }
                continue;
            }
            Err(e) => return Err(e.to_string()),
        };
        hash.update(&buf[..n]);
        got += n as u64;
        let pass = if got == len {
            held = Some(buf[n - 1]);
            n - 1
        } else {
            n
        };
        sink.write_all(&buf[..pass]).map_err(|e| format!("saving the file failed: {e}"))?;
        if got >= next_ack && got < len {
            acks.write_all(&line(json!({"reply": "peer_file_progress", "got": got})))
                .map_err(|e| e.to_string())?;
            next_ack = got - got % WINDOW + WINDOW;
        }
    }
    if crate::share::hex(&hash.finalize()) != want {
        return Err("the file did not match the hash the other computer sent; nothing was saved".into());
    }
    let last = held.ok_or("nothing arrived")?;
    sink.write_all(&[last]).map_err(|e| format!("saving the file failed: {e}"))?;
    sink.flush().map_err(|e| format!("saving the file failed: {e}"))
}

/// `rime-agentd`'s answer once the last byte is in: the saved name and path.
fn saved_reply<S: Read>(sink: &mut S) -> Result<(String, PathBuf), String> {
    let mut reader = BufReader::new(sink.by_ref().take(64 * 1024));
    let mut text = String::new();
    reader.read_line(&mut text).map_err(|e| format!("the file was not put in place: {e}"))?;
    let v: Value = serde_json::from_str(text.trim()).map_err(|_| "the file was not put in place".to_string())?;
    match (v["reply"].as_str(), v["name"].as_str(), v["path"].as_str()) {
        (Some("saved"), Some(name), Some(path)) => Ok((name.to_string(), PathBuf::from(path))),
        _ => Err("this computer could not put the file in place".into()),
    }
}

/// "<Desk> sent you <name>", with a button that shows it in its folder.
fn notify_saved(files: &Files, computer: &Device, name: &str, path: PathBuf) {
    let n = files.remember_saved(path);
    let from = crate::mirror::clean(&computer.name, crate::mirror::APP_CAP, 0, false);
    let from = if from.is_empty() { "A paired computer".to_string() } else { from };
    let shown = crate::mirror::Shown {
        app_name: "Rime Remote".into(),
        summary: format!("{from} sent you {}", crate::mirror::clean(name, crate::mirror::TITLE_CAP, 0, false)),
        body: "Saved in Downloads. Nothing was opened.".into(),
        reply: None,
        actions: vec![(SHOW_ACTION.to_string(), "Show in folder".to_string())],
        sticky: false,
        expires_ms: None,
    };
    crate::mirror::mirror().submit(crate::mirror::Job::Post {
        device: computer.id.clone(),
        key: format!("{KEY_PREFIX}{n}"),
        shown,
    });
}

/// "Show in folder" pressed on a saved file's notification: show it, if it
/// is still a regular file where it was saved. Nothing is opened. The path
/// comes from this process's own table, never from the notification.
pub fn on_action(state: &State, key: &str, action: &str) -> bool {
    let Some(n) = key.strip_prefix(KEY_PREFIX).and_then(|n| n.parse::<u64>().ok()) else {
        return false;
    };
    if action != SHOW_ACTION {
        return true;
    }
    let Some(path) = state.files.saved(n) else { return true };
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file()) {
        crate::mirror::mirror().submit(crate::mirror::Job::ShowItem(path));
    }
    true
}

/// What [`send`] reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    pub id: String,
    pub computer: String,
    pub name: String,
    pub bytes: u64,
}

/// Send `file` to paired computer `computer` (an id or a name), offered as
/// `raw_name`. Blocks until the other computer says it saved it.
pub fn send(state: &State, computer: &str, file: File, raw_name: &str, via: crate::dialer::Via) -> Result<Sent, String> {
    let c = match state.devices() {
        Ok(store) => store.find_computer(computer).map_err(|e| e.to_string())?.clone(),
        Err(e) => return Err(e.to_string()),
    };
    if !c.is_active() {
        return Err(format!("'{}' was removed; nothing is sent to it", crate::dialer::printable(&c.name)));
    }
    if !c.trusted() {
        return Err(format!(
            "'{}' was introduced by a phone and is not allowed yet; run `rime remote computers allow` first",
            crate::dialer::printable(&c.name)
        ));
    }
    let crate::phone::Outgoing::File { file, name, len } = crate::phone::Outgoing::file(file, raw_name)? else {
        return Err("only a file can be sent".into());
    };
    let name = offered_name(&name)?;
    let sha256 = hash_of(&file, len)?;
    let mut link = crate::dialer::open(state, &c, via)?;
    let hello = link.call(br#"{"cmd":"remote_hello"}"#)?;
    if !crate::share::offers(&hello, FILE_VERB) {
        return Err(format!("'{}' runs a build that does not take files", crate::dialer::printable(&c.name)));
    }
    let request = json!({"cmd": FILE_VERB, "name": name, "len": len, "sha256": sha256}).to_string();
    let outcome = link.upload(request.as_bytes(), len, |buf, at| file.read_at(buf, at))?;
    match outcome["reply"].as_str() {
        Some("peer_file_saved") => Ok(Sent {
            id: c.id.clone(),
            computer: c.name.clone(),
            name: crate::dialer::printable(outcome["name"].as_str().unwrap_or(&name)),
            bytes: len,
        }),
        _ => Err(format!(
            "'{}' did not take it: {}",
            crate::dialer::printable(&c.name),
            crate::dialer::printable(outcome["message"].as_str().unwrap_or("no reason given"))
        )),
    }
}

/// The SHA-256 of the first `len` bytes of `file`, read by offset.
fn hash_of(file: &File, len: u64) -> Result<String, String> {
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut at = 0u64;
    while at < len {
        let want = buf.len().min((len - at) as usize);
        let n = file.read_at(&mut buf[..want], at).map_err(|e| format!("could not read that file: {e}"))?;
        if n == 0 {
            return Err("the file got shorter while it was being read".into());
        }
        hash.update(&buf[..n]);
        at += n as u64;
    }
    Ok(crate::share::hex(&hash.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serve::tests::a_state;

    /// A computer paired here with a code (files on by default).
    fn code_paired(state: &State, seed: u8) -> Device {
        let mut store = state.devices().expect("store");
        let c = store
            .pair_computer(&[seed; 32], &state.identity.public_bytes(), &format!("desk {seed}"), vec![], None, 1)
            .expect("pair");
        state.save_devices(&store).expect("save");
        c
    }

    fn introduced(state: &State, seed: u8) -> Device {
        let mut store = state.devices().expect("store");
        let added = store
            .introduce_computer(
                &[seed; 32],
                &state.identity.public_bytes(),
                &format!("lab {seed}"),
                vec![],
                None,
                rime_remote_core::device::Introducer { id: "phone".into(), name: "Pixel".into() },
                5,
            )
            .expect("introduce");
        let rime_remote_core::device::Introduced::Added(c) = added else { panic!("not added") };
        state.save_devices(&store).expect("save");
        *c
    }

    fn sha(bytes: &[u8]) -> String {
        crate::share::hex(&Sha256::digest(bytes))
    }

    fn offer(name: &str, len: u64, sha256: &str) -> Vec<u8> {
        json!({"cmd": FILE_VERB, "name": name, "len": len, "sha256": sha256}).to_string().into_bytes()
    }

    fn refused(r: Result<(UnixStream, Vec<u8>), Vec<u8>>) -> Value {
        match r {
            Ok((_, reply)) => panic!("taken: {}", String::from_utf8_lossy(&reply)),
            Err(e) => serde_json::from_slice(&e).expect("json"),
        }
    }

    /// A stand-in for `rime-agentd`'s `save_file`: takes `len` bytes, then
    /// says `saved`. What it took comes out of the handle (`None` when the
    /// stream ended short, which is when `rime-agentd` removes its file).
    fn fake_agentd(name: &str, len: u64) -> (UnixStream, std::thread::JoinHandle<Option<Vec<u8>>>) {
        let (ours, mut theirs) = UnixStream::pair().expect("pair");
        let name = name.to_string();
        let h = std::thread::spawn(move || {
            let mut got = vec![0u8; len as usize];
            theirs.read_exact(&mut got).ok()?;
            let _ = writeln!(theirs, "{}", json!({"reply": "saved", "name": name, "path": format!("/fake/{name}")}));
            Some(got)
        });
        (ours, h)
    }

    #[test]
    fn a_name_that_is_not_one_files_name_is_refused_not_cleaned() {
        for bad in [
            "../../.ssh/authorized_keys",
            "..",
            ".",
            "",
            "a/b",
            "/etc/passwd",
            "..\\..\\win.ini",
            "dir\\a.txt",
            "a\nb.txt",
            "a\rb",
            "a\u{0}b",
            "a\u{1b}[31mb",
            "a\u{7f}b",
            "a\u{85}b",
            "invoice\u{202E}fdp.exe",
            "a\u{200B}b",
            "\u{FEFF}a.txt",
            "a\u{2066}b",
            "a\u{061C}b",
            "soft\u{00AD}hyphen.txt",
            "line\u{2028}break.txt",
            "para\u{2029}.txt",
            "tag\u{E0041}.txt",
            "anchor\u{FFF9}.txt",
            "mongol\u{180E}.txt",
        ] {
            assert!(offered_name(bad).is_err(), "{bad:?} was taken");
        }
        let long = "a".repeat(MAX_NAME + 1);
        assert!(offered_name(&long).is_err());
        let longest = "é".repeat(MAX_NAME / 2);
        assert_eq!(offered_name(&longest).expect("fits"), longest);
        // Reduced as every saved name is: never hidden.
        assert_eq!(offered_name(".bashrc").expect("ok"), "bashrc");
        assert_eq!(offered_name("report (final).pdf").expect("ok"), "report (final).pdf");
        // Only dots: the fallback name, never a hidden or relative one.
        let dots = offered_name("...").expect("a fallback");
        assert!(!dots.is_empty() && !dots.starts_with('.'), "{dots}");
    }

    #[test]
    fn an_offer_needs_a_length_from_one_byte_to_the_limit_and_a_real_hash() {
        let h = sha(b"x");
        assert!(parse_offer(&offer("a.txt", 1, &h)).is_ok());
        assert!(parse_offer(&offer("a.txt", MAX_FILE_BYTES, &h)).is_ok());
        assert_eq!(parse_offer(&offer("a.txt", 0, &h)).unwrap_err().0, "too_large");
        assert_eq!(parse_offer(&offer("a.txt", MAX_FILE_BYTES + 1, &h)).unwrap_err().0, "too_large");
        assert_eq!(parse_offer(&offer("a.txt", u64::MAX, &h)).unwrap_err().0, "too_large");
        for bad in [
            json!({"cmd": FILE_VERB, "name": "a.txt", "len": -1, "sha256": h}),
            json!({"cmd": FILE_VERB, "name": "a.txt", "len": 1.5, "sha256": h}),
            json!({"cmd": FILE_VERB, "name": "a.txt", "len": "12", "sha256": h}),
            json!({"cmd": FILE_VERB, "name": "a.txt", "len": 1e30, "sha256": h}),
            json!({"cmd": FILE_VERB, "name": 7, "len": 1, "sha256": h}),
            json!({"cmd": FILE_VERB, "len": 1, "sha256": h}),
            json!({"cmd": FILE_VERB, "name": "a.txt", "len": 1, "sha256": h.to_uppercase()}),
            json!({"cmd": FILE_VERB, "name": "a.txt", "len": 1, "sha256": &h[..63]}),
            json!({"cmd": FILE_VERB, "name": "a.txt", "len": 1, "sha256": format!("{h}0")}),
            json!({"cmd": FILE_VERB, "name": "a.txt", "len": 1}),
            json!({"cmd": "save_file", "name": "a.txt", "len": 1, "sha256": h}),
        ] {
            assert!(parse_offer(bad.to_string().as_bytes()).is_err(), "{bad}");
        }
        assert!(parse_offer(b"not json").is_err());
    }

    #[test]
    fn a_file_is_taken_only_from_a_trusted_computer_whose_switch_is_on() {
        let (state, _dir) = a_state("files-gate");
        let desk = code_paired(&state, 31);
        let lab = introduced(&state, 32);
        let body = b"hello";
        let line = offer("a.txt", 5, &sha(body));
        let start = |_: &str, _: u64| -> Result<UnixStream, String> { panic!("rime-agentd was asked") };
        // Not paired at all.
        let v = refused(open_incoming_with(&state, "stranger", &line, Instant::now(), start));
        assert_eq!(v["kind"], "not_shared", "{v}");
        // Introduced and not allowed: refused, even with the switch set in
        // the file by hand.
        {
            let mut store = state.devices().expect("store");
            assert!(store.set_computer_files(&lab.id, true).is_err());
            store.computers.get_mut(&lab.id).expect("there").share.files = Some(true);
            state.save_devices(&store).expect("save");
        }
        let v = refused(open_incoming_with(&state, &lab.id, &line, Instant::now(), start));
        assert_eq!(v["kind"], "not_shared", "{v}");
        // Allowed: taken.
        {
            let mut store = state.devices().expect("store");
            store.computers.get_mut(&lab.id).expect("there").share.files = None;
            store.allow_computer(&lab.id, 6).expect("allow");
            state.save_devices(&store).expect("save");
        }
        let (sink, saved) = fake_agentd("a.txt", 5);
        let (mut ch, reply) =
            open_incoming_with(&state, &lab.id, &line, Instant::now(), |_, _| Ok(sink)).expect("taken once allowed");
        assert_eq!(serde_json::from_slice::<Value>(&reply).expect("json")["reply"], FILE_VERB);
        ch.write_all(body).expect("write");
        assert_eq!(saved.join().expect("agentd").as_deref(), Some(&body[..]));
        // The code-paired one, switched off: refused.
        {
            let mut store = state.devices().expect("store");
            store.set_computer_files(&desk.id, false).expect("off");
            state.save_devices(&store).expect("save");
        }
        let v = refused(open_incoming_with(&state, &desk.id, &line, Instant::now(), start));
        assert_eq!(v["kind"], "not_shared", "{v}");
        // On again, then removed: a tombstoned computer is accepted for
        // nothing, whatever its switch says.
        {
            let mut store = state.devices().expect("store");
            store.set_computer_files(&desk.id, true).expect("on");
            store.revoke_computer(&desk.id, 7).expect("remove");
            store.computers.get_mut(&desk.id).expect("kept as a tombstone").share.files = Some(true);
            state.save_devices(&store).expect("save");
        }
        let v = refused(open_incoming_with(&state, &desk.id, &line, Instant::now(), start));
        assert_eq!(v["kind"], "not_shared", "{v}");
    }

    #[test]
    fn a_traversal_name_never_reaches_rime_agentd() {
        let (state, _dir) = a_state("files-traversal");
        let desk = code_paired(&state, 33);
        for name in ["../../.ssh/authorized_keys", "..", "a/b", "x\u{202E}txt.exe", "a\nb"] {
            let line = offer(name, 5, &sha(b"hello"));
            let v = refused(open_incoming_with(&state, &desk.id, &line, Instant::now(), |_, _| -> Result<UnixStream, String> {
                panic!("rime-agentd was asked for {name:?}")
            }));
            assert_eq!(v["kind"], "bad_request", "{name:?}: {v}");
        }
        assert_eq!(state.files.arriving(), 0);
    }

    #[test]
    fn the_hourly_quota_counts_every_offer_and_every_byte_and_runs_out() {
        let (state, _dir) = a_state("files-quota");
        let desk = code_paired(&state, 34);
        let other = code_paired(&state, 35);
        let t0 = Instant::now();
        // Bad offers count: a flood of them runs out too.
        for _ in 0..QUOTA_FILES {
            let v = refused(open_incoming_with(&state, &desk.id, b"{}", t0, |_, _| -> Result<UnixStream, String> {
                panic!("asked")
            }));
            assert_eq!(v["kind"], "bad_request");
        }
        let good = offer("a.txt", 5, &sha(b"hello"));
        let v = refused(open_incoming_with(&state, &desk.id, &good, t0, |_, _| -> Result<UnixStream, String> {
            panic!("asked")
        }));
        assert_eq!(v["kind"], "rate_limited", "{v}");
        // Another computer has its own.
        let (sink, saved) = fake_agentd("a.txt", 5);
        let (mut ch, _) = open_incoming_with(&state, &other.id, &good, t0, |_, _| Ok(sink)).expect("own quota");
        ch.write_all(b"hello").expect("write");
        saved.join().expect("agentd");
        // An hour later the first is taken again.
        let later = t0 + HOUR;
        let (sink, saved) = fake_agentd("a.txt", 5);
        let (mut ch, _) = open_incoming_with(&state, &desk.id, &good, later, |_, _| Ok(sink)).expect("a new hour");
        ch.write_all(b"hello").expect("write");
        saved.join().expect("agentd");
        // Bytes: what was accepted this hour, failed transfers included.
        let (state, _dir) = a_state("files-quota-bytes");
        let desk = code_paired(&state, 36);
        let big = MAX_FILE_BYTES;
        let line = offer("big.bin", big, &sha(b""));
        let per_hour = (QUOTA_BYTES / big) as usize;
        for i in 0..per_hour {
            let (theirs, ours) = UnixStream::pair().expect("pair");
            let (ch, _) = open_incoming_with(&state, &desk.id, &line, t0, |_, _| Ok(ours)).expect("within the bytes");
            // Cut short at once: the slot is given back, the bytes stay charged.
            drop(ch);
            drop(theirs);
            let deadline = Instant::now() + Duration::from_secs(10);
            while state.files.arriving() != 0 {
                assert!(Instant::now() < deadline, "transfer {i} kept its slot");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let v = refused(open_incoming_with(&state, &desk.id, &line, t0, |_, _| -> Result<UnixStream, String> {
            panic!("asked")
        }));
        assert_eq!(v["kind"], "rate_limited", "{v}");
        assert!(v["message"].as_str().unwrap_or_default().contains("MiB"), "{v}");
    }

    #[test]
    fn one_transfer_per_computer_and_two_at_once_and_a_failed_one_gives_its_slot_back() {
        let (state, _dir) = a_state("files-busy");
        let a = code_paired(&state, 41);
        let b = code_paired(&state, 42);
        let c = code_paired(&state, 43);
        let line = offer("a.txt", 5, &sha(b"hello"));
        let (_a_agentd_end, a_sink) = UnixStream::pair().expect("pair");
        let (a_ch, _) = open_incoming_with(&state, &a.id, &line, Instant::now(), |_, _| Ok(a_sink)).expect("first");
        let v = refused(open_incoming_with(&state, &a.id, &line, Instant::now(), |_, _| -> Result<UnixStream, String> {
            panic!("asked")
        }));
        assert_eq!(v["kind"], "busy", "a second at once from one computer: {v}");
        let (_b_agentd_end, b_sink) = UnixStream::pair().expect("pair");
        let (_b_ch, _) = open_incoming_with(&state, &b.id, &line, Instant::now(), |_, _| Ok(b_sink)).expect("second");
        assert_eq!(state.files.arriving(), MAX_INCOMING);
        let v = refused(open_incoming_with(&state, &c.id, &line, Instant::now(), |_, _| -> Result<UnixStream, String> {
            panic!("asked")
        }));
        assert_eq!(v["kind"], "busy", "a third at once: {v}");
        // The first ends early (its computer went away): its slot is free.
        drop(a_ch);
        let deadline = Instant::now() + Duration::from_secs(10);
        while state.files.arriving() != 1 {
            assert!(Instant::now() < deadline, "a cut-short transfer kept its slot");
            std::thread::sleep(Duration::from_millis(5));
        }
        // A start that fails gives its slot back at once, and the peer is
        // told none of rime-agentd's words.
        let v = refused(open_incoming_with(&state, &c.id, &line, Instant::now(), |_, _| -> Result<UnixStream, String> {
            Err("/home/someone/Downloads: no space".into())
        }));
        assert_eq!(v["kind"], "unavailable");
        assert!(!v.to_string().contains("/home"), "a path went to the peer: {v}");
        assert_eq!(state.files.arriving(), 1);
    }

    /// `receive` over a socketpair: what reached the sink, the progress lines
    /// and the outcome.
    fn run_receive(sent: &[u8], len: u64, want: &str, close: bool) -> (Vec<u8>, String, Result<(), String>) {
        let (peer_end, ours) = UnixStream::pair().expect("pair");
        let sent = sent.to_vec();
        let writer = std::thread::spawn(move || {
            let mut w = &peer_end;
            let _ = w.write_all(&sent);
            if close {
                let _ = peer_end.shutdown(std::net::Shutdown::Write);
            }
            std::thread::sleep(Duration::from_millis(300));
        });
        let mut acks = Vec::new();
        let mut sink = Vec::new();
        let r = receive(&ours, &mut acks, &mut sink, len, want, Duration::from_millis(200), Duration::from_secs(10));
        writer.join().expect("writer");
        (sink, String::from_utf8(acks).expect("utf8"), r)
    }

    #[test]
    fn the_last_byte_waits_for_the_hash_and_no_byte_past_the_length_is_taken() {
        // Past the third window by more than one read, so its line is due.
        let body: Vec<u8> = (0..(3 * WINDOW + 100_000)).map(|i| (i % 251) as u8).collect();
        let len = body.len() as u64;
        // Right hash: every byte, and a progress line per window.
        let (sink, acks, r) = run_receive(&body, len, &sha(&body), true);
        assert_eq!(r, Ok(()));
        assert_eq!(sink, body);
        let gots: Vec<u64> = acks.lines().map(|l| serde_json::from_str::<Value>(l).expect("json")["got"].as_u64().expect("got")).collect();
        assert_eq!(gots.len(), 3, "{acks}");
        assert!(gots.iter().enumerate().all(|(i, g)| *g / WINDOW == i as u64 + 1 && *g < len), "{gots:?}");
        // Wrong hash: all but the last byte, so rime-agentd never completes it.
        let (sink, _, r) = run_receive(&body, len, &sha(b"something else"), true);
        assert!(r.is_err());
        assert_eq!(sink.len() as u64, len - 1);
        // More than offered: only the offered bytes are read.
        let mut more = body.clone();
        more.extend_from_slice(b"EXTRA");
        let (sink, _, r) = run_receive(&more, len, &sha(&body), false);
        assert_eq!(r, Ok(()));
        assert_eq!(sink, body);
        // Fewer, then the peer goes: an error, never the full length.
        let (sink, _, r) = run_receive(&body[..1000], len, &sha(&body), true);
        assert!(r.unwrap_err().contains("stopped after 1000"));
        assert!((sink.len() as u64) < len);
        // Fewer, then silence: the stall ends it.
        let (_, _, r) = run_receive(&body[..1000], len, &sha(&body), false);
        assert!(r.unwrap_err().contains("nothing arrived"));
    }

    #[test]
    fn a_computer_that_trickles_loses_its_slot_once_it_falls_behind() {
        let body: Vec<u8> = (0..1_000_000u32).map(|i| (i % 251) as u8).collect();
        let len = body.len() as u64;
        let (peer_end, ours) = UnixStream::pair().expect("pair");
        // A few bytes every 50 ms: never a 2 s stall, far below the pace.
        let writer = std::thread::spawn(move || {
            let mut w = &peer_end;
            for chunk in body.chunks(16).take(40) {
                if w.write_all(chunk).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let started = Instant::now();
        let (mut acks, mut sink) = (Vec::new(), Vec::new());
        let r = receive(&ours, &mut acks, &mut sink, len, &sha(b"x"), Duration::from_secs(2), Duration::from_millis(300));
        let took = started.elapsed();
        drop(ours);
        writer.join().expect("writer");
        assert!(r.as_ref().is_err_and(|e| e.contains("too slowly")), "{r:?}");
        assert!(took < Duration::from_secs(1), "it held the slot for {took:?}");
        // The schedule: the grace, then the pace, and the whole of it is the budget.
        assert_eq!(due(GRACE, 0), GRACE);
        assert_eq!(due(GRACE, PACE * 10), GRACE + Duration::from_secs(10));
        assert_eq!(due(GRACE, MAX_FILE_BYTES), budget(MAX_FILE_BYTES));
    }

    #[test]
    fn a_saved_file_is_reported_by_name_and_its_path_stays_here() {
        let (state, _dir) = a_state("files-saved");
        let desk = code_paired(&state, 44);
        let body = b"twelve bytes";
        let line = offer("notes.txt", body.len() as u64, &sha(body));
        let (sink, saved) = fake_agentd("notes (1).txt", body.len() as u64);
        let (mut ch, _) = open_incoming_with(&state, &desk.id, &line, Instant::now(), |name, len| {
            assert_eq!((name, len), ("notes.txt", body.len() as u64));
            Ok(sink)
        })
        .expect("taken");
        ch.write_all(body).expect("write");
        let mut out = String::new();
        BufReader::new(&ch).read_line(&mut out).expect("outcome");
        let v: Value = serde_json::from_str(out.trim()).expect("json");
        assert_eq!(v, json!({"reply": "peer_file_saved", "name": "notes (1).txt"}));
        assert!(!out.contains("/fake"), "the path went to the peer: {out}");
        saved.join().expect("agentd");
        // The notification's button shows the path from this table only.
        let deadline = Instant::now() + Duration::from_secs(5);
        let n = loop {
            if let Some((n, p)) = state.files.lock().saved.back().cloned() {
                assert_eq!(p, PathBuf::from("/fake/notes (1).txt"));
                break n;
            }
            assert!(Instant::now() < deadline, "never remembered");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(on_action(&state, &format!("{KEY_PREFIX}{n}"), SHOW_ACTION));
        assert!(!on_action(&state, "intro:1", SHOW_ACTION), "another module's key was claimed");
    }
}
