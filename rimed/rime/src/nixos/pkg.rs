//! `rime install`, `remove`, `search`, `resolve`, `repo` and `pkg` on NixOS (PLAN.md 4.5, decision D5).
//!
//! On bootc these verbs hand their argv to `/usr/libexec/rime-pkg`, which builds RPMs into a system extension. NixOS
//! has no such engine and evaluates no nixpkgs on the device, so every name resolves through the catalogue built
//! into the system (`/etc/rime/catalogue.json`, rendered from nix/catalogue/catalogue.nix by
//! nix/modules/packages-catalogue.nix). `ops::pkg` calls [`run`] with the same argv it would have handed the bootc
//! engine, so the CLI's flags, help and root gate are unchanged.
//!
//! Five kinds of entry (the data file explains each):
//! * `flatpak`  — `flatpak install --system <remote> <id>`.
//! * `toggle`   — a class 1 package that belongs to a prebuilt system feature (Steam → gaming). Installing it writes
//!   the feature's switch file (`/var/lib/rime/switch/rime-feature@<feature>.target.on`, one requesting name per line). A system built with
//!   the feature starts `rime-feature@<feature>.target` at once; a system without it gets it from the update client,
//!   which reads the switch directory to pick the system to install (the contract handed to the D6 update client).
//! * `nix`      — a class 2 app: its store path comes from the release record the update client writes
//!   (`/var/lib/rime/nix/catalogue-outs.json`), is fetched from the cache with `nix-store --realise`, and becomes part
//!   of the `rime-apps` profile. The profile is rewritten as a whole from the requested list, so `rime pkg rollback`
//!   is the profile's own rollback.
//! * `user-npm` — claude-code: `npm install -g` as the user behind sudo, into their `~/.local`.
//! * `refuse`   — the reason is printed as it is.
//!
//! What is requested is recorded where bootc recorded it, `/var/lib/rime/pkg/requested` (one name per line; read by
//! `rime blueprint` and checkpoints). Flatpaks are recorded by Flatpak itself, as on bootc.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The catalogue the system was built with.
pub const CATALOGUE: &str = "/etc/rime/catalogue.json";
/// The names `rime install` was asked for that live on the system side (toggles, class 2 apps, claude-code).
pub const REQUESTED: &str = "/var/lib/rime/pkg/requested";

/// Programs, by their fixed place on a NixOS system (rime-paths: never a PATH lookup).
fn sys(name: &str) -> String {
    format!("{}/{name}", rime_paths::SYSTEM_BIN_DIR)
}

// ── the catalogue document ──────────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct Catalogue {
    pub schema: u32,
    pub flathub_remote: String,
    pub apps_profile: String,
    pub outs_file: String,
    #[serde(default)]
    pub features: BTreeMap<String, Feature>,
    #[serde(default)]
    pub entries: BTreeMap<String, Entry>,
    #[serde(default)]
    pub aliases: BTreeMap<String, String>,
    #[serde(default)]
    pub refuse: Vec<RefuseRule>,
}

#[derive(Debug, Deserialize)]
pub struct Feature {
    pub title: String,
    pub offered: bool,
    pub in_system: bool,
    pub unit: String,
    /// The D2 runtime switch the unit's ConditionPathExists names (`<unit>.on`), written by the module.
    pub switch: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Entry {
    Flatpak {
        id: String,
        #[serde(default)]
        remote: Option<String>,
        #[serde(default)]
        summary: String,
        #[serde(default)]
        why: Option<String>,
    },
    Toggle {
        feature: String,
        #[serde(default)]
        summary: String,
    },
    Nix {
        #[serde(default)]
        summary: String,
        #[serde(default)]
        buildable: bool,
        #[serde(default)]
        pending: Option<String>,
        #[serde(default)]
        version: Option<String>,
    },
    UserNpm {
        package: String,
        npm: String,
        #[serde(default)]
        summary: String,
    },
    Refuse {
        reason: String,
    },
}

impl Entry {
    fn kind(&self) -> &'static str {
        match self {
            Entry::Flatpak { .. } => "flatpak",
            Entry::Toggle { .. } => "feature",
            Entry::Nix { .. } => "app",
            Entry::UserNpm { .. } => "per-user",
            Entry::Refuse { .. } => "refused",
        }
    }

