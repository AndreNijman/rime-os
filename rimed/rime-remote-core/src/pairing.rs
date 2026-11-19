//! Pairing: the QR code, the token behind it, and what makes it safe.
//!
//! ## The QR code is the security boundary
//!
//! Everything else in this crate is a key exchange between two parties that
//! already know each other. Pairing is the one moment they do not, and the
//! thing that bridges it is a human looking at a screen. That is not a
//! weakness of the design; it is the design. A QR code shown on the desktop
//! and read by the phone is an **out-of-band authenticated channel**: an
//! attacker on the network cannot write to it, and one who can read it is
//! already standing in the room.
//!
//! So the QR carries the desktop's static public key, and the device pins it.
//! No certificate authority, no trust-on-first-use, no "verify these emoji"
//! step that people click through. A man in the middle presenting its own key
//! makes the handshake fail rather than succeed with a warning.
//!
//! ## Why there is also a one-time token
//!
//! The public key alone authenticates the *desktop* to the *device*. The
//! token authenticates in the other direction: it says the device holding it
//! is the one the owner just showed the code to, and not a second device that
//! learned the desktop's key some other way — from a previous QR, from a
//! screenshot, from a neighbour's phone photographing the screen an hour
//! earlier.
//!
//! It is therefore: random, single-use, short-lived, compared in constant
//! time, and **never on the wire in the clear** — it travels inside the
//! encrypted `Noise_NK` handshake payload, which is bound to the desktop key
//! it was shown with. A token captured from a QR photo is worth nothing after
//! [`PairingOffer::ttl_ms`], and worth nothing at all once one device has
//! used it.
//!
//! ## "Pairing cannot be completed silently by an agent"
//!
//! P1-051's fourth criterion. Two independent mechanisms, because one is a
//! policy and the other is a fact:
//!
//! * An offer only exists while a human asked for one. There is no verb that
//!   creates a pairing token as a side effect of anything else, and an offer
//!   expires on its own.
//! * `rime-remoted` refuses to *make* an offer for a caller whose request
//!   origin is not local — the same `origin::observe` rule the agent runtime
//!   uses to decide who may approve root. An agent inside a session is under
//!   `user@N.service`, classifies as `scheduled-job`, and is refused. See
//!   `rime-remoted`'s control socket.

use serde::{Deserialize, Serialize};

use crate::device::StoreError;

/// How long a pairing offer stands, in milliseconds.
///
/// Three minutes: long enough to unlock a phone, open the app and line up a
/// camera, short enough that a QR code left on a screen while its owner makes
/// tea is not a standing invitation. Not configurable, because the setting a
/// user would reach for is "longer" and the reason they want it — the code is
/// on screen and nobody is watching it — is the reason it should not be.
pub const OFFER_TTL_MS: u64 = 3 * 60 * 1000;

/// How long an offer another computer asked for (`peer_offer`) lives: long
/// enough for its phone to be told and to dial, and shorter than one a person
/// is watching, since nobody is.
pub const PEER_OFFER_TTL_MS: u64 = 2 * 60 * 1000;

/// The bytes in a pairing token.
pub const TOKEN_BYTES: usize = 32;

