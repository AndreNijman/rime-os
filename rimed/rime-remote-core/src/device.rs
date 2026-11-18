//! Paired devices: what is stored about one, and what revoking it means.
//!
//! ## What is stored, and what is not
//!
//! A device record holds a **public** key and nothing else that is secret.
//! This machine's own static secret key lives in a separate file
//! ([`crate::identity`]) that no listing, no reply and no log line ever
//! touches; a device's *private* key never leaves the device, which is the
//! point of pairing a key rather than sharing a passphrase.
//!
//! So a stolen `devices.json` tells an attacker which phones the owner has
//! paired and when they last connected. It does not let them connect.
//!
//! ## Why revocation is a tombstone and not a deletion
//!
//! `revoked_ms` is set; the record stays. Deleting it would leave the store
//! unable to tell "a device that was never paired" from "a device the owner
//! threw off this machine on Tuesday", and those want different answers: the
//! first may pair again with a fresh token the owner approves, and the second
//! must be refused even if it somehow obtains one. P1-051 asks for a
//! paired-device list showing revocation state, which is the same requirement
//! from the other side — a list that forgets cannot show it.
//!
//! A tombstone also survives the race that matters. A phone with a live
//! connection when the owner revokes it is holding a channel this store
//! cannot reach into; the connection is dropped by `rime-remoted`, and the
//! tombstone is what stops the immediate reconnect.
//!
//! ## The companion key, and why it is a second key rather than a mode
//!
//! A phone's own key is held behind its lock screen, so after the phone
//! restarts nothing can open a session until somebody unlocks it. The
//! companion key is a second X25519 key the phone keeps WITHOUT that gate, so
//! its device features (battery report, notifications, ring) keep working
//! after a restart. It is minted only by a session on the full key
//! (`companion_register` in `rime-remoted`), stored here on the same record,
//! and a session it opens gets [`Scope::Companion`]: `rime-remoted` lets such
//! a session use an allow list and nothing else. The scope comes from WHICH
//! key completed the handshake, never from anything the phone says.
//!
//! Revoking the device refuses both keys (the tombstone is on the record, not
//! on a key), and pairing the device again starts with no companion key.
//!
//! ## Other computers, and why they are a separate file
//!
//! Another Rime computer paired with a code (`rime remote computers add`) is
//! kept under `computers`, never under `devices`, and a session its key opens
//! gets [`Scope::Peer`]: `rime-remoted` answers such a session from a short
//! allow list and never forwards it to `rime-agentd`. Not a flag on a phone's
//! record, because an older `rime-remoted` (after a rollback) would ignore an
//! unknown flag and load a computer as a phone with everything a phone may do.
//!
//! And not in `devices.json` at all, but in `computers.json` beside it
//! ([`DeviceStore::computers_path`]). An older `rime-remoted` reads
//! `devices.json` into a struct with only `devices` and writes it back whole
//! on every pairing, revocation and completed handshake, which would drop an
//! unknown `computers` map, and with it every removal's tombstone and the
//! keys a phone may never take. It never opens `computers.json`, so after a
//! rollback and a roll forward the computers, removed ones included, are all
//! still there. A key that ends up in both files anyway (an older daemon
//! paired it as a phone in between) authenticates as nothing
//! ([`DeviceStore::authenticate`]).
//!
//! One key is one record across both maps: a key that is, or ever was, a
//! phone's key or companion key cannot become a computer, nor the other way
//! round.
//!
//! ## Removing a computer, and adding it back
//!
//! A removed computer's tombstone is never deleted and its key never
//! authenticates again. The one way back is the person at this machine
//! running `rime remote computers forget`, which records `forgotten_ms`
//! beside the removal and lets the key pair again, with a code, here only.
//! The record then has three states: paired, removed, and removed but
//! forgotten (refused as a peer, allowed to pair).
//!
//! Members pass their tombstones on (`crate::peer`). One received for a
//! computer applies when it is newer than that computer's forget
//! ([`DeviceStore::apply_tombstones`]): a member replaying the old removal
//! cannot undo a forget, and a new removal still wins. A tombstone never
//! touches a phone, and one for this machine's own key is never recorded:
//! it says the computer that sent it has removed this one, so this one
//! removes the sender.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A device that has been paired with this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    /// Short, stable, derived from the public key — see [`Device::id_for`].
    ///
    /// Derived rather than assigned so that two records for one key are
    /// impossible, and so an id in a log line can be checked against the key
    /// it names without a lookup.
    pub id: String,
    /// What the owner calls it. Chosen on the device at pairing time and
    /// changeable from either end; never used for anything but display, so a
    /// duplicate name is untidy rather than dangerous.
    pub name: String,
    /// The device's static public key, base64url, no padding.
    ///
    /// The authenticating fact. A connection is this device exactly when the
    /// handshake proves possession of the matching secret.
    pub public_key: String,
    /// Unix milliseconds at which pairing completed.
    pub paired_ms: u64,
    /// Unix milliseconds of the last completed handshake.
    #[serde(default)]
    pub last_seen_ms: Option<u64>,
    /// Unix milliseconds at which the owner revoked it, if they have.
    #[serde(default)]
    pub revoked_ms: Option<u64>,
    /// Whether this machine requires the device to have re-authenticated its
    /// user before it acts.
    ///
    /// P1-051's last criterion. Set here, enforced on Android: the desktop
    /// cannot see a fingerprint, so what this actually buys is that the
    /// device's key is held in a keystore that will not sign without a
    /// biometric or device credential. That is a claim the device makes and
    /// the desktop cannot verify — which is exactly why it is recorded as a
    /// requirement the owner set and not as a fact about the device. See the
    /// design note on what this is and is not worth.
    #[serde(default)]
    pub requires_user_verification: bool,
    /// The last transport a completed handshake arrived over, for the
    /// connection-path display P1-052 asks for.
    #[serde(default)]
    pub last_path: Option<String>,
    /// A second public key, base64url, that opens a [`Scope::Companion`]
    /// session for this device. Set only from a full-scope session; see the
    /// module note. Absent from stores written before it existed, which
    /// therefore load with none.
    ///
    /// Never shown: `rime remote devices` says whether one is set and
    /// nothing more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub companion_key: Option<String>,
    /// Every companion key this device had and gave up: replaced, forgotten,
    /// or carried over when it was paired again. Kept so a key that leaked
    /// can never come back, neither as somebody's device key nor as a
    /// companion key again. Never shown, like `companion_key`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retired_companion_keys: Vec<String>,
    /// For another computer only: `host:port` addresses it can be dialled at
    /// on a LAN, the one the owner typed first. Empty for a phone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<String>,
    /// For another computer only: the relay it waits on, when it said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<String>,
    /// For a removed computer only: when the person here ran `forget` for
    /// it. A removal at or before this time no longer blocks pairing it
    /// again, and a tombstone a member passes on applies only when newer.
    /// Kept after it pairs again, for exactly that comparison.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forgotten_ms: Option<u64>,
    /// For a removed computer only: when it acknowledged a tombstone list
    /// that named it, so it knows it was removed here and stops dialling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub told_ms: Option<u64>,
}

impl Device {
    /// Whether `raw` is this record's companion key or one it retired.
    fn ever_had_companion(&self, raw: &[u8; 32]) -> bool {
        self.companion_key
            .iter()
            .chain(self.retired_companion_keys.iter())
            .any(|k| canonical(k).as_ref() == Some(raw))
    }

    /// Move the companion key, if any, to the retired ones.
    fn retire_companion(&mut self) -> Option<String> {
        let old = self.companion_key.take()?;
        if !self.retired_companion_keys.contains(&old) {
            self.retired_companion_keys.push(old.clone());
        }
        Some(old)
    }
}

/// What a session may do, decided by which of a device's keys completed the
/// handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The device's own key: everything a paired device may do.
    Full,
    /// The device's companion key: only `rime-remoted`'s allow list.
    Companion,
    /// Another of the owner's computers: only `rime-remoted`'s peer allow
    /// list, never `rime-agentd`, never the human gate.
    Peer,
}

impl Scope {
    /// The word `remote_hello` tells the phone.
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Full => "full",
            Scope::Companion => "companion",
            Scope::Peer => "peer",
        }
    }
}

impl Device {
    /// The id a given public key gets.
    ///
    /// The first 16 characters of the key's own base64url text. Not a hash:
    /// the key is already uniformly random 32 bytes, so hashing it would add
    /// a step without adding entropy, and an id that is a literal prefix of
    /// the key can be checked by eye against a record.
    ///
    /// 16 characters is 96 bits, which is not a collision anybody reaches by
    /// accident. It is not collision-*proof* against an attacker who can
    /// grind keys, and it does not have to be: nothing authenticates on the
    /// id. [`DeviceStore::authenticate`] looks up by the full key.
    pub fn id_for(public_key: &str) -> String {
        public_key.chars().take(16).collect()
    }

