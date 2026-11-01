//! The two signed documents (§4, §3): the channel document at `nix-channels`, and the release manifest it names by
//! digest. Both go through `verify.rs` (skopeo, openssl, the pinned Fulcio root, SAN + issuer + payload binding),
//! against the Nix release identity from the configuration.

use rimed_core::nixrel::channel::{self, ChannelDoc, ChannelVerdict, Query};
use rimed_core::nixrel::manifest::{self, Manifest, Refusal};
use rimed_core::nixrel::{NIX_CHANNELS_MEDIA_TYPE, NIX_CHANNELS_TAG, NIX_RELEASE_MEDIA_TYPE};

use crate::trust::Roots;
use crate::verify::{self, Verdict};

use super::{now, read_json, state, write_json, Config};

/// A refusal: the exit code and the lines the user reads.
#[derive(Debug)]
pub struct Stop {
    pub code: i32,
    pub message: String,
}

fn stop(message: impl Into<String>) -> Stop {
    Stop { code: 1, message: message.into() }
}

/// How a signature verdict reads (§14 F2-F4). `None` = verified.
fn refusal(what: &str, v: &Verdict) -> Option<String> {
    match v {
        Verdict::Verified { .. } => None,
        Verdict::Absent(why) => Some(format!("rime: {what} is not signed ({why}), so it was not read")),
        Verdict::Failed(why) => Some(format!(
            "rime: this update is being held. The release it would install does not verify.\n  {what}: {why}"
        )),
        Verdict::CouldNotRun(why) => Some(format!(
            "rime: the signature on {what} could not be verified — which is not the same as failing to verify.\n  {why}"
        )),
    }
}