    fn summary(&self) -> &str {
        match self {
            Entry::Flatpak { summary, .. }
            | Entry::Toggle { summary, .. }
            | Entry::Nix { summary, .. }
            | Entry::UserNpm { summary, .. } => summary,
            Entry::Refuse { reason } => reason,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct RefuseRule {
    pub reason: String,
    pub patterns: Vec<String>,
}

impl Catalogue {
    pub fn parse(text: &str) -> Result<Catalogue, String> {
        let c: Catalogue =
            serde_json::from_str(text).map_err(|e| format!("the package catalogue does not parse: {e}"))?;
        if c.schema != 1 {
            return Err(format!(
                "the package catalogue is schema {}, and this rime reads schema 1",
                c.schema
            ));
        }
        Ok(c)
    }

    fn load() -> Result<Catalogue, String> {
        let text = fs::read_to_string(CATALOGUE).map_err(|e| {
            format!("this system has no package catalogue ({CATALOGUE}: {e}); it was built without rime.catalogue")
        })?;
        Catalogue::parse(&text)
    }

    /// The catalogue name a typed name means: aliases first, case-insensitively.
    pub fn canonical(&self, name: &str) -> String {
        let lower = name.to_ascii_lowercase();
        if let Some(target) = self.aliases.get(&lower) {
            return target.clone();
        }
        if self.entries.contains_key(name) {
            return name.to_string();
        }
        lower
    }

    fn entry(&self, name: &str) -> Option<(String, &Entry)> {
        let canon = self.canonical(name);
        self.entries.get(&canon).map(|e| (canon, e))
    }

    fn refused_by_rule(&self, name: &str) -> Option<&str> {
        let lower = name.to_ascii_lowercase();
        self.refuse
            .iter()
            .find(|r| r.patterns.iter().any(|p| glob(p, &lower)))
            .map(|r| r.reason.as_str())
    }
}

/// `*` at the start and/or end of a pattern; anything else is literal.
pub fn glob(pattern: &str, name: &str) -> bool {
    match (pattern.strip_prefix('*'), pattern.strip_suffix('*')) {
        _ if pattern == "*" => true,
        (Some(rest), _) if rest.ends_with('*') => name.contains(&rest[..rest.len() - 1]),
        (Some(suffix), _) => name.ends_with(suffix),
        (None, Some(prefix)) => name.starts_with(prefix),
        (None, None) => name == pattern,
    }
}

/// A reverse-DNS application id (three or more dot-separated segments), which Flatpak owns, as on bootc.
pub fn is_app_id(name: &str) -> bool {
    let parts: Vec<&str> = name.split('.').collect();
    parts.len() >= 3
        && parts.iter().all(|p| {
            !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
        && parts[0].starts_with(|c: char| c.is_ascii_alphabetic())
}

// ── resolving a name ────────────────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
pub enum Route {
    Flatpak { id: String, remote: String, note: Option<String> },
    Toggle { name: String, feature: String },
    Nix { name: String },
    UserNpm { name: String, package: String, npm: String },
    /// Not in the catalogue: look for a Flathub application of exactly this name.
    Flathub { term: String },
    Capsule,
    Refuse(String),
}

pub const RPM_REASON: &str = "Rime on NixOS has no RPM packages and no system extension. To use an RPM or a .deb, \
     install it in a capsule (rime env create fedora, then rime install --source capsule <name>)";
pub const APPIMAGE_REASON: &str = "AppImages run directly on Rime: make the file executable and run it \
     (chmod +x FILE.AppImage, then ./FILE.AppImage). Nothing has to be installed";
pub const GITHUB_REASON: &str = "Rime on NixOS does not follow GitHub releases: their assets carry no signature \
     Rime can check, and an RPM from one has nowhere to go. Use the project's Flatpak, or a capsule";

/// Where one name goes. `source` is `--source`, already stripped of its flag.
pub fn route(cat: &Catalogue, arg: &str, source: Option<&str>) -> Route {
    let lower = arg.to_ascii_lowercase();
    if arg.is_empty() {
        return Route::Refuse("an empty name names nothing".into());
    }
    if lower.starts_with("github:") {
        return Route::Refuse(GITHUB_REASON.into());
    }
    if lower.ends_with(".appimage") {
        return Route::Refuse(APPIMAGE_REASON.into());
    }
    if lower.ends_with(".rpm") || lower.ends_with(".deb") || arg.contains('/') {
        return Route::Refuse(RPM_REASON.into());
    }
    match source {
        None => {}
        Some("capsule") => return Route::Capsule,
        Some("flatpak") => {
            if is_app_id(arg) {
                return Route::Flatpak { id: arg.into(), remote: cat.flathub_remote.clone(), note: None };
            }
            if let Some((_, Entry::Flatpak { id, remote, .. })) = cat.entry(arg) {
                return Route::Flatpak {
                    id: id.clone(),
                    remote: remote.clone().unwrap_or_else(|| cat.flathub_remote.clone()),
                    note: None,
                };
            }
            return Route::Flathub { term: arg.into() };
        }
        Some("rpm") => return Route::Refuse(RPM_REASON.into()),
        Some(other) => {
            return Route::Refuse(format!(
                "--source {other} is not a source Rime on NixOS has; the sources are flatpak and capsule \
                 (without --source, Rime's catalogue comes first)"
            ))
        }
    }
    if is_app_id(arg) {
        return Route::Flatpak { id: arg.into(), remote: cat.flathub_remote.clone(), note: None };
    }
    if let Some((name, entry)) = cat.entry(arg) {
        return match entry {
            Entry::Flatpak { id, remote, why, .. } => Route::Flatpak {
                id: id.clone(),
                remote: remote.clone().unwrap_or_else(|| cat.flathub_remote.clone()),
                note: why.clone(),
            },
            Entry::Toggle { feature, .. } => match cat.features.get(feature) {
                None => Route::Refuse(format!("the catalogue names a feature '{feature}' that it does not describe")),
                Some(f) if !f.offered && !f.in_system => Route::Refuse(
                    f.reason.clone().unwrap_or_else(|| format!("no Rime system carries {} yet", f.title)),
                ),
                Some(_) => Route::Toggle { name, feature: feature.clone() },
            },
            Entry::Nix { buildable, pending, .. } => {
                if *buildable {
                    Route::Nix { name }
                } else {
                    Route::Refuse(pending.clone().unwrap_or_else(|| format!("{name} is not built for this release")))
                }
            }
            Entry::UserNpm { package, npm, .. } => {
                Route::UserNpm { name, package: package.clone(), npm: npm.clone() }
            }
            Entry::Refuse { reason } => Route::Refuse(reason.clone()),
        };
    }
    if let Some(reason) = cat.refused_by_rule(arg) {
        return Route::Refuse(reason.to_string());
    }
    Route::Flathub { term: arg.into() }
}

/// Flathub applications that ARE the term, from `flatpak remote-ls --app --columns=application,name` output: the
/// id's last segment or the name equals it, ignoring case (rime-pkg flatpak_match). Sorted, so the first is stable.
pub fn flathub_matches(term: &str, listing: &str) -> Vec<String> {
    let mut hits: Vec<String> = listing
        .lines()
        .filter_map(|line| {
            let mut cols = line.split('\t');
            let id = cols.next()?.trim();
            let name = cols.next().unwrap_or("").trim();
            if !is_app_id(id) {
                return None;
            }
            let last = id.rsplit('.').next().unwrap_or("");
            (last.eq_ignore_ascii_case(term) || name.eq_ignore_ascii_case(term)).then(|| id.to_string())
        })
        .collect();
    hits.sort();
    hits.dedup();
    hits
}

// ── state files ─────────────────────────────────────────────────────────────────────────────────────────────────

/// One name per line, in the order first asked for; blank lines and `#` comments ignored.
pub fn read_list(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect()
}

/// Write a list atomically (temporary file in the same directory, then rename). An empty list removes the file
/// when `remove_if_empty`.
pub fn write_list(path: &Path, names: &[String], remove_if_empty: bool) -> Result<(), String> {
    if names.is_empty() && remove_if_empty {
        return match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("cannot remove {}: {e}", path.display())),
        };
    }
    let dir = path.parent().ok_or_else(|| format!("{} has no directory", path.display()))?;
    fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let tmp = dir.join(format!(
        ".{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("list")
    ));
    let mut f = fs::File::create(&tmp).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    for n in names {
        writeln!(f, "{n}").map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    }
    f.sync_all().map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| format!("cannot replace {}: {e}", path.display()))
}

/// `list` with `add` appended (once) and `remove` taken out.
pub fn edit_list(list: &[String], add: &[String], remove: &[String]) -> Vec<String> {
    let mut out: Vec<String> = list.iter().filter(|n| !remove.contains(n)).cloned().collect();
    for a in add {
        if !out.contains(a) {
            out.push(a.clone());
        }
    }
    out
}

/// The class 2 apps a requested list names, by catalogue name and each once, in list order. A line written on bootc
/// may use another spelling of the same app (`golang`, `java-latest-openjdk-devel`, `helium-bin`), which the
/// catalogue's aliases map; a list carried over from bootc keeps those lines.
pub fn requested_apps(cat: &Catalogue, list: &[String]) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for n in list {
        // An app this release does not build (chatgpt, still pending) has no store path to put in the profile; it
        // stays in the list and `rime pkg list` shows it, but it must not stop every other app's install.
        if let Some((canon, Entry::Nix { buildable: true, .. })) = cat.entry(n) {
            if !out.contains(&canon) {
                out.push(canon);
            }
        }
    }
    out
}

/// The lines of a requested list that mean catalogue entry `canon`, whatever spelling they were written with.
pub fn lines_naming(cat: &Catalogue, list: &[String], canon: &str) -> Vec<String> {
    list.iter().filter(|n| cat.canonical(n) == canon).cloned().collect()
}

/// Why `rime install`, `remove` and the writing `pkg` verbs need root on NixOS (ops::root_required_message).
pub const ROOT_WHY: &str = "it changes what is installed for every user: /var/lib/rime, the system Flatpak \
     installation and the rime-apps profile";

/// The class 2 builds this machine's release names: `{"apps": {"<name>": "/nix/store/..."}}`.
pub fn parse_outs(text: &str) -> Result<BTreeMap<String, String>, String> {
    #[derive(Deserialize)]
    struct Outs {
        apps: BTreeMap<String, String>,
    }
    let o: Outs = serde_json::from_str(text).map_err(|e| format!("the release record does not parse: {e}"))?;
    for (name, path) in &o.apps {
        if !path.starts_with("/nix/store/") || path.contains("..") || path.contains('\n') {
            return Err(format!("the release record names {name} at {path}, which is not a store path"));
        }
    }
    Ok(o.apps)
}

fn switch_path(f: &Feature) -> PathBuf {
    PathBuf::from(&f.switch)
}

// ── running things ──────────────────────────────────────────────────────────────────────────────────────────────

fn say(msg: impl AsRef<str>) {
    eprintln!("rime: {}", msg.as_ref());
}

fn fail(msg: impl AsRef<str>) -> i32 {
    eprintln!("rime: error: {}", msg.as_ref());
    1
}

fn status(program: &str, args: &[&str]) -> Result<(), String> {
    match Command::new(program).args(args).status() {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("{program} {} failed ({s})", args.join(" "))),
        Err(e) => Err(format!("cannot run {program}: {e}")),
    }
}

