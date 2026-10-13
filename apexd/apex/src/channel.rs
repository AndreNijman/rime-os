//! `apex channel` — which update channel this machine follows, moving between
//! them safely in both directions, and the health signal that stops a rollout.
//! Roadmap §26.
//!
//! ## What is here and what is honestly not
//!
//! **Here.** Four channels as four tags on the one repository, a readout that
//! tells a machine where it actually stands, `set` in both directions with the
//! backwards one pinning the current deployment first, a health verdict taken
//! from the machine after an update, and a gate that refuses to advance a
//! machine that came back broken.
//!
//! **Not here, and not pretended.** No health report is transmitted anywhere.
//! `apex channel report` prints the exact payload it would send and says that
//! no endpoint is configured, because APEX operates no telemetry service and
//! writing a URL into the source would not create one. The opt-in is real —
//! `report = false` in `~/.config/apex/channel.toml`, and an `endpoint` the
//! user sets themselves — and with nothing configured the answer is that
//! nothing leaves the machine.
//!
//! Nor is there a fleet-wide rollout ramp. `apexd_core::channel::admits` and
//! the machine's bucket are real and tested, and the percentage is read from
//! the image's own label; but a label is baked once per build, so the ramp
//! needs a mutable pointer that does not exist. The gate stops a rollout on
//! this machine, which is the half that can be true without a server.
//!
//! ## The stop is a refusal, not a notification
//!
//! §26's second criterion is that a rollout can stop. On one machine that
//! means: after an update, if the machine came back with a regression the
//! update could have caused, `apex update` refuses to take it further and
//! points at `sudo apex rollback`. Advancing a machine that is already broken
//! is how one bad release becomes two, and the user is at the keyboard of the
//! machine that would do it.

use std::path::PathBuf;
use std::process::Command;

use apexd_core::channel::{
    self, Channel, Direction, LastUpdate, Tracking, Verdict, LAST_UPDATE_SCHEMA,
};
use clap::Subcommand;
use serde_json::{json, Value};

/// Where `apex update` records what the machine was running before it ran.
///
/// Under `/var/lib/apex`, root-written and world-readable, because `apex update`
/// runs as root and `apex channel status` must not need to.
///
/// Resolved through `trust::Roots`, so the whole stop — record, digest,
/// verdict, refusal — is reachable from a fixture tree. It was not, and the
/// consequence was precise: three mutation tests covered the channel model, the
/// health verdict and the CI promotion, and none of them covered the thing that
/// actually stops a rollout. A gate nobody has watched fire is a gate nobody
/// has tested.
const RECORD: &str = "/var/lib/apex/channel/last-update.json";

fn roots() -> crate::trust::Roots {
    crate::trust::Roots::from_env()
}

fn record_path() -> PathBuf {
    roots().path(RECORD)
}

/// The user's channel preferences, including the telemetry opt-in.
fn config_path() -> PathBuf {
    apex_agent_core::paths::config_home().join("apex/channel.toml")
}