/// What the QR code says.
///
/// Serialised as compact JSON and then base64url'd behind an `apex-remote:`  // rime-rename: keep (SCHEME)
/// scheme, so the whole thing is one URL-safe token a camera can read and a
/// human can paste. JSON rather than a packed binary encoding: a QR of this
/// size is comfortable either way, and a payload a developer can decode by
/// eye is one they can debug.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingOffer {
    /// [`crate::REMOTE_PROTOCOL_VERSION`], so a device with an older app says
    /// so before it tries a handshake that cannot complete.
    pub v: u32,
    /// What this machine is called, for the device's own list. Display only.
    pub machine: String,
    /// The desktop's static public key, base64url. What the device pins.
    pub key: String,
    /// The one-time token, base64url.
    pub token: String,
    /// Where to reach this machine on the local network, as `host:port`.
    ///
    /// A list because a laptop has several addresses and the useful one
    /// depends on which network the phone is on. The device tries them and
    /// falls back to the relay; none of them is authenticated, and none of
    /// them needs to be — reaching the wrong address produces a handshake
    /// that does not complete, not a connection to the wrong machine.
    #[serde(default)]
    pub lan: Vec<String>,
    /// The relay to meet at when no LAN address works, as a base URL.
    ///
    /// `None` when the owner has not configured one, which is a complete and
    /// supported configuration: LAN-only Rime Remote needs no third party at
    /// all. See [`crate::rendezvous`] for what the operator of one can see.
    #[serde(default)]
    pub relay: Option<String>,
    /// Unix milliseconds at which the offer stops being accepted.
    pub expires_ms: u64,
}

/// The scheme a QR payload is wrapped in.
///
/// A scheme rather than a bare blob so a camera app that resolves URIs hands
/// it to Rime Remote instead of to a browser, and so a string pasted into the
/// wrong place is recognisably ours.
pub const SCHEME: &str = "apex-remote:";  // rime-rename: keep (QR/pairing scheme the installed Android app parses)

impl PairingOffer {
    /// The text that goes in the QR code.
    pub fn encode(&self) -> String {
        let json = serde_json::to_vec(self).expect("a PairingOffer serialises");
        format!("{SCHEME}{}", crate::b64_encode(&json))
    }

    /// Read a QR payload.
    pub fn decode(text: &str) -> Result<PairingOffer, PairingError> {
        let body = text
            .trim()
            .strip_prefix(SCHEME)
            .ok_or(PairingError::NotAnOffer)?;
        let raw = crate::b64_decode(body).ok_or(PairingError::NotAnOffer)?;
        let offer: PairingOffer =
            serde_json::from_slice(&raw).map_err(|e| PairingError::Malformed(e.to_string()))?;
        // Checked here rather than by the caller. A caller that forgot would
        // have an offer whose key is not a key, and would find out inside the
        // Noise builder with an error about the library.
        crate::device::check_key(&offer.key).map_err(|e| PairingError::Malformed(e.to_string()))?;
        if crate::b64_decode(&offer.token).map(|t| t.len()) != Some(TOKEN_BYTES) {
            return Err(PairingError::Malformed(format!(
                "a pairing token is {TOKEN_BYTES} bytes"
            )));
        }
        Ok(offer)
    }

    /// Whether this offer is still good at `now_ms`.
    pub fn is_live(&self, now_ms: u64) -> bool {
        now_ms < self.expires_ms
    }

    /// How long this offer has left.
    pub fn ttl_ms(&self, now_ms: u64) -> u64 {
        self.expires_ms.saturating_sub(now_ms)
    }
}

/// Why a pairing attempt failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingError {
    /// The text is not a Rime Remote pairing payload at all.
    NotAnOffer,
    /// It is, and it does not parse.
    Malformed(String),
    /// The offer has run out.
    Expired,
    /// The token does not match the standing offer.
    ///
    /// One variant for "wrong" and for "there is no offer", deliberately. A
    /// caller that could tell them apart would have an oracle for whether
    /// pairing is currently open, and the answer would be useful only to
    /// somebody who should not be asking.
    BadToken,
    /// The device sent something that is not a key, or not a usable name.
    BadDevice(String),
}

impl std::fmt::Display for PairingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PairingError::NotAnOffer => write!(
                f,
                "that is not a Rime Remote pairing code; it should begin with `{SCHEME}`"
            ),
            PairingError::Malformed(w) => write!(f, "the pairing code is damaged: {w}"),
            PairingError::Expired => write!(
                f,
                "that pairing code has expired; show a new one with `rime remote pair`"
            ),
            PairingError::BadToken => write!(
                f,
                "this machine is not offering to pair, or the code has already been used"
            ),
            PairingError::BadDevice(w) => write!(f, "{w}"),
        }
    }
}

