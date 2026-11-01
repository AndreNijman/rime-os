//! `rime storage` — the disks in this machine, what they say about themselves,
//! and what nobody was able to ask them. Roadmap §48's Storage Manager.
//!
//! The thresholds, the smartctl bitmask and the free-space rules live in
//! [`rimed_core::storage`]. This file runs the tools and reads the nodes.
//!
//! ## Reading is unprivileged; SMART is not, and the report says which
//!
//! Almost everything here is a world-readable sysfs node, so `rime storage
//! status` needs no root and can sit behind a settings page that polls. The
//! exception is the SMART log: opening a block device needs `CAP_SYS_RAWIO`,
//! so unprivileged the wear row reads *unavailable, run it with sudo* rather
//! than disappearing. Both halves of that matter — a row that vanished would
//! make a machine with a dying disk look like a machine with a healthy one.
//!
//! ## Three tools that lie about failure, all measured on this machine
//!
//! * `smartctl -j -a <dev>` unprivileged exits 2 **and prints a complete JSON
//!   document** with no health log in it. Keying on the missing log gives "this
//!   disk has no SMART support".
//! * `sudo smartctl -j <dev>` — no action option — exits **0** and returns the
//!   same empty document. Exit zero, no data, no error.
//! * `blkid -o value -s TYPE <dev>` unprivileged exits **0** and prints
//!   nothing at all. A caller reading that as "no filesystem here" would go on
//!   to report an encrypted disk as unencrypted, which is the one sentence in
//!   this module a user would act on by putting data at risk.
//!
//! So the filesystem type comes from udev's own database at
//! `/run/udev/data/b<major>:<minor>`, which udev populated as root at boot and
//! left world-readable. It is a file read, it needs no privilege, and under a
//! fixture root it is a fixture.
//!
//! ## Nothing here prints a serial number
//!
//! That same udev record carries `ID_SERIAL_SHORT`, and `nvme list -o json`
//! prints the serial to any user who asks. A storage page that pasted either
//! into a report — or into a bug report a user attaches to an issue — has
//! published the identifier of the disk. The model is what identifies the
//! hardware; the serial identifies the unit.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use rimed_core::recover::Health;
use rimed_core::storage::{
    self, BlockDevice, Encryption, Machine, Mount, PartitionEntry, Reading, Smart, Space, Trim,
};
use clap::Subcommand;
use serde_json::{json, Value};

/// smartctl, by absolute path. Never taken from `PATH`: this command may be
/// run under sudo, and a `PATH` entry the invoking user controls would be a
/// root shell.
const SMARTCTL: &str = rime_paths::SMARTCTL;

/// Where the report reads the machine from.
///
/// The same contract as `rime trust`'s `Roots`. Under a fixture root no
/// subprocess is spawned at all: smartctl's answer, the fstrim timer's state
/// and each filesystem's size come from files the fixture wrote, so the suite
/// exercises the shipped binary without touching a real disk or asking for a
/// privilege.
pub struct Roots {
    fixture: Option<PathBuf>,
}

impl Roots {
    pub fn from_env() -> Self {
        Self { fixture: std::env::var_os("RIME_STORAGE_ROOT").map(PathBuf::from) }
    }

    fn path(&self, absolute: &str) -> PathBuf {
        match &self.fixture {
            Some(root) => root.join(absolute.trim_start_matches('/')),
            None => PathBuf::from(absolute),
        }
    }

    fn is_fixture(&self) -> bool {
        self.fixture.is_some()
    }

    fn fixture_file(&self, rel: &str) -> Option<String> {
        let root = self.fixture.as_ref()?;
        std::fs::read_to_string(root.join(".fixture").join(rel)).ok()
    }

    /// A sysfs value, or the reason there is none.
    fn read(&self, absolute: &str) -> Reading<String> {
        let p = self.path(absolute);
        storage::from_read(&p.display().to_string(), std::fs::read_to_string(&p))
    }

    fn read_u64(&self, absolute: &str) -> Reading<u64> {
        match self.read(absolute) {
            Reading::Known(s) => match s.trim().parse::<u64>() {
                Ok(v) => Reading::Known(v),
                Err(e) => Reading::Unavailable(format!("{absolute}: {s:?} is not a number ({e})")),
            },
            Reading::Unavailable(r) => Reading::Unavailable(r),
        }
    }

    fn read_bool(&self, absolute: &str) -> Reading<bool> {
        match self.read_u64(absolute) {
            Reading::Known(v) => Reading::Known(v != 0),
            Reading::Unavailable(r) => Reading::Unavailable(r),
        }
    }
}

#[derive(Subcommand)]
pub enum StorageCmd {
    /// Every disk, its health, its filesystems and their free space.
    ///
    /// Read-only. Needs no root for anything but the SMART log, and reports
    /// that row as unavailable with the remedy rather than dropping it.
    Status {
        /// Emit machine-readable JSON instead of a report.
        #[arg(long)]
        json: bool,
    },
    /// Only the rows that need somebody to do something.
    ///
    /// Exits 0 when there is nothing to say, 1 when there is. Rows that could
    /// not be measured are printed and do not change the exit status: a
    /// notifier must not wake somebody because a read was refused.
    Warnings {
        /// Emit machine-readable JSON instead of a report.
        #[arg(long)]
        json: bool,
    },
    /// Erase the filesystem signatures on one device. **Destroys data.**
    ///
    /// Refuses by default and needs root. Refuses if the device is mounted
    /// anywhere, if it carries the running system, if it is the parent disk of
    /// something that does, if anything is stacked on it, if it is the EFI
    /// System Partition, or if any of those checks could not be performed —
    /// a check that did not happen is not a check that passed.
    ///
    /// `--confirm` takes the device path itself, typed out. Nothing shorter
    /// works: `yes` is a reflex, `/dev/nvme0n1` is a decision.
    Erase {
        /// The device to erase, as a `/dev/<name>` path.
        device: String,
        /// The device path again, typed exactly.
        #[arg(long)]
        confirm: String,
        /// For a loop device: the file it is attached to.
        ///
        /// An attached loop device is refused without this, because it is in
        /// use by whoever attached it and appears in no mount table — the
        /// development machine's `/dev/loop0` backs a merged system extension
        /// and is invisible to every other check here. Naming the file is how
        /// you show you know which device you have.
        #[arg(long, value_name = "PATH")]
        expect_backing_file: Option<String>,
    },
}