#[derive(Subcommand)]
pub enum ChannelCmd {
    /// Which channel this machine follows, and how the last update went.
    ///
    /// Read-only and root-free: the channel comes from the booted deployment's
    /// origin file, which is world-readable, and the health verdict from the
    /// same probes `apex recover status` uses. Nothing is contacted.
    Status {
        /// Emit machine-readable JSON instead of a report.
        #[arg(long)]
        json: bool,
    },
    /// The four channels, what each one means, and which one you are on.
    List,
    /// Follow a different channel from the next update onwards.
    ///
    /// Requires root: it is `bootc switch`, which writes the deployment origin.
    /// Moving toward `stable` usually deploys an OLDER image, so that direction
    /// pins the current deployment first and prints what your persistent state
    /// would do about it — `/usr` rolls back and `/etc`, `/var` and your home
    /// do not.
    Set {
        /// stable, candidate, beta or edge.
        ///
        /// Parsed by clap, deliberately, so a name nobody has heard of is
        /// refused before the root gate is consulted. It was the other way
        /// round: `apex channel set nightl` answered "must run as root", the
        /// user typed `sudo`, and only then learned they had made a typo. A
        /// command that asks for a password to deliver a spelling mistake is
        /// teaching people to sudo without reading.
        #[arg(value_parser = parse_channel)]
        channel: Channel,
        /// Print what would run and change nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// The health report, and whether anything would be sent.
    ///
    /// Off by default. With no endpoint configured — and APEX configures none —
    /// this prints the payload and sends nothing.
    Report {
        /// Emit machine-readable JSON instead of a report.
        #[arg(long)]
        json: bool,
    },
}

fn parse_channel(s: &str) -> Result<Channel, String> {
    s.parse()
}

// ── where this machine stands ────────────────────────────────────────────────

/// The booted image reference, from the deployment's own origin.
///
/// Shares `trust`'s reader rather than growing a second one: it is the same
/// file, read the same unprivileged way, and two decoders of it would disagree
/// the first time one was corrected.
fn booted_reference() -> Result<String, String> {
    let roots = crate::trust::Roots::from_env();
    let origin = crate::trust::booted_origin(&roots)?;
    let reference = crate::trust::origin_image_reference(&origin)
        .ok_or_else(|| "the booted deployment names no container image".to_string())?;
    crate::trust::parse_origin_reference(&reference)
        .1
        .ok_or_else(|| format!("{reference} names no image"))
}

fn tracking() -> Result<Tracking, String> {
    booted_reference().map(|r| channel::from_tag(&r))
}

/// The tag this machine follows, for the record `apex update` leaves.
pub fn current_tag() -> Result<String, String> {
    tracking().map(|t| t.tag)
}

/// The registry and repository, without the tag — what a channel tag hangs off.
fn repository(reference: &str) -> &str {
    match (reference.rfind(':'), reference.rfind('/')) {
        (Some(c), Some(s)) if c > s => &reference[..c],
        (Some(c), None) => &reference[..c],
        _ => reference,
    }
}

fn machine_bucket() -> Result<u8, String> {
    // /etc/machine-id is world-readable by design; systemd documents it as a
    // public identifier, which is exactly why the bucket derived from it is
    // still never transmitted.
    let id = std::fs::read_to_string("/etc/machine-id")
        .map_err(|e| format!("/etc/machine-id: {e}"))?;
    Ok(channel::bucket(&id))
}

/// Units systemd reports as failed.
///
/// An empty list on an error, deliberately: this feeds a gate that REFUSES an
/// update, and inventing a failure out of a systemctl that would not run is the
/// direction that strands somebody on the release that broke them. The reason
/// is surfaced by the caller instead.
///
/// Under a fixture root it reads a pre-rendered `systemctl-failed` — one unit
/// per line — rather than spawning, the same shape `apex boot status` uses for
/// `bootctl list`. This is the signal that works on every boot path, so it is
/// the one the rollout stop is driven by in the suite; a gate that could not be
/// pushed into refusing has not been shown to refuse.
fn failed_units() -> (Vec<String>, Option<String>) {
    let r = roots();
    if r.fixture.is_some() {
        let p = r.path("/systemctl-failed");
        return match std::fs::read_to_string(&p) {
            Ok(text) => (
                text.lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(str::to_string)
                    .collect(),
                None,
            ),
            // No file is no failed units, which is what a healthy machine
            // looks like. Any other error is a read that did not happen.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Vec::new(), None),
            Err(e) => (Vec::new(), Some(format!("{}: {e}", p.display()))),
        };
    }
    let out = match Command::new("/usr/bin/systemctl")
        .args(["--failed", "--no-legend", "--plain", "--no-pager"])
        .output()
    {
        Ok(o) => o,
        Err(e) => return (Vec::new(), Some(format!("could not run systemctl: {e}"))),
    };
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return (Vec::new(), Some(if why.is_empty() { "systemctl --failed did not succeed".into() } else { why }));
    }
    let units = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|u| u.contains('.'))
        .map(str::to_string)
        .collect();
    (units, None)
}

fn health() -> (Verdict, Option<String>) {
    let (units, why) = failed_units();
    let rows = crate::recover::health_rows();
    (channel::verdict(&rows, &units), why)
}

