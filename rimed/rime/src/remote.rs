//! `rime remote` — pairing a phone, and taking it away again.
//!
//! Four verbs and one rule between them: **pairing is something a human does
//! at this machine**, and everything else can be scripted. `rime remote pair`
//! is refused by `rime-remoted` for any caller that does not classify as a
//! §7 local origin, which an agent inside a managed session never does.
//!
//! The pairing payload is printed here rather than by the service, because
//! the service has no terminal and the terminal is where the person is. What
//! this build does **not** print is a QR code: no encoder is vendored, and a
//! wrong QR is worse than none — a phone scans it, fails, and the person
//! concludes their camera is broken. P1-051's first criterion is a QR from
//! Rime Shell, which has a renderer; see [`qr_block`].

use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use clap::Subcommand;
use serde::{Deserialize, Serialize};

#[derive(Subcommand, Debug)]
pub enum RemoteCmd {
    /// Show a pairing code for a phone to scan.
    ///
    /// The code is good for three minutes and pairs exactly one device. It
    /// carries this machine's public key, so the device that scans it can
    /// never be talked into trusting a different machine.
    ///
    /// Refused unless you are running it yourself: an agent cannot pair a
    /// device on your behalf.
    Pair {
        /// Print the pairing payload as text instead of drawing a QR code.
        ///
        /// For a terminal that cannot draw one, and for pasting into a device
        /// that has no camera.
        #[arg(long)]
        text: bool,
    },
    /// Every device this machine has paired, and which of them are revoked.
    Devices {
        #[arg(long)]
        json: bool,
    },
    /// Take a device's access away.
    ///
    /// Immediate: a connection the device is already holding is dropped, not
    /// merely refused next time. The record is kept so the listing can still
    /// show that it was revoked and when.
    Revoke {
        /// Device id, or its name when that names exactly one.
        device: String,
    },
    /// Make a paired phone ring, to find it.
    ///
    /// Goes over its connection when Rime Remote is open on it, and through
    /// its push service when it is not. Says which, and says "asked" when
    /// nothing came back to prove the phone heard it.
    Ring {
        /// Device id, or its name when that names exactly one.
        device: String,
    },
    /// Whether the service is running, and how a device would reach it.
    ///
    /// Including every connection open right now: which path it came in on,
    /// and how good it is. That is the measurement P1-052 asks to be visible
    /// on the desktop, and `--json` is how Rime Settings reads it.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Turn Rime Remote on for this account.
    ///
    /// Enables and starts the per-user service, the same way `rime agent
    /// enable` does for the agent runtime. It does **not** open the firewall:
    /// LAN access needs `rime firewall allow rime-remote`, which is root, and
    /// is a separate decision because the relay path works without it.
    Enable,
}

// The control protocol, duplicated here as the client half. Kept in step by
// `rime-remoted`'s own round-trip tests plus the shape assertions below; a
// shared crate for four request variants would be a crate to keep in step
// instead of a struct.
#[derive(Debug, Serialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum Request {
    Status,
    Pair,
    Devices,
    Revoke { device: String },
    Ring { device: String },
}

/// A phone's last report, as `rime-remoted` keeps it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Phone {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    battery: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    charging: Option<bool>,
    #[serde(default)]
    can: Vec<String>,
    reported_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
enum Reply {
    Status {
        version: u32,
        key: String,
        machine: String,
        lan: Vec<String>,
        relay: Option<String>,
        rendezvous: String,
        paired: usize,
        offer_ms_left: Option<u64>,
        #[serde(default)]
        connections: Vec<rime_remote_core::rendezvous::Connection>,
    },
    Offer {
        qr: String,
        expires_ms: u64,
    },
    Devices {
        devices: Vec<rime_remote_core::device::Device>,
        #[serde(default)]
        phones: std::collections::BTreeMap<String, Phone>,
    },
    Rang {
        device: String,
        path: String,
    },
    Ok,
    Error {
        message: String,
    },
}

