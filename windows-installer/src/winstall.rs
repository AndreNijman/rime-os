//! The install, start to finish, on a Windows machine.
//!
//! Shared by the command line and the window: both call `candidates()` to
//! find where Rime can go, `install()` to put it there, and `undo()` to take
//! it away again. Every rule that decides anything lives in the pure modules
//! (`plan`, `stage`, `gptwrite`, `bootentry`, `payload`) where it is tested on
//! every CI run; this file is the order in which they are applied to a real
//! machine, and the re-checks between the steps.

#![cfg(windows)]

use crate::bootentry::{self, HardDrive};
use crate::gptwrite::{self, GptSnapshot};
use crate::plan::{self, Claim, DiskIdentity, PartitionFacts};
use crate::stage::{self, Phase, Plan, Space};
use crate::windows as w;
use crate::winwrite;
use crate::{enumerate_in, pin, random_bytes, random_guid};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub const APP_VERSION: &str = concat!("rime-windows-installer ", env!("CARGO_PKG_VERSION"));

/// One disk, with both readings of its partition table.
pub struct Surveyed {
    pub identity: DiskIdentity,
    pub path: String,
    pub number: u32,
    pub removable: bool,
    pub partitions: Vec<PartitionFacts>,
    pub agreement: Result<(), String>,
}

pub struct Volumes(pub Vec<w::Volume>);

impl Volumes {
    pub fn claims(&self, disk: u32, offset: u64, length: u64) -> Vec<Claim> {
        self.0
            .iter()
            .filter(|v| v.overlaps(disk, offset, length))
            .map(|v| Claim { what: v.describe_use() })
            .collect()
    }
}

pub fn facts(v: &[w::WinPartition]) -> Vec<PartitionFacts> {
    v.iter()
        .map(|p| PartitionFacts {
            id: p.id.clone(),
            type_guid: p.type_guid.clone(),
            name: p.name.clone(),
            offset: p.offset,
            length: p.length,
            attributes: p.attributes,
        })
        .collect()
}

/// Reading a disk fails for ordinary reasons; those are REPORTED, never
/// dropped: a survey that silently loses a disk can call a machine safe when
/// it is not.
pub fn surveyed() -> (Vec<Surveyed>, Volumes, Vec<String>) {
    let (drives, mut problems) = w::drives();
    let (vols, vproblems) = w::volumes();
    problems.extend(vproblems);
    let mut out = Vec::new();
    for d in drives {
        let Some(number) = w::drive_number(&d.path) else {
            problems.push(format!("{}: cannot determine the disk number", d.path));
            continue;
        };
        let mut dev = match w::Device::open(&d.path) {
            Ok(v) => v,
            Err(e) => {
                problems.push(format!("{}: {e}", d.path));
                continue;
            }
        };
        let (win_disk_guid, win_parts) = match w::layout(&dev) {
            Ok(v) => v,
            Err(e) => {
                problems.push(format!("{} ({}): {e}", d.describe(), d.path));
                continue;
            }
        };
        let agreement = match enumerate_in(&mut dev, d.length) {
            Ok(gpt) if gpt.disk_id != win_disk_guid => Err(format!(
                "the disk GUID in the GPT is {} but Windows reports {win_disk_guid}",
                gpt.disk_id
            )),
            Ok(gpt) => plan::cross_check(
                &gpt.partitions
                    .iter()
                    .map(|p| PartitionFacts {
                        id: p.id.clone(),
                        type_guid: p.kind.clone(),
                        name: p.name.clone(),
                        offset: p.offset,
                        length: p.length,
                        attributes: p.attributes,
                    })
                    .collect::<Vec<_>>(),
                &facts(&win_parts),
            ),
            Err(e) => Err(format!("the on-disk GPT could not be read independently: {e}")),
        };
        out.push(Surveyed {
            identity: DiskIdentity {
                model: d.model.clone(),
                serial: d.serial.clone(),
                bus: d.bus.clone(),
                length: d.length,
                sector_size: d.bytes_per_sector,
                gpt_disk_guid: win_disk_guid,
            },
            path: d.path.clone(),
            number,
            removable: d.removable,
            partitions: facts(&win_parts),
            agreement,
        });
    }
    (out, Volumes(vols), problems)
}