fn output(program: &str, args: &[&str]) -> Result<String, String> {
    match Command::new(program).args(args).output() {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
        Ok(o) => Err(format!(
            "{program} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => Err(format!("cannot run {program}: {e}")),
    }
}

fn flatpak() -> &'static str {
    rime_paths::FLATPAK
}

/// The configured system remotes, or why there are none.
fn system_remotes() -> Result<Vec<String>, String> {
    if !Path::new(flatpak()).exists() {
        return Err("Flatpak is not part of this system".into());
    }
    Ok(output(flatpak(), &["remotes", "--system", "--columns=name"])?
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

fn require_remote(remote: &str) -> Result<(), String> {
    if system_remotes()?.iter().any(|r| r == remote) {
        return Ok(());
    }
    Err(format!(
        "the Flatpak remote '{remote}' is not set up on this machine; rime-flathub-setup.service adds Flathub at \
         first boot (systemctl status rime-flathub-setup)"
    ))
}

fn flathub_lookup(cat: &Catalogue, term: &str) -> Result<Option<String>, String> {
    require_remote(&cat.flathub_remote)?;
    let listing = output(
        flatpak(),
        &["remote-ls", "--system", "--app", "--columns=application,name", &cat.flathub_remote],
    )?;
    Ok(flathub_matches(term, &listing).into_iter().next())
}

// ── install ─────────────────────────────────────────────────────────────────────────────────────────────────────

struct InstallArgs {
    names: Vec<String>,
    source: Option<String>,
    env: Option<String>,
    refusals: Vec<String>,
    notes: Vec<String>,
}

fn parse_install(args: &[String]) -> InstallArgs {
    let mut a = InstallArgs { names: vec![], source: None, env: None, refusals: vec![], notes: vec![] };
    for arg in args {
        if let Some(s) = arg.strip_prefix("--source=") {
            a.source = Some(s.to_string());
        } else if let Some(e) = arg.strip_prefix("--env=") {
            a.env = Some(e.to_string());
        } else if arg.starts_with("--enable-repo=") {
            a.refusals.push(
                "--enable-repo: Rime on NixOS has no package repositories; apps come from Rime's catalogue and \
                 Flathub"
                    .into(),
            );
        } else if arg == "--allow-unsigned" {
            a.refusals.push(
                "--allow-unsigned: nothing unsigned is installed on Rime on NixOS; catalogue apps are signed by \
                 Rime's cache and Flatpaks by their remote"
                    .into(),
            );
        } else if arg == "--no-weak-deps" {
            a.notes.push("--no-weak-deps changes nothing here: catalogue apps are built whole".into());
        } else {
            a.names.push(arg.clone());
        }
    }
    a
}

/// The install plan: what each name resolves to, with every Flathub lookup done. Refusals are collected, not
/// acted on, so nothing changes unless every name resolves (bootc did the same before its root gate).
fn plan_install(cat: &Catalogue, names: &[String], source: Option<&str>) -> (Vec<(String, Route)>, Vec<String>) {
    let mut plan = vec![];
    let mut refused = vec![];
    for name in names {
        match route(cat, name, source) {
            Route::Refuse(reason) => refused.push(format!("{name}: {reason}")),
            Route::Flathub { term } => match flathub_lookup(cat, &term) {
                Ok(Some(id)) => plan.push((
                    name.clone(),
                    Route::Flatpak {
                        id: id.clone(),
                        remote: cat.flathub_remote.clone(),
                        note: Some(format!("'{term}' is not in Rime's catalogue; Flathub has it as {id}")),
                    },
                )),
                Ok(None) => refused.push(format!(
                    "{name}: it is not in Rime's catalogue and Flathub has no application of that name \
                     (rime search {term}). For a distribution package, use a capsule: \
                     rime install --source capsule {term}"
                )),
                Err(e) => refused.push(format!("{name}: {e}")),
            },
            r => plan.push((name.clone(), r)),
        }
    }
    (plan, refused)
}

fn install(cat: &Catalogue, args: &[String]) -> i32 {
    let a = parse_install(args);
    for n in &a.notes {
        say(n);
    }
    if !a.refusals.is_empty() {
        for r in &a.refusals {
            fail(r);
        }
        return 1;
    }
    if a.names.is_empty() {
        return fail("usage: rime install <name>...");
    }
    if a.source.as_deref() == Some("capsule") {
        return capsule(&a.names, a.env.as_deref());
    }
    let (plan, refused) = plan_install(cat, &a.names, a.source.as_deref());
    if !refused.is_empty() {
        for r in &refused {
            fail(format!("refusing {r}"));
        }
        say("nothing was installed");
        return 1;
    }

    let mut rc = 0;
    let mut requested = read_list(Path::new(REQUESTED));
    let mut flatpaks: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut nix_add: Vec<String> = vec![];
    for (name, r) in &plan {
        match r {
            Route::Toggle { name: canon, feature } => match select_feature(cat, canon, feature) {
                Ok(()) => requested = edit_list(&requested, &[canon.clone()], &[]),
                Err(e) => rc = fail(format!("{name}: {e}")),
            },
            Route::Nix { name: canon } => nix_add.push(canon.clone()),
            Route::Flatpak { id, remote, note } => {
                if let Some(n) = note {
                    say(format!("{name}: {n}"));
                }
                flatpaks.entry(remote.clone()).or_default().push(id.clone());
            }
            Route::UserNpm { name: canon, package, npm } => match npm_as_user(npm, "install", package) {
                Ok(()) => {
                    say(format!("{canon}: installed into your ~/.local (it updates itself)"));
                    requested = edit_list(&requested, &[canon.clone()], &[]);
                }
                Err(e) => rc = fail(format!("{name}: {e}")),
            },
            Route::Flathub { .. } | Route::Capsule | Route::Refuse(_) => unreachable!("resolved by plan_install"),
        }
    }
    if !nix_add.is_empty() {
        let wanted = edit_list(&requested, &nix_add, &[]);
        match set_apps_profile(cat, &wanted) {
            Ok(()) => {
                requested = wanted;
                for n in &nix_add {
                    say(format!("{n}: installed (in {}, on PATH for new logins)", cat.apps_profile));
                }
            }
            Err(e) => rc = fail(e),
        }
    }
    for (remote, ids) in &flatpaks {
        let r = require_remote(remote).and_then(|()| {
            let mut argv: Vec<&str> = vec!["install", "--system", "--noninteractive", "--or-update", remote];
            argv.extend(ids.iter().map(String::as_str));
            status(flatpak(), &argv)
        });
        match r {
            Ok(()) => say(format!("installed from {remote}: {}", ids.join(", "))),
            Err(e) => rc = fail(e),
        }
    }
    if let Err(e) = write_list(Path::new(REQUESTED), &requested, false) {
        rc = fail(e);
    }
    rc
}

/// Select a class 1 feature for this machine: add `name` to its switch file and, when the running system carries
/// the feature, start its target.
fn select_feature(cat: &Catalogue, name: &str, feature: &str) -> Result<(), String> {
    let f = cat
        .features
        .get(feature)
        .ok_or_else(|| format!("the catalogue describes no feature '{feature}'"))?;
    let path = switch_path(f);
    let wanted = edit_list(&read_list(&path), &[name.to_string()], &[]);
    write_list(&path, &wanted, false)?;
    if f.in_system {
        status(rime_paths::SYSTEMCTL, &["start", &f.unit])?;
        say(format!("{name}: {} is part of this system and is now on ({})", f.title, f.unit));
    } else {
        say(format!(
            "{name} comes with Rime's {} system, which this machine does not have yet",
            f.title
        ));
        say(format!(
            "selected for this machine ({}): `sudo rime update` installs the system that carries it as soon as \
             Rime's release offers one for this machine",
            path.display()
        ));
    }
    Ok(())
}

/// Take `name` out of a feature's switch file; the last name out removes the switch and stops the target.
fn deselect_feature(cat: &Catalogue, name: &str, feature: &str) -> Result<(), String> {
    let f = cat
        .features
        .get(feature)
        .ok_or_else(|| format!("the catalogue describes no feature '{feature}'"))?;
    let path = switch_path(f);
    let left = edit_list(&read_list(&path), &[], &[name.to_string()]);
    write_list(&path, &left, true)?;
    if !left.is_empty() {
        say(format!("{name}: removed; {} stays on for {}", f.title, left.join(", ")));
        return Ok(());
    }
    if f.in_system {
        status(rime_paths::SYSTEMCTL, &["stop", &f.unit])?;
        say(format!(
            "{name}: {} is switched off for this machine; the next `sudo rime update` installs the system without it",
            f.title
        ));
    } else {
        say(format!("{name}: {} is switched off for this machine", f.title));
    }
    Ok(())
}

/// Rewrite the rime-apps profile to exactly the class 2 apps in `requested`: fetch each one's store path from the
/// cache, then one new profile generation.
fn set_apps_profile(cat: &Catalogue, requested: &[String]) -> Result<(), String> {
    let apps = requested_apps(cat, requested);
    let profile = cat.apps_profile.as_str();
    if apps.is_empty() {
        if Path::new(profile).exists() {
            status(&sys("nix-env"), &["--profile", profile, "--uninstall", ".*"])?;
        }
        return Ok(());
    }
    let text = fs::read_to_string(&cat.outs_file).map_err(|e| {
        format!(
            "this machine has no release record naming the builds of Rime's catalogue apps ({}: {e}); \
             `sudo rime update` writes it",
            cat.outs_file
        )
    })?;
    let outs = parse_outs(&text)?;
    let mut paths = vec![];
    for name in &apps {
        let p = outs.get(name).ok_or_else(|| {
            format!("{name}: this machine's release ({}) names no build of it; `sudo rime update` first", cat.outs_file)
        })?;
        paths.push(p.as_str());
    }
    let mut realise = vec!["--realise"];
    realise.extend(&paths);
    status(&sys("nix-store"), &realise).map_err(|e| format!("cannot fetch the apps from the cache: {e}"))?;
    let mut set = vec!["--profile", profile, "--install", "--remove-all"];
    set.extend(&paths);
    status(&sys("nix-env"), &set)
}

/// claude-code's route (rime-pkg user_tool_run): npm as the user behind sudo, into their own ~/.local.
fn npm_as_user(npm: &str, verb: &str, package: &str) -> Result<(), String> {
    let as_root = unsafe { libc::geteuid() } == 0;
    let (user, home) = if as_root {
        let user = std::env::var("SUDO_USER").unwrap_or_default();
        if user.is_empty() || user == "root" {
            return Err(
                "claude-code installs for one user, into their ~/.local; run it as that user with sudo".into(),
            );
        }
        let home = home_of(&user).ok_or_else(|| format!("cannot find the home directory of {user}"))?;
        (Some(user), home)
    } else {
        (None, std::env::var("HOME").map_err(|_| "HOME is not set".to_string())?)
    };
    let npm_dir = Path::new(npm).parent().map(|p| p.display().to_string()).unwrap_or_default();
    // env HOME=... PATH=... npm <verb> -g --prefix ~/.local <package>, under runuser when started as root.
    let mut argv: Vec<String> = vec![];
    if let Some(u) = &user {
        argv.extend(["-u".into(), u.clone(), "--".into(), sys("env")]);
    }
    argv.extend([
        format!("HOME={home}"),
        format!("PATH={npm_dir}:{}", rime_paths::SYSTEM_BIN_DIR),
        npm.to_string(),
        verb.to_string(),
        "-g".into(),
        "--prefix".into(),
        format!("{home}/.local"),
        package.to_string(),
    ]);
    let program = if user.is_some() { sys("runuser") } else { sys("env") };
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    status(&program, &args)
}

fn home_of(user: &str) -> Option<String> {
    let c = std::ffi::CString::new(user).ok()?;
    let pw = unsafe { libc::getpwnam(c.as_ptr()) };
    if pw.is_null() {
        return None;
    }
    let dir = unsafe { std::ffi::CStr::from_ptr((*pw).pw_dir) };
    Some(dir.to_string_lossy().into_owned())
}

/// `--source capsule`: the names go to `rime env install`, unchanged from bootc (a capsule is per-user, so root has
/// none).
fn capsule(names: &[String], env: Option<&str>) -> i32 {
    if unsafe { libc::geteuid() } == 0 {
        return fail(format!(
            "a capsule belongs to a user, and root has none. Run it without sudo: rime install --source capsule {}",
            names.join(" ")
        ));
    }
    let boxname = match env {
        Some(e) => e.to_string(),
        None => match first_capsule() {
            Some(b) => {
                say(format!("using capsule '{b}' (name another with --env)"));
                b
            }
            None => return fail("no capsule to install into. Make one first: rime env create fedora"),
        },
    };
    let mut argv: Vec<&str> = vec!["install", &boxname];
    argv.extend(names.iter().map(String::as_str));
    match Command::new(rime_paths::RIME_ENV).args(&argv).status() {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => fail(format!("cannot run the capsule engine {}: {e}", rime_paths::RIME_ENV)),
    }
}

fn first_capsule() -> Option<String> {
    let base = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOME").ok().map(|h| format!("{h}/.local/share")))?;
    let mut names: Vec<String> = fs::read_dir(Path::new(&base).join("rime/env"))
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".json")).map(String::from))
        .collect();
    names.sort();
    names.into_iter().next()
}