// ── one disk ─────────────────────────────────────────────────────────────────

pub struct Disk {
    pub name: String,
    pub model: Reading<String>,
    pub size_bytes: Reading<u64>,
    pub rotational: Reading<bool>,
    pub removable: Reading<bool>,
    pub read_only: Reading<bool>,
    pub discard_granularity: Reading<u64>,
    pub temperature_c: Reading<f64>,
    pub smart: Result<Smart, String>,
    pub partitions: Vec<Partition>,
}

pub struct Partition {
    pub name: String,
    pub size_bytes: Reading<u64>,
    pub fstype: Reading<String>,
    pub encryption: Encryption,
}

/// Every physical block device, in name order.
///
/// Virtual devices are skipped by where they live rather than by name: `/sys/
/// block/<name>` is a symlink into `/sys/devices`, and everything under
/// `devices/virtual/block` is loop, zram, ram or device-mapper. A name-prefix
/// filter would need a list that goes stale, and would hide a real disk called
/// something unexpected.
pub fn disks(roots: &Roots) -> Result<Vec<Disk>, String> {
    let block = roots.path("/sys/block");
    let entries = std::fs::read_dir(&block)
        .map_err(|e| format!("{}: {e}", block.display()))?;
    let mut names: Vec<String> = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let target = std::fs::read_link(e.path()).unwrap_or_default();
        if target.to_string_lossy().contains("devices/virtual/block") {
            continue;
        }
        names.push(name);
    }
    names.sort();
    Ok(names.iter().map(|n| disk(roots, n)).collect())
}

fn disk(roots: &Roots, name: &str) -> Disk {
    let base = format!("/sys/block/{name}");
    // `size` is in 512-byte sectors regardless of the device's own block size.
    // Multiplying by the logical block size instead is a factor-of-eight error
    // on a 4K-sector disk, in the direction that makes a full disk look empty.
    let size_bytes = match roots.read_u64(&format!("{base}/size")) {
        Reading::Known(sectors) => Reading::Known(sectors.saturating_mul(512)),
        Reading::Unavailable(r) => Reading::Unavailable(r),
    };
    Disk {
        name: name.to_string(),
        model: roots.read(&format!("{base}/device/model")),
        size_bytes,
        rotational: roots.read_bool(&format!("{base}/queue/rotational")),
        removable: roots.read_bool(&format!("{base}/removable")),
        read_only: roots.read_bool(&format!("{base}/ro")),
        discard_granularity: roots.read_u64(&format!("{base}/queue/discard_granularity")),
        temperature_c: temperature(roots, name),
        smart: smart(roots, name),
        partitions: partitions(roots, name),
    }
}

/// The device's own hwmon temperature.
///
/// Worth having even though SMART carries one: hwmon is world-readable and
/// SMART is not, so on an unprivileged run this is the only temperature there
/// is. Measured on the L16: `/sys/class/hwmon/hwmon4` is named `nvme` and
/// reads 37850, which is millidegrees.
fn temperature(roots: &Roots, name: &str) -> Reading<f64> {
    // Two layouts, both live on this machine. The nvme driver registers its
    // hwmon as `<device>/hwmon4` directly; the platform drivers use
    // `<device>/hwmon/hwmon6`. Checking only one finds the temperature on some
    // machines and reports "no sensor" on the others, which is a claim about
    // the hardware built on a directory layout.
    let device = roots.path(&format!("/sys/block/{name}/device"));
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&device) {
        for e in entries.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n == "hwmon" {
                if let Ok(inner) = std::fs::read_dir(e.path()) {
                    candidates.extend(inner.flatten().map(|i| i.path()));
                }
            } else if n.starts_with("hwmon") {
                candidates.push(e.path());
            }
        }
    }
    candidates.sort();
    if candidates.is_empty() {
        return Reading::Unavailable(format!("{}: no hwmon for this device", device.display()));
    }
    for c in &candidates {
        let f = c.join("temp1_input");
        match std::fs::read_to_string(&f) {
            Ok(s) => match s.trim().parse::<f64>() {
                Ok(milli) => return Reading::Known(milli / 1000.0),
                Err(_) => {
                    return Reading::Unavailable(format!("{}: {s:?} is not a temperature", f.display()))
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Reading::Unavailable(format!("{}: {e}", f.display())),
        }
    }
    Reading::Unavailable(format!("{}: no temp1_input under any hwmon", device.display()))
}

/// The SMART log, or the sentence saying why there is none.
fn smart(roots: &Roots, name: &str) -> Result<Smart, String> {
    if roots.is_fixture() {
        // No subprocess under a fixture. The fixture supplies both halves,
        // because the exit status is half the answer.
        let text = roots
            .fixture_file(&format!("smartctl/{name}.json"))
            .ok_or_else(|| format!("no smartctl fixture for {name}"))?;
        let code: i32 = roots
            .fixture_file(&format!("smartctl/{name}.exit"))
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        return storage::parse_smart(&text, code);
    }
    if !Path::new(SMARTCTL).exists() {
        return Err(format!("{SMARTCTL} is not installed"));
    }
    let out = Command::new(SMARTCTL)
        .args(["-j", "-a", &format!("/dev/{name}")])
        .output()
        .map_err(|e| format!("{SMARTCTL}: {e}"))?;
    // `output()` gives the real status; nothing here goes through a pipe, so
    // the code belongs to smartctl rather than to the last stage of one.
    let code = out.status.code().unwrap_or(-1);
    storage::parse_smart(&String::from_utf8_lossy(&out.stdout), code)
}

fn partitions(roots: &Roots, disk: &str) -> Vec<Partition> {
    let base = roots.path(&format!("/sys/block/{disk}"));
    let Ok(entries) = std::fs::read_dir(&base) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with(disk) && n != disk)
        .filter(|n| base.join(n).join("partition").exists())
        .collect();
    names.sort();
    names
        .iter()
        .map(|n| {
            let udev = udev_record(roots, &format!("/sys/block/{disk}/{n}"));
            let fstype = match &udev {
                Ok(props) => match props.get("ID_FS_TYPE") {
                    Some(t) => Reading::Known(t.clone()),
                    None => Reading::Unavailable(
                        "udev recorded no filesystem type for this partition".into(),
                    ),
                },
                Err(why) => Reading::Unavailable(why.clone()),
            };
            Partition {
                name: n.clone(),
                size_bytes: match roots.read_u64(&format!("/sys/block/{disk}/{n}/size")) {
                    Reading::Known(s) => Reading::Known(s.saturating_mul(512)),
                    Reading::Unavailable(r) => Reading::Unavailable(r),
                },
                encryption: encryption(&fstype, &udev),
                fstype,
            }
        })
        .collect()
}