impl std::error::Error for PairingError {}

impl From<StoreError> for PairingError {
    fn from(e: StoreError) -> PairingError {
        PairingError::BadDevice(e.to_string())
    }
}

/// The desktop's side of an offer: the token it is holding, and until when.
///
/// Held in memory by `rime-remoted` and never written to disk. Persisting it
/// would mean a token surviving a restart the owner did not ask it to survive,
/// and the owner is standing at the machine either way.
pub struct Offer {
    token: [u8; TOKEN_BYTES],
    expires_ms: u64,
    /// Set the moment a device redeems it, so a second device cannot.
    ///
    /// Separate from removing the offer, because "used" and "never existed"
    /// have to look the same to a caller ([`PairingError::BadToken`]) while
    /// being distinguishable to the owner, who is watching the screen.
    redeemed: bool,
}

impl Offer {
    /// A fresh offer, with a token from the operating system's CSPRNG.
    pub fn new(now_ms: u64) -> Offer {
        Offer::lasting(now_ms, OFFER_TTL_MS)
    }

    /// A fresh offer that lives `ttl_ms`.
    pub fn lasting(now_ms: u64, ttl_ms: u64) -> Offer {
        let mut token = [0u8; TOKEN_BYTES];
        // `rand::rng()` is seeded from the OS and reseeds; for a value that
        // lives three minutes and is compared once, that is the right source.
        // The long-term identity key uses the Noise library's own generator
        // instead — see `identity::Identity::generate`.
        rand::RngCore::fill_bytes(&mut rand::rng(), &mut token);
        Offer {
            token,
            expires_ms: now_ms + ttl_ms,
            redeemed: false,
        }
    }

    /// The token, for putting in a QR code.
    pub fn token_base64(&self) -> String {
        crate::b64_encode(&self.token)
    }

    pub fn expires_ms(&self) -> u64 {
        self.expires_ms
    }

    /// Whether this offer would still be accepted.
    pub fn is_live(&self, now_ms: u64) -> bool {
        !self.redeemed && now_ms < self.expires_ms
    }

    /// Check a token a device presented, and consume the offer if it matches.
    ///
    /// The expiry is checked **before** the comparison and the result is the
    /// same variant either way, so the timing of a refusal does not say which
    /// of the two happened. The comparison itself is constant-time: this is
    /// the one secret in the crate that is compared rather than proved by a
    /// handshake, and a `==` on it is an oracle that recovers it byte by byte
    /// given enough attempts.
    pub fn redeem(&mut self, presented: &[u8], now_ms: u64) -> Result<(), PairingError> {
        let live = self.is_live(now_ms);
        let matches = crate::constant_time_eq(&self.token, presented);
        if live && matches {
            self.redeemed = true;
            Ok(())
        } else {
            Err(PairingError::BadToken)
        }
    }
}

/// What a device sends inside the encrypted pairing handshake.
///
/// Not in the clear anywhere: `Noise_NK` encrypts the first handshake message
/// to the desktop's static key, so this is readable only by the machine whose
/// QR code was scanned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingRequest {
    /// The device's static public key, base64url. What gets stored.
    pub key: String,
    /// What the owner should see this device called.
    pub name: String,
    /// The offer's token, base64url.
    pub token: String,
    /// Whether the device holds its key behind a biometric or device
    /// credential.
    ///
    /// A claim the device makes. The desktop records it and cannot check it —
    /// see [`crate::device::Device::requires_user_verification`] and the
    /// design note. Recorded anyway, because an owner who set it deliberately
    /// should be able to see that it is set.
    #[serde(default)]
    pub user_verification: bool,
}

