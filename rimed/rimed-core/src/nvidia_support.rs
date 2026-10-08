//! Does the NVIDIA driver in this image support this machine's NVIDIA GPU?
//!
//! Rime ships one NVIDIA driver branch, the current one, built and signed into
//! the image (Containerfile.core stage 1d). NVIDIA's 590 branch dropped the
//! Maxwell, Pascal and Volta GPUs (GeForce GTX 750 to GTX 1080 Ti, the MX110
//! to MX350 laptop parts, TITAN X, TITAN V); the 580 branch is the last that drives
//! them. On such a GPU the shipped module refuses the device at probe and
//! loads nothing, and Rime blacklists nouveau (stage 1f), so NO driver binds.
//! What the person sees then depends on whether the machine has a second GPU:
//!
//! * an Intel or AMD GPU beside it (most laptops): the desktop runs on that
//!   GPU and the NVIDIA one simply goes unused;
//! * the NVIDIA card is the only GPU (a desktop): the screen runs on the
//!   framebuffer the firmware set up (simpledrm) — one output, the firmware's
//!   resolution, software rendering.
//!
//! Neither state is visible anywhere else. The recovery surface used to call
//! the second one a missing driver and recommend `sudo rime rollback`, which
//! cannot help: every Rime image carries the same branch.
//!
//! ## Where the ranges come from
//!
//! NVIDIA's own `supported-gpus/supported-gpus.json`, shipped inside each
//! driver's kernel-module source (RPM Fusion `xorg-x11-drv-nvidia-kmodsrc`).
//! Read 2026-10-08 from 615.71.09 (the branch Rime ships) and 580.178.04:
//!
//! * the lowest device id 615 supports is `0x1E02` (TITAN RTX, Turing);
//! * the 162 entries 615 tags `"legacybranch": "580.xx"` are exactly the ids
//!   in `0x1340..=0x1DF6`, and no id 615 supports falls inside that span;
//! * every id below `0x1340` is tagged with an older branch (470.xx and
//!   earlier: Kepler, Fermi, ...).
//!
//! So a device id below `0x1E00` is one the shipped driver cannot drive, and
//! `0x1340..0x1E00` is the part the 580 branch still covers. If a later
//! branch drops Turing, [`FIRST_SUPPORTED`] moves and the tests here say so.
//!
//! Pure apart from the sysfs reads in [`scan`], which take a root so the tests
//! drive it from a fixture tree.

use std::path::Path;

/// PCI vendor id of NVIDIA.
pub const NVIDIA: u16 = 0x10de;

/// Device ids at or above this are Turing or newer: the shipped branch.
pub const FIRST_SUPPORTED: u16 = 0x1e00;

/// The first Maxwell id (GM108, GeForce 830M). From here to
/// [`FIRST_SUPPORTED`] is the 580 legacy branch.
pub const FIRST_580: u16 = 0x1340;

/// Which NVIDIA driver branch a device id needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Branch {
    /// Turing or newer: the driver in the image drives it.
    Current,
    /// Maxwell, Pascal or Volta: the 580 legacy branch, which Rime does not ship.
    Legacy580,
    /// Kepler or older: a branch NVIDIA no longer updates (470.xx and before).
    Older,
}

impl Branch {
    pub fn for_device(device: u16) -> Branch {
        if device >= FIRST_SUPPORTED {
            Branch::Current
        } else if device >= FIRST_580 {
            Branch::Legacy580
        } else {
            Branch::Older
        }
    }
}

/// One NVIDIA display device the shipped driver cannot drive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported {
    /// `0000:01:00.0`.
    pub slot: String,
    pub device: u16,
    pub branch: Branch,
    /// The kernel driver bound to it, if any. Expected to be `None`; anything
    /// else is reported as found rather than assumed away.
    pub driver: Option<String>,
}

/// The NVIDIA picture of one machine.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// NVIDIA display devices the shipped driver cannot drive, in slot order.
    pub unsupported: Vec<Unsupported>,
    /// NVIDIA display devices the shipped driver does drive.
    pub supported: usize,
    /// Display devices from any other vendor (an Intel or AMD iGPU, a VM's
    /// virtual GPU). When there is one, the desktop has hardware to run on.
    pub other_display: usize,
}

