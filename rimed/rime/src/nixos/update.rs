//! `rime update` on NixOS: §8.2's steps in order. Each refusal names itself (§14) and leaves the profile and the
//! boot menu as they were unless the message says otherwise.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

use rimed_core::live::Component;
use rimed_core::nixrel::activation::{activation, parse_dry_activate, Activation, BootFacts, DryActivate, Inputs, Kind};
use rimed_core::nixrel::classify::by_component;
use rimed_core::nixrel::manifest::{bind, check_local, BindError, Manifest, Variant};
use rimed_core::nixrel::release;
use serde_json::{json, Value};

use crate::ops::UpdateOptions;
use crate::trust::Roots;

use super::fetch::{self, Pointer, Release, Stop};
use super::{delta, generations, lock, now, resolve_link, state, store, write_atomic, write_json, write_status, Applied, AppliedRelease, Config, Pins, Status, CHECK_STATE, CONFIG, GCROOTS};

const GB: f64 = 1_000_000_000.0;
/// §11.5: the reserve on top of the unpacked size.
const RESERVE: u64 = 1_000_000_000;

fn stop(m: impl Into<String>) -> Stop {
    Stop { code: 1, message: m.into() }
}

pub fn update(opts: UpdateOptions) -> i32 {
    let started = Instant::now();
    let mut worst = 0;
    if !opts.firmware_only {
        let cfg = match Config::load() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("rime: {e}");
                return 1;
            }
        };
        let roots = Roots::from_env();
        if opts.check {
            return check(&cfg, &roots, &opts);
        }
        if let Err(c) = crate::ops::require_root("update") {
            return c;
        }
        let _lock = match lock("the update") {
            Ok(l) => l,
            Err(m) => {
                eprintln!("{m}");
                return 1;
            }
        };
        worst = match os_step(&cfg, &roots, &opts) {
            Ok(c) => c,
            Err(s) => {
                eprintln!("{}", s.message);
                s.code
            }
        };
        if opts.live.plan_only || opts.download_only {
            return worst;
        }
    }
    if !opts.skip_flatpak && !opts.firmware_only {
        worst = worst.max(flatpak_pass());
    }
    if !opts.skip_firmware {
        worst = worst.max(crate::ops::firmware_pass());
    }
    // §8.2 step 15 (bootc: rime-pkg upgrade): the catalogue apps follow the release record step 14 wrote.
    if !opts.skip_packages && !opts.firmware_only && !opts.check {
        worst = worst.max(super::pkg::catalogue_pass());
    }
    println!("rime: update finished in {:.1}s", started.elapsed().as_secs_f64());
    worst
}

/// The Flatpak pass without rime-pkg (§16): system installation only.
/// TODO(handover): `--user` remotes of SUDO_USER under runuser, as rime-pkg does on bootc.
fn flatpak_pass() -> i32 {
    if !Path::new(rime_paths::FLATPAK).exists() {
        return 0;
    }
    match crate::ops::run(rime_paths::FLATPAK, &["update", "--system", "-y", "--noninteractive"]) {
        Ok(0) => 0,
        Ok(c) => {
            eprintln!("rime: Flatpak update returned {c}; continuing");
            0
        }
        Err(e) => {
            eprintln!("rime: Flatpak update skipped: {e}");
            0
        }
    }
}

/// `--check` (§8.3): steps 3-5 and the size; nothing downloaded beyond the two documents, nothing recorded. The
/// checker's state file is written when run as root (the update-notice timer).
fn check(cfg: &Config, roots: &Roots, opts: &UpdateOptions) -> i32 {
    let r = (|| -> Result<Option<(Release, String)>, Stop> {
        let (digest, rel) = match fetch::channel(cfg, roots, opts.allow_unverified, opts.force, false)? {
            Pointer::Take { digest, release } => (digest, release),
            Pointer::Hold(m) => {
                println!("{m}");
                return Ok(None);
            }
        };
        let r = fetch::manifest(cfg, roots, &digest, &rel, opts.allow_unverified)?;
        Ok(Some((r, digest)))
    })();
    let (state, digest, offered) = match r {
        Err(s) => {
            eprintln!("{}", s.message);
            return s.code;
        }
        Ok(None) => ("none", String::new(), String::new()),
        Ok(Some((rel, digest))) => {
            let m = &rel.manifest;
            let v = m.variant(&rel.variant.chosen).expect("chosen by fetch::manifest");
            let target = m.path(v.toplevel);
            let current = generations::list().ok().and_then(|g| g.all.get(&g.current).cloned());
            let newer = Applied::load().high_water().map_or(true, |hw| release::compare(&m.release, &hw) != Some(std::cmp::Ordering::Less));
            if current.as_deref() == Some(target) || !newer {
                println!("rime: the OS is up to date ({})", Applied::load().high_water().unwrap_or(m.release.clone()));
                ("none", digest, String::new())
            } else {
                let closure: Vec<&str> = m.variant_closure(v).keys().copied().collect();
                let missing = store::invalid(cfg, &closure).unwrap_or_default();
                let want: Vec<&str> = missing.iter().map(String::as_str).collect();
                let bytes: u64 = match delta::prepare(cfg, m, &want, opts.no_deltas).filter(|p| p.any()) {
                    Some(d) => d.download_bytes(),
                    None => m.paths.iter().filter(|p| missing.contains(&p.path)).map(|p| p.file_size).sum(),
                };
                println!("rime: release {} is available: {} new store paths, {:.2} GB to download", m.release, missing.len(), bytes as f64 / GB);
                ("available", digest, m.release.clone())
            }
        }
    };
    if crate::ops::effective_uid() == Some(0) {
        // release=: the release on offer (rimed's Updates interface names it); running=: the release this machine
        // runs now (none recorded: empty).
        let running = resolve_link("/run/current-system").and_then(|t| super::release_of(&t)).map(|r| r.release).unwrap_or_default();
        let body = check_state(state, &digest, &offered, &running);
        let _ = write_atomic(Path::new(CHECK_STATE), body.as_bytes(), 0o644);
    }
    0
}