// ── remove ──────────────────────────────────────────────────────────────────────────────────────────────────────

fn remove(cat: &Catalogue, names: &[String]) -> i32 {
    if names.is_empty() {
        return fail("usage: rime remove <name>...");
    }
    let mut rc = 0;
    let mut requested = read_list(Path::new(REQUESTED));
    let mut nix_removed = vec![];
    let mut nix_lines = vec![];
    let mut flatpak_ids = vec![];
    for name in names {
        if is_app_id(name) {
            flatpak_ids.push(name.clone());
            continue;
        }
        match cat.entry(name) {
            Some((_, Entry::Flatpak { id, .. })) => flatpak_ids.push(id.clone()),
            Some((canon, Entry::Toggle { feature, .. })) => match deselect_feature(cat, &canon, feature) {
                Ok(()) => requested = edit_list(&requested, &[], &lines_naming(cat, &requested, &canon)),
                Err(e) => rc = fail(format!("{name}: {e}")),
            },
            Some((canon, Entry::Nix { .. })) => {
                let lines = lines_naming(cat, &requested, &canon);
                if !lines.is_empty() {
                    nix_lines.extend(lines);
                    nix_removed.push(canon);
                } else {
                    rc = fail(format!("{name} was not installed with rime install"));
                }
            }
            Some((canon, Entry::UserNpm { package, npm, .. })) => match npm_as_user(npm, "uninstall", package) {
                Ok(()) => requested = edit_list(&requested, &[], &lines_naming(cat, &requested, &canon)),
                Err(e) => rc = fail(format!("{name}: {e}")),
            },
            Some((_, Entry::Refuse { reason })) => rc = fail(format!("{name}: {reason}")),
            None => rc = fail(format!("{name} is not in Rime's catalogue; to remove a Flatpak, name its id")),
        }
    }
    if !nix_removed.is_empty() {
        let wanted = edit_list(&requested, &[], &nix_lines);
        match set_apps_profile(cat, &wanted) {
            Ok(()) => {
                requested = wanted;
                say(format!("removed: {}", nix_removed.join(", ")));
            }
            Err(e) => rc = fail(e),
        }
    }
    if !flatpak_ids.is_empty() {
        let mut argv: Vec<&str> = vec!["uninstall", "--system", "--noninteractive"];
        argv.extend(flatpak_ids.iter().map(String::as_str));
        match status(flatpak(), &argv) {
            Ok(()) => say(format!("removed: {}", flatpak_ids.join(", "))),
            Err(e) => rc = fail(e),
        }
    }
    if let Err(e) = write_list(Path::new(REQUESTED), &requested, false) {
        rc = fail(e);
    }
    rc
}

