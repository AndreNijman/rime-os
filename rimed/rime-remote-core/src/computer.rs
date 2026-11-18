//! Pairing two computers with a short code.
//!
//! A phone pairs by scanning a QR code that carries this machine's key, so it
//! knows who it is talking to before it says a word. Two computers have no
//! camera between them: one shows a six-digit code, a person types it into the
//! other, and neither knows the other's key until the handshake gives it one.
//!
//! ## Why the code is never a key
//!
//! Six digits is a million values. A side that sent anything computed from the
//! code alone — an HMAC of the code over the keys, say — would hand whoever
//! received it (a forged mDNS answer, a forged dialler) a value to try all
//! million codes against offline, in milliseconds, and it would then pair
//! with the real computer. So the code only ever goes into **SPAKE2**
//! (RustCrypto `spake2`, Ed25519 group), which turns a low-entropy password
//! into a strong shared key and gives an active attacker exactly one guess
//! per run, with nothing to test further guesses against.
//!
//! ## What the code is bound to
//!
//! SPAKE2 runs INSIDE a finished `Noise_XX` session (see `noise.rs`), with the
//! two static keys that session proved as its identities, taken from the
//! handshake and never from a payload. Each side then sends a confirmation
//! MAC: HMAC-SHA256 under the SPAKE2 key over a label, its role, and the
//! Noise handshake hash. Two ends with someone in the middle hold two
//! different handshake hashes, so a relayed exchange fails confirmation even
//! with the right code. A side that confirms has proved it knows the code AND
//! holds the static key it showed, which is what gets stored.
//!
//! ## The order, and why it is this order
//!
//! The typing side (the dialler) sends its MAC first; the showing side checks
//! it before sending anything that depends on the SPAKE2 key. So a dialler
//! that guessed gets nothing back to test its guess against, and the showing
//! side's code is spent by the attempt (the daemon takes it out of memory the
//! moment the XX handshake finishes, before any of this). A forged showing
//! side does get the dialler's MAC, which tests one guess, and the person
//! typed that code once.
//!
//! Then the two sides tell each other their names and addresses, only after
//! both MACs have verified, and the showing side stores the pairing before it
//! says `ok`. The dialler stores it on `ok`.

use std::io::{Read, Write};

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use spake2::{Ed25519Group, Identity as SpakeIdentity, Password, Spake2};

use crate::noise::{Channel, Handshake, MAX_MESSAGE};

/// How long a code is accepted: two minutes.
pub const CODE_TTL_MS: u64 = 2 * 60 * 1000;

/// How many digits a code has.
pub const CODE_DIGITS: usize = 6;

/// What the confirmation MAC covers before the role and the handshake hash.
/// Versioned, so a later exchange can never be confused with this one.
const CONFIRM_LABEL: &[u8] = b"rime-computer-pair/1/confirm";

/// Longest list of addresses a computer may tell the other.
pub const MAX_ADDRESSES: usize = 8;

/// Longest single address, and longest relay URL, kept from the other side.
const MAX_ADDRESS: usize = 64;
const MAX_RELAY: usize = 512;

/// The showing side's code, and until when it is accepted.
///
/// Held in memory by `rime-remoted` and never written down: a code that
/// survived a restart would be one the person did not ask to survive.
#[derive(Debug, Clone)]
pub struct CodeOffer {
    code: String,
    expires_ms: u64,
}

impl CodeOffer {
    /// A fresh code from the thread RNG (seeded from the OS, reseeding): a
    /// value that lives two minutes and is used once.
    pub fn new(now_ms: u64) -> CodeOffer {
        use rand::Rng;
        let n: u32 = rand::rng().random_range(0..1_000_000);
        CodeOffer {
            code: format!("{n:06}"),
            expires_ms: now_ms + CODE_TTL_MS,
        }
    }