/// The checker's state file (/run/rime-update/state), as rimed's Updates interface reads it (rimed/src/updates.rs).
fn check_state(state: &str, digest: &str, release: &str, running: &str) -> String {
    format!("state={state}\ndigest={digest}\nrelease={release}\nrunning={running}\n")
}


/// Unit names in a generation's `etc/systemd/{system,user}` (unit files and links; not the `.wants`/`.d` directories).
fn unit_names(toplevel: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for sub in ["etc/systemd/system", "etc/systemd/user"] {
        for e in std::fs::read_dir(toplevel.join(sub)).into_iter().flatten().flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            let dir = [".wants", ".requires", ".upholds", ".d"].iter().any(|x| n.ends_with(x));
            if n.contains('.') && !dir {
                out.insert(n);
            }
        }
    }
    out
}

/// The units only one of `running` and `target` has (activation rule 3 lets them stop or start). Nothing, when either
/// generation's units could not be listed: an unreadable list must not make every unit look one-sided.
fn one_sided_units(running: &str, target: &str) -> BTreeSet<String> {
    let (a, b) = (unit_names(Path::new(running)), unit_names(Path::new(target)));
    if a.is_empty() || b.is_empty() {
        return BTreeSet::new();
    }
    a.symmetric_difference(&b).cloned().collect()
}

fn manifest_boot(m: &Manifest, v: &Variant) -> BootFacts {
    BootFacts {
        kernel: m.path(v.boot.kernel).to_string(),
        initrd: m.path(v.boot.initrd).to_string(),
        kernel_modules: v.boot.kernel_modules.map(|i| m.path(i).to_string()),
        kernel_params: v.boot.kernel_params.clone(),
    }
}

fn components_json(a: &Activation, live: bool, overrides: &[(Component, &str, String)]) -> Vec<Value> {
    a.changed
        .iter()
        .filter(|c| **c != Component::Metadata)
        .map(|c| {
            let (state, detail) = if let Some((_, s, d)) = overrides.iter().find(|(x, _, _)| x == c) {
                (s.to_string(), d.clone())
            } else if let Some((_, r)) = a.deferred.iter().find(|(x, _)| x == c) {
                ("deferred".to_string(), r.describe())
            } else if live {
                ("active".to_string(), "running the new generation".to_string())
            } else {
                ("pending".to_string(), "applies at the next restart".to_string())
            };
            let requirement = match (live, state.as_str()) {
                (true, "pending") => "session-restart",
                (true, _) => "nothing",
                (false, _) => "reboot",
            };
            json!({"component": c.slug(), "label": c.label(), "state": state,
                   "requirement": requirement, "detail": detail, "versions": []})
        })
        .collect()
}

/// §11.5: refused by name unless the filesystem holding /nix has room for `unpacked` bytes of new store paths, counted
/// as they will sit on disk (variant.rs `store_ratio`: compressed only on a compress-force=zstd btrfs), plus RESERVE,
/// plus `scratch`: the delta decoder's working files (delta.md §7; 0 when the paths come whole).
fn headroom(cfg: &Config, unpacked: u64, scratch: u64, missing: &BTreeSet<String>) -> Result<(), Stop> {
    if missing.is_empty() {
        return Ok(());
    }
    let mounts = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    let ratio = super::variant::store_ratio(&mounts, "/nix/store", cfg.on_disk_ratio);
    let need = super::variant::need_on_disk(unpacked, ratio, RESERVE) + scratch;
    match free_bytes("/nix/store") {
        Some(free) if free < need => Err(stop(format!(
            "rime: this update needs {:.2} GB free on /nix ({:.2} GB of new store paths{}, plus 1 GB to spare) and {:.2} GB is free. Nothing was changed. (`sudo rime gc` removes old generations.)",
            need as f64 / GB,
            unpacked as f64 / GB,
            if ratio < 1.0 { format!(", about {:.2} GB on this compressed disk", (need - RESERVE - scratch) as f64 / GB) } else { String::new() },
            free as f64 / GB
        ))),
        _ => Ok(()),
    }
}