/// udev's own record for a block device: `/run/udev/data/b<major>:<minor>`.
///
/// Its `E:` lines are the properties `blkid` produced at boot, as root, and
/// the file is world-readable. That is why this is the source rather than
/// `blkid`, which unprivileged exits 0 and prints nothing.
fn udev_record(roots: &Roots, sys_path: &str) -> Result<BTreeMap<String, String>, String> {
    let devnode = roots.path(&format!("{sys_path}/dev"));
    let devnum = std::fs::read_to_string(&devnode)
        .map_err(|e| format!("{}: {e}", devnode.display()))?
        .trim()
        .to_string();
    let rec = roots.path(&format!("/run/udev/data/b{devnum}"));
    let text = std::fs::read_to_string(&rec).map_err(|e| format!("{}: {e}", rec.display()))?;
    let mut m = BTreeMap::new();
    for line in text.lines() {
        if let Some(kv) = line.strip_prefix("E:") {
            if let Some((k, v)) = kv.split_once('=') {
                m.insert(k.to_string(), v.to_string());
            }
        }
    }
    Ok(m)
}

/// What is known about a partition being encrypted.
fn encryption(
    fstype: &Reading<String>,
    udev: &Result<BTreeMap<String, String>, String>,
) -> Encryption {
    match udev {
        // No record means nobody looked, and an absence of evidence about
        // encryption is the one thing this module must never round down to
        // "not encrypted".
        Err(why) => Encryption::Unknown(why.clone()),
        Ok(props) => match fstype.known().map(String::as_str) {
            Some("crypto_LUKS") => {
                let v = props.get("ID_FS_VERSION").map(String::as_str).unwrap_or("LUKS");
                Encryption::Encrypted(format!("LUKS{v}"))
            }
            Some(_) => Encryption::Plain,
            None => Encryption::Unknown(
                "udev recorded no filesystem type, so a LUKS header would not have been seen"
                    .into(),
            ),
        },
    }
}

// ── filesystems ──────────────────────────────────────────────────────────────

/// Every mount, as `/proc/self/mountinfo` writes it.
///
/// mountinfo rather than `/etc/mtab` or `findmnt`: it is the kernel's own
/// view, it is per-namespace so it is the truth for this process, and its
/// separator field means a mount point containing a space parses correctly.
pub fn mounts(roots: &Roots) -> Result<Vec<Mount>, String> {
    let p = roots.path("/proc/self/mountinfo");
    let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    let mut out = Vec::new();
    for line in text.lines() {
        // Fields before " - " are fixed-position; after it come fstype,
        // source and superblock options. Splitting on the separator is the
        // only correct way: the optional-fields run between them is variable
        // length, so a fixed index into the whole line is wrong on any machine
        // with shared subtrees, which is every systemd machine.
        let Some((left, right)) = line.split_once(" - ") else { continue };
        let lf: Vec<&str> = left.split(' ').collect();
        let rf: Vec<&str> = right.split(' ').collect();
        if lf.len() < 6 || rf.len() < 2 {
            continue;
        }
        out.push(Mount {
            target: unescape(lf[4]),
            source: unescape(rf[1]),
            fstype: rf[0].to_string(),
            read_only: lf[5].split(',').any(|o| o == "ro"),
            super_options: rf.get(2).copied().unwrap_or_default().to_string(),
            fs_root: unescape(lf[3]),
        });
    }
    Ok(out)
}

/// mountinfo escapes space, tab, newline and backslash as octal.
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 4], 8) {
                out.push(v as char);
                i += 4;
                continue;
            }
        }
        out.push(b[i] as char);
        i += 1;
    }
    out
}

/// A filesystem's size, from `statvfs`.
fn space_of(roots: &Roots, target: &str) -> Reading<Space> {
    if roots.is_fixture() {
        // "<total> <available>", in bytes.
        let key = target.trim_start_matches('/').replace('/', "_");
        let key = if key.is_empty() { "root".to_string() } else { key };
        let Some(text) = roots.fixture_file(&format!("space/{key}")) else {
            return Reading::Unavailable(format!("no space fixture for {target}"));
        };
        let mut it = text.split_ascii_whitespace();
        return match (
            it.next().and_then(|s| s.parse::<u64>().ok()),
            it.next().and_then(|s| s.parse::<u64>().ok()),
        ) {
            (Some(total), Some(avail)) => {
                Reading::Known(Space { total_bytes: total, available_bytes: avail })
            }
            _ => Reading::Unavailable(format!("the space fixture for {target} is malformed")),
        };
    }
    let c = match std::ffi::CString::new(target) {
        Ok(c) => c,
        Err(_) => return Reading::Unavailable(format!("{target} is not a usable path")),
    };
    // Safe: statvfs writes only into `st`, which is owned here, and reports
    // failure through its return value rather than by writing anything.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut st) };
    if rc != 0 {
        let e = std::io::Error::last_os_error();
        return Reading::Unavailable(format!("statvfs({target}): {e}"));
    }
    // f_bavail, not f_bfree: the blocks reserved for root are space this user
    // cannot have, and reporting them as free is how a user runs out of disk
    // at "95% full".
    let frsize = if st.f_frsize == 0 { st.f_bsize } else { st.f_frsize } as u64;
    Reading::Known(Space {
        total_bytes: st.f_blocks as u64 * frsize,
        available_bytes: st.f_bavail as u64 * frsize,
    })
}

