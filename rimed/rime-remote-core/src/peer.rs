//! What two paired computers tell each other on every connection: the
//! computers each has removed (tombstones) and how each is doing right now
//! (presence). `docs/remote-live-contract.md` §2.15.
//!
//! Both arrive from another machine, so both are checked here, whole, before
//! anything is stored: a message with one bad entry is refused, not trimmed.
//!
//! ## Tombstones
//!
//! A tombstone is a removed computer's key and when it was removed. Members
//! pass on every tombstone they hold (their own removals and ones they
//! learned), so a removal reaches everyone. The removal time travels
//! unchanged, never re-stamped on the way: `forget` on one computer compares
//! against it, and a re-stamped copy would come back newer than every
//! forget. A time of 0, or one more than [`MAX_SKEW_MS`] ahead of this
//! computer's clock, is refused: a removal "in the year 2500" would beat
//! every forget until then and spread through honest members.
//!
//! ## Presence
//!
//! Name, battery, AC, power mode, lock, network kind and uptime, each from a
//! closed set or a range. Kept in memory only by the receiver.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The most tombstones one message may carry, and the most a computer
/// records from others (`DeviceStore::apply_tombstones`): a peer's list is
/// written to disk. A household has nowhere near this many computers.
pub const MAX_TOMBSTONES: usize = 128;

/// How far ahead of this computer's clock a removal time may be.
pub const MAX_SKEW_MS: u64 = 10 * 60 * 1000;

/// The power modes rimed reports (`org.rimeos.Rimed1.Power.Tier`).
pub const POWER_MODES: [&str; 3] = ["power-saver", "balanced", "performance"];

/// The network kinds presence may name.
pub const NETWORKS: [&str; 3] = ["wired", "wifi", "none"];

/// The longest uptime believed, in seconds: twenty years.
pub const MAX_UPTIME_S: u64 = 20 * 365 * 24 * 3600;

/// One removed computer, as it travels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tombstone {
    /// Its static public key, base64url.
    pub key: String,
    /// Unix milliseconds at which it was first removed, anywhere.
    pub removed_ms: u64,
    /// What it was called, so a computer that never knew it can list it.
    pub name: String,
}

/// A tombstone list from another computer, checked: at most
/// [`MAX_TOMBSTONES`], each key 32 bytes, each name by the device-name rule,
/// each time neither 0 nor more than [`MAX_SKEW_MS`] after `now_ms`. Keys
/// come back in their canonical spelling.
pub fn checked_tombstones(v: &Value, now_ms: u64) -> Result<Vec<Tombstone>, String> {
    let list = v.as_array().ok_or("tombstones is not a list")?;
    if list.len() > MAX_TOMBSTONES {
        return Err(format!("{} tombstones; at most {MAX_TOMBSTONES} are taken", list.len()));
    }
    let mut out = Vec::with_capacity(list.len());
    for t in list {
        let t: Tombstone = serde_json::from_value(t.clone()).map_err(|e| format!("a tombstone that is not one: {e}"))?;
        let raw = crate::device::check_key(&t.key).map_err(|e| e.to_string())?;
        let name = crate::device::check_name(&t.name).map_err(|e| e.to_string())?;
        if t.removed_ms == 0 {
            return Err("a tombstone with no removal time".into());
        }
        if t.removed_ms > now_ms.saturating_add(MAX_SKEW_MS) {
            return Err("a tombstone removed in the future (the other computer's clock is wrong)".into());
        }
        out.push(Tombstone { key: crate::b64_encode(&raw), removed_ms: t.removed_ms, name });
    }
    Ok(out)
}

/// A battery, as presence carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
}

/// How a computer is doing, as it said at `seen` time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presence {
    pub name: String,
    pub battery: Option<Battery>,
    pub on_ac: Option<bool>,
    pub power_mode: Option<String>,
    pub locked: Option<bool>,
    pub network: Option<String>,
    pub uptime_s: Option<u64>,
}

/// What arrives, before the checks: wide types, so a percent of 300 is a
/// refusal with a reason and not a parse error that reads like one.
#[derive(Deserialize)]
struct Wire {
    name: String,
    #[serde(default)]
    battery: Option<WireBattery>,
    #[serde(default)]
    on_ac: Option<bool>,
    #[serde(default)]
    power_mode: Option<String>,
    #[serde(default)]
    locked: Option<bool>,
    #[serde(default)]
    network: Option<String>,
    #[serde(default)]
    uptime_s: Option<u64>,
}

#[derive(Deserialize)]
struct WireBattery {
    percent: u64,
    charging: bool,
}