pub fn describe_disk(d: &DiskIdentity) -> String {
    // NVMe controllers report fixed-width, space-padded strings.
    let tidy = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let (model, serial) = (tidy(&d.model), tidy(&d.serial));
    format!(
        "{} (serial {}, {}, {})",
        if model.is_empty() { "disk with no model name" } else { &model },
        if serial.is_empty() { "not reported" } else { &serial },
        d.bus,
        plan::human(d.length)
    )
}

/// A place Rime could go, or a place that was looked at and refused.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// Stable across runs: `free:<disk guid>:<first lba>` or `part:<partition guid>`.
    pub id: String,
    pub disk: DiskIdentity,
    pub disk_path: String,
    pub disk_number: u32,
    pub space: Space,
    pub bytes: u64,
    pub what: String,
    pub refusal: Option<String>,
}

/// The space Rime needs: its own ESP (sized for the pinned image, which is
/// larger than the files taken from it) plus the smallest root offered.
pub fn needed_bytes() -> u64 {
    let iso = pin::pin().map(|p| p.bytes).unwrap_or(2_000_000_000);
    stage::esp_size_bytes(iso) + stage::MIN_ROOT_BYTES + 2 * stage::MIB
}

fn read_at(dev: &mut w::Device, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    dev.seek(SeekFrom::Start(offset))?;
    let mut b = vec![0u8; len];
    dev.read_exact(&mut b)?;
    Ok(b)
}

/// The three places `plan::signature` looks.
fn probe(dev: &mut w::Device, offset: u64, length: u64) -> io::Result<Option<&'static str>> {
    let mib = stage::MIB;
    let first = read_at(dev, offset, mib.min(length) as usize)?;
    let at64 = if length >= 64 * mib + 65536 { read_at(dev, offset + 64 * mib, 65536)? } else { Vec::new() };
    let last = if length >= 2 * mib { read_at(dev, offset + length - mib, mib as usize)? } else { Vec::new() };
    Ok(plan::signature(&first, &at64, &last))
}

pub fn candidates() -> (Vec<Candidate>, Vec<String>) {
    let (disks, vols, mut problems) = surveyed();
    let need = needed_bytes();
    let mut out = Vec::new();
    let os_disks: Vec<u32> = vols
        .0
        .iter()
        .filter(|v| v.mount_points.iter().any(|m| m.eq_ignore_ascii_case("C:\\")))
        .flat_map(|v| v.extents.iter().map(|e| e.0))
        .collect();
    for d in &disks {
        if let Err(e) = &d.agreement {
            problems.push(format!("{}: skipped, its two partition tables disagree: {e}", describe_disk(&d.identity)));
            continue;
        }
        if d.identity.sector_size != 512 {
            problems.push(format!("{}: skipped, {}-byte sectors are not supported", describe_disk(&d.identity), d.identity.sector_size));
            continue;
        }
        if d.removable {
            continue;
        }
        let mut dev = match w::Device::open(&d.path) {
            Ok(v) => v,
            Err(e) => {
                problems.push(format!("{}: {e}", describe_disk(&d.identity)));
                continue;
            }
        };
        let snap = match GptSnapshot::read(&mut dev, d.identity.length) {
            Ok(s) => s,
            Err(e) => {
                problems.push(format!("{}: {e}", describe_disk(&d.identity)));
                continue;
            }
        };
        let on_os_disk = if os_disks.contains(&d.number) { ", the disk Windows runs from" } else { "" };
        for (a, b) in snap.free_regions(2048, 2048) {
            let bytes = (b - a + 1) * 512;
            if bytes < 1024 * stage::MIB {
                continue;
            }
            out.push(Candidate {
                id: format!("free:{}:{a}", d.identity.gpt_disk_guid),
                disk: d.identity.clone(),
                disk_path: d.path.clone(),
                disk_number: d.number,
                space: Space::Free { first_lba: a, last_lba: b },
                bytes,
                what: format!("Unallocated space on {}{on_os_disk}", describe_disk(&d.identity)),
                refusal: (bytes < need).then(|| format!("{} is too small; Rime needs {}", plan::human(bytes), plan::human(need))),
            });
        }
        for p in &d.partitions {
            let claims = vols.claims(d.number, p.offset, p.length);
            // Partitions Windows needs, and volumes holding a filesystem
            // Windows recognises, are not offered at all: listing C: or a
            // USB stick as "refused" only invites the question. What is
            // listed with a refusal is the near-miss a person may have
            // made on purpose: an unformatted partition Windows lettered.
            let mounted_fs = vols.0.iter().any(|v| v.overlaps(d.number, p.offset, p.length) && !v.filesystem.is_empty());
            if matches!(p.type_guid.as_str(), plan::EFI_SYSTEM | plan::MICROSOFT_RESERVED | plan::WINDOWS_RECOVERY) || mounted_fs {
                continue;
            }
            let refusal = replace_refusal(p, &claims, need, || probe(&mut dev, p.offset, p.length));
            out.push(Candidate {
                id: format!("part:{}", p.id),
                disk: d.identity.clone(),
                disk_path: d.path.clone(),
                disk_number: d.number,
                space: Space::Replace { partition_guid: p.id.clone() },
                bytes: p.length,
                what: format!(
                    "{} partition {} on {}{on_os_disk}",
                    if refusal.is_none() { "Empty" } else { "Unformatted" },
                    if p.name.is_empty() { "(unnamed)".to_string() } else { format!("{:?}", p.name) },
                    describe_disk(&d.identity)
                ),
                refusal,
            });
        }
    }
    (out, problems)
}