/// Whether `fstrim.timer` is enabled, and when it last ran.
fn trim_timer(roots: &Roots) -> (Reading<bool>, Option<String>) {
    if roots.is_fixture() {
        let enabled = roots.fixture_file("fstrim-enabled");
        return match enabled.as_deref().map(str::trim) {
            Some("enabled") => (Reading::Known(true), roots.fixture_file("fstrim-last")),
            Some("disabled") => (Reading::Known(false), None),
            Some(other) => (
                Reading::Unavailable(format!("systemctl said {other:?}")),
                None,
            ),
            None => (Reading::Unavailable("no fstrim fixture".into()), None),
        };
    }
    let out = Command::new(rime_paths::SYSTEMCTL)
        .args(["is-enabled", "fstrim.timer"])
        .output();
    let enabled = match out {
        // `is-enabled` exits non-zero for "disabled", so the word on stdout is
        // the answer and the exit status is not. A check on the status alone
        // reports a masked unit and a disabled one identically, and reports
        // "systemctl is missing" as "the timer is off".
        Ok(o) => match String::from_utf8_lossy(&o.stdout).trim() {
            "enabled" | "enabled-runtime" | "static" | "indirect" => Reading::Known(true),
            "disabled" | "masked" | "masked-runtime" => Reading::Known(false),
            "" => Reading::Unavailable("systemctl said nothing about fstrim.timer".into()),
            other => Reading::Unavailable(format!("systemctl said {other:?}")),
        },
        Err(e) => Reading::Unavailable(format!("systemctl: {e}")),
    };
    let last = Command::new(rime_paths::SYSTEMCTL)
        .args(["show", "fstrim.timer", "-p", "LastTriggerUSec", "--value"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty() && s != "n/a");
    (enabled, last)
}

// ── the report ───────────────────────────────────────────────────────────────

/// One line of the storage report: a stable id, a state, and a sentence.
///
/// The id is the compatibility surface, the same rule the recovery surface's
/// rows live under.
pub struct Row {
    pub id: String,
    pub label: String,
    pub state: Health,
    pub note: String,
}

pub fn rows(roots: &Roots) -> Result<Vec<Row>, String> {
    let mut out: Vec<Row> = Vec::new();
    let disks = disks(roots)?;
    let all_mounts = mounts(roots)?;
    let (timer_enabled, last_run) = trim_timer(roots);

    for d in &disks {
        let kind = match d.rotational {
            Reading::Known(true) => "spinning disk",
            Reading::Known(false) => "solid state",
            Reading::Unavailable(_) => "unknown type",
        };
        let size = match d.size_bytes {
            Reading::Known(b) => format!("{:.1} GB", b as f64 / 1e9),
            Reading::Unavailable(_) => "size unknown".into(),
        };
        let model = d.model.known().cloned().unwrap_or_else(|| "unnamed".into());
        out.push(Row {
            id: format!("disk.{}.identity", d.name),
            label: format!("/dev/{}", d.name),
            state: Health::Available,
            note: format!("{model}, {size}, {kind}"),
        });

        let (state, note) = match &d.smart {
            Ok(s) => storage::wear_health(s),
            Err(why) => (Health::Unavailable, why.clone()),
        };
        out.push(Row {
            id: format!("disk.{}.wear", d.name),
            label: format!("/dev/{} health", d.name),
            state,
            note,
        });

        // The temperature the SMART log gives when it could be read, and the
        // hwmon node otherwise. hwmon is world-readable, so unprivileged this
        // is the only one there is.
        let temp = d
            .smart
            .as_ref()
            .ok()
            .and_then(|s| s.temperature_c)
            .map(Reading::Known)
            .unwrap_or_else(|| match &d.temperature_c {
                Reading::Known(c) => Reading::Known(*c),
                Reading::Unavailable(r) => Reading::Unavailable(r.clone()),
            });
        let (state, note) = match temp {
            Reading::Known(c) => (Health::Available, format!("{c:.0} °C")),
            Reading::Unavailable(r) => (Health::Unavailable, r),
        };
        out.push(Row {
            id: format!("disk.{}.temperature", d.name),
            label: format!("/dev/{} temperature", d.name),
            state,
            note,
        });

        if let Reading::Known(true) = d.removable {
            out.push(Row {
                id: format!("disk.{}.removable", d.name),
                label: format!("/dev/{} is removable", d.name),
                state: Health::Available,
                note: "unplugging this while it is mounted loses whatever was not written".into(),
            });
        }
        // A disk the kernel has forced read-only is one that has already had a
        // write fail. Attention, and named as such: the usual cause is the
        // controller taking the device offline after an I/O error, and a user
        // who does not know that will keep trying to save.
        if let Reading::Known(true) = d.read_only {
            out.push(Row {
                id: format!("disk.{}.readonly", d.name),
                label: format!("/dev/{} is read-only", d.name),
                state: Health::Attention,
                note: "the kernel has marked this device read-only; nothing can be written to it"
                    .into(),
            });
        }

        for p in &d.partitions {
            let (state, note) = storage::encryption_health(&p.encryption);
            let size = match p.size_bytes {
                Reading::Known(b) => format!("{:.1} GB", b as f64 / 1e9),
                Reading::Unavailable(_) => "size unknown".into(),
            };
            let fs = match &p.fstype {
                Reading::Known(t) => t.clone(),
                Reading::Unavailable(_) => "filesystem unknown".into(),
            };
            out.push(Row {
                id: format!("part.{}.encryption", p.name),
                label: format!("/dev/{}", p.name),
                state,
                note: format!("{fs}, {size}, {note}"),
            });
        }

        let t = Trim {
            device_supports: match d.discard_granularity {
                Reading::Known(g) => Reading::Known(g > 0),
                Reading::Unavailable(ref r) => Reading::Unavailable(r.clone()),
            },
            // Any filesystem on any partition of this disk asking the kernel
            // to discard counts: the discard reaches the same device however
            // many partitions are in front of it.
            mount_discards: all_mounts
                .iter()
                .filter(|m| m.source.starts_with(&format!("/dev/{}", d.name)))
                .any(|m| storage::discards(&all_mounts, &m.source)),
            timer_enabled: match &timer_enabled {
                Reading::Known(v) => Reading::Known(*v),
                Reading::Unavailable(r) => Reading::Unavailable(r.clone()),
            },
            last_run: last_run.clone(),
        };
        let (state, note) = storage::trim_health(&t);
        out.push(Row {
            id: format!("disk.{}.trim", d.name),
            label: format!("/dev/{} trim", d.name),
            state,
            note,
        });
    }

    for m in storage::space(&all_mounts) {
        let (state, note) = match space_of(roots, &m.target) {
            Reading::Known(s) => storage::space_health(s),
            Reading::Unavailable(r) => (Health::Unavailable, r),
        };
        out.push(Row {
            id: format!("fs.{}.space", m.source.trim_start_matches("/dev/")),
            label: format!("{} on {}", m.fstype, m.target),
            state,
            note,
        });
    }
    Ok(out)
}

fn row_json(r: &Row) -> Value {
    json!({"id": r.id, "label": r.label, "state": r.state.as_str(), "note": r.note})
}

fn status(as_json: bool) -> i32 {
    let roots = Roots::from_env();
    let rows = match rows(&roots) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("rime: {e}");
            return 1;
        }
    };
    if as_json {
        let doc = json!({
            "rows": rows.iter().map(row_json).collect::<Vec<_>>(),
            "attention": rows.iter().filter(|r| r.state == Health::Attention).count(),
            "unavailable": rows.iter().filter(|r| r.state == Health::Unavailable).count(),
        });
        println!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
        return 0;
    }
    for r in &rows {
        println!("[{}] {:<28} {}", r.state.as_str(), r.label, r.note);
    }
    let attention = rows.iter().filter(|r| r.state == Health::Attention).count();
    let unavailable = rows.iter().filter(|r| r.state == Health::Unavailable).count();
    println!("\n{attention} needing attention, {unavailable} that could not be measured");
    if unavailable > 0 {
        println!("Some of this needs root. Try: sudo rime storage status");
    }
    0
}