/// §8.5: bind `missing` to the signed manifest, then download it. GitHub backend (§18.1): each path from the cache its
/// `origin` names, Rime's or the upstream one, every one checked against exactly the verified manifest's `cache.keys`
/// (Rime's keys and the upstream's); under --allow-unverified, the keys compiled into the system. `bound` runs between
/// the bind and the first download. Shared by the OS step and `rime install` of a class 2 app. `deltas` (the OS step
/// only, delta.md §7): the prepared delta plan, which downloads every path itself and falls back to the whole path
/// from the same cache on any refusal; None downloads every path whole.
fn fetch_paths(
    cfg: &Config,
    m: &Manifest,
    verified: bool,
    missing: &BTreeSet<String>,
    keep_fsync: bool,
    deltas: Option<&delta::Prepared>,
    bound: impl FnOnce(),
) -> Result<(), Stop> {
    if missing.is_empty() {
        return Ok(());
    }
    let keys: Vec<String> = if verified {
        m.cache.keys.clone()
    } else {
        cfg.cache_keys.iter().chain(cfg.upstream_cache_keys.iter()).cloned().collect()
    };
    let want: Vec<&str> = missing.iter().map(String::as_str).collect();
    let groups = m.by_origin(&want);
    let mut infos = std::collections::BTreeMap::new();
    let mut copies: Vec<(&str, &Vec<&str>)> = Vec::new();
    for (origin, paths) in &groups {
        let Some(from) = cfg.cache_for(*origin) else {
            return Err(stop(format!(
                "rime: release {} takes {} store paths from the upstream cache, and {CONFIG} names none. Nothing was changed.",
                m.release,
                paths.len()
            )));
        };
        infos.extend(
            store::path_info(cfg, Some(from), paths, false)
                .map_err(|e| stop(format!("rime: could not reach the release server ({e}); nothing was changed.")))?,
        );
        copies.push((from, paths));
    }
    match bind(m, &want, &infos) {
        Ok(()) => {}
        Err(BindError::Missing(p)) => {
            return Err(stop(format!("rime: the update server does not hold all of release {} yet ({p}). Try again later; nothing was changed.", m.release)))
        }
        Err(BindError::Mismatch { path, what }) => {
            return Err(stop(format!("rime: the update server's copy of {path} does not match the signed release. Nothing was downloaded or changed.\n  ({what})")))
        }
    }
    bound();
    let started = std::time::Instant::now();
    match deltas {
        // Deltas (delta.md §7): any refusal falls back to the whole path from the same cache.
        Some(d) => {
            let o = d.run(cfg, &keys, keep_fsync).map_err(|e| {
                stop(format!("rime: a download did not match its signature ({e}). The partly downloaded paths are not used; nothing was changed."))
            })?;
            delta::report(&o);
        }
        None => {
            // Upstream first: an upstream path never refers to a Rime one, so a Rime path's references are valid by
            // the time it arrives (Origin orders Rime before Upstream, hence the reverse).
            for (from, paths) in copies.iter().rev() {
                store::copy_from_cache(cfg, from, paths, &keys, keep_fsync).map_err(|e| {
                    stop(format!("rime: a download did not match its signature ({e}). The partly downloaded paths are not used; nothing was changed."))
                })?;
            }
        }
    }
    println!("rime: downloaded in {:.1} s", started.elapsed().as_secs_f64());
    Ok(())
}

