//! `rime pkg translate`: what becomes of every package a bootc Rime machine installed, on Rime on NixOS
//! (PLAN.md 4.5 + D5, p2-gates P1).
//!
//! It reads a bootc machine's package records under a root (`/` on the machine itself, or a copy of its state) and
//! reports one row for every item, whatever the outcome: a name that stays requested, a feature it selects, a
//! Flatpak to install, something that carries over as it is, or a refusal with its reason. Nothing is skipped: an
//! item the translator does not understand is reported as such, never left out.
//!
//! Sources (each reported as present or absent):
//! * `/var/lib/rime/pkg/requested` and `state.json`'s `requested` (one row per distinct name);
//!   `state.json`'s `local_files`/`deb_files`/`vendor_verified`/`unsigned_accepted`/`github_sources` annotate those
//!   rows, and get rows of their own only when they name something the requested list does not;
//! * `/var/lib/rime/pkg/local/*`, `copr-keys/<owner>`, `etc.list`, `var.list`, `rollback/`, and the package
//!   repositories in `/etc/yum.repos.d/*.repo`;
//! * `/var/lib/rime/appimage/<name>.AppImage`;
//! * system Flatpaks (`/var/lib/flatpak/app/<id>`), system extensions (`/var/lib/extensions/*.raw`),
//!   `/var/usrlocal/lib/<name>` (Claude Desktop's hand install lives there);
//! * per user (`/var/home/*`, `/home/*`): npm packages in `~/.local/lib/node_modules`, `~/.local/opt/<name>`,
//!   user Flatpaks, capsule records (`~/.local/share/rime/env/*.json`) and `~/.config/rime/blueprint.toml`.
//!
//! `--write` turns the report into the NixOS records under the same root: the requested list (the bootc one is kept
//! beside it as `requested.bootc`), each selected feature's switch file, and `translate.json` (the full report,
//! which also names the Flatpaks to install). It installs nothing: the Flatpaks and the catalogue apps come with the
//! first `sudo rime install`/`sudo rime pkg upgrade` on the new system.

use super::pkg::{self, Catalogue, Entry, Route};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    /// Stays in the requested list: a class 2 app or claude-code.
    Kept,
    /// Selects a prebuilt system feature (and stays in the requested list).
    Feature,
    /// Not built for this release yet: stays requested, installs once a release builds it.
    Pending,
    /// Installs from Flathub.
    Flatpak,
    /// Not in the catalogue: `rime install` looks for a Flathub application of exactly this name.
    Lookup,
    /// Survives the move as it is (it lives in /var or a home directory).
    Carried,
    /// A bootc mechanism NixOS replaces; the reason says with what.
    Dropped,
    /// Cannot be installed on Rime on NixOS; the reason says what to do instead.
    Refused,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Row {
    pub source: String,
    pub item: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical: Option<String>,
    pub outcome: Outcome,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Source {
    pub name: String,
    pub path: String,
    pub present: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub schema: u32,
    pub root: String,
    pub sources: Vec<Source>,
    pub rows: Vec<Row>,
    /// The NixOS requested list (`/var/lib/rime/pkg/requested`), by catalogue name.
    pub requested_after: Vec<String>,
    /// Features to select, each with the requested names that select it.
    pub features: BTreeMap<String, Vec<String>>,
    /// Flatpak application ids to install (system), not already installed.
    pub flatpak_install: Vec<String>,
    /// Names `rime install` must look up on Flathub.
    pub flathub_lookup: Vec<String>,
}

impl Report {
    pub fn count(&self, outcome: Outcome) -> usize {
        self.rows.iter().filter(|r| r.outcome == outcome).count()
    }
}

/// `root` joined with an absolute path.
fn under(root: &Path, abs: &str) -> PathBuf {
    root.join(abs.trim_start_matches('/'))
}

/// Entry names in a directory, sorted; None when the directory does not exist.
fn names(dir: &Path) -> Option<Vec<String>> {
    let rd = fs::read_dir(dir).ok()?;
    let mut v: Vec<String> = rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    Some(v)
}

fn str_list(v: &Value, key: &str) -> Vec<String> {
    match v.get(key) {
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        Some(Value::Object(o)) => o.keys().cloned().collect(),
        _ => vec![],
    }
}

struct Ctx<'a> {
    cat: &'a Catalogue,
    root: &'a Path,
    r: Report,
    /// System Flatpak ids already installed (a requested Flatpak entry that is already there is carried).
    system_flatpaks: BTreeSet<String>,
}

