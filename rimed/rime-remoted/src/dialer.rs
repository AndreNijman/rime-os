//! Dialling another of the owner's computers for one request.
//!
//! The same session the phones open (`Noise_IK`, the session hello, then
//! frames), with this computer in the phone's place: its own identity key as
//! the device key, and the other computer's key, proved at pairing, as the
//! desktop key. The other side's store holds this computer under its
//! `computers`, so the session it accepts is a peer session (`Scope::Peer`).
//!
//! Where: each stored address in turn (the one the owner typed first, then
//! the ones the computer listed, then wherever it last answered), then the
//! relay as a guest, at the other computer's rendezvous on **this computer's
//! own relay**. The relay the other computer named at pairing is kept for
//! the listing and never dialled: a paired computer must not be able to
//! choose where this one connects (and what it reveals there). Two computers
//! meet on the relay only when both are configured with the same one. The
//! addresses it listed were checked at pairing (`About::checked`: no
//! loopback, unspecified, multicast, broadcast or port 0). mDNS is not
//! browsed: a `.local` name the owner typed resolves through the system
//! resolver like any other.
//!
//! One request, one connection, no held link: presence (a held link and its
//! power cost) is the next step and is not here.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rime_remote_core::device::Device;
use rime_remote_core::noise::{Channel, Handshake, MAX_MESSAGE};
use rime_remote_core::relay::{Endpoint, Notice, Op, Receiver, Role, Sender};
use rime_remote_core::tls::Trust;
use rime_remote_core::wire::Frame;

use crate::state::State;

/// Which paths a dial may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    Any,
    Lan,
    Relay,
}

/// How long to try one address.
const CONNECT: Duration = Duration::from_secs(2);
/// How long to wait for any one read once connected.
const READ: Duration = Duration::from_secs(10);
/// How long to keep asking the relay while it says the other computer is
/// not there yet (a 409 in the moment between two of its waiting connections).
const RELAY_RETRY: Duration = Duration::from_secs(5);

trait Stream: Read + Write + Send {
    /// How long one read may wait from now on.
    fn wait(&self, d: Duration) -> std::io::Result<()>;
    /// A second handle that writes to the same connection, for a link split
    /// in two ([`Link::into_duplex`]).
    fn writer(&self) -> std::io::Result<Box<dyn Write + Send>>;
    /// The socket under it, to shut it from another thread.
    fn socket(&self) -> std::io::Result<TcpStream>;
}

impl Stream for TcpStream {
    fn wait(&self, d: Duration) -> std::io::Result<()> {
        self.set_read_timeout(Some(d))
    }
    fn writer(&self) -> std::io::Result<Box<dyn Write + Send>> {
        Ok(Box::new(self.try_clone()?))
    }
    fn socket(&self) -> std::io::Result<TcpStream> {
        self.try_clone()
    }
}

impl Stream for RelayPipe {
    fn wait(&self, d: Duration) -> std::io::Result<()> {
        self.socket.set_read_timeout(Some(d))
    }
    fn writer(&self) -> std::io::Result<Box<dyn Write + Send>> {
        Ok(Box::new(RelayWriter { tx: Arc::clone(&self.tx) }))
    }
    fn socket(&self) -> std::io::Result<TcpStream> {
        self.socket.try_clone()
    }
}

/// The writing half of a relay connection: the same sender the reading half
/// answers the relay's pings with.
struct RelayWriter {
    tx: Arc<Mutex<Sender<Box<dyn Write + Send>>>>,
}

impl Write for RelayWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.tx
            .lock()
            .map_err(|_| std::io::Error::other("relay sender poisoned"))?
            .binary(bytes)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The writing half of a split link; cloned freely, one frame at a time.
#[derive(Clone)]
pub struct LinkWriter {
    inner: Arc<Mutex<(Box<dyn Write + Send>, Arc<Mutex<Channel>>)>>,
}

impl LinkWriter {
    pub fn send(&self, frame: Frame) -> Result<(), String> {
        let bytes = frame.encode().map_err(|e| e.to_string())?;
        let mut g = self.inner.lock().map_err(|_| "the link is broken".to_string())?;
        let sealed = g.1.lock().map_err(|_| "the link is broken".to_string())?.seal(&bytes).map_err(|e| e.to_string())?;
        let mut framed = Vec::with_capacity(4 + sealed.len());
        framed.extend_from_slice(&(sealed.len() as u32).to_be_bytes());
        framed.extend_from_slice(&sealed);
        g.0.write_all(&framed).and_then(|_| g.0.flush()).map_err(|e| e.to_string())
    }
}

