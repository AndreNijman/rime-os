//! The `nix`, `nix-env` and `nix-store` calls, with the fixed option set of §8.5. Every one runs by the store path
//! in `/etc/rime/update.toml`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rimed_core::nixrel::activation::BootFacts;
use rimed_core::nixrel::manifest::{parse_path_info, NarInfo};

use super::{run, run_visible, Config, PROFILE};

const NIX_COMMAND: &[&str] = &["--extra-experimental-features", "nix-command"];

/// Paths of `paths` that are NOT valid in the local store.
pub fn invalid(cfg: &Config, paths: &[&str]) -> Result<BTreeSet<String>, String> {
    let mut out = BTreeSet::new();
    for chunk in paths.chunks(500) {
        let mut args = vec!["--check-validity", "--print-invalid"];
        args.extend_from_slice(chunk);
        let r = run(&cfg.nix_store, &args, None)?;
        if !r.ok {
            return Err(format!("nix-store --check-validity: {}", r.why()));
        }
        out.extend(r.stdout.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string));
    }
    Ok(out)
}

/// `nix path-info --json --json-format 1` for `paths` in `store` (`None` = the local store). A path the store
/// does not have comes back as `None`.
pub fn path_info(cfg: &Config, store: Option<&str>, paths: &[&str], recursive: bool) -> Result<BTreeMap<String, Option<NarInfo>>, String> {
    let mut out = BTreeMap::new();
    for chunk in paths.chunks(500) {
        let mut args: Vec<&str> = NIX_COMMAND.to_vec();
        args.extend(["path-info", "--json", "--json-format", "1", "--option", "narinfo-cache-negative-ttl", "0"]);
        if let Some(s) = store {
            args.extend(["--store", s]);
        }
        if recursive {
            args.push("--recursive");
        }
        args.extend_from_slice(chunk);
        let r = run(&cfg.nix, &args, None)?;
        // A missing path makes `nix path-info` exit non-zero while still printing it as null; a store that did not
        // answer prints nothing parseable.
        match parse_path_info(r.stdout.trim()) {
            Ok(m) if !r.stdout.trim().is_empty() => out.extend(m),
            _ => return Err(format!("nix path-info: {}", r.why())),
        }
        for p in chunk {
            if !recursive {
                out.entry(p.to_string()).or_insert(None);
            }
        }
    }
    Ok(out)
}

/// §8.5 step 3: exactly `paths`, checked against exactly `keys`.
pub fn copy_from_cache(cfg: &Config, paths: &[&str], keys: &[String], keep_fsync: bool) -> Result<(), String> {
    let keys = keys.join(" ");
    let mut args: Vec<&str> = NIX_COMMAND.to_vec();
    args.extend([
        "copy", "--from", cfg.cache_url.as_str(), "--no-recursive", "--stdin",
        "--option", "require-sigs", "true",
        "--option", "trusted-public-keys", keys.as_str(),
        "--option", "substituters", "",
        "--option", "fallback", "false",
        "--option", "min-free", "0",
        "--option", "narinfo-cache-negative-ttl", "0",
    ]);
    if keep_fsync {
        args.extend(["--option", "fsync-store-paths", "true"]);
    }
    let stdin = paths.join("\n") + "\n";
    let r = run_visible(&cfg.nix, &args, Some(&stdin))?;
    if r.ok {
        Ok(())
    } else {
        Err(r.why())
    }
}

/// The closure of a store path in the local store (`nix-store -qR`).
pub fn closure(cfg: &Config, path: &str) -> Result<Vec<String>, String> {
    let r = run(&cfg.nix_store, &["--query", "--requisites", path], None)?;
    if !r.ok {
        return Err(format!("nix-store -qR {path}: {}", r.why()));
    }
    Ok(r.stdout.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect())
}

/// What a generation boots, read from the generation itself: bootspec `boot.json` (kernel, initrd, kernelParams)
/// and the `kernel-modules` link.
pub fn boot_facts(toplevel: &Path) -> Result<BootFacts, String> {
    let p = toplevel.join("boot.json");
    let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", p.display()))?;
    let spec = v.get("org.nixos.bootspec.v1").ok_or_else(|| format!("{}: no org.nixos.bootspec.v1", p.display()))?;
    let s = |k: &str| spec.get(k).and_then(|x| x.as_str()).map(str::to_string).ok_or_else(|| format!("{}: no {k}", p.display()));
    // Bootspec names the kernel FILE (<linux>/bzImage) and initrd FILE; the manifest names store paths.
    let store_path = |f: String| -> String {
        f.strip_prefix("/nix/store/").and_then(|r| r.split('/').next()).map(|b| format!("/nix/store/{b}")).unwrap_or(f)
    };
    let modules = std::fs::read_link(toplevel.join("kernel-modules")).ok().map(|l| store_path(l.display().to_string()));
    Ok(BootFacts {
        kernel: store_path(s("kernel")?),
        initrd: store_path(s("initrd")?),
        kernel_modules: modules,
        kernel_params: spec
            .get("kernelParams")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default(),
    })
}

pub fn set_profile(cfg: &Config, toplevel: &str) -> Result<(), String> {
    let r = run(&cfg.nix_env, &["--profile", PROFILE, "--set", toplevel], None)?;
    if r.ok { Ok(()) } else { Err(format!("nix-env --set: {}", r.why())) }
}

pub fn switch_generation(cfg: &Config, n: u32) -> Result<(), String> {
    let n = n.to_string();
    let r = run(&cfg.nix_env, &["--profile", PROFILE, "--switch-generation", &n], None)?;
    if r.ok { Ok(()) } else { Err(format!("nix-env --switch-generation {n}: {}", r.why())) }
}

/// Delete generations BY NUMBER, never `old` or `-d` (§11.3).
pub fn delete_generations(cfg: &Config, ns: &[u32]) -> Result<(), String> {
    if ns.is_empty() {
        return Ok(());
    }
    let nums: Vec<String> = ns.iter().map(u32::to_string).collect();
    let mut args = vec!["--profile", PROFILE, "--delete-generations"];
    args.extend(nums.iter().map(String::as_str));
    let r = run(&cfg.nix_env, &args, None)?;
    if r.ok { Ok(()) } else { Err(format!("nix-env --delete-generations: {}", r.why())) }
}

/// `<toplevel>/bin/switch-to-configuration <action>`; the output is shown (activation prints what it does).
pub fn switch_to_configuration(toplevel: &str, action: &str) -> Result<super::Ran, String> {
    let prog = format!("{toplevel}/bin/switch-to-configuration");
    run_visible(&prog, &[action], None)
}

/// The same with output captured (dry-activate).
pub fn dry_activate(toplevel: &str) -> Result<super::Ran, String> {
    let prog = format!("{toplevel}/bin/switch-to-configuration");
    run(&prog, &["dry-activate"], None)
}

/// `nix-store --gc` with the profile's generations already reduced.
pub fn collect_garbage(cfg: &Config) -> Result<String, String> {
    let r = run(&cfg.nix_store, &["--gc"], None)?;
    if r.ok {
        Ok(r.stderr.lines().last().unwrap_or("").to_string())
    } else {
        Err(format!("nix-store --gc: {}", r.why()))
    }
}