// ── the record `apex update` leaves ──────────────────────────────────────────

/// The record, if there is one, or the reason there is no answer.
///
/// Three states, not two, and the third is the point. `Ok(None)` means the
/// file is not there — a machine that has never updated, which is a real and
/// normal state. `Err` means the file IS there and nobody can say what it
/// says: unreadable, or not JSON.
///
/// It used to be `Option`, collapsed with two `.ok()?`, and the cost was a
/// sentence the machine was not entitled to say. `tests/chaos/cases/
/// power-loss-during-update.sh` tears this file the way a power loss during
/// [`record_update`]'s write tears it, and the surface answered "nothing
/// recorded yet — `apex update` writes this" for a machine that had just
/// updated. That is this repository's "permission denied is not absence",
/// printed on the screen somebody reads while their machine is misbehaving.
fn read_record() -> Result<Option<LastUpdate>, String> {
    let path = record_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Note what the machine was running before an update runs.
///
/// Called by `ops::update` after a successful `bootc upgrade`. A failure here
/// is reported and never fatal: losing the record costs the next update its
/// health gate, and refusing to update because a note could not be written
/// would be worse than the thing the note is for.
pub fn record_update(tag: &str) {
    let digest = match crate::trust::booted_digest(&roots()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("apex: the update health gate is not armed: {e}");
            return;
        }
    };
    let record = LastUpdate {
        schema: Some(LAST_UPDATE_SCHEMA),
        from_digest: digest,
        tag: tag.to_string(),
        at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    };
    let path = record_path();
    if let Some(dir) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("apex: the update health gate is not armed: {}: {e}", dir.display());
            return;
        }
    }
    // Written to a sibling temp file and renamed, rather than straight over
    // the record. `rename(2)` within a directory is atomic, so a crash during
    // this leaves either the previous record or the new one and never a
    // half-written file — and a half-written file here is not a lost record,
    // it is a record that reads as a LIE about whether an update ran.
    //
    // The pid is in the temp name because there is no lock on this path and a
    // fixed `.tmp` suffix lets two writers interleave: A writes its temp, B
    // truncates the same temp, A renames B's partial content into place. That
    // is the same defect one level down. (`apex/src/host.rs` and `task.rs`
    // already use the pid-suffixed form; most of this tree does not.)
    //
    // What this still does not survive is power loss: neither the file nor the
    // containing directory is fsynced, so the rename can be in the page cache
    // when the power goes. `tests/chaos/cases/power-loss-during-update.sh`
    // therefore injects the state a tear leaves and asserts the READER reports
    // it, which is the half that is worth having either way.
    let text = match serde_json::to_string_pretty(&record) {
        Ok(t) => t + "\n",
        Err(e) => {
            eprintln!("apex: the update health gate is not armed: {e}");
            return;
        }
    };
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    if let Err(e) = std::fs::write(&tmp, &text) {
        eprintln!("apex: the update health gate is not armed: {}: {e}", tmp.display());
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        eprintln!("apex: the update health gate is not armed: {}: {e}", path.display());
        let _ = std::fs::remove_file(&tmp);
    }
}