/// The reading half of a split link. Answers pings itself.
pub struct LinkReader {
    stream: Box<dyn Stream>,
    channel: Arc<Mutex<Channel>>,
    writer: LinkWriter,
}

impl LinkReader {
    /// The next frame that is not a ping.
    pub fn next(&mut self) -> Result<Frame, String> {
        loop {
            let sealed = read_message(&mut *self.stream).map_err(|e| format!("no answer: {e}"))?;
            let plain = self
                .channel
                .lock()
                .map_err(|_| "the link is broken".to_string())?
                .open(&sealed)
                .map_err(|e| e.to_string())?;
            match Frame::decode(&plain).map_err(|e| e.to_string())? {
                Frame::Ping { token } => self.writer.send(Frame::Pong { token })?,
                other => return Ok(other),
            }
        }
    }
}

/// How long a sender waits for the outcome once every byte is out: the other
/// computer syncs up to [`crate::files::MAX_FILE_BYTES`] to disk first.
const OUTCOME: Duration = Duration::from_secs(150);

/// An open session to another computer.
pub struct Link {
    stream: Box<dyn Stream>,
    channel: Channel,
    /// `lan` or `relay`.
    pub path: &'static str,
    /// The address that answered, on the LAN.
    pub address: Option<String>,
}

/// Text from another computer, made safe to print: control characters out,
/// at most 200 characters.
pub fn printable(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(200).collect()
}

/// Open a session to `computer`.
pub fn open(state: &State, computer: &Device, via: Via) -> Result<Link, String> {
    let key = rime_remote_core::device::check_key(&computer.public_key).map_err(|e| e.to_string())?;
    if refuses(state, &key) {
        return Err(format!(
            "'{}' has this computer's own Rime Remote key (a copied home folder), so it is not dialled",
            computer.name
        ));
    }
    let secret = crate::secret_of(state);
    let mut why: Vec<String> = Vec::new();
    if via != Via::Relay {
        for a in &computer.addresses {
            match dial_tcp(a) {
                Ok(s) => match handshake(Box::new(s), &secret, &key) {
                    Ok((stream, channel)) => {
                        return Ok(Link {
                            stream,
                            channel,
                            path: "lan",
                            address: Some(a.clone()),
                        })
                    }
                    Err(e) => why.push(format!("{a}: {e}")),
                },
                Err(e) => why.push(format!("{a}: {e}")),
            }
        }
        if computer.addresses.is_empty() {
            why.push("no address is known for it".into());
        }
    }
    if via != Via::Lan {
        match relay_for(state, computer) {
            Ok(url) => match relay_pipe(&url, &key) {
                Ok(pipe) => match handshake(Box::new(pipe), &secret, &key) {
                    Ok((stream, channel)) => {
                        return Ok(Link {
                            stream,
                            channel,
                            path: "relay",
                            address: None,
                        })
                    }
                    Err(e) => why.push(format!("relay: {e}")),
                },
                Err(e) => why.push(format!("relay: {e}")),
            },
            Err(e) => why.push(e),
        }
    }
    Err(format!("could not reach '{}': {}", computer.name, why.join("; ")))
}

/// The relay a dial to `computer` may use: this computer's own, never one
/// the other computer named. When they differ, the error says so, since the
/// other computer is then not waiting where this one looks.
fn relay_for(state: &State, computer: &Device) -> Result<String, String> {
    match (&state.relay, &computer.relay) {
        (Some(own), _) => Ok(own.clone()),
        (None, Some(theirs)) => Err(format!(
            "this computer has no relay configured; '{}' waits on {}, which is not dialled because a paired \
             computer does not choose where this one connects",
            printable(&computer.name),
            printable(theirs)
        )),
        (None, None) => Err("no relay is configured on this computer".into()),
    }
}

/// Whether a dial to this key is refused: it is this computer's own.
fn refuses(state: &State, key: &[u8; 32]) -> bool {
    *key == state.identity.public_bytes()
}