    /// A code with a known value, for tests on both sides of the exchange.
    pub fn with_code(code: &str, now_ms: u64) -> CodeOffer {
        CodeOffer {
            code: code.to_string(),
            expires_ms: now_ms + CODE_TTL_MS,
        }
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn expires_ms(&self) -> u64 {
        self.expires_ms
    }

    /// Whether it would still be accepted at `now_ms`.
    pub fn is_live(&self, now_ms: u64) -> bool {
        now_ms < self.expires_ms
    }
}

/// A code as a person typed it: spaces and dashes dropped, then exactly six
/// ASCII digits.
pub fn normalize_code(typed: &str) -> Result<String, PairError> {
    let code: String = typed.chars().filter(|c| !c.is_whitespace() && *c != '-').collect();
    if code.len() == CODE_DIGITS && code.bytes().all(|b| b.is_ascii_digit()) {
        Ok(code)
    } else {
        Err(PairError::BadCode(format!(
            "a code is {CODE_DIGITS} digits, as the other computer shows it"
        )))
    }
}

/// Which end of the exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The computer the code is typed into. It dials.
    Dialer,
    /// The computer showing the code. It answers.
    Shower,
}

impl Role {
    fn byte(self) -> u8 {
        match self {
            Role::Dialer => b'D',
            Role::Shower => b'S',
        }
    }
}

/// One side of SPAKE2, started.
pub struct Pake {
    state: Spake2<Ed25519Group>,
}

impl Pake {
    /// Start, with the code and both static keys as SPAKE2's identities. The
    /// dialler is SPAKE2's side A and the showing computer side B, whichever
    /// end calls this.
    pub fn start(role: Role, code: &str, dialer_key: &[u8; 32], shower_key: &[u8; 32]) -> (Pake, Vec<u8>) {
        let pw = Password::new(code.as_bytes());
        let a = SpakeIdentity::new(dialer_key);
        let b = SpakeIdentity::new(shower_key);
        let (state, msg) = match role {
            Role::Dialer => Spake2::<Ed25519Group>::start_a(&pw, &a, &b),
            Role::Shower => Spake2::<Ed25519Group>::start_b(&pw, &a, &b),
        };
        (Pake { state }, msg)
    }

    /// Finish with the other side's message. A wrong code does NOT fail here
    /// (SPAKE2 cannot tell): it yields a different key, and only the
    /// confirmation MAC finds out.
    pub fn finish(self, theirs: &[u8], handshake_hash: &[u8]) -> Result<Confirmation, PairError> {
        let key = self
            .state
            .finish(theirs)
            .map_err(|e| PairError::Protocol(format!("the other computer's SPAKE2 message: {e}")))?;
        Ok(Confirmation {
            key,
            hash: handshake_hash.to_vec(),
        })
    }
}

/// The SPAKE2 key and the handshake hash it confirms.
pub struct Confirmation {
    key: Vec<u8>,
    hash: Vec<u8>,
}

impl Confirmation {
    fn hmac(&self, role: Role) -> Hmac<Sha256> {
        let mut m = <Hmac<Sha256> as Mac>::new_from_slice(&self.key).expect("HMAC takes a key of any length");
        m.update(CONFIRM_LABEL);
        m.update(&[role.byte()]);
        m.update(&self.hash);
        m
    }

    /// This side's MAC.
    pub fn mac(&self, role: Role) -> Vec<u8> {
        self.hmac(role).finalize().into_bytes().to_vec()
    }

    /// Whether `mac` is the other side's, in constant time.
    pub fn verify(&self, role: Role, mac: &[u8]) -> bool {
        self.hmac(role).verify_slice(mac).is_ok()
    }
}

/// What one computer tells the other once both have confirmed: its name,
/// where it can be dialled on a LAN, and the relay it waits on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct About {
    pub name: String,
    #[serde(default)]
    pub lan: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<String>,
    /// The port it listens on, so the other side can dial it back at the
    /// address the connection came from even when `lan` is empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

impl About {
    /// The other side's account of itself, bounded and checked: a name that
    /// is safe to print (the device-name rule), at most [`MAX_ADDRESSES`]
    /// addresses that each parse as `host:port`, and a relay that parses as
    /// one. Anything else is refused, not trimmed.
    pub fn checked(self) -> Result<About, PairError> {
        let name = crate::device::check_name(&self.name).map_err(|e| PairError::Protocol(e.to_string()))?;
        if self.lan.len() > MAX_ADDRESSES {
            return Err(PairError::Protocol(format!(
                "the other computer listed {} addresses; at most {MAX_ADDRESSES} are kept",
                self.lan.len()
            )));
        }
        for a in &self.lan {
            if a.len() > MAX_ADDRESS || a.parse::<std::net::SocketAddr>().is_err() {
                return Err(PairError::Protocol(format!(
                    "the other computer listed {:?}, which is not an address and port",
                    a.escape_debug().to_string()
                )));
            }
        }
        if let Some(r) = &self.relay {
            if r.len() > MAX_RELAY || crate::relay::Endpoint::parse(r).is_err() {
                return Err(PairError::Protocol("the other computer named a relay that is not one".into()));
            }
        }
        Ok(About {
            name,
            lan: self.lan,
            relay: self.relay,
            port: self.port,
        })
    }
}