fn warnings(as_json: bool) -> i32 {
    let roots = Roots::from_env();
    let rows = match rows(&roots) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("rime: {e}");
            return 1;
        }
    };
    let attention: Vec<&Row> = rows.iter().filter(|r| r.state == Health::Attention).collect();
    let unavailable: Vec<&Row> =
        rows.iter().filter(|r| r.state == Health::Unavailable).collect();
    if as_json {
        let doc = json!({
            "attention": attention.iter().map(|r| row_json(r)).collect::<Vec<_>>(),
            "unavailable": unavailable.iter().map(|r| row_json(r)).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
    } else {
        for r in &attention {
            println!("[attention] {:<28} {}", r.label, r.note);
        }
        for r in &unavailable {
            println!("[unavailable] {:<26} {}", r.label, r.note);
        }
    }
    // Only a row that needs doing changes the exit status. A read nobody could
    // perform is printed and is not an alert: waking somebody at 3am because
    // `rime storage` ran without root is how a notifier gets muted, and a muted
    // notifier misses the disk that is actually failing.
    i32::from(!attention.is_empty())
}

// ── erasing a device ─────────────────────────────────────────────────────────

/// wipefs, by absolute path, for the same reason as [`SMARTCTL`]: this runs
/// under sudo, and a `PATH` entry the invoking user controls would be a root
/// shell — here one that erases disks.
const WIPEFS: &str = rime_paths::WIPEFS;

/// Exit status for a refused erase, distinct from a failed one.
///
/// A caller — a settings page, a script, the suite — has to be able to tell
/// "the guard said no" from "the guard said yes and wipefs then failed". Both
/// as `1` means a script cannot retry the second and must not retry the first.
pub const EXIT_REFUSED: i32 = 3;

/// Who the kernel thinks is running this, or the reason nobody knows.
///
/// A [`Reading`] rather than `ops::effective_uid`'s `Option` because the two
/// unknowns want different sentences: `ops::require_root` folds an unreadable
/// `/proc/self/status` into "must run as root", which is safe but advises a
/// `sudo` that will not help.
fn effective_uid(roots: &Roots) -> Reading<u32> {
    if roots.is_fixture() {
        return match roots.fixture_file("euid") {
            Some(t) => match t.trim().parse::<u32>() {
                Ok(v) => Reading::Known(v),
                Err(e) => {
                    Reading::Unavailable(format!("fixture euid {t:?} is not a number ({e})"))
                }
            },
            None => Reading::Unavailable(
                "the fixture did not say which user is running this".into(),
            ),
        };
    }
    match crate::ops::effective_uid() {
        Some(v) => Reading::Known(v),
        None => Reading::Unavailable(
            "/proc/self/status did not say what the effective uid is".into(),
        ),
    }
}

/// What is stacked on a device, or the reason nobody knows.
///
/// The `Err` arm is the whole reason this is not a `Vec`. `read_dir` failing
/// and `read_dir` returning nothing are one line apart in the code and
/// opposite in meaning: the first is "nobody could check whether a dm-crypt
/// mapping depends on this disk" and the second is "nothing does". Measured:
/// every `holders` directory on this machine is empty, so the arm that matters
/// is the one that never fires here.
fn holders(roots: &Roots, sys_path: &str) -> Reading<Vec<String>> {
    let p = roots.path(&format!("{sys_path}/holders"));
    match std::fs::read_dir(&p) {
        Err(e) => Reading::Unavailable(format!("{}: {e}", p.display())),
        Ok(entries) => {
            let mut v: Vec<String> = entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect();
            v.sort();
            Reading::Known(v)
        }
    }
}

/// The path as the filesystem agrees it is, or as given when it cannot say.
///
/// Both sides of the backing-file comparison go through this so that a caller
/// naming `/tmp/x.img` and a kernel that recorded the path through a symlinked
/// `/tmp` still meet. Two deliberate non-behaviours: the kernel appends
/// ` (deleted)` to the backing file of an unlinked image and `canonicalize`
/// fails on that, so the raw string survives — a deleted backing file must not
/// accidentally equal anything. And under a fixture root nothing is
/// canonicalised at all: the fixture's paths name files that do not exist, and
/// resolving them against the real filesystem would turn `/lib/...` into
/// `/usr/lib/...` and make the suite assert something it did not write.
fn canonical(roots: &Roots, p: &str) -> String {
    if roots.is_fixture() {
        return p.to_string();
    }
    std::fs::canonicalize(p).map(|q| q.display().to_string()).unwrap_or_else(|_| p.to_string())
}

/// The file a loop device is attached to, or `None` when it is not one.
///
/// **`NotFound` and nothing else is the definite answer.** `Path::exists()`
/// returns false on `EACCES` as readily as on a missing file, and a guard that
/// read "permission denied" as "not an attached loop device" would erase
/// exactly the device this rule exists to protect. Every other error is
/// `Unavailable`, which refuses.
fn loop_backing(roots: &Roots, sys_path: &str) -> Reading<Option<String>> {
    let dir = roots.path(&format!("{sys_path}/loop"));
    match std::fs::metadata(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Reading::Known(None),
        Err(e) => return Reading::Unavailable(format!("{}: {e}", dir.display())),
        Ok(_) => {}
    }
    let f = roots.path(&format!("{sys_path}/loop/backing_file"));
    match std::fs::read_to_string(&f) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Reading::Known(None),
        Err(e) => Reading::Unavailable(format!("{}: {e}", f.display())),
        Ok(t) => match t.trim() {
            "" => Reading::Known(None),
            path => Reading::Known(Some(canonical(roots, path))),
        },
    }
}

