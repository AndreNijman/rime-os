//! `rime trust` — is the image this machine is running the one Rime published,
//! and how much of that has anybody checked?
//!
//! Roadmap §27 asks for supply-chain verification and says "`rime status`
//! should surface trust state clearly". The half that already exists is
//! producer-side: `build-image.yml` cosign-signs every published digest under a
//! keyless GitHub identity, attaches an SPDX SBOM as a signed attestation, and
//! verifies its own work before moving a tag. None of that reaches the machine.
//!
//! ## The three questions, kept apart
//!
//! A trust readout that answers them as one number is the readout that lies.
//!
//! 1. **What was checked when this image was pulled?** ostree records the
//!    answer in the deployment's own origin file, as the prefix on
//!    `container-image-reference`. `ostree-unverified-registry:` means the pull
//!    checked a content digest and nothing else — no signature, no identity.
//!    This is a fact about the past, it needs no network, and it cannot change.
//! 2. **What will be checked on the next pull?** `/etc/containers/policy.json`,
//!    which is what `bootc upgrade` consults. A machine can be running an image
//!    nobody verified and still be configured to verify the next one, or the
//!    reverse.
//! 3. **Does the registry hold a signature for the digest we are running?**
//!    The only question that needs the network, so it is the only one behind
//!    `--verify`. Everything else is answered offline, every time.
//!
//! ## Absence, refusal and distance are three different answers
//!
//! `rime boot status` already refuses to let a refused read report as a
//! measurement, and `docs/boot-v2.md` states the rule: an empty entry list is
//! indistinguishable from "no deployment has failed". Trust has the same shape
//! and worse stakes, so every fact here is `Option` plus a reason rather than a
//! defaulted `false`:
//!
//! - **offline** — the registry could not be reached. Reported as
//!   `unavailable` with the transport error. It is never `unsigned`.
//! - **unsigned** — the registry answered, and has no signature for this
//!   digest. That is the substituted-image signal, and it is a claim only
//!   after a round trip that succeeded.
//! - **present but unchecked** — a signature artifact exists and this machine
//!   has no `cosign`, so its cryptography was not checked here. The identity
//!   printed in that state is what an unverified certificate asserts about
//!   itself, and the report says so in those words rather than promoting it to
//!   "signed by".
//!
//! ## No root, no daemon, no prompt for the offline half
//!
//! `bootc status` requires root even to read (it opens the sysroot for write),
//! so it is unusable from a command Rime Settings polls. Every offline fact
//! here is a file read or a `readlink` of a world-readable path, which is why
//! the trust block can sit inside `rime status` without turning it into a
//! privileged command.

use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Args;
use serde_json::{json, Map, Value};

/// The keyless Sigstore identity `build-image.yml` has signed every image
/// under so far.
///
/// It is a constant because it is a fact about this repository's release
/// pipeline, not a setting: `.github/workflows/build-image.yml` computes
/// `IDENTITY="https://github.com/$GITHUB_REPOSITORY/.github/workflows/build-image.yml@$REF"`
/// and verifies its own signature against it before promoting any tag. A fork
/// that publishes its own images changes [`SIGNER_OVERRIDE`] rather than
/// patching this file.
const EXPECTED_SIGNER: &str =
    "https://github.com/AndreNijman/apex-os/.github/workflows/build-image.yml@refs/heads/main";  // rime-rename: keep

/// The same identity after the repository is renamed to `AndreNijman/rime-os`.
///
/// A Sigstore identity names the repository, so the rename changes the signer
/// of every image built after it. A machine that knew only [`EXPECTED_SIGNER`]
/// would refuse all of them under `signature=enforce` and could never update
/// again, which is also how it would miss the fix. So the fleet learns the new
/// name one release BEFORE the rename (2026-09-28), and keeps the old one so an
/// image signed before the rename, and a rollback to one, still verifies.
const RENAMED_SIGNER: &str =
    "https://github.com/AndreNijman/rime-os/.github/workflows/build-image.yml@refs/heads/main";

/// Every identity this machine accepts an image from when no override names one.
fn default_signers() -> Vec<String> {
    vec![EXPECTED_SIGNER.to_string(), RENAMED_SIGNER.to_string()]
}

/// The OIDC issuer behind that identity. GitHub Actions' token endpoint.
pub const EXPECTED_ISSUER: &str = "https://token.actions.githubusercontent.com";

/// An image-owned file that replaces [`default_signers`] when present.
///
/// One Sigstore certificate identity per line (blank lines and `#` comments are
/// ignored), any of which is accepted. A downstream that
/// rebuilds Rime under its own workflow has a different signer and every other
/// word in this report stays true, so the identity is the one thing worth
/// making a file.
const SIGNER_OVERRIDE: &str = rime_paths::TRUST_EXPECTED_SIGNER;

/// The workflow file in [`EXPECTED_SIGNER`], and the one that replaces it for a
/// rollout document.
///
/// The document has to be publishable BETWEEN image builds — that is the whole
/// reason it is not a label — so it cannot be signed by the build workflow, and
/// it is signed by the workflow that already moves channel tags.
const BUILD_WORKFLOW: &str = "build-image.yml";
const ROLLOUT_WORKFLOW: &str = "promote-channel.yml";

/// An image-owned file that replaces the derived rollout identity.
const ROLLOUT_SIGNER_OVERRIDE: &str = rime_paths::TRUST_EXPECTED_ROLLOUT_SIGNER;

/// Where the containers signature policy lives, and what `bootc upgrade` reads.
const POLICY_PATH: &str = "/etc/containers/policy.json";

#[derive(Args)]
pub struct TrustArgs {
    /// Ask the registry whether it holds a signature and an SBOM attestation
    /// for the digest this machine is running.
    ///
    /// Off by default: everything else in this report is a local file read,
    /// and a status command must not reach the network unless it was asked to.
    #[arg(long)]
    pub verify: bool,
    /// Answer the question `rime update` asks: would the image this machine
    /// would deploy NEXT be accepted?
    ///
    /// Not the same question as `--verify`, which is about the image already
    /// booted. This one resolves the origin's tag against the registry first,
    /// because the four Rime tags are aliases for one digest that moves on
    /// every successful main build — so what a machine is running and what it
    /// is about to run are routinely different, and only the second one can
    /// be refused.
    ///
    /// Exits 1 if the update would be refused, and prints the same refusal
    /// `rime update` would print, from the same function. It needs no root
    /// and stages nothing, so it is the way to find out what the gate will do
    /// before asking it to do it.
    #[arg(long, conflicts_with = "verify")]
    pub gate: bool,
    /// Emit machine-readable JSON instead of a report.
    #[arg(long)]
    pub json: bool,
}

/// Where the report reads from.
///
/// A prefix, and only a prefix — the same contract as `rime boot status`'s
/// `Roots`. `tests/test-rime-trust.sh` points `$RIME_TRUST_ROOT` at a fixture
/// tree so the assertions run against the shipped binary. No program name is
/// ever taken from the environment; `skopeo` and `cosign` are absolute paths
/// and under a fixture root neither is executed at all.
pub struct Roots {
    pub(crate) fixture: Option<PathBuf>,
}