/// One message of the exchange, inside the Noise session. Each step fills
/// one or two fields.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Step {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    spake: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    confirm: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    about: Option<About>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ok: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Why a computer pairing did not complete.
#[derive(Debug)]
pub enum PairError {
    /// The other computer is not showing a code (no add screen open, the code
    /// expired, or it was already spent), so it ended the connection before
    /// the handshake finished.
    NoCode,
    /// The confirmation failed: the code was wrong, or someone is in the
    /// middle. The showing side's code is spent either way.
    WrongCode,
    /// The other computer holds THIS computer's own key: a copied home.
    OwnKey,
    /// The other side refused for a reason it gave, or ended the exchange.
    Refused(String),
    /// What was typed is not a code.
    BadCode(String),
    Protocol(String),
    Io(std::io::Error),
}

impl std::fmt::Display for PairError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PairError::NoCode => write!(
                f,
                "that computer is not showing a code: open \"Add a computer\" there \
                 (`rime remote computers add --show-code`) and type the code it shows; a code \
                 lasts two minutes and is used once"
            ),
            PairError::WrongCode => write!(
                f,
                "the code did not match, so the two computers did not pair; that code is now \
                 used up, so show a new one and try again"
            ),
            PairError::OwnKey => write!(
                f,
                "the other computer has this computer's own Rime Remote key, which happens when a \
                 home folder is copied from one to the other; it was refused"
            ),
            PairError::Refused(why) => write!(f, "the other computer refused: {why}"),
            PairError::BadCode(why) => write!(f, "{why}"),
            PairError::Protocol(why) => write!(f, "{why}"),
            PairError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for PairError {}

impl From<std::io::Error> for PairError {
    fn from(e: std::io::Error) -> PairError {
        PairError::Io(e)
    }
}

/// A finished pairing: the other computer's static key, which the handshake
/// proved and the code confirmed, and what it said about itself.
#[derive(Debug, Clone)]
pub struct Paired {
    pub key: [u8; 32],
    pub about: About,
}

fn write_message(w: &mut impl Write, message: &[u8]) -> std::io::Result<()> {
    if message.len() > MAX_MESSAGE {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "message too long"));
    }
    let mut framed = Vec::with_capacity(4 + message.len());
    framed.extend_from_slice(&(message.len() as u32).to_be_bytes());
    framed.extend_from_slice(message);
    w.write_all(&framed)?;
    w.flush()
}

fn read_message(r: &mut impl Read) -> std::io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_MESSAGE {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "message too long"));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

fn noise(e: crate::noise::NoiseError) -> PairError {
    PairError::Protocol(format!("the pairing handshake did not complete: {e}"))
}

fn is_eof(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
    )
}

fn send(s: &mut impl Write, channel: &mut Channel, step: &Step) -> Result<(), PairError> {
    let plain = serde_json::to_vec(step).map_err(|e| PairError::Protocol(e.to_string()))?;
    let sealed = channel.seal(&plain).map_err(noise)?;
    write_message(s, &sealed)?;
    Ok(())
}

/// The next step, or `on_eof` when the other side has hung up.
fn recv(s: &mut impl Read, channel: &mut Channel, on_eof: impl FnOnce() -> PairError) -> Result<Step, PairError> {
    let sealed = match read_message(s) {
        Ok(m) => m,
        Err(e) if is_eof(&e) => return Err(on_eof()),
        Err(e) => return Err(PairError::Io(e)),
    };
    let plain = channel.open(&sealed).map_err(noise)?;
    serde_json::from_slice(&plain).map_err(|e| PairError::Protocol(format!("a pairing step is malformed: {e}")))
}

fn b64_field(v: Option<String>, what: &str) -> Result<Vec<u8>, PairError> {
    v.and_then(|t| crate::b64_decode(&t))
        .ok_or_else(|| PairError::Protocol(format!("the other computer sent no {what}")))
}