/// `rime install` of class 2 apps (pkg.rs set_apps_profile): `outs` are store paths the release record names
/// (catalogue-outs.json). Each one's closure is taken from the signed manifest saved with that record (§8.2 step 14),
/// bound and downloaded exactly as the OS step downloads, with the same free-space gate.
pub fn fetch_apps(outs: &[&str]) -> Result<(), String> {
    let cfg = Config::load()?;
    // Already in the store (a valid path's closure is valid): nothing to download, so nothing to bind.
    if store::invalid(&cfg, outs)?.is_empty() {
        return Ok(());
    }
    let cat = super::pkg::Catalogue::load()?;
    let text = std::fs::read_to_string(&cat.outs_file).map_err(|e| format!("{}: {e}", cat.outs_file))?;
    let doc: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", cat.outs_file))?;
    let release = doc.get("release").and_then(Value::as_str).ok_or_else(|| format!("{} names no release", cat.outs_file))?;
    let dir = state("releases").join(release);
    let bytes = std::fs::read(dir.join("manifest.json")).map_err(|e| {
        format!("the signed manifest of release {release} is not kept on this machine ({e}); `sudo rime update` keeps it")
    })?;
    let m = rimed_core::nixrel::manifest::parse(&bytes).map_err(|r| r.message())?;
    let verified = std::fs::read_to_string(dir.join("verified")).map(|v| v.trim() == "true").unwrap_or(false);
    let mut closure: BTreeSet<usize> = BTreeSet::new();
    for out in outs {
        let entry = m
            .catalogue
            .iter()
            .find(|c| c.class == 2 && c.out < m.paths.len() && m.path(c.out) == *out)
            .ok_or_else(|| format!("release {release}'s signed manifest names no build {out}"))?;
        closure.extend(m.closure_of(entry.out));
    }
    let all: Vec<&str> = closure.iter().map(|i| m.path(*i)).collect();
    let missing = store::invalid(&cfg, &all)?;
    let unpacked: u64 = m.paths.iter().filter(|p| missing.contains(&p.path)).map(|p| p.nar_size).sum();
    headroom(&cfg, unpacked, 0, &missing).map_err(|s| s.message)?;
    fetch_paths(&cfg, &m, verified, &missing, false, None, || {}).map_err(|s| s.message)
}

fn free_bytes(path: &str) -> Option<u64> {
    let c = std::ffi::CString::new(path).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: statvfs into a zeroed struct we own.
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    Some(s.f_bavail as u64 * s.f_frsize as u64)
}

fn os_step(cfg: &Config, roots: &Roots, opts: &UpdateOptions) -> Result<i32, Stop> {
    os_step_from(cfg, roots, opts, None, &mut None)
}

/// What a same-release install did (docs/nixos/desktops.md §5: `rime install <desktop>` / `rime remove <desktop>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SameRelease {
    /// The new system is running now.
    Live,
    /// The new system is installed for the next restart.
    NextBoot,
    /// The running release builds no system with the switches as they are now; nothing was installed.
    NotInRelease,
    /// A newer release is already installed for the next restart; this one was not touched.
    NewerStaged(String),
}

/// Install the system the switch files select from the release this machine RUNS: its digest from the record
/// `rime update` kept (`/var/lib/rime/releases/<release>/digest`), its manifest fetched by that digest and verified
/// again. The channel is never read, so a newer release on the channel is not installed (§8.2 step 3 is skipped).
pub fn install_running_release(no_live: bool) -> Result<SameRelease, String> {
    let cfg = Config::load()?;
    let roots = Roots::from_env();
    let _lock = lock("the update")?;
    let running = resolve_link("/run/current-system").unwrap_or_default();
    let applied = Applied::load();
    let Some(rec) = applied.by_toplevel(&running).cloned() else {
        return Err("this system's release is not recorded on this machine; `sudo rime update` records it".into());
    };
    if let Some(hw) = applied.high_water() {
        if release::compare(&rec.release, &hw) == Some(std::cmp::Ordering::Less) {
            return Ok(SameRelease::NewerStaged(hw));
        }
    }
    let digest = std::fs::read_to_string(state("releases").join(&rec.release).join("digest"))
        .map(|d| d.trim().to_string())
        .map_err(|e| format!("the record of release {} on this machine has no digest ({e}); `sudo rime update` keeps it", rec.release))?;
    let mut opts = UpdateOptions::default();
    opts.live.no_live = no_live;
    let mut chosen = None;
    os_step_from(&cfg, &roots, &opts, Some((digest, rec.release.clone())), &mut chosen).map_err(|s| s.message)?;
    let current = resolve_link("/run/current-system").unwrap_or_default();
    let profile = resolve_link(super::PROFILE).unwrap_or_default();
    Ok(match chosen {
        Some(t) if t == current && t != running => SameRelease::Live,
        Some(t) if t != running && t == profile => SameRelease::NextBoot,
        _ => SameRelease::NotInRelease,
    })
}

