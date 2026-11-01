//! Update channels, staged rollout, and the health signal that stops one.
//! Roadmap §26.
//!
//! Pure: no files, no processes, no network. The CLI measures the machine and
//! hands the measurements in, the same division `blueprint` and `task` use.
//!
//! ## What a channel is here
//!
//! A tag on one repository. `ghcr.io/andrenijman/rime-os:stable` and
//! `…:edge` name different digests of the same image, and which one a machine
//! follows is the `container-image-reference` in its deployment origin —
//! written at install time, changed by `bootc switch`. So a channel is not a
//! new build tier, a new edition, or a second update mechanism: it is which
//! tag `bootc upgrade` resolves.
//!
//! ## Every Rime machine is already on edge, and has never been told so
//!
//! `rime`, `daily`, `gaming-mesa` and `gaming-nvidia` are four names for one
//! digest, and `build-image.yml` moves all four on every successful build of
//! `main`. That is the definition of an edge channel. Andre's L16 follows
//! `:daily`. [`from_tag`] therefore maps those four to [`Channel::Edge`] and
//! keeps the alias, so `rime channel status` can say "tracking `daily`, which
//! moves with every build — that is the edge channel under an older name"
//! instead of "unknown". Reporting a machine's real state is the entire value
//! of this surface, and those four tags must go on meaning what they mean:
//! they are what installed machines track, and a tag that stops moving does
//! not error, it silently stops delivering security updates.
//!
//! ## Which direction is dangerous
//!
//! Moving toward `edge` takes a newer image, which is an ordinary update.
//! Moving toward `stable` usually takes an OLDER one, and that is the direction
//! that needs care for the reason §25 exists: `/usr` goes back and the state
//! the newer build migrated does not. [`Direction::Behind`] is the flag for
//! that, and the CLI pins the current deployment and prints §25's rollback
//! table before it switches.
//!
//! ## Staged rollout, without inventing a server
//!
//! A machine derives a stable bucket from its own `/etc/machine-id`
//! ([`bucket`]) and takes a release only when its bucket falls inside the
//! release's percentage ([`admits`]). The bucket never leaves the machine — it
//! is one of a hundred values, and it is not in the report `rime channel
//! report` would send.
//!
//! The percentage has to live somewhere that can change between builds — 1,
//! then 5, then 25 — and an OCI label is baked once per image, so a label
//! cannot be it. The pointer is a **signed rollout document**, published as its
//! own object in the same registry under the `rollout` tag and re-published
//! whenever the number moves. [`RolloutDoc`] is that document and
//! [`decide_rollout`] is the whole of what a machine does with one.
//!
//! Why the registry rather than a service: the machine already contacts that
//! registry on every update, so the document costs no new host, no new trust
//! root, and no new fact about the machine that anybody can observe. Rime
//! operates no server and this does not ask it to. The cost is stated in
//! `docs/update-channels.md`: a document in a registry is a **broadcast**, so
//! a ramp is per-channel and never per-machine, and there is no back channel —
//! the operator learns nothing about who took what except from the opt-in
//! health report, which is off by default.
//!
//! There is no second source. A machine that cannot reach the document, cannot
//! read it, or holds one that does not apply to it reaches everybody — which is
//! what every Rime image published so far does. An image label was named here
//! as a fallback for a while and was never stamped by anything; it is gone,
//! because a fallback the registry serves is no use to a machine that could not
//! reach the registry, and a percentage that reaches the gate without a
//! signature over it is the one thing this must not have.

use crate::recover::Health;

/// The four channels §26 names, ordered from most cautious to least.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Channel {
    Stable,
    Candidate,
    Beta,
    Edge,
}

impl Channel {
    pub const ALL: [Channel; 4] = [
        Channel::Stable,
        Channel::Candidate,
        Channel::Beta,
        Channel::Edge,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Candidate => "candidate",
            Channel::Beta => "beta",
            Channel::Edge => "edge",
        }
    }

    /// How far from `stable`. 0 is stable, 3 is edge.
    pub fn rank(self) -> u8 {
        match self {
            Channel::Stable => 0,
            Channel::Candidate => 1,
            Channel::Beta => 2,
            Channel::Edge => 3,
        }
    }

    /// What a person is choosing when they choose this channel.
    ///
    /// Written for somebody deciding, not for somebody who already knows the
    /// release process: what arrives, how often, and what it costs.
    pub fn summary(self) -> &'static str {
        match self {
            Channel::Stable => {
                "only builds that have run on the other channels first. The fewest updates, and the longest wait for a fix"
            }
            Channel::Candidate => {
                "a build being considered for stable. It has already been on beta"
            }
            Channel::Beta => "a build that has been on edge and looks sound",
            Channel::Edge => {
                "every successful build of main, as soon as it is published. New work arrives first here, and so do its faults"
            }
        }
    }
}

impl std::str::FromStr for Channel {
    type Err = String;

