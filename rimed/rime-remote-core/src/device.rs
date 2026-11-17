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
        if self.revoked_ms.is_some() {
            "revoked"
        } else {
            "paired"
        }
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

    /// Read the store, or an empty one when there is no file yet.
    ///
    /// A file that exists and does not parse is an **error**, never an empty
    /// store. Returning empty there would silently unpair every device the
    /// owner has, and the symptom — "my phone stopped working" — points
    /// nowhere near the cause.
    pub fn load(path: &Path) -> Result<DeviceStore, StoreError> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| StoreError::Corrupt(format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DeviceStore::default()),
            Err(e) => Err(StoreError::Io(e)),
        }
    }

    /// Write the store, 0600, in a directory this user alone can enter.
    ///
    /// Written to a temporary file in the same directory and renamed, so a
    /// crash mid-write leaves the previous store rather than half of one.
    /// `rename(2)` within a directory is atomic; writing in place is not, and
    /// the failure it produces is the corrupt-file case above.
    pub fn save(&self, path: &Path) -> Result<(), StoreError> {
        let dir = path.parent().unwrap_or(Path::new("."));
        rime_agent_core::paths::ensure_private_dir(dir)?;
        let tmp = dir.join(format!(
            ".{}.{}",
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "devices.json".into()),
            std::process::id()
        ));
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| StoreError::Corrupt(format!("serialising the store: {e}")))?;
        std::fs::write(&tmp, text.as_bytes())?;
        set_owner_only(&tmp)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Add a device, or say why it cannot be added.
    pub fn pair(
        &mut self,
        public_key: &str,
        name: &str,
        now_ms: u64,
        requires_user_verification: bool,
    ) -> Result<Device, StoreError> {
        check_key(public_key)?;
        let name = check_name(name)?;
        let id = Device::id_for(public_key);
        if let Some(existing) = self.devices.get(&id) {
            if existing.is_active() {
                return Err(StoreError::AlreadyPaired(existing.name.clone()));
            }
        }
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
        };
        self.devices.insert(id, device.clone());
        Ok(device)
    }

    /// The device this public key belongs to, if it may connect.
    ///
    /// The only authentication path. It takes the full key rather than an id,
    /// because an id is 16 characters of a 43-character value and matching on
    /// a prefix is how two keys become one device.
    pub fn authenticate(&self, public_key: &str) -> Option<&Device> {
        self.devices
            .values()
            .find(|d| d.public_key == public_key && d.is_active())
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
        if let Some(d) = self.devices.get_mut(id) {
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
        if self.devices.contains_key(id_or_name) {
            return Ok(id_or_name.to_string());
        }
        let matches: Vec<&Device> = self
            .devices
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
        assert_eq!(s.authenticate(&k).map(|d| d.id.clone()), Some(d.id.clone()));

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
}