pub fn remote(cmd: RemoteCmd) -> i32 {
    // The exit code is this function's contract and the error printing is
    // its own, matching `agent::agent`: the dispatcher in `main` takes an
    // `i32` from every arm and a `Result` here would be one arm that behaves
    // differently for no reason a user could see.
    let result = match cmd {
        RemoteCmd::Pair { text } => pair(text),
        RemoteCmd::Devices { json } => devices(json),
        RemoteCmd::Revoke { device } => revoke(&device),
        RemoteCmd::Ring { device } => ring(&device),
        RemoteCmd::Status { json } => status(json),
        RemoteCmd::Enable => enable(),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("rime: {e:#}");
            1
        }
    }
}

fn socket_path() -> PathBuf {
    rime_agent_core::paths::runtime_dir()
        .join("rime-remoted")
        .join("control.sock")
}

fn call(req: &Request) -> Result<Reply> {
    let path = socket_path();
    let stream = UnixStream::connect(&path).with_context(|| {
        format!(
            "Rime Remote is not running.\n\
             turn it on with: systemctl --user enable --now rime-remoted\n\
             (socket: {})",
            path.display()
        )
    })?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(20)))
        .ok();
    let mut writer = stream.try_clone().context("cloning the control socket")?;
    let mut reader = BufReader::new(stream);
    let mut line = serde_json::to_string(req)?;
    line.push('\n');
    writer.write_all(line.as_bytes())?;
    writer.flush()?;
    let mut reply = String::new();
    reader.read_line(&mut reply).context("reading the reply")?;
    serde_json::from_str(reply.trim()).with_context(|| format!("unreadable reply: {reply}"))
}

/// Start the daemon if it is not answering, and wait for it to.
///
/// Rime Remote is enabled for every account, but pairing must not depend on
/// it having come up: a unit that crash-looped once (a port another account
/// held, before that was fixed) stays down until something starts it. So the
/// commands a person reaches for to pair start it themselves. `start`, not
/// `enable` — whether it runs at login is the image's decision, not this
/// command's side effect.
fn ensure_running() -> bool {
    let socket = socket_path();
    if UnixStream::connect(&socket).is_ok() {
        return true;
    }
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "reset-failed", "rime-remoted"])
        .output();
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "start", "rime-remoted"])
        .output();
    wait_for_socket(&socket)
}

/// Whether the daemon answers on `socket` within three seconds.
fn wait_for_socket(socket: &std::path::Path) -> bool {
    (0..30).any(|_| {
        if UnixStream::connect(socket).is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        false
    })
}

fn pair(text: bool) -> Result<i32> {
    // Best effort: if it still cannot be reached, `call` below says so, with
    // the socket path, exactly as before.
    ensure_running();
    match call(&Request::Pair)? {
        Reply::Offer { qr, expires_ms } => {
            let left = expires_ms.saturating_sub(rime_remote_core::now_ms()) / 1000;
            if text {
                println!("{qr}");
            } else {
                print!("{}", qr_block(&qr));
            }
            eprintln!(
                "\nScan this with Rime Remote. It is good for {left} seconds and pairs one \
                 device."
            );
            eprintln!(
                "The code carries this machine's public key, so the phone that scans it cannot \
                 be talked into trusting a different machine."
            );
            // With a relay the code also names it, and pairing is the moment to
            // say what its operator can see — the same sentence `status` prints.
            if let Ok(Reply::Status { relay: Some(_), .. }) = call(&Request::Status) {
                eprintln!(
                    "Away from this network the phone connects {}.",
                    rime_remote_core::rendezvous::Path::Relay.disclosure()
                );
            }
            Ok(0)
        }
        Reply::Error { message } => {
            eprintln!("rime: {message}");
            Ok(1)
        }
        other => bail!("unexpected reply: {other:?}"),
    }
}

