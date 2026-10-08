//! Rime OS installer for Windows.
//!
//! With no arguments it opens the installer window. The commands below are
//! the same installer without the window: the lab drives them, and they are
//! what a person reaching for a terminal gets. Every command that changes
//! anything says so in its name (`install`, `undo`, `stage-image`); the rest
//! only read.
#![cfg_attr(windows, windows_subsystem = "windows")]

use rime_windows_installer::{Scan, enumerate, lab_policy, open_image, scan};
use std::{env, io, path::Path, process::ExitCode};

const USAGE: &str = "\
rime-windows-installer -- install Rime OS beside Windows

  rime-windows-installer                 open the installer window
  rime-windows-installer candidates      where Rime could go, and why not elsewhere
  rime-windows-installer install ID [--iso FILE] [--yes] [--restart]
                                         prepare the disk and start Rime's installer
                                         on the next restart (ID from `candidates`)
  rime-windows-installer undo [--yes]    remove what `install` added
  rime-windows-installer download        fetch and check the installer image only
  rime-windows-installer survey          every disk, its identity, its partitions
                                         (read-only)
  rime-windows-installer inspect GUID    one partition, every byte read (read-only)
  rime-windows-installer lab FILE.img    the offline GPT laboratory (read-only)
  rime-windows-installer stage-image FILE.img --iso FILE --space free:FIRST-LAST|part:GUID
                                         [--bitlocker] [--bootnum HEX]
                                         developer: the install's disk writes,
                                         applied to a disk image file

Nothing is written to a disk or a firmware variable except by install,
undo and stage-image, and install changes nothing until you confirm.";

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("lab") if args.len() == 2 => lab(&args[1]),
        Some("survey") if args.len() == 1 => survey(),
        Some("inspect") if args.len() == 2 => inspect(&args[1]),
        Some("stage-image") => stage_image(&args[1..]),
        #[cfg(windows)]
        Some("candidates") if args.len() == 1 => cli::candidates(),
        #[cfg(windows)]
        Some("install") => cli::install(&args[1..]),
        #[cfg(windows)]
        Some("undo") => cli::undo(&args[1..]),
        #[cfg(windows)]
        Some("download") if args.len() == 1 => cli::download(),
        Some("--help" | "-h" | "help") => {
            println!("{USAGE}");
            Ok(())
        }
        _ => Err(USAGE.into()),
    }
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