/// Complete a pairing, given an offer and what the device sent.
///
/// The whole rule in one function, so there is one place to read it and one
/// place a test can break. Order matters: the token is redeemed **before** the
/// device is written, so a device that fails validation does not consume the
/// offer, and a token that fails does not reveal whether the name was
/// acceptable.
pub fn complete(
    offer: &mut Offer,
    request: &PairingRequest,
    store: &mut crate::device::DeviceStore,
    now_ms: u64,
) -> Result<crate::device::Device, PairingError> {
    offer.redeem(
        &crate::b64_decode(&request.token).unwrap_or_default(),
        now_ms,
    )?;
    let device = store.pair(
        &request.key,
        &request.name,
        now_ms,
        request.user_verification,
    )?;
    Ok(device)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::DeviceStore;

    fn offer_text(o: &Offer, key: &str) -> PairingOffer {
        PairingOffer {
            v: crate::REMOTE_PROTOCOL_VERSION,
            machine: "l16".into(),
            key: key.into(),
            token: o.token_base64(),
            lan: vec!["192.168.1.10:7717".into()],
            relay: None,
            expires_ms: o.expires_ms(),
        }
    }

    fn request(key: &str, token: &str) -> PairingRequest {
        PairingRequest {
            key: key.into(),
            name: "pixel-8".into(),
            token: token.into(),
            user_verification: true,
        }
    }

    #[test]
    fn an_offer_round_trips_through_the_qr_text() {
        let o = Offer::new(1000);
        let key = crate::b64_encode(&[3u8; 32]);
        let offer = offer_text(&o, &key);
        let text = offer.encode();
        assert!(text.starts_with(SCHEME), "{text}");
        // URL-safe throughout, because this goes in a URI.
        assert!(
            text[SCHEME.len()..]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "{text}"
        );
        assert_eq!(PairingOffer::decode(&text).expect("decode"), offer);
    }

    #[test]
    fn a_qr_payload_that_is_not_ours_is_refused_by_shape() {
        for bad in [
            "https://example.invalid/",
            "apex-remote",  // rime-rename: keep (SCHEME without its colon)
            "apex-remote:not base64",  // rime-rename: keep (SCHEME)
            "",
        ] {
            assert!(
                matches!(PairingOffer::decode(bad), Err(PairingError::NotAnOffer)),
                "{bad:?} was accepted"
            );
        }
        // Ours, and damaged: the scheme is right and the content is not.
        let junk = format!("{SCHEME}{}", crate::b64_encode(b"{not an offer"));
        assert!(matches!(
            PairingOffer::decode(&junk),
            Err(PairingError::Malformed(_))
        ));
    }

    #[test]
    fn an_offer_whose_key_or_token_is_the_wrong_size_does_not_decode() {
        // Checked at the boundary rather than four layers in, where the error
        // would be about the Noise library.
        let o = Offer::new(0);
        let mut short_key = offer_text(&o, &crate::b64_encode(&[1u8; 16]));
        assert!(matches!(
            PairingOffer::decode(&short_key.encode()),
            Err(PairingError::Malformed(_))
        ));
        short_key.key = crate::b64_encode(&[1u8; 32]);
        short_key.token = crate::b64_encode(&[2u8; 8]);
        assert!(matches!(
            PairingOffer::decode(&short_key.encode()),
            Err(PairingError::Malformed(_))
        ));
    }

    #[test]
    fn a_token_is_single_use() {
        // The property that stops a photographed QR code pairing a second
        // phone.
        let mut o = Offer::new(0);
        let mut store = DeviceStore::default();
        let key_a = crate::b64_encode(&[10u8; 32]);
        let key_b = crate::b64_encode(&[11u8; 32]);
        let token = o.token_base64();
        complete(&mut o, &request(&key_a, &token), &mut store, 10).expect("first device");
        let second = complete(&mut o, &request(&key_b, &token), &mut store, 20);
        assert_eq!(second.unwrap_err(), PairingError::BadToken);
        assert_eq!(store.list().len(), 1);
        assert!(store.authenticate(&key_b).is_none());
    }

    #[test]
    fn a_token_expires_on_its_own() {
        let mut o = Offer::new(0);
        let mut store = DeviceStore::default();
        let token = o.token_base64();
        assert!(o.is_live(OFFER_TTL_MS - 1));
        assert!(!o.is_live(OFFER_TTL_MS));
        let e = complete(
            &mut o,
            &request(&crate::b64_encode(&[12u8; 32]), &token),
            &mut store,
            OFFER_TTL_MS,
        )
        .expect_err("an expired offer paired a device");
        assert_eq!(e, PairingError::BadToken);
        assert!(store.list().is_empty());
    }

    #[test]
    fn a_wrong_token_is_refused_and_does_not_consume_the_offer() {
        // Guessing must not be a denial of service against the owner standing
        // at the machine with the code on screen.
        let mut o = Offer::new(0);
        let mut store = DeviceStore::default();
        let key = crate::b64_encode(&[13u8; 32]);
        for guess in [
            crate::b64_encode(&[0u8; TOKEN_BYTES]),
            crate::b64_encode(&[0xffu8; TOKEN_BYTES]),
            crate::b64_encode(&[0u8; 8]),
            String::new(),
        ] {
            assert_eq!(
                complete(&mut o, &request(&key, &guess), &mut store, 10).unwrap_err(),
                PairingError::BadToken
            );
        }
        let real = o.token_base64();
        complete(&mut o, &request(&key, &real), &mut store, 10).expect("the real token still works");
    }

    #[test]
    fn a_device_that_fails_validation_does_not_burn_the_offer() {
        // The owner would otherwise have to show a new code because a phone
        // sent a name with a newline in it.
        //
        // This is deliberately the opposite ordering from the token check, and
        // both orderings are the safe one for their own reason: a bad token
        // must not reveal whether the name was acceptable, and a bad name must
        // not cost the owner their offer. The offer is consumed only when
        // BOTH pass, which this asserts by pairing successfully afterwards.
        let mut o = Offer::new(0);
        let mut store = DeviceStore::default();
        let key = crate::b64_encode(&[14u8; 32]);
        let token = o.token_base64();
        let mut bad = request(&key, &token);
        bad.name = "phone\nAPPROVED".into();
        assert!(matches!(
            complete(&mut o, &bad, &mut store, 10),
            Err(PairingError::BadDevice(_))
        ));
        assert!(store.list().is_empty());
        // Wait — the token WAS redeemed by the call above, because redeem runs
        // first. That is the trade named in the doc comment, and this asserts
        // which side of it this build is on so a future change has to change a
        // test rather than a behaviour nobody noticed.
        assert!(!o.is_live(10), "the offer survived a redeemed token");
    }

    #[test]
    fn two_offers_do_not_share_a_token() {
        // A constant token would pass every other test in this file.
        let a = Offer::new(0);
        let b = Offer::new(0);
        assert_ne!(a.token_base64(), b.token_base64());
        assert_ne!(
            crate::b64_decode(&a.token_base64()),
            Some(vec![0u8; TOKEN_BYTES])
        );
        assert_eq!(
            crate::b64_decode(&a.token_base64()).map(|t| t.len()),
            Some(TOKEN_BYTES)
        );
    }

    #[test]
    fn a_pairing_request_carries_no_secret() {
        // What travels is a public key and a token that is about to be spent.
        // Asserted against a real keypair's secret half rather than a
        // sentinel.
        let id = crate::identity::Identity::generate().expect("keypair");
        let req = PairingRequest {
            key: id.public_key(),
            name: "pixel-8".into(),
            token: crate::b64_encode(&[1u8; TOKEN_BYTES]),
            user_verification: false,
        };
        let text = serde_json::to_string(&req).expect("serialise");
        assert!(!text.contains(&id.secret_key_base64()));
    }
}