/// §26's stop: the reason this machine must not take another update, if there
/// is one.
///
/// Answers `None` — take the update — for every state except one: the machine
/// has rebooted into what the last `apex update` staged, and it came back with
/// a regression an image change could have caused. Everything else, including
/// every failure to measure, permits the update. A gate that fails closed here
/// would refuse the update that fixes a machine, on a machine whose only
/// problem was an unreadable file.
pub fn halt_reason() -> Option<String> {
    // An unreadable record permits the update, exactly as an absent one does,
    // and for the reason in the doc comment above: a gate that failed closed
    // here would refuse the update that fixes a machine whose only problem is
    // a damaged file. `status` is where the damage is REPORTED; this is where
    // it is forgiven. Both halves are needed — forgiving it silently is what
    // the chaos case caught.
    let record = read_record().ok()??;
    let booted = crate::trust::booted_digest(&roots()).ok()?;
    if !channel::rebooted_into_new(&record, &booted) {
        // Still running what we were before the last update: either it has not
        // been rebooted into yet, or it failed. Neither is evidence about the
        // new image.
        return None;
    }
    let (verdict, _) = health();
    if verdict.healthy {
        return None;
    }
    // Worded to claim only what is measured. The machine HAS a regression and
    // HAS not been updated since the recorded run — it does not follow that
    // the update caused it, and the record can be months old with an unrelated
    // unit having failed yesterday. Saying "the update broke this" in that
    // case would be a confident wrong diagnosis on the one screen somebody
    // reads while their machine is misbehaving.
    let mut s = String::from(
        "apex: this machine has a problem an update could have caused, so the next \
         update is being held.\n",
    );
    for r in &verdict.reasons {
        s.push_str(&format!("  {r}\n"));
    }
    s.push_str(&format!(
        "\nIt has not been updated since {}, when it was running {}.\n\
         If that update caused this, `sudo apex rollback` and reboot undoes it.\n\
         If it did not, `sudo apex update --force` takes the update anyway.\n",
        stamp(record.at),
        short(&record.from_digest),
    ));
    Some(s)
}

fn short(digest: &str) -> String {
    digest.chars().take(19).collect()
}

fn stamp(unix: u64) -> String {
    // No date library in this crate, and one would be a dependency for a line
    // of prose. Seconds since the epoch, labelled, beats a wrong local time.
    format!("unix {unix}")
}

// ── the opt-in ───────────────────────────────────────────────────────────────

/// What the user has agreed to, if anything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Consent {
    report: bool,
    endpoint: Option<String>,
    /// Why this could not be read, when it could not be.
    error: Option<String>,
}

fn consent() -> Consent {
    let path = config_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        // Absent is the default and the default is off. A file nobody could
        // read is also off — a consent that fails open is not consent.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Consent::default(),
        Err(e) => {
            return Consent {
                error: Some(format!("{}: {e}", path.display())),
                ..Default::default()
            }
        }
    };
    let doc: toml::Value = match text.parse() {
        Ok(v) => v,
        Err(e) => {
            return Consent {
                error: Some(format!("{}: {e}", path.display())),
                ..Default::default()
            }
        }
    };
    Consent {
        report: doc.get("report").and_then(toml::Value::as_bool).unwrap_or(false),
        endpoint: doc
            .get("endpoint")
            .and_then(toml::Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.is_empty()),
        error: None,
    }
}

/// Exactly what a report would carry.
///
/// The bucket is not in it. It is one of a hundred values derived from the
/// machine id, and a health report has no use for it — including it would make
/// the payload a weak identifier for no gain.
fn payload(t: &Tracking, digest: Option<&str>, v: &Verdict) -> Value {
    json!({
        "channel": t.channel.map(|c| c.as_str()),
        "tag": t.tag,
        "digest": digest,
        "healthy": v.healthy,
        "reasons": v.reasons,
    })
}

// ── the verbs ────────────────────────────────────────────────────────────────