/// Whether an existing partition may be replaced by Rime's two. `None` = yes.
pub fn replace_refusal(
    p: &PartitionFacts,
    claims: &[Claim],
    need: u64,
    probe: impl FnOnce() -> io::Result<Option<&'static str>>,
) -> Option<String> {
    if !claims.is_empty() {
        return Some(format!(
            "Windows is using it ({}). If it is empty, delete it in Disk Management so it becomes unallocated space, then refresh",
            claims.iter().map(|c| c.what.as_str()).collect::<Vec<_>>().join("; ")
        ));
    }
    match p.type_guid.as_str() {
        plan::WINDOWS_BASIC_DATA | plan::LINUX_FILESYSTEM => {}
        other => return Some(format!("its type ({}) is not one Rime replaces", plan::type_name(other))),
    }
    // Bit 0 is "required partition": the platform said it needs this.
    if p.attributes & 1 != 0 {
        return Some("it is marked as required by the platform".to_string());
    }
    if p.length < need {
        return Some(format!("{} is too small; Rime needs {}", plan::human(p.length), plan::human(need)));
    }
    match probe() {
        Ok(None) => None,
        Ok(Some(fs)) => Some(format!(
            "it holds {fs} data. Rime never overwrites a filesystem; if you no longer need it, delete it in Disk Management, then refresh"
        )),
        Err(e) => Some(format!("it could not be read to check it is empty ({e})")),
    }
}

/// Where the program keeps the image it downloaded, its journal and the
/// partition-table backups: %ProgramData%\Rime\Installer.
pub fn data_dir() -> io::Result<PathBuf> {
    let base = std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\ProgramData"));
    let d = base.join("Rime").join("Installer");
    fs::create_dir_all(&d)?;
    Ok(d)
}

#[derive(Debug, Clone)]
pub enum Event {
    Step(String),
    Progress { what: &'static str, done: u64, total: u64 },
    Note(String),
}

pub fn sha256_file(path: &Path, progress: &mut dyn FnMut(u64, u64)) -> io::Result<String> {
    let mut f = File::open(path)?;
    let total = f.metadata()?.len();
    let mut h = crate::payload::Sha256::new();
    let mut buf = vec![0u8; 4 << 20];
    let mut done = 0;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        done += n as u64;
        progress(done, total);
    }
    Ok(crate::payload::hex(&h.finalize()))
}

fn pin_bytes() -> u64 {
    pin::pin().map(|p| p.bytes).unwrap_or(u64::MAX)
}