// ── read-only verbs ─────────────────────────────────────────────────────────────────────────────────────────────

/// One line per catalogue entry that matches any term (name, alias target or summary, ignoring case).
pub fn catalogue_search<'a>(cat: &'a Catalogue, terms: &[String]) -> Vec<(&'a str, &'a Entry)> {
    let terms: Vec<String> = terms.iter().map(|t| t.to_ascii_lowercase()).collect();
    cat.entries
        .iter()
        .filter(|(name, e)| {
            terms.iter().any(|t| {
                name.contains(t.as_str())
                    || e.summary().to_ascii_lowercase().contains(t.as_str())
                    || cat.aliases.iter().any(|(a, target)| target == *name && a.contains(t.as_str()))
            })
        })
        .map(|(n, e)| (n.as_str(), e))
        .collect()
}

fn search(cat: &Catalogue, terms: &[String]) -> i32 {
    if terms.is_empty() {
        return fail("usage: rime search <term>...");
    }
    let hits = catalogue_search(cat, terms);
    println!("Rime catalogue:");
    if hits.is_empty() {
        println!("  (nothing)");
    }
    for (name, e) in &hits {
        println!("  {name:<20} {:<9} {}", e.kind(), e.summary());
    }
    println!("Flathub:");
    match require_remote(&cat.flathub_remote).and_then(|()| {
        output(flatpak(), &["remote-ls", "--system", "--app", "--columns=application,name", &cat.flathub_remote])
    }) {
        Ok(listing) => {
            let mut n = 0;
            for line in listing.lines() {
                let l = line.to_ascii_lowercase();
                if terms.iter().any(|t| l.contains(&t.to_ascii_lowercase())) {
                    let mut cols = line.split('\t');
                    let id = cols.next().unwrap_or("").trim();
                    let title = cols.next().unwrap_or("").trim();
                    println!("  {id:<40} {title}");
                    n += 1;
                }
            }
            if n == 0 {
                println!("  (nothing)");
            }
        }
        Err(e) => println!("  (not searched: {e})"),
    }
    0
}