fn status(as_json: bool) -> i32 {
    let t = tracking();
    let digest = crate::trust::booted_digest(&roots()).ok();
    let (verdict, systemctl_error) = health();
    let bucket = machine_bucket();
    let record = read_record();

    if as_json {
        let doc = json!({
            "channel": t.as_ref().ok().and_then(|t| t.channel).map(|c| c.as_str()),
            "tag": t.as_ref().ok().map(|t| t.tag.clone()),
            "alias": t.as_ref().ok().and_then(|t| t.alias),
            "trackingError": t.as_ref().err(),
            "digest": digest,
            "rolloutBucket": bucket.as_ref().ok(),
            "rolloutBucketError": bucket.as_ref().err(),
            "health": {
                "healthy": verdict.healthy,
                "reasons": verdict.reasons,
                "partial": systemctl_error,
            },
            // Two fields, because `null` has to keep meaning one thing. A
            // `lastUpdate` of null with no error is "no update has run here";
            // a null with an error beside it is "one ran and the note about it
            // is damaged". APEX Settings reads this document, and collapsing
            // the two would put the wrong sentence on the screen.
            "lastUpdate": record.as_ref().ok().and_then(|r| r.clone()),
            "lastUpdateError": record.as_ref().err(),
            "held": halt_reason().is_some(),
            "telemetry": {
                "optedIn": consent().report,
                "endpoint": consent().endpoint,
                "sent": false,
            },
        });
        println!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
        return 0;
    }

    println!("Update channel");
    match &t {
        Ok(t) => {
            match (t.channel, t.alias) {
                (Some(c), Some(alias)) => {
                    println!("  following    : {alias}");
                    println!(
                        "  which is     : {} — `{alias}` moves with every build of main, so it is \
                         the {} channel under the name it had before channels existed",
                        c.as_str(),
                        c.as_str()
                    );
                }
                (Some(c), None) => {
                    println!("  following    : {}", c.as_str());
                    println!("  which means  : {}", c.summary());
                }
                (None, _) => {
                    println!("  following    : {} — not one of the channels", t.tag);
                }
            }
        }
        Err(e) => println!("  following    : unavailable — {e}"),
    }
    match &digest {
        Some(d) => println!("  digest       : {d}"),
        None => println!("  digest       : unavailable — rpm-ostree could not be read"),
    }
    match &bucket {
        Ok(b) => println!(
            "  rollout slot : {b} of 100 — a staged release at {}% or more reaches this machine. \
             It is derived from this machine's id and is never sent anywhere",
            b + 1
        ),
        Err(e) => println!("  rollout slot : unavailable — {e}"),
    }

    println!("\nAfter the last update");
    match &record {
        Err(e) => {
            // The state a power loss during `record_update`'s write leaves.
            // Saying "nothing recorded yet" here would be a claim about the
            // machine made out of a read nobody completed.
            println!("  unknown — the record exists and could not be read ({e})");
            println!("  An update may well have run. The health gate that reads this note is");
            println!("  therefore not armed; `sudo apex update` still works and is not held by it.");
        }
        Ok(None) => println!("  nothing recorded yet — `apex update` writes this"),
        Ok(Some(r)) => {
            println!("  ran at       : {}", stamp(r.at));
            println!("  came from    : {}", short(&r.from_digest));
            let rebooted = digest
                .as_deref()
                .map(|d| channel::rebooted_into_new(r, d))
                .unwrap_or(false);
            println!(
                "  rebooted into it: {}",
                if rebooted { "yes" } else { "not yet" }
            );
        }
    }
    println!(
        "  health       : {}",
        if verdict.healthy { "no regression an update could have caused" } else { "a regression" }
    );
    for r in &verdict.reasons {
        println!("      {r}");
    }
    if let Some(e) = &systemctl_error {
        println!("      failed units could not be listed ({e}), so this verdict is partial");
    }
    if halt_reason().is_some() {
        println!("\n  The next `apex update` is held. `apex channel status --json` has the reasons,");
        println!("  `sudo apex rollback` goes back, `sudo apex update --force` takes it anyway.");
    }

    let c = consent();
    println!("\nHealth reporting");
    println!("  opted in     : {}", if c.report { "yes" } else { "no (the default)" });
    match &c.endpoint {
        Some(e) => println!("  endpoint     : {e} (yours; APEX operates none)"),
        None => println!("  endpoint     : none is configured, and APEX ships none"),
    }
    if let Some(e) = &c.error {
        println!("  note         : {e}, so reporting stays off");
    }
    println!("  sent so far  : nothing. `apex channel report` shows what would go.");
    0
}

fn list() -> i32 {
    let current = tracking().ok().and_then(|t| t.channel);
    println!("Update channels\n");
    for c in Channel::ALL {
        let mark = if Some(c) == current { "*" } else { " " };
        println!("{mark} {:<10} {}", c.as_str(), c.summary());
    }
    println!("\nMove with `sudo apex channel set <name>`.");
    println!("Moving toward stable usually deploys an older image; that direction pins the");
    println!("current deployment first and tells you what your saved settings would do about it.");
    0
}

