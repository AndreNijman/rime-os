//! The encrypted channel, and the two handshakes that open one.
//!
//! ## Which library, and what "audited" is worth here
//!
//! [`snow`] is the reference Rust implementation of the Noise Protocol
//! Framework, over RustCrypto's ChaCha20-Poly1305 and BLAKE2s and
//! dalek-cryptography's X25519. Noise itself has been formally analysed
//! several times and is what WireGuard is built on; the primitives underneath
//! have been audited independently. `snow` has no standalone audit of its own,
//! and P1-051's "audited standard cryptographic libraries" should be read as
//! *standard, widely deployed and not written here* rather than as a
//! certificate. Writing the handshake by hand out of the same primitives would
//! be strictly worse and is the alternative this rejects.
//!
//! ## Two patterns, for two different situations
//!
//! **`Noise_NK` for pairing.** The device has just scanned a QR code, so it
//! knows the desktop's static public key and the desktop knows nothing about
//! the device. NK is exactly that: responder static known to the initiator,
//! initiator anonymous. The device sends its own static key and the one-time
//! pairing token *inside* the encrypted handshake payload, so neither is on
//! the wire in the clear and neither is replayable against a different
//! desktop — the QR key is mixed into the handshake hash.
//!
//! **`Noise_IK` afterwards.** Both statics are known, and IK transmits the
//! initiator's static encrypted to the responder's. Two consequences that
//! matter: a relay operator carrying the bytes never learns which device is
//! connecting, and the responder identifies the device by decrypting rather
//! than by being told — so there is no plaintext device id for anyone to
//! correlate across connections. This is WireGuard's pattern for the same
//! reasons.
//!
//! `XX` is not used for a phone. It would let a device connect without
//! knowing the desktop's key in advance, and "the device does not need to know
//! who it is talking to" is the property that makes a man in the middle
//! possible. The QR code is the out-of-band authenticated channel, and
//! refusing to work without it is the design.
//!
//! **`Noise_XX` for pairing two computers, and only there.** Two computers
//! have no camera between them, so neither knows the other's key, and XX is
//! the pattern for exactly that. On its own it would be the man-in-the-middle
//! hole the paragraph above describes. It is never used on its own: the
//! out-of-band channel is a six-digit code a person reads off one screen and
//! types into the other, run through SPAKE2 inside the finished XX session
//! and confirmed by a MAC over this handshake's hash
//! ([`crate::computer`]). A different prologue from the other two patterns,
//! so an XX transcript can never be mistaken for, or spliced into, an NK or
//! IK one. Once paired, the two computers talk over `Noise_IK` like a phone.
//!
//! ## The prologue
//!
//! Both handshakes mix a prologue: the protocol version and, for pairing, the
//! desktop's own public key. Noise binds the prologue into the handshake
//! hash, so two ends that disagree about either simply fail to complete —
//! rather than completing and then discovering they cannot parse each other,
//! which is how a downgrade gets negotiated.

use snow::params::{DHChoice, NoiseParams};
use snow::resolvers::{CryptoResolver, DefaultResolver};
use snow::{Builder, HandshakeState, TransportState};

/// The cipher suite, for both patterns.
///
/// ChaCha20-Poly1305 rather than AES-GCM because a phone and a laptop do not
/// reliably both have AES hardware, and ChaCha is constant-time in software
/// where AES is not. BLAKE2s rather than SHA-256 for the same reason on the
/// same devices.
pub const PAIRING_PATTERN: &str = "Noise_NK_25519_ChaChaPoly_BLAKE2s";
pub const SESSION_PATTERN: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2s";
/// Pairing two computers with a code; see the module note and
/// [`crate::computer`].
pub const COMPUTER_PAIRING_PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";

/// Why a channel could not be opened or used.
#[derive(Debug)]
pub enum NoiseError {
    /// The library refused something: a bad key, a failed decryption, a
    /// handshake message out of order.
    ///
    /// Deliberately one variant with the library's own words rather than a
    /// taxonomy of our own. Every one of them means the same thing to a
    /// caller — this peer is not who it claims to be, or the bytes have been
    /// tampered with — and a caller that branched on which would be making a
    /// distinction the security model does not have.
    Crypto(String),
    /// A message larger than Noise can carry.
    TooLong(usize),
    /// A handshake payload that is not the shape it must be.
    Malformed(String),
}