fn resolve(cat: &Catalogue, args: &[String]) -> i32 {
    let Some(name) = args.first() else {
        return fail("usage: rime resolve <name>");
    };
    match route(cat, name, None) {
        Route::Flatpak { id, remote, note } => {
            println!("{name}: Flatpak {id} from {remote}");
            if let Some(n) = note {
                println!("  why: {n}");
            }
            println!("  install: sudo rime install {name}");
        }
        Route::Toggle { name: canon, feature } => {
            let f = &cat.features[&feature];
            let state = if f.in_system { "this system carries it" } else { "this system does not carry it yet" };
            println!("{name}: part of Rime's {} system ({state})", f.title);
            println!("  {}", f.summary);
            println!("  install: sudo rime install {canon}");
        }
        Route::Nix { name: canon } => {
            if let Some(Entry::Nix { version, summary, .. }) = cat.entries.get(&canon) {
                println!(
                    "{name}: Rime catalogue app {canon} {}, fetched from Rime's cache",
                    version.clone().unwrap_or_default()
                );
                println!("  {summary}");
            }
            println!("  install: sudo rime install {canon}");
        }
        Route::UserNpm { name: canon, package, .. } => {
            println!("{name}: npm package {package}, installed for you into ~/.local");
            println!("  install: sudo rime install {canon}");
        }
        Route::Flathub { term } => match flathub_lookup(cat, &term) {
            Ok(Some(id)) => {
                println!("{name}: not in Rime's catalogue; Flathub has it as {id}");
                println!("  install: sudo rime install {name}");
            }
            Ok(None) => {
                println!("{name}: not in Rime's catalogue, and Flathub has no application of that name");
                println!("  in a capsule: rime install --source capsule {name}");
                return 1;
            }
            Err(e) => return fail(e),
        },
        Route::Capsule => unreachable!("no --source here"),
        Route::Refuse(reason) => {
            println!("{name}: refused: {reason}");
            return 1;
        }
    }
    0
}

fn list(cat: &Catalogue) -> i32 {
    let requested = read_list(Path::new(REQUESTED));
    println!("Installed with rime install:");
    if requested.is_empty() {
        println!("  (nothing)");
    }
    for name in &requested {
        let what = match cat.entry(name) {
            Some((canon, e)) => {
                let kind = match e {
                    Entry::Toggle { feature, .. } => {
                        let on = cat.features.get(feature).map(|f| f.in_system).unwrap_or(false);
                        format!(
                            "feature {feature} ({})",
                            if on { "in this system" } else { "selected; comes with the next update" }
                        )
                    }
                    e => e.kind().to_string(),
                };
                if canon == *name {
                    kind
                } else {
                    format!("{kind}, as {canon}")
                }
            }
            None => format!("not in Rime's catalogue (rime resolve {name})"),
        };
        println!("  {name:<20} {what}");
    }
    println!("Features selected for this machine:");
    let mut any = false;
    for (name, f) in &cat.features {
        if switch_path(f).exists() {
            any = true;
            let on = if f.in_system { "in this system" } else { "not in this system yet" };
            println!("  {name:<20} {} ({on})", f.title);
        }
    }
    if !any {
        println!("  (none)");
    }
    println!("Flatpak applications (system):");
    match output(flatpak(), &["list", "--system", "--app", "--columns=application"]) {
        Ok(l) if !l.trim().is_empty() => l.lines().for_each(|id| println!("  {}", id.trim())),
        Ok(_) => println!("  (none)"),
        Err(e) => println!("  (not read: {e})"),
    }
    0
}

fn upgrade(cat: &Catalogue) -> i32 {
    let mut rc = 0;
    let requested = read_list(Path::new(REQUESTED));
    if !requested_apps(cat, &requested).is_empty() {
        match set_apps_profile(cat, &requested) {
            Ok(()) => say("catalogue apps: at the builds this release names"),
            Err(e) => rc = fail(e),
        }
    }
    if flatpak_upgrade() != 0 {
        rc = 1;
    }
    rc
}