fn set(want: Channel, dry_run: bool) -> i32 {
    let reference = match booted_reference() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("apex: cannot tell which image this machine follows: {e}");
            return 1;
        }
    };
    let here = channel::from_tag(&reference);
    let target = format!("{}:{}", repository(&reference), want.as_str());

    if here.channel == Some(want) && here.alias.is_none() {
        println!("apex: already following {}", want.as_str());
        return 0;
    }

    let backwards = here
        .channel
        .map(|from| channel::direction(from, want) == Direction::Behind)
        .unwrap_or(false);

    println!("apex: {reference} -> {target}");
    if backwards {
        println!(
            "\nThis moves toward stable, which usually deploys an OLDER image. What that does:"
        );
        println!("  /usr goes back. /etc, /var and your home do not — they are not part of the image.");
        for s in apexd_core::migrate::STORES {
            println!("  {:<16} {}", s.id, apexd_core::migrate::rollback_note(s));
        }
        println!("  `apex schema status` says which of those files this machine actually has.");
        println!("\nThe current deployment is pinned first, so you can boot back into it.");
    }

    if dry_run {
        println!("\nDry run. Nothing has been changed. Without --dry-run this would run:");
        if backwards {
            println!("  ostree admin pin 0");
        }
        println!("  bootc switch {target}");
        println!("  (then `sudo apex update` and a reboot)");
        return 0;
    }

    if backwards {
        // docs/rollback.md has promised this since M3 and nothing did it: bootc
        // keeps the booted deployment and one more, so a switch backwards
        // followed by one update can evict the deployment you would return to.
        match crate::ops::run("ostree", &["admin", "pin", "0"]) {
            Ok(0) => println!("apex: pinned the current deployment"),
            Ok(code) => {
                eprintln!("apex: could not pin the current deployment (exit {code}); not switching");
                return code;
            }
            Err(e) => {
                eprintln!("apex: could not pin the current deployment: {e}; not switching");
                return 1;
            }
        }
    }

    match crate::ops::run("bootc", &["switch", &target]) {
        Ok(0) => {
            println!("apex: now following {}", want.as_str());
            println!("apex: run `sudo apex update` and reboot to take it");
            0
        }
        Ok(code) => code,
        Err(e) => {
            eprintln!("apex: could not switch: {e}");
            1
        }
    }
}

fn report(as_json: bool) -> i32 {
    let t = match tracking() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("apex: cannot tell which image this machine follows: {e}");
            return 1;
        }
    };
    let digest = crate::trust::booted_digest(&roots()).ok();
    let (verdict, _) = health();
    let body = payload(&t, digest.as_deref(), &verdict);
    let c = consent();

    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "wouldSend": body,
                "optedIn": c.report,
                "endpoint": c.endpoint,
                "sent": false,
                "why": "no endpoint is configured, and APEX operates no telemetry service",
            }))
            .unwrap_or_default()
        );
        return 0;
    }

    println!("This is the whole report. Nothing else about this machine is in it.\n");
    println!("{}", serde_json::to_string_pretty(&body).unwrap_or_default());
    println!("\nNot in it: the machine id, its rollout slot, its name, its hardware, its");
    println!("packages, its user, its network. The rollout slot is left out on purpose —");
    println!("one of a hundred values derived from the machine id is a weak identifier and");
    println!("a health report has no use for it.\n");
    match (c.report, &c.endpoint) {
        (false, _) => println!("Nothing was sent: reporting is off, which is the default."),
        (true, None) => println!(
            "Nothing was sent: reporting is on, but no endpoint is configured. APEX operates \
             no telemetry service, so there is nothing to send it to unless you run one."
        ),
        (true, Some(e)) => println!(
            "Nothing was sent. Reporting is on and {e} is configured, but this build has no \
             transmitter: the send is the part of §26 that is not implemented."
        ),
    }
    if let Some(e) = &c.error {
        println!("({e}, so reporting stays off.)");
    }
    println!("\nOpt in by writing {} with `report = true` and your own", config_path().display());
    println!("`endpoint = \"...\"`. The default file does not exist and the default is off.");
    0
}