impl std::fmt::Display for NoiseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NoiseError::Crypto(w) => write!(f, "{w}"),
            NoiseError::TooLong(n) => write!(f, "a message of {n} bytes exceeds 65535"),
            NoiseError::Malformed(w) => write!(f, "{w}"),
        }
    }
}

impl std::error::Error for NoiseError {}

fn crypto(e: impl std::fmt::Display) -> NoiseError {
    NoiseError::Crypto(e.to_string())
}

/// The largest Noise message, including its authentication tag.
pub const MAX_MESSAGE: usize = 65535;

/// A builder for the session pattern, used for key generation.
///
/// The pattern does not matter for `generate_keypair` — every pattern here
/// uses the same curve — but a `Builder` needs one, and naming the session
/// pattern says which curve the key is for.
pub fn builder() -> Builder<'static> {
    Builder::new(params(SESSION_PATTERN))
}

fn params(s: &str) -> NoiseParams {
    // The two pattern strings are constants in this file and are the only
    // values ever passed. A parse failure would be a typo in a constant,
    // which is a bug rather than a runtime condition — and returning a
    // `Result` for it would push an impossible error onto every caller.
    s.parse().expect("a pattern constant does not parse")
}

/// The X25519 public key for a secret key.
pub fn public_from_secret(secret: &[u8; 32]) -> Result<[u8; 32], NoiseError> {
    // Derived through the same Diffie-Hellman implementation the handshake
    // itself uses, rather than by pulling in a second X25519 crate and
    // trusting the two to agree. If they ever did not, every handshake would
    // fail with an error about the library and nothing would point here.
    let mut dh = DefaultResolver
        .resolve_dh(&DHChoice::Curve25519)
        .ok_or_else(|| NoiseError::Crypto("this build has no X25519 implementation".into()))?;
    dh.set(secret);
    dh.pubkey()
        .try_into()
        .map_err(|_| NoiseError::Malformed("a derived public key is not 32 bytes".into()))
}

/// Everything both patterns agree on before a byte is sent.
///
/// The version is in here rather than in a frame, because a frame is only
/// readable after the handshake and a version mismatch has to be catchable
/// before that. Noise binds it into the handshake hash: two ends that
/// disagree do not complete.
pub fn prologue(version: u32) -> Vec<u8> {
    let mut p = b"apex-remote/".to_vec();  // rime-rename: keep (Noise prologue; paired phones hash it into every handshake)
    p.extend_from_slice(version.to_string().as_bytes());
    p
}

/// The prologue of a computer pairing: its own, so an XX transcript is never
/// one an NK or IK peer would complete.
pub fn computer_prologue(version: u32) -> Vec<u8> {
    let mut p = b"rime-computer-pair/".to_vec();
    p.extend_from_slice(version.to_string().as_bytes());
    p
}

/// One side of a handshake in progress.
pub struct Handshake {
    state: HandshakeState,
}

impl Handshake {
    /// The device's side of pairing: it knows the desktop's key from the QR.
    pub fn pairing_initiator(
        desktop_public: &[u8; 32],
        version: u32,
    ) -> Result<Handshake, NoiseError> {
        // The prologue is borrowed by the builder, so it has to outlive the
        // chain. A `&prologue(version)` inline compiles nowhere and would be
        // a temporary the builder still held.
        let prologue = prologue(version);
        let state = Builder::new(params(PAIRING_PATTERN))
            .prologue(&prologue)
            .remote_public_key(desktop_public)
            .build_initiator()
            .map_err(crypto)?;
        Ok(Handshake { state })
    }

    /// The desktop's side of pairing.
    pub fn pairing_responder(secret: &[u8; 32], version: u32) -> Result<Handshake, NoiseError> {
        let prologue = prologue(version);
        let state = Builder::new(params(PAIRING_PATTERN))
            .prologue(&prologue)
            .local_private_key(secret)
            .build_responder()
            .map_err(crypto)?;
        Ok(Handshake { state })
    }

    /// A paired device opening a session.
    pub fn session_initiator(
        device_secret: &[u8; 32],
        desktop_public: &[u8; 32],
        version: u32,
    ) -> Result<Handshake, NoiseError> {
        let prologue = prologue(version);
        let state = Builder::new(params(SESSION_PATTERN))
            .prologue(&prologue)
            .local_private_key(device_secret)
            .remote_public_key(desktop_public)
            .build_initiator()
            .map_err(crypto)?;
        Ok(Handshake { state })
    }