fn dial_tcp(address: &str) -> Result<TcpStream, String> {
    let addrs: Vec<_> = address
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .collect();
    let s = crate::computers::connect_any(&addrs, CONNECT)?;
    s.set_nodelay(true).ok();
    s.set_read_timeout(Some(READ)).map_err(|e| e.to_string())?;
    s.set_write_timeout(Some(READ)).map_err(|e| e.to_string())?;
    Ok(s)
}

fn write_message(w: &mut dyn Stream, message: &[u8]) -> std::io::Result<()> {
    let mut framed = Vec::with_capacity(4 + message.len());
    framed.extend_from_slice(&(message.len() as u32).to_be_bytes());
    framed.extend_from_slice(message);
    w.write_all(&framed)?;
    w.flush()
}

fn read_message(r: &mut dyn Stream) -> std::io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let n = u32::from_be_bytes(len) as usize;
    if n > MAX_MESSAGE {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "message too long"));
    }
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

/// The session hello and `Noise_IK`, as a device would.
fn handshake(mut s: Box<dyn Stream>, secret: &[u8; 32], key: &[u8; 32]) -> Result<(Box<dyn Stream>, Channel), String> {
    s.write_all(&[crate::serve::HELLO_SESSION]).map_err(|e| e.to_string())?;
    let mut hs = Handshake::session_initiator(secret, key, rime_remote_core::REMOTE_PROTOCOL_VERSION)
        .map_err(|e| e.to_string())?;
    let m1 = hs.write(b"").map_err(|e| e.to_string())?;
    write_message(&mut *s, &m1).map_err(|e| e.to_string())?;
    // A computer that does not accept this one's key (it removed this one,
    // or never paired) ends the connection here rather than answering.
    let m2 = read_message(&mut *s).map_err(|e| match e.kind() {
        std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset => {
            "it does not accept this computer (removed there, or never paired)".to_string()
        }
        _ => e.to_string(),
    })?;
    hs.read(&m2).map_err(|e| e.to_string())?;
    let channel = hs.into_transport().map_err(|e| e.to_string())?;
    Ok((s, channel))
}

impl Link {
    /// Send one control line and return the first control reply, answering
    /// pings on the way.
    pub fn call(&mut self, line: &[u8]) -> Result<serde_json::Value, String> {
        self.send(Frame::Control(line.to_vec()))?;
        loop {
            let sealed = read_message(&mut *self.stream).map_err(|e| format!("no answer: {e}"))?;
            let plain = self.channel.open(&sealed).map_err(|e| e.to_string())?;
            match Frame::decode(&plain).map_err(|e| e.to_string())? {
                Frame::Ping { token } => self.send(Frame::Pong { token })?,
                Frame::Control(reply) => {
                    return serde_json::from_slice(&reply).map_err(|e| format!("an unreadable answer: {e}"))
                }
                _ => continue,
            }
        }
    }

    /// Open `channel` with `request` and return the other computer's answer
    /// to it, answering pings; an error when it closes the channel instead.
    pub fn open_channel(&mut self, channel: u32, request: &[u8]) -> Result<serde_json::Value, String> {
        self.send(Frame::Open { channel, request: request.to_vec() })?;
        loop {
            match self.next()? {
                Frame::Ping { token } => self.send(Frame::Pong { token })?,
                Frame::Control(reply) => {
                    return serde_json::from_slice(&reply).map_err(|e| format!("an unreadable answer: {e}"))
                }
                Frame::Close { channel: c, .. } if c == channel => return Err("the other computer closed it".into()),
                _ => continue,
            }
        }
    }

    /// Split the link for a channel held open: frames are read on one
    /// thread (each read waiting at most `wait`) and written from another.
    /// The socket is for shutting it from either.
    pub fn into_duplex(self, wait: Duration) -> Result<(LinkReader, LinkWriter, TcpStream), String> {
        self.stream.wait(wait).map_err(|e| e.to_string())?;
        let w = self.stream.writer().map_err(|e| e.to_string())?;
        let socket = self.stream.socket().map_err(|e| e.to_string())?;
        let channel = Arc::new(Mutex::new(self.channel));
        let writer = LinkWriter {
            inner: Arc::new(Mutex::new((w, Arc::clone(&channel)))),
        };
        Ok((
            LinkReader {
                stream: self.stream,
                channel,
                writer: writer.clone(),
            },
            writer,
            socket,
        ))
    }