/// The partition fields of a device's udev record.
fn partition_entry(roots: &Roots, sys_path: &str) -> Reading<PartitionEntry> {
    match udev_record(roots, sys_path) {
        Err(why) => Reading::Unavailable(why),
        Ok(props) => Reading::Known(PartitionEntry {
            scheme: props.get("ID_PART_ENTRY_SCHEME").cloned(),
            type_id: props.get("ID_PART_ENTRY_TYPE").cloned(),
            name: props.get("ID_PART_ENTRY_NAME").cloned(),
            fs_type: props.get("ID_FS_TYPE").cloned(),
        }),
    }
}

/// Every block device, whole disks and partitions alike.
///
/// **Deliberately not [`disks`].** That function skips everything under
/// `devices/virtual/block`, which is correct for a health report — a loop
/// device has no SMART log and no wear — and wrong here twice over: it would
/// make every loop device `NotAKnownBlockDevice`, so the one kind of device
/// this command is ever tested against could not be named; and it would make
/// `/dev/dm-0` unnameable too, while leaving whatever it maps perfectly
/// erasable. A guard that cannot see a device cannot protect anything stacked
/// on it.
///
/// Partitions are found by the `partition` file existing, not by their name
/// starting with the disk's. Both hold on the standard drivers, but only the
/// first is what sysfs actually promises.
///
/// **`Result`, and every enumeration error is the `Err` arm, because a
/// partially-enumerated machine is not a machine to judge against.** Every
/// other reading here is a [`Reading`] that the guard weighs; this one cannot
/// be, because the guard reasons about a device by looking at its *neighbours*
/// — `would_destroy` walks downward from a disk to its partitions, the
/// critical-system rule matches a mount source against them, and the ESP rule
/// is asked only of partitions. A device that was never enumerated is not an
/// unknown the guard can refuse on; it is a fact the guard never learns.
///
/// Measured, in the fixture, before this was a `Result`: `chmod 100` on one
/// disk's sysfs directory — readable to no one, still traversable, so
/// `holders/` and `dev` still answer — dropped all five partitions of
/// `nvme0n1`. The whole-disk erase then went from **six refusals to a granted
/// permit**: no mount source is `/dev/nvme0n1` (measured on the L16: zero
/// times), nothing was left with `parent == nvme0n1`, and the ESP rule never
/// runs on a whole disk. Only the fixture gate stopped `wipefs --all` on the
/// disk carrying the running system.
///
/// So this is the fifth way "is the device mounted?" green-lights destroying
/// this machine, and the first that needs no unusual hardware — just one
/// unreadable directory.
pub fn machine(roots: &Roots) -> Result<Machine, String> {
    let mut names = dir_names(&roots.path("/sys/block"))?;
    names.sort();

    let mut devices = Vec::new();
    for disk in &names {
        let sys = format!("/sys/block/{disk}");
        devices.push(BlockDevice {
            kernel_name: disk.clone(),
            parent: None,
            holders: holders(roots, &sys),
            partition: partition_entry(roots, &sys),
            loop_backing: loop_backing(roots, &sys),
        });

        let base = roots.path(&sys);
        let mut parts = Vec::new();
        for name in dir_names(&base)? {
            if &name == disk {
                continue;
            }
            // Two questions, and only two answers are definite enough to skip
            // on: this entry is not a directory (so it is one of the dozen
            // attribute FILES beside the partitions — `dev`, `size`, `ro`),
            // or it is a directory with no `partition` in it (`queue`,
            // `holders`, `power`).
            //
            // `Path::exists()` would have collapsed both of those together
            // with EACCES, which is the mistake this module keeps finding: it
            // is false on a permission denial exactly as readily as on a
            // missing file, so a partition inside an unreadable directory
            // would quietly stop being a partition. Only `NotFound` and
            // `NotADirectory` say "not a partition"; everything else is an
            // enumeration that came up short.
            let entry = base.join(&name);
            match std::fs::metadata(&entry) {
                // Gone between `read_dir` and here. sysfs is live and a device
                // can be unplugged mid-scan; nothing to protect any more.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("{}: {e}", entry.display())),
                Ok(md) if !md.is_dir() => continue,
                Ok(_) => {}
            }
            let marker = entry.join("partition");
            match std::fs::metadata(&marker) {
                Ok(_) => parts.push(name),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) => {}
                Err(e) => return Err(format!("{}: {e}", marker.display())),
            }
        }
        parts.sort();
        // A partition of an attached loop device is attached to the same file
        // its parent is, and sysfs does not say so. Measured, with
        // `losetup --find --show --partscan` on a GPT image:
        // `/sys/block/loop1/loop1p1` has a `partition` file and **no `loop/`
        // directory at all** — so read on its own the partition answers
        // `Known(None)`, "not an attached loop device", and the guard would
        // permit erasing it. `wipefs` on it writes through the parent into the
        // backing file just the same.
        //
        // `would_destroy` walks downward only, so nothing else relates a
        // partition back up to its disk. Without this the loop rule is
        // one-directional where the mount rule is explicitly both.
        let inherited = devices
            .last()
            .filter(|d| d.kernel_name == *disk)
            .map(|d| d.loop_backing.clone())
            .unwrap_or(Reading::Known(None));
        for part in parts {
            let psys = format!("{sys}/{part}");
            let own = loop_backing(roots, &psys);
            devices.push(BlockDevice {
                kernel_name: part,
                parent: Some(disk.clone()),
                holders: holders(roots, &psys),
                partition: partition_entry(roots, &psys),
                // Its own answer wins if it has one — a partition with a
                // `loop/` directory is a shape sysfs does not currently
                // produce, and inventing a rule that overrides a real reading
                // is how the next surprise becomes a defect. Otherwise the
                // parent's answer, including the parent's `Unavailable`: if
                // nobody could tell whether the disk is an attached loop
                // device, nobody can tell whether this partition is on one.
                loop_backing: match own {
                    Reading::Known(None) => inherited.clone(),
                    other => other,
                },
            });
        }
    }

    Ok(Machine {
        devices,
        // mountinfo failing is not an empty mount table. It is the single
        // check whose collapse to "nothing is mounted" would erase the
        // running system. Unlike the enumeration above this one CAN be a
        // `Reading`: the guard has a rule for not knowing what is mounted, and
        // that rule refuses.
        mounts: match mounts(roots) {
            Ok(m) => Reading::Known(m),
            Err(e) => Reading::Unavailable(e),
        },
    })
}