fn read_trim(p: &Path) -> Option<String> {
    std::fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

fn hex16(s: &str) -> Option<u16> {
    let s = s.trim();
    u16::from_str_radix(s.strip_prefix("0x").unwrap_or(s), 16).ok()
}

/// Read every display-class PCI device under `<sys>/bus/pci/devices`.
///
/// PCI, not `/sys/class/drm`: a GPU no driver bound has no DRM card node, so
/// the DRM view misses exactly the device this module exists to find.
pub fn scan(sys: &Path) -> Report {
    let mut report = Report::default();
    let Ok(entries) = std::fs::read_dir(sys.join("bus/pci/devices")) else {
        return report;
    };
    let mut dirs: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    dirs.sort();
    for dir in dirs {
        // Class 0x03xxxx: VGA (0x0300), XGA, 3D controller (0x0302, which is
        // what a laptop's render-only dGPU reports), other display.
        let class = read_trim(&dir.join("class")).unwrap_or_default();
        if !class.starts_with("0x03") {
            continue;
        }
        let vendor = read_trim(&dir.join("vendor")).and_then(|s| hex16(&s));
        let device = read_trim(&dir.join("device")).and_then(|s| hex16(&s));
        let (Some(vendor), Some(device)) = (vendor, device) else {
            continue;
        };
        if vendor != NVIDIA {
            report.other_display += 1;
            continue;
        }
        match Branch::for_device(device) {
            Branch::Current => report.supported += 1,
            branch => report.unsupported.push(Unsupported {
                slot: dir
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default(),
                device,
                branch,
                driver: std::fs::read_link(dir.join("driver"))
                    .ok()
                    .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string())),
            }),
        }
    }
    report
}

impl Report {
    /// True when no display device has a driver Rime ships for it: the
    /// NVIDIA card is all there is.
    pub fn nothing_else_to_run_on(&self) -> bool {
        !self.unsupported.is_empty() && self.supported == 0 && self.other_display == 0
    }

    /// The sentence a person reads, or `None` when every NVIDIA GPU here is
    /// supported (or there is none).
    pub fn explain(&self) -> Option<String> {
        if self.unsupported.is_empty() {
            return None;
        }
        let which = self
            .unsupported
            .iter()
            .map(|u| format!("10de:{:04x} at {}", u.device, u.slot))
            .collect::<Vec<_>>()
            .join(", ");
        let generation = if self.unsupported.iter().all(|u| u.branch == Branch::Legacy580) {
            "a Maxwell, Pascal or Volta GPU (GeForce GTX 750 to GTX 1080 Ti, MX110 to \
             MX350, TITAN X and TITAN V). NVIDIA's driver dropped these after its 580 series"
        } else {
            "older than NVIDIA's current driver supports (Kepler or earlier, or a mix \
             with Maxwell/Pascal/Volta). NVIDIA's driver dropped these GPUs"
        };
        let bound = self
            .unsupported
            .iter()
            .filter_map(|u| u.driver.as_deref().map(|d| format!("{} is bound to {d}", u.slot)))
            .collect::<Vec<_>>();
        let state = if !bound.is_empty() {
            format!("Found anyway: {}.", bound.join(", "))
        } else if self.nothing_else_to_run_on() {
            "No driver is loaded for it, so the screen runs on the basic display the \
             firmware set up: one screen at a fixed resolution, with no graphics \
             acceleration. Games and 3D apps will be very slow or will not start."
                .to_string()
        } else if self.supported > 0 {
            "No driver is loaded for it. The other NVIDIA GPU in this machine is one \
             the driver in this image supports."
                .to_string()
        } else {
            "No driver is loaded for it, so it goes unused: the desktop, apps and \
             games run on the machine's other graphics."
                .to_string()
        };
        Some(format!(
            "NVIDIA GPU {which} is {generation}, and Rime OS ships only the current \
             driver. {state} A kernel driver cannot be added with `rime install`. \
             For NVIDIA acceleration on this GPU, use a system that offers NVIDIA's \
             580 legacy driver, or a GPU Rime supports: NVIDIA GeForce GTX 16 / RTX 20 \
             series or newer, AMD or Intel."
        ))
    }