impl Roots {
    pub fn from_env() -> Self {
        Self { fixture: std::env::var_os("RIME_TRUST_ROOT").map(PathBuf::from) }
    }

    pub(crate) fn path(&self, absolute: &str) -> PathBuf {
        match &self.fixture {
            // `absolute` always starts with '/'; without the strip, `join`
            // discards the prefix and reads the real machine, which is a
            // fixture that passes on the author's box and nowhere else.
            Some(root) => root.join(absolute.trim_start_matches('/')),
            None => PathBuf::from(absolute),
        }
    }

    /// A file's text, or the reason it could not be read.
    ///
    /// There is no `.ok()` twin here on purpose. Every path this module reads
    /// feeds a claim about whether the operating system is trustworthy, and
    /// there is no such claim that is safe to make out of a read nobody
    /// completed.
    pub(crate) fn read(&self, absolute: &str) -> Result<String, String> {
        let p = self.path(absolute);
        match std::fs::read_to_string(&p) {
            Ok(s) => Ok(s),
            Err(e) => Err(format!("{}: {e}", p.display())),
        }
    }

    /// A file's text, `None` if it does not exist, or the reason it could not
    /// be read.
    ///
    /// The difference from [`Self::read`] matters for configuration: an absent
    /// policy file means "no policy was written", which is a real and normal
    /// state, while an unreadable one means nobody knows what the policy is.
    /// One may fall back to a default; the other may not.
    pub(crate) fn read_optional(&self, absolute: &str) -> Result<Option<String>, String> {
        let p = self.path(absolute);
        match std::fs::read_to_string(&p) {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", p.display())),
        }
    }

    fn read_link(&self, path: &Path) -> Result<PathBuf, String> {
        std::fs::read_link(path).map_err(|e| format!("{}: {e}", path.display()))
    }
}

// ── what was checked when this image was pulled ──────────────────────────────

/// The verification prefix ostree stamped into the booted deployment's origin.
///
/// ostree's container image references carry their verification mode in front
/// of the transport: `ostree-unverified-registry:REF`,
/// `ostree-unverified-image:TRANSPORT:REF`, `ostree-image-signed:TRANSPORT:REF`
/// and `ostree-remote-image:REMOTE:TRANSPORT:REF`. The prefix is not cosmetic —
/// it is what ostree used, recorded by the code that used it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pull {
    /// Content digest only. No signature was checked, by anything.
    Unverified,
    /// The containers signature policy was applied to this pull.
    ImageSigned,
    /// An ostree remote's GPG configuration was applied to this pull.
    Remote(String),
    /// The origin names a verification mode this build does not know.
    Unknown(String),
    /// This is not an ostree deployment at all.
    NotOstree,
    /// Could not be determined. Never a synonym for "unverified".
    Unavailable(String),
}

impl Pull {
    pub fn as_str(&self) -> &'static str {
        match self {
            Pull::Unverified => "unverified",
            Pull::ImageSigned => "image-signed",
            Pull::Remote(_) => "ostree-remote",
            Pull::Unknown(_) => "unknown",
            Pull::NotOstree => "not-ostree",
            Pull::Unavailable(_) => "unavailable",
        }
    }
}

/// Split an ostree container image reference into its verification mode and the
/// plain image reference underneath.
///
/// Returns the mode and the `registry/repo:tag` a user would recognise, so the
/// report can print one line rather than making the reader parse a scheme.
pub fn parse_origin_reference(reference: &str) -> (Pull, Option<String>) {
    let (scheme, rest) = match reference.split_once(':') {
        Some(p) => p,
        None => return (Pull::Unknown(reference.to_string()), None),
    };
    match scheme {
        // The shorthand: the transport is implied to be `registry`.
        "ostree-unverified-registry" => (Pull::Unverified, Some(rest.to_string())),
        // `TRANSPORT:REF` — drop the transport, keep the reference.
        "ostree-unverified-image" => (Pull::Unverified, strip_transport(rest)),
        "ostree-image-signed" => (Pull::ImageSigned, strip_transport(rest)),
        // `REMOTE:TRANSPORT:REF`.
        "ostree-remote-image" => match rest.split_once(':') {
            Some((remote, tail)) => (Pull::Remote(remote.to_string()), strip_transport(tail)),
            None => (Pull::Unknown(reference.to_string()), None),
        },
        _ => (Pull::Unknown(reference.to_string()), None),
    }
}

/// Drop a containers-transport prefix (`registry:`, `docker://`, …) if there is
/// one, leaving the image reference.
fn strip_transport(rest: &str) -> Option<String> {
    if let Some(tail) = rest.strip_prefix("registry:") {
        return Some(tail.to_string());
    }
    if let Some(tail) = rest.strip_prefix("docker://") {
        return Some(tail.to_string());
    }
    Some(rest.to_string())
}

/// The booted deployment's origin file, found the way the kernel found the
/// deployment: through the `ostree=` argument on the command line.
///
/// That argument is a path to a symlink under `/ostree/boot.N/`, and its target
/// names the deployment directory. Both the symlink and the `.origin` file
/// beside the directory are world-readable, which is the whole reason this
/// works without root — `bootc status`, the obvious alternative, opens the
/// sysroot for write and refuses every non-root caller.
pub fn booted_origin(roots: &Roots) -> Result<String, String> {
    let cmdline = roots.read("/proc/cmdline")?;
    let arg = cmdline
        .split_whitespace()
        .find_map(|w| w.strip_prefix("ostree="))
        .ok_or_else(|| "not-ostree".to_string())?;
    // The `ostree=` value is absolute in the booted root's namespace, so it
    // goes back through `Roots` rather than being used as-is.
    let link = roots.path(arg);
    let target = roots.read_link(&link)?;
    // The target is relative to the symlink's own directory.
    let deploy_dir = link
        .parent()
        .map(|p| p.join(&target))
        .ok_or_else(|| format!("{}: has no parent directory", link.display()))?;
    let origin = {
        let mut s = deploy_dir.into_os_string();
        s.push(".origin");
        PathBuf::from(s)
    };
    std::fs::read_to_string(&origin).map_err(|e| format!("{}: {e}", origin.display()))
}

/// `container-image-reference=` out of an origin file's `[origin]` section.
pub fn origin_image_reference(origin: &str) -> Option<String> {
    origin
        .lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("container-image-reference="))
        .map(|v| v.trim().to_string())
}

// ── what will be checked on the next pull ────────────────────────────────────

/// What `/etc/containers/policy.json` would require of the next pull.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Policy {
    /// `insecureAcceptAnything`. Every image is accepted, signed or not.
    AcceptsAnything,
    /// `reject`. No image from this scope is accepted at all.
    Rejects,
    /// A signature is required. The string names the requirement type, so a
    /// `sigstoreSigned` policy does not read the same as an old `signedBy` one.
    RequiresSignature(String),
    /// The file parsed and had no requirement that covers this image.
    NoRequirement,
    /// Could not be determined. Never a synonym for "accepts anything".
    Unavailable(String),
}

impl Policy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Policy::AcceptsAnything => "accepts-anything",
            Policy::Rejects => "rejects",
            Policy::RequiresSignature(_) => "requires-signature",
            Policy::NoRequirement => "no-requirement",
            Policy::Unavailable(_) => "unavailable",
        }
    }
}

