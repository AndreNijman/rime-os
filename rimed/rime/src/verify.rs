//! Sigstore verification, and the gate that refuses an image which fails it.
//!
//! `trust.rs` answers "has anybody checked this image?". Until this module
//! existed the answer on every Rime machine was no, and the reason was not
//! philosophical: the cryptographic arm of `rime trust --verify` keyed off
//! `/usr/bin/cosign` existing, and **cosign is not packaged for Fedora**.
//! `dnf5 repoquery cosign 'cosign*' 'sigstore*'` is empty across fedora,
//! updates, updates-archive and both RPM Fusion repositories, so that branch
//! was dead code on every machine this project can ship to, and the report
//! always ended "…were not checked".
//!
//! It turns out cosign is not needed. A cosign keyless signature is an OCI
//! artifact whose parts are all standard, and the two tools that read them —
//! `skopeo` and `openssl` — are already in the image. So this module verifies
//! the signature itself.
//!
//! ## What a signature verification consists of here
//!
//! For the `.sig` artifact of a digest, every one of these must hold:
//!
//! 1. the payload blob's sha256 equals the layer digest that named it;
//! 2. the ECDSA signature in `dev.cosignproject.cosign/signature` verifies
//!    over that blob under the public key of the leaf certificate in
//!    `dev.sigstore.cosign/certificate`;
//! 3. the leaf chains to a **pinned** Fulcio root — [`FULCIO_ROOT`], shipped by
//!    the image — with the `dev.sigstore.cosign/chain` annotation supplied only
//!    as untrusted intermediates;
//! 4. the leaf's subject alternative name is the identity this machine expects,
//!    and its Sigstore OIDC-issuer extension is the issuer it expects;
//! 5. the payload binds the digest being verified and the repository it came
//!    from — `critical.image.docker-manifest-digest` and
//!    `critical.identity.docker-reference`.
//!
//! Any of those answering "no" is a failure. Any of them failing to *run* —
//! openssl missing, the pinned root unreadable, the registry unreachable — is
//! not a failure, and is not a pass either. See [`Verdict`].
//!
//! ### The `-attime` trap
//!
//! A Fulcio leaf lives for **ten minutes**. The one on the image the author's
//! L16 is running was valid from 13:43:25 to 13:53:25 on 2026-09-05 and has
//! been expired ever since, so `openssl verify` at the current time reports
//! "certificate has expired" for a perfectly good signature. Verification must
//! therefore be done at a past instant, and *which* instant is a security
//! decision:
//!
//! * The `dev.sigstore.cosign/bundle` annotation carries Rekor's
//!   `integratedTime`, which is the honest answer — but that annotation is not
//!   covered by anything this module verifies, so an attacker who can serve a
//!   manifest can put any number in it. Using it would let a signature made
//!   with a leaked ephemeral key long after its window verify anyway.
//! * The leaf's own `notBefore` is inside the certificate, and the certificate
//!   is authenticated by the chain in step 3. That is what this module uses.
//!
//! What is given up by not verifying the Rekor entry is transparency-log
//! inclusion: this module cannot tell that the signature was ever published to
//! a log, only that a certificate issued to the expected identity signed this
//! exact digest. That is the same posture as `cosign verify
//! --insecure-ignore-tlog`, and the report says so in those words rather than
//! printing an unqualified "verified".
//!
//! ## Provenance
//!
//! The `.att` artifact is a DSSE envelope wrapping an in-toto statement whose
//! predicate is the syft SPDX SBOM. It is verified the same way, over the DSSE
//! pre-authentication encoding, and the statement's subject must be the digest.
//!
//! **No published Rime image has one yet.** The `cosign attest` step exists
//! only on `roadmap/v2.2`; `origin/main`'s `build-image.yml` has no `syft` and
//! no `attest`, and `skopeo inspect` on the booted digest's `.att` tag returns
//! `manifest unknown`. So provenance is `Absent` on every machine today, which
//! is why the shipped default treats it as advisory — see [`Enforcement`].
//!
//! Because that also means the attestation path could not be exercised against
//! a real artifact, an envelope this module cannot parse is reported as
//! [`Verdict::CouldNotRun`] and never as a failure. A format that turns out to
//! differ from the one assumed here must not refuse the first genuinely signed
//! image that carries it.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Map, Value};

use crate::trust::{classify_skopeo_failure, cosign_tag, san_uri, Artifact, Roots};

/// The pinned Fulcio root, shipped by the image.
///
/// Pinning is the whole point: the `chain` annotation in a cosign signature
/// carries Fulcio's intermediate **and root**, and verifying a signature
/// against a root the signature itself supplied proves nothing. The shipped
/// file was taken from `sigstore/root-signing` and matches, fingerprint for
/// fingerprint, the root the real ghcr.io signature carries — which is the
/// check that makes pinning it safe rather than merely different.
pub const FULCIO_ROOT: &str = rime_paths::TRUST_FULCIO_ROOT;

/// An image-owned file that replaces [`crate::trust::EXPECTED_ISSUER`].
///
/// The twin of `trust.rs`'s signer override, and it exists for the same reason
/// plus one more: the test suite mints its own certificate authority and its
/// own identity, and a suite that could not say which issuer to expect would
/// have to skip the identity check — which is one of the two arms that make
/// this verification mean anything.
const ISSUER_OVERRIDE: &str = rime_paths::TRUST_EXPECTED_ISSUER;

/// Where the image ships its enforcement defaults.
const DEFAULTS_PATH: &str = rime_paths::TRUST_ENFORCEMENT;

/// Where an administrator overrides them, per key.
const OVERRIDE_PATH: &str = "/etc/rime/trust.conf";

/// The Sigstore certificate extension holding the OIDC issuer.
///
/// `1.3.6.1.4.1.57264.1.1` is the original form and holds the raw string.
/// `.1.8` is the same value re-encoded as a DER UTF8String, which openssl
/// renders with two bytes of tag and length in front of it. Either is accepted;
/// see [`issuer_from_openssl_text`].
const OID_ISSUER_V1: &str = "1.3.6.1.4.1.57264.1.1";
const OID_ISSUER_V2: &str = "1.3.6.1.4.1.57264.1.8";

const MEDIA_SIMPLE_SIGNING: &str = "application/vnd.dev.cosign.simplesigning.v1+json";
const MEDIA_DSSE: &str = "application/vnd.dsse.envelope.v1+json";

// ── verdicts ─────────────────────────────────────────────────────────────────

/// What verification concluded about one artifact.
///
/// Four values, not two, and not three. The distinction this program keeps
/// re-learning is that "no" and "I could not tell" are different answers; this
/// module needs one more, because "the registry answered and holds nothing" is
/// a measurement about the publisher, while "the registry did not answer" is a
/// measurement about the network. Collapsing those two would either refuse
/// every update whenever a machine is offline, or accept an image nobody ever
/// signed — depending which way round the collapse went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Every check in the module documentation passed.
    Verified {
        /// The identity in the certificate, now actually verified.
        signer: String,
        /// The instant the chain was verified at, and where it came from.
        at: String,
    },
    /// The registry answered, and holds no such artifact for this digest.
    /// Nobody signed it. That is not the same as a signature that is wrong.
    Absent(String),
    /// Verification ran to a conclusion and the conclusion is no.
    Failed(String),
    /// Verification did not run to a conclusion. Never a pass, never a failure.
    CouldNotRun(String),
}

impl Verdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::Verified { .. } => "verified",
            Verdict::Absent(_) => "absent",
            Verdict::Failed(_) => "failed",
            Verdict::CouldNotRun(_) => "could-not-run",
        }
    }

    /// The reason, for every state that has one.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Verdict::Verified { .. } => None,
            Verdict::Absent(w) | Verdict::Failed(w) | Verdict::CouldNotRun(w) => Some(w),
        }
    }

    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("state".into(), Value::from(self.as_str()));
        if let Some(w) = self.reason() {
            m.insert("reason".into(), Value::from(w));
        }
        if let Verdict::Verified { signer, at } = self {
            m.insert("signer".into(), Value::from(signer.clone()));
            m.insert("verifiedAt".into(), Value::from(at.clone()));
        }
        Value::Object(m)
    }
}

/// Both verdicts for one digest.
#[derive(Debug, Clone)]
pub struct Verification {
    /// The digest that was verified — not necessarily the booted one.
    pub digest: String,
    /// The repository the artifacts were looked for in.
    pub repo: String,
    pub signature: Verdict,
    pub provenance: Verdict,
}

// ── enforcement ──────────────────────────────────────────────────────────────

/// How hard one check refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strictness {
    /// Anything short of `Verified` stops the update.
    Enforce,
    /// A `Failed` check stops the update; `Absent` and `CouldNotRun` warn.
    ///
    /// The asymmetry is deliberate. A signature that verifies *wrongly* is the
    /// signature of an attack, and no amount of "the network was flaky" makes
    /// it not one; a signature that is missing, or that could not be fetched,
    /// is a gap and warning about a gap is proportionate.
    Warn,
    /// Report, never refuse.
    Off,
}

impl Strictness {
    pub fn as_str(&self) -> &'static str {
        match self {
            Strictness::Enforce => "enforce",
            Strictness::Warn => "warn",
            Strictness::Off => "off",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "enforce" => Some(Strictness::Enforce),
            "warn" => Some(Strictness::Warn),
            "off" => Some(Strictness::Off),
            _ => None,
        }
    }
}

/// The configured strictness of each check, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enforcement {
    pub signature: Strictness,
    pub provenance: Strictness,
    /// Every line that could not be understood, and every file that could not
    /// be read, so a typo in the config never silently relaxes a check.
    pub notes: Vec<String>,
}

impl Default for Enforcement {
    /// What a machine with no configuration at all does.
    ///
    /// `signature=enforce` because every published Rime image is signed and
    /// always has been: refusing an unsigned one costs nothing and is the
    /// entire point of this module.
    ///
    /// `provenance=warn` because **no published Rime image has an SBOM
    /// attestation**. The `cosign attest` step lives on `roadmap/v2.2` and has
    /// never run on `main`, so `.att` is `manifest unknown` for every digest in
    /// the registry today. Shipping `enforce` would refuse every update on
    /// every machine on the grounds that the publisher has not caught up yet,
    /// which is an outage dressed as a security control.
    fn default() -> Self {
        Self { signature: Strictness::Enforce, provenance: Strictness::Warn, notes: Vec::new() }
    }
}

/// Parse `signature=` / `provenance=` out of one config file's text.
///
/// Anything else is a note rather than an error: this file is edited by hand on
/// a machine whose updates depend on it, and a stray line must not be the
/// difference between updating and not.
pub fn parse_enforcement(text: &str, into: &mut Enforcement, source: &str) {
    for (n, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            into.notes.push(format!("{source}:{}: not a key=value line: {line}", n + 1));
            continue;
        };
        let Some(level) = Strictness::parse(value) else {
            into.notes.push(format!(
                "{source}:{}: {} is not one of enforce, warn, off — leaving {} as it was",
                n + 1,
                value.trim(),
                key.trim()
            ));
            continue;
        };
        match key.trim() {
            "signature" => into.signature = level,
            "provenance" => into.provenance = level,
            other => into
                .notes
                .push(format!("{source}:{}: no such setting: {other}", n + 1)),
        }
    }
}

/// The enforcement this machine is configured for.
///
/// The image's defaults first, then the administrator's overrides on top, per
/// key. A file that is simply not there is normal and silent; a file that
/// exists and cannot be read is a note, because an unreadable policy file must
/// never quietly become a permissive one.
pub fn enforcement(roots: &Roots) -> Enforcement {
    let mut e = Enforcement::default();
    for path in [DEFAULTS_PATH, OVERRIDE_PATH] {
        match roots.read_optional(path) {
            Ok(Some(text)) => parse_enforcement(&text, &mut e, path),
            Ok(None) => {}
            Err(why) => e.notes.push(format!("{why} — using the built-in default")),
        }
    }
    e
}