    /// Whether this device may connect right now.
    pub fn is_active(&self) -> bool {
        self.revoked_ms.is_none()
    }

    /// The state word a listing shows.
    pub fn state(&self) -> &'static str {
        if self.removal_forgotten() {
            "forgotten"
        } else if self.revoked_ms.is_some() {
            "revoked"
        } else {
            "paired"
        }
    }

    /// Removed, and the removal forgotten here since: refused as a peer, but
    /// it may pair again with a code.
    pub fn removal_forgotten(&self) -> bool {
        matches!((self.revoked_ms, self.forgotten_ms), (Some(r), Some(f)) if f >= r)
    }

    /// Whether a removal at `removed_ms` blocks this record: it is newer than
    /// any forget recorded here.
    fn removal_counts(&self, removed_ms: u64) -> bool {
        self.forgotten_ms.map_or(true, |f| removed_ms > f)
    }

    /// A removed computer whose removal still stands: what is passed on.
    pub fn is_tombstone(&self) -> bool {
        self.revoked_ms.is_some() && !self.removal_forgotten()
    }
}

/// Why a device store operation failed.
#[derive(Debug)]
pub enum StoreError {
    Io(std::io::Error),
    /// The file exists and is not a device store.
    Corrupt(String),
    /// No device with that id or key.
    NoSuchDevice(String),
    /// A key that is not 32 bytes of base64url.
    BadKey(String),
    /// A name that could not be displayed safely.
    BadName(String),
    /// This key is already paired and not revoked.
    AlreadyPaired(String),
    /// This computer was removed, and the removal has not been forgotten.
    Removed(String),
    /// `forget` on a computer that is not removed.
    NotRemoved(String),
    /// A key that may not become a companion key, or may not be paired
    /// because it is one. One message for every reason, so the answer does
    /// not say which other device holds it.
    KeyInUse,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io(e) => write!(f, "{e}"),
            StoreError::Corrupt(w) => write!(f, "the paired-device store is unreadable: {w}"),
            StoreError::NoSuchDevice(d) => write!(f, "no paired device '{d}'"),
            StoreError::BadKey(w) => write!(f, "{w}"),
            StoreError::BadName(w) => write!(f, "{w}"),
            StoreError::AlreadyPaired(d) => write!(
                f,
                "'{d}' is already paired with this machine; revoke it first if you are replacing it"
            ),
            StoreError::Removed(name) => write!(
                f,
                "'{name}' was removed from this computer, so it cannot pair again with the same key until \
                 the person here runs `rime remote computers forget` for it"
            ),
            StoreError::NotRemoved(name) => write!(f, "'{name}' is not removed, so there is nothing to forget"),
            StoreError::KeyInUse => write!(
                f,
                "that key is already known to this machine; generate a new one"
            ),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> StoreError {
        StoreError::Io(e)
    }
}

/// Longest device name kept. It is shown in the Agent Center, on privilege
/// prompts as the `actor`, and in the audit log; the bound is the same one
/// `rime-agentd` puts on an actor, because it becomes one.
pub const MAX_NAME: usize = 64;

/// The paired devices of one user.
///
/// Loaded and saved whole. There are at most a handful of them, the file is
/// under a kilobyte per device, and a partial-write scheme for a file that
/// small would be more failure modes than it removes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceStore {
    /// Keyed by device id, so a listing is ordered and a lookup is direct.
    #[serde(default)]
    pub devices: BTreeMap<String, Device>,
    /// The owner's other computers, keyed the same way. Never in
    /// `devices.json`: [`DeviceStore::load`] and [`DeviceStore::save`] keep
    /// them in [`DeviceStore::computers_path`]; see the module note.
    #[serde(skip)]
    pub computers: BTreeMap<String, Device>,
}

/// What [`DeviceStore::apply_tombstones`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// Whether anything in the store changed, so it needs saving.
    pub changed: bool,
    /// The computers that were paired and are removed now.
    pub removed: Vec<String>,
}

/// What `computers.json` holds.
#[derive(Debug, Default, Serialize, Deserialize)]
struct ComputersFile {
    #[serde(default)]
    computers: BTreeMap<String, Device>,
}

impl DeviceStore {
    /// Where the store lives for this user.
    pub fn path() -> PathBuf {
        Self::path_in(&rime_agent_core::paths::state_home())
    }

    /// Where it lives under an arbitrary state root, for tests and for a
    /// second daemon.
    pub fn path_in(state_home: &Path) -> PathBuf {
        state_home
            .join("rime")
            .join("remote")
            .join("devices.json")
    }