impl Ctx<'_> {
    fn source(&mut self, name: &str, abs: &str) -> PathBuf {
        let p = under(self.root, abs);
        self.r.sources.push(Source { name: name.into(), path: abs.into(), present: p.exists() });
        p
    }

    fn row(&mut self, source: &str, item: &str, canonical: Option<String>, outcome: Outcome, detail: impl Into<String>) {
        self.r.rows.push(Row { source: source.into(), item: item.into(), canonical, outcome, detail: detail.into() });
    }

    fn keep(&mut self, canon: &str) {
        if !self.r.requested_after.iter().any(|n| n == canon) {
            self.r.requested_after.push(canon.to_string());
        }
    }

    /// One package name, as `rime install` on NixOS would take it, plus what a bootc record said about it.
    fn name(&mut self, source: &str, item: &str, note: &str) {
        let suffix = if note.is_empty() { String::new() } else { format!(" ({note})") };
        let lower = item.to_ascii_lowercase();
        // A bootc `local:`/`deb:` line: a file it installed. One the catalogue knows by name (a vendor app fetched
        // with its vendor's key) becomes that entry; anything else has nowhere to go.
        let bare = lower.strip_prefix("local:").or_else(|| lower.strip_prefix("deb:"));
        let lookup = match bare {
            Some(b) if self.cat.entries.contains_key(&self.cat.canonical(b)) => b.to_string(),
            Some(_) => {
                return self.row(source, item, None, Outcome::Refused, format!("{}{suffix}", pkg::RPM_REASON));
            }
            None => item.to_string(),
        };
        if let Some(Entry::Nix { buildable: false, pending, .. }) = self.cat.entries.get(&self.cat.canonical(&lookup)) {
            let canon = self.cat.canonical(&lookup);
            let why = pending.clone().unwrap_or_else(|| format!("{canon} is not built for this release"));
            self.keep(&canon);
            return self.row(
                source,
                item,
                Some(canon),
                Outcome::Pending,
                format!("stays requested and installs once a release builds it: {why}{suffix}"),
            );
        }
        match pkg::route(self.cat, &lookup, None) {
            Route::Toggle { name, feature } => {
                self.keep(&name);
                let names = self.r.features.entry(feature.clone()).or_default();
                if !names.contains(&name) {
                    names.push(name.clone());
                }
                let title = self.cat.features.get(&feature).map(|f| f.title.clone()).unwrap_or_default();
                self.row(source, item, Some(name), Outcome::Feature, format!("selects the {title} system ({feature}){suffix}"));
            }
            Route::Nix { name } => {
                self.keep(&name);
                self.row(source, item, Some(name), Outcome::Kept, format!("Rime catalogue app, from Rime's cache{suffix}"));
            }
            Route::UserNpm { name, package, .. } => {
                self.keep(&name);
                self.row(
                    source,
                    item,
                    Some(name),
                    Outcome::Kept,
                    format!("npm {package} into the user's ~/.local, runs under nix-ld{suffix}"),
                );
            }
            Route::Flatpak { id, remote, .. } => {
                let canon = Some(self.cat.canonical(&lookup))
                    .filter(|k| self.cat.entries.contains_key(k))
                    .unwrap_or_else(|| id.clone());
                if self.system_flatpaks.contains(&id) {
                    self.row(source, item, Some(canon), Outcome::Carried, format!("already installed as Flatpak {id}{suffix}"));
                } else {
                    if !self.r.flatpak_install.contains(&id) {
                        self.r.flatpak_install.push(id.clone());
                    }
                    self.row(source, item, Some(canon), Outcome::Flatpak, format!("Flatpak {id} from {remote}{suffix}"));
                }
            }
            Route::Flathub { term } => {
                if !self.r.flathub_lookup.contains(&term) {
                    self.r.flathub_lookup.push(term.clone());
                }
                self.row(
                    source,
                    item,
                    None,
                    Outcome::Lookup,
                    format!("not in Rime's catalogue: rime install looks for a Flathub application named {term}{suffix}"),
                );
            }
            Route::Refuse(reason) => self.row(source, item, None, Outcome::Refused, format!("{reason}{suffix}")),
            Route::Capsule => unreachable!("no --source"),
        }
    }
}