fn flatpak_upgrade() -> i32 {
    if !Path::new(flatpak()).exists() {
        return 0;
    }
    let rc = match status(flatpak(), &["update", "--system", "--noninteractive"]) {
        Ok(()) => 0,
        Err(e) => fail(e),
    };
    // Then the invoking user's own installation, as rime-pkg cmd_flatpak_upgrade did on bootc: `flatpak install
    // --user` is what many app stores do, and updating only one installation leaves the other stale. A failure here
    // is a warning, as it was there.
    // runuser (like su) sets HOME to the user's own, so `--user` is their installation even under sudo's HOME=/root.
    let user = std::env::var("SUDO_USER").unwrap_or_default();
    if unsafe { libc::geteuid() } == 0 && !user.is_empty() && user != "root" {
        let runuser = sys("runuser");
        let has_remote = output(&runuser, &["-u", &user, "--", flatpak(), "--user", "remotes", "--columns=name"])
            .map(|o| !o.trim().is_empty())
            .unwrap_or(false);
        if has_remote {
            if let Err(e) = status(&runuser, &["-u", &user, "--", flatpak(), "update", "--user", "--noninteractive"]) {
                say(format!("warning: {user}'s own Flatpak update failed: {e}"));
            }
        }
    }
    rc
}

fn verify(cat: &Catalogue) -> i32 {
    let profile = cat.apps_profile.as_str();
    if !Path::new(profile).exists() {
        println!("no catalogue apps installed; nothing to verify");
        return 0;
    }
    match status(&sys("nix-store"), &["--verify-path", profile]) {
        Ok(()) => {
            println!("catalogue apps: every store path matches its recorded hash");
            0
        }
        Err(e) => fail(e),
    }
}