    fn from_str(s: &str) -> Result<Channel, String> {
        Channel::ALL
            .into_iter()
            .find(|c| c.as_str() == s)
            .ok_or_else(|| {
                format!(
                    "{s:?} is not a channel. The channels are: {}",
                    Channel::ALL
                        .iter()
                        .map(|c| c.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
    }
}

/// The tags that predate channels.
///
/// They are not editions any more — they are names for one digest, kept alive
/// because they are what installed machines track. All of them move on every
/// successful build of `main`, so a machine following one of them is on edge.
/// `apex` is the name the fleet was installed under and keeps it; `rime` is the
/// same digest under the new name, published beside it from the rename on.
pub const LEGACY_TAGS: [&str; 5] = ["apex", "rime", "daily", "gaming-mesa", "gaming-nvidia"];  // rime-rename: keep (apex is a tag machines track)

/// What a machine is following.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tracking {
    /// The channel, when the tag names one.
    pub channel: Option<Channel>,
    /// The tag as written in the deployment origin.
    pub tag: String,
    /// Set when the tag is one of [`LEGACY_TAGS`]: the same content as `edge`,
    /// under the name it had before channels existed.
    pub alias: Option<&'static str>,
}

/// Read a channel out of an image reference or a bare tag.
///
/// Accepts either `ghcr.io/owner/repo:tag` or `tag`. A tag nobody recognises is
/// not an error — a machine can legitimately follow a digest, a revision tag or
/// somebody's own build, and calling that "unknown channel" is more useful than
/// refusing to answer.
pub fn from_tag(reference: &str) -> Tracking {
    // Split off the tag: the last colon, but only when it comes after the last
    // slash. A colon before that is a port on the registry host.
    let tag = match (reference.rfind(':'), reference.rfind('/')) {
        (Some(c), Some(s)) if c > s => &reference[c + 1..],
        (Some(c), None) => &reference[c + 1..],
        _ => reference,
    };
    let channel = tag.parse::<Channel>().ok();
    let alias = LEGACY_TAGS.into_iter().find(|t| *t == tag);
    Tracking {
        // A legacy tag IS edge: same digest, moved by the same job, on every
        // build. Reporting it as no channel at all would leave every machine
        // that exists today unable to see where it stands.
        channel: channel.or(alias.map(|_| Channel::Edge)),
        tag: tag.to_string(),
        alias,
    }
}

/// Which way a channel change moves, in terms of what it does to the image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Toward `edge`. Takes a newer image — an ordinary update.
    Ahead,
    /// Toward `stable`. Usually takes an OLDER image, which is the direction
    /// §25 cares about: `/usr` goes back and the state a newer build migrated
    /// stays where it is.
    Behind,
    Same,
}

pub fn direction(from: Channel, to: Channel) -> Direction {
    match to.rank().cmp(&from.rank()) {
        std::cmp::Ordering::Greater => Direction::Ahead,
        std::cmp::Ordering::Less => Direction::Behind,
        std::cmp::Ordering::Equal => Direction::Same,
    }
}

// ── staged rollout ───────────────────────────────────────────────────────────

/// This machine's rollout bucket, 0 to 99.
///
/// FNV-1a over the machine id, because it needs no dependency and the
/// distribution only has to be even across a hundred buckets rather than
/// cryptographically uniform. Stable across reboots and across updates, which
/// is the property that matters: a machine must not creep into a 1% cohort by
/// re-rolling on every check.
///
/// It never leaves the machine. `rime channel report` does not carry it: one of
/// a hundred values is a hundredth of a machine id, and a supply-chain report
/// has no use for it.
pub fn bucket(machine_id: &str) -> u8 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in machine_id.trim().as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    (hash % 100) as u8
}

/// Whether a machine in `bucket` takes a release published at `percent`.
///
/// `percent` above 100 is clamped rather than refused: it arrives from a
/// document a person typed, and a typo there must not stop a machine updating.
pub fn admits(bucket: u8, percent: u8) -> bool {
    u32::from(bucket) < u32::from(percent.min(100))
}

/// What a machine with no usable rollout document is at, which is every Rime
/// machine today.
pub const FULL_ROLLOUT: u8 = 100;

// There is no OCI label for the ramp, and the absence is deliberate.
//
// `org.rimeos.rollout.percent` was named here for a while and nothing ever
// stamped it — MEASURED 2026-09-22: no Containerfile, no workflow and no script
// in either repository writes it, and the live image carries no such label. It
// could not have worked: the number has to move between builds and a label is
// baked once, which is the observation the signed document below exists to
// answer. The absence is written down rather than quietly left blank because a
// constant with a test beside it reads as a wired gate, and that is the mistake
// worth not repeating.


// ── the mutable pointer: a signed rollout document ───────────────────────────

/// The tag the rollout document is published under, in the same repository as
/// the image.
///
/// A separate object rather than a label on the image, because the number has
/// to move between builds and a label is baked once. A tag in the registry the
/// machine already talks to rather than an endpoint, because an endpoint is a
/// server somebody has to run, a second name to trust, and a record of which
/// machines asked.
pub const ROLLOUT_TAG: &str = "rollout";

/// The media type of the document layer inside that object.
pub const ROLLOUT_MEDIA_TYPE: &str = "application/vnd.apexos.rollout.v1+json";  // rime-rename: keep (media type Release A clients and promote-channel.yml agree on)

/// The schema this build understands. A document declaring a higher one is
/// ignored whole rather than read in part.
pub const ROLLOUT_SCHEMA: u32 = 1;

/// How old a document may be before the client stops believing it, whatever it
/// says about its own expiry.
///
/// Thirty days. Longer than any ramp worth the name — a rollout that takes more
/// than a month is not a rollout — and short enough that a publisher who stops
/// publishing releases every machine back to its own behaviour inside a month
/// instead of pinning it to a last instruction forever. `docs/fleet.md` asked
/// whether the freshness bound belongs in the document or in the client; the
/// answer is both, and the client's cap is the one that cannot be forgotten by
/// whoever writes the document.
pub const MAX_DOCUMENT_AGE: u64 = 30 * 24 * 60 * 60;

/// How far into the future a document may claim to have been issued.
///
/// A machine whose clock is wrong is common; a machine whose clock is wrong
/// must not silently ignore every document, and must not accept one minted for
/// a date it cannot have reached. An hour covers a timezone mistake and a
/// slow NTP, and nothing longer is needed because the ceiling above is
/// measured from `issued`.
pub const MAX_CLOCK_SKEW: u64 = 60 * 60;

/// One channel's current rollout state.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RolloutEntry {
    /// `stable`, `candidate`, `beta` or `edge`. A string rather than [`Channel`]
    /// so a document naming a channel this build has never heard of is data to
    /// be skipped rather than a parse failure that discards the whole document.
    pub channel: String,
    /// The digest this entry is about.
    ///
    /// Load-bearing. Without it a 5% entry written for candidate build X would
    /// go on holding machines back from build Y the moment the tag moved, and
    /// nobody would see it happen — the entry would still read as current.
    pub digest: String,
    /// The slots admitted, 0–100. 100 is everybody.
    pub percent: u8,
    /// Stop this digest reaching any further machines.
    #[serde(default)]
    pub halt: bool,
    /// Why, in the publisher's own words. Printed to the user verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The document itself.
///
/// Small on purpose. It carries the ramp and nothing else: no machine list, no
/// identifiers, no commands. A response that named something to run would be
/// remote execution through a data channel — `docs/fleet.md`'s first
/// never-build item, wearing a different hat.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RolloutDoc {
    /// Absent means 1, on the same rule §25 uses for every other stored schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<u32>,
    /// Monotonic. A client refuses to go backwards on it, because an old
    /// validly-signed document is otherwise a replay: "everybody takes this"
    /// re-served after it was halted.
    pub serial: u64,
    /// Unix seconds. What [`MAX_DOCUMENT_AGE`] is measured from.
    pub issued: u64,
    /// Unix seconds. The publisher's own bound, which may be shorter than the
    /// client's and may not be longer.
    pub expires: u64,
    /// The repository this document governs.
    ///
    /// The signature already binds the document to a repository, and this is
    /// checked anyway: `signature=warn` is a configuration a machine can be in,
    /// and a document lifted from one repository and served from another must
    /// not govern the second one even then.
    pub repository: String,
    pub channels: Vec<RolloutEntry>,
}