fn remote_key(hs: &Handshake) -> Result<[u8; 32], PairError> {
    hs.remote_static()
        .ok_or_else(|| PairError::Protocol("the other computer sent no static key".into()))
}

/// The computer the code is typed into. The caller has connected `stream` and
/// written the computer-pairing hello byte; this runs the rest.
///
/// `may_store` is asked as soon as the other computer has shown its key, so a
/// key this side would refuse to store (one it removed, one that is a phone)
/// ends the attempt before the other side stores anything.
pub fn dial<S: Read + Write>(
    stream: &mut S,
    own_secret: &[u8; 32],
    code: &str,
    about: &About,
    version: u32,
    may_store: impl FnOnce(&[u8; 32]) -> Result<(), PairError>,
) -> Result<Paired, PairError> {
    dial_checked(stream, own_secret, code, about, version, may_store, true)
}

/// [`dial`], with the copied-home check switchable so a test can reach the
/// showing side's own check. Never `false` outside tests.
fn dial_checked<S: Read + Write>(
    stream: &mut S,
    own_secret: &[u8; 32],
    code: &str,
    about: &About,
    version: u32,
    may_store: impl FnOnce(&[u8; 32]) -> Result<(), PairError>,
    refuse_own_key: bool,
) -> Result<Paired, PairError> {
    let code = normalize_code(code)?;
    let own = crate::noise::public_from_secret(own_secret).map_err(noise)?;
    let mut hs = Handshake::computer_pairing_initiator(own_secret, version).map_err(noise)?;
    write_message(stream, &hs.write(b"").map_err(noise)?)?;
    // The showing computer ends the connection here, before it shows its key,
    // unless it has a code open.
    let m2 = match read_message(stream) {
        Ok(m) => m,
        Err(e) if is_eof(&e) => return Err(PairError::NoCode),
        Err(e) => return Err(PairError::Io(e)),
    };
    hs.read(&m2).map_err(noise)?;
    let theirs = remote_key(&hs)?;
    if refuse_own_key && theirs == own {
        return Err(PairError::OwnKey);
    }
    may_store(&theirs)?;
    write_message(stream, &hs.write(b"").map_err(noise)?)?;
    let hash = hs.handshake_hash();
    let mut channel = hs.into_transport().map_err(noise)?;

    let (pake, msg) = Pake::start(Role::Dialer, &code, &own, &theirs);
    send(stream, &mut channel, &Step { spake: Some(crate::b64_encode(&msg)), ..Step::default() })?;
    let step = recv(stream, &mut channel, || {
        PairError::Refused("it ended the pairing before it started (no code, or the same key as this computer)".into())
    })?;
    let confirmation = pake.finish(&b64_field(step.spake, "SPAKE2 message")?, &hash)?;
    // Ours first: the showing side checks it before it sends anything that
    // depends on the key. See the module note.
    send(
        stream,
        &mut channel,
        &Step { confirm: Some(crate::b64_encode(&confirmation.mac(Role::Dialer))), ..Step::default() },
    )?;
    let step = recv(stream, &mut channel, || PairError::WrongCode)?;
    let mac = b64_field(step.confirm, "confirmation")?;
    if !confirmation.verify(Role::Shower, &mac) {
        return Err(PairError::WrongCode);
    }
    let their_about = step
        .about
        .ok_or_else(|| PairError::Protocol("the other computer did not say who it is".into()))?
        .checked()?;
    send(stream, &mut channel, &Step { about: Some(about.clone()), ..Step::default() })?;
    let step = recv(stream, &mut channel, || {
        PairError::Refused("it ended the pairing before it confirmed".into())
    })?;
    match step.ok {
        Some(true) => Ok(Paired { key: theirs, about: their_about }),
        _ => Err(PairError::Refused(step.error.unwrap_or_else(|| "no reason given".into()))),
    }
}