/// Translate the package records under `root`.
pub fn translate(cat: &Catalogue, root: &Path) -> Report {
    let mut c = Ctx {
        cat,
        root,
        r: Report { schema: 1, root: root.display().to_string(), ..Default::default() },
        system_flatpaks: BTreeSet::new(),
    };

    // System Flatpaks first: a requested Flatpak entry that is already installed is not installed twice.
    let fp_dir = c.source("system-flatpak", "/var/lib/flatpak/app");
    let system_flatpaks = names(&fp_dir).unwrap_or_default();
    c.system_flatpaks = system_flatpaks.iter().cloned().collect();

    // ── the requested list and state.json ───────────────────────────────────────────────────────────────────────
    let req_path = c.source("requested", "/var/lib/rime/pkg/requested");
    let requested = pkg::read_list(&req_path);
    let state_path = c.source("state.json", "/var/lib/rime/pkg/state.json");
    let state: Value = fs::read_to_string(&state_path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    if state_path.exists() && state.is_null() {
        c.row("state.json", "state.json", None, Outcome::Dropped, "does not parse as JSON: nothing in it was read");
    }
    let vendor: BTreeSet<String> = str_list(&state, "vendor_verified").into_iter().collect();
    let unsigned: BTreeSet<String> = str_list(&state, "unsigned_accepted").into_iter().collect();
    let github: BTreeMap<String, String> = match state.get("github_sources") {
        Some(Value::Object(o)) => o.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string())).collect(),
        _ => BTreeMap::new(),
    };
    let note_for = |line: &str| -> String {
        let bare = line.split_once(':').map(|(_, b)| b).unwrap_or(line);
        let mut n = vec![];
        if vendor.contains(bare) {
            n.push("vendor-verified on bootc".to_string());
        }
        if unsigned.contains(bare) {
            n.push("accepted unsigned on bootc".to_string());
        }
        if let Some(r) = github.get(bare) {
            n.push(format!("followed github:{r} on bootc"));
        }
        n.join("; ")
    };
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for line in &requested {
        if seen.insert(line.clone()) {
            let note = note_for(line);
            c.name("requested", line, &note);
        }
    }
    for line in str_list(&state, "requested") {
        if seen.insert(line.clone()) {
            let note = ["only in state.json, not in the requested file", &note_for(&line)]
                .iter()
                .filter(|s| !s.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join("; ");
            c.name("state.json", &line, &note);
        }
    }
    // The per-file lists name local:/deb: requests by their bare name; one the requested list does not hold is a
    // record of its own.
    let bare_requested: BTreeSet<String> =
        seen.iter().map(|l| l.split_once(':').map(|(_, b)| b.to_string()).unwrap_or_else(|| l.clone())).collect();
    for key in ["local_files", "deb_files", "vendor_verified", "unsigned_accepted", "github_sources"] {
        for n in str_list(&state, key) {
            if !bare_requested.contains(&n) {
                c.row(
                    "state.json",
                    &n,
                    None,
                    Outcome::Refused,
                    format!("listed in {key} but not requested; {}", pkg::RPM_REASON),
                );
            }
        }
    }

    // ── the rest of the bootc package engine's records ──────────────────────────────────────────────────────────
    let local = c.source("pkg-local", "/var/lib/rime/pkg/local");
    for f in names(&local).unwrap_or_default() {
        c.row("pkg-local", &f, None, Outcome::Refused, format!("a package file kept for a local: request; {}", pkg::RPM_REASON));
    }
    let copr = c.source("copr-keys", "/var/lib/rime/pkg/copr-keys");
    for owner in names(&copr).unwrap_or_default() {
        c.row(
            "copr-keys",
            &owner,
            None,
            Outcome::Dropped,
            format!(
                "COPR repository {owner}: Rime on NixOS uses no package repositories; what was installed from it is \
                 reported by its own name"
            ),
        );
    }
    let repos = c.source("repos", "/etc/yum.repos.d");
    for f in names(&repos).unwrap_or_default() {
        if !f.ends_with(".repo") {
            continue;
        }
        let what = if f.starts_with("_copr") { "a COPR repository" } else { "a package repository" };
        c.row(
            "repos",
            &f,
            None,
            Outcome::Dropped,
            format!("{what}: Rime on NixOS uses no package repositories; apps come from Rime's catalogue and Flathub"),
        );
    }
    for (name, abs, what) in [
        ("etc-list", "/var/lib/rime/pkg/etc.list", "the /etc files the extension's packages installed"),
        ("var-list", "/var/lib/rime/pkg/var.list", "the /var directories the extension's packages created"),
    ] {
        let p = c.source(name, abs);
        if p.exists() {
            let n = pkg::read_list(&p).len();
            c.row(
                name,
                abs,
                None,
                Outcome::Dropped,
                format!("{what} ({n} lines): a Nix package carries its own configuration, nothing is copied"),
            );
        }
    }
    let rollback = c.source("pkg-rollback", "/var/lib/rime/pkg/rollback");
    if rollback.exists() {
        let before: Vec<String> = fs::read_to_string(rollback.join("state.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .map(|v| str_list(&v, "requested"))
            .unwrap_or_default();
        let only: Vec<&String> = before.iter().filter(|n| !seen.contains(*n)).collect();
        c.row(
            "pkg-rollback",
            "/var/lib/rime/pkg/rollback",
            None,
            Outcome::Dropped,
            format!(
                "the previous system extension ({} names requested{}): `rime pkg rollback` on NixOS steps back through \
                 the rime-apps profile's own generations, which start with this move",
                before.len(),
                if only.is_empty() {
                    String::new()
                } else {
                    format!("; not requested now: {}", only.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" "))
                }
            ),
        );
    }

    // ── AppImages ───────────────────────────────────────────────────────────────────────────────────────────────
    let ai = c.source("appimage", "/var/lib/rime/appimage");
    let mut appimages = BTreeSet::new();
    for f in names(&ai).unwrap_or_default() {
        let lower = f.to_ascii_lowercase();
        if let Some(stem) = lower.strip_suffix(".appimage") {
            appimages.insert(f[..stem.len()].to_string());
            c.row(
                "appimage",
                &f,
                None,
                Outcome::Carried,
                "the AppImage file stays in /var/lib/rime/appimage and runs directly (AppImage support is on); its \
                 bootc launcher in /usr/local is not on NixOS's PATH",
            );
        } else if [".trust", ".files", ".json"].iter().any(|s| lower.ends_with(s)) {
            // The record files of an AppImage: part of its row.
            let stem = f.rsplit_once('.').map(|(s, _)| s).unwrap_or(&f);
            if !names(&ai).unwrap_or_default().iter().any(|n| n.eq_ignore_ascii_case(&format!("{stem}.AppImage"))) {
                c.row("appimage", &f, None, Outcome::Dropped, "the record of an AppImage whose file is gone");
            }
        } else {
            c.row("appimage", &f, None, Outcome::Carried, "a file in the AppImage directory that is no AppImage record");
        }
    }

    // ── system-wide ─────────────────────────────────────────────────────────────────────────────────────────────
    for id in &system_flatpaks {
        c.row("system-flatpak", id, Some(id.clone()), Outcome::Carried, "the system Flatpak installation lives in /var and carries over");
    }
    let ext = c.source("extensions", "/var/lib/extensions");
    for f in names(&ext).unwrap_or_default() {
        c.row(
            "extensions",
            &f,
            None,
            Outcome::Dropped,
            "a systemd system extension: Rime on NixOS has none, and what it held is reported name by name",
        );
    }
    let usrlocal = c.source("usrlocal", "/var/usrlocal/lib");
    for n in names(&usrlocal).unwrap_or_default() {
        if n == "rime-appimage" {
            for p in names(&usrlocal.join(&n)).unwrap_or_default() {
                if !appimages.contains(&p) {
                    c.row("usrlocal", &format!("rime-appimage/{p}"), None, Outcome::Dropped, "the unpacked payload of an AppImage whose file is gone");
                }
            }
            continue;
        }
        if c.cat.entries.contains_key(&c.cat.canonical(&n)) {
            let note = format!("installed by hand in /usr/local/lib/{n}");
            c.name("usrlocal", &n, &note);
        } else {
            c.row(
                "usrlocal",
                &n,
                None,
                Outcome::Carried,
                format!("/usr/local/lib/{n} stays in /var/usrlocal, unmanaged by rime; it is not on NixOS's paths"),
            );
        }
    }

    // ── per user ────────────────────────────────────────────────────────────────────────────────────────────────
    let mut homes = vec![];
    for base in ["/var/home", "/home"] {
        let p = under(root, base);
        // On bootc /home is a symlink to var/home: one tree, read once.
        if fs::symlink_metadata(&p).map(|m| m.file_type().is_symlink()).unwrap_or(true) {
            continue;
        }
        for u in names(&p).unwrap_or_default() {
            homes.push((format!("{base}/{u}"), u));
        }
    }
    c.r.sources.push(Source { name: "homes".into(), path: "/var/home, /home".into(), present: !homes.is_empty() });
    for (home, user) in homes {
        let hp = under(root, &home);
        let nm = hp.join(".local/lib/node_modules");
        for n in names(&nm).unwrap_or_default() {
            let pkgs: Vec<String> = if n.starts_with('@') {
                names(&nm.join(&n)).unwrap_or_default().into_iter().map(|s| format!("{n}/{s}")).collect()
            } else if n.starts_with('.') {
                vec![]
            } else {
                vec![n.clone()]
            };
            for p in pkgs {
                let item = format!("{user}: npm {p}");
                let npm_entry = c.cat.entries.iter().find_map(|(name, e)| match e {
                    Entry::UserNpm { package, .. } if *package == p => Some(name.clone()),
                    _ => None,
                });
                match npm_entry {
                    Some(name) => c.row(
                        "user-npm",
                        &item,
                        Some(name.clone()),
                        Outcome::Carried,
                        format!("stays in {home}/.local and runs under nix-ld; the catalogue's {name} entry updates it"),
                    ),
                    None => c.row(
                        "user-npm",
                        &item,
                        None,
                        Outcome::Carried,
                        format!("stays in {home}/.local/lib/node_modules, unmanaged by rime; native parts run under nix-ld"),
                    ),
                }
            }
        }
        for n in names(&hp.join(".local/opt")).unwrap_or_default() {
            let item = format!("{user}: ~/.local/opt/{n}");
            let canon = c.cat.entries.contains_key(&c.cat.canonical(&n)).then(|| c.cat.canonical(&n));
            let detail = match &canon {
                Some(k) => format!("stays in the home directory and runs under nix-ld; Rime's catalogue also has {k} (sudo rime install {k})"),
                None => "stays in the home directory, unmanaged by rime; its programs run under nix-ld".into(),
            };
            c.row("user-opt", &item, canon, Outcome::Carried, detail);
        }
        for id in names(&hp.join(".local/share/flatpak/app")).unwrap_or_default() {
            c.row("user-flatpak", &format!("{user}: {id}"), Some(id.clone()), Outcome::Carried, "the user's own Flatpak installation carries over");
        }
        for f in names(&hp.join(".local/share/rime/env")).unwrap_or_default() {
            if f.ends_with(".json") {
                c.row("capsule", &format!("{user}: {f}"), None, Outcome::Carried, "a capsule record: its container lives in the user's storage and carries over");
            }
        }
        let bp = hp.join(".config/rime/blueprint.toml");
        if let Ok(text) = fs::read_to_string(&bp) {
            match text.parse::<toml::Table>() {
                Err(e) => c.row("blueprint", &format!("{user}: blueprint.toml"), None, Outcome::Carried, format!("carried, but does not parse: {e}")),
                Ok(t) => {
                    for (section, v) in &t {
                        if section == "apps" {
                            let installs: Vec<String> = v
                                .get("install")
                                .and_then(|i| i.as_array())
                                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                                .unwrap_or_default();
                            for n in installs {
                                let note = format!("{user}'s blueprint [apps] install");
                                c.name("blueprint", &n, &note);
                            }
                            continue;
                        }
                        let detail = if section == "gaming" {
                            "carried; observed, never converged: steam in the requested list is what selects Gaming"
                        } else {
                            "carried as it is; not a package"
                        };
                        c.row("blueprint", &format!("{user}: [{section}]"), None, Outcome::Carried, detail);
                    }
                }
            }
        }
    }
    c.r
}

/// Write the NixOS records under the root: requested (bootc's kept as requested.bootc), feature switches,
/// translate.json.
pub fn write(cat: &Catalogue, root: &Path, report: &Report) -> Result<Vec<PathBuf>, String> {
    let mut wrote = vec![];
    let req = under(root, pkg::REQUESTED);
    let saved = req.with_file_name("requested.bootc");
    if req.exists() && !saved.exists() {
        fs::copy(&req, &saved).map_err(|e| format!("cannot keep {} as {}: {e}", req.display(), saved.display()))?;
        wrote.push(saved);
    }
    pkg::write_list(&req, &report.requested_after, false)?;
    wrote.push(req.clone());
    for (feature, names) in &report.features {
        let f = cat.features.get(feature).ok_or_else(|| format!("the catalogue describes no feature '{feature}'"))?;
        let path = under(root, &f.switch);
        let wanted = pkg::edit_list(&pkg::read_list(&path), names, &[]);
        pkg::write_list(&path, &wanted, false)?;
        wrote.push(path);
    }
    let out = req.with_file_name("translate.json");
    let json = serde_json::to_string_pretty(report).map_err(|e| e.to_string())?;
    fs::write(&out, json + "\n").map_err(|e| format!("cannot write {}: {e}", out.display()))?;
    wrote.push(out);
    Ok(wrote)
}

fn print(report: &Report) {
    println!("Package records under {}:", report.root);
    for s in &report.sources {
        println!("  {:<15} {:<34} {}", s.name, s.path, if s.present { "read" } else { "absent" });
    }
    println!();
    let mut by_source: Vec<&Row> = report.rows.iter().collect();
    by_source.sort_by(|a, b| a.outcome.cmp(&b.outcome).then(a.source.cmp(&b.source)));
    for r in by_source {
        let outcome = serde_json::to_value(r.outcome).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default();
        let canon = match &r.canonical {
            Some(c) if *c != r.item => format!(" -> {c}"),
            _ => String::new(),
        };
        println!("  {outcome:<8} {:<14} {}{canon}: {}", r.source, r.item, r.detail);
    }
    println!();
    println!("requested on NixOS: {}", if report.requested_after.is_empty() { "(nothing)".into() } else { report.requested_after.join(" ") });
    for (f, n) in &report.features {
        println!("feature {f}: selected by {}", n.join(" "));
    }
    if !report.flatpak_install.is_empty() {
        println!("Flatpaks to install: {}", report.flatpak_install.join(" "));
    }
    if !report.flathub_lookup.is_empty() {
        println!("to look up on Flathub: {}", report.flathub_lookup.join(" "));
    }
    let counts: Vec<String> = [
        Outcome::Kept,
        Outcome::Feature,
        Outcome::Pending,
        Outcome::Flatpak,
        Outcome::Lookup,
        Outcome::Carried,
        Outcome::Dropped,
        Outcome::Refused,
    ]
    .iter()
    .map(|o| (o, report.count(*o)))
    .filter(|(_, n)| *n > 0)
    .map(|(o, n)| format!("{n} {}", serde_json::to_value(o).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default()))
    .collect();
    println!("{} items reported: {}", report.rows.len(), counts.join(", "));
}

/// `rime pkg translate [--root DIR] [--catalogue FILE] [--json] [--write]`.
pub fn run(args: &[String]) -> i32 {
    let mut root = PathBuf::from("/");
    let mut catalogue = PathBuf::from(pkg::CATALOGUE);
    let (mut json, mut do_write) = (false, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--root" => match it.next() {
                Some(v) => root = PathBuf::from(v),
                None => return usage(),
            },
            "--catalogue" => match it.next() {
                Some(v) => catalogue = PathBuf::from(v),
                None => return usage(),
            },
            "--json" => json = true,
            "--write" => do_write = true,
            _ => return usage(),
        }
    }
    let cat = match fs::read_to_string(&catalogue)
        .map_err(|e| format!("cannot read the package catalogue {}: {e}", catalogue.display()))
        .and_then(|t| Catalogue::parse(&t))
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("rime: {e}");
            return 1;
        }
    };
    let report = translate(&cat, &root);
    if json {
        match serde_json::to_string_pretty(&report) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("rime: {e}");
                return 1;
            }
        }
    } else {
        print(&report);
    }
    if do_write {
        match write(&cat, &root, &report) {
            Ok(paths) => {
                for p in paths {
                    eprintln!("rime: wrote {}", p.display());
                }
            }
            Err(e) => {
                eprintln!("rime: {e}");
                return 1;
            }
        }
    }
    0
}