    /// One frame from the other computer.
    fn next(&mut self) -> Result<Frame, String> {
        let sealed = read_message(&mut *self.stream).map_err(|e| format!("no answer: {e}"))?;
        let plain = self.channel.open(&sealed).map_err(|e| e.to_string())?;
        Frame::decode(&plain).map_err(|e| e.to_string())
    }

    /// Send `len` bytes read by `read_at` on a channel opened with
    /// `request`, and return the other computer's last line on it: its
    /// refusal of the request, or the outcome. Never more than
    /// [`crate::files::IN_FLIGHT`] bytes are unacknowledged; waiting for the
    /// next progress line is where pings are answered.
    pub fn upload(
        &mut self,
        request: &[u8],
        len: u64,
        read_at: impl Fn(&mut [u8], u64) -> std::io::Result<usize>,
    ) -> Result<serde_json::Value, String> {
        const CH: u32 = 1;
        // The whole transfer, however the other computer paces its frames:
        // what it gives itself to receive, and the wait for its outcome.
        let deadline = Instant::now() + crate::files::budget(len) + OUTCOME;
        self.send(Frame::Open { channel: CH, request: request.to_vec() })?;
        let accepted = loop {
            if Instant::now() >= deadline {
                return Err("the other computer never answered the offer".into());
            }
            match self.next()? {
                Frame::Ping { token } => self.send(Frame::Pong { token })?,
                Frame::Control(reply) => {
                    break serde_json::from_slice::<serde_json::Value>(&reply)
                        .map_err(|e| format!("an unreadable answer: {e}"))?
                }
                Frame::Close { channel: CH, .. } => return Err("the other computer closed the transfer".into()),
                _ => continue,
            }
        };
        if accepted["reply"] != crate::files::FILE_VERB || accepted["len"].as_u64() != Some(len) {
            return Ok(accepted);
        }
        let mut lines = Lines::default();
        let mut sent = 0u64;
        let mut acked = 0u64;
        let mut buf = vec![0u8; 60 * 1024];
        let result = (|| {
            while sent < len {
                let want = buf.len().min((len - sent) as usize);
                let n = read_at(&mut buf[..want], sent).map_err(|e| format!("could not read the file: {e}"))?;
                if n == 0 {
                    return Err("the file got shorter while it was being sent".to_string());
                }
                for f in Frame::data_frames(CH, &buf[..n]) {
                    self.send(f)?;
                }
                sent += n as u64;
                while sent - acked >= crate::files::IN_FLIGHT {
                    match self.channel_line(CH, &mut lines, deadline)? {
                        Some(v) if v["reply"] == "peer_file_progress" => {
                            acked = acked.max(v["got"].as_u64().unwrap_or(0).min(sent))
                        }
                        Some(v) => return Ok(Some(v)),
                        None => return Err("the other computer closed the transfer".to_string()),
                    }
                }
            }
            Ok(None)
        })();
        let early = match result {
            Ok(early) => early,
            Err(why) => {
                // The other computer drops what it has when the channel closes.
                let _ = self.send(Frame::Close { channel: CH, reason: String::new() });
                return Err(why);
            }
        };
        if let Some(v) = early {
            return Ok(v);
        }
        self.stream.wait(OUTCOME).map_err(|e| e.to_string())?;
        loop {
            match self.channel_line(CH, &mut lines, deadline)? {
                Some(v) if v["reply"] == "peer_file_progress" => continue,
                Some(v) => return Ok(v),
                None => return Err("the other computer closed the transfer without saying whether it saved it".into()),
            }
        }
    }

    /// The next line the other computer wrote on `channel`, answering pings;
    /// `None` when it closed the channel. An error once `deadline` passes.
    fn channel_line(
        &mut self,
        channel: u32,
        lines: &mut Lines,
        deadline: Instant,
    ) -> Result<Option<serde_json::Value>, String> {
        loop {
            if let Some(l) = lines.next() {
                return serde_json::from_slice(&l).map(Some).map_err(|e| format!("an unreadable answer: {e}"));
            }
            if Instant::now() >= deadline {
                return Err("the other computer took too long to take the file".into());
            }
            match self.next()? {
                Frame::Ping { token } => self.send(Frame::Pong { token })?,
                Frame::Data { channel: c, bytes } if c == channel => lines.push(&bytes)?,
                Frame::Close { channel: c, .. } if c == channel => return Ok(None),
                _ => continue,
            }
        }
    }