/// The pinned installer image, from `given`, from beside the program, from
/// the cache, or downloaded.
/// Whatever its origin it is used only if it hashes to the pin.
pub fn obtain_iso(given: Option<&Path>, ev: &mut dyn FnMut(Event)) -> io::Result<PathBuf> {
    let p = pin::pin().map_err(io::Error::other)?;
    // An image already beside the program (downloaded separately, or carried
    // on a stick to a machine without internet) is used when its size is
    // right; like any other copy it is used only if it hashes to the pin.
    let beside = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join(pin::ISO_FILE_NAME)))
        .filter(|p| fs::metadata(p).map(|m| m.len() == pin_bytes()).unwrap_or(false));
    // Only a file this program downloaded is ever deleted; a file the user
    // supplied is theirs, wrong or not.
    let supplied = given.map(Path::to_path_buf).or(beside);
    let ours = supplied.is_none();
    let path = match supplied {
        Some(g) => g,
        None => {
            let dest = data_dir()?.join(pin::ISO_FILE_NAME);
            ev(Event::Step("Downloading the Rime OS installer".into()));
            let mut last_err = None;
            for attempt in 1..=5 {
                match crate::net::download(&p.url, &dest, p.bytes, &mut |d, t| {
                    ev(Event::Progress { what: "download", done: d, total: t })
                }) {
                    Ok(()) => {
                        last_err = None;
                        break;
                    }
                    Err(e) => {
                        ev(Event::Note(format!("download attempt {attempt} stopped: {e}")));
                        last_err = Some(e);
                    }
                }
            }
            if let Some(e) = last_err {
                return Err(e);
            }
            dest
        }
    };
    ev(Event::Step("Checking the installer image".into()));
    let len = fs::metadata(&path)?.len();
    if len != p.bytes {
        return Err(io::Error::other(format!("{} is {len} bytes, not the {} the pinned image has", path.display(), p.bytes)));
    }
    let got = sha256_file(&path, &mut |d, t| ev(Event::Progress { what: "verify image", done: d, total: t }))?;
    if got != p.sha256 {
        if ours {
            let _ = fs::remove_file(&path);
        }
        return Err(io::Error::other(format!(
            "the installer image does not match the one this program was built for (SHA-256 {got}, expected {}); it was not used",
            p.sha256
        )));
    }
    Ok(path)
}

/// BitLocker on the volume Windows runs from, asked of Windows itself
/// (Win32_EncryptableVolume, which answers in numbers, not in the display
/// language). `pcrs` is the TPM protector's platform validation profile.
#[derive(Debug, Clone, Default)]
pub struct BitLocker {
    pub encrypted: bool,
    pub protection_on: bool,
    pub pcrs: Option<Vec<u32>>,
}

const BITLOCKER_QUERY: &str = r#"
$ErrorActionPreference = 'Stop'
try {
  $v = Get-CimInstance -Namespace root\cimv2\Security\MicrosoftVolumeEncryption -ClassName Win32_EncryptableVolume -Filter "DriveLetter='C:'"
} catch { 'absent=1'; exit 0 }
if (-not $v) { 'absent=1'; exit 0 }
$c = Invoke-CimMethod -InputObject $v -MethodName GetConversionStatus
$p = Invoke-CimMethod -InputObject $v -MethodName GetProtectionStatus
"conversion=$($c.ConversionStatus)"
"protection=$($p.ProtectionStatus)"
$k = Invoke-CimMethod -InputObject $v -MethodName GetKeyProtectors -Arguments @{KeyProtectorType=[uint32]0}
foreach ($id in $k.VolumeKeyProtectorID) {
  $t = Invoke-CimMethod -InputObject $v -MethodName GetKeyProtectorType -Arguments @{VolumeKeyProtectorID=$id}
  if ($t.KeyProtectorType -in 1,4,5,6) {
    $r = Invoke-CimMethod -InputObject $v -MethodName GetKeyProtectorPlatformValidationProfile -Arguments @{VolumeKeyProtectorID=$id}
    "pcrs=$(($r.PlatformValidationProfile) -join ',')"
  }
}
"#;