impl Presence {
    /// Another computer's presence, checked whole: the name by the
    /// device-name rule, the percent 0-100, the power mode and the network
    /// from their closed sets, the uptime at most [`MAX_UPTIME_S`].
    pub fn checked(v: &Value) -> Result<Presence, String> {
        let w: Wire = serde_json::from_value(v.clone()).map_err(|e| format!("presence that is not presence: {e}"))?;
        let name = crate::device::check_name(&w.name).map_err(|e| e.to_string())?;
        let battery = match w.battery {
            None => None,
            Some(b) if b.percent <= 100 => Some(Battery { percent: b.percent as u8, charging: b.charging }),
            Some(b) => return Err(format!("a battery at {} percent", b.percent)),
        };
        if let Some(m) = &w.power_mode {
            if !POWER_MODES.contains(&m.as_str()) {
                return Err(format!("a power mode that is not one: {:?}", m.chars().take(40).collect::<String>()));
            }
        }
        if let Some(n) = &w.network {
            if !NETWORKS.contains(&n.as_str()) {
                return Err(format!("a network kind that is not one: {:?}", n.chars().take(40).collect::<String>()));
            }
        }
        if w.uptime_s.is_some_and(|u| u > MAX_UPTIME_S) {
            return Err("an uptime of more than twenty years".into());
        }
        Ok(Presence {
            name,
            battery,
            on_ac: w.on_ac,
            power_mode: w.power_mode,
            locked: w.locked,
            network: w.network,
            uptime_s: w.uptime_s,
        })
    }
}

/// One computer as a listing shows it: its record, and the presence it last
/// gave with when, which the listing computer keeps in memory only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listed {
    #[serde(flatten)]
    pub computer: crate::device::Device,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<Presence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_ms: Option<u64>,
    /// The sharing switches as they count right now (§2.15), from the
    /// record's [`crate::device::Share`]: all off for a computer that is
    /// removed, or introduced and not allowed yet.
    #[serde(default)]
    pub clipboard_send: bool,
    #[serde(default)]
    pub clipboard_receive: bool,
    #[serde(default)]
    pub files: bool,
}

impl Listed {
    /// A record as a listing shows it, with no presence.
    pub fn of(computer: crate::device::Device) -> Listed {
        Listed {
            clipboard_send: computer.sends_clipboard(),
            clipboard_receive: computer.takes_clipboard(),
            files: computer.takes_files(),
            computer,
            presence: None,
            presence_ms: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn key(seed: u8) -> String {
        crate::b64_encode(&[seed; 32])
    }

    #[test]
    fn a_tombstone_list_is_taken_whole_or_not_at_all() {
        let now = 1_000_000;
        let good = json!([{"key": key(1), "removed_ms": 10, "name": "desk"}]);
        let t = checked_tombstones(&good, now).expect("good");
        assert_eq!(t, [Tombstone { key: key(1), removed_ms: 10, name: "desk".into() }]);
        // Just inside the clock allowance is taken; just past it is not.
        assert!(checked_tombstones(&json!([{"key": key(1), "removed_ms": now + MAX_SKEW_MS, "name": "d"}]), now).is_ok());
        for bad in [
            json!({"key": key(1)}),
            json!([{"key": "short", "removed_ms": 10, "name": "desk"}]),
            json!([{"key": key(1), "removed_ms": 0, "name": "desk"}]),
            json!([{"key": key(1), "removed_ms": now + MAX_SKEW_MS + 1, "name": "desk"}]),
            json!([{"key": key(1), "removed_ms": u64::MAX, "name": "desk"}]),
            json!([{"key": key(1), "removed_ms": 10, "name": "desk\r\nAPPROVED"}]),
            json!([{"key": key(1), "removed_ms": 10, "name": ""}]),
            json!([{"key": key(1), "removed_ms": 10}]),
            json!([{"key": key(1), "removed_ms": 10, "name": "ok"}, {"key": key(2), "removed_ms": -1, "name": "x"}]),
        ] {
            assert!(checked_tombstones(&bad, now).is_err(), "{bad}");
        }
        let many: Vec<Value> = (0..=MAX_TOMBSTONES).map(|i| json!({"key": key(i as u8), "removed_ms": 1, "name": "x"})).collect();
        assert!(checked_tombstones(&Value::Array(many), now).is_err(), "more than the cap");
    }

    #[test]
    fn presence_is_checked_against_ranges_and_closed_sets() {
        let good = json!({"name": "desk", "battery": {"percent": 100, "charging": true}, "on_ac": true,
                          "power_mode": "balanced", "locked": false, "network": "wifi", "uptime_s": 3600,
                          "later": "ignored"});
        let p = Presence::checked(&good).expect("good");
        assert_eq!(p.battery, Some(Battery { percent: 100, charging: true }));
        assert_eq!((p.network.as_deref(), p.uptime_s), (Some("wifi"), Some(3600)));
        let bare = Presence::checked(&json!({"name": "desk"})).expect("all unknown");
        assert_eq!((bare.battery, bare.power_mode, bare.network), (None, None, None));
        for bad in [
            json!({"name": "desk", "battery": {"percent": 101, "charging": false}}),
            json!({"name": "desk", "battery": {"percent": -1, "charging": false}}),
            json!({"name": "desk", "battery": {"percent": 50}}),
            json!({"name": "desk", "power_mode": "turbo"}),
            json!({"name": "desk", "network": "carrier pigeon"}),
            json!({"name": "desk", "network": 5}),
            json!({"name": "desk", "uptime_s": MAX_UPTIME_S + 1}),
            json!({"name": "x".repeat(65)}),
            json!({"name": "a\u{1b}[2Jb"}),
            json!({"battery": null}),
            json!("desk"),
        ] {
            assert!(Presence::checked(&bad).is_err(), "{bad}");
        }
    }
}