/// The OS step. `same`: install from this (digest, release) instead of the channel's (a same-release install).
/// `chosen`: set to the toplevel of the system the release offers this machine, once known.
fn os_step_from(
    cfg: &Config,
    roots: &Roots,
    opts: &UpdateOptions,
    same: Option<(String, String)>,
    chosen: &mut Option<String>,
) -> Result<i32, Stop> {
    if roots.fixture.is_some() && !opts.live.plan_only {
        return Err(stop("rime: RIME_TRUST_ROOT is set: this program will not deploy on fixture facts"));
    }
    // 3. channel document; a same-release install never reads it (and a hold does not apply: nothing is updated).
    let (digest, rel_id) = match same {
        Some(p) => p,
        None => {
            if let Some(h) = Pins::load().hold {
                println!("rime: updates are held at {h} (`sudo rime update --unpin` releases them); the OS was not updated");
                return Ok(0);
            }
            match fetch::channel(cfg, roots, opts.allow_unverified || opts.live.allow_unverified, opts.force, true)? {
                Pointer::Take { digest, release } => (digest, release),
                Pointer::Hold(m) => {
                    println!("{m}");
                    return Ok(0);
                }
            }
        }
    };
    // 4. manifest: the trust gate.
    let rel = fetch::manifest(cfg, roots, &digest, &rel_id, opts.allow_unverified || opts.live.allow_unverified)?;
    let m = &rel.manifest;
    let v = m.variant(&rel.variant.chosen).expect("chosen by fetch::manifest");
    let target = m.path(v.toplevel).to_string();
    *chosen = Some(target.clone());
    let gens = generations::list().map_err(|e| stop(format!("rime: {e}")))?;
    let current_gen_top = gens.all.get(&gens.current).cloned().unwrap_or_default();
    let running = resolve_link("/run/current-system").unwrap_or_default();
    let booted = resolve_link("/run/booted-system").unwrap_or_default();
    let mut applied = Applied::load();
    let from_release = applied.by_toplevel(&running).map(|r| r.release.clone()).unwrap_or_else(|| "this system".into());

    // 5. up to date.
    // TODO(handover): also ask the D3 boot hook's `--check` which generation the menu boots (an interrupted step 12).
    // `installed`: the profile already boots the target. If it is not running yet (deferred because a game ran or a
    // session was locked, or boot-class), the steps below run again without downloading or installing anything, so
    // running `sudo rime update` again is how a deferred release goes live (bootc: `rime live apply`, §10.3).
    let installed = current_gen_top == target;
    if installed && !opts.live.live_only {
        if applied.by_toplevel(&target).is_none() {
            applied.record(AppliedRelease { release: m.release.clone(), digest: digest.clone(), toplevel: target.clone(), generation: Some(gens.current), at: now() });
            let _ = applied.save();
        }
        save_release(&rel);
        if booted == target || running == target {
            println!("rime: the OS is up to date ({})", m.release);
            return Ok(0);
        }
        println!("rime: the OS update {} is installed; checking whether it can be activated now", m.release);
    }

    // 6. downgrade.
    if let Some(hw) = applied.high_water() {
        if release::compare(&m.release, &hw) == Some(std::cmp::Ordering::Less) {
            println!("rime: the channel offers {}, which is older than the installed {hw}; this machine stays on {hw}.", m.release);
            return Ok(0);
        }
    }

    // min_from: a release that needs an intermediate one refuses to install over anything older. A machine whose
    // release is unknown (no record of the running system) is not refused: it has nothing to compare.
    if let (Some(min), Some(have)) = (&m.min_from, applied.by_toplevel(&running).map(|r| r.release.clone())) {
        if release::compare(&have, min) == Some(std::cmp::Ordering::Less) {
            return Err(stop(format!(
                "rime: release {} can only be installed over {min} or newer, and this machine runs {have}. Nothing was changed.",
                m.release
            )));
        }
    }

    // 8. plan.
    let closure: Vec<&str> = m.variant_closure(v).keys().copied().collect();
    let missing: BTreeSet<String> = store::invalid(cfg, &closure).map_err(|e| stop(format!("rime: {e}")))?;
    let dl: u64 = m.paths.iter().filter(|p| missing.contains(&p.path)).map(|p| p.file_size).sum();
    let unpacked: u64 = m.paths.iter().filter(|p| missing.contains(&p.path)).map(|p| p.nar_size).sum();
    println!("rime: {from_release} → {}", m.release);
    if rel.variant.chosen != cfg.variant {
        println!("rime: installing the {} system (switched on for this machine: {})", rel.variant.chosen,
                 rel.variant.chosen.strip_prefix(&format!("{}-", cfg.variant)).unwrap_or(&rel.variant.chosen).replace('-', ", "));
    }
    if !rel.variant.waiting.is_empty() {
        println!("rime: {} is switched on for this machine, and release {} builds no {} system yet; it stays selected for a later release",
                 rel.variant.waiting.join(" and "), m.release, rel.variant.wanted);
    }
    if let Some(b) = &m.batch {
        if b.mass_rebuild {
            println!("rime: this release rebuilds the whole system, so most of it is downloaded again.");
        }
        if !b.security.is_empty() {
            println!("rime: includes security fixes: {}", b.security.join(", "));
        }
    }
    let want: Vec<&str> = missing.iter().map(String::as_str).collect();
    let prepared = delta::prepare(cfg, m, &want, opts.no_deltas);
    for why in prepared.iter().flat_map(|p| &p.skipped) {
        println!("rime: delta not tried: {why}");
    }
    let deltas = prepared.filter(|p| p.any());
    match &deltas {
        Some(d) => println!(
            "rime: {} new store paths, {:.2} GB to download with deltas ({:.2} GB without; {:.2} GB unpacked)",
            missing.len(),
            d.download_bytes() as f64 / GB,
            dl as f64 / GB,
            unpacked as f64 / GB
        ),
        None => println!("rime: {} new store paths, {:.2} GB to download ({:.2} GB unpacked)", missing.len(), dl as f64 / GB, unpacked as f64 / GB),
    }

    let target_bc = by_component(closure.iter().copied());
    let running_bc = store::closure(cfg, &running).ok().map(|c| by_component(c.iter().map(String::as_str)));
    let booted_bc = store::closure(cfg, &booted).ok().map(|c| by_component(c.iter().map(String::as_str)));
    let booted_boot = store::boot_facts(Path::new("/run/booted-system")).ok();
    let target_boot = manifest_boot(m, v);
    let machine = crate::live::machine_state(&crate::live::Paths::from_env());
    let other_desktops = super::pkg::Catalogue::load().map(|c| c.other_desktop_sessions()).unwrap_or_default();
    let decide = |dry: &DryActivate, one_sided: &BTreeSet<String>| {
        activation(&Inputs {
            booted_boot: booted_boot.as_ref(),
            booted: booted_bc.as_ref(),
            current: running_bc.as_ref(),
            target_boot: &target_boot,
            target: &target_bc,
            dry_run: dry,
            machine: &machine,
            one_sided,
            other_desktops: &other_desktops,
        })
    };
    let plan = decide(&DryActivate::NotYet, &BTreeSet::new());
    for c in plan.changed.iter().filter(|c| **c != Component::Metadata) {
        let how = if plan.kind == Kind::Switch { "can be activated now" } else { "applies at the next restart" };
        println!("  {:<22} {how}", c.label());
    }
    for r in &plan.reasons {
        println!("  ({r})");
    }
    if opts.live.plan_only {
        println!("rime: --plan: nothing was downloaded or changed");
        return Ok(0);
    }

    // 9. headroom.
    headroom(cfg, unpacked, deltas.as_ref().map_or(0, |d| d.scratch_bytes()), &missing)?;

    // 10. bind, then download.
    fetch_paths(cfg, m, rel.verified, &missing, opts.keep_fsync, deltas.as_ref(), || {
        let _ = write_json(&state("pending.json"), &json!({"release": m.release, "variant": rel.variant.chosen, "manifest": serde_json::from_slice::<Value>(&rel.bytes).unwrap_or(Value::Null)}), 0o644);
    })?;

    // 11. verify what arrived.
    let local = store::path_info(cfg, None, &[target.as_str()], true).map_err(|e| stop(format!("rime: {e}")))?;
    if let Err(why) = check_local(m, v, &local) {
        // TODO(handover): the one-time repair of §8.6 (delete the mismatching paths so the next run re-downloads them).
        return Err(stop(format!("rime: the installed copy of the release differs from the signed release ({why}). Nothing was changed.")));
    }
    match store::boot_facts(Path::new(&target)) {
        Ok(b) if b == target_boot => {}
        Ok(_) | Err(_) => {
            return Err(stop(format!("rime: release {} describes its kernel differently from what it contains. Nothing was changed.", m.release)))
        }
    }

    if opts.download_only {
        println!("rime: --download-only: release {} is downloaded and verified; nothing was installed", m.release);
        return Ok(0);
    }

    let status_target = (digest.as_str(), m.release.as_str(), target.as_str());
    let booted_release = super::release_of(&booted);
    let status = |state: &str, staged: bool, comps: Vec<Value>, remaining: &str, rec: &str, summary: String| {
        write_status(&Status {
            state,
            booted_release: booted_release.as_ref().map(|r| r.release.clone()),
            booted_digest: booted_release.as_ref().map(|r| r.digest.clone()),
            booted_toplevel: Some(booted.clone()),
            target: Some(status_target),
            staged_for_boot: staged,
            verified: rel.verified,
            components: comps,
            remaining,
            recommendation: rec,
            summary,
        })
    };

    // 12. install for the next boot.
    if !opts.live.live_only && !installed {
        let _ = write_json(&state("last-update.json"), &json!({"schema": 1, "from": running, "toplevel": target, "kind": "boot", "release": m.release, "at": now()}), 0o644);
        let _ = std::fs::create_dir_all(GCROOTS);
        let pending_root = Path::new(GCROOTS).join("pending");
        let _ = std::fs::remove_file(&pending_root);
        let _ = std::os::unix::fs::symlink(&target, &pending_root);
        let prev = gens.current;
        store::set_profile(cfg, &target).map_err(|e| stop(format!("rime: {e}; nothing was changed.")))?;
        let hook = store::switch_to_configuration(&target, "boot");
        if !matches!(&hook, Ok(r) if r.ok) {
            let why = match hook {
                Ok(r) => r.why(),
                Err(e) => e,
            };
            // §8.7: profile back, the failed generation deleted, the hook run again.
            let new_gen = generations::list().ok().map(|g| g.current);
            let _ = store::switch_generation(cfg, prev);
            if let Some(n) = new_gen.filter(|n| *n != prev) {
                let _ = store::delete_generations(cfg, &[n]);
            }
            let _ = store::switch_to_configuration(super::PROFILE, "boot");
            let _ = std::fs::remove_file(&pending_root);
            return Err(stop(format!("rime: the boot menu could not be updated ({why}). This machine still boots {from_release}; nothing else changed.")));
        }
        let _ = std::fs::remove_file(&pending_root);
        let _ = std::fs::remove_file(state("pending.json"));
        let new_gen = generations::list().ok().map(|g| g.current);
        applied.record(AppliedRelease { release: m.release.clone(), digest: digest.clone(), toplevel: target.clone(), generation: new_gen, at: now() });
        applied.save().map_err(|e| stop(format!("rime: {e}")))?;
        save_release(&rel);
        status("staged", true, components_json(&plan, false, &[]), "reboot", "restart when convenient", format!("{} is installed for the next restart.", m.release));
    }

    // 13. live.
    if opts.live.no_live {
        status("active", true, components_json(&plan, false, &[]), "reboot", "restart when convenient", format!("{} applies at the next restart.", m.release));
        println!("rime: {} installs at the next restart (--no-live)", m.release);
        return Ok(0);
    }
    let dry = match store::dry_activate(&target) {
        Ok(r) if r.ok => DryActivate::Ran(parse_dry_activate(&format!("{}\n{}", r.stderr, r.stdout))),
        Ok(r) => DryActivate::Failed(r.why()),
        Err(e) => DryActivate::Failed(e),
    };
    let a = decide(&dry, &one_sided_units(&running, &target));
    if a.kind == Kind::Boot {
        let locked_or_busy = !a.deferred.is_empty();
        for (c, r) in a.deferred.iter().filter(|(c, _)| *c == Component::Shell || *c == Component::HyprlandConfig) {
            println!("rime: {}: {}", c.label(), r.describe());
        }
        if !locked_or_busy {
            for r in &a.reasons {
                println!("rime: needs a restart: {r}");
            }
        }
        if opts.live.live_only {
            println!("rime: --live-only: nothing was installed; this release needs a restart");
        } else {
            println!("rime: this update installs for the next restart.");
        }
        if locked_or_busy {
            println!("rime: run `sudo rime update` again once that is over to activate it without a restart");
        }
        // The final state, as bootc's engine ends an update with nothing live: `active` with a restart owed (Rime
        // Shell: "Restart recommended"), or `deferred`. Never `staged`, which the Shell shows as still applying.
        status(if locked_or_busy { "deferred" } else { "active" }, !opts.live.live_only, components_json(&a, false, &[]), "reboot", "restart when convenient",
               format!("{} applies at the next restart.", m.release));
        return Ok(0);
    }
    status("activating", !opts.live.live_only, components_json(&a, false, &[]), "reboot", "restart when convenient", "Activating the update.".into());
    let undo = |why: String| -> Stop {
        eprintln!("rime: live activation failed: {why}; undoing it");
        let back = store::switch_to_configuration(&running, "test");
        if matches!(back, Ok(ref r) if r.ok) {
            eprintln!("rime: undone; the update applies at the next restart");
        }
        Stop { code: 1, message: format!("rime: {} applies at the next restart", m.release) }
    };
    match store::switch_to_configuration(&target, "test") {
        Ok(r) if r.ok => {}
        Ok(r) => {
            let s = undo(r.why());
            status("rolled-back", !opts.live.live_only, components_json(&a, false, &[]), "reboot", "restart when convenient", "Live activation failed and was undone; the update applies at the next restart.".into());
            return Err(s);
        }
        Err(e) => return Err(undo(e)),
    }
    // Per-session activators (§10.2 step 3).
    let mut overrides: Vec<(Component, &str, String)> = Vec::new();
    if a.changed.contains(&Component::Shell) {
        let sessions = crate::live::graphical(&machine.sessions);
        if sessions.is_empty() {
            overrides.push((Component::Shell, "active-at-next-use", "no graphical session; the next login starts the new Shell".into()));
        } else if !Path::new(rime_paths::RIME_LIVE_SESSION).exists() {
            // TODO(handover): rime-live-session is not packaged on NixOS yet (rime-libexec.nix drops it).
            overrides.push((Component::Shell, "pending", "log out and back in to start the new Shell".into()));
        } else {
            for s in sessions {
                match crate::live::session_helper(s, "shell", &target) {
                    Ok(crate::live::ActResult::Done(d)) => overrides.push((Component::Shell, "active", d)),
                    Ok(crate::live::ActResult::Deferred(r)) => {
                        let s = undo(r.describe());
                        overrides.push((Component::Shell, "deferred", r.describe()));
                        status("rolled-back", true, components_json(&a, false, &overrides), "reboot", "restart when convenient",
                               "A session locked during the update, so it was withdrawn; it applies once unlocked or at the next restart.".into());
                        eprintln!("{}", s.message);
                        return Ok(0);
                    }
                    Err(e) => {
                        let s = undo(e);
                        return Err(s);
                    }
                }
            }
        }
    }
    let _ = write_json(&state("last-update.json"), &json!({"schema": 1, "from": running, "toplevel": target, "kind": "switch", "release": m.release, "at": now()}), 0o644);
    let relogin = overrides.iter().any(|(_, s, _)| *s == "pending");
    let n = a.changed.iter().filter(|c| **c != Component::Metadata).count();
    if relogin {
        status("active", !opts.live.live_only, components_json(&a, true, &overrides), "session-restart", "log out and back in",
               format!("{n} component(s) updated live; log out and back in to start the new Shell."));
    } else {
        status("active", !opts.live.live_only, components_json(&a, true, &overrides), "nothing", "nothing: everything in this release is active",
               format!("{n} component(s) updated live; nothing else to do."));
    }
    println!("rime: {} is active now{}", m.release, if opts.live.live_only { " (until the next restart: --live-only)" } else { "" });
    Ok(0)
}