/// What the update path should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Nothing to say. Deploy.
    Proceed,
    /// Deploy, but tell the user what was not established.
    ProceedWithWarnings(Vec<String>),
    /// Do not deploy. `failed` names which checks refused, in the words the
    /// user needs — "signature", "provenance", or both.
    Refuse { refused_by: Vec<String>, lines: Vec<String> },
}

impl Decision {
    pub fn refuses(&self) -> bool {
        matches!(self, Decision::Refuse { .. })
    }
}

/// One check's contribution to the decision.
///
/// Pure, exhaustive over the 4 × 3 table, and the only place the policy lives.
/// `ops::update`, `rime trust` and the JSON all read their answer from here, so
/// there is no second implementation to drift.
fn arm(what: &str, v: &Verdict, how: Strictness) -> Result<Option<String>, String> {
    let detail = |head: &str| match v.reason() {
        Some(w) => format!("{head}: {w}"),
        None => head.to_string(),
    };
    match (v, how) {
        (Verdict::Verified { .. }, _) => Ok(None),
        (_, Strictness::Off) => Ok(Some(detail(&format!(
            "the {what} was not established, and {what} checking is switched off"
        )))),
        (Verdict::Failed(_), _) => Err(detail(&format!("the {what} does not verify"))),
        (Verdict::Absent(_), Strictness::Enforce) => {
            Err(detail(&format!("this image has no {what}")))
        }
        (Verdict::CouldNotRun(_), Strictness::Enforce) => {
            Err(detail(&format!("the {what} could not be checked")))
        }
        (Verdict::Absent(_), Strictness::Warn) => {
            Ok(Some(detail(&format!("this image has no {what}"))))
        }
        (Verdict::CouldNotRun(_), Strictness::Warn) => {
            Ok(Some(detail(&format!("the {what} could not be checked"))))
        }
    }
}

/// Turn two verdicts and a configuration into a decision.
pub fn decide(v: &Verification, e: &Enforcement) -> Decision {
    let mut refused_by = Vec::new();
    let mut lines = Vec::new();
    let mut warnings = Vec::new();

    for (what, verdict, how) in [
        ("signature", &v.signature, e.signature),
        ("provenance", &v.provenance, e.provenance),
    ] {
        match arm(what, verdict, how) {
            Ok(None) => {}
            Ok(Some(w)) => warnings.push(w),
            Err(line) => {
                refused_by.push(what.to_string());
                lines.push(line);
            }
        }
    }

    if !refused_by.is_empty() {
        // The warnings ride along: a refusal that mentions only the check that
        // failed, on a machine where the other one was also never established,
        // sends the reader to fix half the problem.
        lines.extend(warnings);
        return Decision::Refuse { refused_by, lines };
    }
    if warnings.is_empty() {
        Decision::Proceed
    } else {
        Decision::ProceedWithWarnings(warnings)
    }
}

// ── time ─────────────────────────────────────────────────────────────────────

/// Seconds since the epoch from openssl's `-dateopt iso_8601` rendering.
///
/// `openssl x509 -noout -startdate -dateopt iso_8601` prints
/// `notBefore=2026-09-05 13:43:25Z`. This crate has no date library and one
/// would be a dependency for a single conversion, so the civil-to-days
/// algorithm is here, with the epoch check as a test. Shelling out to `date -d`
/// was the alternative and would put locale parsing in the middle of a
/// signature check.
pub fn unix_from_iso(text: &str) -> Result<i64, String> {
    let s = text.trim().trim_start_matches("notBefore=").trim_start_matches("notAfter=").trim();
    let s = s.trim_end_matches('Z').trim();
    let (date, time) = s
        .split_once([' ', 'T'])
        .ok_or_else(|| format!("not an ISO-8601 instant: {text:?}"))?;
    let d: Vec<&str> = date.split('-').collect();
    let t: Vec<&str> = time.split(':').collect();
    if d.len() != 3 || t.len() != 3 {
        return Err(format!("not an ISO-8601 instant: {text:?}"));
    }
    let num = |v: &str| -> Result<i64, String> {
        v.parse::<i64>().map_err(|e| format!("{v:?} in {text:?}: {e}"))
    };
    let (y, mo, da) = (num(d[0])?, num(d[1])?, num(d[2])?);
    let (h, mi, se) = (num(t[0])?, num(t[1])?, num(t[2])?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&da) {
        return Err(format!("not a real date: {text:?}"));
    }
    Ok(days_from_civil(y, mo, da) * 86_400 + h * 3600 + mi * 60 + se)
}

/// Days from 1970-01-01 to a proleptic-Gregorian date. Hinnant's algorithm.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

// ── certificate reading ──────────────────────────────────────────────────────

/// The OIDC issuer out of a certificate's Sigstore extension.
///
/// openssl renders an unknown extension by printing the OID, a colon, and the
/// value on the following line. For `1.3.6.1.4.1.57264.1.1` that value is the
/// issuer string; for `.1.8` it is the same string wrapped in a DER
/// UTF8String, so openssl prints two bytes of tag and length in front of it and
/// the line reads `.+https://token.actions.githubusercontent.com`. Both forms
/// are accepted, and the wrapper is peeled by taking the tail — never by
/// searching for the expected value inside the line, which would let
/// `https://evil.example/?x=https://token.actions.githubusercontent.com`
/// match.
pub fn issuer_from_openssl_text(text: &str) -> Option<String> {
    let mut best: Option<String> = None;
    let mut lines = text.lines().peekable();
    while let Some(l) = lines.next() {
        let t = l.trim();
        let is_v1 = t == format!("{OID_ISSUER_V1}:");
        let is_v2 = t == format!("{OID_ISSUER_V2}:");
        if !is_v1 && !is_v2 {
            continue;
        }
        let Some(raw) = lines.peek() else { continue };
        let value = raw.trim();
        // Peel at most a two-byte DER header. Anything longer is not a
        // UTF8String wrapper and is left alone so it fails the comparison.
        let peeled = match value.char_indices().find(|(_, c)| *c == 'h') {
            Some((i, _)) if i <= 2 => &value[i..],
            _ => value,
        };
        if peeled.is_empty() {
            continue;
        }
        // v1 wins when both are present: it is the unwrapped form.
        if is_v1 || best.is_none() {
            best = Some(peeled.to_string());
        }
        if is_v1 {
            return best;
        }
    }
    best
}

/// What the payload of a cosign simple-signing blob must say.
///
/// The signature proves that *something* was signed by the expected identity.
/// Without this check, a valid signature over a different image from the same
/// publisher would verify — which is precisely the substitution a moved tag
/// makes easy.
pub fn simple_signing_binds(payload: &str, repo: &str, digest: &str) -> Result<(), String> {
    let doc: Value = serde_json::from_str(payload)
        .map_err(|e| format!("the signed payload is not JSON: {e}"))?;
    let critical = doc
        .get("critical")
        .ok_or_else(|| "the signed payload has no `critical` section".to_string())?;
    let signed_digest = critical
        .get("image")
        .and_then(|i| i.get("docker-manifest-digest"))
        .and_then(Value::as_str)
        .ok_or_else(|| "the signed payload names no manifest digest".to_string())?;
    if signed_digest != digest {
        return Err(format!(
            "the signature covers {signed_digest}, not the {digest} being deployed"
        ));
    }
    let signed_repo = critical
        .get("identity")
        .and_then(|i| i.get("docker-reference"))
        .and_then(Value::as_str)
        .ok_or_else(|| "the signed payload names no repository".to_string())?;
    // Compared without a tag on either side: cosign records the repository, and
    // the four Rime tags are aliases for one digest, so a tag comparison would
    // fail on an image that is genuinely the right one.
    if repo_of(signed_repo) != repo_of(repo) {
        return Err(format!(
            "the signature covers an image in {signed_repo}, not in {repo}"
        ));
    }
    Ok(())
}

/// What the in-toto statement inside a DSSE envelope must say.
pub fn intoto_binds(statement: &str, digest: &str) -> Result<(), String> {
    let doc: Value = serde_json::from_str(statement)
        .map_err(|e| format!("the attested statement is not JSON: {e}"))?;
    let hex = digest.split_once(':').map(|(_, h)| h).unwrap_or(digest);
    let subjects = doc
        .get("subject")
        .and_then(Value::as_array)
        .ok_or_else(|| "the attested statement has no subject".to_string())?;
    let covered = subjects.iter().any(|s| {
        s.get("digest")
            .and_then(|d| d.get("sha256"))
            .and_then(Value::as_str)
            .is_some_and(|h| h.eq_ignore_ascii_case(hex))
    });
    if !covered {
        return Err(format!("the attestation's subject is not {digest}"));
    }
    let kind = doc.get("predicateType").and_then(Value::as_str).unwrap_or("");
    if !kind.contains("spdx") {
        return Err(format!(
            "the attestation is a {kind} predicate, not the SPDX SBOM this image publishes"
        ));
    }
    Ok(())
}

/// The DSSE pre-authentication encoding: what a DSSE signature is actually over.
///
/// `DSSEv1 SP len(payloadType) SP payloadType SP len(payload) SP payload`, with
/// the payload as raw bytes after base64 decoding — not the base64 text.
pub fn pae(payload_type: &str, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + payload_type.len() + 32);
    out.extend_from_slice(b"DSSEv1 ");
    out.extend_from_slice(payload_type.len().to_string().as_bytes());
    out.push(b' ');
    out.extend_from_slice(payload_type.as_bytes());
    out.push(b' ');
    out.extend_from_slice(payload.len().to_string().as_bytes());
    out.push(b' ');
    out.extend_from_slice(payload);
    out
}

/// A repository reference with any tag or digest removed.
fn repo_of(reference: &str) -> &str {
    let head = reference.split_once('@').map(|(r, _)| r).unwrap_or(reference);
    match head.rsplit_once(':') {
        // A colon after the last slash is a tag. A colon before it is a port.
        Some((r, _)) if head.rfind(':') > head.rfind('/') => r,
        _ => head,
    }
}

// ── base64, without a dependency ─────────────────────────────────────────────

/// Decode standard base64, ignoring whitespace. Returns None on anything else.
///
/// The alternative was a new dependency in the crate that verifies signatures,
/// which is the crate where a new dependency is least welcome.
pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut pad = 0;
    for c in s.chars() {
        if c.is_whitespace() {
            continue;
        }
        if c == '=' {
            pad += 1;
            continue;
        }
        if pad > 0 {
            return None;
        }
        let v = match c {
            'A'..='Z' => c as u32 - 'A' as u32,
            'a'..='z' => c as u32 - 'a' as u32 + 26,
            '0'..='9' => c as u32 - '0' as u32 + 52,
            '+' => 62,
            '/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    if pad > 2 || (acc & ((1 << bits) - 1)) != 0 {
        return None;
    }
    Some(out)
}

// ── fetching ─────────────────────────────────────────────────────────────────

/// One fetched cosign artifact: its manifest, and the directory its blobs are in.
pub struct Fetched {
    pub manifest: String,
    pub blobs: PathBuf,
    /// Kept so the temporary directory outlives the verification.
    _keep: Option<TempDir>,
}

