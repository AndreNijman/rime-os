//! Which system of a release this machine installs (docs/nixos/updates-and-release.md §3.1 `variants`, §11.6), and
//! the disk floors a feature needs before `rime install` selects it (§11.5, closure.md §11).
//!
//! A release publishes one system per set of offered catalogue FEATURES (nix/release/variants.nix): `plain`,
//! `plain-gaming`, `plain-virtualization`, `plain-gaming-virtualization`. This machine's base (`variant` in
//! /etc/rime/update.toml, `plain`) plus every feature switched on for it (the switch files
//! /var/lib/rime/switch/rime-feature@<feature>.target.on that `rime install steam` writes) names the system it wants.
//! The two sides agree by construction: [`name`] here and `name` in variants.nix both sort the features and join them
//! to the base with "-", and checks.release-plan proves the Nix side.
//!
//! Decision (2026-11-02): a release that builds no system with a switched-on feature does not block the update. The
//! machine takes the largest set of its features the release does build (at least its base) and says which feature
//! waits; the switch stays, so a later release that builds it installs it. A release without the BASE system is
//! still refused (§3.1: no fallback to another base).

use std::collections::BTreeMap;

use serde::Deserialize;

use super::pkg::{read_list, Catalogue};

/// The release variant for `base` with `features` switched on: the features sorted, deduplicated, joined with "-".
pub fn name(base: &str, features: &[String]) -> String {
    let mut f: Vec<&str> = features.iter().map(String::as_str).collect();
    f.sort_unstable();
    f.dedup();
    std::iter::once(base).chain(f).collect::<Vec<_>>().join("-")
}

/// The features switched on for this machine: every feature of the catalogue whose switch file names at least one
/// package. No catalogue (a system built without it): none.
pub fn switched_on(cat: Option<&Catalogue>) -> Vec<String> {
    let Some(cat) = cat else {
        return vec![];
    };
    cat.features
        .iter()
        .filter(|(_, f)| !read_list(std::path::Path::new(&f.switch)).is_empty())
        .map(|(n, _)| n.clone())
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    /// What the base and the switches ask for.
    pub wanted: String,
    /// The system this release builds that comes closest: `wanted` when it exists.
    pub chosen: String,
    /// Switched-on features the chosen system does not carry (the release builds no system with them yet).
    pub waiting: Vec<String>,
}

/// Pick the variant for `base` + `features` among those `has` says the release builds. The largest subset of the
/// features wins; between subsets of one size, the first in sorted order. `None`: the release has no `base` system.
pub fn choose(base: &str, features: &[String], has: impl Fn(&str) -> bool) -> Option<Choice> {
    choose_keeping(base, features, has, |_| false)
}

/// [`choose`], where the features `waits_first` names (the desktops, docs/nixos/desktops.md §3.2) are the ones that
/// wait when the release cannot carry everything: among the largest subsets, one that keeps every other switched-on
/// feature wins, so adding a desktop never parks gaming or virtualization. Then the first in sorted order.
pub fn choose_keeping(
    base: &str,
    features: &[String],
    has: impl Fn(&str) -> bool,
    waits_first: impl Fn(&str) -> bool,
) -> Option<Choice> {
    let mut f: Vec<String> = features.to_vec();
    f.sort_unstable();
    f.dedup();
    let wanted = name(base, &f);
    let n = f.len().min(16);
    let mut subsets: Vec<Vec<String>> = (0u32..(1 << n))
        .map(|mask| (0..n).filter(|i| mask & (1 << i) != 0).map(|i| f[i].clone()).collect())
        .collect();
    let keeps_the_rest = |s: &Vec<String>| f.iter().all(|x| waits_first(x) || s.contains(x));
    subsets.sort_by(|a: &Vec<String>, b| {
        b.len().cmp(&a.len()).then_with(|| keeps_the_rest(b).cmp(&keeps_the_rest(a))).then_with(|| a.cmp(b))
    });
    for s in subsets {
        let v = name(base, &s);
        if has(&v) {
            let waiting = f.iter().filter(|x| !s.contains(x)).cloned().collect();
            return Some(Choice { wanted, chosen: v, waiting });
        }
    }
    None
}