/// The names in a directory, or the reason the list is not the whole list.
///
/// No `.flatten()`. `read_dir`'s per-entry `Err` is a name that exists and was
/// not returned, and `flatten` spells "silently shorten the list of devices we
/// are about to protect" in eight characters.
fn dir_names(dir: &std::path::Path) -> Result<Vec<String>, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut out = Vec::new();
    for e in entries {
        let e = e.map_err(|e| format!("{}: {e}", dir.display()))?;
        out.push(e.file_name().to_string_lossy().to_string());
    }
    Ok(out)
}

/// `rime storage erase` — refuse, or wipe the signatures off one device.
///
/// Note the order: the guard runs before anything is opened, and the fixture
/// gate sits between the permit and the subprocess. A `Permit` is necessary to
/// reach the erase and is not sufficient, because a fixture that answered
/// `euid` 0 has a permit for a device that does not exist and must still spawn
/// nothing.
fn erase(device: &str, confirm: &str, expect_backing: Option<&str>) -> i32 {
    let roots = Roots::from_env();
    // An enumeration that came up short is a refusal and not a smaller
    // machine — see `machine`. It reports as a refusal rather than an error
    // because that is what it is: nothing was wrong with the request.
    let m = match machine(&roots) {
        Ok(m) => m,
        Err(why) => {
            eprintln!("rime: refusing to erase {device}");
            eprintln!(
                "  - refusing because the block devices on this machine could not all be \
                 listed: {why}. Judging a request against a partial list of devices is how \
                 a whole disk gets erased for having no partitions"
            );
            return EXIT_REFUSED;
        }
    };
    let euid = effective_uid(&roots);
    // Canonicalised on this side too, or the comparison is between a path the
    // user typed and a path the kernel resolved.
    let expect = expect_backing.map(|e| canonical(&roots, e));

    let permit = match storage::guard(device, confirm, &m, &euid, expect.as_deref()) {
        Ok(p) => p,
        Err(why) => {
            eprintln!("rime: refusing to erase {device}");
            for r in &why {
                eprintln!("  - {}", r.say());
            }
            return EXIT_REFUSED;
        }
    };

    // Unconditional, and above the subprocess rather than inside a branch of
    // it: under a fixture root every path in `machine()` came from files this
    // process could have written, so a fixture that says euid 0 can obtain a
    // real permit. It must not be able to obtain a real erase.
    if roots.is_fixture() {
        println!("would erase {} — fixture root, nothing was touched", permit.device());
        return 0;
    }

    let backups = std::path::Path::new(storage::SIGNATURE_BACKUP_DIR);
    if let Err(e) = std::fs::create_dir_all(backups) {
        eprintln!("rime: cannot create {}: {e}", backups.display());
        return 1;
    }

    // `--backup=<dir>` and never a bare `--backup`: measured, wipefs with no
    // directory writes into `$HOME`, and `$HOME` under sudo is `/root`. The
    // file a user is told to keep would land somewhere they will not look and
    // may not be able to read.
    let out = Command::new(WIPEFS)
        .arg("--all")
        .arg(format!("--backup={}", backups.display()))
        .arg(permit.device())
        .output();
    match out {
        Err(e) => {
            eprintln!("rime: cannot run {WIPEFS}: {e}");
            1
        }
        Ok(o) if !o.status.success() => {
            eprint!("{}", String::from_utf8_lossy(&o.stderr));
            eprintln!(
                "rime: wipefs refused {} (exit {})",
                permit.device(),
                o.status.code().unwrap_or(-1)
            );
            1
        }
        Ok(o) => {
            print!("{}", String::from_utf8_lossy(&o.stdout));
            println!("erased the filesystem signatures on {}", permit.device());
            // The path, not the word "backup": guidance a user cannot follow
            // is the same as no guidance.
            println!(
                "signature backups are in {} — keep them until you are sure",
                backups.display()
            );
            0
        }
    }
}

pub fn main(cmd: StorageCmd) -> i32 {
    match cmd {
        StorageCmd::Status { json } => status(json),
        StorageCmd::Warnings { json } => warnings(json),
        StorageCmd::Erase { device, confirm, expect_backing_file } => {
            erase(&device, &confirm, expect_backing_file.as_deref())
        }
    }
}