/// Resolve the signature policy that applies to one image reference.
///
/// containers-policy matches the most specific `transports.docker` scope first
/// — `registry/namespace/repo`, then each shorter prefix, then the bare
/// registry host, then the empty scope — and falls back to the top-level
/// `default` only when no docker scope matched. Resolving it the other way
/// round is the mistake that makes a report say "signatures required" on a
/// machine whose narrower scope accepts anything.
pub fn policy_for(policy_json: &str, image_reference: &str) -> Policy {
    let doc: Value = match serde_json::from_str(policy_json) {
        Ok(v) => v,
        Err(e) => return Policy::Unavailable(format!("{POLICY_PATH}: {e}")),
    };
    // The repository part of `registry/ns/repo:tag` — scopes never carry a tag
    // or a digest.
    let repo = image_reference
        .split_once('@')
        .map(|(r, _)| r)
        .unwrap_or(image_reference);
    let repo = match repo.rsplit_once(':') {
        // A colon after the last '/' is a tag; a colon before it is a port on
        // the registry host and stays.
        Some((head, _)) if !head.contains('/') || repo.rfind(':') > repo.rfind('/') => head,
        _ => repo,
    };

    if let Some(docker) = doc.get("transports").and_then(|t| t.get("docker")) {
        let mut scope = repo;
        loop {
            if let Some(reqs) = docker.get(scope) {
                return classify(reqs);
            }
            match scope.rsplit_once('/') {
                Some((head, _)) => scope = head,
                None => break,
            }
        }
        if let Some(reqs) = docker.get("") {
            return classify(reqs);
        }
    }
    match doc.get("default") {
        Some(reqs) => classify(reqs),
        None => Policy::NoRequirement,
    }
}

/// Turn one policy requirement array into a verdict.
///
/// containers-policy accepts an image when ANY requirement in the array
/// accepts it, so one `insecureAcceptAnything` beside a `sigstoreSigned` is a
/// policy that requires nothing. Reporting the stricter entry because it is
/// listed would describe a machine that does not exist.
fn classify(reqs: &Value) -> Policy {
    let arr = match reqs.as_array() {
        Some(a) => a,
        None => return Policy::Unavailable(format!("{POLICY_PATH}: a scope is not a list")),
    };
    if arr.is_empty() {
        return Policy::Rejects;
    }
    let types: Vec<&str> = arr
        .iter()
        .filter_map(|r| r.get("type").and_then(Value::as_str))
        .collect();
    if types.contains(&"insecureAcceptAnything") {
        return Policy::AcceptsAnything;
    }
    if let Some(t) = types
        .iter()
        .find(|t| **t == "sigstoreSigned" || **t == "signedBy")
    {
        return Policy::RequiresSignature((*t).to_string());
    }
    if types.iter().all(|t| *t == "reject") {
        return Policy::Rejects;
    }
    Policy::NoRequirement
}

// ── does the registry hold a signature for what we are running ───────────────

/// One registry-side artifact for the booted digest.
/// There is no `Present` variant any more, and its absence is the unit's whole
/// point. `Present` meant "the tag exists", which is what this module used to
/// report because it could not do better; a tag existing is not a signature
/// verifying, and printing the first as if it were the second is the lie
/// `crate::verify` was written to stop telling. A fetch now either hands back
/// the artifact's bytes — which `crate::verify` judges — or one of these two
/// reasons it could not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Artifact {
    /// The registry answered and does not hold it.
    Absent,
    /// The registry was not reached, or answered something else. The reason is
    /// carried so this can never be read as `Absent`.
    Unavailable(String),
}

/// Whether an error from `skopeo inspect` means the tag is not there, or that
/// we never got an answer.
///
/// A registry says "manifest unknown" for a tag it does not have. Everything
/// else — DNS, TLS, 401, 429, a proxy's HTML error page — is distance, not
/// absence, and the difference is the whole point of this module. The match is
/// on the wire words rather than on an exit code because skopeo exits 1 for
/// both.
pub fn classify_skopeo_failure(stderr: &str) -> Artifact {
    let s = stderr.to_ascii_lowercase();
    if s.contains("manifest unknown") || s.contains("manifest_unknown") || s.contains("404") {
        Artifact::Absent
    } else {
        let line = stderr
            .lines()
            .last()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .unwrap_or("skopeo failed without a message");
        Artifact::Unavailable(line.to_string())
    }
}

/// The cosign tag for a digest: `sha256:abc…` becomes `sha256-abc….sig`.
pub fn cosign_tag(digest: &str, suffix: &str) -> Option<String> {
    let (alg, hex) = digest.split_once(':')?;
    if alg.is_empty() || hex.is_empty() || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("{alg}-{hex}{suffix}"))
}

/// The `URI:` entry of a Sigstore certificate's subject alternative name.
///
/// This is what the certificate claims about who signed. Reading it is not
/// verifying it, and every caller here labels it as a claim.
pub fn san_uri(openssl_text: &str) -> Option<String> {
    let mut lines = openssl_text.lines();
    while let Some(l) = lines.next() {
        if l.contains("Subject Alternative Name") {
            for next in lines.by_ref() {
                let t = next.trim();
                if let Some(uri) = t.strip_prefix("URI:") {
                    return Some(uri.trim().to_string());
                }
                if !t.is_empty() && !t.starts_with("URI:") {
                    break;
                }
            }
            return None;
        }
    }
    None
}

/// The registry half of the report, filled in only under `--verify`.
///
/// It used to hold a `claimed_signer` and a `checked: Option<bool>`, because
/// the cryptography was `cosign verify` and cosign is not installed on any Rime
/// machine — so `checked` was `None` every time and the report could only
/// repeat what the certificate said about itself. `crate::verify` replaced that
/// with a verification this machine can actually perform, so there is one field
/// for the answer and no field for the claim.
#[derive(Debug, Clone)]
pub struct Registry {
    pub digest: Option<String>,
    pub digest_error: Option<String>,
    /// The verification of that digest, when there was a digest to verify.
    pub verification: Option<crate::verify::Verification>,
    /// What the update path would do about it.
    pub decision: Option<crate::verify::Decision>,
    /// Set when `--verify` deliberately did nothing, and why.
    pub note: Option<String>,
}