/// §8.2 step 14: the manifest kept for `rime trust` and the next update's measurement.
fn save_release(rel: &Release) {
    let dir = state("releases").join(&rel.manifest.release);
    let _ = write_atomic(&dir.join("manifest.json"), &rel.bytes, 0o644);
    let _ = write_atomic(&dir.join("digest"), rel.digest.as_bytes(), 0o644);
    let _ = write_atomic(&dir.join("verified"), if rel.verified { b"true\n" } else { b"false\n" }, 0o644);
    save_outs(&rel.manifest);
}

/// The class 2 builds this release names (manifest `catalogue[]`), where `rime install` reads them
/// (the catalogue's `outs_file`). A system built without the catalogue has nowhere to put them and skips this.
fn save_outs(m: &Manifest) {
    let Ok(cat) = super::pkg::Catalogue::load() else {
        return;
    };
    let apps: Vec<(String, String)> = catalogue_outs(m);
    let doc = super::pkg::outs_document(&m.release, &apps);
    if let Err(e) = write_atomic(Path::new(&cat.outs_file), format!("{doc}\n").as_bytes(), 0o644) {
        eprintln!("rime: the catalogue apps' release record was not written: {e}");
    }
}

/// Manifest `catalogue[]` as (name, store path); `check_local` has already refused an index outside the table.
pub fn catalogue_outs(m: &Manifest) -> Vec<(String, String)> {
    m.catalogue.iter().filter(|c| c.class == 2 && c.out < m.paths.len()).map(|c| (c.name.clone(), m.path(c.out).to_string())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_check_state_file_carries_the_release_on_offer_and_the_running_one() {
        assert_eq!(
            check_state("available", "sha256:x", "2026.11.02", "2026.11.01"),
            "state=available\ndigest=sha256:x\nrelease=2026.11.02\nrunning=2026.11.01\n"
        );
    }

    fn generation(root: &Path, system: &[&str], user: &[&str]) -> String {
        for (sub, names) in [("etc/systemd/system", system), ("etc/systemd/user", user)] {
            let d = root.join(sub);
            std::fs::create_dir_all(&d).unwrap();
            for n in names {
                if n.ends_with(".wants") || n.ends_with(".d") {
                    std::fs::create_dir_all(d.join(n)).unwrap();
                } else {
                    std::fs::write(d.join(n), "").unwrap();
                }
            }
        }
        root.to_string_lossy().into_owned()
    }

    #[test]
    fn one_sided_units_are_the_units_only_one_generation_has() {
        let t = std::env::temp_dir().join(format!("rime-onesided-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let plain = generation(&t.join("a"), &["rimed.service", "multi-user.target.wants", "dbus.service"], &["rime-shell.service"]);
        let gnome = generation(
            &t.join("b"),
            &["rimed.service", "multi-user.target.wants", "dbus.service", "accounts-daemon.service", "dbus.service.d"],
            &["rime-shell.service", "gnome-session-manager@.service"],
        );
        let want: BTreeSet<String> = ["accounts-daemon.service", "gnome-session-manager@.service"].map(String::from).into();
        assert_eq!(one_sided_units(&plain, &gnome), want);
        assert_eq!(one_sided_units(&gnome, &plain), want, "removing is the same set");
        // An unreadable generation lists nothing, and nothing is one-sided (not every unit of the other).
        assert!(one_sided_units(&t.join("missing").to_string_lossy(), &gnome).is_empty());
        let _ = std::fs::remove_dir_all(&t);
    }
}