    fn send(&mut self, frame: Frame) -> Result<(), String> {
        let bytes = frame.encode().map_err(|e| e.to_string())?;
        let sealed = self.channel.seal(&bytes).map_err(|e| e.to_string())?;
        write_message(&mut *self.stream, &sealed).map_err(|e| e.to_string())
    }
}

/// Lines on a channel, as its `Data` frames carry them: whole lines only,
/// and no line longer than the cap.
#[derive(Default)]
struct Lines {
    pending: Vec<u8>,
}

impl Lines {
    const CAP: usize = 16 * 1024;

    fn push(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.pending.extend_from_slice(bytes);
        if self.pending.len() > Self::CAP && !self.pending.contains(&b'\n') {
            return Err("the other computer sent a line too long to read".into());
        }
        Ok(())
    }

    fn next(&mut self) -> Option<Vec<u8>> {
        let at = self.pending.iter().position(|b| *b == b'\n')?;
        let mut l: Vec<u8> = self.pending.drain(..=at).collect();
        l.pop();
        Some(l)
    }
}

/// A relay connection as a byte stream: the guest's half of the splice the
/// waiting computer runs (`relay.rs`).
struct RelayPipe {
    rx: Receiver<Box<dyn Read + Send>>,
    tx: Arc<Mutex<Sender<Box<dyn Write + Send>>>>,
    spare: Vec<u8>,
    at: usize,
    socket: TcpStream,
}

impl Drop for RelayPipe {
    fn drop(&mut self) {
        let _ = self.socket.shutdown(std::net::Shutdown::Both);
    }
}