/// The variant this machine wants from a release that builds `variants`, with its switches read from the catalogue.
pub fn for_machine(base: &str, variants: &[&str]) -> Option<Choice> {
    let cat = Catalogue::load().ok();
    let is_desktop = |f: &str| cat.as_ref().is_some_and(|c| c.is_desktop(f));
    choose_keeping(base, &switched_on(cat.as_ref()), |v| variants.contains(&v), is_desktop)
}

/// A floor from nix/release/floors.nix, rendered into the catalogue (`floors.<variant>`).
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Floor {
    /// What the filesystem holding /nix must hold: the system, room for one mass rebuild, home.
    pub store_bytes: u64,
    /// The whole-disk figure the installer and the docs publish.
    pub disk_gb: u64,
}

/// The floor of the system `base` + `features`: its own when one was measured, otherwise the largest floor of a
/// system whose features it contains (a combination nobody measured is at least as big as each of its parts).
pub fn floor_for<'a>(floors: &'a BTreeMap<String, Floor>, base: &str, features: &[String]) -> Option<&'a Floor> {
    let exact = name(base, features);
    if let Some(f) = floors.get(&exact) {
        return Some(f);
    }
    let mut f: Vec<String> = features.to_vec();
    f.sort_unstable();
    f.dedup();
    let n = f.len().min(16);
    (0u32..(1 << n))
        .filter_map(|mask| {
            let s: Vec<String> = (0..n).filter(|i| mask & (1 << i) != 0).map(|i| f[i].clone()).collect();
            floors.get(&name(base, &s))
        })
        .max_by_key(|fl| fl.store_bytes)
}

/// `Some(refusal)` when a filesystem of `fs_bytes` is below `floor`: the message names the system the package would
/// select and both sizes (the caller puts the package name in front).
pub fn floor_refusal(title: &str, floor: &Floor, fs_bytes: u64) -> Option<String> {
    if fs_bytes >= floor.store_bytes {
        return None;
    }
    const GB: f64 = 1_000_000_000.0;
    Some(format!(
        "Rime's {title} system needs a disk of at least {} GB, and the filesystem holding /nix here is \
         {:.1} GB (it must hold {:.1} GB: the system, room for one whole update beside it, and your files). \
         Nothing was changed.",
        floor.disk_gb,
        fs_bytes as f64 / GB,
        floor.store_bytes as f64 / GB
    ))
}

/// Size of the filesystem holding `path` (statvfs: blocks × fragment size).
pub fn fs_size(path: &str) -> Option<u64> {
    let c = std::ffi::CString::new(path).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: statvfs into a zeroed struct we own.
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    Some(s.f_blocks as u64 * s.f_frsize as u64)
}

/// The on-disk estimate's ratio for the store at `path` (§11.5): `compressed` when the mount holding it is btrfs with
/// `compress-force=zstd` (what the installer and the bridge mount), 1.0 (count the unpacked size) otherwise.
/// `mounts` is /proc/self/mounts.
pub fn store_ratio(mounts: &str, path: &str, compressed: f64) -> f64 {
    let mut best: Option<(&str, &str, &str)> = None;
    for line in mounts.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 4 {
            continue;
        }
        let (mnt, fstype, opts) = (f[1], f[2], f[3]);
        let under = mnt == "/" || path == mnt || path.starts_with(&format!("{mnt}/"));
        if under && best.map_or(true, |(b, _, _)| mnt.len() >= b.len()) {
            best = Some((mnt, fstype, opts));
        }
    }
    match best {
        Some((_, "btrfs", opts)) if opts.split(',').any(|o| o.starts_with("compress-force=zstd")) => {
            compressed.clamp(0.05, 1.0)
        }
        _ => 1.0,
    }
}