fn powershell(script: &str) -> io::Result<String> {
    let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\Windows"));
    let exe = root.join("System32").join("WindowsPowerShell").join("v1.0").join("powershell.exe");
    let out = std::process::Command::new(exe)
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script])
        .output()?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn bitlocker() -> BitLocker {
    let Ok(out) = powershell(BITLOCKER_QUERY) else { return BitLocker::default() };
    parse_bitlocker(&out)
}

pub fn parse_bitlocker(out: &str) -> BitLocker {
    let mut b = BitLocker::default();
    for line in out.lines().map(str::trim) {
        if let Some(v) = line.strip_prefix("conversion=") {
            b.encrypted = v != "0";
        } else if let Some(v) = line.strip_prefix("protection=") {
            b.protection_on = v == "1";
        } else if let Some(v) = line.strip_prefix("pcrs=") {
            let nums: Vec<u32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            b.pcrs = Some(nums);
        }
    }
    b
}

impl BitLocker {
    /// PCR 5 is the partition table. Where the profile binds it, adding
    /// Rime's partitions makes the next Windows start ask for the 48-digit
    /// recovery key, so protection is suspended for exactly one restart
    /// (Microsoft's own procedure before firmware or partition changes). An
    /// unreadable profile is treated as binding it.
    pub fn needs_suspend(&self) -> bool {
        self.protection_on && self.pcrs.as_ref().is_none_or(|p| p.contains(&5))
    }
}

const BITLOCKER_SUSPEND: &str = r#"
$ErrorActionPreference = 'Stop'
$v = Get-CimInstance -Namespace root\cimv2\Security\MicrosoftVolumeEncryption -ClassName Win32_EncryptableVolume -Filter "DriveLetter='C:'"
$r = Invoke-CimMethod -InputObject $v -MethodName DisableKeyProtectors -Arguments @{DisableCount=[uint32]1}
"result=$($r.ReturnValue)"
"#;

fn suspend_bitlocker() -> io::Result<()> {
    let out = powershell(BITLOCKER_SUSPEND)?;
    if out.lines().any(|l| l.trim() == "result=0") {
        Ok(())
    } else {
        Err(io::Error::other(format!("BitLocker could not be suspended for one restart: {}", out.trim())))
    }
}

/// What a finished `install` leaves behind, also written to the journal.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub boot_number: u16,
    pub esp_guid: String,
    pub root_guid: String,
    pub disk_guid: String,
    pub journal: PathBuf,
    pub bitlocker_suspended: bool,
}

fn now_text() -> String {
    let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format!("unix:{s}")
}

fn find_disk(guid: &str) -> io::Result<Surveyed> {
    let (disks, _, _) = surveyed();
    let mut hits: Vec<Surveyed> = disks.into_iter().filter(|d| d.identity.gpt_disk_guid == guid).collect();
    match hits.len() {
        1 => Ok(hits.remove(0)),
        0 => Err(io::Error::other(format!("no disk with GPT disk GUID {guid} is attached now"))),
        _ => Err(io::Error::other(format!("{} disks share GPT disk GUID {guid} (a cloned disk); refusing to guess", hits.len()))),
    }
}