    /// Where the other computers of the store at `path` live: `computers.json`
    /// beside `devices.json` (any other store name gets `<name>.computers`,
    /// so two stores in one directory never share one).
    pub fn computers_path(path: &Path) -> PathBuf {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "devices.json".into());
        if name == "devices.json" {
            path.with_file_name("computers.json")
        } else {
            path.with_file_name(format!("{name}.computers"))
        }
    }

    /// Read the store, or an empty one when there is no file yet: the phones
    /// from `path`, the computers from [`DeviceStore::computers_path`].
    ///
    /// A file that exists and does not parse is an **error**, never an empty
    /// store. Returning empty there would silently unpair every device the
    /// owner has, and the symptom — "my phone stopped working" — points
    /// nowhere near the cause. For the computers it would also forget every
    /// removal, which is the one thing a removal promises not to do.
    pub fn load(path: &Path) -> Result<DeviceStore, StoreError> {
        let mut store: DeviceStore = match read_if_there(path)? {
            Some(text) => serde_json::from_str(&text)
                .map_err(|e| StoreError::Corrupt(format!("{}: {e}", path.display())))?,
            None => DeviceStore::default(),
        };
        let computers = Self::computers_path(path);
        if let Some(text) = read_if_there(&computers)? {
            let file: ComputersFile = serde_json::from_str(&text)
                .map_err(|e| StoreError::Corrupt(format!("{}: {e}", computers.display())))?;
            store.computers = file.computers;
        }
        Ok(store)
    }

    /// Write the store, 0600, in a directory this user alone can enter.
    ///
    /// Written to a temporary file in the same directory and renamed, so a
    /// crash mid-write leaves the previous store rather than half of one.
    /// `rename(2)` within a directory is atomic; writing in place is not, and
    /// the failure it produces is the corrupt-file case above.
    ///
    /// The computers go to [`DeviceStore::computers_path`], first, and only
    /// once there is one (a machine with phones alone keeps the one file).
    /// Computers are never deleted, only tombstoned, so a map that is empty
    /// here never stands for a file that should be emptied.
    pub fn save(&self, path: &Path) -> Result<(), StoreError> {
        if !self.computers.is_empty() {
            let file = ComputersFile {
                computers: self.computers.clone(),
            };
            let text = serde_json::to_string_pretty(&file)
                .map_err(|e| StoreError::Corrupt(format!("serialising the computers: {e}")))?;
            write_private(&Self::computers_path(path), &text)?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| StoreError::Corrupt(format!("serialising the store: {e}")))?;
        write_private(path, &text)
    }

    /// Add a device, or say why it cannot be added.
    pub fn pair(
        &mut self,
        public_key: &str,
        name: &str,
        now_ms: u64,
        requires_user_verification: bool,
    ) -> Result<Device, StoreError> {
        let raw = check_key(public_key)?;
        let name = check_name(name)?;
        let id = Device::id_for(public_key);
        if let Some(existing) = self.devices.get(&id) {
            if existing.is_active() {
                return Err(StoreError::AlreadyPaired(existing.name.clone()));
            }
        }
        // A key that is some device's companion key, revoked or not, or one
        // it ever retired, may not become a device of its own: one key would
        // then open two sessions with two different scopes, and a revoked or
        // leaked companion key would come back as a full one.
        if self.devices.values().any(|d| d.ever_had_companion(&raw)) {
            return Err(StoreError::KeyInUse);
        }
        // Nor may another computer's key, or its id, become a phone.
        if self.computer_holds(&raw) || self.computers.contains_key(&id) {
            return Err(StoreError::KeyInUse);
        }
        // Pairing again after a revoke starts with no companion key, but
        // remembers the old record's, so they stay refused.
        let retired = match self.devices.get_mut(&id) {
            Some(old) => {
                old.retire_companion();
                std::mem::take(&mut old.retired_companion_keys)
            }
            None => Vec::new(),
        };
        let device = Device {
            id: id.clone(),
            name,
            public_key: public_key.to_string(),
            paired_ms: now_ms,
            last_seen_ms: None,
            // A device paired again after being revoked starts clean. The
            // tombstone did its job — it refused the old key until the owner
            // deliberately paired it again — and leaving it set would make the
            // new pairing inert in a way the owner just contradicted.
            revoked_ms: None,
            requires_user_verification,
            last_path: None,
            // Pairing again after a revoke starts with no companion key: the
            // phone mints a new one over its new full session.
            companion_key: None,
            retired_companion_keys: retired,
            addresses: Vec::new(),
            relay: None,
            forgotten_ms: None,
            told_ms: None,
        };
        self.devices.insert(id, device.clone());
        Ok(device)
    }

    /// The device this public key belongs to, if it may connect.
    ///
    /// The only authentication path. It takes the full key rather than an id,
    /// because an id is 16 characters of a 43-character value and matching on
    /// a prefix is how two keys become one device.
    ///
    /// Full keys first. A companion key authenticates only on an active
    /// device, and only when no record at all, revoked ones included, has it
    /// as its full key: `companion_register` and `pair` already refuse that,
    /// and this is the second lock on the same door, so a revoked full key
    /// can never come back as somebody's companion.
    ///
    /// A key that is both a computer's (removed or not) and a phone's key or
    /// companion key, now or ever, authenticates as nothing. Pairing refuses
    /// that in both directions, so it means an older `rime-remoted` paired it
    /// as a phone after a rollback, and neither record can be trusted: a
    /// removed computer must not come back as a phone with everything a phone
    /// may do. The owner revokes the phone and pairs it again.
    pub fn authenticate(&self, public_key: &str) -> Option<(&Device, Scope)> {
        if let Some(raw) = canonical(public_key) {
            if self.computer_holds(&raw)
                && self
                    .devices
                    .values()
                    .any(|d| canonical(&d.public_key).as_ref() == Some(&raw) || d.ever_had_companion(&raw))
            {
                return None;
            }
        }
        if let Some(d) = self
            .devices
            .values()
            .find(|d| d.public_key == public_key && d.is_active())
        {
            return Some((d, Scope::Full));
        }
        if self.devices.values().any(|d| d.public_key == public_key) {
            return None;
        }
        // Another computer: active is a peer, and a removed one is refused
        // here before anything else could match it.
        if let Some(c) = self.computers.values().find(|c| c.public_key == public_key) {
            return if c.is_active() { Some((c, Scope::Peer)) } else { None };
        }
        self.devices
            .values()
            .find(|d| d.is_active() && d.companion_key.as_deref() == Some(public_key))
            .map(|d| (d, Scope::Companion))
    }

    /// Whether `raw` is any computer record's key, removed ones included.
    fn computer_holds(&self, raw: &[u8; 32]) -> bool {
        self.computers.values().any(|c| canonical(&c.public_key).as_ref() == Some(raw))
    }

    /// Add another computer whose key a code pairing just proved, or refresh
    /// the name and addresses of one already paired.
    ///
    /// Refused when the key is a phone's key or companion key (now or ever),
    /// when its id is a phone's, and when that computer was removed and the
    /// removal was not forgotten here (`forget`). `own_key` is this machine's
    /// own public key, refused too (a copied home), so no caller can forget
    /// that check.
    pub fn pair_computer(
        &mut self,
        public_key: &[u8; 32],
        own_key: &[u8; 32],
        name: &str,
        addresses: Vec<String>,
        relay: Option<String>,
        now_ms: u64,
    ) -> Result<Device, StoreError> {
        if public_key == own_key {
            return Err(StoreError::KeyInUse);
        }
        let text = crate::b64_encode(public_key);
        let name = check_name(name)?;
        let id = Device::id_for(&text);
        if self.devices.contains_key(&id)
            || self
                .devices
                .values()
                .any(|d| canonical(&d.public_key).as_ref() == Some(public_key) || d.ever_had_companion(public_key))
        {
            return Err(StoreError::KeyInUse);
        }
        if let Some(existing) = self.computers.get(&id) {
            if existing.public_key != text {
                return Err(StoreError::KeyInUse);
            }
            if !existing.is_active() && !existing.removal_forgotten() {
                return Err(StoreError::Removed(existing.name.clone()));
            }
        }
        // A forgotten removal pairs as new, and keeps when it was forgotten,
        // which later tombstones are compared against.
        let earlier = self.computers.get(&id).filter(|c| c.is_active());
        let paired_ms = earlier.map(|c| c.paired_ms).unwrap_or(now_ms);
        let last_seen_ms = earlier.and_then(|c| c.last_seen_ms);
        let forgotten_ms = self.computers.get(&id).and_then(|c| c.forgotten_ms);
        let computer = Device {
            id: id.clone(),
            name,
            public_key: text,
            paired_ms,
            last_seen_ms,
            revoked_ms: None,
            requires_user_verification: false,
            last_path: None,
            companion_key: None,
            retired_companion_keys: Vec::new(),
            addresses,
            relay,
            forgotten_ms,
            told_ms: None,
        };
        self.computers.insert(id, computer.clone());
        Ok(computer)
    }

    /// Other computers, newest pairing last.
    pub fn list_computers(&self) -> Vec<&Device> {
        let mut v: Vec<&Device> = self.computers.values().collect();
        v.sort_by_key(|d| d.paired_ms);
        v
    }

    /// A computer by id or name, resolved the way a phone is.
    pub fn find_computer(&self, id_or_name: &str) -> Result<&Device, StoreError> {
        let id = resolve_in(&self.computers, id_or_name)?;
        Ok(self.computers.get(&id).expect("resolved"))
    }

    /// Remove another computer: a tombstone, kept for good. Idempotent while
    /// it stands; removing a forgotten one again is a new removal.
    pub fn revoke_computer(&mut self, id_or_name: &str, now_ms: u64) -> Result<Device, StoreError> {
        let id = resolve_in(&self.computers, id_or_name)?;
        let c = self.computers.get_mut(&id).expect("resolved");
        if c.revoked_ms.is_none() || c.removal_forgotten() {
            // Strictly after any forget, so the new removal counts.
            c.revoked_ms = Some(c.forgotten_ms.map_or(now_ms, |f| now_ms.max(f + 1)));
            c.told_ms = None;
        }
        Ok(c.clone())
    }

    /// `rime remote computers forget`: lift a removal here, so the computer
    /// may pair again with a code. It still does not authenticate until it
    /// has. Refused on a computer that is not removed.
    ///
    /// Records the later of now and the removal time, so a member whose
    /// clock runs ahead cannot replay this same removal over the forget.
    pub fn forget_computer(&mut self, id_or_name: &str, now_ms: u64) -> Result<Device, StoreError> {
        let id = resolve_in(&self.computers, id_or_name)?;
        let c = self.computers.get_mut(&id).expect("resolved");
        let Some(removed) = c.revoked_ms else {
            return Err(StoreError::NotRemoved(c.name.clone()));
        };
        let at = now_ms.max(removed).max(c.forgotten_ms.unwrap_or(0));
        c.forgotten_ms = Some(at);
        Ok(c.clone())
    }

    /// The removals this computer passes on: every computer removed and not
    /// forgotten, with the time it was first removed.
    pub fn computer_tombstones(&self) -> Vec<crate::peer::Tombstone> {
        self.computers
            .values()
            .filter(|c| c.is_tombstone())
            .map(|c| crate::peer::Tombstone {
                key: c.public_key.clone(),
                removed_ms: c.revoked_ms.unwrap_or(0),
                name: c.name.clone(),
            })
            .collect()
    }

    /// Record the tombstones another computer passed on. `sender` is the id
    /// of the computer whose handshake carried them, never a key named in the
    /// list. Says whether the store changed (to be saved) and which paired
    /// computers this removed, the sender among them when the list named this
    /// machine's own key (the sender removed this one).
    ///
    /// For each:
    /// * This machine's own key is never recorded. It means the sender
    ///   refuses this computer now, so the sender is removed here, subject
    ///   to the same forget rule as any other removal.
    /// * A key that is a phone's (its key, companion key or one it gave up),
    ///   or whose id is a phone's, is skipped: a peer cannot remove a phone,
    ///   and a computer record on a phone's key would lock that phone out.
    /// * A computer is matched on its full key. An id shared with a
    ///   different key is skipped.
    /// * A removal at or before this computer's forget of it is skipped;
    ///   a newer one removes it again, keeping the original time.
    /// * An unknown key is recorded as a removed computer, so it cannot pair
    ///   here until forgotten.
    pub fn apply_tombstones(
        &mut self,
        sender: &str,
        own_key: &[u8; 32],
        list: &[crate::peer::Tombstone],
        now_ms: u64,
    ) -> Applied {
        let mut out = Applied::default();
        for t in list {
            let Some(raw) = canonical(&t.key) else { continue };
            if raw == *own_key {
                self.remove_on_tombstone(sender, t.removed_ms, &mut out);
                continue;
            }
            let text = crate::b64_encode(&raw);
            let id = Device::id_for(&text);
            if self.devices.contains_key(&id)
                || self
                    .devices
                    .values()
                    .any(|d| canonical(&d.public_key).as_ref() == Some(&raw) || d.ever_had_companion(&raw))
            {
                continue;
            }
            match self.computers.get(&id) {
                Some(c) if canonical(&c.public_key) != Some(raw) => continue,
                Some(_) => self.remove_on_tombstone(&id, t.removed_ms, &mut out),
                None => {
                    out.changed = true;
                    self.computers.insert(
                        id.clone(),
                        Device {
                            id: id.clone(),
                            name: t.name.clone(),
                            public_key: text,
                            paired_ms: now_ms,
                            last_seen_ms: None,
                            revoked_ms: Some(t.removed_ms),
                            requires_user_verification: false,
                            last_path: None,
                            companion_key: None,
                            retired_companion_keys: Vec::new(),
                            addresses: Vec::new(),
                            relay: None,
                            forgotten_ms: None,
                            told_ms: None,
                        },
                    );
                }
            }
        }
        out
    }

    /// Remove computer `id` on a tombstone removed at `removed_ms`, unless a
    /// forget here is as new or newer, or it is already removed.
    fn remove_on_tombstone(&mut self, id: &str, removed_ms: u64, out: &mut Applied) {
        let Some(c) = self.computers.get_mut(id) else { return };
        if c.is_tombstone() || !c.removal_counts(removed_ms) {
            return;
        }
        if c.is_active() {
            out.removed.push(id.to_string());
        }
        c.revoked_ms = Some(removed_ms);
        c.told_ms = None;
        out.changed = true;
    }

    /// A removed computer acknowledged a list naming it.
    pub fn computer_told(&mut self, id: &str, now_ms: u64) {
        if let Some(c) = self.computers.get_mut(id) {
            if c.is_tombstone() && c.told_ms.is_none() {
                c.told_ms = Some(now_ms);
            }
        }
    }

    /// Remember where a computer answered, first in its list, at most
    /// [`crate::computer::MAX_ADDRESSES`] kept.
    pub fn computer_reached_at(&mut self, id: &str, address: &str) {
        if let Some(c) = self.computers.get_mut(id) {
            c.addresses.retain(|a| a != address);
            c.addresses.insert(0, address.to_string());
            c.addresses.truncate(crate::computer::MAX_ADDRESSES);
        }
    }

    /// Give an active device a companion key, replacing any it had.
    ///
    /// Refused when the key is malformed, or is the full key or the
    /// companion key of any record (this device's own full key, another
    /// device's either key, a revoked device's either key). Registering the
    /// key this device already has is not an error. Keys are compared as
    /// bytes, and stored re-encoded, so a second spelling of one key is the
    /// same key.
    pub fn set_companion(&mut self, id: &str, key: &str) -> Result<Device, StoreError> {
        let raw = check_key(key)?;
        match self.devices.get(id) {
            Some(d) if d.is_active() => {}
            _ => return Err(StoreError::NoSuchDevice(id.to_string())),
        }
        let d = &self.devices[id];
        if d.companion_key.as_deref().and_then(canonical).as_ref() == Some(&raw) {
            return Ok(d.clone());
        }
        // Any record's device key, any other record's companion key, and any
        // companion key any record ever retired, this one's included.
        let taken = self.devices.values().any(|d| {
            canonical(&d.public_key).as_ref() == Some(&raw)
                || (d.id != id && d.companion_key.as_deref().and_then(canonical).as_ref() == Some(&raw))
                || d.retired_companion_keys.iter().any(|k| canonical(k).as_ref() == Some(&raw))
        }) || self.computer_holds(&raw);
        if taken {
            return Err(StoreError::KeyInUse);
        }
        let d = self.devices.get_mut(id).expect("checked above");
        d.retire_companion();
        d.companion_key = Some(crate::b64_encode(&raw));
        Ok(d.clone())
    }

    /// Take a device's companion key away, and retire it. `true` when it had
    /// one.
    pub fn forget_companion(&mut self, id: &str) -> bool {
        self.devices
            .get_mut(id)
            .and_then(Device::retire_companion)
            .is_some()
    }

    /// Take a device's access away. Idempotent: revoking twice is not an
    /// error, because the owner's intent is the same both times and a phone
    /// that has been lost twice is still lost.
    pub fn revoke(&mut self, id_or_name: &str, now_ms: u64) -> Result<Device, StoreError> {
        let id = self.resolve(id_or_name)?;
        let d = self.devices.get_mut(&id).expect("resolved");
        if d.revoked_ms.is_none() {
            d.revoked_ms = Some(now_ms);
        }
        Ok(d.clone())
    }

    /// Record a completed handshake.
    pub fn seen(&mut self, id: &str, now_ms: u64, path: &str) {
        if let Some(d) = self.devices.get_mut(id).or_else(|| self.computers.get_mut(id)) {
            d.last_seen_ms = Some(now_ms);
            d.last_path = Some(path.to_string());
        }
    }

    /// A device by id or name, the same way `revoke` resolves one.
    pub fn find(&self, id_or_name: &str) -> Result<&Device, StoreError> {
        let id = self.resolve(id_or_name)?;
        Ok(self.devices.get(&id).expect("resolved"))
    }

    /// Devices, newest pairing last.
    pub fn list(&self) -> Vec<&Device> {
        let mut v: Vec<&Device> = self.devices.values().collect();
        v.sort_by_key(|d| d.paired_ms);
        v
    }

    /// An id or a name to an id.
    ///
    /// A name is accepted because it is what the owner remembers when a phone
    /// is lost, and refused when it is ambiguous rather than resolved to
    /// whichever came first — revoking the wrong device is the failure this
    /// avoids, and it is silent.
    fn resolve(&self, id_or_name: &str) -> Result<String, StoreError> {
        resolve_in(&self.devices, id_or_name)
    }
}