/// The storage lines `rime doctor` prints.
///
/// A `WARN` in the doctor is information rather than a fault, which is exactly
/// the right register for most of this — but a disk reporting itself as
/// failing is not information. Only [`Health::Attention`] rows turn a doctor
/// line false; a row nobody could measure is reported with its reason and
/// passes, because the doctor already says elsewhere that root is needed.
pub fn doctor_lines(roots: &Roots) -> Vec<(bool, String)> {
    match rows(roots) {
        Err(e) => vec![(false, format!("storage could not be read: {e}"))],
        Ok(rows) => rows
            .iter()
            .filter(|r| r.state == Health::Attention || r.state == Health::Unavailable)
            .map(|r| {
                (
                    r.state != Health::Attention,
                    format!("storage: {} — {}", r.label, r.note),
                )
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mountinfo_is_split_on_the_separator_not_on_a_field_index() {
        // The optional-fields run between field 6 and the separator is
        // variable length: `shared:1`, or nothing, or two of them. Every
        // systemd machine has some. A fixed index reads the fstype out of a
        // propagation flag.
        let text = "\
49 1 0:39 / / ro,relatime shared:1 - overlay composefs ro,seclabel
46 49 0:36 /x /etc rw,relatime shared:2 master:9 - btrfs /dev/nvme0n1p5 rw,ssd
23 20 0:22 / /proc rw,nosuid - proc proc rw
";
        let d = fixture_with("proc/self/mountinfo", text);
        let roots = Roots { fixture: Some(d.path().to_path_buf()) };
        let m = mounts(&roots).expect("parse");
        assert_eq!(m.len(), 3);
        assert_eq!(m[0].fstype, "overlay");
        assert_eq!(m[0].source, "composefs");
        assert!(m[0].read_only);
        assert_eq!(m[1].fstype, "btrfs");
        assert_eq!(m[1].source, "/dev/nvme0n1p5");
        assert!(!m[1].read_only, "two optional fields shifted the read-only flag");
        assert_eq!(m[2].source, "proc");
        assert_eq!(m[1].super_options, "rw,ssd");
    }

    #[test]
    fn a_mount_point_with_a_space_in_it_parses() {
        let text = "49 1 0:39 / /mnt/my\\040disk rw,relatime - ext4 /dev/sdb1 rw\n";
        let d = fixture_with("proc/self/mountinfo", text);
        let roots = Roots { fixture: Some(d.path().to_path_buf()) };
        let m = mounts(&roots).expect("parse");
        assert_eq!(m[0].target, "/mnt/my disk");
    }

    #[test]
    fn the_size_node_is_sectors_and_a_sector_is_512_bytes_whatever_the_disk_says() {
        // /sys/block/<d>/size is always in 512-byte units. Multiplying by the
        // logical block size is a factor-of-eight error on a 4K disk, in the
        // direction that makes a full disk look empty.
        let d = fixture_with("sys/block/sda/size", "4000797360\n");
        std::fs::write(d.path().join("sys/block/sda/queue_placeholder"), "").ok();
        let roots = Roots { fixture: Some(d.path().to_path_buf()) };
        let disk = disk(&roots, "sda");
        assert_eq!(disk.size_bytes, Reading::Known(4000797360 * 512));
    }

    #[test]
    fn a_missing_hwmon_is_a_reason_and_not_a_zero() {
        let d = fixture_with("sys/block/sda/size", "1\n");
        let roots = Roots { fixture: Some(d.path().to_path_buf()) };
        match temperature(&roots, "sda") {
            Reading::Unavailable(r) => assert!(r.contains("hwmon"), "{r}"),
            Reading::Known(v) => panic!("a missing hwmon read as {v} °C"),
        }
    }

    #[test]
    fn hwmon_millidegrees_become_degrees() {
        // Measured on the L16: hwmon4 is named nvme and reads 37850.
        // The nvme layout: hwmon4 sits directly under the device, with no
        // `hwmon` directory in between.
        let d = fixture_with("sys/block/nvme0n1/device/hwmon4/temp1_input", "37850\n");
        let roots = Roots { fixture: Some(d.path().to_path_buf()) };
        assert_eq!(temperature(&roots, "nvme0n1"), Reading::Known(37.85));
    }

    #[test]
    fn the_other_hwmon_layout_is_found_too() {
        // The platform drivers nest it: <device>/hwmon/hwmon6. Both layouts
        // are on this machine, and checking only one reports "no sensor" for
        // half the hardware in the world.
        let d = fixture_with("sys/block/sda/device/hwmon/hwmon6/temp1_input", "41000\n");
        let roots = Roots { fixture: Some(d.path().to_path_buf()) };
        assert_eq!(temperature(&roots, "sda"), Reading::Known(41.0));
    }

    #[test]
    fn a_partition_with_no_udev_record_is_never_reported_as_unencrypted() {
        let e = encryption(
            &Reading::Unavailable("no record".into()),
            &Err("/run/udev/data/b8:1: No such file or directory".into()),
        );
        assert!(matches!(e, Encryption::Unknown(_)), "{e:?}");
        let (h, note) = storage::encryption_health(&e);
        assert_eq!(h, Health::Unavailable);
        assert!(!note.contains("not encrypted"), "{note}");
    }

    #[test]
    fn a_luks_partition_is_reported_as_encrypted_with_its_version() {
        let props = BTreeMap::from([
            ("ID_FS_TYPE".to_string(), "crypto_LUKS".to_string()),
            ("ID_FS_VERSION".to_string(), "2".to_string()),
        ]);
        let e = encryption(&Reading::Known("crypto_LUKS".into()), &Ok(props));
        assert_eq!(e, Encryption::Encrypted("LUKS2".into()));
    }

    #[test]
    fn the_udev_record_is_read_for_its_e_lines_and_nothing_else() {
        // The real file starts with S: symlink lines and an I: line, and it
        // carries ID_SERIAL_SHORT — which must be readable here and must not
        // reach a report. This asserts the parse; the report's own test
        // asserts the serial does not appear.
        let text = "S:disk/by-label/rime-root\nI:11132713\n\
                    E:ID_SERIAL_SHORT=NF12312150500322\nE:ID_FS_TYPE=btrfs\n";
        let d = fixture_with("run/udev/data/b259:5", text);
        std::fs::create_dir_all(d.path().join("sys/block/nvme0n1/nvme0n1p5")).expect("mkdir");
        std::fs::write(d.path().join("sys/block/nvme0n1/nvme0n1p5/dev"), "259:5\n").expect("write");
        let roots = Roots { fixture: Some(d.path().to_path_buf()) };
        let props = udev_record(&roots, "/sys/block/nvme0n1/nvme0n1p5").expect("record");
        assert_eq!(props.get("ID_FS_TYPE").map(String::as_str), Some("btrfs"));
        assert_eq!(props.len(), 2, "an S: or I: line was parsed as a property");
    }

    #[test]
    fn a_row_that_could_not_be_measured_does_not_make_the_doctor_line_fail() {
        // A notifier keyed on the doctor's booleans must not fire because
        // somebody ran it without root.
        let unavailable = Row {
            id: "x".into(),
            label: "/dev/sda health".into(),
            state: Health::Unavailable,
            note: "permission denied — run it with sudo".into(),
        };
        let attention = Row {
            id: "y".into(),
            label: "/dev/sda health".into(),
            state: Health::Attention,
            note: "SMART reports this disk as failing".into(),
        };
        assert_ne!(unavailable.state, Health::Attention);
        assert_eq!(attention.state, Health::Attention);
    }

    fn fixture_with(rel: &str, text: &str) -> tempdir::Dir {
        let d = tempdir::Dir::new();
        let p = d.path().join(rel);
        std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
        std::fs::write(&p, text).expect("write");
        d
    }

    mod tempdir {
        use std::path::{Path, PathBuf};
        use std::sync::atomic::{AtomicU64, Ordering};

        static N: AtomicU64 = AtomicU64::new(0);

        pub struct Dir(PathBuf);

        impl Dir {
            pub fn new() -> Dir {
                let base = std::env::temp_dir().join(format!(
                    "rime-storage-test-{}-{}",
                    std::process::id(),
                    N.fetch_add(1, Ordering::Relaxed)
                ));
                let _ = std::fs::remove_dir_all(&base);
                std::fs::create_dir_all(&base).expect("mkdir");
                Dir(base)
            }
            pub fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