/// The install's disk writes, against a disk image. Exists so the whole
/// staging path (plan, payload, read-back, partition table) runs on Linux
/// against images that sfdisk, fsck.fat and a real UEFI boot can then judge.
fn stage_image(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    use rime_windows_installer::{bootentry, gptwrite, random_bytes, random_guid, stage};
    let img = args.first().ok_or("stage-image needs an image file")?;
    if !(img.ends_with(".img") || img.ends_with(".raw")) {
        return Err("stage-image writes only to .img or .raw files".into());
    }
    let iso_path = flag(args, "--iso").ok_or("--iso FILE is required")?;
    let space = match flag(args, "--space").ok_or("--space is required")? {
        s if s.starts_with("free:") => {
            let (a, b) = s[5..].split_once('-').ok_or("--space free:FIRST-LAST")?;
            stage::Space::Free { first_lba: a.parse()?, last_lba: b.parse()? }
        }
        s if s.starts_with("part:") => stage::Space::Replace { partition_guid: s[5..].to_ascii_lowercase() },
        _ => return Err("--space free:FIRST-LAST or part:GUID".into()),
    };
    let bootnum = u16::from_str_radix(flag(args, "--bootnum").unwrap_or("0009"), 16)?;
    let meta = std::fs::symlink_metadata(img)?;
    if !meta.file_type().is_file() {
        return Err("stage-image writes only to a regular file".into());
    }
    let mut disk = std::fs::OpenOptions::new().read(true).write(true).open(img)?;
    let before = gptwrite::GptSnapshot::read(&mut disk, meta.len())?;
    let mut iso = std::fs::File::open(iso_path)?;
    let payload = stage::locate_payload(&mut iso)?;
    let id = random_bytes(4)?;
    let esp_guid = random_guid()?;
    let plan = stage::plan(&stage::Inputs {
        before: &before,
        space,
        payload: &payload,
        esp_guid: esp_guid.clone(),
        root_guid: random_guid()?,
        fat_volume_id: u32::from_le_bytes([id[0], id[1], id[2], id[3]]),
        boot_number: bootnum,
        windows_bitlocker: args.iter().any(|a| a == "--bitlocker"),
        created: "stage-image".into(),
        app_version: concat!("rime-windows-installer ", env!("CARGO_PKG_VERSION")),
    })?;
    let mut pr = |_: stage::Phase, _: u64, _: u64| {};
    stage::table_unchanged(&plan, &mut disk)?;
    let hashes = stage::write_payload(&plan, &mut disk, &mut iso, &mut pr)?;
    stage::verify_payload(&plan, &mut disk, &hashes, &mut pr)?;
    stage::table_unchanged(&plan, &mut disk)?;
    stage::write_table(&plan, &mut disk, &mut pr)?;
    stage::table_committed(&plan, &mut disk)?;
    let opt = bootentry::encode(
        bootentry::SETUP_DESCRIPTION,
        &bootentry::HardDrive {
            partition_number: plan.esp_slot,
            start_lba: plan.esp.first_lba,
            size_lba: plan.esp.last_lba - plan.esp.first_lba + 1,
            guid: gptwrite::guid_bytes(&esp_guid)?,
        },
        bootentry::SETUP_LOADER,
    )?;
    println!("ESP_PARTUUID={}", plan.esp.unique_guid);
    println!("ROOT_PARTUUID={}", plan.root.unique_guid);
    println!("ESP_OFFSET={}", plan.esp_offset());
    println!("ESP_SLOT={}", plan.esp_slot);
    println!("BOOTNUM={bootnum:04X}");
    println!("LOADOPTION={}", rime_windows_installer::payload::hex(&opt));
    println!("STAGED-OK");
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
//  The offline laboratory, unchanged: it is what proves the GPT reader and the
//  content scanner against fixtures that can be built anywhere, including on
//  the Linux machine this is developed on.
// ─────────────────────────────────────────────────────────────────────────────
fn lab(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut image = open_image(Path::new(path))?;
    let original_len = image.metadata()?.len();
    let layout = enumerate(&mut image)?;
    println!(
        "READ-ONLY IMAGE LAB -- no installation available\nDisk GPT GUID: {}\nModel/serial: unavailable (image fixture, not hardware)",
        layout.disk_id
    );
    for p in &layout.partitions {
        println!(
            "{} | {} bytes | GPT name {:?} | filesystem label: not probed | type {} | offset {} | attributes {:#x}",
            p.id, p.length, p.name, p.kind, p.offset, p.attributes
        );
    }
    println!("Select a partition by typing its full GPT GUID (blank cancels):");
    let mut selection = String::new();
    io::stdin().read_line(&mut selection)?;
    let selected = layout
        .partitions
        .iter()
        .find(|p| p.id == selection.trim())
        .ok_or("no exact partition selected; refusing")?;
    lab_policy(selected)?;
    println!(
        "Checks passed: both GPT CRCs, matching tables and table CRC, bounds, unique GUIDs, no overlaps, Linux type, zero attributes.\nScanning all {} bytes; this may take a long time.",
        selected.length
    );
    let result = scan(&mut image, selected.offset, selected.length)?;
    if image.metadata()?.len() != original_len || enumerate(&mut image)? != layout {
        return Err("image layout changed during inspection".into());
    }
    match result {
        Scan::AllZero { bytes_read } => println!(
            "ALL-ZERO CONTENT: {bytes_read}/{bytes_read} bytes read. No nonzero filesystem, encryption, RAID or nested partition-table bytes observed in this extent.\nThis is not installation authorization: ownership, locking and stable hardware identity are NOT verified. No writes performed."
        ),
        Scan::Nonzero { bytes_read, relative_offset, value } => {
            return Err(format!(
                "NOT EMPTY: nonzero byte {value:#04x} at partition offset {relative_offset} (image offset {}); {bytes_read} bytes read. No writes performed.",
                selected.offset + relative_offset
            )
            .into());
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
//  Windows
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(not(windows))]
fn survey() -> Result<(), Box<dyn std::error::Error>> {
    Err("survey needs Windows storage APIs; this build is not for Windows".into())
}
#[cfg(not(windows))]
fn inspect(_: &str) -> Result<(), Box<dyn std::error::Error>> {
    Err("inspect needs Windows storage APIs; this build is not for Windows".into())
}

#[cfg(windows)]
mod win {
    use rime_windows_installer::plan::{self, DiskIdentity, Verdict};
    use rime_windows_installer::windows as w;
    use rime_windows_installer::{Scan, scan};

    use rime_windows_installer::winstall::{facts, surveyed};

    pub fn print_survey() -> Result<(), Box<dyn std::error::Error>> {
        let (disks, vols, problems) = surveyed();
        println!("Rime WINDOWS INSTALLER -- READ-ONLY SURVEY. Nothing is written.");
        println!("disks-found: {}", disks.len());
        for d in &disks {
            println!("\nDISK {}", d.identity.gpt_disk_guid);
            println!("  identity   {}", describe(&d.identity));
            println!("  reached by {} (an enumeration artefact, not an identity)", d.path);
            println!("  sector     {} bytes", d.identity.sector_size);
            match &d.agreement {
                Ok(()) => println!(
                    "  agreement  the on-disk GPT and Windows' partition table AGREE ({} partitions)",
                    d.partitions.len()
                ),
                Err(e) => println!("  agreement  DISAGREE -- {e}"),
            }
            for p in &d.partitions {
                let claims = vols.claims(d.number, p.offset, p.length);
                let verdict = plan::assess(p, &claims);
                println!(
                    "  PARTITION {id}\n    {size}  {tname}  name {name:?}  attributes {attr:#x}\n    bytes {start}..{end}",
                    id = p.id,
                    size = plan::human(p.length),
                    tname = plan::type_name(&p.type_guid),
                    name = p.name,
                    attr = p.attributes,
                    start = p.offset,
                    end = p.offset + p.length,
                );
                if claims.is_empty() {
                    println!("    windows-claims: none");
                } else {
                    for c in &claims {
                        println!("    windows-claims: {}", c.what);
                    }
                }
                match verdict {
                    Verdict::ContentCheckAllowed => println!(
                        "    VERDICT: may be content-checked. Run: rime-windows-installer inspect {}",
                        p.id
                    ),
                    Verdict::Refused(r) => {
                        for line in r.to_string().lines() {
                            println!("    {line}");
                        }
                    }
                }
            }
        }
        if !problems.is_empty() {
            println!("\nCOULD NOT BE READ -- these are reported, not ignored:");
            for p in &problems {
                println!("  {p}");
            }
        }
        println!("\nsurvey-complete");
        Ok(())
    }

    fn describe(d: &DiskIdentity) -> String {
        format!(
            "{} / {} / {} bus, {}",
            if d.model.is_empty() { "(no model reported)" } else { &d.model },
            if d.serial.is_empty() { "(no serial reported)" } else { &d.serial },
            d.bus,
            plan::human(d.length)
        )
    }

    pub fn print_inspect(guid: &str) -> Result<(), Box<dyn std::error::Error>> {
        let guid = guid.trim().to_ascii_lowercase();
        let (disks, vols, problems) = surveyed();

        // A GUID is supposed to be unique across the machine. If it is not,
        // that is a cloned disk, and picking either one is guessing.
        let hits: Vec<_> = disks
            .iter()
            .flat_map(|d| d.partitions.iter().map(move |p| (d, p)))
            .filter(|(_, p)| p.id == guid)
            .collect();
        if hits.is_empty() {
            return Err(format!(
                "no partition on this machine has GPT GUID {guid}. {} disk(s) were read; {} could not be.",
                disks.len(),
                problems.len()
            )
            .into());
        }
        if hits.len() > 1 {
            return Err(format!(
                "GPT GUID {guid} appears on {} partitions. A duplicated partition GUID means a cloned disk; refusing to guess which one you meant.",
                hits.len()
            )
            .into());
        }
        let (disk, part) = hits[0];

        println!("INSPECTING {guid} -- READ-ONLY. Nothing is written.");
        println!("disk  {}", describe(&disk.identity));
        println!("disk-guid {}", disk.identity.gpt_disk_guid);
        match &disk.agreement {
            Ok(()) => println!("agreement the on-disk GPT and Windows' partition table AGREE"),
            Err(e) => return Err(e.clone().into()),
        }

        let claims = vols.claims(disk.number, part.offset, part.length);
        match plan::assess(part, &claims) {
            Verdict::Refused(r) => {
                println!("{r}");
                return Err("this partition is not a candidate".into());
            }
            Verdict::ContentCheckAllowed => {}
        }

        // Exclusivity, re-asked rather than remembered.
        //
        // There is deliberately no FSCTL_LOCK_VOLUME here, and the reason is
        // a finding rather than an omission: Windows creates NO VOLUME OBJECT
        // for a Linux-filesystem-type partition. Measured in the lab — the
        // eligible fixture partitions have no volume, no drive letter and
        // nothing to open. And a partition that DOES have a volume has
        // already been refused by `assess` above, because an overlapping
        // volume is a Claim. So a lock call on this path could only ever run
        // on a partition that was already refused: it would be safety code
        // that never executes, which reads as coverage and is worse than
        // none.
        //
        // What this does instead is re-enumerate the volumes NOW, immediately
        // before the content is read, rather than trusting the survey's
        // answer. Between the survey and here a service can rescan, a user
        // can act in Disk Management, or removable media can appear.
        let (fresh, problems) = w::volumes();
        let now: Vec<_> = fresh
            .iter()
            .filter(|v| v.overlaps(disk.number, part.offset, part.length))
            .collect();
        if !now.is_empty() {
            return Err(format!(
                "Windows has taken this partition since the survey: {}",
                now.iter().map(|v| v.describe_use()).collect::<Vec<_>>().join("; ")
            )
            .into());
        }
        if !problems.is_empty() {
            return Err(format!(
                "the volume list could not be read completely, so 'Windows is not using this' cannot be established: {}",
                problems.join("; ")
            )
            .into());
        }
        println!(
            "exclusivity no volume object covers this partition, re-checked against a fresh volume enumeration. Windows has not mounted it and has nothing here to lock."
        );

        println!("scanning  every one of the {} bytes", part.length);
        let mut dev = w::Device::open(&disk.path)?;
        let result = scan(&mut dev, part.offset, part.length)?;

        // Re-read the table afterwards. A layout that changed under the scan
        // means the answer describes bytes that are no longer where they were.
        let dev2 = w::Device::open(&disk.path)?;
        let (guid2, parts2) = w::layout(&dev2)?;
        if guid2 != disk.identity.gpt_disk_guid || facts(&parts2) != disk.partitions {
            return Err(
                "the partition table changed while its contents were being read".into()
            );
        }

        match result {
            Scan::Nonzero { bytes_read, relative_offset, value } => Err(format!(
                "NOT EMPTY: nonzero byte {value:#04x} at partition offset {relative_offset} (disk offset {}); {bytes_read} bytes read. No writes performed.",
                part.offset + relative_offset
            )
            .into()),
            Scan::AllZero { bytes_read } => {
                println!("ALL-ZERO CONTENT: {bytes_read}/{bytes_read} bytes read.");
                println!("\n{}\n", plan::confirmation_text(&disk.identity, part, None, bytes_read));
                println!(
                    "INSTALLATION IS NOT IMPLEMENTED IN THIS BUILD. Nothing was written, no firmware variable was changed, and no confirmation was requested."
                );
                Ok(())
            }
        }
    }
}

#[cfg(windows)]
fn survey() -> Result<(), Box<dyn std::error::Error>> {
    win::print_survey()
}
#[cfg(windows)]
fn inspect(guid: &str) -> Result<(), Box<dyn std::error::Error>> {
    win::print_inspect(guid)
}

#[cfg(windows)]
mod cli {
    use rime_windows_installer::plan;
    use rime_windows_installer::winstall::{self, Event};
    use std::io::Write;
    use std::path::Path;

    pub fn candidates() -> Result<(), Box<dyn std::error::Error>> {
        let (c, problems) = winstall::candidates();
        println!("needed: {}", plan::human(winstall::needed_bytes()));
        for x in &c {
            println!("CANDIDATE {}\n  {}\n  {}", x.id, x.what, plan::human(x.bytes));
            match &x.refusal {
                None => println!("  usable"),
                Some(r) => println!("  REFUSED: {r}"),
            }
        }
        for p in &problems {
            println!("PROBLEM {p}");
        }
        if !c.iter().any(|x| x.refusal.is_none()) {
            println!("{}", plan::NO_SPACE_HELP);
        }
        println!("candidates-complete");
        Ok(())
    }

    fn print_event(e: Event) {
        match e {
            Event::Step(s) => println!("STEP {s}"),
            Event::Note(s) => println!("NOTE {s}"),
            Event::Progress { what, done, total } => {
                // A line per 5 %, not per megabyte: this goes to a log.
                thread_local!(static LAST: std::cell::Cell<(&'static str, u64)> = const { std::cell::Cell::new(("", u64::MAX)) });
                let step = done.saturating_mul(20).checked_div(total).unwrap_or(20);
                if LAST.with(|l| l.replace((what, step))) != (what, step) {
                    println!("PROGRESS {what} {}%", step * 5);
                }
            }
        }
        let _ = std::io::stdout().flush();
    }

    pub fn install(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
        let id = args.first().ok_or("install needs a candidate ID (see `candidates`)")?;
        let (c, _) = winstall::candidates();
        let pick = c.iter().find(|x| &x.id == id).ok_or("no such candidate; run `candidates` again")?;
        if let Some(r) = &pick.refusal {
            return Err(format!("that space cannot be used: {r}").into());
        }
        println!("{}", plan::install_confirmation(&pick.disk, &pick.what, pick.bytes));
        if !args.iter().any(|a| a == "--yes") {
            print!("Type INSTALL to continue: ");
            std::io::stdout().flush()?;
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            if line.trim() != "INSTALL" {
                return Err("not confirmed; nothing was changed".into());
            }
        }
        let iso = super::flag(args, "--iso").map(Path::new);
        let out = winstall::install(pick, iso, &mut print_event)?;
        println!("BOOTNUM={:04X}", out.boot_number);
        println!("ESP_PARTUUID={}", out.esp_guid);
        println!("ROOT_PARTUUID={}", out.root_guid);
        if out.bitlocker_suspended {
            println!("NOTE BitLocker is suspended until Windows next starts.");
        }
        println!("INSTALL-STAGED-OK");
        println!("Restart to continue: Rime OS's installer starts once, by itself.");
        if args.iter().any(|a| a == "--restart") {
            rime_windows_installer::winwrite::restart()?;
        }
        Ok(())
    }

    pub fn download() -> Result<(), Box<dyn std::error::Error>> {
        let p = winstall::obtain_iso(None, &mut print_event)?;
        println!("IMAGE {}", p.display());
        println!("DOWNLOAD-OK");
        Ok(())
    }

    pub fn undo(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
        println!("{}", winstall::undo_preview()?);
        if !args.iter().any(|a| a == "--yes") {
            print!("Type UNDO to continue: ");
            std::io::stdout().flush()?;
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            if line.trim() != "UNDO" {
                return Err("not confirmed; nothing was changed".into());
            }
        }
        winstall::undo(&mut print_event)?;
        println!("UNDO-OK");
        Ok(())
    }
}

fn main() -> ExitCode {
    #[cfg(windows)]
    {
        if env::args().len() == 1 {
            return rime_windows_installer::gui::run();
        }
        // A GUI-subsystem program has no console of its own; borrow the one
        // it was started from so the commands can print.
        rime_windows_installer::gui::attach_parent_console();
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("REFUSED: {e}");
            ExitCode::FAILURE
        }
    }
}