    /// The desktop accepting a session from a device it has not yet
    /// identified.
    ///
    /// No remote key is supplied, which is the whole point of IK: the device's
    /// static arrives encrypted in the first message and
    /// [`Handshake::remote_static`] reads it out afterwards. The caller then
    /// looks it up in the device store. A design that took the device id from
    /// the client and used it to pick a key would be authenticating on
    /// something the client chose.
    pub fn session_responder(secret: &[u8; 32], version: u32) -> Result<Handshake, NoiseError> {
        let prologue = prologue(version);
        let state = Builder::new(params(SESSION_PATTERN))
            .prologue(&prologue)
            .local_private_key(secret)
            .build_responder()
            .map_err(crypto)?;
        Ok(Handshake { state })
    }

    /// The computer typing a code: it dials, and knows nothing of the other
    /// computer's key until the handshake gives it one.
    pub fn computer_pairing_initiator(secret: &[u8; 32], version: u32) -> Result<Handshake, NoiseError> {
        let prologue = computer_prologue(version);
        let state = Builder::new(params(COMPUTER_PAIRING_PATTERN))
            .prologue(&prologue)
            .local_private_key(secret)
            .build_initiator()
            .map_err(crypto)?;
        Ok(Handshake { state })
    }

    /// The computer showing a code.
    pub fn computer_pairing_responder(secret: &[u8; 32], version: u32) -> Result<Handshake, NoiseError> {
        let prologue = computer_prologue(version);
        let state = Builder::new(params(COMPUTER_PAIRING_PATTERN))
            .prologue(&prologue)
            .local_private_key(secret)
            .build_responder()
            .map_err(crypto)?;
        Ok(Handshake { state })
    }

    /// The handshake hash: a digest of everything both ends sent and mixed,
    /// statics and prologue included. Two ends with a man in the middle hold
    /// two different ones. Read before [`Handshake::into_transport`], which
    /// gives it up.
    pub fn handshake_hash(&self) -> Vec<u8> {
        self.state.get_handshake_hash().to_vec()
    }

    /// Write the next handshake message, carrying `payload`.
    pub fn write(&mut self, payload: &[u8]) -> Result<Vec<u8>, NoiseError> {
        let mut buf = vec![0u8; MAX_MESSAGE];
        let n = self.state.write_message(payload, &mut buf).map_err(crypto)?;
        buf.truncate(n);
        Ok(buf)
    }

    /// Read the next handshake message, returning its payload.
    pub fn read(&mut self, message: &[u8]) -> Result<Vec<u8>, NoiseError> {
        if message.len() > MAX_MESSAGE {
            return Err(NoiseError::TooLong(message.len()));
        }
        let mut buf = vec![0u8; MAX_MESSAGE];
        let n = self.state.read_message(message, &mut buf).map_err(crypto)?;
        buf.truncate(n);
        Ok(buf)
    }

    /// Whether the handshake is finished.
    pub fn is_done(&self) -> bool {
        self.state.is_handshake_finished()
    }

    /// The peer's static public key, once the pattern has transmitted one.
    ///
    /// `None` for `NK`, where the initiator has no static at all — the
    /// device's key is in the *payload* there, because pairing is the moment
    /// it becomes known and there is nothing yet to authenticate it against.
    pub fn remote_static(&self) -> Option<[u8; 32]> {
        self.state
            .get_remote_static()
            .and_then(|k| k.try_into().ok())
    }

    /// Turn a finished handshake into a transport channel.
    pub fn into_transport(self) -> Result<Channel, NoiseError> {
        Ok(Channel {
            state: self.state.into_transport_mode().map_err(crypto)?,
        })
    }
}

/// An encrypted, authenticated, ordered channel.
///
/// One `Channel` per connection, carrying every multiplexed
/// [`crate::wire::Frame`]. The nonce is the message counter and neither side
/// chooses it, so a replayed or reordered message does not decrypt: that is
/// the property the whole multiplexing layer above rests on, because a frame
/// arriving twice would be a keystroke arriving twice.
pub struct Channel {
    state: TransportState,
}

impl Channel {
    /// Encrypt one plaintext message.
    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, NoiseError> {
        if plaintext.len() + 16 > MAX_MESSAGE {
            return Err(NoiseError::TooLong(plaintext.len()));
        }
        let mut buf = vec![0u8; MAX_MESSAGE];
        let n = self.state.write_message(plaintext, &mut buf).map_err(crypto)?;
        buf.truncate(n);
        Ok(buf)
    }

