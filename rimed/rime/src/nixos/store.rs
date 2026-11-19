//! The `nix`, `nix-env` and `nix-store` calls, with the fixed option set of §8.5. Every one runs by the store path
//! in `/etc/rime/update.toml`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use rimed_core::nixrel::activation::BootFacts;
use rimed_core::nixrel::manifest::{parse_path_info, NarInfo};

use super::{run, run_visible, Config, Ran, PROFILE};

const NIX_COMMAND: &[&str] = &["--extra-experimental-features", "nix-command"];

/// The error [`path_info`] and [`copy_from_cache`] return when the cache was still busy after [`BUSY_TRIES`] tries.
pub const BUSY: &str = "the Rime download server is busy";
/// What `rime update` says then.
pub const BUSY_MESSAGE: &str = "rime: the Rime download server is busy; nothing was changed; try again later.";
/// Tries of one cache call while the server answers busy, and the waits between them. Nix already retried each
/// download itself (download-attempts) before a try fails.
pub const BUSY_TRIES: u32 = 3;
const BUSY_WAITS: [u64; 2] = [15, 60];
/// curl's words, as Nix prints them, for a connection that failed or broke off.
const TRANSPORT: &[&str] = &[
    "curl error", "Could not connect to server", "Couldn't connect to server", "Couldn't resolve host",
    "Connection reset", "Connection refused", "Timeout was reached", "Operation timed out",
    "Failure when receiving data", "Recv failure", "Send failure", "SSL connect error", "Stream error", "HTTP/2",
    "Empty reply from server", "transfer closed",
];

/// True when every error Nix printed is a download the server turned away for now (HTTP 429 or 5xx) or a connection
/// that failed: worth asking again later. A signature or hash refusal, a 404 or anything else is not. Only `error:`
/// lines count: a download Nix retried into success leaves warnings, and the HTML of an error page follows the line.
pub fn server_busy(stderr: &str) -> bool {
    let errors: Vec<&str> = stderr.lines().filter(|l| l.starts_with("error:")).collect();
    !errors.is_empty() && errors.iter().all(|l| busy_line(l))
}

fn busy_line(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("error: unable to download '") else {
        return false;
    };
    let Some((_url, why)) = rest.split_once("': ") else {
        return false;
    };
    let code = why.strip_prefix("HTTP error ").and_then(|c| c.get(..3)).and_then(|c| c.parse::<u16>().ok());
    match code {
        Some(c) if c != 200 => c == 429 || (500..600).contains(&c),
        _ => TRANSPORT.iter().any(|t| why.contains(t)),
    }
}

/// Run `call` until it succeeds, fails for another reason than a busy server, or has run [`BUSY_TRIES`] times;
/// `wait(n, d)` runs after busy try n. The last result is returned as it is.
fn ask_again(mut call: impl FnMut() -> Result<Ran, String>, wait: &dyn Fn(u32, Duration)) -> Result<Ran, String> {
    let mut n = 1;
    loop {
        let r = call()?;
        if r.ok || !server_busy(&r.stderr) || n >= BUSY_TRIES {
            return Ok(r);
        }
        wait(n, Duration::from_secs(BUSY_WAITS[(n - 1) as usize]));
        n += 1;
    }
}

fn pause(_n: u32, d: Duration) {
    eprintln!("rime: the Rime download server is busy; asking again in {} s", d.as_secs());
    std::thread::sleep(d);
}

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
        let r = ask_again(|| run(&cfg.nix, &args, None), &pause)?;
        if !r.ok && server_busy(&r.stderr) {
            return Err(BUSY.to_string());
        }
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

