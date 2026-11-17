//! Waking a phone that is **not** connected (P1-058).
//!
//! Everything else in this crate assumes a live channel: a device dialled the
//! LAN listener or met this machine at the relay, and both ends are holding a
//! socket open. Notifications built on that arrive only while the app is
//! running and its poll loop is alive, which is the honest limit the Android
//! client has been carrying in its own source since the feature landed.
//!
//! This module is the other half. It is what lets an agent that needs somebody
//! reach a phone whose screen is off, whose app was killed by the platform
//! hours ago, and whose network Android has suspended in Doze.
//!
//! ## Why UnifiedPush, stated as a decision and not as a default
//!
//! Three transports were weighed. The criterion that decided it is P1-058's
//! second — *"notification content is encrypted/minimised so push
//! infrastructure does not receive sensitive prompt/code content"* — together
//! with the one physical fact that constrains all of them: **only a process
//! that Android exempts from Doze can wake a sleeping phone.**
//!
//! * **FCM** would work on stock Android and is what most apps use. It is
//!   refused here for two independent reasons, either of which is enough. It
//!   requires a Firebase project — an account, a `google-services.json` in the
//!   repository, and a Google dependency in a distribution that has
//!   deliberately avoided one. And every wakeup would be a request to Google,
//!   which learns *when* an agent on this person's laptop wanted them even if
//!   it learns nothing about *what*.
//! * **The Rime relay** (`relay/`, deployed at `apex-relay.andrenijman.com`)  // rime-rename: keep (the deployed relay's domain)
//!   looks like the answer because Rime already owns it, and it is the wrong
//!   layer. It is a rendezvous: a Durable Object that copies bytes between two
//!   WebSockets **that have both dialled in**. Push is exactly the case where
//!   the phone has not. Teaching it to queue would not help, because no
//!   Cloudflare Worker can wake a Doze'd phone either — the wakeup has to come
//!   from a process already running on the device.
//! * **UnifiedPush** is what remains, and it is the right answer rather than
//!   the leftover one. The user's chosen *distributor* — ntfy, NextPush,
//!   Conversations — is the app holding the persistent connection, and it is
//!   exempt from battery optimisation because the user installed it for that
//!   purpose. Rime Remote never holds a socket open, never runs a foreground
//!   service, and needs no Google. The distributor is replaceable, the push
//!   server may be the user's own, and neither is trusted by a single line of
//!   this module.
//!
//! **What Andre has to do for this to work is named in `docs/remote.md` and is
//! not nothing:** a UnifiedPush distributor has to be installed on the phone.
//! Nothing has to be stood up on the desktop, nothing is deployed anywhere,
//! and `ntfy.sh` needs no account.
//!
//! ## What crosses the push infrastructure
//!
//! An [`Envelope`]: **51 bytes**, of which 38 are ciphertext. There is no
//! field in it that can hold a sentence. The plaintext is
//! [`Body`] — a kind, an adapter *code*, a session number, the session's start
//! time and a sequence number — five fixed-width integers, and the type has no
//! `String` in it at all. That is deliberate and it is the whole answer to
//! criterion 2: a payload that cannot express prompt text cannot leak it,
//! whatever a future caller does.
//!
//! What the push server and the distributor *do* learn is stated rather than
//! glossed: that a registration received something, when, and that it was 51
//! bytes. The size is constant across every kind, so the length discloses
//! nothing about which one it was. They do not learn the machine, the project,
//! the adapter, or what the agent was doing.
//!
//! There is no cleartext machine identifier in the envelope because none is
//! needed: the phone registers **one UnifiedPush instance per paired machine**
//! and the instance token, which is the phone's own and never leaves it,
//! is what says which machine a message came from. A successful decryption
//! then proves it: the key is per-registration, so only the machine the phone
//! handed it to can produce a body that opens.
//!
//! ## Why the key is handed over and not derived
//!
//! The obvious design is a static-static Diffie-Hellman between this machine's
//! identity key and the device's — no new secret, nothing to store. It does
//! not work, and the reason is on the phone rather than here.
//!
//! The Android device key lives wrapped by a keystore key created with
//! `setUserAuthenticationRequired(true)`, per-use. `StaticKey.agree` can raise
//! a biometric prompt and can throw. A `BroadcastReceiver` woken at three in
//! the morning by a distributor cannot satisfy that, so every push would fail
//! to decrypt on exactly the phones whose owner asked for more protection, and
//! fail silently.
//!
//! So the phone generates 32 random bytes per registration and sends them
//! **inside the Noise channel**, which is already authenticated and already
//! end-to-end. What the key protects is bounded by what an envelope can say,
//! and rotating it costs one re-registration. It is stored here in a file of
//! its own, 0600, and never in `devices.json` — `device.rs` asserts that store
//! holds no secret, and this is a secret.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The envelope format this build writes and reads.
///
/// Authenticated as the AEAD's associated data, so a downgrade to a format
/// with weaker rules is a tag mismatch rather than a body that opens.
pub const PUSH_VERSION: u8 = 1;

/// Bytes of key a registration carries.
pub const KEY_LEN: usize = 32;

/// Bytes of nonce on the wire.
pub const NONCE_LEN: usize = 12;

/// Bytes of Poly1305 tag.
pub const TAG_LEN: usize = 16;

/// The fixed size of a [`Body`] once packed.
pub const BODY_LEN: usize = 22;

/// The fixed size of a sealed envelope. Every kind is this long.
pub const ENVELOPE_LEN: usize = 1 + NONCE_LEN + BODY_LEN + TAG_LEN;

/// The session number an alert carries when it belongs to no session.
///
/// The same value `Alert.NO_SESSION` uses on Android, and negative for the
/// same reason: session ids are a non-negative per-daemon counter, so no real
/// session can collide with it.
pub const NO_SESSION: i32 = -1;

/// Longest endpoint URL accepted. The UnifiedPush specification (AND_3.0.0)
/// caps `endpoint` at 1000 bytes, and a value longer than the specification
/// allows did not come from a distributor.
pub const MAX_ENDPOINT: usize = 1000;

/// HKDF salt. Fixed, public, and here so the two implementations cannot
/// disagree about it silently.
const HKDF_SALT: &[u8] = b"apex.push.v1";  // rime-rename: keep (HKDF salt; phones derive the push key with it)