fn usage() -> i32 {
    eprintln!("usage: rime pkg translate [--root DIR] [--catalogue FILE] [--json] [--write]");
    2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat() -> Catalogue {
        Catalogue::parse(
            r#"{
              "schema": 1, "flathub_remote": "flathub", "apps_profile": "/nix/var/nix/profiles/rime-apps",
              "outs_file": "/var/lib/rime/nix/catalogue-outs.json",
              "features": {
                "gaming": {"title": "Gaming", "offered": true, "in_system": false, "unit": "rime-feature@gaming.target",
                           "switch": "/var/lib/rime/switch/rime-feature@gaming.target.on"}
              },
              "entries": {
                "steam": {"kind": "toggle", "feature": "gaming"},
                "blender": {"kind": "flatpak", "id": "org.blender.Blender"},
                "wine": {"kind": "nix", "buildable": true},
                "helium": {"kind": "nix", "buildable": false, "pending": "not built"},
                "claude-desktop": {"kind": "nix", "buildable": false, "pending": "no vendor build"},
                "claude-code": {"kind": "user-npm", "package": "@anthropic-ai/claude-code", "npm": "/x/npm"},
                "libgcc.i686": {"kind": "refuse", "reason": "32-bit"}
              },
              "aliases": {"helium-bin": "helium"},
              "refuse": [{"reason": "32-bit", "patterns": ["*.i686"]}, {"reason": "os", "patterns": ["kernel"]}]
            }"#,
        )
        .unwrap()
    }

    fn tree() -> PathBuf {
        let d = std::env::temp_dir().join(format!("rime-translate-{}-{}", std::process::id(), rand_suffix()));
        let w = |p: &str, s: &str| {
            let f = d.join(p);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(f, s).unwrap();
        };
        w(
            "var/lib/rime/pkg/requested",
            "steam\nblender\nwine\nhelium-bin\nlibgcc.i686\nfoo.i686\nlocal:mytool\ndeb:claude-desktop\nnotathing\n",
        );
        w(
            "var/lib/rime/pkg/state.json",
            r#"{"requested":["steam","kernel"],"local_files":["mytool","ghost"],"vendor_verified":["claude-desktop"],
                "github_sources":{"mytool":"o/r"}}"#,
        );
        w("var/lib/rime/pkg/local/mytool.rpm", "");
        w("etc/yum.repos.d/_copr:copr.fedorainfracloud.org:imput:helium.repo", "");
        w("etc/yum.repos.d/README", "");
        w("var/lib/rime/pkg/copr-keys/imput/helium.gpg", "");
        w("var/lib/rime/appimage/Thing.AppImage", "");
        w("var/lib/rime/appimage/Thing.json", "{}");
        fs::create_dir_all(d.join("var/lib/flatpak/app/org.blender.Blender")).unwrap();
        fs::create_dir_all(d.join("var/lib/flatpak/app/com.spotify.Client")).unwrap();
        w("var/lib/extensions/rime-user.raw", "");
        fs::create_dir_all(d.join("var/usrlocal/lib/claude-desktop")).unwrap();
        fs::create_dir_all(d.join("var/usrlocal/lib/other")).unwrap();
        w("var/home/u/.local/lib/node_modules/@anthropic-ai/claude-code/package.json", "{}");
        w("var/home/u/.local/lib/node_modules/prettier/package.json", "{}");
        fs::create_dir_all(d.join("var/home/u/.local/opt/equibop")).unwrap();
        w("var/home/u/.config/rime/blueprint.toml", "[gaming]\nenabled = true\n[apps]\ninstall = [\"wine\", \"md.obsidian.Obsidian\"]\n");
        std::os::unix::fs::symlink("var/home", d.join("home")).unwrap();
        d
    }

    fn rand_suffix() -> u128 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    }

    fn outcome(r: &Report, item: &str) -> Outcome {
        r.rows.iter().find(|x| x.item == item).unwrap_or_else(|| panic!("{item} not reported: {:#?}", r.rows)).outcome
    }

    #[test]
    fn every_record_gets_exactly_one_row() {
        let d = tree();
        let r = translate(&cat(), &d);
        // 9 requested lines + 1 state-only name + 1 orphan local_files + 1 local file + 1 copr + 1 AppImage
        // + 1 repo + 2 system Flatpaks + 1 extension + 2 usrlocal + 2 npm + 1 opt + 2 blueprint apps + 1 blueprint
        // section.
        assert_eq!(r.rows.len(), 9 + 1 + 1 + 1 + 1 + 1 + 1 + 2 + 1 + 2 + 2 + 1 + 2 + 1, "{:#?}", r.rows);
        for line in pkg::read_list(&d.join("var/lib/rime/pkg/requested")) {
            assert_eq!(r.rows.iter().filter(|x| x.source == "requested" && x.item == line).count(), 1, "{line}");
        }
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn each_kind_lands_where_it_should() {
        let d = tree();
        let r = translate(&cat(), &d);
        assert_eq!(outcome(&r, "steam"), Outcome::Feature);
        assert_eq!(outcome(&r, "blender"), Outcome::Carried, "already a system Flatpak");
        assert_eq!(outcome(&r, "wine"), Outcome::Kept);
        assert_eq!(outcome(&r, "helium-bin"), Outcome::Pending);
        assert_eq!(outcome(&r, "libgcc.i686"), Outcome::Refused);
        assert_eq!(outcome(&r, "foo.i686"), Outcome::Refused);
        assert_eq!(outcome(&r, "local:mytool"), Outcome::Refused);
        assert!(r.rows.iter().any(|x| x.item == "local:mytool" && x.detail.contains("github:o/r")));
        assert_eq!(outcome(&r, "deb:claude-desktop"), Outcome::Pending, "a vendor app the catalogue knows");
        assert_eq!(outcome(&r, "notathing"), Outcome::Lookup);
        assert_eq!(outcome(&r, "kernel"), Outcome::Refused);
        assert_eq!(outcome(&r, "ghost"), Outcome::Refused);
        assert_eq!(outcome(&r, "imput"), Outcome::Dropped);
        assert_eq!(outcome(&r, "Thing.AppImage"), Outcome::Carried);
        assert_eq!(outcome(&r, "rime-user.raw"), Outcome::Dropped);
        assert_eq!(outcome(&r, "claude-desktop"), Outcome::Pending);
        assert_eq!(outcome(&r, "other"), Outcome::Carried);
        assert_eq!(outcome(&r, "u: npm @anthropic-ai/claude-code"), Outcome::Carried);
        assert_eq!(outcome(&r, "md.obsidian.Obsidian"), Outcome::Flatpak);
        assert_eq!(r.requested_after, vec!["steam", "wine", "helium", "claude-desktop"]);
        assert_eq!(r.features["gaming"], vec!["steam"]);
        assert_eq!(r.flatpak_install, vec!["md.obsidian.Obsidian"]);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn write_keeps_the_bootc_list_and_selects_features() {
        let d = tree();
        let c = cat();
        let r = translate(&c, &d);
        write(&c, &d, &r).unwrap();
        assert_eq!(pkg::read_list(&d.join("var/lib/rime/pkg/requested")), r.requested_after);
        assert!(pkg::read_list(&d.join("var/lib/rime/pkg/requested.bootc")).contains(&"local:mytool".to_string()));
        assert_eq!(pkg::read_list(&d.join("var/lib/rime/switch/rime-feature@gaming.target.on")), vec!["steam"]);
        assert!(d.join("var/lib/rime/pkg/translate.json").exists());
        // A second run keeps the first bootc copy.
        write(&c, &d, &translate(&c, &d)).unwrap();
        assert!(pkg::read_list(&d.join("var/lib/rime/pkg/requested.bootc")).contains(&"local:mytool".to_string()));
        fs::remove_dir_all(&d).unwrap();
    }
}
