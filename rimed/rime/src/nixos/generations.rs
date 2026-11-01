//! The system profile's generations: `rime rollback` (§11.1), `rime pin` (§11.2), `rime update --pin/--unpin`, and
//! `rime gc` (§11.3).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use rimed_core::nixrel::gc::{gc_plan, GcInputs};

use super::{lock, resolve_link, Applied, Config, Pins, GCROOTS, PROFILE};

pub struct Generations {
    /// number → toplevel
    pub all: BTreeMap<u32, String>,
    pub current: u32,
    /// The generation `/run/booted-system` resolves to.
    pub booted: Option<u32>,
}

impl Generations {
    pub fn previous(&self) -> Option<u32> {
        self.all.range(..self.current).next_back().map(|(n, _)| *n)
    }
    pub fn number_of(&self, toplevel: &str) -> Option<u32> {
        // Newest first: the same toplevel can be in two generations (a rollback then the same update again).
        self.all.iter().rev().find(|(_, t)| t.as_str() == toplevel).map(|(n, _)| *n)
    }
}

/// Read the profile directly: `system` → `system-<n>-link` → toplevel.
pub fn list() -> Result<Generations, String> {
    let dir = Path::new(PROFILE).parent().ok_or("profile has no directory")?;
    let base = Path::new(PROFILE).file_name().and_then(|n| n.to_str()).ok_or("bad profile")?;
    let mut all = BTreeMap::new();
    for e in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(n) = name.strip_prefix(&format!("{base}-")).and_then(|r| r.strip_suffix("-link")).and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        if let Ok(t) = fs::read_link(e.path()) {
            all.insert(n, t.display().to_string());
        }
    }
    let cur_link = fs::read_link(PROFILE).map_err(|e| format!("{PROFILE}: {e}"))?;
    let current = cur_link
        .to_string_lossy()
        .rsplit('/')
        .next()
        .and_then(|n| n.strip_prefix(&format!("{base}-")))
        .and_then(|r| r.strip_suffix("-link"))
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| format!("{PROFILE} points at {}, not a generation", cur_link.display()))?;
    let booted_top = resolve_link("/run/booted-system");
    let mut g = Generations { all, current, booted: None };
    // The booted generation: the one whose toplevel /run/booted-system is; when several share it, the newest that
    // is not newer than the current one.
    g.booted = booted_top.as_deref().and_then(|t| {
        g.all.iter().rev().filter(|(n, _)| **n <= g.current).find(|(_, top)| top.as_str() == t).map(|(n, _)| *n)
            .or_else(|| g.number_of(t))
    });
    Ok(g)
}

/// The hook re-run after the profile changed: `switch-to-configuration boot` of the profile's current generation.
fn rerun_boot_hook() -> Result<(), String> {
    let r = super::store::switch_to_configuration(PROFILE, "boot")?;
    if r.ok { Ok(()) } else { Err(r.why()) }
}

fn release_label(toplevel: &str) -> String {
    Applied::load().by_toplevel(toplevel).map(|r| r.release.clone()).unwrap_or_else(|| "an unrecorded release".into())
}

/// `rime rollback` (§11.1): the newest generation OLDER THAN THE BOOTED ONE becomes the boot default.
pub fn rollback() -> i32 {
    if let Err(c) = crate::ops::require_root("rollback") {
        return c;
    }
    let _l = match lock("rollback") {
        Ok(l) => l,
        Err(m) => {
            eprintln!("{m}");
            return 1;
        }
    };
    let cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("rime: {e}");
            return 1;
        }
    };
    let g = match list() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("rime: {e}");
            return 1;
        }
    };
    let Some(booted) = g.booted else {
        eprintln!("rime: the running system is not a generation of {PROFILE}; nothing was changed.");
        return 1;
    };
    let Some((&n, top)) = g.all.range(..booted).next_back() else {
        eprintln!("rime: there is no generation older than the running one ({booted}); nothing was changed.");
        return 1;
    };
    if let Err(e) = super::store::switch_generation(&cfg, n) {
        eprintln!("rime: {e}");
        return 1;
    }
    if let Err(e) = rerun_boot_hook() {
        eprintln!("rime: the boot menu could not be updated ({e}); putting the profile back.");
        let _ = super::store::switch_generation(&cfg, g.current);
        let _ = rerun_boot_hook();
        return 1;
    }
    println!("rime: the next restart boots {} (generation {n}). Restart when you are ready.", release_label(top));
    0
}