/// HKDF info.
const HKDF_INFO: &[u8] = b"apex.push.envelope";  // rime-rename: keep (HKDF info; phones derive the push key with it)

/// What happened, in the vocabulary P1-058 asks for.
///
/// The discriminants are the wire. Adding a variant is a wire change and must
/// be appended, never inserted, or a phone built before the change renders the
/// wrong sentence for an alert it thinks it understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Kind {
    /// The agent finished its turn and wants input.
    ///
    /// **This is also how "working complete" arrives**, and that is a property
    /// of the runtime rather than a simplification. `AgentState::Complete`
    /// comes only from the agent process exiting 0, and the `claude` adapter
    /// runs claude interactively, so a turn ending publishes
    /// `waiting_for_user` through the stop hook. A separate "finished" alert
    /// for the end of a turn would never fire.
    Waiting = 1,
    /// The agent is asking before it acts.
    Permission = 2,
    /// The session's process exited non-zero.
    Failed = 3,
    /// The session ended.
    Finished = 4,
    /// A test run Rime observed in one of this machine's worktrees failed.
    TestFailed = 5,
    /// A privileged operation is waiting for a decision at the machine.
    Approval = 6,
    /// A deployment operation completed. See [`Kind::is_deployment`].
    Deployed = 7,
    /// A deployment operation completed with a non-zero exit code.
    DeployFailed = 8,
    /// nex is waiting for somebody to approve or deny a tool call.
    ///
    /// nex sessions are named by strings this body has no room for, so the
    /// alert carries [`NO_SESSION`] and the phone opens its nex screen, which
    /// reads what is waiting over the paired link. What the tool would do
    /// (the command, the file) never goes into an envelope: the body has
    /// nowhere to put it.
    NexApproval = 9,
}

impl Kind {
    /// Decode a wire byte, or `None` for one this build has not been taught.
    ///
    /// `None` and never a default. A kind this build does not know would be
    /// rendered with another kind's words, which is a notification that lies.
    pub fn from_code(code: u8) -> Option<Kind> {
        match code {
            1 => Some(Kind::Waiting),
            2 => Some(Kind::Permission),
            3 => Some(Kind::Failed),
            4 => Some(Kind::Finished),
            5 => Some(Kind::TestFailed),
            6 => Some(Kind::Approval),
            7 => Some(Kind::Deployed),
            8 => Some(Kind::DeployFailed),
            9 => Some(Kind::NexApproval),
            _ => None,
        }
    }

    pub fn code(self) -> u8 {
        self as u8
    }

    /// Every kind, so a test can walk the vocabulary rather than a list
    /// somebody has to remember to extend.
    pub const ALL: [Kind; 9] = [
        Kind::Waiting,
        Kind::Permission,
        Kind::Failed,
        Kind::Finished,
        Kind::TestFailed,
        Kind::Approval,
        Kind::Deployed,
        Kind::DeployFailed,
        Kind::NexApproval,
    ];

    /// Whether this kind reports a deployment.
    ///
    /// P1-058's first criterion asks for deployment notifications and the
    /// previous round recorded, correctly, that nothing on this socket could
    /// raise one. That is no longer true, and the source is named rather than
    /// invented: `PrivilegeRequest` carries `executed_ms` and `exit_code`, and
    /// five of the eight verbs in `rime-agent-core`'s vocabulary are
    /// deployments of the OS image or of the system extension — see
    /// [`is_deployment_verb`]. A request for one of those acquiring an
    /// `executed_ms` is a deployment having finished, with its own exit code,
    /// observed on the wire that already exists.
    ///
    /// What this deliberately does **not** claim to cover is an agent
    /// deploying somebody else's software by running a command in a terminal.
    /// Nothing on this socket can see that: it would be a Bash tool call, and
    /// the only record of it is `SessionInfo.detail`, which is the one field
    /// this module exists to keep away from a push server.
    pub fn is_deployment(self) -> bool {
        matches!(self, Kind::Deployed | Kind::DeployFailed)
    }
}

/// Whether a privileged verb deploys something.
///
/// The five that change what the machine boots or what is installed on it.
/// `install` and `remove` are **not** here even though they rebuild the system
/// extension, because the notification a person wants for those is the
/// approval — which they already get — and a second one when it lands would
/// double every package operation.
pub fn is_deployment_verb(verb: &str) -> bool {
    matches!(
        verb,
        "update" | "rollback" | "pin" | "pkg_rebuild" | "pkg_rollback"
    )
}

/// The adapter, as a **code** and never as the name the daemon reported.
///
/// An allowlist, so a site-local adapter called `acme-internal-bot` reaches a
/// push server as [`Adapter::Unknown`] rather than as the name of a product
/// the owner may not want disclosed. The phone renders `Unknown` as "Agent",
/// which is what `AgentNames.of` already does for an empty id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Adapter {
    Unknown = 0,
    Claude = 1,
    Opencode = 2,
    Codex = 3,
    Gemini = 4,
    Kimi = 5,
    Generic = 6,
}

impl Adapter {
    /// The code for an adapter name, or [`Adapter::Unknown`].
    ///
    /// Case-folded and trimmed exactly as `AgentNames.of` does, so the two
    /// ends agree about `Claude` and ` claude `.
    pub fn of(name: &str) -> Adapter {
        match name.trim().to_ascii_lowercase().as_str() {
            "claude" => Adapter::Claude,
            "opencode" => Adapter::Opencode,
            "codex" => Adapter::Codex,
            "gemini" => Adapter::Gemini,
            "kimi" => Adapter::Kimi,
            "generic" => Adapter::Generic,
            _ => Adapter::Unknown,
        }
    }

    pub fn from_code(code: u8) -> Adapter {
        match code {
            1 => Adapter::Claude,
            2 => Adapter::Opencode,
            3 => Adapter::Codex,
            4 => Adapter::Gemini,
            5 => Adapter::Kimi,
            6 => Adapter::Generic,
            _ => Adapter::Unknown,
        }
    }

    pub fn code(self) -> u8 {
        self as u8
    }
}