fn devices(json: bool) -> Result<i32> {
    let Reply::Devices { devices, phones } = call(&Request::Devices)? else {
        return unexpected();
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&with_phones(&devices, &phones)?)?);
        return Ok(0);
    }
    if devices.is_empty() {
        println!("no devices paired. `rime remote pair` shows a code.");
        return Ok(0);
    }
    println!("{:<18} {:<20} {:<9} {:<8} LAST SEEN", "ID", "NAME", "STATE", "PATH");
    for d in &devices {
        println!(
            "{:<18} {:<20} {:<9} {:<8} {}",
            d.id,
            truncate(&d.name, 20),
            d.state(),
            d.last_path.as_deref().unwrap_or("-"),
            d.last_seen_ms.map(ago).unwrap_or_else(|| "never".into()),
        );
    }
    // Said once, at the end, rather than as a column: it is a property of the
    // device the desktop cannot check, and a tick in a table would read as a
    // fact this machine had verified.
    let gated: Vec<&str> = devices
        .iter()
        .filter(|d| d.requires_user_verification && d.is_active())
        .map(|d| d.name.as_str())
        .collect();
    if !gated.is_empty() {
        println!(
            "\n{} said its key is held behind a biometric or device lock. That is the \
             device's own claim; this machine cannot verify it.",
            gated.join(", ")
        );
    }
    let reported: Vec<String> = devices
        .iter()
        .filter(|d| d.is_active())
        .filter_map(|d| phones.get(&d.id).and_then(|p| battery_line(&d.name, p)))
        .collect();
    if !reported.is_empty() {
        println!();
        for line in reported {
            println!("{line}");
        }
    }
    Ok(0)
}

/// The device list as JSON, each paired phone with its last report under
/// `phone`. Added to each device rather than beside the list, so a reader of
/// the old array keeps working.
fn with_phones(
    devices: &[rime_remote_core::device::Device],
    phones: &std::collections::BTreeMap<String, Phone>,
) -> Result<serde_json::Value> {
    let mut out = Vec::with_capacity(devices.len());
    for d in devices {
        let mut v = serde_json::to_value(d)?;
        if let (Some(p), Some(obj)) = (phones.get(&d.id).filter(|_| d.is_active()), v.as_object_mut()) {
            obj.insert("phone".into(), serde_json::to_value(p)?);
        }
        out.push(v);
    }
    Ok(serde_json::Value::Array(out))
}

/// "Pixel 8: battery 62%, charging, reported 3m ago", or nothing when the phone
/// reported no battery.
fn battery_line(name: &str, p: &Phone) -> Option<String> {
    let battery = p.battery?;
    let charging = match p.charging {
        Some(true) => ", charging",
        _ => "",
    };
    Some(format!("{name}: battery {battery}%{charging}, reported {}", ago(p.reported_ms)))
}

fn ring(device: &str) -> Result<i32> {
    match call(&Request::Ring {
        device: device.to_string(),
    })? {
        Reply::Rang { device, path } if path == "link" => {
            eprintln!("rime: {device} is ringing.");
            Ok(0)
        }
        Reply::Rang { device, .. } => {
            eprintln!(
                "rime: asked {device} to ring through its push service. It rings if it is \
                 switched on and online."
            );
            Ok(0)
        }
        Reply::Error { message } => {
            eprintln!("rime: {message}");
            Ok(1)
        }
        _ => unexpected(),
    }
}

fn revoke(device: &str) -> Result<i32> {
    match call(&Request::Revoke {
        device: device.to_string(),
    })? {
        Reply::Ok => {
            eprintln!("rime: {device} revoked. Any connection it was holding has been dropped.");
            Ok(0)
        }
        Reply::Error { message } => {
            eprintln!("rime: {message}");
            Ok(1)
        }
        _ => unexpected(),
    }
}