/// The whole Windows side. Re-surveys the chosen disk rather than trusting
/// the candidate it was given: everything between choosing and writing is
/// time in which the machine could have changed.
pub fn install(c: &Candidate, iso: Option<&Path>, ev: &mut dyn FnMut(Event)) -> io::Result<Outcome> {
    // 1. The image, before anything else: a failed download changes nothing.
    let iso_path = obtain_iso(iso, ev)?;
    let mut iso = File::open(&iso_path)?;
    let payload = stage::locate_payload(&mut iso)?;

    // 2. Firmware: readable, and a number free for the setup entry.
    ev(Event::Step("Reading the firmware boot entries".into()));
    winwrite::firmware_access()?;
    let options = winwrite::boot_options()?;
    let order = winwrite::boot_order()?;
    let existing: Vec<u16> = options.iter().map(|(n, _)| *n).collect();
    let boot_number = bootentry::free_number(&existing, &order).ok_or_else(|| io::Error::other("no free boot entry number"))?;

    // 3. The disk, read again now.
    ev(Event::Step("Checking the disk again".into()));
    let disk = find_disk(&c.disk.gpt_disk_guid)?;
    if disk.identity != c.disk {
        return Err(io::Error::other("the disk is not the one that was chosen (its model, serial or size changed)"));
    }
    disk.agreement.clone().map_err(io::Error::other)?;
    let mut dev = w::Device::open(&disk.path)?;
    let before = GptSnapshot::read(&mut dev, disk.identity.length)?;
    if let Space::Replace { partition_guid } = &c.space {
        let p = disk
            .partitions
            .iter()
            .find(|p| &p.id == partition_guid)
            .ok_or_else(|| io::Error::other("the chosen partition is gone"))?;
        let (vols, vp) = w::volumes();
        if !vp.is_empty() {
            return Err(io::Error::other(format!("the volume list could not be read completely: {}", vp.join("; "))));
        }
        let claims = Volumes(vols).claims(disk.number, p.offset, p.length);
        if let Some(r) = replace_refusal(p, &claims, needed_bytes(), || probe(&mut dev, p.offset, p.length)) {
            return Err(io::Error::other(r));
        }
    }

    // 4. BitLocker, before the partition table changes.
    let bl = bitlocker();

    // 5. The plan: every byte that will be written, decided now.
    let esp_guid = random_guid()?;
    let root_guid = random_guid()?;
    let id = random_bytes(4)?;
    let fat_id = u32::from_le_bytes([id[0], id[1], id[2], id[3]]);
    let plan = stage::plan(&stage::Inputs {
        before: &before,
        space: c.space.clone(),
        payload: &payload,
        esp_guid: esp_guid.clone(),
        root_guid: root_guid.clone(),
        fat_volume_id: fat_id,
        boot_number,
        windows_bitlocker: bl.encrypted,
        created: now_text(),
        app_version: APP_VERSION,
    })?;

    // 6. Backups and the journal, on disk before the first write.
    let dir = data_dir()?;
    let stamp = now_text().replace(':', "-");
    let backup = dir.join(format!("gpt-{}-{stamp}.rimegpt", disk.identity.gpt_disk_guid));
    fs::write(&backup, before.to_backup_file())?;
    let journal = dir.join("journal.cfg");
    let journal_text = format!(
        "# Rime installer for Windows: what the last install changed. Used by undo.\n\
         version=1\nstate=staging\ndisk_guid={}\nesp_partuuid={esp_guid}\nroot_partuuid={root_guid}\n\
         bootnum={boot_number:04X}\ngpt_backup={}\nfirmware_before={}\ncreated={}\n",
        disk.identity.gpt_disk_guid,
        backup.display(),
        options.iter().map(|(n, _)| bootentry::option_name(*n)).collect::<Vec<_>>().join(","),
        now_text()
    );
    fs::write(&journal, &journal_text)?;

    let suspended = if bl.needs_suspend() {
        ev(Event::Step("Suspending BitLocker for one restart".into()));
        suspend_bitlocker()?;
        true
    } else {
        false
    };

    let committed = (|| -> io::Result<()> {
        // 7. Payload into space nothing describes yet, then read it all back.
        let mut wr = winwrite::DiskWriter::open(&disk.path, plan.write_windows())?;
        stage::table_unchanged(&plan, &mut wr)?;
        exclusive(&disk, &plan)?;
        ev(Event::Step("Writing the Rime OS installer to its partition".into()));
        let hashes = stage::write_payload(&plan, &mut wr, &mut iso, &mut pr_of(ev))?;
        ev(Event::Step("Reading it back".into()));
        stage::verify_payload(&plan, &mut wr, &hashes, &mut pr_of(ev))?;

        // 8. The commit: the partition table, after one more fresh comparison.
        ev(Event::Step("Adding Rime's partitions to the partition table".into()));
        stage::table_unchanged(&plan, &mut wr)?;
        exclusive(&disk, &plan)?;
        stage::write_table(&plan, &mut wr, &mut pr_of(ev))?;
        stage::table_committed(&plan, &mut wr)?;
        wr.update_properties()?;
        drop(wr);
        windows_sees(&disk, &plan)?;
        fs::write(&journal, journal_text.replace("state=staging", "state=table-written"))?;

        // 9. The firmware: one new entry, started once.
        ev(Event::Step("Adding a one-time boot entry".into()));
        let opt = bootentry::encode(
            bootentry::SETUP_DESCRIPTION,
            &HardDrive {
                partition_number: plan.esp_slot,
                start_lba: plan.esp.first_lba,
                size_lba: plan.esp.last_lba - plan.esp.first_lba + 1,
                guid: gptwrite::guid_bytes(&esp_guid)?,
            },
            bootentry::SETUP_LOADER,
        )?;
        winwrite::create_option(boot_number, &opt)?;
        winwrite::set_boot_next(boot_number)?;
        fs::write(&journal, journal_text.replace("state=staging", "state=ready"))?;
        Ok(())
    })();
    if let Err(e) = committed {
        return Err(if suspended {
            io::Error::new(e.kind(), format!("{e}\n\nBitLocker on C: was suspended for this install and stays suspended until Windows next restarts; it then turns itself back on."))
        } else {
            e
        });
    }
    Ok(Outcome { boot_number, esp_guid, root_guid, disk_guid: disk.identity.gpt_disk_guid, journal, bitlocker_suspended: suspended })
}