/// The plaintext, and the reason criterion 2 holds by construction.
///
/// Five integers. There is no `String` field, no `Vec<u8>`, and no way to add
/// one without changing [`BODY_LEN`] and every vector committed for the
/// Android suite — which is the point. A caller who decided a notification
/// would be nicer with the file name in it cannot express that here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Body {
    pub kind: Kind,
    pub adapter: Adapter,
    /// The session, or [`NO_SESSION`].
    pub session: i32,
    /// When that session started, in Unix **seconds**.
    ///
    /// Seconds because that is the unit `SessionInfo.started` is in — the
    /// Android `AgentSession` documents the same trap — and a value that had
    /// to be converted on one side and not the other would compare unequal
    /// every time while looking right in both.
    ///
    /// Carried because a session id is **not** an identity: `registry.rs`
    /// reuses ids after a prune, so a notification tapped minutes later could
    /// otherwise open a different agent's terminal. `Reply.Target` on Android
    /// already compares this before liveness for the same reason. Zero for an
    /// alert that belongs to no session.
    pub started: u64,
    /// Monotonic per registration, so the phone can drop a replay and order
    /// two envelopes that arrive out of order.
    pub seq: u64,
}

impl Body {
    /// Pack into exactly [`BODY_LEN`] bytes, big-endian throughout.
    pub fn pack(&self) -> [u8; BODY_LEN] {
        let mut out = [0u8; BODY_LEN];
        out[0] = self.kind.code();
        out[1] = self.adapter.code();
        out[2..6].copy_from_slice(&self.session.to_be_bytes());
        out[6..14].copy_from_slice(&self.started.to_be_bytes());
        out[14..22].copy_from_slice(&self.seq.to_be_bytes());
        out
    }

    /// Unpack, or say why the bytes are not a body.
    pub fn unpack(bytes: &[u8]) -> Result<Body, PushError> {
        if bytes.len() != BODY_LEN {
            return Err(PushError::Malformed("a push body is 22 bytes"));
        }
        let kind = Kind::from_code(bytes[0]).ok_or(PushError::UnknownKind(bytes[0]))?;
        let mut session = [0u8; 4];
        session.copy_from_slice(&bytes[2..6]);
        let mut started = [0u8; 8];
        started.copy_from_slice(&bytes[6..14]);
        let mut seq = [0u8; 8];
        seq.copy_from_slice(&bytes[14..22]);
        Ok(Body {
            kind,
            adapter: Adapter::from_code(bytes[1]),
            session: i32::from_be_bytes(session),
            started: u64::from_be_bytes(started),
            seq: u64::from_be_bytes(seq),
        })
    }

    /// What two envelopes must share to be the same notification.
    ///
    /// The same rule the Android `AlertWatcher` uses — (machine, session,
    /// kind) — minus the machine, which is the registration here. The phone
    /// folds the machine back in when it derives the Android notification id,
    /// which is what makes a pushed alert and a polled alert for one moment
    /// **replace** each other instead of stacking. That is criterion 4, and
    /// this is the half of it that has to be true on the wire.
    pub fn dedup_key(&self) -> (i32, u8) {
        (self.session, self.kind.code())
    }
}

/// Why an envelope could not be built, opened, or delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushError {
    /// The bytes are not the shape an envelope has.
    Malformed(&'static str),
    /// A version byte this build does not speak.
    Version(u8),
    /// A kind byte this build does not speak.
    UnknownKind(u8),
    /// The tag did not verify: a forgery, a corruption, or the wrong key.
    BadTag,
    /// A key that is not [`KEY_LEN`] bytes.
    BadKey(String),
    /// An endpoint that cannot be posted to, and why.
    BadEndpoint(String),
}

impl std::fmt::Display for PushError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PushError::Malformed(why) => write!(f, "{why}"),
            PushError::Version(v) => write!(
                f,
                "this build speaks push envelope version {PUSH_VERSION}; that one says {v}"
            ),
            PushError::UnknownKind(k) => write!(
                f,
                "{k} is not a notification kind this build knows; a newer machine sent it"
            ),
            PushError::BadTag => write!(
                f,
                "the envelope did not authenticate: it was not sealed with this registration's key"
            ),
            PushError::BadKey(why) => write!(f, "{why}"),
            PushError::BadEndpoint(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for PushError {}

/// The AEAD key a registration's raw key produces.
///
/// RFC 5869 HKDF-SHA256 with one 32-byte output, spelled out rather than taken
/// from a crate, for the reason `Crypto.hmacBlake2s` on the Android side gives
/// for the same choice: the construction is load-bearing enough that it
/// belongs written down where a reader can check it against the RFC, and the
/// Kotlin half has to be able to reproduce it byte for byte.
///
/// Domain separation is not decoration here. The registration key is handed
/// over inside a Noise channel and could in principle be reused; running it
/// through HKDF with this module's own salt and info means the bytes that
/// encrypt an envelope are good for encrypting an envelope and nothing else.
pub fn aead_key(raw: &[u8]) -> Result<[u8; 32], PushError> {
    if raw.len() != KEY_LEN {
        return Err(PushError::BadKey(format!(
            "a push key is {KEY_LEN} bytes; this one is {}",
            raw.len()
        )));
    }
    let prk = hmac_sha256(HKDF_SALT, raw);
    let mut info = HKDF_INFO.to_vec();
    info.push(0x01);
    Ok(hmac_sha256(&prk, &info))
}

/// HMAC-SHA256, RFC 2104, over the `sha2` crate this crate already depends on.
fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        let d = Sha256::digest(key);
        k[..32].copy_from_slice(&d);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let inner = Sha256::new().chain_update(ipad).chain_update(data).finalize();
    let outer = Sha256::new().chain_update(opad).chain_update(inner).finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&outer);
    out
}

/// A sealed envelope: what actually crosses the push infrastructure.
///
/// ```text
/// [u8 version][12-byte nonce][38 bytes of ciphertext and tag]
/// ```
///
/// The version byte is outside the ciphertext because the reader has to know
/// how to read before it can decrypt, and it is the **associated data** so it
/// is still authenticated: an on-path party who rewrites it produces something
/// that does not open rather than something that opens as an older format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope(pub Vec<u8>);