/// The digest of the booted image, from `rpm-ostree status --json`.
///
/// It is not on disk in any plain file: ostree keeps it in the commit object's
/// GVariant metadata. `rpm-ostree` answers over its system D-Bus service, which
/// works unprivileged — `bootc status` does not.
///
/// Under a fixture root it reads a pre-rendered `rpm-ostree-status.json`
/// instead of spawning anything, the same shape `rime boot status` uses for
/// `bootctl list`. Without that, every path needing the booted digest is
/// unreachable from a test — including §26's rollout stop, and the one thing a
/// rollout stop has to do is stop.
pub fn booted_digest(roots: &Roots) -> Result<String, String> {
    let doc: Value = if roots.fixture.is_some() {
        let p = roots.path("/rpm-ostree-status.json");
        let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", p.display()))?
    } else {
        let out = Command::new(rime_paths::RPM_OSTREE)
            .args(["status", "--json"])
            .output()
            .map_err(|e| format!("could not run rpm-ostree: {e}"))?;
        if !out.status.success() {
            let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(if why.is_empty() { "rpm-ostree status failed".into() } else { why });
        }
        serde_json::from_slice(&out.stdout)
            .map_err(|e| format!("could not parse rpm-ostree status: {e}"))?
    };
    let deployments = doc
        .get("deployments")
        .and_then(Value::as_array)
        .ok_or_else(|| "rpm-ostree status has no deployments".to_string())?;
    let booted = deployments
        .iter()
        .find(|d| d.get("booted").and_then(Value::as_bool) == Some(true))
        .ok_or_else(|| "rpm-ostree status names no booted deployment".to_string())?;
    booted
        .get("base-commit-meta")
        .and_then(|m| m.get("ostree.manifest-digest"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "the booted deployment records no manifest digest".to_string())
}

/// The identities in an override file: one per line, blanks and `#` comments
/// skipped.
fn identity_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// The identities this machine accepts, and where that expectation came from.
pub(crate) fn expected_signers(roots: &Roots) -> Vec<String> {
    match roots.read(SIGNER_OVERRIDE) {
        Ok(s) => {
            let v = identity_lines(&s);
            if v.is_empty() {
                default_signers()
            } else {
                v
            }
        }
        Err(_) => default_signers(),
    }
}

/// The identity this machine expects on a rollout document.
///
/// Derived from [`expected_signers`] by swapping the workflow file, rather than
/// being a second constant. The two identities are the same repository and the
/// same ref and differ only in which workflow signs; a downstream that
/// overrides one and forgets the other would get a rollout document that never
/// verifies, which fails silently as "no rollout is staged" — the ramp stops
/// happening and nothing says so. One file to set is the whole argument.
///
/// [`ROLLOUT_SIGNER_OVERRIDE`] exists anyway for the case the derivation cannot
/// cover: a downstream that signs its documents from a differently NAMED
/// workflow.
pub(crate) fn expected_rollout_signers(roots: &Roots) -> Vec<String> {
    if let Ok(s) = roots.read(ROLLOUT_SIGNER_OVERRIDE) {
        let v = identity_lines(&s);
        if !v.is_empty() {
            return v;
        }
    }
    // Only when the base identity really does name the build workflow. An
    // override that names something else entirely is left alone rather than
    // rewritten into a path nobody publishes under.
    expected_signers(roots)
        .into_iter()
        .map(|image| match image.rfind(BUILD_WORKFLOW) {
            Some(at) => format!(
                "{}{ROLLOUT_WORKFLOW}{}",
                &image[..at],
                &image[at + BUILD_WORKFLOW.len()..]
            ),
            None => image,
        })
        .collect()
}

/// The registry half, under `--verify`.
///
/// Every cryptographic decision is `crate::verify`'s; this function's only job
/// is to find the digest to ask about and to keep "there was no digest" from
/// looking like "the registry holds no signature".
fn verify_registry(roots: &Roots, image_reference: &str) -> Registry {
    if crate::verify::fixture_without_registry(roots) {
        // Historic and deliberate: most fixture roots exist to exercise the
        // offline report, and `--verify` against one of those must stay a
        // no-op. Reporting an absent signature instead would be a fixture's
        // shape reported as a fact about an image.
        return Registry {
            digest: None,
            digest_error: None,
            verification: None,
            decision: None,
            note: Some(
                "--verify does not run under a fixture root that supplies no registry/ fixture"
                    .to_string(),
            ),
        };
    }
    let (digest, digest_error) = match booted_digest(roots) {
        Ok(d) => (Some(d), None),
        Err(e) => (None, Some(e)),
    };
    let Some(digest) = digest else {
        // Without a digest there is nothing to ask the registry ABOUT. That is
        // not the registry being silent, so it is not an absent signature.
        return Registry {
            digest: None,
            digest_error,
            verification: None,
            decision: None,
            note: Some("the booted digest is unknown, so nothing could be verified".to_string()),
        };
    };
    let verification = crate::verify::verify_image(roots, image_reference, &digest);
    let decision = crate::verify::decide(&verification, &crate::verify::enforcement(roots));
    Registry {
        digest: Some(digest),
        digest_error,
        verification: Some(verification),
        decision: Some(decision),
        note: None,
    }
}

// ── the report ───────────────────────────────────────────────────────────────

/// Everything `rime trust` knows, and nothing it assumes.
#[derive(Debug, Clone)]
pub struct Report {
    pub image: Option<String>,
    pub image_error: Option<String>,
    pub pull: Pull,
    pub policy: Policy,
    pub expected_signers: Vec<String>,
    /// What this machine refuses to deploy. A file read, so `rime status` can
    /// print it without touching the network — and it must, because a machine
    /// that enforces nothing and a machine that enforces everything look
    /// identical in every other line of this report.
    pub enforcement: crate::verify::Enforcement,
    pub registry: Option<Registry>,
}

/// Build the offline half. Reads files; runs nothing.
pub fn offline_report(roots: &Roots) -> Report {
    let (image, image_error, pull) = match booted_origin(roots) {
        Ok(origin) => match origin_image_reference(&origin) {
            Some(reference) => {
                let (pull, image) = parse_origin_reference(&reference);
                (image, None, pull)
            }
            // An ostree deployment with no `container-image-reference` was not
            // deployed from a container at all. That is a real, measured state
            // — an ostree-native install — so it carries no error: dressing it
            // as `unavailable` would send the reader looking for a permission
            // problem that is not there.
            None => (None, None, Pull::NotOstree),
        },
        Err(e) if e == "not-ostree" => (None, None, Pull::NotOstree),
        Err(e) => (None, Some(e.clone()), Pull::Unavailable(e)),
    };

    let policy = match roots.read(POLICY_PATH) {
        Ok(text) => policy_for(&text, image.as_deref().unwrap_or("")),
        Err(e) => Policy::Unavailable(e),
    };

    Report {
        image,
        image_error,
        pull,
        policy,
        expected_signers: expected_signers(roots),
        enforcement: crate::verify::enforcement(roots),
        registry: None,
    }
}

/// Build the report, asking the registry only when told to.
pub fn build(roots: &Roots, verify: bool) -> Report {
    let mut report = offline_report(roots);
    if verify {
        if let Some(reference) = report.image.clone() {
            report.registry = Some(verify_registry(roots, &reference));
        }
    }
    report
}

pub fn to_json(r: &Report) -> Value {
    let mut root = Map::new();
    root.insert("image".into(), r.image.clone().map(Value::from).unwrap_or(Value::Null));
    if let Some(e) = &r.image_error {
        root.insert("imageError".into(), Value::from(e.clone()));
    }
    let mut pull = Map::new();
    pull.insert("state".into(), Value::from(r.pull.as_str()));
    match &r.pull {
        Pull::Unavailable(why) => {
            pull.insert("reason".into(), Value::from(why.clone()));
        }
        Pull::Remote(name) => {
            pull.insert("remote".into(), Value::from(name.clone()));
        }
        Pull::Unknown(raw) => {
            pull.insert("reference".into(), Value::from(raw.clone()));
        }
        _ => {}
    }
    root.insert("pullVerification".into(), Value::Object(pull));

    let mut pol = Map::new();
    pol.insert("state".into(), Value::from(r.policy.as_str()));
    match &r.policy {
        Policy::Unavailable(why) => {
            pol.insert("reason".into(), Value::from(why.clone()));
        }
        Policy::RequiresSignature(kind) => {
            pol.insert("requirement".into(), Value::from(kind.clone()));
        }
        _ => {}
    }
    root.insert("nextPullPolicy".into(), Value::Object(pol));
    // `expectedSigner` stays, as the first identity, for anything that read the
    // single-identity report; `expectedSigners` is the whole set.
    root.insert(
        "expectedSigner".into(),
        Value::from(r.expected_signers.first().cloned().unwrap_or_default()),
    );
    root.insert("expectedSigners".into(), Value::from(r.expected_signers.clone()));

    root.insert(
        "enforcement".into(),
        json!({
            "signature": r.enforcement.signature.as_str(),
            "provenance": r.enforcement.provenance.as_str(),
            "notes": r.enforcement.notes,
        }),
    );

    match &r.registry {
        None => {
            root.insert("registry".into(), Value::Null);
            // The absence of a registry section must not read as "we asked and
            // found nothing".
            root.insert(
                "registryNote".into(),
                Value::from("not contacted; pass --verify to ask the registry"),
            );
        }
        Some(reg) => {
            let mut m = Map::new();
            m.insert("digest".into(), reg.digest.clone().map(Value::from).unwrap_or(Value::Null));
            if let Some(e) = &reg.digest_error {
                m.insert("digestError".into(), Value::from(e.clone()));
            }
            match (&reg.verification, &reg.decision) {
                (Some(v), Some(d)) => {
                    // Produced by `crate::verify`, not rebuilt here. `rime
                    // trust --json` and the gate `rime update` consults have
                    // to agree about which word means which state, and two
                    // functions writing the same object is how they stop.
                    let ver = crate::verify::to_json(v, &r.enforcement, d);
                    let Value::Object(mut ver) = ver else { unreachable!("to_json is an object") };
                    // This report has always called the attestation
                    // `sbomAttestation`; the gate calls the same thing
                    // provenance, because that is the name of what it
                    // enforces. Renamed rather than duplicated: two keys for
                    // one fact is two things for a consumer to disagree about.
                    if let Some(p) = ver.remove("provenance") {
                        ver.insert("sbomAttestation".into(), p);
                    }
                    // Already at the top level of this report, where `rime
                    // status` reads it without a network round trip.
                    ver.remove("enforcement");
                    m.extend(ver);
                }
                _ => {
                    // Every state that is not a verification carries its
                    // reason, so no consumer can read a missing field as a
                    // signature that was checked and found wanting.
                    let why = reg
                        .note
                        .clone()
                        .or_else(|| reg.digest_error.clone())
                        .unwrap_or_else(|| "nothing was verified".to_string());
                    let unrun = json!({ "state": "could-not-run", "reason": why });
                    m.insert("signature".into(), unrun.clone());
                    m.insert("sbomAttestation".into(), unrun);
                    m.insert("decision".into(), Value::from("not-decided"));
                }
            }
            root.insert("registry".into(), Value::Object(m));
        }
    }
    Value::Object(root)
}

/// The three offline lines both renderings share.
fn render_offline(r: &Report) -> String {
    let mut s = String::from("Trust\n");
    match (&r.image, &r.image_error) {
        (Some(image), _) => s.push_str(&format!("  image         : {image}\n")),
        (None, Some(why)) => s.push_str(&format!("  image         : unavailable — {why}\n")),
        (None, None) => s.push_str("  image         : not deployed from a container image\n"),
    }
    s.push_str(&format!("  pulled        : {}\n", pull_sentence(&r.pull)));
    s.push_str(&format!("  next update   : {}\n", policy_sentence(&r.policy)));
    s
}

/// The compact block `rime status` prints. Four lines at most, no network.
///
/// The last line exists so silence is not read as an answer: a trust section
/// that stopped after the local facts would leave a reader assuming the
/// registry agreed with them.
pub fn render_block(r: &Report) -> String {
    let mut s = render_offline(r);
    s.push_str(&format!("  enforced      : {}\n", enforcement_sentence(&r.enforcement)));
    s.push_str("  registry      : not contacted — `rime trust --verify` asks it\n");
    s
}

/// What this machine will refuse, in one line.
///
/// Worded as an action rather than a setting name. "signature enforce" is a
/// configuration dump; "refuses an image whose signature does not verify" is
/// the sentence a reader can check against what happened to them.
pub fn enforcement_sentence(e: &crate::verify::Enforcement) -> String {
    use crate::verify::Strictness::*;
    match (e.signature, e.provenance) {
        (Off, Off) => "nothing — this machine will deploy an image nobody signed".to_string(),
        (Enforce, Enforce) => {
            "an image whose signature or SBOM attestation does not verify is refused".to_string()
        }
        (Enforce, _) => {
            "an image whose signature does not verify is refused; provenance is advisory"
                .to_string()
        }
        (_, Enforce) => {
            "an image whose SBOM attestation does not verify is refused; the signature is advisory"
                .to_string()
        }
        _ => "a signature or attestation that fails is refused; a missing one only warns"
            .to_string(),
    }
}

fn pull_sentence(p: &Pull) -> String {
    match p {
        // Deliberately not naming a scheme. `ostree-unverified-registry:` and
        // `ostree-unverified-image:` both land here, and printing the first
        // for a machine deployed with the second would be a small, confident
        // lie in the one report whose value is that it does not tell any.
        Pull::Unverified => {
            "no signature was checked — the deployment's origin records an unverified pull"
                .to_string()
        }
        Pull::ImageSigned => "the signature policy was applied".to_string(),
        Pull::Remote(name) => format!("verified against ostree remote {name}"),
        Pull::Unknown(raw) => format!("unrecognised verification mode in {raw}"),
        Pull::NotOstree => "not an ostree container deployment".to_string(),
        Pull::Unavailable(why) => format!("unavailable — {why}"),
    }
}

fn policy_sentence(p: &Policy) -> String {
    match p {
        Policy::AcceptsAnything => {
            format!("{POLICY_PATH} accepts any image, signed or not")
        }
        Policy::Rejects => format!("{POLICY_PATH} rejects this image"),
        Policy::RequiresSignature(kind) => {
            format!("{POLICY_PATH} requires a signature ({kind})")
        }
        Policy::NoRequirement => format!("{POLICY_PATH} states no requirement for this image"),
        Policy::Unavailable(why) => format!("unavailable — {why}"),
    }
}

/// The full report `rime trust` prints.
pub fn render(r: &Report) -> String {
    let Some(reg) = &r.registry else {
        // Without `--verify` the full report is the status block: there is
        // nothing more to say, and inventing a section would suggest there is.
        let mut s = render_block(r);
        s.push_str(&signer_block(&r.expected_signers));
        return s;
    };
    let mut s = render_offline(r);
    s.push_str(&format!("  enforced      : {}\n", enforcement_sentence(&r.enforcement)));
    match (&reg.digest, &reg.digest_error) {
        (Some(d), _) => s.push_str(&format!("  digest        : {d}\n")),
        (None, Some(e)) => s.push_str(&format!("  digest        : unavailable — {e}\n")),
        (None, None) => s.push_str("  digest        : unavailable\n"),
    }
    if let Some(note) = &reg.note {
        s.push_str(&format!("  note          : {note}\n"));
    }
    match (&reg.verification, &reg.decision) {
        (Some(v), Some(d)) => {
            s.push('\n');
            s.push_str("Verification\n");
            s.push_str(&crate::verify::render(v, &r.enforcement, d));
        }
        _ => {
            s.push_str("  signature     : not checked — nothing was verified\n");
        }
    }
    if r.expected_signers != default_signers()
        || !matches!(
            reg.verification.as_ref().map(|v| &v.signature),
            Some(crate::verify::Verdict::Verified { .. })
        )
    {
        s.push_str(&signer_block(&r.expected_signers));
    }
    s
}

/// The "Expected signer" section: every accepted identity, one per line.
fn signer_block(signers: &[String]) -> String {
    let mut s = String::from(if signers.len() == 1 {
        "\nExpected signer\n"
    } else {
        "\nExpected signer (any of)\n"
    });
    for id in signers {
        s.push_str(&format!("  {id}\n"));
    }
    s
}

/// `rime trust --gate` — what `rime update` would decide, without updating.
///
/// It calls exactly the functions the update path calls, in the same order,
/// and prints the refusal from the same producer. A second implementation of
/// "would this be accepted" is a second thing to drift, and the whole reason
/// this unit exists is that a machine's report and its behaviour had drifted
/// as far as they can: it said nothing was checked, and nothing was.
fn gate_main(roots: &Roots, json: bool) -> i32 {
    let report = offline_report(roots);
    // Handed to the gate rather than unwrapped here, for the same reason
    // `ops::update` does: an origin file nobody could read is not a reason to
    // skip the check, it is a reason nothing could be checked.
    let found = match (&report.image, &report.image_error) {
        (Some(r), _) => Ok(Some(r.as_str())),
        (None, Some(e)) => Err(e.as_str()),
        (None, None) => Ok(None),
    };
    let reference = report.image.clone().unwrap_or_else(|| "(none)".to_string());
    let g = crate::verify::gate(roots, found);
    if json {
        let mut j = crate::verify::to_json(&g.verification, &g.enforcement, &g.decision);
        if let Value::Object(m) = &mut j {
            m.insert("reference".into(), Value::from(reference));
        }
        println!("{}", serde_json::to_string_pretty(&j).unwrap_or_default());
    } else {
        print!("{}", crate::verify::render(&g.verification, &g.enforcement, &g.decision));
        if let Some(why) =
            crate::verify::refusal(&g.verification, &g.enforcement, &g.decision, "--allow-unverified")
        {
            eprint!("{why}");
        }
    }
    i32::from(g.decision.refuses())
}

pub fn main(args: TrustArgs) -> i32 {
    let roots = Roots::from_env();
    if args.gate {
        return gate_main(&roots, args.json);
    }
    let report = build(&roots, args.verify);
    if args.json {
        println!("{}", serde_json::to_string_pretty(&to_json(&report)).unwrap_or_default());
    } else {
        print!("{}", render(&report));
    }
    // A machine whose pull was unverified is the normal state today and must
    // not make a status command exit non-zero — that would put every Rime
    // machine's `rime trust` in a failing state in every script that runs it.
    // Only a verification that ran and FAILED is an error.
    //
    // Deliberately NOT the gate's decision. `Absent` plus `signature=enforce`
    // refuses an update, and rightly, but it is not this command reporting a
    // failure — it is this command reporting that nobody signed the image, and
    // an offline machine or a fork with no signatures would then have every
    // script that calls `rime trust` start failing. `rime update` is where a
    // refusal belongs, and it has its own exit code.
    let failed = report.registry.as_ref().and_then(|r| r.verification.as_ref()).is_some_and(|v| {
        matches!(v.signature, crate::verify::Verdict::Failed(_))
            || matches!(v.provenance, crate::verify::Verdict::Failed(_))
    });
    i32::from(failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_four_ostree_verification_prefixes_are_told_apart() {
        let (p, r) = parse_origin_reference(
            "ostree-unverified-registry:ghcr.io/andrenijman/rime-os:daily",
        );
        assert_eq!(p, Pull::Unverified);
        assert_eq!(r.as_deref(), Some("ghcr.io/andrenijman/rime-os:daily"));

        let (p, r) = parse_origin_reference("ostree-image-signed:registry:ghcr.io/x/y:z");
        assert_eq!(p, Pull::ImageSigned);
        assert_eq!(r.as_deref(), Some("ghcr.io/x/y:z"));

        let (p, r) = parse_origin_reference("ostree-remote-image:fedora:registry:quay.io/a/b:c");
        assert_eq!(p, Pull::Remote("fedora".into()));
        assert_eq!(r.as_deref(), Some("quay.io/a/b:c"));

        let (p, _) = parse_origin_reference("ostree-unverified-image:docker://ghcr.io/x/y:z");
        assert_eq!(p, Pull::Unverified);
    }

    #[test]
    fn an_unsigned_pull_is_never_reported_as_signed() {
        // The exact string on Andre's L16, read out of
        // /ostree/deploy/default/deploy/<csum>.0.origin.
        let origin = "[origin]\ncontainer-image-reference=ostree-unverified-registry:ghcr.io/andrenijman/rime-os:daily\n";
        let reference = origin_image_reference(origin).unwrap();
        let (pull, _) = parse_origin_reference(&reference);
        assert_eq!(pull, Pull::Unverified);
        assert!(pull_sentence(&pull).contains("no signature was checked"));
    }

    #[test]
    fn the_shipped_policy_is_reported_as_accepting_anything() {
        // Byte-identical to /usr/etc/containers/policy.json in the image, which
        // is what /etc/containers/policy.json is a copy of.
        let shipped = r#"{
            "default": [{"type": "insecureAcceptAnything"}],
            "transports": {"docker-daemon": {"": [{"type":"insecureAcceptAnything"}]}}
        }"#;
        assert_eq!(
            policy_for(shipped, "ghcr.io/andrenijman/rime-os:daily"),
            Policy::AcceptsAnything
        );
    }

    #[test]
    fn a_narrower_docker_scope_beats_a_strict_default() {
        // The mistake this guards: reading `default` first would report
        // "signatures required" on a machine that accepts anything from the
        // one registry it pulls from.
        let doc = r#"{
            "default": [{"type": "sigstoreSigned"}],
            "transports": {"docker": {
                "ghcr.io/andrenijman/rime-os": [{"type": "insecureAcceptAnything"}]
            }}
        }"#;
        assert_eq!(
            policy_for(doc, "ghcr.io/andrenijman/rime-os:daily"),
            Policy::AcceptsAnything
        );
        // A different repository on the same registry still gets the default.
        assert_eq!(
            policy_for(doc, "ghcr.io/someone/else:latest"),
            Policy::RequiresSignature("sigstoreSigned".into())
        );
    }

    #[test]
    fn a_registry_wide_scope_covers_a_repository_under_it() {
        let doc = r#"{
            "default": [{"type": "reject"}],
            "transports": {"docker": {
                "ghcr.io": [{"type": "sigstoreSigned"}]
            }}
        }"#;
        assert_eq!(
            policy_for(doc, "ghcr.io/andrenijman/rime-os:daily"),
            Policy::RequiresSignature("sigstoreSigned".into())
        );
    }

    #[test]
    fn one_permissive_entry_makes_the_whole_scope_permissive() {
        // containers-policy accepts if ANY requirement accepts. A report that
        // named the strict entry because it is listed would describe a machine
        // that does not exist.
        let doc = r#"{"default": [
            {"type": "sigstoreSigned"},
            {"type": "insecureAcceptAnything"}
        ]}"#;
        assert_eq!(policy_for(doc, "ghcr.io/x/y:z"), Policy::AcceptsAnything);
    }

    #[test]
    fn an_unparseable_policy_is_unavailable_rather_than_permissive() {
        let p = policy_for("{not json", "ghcr.io/x/y:z");
        assert_eq!(p.as_str(), "unavailable");
        assert_ne!(p, Policy::AcceptsAnything);
    }

    #[test]
    fn a_policy_we_may_not_read_is_unavailable_rather_than_permissive() {
        // The EACCES case, through the real reader. A directory sealed to 0000
        // makes the read fail with PermissionDenied rather than NotFound, and
        // the seal is verified before the assertion so a root test run does not
        // report a phantom pass.
        let dir = std::env::temp_dir().join(format!("rime-trust-eacces-{}", std::process::id()));
        let etc = dir.join("etc/containers");
        std::fs::create_dir_all(&etc).unwrap();
        std::fs::write(etc.join("policy.json"), "{}").unwrap();
        let sealed = dir.join("etc");
        set_mode(&sealed, 0o000);
        let refused = std::fs::read_to_string(etc.join("policy.json"))
            .err()
            .map(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
            .unwrap_or(false);
        if refused {
            let roots = Roots { fixture: Some(dir.clone()) };
            let r = offline_report(&roots);
            assert_eq!(r.policy.as_str(), "unavailable");
            assert_ne!(r.policy, Policy::AcceptsAnything);
            assert!(policy_sentence(&r.policy).contains("unavailable"));
        } else {
            eprintln!("skipping: this user walks through a 0000 directory (root or CAP_DAC_OVERRIDE)");
        }
        set_mode(&sealed, 0o755);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(refused || unsafe { libc::geteuid() } == 0);
    }

    fn set_mode(p: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode));
    }

    #[test]
    fn an_unreachable_registry_is_never_reported_as_unsigned() {
        // Every one of these is distance, and calling any of them "absent"
        // would tell a user their image is unsigned because their wifi is off.
        for stderr in [
            "dial tcp: lookup ghcr.io: no such host",
            "x509: certificate signed by unknown authority",
            "unauthorized: authentication required",
            "toomanyrequests: retry later",
            "",
        ] {
            let a = classify_skopeo_failure(stderr);
            assert!(matches!(a, Artifact::Unavailable(_)), "stderr: {stderr:?} gave {a:?}");
            assert_ne!(a, Artifact::Absent);
        }
        // And the one answer that IS absence: the registry said so.
        assert_eq!(
            classify_skopeo_failure(
                "level=fatal msg=\"reading manifest sha256-abc.sig in ghcr.io/x/y: manifest unknown\""
            ),
            Artifact::Absent
        );
    }

    #[test]
    fn an_unavailable_artifact_says_it_is_not_the_same_as_unsigned() {
        // The sentence moved from `artifact_sentence` to the verification
        // block, and the property did not move with it by accident: a report
        // that says "not checked" where it means "not signed" is the same lie
        // in a new module. So a `CouldNotRun` must print its reason and must
        // never print the words this report reserves for absence.
        let out = render(&report_with(
            crate::verify::Verdict::CouldNotRun("no route to host".into()),
            crate::verify::Verdict::CouldNotRun("no route to host".into()),
        ));
        assert!(out.contains("not checked — no route to host"), "{out}");
        assert!(!out.contains("none published"), "{out}");
        assert!(!out.contains("DOES NOT VERIFY"), "{out}");
    }

    /// A report whose registry half carries exactly these two verdicts.
    fn report_with(signature: crate::verify::Verdict, provenance: crate::verify::Verdict) -> Report {
        let v = crate::verify::Verification {
            digest: "sha256:abc".into(),
            repo: "ghcr.io/andrenijman/rime-os".into(),
            signature,
            provenance,
        };
        let e = crate::verify::Enforcement::default();
        let d = crate::verify::decide(&v, &e);
        Report {
            image: Some("ghcr.io/andrenijman/rime-os:daily".into()),
            image_error: None,
            pull: Pull::Unverified,
            policy: Policy::AcceptsAnything,
            expected_signers: default_signers(),
            enforcement: e,
            registry: Some(Registry {
                digest: Some("sha256:abc".into()),
                digest_error: None,
                verification: Some(v),
                decision: Some(d),
                note: None,
            }),
        }
    }

    #[test]
    fn the_rollout_identity_is_the_image_identity_with_the_workflow_swapped() {
        // The document has to be publishable between builds, so it cannot carry
        // the build's signature. Deriving rather than declaring means a fork
        // that overrides the image signer gets a consistent rollout signer for
        // free — and forgetting the second file would not break loudly, it
        // would make every rollout document fail to verify, which a machine
        // reports as "no rollout is staged".
        let none = Roots { fixture: Some(PathBuf::from("/nonexistent-fixture-root")) };
        assert_eq!(
            expected_rollout_signers(&none),
            vec![
                "https://github.com/AndreNijman/apex-os/.github/workflows/promote-channel.yml@refs/heads/main",  // rime-rename: keep
                "https://github.com/AndreNijman/rime-os/.github/workflows/promote-channel.yml@refs/heads/main",
            ]
        );
        // And it is NOT the image identity, which is the mistake worth failing
        // on: both live in the same repository and are signed by the same
        // account, so "same signer" is the plausible wrong answer.
        for id in expected_rollout_signers(&none) {
            assert!(!expected_signers(&none).contains(&id), "{id}");
        }

        // A downstream that overrides the image signer is followed.
        let dir = std::env::temp_dir().join(format!("rime-rollout-signer-{}", std::process::id()));
        let trust = dir.join(
            std::path::Path::new(rime_paths::TRUST_EXPECTED_SIGNER.trim_start_matches('/')).parent().unwrap(),
        );
        std::fs::create_dir_all(&trust).unwrap();
        std::fs::write(
            trust.join("expected-signer"),
            "https://github.com/acme/rime-os/.github/workflows/build-image.yml@refs/heads/release\n",
        )
        .unwrap();
        let forked = Roots { fixture: Some(dir.clone()) };
        assert_eq!(
            expected_rollout_signers(&forked),
            vec!["https://github.com/acme/rime-os/.github/workflows/promote-channel.yml@refs/heads/release"]
        );
        // An override that names something else entirely is left alone rather
        // than rewritten into a path nobody publishes under.
        std::fs::write(trust.join("expected-signer"), "https://example.invalid/whoever\n").unwrap();
        assert_eq!(expected_rollout_signers(&forked), vec!["https://example.invalid/whoever"]);
        // And the explicit override wins over the derivation.
        std::fs::write(trust.join("expected-rollout-signer"), "https://example.invalid/ramp\n").unwrap();
        assert_eq!(expected_rollout_signers(&forked), vec!["https://example.invalid/ramp"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_cosign_tag_is_the_digest_with_its_colon_turned_into_a_dash() {
        assert_eq!(
            cosign_tag("sha256:308127d9cefeada9", ".sig").as_deref(),
            Some("sha256-308127d9cefeada9.sig")
        );
        assert_eq!(cosign_tag("sha256:abc", ".att").as_deref(), Some("sha256-abc.att"));
        assert_eq!(cosign_tag("not-a-digest", ".sig"), None);
        assert_eq!(cosign_tag("sha256:", ".sig"), None);
        assert_eq!(cosign_tag("sha256:zz zz", ".sig"), None);
    }

    #[test]
    fn the_signer_identity_is_read_out_of_the_certificate_san() {
        // Trimmed from `openssl x509 -text` over the certificate cosign
        // attached to sha256:308127d9…, the image the L16 is booted on.
        let text = "        X509v3 extensions:\n\
                    \x20           X509v3 Subject Alternative Name: critical\n\
                    \x20               URI:https://github.com/AndreNijman/apex-os/.github/workflows/build-image.yml@refs/heads/main\n\
                    \x20           1.3.6.1.4.1.57264.1.1:\n";
        assert_eq!(san_uri(text).as_deref(), Some(EXPECTED_SIGNER));
        assert_eq!(san_uri("no san here"), None);
    }

    #[test]
    fn a_claimed_signer_is_never_printed_as_verified() {
        // This test used to assert that an unverified certificate's own claim
        // about its signer was labelled "claimed … not checked here", because
        // that claim was all the report had. It now asserts something
        // stronger, and the reason the wording changed is the unit: the
        // certificate's self-description is not printed AT ALL unless it was
        // verified. A signer name on the screen is now a verified signer name.
        let out = render(&report_with(
            crate::verify::Verdict::CouldNotRun(
                "the pinned Fulcio root could not be read".into(),
            ),
            crate::verify::Verdict::Absent("the registry holds no SBOM attestation".into()),
        ));
        assert!(out.contains("not checked"), "{out}");
        assert!(!out.contains("verified — signed by"), "{out}");
        // And the identity is still on the screen, as the expectation it is,
        // never as an assertion about who signed.
        assert!(out.contains("Expected signer"), "{out}");
    }

    #[test]
    fn a_verified_signature_is_the_only_state_that_prints_a_signer() {
        let out = render(&report_with(
            crate::verify::Verdict::Verified {
                signer: EXPECTED_SIGNER.to_string(),
                at: "the certificate's own notBefore, unix 1757079805".into(),
            },
            crate::verify::Verdict::Absent("the registry holds no SBOM attestation".into()),
        ));
        assert!(out.contains(&format!("verified — signed by {EXPECTED_SIGNER}")), "{out}");
        // The transparency log is NOT checked, and a report that says
        // "verified" without saying so is overclaiming.
        assert!(out.contains("transparency log was not checked"), "{out}");
    }

    #[test]
    fn the_status_block_says_what_this_machine_refuses() {
        // A machine that enforces nothing and a machine that enforces
        // everything are identical in every other line of this report, so the
        // line that tells them apart is not optional.
        let mut r = offline_report(&Roots { fixture: Some(std::env::temp_dir().join("nope")) });
        r.enforcement = crate::verify::Enforcement::default();
        assert!(render_block(&r).contains("signature does not verify is refused"), "{}", render_block(&r));
        r.enforcement = crate::verify::Enforcement {
            signature: crate::verify::Strictness::Off,
            provenance: crate::verify::Strictness::Off,
            notes: Vec::new(),
        };
        let out = render_block(&r);
        assert!(out.contains("deploy an image nobody signed"), "{out}");
    }

    #[test]
    fn json_says_the_registry_was_not_contacted_rather_than_leaving_it_null_alone() {
        let r = Report {
            image: Some("ghcr.io/x/y:z".into()),
            image_error: None,
            pull: Pull::Unverified,
            policy: Policy::AcceptsAnything,
            expected_signers: default_signers(),
            enforcement: crate::verify::Enforcement::default(),
            registry: None,
        };
        let j = to_json(&r);
        assert!(j["registry"].is_null());
        assert!(j["registryNote"].as_str().unwrap().contains("not contacted"));
    }

    #[test]
    fn every_unavailable_state_carries_its_reason_into_the_json() {
        let r = Report {
            image: None,
            image_error: Some("/proc/cmdline: Permission denied".into()),
            pull: Pull::Unavailable("/proc/cmdline: Permission denied".into()),
            policy: Policy::Unavailable("/etc/containers/policy.json: Permission denied".into()),
            expected_signers: default_signers(),
            enforcement: crate::verify::Enforcement::default(),
            registry: None,
        };
        let j = to_json(&r);
        assert_eq!(j["pullVerification"]["state"], "unavailable");
        assert!(j["pullVerification"]["reason"]
            .as_str()
            .unwrap()
            .contains("Permission denied"));
        assert_eq!(j["nextPullPolicy"]["state"], "unavailable");
        assert!(j["nextPullPolicy"]["reason"].as_str().unwrap().contains("Permission denied"));
    }

    #[test]
    fn a_missing_ostree_argument_is_not_the_same_as_a_cmdline_nobody_read() {
        let dir = std::env::temp_dir().join(format!("rime-trust-cmdline-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("proc")).unwrap();
        std::fs::create_dir_all(dir.join("etc/containers")).unwrap();
        std::fs::write(dir.join("etc/containers/policy.json"), "{}").unwrap();

        // Read, and genuinely has no ostree= argument.
        std::fs::write(dir.join("proc/cmdline"), "root=UUID=x rw quiet\n").unwrap();
        let r = offline_report(&Roots { fixture: Some(dir.clone()) });
        assert_eq!(r.pull, Pull::NotOstree);

        // Not read at all.
        std::fs::remove_file(dir.join("proc/cmdline")).unwrap();
        let r = offline_report(&Roots { fixture: Some(dir.clone()) });
        assert_eq!(r.pull.as_str(), "unavailable");
        assert_ne!(r.pull, Pull::NotOstree);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_status_block_never_claims_the_registry_was_asked() {
        let r = Report {
            image: Some("ghcr.io/x/y:z".into()),
            image_error: None,
            pull: Pull::Unverified,
            policy: Policy::AcceptsAnything,
            expected_signers: default_signers(),
            enforcement: crate::verify::Enforcement::default(),
            registry: None,
        };
        let b = render_block(&r);
        assert!(b.contains("not contacted"), "{b}");
        assert!(!b.contains("unsigned"), "{b}");
    }
}