    /// Decrypt one message, or fail.
    pub fn open(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, NoiseError> {
        if ciphertext.len() > MAX_MESSAGE {
            return Err(NoiseError::TooLong(ciphertext.len()));
        }
        let mut buf = vec![0u8; MAX_MESSAGE];
        let n = self.state.read_message(ciphertext, &mut buf).map_err(crypto)?;
        buf.truncate(n);
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::REMOTE_PROTOCOL_VERSION as V;

    fn keypair() -> ([u8; 32], [u8; 32]) {
        let kp = builder().generate_keypair().expect("keypair");
        (
            kp.private.as_slice().try_into().expect("32"),
            kp.public.as_slice().try_into().expect("32"),
        )
    }

    /// A completed pairing handshake, as both sides.
    fn pair_up(v_dev: u32, v_desk: u32) -> Result<(Channel, Channel, [u8; 32]), NoiseError> {
        let (desk_sec, desk_pub) = keypair();
        let (dev_sec, dev_pub) = keypair();
        let mut dev = Handshake::pairing_initiator(&desk_pub, v_dev)?;
        let mut desk = Handshake::pairing_responder(&desk_sec, v_desk)?;
        // The device's static key travels in the payload: NK has no slot for
        // an initiator static, and pairing is the moment the key becomes
        // known.
        let m1 = dev.write(&dev_pub)?;
        let got = desk.read(&m1)?;
        assert_eq!(got, dev_pub, "the device key did not survive the handshake");
        let m2 = desk.write(b"paired")?;
        assert_eq!(dev.read(&m2)?, b"paired");
        let _ = dev_sec;
        Ok((dev.into_transport()?, desk.into_transport()?, dev_pub))
    }

    #[test]
    fn a_pairing_handshake_completes_and_the_channel_carries_traffic() {
        let (mut dev, mut desk, _) = pair_up(V, V).expect("handshake");
        let sealed = dev.seal(br#"{"cmd":"list"}"#).expect("seal");
        assert_ne!(sealed, br#"{"cmd":"list"}"#, "the channel is not encrypting");
        assert_eq!(desk.open(&sealed).expect("open"), br#"{"cmd":"list"}"#);
        // And back.
        let reply = desk.seal(b"{\"reply\":\"sessions\"}").expect("seal");
        assert_eq!(dev.open(&reply).expect("open"), b"{\"reply\":\"sessions\"}");
    }

    #[test]
    fn a_device_that_pins_the_wrong_desktop_key_never_completes() {
        // The property the QR code buys. A man in the middle presenting its
        // own key cannot make the handshake finish against the real desktop,
        // and the device finds out on the first message rather than after
        // sending anything sensitive.
        let (desk_sec, _) = keypair();
        let (_, wrong_pub) = keypair();
        let mut dev = Handshake::pairing_initiator(&wrong_pub, V).expect("initiator");
        let mut desk = Handshake::pairing_responder(&desk_sec, V).expect("responder");
        let m1 = dev.write(b"whatever").expect("write");
        assert!(desk.read(&m1).is_err(), "a wrong pinned key completed");
    }

    #[test]
    fn two_ends_on_different_protocol_versions_do_not_complete() {
        // The prologue is bound into the handshake hash, so this fails during
        // the handshake rather than after it. A version check done in the
        // first frame instead would be a check the two ends had already
        // agreed a key to disagree about.
        // `expect_err` is unavailable: `Channel` has no `Debug`, so nothing
        // can print a live key schedule by accident. Matched instead.
        match pair_up(V, V + 1) {
            Err(NoiseError::Crypto(_)) => {}
            Err(other) => panic!("wrong error: {other}"),
            Ok(_) => panic!("two ends on different protocol versions completed a handshake"),
        }
    }

    #[test]
    fn a_session_handshake_tells_the_desktop_which_device_it_is() {
        // IK's whole reason for being here: the responder learns the
        // initiator's static from the handshake rather than from anything the
        // client asserts, and can then look it up.
        let (desk_sec, desk_pub) = keypair();
        let (dev_sec, dev_pub) = keypair();
        let mut dev = Handshake::session_initiator(&dev_sec, &desk_pub, V).expect("initiator");
        let mut desk = Handshake::session_responder(&desk_sec, V).expect("responder");
        let m1 = dev.write(b"").expect("write");
        // Not in the clear: this is what stops a relay operator correlating a
        // device across connections.
        assert!(
            !m1.windows(32).any(|w| w == dev_pub),
            "the device's static key is on the wire in plaintext"
        );
        desk.read(&m1).expect("read");
        assert_eq!(
            desk.remote_static(),
            Some(dev_pub),
            "the responder did not learn the device key"
        );
        let m2 = desk.write(b"").expect("write");
        dev.read(&m2).expect("read");
        assert!(dev.is_done() && desk.is_done());
        let (mut a, mut b) = (
            dev.into_transport().expect("transport"),
            desk.into_transport().expect("transport"),
        );
        let sealed = a.seal(b"hello").expect("seal");
        assert_eq!(b.open(&sealed).expect("open"), b"hello");
    }

    #[test]
    fn a_device_with_the_wrong_key_cannot_open_a_session() {
        // The property revocation rests on from the other side: possessing a
        // device *id*, or any other key, is not possessing the device's key.
        let (desk_sec, desk_pub) = keypair();
        let (impostor_sec, _) = keypair();
        let mut dev = Handshake::session_initiator(&impostor_sec, &desk_pub, V).expect("init");
        let mut desk = Handshake::session_responder(&desk_sec, V).expect("resp");
        let m1 = dev.write(b"").expect("write");
        // The handshake itself completes — IK authenticates the initiator to
        // the responder by *identifying* it, not by approving it — and the
        // static it yields is the impostor's, which the device store will not
        // find. That is the check, and this asserts where it lives.
        desk.read(&m1).expect("read");
        let learned = desk.remote_static().expect("a static");
        assert_ne!(learned, desk_pub);
        let store = crate::device::DeviceStore::default();
        assert!(
            store.authenticate(&crate::b64_encode(&learned)).is_none(),
            "an unpaired key authenticated"
        );
    }

    #[test]
    fn a_tampered_message_does_not_decrypt() {
        let (mut dev, mut desk, _) = pair_up(V, V).expect("handshake");
        let mut sealed = dev.seal(b"the original bytes").expect("seal");
        let last = sealed.len() - 1;
        sealed[last] ^= 0x01;
        assert!(desk.open(&sealed).is_err(), "a flipped bit decrypted");
    }

    #[test]
    fn a_replayed_message_does_not_decrypt() {
        // The nonce is the message counter and neither side chooses it, which
        // is what makes the multiplexing above safe: a replayed frame would
        // be a keystroke delivered twice.
        let (mut dev, mut desk, _) = pair_up(V, V).expect("handshake");
        let first = dev.seal(b"ls -la\r").expect("seal");
        assert_eq!(desk.open(&first).expect("open"), b"ls -la\r");
        assert!(desk.open(&first).is_err(), "a replayed frame decrypted");
    }

    #[test]
    fn a_message_delivered_out_of_order_does_not_decrypt() {
        // Measured rather than assumed, and the measurement corrected a
        // guess: the receiving nonce advances only on a *successful* decrypt,
        // so a message arriving early is refused and the channel stays where
        // it was. An attacker therefore cannot make the far end skip a frame
        // by injecting a later one — which is the property that matters, and
        // it is stronger than "the channel breaks".
        //
        // What it means for the transport: delivery must be ordered. TCP is,
        // and a relay that copies bytes is. A datagram transport would need
        // its own sequencing above this, and none is used.
        let (mut dev, mut desk, _) = pair_up(V, V).expect("handshake");
        let a = dev.seal(b"first").expect("seal");
        let b = dev.seal(b"second").expect("seal");
        assert!(desk.open(&b).is_err(), "a message from the future decrypted");
        assert_eq!(
            desk.open(&a).expect("the expected message still opens"),
            b"first",
            "the refusal moved the channel on"
        );
        assert_eq!(desk.open(&b).expect("and then the next one"), b"second");
    }

    #[test]
    fn a_message_too_long_for_noise_is_refused_rather_than_truncated() {
        let (mut dev, _, _) = pair_up(V, V).expect("handshake");
        match dev.seal(&vec![0u8; MAX_MESSAGE]) {
            Err(NoiseError::TooLong(_)) => {}
            Err(other) => panic!("wrong error: {other}"),
            Ok(_) => panic!("an over-long message was sealed rather than refused"),
        }
        // A frame at the wire layer's own limit does fit, which is the number
        // that constant was chosen to satisfy.
        assert!(dev.seal(&vec![0u8; crate::wire::MAX_PAYLOAD + 5]).is_ok());
    }

    #[test]
    fn a_derived_public_key_matches_the_one_the_handshake_uses() {
        let (sec, public) = keypair();
        assert_eq!(public_from_secret(&sec).expect("derive"), public);
    }
}