fn pr_of(ev: &mut dyn FnMut(Event)) -> impl FnMut(Phase, u64, u64) + '_ {
    move |ph, d, t| ev(Event::Progress { what: phase_name(ph), done: d, total: t })
}

fn phase_name(p: Phase) -> &'static str {
    match p {
        Phase::Wipe => "clear",
        Phase::Payload => "write",
        Phase::Verify => "verify",
        Phase::Table => "partition table",
    }
}

/// Nothing Windows mounts may overlap anything this plan writes, asked
/// again immediately before the writes rather than remembered.
fn exclusive(disk: &Surveyed, plan: &Plan) -> io::Result<()> {
    let (vols, problems) = w::volumes();
    if !problems.is_empty() {
        return Err(io::Error::other(format!("the volume list could not be read completely: {}", problems.join("; "))));
    }
    let v = Volumes(vols);
    for (o, l) in [(plan.esp_offset(), plan.esp_bytes()), (plan.root_offset(), plan.root_bytes())] {
        let c = v.claims(disk.number, o, l);
        if !c.is_empty() {
            return Err(io::Error::other(format!(
                "Windows is now using part of the chosen space: {}",
                c.iter().map(|x| x.what.as_str()).collect::<Vec<_>>().join("; ")
            )));
        }
    }
    Ok(())
}

/// Windows' own description of the disk must now include Rime's two
/// partitions exactly, and still everything else.
fn windows_sees(disk: &Surveyed, plan: &Plan) -> io::Result<()> {
    let mut last = String::new();
    for _ in 0..20 {
        let dev = w::Device::open(&disk.path)?;
        let (_, parts) = w::layout(&dev)?;
        let have = |guid: &str, ty: &str, off: u64, len: u64| {
            parts.iter().any(|p| p.id == guid && p.type_guid == ty && p.offset == off && p.length == len)
        };
        if have(&plan.esp.unique_guid, plan::EFI_SYSTEM, plan.esp_offset(), plan.esp_bytes())
            && have(&plan.root.unique_guid, plan::LINUX_FILESYSTEM, plan.root_offset(), plan.root_bytes())
        {
            return Ok(());
        }
        last = format!("{} partitions listed", parts.len());
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    Err(io::Error::other(format!("Windows does not show Rime's new partitions after the table was written ({last})")))
}

/// Read the journal written by `install`.
pub fn journal() -> io::Result<Vec<(String, String)>> {
    let text = fs::read_to_string(data_dir()?.join("journal.cfg"))?;
    Ok(text
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect())
}

fn jget<'a>(j: &'a [(String, String)], k: &str) -> io::Result<&'a str> {
    j.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str()).ok_or_else(|| io::Error::other(format!("the journal has no {k}")))
}