impl RolloutDoc {
    pub fn schema_version(&self) -> u32 {
        self.schema.unwrap_or(1)
    }

    pub fn entry(&self, channel: Channel) -> Option<&RolloutEntry> {
        self.channels.iter().find(|e| e.channel == channel.as_str())
    }
}

/// Where the percentage that governed this decision came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RolloutSource {
    /// The signed document in the registry.
    Document,
    /// Nothing said otherwise, so everybody.
    Default,
}

impl RolloutSource {
    pub fn as_str(self) -> &'static str {
        match self {
            RolloutSource::Document => "the signed rollout document",
            RolloutSource::Default => "nothing — no rollout is in progress",
        }
    }
}

/// What the update path should do about the staged rollout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rollout {
    /// Take it.
    Admitted { percent: u8, source: RolloutSource },
    /// This machine's slot is outside the ramp. It is not broken and there is
    /// nothing to fix; it is not its turn.
    Held { slot: u8, percent: u8 },
    /// The publisher stopped this release.
    Halted { reason: String },
}

/// Everything the decision needs, and nothing it can go and read for itself.
///
/// Pure, on the division the rest of this crate uses: the CLI measures the
/// machine and the registry, this decides. Every field is something a caller
/// had to obtain, which is what makes the whole table exercisable from a test.
#[derive(Debug, Clone)]
pub struct RolloutQuery<'a> {
    /// Unix seconds, as the machine understands them.
    pub now: u64,
    /// This machine's slot, 0–99. See [`bucket`].
    pub slot: u8,
    pub channel: Channel,
    /// The repository the machine follows, without a tag.
    pub repository: &'a str,
    /// What the channel's tag resolves to right now — the digest the machine is
    /// about to pull. `None` when the registry could not be asked.
    pub target_digest: Option<&'a str>,
    /// The document, if one was fetched AND its signature was accepted. A
    /// document whose signature failed must never reach this function: pass
    /// `None` and say so in a note. Trusting the contents of an object whose
    /// signature did not verify is the only mistake here that cannot be
    /// recovered from.
    pub document: Option<&'a RolloutDoc>,
    /// The highest serial this machine has already accepted.
    pub seen_serial: Option<u64>,
    /// A ceiling an enrolled machine's fleet set, which may only LOWER the
    /// percentage and may never raise it. `None` on every machine today: no
    /// fleet client exists, and `docs/fleet.md` is a design. It is a parameter
    /// rather than a future edit so that "a fleet may hold its machines back
    /// and may not push them ahead" is a property of this function instead of a
    /// promise in a document.
    pub fleet_ceiling: Option<u8>,
}

/// The decision, and everything the machine could not use and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolloutAnswer {
    pub rollout: Rollout,
    /// One line per thing that was ignored. Never empty for a document that was
    /// fetched and not used: a pointer silently discarded is a ramp that
    /// silently does not happen.
    pub notes: Vec<String>,
}

/// Decide, and say what was ignored on the way.
///
/// Every uncertainty admits. A machine that cannot reach the document, cannot
/// resolve its tag, or holds a document it cannot read takes its update exactly
/// as it does today — the shape the rest of this program already uses for the
/// same reason, and it costs nothing here: a machine that could not resolve the
/// tag cannot pull the image either, so failing open at this gate changes
/// nothing except which message the user reads.
///
/// The one thing that is NOT an uncertainty is a document that verified and
/// says stop.
pub fn decide_rollout(q: &RolloutQuery<'_>) -> RolloutAnswer {
    let mut notes = Vec::new();
    let entry = usable_entry(q, &mut notes);

    if let Some(e) = entry {
        if e.halt {
            let why = e
                .reason
                .clone()
                .unwrap_or_else(|| "the publisher gave no reason".to_string());
            return RolloutAnswer { rollout: Rollout::Halted { reason: why }, notes };
        }
    }

    let (mut percent, source) = match entry {
        Some(e) => (e.percent.min(100), RolloutSource::Document),
        // No usable document means no ramp, and no ramp means everybody. There
        // is deliberately no second source to fall back to: see [`FULL_ROLLOUT`].
        None => (FULL_ROLLOUT, RolloutSource::Default),
    };

    if let Some(ceiling) = q.fleet_ceiling {
        let ceiling = ceiling.min(100);
        if ceiling < percent {
            notes.push(format!(
                "this machine's fleet lowered the ceiling from {percent}% to {ceiling}%"
            ));
            percent = ceiling;
        }
    }

    let rollout = if admits(q.slot, percent) {
        Rollout::Admitted { percent, source }
    } else {
        Rollout::Held { slot: q.slot, percent }
    };
    RolloutAnswer { rollout, notes }
}

/// The entry that applies to this machine, or `None` and a note saying why not.
fn usable_entry<'a>(q: &RolloutQuery<'a>, notes: &mut Vec<String>) -> Option<&'a RolloutEntry> {
    let doc = q.document?;
    if doc.schema_version() > ROLLOUT_SCHEMA {
        notes.push(format!(
            "the rollout document is schema {} and this build reads {ROLLOUT_SCHEMA}, so it was ignored whole rather than read in part",
            doc.schema_version()
        ));
        return None;
    }
    if doc.repository != q.repository {
        notes.push(format!(
            "the rollout document governs {} and this machine follows {}, so it was ignored",
            doc.repository, q.repository
        ));
        return None;
    }
    if q.now > doc.expires {
        notes.push(format!(
            "the rollout document expired at unix {} and it is unix {}, so this machine is on its own configuration",
            doc.expires, q.now
        ));
        return None;
    }
    if q.now.saturating_sub(doc.issued) > MAX_DOCUMENT_AGE {
        notes.push(format!(
            "the rollout document was issued at unix {} and nothing older than {MAX_DOCUMENT_AGE} seconds is believed, whatever its own expiry says",
            doc.issued
        ));
        return None;
    }
    if doc.issued > q.now.saturating_add(MAX_CLOCK_SKEW) {
        notes.push(format!(
            "the rollout document is dated unix {}, which is ahead of this machine's clock (unix {}); check the clock",
            doc.issued, q.now
        ));
        return None;
    }
    if let Some(seen) = q.seen_serial {
        if doc.serial < seen {
            notes.push(format!(
                "the rollout document is serial {} and this machine has already accepted {seen}; an older document served again is a replay, so it was ignored",
                doc.serial
            ));
            return None;
        }
    }
    let Some(entry) = doc.entry(q.channel) else {
        notes.push(format!(
            "the rollout document says nothing about {}, so no rollout is staged for it",
            q.channel.as_str()
        ));
        return None;
    };
    let Some(target) = q.target_digest else {
        notes.push(
            "the registry could not be asked what this channel resolves to, so the rollout document was not applied"
                .to_string(),
        );
        return None;
    };
    if entry.digest != target {
        notes.push(format!(
            "the rollout document's {} entry is about {}, and this channel now resolves to {}; the entry is stale and was ignored",
            q.channel.as_str(),
            entry.digest,
            target
        ));
        return None;
    }
    Some(entry)
}