fn status(json: bool) -> Result<i32> {
    let reply = call(&Request::Status)?;
    let Reply::Status {
        version,
        key,
        machine,
        lan,
        relay,
        rendezvous,
        paired,
        offer_ms_left,
        connections,
    } = reply
    else {
        return unexpected();
    };
    if json {
        // The daemon's own reply, re-emitted rather than re-assembled: a
        // second serialisation here is a second thing to keep in step with
        // the page that reads it.
        println!(
            "{}",
            serde_json::json!({
                "machine": machine,
                "protocol": version,
                "identity": key,
                "paired": paired,
                "lan": lan,
                "relay": relay,
                "rendezvous": rendezvous,
                "offer_ms_left": offer_ms_left,
                "connections": connections
                    .iter()
                    .map(|c| serde_json::json!({
                        "device_id": c.device_id,
                        "device_name": c.device_name,
                        "path": c.path,
                        "since_ms": c.since_ms,
                        "rtt_ms": c.rtt_ms,
                        "quality": c.quality().as_str(),
                        "disclosure": c.disclosure(),
                    }))
                    .collect::<Vec<_>>(),
            })
        );
        return Ok(0);
    }
    println!("machine        {machine}");
    println!("protocol       {version}");
    println!("identity       {key}");
    println!("paired         {paired}");
    println!(
        "lan            {}",
        if lan.is_empty() {
            "none — this machine has no reachable address".to_string()
        } else {
            lan.join(", ")
        }
    );
    match &relay {
        Some(r) => {
            println!("relay          {r}");
            println!("rendezvous     {rendezvous}");
            println!(
                "\n{}",
                rime_remote_core::rendezvous::Path::Relay.disclosure()
            );
        }
        None => println!("relay          none — this machine is reachable on the LAN only"),
    }
    if connections.is_empty() {
        println!("connected      no device is connected right now");
    } else {
        println!("connected      {} device(s)", connections.len());
        for c in &connections {
            // The round trip AND the band. The number is for whoever wants
            // it; the word is what tells somebody whether their terminal is
            // going to feel wrong.
            let rtt = match c.rtt_ms {
                Some(ms) => format!("{ms} ms"),
                None => "not measured yet".to_string(),
            };
            println!("{}", connection_line(c, &rtt));
        }
    }
    if let Some(ms) = offer_ms_left {
        println!("\na pairing code is open for another {} seconds", ms / 1000);
    }
    Ok(0)
}

/// One line about one open connection.
///
/// Extracted so it can be asserted about without starting a daemon, and
/// because the three things it has to carry — which device, which path, how
/// good — are exactly P1-052's last criterion and are easy to lose one of.
fn connection_line(c: &rime_remote_core::rendezvous::Connection, rtt: &str) -> String {
    format!(
        "  {} over {} — {} ({})",
        c.device_name,
        c.path,
        c.quality(),
        rtt
    )
}

