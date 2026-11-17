//! Rime Remote — the protocol a paired device speaks to this machine.
//!
//! Roadmap §46, tasks P1-050/051/052. `rime-remoted` is the desktop service
//! that speaks it; `rime remote` is the CLI that pairs and revokes; the
//! Android app is the first client and deliberately not the only one it can
//! have.
//!
//! ## The one idea
//!
//! **A remote client is a viewport.** P1-030 settled the same argument about
//! terminal multiplexers — a durable thing must not live inside an ephemeral
//! one — and the same answer applies here: `rime-agentd` owns the PTYs, the
//! sessions, the projects and the policy, and it goes on owning them whether
//! or not a phone is connected. This crate is transport, authentication and
//! framing. It is not a second agent runtime, and nothing it does is visible
//! to a session.
//!
//! Concretely: [`wire::Frame::Control`] carries one line of `rime-agentd`'s
//! own NDJSON protocol, unaltered. Provider neutrality is then not something
//! this crate has to achieve — the daemon already speaks to Claude, OpenCode,
//! Codex, Gemini and a generic managed PTY through one adapter layer, and a
//! remote client reaches all of them by reaching the daemon.
//!
//! ## The four things it does add
//!
//! * [`identity`] — this machine's long-term static keypair.
//! * [`device`] — which devices are paired, and which are revoked.
//! * [`pairing`] — the QR code, the one-time token behind it, and the
//!   handshake that turns them into a paired key.
//! * [`push`] — the one thing here that does NOT need a live channel: a
//!   minimised, encrypted envelope that reaches a phone whose app is not
//!   running, over a push transport that is never trusted with its contents.
//! * [`session`] — a Noise channel, and the frames inside it.
//!
//! and one it deliberately does not: it never sees a credential. `SecretValue`
//! in `rime-secret-core` implements neither `Serialize` nor `Deserialize`, and
//! the daemon's `Response` derives `Serialize`, so a reply carrying a
//! credential does not compile. That property is inherited here for free,
//! because what this forwards is that same `Response`.

pub mod device;
pub mod fdpass;
pub mod identity;
pub mod noise;
pub mod pairing;
pub mod push;
pub mod relay;
pub mod rendezvous;
pub mod tls;
pub mod wire;

/// Test-only: the byte vectors the Android client is checked against.
///
/// Compiled only under `cfg(test)` — the module's own attribute — so nothing
/// it contains reaches a shipped binary. See the module documentation for why
/// a second implementation of a hand-rolled protocol needs them.
#[cfg(test)]
mod vectors;

/// The protocol revision this build speaks.
///
/// Exchanged in the pairing payload and again in the handshake prologue, so a
/// device and a desktop that disagree find out before either has sent a frame
/// the other would misread. Separate from `rime-agentd`'s `PROTOCOL_VERSION`,
/// which versions what travels *inside* a control frame: the two change for
/// different reasons and a single number would force a lockstep neither needs.
pub const REMOTE_PROTOCOL_VERSION: u32 = 1;

/// The base64 alphabet used everywhere in this crate: URL-safe, unpadded.
///
/// URL-safe because these strings go in a QR code, in a relay path and on a
/// command line, and `+` and `/` are wrong in at least two of those.
/// Unpadded because `=` is the third character that is wrong in a URL and the
/// length is fixed anyway.
const B64: data_encoding::Encoding = data_encoding::BASE64URL_NOPAD;

/// Encode bytes the way every key and token in this crate is encoded.
pub fn b64_encode(bytes: &[u8]) -> String {
    B64.encode(bytes)
}

/// Decode, or `None`. Never a partial decode: a key that is nearly valid is
/// not a key.
pub fn b64_decode(text: &str) -> Option<Vec<u8>> {
    B64.decode(text.as_bytes()).ok()
}

/// Unix milliseconds now.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Compare two byte strings without leaking where they differ through timing.
///
/// Used for the pairing token, which is the one secret in this crate that is
/// compared rather than proved by a handshake. A `==` on a token is a timing
/// oracle that lets an attacker with many attempts recover it byte by byte —
/// and the token's whole job is to be unguessable for the three minutes it
/// lives.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips_and_is_url_safe() {
        // The bytes that force the two alphabet differences: `+`/`/` in
        // standard base64, `-`/`_` here.
        let awkward = [0xfb, 0xff, 0xbf, 0x00, 0x10, 0x83];
        let text = b64_encode(&awkward);
        assert!(!text.contains('+') && !text.contains('/') && !text.contains('='), "{text}");
        assert_eq!(b64_decode(&text).as_deref(), Some(&awkward[..]));
    }

    #[test]
    fn a_nearly_valid_key_does_not_decode() {
        let good = b64_encode(&[7u8; 32]);
        assert!(b64_decode(&good).is_some());
        assert!(b64_decode(&format!("{good}=")).is_none(), "padding accepted");
        assert!(b64_decode(&format!("{good}!")).is_none());
        assert!(b64_decode("++//").is_none(), "standard alphabet accepted");
    }

    #[test]
    fn constant_time_eq_agrees_with_equality() {
        assert!(constant_time_eq(b"", b""));
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(!constant_time_eq(b"", b"a"));
        // Differing in the first byte and in the last must both be false —
        // the property a short-circuiting comparison gets right and leaks.
        assert!(!constant_time_eq(b"\x00bcdefgh", b"\x01bcdefgh"));
        assert!(!constant_time_eq(b"abcdefg\x00", b"abcdefg\x01"));
    }
}