/// A private temporary directory that removes itself.
pub struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Result<Self, String> {
        // Nanosecond-and-pid naming rather than a counter: two `rime trust`
        // runs at once must not share a directory that one of them deletes.
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let safe: String = tag.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        let p = std::env::temp_dir().join(format!("rime-verify-{}-{stamp}-{safe}", std::process::id()));
        std::fs::create_dir_all(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        Ok(TempDir(p))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Ask the registry for one cosign artifact and put its parts on disk.
///
/// `skopeo copy … dir:` rather than `skopeo inspect` plus a blob fetch: it is
/// one network round trip, it writes the raw manifest beside the blobs, and it
/// validates every blob's digest against the manifest on the way in.
///
/// Under a fixture root nothing is spawned. The fixture supplies
/// `registry/<tag>/manifest.json` plus the blobs, or `registry/<tag>.error`
/// with the text of a registry failure, or neither — which is the registry
/// answering that it holds nothing.
/// Whether a fixture root exists but supplies no `registry/` tree.
///
/// The distinction the shell suite depends on. Most fixture roots in
/// `tests/test-rime-trust.sh` exist to exercise the OFFLINE report — an origin
/// file, a `policy.json` — and `--verify` against one of those must stay the
/// no-op it has always been, because reporting "the registry holds no
/// signature" there would be a fixture's shape printed as a fact about an
/// image. A fixture that DOES carry `registry/` is asking for the verification
/// path, and gets it, without a byte of network.
pub fn fixture_without_registry(roots: &Roots) -> bool {
    roots.fixture.is_some() && !roots.path("/registry").is_dir()
}

pub fn fetch(roots: &Roots, repo: &str, tag: &str) -> Result<Fetched, Artifact> {
    if roots.fixture.is_some() {
        let dir = roots.path(&format!("/registry/{tag}"));
        let err = roots.path(&format!("/registry/{tag}.error"));
        if let Ok(text) = std::fs::read_to_string(&err) {
            return Err(classify_skopeo_failure(&text));
        }
        let manifest = dir.join("manifest.json");
        return match std::fs::read_to_string(&manifest) {
            Ok(m) => Ok(Fetched { manifest: m, blobs: dir, _keep: None }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(Artifact::Absent)
            }
            Err(e) => Err(Artifact::Unavailable(format!("{}: {e}", manifest.display()))),
        };
    }

    let tmp = TempDir::new(tag).map_err(Artifact::Unavailable)?;
    let out = Command::new(rime_paths::SKOPEO)
        .args(["copy", "--quiet", &format!("docker://{repo}:{tag}")])
        .arg(format!("dir:{}", tmp.path().display()))
        .output()
        .map_err(|e| Artifact::Unavailable(format!("could not run skopeo: {e}")))?;
    if !out.status.success() {
        return Err(classify_skopeo_failure(&String::from_utf8_lossy(&out.stderr)));
    }
    let manifest = tmp.path().join("manifest.json");
    let text = std::fs::read_to_string(&manifest)
        .map_err(|e| Artifact::Unavailable(format!("{}: {e}", manifest.display())))?;
    let blobs = tmp.path().to_path_buf();
    Ok(Fetched { manifest: text, blobs, _keep: Some(tmp) })
}

/// The bytes of the first layer of `media` in the artifact at `repo:tag`,
/// having first confirmed that the manifest served is the one named by
/// `expect`.
///
/// **`expect` is the whole point of this function.** The caller resolved the
/// tag to a digest and verified a signature over THAT DIGEST; this then fetches
/// by tag, which is a second round trip to a registry that is free to answer
/// differently. Without this check the sequence would be "verify one object,
/// read another", and a registry that moved the tag between the two calls —
/// or an attacker who could — would have its bytes parsed and acted on with a
/// signature over something else standing behind them. The signature is not
/// over the tag. It is over the digest, and the digest is what has to be read.
///
/// The blob is then checked against the digest its manifest names — `blob` does
/// that for every caller — so the chain is complete: signature over the
/// manifest digest, manifest digest over the manifest, manifest over the blob.
pub fn artifact_layer_at(
    roots: &Roots,
    repo: &str,
    tag: &str,
    expect: &str,
    media: &str,
) -> Result<Vec<u8>, String> {
    let fetched = match fetch(roots, repo, tag) {
        Ok(f) => f,
        Err(Artifact::Absent) => return Err(format!("{repo}:{tag} does not exist")),
        Err(Artifact::Unavailable(why)) => return Err(why),
    };
    let path = fetched.blobs.join("manifest.json");
    let got = sha256_of(&path)?;
    let want = expect.split_once(':').map(|(_, h)| h).unwrap_or(expect);
    if !got.eq_ignore_ascii_case(want) {
        return Err(format!(
            "{repo}:{tag} served sha256:{got}, and the signature that was checked is over {expect}; \
             the tag moved between resolving it and reading it, so nothing was read"
        ));
    }
    let layers = layers_of(&fetched.manifest, media)?;
    let layer = layers
        .first()
        .ok_or_else(|| format!("{repo}:{tag} carries no {media} layer"))?;
    blob(&fetched, layer)
}

/// The sha256 of a file, lower-case hex, through the openssl this program
/// already depends on rather than a second hashing crate.
fn sha256_of(path: &Path) -> Result<String, String> {
    let out = Command::new(rime_paths::OPENSSL)
        .args(["dgst", "-sha256", "-r"])
        .arg(path)
        .output()
        .map_err(|e| format!("could not run openssl: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "could not hash {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text.split_whitespace().next().unwrap_or("").to_ascii_lowercase())
}

/// Whether a [`resolve`] failure means the registry ANSWERED and holds no such
/// tag, as opposed to not having answered at all.
///
/// The distinction is the difference between two different facts, and a caller
/// that conflates them refuses on the wrong one. `rime channel set` is the
/// caller that cares: a tag the registry does not serve is a switch that would
/// strand the machine and must be refused, while a registry that could not be
/// reached is a fact about the network — the user's aeroplane, their captive
/// portal — and refusing a configuration change over that would be this
/// program deciding a laptop may not be configured offline.
///
/// One spelling, [`classify_skopeo_failure`]'s, reused rather than a second
/// pattern: two lists of registry error strings drift, and the one that drifts
/// is the one nobody is testing that day.
pub fn tag_is_absent(err: &str) -> bool {
    matches!(classify_skopeo_failure(err), Artifact::Absent)
}

/// The digest the registry currently serves for a tag.
///
/// This is the load-bearing difference between a readout and a gate: `rime
/// trust` verifies what the machine is *running*, and an update has to verify
/// what it is *about to run*. On Rime those are routinely different for the
/// same tag — `rime`, `daily`, `gaming-mesa` and `gaming-nvidia` are four
/// aliases for one digest that moves on every successful main build.
pub fn resolve(roots: &Roots, reference: &str) -> Result<String, String> {
    if roots.fixture.is_some() {
        // Per-tag first, then the single answer. A fixture used to resolve one
        // tag; the rollout document means one machine resolves TWO — its
        // channel and `:rollout` — and a fixture that answered the same digest
        // for both would make the document's digest binding untestable, which
        // is the one check that keeps a stale entry from gating a build it was
        // never about.
        // The last colon, but only after the last slash — a colon before that
        // is a port on the registry host, not a tag.
        let tag = match (reference.rfind(':'), reference.rfind('/')) {
            (Some(c), Some(sl)) if c > sl => &reference[c + 1..],
            (Some(c), None) => &reference[c + 1..],
            _ => "",
        };
        let per_tag = if tag.is_empty() { String::new() } else { format!(".{tag}") };
        for suffix in [per_tag, String::new()] {
            if let Ok(Some(why)) = roots.read_optional(&format!("/registry/resolve{suffix}.error")) {
                return Err(why.trim().to_string());
            }
            if let Some(d) = roots
                .read_optional(&format!("/registry/resolve{suffix}"))
                .map_err(|e| e.to_string())?
            {
                return Ok(d.trim().to_string());
            }
        }
        return Err("this fixture root resolves no tag".to_string());
    }
    let out = Command::new(rime_paths::SKOPEO)
        .args(["inspect", "--format", "{{.Digest}}", &format!("docker://{reference}")])
        .output()
        .map_err(|e| format!("could not run skopeo: {e}"))?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        let line = why.lines().last().map(str::trim).unwrap_or("skopeo failed without a message");
        return Err(line.to_string());
    }
    let d = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !d.starts_with("sha256:") {
        return Err(format!("the registry answered {d:?}, which is not a digest"));
    }
    Ok(d)
}

// ── the cryptography ─────────────────────────────────────────────────────────

/// An openssl invocation that did not run, as distinct from one that said no.
struct Openssl {
    ran: bool,
    ok: bool,
    stdout: String,
    stderr: String,
}

fn openssl(args: &[&str], paths: &[&Path]) -> Openssl {
    let mut cmd = Command::new(rime_paths::OPENSSL);
    cmd.args(args);
    for p in paths {
        cmd.arg(p);
    }
    match cmd.output() {
        Ok(o) => Openssl {
            ran: true,
            ok: o.status.success(),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).trim().to_string(),
        },
        Err(e) => Openssl {
            ran: false,
            ok: false,
            stdout: String::new(),
            stderr: format!("could not run openssl: {e}"),
        },
    }
}

/// The reason out of `openssl verify`'s output.
///
/// Measured, because the obvious reading is wrong. `openssl verify` prints the
/// SUBJECT of the offending certificate on its first line and the reason on
/// the second:
///
/// ```text
/// O=sigstore.dev, CN=sigstore
/// error 9 at 2 depth lookup: certificate is not yet valid
/// error leaf.pem: verification failed
/// ```
///
/// Taking `lines().next()` — which this function replaced — put a
/// distinguished name where a refusal's reason should be, so the one line a
/// user reads when their machine will not update said `O=sigstore.dev,
/// CN=sigstore` instead of telling them what was wrong with it.
fn openssl_verify_reason(stderr: &str) -> String {
    stderr
        .lines()
        .map(str::trim)
        .find(|l| l.contains("depth lookup:"))
        .and_then(|l| l.split_once("depth lookup:").map(|(_, why)| why.trim().to_string()))
        // No `depth lookup:` line at all: report every non-empty line rather
        // than guessing which one mattered. An empty reason is worse than a
        // verbose one here.
        .unwrap_or_else(|| {
            let all: Vec<&str> = stderr.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
            if all.is_empty() { "verification failed".to_string() } else { all.join("; ") }
        })
}

/// The verification of one certificate-and-signature pair, shared by the
/// signature and provenance paths.
///
/// `signed` is the exact byte string the signature is over: the payload blob
/// for a cosign signature, the DSSE pre-authentication encoding for an
/// attestation.
/// One certificate-and-signature pair, and the exact bytes it covers.
///
/// Grouped rather than passed as eight arguments because the four travel
/// together and are meaningless apart: a certificate without the signature it
/// made, or a signature without the byte string it is over, cannot be checked
/// at all.
struct Candidate<'a> {
    leaf_pem: &'a str,
    /// Untrusted intermediates, never a root. See [`FULCIO_ROOT`].
    chain_pem: Option<&'a str>,
    signature_b64: &'a str,
    /// The payload blob for a cosign signature; the DSSE pre-authentication
    /// encoding for an attestation.
    signed: &'a [u8],
}

/// Who this machine will accept a signature from.
pub struct Expect {
    /// Every identity accepted; a signature from any one of them verifies.
    pub signers: Vec<String>,
    pub issuer: String,
}

fn verify_signed_bytes(roots: &Roots, work: &Path, c: &Candidate<'_>, want: &Expect) -> Verdict {
    let (leaf_pem, chain_pem, signature_b64, signed) =
        (c.leaf_pem, c.chain_pem, c.signature_b64, c.signed);
    let expect_issuer = want.issuer.as_str();
    let leaf = work.join("leaf.pem");
    if let Err(e) = std::fs::write(&leaf, leaf_pem) {
        return Verdict::CouldNotRun(format!("{}: {e}", leaf.display()));
    }
    let payload = work.join("signed.bin");
    if let Err(e) = std::fs::write(&payload, signed) {
        return Verdict::CouldNotRun(format!("{}: {e}", payload.display()));
    }
    let Some(sig) = b64_decode(signature_b64) else {
        return Verdict::Failed("the signature is not valid base64".to_string());
    };
    let sigfile = work.join("sig.der");
    if let Err(e) = std::fs::write(&sigfile, &sig) {
        return Verdict::CouldNotRun(format!("{}: {e}", sigfile.display()));
    }

    // 1. the public key out of the leaf.
    let pub_out = openssl(&["x509", "-pubkey", "-noout", "-in"], &[&leaf]);
    if !pub_out.ran {
        return Verdict::CouldNotRun(pub_out.stderr);
    }
    if !pub_out.ok {
        return Verdict::Failed(format!("the certificate does not parse: {}", pub_out.stderr));
    }
    let pubkey = work.join("pub.pem");
    if let Err(e) = std::fs::write(&pubkey, &pub_out.stdout) {
        return Verdict::CouldNotRun(format!("{}: {e}", pubkey.display()));
    }

    // 2. the signature over the signed bytes.
    let dgst = openssl(
        &["dgst", "-sha256", "-verify", &pubkey.display().to_string(), "-signature", &sigfile.display().to_string()],
        &[&payload],
    );
    if !dgst.ran {
        return Verdict::CouldNotRun(dgst.stderr);
    }
    if !dgst.ok {
        return Verdict::Failed(
            "the signature does not verify against the certificate's public key".to_string(),
        );
    }

    // 3. the chain, at the leaf's own notBefore. See the module note on why
    //    not the bundle's integratedTime.
    let start = openssl(&["x509", "-noout", "-startdate", "-dateopt", "iso_8601", "-in"], &[&leaf]);
    if !start.ran {
        return Verdict::CouldNotRun(start.stderr);
    }
    if !start.ok {
        return Verdict::Failed(format!("the certificate has no start date: {}", start.stderr));
    }
    let at = match unix_from_iso(&start.stdout) {
        Ok(t) => t,
        Err(e) => return Verdict::Failed(format!("the certificate's start date is unreadable: {e}")),
    };
    let root_pem = match roots.read(FULCIO_ROOT) {
        Ok(s) => s,
        Err(e) => {
            return Verdict::CouldNotRun(format!(
                "the pinned Fulcio root could not be read, so nothing could be checked \
                 against it: {e}"
            ))
        }
    };
    let rootfile = work.join("root.pem");
    if let Err(e) = std::fs::write(&rootfile, &root_pem) {
        return Verdict::CouldNotRun(format!("{}: {e}", rootfile.display()));
    }
    let mut verify_args: Vec<String> = vec![
        "verify".into(),
        "-CAfile".into(),
        rootfile.display().to_string(),
        "-attime".into(),
        at.to_string(),
    ];
    if let Some(chain) = chain_pem {
        let chainfile = work.join("chain.pem");
        if let Err(e) = std::fs::write(&chainfile, chain) {
            return Verdict::CouldNotRun(format!("{}: {e}", chainfile.display()));
        }
        verify_args.push("-untrusted".into());
        verify_args.push(chainfile.display().to_string());
    }
    let borrowed: Vec<&str> = verify_args.iter().map(String::as_str).collect();
    let chained = openssl(&borrowed, &[&leaf]);
    if !chained.ran {
        return Verdict::CouldNotRun(chained.stderr);
    }
    if !chained.ok {
        return Verdict::Failed(format!(
            "the signing certificate does not chain to the pinned Fulcio root: {}",
            openssl_verify_reason(&chained.stderr)
        ));
    }

    // 4. who signed, and who said so.
    let text = openssl(&["x509", "-noout", "-text", "-in"], &[&leaf]);
    if !text.ran {
        return Verdict::CouldNotRun(text.stderr);
    }
    if !text.ok {
        return Verdict::Failed(format!("the certificate does not print: {}", text.stderr));
    }
    let signer = match san_uri(&text.stdout) {
        Some(s) => s,
        None => {
            return Verdict::Failed(
                "the signing certificate names no identity in its subject alternative name"
                    .to_string(),
            )
        }
    };
    if !want.signers.contains(&signer) {
        return Verdict::Failed(format!(
            "signed by {signer}, and this machine expects {}",
            want.signers.join(" or ")
        ));
    }
    let issuer = match issuer_from_openssl_text(&text.stdout) {
        Some(s) => s,
        None => {
            return Verdict::Failed(
                "the signing certificate carries no Sigstore OIDC issuer extension".to_string(),
            )
        }
    };
    if issuer != expect_issuer {
        return Verdict::Failed(format!(
            "the identity was issued by {issuer}, and this machine expects {expect_issuer}"
        ));
    }

    Verdict::Verified {
        signer,
        at: format!(
            "the certificate's own notBefore, unix {at}; the transparency log was not checked"
        ),
    }
}

// ── the two artifacts ────────────────────────────────────────────────────────

/// The identity and issuer this machine expects, and nothing inferred.
pub fn expectations(roots: &Roots) -> Expect {
    let signers = crate::trust::expected_signers(roots);
    let issuer = match roots.read_optional(ISSUER_OVERRIDE) {
        Ok(Some(s)) if !s.trim().is_empty() => s.trim().to_string(),
        _ => crate::trust::EXPECTED_ISSUER.to_string(),
    };
    Expect { signers, issuer }
}

/// Who this machine will accept a ROLLOUT DOCUMENT from.
///
/// A different workflow signs it — the image is signed per build by
/// `build-image.yml` and the document has to be publishable between builds —
/// so it is a different Sigstore identity, and verifying the document against
/// the image's identity would refuse every document ever published.
///
/// Same issuer, because it is the same GitHub Actions token endpoint.
pub fn rollout_expectations(roots: &Roots) -> Expect {
    let issuer = match roots.read_optional(ISSUER_OVERRIDE) {
        Ok(Some(s)) if !s.trim().is_empty() => s.trim().to_string(),
        _ => crate::trust::EXPECTED_ISSUER.to_string(),
    };
    Expect { signers: crate::trust::expected_rollout_signers(roots), issuer }
}

/// One layer's cosign annotations.
struct Annotations {
    signature: String,
    certificate: String,
    chain: Option<String>,
}

fn annotations(layer: &Value) -> Option<Annotations> {
    let a = layer.get("annotations")?;
    let get = |k: &str| a.get(k).and_then(Value::as_str).map(str::to_string);
    Some(Annotations {
        // `dev.cosignproject.cosign/signature` is what cosign actually writes —
        // measured on the real ghcr.io artifact, not assumed. The
        // `dev.sigstore.cosign/` spelling is accepted because older and
        // third-party signers use it, and reading only one of the two is how a
        // valid signature reports as "no signature annotation".
        signature: get("dev.cosignproject.cosign/signature")
            .or_else(|| get("dev.sigstore.cosign/signature"))?,
        certificate: get("dev.sigstore.cosign/certificate")?,
        chain: get("dev.sigstore.cosign/chain"),
    })
}

/// The blob a layer names, checked against the digest that named it.
fn blob(fetched: &Fetched, layer: &Value) -> Result<Vec<u8>, String> {
    let digest = layer
        .get("digest")
        .and_then(Value::as_str)
        .ok_or_else(|| "a layer has no digest".to_string())?;
    let hex = digest.split_once(':').map(|(_, h)| h).unwrap_or(digest);
    let path = fetched.blobs.join(hex);
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    // `skopeo copy` already validates this on the way in, but the fixture path
    // does not go through skopeo, and a check that only runs in production is a
    // check nobody has ever seen run.
    let out = Command::new(rime_paths::OPENSSL)
        .args(["dgst", "-sha256", "-r"])
        .arg(&path)
        .output()
        .map_err(|e| format!("could not run openssl: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "could not hash {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let got = text.split_whitespace().next().unwrap_or("");
    if !got.eq_ignore_ascii_case(hex) {
        return Err(format!("the blob is {got}, and its manifest says {hex}"));
    }
    Ok(bytes)
}

/// Layers of one media type, in manifest order.
fn layers_of(manifest: &str, media: &str) -> Result<Vec<Value>, String> {
    let doc: Value = serde_json::from_str(manifest)
        .map_err(|e| format!("the artifact manifest is not JSON: {e}"))?;
    let all = doc
        .get("layers")
        .and_then(Value::as_array)
        .ok_or_else(|| "the artifact manifest has no layers".to_string())?;
    Ok(all
        .iter()
        .filter(|l| l.get("mediaType").and_then(Value::as_str) == Some(media))
        .cloned()
        .collect())
}

/// Turn a fetch failure into a verdict.
fn from_artifact(a: Artifact, what: &str) -> Verdict {
    match a {
        // The registry answered. Nobody signed this digest.
        Artifact::Absent => Verdict::Absent(format!("the registry holds no {what} for this digest")),
        // Distance, not absence — and the reason is carried so no caller can
        // read a network failure as an unsigned image. This is the mapping the
        // gate's `an_unreachable_registry_is_never_refused_as_unsigned` test
        // exists to hold down.
        Artifact::Unavailable(why) => Verdict::CouldNotRun(why),
    }
}

/// Verify the `.sig` artifact for a digest, against the image signer.
pub fn verify_signature(roots: &Roots, repo: &str, digest: &str) -> Verdict {
    verify_signature_as(roots, repo, digest, &expectations(roots))
}

/// Verify the `.sig` artifact for a digest against a named identity.
///
/// Split out from [`verify_signature`] rather than duplicated: the rollout
/// document is an ordinary cosign-signed object in the same repository, and a
/// second verifier for it would be a second place for this to be got wrong.
pub fn verify_signature_as(roots: &Roots, repo: &str, digest: &str, want: &Expect) -> Verdict {
    let Some(tag) = cosign_tag(digest, ".sig") else {
        return Verdict::CouldNotRun(format!("unusable digest: {digest}"));
    };
    let fetched = match fetch(roots, repo, &tag) {
        Ok(f) => f,
        Err(a) => return from_artifact(a, "signature"),
    };
    let layers = match layers_of(&fetched.manifest, MEDIA_SIMPLE_SIGNING) {
        Ok(l) => l,
        Err(e) => return Verdict::CouldNotRun(e),
    };
    if layers.is_empty() {
        // The tag exists but carries nothing this module recognises. Not a
        // failed signature — an artifact of a shape it cannot judge.
        return Verdict::CouldNotRun(format!(
            "{repo}:{tag} carries no {MEDIA_SIMPLE_SIGNING} layer"
        ));
    }
    let mut last = Verdict::CouldNotRun("no layer was examined".to_string());
    // Every layer, and any one verifying is enough: a re-signed image carries
    // more than one signature, and taking only the first would refuse it.
    for layer in &layers {
        let Some(ann) = annotations(layer) else {
            last = Verdict::Failed(
                "a signature layer carries no certificate and signature pair".to_string(),
            );
            continue;
        };
        let bytes = match blob(&fetched, layer) {
            Ok(b) => b,
            Err(e) => {
                last = Verdict::CouldNotRun(e);
                continue;
            }
        };
        let payload = String::from_utf8_lossy(&bytes).into_owned();
        if let Err(e) = simple_signing_binds(&payload, repo, digest) {
            last = Verdict::Failed(e);
            continue;
        }
        let work = match TempDir::new("sig") {
            Ok(w) => w,
            Err(e) => return Verdict::CouldNotRun(e),
        };
        let v = verify_signed_bytes(
            roots,
            work.path(),
            &Candidate {
                leaf_pem: &ann.certificate,
                chain_pem: ann.chain.as_deref(),
                signature_b64: &ann.signature,
                signed: &bytes,
            },
            want,
        );
        if matches!(v, Verdict::Verified { .. }) {
            return v;
        }
        last = v;
    }
    last
}

/// Verify the `.att` artifact — the signed SPDX SBOM — for a digest.
pub fn verify_provenance(roots: &Roots, repo: &str, digest: &str) -> Verdict {
    let Some(tag) = cosign_tag(digest, ".att") else {
        return Verdict::CouldNotRun(format!("unusable digest: {digest}"));
    };
    let fetched = match fetch(roots, repo, &tag) {
        Ok(f) => f,
        Err(a) => return from_artifact(a, "SBOM attestation"),
    };
    let layers = match layers_of(&fetched.manifest, MEDIA_DSSE) {
        Ok(l) => l,
        Err(e) => return Verdict::CouldNotRun(e),
    };
    if layers.is_empty() {
        return Verdict::CouldNotRun(format!("{repo}:{tag} carries no {MEDIA_DSSE} layer"));
    }
    let want = expectations(roots);
    let mut last = Verdict::CouldNotRun("no envelope was examined".to_string());
    for layer in &layers {
        let bytes = match blob(&fetched, layer) {
            Ok(b) => b,
            Err(e) => {
                last = Verdict::CouldNotRun(e);
                continue;
            }
        };
        // Everything about the envelope's *shape* is CouldNotRun rather than
        // Failed. This path has never met a real attestation — the publisher
        // has not produced one yet — so a shape this code does not know is far
        // more likely to be this code's gap than an attack, and refusing an
        // update on the strength of a guess is the failure mode the whole
        // module is built to avoid.
        let envelope: Value = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(e) => {
                last = Verdict::CouldNotRun(format!("the attestation is not a DSSE envelope: {e}"));
                continue;
            }
        };
        let (Some(payload_type), Some(payload_b64)) = (
            envelope.get("payloadType").and_then(Value::as_str),
            envelope.get("payload").and_then(Value::as_str),
        ) else {
            last = Verdict::CouldNotRun(
                "the DSSE envelope has no payloadType and payload pair".to_string(),
            );
            continue;
        };
        let Some(payload) = b64_decode(payload_b64) else {
            last = Verdict::CouldNotRun("the DSSE payload is not valid base64".to_string());
            continue;
        };
        let statement = String::from_utf8_lossy(&payload).into_owned();
        // The binding, by contrast, IS a real failure: the envelope parsed, it
        // says what it covers, and what it covers is not this image.
        if let Err(e) = intoto_binds(&statement, digest) {
            last = Verdict::Failed(e);
            continue;
        }
        let signed = pae(payload_type, &payload);
        // cosign puts the certificate in the layer annotations, the same place
        // as for a signature; some producers put it in the envelope's own
        // signature entry instead, so both are read.
        //
        // The SIGNATURE, though, is in the envelope. `cosign attest` writes the
        // `dev.cosignproject.cosign/signature` annotation present but EMPTY —
        // measured on the first real Rime attestation (length 0, with the
        // certificate and chain beside it) — because a DSSE signature belongs
        // to the envelope. Taking the empty annotation checked a zero-byte
        // signature and refused every update as "does not verify".
        let entry = envelope.get("signatures").and_then(Value::as_array).and_then(|a| a.first());
        let env_sig = entry.and_then(|e| e.get("sig")).and_then(Value::as_str);
        let (cert, chain, sig_b64) = match annotations(layer) {
            Some(a) => match attestation_signature(&a.signature, env_sig) {
                Some(s) => (a.certificate, a.chain, s),
                None => {
                    last = Verdict::CouldNotRun(
                        "the attestation's signature annotation is empty and its envelope \
                         carries no signature"
                            .to_string(),
                    );
                    continue;
                }
            },
            None => {
                let sig = env_sig;
                let cert = entry.and_then(|e| e.get("cert")).and_then(Value::as_str);
                match (sig, cert) {
                    (Some(s), Some(c)) => (c.to_string(), None, s.to_string()),
                    _ => {
                        last = Verdict::CouldNotRun(
                            "the attestation carries no certificate and signature pair, in the \
                             layer annotations or in the envelope"
                                .to_string(),
                        );
                        continue;
                    }
                }
            }
        };
        let work = match TempDir::new("att") {
            Ok(w) => w,
            Err(e) => return Verdict::CouldNotRun(e),
        };
        let v = verify_signed_bytes(
            roots,
            work.path(),
            &Candidate {
                leaf_pem: &cert,
                chain_pem: chain.as_deref(),
                signature_b64: &sig_b64,
                signed: &signed,
            },
            &want,
        );
        if matches!(v, Verdict::Verified { .. }) {
            return v;
        }
        last = v;
    }
    last
}

/// Which signature an attestation layer's check uses: the annotation when it
/// holds one, else the DSSE envelope's. `cosign attest` leaves the annotation
/// present and empty. `None` when neither has one.
fn attestation_signature(annotation: &str, envelope: Option<&str>) -> Option<String> {
    if !annotation.is_empty() {
        return Some(annotation.to_string());
    }
    envelope.filter(|s| !s.is_empty()).map(str::to_string)
}

/// Both checks for one digest.
pub fn verify_image(roots: &Roots, repo: &str, digest: &str) -> Verification {
    let repo = repo_of(repo).to_string();
    Verification {
        signature: verify_signature(roots, &repo, digest),
        provenance: verify_provenance(roots, &repo, digest),
        digest: digest.to_string(),
        repo,
    }
}

// ── the gate ─────────────────────────────────────────────────────────────────

/// Everything the update path needs to decide, in one value.
pub struct Gate {
    pub verification: Verification,
    pub enforcement: Enforcement,
    pub decision: Decision,
}

/// The gate for the image a reference would deploy RIGHT NOW.
///
/// The load-bearing difference between this and [`verify_image`] as `rime
/// trust --verify` calls it: this resolves the reference against the registry
/// first, so it verifies the digest the machine is about to run rather than
/// the one it is already running. On Rime those differ for the same tag as a
/// matter of routine — `rime`, `daily`, `gaming-mesa` and `gaming-nvidia` are
/// four aliases for ONE digest that moves on every successful main build, so
/// every machine is effectively on edge and has never been told so. A gate
/// that verified the booted digest would wave through an update to an
/// unsigned image every single time, while printing "verified".
///
/// A reference that cannot be resolved is not an error the caller has to
/// handle: it is both checks answering [`Verdict::CouldNotRun`], with the
/// reason, and [`decide`] deciding what that is worth. Returning `Err` here
/// would put a second policy decision in every caller.
///
/// `found` is what [`crate::trust::offline_report`] made of the deployment's
/// origin: the reference, or the reason it could not be read. A reference
/// nobody could READ is `CouldNotRun` on both arms, exactly like a reference
/// nobody could resolve — this is the EACCES rule this repository swept
/// fourteen readers for in September, and skipping the gate on it was the
/// same defect one layer up. `Ok(None)` — a deployment that genuinely has no
/// container image reference — is the only state that skips: there is no
/// image, so refusing on the grounds that one could not be verified would
/// refuse on the grounds that something which does not exist was not checked.
pub fn gate(roots: &Roots, found: Result<Option<&str>, &str>) -> Gate {
    let enforcement = enforcement(roots);
    let reference = match found {
        Ok(Some(r)) => r,
        Ok(None) => {
            // Nothing to deploy from. Both arms could not run, and `decide`
            // is still the one that says what that is worth — but the reason
            // says which kind of nothing it is, because "this is not a
            // container deployment" and "I could not tell" must not print the
            // same sentence.
            let why = "this deployment has no container image reference, so there is no \
                       image to verify"
                .to_string();
            return not_run("(no image)", "(no repository)", &why, &enforcement);
        }
        Err(e) => {
            let why = format!(
                "the deployment's own record of which image it runs could not be read, so \
                 there is nothing to look up: {e}"
            );
            return not_run("(unknown)", "(unknown)", &why, &enforcement);
        }
    };
    match resolve(roots, reference) {
        Ok(digest) => {
            let verification = verify_image(roots, reference, &digest);
            let decision = decide(&verification, &enforcement);
            Gate { verification, enforcement, decision }
        }
        Err(why) => {
            let why = format!("the registry would not say which digest {reference} is: {why}");
            // The reference stands in for the digest, because there is no
            // digest — and saying so in the field a reader looks at beats
            // inventing one or printing an empty one.
            not_run(reference, repo_of(reference), &why, &enforcement)
        }
    }
}

/// A gate whose two checks could not run, for the same reason.
///
/// One helper so that every "nothing could be checked" path reaches [`decide`]
/// rather than returning early past it. An early return is how the reference
/// this gate could not read came to be deployed without being checked.
fn not_run(digest: &str, repo: &str, why: &str, e: &Enforcement) -> Gate {
    let verification = Verification {
        digest: digest.to_string(),
        repo: repo.to_string(),
        signature: Verdict::CouldNotRun(why.to_string()),
        provenance: Verdict::CouldNotRun(why.to_string()),
    };
    let decision = decide(&verification, e);
    Gate { verification, enforcement: e.clone(), decision }
}

// ── reporting ────────────────────────────────────────────────────────────────

/// A decision as the three things every consumer of it needs: the word, which
/// checks refused, and the lines to print.
///
/// One function because `rime trust --json`, `verify::to_json` and the update
/// path all have to agree on which word means which state, and three
/// `match`es on the same enum is three chances for one of them to call a
/// refusal a warning.
pub fn decision_json(d: &Decision) -> (&'static str, Vec<String>, Vec<String>) {
    match d {
        Decision::Proceed => ("proceed", Vec::new(), Vec::new()),
        Decision::ProceedWithWarnings(w) => ("proceed-with-warnings", Vec::new(), w.clone()),
        Decision::Refuse { refused_by, lines } => ("refuse", refused_by.clone(), lines.clone()),
    }
}

pub fn to_json(v: &Verification, e: &Enforcement, d: &Decision) -> Value {
    let (decision, refused_by, lines) = decision_json(d);
    json!({
        "digest": v.digest,
        "repository": v.repo,
        "signature": v.signature.to_json(),
        "provenance": v.provenance.to_json(),
        "enforcement": {
            "signature": e.signature.as_str(),
            "provenance": e.provenance.as_str(),
            "notes": e.notes,
        },
        "decision": decision,
        "refusedBy": refused_by,
        "reasons": lines,
    })
}

/// One line for `rime status`, and the same words the gate would use.
pub fn render_mode(e: &Enforcement) -> String {
    format!(
        "  Enforcement       signature {}, provenance {}\n",
        e.signature.as_str(),
        e.provenance.as_str()
    )
}

fn verdict_sentence(what: &str, v: &Verdict) -> String {
    match v {
        Verdict::Verified { signer, .. } => format!(
            "  {what:<18}verified — signed by {signer}\n                    \
             (the transparency log was not checked)\n"
        ),
        Verdict::Absent(w) => format!("  {what:<18}none published — {w}\n"),
        Verdict::Failed(w) => format!("  {what:<18}DOES NOT VERIFY — {w}\n"),
        Verdict::CouldNotRun(w) => format!("  {what:<18}not checked — {w}\n"),
    }
}

/// The verification block of `rime trust --verify`.
pub fn render(v: &Verification, e: &Enforcement, d: &Decision) -> String {
    let mut s = String::new();
    s.push_str(&format!("  Digest            {}\n", v.digest));
    s.push_str(&verdict_sentence("Signature", &v.signature));
    s.push_str(&verdict_sentence("Provenance", &v.provenance));
    s.push_str(&render_mode(e));
    for note in &e.notes {
        s.push_str(&format!("  note              {note}\n"));
    }
    match d {
        Decision::Proceed => {
            s.push_str("  Deploying this    yes — both checks pass\n");
        }
        Decision::ProceedWithWarnings(w) => {
            s.push_str("  Deploying this    yes, with what follows unestablished\n");
            for line in w {
                s.push_str(&format!("                    {line}\n"));
            }
        }
        Decision::Refuse { refused_by, lines } => {
            s.push_str(&format!(
                "  Deploying this    REFUSED — {}\n",
                refused_by.join(" and ")
            ));
            for line in lines {
                s.push_str(&format!("                    {line}\n"));
            }
        }
    }
    s
}

/// The refusal an update prints, in full, or nothing.
///
/// Shaped like `channel::halt_reason` on purpose: `ops::update` already knows
/// how to consult one of these, and a second shape would be a second thing to
/// get wrong.
///
/// The headline is chosen from the verdicts of the checks that actually
/// refused, not from the fact of a refusal. "This image does not verify" and
/// "this image could not be verified" are different accusations — the first
/// says somebody tampered with it, the second says the machine could not
/// reach a registry — and a gate that prints the first when it means the
/// second teaches its users to ignore it.
pub fn refusal(v: &Verification, e: &Enforcement, d: &Decision, escape: &str) -> Option<String> {
    let Decision::Refuse { refused_by, lines } = d else { return None };
    let verdict_of = |what: &str| -> &Verdict {
        if what == "signature" {
            &v.signature
        } else {
            &v.provenance
        }
    };
    let any = |f: &dyn Fn(&Verdict) -> bool| refused_by.iter().any(|w| f(verdict_of(w)));
    // Worst first: a tampering signal outranks a missing one, which outranks a
    // check that never ran.
    let headline = if any(&|x| matches!(x, Verdict::Failed(_))) {
        "does not verify"
    } else if any(&|x| matches!(x, Verdict::Absent(_))) {
        "carries no signature from anybody"
    } else {
        "could not be verified — which is not the same as failing to verify"
    };
    let mut s = format!(
        "rime: this update is being held. The image it would deploy {headline}.\n  \
         the {} for {}\n",
        refused_by.join(" and the "),
        v.digest
    );
    for line in lines {
        s.push_str(&format!("  {line}\n"));
    }
    // What is enforced, named, because the user's next question is "why is
    // this stopping me" and the answer is a setting they can see.
    s.push_str(&format!(
        "\nThis machine enforces: signature {}, provenance {}.\n",
        e.signature.as_str(),
        e.provenance.as_str()
    ));
    for note in &e.notes {
        s.push_str(&format!("  {note}\n"));
    }
    s.push_str(&format!(
        "\nRime publishes a cosign signature for every image, and this machine checks it \
         before deploying.\nA refusal means this machine could not establish that the image \
         the registry is serving\nis the one it was told to expect — see the line above for \
         whether that is because the\nsignature was wrong or because the check could not be \
         made. Either way the update\nstops before anything is downloaded.\n\n\
         If you know why and want it anyway: `sudo rime update {escape}`.\n\
         To change what is enforced permanently, edit {OVERRIDE_PATH} — see \
         docs/trust-enforcement.md.\n"
    ));
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIGNER: &str =
        "https://github.com/AndreNijman/rime-os/.github/workflows/build-image.yml@refs/heads/main";
    const ISSUER: &str = "https://token.actions.githubusercontent.com";
    const DIGEST: &str = "sha256:308127d9cefeada90b1cb47b8f9c1cf6e8bd8f13ae5f3b0e2d7f4a6c8e1b3d5f";
    const REPO: &str = "ghcr.io/andrenijman/rime-os";

    fn verified() -> Verdict {
        Verdict::Verified { signer: SIGNER.into(), at: "unix 1757079805".into() }
    }

    /// A verification whose signature is `sig` and whose provenance is `prov`.
    fn v(sig: Verdict, prov: Verdict) -> Verification {
        Verification { digest: DIGEST.into(), repo: REPO.into(), signature: sig, provenance: prov }
    }

    /// Strictness for the signature only; provenance is pinned `Off` so a test
    /// of one arm cannot be answered by the other.
    fn sig_only(level: Strictness) -> Enforcement {
        Enforcement { signature: level, provenance: Strictness::Off, notes: Vec::new() }
    }

    // ── the decision table, one named test per cell ──────────────────────────
    //
    // Written out rather than looped. A loop over a table is a test that fails
    // once with a subscript in the message; twelve names mean the failure tells
    // you which rule broke, and this is the table that decides whether a
    // machine takes an update.

    #[test]
    fn a_verified_signature_under_enforce_proceeds() {
        assert_eq!(
            decide(&v(verified(), verified()), &Enforcement {
                signature: Strictness::Enforce,
                provenance: Strictness::Enforce,
                notes: Vec::new()
            }),
            Decision::Proceed
        );
    }

    #[test]
    fn a_verified_signature_under_warn_proceeds() {
        assert_eq!(decide(&v(verified(), verified()), &sig_only(Strictness::Warn)), Decision::Proceed);
    }

    #[test]
    fn a_verified_signature_under_off_proceeds() {
        assert_eq!(decide(&v(verified(), verified()), &sig_only(Strictness::Off)), Decision::Proceed);
    }

    #[test]
    fn a_failed_signature_under_enforce_refuses() {
        let d = decide(&v(Verdict::Failed("signed by nobody".into()), verified()), &sig_only(Strictness::Enforce));
        let Decision::Refuse { refused_by, lines } = &d else { panic!("{d:?}") };
        assert_eq!(refused_by, &["signature".to_string()]);
        assert!(lines[0].contains("does not verify"), "{lines:?}");
    }

    #[test]
    fn a_failed_signature_refuses_even_under_warn() {
        // The asymmetry that makes `warn` usable. A signature that verifies
        // WRONGLY is the signature of an attack, and no amount of "the network
        // was flaky" makes it not one. `warn` forgives a gap, never a lie.
        let d = decide(&v(Verdict::Failed("the digest is not the one signed".into()), verified()), &sig_only(Strictness::Warn));
        assert!(d.refuses(), "{d:?}");
    }

    #[test]
    fn a_failed_signature_under_off_only_warns() {
        // `off` means off. A user who switched signature checking off gets a
        // warning and their update; the alternative is a setting that does not
        // do what it says, which is worse than not having it.
        let d = decide(&v(Verdict::Failed("bad".into()), verified()), &sig_only(Strictness::Off));
        let Decision::ProceedWithWarnings(w) = &d else { panic!("{d:?}") };
        assert!(w[0].contains("switched off"), "{w:?}");
    }

    #[test]
    fn an_absent_signature_under_enforce_refuses() {
        let d = decide(&v(Verdict::Absent("the registry holds no signature".into()), verified()), &sig_only(Strictness::Enforce));
        let Decision::Refuse { refused_by, lines } = &d else { panic!("{d:?}") };
        assert_eq!(refused_by, &["signature".to_string()]);
        assert!(lines[0].contains("has no signature"), "{lines:?}");
        // Absence is not failure, and the refusal must not say it is.
        assert!(!lines[0].contains("does not verify"), "{lines:?}");
    }

    #[test]
    fn an_absent_signature_under_warn_only_warns() {
        let d = decide(&v(Verdict::Absent("nobody signed it".into()), verified()), &sig_only(Strictness::Warn));
        let Decision::ProceedWithWarnings(w) = &d else { panic!("{d:?}") };
        assert!(w[0].contains("has no signature"), "{w:?}");
    }

    #[test]
    fn an_absent_signature_under_off_only_warns() {
        assert!(matches!(
            decide(&v(Verdict::Absent("nobody signed it".into()), verified()), &sig_only(Strictness::Off)),
            Decision::ProceedWithWarnings(_)
        ));
    }

    #[test]
    fn a_signature_that_could_not_be_checked_under_enforce_refuses() {
        let d = decide(&v(Verdict::CouldNotRun("no route to host".into()), verified()), &sig_only(Strictness::Enforce));
        let Decision::Refuse { refused_by, lines } = &d else { panic!("{d:?}") };
        assert_eq!(refused_by, &["signature".to_string()]);
        assert!(lines[0].contains("could not be checked"), "{lines:?}");
        // The two claims this must never make about an unreachable registry.
        assert!(!lines[0].contains("has no signature"), "{lines:?}");
        assert!(!lines[0].contains("does not verify"), "{lines:?}");
    }

    #[test]
    fn a_signature_that_could_not_be_checked_under_warn_only_warns() {
        let d = decide(&v(Verdict::CouldNotRun("no route to host".into()), verified()), &sig_only(Strictness::Warn));
        let Decision::ProceedWithWarnings(w) = &d else { panic!("{d:?}") };
        assert!(w[0].contains("could not be checked"), "{w:?}");
        assert!(w[0].contains("no route to host"), "{w:?}");
    }

    #[test]
    fn a_signature_that_could_not_be_checked_under_off_only_warns() {
        assert!(matches!(
            decide(&v(Verdict::CouldNotRun("no openssl".into()), verified()), &sig_only(Strictness::Off)),
            Decision::ProceedWithWarnings(_)
        ));
    }

    // ── what a refusal says ─────────────────────────────────────────────────

    #[test]
    fn a_refusal_names_which_of_the_two_checks_refused() {
        let both = Enforcement {
            signature: Strictness::Enforce,
            provenance: Strictness::Enforce,
            notes: Vec::new(),
        };
        let d = decide(&v(Verdict::Failed("bad".into()), Verdict::Absent("none".into())), &both);
        let Decision::Refuse { refused_by, .. } = &d else { panic!("{d:?}") };
        assert_eq!(refused_by, &["signature".to_string(), "provenance".to_string()]);

        let d = decide(&v(verified(), Verdict::Absent("none".into())), &both);
        let Decision::Refuse { refused_by, .. } = &d else { panic!("{d:?}") };
        assert_eq!(refused_by, &["provenance".to_string()]);
    }

    #[test]
    fn a_refusal_carries_the_other_check_s_warning_too() {
        // Refusing on the signature and staying silent about a provenance
        // nobody could establish sends the reader to fix half the problem.
        let e = Enforcement {
            signature: Strictness::Enforce,
            provenance: Strictness::Warn,
            notes: Vec::new(),
        };
        let d = decide(
            &v(Verdict::Failed("bad".into()), Verdict::CouldNotRun("no route to host".into())),
            &e,
        );
        let Decision::Refuse { lines, .. } = &d else { panic!("{d:?}") };
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("provenance could not be checked")), "{lines:?}");
    }

    #[test]
    fn a_refusal_that_could_not_run_is_never_worded_as_a_failure() {
        // The whole point of the four-valued verdict, at the one place a user
        // reads it. "Does not verify" accuses the publisher; "could not be
        // verified" describes the machine, and a gate that confuses them
        // teaches people to ignore it.
        let e = sig_only(Strictness::Enforce);
        let ver = v(Verdict::CouldNotRun("dial tcp: lookup ghcr.io: no such host".into()), verified());
        let d = decide(&ver, &e);
        let out = refusal(&ver, &e, &d, "--allow-unverified").expect("a refusal");
        assert!(out.contains("could not be verified"), "{out}");
        assert!(!out.contains("does not verify."), "{out}");
        assert!(out.contains("no such host"), "{out}");
        // And the enforcement that caused it, so the reader knows what to change.
        assert!(out.contains("signature enforce"), "{out}");
        assert!(out.contains("--allow-unverified"), "{out}");

        // A genuine failure DOES get the accusation.
        let ver = v(Verdict::Failed("the digest is not the one signed".into()), verified());
        let d = decide(&ver, &e);
        let out = refusal(&ver, &e, &d, "--allow-unverified").expect("a refusal");
        assert!(out.contains("does not verify"), "{out}");

        // An absent one is neither.
        let ver = v(Verdict::Absent("the registry holds no signature".into()), verified());
        let d = decide(&ver, &e);
        let out = refusal(&ver, &e, &d, "--allow-unverified").expect("a refusal");
        assert!(out.contains("carries no signature from anybody"), "{out}");
    }

    #[test]
    fn no_part_of_a_refusal_accuses_the_publisher_when_the_check_did_not_run() {
        // The headline was the obvious half. The paragraph underneath used to
        // say, unconditionally, that "the image the registry is serving is not
        // the one this machine was told to expect" — which contradicts the
        // headline three lines above it whenever the verdict is CouldNotRun,
        // and tells a user with a broken network that they have been attacked.
        let e = sig_only(Strictness::Enforce);
        let ver = v(Verdict::CouldNotRun("dial tcp: lookup ghcr.io: no such host".into()), verified());
        let d = decide(&ver, &e);
        let out = refusal(&ver, &e, &d, "--allow-unverified").expect("a refusal");
        assert!(!out.contains("is not the one this machine"), "{out}");
        assert!(out.contains("could not establish"), "{out}");
        // The sentence still has to hold for a real failure, which is the
        // reason it was phrased as an accusation in the first place.
        let ver = v(Verdict::Failed("the digest is not the one signed".into()), verified());
        let d = decide(&ver, &e);
        let out = refusal(&ver, &e, &d, "--allow-unverified").expect("a refusal");
        assert!(out.contains("could not establish"), "{out}");
        assert!(out.contains("does not verify"), "{out}");
    }

    #[test]
    fn nothing_that_proceeds_produces_a_refusal() {
        let e = sig_only(Strictness::Warn);
        for verdict in [verified(), Verdict::Absent("x".into()), Verdict::CouldNotRun("y".into())] {
            let ver = v(verdict.clone(), verified());
            let d = decide(&ver, &e);
            assert!(refusal(&ver, &e, &d, "--allow-unverified").is_none(), "{verdict:?} -> {d:?}");
        }
    }

    #[test]
    fn the_decision_word_is_defined_in_exactly_one_place() {
        assert_eq!(decision_json(&Decision::Proceed).0, "proceed");
        assert_eq!(decision_json(&Decision::ProceedWithWarnings(vec!["w".into()])).0, "proceed-with-warnings");
        assert_eq!(
            decision_json(&Decision::Refuse { refused_by: vec!["signature".into()], lines: vec!["l".into()] }).0,
            "refuse"
        );
        // A warning is not a refusal, and the JSON must not let a consumer
        // read it as one: `refusedBy` is empty for everything that proceeds.
        assert!(decision_json(&Decision::ProceedWithWarnings(vec!["w".into()])).1.is_empty());
    }

    // ── enforcement configuration ───────────────────────────────────────────

    #[test]
    fn the_shipped_default_enforces_the_signature_and_only_warns_on_provenance() {
        let e = Enforcement::default();
        assert_eq!(e.signature, Strictness::Enforce);
        // No published Rime image has an SBOM attestation: the `cosign attest`
        // step lives on roadmap/v2.2 and has never run on main, so `.att` is
        // `manifest unknown` for every digest in the registry today. Shipping
        // `enforce` would refuse every update on every machine because the
        // publisher has not caught up, which is an outage dressed as a
        // security control.
        assert_eq!(e.provenance, Strictness::Warn);
        assert!(e.notes.is_empty());
    }

    #[test]
    fn the_administrator_overrides_the_image_per_key() {
        let mut e = Enforcement::default();
        parse_enforcement("signature=enforce\nprovenance=warn\n", &mut e, "image");
        parse_enforcement("provenance=enforce\n", &mut e, "admin");
        assert_eq!(e.signature, Strictness::Enforce, "the key nobody overrode must survive");
        assert_eq!(e.provenance, Strictness::Enforce);
        assert!(e.notes.is_empty(), "{:?}", e.notes);
    }

    #[test]
    fn a_typo_in_the_config_never_silently_relaxes_a_check() {
        let mut e = Enforcement::default();
        parse_enforcement(
            "signature = enfore\nprovenanace=off\nsignature\n# a comment\n\nprovenance=off # trailing\n",
            &mut e,
            "/etc/rime/trust.conf",
        );
        // The misspelled VALUE left the setting where it was — enforce — and
        // said so. Falling back to a default here would mean a slip of the
        // finger switches signature checking off on a machine that takes
        // updates unattended.
        assert_eq!(e.signature, Strictness::Enforce);
        // The misspelled KEY set nothing.
        assert_eq!(e.notes.iter().filter(|n| n.contains("no such setting: provenanace")).count(), 1, "{:?}", e.notes);
        assert_eq!(e.notes.iter().filter(|n| n.contains("not a key=value line")).count(), 1, "{:?}", e.notes);
        assert!(e.notes.iter().any(|n| n.contains("is not one of enforce, warn, off")), "{:?}", e.notes);
        // A comment after a good value is still a good value.
        assert_eq!(e.provenance, Strictness::Off);
        // Every complaint names its file and line.
        for n in &e.notes {
            assert!(n.starts_with("/etc/rime/trust.conf:"), "{n}");
        }
    }

    #[test]
    fn an_unreadable_policy_file_is_a_note_and_not_a_permissive_default() {
        let dir = std::env::temp_dir().join(format!("rime-verify-conf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("etc/rime")).unwrap();
        let f = dir.join("etc/rime/trust.conf");
        std::fs::write(&f, "signature=off\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o000)).unwrap();
        let e = enforcement(&Roots { fixture: Some(dir.clone()) });
        let _ = std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644));
        let _ = std::fs::remove_dir_all(&dir);
        if e.notes.is_empty() {
            // Running as root, which can read a 000 file. The property is
            // untestable here rather than false, and a test that quietly
            // passes in that case is the "skipped counts as success" failure
            // this repository has recorded three times.
            assert_eq!(e.signature, Strictness::Off, "root read the file, so it must have applied");
            return;
        }
        assert_eq!(e.signature, Strictness::Enforce, "an unreadable file must not relax anything");
        assert!(e.notes.iter().any(|n| n.contains("using the built-in default")), "{:?}", e.notes);
    }

    #[test]
    fn a_config_file_that_is_simply_absent_is_normal_and_silent() {
        let dir = std::env::temp_dir().join(format!("rime-verify-noconf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let e = enforcement(&Roots { fixture: Some(dir.clone()) });
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(e, Enforcement::default());
    }

    // ── the fixture path, and the rule it exists to hold down ───────────────

    /// A fixture root with a `registry/` tree, so `--verify` runs against it.
    fn fixture(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("rime-verify-fx-{}-{name}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("registry")).unwrap();
        dir
    }

    #[test]
    fn an_unreachable_registry_is_never_refused_as_unsigned() {
        // The enforcement-path twin of trust.rs's
        // `an_unreachable_registry_is_never_reported_as_unsigned`. That one
        // guards the readout; this one guards the gate, which is where getting
        // it wrong stops somebody's machine from updating.
        for stderr in [
            "dial tcp: lookup ghcr.io: no such host",
            "x509: certificate signed by unknown authority",
            "unauthorized: authentication required",
            "toomanyrequests: retry later",
        ] {
            let dir = fixture("unreachable");
            let tag = cosign_tag(DIGEST, ".sig").unwrap();
            std::fs::write(dir.join("registry").join(format!("{tag}.error")), stderr).unwrap();
            let roots = Roots { fixture: Some(dir.clone()) };
            let got = verify_signature(&roots, REPO, DIGEST);
            let _ = std::fs::remove_dir_all(&dir);

            assert!(matches!(got, Verdict::CouldNotRun(_)), "{stderr:?} -> {got:?}");
            assert_ne!(got.as_str(), "absent", "{stderr:?}");

            // And the two decisions that follow from it.
            let ver = v(got.clone(), verified());
            let warn = decide(&ver, &sig_only(Strictness::Warn));
            assert!(!warn.refuses(), "an offline machine must still update: {warn:?}");

            let strict = decide(&ver, &sig_only(Strictness::Enforce));
            let Decision::Refuse { lines, .. } = &strict else { panic!("{strict:?}") };
            assert!(lines[0].contains("could not be checked"), "{lines:?}");
            assert!(!lines[0].contains("has no signature"), "{lines:?}");
        }
    }

    #[test]
    fn a_registry_that_answers_and_holds_nothing_is_absent_not_unreachable() {
        // The other half of the same distinction: no `.error` file and no
        // manifest is the fixture's way of saying the registry answered
        // `manifest unknown`. THAT is the substituted-image signal.
        let dir = fixture("absent");
        let roots = Roots { fixture: Some(dir.clone()) };
        let got = verify_signature(&roots, REPO, DIGEST);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(matches!(got, Verdict::Absent(_)), "{got:?}");
    }

    #[test]
    fn a_manifest_unknown_error_is_absence_because_the_registry_said_so() {
        let dir = fixture("unknown");
        let tag = cosign_tag(DIGEST, ".sig").unwrap();
        std::fs::write(
            dir.join("registry").join(format!("{tag}.error")),
            "level=fatal msg=\"reading manifest sha256-x.sig in ghcr.io/a/b: manifest unknown\"",
        )
        .unwrap();
        let roots = Roots { fixture: Some(dir.clone()) };
        let got = verify_signature(&roots, REPO, DIGEST);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(matches!(got, Verdict::Absent(_)), "{got:?}");
    }

    #[test]
    fn a_fixture_root_with_no_registry_tree_keeps_verify_a_no_op() {
        let dir = std::env::temp_dir().join(format!("rime-verify-bare-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(fixture_without_registry(&Roots { fixture: Some(dir.clone()) }));
        let with = fixture("has-registry");
        assert!(!fixture_without_registry(&Roots { fixture: Some(with.clone()) }));
        // And a real machine is never "a fixture without a registry".
        assert!(!fixture_without_registry(&Roots { fixture: None }));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&with);
    }

    #[test]
    fn an_origin_file_nobody_could_read_is_never_deployed_ungated() {
        // The EACCES rule, at the gate. This repository swept fourteen readers
        // in September for collapsing "permission denied" into "absent" and
        // then reporting the guess as a checked fact; skipping the gate on an
        // unreadable origin was the same defect one layer up, and it deployed
        // rather than merely mis-reported.
        let dir = fixture("unreadable-origin");
        let roots = Roots { fixture: Some(dir.clone()) };
        let g = gate(&roots, Err("/proc/cmdline: Permission denied"));
        let _ = std::fs::remove_dir_all(&dir);

        assert!(matches!(g.verification.signature, Verdict::CouldNotRun(_)), "{:?}", g.verification);
        assert!(matches!(g.verification.provenance, Verdict::CouldNotRun(_)), "{:?}", g.verification);
        assert!(g.decision.refuses(), "the shipped default must refuse this: {:?}", g.decision);
        let Decision::Refuse { lines, .. } = &g.decision else { unreachable!() };
        assert!(lines[0].contains("Permission denied"), "{lines:?}");
        // Not an unsigned image. Nobody knows whether it is signed.
        assert!(!lines[0].contains("has no signature"), "{lines:?}");

        // Under `warn` the same machine still updates, because a gap is a gap.
        let dir = fixture("unreadable-origin-warn");
        let roots = Roots { fixture: Some(dir.clone()) };
        std::fs::create_dir_all(dir.join("etc/rime")).unwrap();
        std::fs::write(dir.join("etc/rime/trust.conf"), "signature=warn\nprovenance=warn\n").unwrap();
        let g = gate(&roots, Err("/proc/cmdline: Permission denied"));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!g.decision.refuses(), "{:?}", g.decision);
    }

    #[test]
    fn a_deployment_with_no_image_at_all_is_told_apart_from_one_nobody_could_read() {
        // Both reach `decide` — neither returns early past it — but they must
        // not print the same sentence. "This is not a container deployment"
        // and "I could not tell what it runs" are different facts, and only
        // one of them is anybody's fault.
        let dir = fixture("no-image");
        let roots = Roots { fixture: Some(dir.clone()) };
        let none = gate(&roots, Ok(None));
        let unread = gate(&roots, Err("/proc/cmdline: Permission denied"));
        let _ = std::fs::remove_dir_all(&dir);

        let a = none.verification.signature.reason().unwrap_or_default().to_string();
        let b = unread.verification.signature.reason().unwrap_or_default().to_string();
        assert!(a.contains("no container image reference"), "{a}");
        assert!(b.contains("could not be read"), "{b}");
        assert_ne!(a, b);
    }

    #[test]
    fn a_digest_that_is_not_a_digest_could_not_run_rather_than_failing() {
        let dir = fixture("baddigest");
        let roots = Roots { fixture: Some(dir.clone()) };
        let got = verify_signature(&roots, REPO, "not-a-digest");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(matches!(got, Verdict::CouldNotRun(_)), "{got:?}");
    }

    // ── the payload bindings ────────────────────────────────────────────────

    #[test]
    fn a_signature_over_a_different_image_does_not_count_as_one_over_this_one() {
        // The substitution a moved tag makes easy, and the reason the payload
        // bindings are checked at all: the ECDSA verification alone proves
        // only that the expected identity signed SOMETHING.
        let payload = format!(
            r#"{{"critical":{{"identity":{{"docker-reference":"{REPO}"}},"image":{{"docker-manifest-digest":"sha256:deadbeef"}},"type":"cosign container image signature"}}}}"#
        );
        let e = simple_signing_binds(&payload, REPO, DIGEST).expect_err("must not bind");
        assert!(e.contains("sha256:deadbeef"), "{e}");
        assert!(e.contains(DIGEST), "{e}");
    }

    #[test]
    fn a_signature_from_a_different_repository_does_not_count() {
        let payload = format!(
            r#"{{"critical":{{"identity":{{"docker-reference":"ghcr.io/someoneelse/rime-os"}},"image":{{"docker-manifest-digest":"{DIGEST}"}}}}}}"#
        );
        let e = simple_signing_binds(&payload, REPO, DIGEST).expect_err("must not bind");
        assert!(e.contains("someoneelse"), "{e}");
    }

    #[test]
    fn the_four_rime_tags_all_bind_because_the_tag_is_not_compared() {
        // `apex`, `rime`, `daily`, `gaming-mesa` and `gaming-nvidia` are
        // aliases for ONE digest that moves on every successful main build, so
        // a tag comparison would refuse an image that is genuinely the right
        // one. cosign records the repository; that is what is compared.
        let payload = format!(
            r#"{{"critical":{{"identity":{{"docker-reference":"{REPO}"}},"image":{{"docker-manifest-digest":"{DIGEST}"}}}}}}"#
        );
        for tag in rimed_core::channel::LEGACY_TAGS {
            simple_signing_binds(&payload, &format!("{REPO}:{tag}"), DIGEST)
                .unwrap_or_else(|e| panic!("{tag}: {e}"));
        }
        simple_signing_binds(&payload, &format!("{REPO}@{DIGEST}"), DIGEST).unwrap();
    }

    #[test]
    fn a_payload_missing_its_bindings_is_an_error_rather_than_a_pass() {
        for payload in [
            "{}",
            r#"{"critical":{}}"#,
            r#"{"critical":{"image":{}}}"#,
            r#"{"critical":{"image":{"docker-manifest-digest":"sha256:x"}}}"#,
            "not json at all",
        ] {
            assert!(simple_signing_binds(payload, REPO, DIGEST).is_err(), "{payload}");
        }
    }

    #[test]
    fn an_attestation_must_cover_this_digest_and_be_the_sbom_this_image_publishes() {
        let hex = DIGEST.split_once(':').unwrap().1;
        let good = format!(
            r#"{{"_type":"https://in-toto.io/Statement/v0.1","predicateType":"https://spdx.dev/Document","subject":[{{"name":"{REPO}","digest":{{"sha256":"{hex}"}}}}]}}"#
        );
        intoto_binds(&good, DIGEST).unwrap();
        // Uppercase hex is the same digest.
        intoto_binds(&good.replace(hex, &hex.to_uppercase()), DIGEST).unwrap();

        let other = good.replace(hex, &"a".repeat(hex.len()));
        assert!(intoto_binds(&other, DIGEST).unwrap_err().contains("subject is not"));

        let wrong_kind = good.replace("https://spdx.dev/Document", "https://slsa.dev/provenance/v1");
        assert!(intoto_binds(&wrong_kind, DIGEST).unwrap_err().contains("slsa.dev"));
    }

    // ── the encodings ───────────────────────────────────────────────────────

    #[test]
    fn an_empty_signature_annotation_falls_back_to_the_envelope_signature() {
        // The shape of the first real Rime attestation: annotation present, 0 bytes.
        assert_eq!(attestation_signature("", Some("MEUCIQ")), Some("MEUCIQ".to_string()));
        assert_eq!(attestation_signature("MEYCIQ", Some("MEUCIQ")), Some("MEYCIQ".to_string()));
        assert_eq!(attestation_signature("", Some("")), None);
        assert_eq!(attestation_signature("", None), None);
    }

    #[test]
    fn the_dsse_preauthentication_encoding_is_over_raw_bytes_and_their_lengths() {
        assert_eq!(pae("t", b"hello"), b"DSSEv1 1 t 5 hello".to_vec());
        // The length is of the DECODED payload. Computing it over the base64
        // text is the mistake that makes every attestation fail to verify.
        let payload = b"\x00\x01\x02\xff";
        let got = pae("application/vnd.in-toto+json", payload);
        assert!(got.starts_with(b"DSSEv1 28 application/vnd.in-toto+json 4 "));
        assert!(got.ends_with(payload));
    }

    #[test]
    fn base64_decodes_what_openssl_produced_and_rejects_what_it_did_not() {
        assert_eq!(b64_decode("aGVsbG8=").unwrap(), b"hello".to_vec());
        assert_eq!(b64_decode("aGVsbG8h").unwrap(), b"hello!".to_vec());
        assert_eq!(b64_decode("").unwrap(), Vec::<u8>::new());
        // Registry annotations arrive wrapped; whitespace is not corruption.
        assert_eq!(b64_decode("aGVs\nbG8=\n").unwrap(), b"hello".to_vec());
        for bad in ["aGVsbG8*", "aGVsbG8=x", "a", "aGVsbG8==="] {
            assert!(b64_decode(bad).is_none(), "{bad} decoded");
        }
    }

    #[test]
    fn a_certificate_start_date_becomes_the_instant_the_chain_is_checked_at() {
        // The trap this exists for: a Fulcio leaf lives ten MINUTES, so it is
        // expired for every machine that ever boots the image, and `openssl
        // verify` at `now` reports "certificate has expired" for a perfectly
        // good signature. Measured on the real signature of the digest the
        // L16 is running: notBefore 2026-09-05 13:43:25, notAfter 13:53:25.
        assert_eq!(unix_from_iso("notBefore=1970-01-01 00:00:00Z").unwrap(), 0);
        assert_eq!(unix_from_iso("1970-01-02 00:00:01Z").unwrap(), 86_401);
        let t = unix_from_iso("notBefore=2026-09-05 13:43:25Z").unwrap();
        assert_eq!(t, 1_788_615_805, "the L16's leaf notBefore");
        // Ten minutes later is the notAfter, which is what makes it a trap.
        assert_eq!(unix_from_iso("notAfter=2026-09-05 13:53:25Z").unwrap(), t + 600);
        // The ISO-8601 `T` separator, and a leap day.
        assert_eq!(
            unix_from_iso("2024-02-29T00:00:00Z").unwrap(),
            unix_from_iso("2024-02-28 00:00:00Z").unwrap() + 86_400
        );
        for bad in ["", "notBefore=", "2026-09-05", "2026-09 13:43:25Z", "2026-13-05 00:00:00Z", "2026-09-32 00:00:00Z", "not a date at all"] {
            assert!(unix_from_iso(bad).is_err(), "{bad:?} parsed");
        }
    }

    #[test]
    fn the_oidc_issuer_is_read_out_of_either_sigstore_extension() {
        // Both renderings captured from `openssl x509 -noout -text` over
        // certificates minted for the purpose, not hand-typed: openssl leaves
        // a trailing space after the OID and indents the value on the next
        // line, and `.1.8` wraps the string in a DER UTF8String which openssl
        // prints as two bytes in front of it.
        let v1 = "            1.3.6.1.4.1.57264.1.1: \n                https://token.actions.githubusercontent.com\n    Signature Algorithm: ecdsa-with-SHA256\n";
        assert_eq!(issuer_from_openssl_text(v1).as_deref(), Some(ISSUER));
        let v2 = "            1.3.6.1.4.1.57264.1.8: \n                .+https://token.actions.githubusercontent.com\n    Signature Algorithm: ecdsa-with-SHA256\n";
        assert_eq!(issuer_from_openssl_text(v2).as_deref(), Some(ISSUER));
        assert_eq!(issuer_from_openssl_text("no extension here"), None);
    }

    #[test]
    fn an_issuer_that_merely_contains_the_expected_one_is_not_the_expected_one() {
        // The reason the wrapper is peeled by taking the tail rather than by
        // searching the line for the value we hoped to find.
        let hostile = format!(
            "            1.3.6.1.4.1.57264.1.8: \n                .Xhttps://evil.example/?x={ISSUER}\n"
        );
        let got = issuer_from_openssl_text(&hostile).expect("something was read");
        assert_ne!(got, ISSUER);
        assert!(got.starts_with("https://evil.example/"), "{got}");
    }

    #[test]
    fn a_chain_failure_reports_the_reason_and_not_a_distinguished_name() {
        // Captured from `openssl verify` rather than typed. It prints the
        // SUBJECT of the offending certificate first and the reason second,
        // so the obvious `lines().next()` — which this replaced — put
        // `O=sigstore.dev, CN=sigstore` where a user needed to read "the
        // certificate is not yet valid".
        let real = "O=sigstore.dev, CN=sigstore\n\
                    error 9 at 2 depth lookup: certificate is not yet valid\n\
                    error leaf.pem: verification failed\n";
        assert_eq!(openssl_verify_reason(real), "certificate is not yet valid");
        let expired = "O=sigstore.dev, CN=sigstore-intermediate\n\
                       error 10 at 0 depth lookup: certificate has expired\n";
        assert_eq!(openssl_verify_reason(expired), "certificate has expired");
        // No `depth lookup:` line: say everything rather than guess, because
        // an empty reason on the screen that stops somebody updating is worse
        // than a verbose one.
        assert_eq!(openssl_verify_reason("something else entirely"), "something else entirely");
        assert_eq!(openssl_verify_reason("   \n\n"), "verification failed");
        assert_eq!(openssl_verify_reason(""), "verification failed");
    }

    #[test]
    fn a_reference_loses_its_tag_and_keeps_its_port() {
        assert_eq!(repo_of("ghcr.io/andrenijman/rime-os:daily"), REPO);
        assert_eq!(repo_of(&format!("{REPO}@{DIGEST}")), REPO);
        assert_eq!(repo_of(REPO), REPO);
        // A colon before the last slash is a port, not a tag.
        assert_eq!(repo_of("registry.local:5000/rime/os"), "registry.local:5000/rime/os");
        assert_eq!(repo_of("registry.local:5000/rime/os:daily"), "registry.local:5000/rime/os");
    }

    // ── what a reader is told ───────────────────────────────────────────────

    #[test]
    fn the_report_never_prints_verified_without_saying_what_was_not_checked() {
        let ver = v(verified(), Verdict::Absent("none published".into()));
        let e = Enforcement::default();
        let out = render(&ver, &e, &decide(&ver, &e));
        assert!(out.contains("verified — signed by"), "{out}");
        assert!(out.contains("transparency log was not checked"), "{out}");
        assert!(out.contains("Enforcement       signature enforce, provenance warn"), "{out}");
        // Provenance is absent and only warned about, so the machine deploys —
        // and says what it did not establish.
        assert!(out.contains("yes, with what follows unestablished"), "{out}");
    }

    #[test]
    fn every_verdict_carries_its_reason_into_the_json() {
        for verdict in [
            Verdict::Absent("nobody signed it".into()),
            Verdict::Failed("the digest is not the one signed".into()),
            Verdict::CouldNotRun("no route to host".into()),
        ] {
            let j = verdict.to_json();
            assert_eq!(j["state"], verdict.as_str());
            assert!(j["reason"].as_str().is_some_and(|r| !r.is_empty()), "{j}");
            assert!(j["signer"].is_null(), "only a verified signature names a signer: {j}");
        }
        let j = verified().to_json();
        assert_eq!(j["state"], "verified");
        assert_eq!(j["signer"], SIGNER);
        assert!(j["reason"].is_null());
        assert!(j["verifiedAt"].as_str().is_some());
    }

    #[test]
    fn the_json_gate_answer_matches_the_rendered_one() {
        let ver = v(Verdict::Failed("bad".into()), Verdict::Absent("none".into()));
        let e = Enforcement::default();
        let d = decide(&ver, &e);
        let j = to_json(&ver, &e, &d);
        assert_eq!(j["decision"], "refuse");
        assert_eq!(j["refusedBy"], serde_json::json!(["signature"]));
        assert_eq!(j["signature"]["state"], "failed");
        assert_eq!(j["provenance"]["state"], "absent");
        assert_eq!(j["enforcement"]["signature"], "enforce");
        assert_eq!(j["digest"], DIGEST);
        assert!(render(&ver, &e, &d).contains("REFUSED — signature"));
    }
}