/// Entry point: the argv `ops::pkg` would have handed the bootc engine.
pub fn run(args: &[String]) -> i32 {
    let Some((verb, rest)) = args.split_first() else {
        return fail("no package verb");
    };
    let cat = match Catalogue::load() {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    match verb.as_str() {
        "install" => install(&cat, rest),
        "remove" => remove(&cat, rest),
        "search" => search(&cat, rest),
        "resolve" => resolve(&cat, rest),
        "list" | "info" | "status" => list(&cat),
        "upgrade" => upgrade(&cat),
        "flatpak-upgrade" => flatpak_upgrade(),
        "verify" => verify(&cat),
        "rollback" => match status(&sys("nix-env"), &["--profile", &cat.apps_profile, "--rollback"]) {
            Ok(()) => {
                say("catalogue apps: back to the previous generation");
                0
            }
            Err(e) => fail(e),
        },
        "rebuild" => {
            say("nothing to rebuild: on NixOS installed apps are store paths, not a system extension");
            0
        }
        "adopt" => fail("adopt moved packages into the system extension, and Rime on NixOS has none"),
        "repo-list" => {
            println!(
                "Rime on NixOS uses no package repositories: apps come from Rime's catalogue (rime search) and Flathub."
            );
            0
        }
        "repo-enable-copr" | "repo-disable-copr" => {
            fail("COPR repositories do not exist on Rime on NixOS; apps come from Rime's catalogue and Flathub")
        }
        other => fail(format!("unknown package verb '{other}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape nix/modules/packages-catalogue.nix renders (a cut-down copy; the VM test reads the real one).
    fn fixture() -> Catalogue {
        Catalogue::parse(
            r#"{
              "schema": 1, "flathub_remote": "flathub", "switch_dir": "/var/lib/rime/switch",
              "apps_profile": "/nix/var/nix/profiles/rime-apps", "outs_file": "/var/lib/rime/nix/catalogue-outs.json",
              "features": {
                "gaming": {"title": "Gaming", "offered": true, "in_system": false, "unit": "rime-feature@gaming.target",
                           "switch": "/var/lib/rime/switch/rime-feature@gaming.target.on", "summary": "Steam"},
                "virtualization": {"title": "Virtual machines", "offered": false, "in_system": false,
                                   "unit": "rime-feature@virtualization.target",
                                   "switch": "/var/lib/rime/switch/rime-feature@virtualization.target.on",
                                   "reason": "no system carries it yet"}
              },
              "entries": {
                "steam": {"kind": "toggle", "feature": "gaming", "summary": "Steam"},
                "qemu": {"kind": "toggle", "feature": "virtualization", "summary": "QEMU"},
                "blender": {"kind": "flatpak", "id": "org.blender.Blender", "summary": "Blender", "why": "A4"},
                "go": {"kind": "nix", "buildable": true, "summary": "Go", "version": "1.26.10"},
                "chatgpt": {"kind": "nix", "buildable": false, "summary": "ChatGPT", "pending": "not built yet"},
                "claude-code": {"kind": "user-npm", "package": "@anthropic-ai/claude-code",
                                "npm": "/nix/store/x-nodejs/bin/npm", "summary": "Claude Code"},
                "waydroid": {"kind": "refuse", "reason": "needs binder"}
              },
              "aliases": {"golang": "go", "qemu-kvm": "qemu"},
              "refuse": [{"reason": "part of the OS", "patterns": ["kernel", "kernel-*", "*-kmod", "nvidia*"]}]
            }"#,
        )
        .expect("fixture parses")
    }

    #[test]
    fn every_kind_routes_to_its_own_route() {
        let c = fixture();
        assert_eq!(route(&c, "steam", None), Route::Toggle { name: "steam".into(), feature: "gaming".into() });
        assert_eq!(
            route(&c, "blender", None),
            Route::Flatpak { id: "org.blender.Blender".into(), remote: "flathub".into(), note: Some("A4".into()) }
        );
        assert_eq!(route(&c, "go", None), Route::Nix { name: "go".into() });
        assert_eq!(
            route(&c, "claude-code", None),
            Route::UserNpm {
                name: "claude-code".into(),
                package: "@anthropic-ai/claude-code".into(),
                npm: "/nix/store/x-nodejs/bin/npm".into()
            }
        );
        assert_eq!(route(&c, "waydroid", None), Route::Refuse("needs binder".into()));
    }

    #[test]
    fn aliases_and_case_reach_the_same_entry() {
        let c = fixture();
        assert_eq!(route(&c, "golang", None), Route::Nix { name: "go".into() });
        assert_eq!(route(&c, "GoLang", None), Route::Nix { name: "go".into() });
        assert_eq!(route(&c, "Steam", None), Route::Toggle { name: "steam".into(), feature: "gaming".into() });
    }

    #[test]
    fn a_feature_no_system_carries_is_refused_with_its_reason() {
        let c = fixture();
        assert_eq!(route(&c, "qemu-kvm", None), Route::Refuse("no system carries it yet".into()));
    }

    #[test]
    fn an_unbuilt_app_is_refused_with_its_pending_reason() {
        assert_eq!(route(&fixture(), "chatgpt", None), Route::Refuse("not built yet".into()));
    }

    #[test]
    fn operating_system_names_are_refused_by_pattern() {
        let c = fixture();
        for n in ["kernel", "kernel-core", "foo-kmod", "nvidia-driver", "KERNEL"] {
            assert_eq!(route(&c, n, None), Route::Refuse("part of the OS".into()), "{n}");
        }
    }

    #[test]
    fn files_and_github_are_refused_and_say_what_to_do_instead() {
        let c = fixture();
        assert_eq!(route(&c, "./thing.rpm", None), Route::Refuse(RPM_REASON.into()));
        assert_eq!(route(&c, "thing.deb", None), Route::Refuse(RPM_REASON.into()));
        assert_eq!(route(&c, "Thing.AppImage", None), Route::Refuse(APPIMAGE_REASON.into()));
        assert_eq!(route(&c, "github:o/r", None), Route::Refuse(GITHUB_REASON.into()));
        assert_eq!(route(&c, "htop", Some("rpm")), Route::Refuse(RPM_REASON.into()));
        assert!(matches!(route(&c, "htop", Some("dnf")), Route::Refuse(r) if r.contains("--source dnf")));
    }

    #[test]
    fn app_ids_go_to_flathub_and_unknown_names_are_looked_up_there() {
        let c = fixture();
        assert_eq!(
            route(&c, "org.gimp.GIMP", None),
            Route::Flatpak { id: "org.gimp.GIMP".into(), remote: "flathub".into(), note: None }
        );
        assert_eq!(route(&c, "gimp", None), Route::Flathub { term: "gimp".into() });
        assert_eq!(route(&c, "blender", Some("capsule")), Route::Capsule);
        assert_eq!(
            route(&c, "blender", Some("flatpak")),
            Route::Flatpak { id: "org.blender.Blender".into(), remote: "flathub".into(), note: None }
        );
    }

    #[test]
    fn flathub_matches_only_the_app_that_is_the_term() {
        let listing = "org.gimp.GIMP\tGNU Image Manipulation Program\n\
                       org.example.GimpHelper\tGimp helper\n\
                       com.visualstudio.code\tVisual Studio Code\n\
                       not-an-id\tgimp\n";
        assert_eq!(flathub_matches("gimp", listing), vec!["org.gimp.GIMP".to_string()]);
        assert_eq!(flathub_matches("visual studio code", listing), vec!["com.visualstudio.code".to_string()]);
        assert!(flathub_matches("image", listing).is_empty());
    }

    #[test]
    fn app_id_shape() {
        assert!(is_app_id("org.gimp.GIMP"));
        assert!(is_app_id("app.zen_browser.zen"));
        assert!(!is_app_id("libgcc.i686"));
        assert!(!is_app_id("1org.a.b"));
        assert!(!is_app_id("org..b"));
    }

    #[test]
    fn globs() {
        assert!(glob("kernel-*", "kernel-core"));
        assert!(!glob("kernel-*", "kernel"));
        assert!(glob("*-kmod", "nvidia-kmod"));
        assert!(glob("*rpm*", "librpm-x"));
        assert!(glob("bash", "bash"));
        assert!(!glob("bash", "bashtop"));
    }

    #[test]
    fn lists_round_trip_and_the_last_name_out_removes_the_file() {
        let dir = std::env::temp_dir().join(format!("rime-pkg-test-{}", std::process::id()));
        let path = dir.join("switch/gaming");
        let one = edit_list(&read_list(&path), &["steam".into()], &[]);
        write_list(&path, &one, false).unwrap();
        let two = edit_list(&read_list(&path), &["gamescope".into(), "steam".into()], &[]);
        write_list(&path, &two, false).unwrap();
        assert_eq!(read_list(&path), vec!["steam".to_string(), "gamescope".to_string()]);
        let left = edit_list(&read_list(&path), &[], &["steam".into()]);
        write_list(&path, &left, true).unwrap();
        assert!(path.exists());
        let none = edit_list(&read_list(&path), &[], &["gamescope".into()]);
        write_list(&path, &none, true).unwrap();
        assert!(!path.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_release_record_names_only_store_paths() {
        let ok = parse_outs(r#"{"apps": {"go": "/nix/store/abc-go-1.26"}}"#).unwrap();
        assert_eq!(ok["go"], "/nix/store/abc-go-1.26");
        assert!(parse_outs(r#"{"apps": {"go": "/tmp/evil"}}"#).is_err());
        assert!(parse_outs(r#"{"apps": {"go": "/nix/store/../etc"}}"#).is_err());
    }

    #[test]
    fn install_flags_that_mean_rpms_are_refused_by_name() {
        let a = parse_install(&[
            "steam".into(),
            "--allow-unsigned".into(),
            "--enable-repo=x".into(),
            "--source=flatpak".into(),
            "--no-weak-deps".into(),
        ]);
        assert_eq!(a.names, vec!["steam".to_string()]);
        assert_eq!(a.source.as_deref(), Some("flatpak"));
        assert_eq!(a.refusals.len(), 2);
        assert_eq!(a.notes.len(), 1);
    }

    #[test]
    fn search_finds_names_summaries_and_aliases() {
        let c = fixture();
        let names = |t: &str| catalogue_search(&c, &[t.into()]).iter().map(|(n, _)| n.to_string()).collect::<Vec<_>>();
        assert_eq!(names("blend"), vec!["blender"]);
        assert_eq!(names("golang"), vec!["go"]);
        assert!(names("claude").contains(&"claude-code".to_string()));
    }

    #[test]
    fn a_requested_list_carried_over_from_bootc_is_read_through_the_aliases() {
        let c = fixture();
        let list: Vec<String> = ["golang", "steam", "go", "chatgpt", "something-else"].map(String::from).into();
        // golang and go are one app; steam is a feature, chatgpt an unbuilt app, something-else unknown.
        assert_eq!(requested_apps(&c, &list), vec!["go".to_string()]);
        assert_eq!(lines_naming(&c, &list, "go"), vec!["golang".to_string(), "go".to_string()]);
        assert_eq!(lines_naming(&c, &list, "steam"), vec!["steam".to_string()]);
        assert!(lines_naming(&c, &list, "blender").is_empty());
    }

    #[test]
    fn a_newer_schema_is_refused() {
        assert!(Catalogue::parse(
            r#"{"schema": 2, "flathub_remote": "f", "switch_dir": "/s", "apps_profile": "/p", "outs_file": "/o"}"#
        )
        .unwrap_err()
        .contains("schema 2"));
    }
}