    /// The `rime doctor` line: `(ok, sentence)`, or `None` on a machine with no
    /// NVIDIA GPU, where there is nothing to say.
    pub fn doctor_line(&self) -> Option<(bool, String)> {
        if let Some(s) = self.explain() {
            return Some((false, s));
        }
        (self.supported > 0).then(|| {
            (
                true,
                format!(
                    "NVIDIA GPU supported by the driver in this image ({} found)",
                    self.supported
                ),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Tree(PathBuf);

    impl Tree {
        fn new() -> Tree {
            static N: AtomicUsize = AtomicUsize::new(0);
            let p = std::env::temp_dir().join(format!(
                "rimed-nvsupport-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(p.join("bus/pci/devices")).unwrap();
            Tree(p)
        }

        fn dev(&self, slot: &str, class: &str, vendor: &str, device: &str) -> &Self {
            let d = self.0.join("bus/pci/devices").join(slot);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("class"), format!("{class}\n")).unwrap();
            std::fs::write(d.join("vendor"), format!("{vendor}\n")).unwrap();
            std::fs::write(d.join("device"), format!("{device}\n")).unwrap();
            self
        }

        fn bind(&self, slot: &str, driver: &str) -> &Self {
            let target = self.0.join("bus/pci/drivers").join(driver);
            std::fs::create_dir_all(&target).unwrap();
            std::os::unix::fs::symlink(
                &target,
                self.0.join("bus/pci/devices").join(slot).join("driver"),
            )
            .unwrap();
            self
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_boundaries_are_the_ones_nvidia_publishes() {
        // Last Volta id 615 tags 580.xx, first Turing id 615 supports.
        assert_eq!(Branch::for_device(0x1df6), Branch::Legacy580);
        assert_eq!(Branch::for_device(0x1dff), Branch::Legacy580);
        assert_eq!(Branch::for_device(0x1e00), Branch::Current);
        assert_eq!(Branch::for_device(0x1e02), Branch::Current);
        // GeForce 830M is the first 580.xx id; the GK208 ids just below it
        // are tagged 470.xx.
        assert_eq!(Branch::for_device(0x1340), Branch::Legacy580);
        assert_eq!(Branch::for_device(0x133f), Branch::Older);
        assert_eq!(Branch::for_device(0x12ba), Branch::Older);
    }

    /// Every device id NVIDIA lists, against the branch NVIDIA gives it. The
    /// ranges above are a summary of this table; this is what keeps them one.
    #[test]
    fn every_id_in_nvidias_own_table_agrees_with_the_ranges() {
        let table = include_str!("testdata/nvidia-615.71.09-branches.txt");
        let mut seen = 0;
        for line in table.lines().filter(|l| !l.starts_with('#') && !l.is_empty()) {
            let (id, branch) = line.split_once(' ').unwrap();
            let id = u16::from_str_radix(id, 16).unwrap();
            let want = match branch {
                "current" => Branch::Current,
                "580.xx" => Branch::Legacy580,
                _ => Branch::Older,
            };
            assert_eq!(Branch::for_device(id), want, "{id:#06x} is {branch} in NVIDIA's table");
            seen += 1;
        }
        assert!(seen > 1000, "the table was not read: {seen} ids");
    }

    #[test]
    fn named_cards_land_in_the_right_branch() {
        for (id, want) in [
            (0x1380, Branch::Legacy580), // GTX 750 Ti
            (0x13c2, Branch::Legacy580), // GTX 970
            (0x1b80, Branch::Legacy580), // GTX 1080
            (0x1c8d, Branch::Legacy580), // GTX 1050
            (0x1d81, Branch::Legacy580), // TITAN V
            (0x1f08, Branch::Current),   // RTX 2060
            (0x2520, Branch::Current),   // RTX 3060 Laptop
            (0x2684, Branch::Current),   // RTX 4090
            (0x0fc6, Branch::Older),     // GTX 650 (Kepler)
        ] {
            assert_eq!(Branch::for_device(id), want, "{id:#06x}");
        }
    }

    #[test]
    fn a_pascal_card_alone_says_the_screen_has_no_acceleration() {
        let t = Tree::new();
        t.dev("0000:01:00.0", "0x030000", "0x10de", "0x1b80");
        t.dev("0000:01:00.1", "0x040300", "0x10de", "0x10f0"); // its HDMI audio
        let r = scan(&t.0);
        assert_eq!(r.unsupported.len(), 1, "{r:?}");
        assert!(r.nothing_else_to_run_on());
        let (ok, s) = r.doctor_line().unwrap();
        assert!(!ok);
        assert!(s.contains("10de:1b80 at 0000:01:00.0"), "{s}");
        assert!(s.contains("Maxwell, Pascal or Volta"), "{s}");
        assert!(s.contains("no graphics acceleration"), "{s}");
        assert!(!s.contains("nouveau"), "Rime blacklists nouveau; never claim it: {s}");
    }

    /// files/system/libexec/rime-gpu-notice finds this line in `rime doctor`
    /// by its `[WARN] NVIDIA GPU ` prefix and takes the rest of that ONE line
    /// as the notification body.
    #[test]
    fn the_sentence_is_one_line_with_the_prefix_the_notice_greps_for() {
        let t = Tree::new();
        t.dev("0000:01:00.0", "0x030000", "0x10de", "0x1c03");
        t.dev("0000:02:00.0", "0x030000", "0x10de", "0x0fc6");
        let (_, s) = scan(&t.0).doctor_line().unwrap();
        assert!(s.starts_with("NVIDIA GPU "), "{s}");
        assert!(!s.contains('\n'), "{s}");
        assert!(s.contains("10de:1c03 at 0000:01:00.0, 10de:0fc6 at 0000:02:00.0"), "{s}");
    }

    #[test]
    fn a_pascal_laptop_dgpu_beside_intel_goes_unused_not_broken() {
        let t = Tree::new();
        t.dev("0000:00:02.0", "0x030000", "0x8086", "0x9bc4");
        t.dev("0000:01:00.0", "0x030200", "0x10de", "0x1c8d");
        let r = scan(&t.0);
        assert!(!r.nothing_else_to_run_on());
        let s = r.explain().unwrap();
        assert!(s.contains("goes unused"), "{s}");
        assert!(!s.contains("no graphics acceleration"), "{s}");
    }

    #[test]
    fn a_supported_card_gets_a_pass_and_no_explanation() {
        let t = Tree::new();
        t.dev("0000:00:02.0", "0x030000", "0x8086", "0x46a6");
        t.dev("0000:01:00.0", "0x030000", "0x10de", "0x2520");
        let r = scan(&t.0);
        assert_eq!(r.explain(), None);
        let (ok, s) = r.doctor_line().unwrap();
        assert!(ok, "{s}");
    }

    #[test]
    fn a_machine_with_no_nvidia_gpu_says_nothing() {
        let t = Tree::new();
        t.dev("0000:c4:00.0", "0x030000", "0x1002", "0x15bf");
        assert_eq!(scan(&t.0).doctor_line(), None);
        // and a missing PCI tree is not a GPU either
        assert_eq!(scan(Path::new("/nonexistent-rime-sys")).doctor_line(), None);
    }

    #[test]
    fn a_kepler_card_is_not_called_pascal() {
        let t = Tree::new();
        t.dev("0000:01:00.0", "0x030000", "0x10de", "0x0fc6");
        let s = scan(&t.0).explain().unwrap();
        assert!(s.contains("Kepler or earlier"), "{s}");
        assert!(!s.contains("after its 580 series"), "{s}");
    }

    #[test]
    fn an_old_card_beside_a_supported_one_names_the_working_one() {
        let t = Tree::new();
        t.dev("0000:01:00.0", "0x030000", "0x10de", "0x2684");
        t.dev("0000:02:00.0", "0x030000", "0x10de", "0x1b06");
        let r = scan(&t.0);
        assert_eq!((r.supported, r.unsupported.len()), (1, 1));
        let s = r.explain().unwrap();
        assert!(s.contains("10de:1b06 at 0000:02:00.0"), "{s}");
        assert!(s.contains("other NVIDIA GPU"), "{s}");
    }

    #[test]
    fn a_driver_that_is_bound_is_reported_not_assumed_away() {
        let t = Tree::new();
        t.dev("0000:01:00.0", "0x030000", "0x10de", "0x1c03");
        t.bind("0000:01:00.0", "vfio-pci");
        let r = scan(&t.0);
        assert_eq!(r.unsupported[0].driver.as_deref(), Some("vfio-pci"));
        let s = r.explain().unwrap();
        assert!(s.contains("0000:01:00.0 is bound to vfio-pci"), "{s}");
        assert!(!s.contains("No driver is loaded"), "{s}");
    }

    #[test]
    fn non_display_nvidia_functions_are_not_gpus() {
        let t = Tree::new();
        t.dev("0000:01:00.1", "0x040300", "0x10de", "0x10f1"); // audio
        t.dev("0000:01:00.2", "0x0c0330", "0x10de", "0x1ad6"); // USB-C
        assert_eq!(scan(&t.0), Report::default());
    }
}