/// An id or a name to an id within one map. See [`DeviceStore::resolve`].
fn resolve_in(map: &BTreeMap<String, Device>, id_or_name: &str) -> Result<String, StoreError> {
    if map.contains_key(id_or_name) {
        return Ok(id_or_name.to_string());
    }
    let matches: Vec<&Device> = map
        .values()
        .filter(|d| d.name == id_or_name)
        .collect();
    match matches.as_slice() {
        [one] => Ok(one.id.clone()),
        [] => Err(StoreError::NoSuchDevice(id_or_name.to_string())),
        many => Err(StoreError::NoSuchDevice(format!(
            "{id_or_name}' names {} devices; use one of the ids: {}",
            many.len(),
            many.iter()
                .map(|d| d.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// A 32-byte key in base64url without padding, or the reason it is not one.
pub fn check_key(key: &str) -> Result<[u8; 32], StoreError> {
    let raw = crate::b64_decode(key)
        .ok_or_else(|| StoreError::BadKey(format!("'{}' is not base64url", key.escape_debug())))?;
    raw.as_slice().try_into().map_err(|_| {
        StoreError::BadKey(format!(
            "a device key is 32 bytes; this one decodes to {}",
            raw.len()
        ))
    })
}

/// A key's 32 bytes, or `None` when it is not a key. For comparing two
/// spellings of one key.
fn canonical(key: &str) -> Option<[u8; 32]> {
    check_key(key).ok()
}

/// A displayable device name, trimmed, or the reason it is not one.
///
/// The same rule `rime-agentd` applies to an actor id, and for the same
/// reason: this name becomes that actor, and it is printed on the prompt a
/// human reads before approving a root operation. A device called
/// `phone\r\nAPPROVED` is a display attack.
pub fn check_name(name: &str) -> Result<String, StoreError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(StoreError::BadName("a device needs a name".into()));
    }
    if name.chars().count() > MAX_NAME {
        return Err(StoreError::BadName(format!(
            "a device name may be at most {MAX_NAME} characters; this one is {}",
            name.chars().count()
        )));
    }
    if name.chars().any(|c| c.is_control()) {
        return Err(StoreError::BadName(
            "a device name may not contain control characters: it is printed on the prompt a \
             human reads before approving a root operation"
                .into(),
        ));
    }
    Ok(name.to_string())
}

/// 0600 on a file that names the owner's devices.
/// A file's text, or `None` when it does not exist.
fn read_if_there(path: &Path) -> Result<Option<String>, StoreError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(StoreError::Io(e)),
    }
}

/// Write `text` to `path`, 0600, in a directory this user alone can enter,
/// through a temporary file renamed over it (see [`DeviceStore::save`]).
fn write_private(path: &Path, text: &str) -> Result<(), StoreError> {
    let dir = path.parent().unwrap_or(Path::new("."));
    rime_agent_core::paths::ensure_private_dir(dir)?;
    let tmp = dir.join(format!(
        ".{}.{}",
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "devices.json".into()),
        std::process::id()
    ));
    std::fs::write(&tmp, text.as_bytes())?;
    set_owner_only(&tmp)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn set_owner_only(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(seed: u8) -> String {
        crate::b64_encode(&[seed; 32])
    }

    fn store_in(dir: &Path) -> PathBuf {
        DeviceStore::path_in(dir)
    }

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn tmp(tag: &str) -> Tmp {
        let p = std::env::temp_dir().join(format!(
            "rime-remote-dev-{}-{tag}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&p).expect("tmp");
        Tmp(p)
    }

    #[test]
    fn a_paired_device_authenticates_and_a_revoked_one_does_not() {
        let mut s = DeviceStore::default();
        let k = key(1);
        let d = s.pair(&k, "pixel-8", 1000, false).expect("pair");
        assert_eq!(d.state(), "paired");
        assert_eq!(s.authenticate(&k).map(|(d, _)| d.id.clone()), Some(d.id.clone()));

        s.revoke(&d.id, 2000).expect("revoke");
        assert!(
            s.authenticate(&k).is_none(),
            "a revoked key still authenticated"
        );
        // And the record is still there to be shown.
        assert_eq!(s.list().len(), 1);
        assert_eq!(s.list()[0].state(), "revoked");
        assert_eq!(s.list()[0].revoked_ms, Some(2000));
    }

    #[test]
    fn revoking_is_idempotent_and_keeps_the_first_time() {
        let mut s = DeviceStore::default();
        let d = s.pair(&key(2), "old-phone", 10, false).expect("pair");
        s.revoke("old-phone", 20).expect("revoke");
        s.revoke(&d.id, 999).expect("revoke again");
        assert_eq!(s.devices[&d.id].revoked_ms, Some(20));
    }

    #[test]
    fn an_ambiguous_name_is_refused_rather_than_resolved() {
        // Revoking the wrong device is silent, and the owner finds out when
        // the phone they still have stops working.
        let mut s = DeviceStore::default();
        s.pair(&key(3), "phone", 1, false).expect("pair");
        s.pair(&key(4), "phone", 2, false).expect("pair");
        let e = s.revoke("phone", 3).expect_err("must not resolve");
        let msg = e.to_string();
        assert!(msg.contains("names 2 devices"), "{msg}");
        assert!(
            s.list().iter().all(|d| d.is_active()),
            "an ambiguous revoke revoked something"
        );
    }

    #[test]
    fn a_live_pairing_is_not_silently_replaced() {
        let mut s = DeviceStore::default();
        s.pair(&key(5), "phone", 1, false).expect("pair");
        let e = s.pair(&key(5), "attacker", 2, false).expect_err("refused");
        assert!(matches!(e, StoreError::AlreadyPaired(_)), "{e}");
        assert_eq!(s.devices.values().next().expect("one").name, "phone");
    }

    #[test]
    fn a_revoked_device_can_be_paired_again_and_starts_clean() {
        let mut s = DeviceStore::default();
        let d = s.pair(&key(6), "phone", 1, false).expect("pair");
        s.revoke(&d.id, 2).expect("revoke");
        let again = s.pair(&key(6), "phone-again", 3, true).expect("re-pair");
        assert!(again.is_active());
        assert_eq!(again.paired_ms, 3);
        assert!(again.requires_user_verification);
        assert!(s.authenticate(&key(6)).is_some());
    }

    #[test]
    fn a_name_that_could_rewrite_a_prompt_is_refused() {
        let mut s = DeviceStore::default();
        for bad in ["", "   ", "a\nb", "a\rb", "a\u{1b}[2Kb", "a\0b"] {
            assert!(
                s.pair(&key(7), bad, 1, false).is_err(),
                "{bad:?} was accepted as a device name"
            );
        }
        assert!(s.pair(&key(7), &"n".repeat(MAX_NAME + 1), 1, false).is_err());
        assert!(s.pair(&key(7), &"n".repeat(MAX_NAME), 1, false).is_ok());
        // Nothing partial was written by the refusals.
        assert_eq!(s.list().len(), 1);
    }

    #[test]
    fn a_key_that_is_not_thirty_two_bytes_is_refused() {
        let mut s = DeviceStore::default();
        for bad in [
            crate::b64_encode(&[0u8; 31]),
            crate::b64_encode(&[0u8; 33]),
            "not base64!!".to_string(),
            String::new(),
        ] {
            assert!(
                s.pair(&bad, "phone", 1, false).is_err(),
                "{bad:?} was accepted as a device key"
            );
        }
    }

    #[test]
    fn the_store_round_trips_through_a_file_that_only_the_owner_can_read() {
        use std::os::unix::fs::PermissionsExt;
        let t = tmp("roundtrip");
        let path = store_in(&t.0);
        let mut s = DeviceStore::default();
        let d = s.pair(&key(8), "pixel-8", 1000, true).expect("pair");
        s.seen(&d.id, 2000, "lan");
        s.save(&path).expect("save");

        let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the device store is readable by others");
        let dir_mode = std::fs::metadata(path.parent().expect("dir"))
            .expect("stat dir")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700, "the device directory is enterable by others");

        let back = DeviceStore::load(&path).expect("load");
        assert_eq!(back.devices, s.devices);
        assert_eq!(back.list()[0].last_path.as_deref(), Some("lan"));
    }

    #[test]
    fn a_paired_device_belongs_to_one_account_and_not_to_the_machine() {
        // P2-016 criterion 4: remote agent permissions are bound to the owning
        // user. A paired phone reaches `rime-agentd` through `rime-remoted`,
        // and both are per-user systemd services — but the thing that actually
        // decides which account a device can drive is where its record lives.
        //
        // It lives under `$XDG_STATE_HOME`, so it is already per-account. That
        // was UNASSERTED: the existing round-trip test checks the file is 0600
        // in a 0700 directory, which is a claim about other accounts not
        // READING it, and says nothing about two accounts not SHARING it. A
        // store that had drifted to /var/lib would still be 0600 and would
        // still pass that test, while every account on the machine paired into
        // one list.
        //
        // Asserted through `path_in`, which `path()` calls, so moving the
        // store to a machine-global constant fails here.
        let mine = Path::new("/home/owner/.local/state");
        let theirs = Path::new("/home/guest/.local/state");
        assert_ne!(DeviceStore::path_in(mine), DeviceStore::path_in(theirs));
        assert!(DeviceStore::path_in(mine).starts_with(mine));
        assert!(DeviceStore::path_in(theirs).starts_with(theirs));

        // The identity key is the other half and has the same requirement: it
        // is what a device authenticates the machine by, so one key shared
        // across accounts would mean a device paired with one account could
        // complete a handshake with another's daemon.
        assert_ne!(
            crate::identity::Identity::path_in(mine),
            crate::identity::Identity::path_in(theirs)
        );
        assert!(crate::identity::Identity::path_in(mine).starts_with(mine));

        // And neither may sit in a machine-global directory, which is the
        // shape the drift would take. Stated as paths rather than as a rule
        // about `state_home()`, because a constant is exactly what would
        // replace the call.
        for p in [DeviceStore::path_in(mine), crate::identity::Identity::path_in(mine)] {
            for global in ["/var", "/etc", "/usr", "/run", "/tmp"] {
                assert!(
                    !p.starts_with(global),
                    "{} is under {global}, so it is the machine's and not one account's",
                    p.display()
                );
            }
        }
    }

    #[test]
    fn the_live_store_paths_are_the_ones_this_account_owns() {
        // `path_in` is only the formula; this ties the live readers to it, so
        // a `path()` that stopped consulting `state_home()` — went back to a
        // constant, or to a shared directory — is caught.
        let state = rime_agent_core::paths::state_home();
        assert_eq!(DeviceStore::path(), DeviceStore::path_in(&state));
        assert_eq!(
            crate::identity::Identity::path(),
            crate::identity::Identity::path_in(&state)
        );
    }

    #[test]
    fn a_missing_store_is_empty_and_a_corrupt_one_is_an_error() {
        let t = tmp("corrupt");
        let path = store_in(&t.0);
        assert!(DeviceStore::load(&path).expect("missing is empty").devices.is_empty());

        std::fs::create_dir_all(path.parent().expect("dir")).expect("mkdir");
        std::fs::write(&path, b"{ this is not json").expect("write");
        let e = DeviceStore::load(&path).expect_err("corrupt must not read as empty");
        assert!(matches!(e, StoreError::Corrupt(_)), "{e}");
    }

    #[test]
    fn the_store_holds_no_secret_key_material() {
        // The property a stolen file has to have. Every value in the
        // serialised store is asserted against the SECRET half of a keypair,
        // not merely against a hardcoded sentinel: the record is built from a
        // real keypair so that a field accidentally carrying the wrong half
        // would be caught.
        let kp = crate::identity::Identity::generate().expect("keypair");
        let mut s = DeviceStore::default();
        s.pair(&kp.public_key(), "pixel-8", 1, false).expect("pair");
        let text = serde_json::to_string(&s).expect("serialise");
        assert!(
            !text.contains(&kp.secret_key_base64()),
            "the device store serialised a secret key"
        );
        assert!(text.contains(&kp.public_key()));
    }

    #[test]
    fn an_id_is_a_prefix_of_the_key_and_never_authenticates_on_its_own() {
        let mut s = DeviceStore::default();
        let k = key(9);
        let d = s.pair(&k, "phone", 1, false).expect("pair");
        assert!(k.starts_with(&d.id));
        assert_eq!(d.id.chars().count(), 16);
        // The id is not a credential: presented as a key it authenticates
        // nothing, and neither does any other prefix of the real key.
        assert!(s.authenticate(&d.id).is_none());
        for n in 1..k.len() {
            assert!(
                s.authenticate(&k[..n]).is_none(),
                "a {n}-character prefix of the key authenticated"
            );
        }
        assert!(s.authenticate(&k).is_some());
    }

    #[test]
    fn a_companion_key_authenticates_its_device_with_the_companion_scope() {
        let mut s = DeviceStore::default();
        let d = s.pair(&key(20), "phone", 1, false).expect("pair");
        assert_eq!(s.authenticate(&key(20)).map(|(_, sc)| sc), Some(Scope::Full));
        assert!(s.authenticate(&key(21)).is_none(), "an unknown key authenticated");

        s.set_companion(&d.id, &key(21)).expect("register");
        let (who, scope) = s.authenticate(&key(21)).expect("the companion key");
        assert_eq!(who.id, d.id);
        assert_eq!(scope, Scope::Companion);
        // The full key keeps its own scope.
        assert_eq!(s.authenticate(&key(20)).map(|(_, sc)| sc), Some(Scope::Full));

        // A new key replaces the old one, which stops working.
        s.set_companion(&d.id, &key(22)).expect("replace");
        assert!(s.authenticate(&key(21)).is_none(), "a replaced companion key still works");
        assert_eq!(s.authenticate(&key(22)).map(|(_, sc)| sc), Some(Scope::Companion));
        // Registering the same key again is not an error.
        s.set_companion(&d.id, &key(22)).expect("idempotent");

        assert!(s.forget_companion(&d.id));
        assert!(!s.forget_companion(&d.id), "forgotten twice");
        assert!(s.authenticate(&key(22)).is_none(), "a forgotten companion key still works");
        assert!(s.authenticate(&key(20)).is_some());
    }

    #[test]
    fn revoking_a_device_refuses_both_its_keys_and_pairing_again_clears_the_companion() {
        let mut s = DeviceStore::default();
        let d = s.pair(&key(23), "phone", 1, false).expect("pair");
        s.set_companion(&d.id, &key(24)).expect("register");
        s.revoke(&d.id, 2).expect("revoke");
        assert!(s.authenticate(&key(23)).is_none());
        assert!(s.authenticate(&key(24)).is_none(), "a revoked device's companion key works");
        // And it cannot be given one while revoked.
        assert!(matches!(s.set_companion(&d.id, &key(25)), Err(StoreError::NoSuchDevice(_))));

        let again = s.pair(&key(23), "phone", 3, false).expect("re-pair");
        assert_eq!(again.companion_key, None);
        assert!(s.authenticate(&key(24)).is_none(), "re-pairing brought the old companion back");
    }

    #[test]
    fn a_companion_key_once_given_up_never_comes_back() {
        let mut s = DeviceStore::default();
        let a = s.pair(&key(70), "a", 1, false).expect("pair");
        let b = s.pair(&key(71), "b", 2, false).expect("pair");
        s.set_companion(&a.id, &key(72)).expect("register");
        s.set_companion(&a.id, &key(73)).expect("replace");
        s.forget_companion(&a.id);
        s.set_companion(&a.id, &key(74)).expect("register again");
        s.revoke(&a.id, 3).expect("revoke");
        let again = s.pair(&key(70), "a", 4, false).expect("re-pair");
        assert_eq!(again.companion_key, None);

        // Replaced (72), forgotten (73), and carried over the re-pair (74):
        // none may be paired as a device of its own, nor registered again,
        // by this device or by another.
        for (k, why) in [(key(72), "replaced"), (key(73), "forgotten"), (key(74), "re-paired over")] {
            assert!(matches!(s.pair(&k, "thief", 5, false), Err(StoreError::KeyInUse)), "{why}: paired");
            for (who, id) in [("a", &a.id), ("b", &b.id)] {
                assert!(
                    matches!(s.set_companion(id, &k), Err(StoreError::KeyInUse)),
                    "{why}: registered again by {who}"
                );
            }
            assert!(s.authenticate(&k).is_none(), "{why}: authenticated");
        }
        // A fresh key still works, and the retired ones survive a save.
        s.set_companion(&a.id, &key(75)).expect("a fresh key");
        let t = tmp("retired");
        let path = store_in(&t.0);
        s.save(&path).expect("save");
        let text = std::fs::read_to_string(&path).expect("read");
        let back = DeviceStore::load(&path).expect("load");
        assert_eq!(back.devices[&a.id].retired_companion_keys.len(), 3, "{text}");
        assert!(matches!(back.clone().pair(&key(73), "thief", 6, false), Err(StoreError::KeyInUse)));
    }

    #[test]
    fn a_companion_key_that_is_any_known_key_is_refused() {
        let mut s = DeviceStore::default();
        let a = s.pair(&key(30), "a", 1, false).expect("pair");
        let b = s.pair(&key(31), "b", 2, false).expect("pair");
        let gone = s.pair(&key(32), "gone", 3, false).expect("pair");
        s.set_companion(&b.id, &key(33)).expect("b's companion");
        s.set_companion(&gone.id, &key(34)).expect("gone's companion");
        s.revoke(&gone.id, 4).expect("revoke");

        for (k, why) in [
            (key(30), "its own full key"),
            (key(31), "another device's full key"),
            (key(33), "another device's companion key"),
            (key(32), "a revoked device's full key"),
            (key(34), "a revoked device's companion key"),
        ] {
            let e = s.set_companion(&a.id, &k).expect_err(why);
            assert!(matches!(e, StoreError::KeyInUse), "{why}: {e}");
            // The same words whoever holds it.
            assert_eq!(e.to_string(), StoreError::KeyInUse.to_string());
        }
        for bad in [crate::b64_encode(&[1u8; 31]), "not base64!!".into(), String::new()] {
            assert!(matches!(s.set_companion(&a.id, &bad), Err(StoreError::BadKey(_))), "{bad:?}");
        }
        assert!(matches!(s.set_companion("nobody", &key(35)), Err(StoreError::NoSuchDevice(_))));
        assert_eq!(s.devices[&a.id].companion_key, None, "a refusal wrote something");
        assert_eq!(s.devices[&b.id].companion_key, Some(key(33)), "a refusal changed b");
    }

    #[test]
    fn a_companion_key_cannot_be_paired_as_a_device_of_its_own() {
        let mut s = DeviceStore::default();
        let a = s.pair(&key(40), "a", 1, false).expect("pair");
        s.set_companion(&a.id, &key(41)).expect("register");
        assert!(matches!(s.pair(&key(41), "thief", 2, false), Err(StoreError::KeyInUse)));
        // Still refused once its device is revoked: a revoked companion key
        // must not come back as a full one.
        s.revoke(&a.id, 3).expect("revoke");
        assert!(matches!(s.pair(&key(41), "thief", 4, false), Err(StoreError::KeyInUse)));
        assert!(s.authenticate(&key(41)).is_none());
    }

    #[test]
    fn a_store_written_before_companion_keys_loads_with_none_and_writes_none_back() {
        let t = tmp("old-store");
        let path = store_in(&t.0);
        std::fs::create_dir_all(path.parent().expect("dir")).expect("mkdir");
        let old = format!(
            r#"{{"devices":{{"{id}":{{"id":"{id}","name":"phone","public_key":"{k}","paired_ms":1}}}}}}"#,
            id = Device::id_for(&key(50)),
            k = key(50)
        );
        std::fs::write(&path, old).expect("write");
        let s = DeviceStore::load(&path).expect("an old store loads");
        let d = s.list()[0].clone();
        assert_eq!(d.companion_key, None);
        assert_eq!(s.authenticate(&key(50)).map(|(_, sc)| sc), Some(Scope::Full));
        s.save(&path).expect("save");
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(!text.contains("companion_key"), "{text}");
    }

    #[test]
    fn a_second_spelling_of_a_known_key_is_the_same_key() {
        // Stored re-encoded, compared as bytes.
        let mut s = DeviceStore::default();
        let a = s.pair(&key(60), "a", 1, false).expect("pair");
        let d = s.set_companion(&a.id, &key(61)).expect("register");
        assert_eq!(d.companion_key, Some(crate::b64_encode(&[61u8; 32])));
    }

    fn raw(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    #[test]
    fn a_computer_authenticates_as_a_peer_and_once_removed_never_comes_back() {
        let mut s = DeviceStore::default();
        let own = raw(99);
        let c = s
            .pair_computer(&raw(7), &own, "desk", vec!["192.168.1.9:7717".into()], None, 10)
            .expect("pair");
        let (d, scope) = s.authenticate(&key(7)).expect("authenticates");
        assert_eq!((d.id.as_str(), scope), (c.id.as_str(), Scope::Peer));
        assert_eq!(Scope::Peer.as_str(), "peer");
        s.revoke_computer("desk", 20).expect("remove");
        assert!(s.authenticate(&key(7)).is_none(), "a removed computer still authenticates");
        // Removing again keeps the first time; pairing again is refused.
        assert_eq!(s.revoke_computer(&c.id, 30).expect("again").revoked_ms, Some(20));
        assert!(matches!(
            s.pair_computer(&raw(7), &own, "desk", vec![], None, 40),
            Err(StoreError::Removed(_))
        ));
        assert!(s.authenticate(&key(7)).is_none(), "a refused re-pair undid the removal");
    }

    fn tomb(seed: u8, removed_ms: u64) -> crate::peer::Tombstone {
        crate::peer::Tombstone { key: key(seed), removed_ms, name: format!("c{seed}") }
    }

    #[test]
    fn a_tombstone_for_this_machines_own_key_is_never_recorded_and_removes_only_the_sender() {
        let own = raw(99);
        let mut s = DeviceStore::default();
        let a = s.pair_computer(&raw(7), &own, "a", vec![], None, 10).expect("a");
        let c = s.pair_computer(&raw(8), &own, "c", vec![], None, 10).expect("c");
        let applied = s.apply_tombstones(&a.id, &own, &[tomb(99, 50)], 60);
        assert_eq!(applied.removed, std::slice::from_ref(&a.id), "the sender that removed this one is removed");
        assert!(applied.changed);
        assert!(s.authenticate(&key(7)).is_none());
        assert_eq!(s.find_computer(&a.id).expect("a").revoked_ms, Some(50), "the time travels unchanged");
        assert!(s.computers.values().all(|d| d.public_key != key(99)), "this machine's own key was recorded");
        assert_eq!(s.authenticate(&key(8)).map(|(_, sc)| sc), Some(Scope::Peer), "another computer was removed");
        // Again: nothing more happens.
        assert_eq!(s.apply_tombstones(&a.id, &own, &[tomb(99, 70)], 80), Applied::default());
        let _ = c;
    }

    #[test]
    fn a_peer_cannot_remove_a_phone() {
        let own = raw(99);
        let mut s = DeviceStore::default();
        let phone = s.pair(&key(1), "phone", 1, false).expect("phone");
        s.set_companion(&phone.id, &key(2)).expect("companion");
        s.set_companion(&phone.id, &key(3)).expect("replaced: 2 retired");
        let a = s.pair_computer(&raw(7), &own, "a", vec![], None, 1).expect("a");
        // A different key whose id is the phone's.
        let mut same_id = [1u8; 32];
        same_id[31] = 0xfe;
        assert_eq!(Device::id_for(&crate::b64_encode(&same_id)), phone.id);
        let list = [tomb(1, 5), tomb(2, 5), tomb(3, 5), crate::peer::Tombstone {
            key: crate::b64_encode(&same_id),
            removed_ms: 5,
            name: "x".into(),
        }];
        assert_eq!(s.apply_tombstones(&a.id, &own, &list, 6), Applied::default());
        assert_eq!(s.list_computers().len(), 1, "a phone's key became a computer record");
        assert_eq!(s.authenticate(&key(1)).map(|(_, sc)| sc), Some(Scope::Full), "the phone was locked out");
        assert_eq!(s.authenticate(&key(3)).map(|(_, sc)| sc), Some(Scope::Companion));
        assert!(s.find(&phone.id).expect("phone").is_active());
    }

    #[test]
    fn a_received_tombstone_removes_a_known_computer_records_an_unknown_one_and_matches_on_the_whole_key() {
        let own = raw(99);
        let mut s = DeviceStore::default();
        let a = s.pair_computer(&raw(7), &own, "a", vec![], None, 1).expect("a");
        let b = s.pair_computer(&raw(8), &own, "b", vec![], None, 1).expect("b");
        // A key sharing b's id but not b's key.
        let mut twin = [8u8; 32];
        twin[31] = 0;
        assert_eq!(Device::id_for(&crate::b64_encode(&twin)), b.id);
        let twin_t = crate::peer::Tombstone { key: crate::b64_encode(&twin), removed_ms: 5, name: "t".into() };
        assert!(!s.apply_tombstones(&a.id, &own, &[twin_t], 6).changed);
        assert!(s.find_computer(&b.id).expect("b").is_active(), "an id prefix removed a computer");

        let applied = s.apply_tombstones(&a.id, &own, &[tomb(8, 5), tomb(9, 4)], 6);
        assert_eq!(applied.removed, std::slice::from_ref(&b.id));
        assert!(s.authenticate(&key(8)).is_none());
        let unknown = s.find_computer(&Device::id_for(&key(9))).expect("recorded").clone();
        assert_eq!((unknown.revoked_ms, unknown.name.as_str()), (Some(4), "c9"));
        assert!(s.authenticate(&key(9)).is_none());
        assert!(matches!(s.pair_computer(&raw(9), &own, "c9", vec![], None, 7), Err(StoreError::Removed(_))));
        // Both are passed on, with their first removal times.
        let mut passed: Vec<(String, u64)> = s.computer_tombstones().into_iter().map(|t| (t.key, t.removed_ms)).collect();
        passed.sort();
        let mut want = vec![(key(8), 5), (key(9), 4)];
        want.sort();
        assert_eq!(passed, want);
    }

    #[test]
    fn forget_lets_a_removed_computer_pair_again_and_only_a_newer_removal_undoes_it() {
        let own = raw(99);
        let mut s = DeviceStore::default();
        let a = s.pair_computer(&raw(7), &own, "a", vec![], None, 1).expect("a");
        let b = s.pair_computer(&raw(8), &own, "b", vec![], None, 1).expect("b");
        assert!(matches!(s.forget_computer(&b.id, 5), Err(StoreError::NotRemoved(_))), "forget on a paired computer");
        s.revoke_computer(&b.id, 100).expect("removed");
        // A forget dated before the removal (a clock that went back) still
        // covers it.
        let f = s.forget_computer(&b.id, 50).expect("forget");
        assert_eq!(f.forgotten_ms, Some(100));
        assert_eq!(f.state(), "forgotten");
        assert!(s.authenticate(&key(8)).is_none(), "forgetting let the key in without a code");
        assert!(s.computer_tombstones().is_empty(), "a forgotten removal is still passed on");
        // The old removal, replayed by a member, does nothing, before and
        // after pairing again.
        assert!(!s.apply_tombstones(&a.id, &own, &[tomb(8, 100)], 150).changed);
        assert_eq!(s.find_computer(&b.id).expect("b").state(), "forgotten");
        let again = s.pair_computer(&raw(8), &own, "b", vec![], None, 200).expect("pairs again with a code");
        assert_eq!((again.paired_ms, again.revoked_ms, again.forgotten_ms), (200, None, Some(100)));
        assert_eq!(s.authenticate(&key(8)).map(|(_, sc)| sc), Some(Scope::Peer));
        assert!(!s.apply_tombstones(&a.id, &own, &[tomb(8, 100), tomb(8, 99)], 250).changed);
        assert_eq!(s.authenticate(&key(8)).map(|(_, sc)| sc), Some(Scope::Peer), "an old removal came back");
        // A removal newer than the forget wins, with its own time.
        let applied = s.apply_tombstones(&a.id, &own, &[tomb(8, 101)], 260);
        assert_eq!(applied.removed, std::slice::from_ref(&b.id));
        assert!(s.authenticate(&key(8)).is_none());
        assert_eq!(s.find_computer(&b.id).expect("b").revoked_ms, Some(101));
        // The same rule for the sender removing this one.
        s.revoke_computer(&a.id, 300).expect("a removed");
        s.forget_computer(&a.id, 400).expect("a forgotten");
        s.pair_computer(&raw(7), &own, "a", vec![], None, 500).expect("a again");
        assert!(!s.apply_tombstones(&a.id, &own, &[tomb(99, 350)], 600).changed, "an old 'you were removed'");
        assert_eq!(s.apply_tombstones(&a.id, &own, &[tomb(99, 401)], 600).removed, std::slice::from_ref(&a.id));
    }

    #[test]
    fn removing_a_forgotten_computer_again_is_a_new_removal() {
        let own = raw(99);
        let mut s = DeviceStore::default();
        let b = s.pair_computer(&raw(8), &own, "b", vec![], None, 1).expect("b");
        s.revoke_computer(&b.id, 10).expect("removed");
        s.forget_computer(&b.id, 20).expect("forgotten");
        // Even with a clock behind the forget, the new removal is after it.
        let again = s.revoke_computer(&b.id, 15).expect("removed again");
        assert!(again.revoked_ms.expect("removed") > 20 && again.is_tombstone(), "{again:?}");
        assert!(matches!(s.pair_computer(&raw(8), &own, "b", vec![], None, 30), Err(StoreError::Removed(_))));
    }

    #[test]
    fn a_tombstone_is_told_once_and_a_store_written_before_forget_loads() {
        let own = raw(99);
        let mut s = DeviceStore::default();
        let b = s.pair_computer(&raw(8), &own, "b", vec![], None, 1).expect("b");
        s.computer_told(&b.id, 5);
        assert_eq!(s.find_computer(&b.id).expect("b").told_ms, None, "a paired computer was told it was removed");
        s.revoke_computer(&b.id, 10).expect("removed");
        s.computer_told(&b.id, 11);
        s.computer_told(&b.id, 12);
        assert_eq!(s.find_computer(&b.id).expect("b").told_ms, Some(11));
        let old = r#"{"id":"x","name":"n","public_key":"k","paired_ms":1,"revoked_ms":2}"#;
        let d: Device = serde_json::from_str(old).expect("old record");
        assert_eq!((d.forgotten_ms, d.told_ms), (None, None));
        let text = serde_json::to_string(&s.find_computer(&b.id).expect("b").clone()).expect("json");
        assert!(text.contains("told_ms") && !text.contains("forgotten_ms"), "{text}");
    }

    #[test]
    fn a_computer_is_never_a_phone_and_never_this_machine() {
        let own = raw(99);
        let mut s = DeviceStore::default();
        s.pair(&key(1), "phone", 1, false).expect("phone");
        s.set_companion(&Device::id_for(&key(1)), &key(2)).expect("companion");
        s.set_companion(&Device::id_for(&key(1)), &key(3)).expect("replace (2 retired)");
        for k in [1u8, 2, 3] {
            assert!(
                matches!(s.pair_computer(&raw(k), &own, "x", vec![], None, 2), Err(StoreError::KeyInUse)),
                "phone key {k} became a computer"
            );
        }
        assert!(matches!(s.pair_computer(&own, &own, "me", vec![], None, 2), Err(StoreError::KeyInUse)));
        s.pair_computer(&raw(7), &own, "desk", vec![], None, 3).expect("computer");
        assert!(matches!(s.pair(&key(7), "x", 4, false), Err(StoreError::KeyInUse)));
        assert!(matches!(
            s.set_companion(&Device::id_for(&key(1)), &key(7)),
            Err(StoreError::KeyInUse)
        ));
        s.revoke_computer("desk", 5).expect("remove");
        assert!(matches!(s.pair(&key(7), "x", 6, false), Err(StoreError::KeyInUse)), "a removed computer came back as a phone");
        // Phones are unaffected by the computers, and the other way round.
        assert_eq!(s.list().len(), 1);
        assert_eq!(s.list_computers().len(), 1);
        assert!(s.find("desk").is_err());
        assert!(s.find_computer("phone").is_err());
    }

    #[test]
    fn pairing_a_paired_computer_again_refreshes_its_name_and_addresses() {
        let own = raw(99);
        let mut s = DeviceStore::default();
        s.pair_computer(&raw(7), &own, "desk", vec!["10.0.0.2:7717".into()], None, 10).expect("pair");
        let c = s
            .pair_computer(&raw(7), &own, "desk 2", vec!["10.0.0.3:7717".into()], Some("wss://r.example".into()), 50)
            .expect("again");
        assert_eq!((c.name.as_str(), c.paired_ms), ("desk 2", 10));
        assert_eq!(c.addresses, ["10.0.0.3:7717"]);
        s.computer_reached_at(&c.id, "10.0.0.4:7717");
        s.computer_reached_at(&c.id, "10.0.0.3:7717");
        assert_eq!(s.find_computer(&c.id).expect("c").addresses, ["10.0.0.3:7717", "10.0.0.4:7717"]);
    }

    #[test]
    fn computers_and_their_tombstones_survive_an_older_daemon_rewriting_devices_json() {
        use std::os::unix::fs::PermissionsExt;
        let own = raw(99);
        let dir = tmp("computers");
        let path = store_in(&dir.0);
        let computers = DeviceStore::computers_path(&path);
        assert_eq!(computers, dir.0.join("rime").join("remote").join("computers.json"));
        let mut s = DeviceStore::default();
        s.pair(&key(1), "phone", 1, false).expect("phone");
        s.save(&path).expect("save");
        assert!(!computers.exists(), "a machine with phones alone got a computers file");
        let c = s.pair_computer(&raw(7), &own, "desk", vec![], None, 2).expect("computer");
        let gone = s.pair_computer(&raw(8), &own, "old desk", vec![], None, 3).expect("computer");
        s.revoke_computer(&gone.id, 4).expect("removed");
        s.save(&path).expect("save");
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(!text.contains("computers"), "devices.json carries computers: {text}");
        assert!(!text.contains(&c.id) && !text.contains(&gone.id), "{text}");
        assert!(!text.contains("addresses"), "{text}");
        let mode = std::fs::metadata(&computers).expect("computers.json").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        // An older rime-remoted, which knows only `devices`, rewrites
        // devices.json (as it does on every handshake) and never opens
        // computers.json.
        #[derive(Serialize, Deserialize)]
        struct Older {
            #[serde(default)]
            devices: BTreeMap<String, serde_json::Value>,
        }
        let older: Older = serde_json::from_str(&text).expect("older shape");
        assert_eq!(older.devices.len(), 1);
        std::fs::write(&path, serde_json::to_string_pretty(&older).expect("older json")).expect("older write");

        let back = DeviceStore::load(&path).expect("load");
        assert_eq!(back.authenticate(&key(7)).map(|(_, s)| s), Some(Scope::Peer));
        assert!(back.authenticate(&key(8)).is_none(), "a removed computer came back");
        let mut back = back;
        assert!(
            matches!(back.pair_computer(&raw(8), &own, "old desk", vec![], None, 5), Err(StoreError::Removed(_))),
            "the tombstone was lost"
        );
        assert!(
            matches!(back.pair(&key(8), "phone", 5, false), Err(StoreError::KeyInUse)),
            "a removed computer's key became a phone"
        );

        // A computers.json that does not parse is an error, not no computers.
        std::fs::write(&computers, "{").expect("corrupt");
        assert!(matches!(DeviceStore::load(&path), Err(StoreError::Corrupt(_))));
    }

    #[test]
    fn a_key_that_an_older_daemon_paired_as_a_phone_after_its_computer_was_removed_is_refused() {
        let own = raw(99);
        let mut s = DeviceStore::default();
        let c = s.pair_computer(&raw(7), &own, "desk", vec![], None, 1).expect("computer");
        s.revoke_computer(&c.id, 2).expect("removed");
        // What an older daemon could leave: the same key as a phone, written
        // into `devices` without asking about computers it cannot see.
        let mut phone = DeviceStore::default();
        let d = phone.pair(&key(7), "phone", 3, false).expect("phone");
        s.devices.insert(d.id.clone(), d);
        assert!(s.authenticate(&key(7)).is_none(), "a removed computer's key came back as a phone");
        // Active computer, same story: neither record is trusted.
        let mut s = DeviceStore::default();
        s.pair_computer(&raw(9), &own, "desk", vec![], None, 1).expect("computer");
        let mut phone = DeviceStore::default();
        let d = phone.pair(&key(9), "phone", 3, false).expect("phone");
        s.devices.insert(d.id.clone(), d);
        assert!(s.authenticate(&key(9)).is_none());
        // And a phone whose companion key is a computer's.
        let mut s = DeviceStore::default();
        s.pair_computer(&raw(5), &own, "desk", vec![], None, 1).expect("computer");
        let mut phone = DeviceStore::default();
        let mut d = phone.pair(&key(1), "phone", 3, false).expect("phone");
        d.companion_key = Some(key(5));
        s.devices.insert(d.id.clone(), d);
        assert!(s.authenticate(&key(5)).is_none());
        assert_eq!(s.authenticate(&key(1)).map(|(_, s)| s), Some(Scope::Full), "the phone's own key still works");
    }

}