impl Envelope {
    /// Seal a body under a registration's key.
    ///
    /// The nonce is supplied rather than generated so that the cross-language
    /// vectors are reproducible. [`Envelope::seal`] is the caller-facing one
    /// and takes its nonce from the system.
    pub fn seal_with_nonce(
        key: &[u8],
        body: &Body,
        nonce: [u8; NONCE_LEN],
    ) -> Result<Envelope, PushError> {
        let key = aead_key(key)?;
        let cipher = ChaCha20Poly1305::new((&key).into());
        let sealed = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &body.pack(),
                    aad: &[PUSH_VERSION],
                },
            )
            .map_err(|_| PushError::Malformed("the envelope could not be sealed"))?;
        let mut out = Vec::with_capacity(ENVELOPE_LEN);
        out.push(PUSH_VERSION);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&sealed);
        Ok(Envelope(out))
    }

    /// Seal with a fresh nonce.
    pub fn seal(key: &[u8], body: &Body) -> Result<Envelope, PushError> {
        let mut nonce = [0u8; NONCE_LEN];
        rand::RngCore::fill_bytes(&mut rand::rng(), &mut nonce);
        Envelope::seal_with_nonce(key, body, nonce)
    }

    /// Open, or say why it did not open.
    pub fn open(key: &[u8], bytes: &[u8]) -> Result<Body, PushError> {
        if bytes.len() != ENVELOPE_LEN {
            return Err(PushError::Malformed("a push envelope is 51 bytes"));
        }
        if bytes[0] != PUSH_VERSION {
            return Err(PushError::Version(bytes[0]));
        }
        let key = aead_key(key)?;
        let cipher = ChaCha20Poly1305::new((&key).into());
        let plain = cipher
            .decrypt(
                Nonce::from_slice(&bytes[1..1 + NONCE_LEN]),
                Payload {
                    msg: &bytes[1 + NONCE_LEN..],
                    aad: &[PUSH_VERSION],
                },
            )
            .map_err(|_| PushError::BadTag)?;
        Body::unpack(&plain)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Where a registration's messages are posted.
///
/// A URL the distributor gave the phone and the phone passed on. This machine
/// treats it as opaque apart from the checks below, because it is: the path is
/// a push server's business and inventing rules about its shape would refuse
/// self-hosted servers for no gain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    /// Path and query together, ready to be a request target.
    pub target: String,
}

impl Endpoint {
    /// Parse and check a push endpoint.
    ///
    /// **`https` only.** A plaintext endpoint would put the registration's URL
    /// — which is a capability to send this phone notifications — in front of
    /// every network the desktop is on, and would let anything on the path
    /// suppress or replay deliveries. The body is ciphertext either way, so
    /// what TLS buys here is the endpoint's secrecy and the delivery's
    /// integrity, and both are worth a refusal.
    ///
    /// A private-network address is **not** refused. An ntfy behind a reverse
    /// proxy on the owner's own LAN is the privacy-maximal deployment of this
    /// whole design, and a rule that blocked it would push people towards a
    /// public server.
    pub fn parse(url: &str) -> Result<Endpoint, PushError> {
        let url = url.trim();
        if url.len() > MAX_ENDPOINT {
            return Err(PushError::BadEndpoint(format!(
                "a push endpoint may be at most {MAX_ENDPOINT} bytes; this one is {}",
                url.len()
            )));
        }
        if url.chars().any(|c| c.is_control() || c == ' ') {
            return Err(PushError::BadEndpoint(
                "a push endpoint may not contain spaces or control characters".into(),
            ));
        }
        let rest = url.strip_prefix("https://").ok_or_else(|| {
            PushError::BadEndpoint(
                "a push endpoint must be https://; a plaintext one would expose the endpoint \
                 itself, which is the capability to notify this phone"
                    .into(),
            )
        })?;
        if rest.contains('#') {
            return Err(PushError::BadEndpoint(
                "a push endpoint may not carry a fragment: it is not sent to the server".into(),
            ));
        }
        let (authority, path) = match rest.find(['/', '?']) {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        if authority.is_empty() {
            return Err(PushError::BadEndpoint(format!("{url:?} names no host")));
        }
        if authority.contains('@') {
            return Err(PushError::BadEndpoint(
                "a push endpoint may not carry credentials".into(),
            ));
        }
        let (host, port_text) = if let Some(open) = authority.strip_prefix('[') {
            let (inside, after) = open.split_once(']').ok_or_else(|| {
                PushError::BadEndpoint(format!("{authority:?} is an unclosed IPv6 literal"))
            })?;
            (inside.to_string(), after.strip_prefix(':').map(str::to_string))
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), Some(p.to_string())),
                None => (authority.to_string(), None),
            }
        };
        if host.is_empty() {
            return Err(PushError::BadEndpoint(format!("{url:?} names no host")));
        }
        let port = match port_text {
            None => 443,
            Some(text) => match text.parse::<u16>() {
                Ok(0) | Err(_) => {
                    return Err(PushError::BadEndpoint(format!("{text:?} is not a port")))
                }
                Ok(p) => p,
            },
        };
        let target = if path.is_empty() {
            "/".to_string()
        } else if path.starts_with('?') {
            format!("/{path}")
        } else {
            path.to_string()
        };
        Ok(Endpoint { host, port, target })
    }

    /// The `Host:` header value.
    pub fn authority(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        if self.port == 443 {
            host
        } else {
            format!("{host}:{}", self.port)
        }
    }

    /// The bytes of an RFC 8030 push request carrying one envelope.
    ///
    /// Hand-written rather than taken from an HTTP crate, for the reason
    /// `relay.rs` gives for hand-writing RFC 6455: this workspace has no HTTP
    /// client, and one request shape with a fixed body length and `Connection:
    /// close` is a smaller thing to get right than a dependency is to audit.
    ///
    /// What is deliberately **not** in these headers: any `User-Agent`, any
    /// `Topic`, and anything naming this machine. A push server that is told
    /// the software and the host learns more from the metadata than it does
    /// from the body.
    pub fn request(&self, envelope: &Envelope) -> Vec<u8> {
        let body = envelope.as_bytes();
        let mut out = Vec::with_capacity(256 + body.len());
        out.extend_from_slice(
            format!(
                "POST {} HTTP/1.1\r\n\
                 Host: {}\r\n\
                 Content-Type: application/octet-stream\r\n\
                 Content-Length: {}\r\n\
                 TTL: {}\r\n\
                 Urgency: high\r\n\
                 Connection: close\r\n\r\n",
                self.target,
                self.authority(),
                body.len(),
                DEFAULT_TTL,
            )
            .as_bytes(),
        );
        out.extend_from_slice(body);
        out
    }
}

/// How long a push server may hold an undelivered envelope, in seconds.
///
/// RFC 8030 makes `TTL` mandatory and a server may reject a request without
/// one. Four hours: an alert about an agent that wanted somebody is worth
/// delivering to a phone that was in a tunnel, and is not worth delivering to
/// one that has been off since yesterday — by then the state it describes has
/// almost certainly changed and the notification would be a lie.
pub const DEFAULT_TTL: u32 = 14_400;