// ── the health signal that stops a rollout ───────────────────────────────────

/// Whether the machine came back healthy from an update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub healthy: bool,
    /// One line per regression, naming the component and what is wrong. Empty
    /// when healthy.
    pub reasons: Vec<String>,
}

/// The `rime recover status` rows whose failure is an update regression.
///
/// Not every row: a machine with no network route has a problem an update did
/// not cause, and refusing to update because the wifi is off would strand the
/// user on the release that broke them. These four are the ones an image
/// change can break and a rollback can fix.
///
/// §26 lists boot failures, shell crash loops, GPU initialisation failures,
/// portal failures and network regressions. Boot failure is not observable from
/// a machine that booted, so it is covered from the other side — the previous
/// deployment row is what says whether there is anything to go back TO.
pub const REGRESSION_ROWS: [&str; 4] = [
    // The driver that did not bind after the update. §26's "GPU initialisation
    // failures".
    "gpu-driver",
    // The user interface, vendored in the image, so an image change is the only
    // thing that moves it. §26's "Rime Shell crash loops".
    "rime-shell",
    // A read-only /usr that is not read-only, or a root that is not the
    // deployment. An image that half-installed.
    "filesystem",
    // Extensions built for the previous OS release, which systemd refuses to
    // merge — the user's own software gone after an update.
    "package-extensions",
];

/// Turn one measurement of the machine into a verdict.
///
/// `rows` are `rime recover status`'s rows: id, health, and the detail line.
/// `failed_units` is `systemctl --failed`.
///
/// [`Health::Unavailable`] is NOT a regression, and that asymmetry is the point.
/// A row that could not be measured is a fact about the reader, and treating it
/// as a failure would refuse a user's update because a file was unreadable —
/// which is the same mistake, in the opposite direction, as reporting an
/// unreadable file as fine. It is reported in `reasons` so the user knows the
/// verdict is partial, without counting against it.
pub fn verdict(rows: &[(String, Health, String)], failed_units: &[String]) -> Verdict {
    let mut reasons = Vec::new();
    let mut healthy = true;

    for (id, health, detail) in rows {
        if !REGRESSION_ROWS.contains(&id.as_str()) {
            continue;
        }
        match health {
            Health::Attention => {
                healthy = false;
                reasons.push(format!("{id}: {}", first_line(detail)));
            }
            Health::Unavailable => {
                reasons.push(format!(
                    "{id}: could not be checked ({}), so this verdict is partial",
                    first_line(detail)
                ));
            }
            Health::Verified | Health::Available => {}
        }
    }

    // A unit that failed to start is the most direct evidence an image change
    // broke something, and it is the one signal that works on every boot path —
    // Rime's boot-health unit is conditioned on systemd-boot's
    // LoaderBootCountPath and every published image boots GRUB, so it never
    // runs and its verdict file does not exist.
    for unit in failed_units {
        if is_splash_unit(unit) || is_package_rebuild_unit(unit) {
            continue;
        }
        healthy = false;
        reasons.push(format!("systemd unit failed: {unit}"));
    }

    Verdict { healthy, reasons }
}

/// The boot splash failing is not evidence an update broke the machine.
///
/// plymouthd 24.004.60 (Fedora 43) segfaults in `hide_splash` when the GPU
/// driver replaces simpledrm while a script theme is showing:
/// `free_keyboards_for_renderer` frees the keyboard without firing
/// `keyboard_removed_handler` (the display case just above it does), so the
/// script plugin's `plugin->keyboard` dangles until the splash is hidden. On
/// the L16, amdgpu binds ~10 s in, from the real root, and plymouth-quit runs
/// under a second later, so the crash is a race that appears on some boots and
/// not others, on the old deployment as much as the new one. It held every
/// update on the L16 and a second machine on 2026-09-24 with
/// `systemd unit failed: plymouth-start.service`, after the splash had already
/// shown. A splash dying on its way out cannot make an update worse, and a
/// rollback would not stop it.
fn is_splash_unit(unit: &str) -> bool {
    unit.starts_with("plymouth-") && unit.ends_with(".service")
}

/// The boot rebuild of the user's package extension failing is not evidence
/// an update broke the machine, and holding the next update over it would
/// strand the machine on the release that refused it.
///
/// `rime-sysext-rebuild.service` fails on purpose when its rebuild cannot
/// happen: offline at boot, or no build of the user's package set in the
/// repositories fits this image (the repositories have moved ahead of it). The
/// cure for the second is the NEXT image, which ships the newer libraries, so
/// counting the failure here would hold exactly the update that fixes it. What
/// the failure costs the user is measured where it belongs: the
/// `package-extensions` row, and the notice the session shows.
fn is_package_rebuild_unit(unit: &str) -> bool {
    unit == "rime-sysext-rebuild.service"
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").trim().to_string()
}

/// The record `rime update` leaves so the NEXT update can tell whether this one
/// went well.
///
/// Kept deliberately small. It answers one question — "is the image I am
/// running the one the last update staged" — and it answers it by digest,
/// because a version string moves for reasons that are not an update.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LastUpdate {
    /// Schema version, on the §25 rule: absent means 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<u32>,
    /// The digest that was booted when the update ran. What the machine is
    /// running now, if the update has not been rebooted into yet.
    pub from_digest: String,
    /// The tag the machine follows.
    pub tag: String,
    /// Unix seconds.
    pub at: u64,
}

/// Schema version of [`LastUpdate`].
pub const LAST_UPDATE_SCHEMA: u32 = 1;