fn pin_root(n: u32) -> std::path::PathBuf {
    Path::new(GCROOTS).join(format!("pin-{n}"))
}

fn add_pin(g: &Generations, n: u32) -> Result<(), String> {
    let top = g.all.get(&n).ok_or_else(|| format!("generation {n} does not exist"))?;
    fs::create_dir_all(GCROOTS).map_err(|e| format!("{GCROOTS}: {e}"))?;
    let root = pin_root(n);
    let _ = fs::remove_file(&root);
    std::os::unix::fs::symlink(top, &root).map_err(|e| format!("{}: {e}", root.display()))?;
    let mut p = Pins::load();
    p.generations.insert(n);
    p.save()
}

/// `rime pin` (§11.2): keep the booted generation.
pub fn pin(remove: Option<u32>) -> i32 {
    if let Err(c) = crate::ops::require_root("pin") {
        return c;
    }
    let g = match list() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("rime: {e}");
            return 1;
        }
    };
    if let Some(n) = remove {
        let mut p = Pins::load();
        p.generations.remove(&n);
        let _ = fs::remove_file(pin_root(n));
        return match p.save() {
            Ok(()) => {
                println!("rime: generation {n} is no longer pinned");
                0
            }
            Err(e) => {
                eprintln!("rime: {e}");
                1
            }
        };
    }
    let Some(n) = g.booted else {
        eprintln!("rime: the running system is not a generation of {PROFILE}; nothing was pinned.");
        return 1;
    };
    match add_pin(&g, n) {
        Ok(()) => {
            println!("rime: generation {n} ({}) is pinned; rime gc keeps it", release_label(&g.all[&n]));
            0
        }
        Err(e) => {
            eprintln!("rime: {e}");
            1
        }
    }
}

/// `rime update --pin <release>`: hold the machine on an installed release (pinned against gc, made the boot default
/// if it is not, and `rime update` installs nothing newer until `--unpin`).
pub fn hold(release: Option<&str>) -> i32 {
    if let Err(c) = crate::ops::require_root("update") {
        return c;
    }
    let _l = match lock("update --pin") {
        Ok(l) => l,
        Err(m) => {
            eprintln!("{m}");
            return 1;
        }
    };
    let mut p = Pins::load();
    let Some(release) = release else {
        p.hold = None;
        return match p.save() {
            Ok(()) => {
                println!("rime: updates are no longer held; the next rime update installs the channel's release");
                0
            }
            Err(e) => {
                eprintln!("rime: {e}");
                1
            }
        };
    };
    let (cfg, g) = match (Config::load(), list()) {
        (Ok(c), Ok(g)) => (c, g),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("rime: {e}");
            return 1;
        }
    };
    let applied = Applied::load();
    let Some(n) = applied
        .releases
        .iter()
        .rev()
        .filter(|r| r.release == release)
        .find_map(|r| g.number_of(&r.toplevel))
    else {
        eprintln!("rime: release {release} is not installed on this machine (installed: {}); nothing was changed.",
            applied.releases.iter().map(|r| r.release.as_str()).collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>().join(", "));
        return 1;
    };
    if let Err(e) = add_pin(&g, n) {
        eprintln!("rime: {e}");
        return 1;
    }
    p = Pins::load();
    p.hold = Some(release.to_string());
    if let Err(e) = p.save() {
        eprintln!("rime: {e}");
        return 1;
    }
    if g.current != n {
        if let Err(e) = super::store::switch_generation(&cfg, n).and_then(|_| rerun_boot_hook()) {
            eprintln!("rime: the boot menu could not be updated ({e})");
            let _ = super::store::switch_generation(&cfg, g.current);
            return 1;
        }
        println!("rime: the next restart boots {release} (generation {n})");
    }
    println!("rime: updates are held at {release}; `sudo rime update --unpin` releases them");
    0
}