impl Read for RelayPipe {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.at >= self.spare.len() {
            let m = self.rx.message().map_err(|e| std::io::Error::other(e.to_string()))?;
            match m.op {
                Op::Binary | Op::Continuation => {
                    self.spare = m.payload;
                    self.at = 0;
                }
                Op::Close => return Ok(0),
                Op::Ping => {
                    if let Ok(mut s) = self.tx.lock() {
                        let _ = s.pong(&m.payload);
                    }
                }
                Op::Text | Op::Pong => continue,
            }
        }
        let n = (self.spare.len() - self.at).min(out.len());
        out[..n].copy_from_slice(&self.spare[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

impl Write for RelayPipe {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.tx
            .lock()
            .map_err(|_| std::io::Error::other("relay sender poisoned"))?
            .binary(bytes)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Join the other computer's rendezvous on `url` as a guest, and wait for the
/// relay to say it is there.
fn relay_pipe(url: &str, key: &[u8; 32]) -> Result<RelayPipe, String> {
    let endpoint = Endpoint::parse(url).map_err(|e| e.to_string())?;
    let trust = if endpoint.secure {
        Some(Trust::system().map_err(|e| e.to_string())?)
    } else {
        None
    };
    let rendezvous = rime_remote_core::rendezvous::rendezvous_id(key);
    let deadline = Instant::now() + RELAY_RETRY;
    let joined = loop {
        match crate::relay::dial(&endpoint, &rendezvous, Role::Guest, trust.as_ref()) {
            Ok(j) => break j,
            Err(e) if e.refused_with() == Some(409) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) if e.refused_with() == Some(409) => {
                return Err("the other computer is not waiting on the relay".into())
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    joined.socket.set_read_timeout(Some(READ)).map_err(|e| e.to_string())?;
    joined.socket.set_write_timeout(Some(READ)).map_err(|e| e.to_string())?;
    let mut pipe = RelayPipe {
        rx: joined.receiver,
        tx: joined.sender,
        spare: Vec::new(),
        at: 0,
        socket: joined.socket,
    };
    loop {
        let m = pipe.rx.message().map_err(|e| e.to_string())?;
        match m.op {
            Op::Text if Notice::parse(&m.payload) == Some(Notice::Paired) => return Ok(pipe),
            Op::Text | Op::Pong => continue,
            Op::Ping => {
                if let Ok(mut s) = pipe.tx.lock() {
                    let _ = s.pong(&m.payload);
                }
            }
            Op::Close => return Err("the relay closed the connection".into()),
            Op::Binary | Op::Continuation => return Err("the relay sent data before the other computer arrived".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_computer_with_this_computers_own_key_is_never_dialled() {
        let (state, dir) = crate::serve::tests::a_state("dial-own");
        let own = state.identity.public_bytes();
        let me = Device {
            id: Device::id_for(&state.identity.public_key()),
            name: "copied".into(),
            public_key: state.identity.public_key(),
            paired_ms: 0,
            last_seen_ms: None,
            revoked_ms: None,
            requires_user_verification: false,
            last_path: None,
            companion_key: None,
            retired_companion_keys: Vec::new(),
            // A listener that would count a dial, so a refusal is measured.
            addresses: Vec::new(),
            relay: None,
            forgotten_ms: None,
            told_ms: None,
            learned: false,
            introduced_by: None,
            allowed_ms: None,
            share: Default::default(),
        };
        assert!(refuses(&state, &own));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let me = Device {
            addresses: vec![listener.local_addr().expect("addr").to_string()],
            ..me
        };
        let r = open(&state, &me, Via::Lan);
        assert!(r.as_ref().err().is_some_and(|e| e.contains("own Rime Remote key")), "{:?}", r.err());
        assert!(listener.accept().is_err(), "the copied key was dialled");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A listener that counts connections and hangs up on each at once:
    /// what a relay the other computer named would see if this one dialled
    /// it. Hanging up keeps a dial that reached it from waiting for a relay
    /// that never answers.
    fn counting() -> (Arc<std::sync::atomic::AtomicUsize>, String) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("ws://{}", l.local_addr().expect("addr"));
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = count.clone();
        std::thread::spawn(move || {
            for s in l.incoming() {
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                drop(s);
            }
        });
        (count, url)
    }

    fn a_computer(state: &State, relay: Option<String>) -> Device {
        let other = rime_remote_core::identity::Identity::generate().expect("identity");
        assert_ne!(other.public_bytes(), state.identity.public_bytes());
        Device {
            id: Device::id_for(&other.public_key()),
            name: "desk".into(),
            public_key: other.public_key(),
            paired_ms: 0,
            last_seen_ms: None,
            revoked_ms: None,
            requires_user_verification: false,
            last_path: None,
            companion_key: None,
            retired_companion_keys: Vec::new(),
            addresses: Vec::new(),
            relay,
            forgotten_ms: None,
            told_ms: None,
            learned: false,
            introduced_by: None,
            allowed_ms: None,
            share: Default::default(),
        }
    }

    #[test]
    fn the_relay_another_computer_named_is_never_dialled() {
        // No relay here: the one the other computer named is refused, and
        // says why, without a connection to it.
        let (state, dir) = crate::serve::tests::a_state("dial-their-relay");
        let (theirs, url) = counting();
        let r = open(&state, &a_computer(&state, Some(url.clone())), Via::Relay);
        assert!(r.as_ref().err().is_some_and(|e| e.contains("not dialled")), "{:?}", r.err());
        assert_eq!(theirs.load(std::sync::atomic::Ordering::SeqCst), 0, "the relay the other computer named was dialled");
        let _ = std::fs::remove_dir_all(dir);

        // A relay here: that one is dialled, and still never theirs.
        let own = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let own_url = format!("ws://{}", own.local_addr().expect("addr"));
        let dir = std::env::temp_dir().join(format!("rime-dial-own-relay-{}-{}", std::process::id(), rime_remote_core::now_ms()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let state = State::new(
            rime_remote_core::identity::Identity::generate().expect("identity"),
            "test-machine".into(),
            47717,
            std::net::IpAddr::from([0u8; 16]),
            Some(own_url),
            dir.join("devices.json"),
            crate::serve::PING_INTERVAL,
        )
        .expect("state");
        // Accept and hang up, so the dial fails at once rather than waiting.
        let reached = std::thread::spawn(move || own.accept().is_ok());
        let r = open(&state, &a_computer(&state, Some(url)), Via::Relay);
        assert!(r.is_err(), "a hung-up relay answered");
        assert!(reached.join().expect("thread"), "this computer's own relay was not dialled");
        assert_eq!(theirs.load(std::sync::atomic::Ordering::SeqCst), 0, "the relay the other computer named was dialled");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn what_another_computer_says_is_printed_without_control_characters() {
        assert_eq!(printable("desk\u{1b}[2J\nok"), "desk[2Jok");
        assert_eq!(printable(&"x".repeat(500)).len(), 200);
    }
}
