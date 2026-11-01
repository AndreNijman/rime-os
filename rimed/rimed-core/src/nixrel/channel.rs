//! The Nix channel document (docs/nixos/updates-and-release.md §4): which release manifest each channel points at.
//!
//! Same shape as the bootc rollout document ([`crate::channel::RolloutDoc`]) plus a `release` per entry, but a
//! different role, so a different decision: on bootc the document only ramps a tag the machine resolves itself, and
//! every uncertainty admits; on NixOS the document IS the pointer (its `digest` is the manifest's), so every
//! uncertainty refuses. The caller has verified the signature before this sees a byte.

use serde::{Deserialize, Serialize};

use super::release::ReleaseId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelEntry {
    pub channel: String,
    pub digest: String,
    #[serde(default = "everyone")]
    pub percent: u8,
    #[serde(default)]
    pub halt: bool,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub release: Option<String>,
}

fn everyone() -> u8 {
    100
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelDoc {
    #[serde(default)]
    pub schema: Option<u32>,
    pub serial: u64,
    pub issued: u64,
    pub expires: u64,
    pub repository: String,
    pub channels: Vec<ChannelEntry>,
}

/// A document older than this is still used, with a warning (§14 F24).
pub const STALE_AFTER_S: u64 = 3 * 86_400;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelVerdict {
    /// Install the manifest at `digest`.
    Take { digest: String, release: String },
    /// This machine's slot is outside the ramp (exit 0, passes run).
    Held { slot: u8, percent: u8 },
    /// The publisher stopped this release (exit 0, passes run).
    Halted { reason: String },
    /// Past its expiry: the caller fetches once more, then refuses (F5).
    Expired { expires: u64 },
    /// Nothing is published on this channel yet.
    NothingPublished { channel: String },
    /// Any other reason not to use it (F6 replay, wrong repository, bad entry). Carries the user's text.
    Refused(String),
}

pub struct Query<'a> {
    pub channel: &'a str,
    pub repository: &'a str,
    pub now: u64,
    /// The highest serial this machine accepted (from a VERIFIED document only).
    pub seen_serial: Option<u64>,
    pub slot: u8,
}

pub fn parse(bytes: &[u8]) -> Result<ChannelDoc, String> {
    serde_json::from_slice(bytes).map_err(|e| format!("the channel document does not parse: {e}"))
}

fn is_digest(d: &str) -> bool {
    d.strip_prefix("sha256:").is_some_and(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The decision, plus warning lines (F24).
pub fn decide(doc: &ChannelDoc, q: &Query) -> (ChannelVerdict, Vec<String>) {
    let mut warn = Vec::new();
    let refuse = |s: String| (ChannelVerdict::Refused(s), Vec::new());
    if doc.schema.unwrap_or(1) != 1 {
        return refuse(format!("rime: the channel document is schema {}, which needs a newer rime", doc.schema.unwrap_or(1)));
    }
    if doc.repository != q.repository {
        return refuse(format!(
            "rime: the channel document governs {}, not {}, so it was not used",
            doc.repository, q.repository
        ));
    }
    if let Some(seen) = q.seen_serial {
        if doc.serial < seen {
            return refuse(format!(
                "rime: the channel document is serial {} and this machine has already accepted {seen}; an older document served again is a replay, so it was ignored",
                doc.serial
            ));
        }
    }
    if q.now > doc.expires {
        return (ChannelVerdict::Expired { expires: doc.expires }, warn);
    }
    if q.now.saturating_sub(doc.issued) > STALE_AFTER_S {
        warn.push(format!(
            "rime: the newest release information this machine can get is {} days old.",
            q.now.saturating_sub(doc.issued) / 86_400
        ));
    }
    let Some(e) = doc.channels.iter().find(|e| e.channel == q.channel) else {
        return (ChannelVerdict::NothingPublished { channel: q.channel.to_string() }, warn);
    };
    if e.halt {
        let reason = e.reason.clone().unwrap_or_else(|| "the publisher gave no reason".into());
        return (ChannelVerdict::Halted { reason }, warn);
    }
    if !is_digest(&e.digest) {
        return (ChannelVerdict::Refused(format!("rime: the {} entry names {:?}, which is not a digest", e.channel, e.digest)), warn);
    }
    let Some(release) = e.release.clone().filter(|r| ReleaseId::parse(r).is_some()) else {
        return (ChannelVerdict::Refused(format!("rime: the {} entry names no release ID", e.channel)), warn);
    };
    let percent = e.percent.min(100);
    if q.slot >= percent {
        return (ChannelVerdict::Held { slot: q.slot, percent }, warn);
    }
    (ChannelVerdict::Take { digest: e.digest.clone(), release }, warn)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPO: &str = "ghcr.io/andrenijman/rime-os";

    fn doc() -> ChannelDoc {
        ChannelDoc {
            schema: Some(1),
            serial: 7,
            issued: 1_000_000,
            expires: 2_000_000,
            repository: REPO.into(),
            channels: vec![ChannelEntry {
                channel: "edge".into(),
                digest: format!("sha256:{}", "a".repeat(64)),
                percent: 100,
                halt: false,
                reason: None,
                release: Some("2026.12.03".into()),
            }],
        }
    }

    fn q(seen: Option<u64>) -> Query<'static> {
        Query { channel: "edge", repository: REPO, now: 1_000_100, seen_serial: seen, slot: 42 }
    }

    #[test]
    fn a_current_document_names_the_release() {
        let (v, w) = decide(&doc(), &q(Some(7)));
        assert!(matches!(v, ChannelVerdict::Take { ref release, .. } if release == "2026.12.03"));
        assert!(w.is_empty());
    }

    #[test]
    fn replay_wrong_repo_expired_and_missing_channel_refuse() {
        assert!(matches!(decide(&doc(), &q(Some(8))).0, ChannelVerdict::Refused(s) if s.contains("replay")));
        let mut d = doc();
        d.repository = "ghcr.io/someone/fork".into();
        assert!(matches!(decide(&d, &q(None)).0, ChannelVerdict::Refused(_)));
        let mut late = q(None);
        late.now = 2_000_001;
        assert_eq!(decide(&doc(), &late).0, ChannelVerdict::Expired { expires: 2_000_000 });
        let mut beta = q(None);
        beta.channel = "beta";
        assert!(matches!(decide(&doc(), &beta).0, ChannelVerdict::NothingPublished { .. }));
    }

    #[test]
    fn halt_ramp_stale_and_bad_entries() {
        let mut d = doc();
        d.channels[0].halt = true;
        assert!(matches!(decide(&d, &q(None)).0, ChannelVerdict::Halted { .. }));
        let mut d = doc();
        d.channels[0].percent = 40;
        assert_eq!(decide(&d, &q(None)).0, ChannelVerdict::Held { slot: 42, percent: 40 });
        let mut old = q(None);
        old.now = 1_000_000 + 4 * 86_400;
        assert!(decide(&doc(), &old).1[0].contains("4 days old"));
        let mut d = doc();
        d.channels[0].digest = "latest".into();
        assert!(matches!(decide(&d, &q(None)).0, ChannelVerdict::Refused(_)));
        let mut d = doc();
        d.channels[0].release = None;
        assert!(matches!(decide(&d, &q(None)).0, ChannelVerdict::Refused(_)));
    }
}