/// The computer showing the code. The caller has read the hello byte.
///
/// * `has_code` is asked after the first handshake message and before this
///   side shows its key: `false` ends the connection there, so a computer with
///   no add screen open never tells a prober its key.
/// * `spend` takes the code out of memory the moment the handshake finishes,
///   before anything else: one attempt per code whatever happens next.
///   `None` means it is gone (expired, closed, or taken by another attempt).
/// * `commit` stores the pairing, after both MACs have verified; an error is
///   told to the other side, which then stores nothing.
pub fn show<S: Read + Write>(
    stream: &mut S,
    own_secret: &[u8; 32],
    version: u32,
    has_code: impl FnOnce() -> bool,
    spend: impl FnOnce() -> Option<String>,
    about: &About,
    commit: impl FnOnce(&[u8; 32], &About) -> Result<(), String>,
) -> Result<Paired, PairError> {
    let own = crate::noise::public_from_secret(own_secret).map_err(noise)?;
    let mut hs = Handshake::computer_pairing_responder(own_secret, version).map_err(noise)?;
    let m1 = read_message(stream)?;
    hs.read(&m1).map_err(noise)?;
    if !has_code() {
        return Err(PairError::NoCode);
    }
    write_message(stream, &hs.write(b"").map_err(noise)?)?;
    let m3 = read_message(stream)?;
    hs.read(&m3).map_err(noise)?;
    let theirs = remote_key(&hs)?;
    // Spent now, before the key check or anything else, so no outcome of
    // this attempt leaves the code usable for another.
    let Some(code) = spend() else {
        return Err(PairError::NoCode);
    };
    if theirs == own {
        return Err(PairError::OwnKey);
    }
    let hash = hs.handshake_hash();
    let mut channel = hs.into_transport().map_err(noise)?;

    let step = recv(stream, &mut channel, || PairError::Refused("the other computer hung up".into()))?;
    let their_msg = b64_field(step.spake, "SPAKE2 message")?;
    let (pake, msg) = Pake::start(Role::Shower, &code, &theirs, &own);
    send(stream, &mut channel, &Step { spake: Some(crate::b64_encode(&msg)), ..Step::default() })?;
    let confirmation = pake.finish(&their_msg, &hash)?;
    let step = recv(stream, &mut channel, || PairError::Refused("the other computer hung up".into()))?;
    let mac = b64_field(step.confirm, "confirmation")?;
    if !confirmation.verify(Role::Dialer, &mac) {
        // Nothing goes back: a guess learns nothing it could test another
        // guess against.
        return Err(PairError::WrongCode);
    }
    send(
        stream,
        &mut channel,
        &Step {
            confirm: Some(crate::b64_encode(&confirmation.mac(Role::Shower))),
            about: Some(about.clone()),
            ..Step::default()
        },
    )?;
    let step = recv(stream, &mut channel, || PairError::Refused("the other computer hung up".into()))?;
    let their_about = step
        .about
        .ok_or_else(|| PairError::Protocol("the other computer did not say who it is".into()))?
        .checked()?;
    match commit(&theirs, &their_about) {
        Ok(()) => {
            send(stream, &mut channel, &Step { ok: Some(true), ..Step::default() })?;
            Ok(Paired { key: theirs, about: their_about })
        }
        Err(why) => {
            let _ = send(
                stream,
                &mut channel,
                &Step { ok: Some(false), error: Some(why.clone()), ..Step::default() },
            );
            Err(PairError::Refused(why))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    const V: u32 = crate::REMOTE_PROTOCOL_VERSION;

    fn secret() -> [u8; 32] {
        crate::identity::Identity::generate().expect("key").secret_bytes_for_handshake()
    }

    fn public(s: &[u8; 32]) -> [u8; 32] {
        crate::noise::public_from_secret(s).expect("public")
    }

    fn about(name: &str) -> About {
        About { name: name.into(), lan: vec!["192.168.1.9:7717".into()], relay: None, port: None }
    }

    struct Run {
        dialer: Result<Paired, PairError>,
        shower: Result<Paired, PairError>,
        commits: usize,
        spent: usize,
    }

    /// Both ends over a socketpair, each on its own thread.
    fn run(
        dialer_secret: [u8; 32],
        shower_secret: [u8; 32],
        typed: &str,
        shown: Option<&str>,
        refuse_own_key: bool,
    ) -> Run {
        let (mut a, mut b) = UnixStream::pair().expect("pair");
        for s in [&a, &b] {
            s.set_read_timeout(Some(std::time::Duration::from_secs(10))).ok();
        }
        let commits = Arc::new(AtomicUsize::new(0));
        let spent = Arc::new(AtomicUsize::new(0));
        let shown = shown.map(str::to_string);
        let (c, sp) = (Arc::clone(&commits), Arc::clone(&spent));
        let shower = std::thread::spawn(move || {
            let has = shown.is_some();
            let r = show(
                &mut b,
                &shower_secret,
                V,
                || has,
                || {
                    sp.fetch_add(1, Ordering::SeqCst);
                    shown
                },
                &about("desk"),
                |_, _| {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            );
            drop(b);
            r
        });
        let dialer = dial_checked(&mut a, &dialer_secret, typed, &about("laptop"), V, |_| Ok(()), refuse_own_key);
        drop(a);
        let shower = shower.join().expect("shower thread");
        Run { dialer, shower, commits: commits.load(Ordering::SeqCst), spent: spent.load(Ordering::SeqCst) }
    }

    #[test]
    fn the_right_code_pairs_both_ends_with_each_others_proven_key() {
        let (d, s) = (secret(), secret());
        let r = run(d, s, "123 456", Some("123456"), true);
        let dp = r.dialer.expect("dialer");
        let sp = r.shower.expect("shower");
        assert_eq!(dp.key, public(&s));
        assert_eq!(sp.key, public(&d));
        assert_eq!(dp.about.name, "desk");
        assert_eq!(sp.about.name, "laptop");
        assert_eq!((r.commits, r.spent), (1, 1));
    }

    #[test]
    fn a_wrong_code_pairs_neither_end_and_still_spends_the_code() {
        let r = run(secret(), secret(), "123457", Some("123456"), true);
        assert!(matches!(r.dialer, Err(PairError::WrongCode)), "{:?}", r.dialer);
        assert!(matches!(r.shower, Err(PairError::WrongCode)), "{:?}", r.shower);
        assert_eq!(r.commits, 0, "a wrong code was stored");
        assert_eq!(r.spent, 1, "the attempt did not spend the code");
    }

    #[test]
    fn with_no_code_open_the_showing_side_ends_before_it_shows_its_key() {
        let r = run(secret(), secret(), "123456", None, true);
        assert!(matches!(r.dialer, Err(PairError::NoCode)), "{:?}", r.dialer);
        assert!(matches!(r.shower, Err(PairError::NoCode)), "{:?}", r.shower);
        assert_eq!((r.commits, r.spent), (0, 0));
    }

    #[test]
    fn a_dialler_that_meets_its_own_key_refuses_before_it_sends_its_own() {
        let k = secret();
        let r = run(k, k, "123456", Some("123456"), true);
        assert!(matches!(r.dialer, Err(PairError::OwnKey)), "{:?}", r.dialer);
        assert!(r.shower.is_err());
        assert_eq!(r.commits, 0);
    }

    #[test]
    fn a_showing_side_that_meets_its_own_key_refuses_even_with_the_right_code() {
        // A dialler without its own check (a modified client): the showing
        // side's check is what stops it.
        let k = secret();
        let r = run(k, k, "123456", Some("123456"), false);
        assert!(matches!(r.shower, Err(PairError::OwnKey)), "{:?}", r.shower);
        assert!(r.dialer.is_err(), "{:?}", r.dialer);
        assert_eq!(r.commits, 0, "a copied home was paired");
        assert_eq!(r.spent, 1, "the copied-home attempt did not spend the code");
    }

    #[test]
    fn a_dialler_refuses_a_showing_side_that_does_not_know_the_code() {
        // A forged showing computer (a spoofed address): it guesses a code,
        // skips checking the dialler's MAC, and answers with its own MAC and a
        // name. The dialler must refuse before it says anything about itself.
        let (mut a, mut b) = UnixStream::pair().expect("pair");
        for s in [&a, &b] {
            s.set_read_timeout(Some(std::time::Duration::from_secs(10))).ok();
        }
        let forger = secret();
        let forged = std::thread::spawn(move || -> Option<Step> {
            let own = public(&forger);
            let mut hs = Handshake::computer_pairing_responder(&forger, V).ok()?;
            hs.read(&read_message(&mut b).ok()?).ok()?;
            write_message(&mut b, &hs.write(b"").ok()?).ok()?;
            hs.read(&read_message(&mut b).ok()?).ok()?;
            let theirs = hs.remote_static()?;
            let hash = hs.handshake_hash();
            let mut ch = hs.into_transport().ok()?;
            let step = recv(&mut b, &mut ch, || PairError::NoCode).ok()?;
            let (pake, msg) = Pake::start(Role::Shower, "000000", &theirs, &own);
            send(&mut b, &mut ch, &Step { spake: Some(crate::b64_encode(&msg)), ..Step::default() }).ok()?;
            let c = pake.finish(&b64_field(step.spake, "spake").ok()?, &hash).ok()?;
            let _dialers_mac = recv(&mut b, &mut ch, || PairError::NoCode).ok()?;
            send(
                &mut b,
                &mut ch,
                &Step { confirm: Some(crate::b64_encode(&c.mac(Role::Shower))), about: Some(about("forged")), ..Step::default() },
            )
            .ok()?;
            // Whatever the dialler sends next, if anything.
            recv(&mut b, &mut ch, || PairError::NoCode).ok()
        });
        let r = dial(&mut a, &secret(), "123456", &about("laptop"), V, |_| Ok(()));
        drop(a);
        assert!(matches!(r, Err(PairError::WrongCode)), "{r:?}");
        let next = forged.join().expect("forger thread");
        assert!(next.is_none(), "the dialler told a forged computer about itself: {next:?}");
    }

    #[test]
    fn the_confirmation_is_bound_to_the_handshake_and_the_role() {
        let (d, s) = (public(&secret()), public(&secret()));
        let (pa, ma) = Pake::start(Role::Dialer, "111111", &d, &s);
        let (pb, mb) = Pake::start(Role::Shower, "111111", &d, &s);
        let ca = pa.finish(&mb, b"hash-one").expect("a");
        let cb = pb.finish(&ma, b"hash-one").expect("b");
        assert!(cb.verify(Role::Dialer, &ca.mac(Role::Dialer)));
        assert!(ca.verify(Role::Shower, &cb.mac(Role::Shower)));
        // A MAC reflected back as the other role does not verify.
        assert!(!ca.verify(Role::Shower, &ca.mac(Role::Dialer)));
        // A different handshake (someone in the middle) does not verify.
        let (pa, ma) = Pake::start(Role::Dialer, "111111", &d, &s);
        let (pb, mb) = Pake::start(Role::Shower, "111111", &d, &s);
        let ca = pa.finish(&mb, b"hash-one").expect("a");
        let cb = pb.finish(&ma, b"hash-two").expect("b");
        assert!(!cb.verify(Role::Dialer, &ca.mac(Role::Dialer)));
        // Different identities (a swapped key) do not verify either.
        let other = public(&secret());
        let (pa, ma) = Pake::start(Role::Dialer, "111111", &d, &s);
        let (pb, mb) = Pake::start(Role::Shower, "111111", &other, &s);
        let ca = pa.finish(&mb, b"h").expect("a");
        let cb = pb.finish(&ma, b"h").expect("b");
        assert!(!cb.verify(Role::Dialer, &ca.mac(Role::Dialer)));
    }

    #[test]
    fn a_code_is_six_digits_however_it_was_typed_and_lives_two_minutes() {
        assert_eq!(normalize_code(" 123-456 ").expect("ok"), "123456");
        for bad in ["12345", "1234567", "12345a", "", "１２３４５６"] {
            assert!(normalize_code(bad).is_err(), "{bad:?}");
        }
        let o = CodeOffer::new(1_000);
        assert_eq!(o.code().len(), CODE_DIGITS);
        assert!(o.code().bytes().all(|b| b.is_ascii_digit()));
        assert!(o.is_live(1_000));
        assert!(o.is_live(1_000 + CODE_TTL_MS - 1));
        assert!(!o.is_live(1_000 + CODE_TTL_MS), "a code outlived two minutes");
        assert_eq!(CODE_TTL_MS, 120_000);
        let codes: std::collections::BTreeSet<String> = (0..50).map(|_| CodeOffer::new(0).code().to_string()).collect();
        assert!(codes.len() > 40, "codes repeat: {codes:?}");
    }

    #[test]
    fn what_the_other_computer_says_about_itself_is_checked() {
        assert!(about("desk").checked().is_ok());
        for bad in [
            About { name: "a\nb".into(), lan: vec![], relay: None, port: None },
            About { name: String::new(), lan: vec![], relay: None, port: None },
            About { name: "x".into(), lan: vec!["not an address".into()], relay: None, port: None },
            About { name: "x".into(), lan: vec!["10.0.0.1:1".into(); MAX_ADDRESSES + 1], relay: None, port: None },
            About { name: "x".into(), lan: vec![], relay: Some("ftp://nope".into()), port: None },
        ] {
            assert!(bad.clone().checked().is_err(), "{bad:?}");
        }
    }
}