/// The gate's need for `unpacked` bytes of new store paths at `ratio`, plus `reserve`.
pub fn need_on_disk(unpacked: u64, ratio: f64, reserve: u64) -> u64 {
    (unpacked as f64 * ratio).ceil() as u64 + reserve
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn variant_names_sort_the_features_as_variants_nix_does() {
        assert_eq!(name("plain", &[]), "plain");
        assert_eq!(name("plain", &s(&["gaming"])), "plain-gaming");
        assert_eq!(name("plain", &s(&["virtualization", "gaming"])), "plain-gaming-virtualization");
        assert_eq!(name("plain", &s(&["gaming", "gaming"])), "plain-gaming");
    }

    #[test]
    fn the_switches_choose_the_system_and_a_missing_one_waits() {
        let all = ["plain", "plain-gaming", "plain-virtualization", "plain-gaming-virtualization"];
        let has = |v: &str| all.contains(&v);
        let c = choose("plain", &s(&["gaming"]), has).unwrap();
        assert_eq!((c.wanted.as_str(), c.chosen.as_str()), ("plain-gaming", "plain-gaming"));
        assert!(c.waiting.is_empty());
        assert_eq!(choose("plain", &[], has).unwrap().chosen, "plain");
        assert_eq!(choose("plain", &s(&["virtualization", "gaming"]), has).unwrap().chosen, "plain-gaming-virtualization");
        // A release that builds only plain and plain-gaming: virtualization waits, gaming is kept.
        let some = |v: &str| v == "plain" || v == "plain-gaming";
        let c = choose("plain", &s(&["gaming", "virtualization"]), some).unwrap();
        assert_eq!((c.chosen.as_str(), c.waiting.clone()), ("plain-gaming", s(&["virtualization"])));
        // Only the base: everything waits. No base: refused.
        let c = choose("plain", &s(&["gaming"]), |v| v == "plain").unwrap();
        assert_eq!((c.chosen.as_str(), c.waiting.clone()), ("plain", s(&["gaming"])));
        assert_eq!(choose("plain", &[], |v| v == "luks"), None);
    }

    #[test]
    fn a_desktop_waits_before_gaming_or_virtualization_does() {
        // docs/nixos/desktops.md §3.2: gaming + virtualization + GNOME on a release that builds plain-gaming-gnome
        // and plain-gaming-virtualization but not the three together. Both are the largest fitting subsets; the
        // one that keeps every non-desktop feature wins and GNOME waits (lexically, gaming-gnome would come first).
        let rel = ["plain", "plain-gaming", "plain-gaming-gnome", "plain-gaming-virtualization"];
        let desktop = |f: &str| f == "gnome" || f == "kde" || f == "cosmic";
        let c = choose_keeping("plain", &s(&["gnome", "virtualization", "gaming"]), |v| rel.contains(&v), desktop).unwrap();
        assert_eq!(c.wanted, "plain-gaming-gnome-virtualization");
        assert_eq!((c.chosen.as_str(), c.waiting.clone()), ("plain-gaming-virtualization", s(&["gnome"])));
        // Without a desktop marker the old order holds (sorted order between equal sizes).
        let c = choose("plain", &s(&["gnome", "virtualization", "gaming"]), |v| rel.contains(&v)).unwrap();
        assert_eq!(c.chosen, "plain-gaming-gnome");
        // The whole set, when the release carries it, still wins over everything.
        let all = |v: &str| rel.contains(&v) || v == "plain-gaming-gnome-virtualization";
        assert_eq!(choose_keeping("plain", &s(&["gnome", "virtualization", "gaming"]), all, desktop).unwrap().chosen,
                   "plain-gaming-gnome-virtualization");
        // Only the desktop switched on, on a release without its variant: plain, the desktop waits.
        let c = choose_keeping("plain", &s(&["kde"]), |v| v == "plain", desktop).unwrap();
        assert_eq!((c.chosen.as_str(), c.waiting.clone()), ("plain", s(&["kde"])));
    }

    #[test]
    fn switch_files_name_the_features_that_are_on() {
        let d = std::env::temp_dir().join(format!("rime-variant-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let sw = |f: &str| d.join(format!("rime-feature@{f}.target.on")).display().to_string();
        let doc = serde_json::json!({
            "schema": 1, "flathub_remote": "flathub", "apps_profile": "/x", "outs_file": "/y",
            "features": {
                "gaming": {"title": "Gaming", "offered": true, "in_system": false, "unit": "u", "switch": sw("gaming")},
                "virtualization": {"title": "VMs", "offered": true, "in_system": false, "unit": "u", "switch": sw("virtualization")}
            }
        });
        let cat = Catalogue::parse(&doc.to_string()).unwrap();
        assert!(switched_on(Some(&cat)).is_empty());
        std::fs::write(sw("gaming"), "steam\n").unwrap();
        assert_eq!(switched_on(Some(&cat)), s(&["gaming"]));
        // An empty switch file is off (the last name out removes it, but an empty one must not count either).
        std::fs::write(sw("virtualization"), "").unwrap();
        assert_eq!(switched_on(Some(&cat)), s(&["gaming"]));
        assert!(switched_on(None).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    fn floors() -> BTreeMap<String, Floor> {
        // nix/release/floors.nix at 2026-11-02 (checks.release-plan proves the rendering).
        [
            ("plain".to_string(), Floor { store_bytes: 13_152_815_581, disk_gb: 16 }),
            ("plain-gaming".to_string(), Floor { store_bytes: 15_245_693_382, disk_gb: 18 }),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn steam_is_refused_by_name_below_the_gaming_floor() {
        let f = floors();
        let gaming = floor_for(&f, "plain", &s(&["gaming"])).unwrap();
        assert_eq!(gaming.disk_gb, 18);
        // A 12 GiB disk (the update-e2e client): refused, and the message says why with both numbers.
        let r = floor_refusal("Gaming", gaming, 12 * 1024 * 1024 * 1024).unwrap();
        assert!(r.starts_with("Rime's Gaming system needs a disk of at least 18 GB"), "{r}");
        assert!(r.contains("Nothing was changed"), "{r}");
        // Exactly at the floor, and above it: accepted.
        assert_eq!(floor_refusal("Gaming", gaming, gaming.store_bytes), None);
        assert_eq!(floor_refusal("Gaming", gaming, 1_000_000_000_000), None);
        // A combination nobody measured takes the largest floor it contains; a base alone takes its own.
        assert_eq!(floor_for(&f, "plain", &s(&["virtualization", "gaming"])).unwrap().disk_gb, 18);
        assert_eq!(floor_for(&f, "plain", &s(&["virtualization"])).unwrap().disk_gb, 16);
        assert_eq!(floor_for(&BTreeMap::new(), "plain", &s(&["gaming"])), None);
    }

    #[test]
    fn the_gate_counts_on_disk_bytes_only_on_a_compressed_store() {
        let installer = "/dev/vda2 / btrfs rw,relatime,compress-force=zstd:3,subvol=/@nixos 0 0\n\
                         /dev/vda2 /nix btrfs rw,relatime,compress-force=zstd:3,subvol=/@nix 0 0\n\
                         /dev/vda1 /boot vfat rw 0 0\n";
        assert_eq!(store_ratio(installer, "/nix/store", 0.55), 0.55);
        // The VM tests' ext4 store, and btrfs without forced compression: the unpacked size.
        assert_eq!(store_ratio("/dev/vda / ext4 rw 0 0\n", "/nix/store", 0.55), 1.0);
        assert_eq!(store_ratio("/dev/vda / btrfs rw,compress=zstd:3 0 0\n", "/nix/store", 0.55), 1.0);
        // The longest mount point wins: a compressed / does not make an uncompressed /nix count compressed.
        let mixed = "/dev/a / btrfs rw,compress-force=zstd:3 0 0\n/dev/b /nix ext4 rw 0 0\n";
        assert_eq!(store_ratio(mixed, "/nix/store", 0.55), 1.0);
        // /nixfoo is not /nix.
        let other = "/dev/a / btrfs rw,compress-force=zstd:3 0 0\n/dev/b /nixfoo ext4 rw 0 0\n";
        assert_eq!(store_ratio(other, "/nix/store", 0.55), 0.55);
        assert_eq!(need_on_disk(10_000, 0.55, 1_000), 6_500);
        assert_eq!(need_on_disk(10_000, 1.0, 1_000), 11_000);
    }

    #[test]
    fn a_floor_holds_the_gates_whole_rebuild() {
        // The floor's rebuild room is exactly what the gate asks for a whole new closure (floors.nix arithmetic):
        // plain store floor = installed on disk + need(unpacked) + 1 GB home. A machine at the floor is not refused
        // a mass rebuild; were the gate to count unpacked bytes (ratio 1, the pre-wave-3 gate), it would be.
        let (installed, unpacked, home) = (5_442_637_824u64, 10_382_141_376u64, 1_000_000_000u64);
        let plain = floors()["plain"].store_bytes;
        assert!(installed + need_on_disk(unpacked, 0.55, 1_000_000_000) + home <= plain);
        assert!(installed + need_on_disk(unpacked, 1.0, 1_000_000_000) + home > plain);
    }
}