/// What the push server said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Accepted.
    Accepted,
    /// The endpoint is gone: 404 or 410. The registration must be dropped.
    ///
    /// Dropped rather than retried because these two codes are RFC 8030's way
    /// of saying the subscription no longer exists, and retrying a subscription
    /// the server has forgotten is a request that will never succeed.
    Gone,
    /// Anything else: retryable, with the code for the log.
    Retry(u16),
}

impl Delivery {
    /// Read a status line. `None` when the bytes are not one.
    pub fn of(status: &[u8]) -> Option<Delivery> {
        let text = std::str::from_utf8(status).ok()?;
        let line = text.lines().next()?;
        let code: u16 = line.split(' ').nth(1)?.parse().ok()?;
        Some(match code {
            200..=299 => Delivery::Accepted,
            404 | 410 => Delivery::Gone,
            other => Delivery::Retry(other),
        })
    }
}


/// The most bytes read while looking for a status line.
///
/// Only the first line is wanted. A server answering with a megabyte before
/// its first newline is answering a request this client did not make.
pub const MAX_STATUS: usize = 4096;

/// Post one envelope over an already-connected stream, and read the answer.
///
/// Generic over the stream so that the HTTP exchange — which is a rule, and
/// therefore has to be testable — is separated from making a TLS connection,
/// which is plumbing. `rime-remoted` supplies a `rustls` stream; the tests
/// here supply a duplex in memory, and `tests/tls.rs` supplies a real one to a
/// real server on loopback.
pub fn exchange<R: std::io::Read, W: std::io::Write>(
    reader: &mut R,
    writer: &mut W,
    endpoint: &Endpoint,
    envelope: &Envelope,
) -> std::io::Result<Delivery> {
    writer.write_all(&endpoint.request(envelope))?;
    writer.flush()?;
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while line.len() < MAX_STATUS {
        match reader.read(&mut byte)? {
            0 => break,
            _ => {
                if byte[0] == b'\n' {
                    break;
                }
                line.push(byte[0]);
            }
        }
    }
    Delivery::of(&line).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{} did not answer with an HTTP status line",
                endpoint.authority()
            ),
        )
    })
}

/// One phone's push registration.
///
/// Keyed by the device id the **handshake** proved, never by anything in the
/// request body: a paired device may only register its own endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registration {
    pub device_id: String,
    pub endpoint: String,
    /// The registration key, base64url. A secret, which is why this record
    /// lives in a file of its own and not in `devices.json`.
    pub key: String,
    pub registered_ms: u64,
    /// The last sequence number sent. Persisted, so a daemon restart does not
    /// rewind and hand the phone a replay it will correctly ignore.
    #[serde(default)]
    pub seq: u64,
    /// Consecutive delivery failures. Reset by a success.
    #[serde(default)]
    pub failures: u32,
}

/// How many consecutive failures retire a registration.
///
/// A push server that has been unreachable this many times in a row is either
/// gone or does not want us, and a desktop that kept posting would be a
/// well-behaved client generating traffic forever. The poll path still works,
/// so retiring a registration degrades the feature rather than breaking it,
/// and the phone re-registers on its next connection.
pub const MAX_FAILURES: u32 = 20;

/// Every registration this machine holds.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PushStore {
    #[serde(default)]
    pub registrations: BTreeMap<String, Registration>,
}

impl PushStore {
    /// Beside `devices.json`, and deliberately not inside it.
    pub fn path_in(state_home: &Path) -> PathBuf {
        state_home.join("rime").join("remote").join("push.json")
    }

    pub fn path() -> PathBuf {
        Self::path_in(&rime_agent_core::paths::state_home())
    }