/// `systemctl --user enable --now rime-remoted`, and say what it did not do.
///
/// Mirrors `rime agent enable`, which exists because a per-user daemon that
/// has to be started with a systemctl invocation is one people get wrong.
fn enable() -> Result<i32> {
    // A unit that crash-looped earlier is start-limited; clear that first, or
    // enabling it again changes nothing and says nothing.
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "reset-failed", "rime-remoted"])
        .output();
    let out = std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", "rime-remoted"])
        .output()
        .context("running systemctl")?;
    // `enable --now` succeeds once the process has STARTED, including one that
    // exits a moment later — which is how "rime:" with nothing after it, and a
    // silent `systemctl`, both happened while the daemon was dying on a port
    // in use. So wait for it to answer, and when it does not, show its own
    // last words rather than systemctl's.
    let running = out.status.success() && wait_for_socket(&socket_path());
    if !running {
        let systemctl = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let journal = std::process::Command::new("journalctl")
            .args(["--user", "-u", "rime-remoted", "-n", "20", "-o", "cat", "--no-pager"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        let why = journal
            .lines()
            .rev()
            .find(|l| l.starts_with("rime-remoted:"))
            .map(str::to_string)
            .or_else(|| (!systemctl.is_empty()).then_some(systemctl))
            .unwrap_or_else(|| "it exited without saying why; see `journalctl --user -u rime-remoted`".to_string());
        eprintln!("rime: Rime Remote did not start: {why}");
        return Ok(1);
    }
    eprintln!("rime: Rime Remote is on. `rime remote pair` shows a code for a phone.");
    eprintln!(
        "rime: the LAN port is still closed. Open it with `sudo rime firewall allow \
         rime-remote`, or leave it shut and reach this machine through a relay."
    );
    Ok(0)
}

fn unexpected() -> Result<i32> {
    bail!("the remote service answered something this build does not understand")
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    // Truncated on characters, not bytes: a name in a non-Latin script would
    // otherwise be cut mid-codepoint and print as a replacement character.
    s.chars().take(n.saturating_sub(1)).collect::<String>() + "…"
}

fn ago(ms: u64) -> String {
    let now = rime_remote_core::now_ms();
    let secs = now.saturating_sub(ms) / 1000;
    match secs {
        0..=59 => format!("{secs}s ago"),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}

/// Draw a QR code for `payload` using half-block characters.
///
/// Deliberately not a dependency. A QR encoder is a few hundred lines of
/// Reed-Solomon and a fixed set of tables, and the version here does the one
/// job this command needs — but the shipped implementation is not that
/// either: see [`qr_block`]'s body. What is here now is the fallback, and it
/// is honest about being one.
fn qr_block(payload: &str) -> String {
    // No QR encoder is vendored in the terminal. Printing a wrong QR code would
    // be worse than printing none: a phone would scan it, fail, and the person
    // would conclude their camera or the app was broken. So this prints the
    // payload and points at the page that does draw one — Settings → Pair a
    // device, which has shipped and has a renderer.
    format!(
        "{payload}\n\n\
         (To scan a QR code, open Rime Settings → Pair a device. Or paste the line above into \
         Rime Remote on your phone.)\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_control_socket_is_the_one_the_service_binds() {
        // Two crates, one path. They are written independently, and a drift
        // here is `rime remote status` reporting that the service is not
        // running while it is.
        let mine = socket_path();
        assert!(mine.ends_with("rime-remoted/control.sock"), "{mine:?}");
    }

    #[test]
    fn every_request_survives_the_line_framing() {
        for r in [
            Request::Status,
            Request::Pair,
            Request::Devices,
            Request::Revoke {
                device: "pixel-8".into(),
            },
            Request::Ring {
                device: "pixel-8".into(),
            },
        ] {
            let text = serde_json::to_string(&r).expect("serialise");
            assert!(!text.contains('\n'), "{text}");
        }
    }

    #[test]
    fn the_replies_this_build_parses_are_the_ones_the_service_sends() {
        // Written as the wire text rather than as the service's own types,
        // because that is what actually crosses the socket and it is the
        // thing that can drift.
        for text in [
            r#"{"reply":"ok"}"#,
            r#"{"reply":"error","message":"nope"}"#,
            r#"{"reply":"offer","qr":"apex-remote:abc","expires_ms":1}"#,  // rime-rename: keep (SCHEME)
            r#"{"reply":"devices","devices":[]}"#,
            r#"{"reply":"devices","devices":[],"phones":{"d":{"battery":5,"can":["ring"],"reported_ms":1}}}"#,
            r#"{"reply":"rang","device":"Pixel","path":"push"}"#,
            r#"{"reply":"status","version":1,"key":"k","machine":"l16","lan":[],"relay":null,"rendezvous":"r","paired":0,"offer_ms_left":null}"#,
        ] {
            serde_json::from_str::<Reply>(text).unwrap_or_else(|e| panic!("{text}: {e}"));
        }
    }

    #[test]
    fn ring_is_its_own_request_on_the_wire() {
        let text = serde_json::to_string(&Request::Ring { device: "Pixel".into() }).expect("serialise");
        assert_eq!(text, r#"{"cmd":"ring","device":"Pixel"}"#);
    }

    #[test]
    fn a_phone_report_rides_on_its_device_and_only_while_it_is_paired() {
        let devices: Vec<rime_remote_core::device::Device> = serde_json::from_str(
            r#"[{"id":"aaaa","name":"Pixel","public_key":"k","paired_ms":1},
                {"id":"bbbb","name":"Old","public_key":"k2","paired_ms":2,"revoked_ms":3},
                {"id":"cccc","name":"Tab","public_key":"k3","paired_ms":4}]"#,
        )
        .expect("devices");
        let phone = Phone { battery: Some(62), charging: Some(true), can: vec!["ring".into()], reported_ms: 0 };
        let phones: std::collections::BTreeMap<String, Phone> =
            [("aaaa".to_string(), phone.clone()), ("bbbb".to_string(), phone.clone())].into();
        let v = with_phones(&devices, &phones).expect("json");
        assert_eq!(v[0]["phone"]["battery"], 62);
        assert!(v[1].get("phone").is_none(), "a revoked device's report must not show");
        assert!(v[2].get("phone").is_none(), "a device that never reported has none");
        assert_eq!(v[0]["name"], "Pixel", "the device's own fields are kept");
        let line = battery_line("Pixel", &phone).expect("line");
        assert!(line.starts_with("Pixel: battery 62%, charging, reported "), "{line}");
        assert_eq!(battery_line("Pixel", &Phone { battery: None, ..phone }), None);
    }

    #[test]
    fn a_name_is_truncated_on_characters_and_not_on_bytes() {
        assert_eq!(truncate("short", 20), "short");
        assert_eq!(truncate("abcdefghij", 5), "abcd…");
        // Multibyte: a byte-wise truncation would panic or emit a broken
        // codepoint here.
        let cyrillic = "телефонтелефон";
        let cut = truncate(cyrillic, 5);
        assert_eq!(cut.chars().count(), 5);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn the_pairing_output_does_not_pretend_to_be_a_qr_code() {
        // The failure this avoids: a phone scans a wrong QR, fails, and the
        // person blames their camera. If an encoder is ever vendored, this
        // test is what has to change with it.
        let out = qr_block("apex-remote:abc");  // rime-rename: keep (SCHEME)
        assert!(out.contains("apex-remote:abc"));  // rime-rename: keep (SCHEME)
        assert!(out.contains("Rime Settings"), "{out}");
        // The pairing page has shipped; the output must not still promise it.
        assert!(!out.contains("once it ships"), "{out}");
        assert!(out.contains("Pair a device"), "{out}");
    }

    #[test]
    fn elapsed_time_reads_in_the_unit_a_person_would_use() {
        let now = rime_remote_core::now_ms();
        assert!(ago(now).ends_with("s ago"));
        assert!(ago(now.saturating_sub(120_000)).ends_with("m ago"));
        assert!(ago(now.saturating_sub(7_200_000)).ends_with("h ago"));
        assert!(ago(now.saturating_sub(3 * 86_400_000)).ends_with("d ago"));
        // A timestamp from the future is not a panic and not a negative.
        assert_eq!(ago(now + 60_000), "0s ago");
    }

    #[test]
    fn a_status_reply_from_a_daemon_that_predates_connections_still_parses() {
        // The CLI and the daemon land in one image but not in one build, and
        // a shell page that showed nothing because a field was absent would
        // be a regression nobody could see coming.
        let old = r#"{"reply":"status","version":1,"key":"k","machine":"l16","lan":[],"relay":null,"rendezvous":"r","paired":0,"offer_ms_left":null}"#;
        let Reply::Status { connections, .. } = serde_json::from_str(old).expect("parse") else {
            panic!("not a status");
        };
        assert!(connections.is_empty());
    }

    #[test]
    fn a_connection_line_names_the_device_the_path_and_the_band() {
        // All three, because each has been the one somebody left out: a line
        // with only a number does not say whether a third party is carrying
        // it, and a line with only a path does not say whether it is usable.
        let c = rime_remote_core::rendezvous::Connection {
            device_id: "abcd".into(),
            device_name: "pixel-8".into(),
            path: "relay".into(),
            since_ms: 0,
            rtt_ms: Some(310),
        };
        let line = connection_line(&c, "310 ms");
        assert!(line.contains("pixel-8"), "{line}");
        assert!(line.contains("relay"), "{line}");
        assert!(line.contains("poor"), "{line}");
        assert!(line.contains("310 ms"), "{line}");

        // And an unmeasured connection says so rather than reporting the
        // worst band for a connection that has simply just opened.
        let fresh = rime_remote_core::rendezvous::Connection { rtt_ms: None, ..c };
        let line = connection_line(&fresh, "not measured yet");
        assert!(line.contains("unmeasured"), "{line}");
        assert!(!line.contains("poor"), "{line}");
    }
}