pub fn main(cmd: ChannelCmd) -> i32 {
    match cmd {
        ChannelCmd::Status { json } => status(json),
        ChannelCmd::List => list(),
        ChannelCmd::Set { channel, dry_run } => set(channel, dry_run),
        ChannelCmd::Report { json } => report(json),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_repository_survives_having_its_tag_replaced() {
        assert_eq!(
            repository("ghcr.io/andrenijman/apex-os:daily"),
            "ghcr.io/andrenijman/apex-os"
        );
        // A port on the registry host is not a tag, and getting this wrong
        // would build a switch target that names a nonexistent repository.
        assert_eq!(
            repository("registry.example:5000/apex/apex-os:stable"),
            "registry.example:5000/apex/apex-os"
        );
        assert_eq!(
            repository("registry.example:5000/apex/apex-os"),
            "registry.example:5000/apex/apex-os"
        );
        // A fork keeps its own registry, so `set` never sends anyone to
        // somebody else's images.
        assert_eq!(
            format!("{}:stable", repository("quay.io/someone/apex-os:edge")),
            "quay.io/someone/apex-os:stable"
        );
    }

    #[test]
    fn the_report_carries_no_identifier_and_says_so() {
        let t = channel::from_tag("ghcr.io/andrenijman/apex-os:daily");
        let v = Verdict { healthy: false, reasons: vec!["gpu-driver: no driver".into()] };
        let p = payload(&t, Some("sha256:308127d9"), &v);
        let keys: Vec<&String> = p.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["channel", "digest", "healthy", "reasons", "tag"]);
        // The bucket is the thing most likely to be added later "because it is
        // only a hundred values". It is a hundredth of a machine id.
        let text = serde_json::to_string(&p).unwrap();
        assert!(!text.contains("bucket"), "{text}");
        assert!(!text.contains("machine"), "{text}");
    }

    #[test]
    fn consent_defaults_to_off_and_an_unreadable_file_stays_off() {
        // A consent that fails open is not consent. Both the absent case and
        // the unreadable case answer no.
        let d = Consent::default();
        assert!(!d.report);
        assert_eq!(d.endpoint, None);
        let broken = Consent {
            report: false,
            endpoint: None,
            error: Some("channel.toml: Permission denied".into()),
        };
        assert!(!broken.report);
    }

    #[test]
    fn a_digest_is_shortened_without_losing_which_algorithm_it_is() {
        assert_eq!(
            short("sha256:308127d9cefeada90414ae37bdc8175d011c1f851ea9dde1661279a5da5bd89b"),
            "sha256:308127d9cefe"
        );
        assert_eq!(short("sha256:abc"), "sha256:abc");
    }

    #[test]
    fn the_record_path_is_readable_without_root() {
        // `apex update` runs as root and writes it; `apex channel status` runs
        // as the user and reads it. Under the user's home it would be invisible
        // to root's update, and under /root it would be invisible to status.
        assert!(RECORD.starts_with("/var/lib/apex/"));
        // And it goes through a fixture root, or the rollout stop is
        // untestable. Built directly rather than through the environment:
        // `cargo test` runs these in threads of one process, and
        // `blueprint.rs` already documents that it is the only test allowed to
        // mutate the environment. Adding a second one made an unrelated
        // blueprint assertion fail intermittently — two threads writing
        // `environ` is a data race, and the symptom lands somewhere else.
        let roots = crate::trust::Roots { fixture: Some(PathBuf::from("/tmp/apex-fixture")) };
        assert_eq!(
            roots.path(RECORD),
            PathBuf::from("/tmp/apex-fixture/var/lib/apex/channel/last-update.json")
        );
        assert!(!RECORD.contains("/root/"));
    }

    #[test]
    fn a_verdict_that_could_not_be_taken_does_not_hold_the_update() {
        // `halt_reason` refuses an update. Every uncertainty in it must permit
        // one instead, or a machine with an unreadable file is stranded on the
        // release that broke it.
        let v = channel::verdict(&[], &[]);
        assert!(v.healthy);
    }

    #[test]
    fn the_default_rollout_is_everybody() {
        // Every image published so far carries no rollout label, and that has
        // to mean "reaches every machine" rather than "reaches none".
        for b in 0..100u8 {
            assert!(channel::admits(b, channel::FULL_ROLLOUT));
        }
    }
}