/// §8.5 step 3: exactly `paths` from the cache `from`, checked against exactly `keys`.
pub fn copy_from_cache(cfg: &Config, from: &str, paths: &[&str], keys: &[String], keep_fsync: bool) -> Result<(), String> {
    let keys = keys.join(" ");
    let mut args: Vec<&str> = NIX_COMMAND.to_vec();
    args.extend([
        "copy", "--from", from, "--no-recursive", "--stdin",
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
    let r = ask_again(|| run_visible(&cfg.nix, &args, Some(&stdin)), &pause)?;
    if r.ok {
        Ok(())
    } else if server_busy(&r.stderr) {
        Err(BUSY.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    // Nix 2.35's own output, from the client's two calls against a cache answering 503 (the rime-nix container).
    const NARINFO_503: &str = "warning: unable to download 'http://127.0.0.1:37601/i0m6qkgcszkc76kdia908fwlxiz87gsg.narinfo': HTTP error 503; retrying in 3250 ms (attempt 1/5)\n\
error: unable to download 'http://127.0.0.1:37601/i0m6qkgcszkc76kdia908fwlxiz87gsg.narinfo': HTTP error 503\n";
    const NAR_503: &str = "copying 2 paths...\n\
copying path '/nix/store/i0m6qkgcszkc76kdia908fwlxiz87gsg-gate-proof-held-a' from 'http://127.0.0.1:37601'...\n\
warning: unable to download 'http://127.0.0.1:37601/nar/1dikbm0p80imkqq7pn8ppp5mwypj81b6i0yy6z60m8hd592bvdwy.nar.xz': HTTP error 503\n\
error (ignored): unable to download 'http://127.0.0.1:37601/nar/0vhksz3wwsw7pr22zx380vlknflj6d9wa31z2awj2148459wrnb4.nar.xz': HTTP error 503\n\
error: unable to download 'http://127.0.0.1:37601/nar/1dikbm0p80imkqq7pn8ppp5mwypj81b6i0yy6z60m8hd592bvdwy.nar.xz': HTTP error 503\n\
       response body:\n\
       <!DOCTYPE html>\n\
       <html>\n\
         <head><title>Unicorn! &middot; GitHub</title></head>\n\
         <body>We're having a really bad day.</body>\n\
       </html>\n";

    fn ran(ok: bool, stderr: &str) -> Ran {
        Ran { ok, code: if ok { 0 } else { 1 }, stdout: String::new(), stderr: stderr.to_string() }
    }

    #[test]
    fn a_busy_server_is_told_apart_from_a_refusal() {
        for busy in [
            NARINFO_503,
            NAR_503,
            "error: unable to download 'https://cache.rimeos.com/x.narinfo': HTTP error 429\n",
            "error: unable to download 'https://cache.rimeos.com/x.narinfo': HTTP error 502\n",
            "error: unable to download 'https://cache.rimeos.com/x.narinfo': Could not connect to server (7) Failed to connect to cache.rimeos.com port 443 after 3 ms: Could not connect to server\n",
            "error: unable to download 'https://github.com/a/b/releases/download/n/x.nar.zst': HTTP error 200 (curl error: Recv failure: Connection reset by peer)\n",
        ] {
            assert!(server_busy(busy), "{busy}");
        }
        for not_busy in [
            "",
            "warning: unable to download 'https://cache.rimeos.com/x.narinfo': HTTP error 503; retrying in 100 ms (attempt 1/5)\n",
            "error: unable to download 'https://cache.rimeos.com/nar/x.nar.zst': HTTP error 404\n",
            "error: unable to download 'https://cache.rimeos.com/nar/x.nar.zst': HTTP error 403\n",
            "error: hash mismatch importing path '/nix/store/abc-x';\n  specified: sha256-a\n  got:       sha256-b\n",
            "error: cannot add path '/nix/store/abc-x' because it lacks a signature by a trusted key\n",
            "error: signature is not valid\n",
            // a busy download beside a real refusal: the refusal decides
            "error: unable to download 'https://cache.rimeos.com/x.narinfo': HTTP error 503\nerror: signature is not valid\n",
            "error: path '/nix/store/abc-x' is not valid\n",
        ] {
            assert!(!server_busy(not_busy), "{not_busy}");
        }
    }

    #[test]
    fn a_busy_call_is_asked_again_a_bounded_number_of_times() {
        let waits = RefCell::new(Vec::new());
        let wait = |n: u32, d: Duration| waits.borrow_mut().push((n, d.as_secs()));
        let calls = RefCell::new(0);
        let r = ask_again(
            || {
                *calls.borrow_mut() += 1;
                Ok(ran(false, NAR_503))
            },
            &wait,
        )
        .unwrap();
        assert!(!r.ok && server_busy(&r.stderr));
        assert_eq!(*calls.borrow(), BUSY_TRIES);
        assert_eq!(*waits.borrow(), vec![(1, 15), (2, 60)]);
    }

    #[test]
    fn a_call_that_recovers_or_is_refused_stops_at_once() {
        let waits = RefCell::new(Vec::new());
        let wait = |n: u32, d: Duration| waits.borrow_mut().push((n, d.as_secs()));
        let mut seq = vec![ran(true, ""), ran(false, NARINFO_503)];
        let r = ask_again(|| Ok(seq.pop().unwrap()), &wait).unwrap();
        assert!(r.ok, "busy, then answered");
        assert_eq!(waits.borrow().len(), 1);

        waits.borrow_mut().clear();
        let mut seq = vec![ran(false, "error: signature is not valid\n")];
        let r = ask_again(|| Ok(seq.pop().unwrap()), &wait).unwrap();
        assert!(!r.ok);
        assert!(waits.borrow().is_empty(), "a refusal is never asked again");

        let mut n = 0;
        assert!(ask_again(|| { n += 1; Err("could not run nix".to_string()) }, &wait).is_err());
        assert_eq!(n, 1, "a call that could not run is not repeated");
    }

    #[test]
    fn the_error_page_is_not_the_reason_given() {
        // Before: the last three stderr lines (the HTML) were quoted as "a download did not match its signature".
        assert!(ran(false, NAR_503).why().contains("</html>"));
        assert!(server_busy(NAR_503));
    }

    #[test]
    fn boot_facts_name_store_paths_the_way_the_manifest_does() {
        let d = std::env::temp_dir().join(format!("rime-nixos-bootfacts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("boot.json"),
            r#"{"org.nixos.bootspec.v1":{"kernel":"/nix/store/kkkk-linux-6.12/bzImage","initrd":"/nix/store/iiii-initrd-linux/initrd","kernelParams":["quiet","loglevel=4"],"init":"/nix/store/x/init","label":"l","system":"x86_64-linux","toplevel":"/nix/store/t"}}"#,
        )
        .unwrap();
        std::os::unix::fs::symlink("/nix/store/mmmm-linux-6.12-modules-shrunk/lib", d.join("kernel-modules")).unwrap();
        let b = boot_facts(&d).unwrap();
        assert_eq!(b.kernel, "/nix/store/kkkk-linux-6.12");
        assert_eq!(b.initrd, "/nix/store/iiii-initrd-linux");
        assert_eq!(b.kernel_modules.as_deref(), Some("/nix/store/mmmm-linux-6.12-modules-shrunk"));
        assert_eq!(b.kernel_params, ["quiet", "loglevel=4"]);
        std::fs::remove_file(d.join("kernel-modules")).unwrap();
        assert_eq!(boot_facts(&d).unwrap().kernel_modules, None);
        std::fs::write(d.join("boot.json"), "{}").unwrap();
        assert!(boot_facts(&d).unwrap_err().contains("bootspec"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