/// What undo would do, in words, without doing it.
pub fn undo_preview() -> io::Result<String> {
    let j = journal()?;
    let disk = find_disk(jget(&j, "disk_guid")?)?;
    let esp = jget(&j, "esp_partuuid")?;
    let root = jget(&j, "root_partuuid")?;
    let find = |g: &str| disk.partitions.iter().find(|p| p.id == g);
    let mut s = format!("On {}:\n", describe_disk(&disk.identity));
    for (name, g) in [("Rime's boot partition", esp), ("Rime's system partition", root)] {
        match find(g) {
            Some(p) => s.push_str(&format!("  remove {name} ({}, partition GUID {g})\n", plan::human(p.length))),
            None => s.push_str(&format!("  {name} is already gone\n")),
        }
    }
    s.push_str("  remove every firmware boot entry that starts from Rime's boot partition\n");
    s.push_str("The space becomes unallocated again. Its contents are not erased, and Windows is not touched.");
    Ok(s)
}

/// Take Rime off the disk: remove its two partition entries and every boot
/// entry that starts from its ESP. Only the entries this program created
/// (named by GUID in the journal) are removed.
pub fn undo(ev: &mut dyn FnMut(Event)) -> io::Result<()> {
    let j = journal()?;
    let disk = find_disk(jget(&j, "disk_guid")?)?;
    disk.agreement.clone().map_err(io::Error::other)?;
    let esp = jget(&j, "esp_partuuid")?.to_string();
    let root = jget(&j, "root_partuuid")?.to_string();
    let mut dev = w::Device::open(&disk.path)?;
    let before = GptSnapshot::read(&mut dev, disk.identity.length)?;
    drop(dev);
    let mut after = before.clone();
    let mut removed = Vec::new();
    for g in [&esp, &root] {
        if gptwrite::slot_of(&after, g).is_some() {
            after = gptwrite::with_removed(&after, g)?;
            removed.push(g.clone());
        }
    }
    let (vols, vp) = w::volumes();
    if !vp.is_empty() {
        return Err(io::Error::other(format!("the volume list could not be read completely: {}", vp.join("; "))));
    }
    for p in disk.partitions.iter().filter(|p| removed.contains(&p.id)) {
        let c = Volumes(vols.clone()).claims(disk.number, p.offset, p.length);
        if !c.is_empty() {
            return Err(io::Error::other(format!("Windows is using one of Rime's partitions: {}", c[0].what)));
        }
    }
    if !removed.is_empty() {
        ev(Event::Step("Removing Rime's partitions from the partition table".into()));
        let last = disk.identity.length / 512 - 1;
        let windows = vec![(0, 34 * 512), ((last - 32) * 512, 33 * 512)];
        let mut wr = winwrite::DiskWriter::open(&disk.path, windows)?;
        let now = GptSnapshot::read(&mut wr, disk.identity.length)?;
        if !gptwrite::writes(&before, &now)?.is_empty() {
            return Err(io::Error::other("the partition table changed while undo was preparing; nothing was written"));
        }
        for (o, b) in gptwrite::writes(&before, &after)? {
            wr.seek(SeekFrom::Start(o))?;
            io::Write::write_all(&mut wr, &b)?;
        }
        io::Write::flush(&mut wr)?;
        let check = GptSnapshot::read(&mut wr, disk.identity.length)?;
        if !gptwrite::writes(&after, &check)?.is_empty() {
            return Err(io::Error::other("the partition table on disk is not the one undo wrote"));
        }
        wr.update_properties()?;
    }
    ev(Event::Step("Removing Rime's boot entries".into()));
    winwrite::firmware_access()?;
    let esp_bytes = gptwrite::guid_bytes(&esp)?;
    for (n, b) in winwrite::boot_options()? {
        if let Some(o) = bootentry::decode(&b)
            && o.hard_drive.as_ref().is_some_and(|h| h.guid == esp_bytes)
        {
            winwrite::remove_option(n)?;
            ev(Event::Note(format!("removed boot entry {} ({})", bootentry::option_name(n), o.description)));
        }
    }
    let text = fs::read_to_string(data_dir()?.join("journal.cfg"))?;
    fs::write(data_dir()?.join("journal.cfg"), text.replace("state=ready", "state=undone").replace("state=table-written", "state=undone").replace("state=staging", "state=undone"))?;
    Ok(())
}