/// Whether the machine has rebooted into whatever the last `rime update`
/// staged.
///
/// True when the booted digest differs from the one recorded before the update
/// ran. Comparing against the digest we came FROM rather than the one we went
/// TO is what makes this work without a second registry round trip: `bootc
/// upgrade` does not report the digest it staged in a form worth parsing, and
/// the machine can see its own.
pub fn rebooted_into_new(record: &LastUpdate, booted_digest: &str) -> bool {
    !booted_digest.is_empty() && booted_digest != record.from_digest
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_four_channels_are_ordered_from_cautious_to_not() {
        assert!(Channel::Stable < Channel::Candidate);
        assert!(Channel::Candidate < Channel::Beta);
        assert!(Channel::Beta < Channel::Edge);
        let ranks: Vec<u8> = Channel::ALL.iter().map(|c| c.rank()).collect();
        assert_eq!(ranks, vec![0, 1, 2, 3]);
    }

    #[test]
    fn every_channel_round_trips_and_an_invented_one_is_refused_by_name() {
        for c in Channel::ALL {
            assert_eq!(c.as_str().parse::<Channel>().unwrap(), c);
            assert!(!c.summary().is_empty());
        }
        // The refusal has to name the four that exist. "unknown channel" on its
        // own leaves somebody guessing at the one word they must not guess at.
        let e = "nightly".parse::<Channel>().unwrap_err();
        for c in Channel::ALL {
            assert!(e.contains(c.as_str()), "{e}");
        }
    }

    #[test]
    fn the_legacy_tags_are_edge_under_an_older_name() {
        // Every machine in the field follows one of these, and they move on
        // every build of main. Reporting them as "no channel" would leave every
        // existing machine unable to see where it stands.
        for tag in LEGACY_TAGS {
            let t = from_tag(&format!("ghcr.io/andrenijman/rime-os:{tag}"));
            assert_eq!(t.channel, Some(Channel::Edge), "{tag}");
            assert_eq!(t.alias, Some(tag));
            assert_eq!(t.tag, tag);
        }
        // The exact string on Andre's L16.
        let t = from_tag("ghcr.io/andrenijman/rime-os:daily");
        assert_eq!(t.channel, Some(Channel::Edge));
        assert_eq!(t.alias, Some("daily"));
    }

    #[test]
    fn a_channel_tag_is_not_reported_as_an_alias() {
        for c in Channel::ALL {
            let t = from_tag(&format!("ghcr.io/andrenijman/rime-os:{}", c.as_str()));
            assert_eq!(t.channel, Some(c));
            assert_eq!(t.alias, None, "{} is not a legacy alias", c.as_str());
        }
    }

    #[test]
    fn a_tag_nobody_recognises_is_answered_rather_than_refused() {
        // A revision tag, a fork, somebody's local build. All legitimate.
        let t = from_tag("ghcr.io/andrenijman/rime-os:apex-57f593a");  // rime-rename: keep (a per-SHA tag old builds published)
        assert_eq!(t.channel, None);
        assert_eq!(t.tag, "apex-57f593a");  // rime-rename: keep
        assert_eq!(t.alias, None);
    }

    #[test]
    fn a_registry_port_is_not_mistaken_for_a_tag() {
        let t = from_tag("registry.example:5000/rime/rime-os");
        assert_eq!(t.tag, "registry.example:5000/rime/rime-os");
        assert_eq!(t.channel, None);
        let t = from_tag("registry.example:5000/rime/rime-os:stable");
        assert_eq!(t.tag, "stable");
        assert_eq!(t.channel, Some(Channel::Stable));
    }

    #[test]
    fn moving_toward_stable_is_flagged_as_the_backwards_direction() {
        assert_eq!(direction(Channel::Edge, Channel::Stable), Direction::Behind);
        assert_eq!(direction(Channel::Beta, Channel::Candidate), Direction::Behind);
        assert_eq!(direction(Channel::Stable, Channel::Edge), Direction::Ahead);
        assert_eq!(direction(Channel::Beta, Channel::Beta), Direction::Same);
    }

    #[test]
    fn a_bucket_is_stable_for_a_machine_and_spread_across_a_hundred() {
        // Stable, because a machine that re-rolled on every check would creep
        // into a 1% cohort it was never in.
        let id = "c03b1fb9a1e24d0b8f3c5e7a9d2b4c60";
        assert_eq!(bucket(id), bucket(id));
        assert!(bucket(id) < 100);
        // Even enough: 1000 distinct ids must land in most of the hundred
        // buckets. A hash that collapsed would silently make every machine take
        // or refuse every staged release together.
        let mut seen = [false; 100];
        for i in 0..1000 {
            seen[bucket(&format!("machine-{i:04}")) as usize] = true;
        }
        let hit = seen.iter().filter(|b| **b).count();
        assert!(hit > 90, "only {hit} of 100 buckets were reached");
    }

    #[test]
    fn a_release_at_full_rollout_admits_every_machine() {
        for b in 0..100u8 {
            assert!(admits(b, FULL_ROLLOUT), "bucket {b} was held back at 100%");
        }
        // And nothing is admitted at 0.
        for b in 0..100u8 {
            assert!(!admits(b, 0), "bucket {b} took a release at 0%");
        }
    }

    #[test]
    fn a_one_percent_release_reaches_exactly_one_bucket() {
        let taken = (0..100u8).filter(|b| admits(*b, 1)).count();
        assert_eq!(taken, 1);
        assert_eq!((0..100u8).filter(|b| admits(*b, 25)).count(), 25);
        // A percent with a typo in it must not stop a machine updating.
        assert!(admits(99, 255));
    }

    #[test]
    fn a_healthy_machine_has_no_reasons() {
        let rows = vec![
            ("gpu-driver".to_string(), Health::Verified, "1 — AMD via amdgpu".to_string()),
            ("rime-shell".to_string(), Health::Verified, "vendored".to_string()),
            ("filesystem".to_string(), Health::Verified, "read-only".to_string()),
            ("package-extensions".to_string(), Health::Verified, "none".to_string()),
        ];
        let v = verdict(&rows, &[]);
        assert!(v.healthy);
        assert!(v.reasons.is_empty());
    }

    #[test]
    fn a_row_that_could_not_be_measured_does_not_fail_the_verdict() {
        // The asymmetry that matters. Refusing somebody's update because a file
        // was unreadable is the same mistake as reporting an unreadable file as
        // fine, from the other side — and this one strands them on the release
        // that broke them.
        let rows = vec![(
            "package-extensions".to_string(),
            Health::Unavailable,
            "/var/lib/rime/pkg: Permission denied".to_string(),
        )];
        let v = verdict(&rows, &[]);
        assert!(v.healthy, "an unmeasurable row failed the verdict");
        // But it is still SAID, so the user knows the verdict is partial.
        assert_eq!(v.reasons.len(), 1);
        assert!(v.reasons[0].contains("could not be checked"), "{:?}", v.reasons);
        assert!(v.reasons[0].contains("partial"), "{:?}", v.reasons);
    }

    #[test]
    fn a_broken_gpu_driver_stops_the_rollout_and_says_which_row() {
        let rows = vec![(
            "gpu-driver".to_string(),
            Health::Attention,
            "no driver is bound to the discrete GPU".to_string(),
        )];
        let v = verdict(&rows, &[]);
        assert!(!v.healthy);
        assert_eq!(v.reasons.len(), 1);
        assert!(v.reasons[0].starts_with("gpu-driver: "), "{:?}", v.reasons);
    }

    #[test]
    fn a_row_an_image_change_cannot_break_is_not_a_regression() {
        // The wifi being off is not a reason to refuse somebody the update that
        // fixes their machine.
        let rows = vec![
            ("network".to_string(), Health::Attention, "no default route".to_string()),
            ("secure-boot".to_string(), Health::Attention, "disabled".to_string()),
        ];
        let v = verdict(&rows, &[]);
        assert!(v.healthy, "{:?}", v.reasons);
        assert!(v.reasons.is_empty());
    }

    #[test]
    fn a_failed_unit_stops_the_rollout_on_every_boot_path() {
        // The one signal that works under GRUB, which is every published image:
        // rime-boot-health is conditioned on systemd-boot's LoaderBootCountPath
        // and never runs, so its verdict file does not exist to read.
        let v = verdict(&[], &["rime-shell.service".to_string()]);
        assert!(!v.healthy);
        assert!(v.reasons[0].contains("rime-shell.service"), "{:?}", v.reasons);
    }

    #[test]
    fn a_crashed_boot_splash_does_not_hold_the_update() {
        // L16, 2026-09-24: plymouthd segfaulted in hide_splash after amdgpu
        // replaced simpledrm, and every update was held on it.
        let v = verdict(&[], &["plymouth-start.service".to_string()]);
        assert!(v.healthy, "{:?}", v.reasons);
        assert!(v.reasons.is_empty(), "{:?}", v.reasons);
        // ...while a real failure alongside it still holds.
        let v = verdict(
            &[],
            &["plymouth-start.service".to_string(), "rime-shell.service".to_string()],
        );
        assert!(!v.healthy);
        assert_eq!(v.reasons, vec!["systemd unit failed: rime-shell.service"]);
        assert!(!is_splash_unit("rime-plymouth-theme.service"));
    }

    #[test]
    fn a_refused_package_rebuild_does_not_hold_the_update_that_fixes_it() {
        // A boot rebuild that cannot happen (offline, or the repositories need
        // a newer image) fails its unit on purpose. The next image is what
        // fixes the second case, so it must not be held.
        let v = verdict(&[], &["rime-sysext-rebuild.service".to_string()]);
        assert!(v.healthy, "{:?}", v.reasons);
        assert!(v.reasons.is_empty(), "{:?}", v.reasons);
        // ...while a real failure alongside it still holds.
        let v = verdict(
            &[],
            &["rime-sysext-rebuild.service".to_string(), "rimed.service".to_string()],
        );
        assert!(!v.healthy);
        assert_eq!(v.reasons, vec!["systemd unit failed: rimed.service"]);
        // ...and the user's packages missing for another OS release still
        // counts, through its own row.
        let rows = vec![(
            "package-extensions".to_string(),
            Health::Attention,
            "12 packages, built for OS 44 but this machine runs OS 45".to_string(),
        )];
        let v = verdict(&rows, &["rime-sysext-rebuild.service".to_string()]);
        assert!(!v.healthy);
        assert_eq!(v.reasons.len(), 1, "{:?}", v.reasons);
        assert!(!is_package_rebuild_unit("rime-sysext-rebuild-other.service"));
    }

    #[test]
    fn the_detail_of_a_multi_line_row_does_not_wrap_into_the_reason() {
        let rows = vec![(
            "rime-shell".to_string(),
            Health::Attention,
            "not provisioned for this account\nrun rime shell firstrun".to_string(),
        )];
        let v = verdict(&rows, &[]);
        assert_eq!(v.reasons, vec!["rime-shell: not provisioned for this account"]);
    }

    #[test]
    fn the_machine_knows_it_has_rebooted_into_the_update_it_staged() {
        let r = LastUpdate {
            schema: Some(LAST_UPDATE_SCHEMA),
            from_digest: "sha256:5e206de5".into(),
            tag: "daily".into(),
            at: 1788700000,
        };
        assert!(rebooted_into_new(&r, "sha256:308127d9"));
        assert!(!rebooted_into_new(&r, "sha256:5e206de5"));
        // A digest nobody could read is not evidence of a reboot, and acting on
        // it would run the health gate against an update that never happened.
        assert!(!rebooted_into_new(&r, ""));
    }


    // ── the rollout document ─────────────────────────────────────────────────

    const REPO: &str = "ghcr.io/andrenijman/rime-os";
    const D1: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
    const D2: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
    const NOW: u64 = 1_790_000_000;

    fn doc(entries: Vec<RolloutEntry>) -> RolloutDoc {
        RolloutDoc {
            schema: Some(ROLLOUT_SCHEMA),
            serial: 7,
            issued: NOW - 3600,
            expires: NOW + 7 * 24 * 3600,
            repository: REPO.to_string(),
            channels: entries,
        }
    }

    fn entry(channel: &str, digest: &str, percent: u8) -> RolloutEntry {
        RolloutEntry {
            channel: channel.to_string(),
            digest: digest.to_string(),
            percent,
            halt: false,
            reason: None,
        }
    }

    fn query<'a>(slot: u8, document: Option<&'a RolloutDoc>) -> RolloutQuery<'a> {
        RolloutQuery {
            now: NOW,
            slot,
            channel: Channel::Candidate,
            repository: REPO,
            target_digest: Some(D1),
            document,
            seen_serial: None,
            fleet_ceiling: None,
        }
    }

    #[test]
    fn a_machine_with_no_document_and_no_label_behaves_exactly_as_it_does_today() {
        // The property that makes this an addition rather than a change. Every
        // Rime machine in the field is in this state and must stay in it.
        for slot in 0..100u8 {
            let a = decide_rollout(&query(slot, None));
            assert_eq!(
                a.rollout,
                Rollout::Admitted { percent: 100, source: RolloutSource::Default },
                "slot {slot}"
            );
            assert!(a.notes.is_empty(), "{:?}", a.notes);
        }
    }

    #[test]
    fn a_ramp_admits_the_slots_below_it_and_holds_the_rest() {
        let d = doc(vec![entry("candidate", D1, 25)]);
        let taken = (0..100u8)
            .filter(|s| {
                matches!(
                    decide_rollout(&query(*s, Some(&d))).rollout,
                    Rollout::Admitted { .. }
                )
            })
            .count();
        assert_eq!(taken, 25);
        // And a held machine is told which slot it is and where the ramp got
        // to, because "not yet" with no number is indistinguishable from broken.
        let a = decide_rollout(&query(40, Some(&d)));
        assert_eq!(a.rollout, Rollout::Held { slot: 40, percent: 25 });
        let a = decide_rollout(&query(24, Some(&d)));
        assert_eq!(
            a.rollout,
            Rollout::Admitted { percent: 25, source: RolloutSource::Document }
        );
    }

    #[test]
    fn a_halt_stops_every_slot_and_carries_the_publishers_own_words() {
        let mut e = entry("candidate", D1, 100);
        e.halt = true;
        e.reason = Some("gpu-driver fails to bind on RTX 30-series".to_string());
        let d = doc(vec![e]);
        for slot in [0u8, 50, 99] {
            match decide_rollout(&query(slot, Some(&d))).rollout {
                Rollout::Halted { reason } => {
                    assert_eq!(reason, "gpu-driver fails to bind on RTX 30-series")
                }
                other => panic!("slot {slot} was not halted: {other:?}"),
            }
        }
        // A halt at 100% still halts: the percentage is not what stops it.
        // And a halt with no reason still says something rather than nothing.
        let mut e = entry("candidate", D1, 100);
        e.halt = true;
        let d = doc(vec![e]);
        match decide_rollout(&query(0, Some(&d))).rollout {
            Rollout::Halted { reason } => assert!(!reason.is_empty()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_entry_about_a_digest_this_channel_no_longer_resolves_to_is_stale() {
        // The defect this binding exists to prevent: a 5% entry written for
        // build X goes on holding machines back from build Y once the tag
        // moves, and the entry still reads as current.
        let d = doc(vec![entry("candidate", D2, 5)]);
        let a = decide_rollout(&query(50, Some(&d)));
        assert_eq!(
            a.rollout,
            Rollout::Admitted { percent: 100, source: RolloutSource::Default }
        );
        assert_eq!(a.notes.len(), 1);
        assert!(a.notes[0].contains("stale"), "{:?}", a.notes);
        // A halt is bound the same way. A halt on a digest nobody is being
        // offered any more is over.
        let mut e = entry("candidate", D2, 100);
        e.halt = true;
        let d = doc(vec![e]);
        assert!(matches!(
            decide_rollout(&query(50, Some(&d))).rollout,
            Rollout::Admitted { .. }
        ));
    }

    #[test]
    fn an_expired_document_returns_the_machine_to_its_own_configuration() {
        // A fleet that stops answering must not pin every machine to its last
        // instruction forever.
        let mut d = doc(vec![entry("candidate", D1, 5)]);
        d.expires = NOW - 1;
        let a = decide_rollout(&query(50, Some(&d)));
        assert!(matches!(a.rollout, Rollout::Admitted { source: RolloutSource::Default, .. }));
        assert!(a.notes[0].contains("expired"), "{:?}", a.notes);
    }

    #[test]
    fn the_client_stops_believing_an_old_document_whatever_its_own_expiry_says() {
        // The half of the freshness bound that cannot be forgotten by whoever
        // writes the document: an expiry ten years out is still an expiry.
        let mut d = doc(vec![entry("candidate", D1, 5)]);
        d.issued = NOW - MAX_DOCUMENT_AGE - 1;
        d.expires = NOW + 10 * 365 * 24 * 3600;
        let a = decide_rollout(&query(50, Some(&d)));
        assert!(matches!(a.rollout, Rollout::Admitted { source: RolloutSource::Default, .. }));
        assert!(a.notes[0].contains(&MAX_DOCUMENT_AGE.to_string()), "{:?}", a.notes);
        // One second inside the window is still believed, so the bound is a
        // bound and not an off-by-a-month.
        d.issued = NOW - MAX_DOCUMENT_AGE;
        assert_eq!(
            decide_rollout(&query(50, Some(&d))).rollout,
            Rollout::Held { slot: 50, percent: 5 }
        );
    }

    #[test]
    fn a_document_dated_ahead_of_the_clock_is_refused_within_an_hour_of_slack() {
        let mut d = doc(vec![entry("candidate", D1, 5)]);
        d.issued = NOW + MAX_CLOCK_SKEW + 1;
        let a = decide_rollout(&query(50, Some(&d)));
        assert!(matches!(a.rollout, Rollout::Admitted { source: RolloutSource::Default, .. }));
        assert!(a.notes[0].contains("clock"), "{:?}", a.notes);
        // A machine an hour out is common and still gets its ramp.
        d.issued = NOW + MAX_CLOCK_SKEW;
        assert_eq!(
            decide_rollout(&query(50, Some(&d))).rollout,
            Rollout::Held { slot: 50, percent: 5 }
        );
    }

    #[test]
    fn an_older_document_served_again_is_a_replay_and_is_refused() {
        // Without this, "everybody takes this" re-served after a halt is a
        // downgrade attack that needs no key at all — only the ability to hand
        // the machine an object the publisher really did sign, once.
        let mut e = entry("candidate", D1, 100);
        e.halt = true;
        e.reason = Some("this build eats /etc".to_string());
        let halted = doc(vec![e]);
        assert!(matches!(
            decide_rollout(&query(50, Some(&halted))).rollout,
            Rollout::Halted { .. }
        ));

        let mut old = doc(vec![entry("candidate", D1, 100)]);
        old.serial = 6;
        let mut q = query(50, Some(&old));
        q.seen_serial = Some(halted.serial);
        let a = decide_rollout(&q);
        assert!(matches!(a.rollout, Rollout::Admitted { source: RolloutSource::Default, .. }));
        assert!(a.notes[0].contains("replay"), "{:?}", a.notes);

        // The same serial is not a replay — it is the same document, fetched
        // again, which is what every poll after the first one is.
        let same = doc(vec![entry("candidate", D1, 100)]);
        let mut q = query(50, Some(&same));
        q.seen_serial = Some(same.serial);
        assert!(matches!(
            decide_rollout(&q).rollout,
            Rollout::Admitted { source: RolloutSource::Document, .. }
        ));
    }

    #[test]
    fn a_document_for_another_repository_governs_nothing_here() {
        let mut d = doc(vec![entry("candidate", D1, 5)]);
        d.repository = "ghcr.io/someone-else/rime-os".to_string();
        let a = decide_rollout(&query(50, Some(&d)));
        assert!(matches!(a.rollout, Rollout::Admitted { source: RolloutSource::Default, .. }));
        assert!(a.notes[0].contains("someone-else"), "{:?}", a.notes);
    }

    #[test]
    fn a_newer_schema_is_ignored_whole_rather_than_read_in_part() {
        let mut d = doc(vec![entry("candidate", D1, 5)]);
        d.schema = Some(ROLLOUT_SCHEMA + 1);
        let a = decide_rollout(&query(50, Some(&d)));
        assert!(matches!(a.rollout, Rollout::Admitted { source: RolloutSource::Default, .. }));
        assert!(a.notes[0].contains("schema"), "{:?}", a.notes);
        // Absent means 1, the §25 rule every other stored schema follows.
        d.schema = None;
        assert_eq!(d.schema_version(), 1);
        assert_eq!(
            decide_rollout(&query(50, Some(&d))).rollout,
            Rollout::Held { slot: 50, percent: 5 }
        );
    }

    #[test]
    fn a_document_that_says_nothing_about_this_channel_stages_nothing_for_it() {
        let d = doc(vec![entry("beta", D1, 5)]);
        let a = decide_rollout(&query(50, Some(&d)));
        assert!(matches!(a.rollout, Rollout::Admitted { source: RolloutSource::Default, .. }));
        assert!(a.notes[0].contains("candidate"), "{:?}", a.notes);
    }

    #[test]
    fn a_registry_that_could_not_be_asked_does_not_hold_the_update() {
        // Fail open, and it costs nothing: a machine that cannot resolve the
        // tag cannot pull the image either, so the only thing this changes is
        // which sentence the user reads.
        let d = doc(vec![entry("candidate", D1, 5)]);
        let mut q = query(50, Some(&d));
        q.target_digest = None;
        let a = decide_rollout(&q);
        assert!(matches!(a.rollout, Rollout::Admitted { source: RolloutSource::Default, .. }));
        assert!(a.notes[0].contains("could not be asked"), "{:?}", a.notes);
    }

    #[test]
    fn a_fleet_may_lower_the_ceiling_and_may_never_raise_it() {
        // The property `docs/fleet.md` promises, held here rather than in the
        // document: an enrolled machine's operator can hold their fleet back
        // and cannot push it ahead of the publisher's ramp.
        let d = doc(vec![entry("candidate", D1, 50)]);
        let mut q = query(60, Some(&d));
        q.fleet_ceiling = Some(90);
        let a = decide_rollout(&q);
        assert_eq!(a.rollout, Rollout::Held { slot: 60, percent: 50 }, "a fleet raised the ramp");
        assert!(a.notes.is_empty(), "{:?}", a.notes);

        let mut q = query(40, Some(&d));
        q.fleet_ceiling = Some(10);
        let a = decide_rollout(&q);
        assert_eq!(a.rollout, Rollout::Held { slot: 40, percent: 10 });
        assert!(a.notes[0].contains("fleet lowered"), "{:?}", a.notes);

        // And with no document at all, a fleet can still hold its own machines
        // back from a release everybody else is taking.
        let mut q = query(40, None);
        q.fleet_ceiling = Some(10);
        assert_eq!(decide_rollout(&q).rollout, Rollout::Held { slot: 40, percent: 10 });
    }

    #[test]
    fn a_percentage_over_a_hundred_is_clamped_rather_than_refused() {
        let d = doc(vec![entry("candidate", D1, 255)]);
        assert_eq!(
            decide_rollout(&query(99, Some(&d))).rollout,
            Rollout::Admitted { percent: 100, source: RolloutSource::Document }
        );
    }

    #[test]
    fn the_document_round_trips_through_json_as_the_publisher_writes_it() {
        // The writer is a workflow and the reader is this crate, and nothing
        // else connects them. A field renamed on one side is a ramp that
        // silently stops ramping.
        let text = r#"{
          "schema": 1,
          "serial": 12,
          "issued": 1790000000,
          "expires": 1790604800,
          "repository": "ghcr.io/andrenijman/rime-os",
          "channels": [
            {"channel":"candidate","digest":"sha256:aa","percent":5},
            {"channel":"beta","digest":"sha256:bb","percent":100,"halt":true,"reason":"boot loop"}
          ]
        }"#;
        let d: RolloutDoc = serde_json::from_str(text).expect("the published shape must parse");
        assert_eq!(d.serial, 12);
        assert_eq!(d.entry(Channel::Candidate).unwrap().percent, 5);
        assert!(!d.entry(Channel::Candidate).unwrap().halt);
        let b = d.entry(Channel::Beta).unwrap();
        assert!(b.halt);
        assert_eq!(b.reason.as_deref(), Some("boot loop"));
        assert_eq!(d.entry(Channel::Stable), None);
        // And back out again, so the workflow can read what it wrote.
        let again: RolloutDoc = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
        assert_eq!(again, d);
    }

    #[test]
    fn the_tag_and_media_type_are_spelled_once() {
        // The writer is a workflow in another file and the reader is this
        // crate; a spelling that drifts is a pointer nobody fetches.
        assert_eq!(ROLLOUT_TAG, "rollout");
        assert_eq!(ROLLOUT_MEDIA_TYPE, "application/vnd.apexos.rollout.v1+json");  // rime-rename: keep (ROLLOUT_MEDIA_TYPE)
        assert_eq!(ROLLOUT_SCHEMA, 1);
    }

    #[test]
    fn a_machine_with_no_usable_document_is_on_nobody_s_ramp() {
        // There is exactly one fallback and it is "everybody". This asserts the
        // absence: a second source added later — a label, a config file, an
        // environment variable — would be a way for a percentage to reach the
        // gate without a signature over it, and this is where that gets caught.
        assert_eq!(FULL_ROLLOUT, 100);
        for slot in [0u8, 1, 50, 99] {
            let a = decide_rollout(&query(slot, None));
            assert_eq!(
                a.rollout,
                Rollout::Admitted { percent: 100, source: RolloutSource::Default },
                "slot {slot} was gated by something other than a signed document"
            );
        }
    }
}