/// `rime gc` (§11.3).
pub fn gc() -> i32 {
    if let Err(c) = crate::ops::require_root("gc") {
        return c;
    }
    let _l = match lock("gc") {
        Ok(l) => l,
        Err(m) => {
            eprintln!("{m}");
            return 1;
        }
    };
    let (cfg, g) = match (Config::load(), list()) {
        (Ok(c), Ok(g)) => (c, g),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("rime: {e}");
            return 1;
        }
    };
    let health: Option<serde_json::Value> = super::read_json(&super::state("last-update.json"));
    let from = health.as_ref().and_then(|h| h.get("from")).and_then(|f| f.as_str()).and_then(|t| g.number_of(t));
    let target = health.as_ref().and_then(|h| h.get("toplevel")).and_then(|f| f.as_str()).map(str::to_string);
    let kind = health.as_ref().and_then(|h| h.get("kind")).and_then(|f| f.as_str()).unwrap_or("boot").to_string();
    // §11.4: booted into it (boot), or running it (switch).
    let confirmed = match target {
        None => true,
        Some(t) => resolve_link(if kind == "switch" { "/run/current-system" } else { "/run/booted-system" }).as_deref() == Some(t.as_str()),
    };
    let plan = gc_plan(&GcInputs {
        generations: g.all.keys().copied().collect(),
        current: g.current,
        booted: g.booted,
        pinned: Pins::load().generations,
        from,
        health_confirmed: confirmed,
        keep_newest: cfg.keep_generations,
    });
    if let Some(why) = &plan.refused {
        eprintln!("rime: gc removed nothing: {why}");
        return 1;
    }
    println!(
        "rime: keeping generations {}; removing {}",
        plan.keep.iter().map(u32::to_string).collect::<Vec<_>>().join(" "),
        if plan.delete.is_empty() { "none".into() } else { plan.delete.iter().map(u32::to_string).collect::<Vec<_>>().join(" ") }
    );
    if let Err(e) = super::store::delete_generations(&cfg, &plan.delete) {
        eprintln!("rime: {e}");
        return 1;
    }
    if !plan.delete.is_empty() {
        if let Err(e) = rerun_boot_hook() {
            eprintln!("rime: the boot menu could not be updated ({e})");
            return 1;
        }
    }
    // An interrupted download's finished paths (§8.5) belong to no generation: root every already-valid path of the
    // pending release's closure while gc runs, so the next `rime update` fetches only the rest.
    let pending = Path::new(GCROOTS).join("pending-gc");
    let _ = fs::remove_dir_all(&pending);
    if let Some(paths) = pending_closure(&cfg) {
        let _ = fs::create_dir_all(&pending);
        let mut kept = 0usize;
        for (i, p) in paths.iter().enumerate() {
            if std::os::unix::fs::symlink(p, pending.join(i.to_string())).is_ok() {
                kept += 1;
            }
        }
        println!("rime: keeping {kept} already-downloaded paths of an unfinished update");
    }
    let r = super::store::collect_garbage(&cfg);
    let _ = fs::remove_dir_all(&pending);
    match r {
        Ok(line) => {
            println!("rime: {}", if line.is_empty() { "store garbage collected" } else { line.as_str() });
            0
        }
        Err(e) => {
            eprintln!("rime: {e}");
            1
        }
    }
}

/// The valid paths of the release an unfinished `rime update` was downloading (`pending.json`, written before the
/// download and removed once the profile owns the closure).
pub fn pending_closure(cfg: &Config) -> Option<Vec<String>> {
    let saved: serde_json::Value = super::read_json(&super::state("pending.json"))?;
    let variant = saved.get("variant")?.as_str()?;
    let m = rimed_core::nixrel::manifest::parse(saved.get("manifest")?.to_string().as_bytes()).ok()?;
    let v = m.variant(variant)?;
    let all: Vec<String> = m.variant_closure(v).keys().map(|p| p.to_string()).collect();
    let refs: Vec<&str> = all.iter().map(String::as_str).collect();
    let invalid = super::store::invalid(cfg, &refs).ok()?;
    Some(all.into_iter().filter(|p| !invalid.contains(p)).collect())
}