    /// Read, or an empty store when there is no file.
    ///
    /// A file that exists and does not parse is an error and never an empty
    /// store, for the reason `DeviceStore::load` gives: silently forgetting
    /// every registration produces "my phone stopped notifying me", which
    /// points nowhere near the cause.
    pub fn load(path: &Path) -> std::io::Result<PushStore> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("{}: {e}", path.display()),
                )
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(PushStore::default()),
            Err(e) => Err(e),
        }
    }

    /// Write, 0600, atomically.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let dir = path.parent().unwrap_or(Path::new("."));
        rime_agent_core::paths::ensure_private_dir(dir)?;
        let tmp = dir.join(format!(".push.json.{}", std::process::id()));
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?.as_bytes())?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&tmp, path)
    }

    /// Record a device's endpoint and key, replacing whatever it had.
    ///
    /// Replacing and not adding: a phone has one endpoint at a time, and a
    /// store that accumulated them would go on posting to endpoints the phone
    /// abandoned — which on a shared push server means notifying whoever the
    /// endpoint is recycled to.
    pub fn register(
        &mut self,
        device_id: &str,
        endpoint: &str,
        key: &str,
        now_ms: u64,
    ) -> Result<&Registration, PushError> {
        Endpoint::parse(endpoint)?;
        let raw = crate::b64_decode(key)
            .ok_or_else(|| PushError::BadKey("a push key must be base64url".into()))?;
        if raw.len() != KEY_LEN {
            return Err(PushError::BadKey(format!(
                "a push key is {KEY_LEN} bytes; this one decodes to {}",
                raw.len()
            )));
        }
        // The sequence carries over a re-registration rather than restarting.
        // The phone's replay guard is "seq greater than the last I saw", and a
        // counter that went back to zero when a phone re-registered would make
        // every envelope after it look like a replay.
        let seq = self.registrations.get(device_id).map(|r| r.seq).unwrap_or(0);
        self.registrations.insert(
            device_id.to_string(),
            Registration {
                device_id: device_id.to_string(),
                endpoint: endpoint.trim().to_string(),
                key: key.to_string(),
                registered_ms: now_ms,
                seq,
                failures: 0,
            },
        );
        Ok(&self.registrations[device_id])
    }

    /// Forget a device's registration. Idempotent.
    pub fn forget(&mut self, device_id: &str) -> bool {
        self.registrations.remove(device_id).is_some()
    }

    /// The next sequence number for a device, consuming it.
    pub fn next_seq(&mut self, device_id: &str) -> Option<u64> {
        let r = self.registrations.get_mut(device_id)?;
        r.seq = r.seq.saturating_add(1);
        Some(r.seq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [7u8; 32];

    fn body(kind: Kind) -> Body {
        Body {
            kind,
            adapter: Adapter::Claude,
            session: 3,
            started: 1_726_000_000,
            seq: 42,
        }
    }

    #[test]
    fn an_envelope_round_trips_and_is_fifty_one_bytes_for_every_kind() {
        for kind in Kind::ALL {
            let sealed = Envelope::seal(&KEY, &body(kind)).expect("seal");
            assert_eq!(
                sealed.as_bytes().len(),
                ENVELOPE_LEN,
                "{kind:?} did not produce a constant-size envelope; the size would then \
                 disclose which kind it was"
            );
            assert_eq!(Envelope::open(&KEY, sealed.as_bytes()).expect("open"), body(kind));
        }
    }

    #[test]
    fn nothing_but_five_integers_can_cross_the_push_infrastructure() {
        // Criterion 2, asserted at the level where it is actually true: the
        // plaintext is a fixed 22 bytes whatever is going on, so there is no
        // room in it for a command line, a path or a prompt. A field added to
        // `Body` fails here before it can reach a push server.
        assert_eq!(BODY_LEN, 22);
        assert_eq!(body(Kind::Waiting).pack().len(), BODY_LEN);
        assert_eq!(ENVELOPE_LEN, 51);
        // And the whole thing fits the UnifiedPush cleartext bound with three
        // orders of magnitude to spare, so no distributor will ever truncate.
        // A const assertion rather than a runtime one: the comparison is
        // decided at compile time, so a change that broke it would otherwise
        // be a test somebody has to run.
        const _: () = assert!(ENVELOPE_LEN < 3993);
    }

    #[test]
    fn a_forged_or_rekeyed_envelope_does_not_open() {
        let sealed = Envelope::seal(&KEY, &body(Kind::Failed)).expect("seal");
        assert_eq!(
            Envelope::open(&[8u8; 32], sealed.as_bytes()),
            Err(PushError::BadTag),
            "an envelope opened under a key it was not sealed with"
        );
        for i in 0..ENVELOPE_LEN {
            let mut bad = sealed.as_bytes().to_vec();
            bad[i] ^= 0x01;
            assert!(
                Envelope::open(&KEY, &bad).is_err(),
                "flipping byte {i} left an envelope that still opened"
            );
        }
    }

    #[test]
    fn the_version_byte_is_authenticated_and_not_merely_present() {
        // A downgrade has to fail as a tag mismatch. If the version were
        // outside the associated data, an on-path party could rewrite it and
        // a future build with different rules for version 1 would apply them.
        let sealed = Envelope::seal(&KEY, &body(Kind::Waiting)).expect("seal");
        let mut bad = sealed.as_bytes().to_vec();
        bad[0] = 2;
        assert_eq!(Envelope::open(&KEY, &bad), Err(PushError::Version(2)));
        // And with the version restored but the aad implicitly wrong, the tag
        // is what catches it: proved by sealing under a different version's
        // aad through the same primitive.
        let key = aead_key(&KEY).expect("key");
        let cipher = ChaCha20Poly1305::new((&key).into());
        let other = cipher
            .encrypt(
                Nonce::from_slice(&[0u8; NONCE_LEN]),
                Payload { msg: &body(Kind::Waiting).pack(), aad: &[9u8] },
            )
            .expect("seal");
        let mut wire = vec![PUSH_VERSION];
        wire.extend_from_slice(&[0u8; NONCE_LEN]);
        wire.extend_from_slice(&other);
        assert_eq!(Envelope::open(&KEY, &wire), Err(PushError::BadTag));
    }

    #[test]
    fn a_kind_this_build_does_not_know_is_refused_rather_than_defaulted() {
        // The failure that matters: rendering an unknown kind with a known
        // kind's words is a notification that lies about what happened.
        let key = aead_key(&KEY).expect("key");
        let cipher = ChaCha20Poly1305::new((&key).into());
        let mut plain = body(Kind::Waiting).pack();
        plain[0] = 99;
        let sealed = cipher
            .encrypt(
                Nonce::from_slice(&[1u8; NONCE_LEN]),
                Payload { msg: &plain, aad: &[PUSH_VERSION] },
            )
            .expect("seal");
        let mut wire = vec![PUSH_VERSION];
        wire.extend_from_slice(&[1u8; NONCE_LEN]);
        wire.extend_from_slice(&sealed);
        assert_eq!(Envelope::open(&KEY, &wire), Err(PushError::UnknownKind(99)));
    }

    #[test]
    fn an_unknown_adapter_reaches_the_wire_as_a_code_and_not_as_a_name() {
        // The allowlist is the point: a site-local adapter name is a fact
        // about the owner's setup and a push server has no business learning
        // it. An unknown one becomes 0, which the phone renders as "Agent".
        assert_eq!(Adapter::of("acme-internal-bot"), Adapter::Unknown);
        assert_eq!(Adapter::of("Claude"), Adapter::Claude);
        assert_eq!(Adapter::of("  codex "), Adapter::Codex);
        let b = Body { adapter: Adapter::of("acme-internal-bot"), ..body(Kind::Waiting) };
        let sealed = Envelope::seal(&KEY, &b).expect("seal");
        assert!(
            !contains(sealed.as_bytes(), b"acme"),
            "an adapter name reached the wire"
        );
        assert_eq!(
            Envelope::open(&KEY, sealed.as_bytes()).expect("open").adapter,
            Adapter::Unknown
        );
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn the_deployment_verbs_are_the_ones_that_change_what_the_machine_runs() {
        for verb in ["update", "rollback", "pin", "pkg_rebuild", "pkg_rollback"] {
            assert!(is_deployment_verb(verb), "{verb} is a deployment");
        }
        // install and remove are excluded deliberately: the approval prompt is
        // already a notification for those, and a second one on completion
        // would double every package operation.
        for verb in ["install", "remove", "pkg_upgrade", "", "deploy"] {
            assert!(!is_deployment_verb(verb), "{verb} is not a deployment verb");
        }
        assert!(Kind::Deployed.is_deployment() && Kind::DeployFailed.is_deployment());
        assert!(Kind::ALL.iter().filter(|k| k.is_deployment()).count() == 2);
    }

    #[test]
    fn every_kind_code_round_trips_and_none_is_zero() {
        for kind in Kind::ALL {
            assert_eq!(Kind::from_code(kind.code()), Some(kind));
            assert_ne!(kind.code(), 0, "0 must stay free as 'not a kind'");
        }
        assert_eq!(Kind::from_code(0), None);
        assert_eq!(Kind::from_code(10), None);
        // The discriminants are the wire: a variant inserted rather than
        // appended would renumber the ones after it and a phone built before
        // the change would render the wrong sentence.
        assert_eq!(Kind::Waiting.code(), 1);
        assert_eq!(Kind::DeployFailed.code(), 8);
        assert_eq!(Kind::NexApproval.code(), 9);
    }

    #[test]
    fn a_push_endpoint_must_be_https_and_may_be_on_a_private_network() {
        let e = Endpoint::parse("https://ntfy.sh/upAbCdEf123").expect("parse");
        assert_eq!(e.host, "ntfy.sh");
        assert_eq!(e.port, 443);
        assert_eq!(e.target, "/upAbCdEf123");
        assert_eq!(e.authority(), "ntfy.sh");

        // The owner's own server on their own LAN is the privacy-maximal
        // deployment of this design and must not be refused.
        let lan = Endpoint::parse("https://192.168.1.10:8443/UP?id=x").expect("parse");
        assert_eq!(lan.port, 8443);
        assert_eq!(lan.target, "/UP?id=x");
        assert_eq!(lan.authority(), "192.168.1.10:8443");

        let v6 = Endpoint::parse("https://[2001:db8::1]/up").expect("parse");
        assert_eq!(v6.host, "2001:db8::1");
        assert_eq!(v6.authority(), "[2001:db8::1]");

        for bad in [
            "http://ntfy.sh/up1",
            "ws://ntfy.sh/up1",
            "ntfy.sh/up1",
            "https://",
            "https:///up",
            "https://user:pw@ntfy.sh/up",
            "https://ntfy.sh/up#frag",
            "https://ntfy.sh:0/up",
            "https://ntfy.sh:notaport/up",
            "https://ntfy.sh/up\nX: y",
            "https://ntfy.sh/up with space",
        ] {
            assert!(
                Endpoint::parse(bad).is_err(),
                "{bad:?} was accepted as a push endpoint"
            );
        }
        assert!(Endpoint::parse(&format!("https://ntfy.sh/{}", "u".repeat(MAX_ENDPOINT))).is_err());
    }

    #[test]
    fn a_request_names_the_body_and_nothing_about_this_machine() {
        let e = Endpoint::parse("https://ntfy.sh/upAbCdEf123").expect("parse");
        let sealed = Envelope::seal(&KEY, &body(Kind::Waiting)).expect("seal");
        let req = e.request(&sealed);
        let text = String::from_utf8_lossy(&req[..req.len() - ENVELOPE_LEN]).to_string();
        assert!(text.starts_with("POST /upAbCdEf123 HTTP/1.1\r\n"), "{text}");
        assert!(text.contains("Host: ntfy.sh\r\n"), "{text}");
        assert!(text.contains(&format!("Content-Length: {ENVELOPE_LEN}\r\n")), "{text}");
        assert!(text.contains(&format!("TTL: {DEFAULT_TTL}\r\n")), "{text}");
        assert!(text.contains("Content-Type: application/octet-stream\r\n"), "{text}");
        // Metadata discloses more than the body does, so these must not be
        // there. A `User-Agent` names the software; a `Topic` would let a
        // server collapse two alerts it cannot read.
        for banned in ["User-Agent", "Topic", "apex", "APEX", "rime", "Rime"] {  // rime-rename: keep (neither name may leak)
            assert!(!text.contains(banned), "the request named {banned}:\n{text}");
        }
        assert_eq!(&req[req.len() - ENVELOPE_LEN..], sealed.as_bytes());
        // The headers end exactly once, so a body cannot be read as headers.
        assert_eq!(text.matches("\r\n\r\n").count(), 1);
    }


    /// A stream that answers with fixed bytes and remembers what it was sent.
    struct Doubled {
        reply: std::io::Cursor<Vec<u8>>,
        wrote: Vec<u8>,
    }

    impl Doubled {
        fn answering(reply: &str) -> Doubled {
            Doubled {
                reply: std::io::Cursor::new(reply.as_bytes().to_vec()),
                wrote: Vec::new(),
            }
        }
    }

    impl std::io::Read for Doubled {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            std::io::Read::read(&mut self.reply, buf)
        }
    }

    impl std::io::Write for Doubled {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.wrote.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn an_exchange_writes_the_whole_request_and_reads_only_the_status_line() {
        let endpoint = Endpoint::parse("https://ntfy.sh/upAbCdEf").expect("endpoint");
        let sealed = Envelope::seal(&KEY, &body(Kind::Waiting)).expect("seal");
        let mut s = Doubled::answering(
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok",
        );
        let mut r = Doubled::answering("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        let got = exchange(&mut r, &mut s, &endpoint, &sealed).expect("exchange");
        assert_eq!(got, Delivery::Accepted);
        assert_eq!(s.wrote, endpoint.request(&sealed));
        // The body is never read. A push server's response body is its own
        // business, and reading to the end would block on one that keeps the
        // connection open.
        assert!(r.reply.position() as usize <= "HTTP/1.1 200 OK\r\n".len());
    }

    #[test]
    fn an_exchange_reports_what_the_server_said_and_refuses_what_is_not_http() {
        let endpoint = Endpoint::parse("https://ntfy.sh/upAbCdEf").expect("endpoint");
        let sealed = Envelope::seal(&KEY, &body(Kind::Waiting)).expect("seal");
        for (reply, want) in [
            ("HTTP/1.1 202 Accepted\r\n\r\n", Delivery::Accepted),
            ("HTTP/1.1 410 Gone\r\n\r\n", Delivery::Gone),
            ("HTTP/1.1 404 Not Found\r\n\r\n", Delivery::Gone),
            ("HTTP/1.1 503 Unavailable\r\n\r\n", Delivery::Retry(503)),
        ] {
            let mut r = Doubled::answering(reply);
            let mut w = Doubled::answering("");
            assert_eq!(
                exchange(&mut r, &mut w, &endpoint, &sealed).expect("exchange"),
                want,
                "{reply:?}"
            );
        }
        // A server that says nothing, or something that is not HTTP, must be an
        // error rather than a silent success — the caller counts a failure and
        // eventually retires the registration.
        for reply in ["", "hello\r\n", "\r\n"] {
            let mut r = Doubled::answering(reply);
            let mut w = Doubled::answering("");
            assert!(
                exchange(&mut r, &mut w, &endpoint, &sealed).is_err(),
                "{reply:?} was read as an answer"
            );
        }
    }

    #[test]
    fn a_status_line_says_retry_drop_or_done() {
        assert_eq!(Delivery::of(b"HTTP/1.1 200 OK\r\n"), Some(Delivery::Accepted));
        assert_eq!(Delivery::of(b"HTTP/1.1 202 Accepted\r\n"), Some(Delivery::Accepted));
        assert_eq!(Delivery::of(b"HTTP/1.1 404 Not Found\r\n"), Some(Delivery::Gone));
        assert_eq!(Delivery::of(b"HTTP/1.1 410 Gone\r\n"), Some(Delivery::Gone));
        assert_eq!(Delivery::of(b"HTTP/1.1 429 Too Many\r\n"), Some(Delivery::Retry(429)));
        assert_eq!(Delivery::of(b"HTTP/1.1 500 Oops\r\n"), Some(Delivery::Retry(500)));
        assert_eq!(Delivery::of(b"garbage"), None);
        assert_eq!(Delivery::of(b""), None);
    }

    #[test]
    fn the_registration_store_keeps_a_secret_and_keeps_it_out_of_the_device_store() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("rime-push-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = PushStore::path_in(&dir);

        let mut s = PushStore::default();
        let key = crate::b64_encode(&KEY);
        s.register("dev0123456789ab", "https://ntfy.sh/up1", &key, 1000)
            .expect("register");
        s.save(&path).expect("save");
        assert_eq!(
            std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777,
            0o600
        );
        // The two stores are different files. The device store is asserted
        // elsewhere to hold no secret, and this holds one, so they must not be
        // the same place.
        assert_ne!(path, crate::device::DeviceStore::path_in(&dir));

        let back = PushStore::load(&path).expect("load");
        assert_eq!(back.registrations["dev0123456789ab"].endpoint, "https://ntfy.sh/up1");

        // A corrupt file is an error and never an empty store.
        std::fs::write(&path, b"{ not json").expect("write");
        assert!(PushStore::load(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn re_registering_replaces_the_endpoint_and_carries_the_sequence_forward() {
        let mut s = PushStore::default();
        let key = crate::b64_encode(&KEY);
        s.register("dev1", "https://ntfy.sh/a", &key, 1).expect("register");
        assert_eq!(s.next_seq("dev1"), Some(1));
        assert_eq!(s.next_seq("dev1"), Some(2));
        s.register("dev1", "https://ntfy.sh/b", &key, 2).expect("re-register");
        assert_eq!(s.registrations.len(), 1, "an abandoned endpoint was kept");
        assert_eq!(s.registrations["dev1"].endpoint, "https://ntfy.sh/b");
        // A sequence that restarted would make every later envelope look like
        // a replay to the phone's own guard.
        assert_eq!(s.next_seq("dev1"), Some(3));
        assert!(s.forget("dev1"));
        assert!(!s.forget("dev1"));
        assert_eq!(s.next_seq("dev1"), None);
    }

    #[test]
    fn a_registration_refuses_a_key_or_endpoint_it_could_not_use() {
        let mut s = PushStore::default();
        assert!(s
            .register("dev1", "http://ntfy.sh/a", &crate::b64_encode(&KEY), 1)
            .is_err());
        assert!(s.register("dev1", "https://ntfy.sh/a", "not base64!!", 1).is_err());
        assert!(s
            .register("dev1", "https://ntfy.sh/a", &crate::b64_encode(&[0u8; 31]), 1)
            .is_err());
        assert!(s.registrations.is_empty(), "a refusal stored something");
    }

    #[test]
    fn the_derived_key_is_not_the_key_that_was_handed_over() {
        // Domain separation, asserted rather than assumed: the bytes that
        // encrypt an envelope must not be the bytes a phone sent, or a key
        // reused for anything else would be the same key.
        let derived = aead_key(&KEY).expect("derive");
        assert_ne!(derived, KEY);
        assert_eq!(aead_key(&KEY).expect("again"), derived, "not deterministic");
        assert_ne!(aead_key(&[8u8; 32]).expect("other"), derived);
        assert!(aead_key(&[0u8; 31]).is_err());
        assert!(aead_key(&[0u8; 33]).is_err());
    }

    #[test]
    fn hmac_sha256_matches_rfc_4231() {
        // The construction is hand-written, so it is checked against a
        // published vector rather than against itself. RFC 4231 test case 1.
        let mac = hmac_sha256(&[0x0b; 20], b"Hi There");
        assert_eq!(
            crate::b64_encode(&mac),
            crate::b64_encode(
                &data_encoding::HEXLOWER
                    .decode(
                        b"b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
                    )
                    .expect("vector")
            )
        );
        // Case 2, a short key.
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            data_encoding::HEXLOWER.encode(&mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // Case 6, a key longer than the 64-byte block, which is the branch
        // that hashes it first.
        let mac = hmac_sha256(&[0xaa; 131], b"Test Using Larger Than Block-Size Key - Hash Key First");
        assert_eq!(
            data_encoding::HEXLOWER.encode(&mac),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn two_envelopes_for_one_moment_share_a_dedup_key_and_two_moments_do_not() {
        // Criterion 4's half that lives on the wire. Four paths inside Rime
        // publish `waiting_for_user` for one turn; what the phone folds them
        // together by is (machine, session, kind), and the machine is the
        // registration. The sequence number must NOT be part of it, or every
        // re-raise would stack a second notification.
        let a = Body { seq: 1, ..body(Kind::Waiting) };
        let b = Body { seq: 2, ..body(Kind::Waiting) };
        assert_eq!(a.dedup_key(), b.dedup_key());
        assert_ne!(a.dedup_key(), Body { kind: Kind::Failed, ..a }.dedup_key());
        assert_ne!(a.dedup_key(), Body { session: 4, ..a }.dedup_key());
        // And the adapter is not in it: the same session cannot change agent,
        // and folding it in would stack two notifications if it ever did.
        assert_eq!(a.dedup_key(), Body { adapter: Adapter::Codex, ..a }.dedup_key());
    }
}
