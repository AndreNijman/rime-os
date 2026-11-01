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
use super::{generations, lock, now, resolve_link, state, store, write_atomic, write_json, write_status, Applied, AppliedRelease, Config, Pins, Status, CHECK_STATE, GCROOTS};

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
        if opts.live.plan_only {
            return worst;
        }
    }
    if !opts.skip_flatpak && !opts.firmware_only {
        worst = worst.max(flatpak_pass());
    }
    if !opts.skip_firmware {
        worst = worst.max(crate::ops::firmware_pass());
    }
    // TODO(handover): the catalogue pass (§8.2 step 15, replaces rime-pkg) belongs to the catalogue PR.
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
    let (state, digest) = match r {
        Err(s) => {
            eprintln!("{}", s.message);
            return s.code;
        }
        Ok(None) => ("none", String::new()),
        Ok(Some((rel, digest))) => {
            let m = &rel.manifest;
            let v = m.variant(&cfg.variant).expect("checked by fetch::manifest");
            let target = m.path(v.toplevel);
            let current = generations::list().ok().and_then(|g| g.all.get(&g.current).cloned());
            let newer = Applied::load().high_water().map_or(true, |hw| release::compare(&m.release, &hw) != Some(std::cmp::Ordering::Less));
            if current.as_deref() == Some(target) || !newer {
                println!("rime: the OS is up to date ({})", Applied::load().high_water().unwrap_or(m.release.clone()));
                ("none", digest)
            } else {
                let closure: Vec<&str> = m.variant_closure(v).keys().copied().collect();
                let missing = store::invalid(cfg, &closure).unwrap_or_default();
                let bytes: u64 = m.paths.iter().filter(|p| missing.contains(&p.path)).map(|p| p.file_size).sum();
                println!("rime: release {} is available: {} new store paths, {:.2} GB to download", m.release, missing.len(), bytes as f64 / GB);
                ("available", digest)
            }
        }
    };
    if crate::ops::effective_uid() == Some(0) {
        let _ = write_atomic(Path::new(CHECK_STATE), format!("state={state}\ndigest={digest}\n").as_bytes(), 0o644);
    }
    0
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
    if roots.fixture.is_some() && !opts.live.plan_only {
        return Err(stop("rime: RIME_TRUST_ROOT is set: this program will not deploy on fixture facts"));
    }
    if let Some(h) = Pins::load().hold {
        println!("rime: updates are held at {h} (`sudo rime update --unpin` releases them); the OS was not updated");
        return Ok(0);
    }
    // 3. channel document.
    let (digest, rel_id) = match fetch::channel(cfg, roots, opts.allow_unverified || opts.live.allow_unverified, opts.force, true)? {
        Pointer::Take { digest, release } => (digest, release),
        Pointer::Hold(m) => {
            println!("{m}");
            return Ok(0);
        }
    };
    // 4. manifest: the trust gate.
    let rel = fetch::manifest(cfg, roots, &digest, &rel_id, opts.allow_unverified || opts.live.allow_unverified)?;
    let m = &rel.manifest;
    let v = m.variant(&cfg.variant).expect("checked by fetch::manifest");
    let target = m.path(v.toplevel).to_string();
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
    if let Some(b) = &m.batch {
        if b.mass_rebuild {
            println!("rime: this release rebuilds the whole system, so most of it is downloaded again.");
        }
        if !b.security.is_empty() {
            println!("rime: includes security fixes: {}", b.security.join(", "));
        }
    }
    println!("rime: {} new store paths, {:.2} GB to download ({:.2} GB unpacked)", missing.len(), dl as f64 / GB, unpacked as f64 / GB);

    let target_bc = by_component(closure.iter().copied());
    let running_bc = store::closure(cfg, &running).ok().map(|c| by_component(c.iter().map(String::as_str)));
    let booted_bc = store::closure(cfg, &booted).ok().map(|c| by_component(c.iter().map(String::as_str)));
    let booted_boot = store::boot_facts(Path::new("/run/booted-system")).ok();
    let target_boot = manifest_boot(m, v);
    let machine = crate::live::machine_state(&crate::live::Paths::from_env());
    let decide = |dry: &DryActivate| {
        activation(&Inputs {
            booted_boot: booted_boot.as_ref(),
            booted: booted_bc.as_ref(),
            current: running_bc.as_ref(),
            target_boot: &target_boot,
            target: &target_bc,
            dry_run: dry,
            machine: &machine,
        })
    };
    let plan = decide(&DryActivate::NotYet);
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
    let need = unpacked + RESERVE;
    match free_bytes("/nix/store") {
        Some(free) if free < need && !missing.is_empty() => {
            return Err(stop(format!(
                "rime: this update needs {:.2} GB free on /nix and {:.2} GB is free. Nothing was changed. (`sudo rime gc` removes old generations.)",
                need as f64 / GB,
                free as f64 / GB
            )))
        }
        _ => {}
    }

    // 10. bind, then download.
    let keys: Vec<String> = if rel.verified { m.cache.keys.clone() } else { cfg.cache_keys.clone() };
    if !missing.is_empty() {
        let want: Vec<&str> = missing.iter().map(String::as_str).collect();
        let infos = store::path_info(cfg, Some(&cfg.cache_url), &want, false)
            .map_err(|e| stop(format!("rime: could not reach the release server ({e}); nothing was changed.")))?;
        match bind(m, &want, &infos) {
            Ok(()) => {}
            Err(BindError::Missing(p)) => {
                return Err(stop(format!("rime: the update server does not hold all of release {} yet ({p}). Try again later; nothing was changed.", m.release)))
            }
            Err(BindError::Mismatch { path, what }) => {
                return Err(stop(format!("rime: the update server's copy of {path} does not match the signed release. Nothing was downloaded or changed.\n  ({what})")))
            }
        }
        let _ = write_json(&state("pending.json"), &json!({"release": m.release, "variant": cfg.variant, "manifest": serde_json::from_slice::<Value>(&rel.bytes).unwrap_or(Value::Null)}), 0o644);
        store::copy_from_cache(cfg, &want, &keys, opts.keep_fsync).map_err(|e| {
            stop(format!("rime: a download did not match its signature ({e}). The partly downloaded paths are not used; nothing was changed."))
        })?;
    }

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
    let a = decide(&dry);
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
}