/// Verify the signature over `digest`; under `--allow-unverified` a refusal is printed and ignored.
fn check_signature(cfg: &Config, roots: &Roots, digest: &str, what: &str, allow_unverified: bool) -> Result<bool, Stop> {
    let v = verify::verify_signature_as(roots, &cfg.repository, digest, &cfg.expect());
    match refusal(what, &v) {
        None => Ok(true),
        Some(m) if allow_unverified => {
            eprintln!("{m}\nrime: continuing anyway (--allow-unverified)");
            Ok(false)
        }
        Some(m) => Err(stop(m)),
    }
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Seen {
    serial: u64,
}

pub enum Pointer {
    Take { digest: String, release: String },
    /// Held or halted: no OS step, the passes still run, exit 0.
    Hold(String),
}

fn fetch_channel_doc(cfg: &Config, roots: &Roots, allow_unverified: bool) -> Result<(ChannelDoc, bool), Stop> {
    let what = format!("{}:{NIX_CHANNELS_TAG}", cfg.repository);
    let digest = resolve_artifact(roots, &cfg.repository, NIX_CHANNELS_TAG)
        .map_err(|e| stop(format!("rime: could not reach the release server ({e}); nothing was changed.")))?;
    let verified = check_signature(cfg, roots, &digest, &what, allow_unverified)?;
    let bytes = verify::artifact_layer_at(roots, &cfg.repository, NIX_CHANNELS_TAG, &digest, NIX_CHANNELS_MEDIA_TYPE)
        .map_err(|e| stop(format!("rime: could not read {what} ({e}); nothing was changed.")))?;
    let doc = channel::parse(&bytes).map_err(|e| stop(format!("rime: {e}; nothing was changed.")))?;
    Ok((doc, verified))
}

/// The digest a tag names, as the sha256 of the manifest the registry serves for it. Not `skopeo inspect`
/// (`verify::resolve`): that parses the config as an image's, and the channel document is an OCI artifact with an
/// empty config (§3.3), which skopeo refuses ("unsupported image-specific operation on artifact").
fn resolve_artifact(roots: &Roots, repo: &str, tag: &str) -> Result<String, String> {
    let fetched = match verify::fetch(roots, repo, tag) {
        Ok(f) => f,
        Err(crate::trust::Artifact::Absent) => return Err(format!("{repo}:{tag} does not exist")),
        Err(crate::trust::Artifact::Unavailable(why)) => return Err(why),
    };
    verify::sha256_of(&fetched.blobs.join("manifest.json")).map(|h| format!("sha256:{h}"))
}

/// §8.2 step 3. `record` = remember the accepted serial (not under `--check`, never for an unverified document).
pub fn channel(cfg: &Config, roots: &Roots, allow_unverified: bool, force: bool, record: bool) -> Result<Pointer, Stop> {
    let seen_path = state("channels.json");
    let seen: Option<Seen> = read_json(&seen_path);
    let slot = rimed_core::channel::bucket(&std::fs::read_to_string("/etc/machine-id").unwrap_or_default());
    let mut refetched = false;
    loop {
        let (doc, verified) = fetch_channel_doc(cfg, roots, allow_unverified)?;
        let q = Query { channel: &cfg.channel, repository: &cfg.repository, now: now(), seen_serial: seen.as_ref().map(|s| s.serial), slot };
        let (verdict, warnings) = channel::decide(&doc, &q);
        for w in warnings {
            eprintln!("{w}");
        }
        let accept = |doc: &ChannelDoc| {
            // Only a VERIFIED document moves the replay bar: an unverified one with a huge serial must not block the
            // next genuine one (§15.2 refuse-replay).
            if record && verified {
                let _ = write_json(&seen_path, &Seen { serial: doc.serial }, 0o644);
            }
        };
        return match verdict {
            ChannelVerdict::Take { digest, release } => {
                accept(&doc);
                Ok(Pointer::Take { digest, release })
            }
            ChannelVerdict::Held { slot, percent } if !force => {
                accept(&doc);
                Ok(Pointer::Hold(format!("rime: this release is reaching {percent}% of machines and this one is slot {slot}; it is not this machine's turn yet")))
            }
            ChannelVerdict::Halted { reason } if !force => {
                accept(&doc);
                Ok(Pointer::Hold(format!("rime: the publisher stopped this release: {reason}")))
            }
            ChannelVerdict::Held { .. } | ChannelVerdict::Halted { .. } => {
                // --force skips the ramp and the halt, never the signature.
                let e = doc.channels.iter().find(|e| e.channel == cfg.channel).expect("entry exists");
                Ok(Pointer::Take { digest: e.digest.clone(), release: e.release.clone().unwrap_or_default() })
            }
            ChannelVerdict::Expired { expires } if !refetched => {
                let _ = expires;
                refetched = true;
                continue;
            }
            ChannelVerdict::Expired { expires } => Err(stop(format!(
                "rime: the newest channel document this machine can find expired on unix {expires}. Rime may be unable to publish, or something is holding updates back. The OS was not updated."
            ))),
            ChannelVerdict::NothingPublished { channel } => {
                Ok(Pointer::Hold(format!("rime: nothing is published on {channel} yet")))
            }
            ChannelVerdict::Refused(m) => Err(stop(m)),
        };
    }
}

pub struct Release {
    pub manifest: Manifest,
    pub digest: String,
    pub bytes: Vec<u8>,
    pub verified: bool,
}

/// §8.2 step 4, the trust gate: by digest, signature, then §3.2.
pub fn manifest(cfg: &Config, roots: &Roots, digest: &str, release: &str, allow_unverified: bool) -> Result<Release, Stop> {
    let what = format!("the release manifest for {release} ({}@{digest})", cfg.repository);
    let verified = check_signature(cfg, roots, digest, &what, allow_unverified)?;
    let bytes = verify::artifact_layer_at(roots, &cfg.repository, digest, digest, NIX_RELEASE_MEDIA_TYPE)
        .map_err(|e| stop(format!("rime: could not read {what} ({e}); nothing was changed.")))?;
    let m = manifest::parse(&bytes).map_err(|r: Refusal| stop(r.message()))?;
    if m.release != release {
        return Err(stop(format!(
            "rime: the release manifest for {release} is not usable: it says it is {}. Nothing was changed.",
            m.release
        )));
    }
    if m.variant(&cfg.variant).is_none() {
        return Err(stop(format!(
            "rime: release {release} has no {} system; this machine was installed as {}. Nothing was changed.",
            cfg.variant, cfg.variant
        )));
    }
    Ok(Release { manifest: m, digest: digest.to_string(), bytes, verified })
}
