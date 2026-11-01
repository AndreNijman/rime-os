//! `rime blueprint` — where the blueprint lives, what the machine actually is,
//! and how the two differ.
//!
//! [`rimed_core::blueprint`] holds the schema and the pure planner and touches
//! nothing. This module is the other half: it finds the files, *measures* the
//! machine, and renders the result. The split is the same one `rimed-core`
//! already uses for power tiers — plan in the pure crate, touch from outside —
//! and it is what lets the planner be exhaustively unit-tested with no machine.
//!
//! ── The three files, and which one is authoritative ─────────────────────────
//!
//! ```text
//!   ~/.config/rime/blueprint.toml        DESIRED   — you edit this
//!   /etc/rime/blueprint.toml             DESIRED   — site default, used when
//!                                                    the user has no file
//!   ~/.local/state/rime/blueprint-state.toml
//!                                        APPLIED   — generated, never read as
//!                                                    intent
//! ```
//!
//! Observed state has no file on purpose. `diff` probes the machine every time,
//! because a cached "current state" is how a converger ends up agreeing with
//! itself: a step that silently did nothing would report converged forever.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rimed_core::blueprint::{
    self, AppliedState, Blueprint, CapsuleLanguage, Change, Domain, Observed, Plan, Step,
    SCHEMA_VERSION,
};

/// `/var/lib/rime-greet/last-session` — the session id the greeter preselects.
/// Written by `rime-session-select` and re-written by greetd on every
/// successful login, so it is the machine's real answer to "which desktop does
/// this box boot into".
const GREETER_SESSION: &str = "var/lib/rime-greet/last-session";

/// Where the shipped `.desktop` sessions live.
const SESSION_DIR: &str = "usr/share/wayland-sessions";

/// The package engine's requested-package list: one name per line, with local
/// `.rpm` entries recorded as `local:<NAME>`.
///
/// This, and not `state.json`, is the right source for "what did the user ask
/// to have installed" — `state.json` records the resolved transaction
/// including every dependency, so diffing a blueprint against it would report
/// convergence based on packages nobody asked for.
const REQUESTED_LIST: &str = "var/lib/rime/pkg/requested";

/// `[desktop] theme` is Rime Shell's matugen scheme, and this is where the
/// shell keeps it.
const WALLPAPER_JSON: &str = ".config/rime-shell/src/user_data/wallpaper.json";

/// Where `rime-env` keeps one JSON record per capsule, relative to the data
/// home.
///
/// **The precedence has to match the engine's, not merely resemble it.**
/// `rime-env` resolves `${RIME_ENV_HOME:-${XDG_DATA_HOME:-$HOME/.local/share}/rime/env}`
/// once, at the top of the file, with a comment explaining that a helper which
/// recomputed the path from `$HOME` would quietly write to the real one. The
/// same trap applies from this side: if the writer and the observer disagree
/// about the directory, `apply` provisions a capsule and the next `diff` cannot
/// see it — and the disagreement shows up first in the isolated-HOME test,
/// which is exactly where it looks like a broken test rather than a broken
/// path.
const CAPSULE_RECORDS: &str = "rime/env";

/// The capsule engine's own override for that directory. Honoured here so the
/// suite can point both halves at one throwaway tree.
const CAPSULE_HOME_ENV: &str = "RIME_ENV_HOME";

/// The shell's own fallback when `wallpaper.json` has no `scheme`, from
/// `WallpaperService.qml`'s `property string scheme: "content"`. An absent file
/// is therefore a *measurement* of "content", not an unknown.
const SHELL_DEFAULT_SCHEME: &str = "content";

/// A blueprint to start from, written by `rime blueprint init`.
///
/// Built as a string rather than `include_str!`'d from `config/`. Two reasons:
/// `rimed-core` already `include_str!`s `config/sysprofiles/*.toml`, which is
/// the sole reason `files/scripts/clippy-local` has to mount the repository
/// root instead of the workspace, and there is no need to widen that coupling
/// for a starter file. And a committed `.toml` would be parsed by the
/// `static` job's `tomllib` sweep, which is fine here but becomes a trap the
/// moment anyone adds a deliberately-invalid example next to it.
const STARTER: &str = r#"# Rime Blueprint — what this machine should be.
#
# `rime blueprint diff` shows how the machine currently differs from this file.
# `rime apply` converges it. Nothing here is applied until you ask.
#
# Every section is optional, and leaving one out means "Rime does not manage
# this", NOT "use the default". A blueprint that asserted defaults for
# everything it did not mention would reformat a machine the first time it ran.
#
# Unknown keys are an error rather than being ignored, so a typo is loud
# instead of producing an `rime apply` that succeeds and changes nothing.

# [desktop]
# compositor = "hyprland"   # hyprland | niri | labwc
# theme = "content"         # content | tonal-spot | fidelity | fruit-salad
#                           # | neutral | monochrome

# [apps]
# Repository package names and Flatpak application ids in one list, classified
# the same way `rime install` classifies its arguments.
# install = ["firefox", "md.obsidian.Obsidian"]

# [development]
# A language is satisfied by a toolchain already on PATH — the Rime images ship
# a full dev stack — or by an `rime env` capsule that provides it. Anything
# neither is provisioned as a capsule, never installed onto the read-only host.
# `rime env languages` is the table.
# languages = ["python", "rust", "typescript"]

# [agent]
# default = "claude"        # claude | opencode | codex | gemini | kimi | generic
# sandbox = "project"       # project | strict | unrestricted

# [gaming]
# Observed and reported, never converged: the gaming userspace is installed on
# demand with `rime install`, which is a deliberate user action, not drift.
# enabled = true
"#;

// ── where things live ────────────────────────────────────────────────────────
//
// `rime_agent_core::paths` already resolves `$HOME`, `$XDG_CONFIG_HOME` and
// `$XDG_STATE_HOME` correctly — including the getpwuid fallback for a process
// started without a login environment — and it is already tested. Reusing it
// beats a second implementation of the base-directory spec in this file.

/// `~/.config/rime/blueprint.toml`, or `$XDG_CONFIG_HOME`'s equivalent.
pub fn user_blueprint_path() -> PathBuf {
    rime_agent_core::paths::config_home().join("rime/blueprint.toml")
}

/// `/etc/rime/blueprint.toml` — a site default for an image or an
/// administrator, used only when the user has no blueprint of their own.
pub fn site_blueprint_path() -> PathBuf {
    PathBuf::from("/etc/rime/blueprint.toml")
}

/// `~/.local/state/rime/blueprint-state.toml`. Generated by `rime apply`.
pub fn applied_state_path() -> PathBuf {
    rime_agent_core::paths::state_home().join("rime/blueprint-state.toml")
}

/// Which file a blueprint came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// An explicit `--file`.
    Explicit(PathBuf),
    /// The user's own blueprint.
    User(PathBuf),
    /// `/etc/rime/blueprint.toml`, because the user has none.
    Site(PathBuf),
    /// Nothing on disk. An empty blueprint that manages nothing.
    None,
}

impl Source {
    pub fn path(&self) -> Option<&Path> {
        match self {
            Source::Explicit(p) | Source::User(p) | Source::Site(p) => Some(p),
            Source::None => None,
        }
    }

    fn describe(&self) -> String {
        match self {
            Source::Explicit(p) => format!("{} (--file)", p.display()),
            Source::User(p) => p.display().to_string(),
            Source::Site(p) => format!("{} (site default; you have no blueprint of your own)", p.display()),
            Source::None => format!(
                "none — no blueprint at {} and no site default at {}",
                user_blueprint_path().display(),
                site_blueprint_path().display()
            ),
        }
    }
}

/// Load the blueprint, saying where it came from.
///
/// A missing file is an empty blueprint, not an error: `rime blueprint diff` on
/// a machine nobody has configured should report "nothing is managed", not fail.
/// A file that exists and is *wrong* is always an error — that is the whole
/// point of the schema.
pub fn load(explicit: Option<&Path>) -> Result<(Blueprint, Source), String> {
    let candidates: Vec<Source> = match explicit {
        Some(p) => vec![Source::Explicit(p.to_path_buf())],
        None => vec![
            Source::User(user_blueprint_path()),
            Source::Site(site_blueprint_path()),
        ],
    };

    for source in candidates {
        let path = source.path().expect("candidate sources always carry a path");
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let bp = Blueprint::parse(&text)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                return Ok((bp, source));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // An explicit --file that does not exist IS an error: the user
                // named it, so silently falling back to an empty blueprint
                // would run `apply` against the wrong thing.
                if matches!(source, Source::Explicit(_)) {
                    return Err(format!("{}: no such file", path.display()));
                }
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Ok((Blueprint::default(), Source::None))
}

/// Read the generated record of the last `apply`, if there is one.
///
/// Never fatal and never fed to the planner. A corrupt record loses history and
/// nothing else.
pub fn load_applied() -> Option<AppliedState> {
    let text = std::fs::read_to_string(applied_state_path()).ok()?;
    AppliedState::parse(&text).ok()
}

// ── measuring the machine ────────────────────────────────────────────────────

/// Reads (never writes) the state a blueprint talks about.
///
/// `root` exists for the same reason [`rimed_core::syswriter::RealWriter`]'s
/// `sys_root` does: it lets a test point the probes at a fixture tree. And
/// `probe_programs` exists for the same reason that writer needs a separate
/// `host_commands` switch — no fixture root can redirect a process spawn, so a
/// test with a fixture root would still ask the *host's* `flatpak` what it has
/// installed, and the answer would depend on the developer's machine.
pub struct Host {
    root: PathBuf,
    probe_programs: bool,
}

impl Default for Host {
    fn default() -> Host {
        Host::new()
    }
}

impl Host {
    /// The real machine: `/` and permitted to run the read-only probes
    /// (`flatpak list`, and `PATH` lookups for toolchains).
    pub fn new() -> Host {
        Host {
            root: PathBuf::from("/"),
            probe_programs: true,
        }
    }

    /// A fixture tree, with process probes off.
    ///
    /// `#[cfg(test)]` rather than merely unused-in-production: `rime` is a
    /// binary crate, so an unreferenced constructor is dead code and CI runs
    /// clippy with `-D warnings`. Gating it also states the intent — there is
    /// no production caller and there should not be one.
    #[cfg(test)]
    pub fn with_root(root: impl Into<PathBuf>) -> Host {
        Host {
            root: root.into(),
            probe_programs: false,
        }
    }

    fn at(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// The user's home, as the blueprint's user-owned targets see it.
    ///
    /// Under a fixture root this is `<root>/home`, so a test can lay out a
    /// `wallpaper.json` without touching the developer's own.
    fn home(&self) -> PathBuf {
        if self.root == Path::new("/") {
            rime_agent_core::paths::home()
        } else {
            self.root.join("home")
        }
    }

    /// Everything the planner needs, measured now.
    pub fn observe(&self) -> Observed {
        let (packages, packages_unreadable) = match self.requested_packages() {
            Ok(names) => (names, None),
            Err(why) => (Vec::new(), Some(why)),
        };
        Observed {
            session: read_trimmed(&self.at(GREETER_SESSION)),
            sessions_available: self.sessions_available(),
            theme: self.theme(),
            packages,
            packages_unreadable,
            flatpaks: self.installed_flatpaks(),
            languages: self.languages(),
            capsule_languages: self.capsule_languages(),
            agent_default: self.agent_config().map(|(a, _)| a),
            agent_sandbox: self.agent_config().map(|(_, s)| s),
            variant_id: self.variant_id(),
        }
    }

    /// Session ids in `/usr/share/wayland-sessions`, sorted. The same set
    /// `rime-session-select --list` prints and validates against.
    /// The sessions a user could actually pick at the greeter.
    ///
    /// `TryExec` is honoured, and that is not a refinement — it is what keeps
    /// this field meaning anything. Rime ships one image for every laptop, so
    /// `rime-gaming.desktop` is present on EVERY machine while gamescope and
    /// Steam install on demand. A plain directory listing would therefore report
    /// the gaming session as available everywhere, which is both untrue and the
    /// opposite of what the greeter shows: the greeter skips an entry whose
    /// `TryExec` binary is absent, and so does this.
    fn sessions_available(&self) -> Vec<String> {
        let mut out: Vec<String> = match std::fs::read_dir(self.at(SESSION_DIR)) {
            Ok(entries) => entries
                .flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    let id = name.strip_suffix(".desktop")?.to_string();
                    let body = std::fs::read_to_string(e.path()).unwrap_or_default();
                    let try_exec = body
                        .lines()
                        .find_map(|l| l.strip_prefix("TryExec="))
                        .map(str::trim)
                        .filter(|v| !v.is_empty());
                    match try_exec {
                        Some(bin) if !self.try_exec_resolves(bin) => None,
                        _ => Some(id),
                    }
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        out.sort();
        out
    }

    /// Resolve a `TryExec` value the way the desktop-entry spec asks: an
    /// absolute path is tested directly, a bare name is looked up on `PATH`.
    /// Both are resolved under the observation root so the tests can build a
    /// machine that does or does not have the binary.
    fn try_exec_resolves(&self, bin: &str) -> bool {
        if bin.starts_with('/') {
            return self.at(bin.trim_start_matches('/')).is_file();
        }
        ["usr/local/bin", "usr/bin", "usr/sbin", "bin", "sbin"]
            .iter()
            .any(|d| self.at(&format!("{d}/{bin}")).is_file())
    }

    /// Rime Shell's matugen scheme.
    ///
    /// An absent file is `content`, not unknown: `WallpaperService.qml`
    /// initialises `scheme` to `"content"` and only overrides it when the JSON
    /// carries a non-empty value, so "no file" has a definite effective value.
    /// A file that exists and is unreadable or malformed IS unknown, because
    /// then the shell's behaviour depends on how far its own parse got.
    fn theme(&self) -> Option<String> {
        let path = self.home().join(WALLPAPER_JSON);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Some(SHELL_DEFAULT_SCHEME.to_string())
            }
            Err(_) => return None,
        };
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        match value.get("scheme").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => Some(s.to_string()),
            // Present, parses, no scheme key: the shell's default applies.
            _ => Some(SHELL_DEFAULT_SCHEME.to_string()),
        }
    }

    /// The package engine's requested list, with the `local:` form stripped.
    ///
    /// An absent list is an empty one, and that is the common case: most
    /// machines install nothing with `rime install`. A list that exists and
    /// could not be read is not. `/var/lib/rime/pkg` is 0700 root:root by a
    /// tmpfiles.d rule and `rime blueprint diff` is something a desktop session
    /// runs as itself, so collapsing the two made the planner report "0 of N
    /// present" and offer to install packages the machine already had.
    fn requested_packages(&self) -> Result<Vec<String>, String> {
        let path = self.at(REQUESTED_LIST);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        Ok(text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| l.strip_prefix("local:").unwrap_or(l).to_string())
            .collect())
    }

    /// Flatpak application ids installed system-wide or for this user.
    ///
    /// `flatpak list` is read-only and cannot prompt. If flatpak is absent this
    /// is empty, and the converger — not the planner — reports the failure when
    /// a step needs it, so a machine with no flatpak gets one clear message
    /// instead of a silently different diff.
    fn installed_flatpaks(&self) -> Vec<String> {
        if !self.probe_programs {
            return Vec::new();
        }
        let Ok(out) = std::process::Command::new("flatpak")
            .args(["list", "--app", "--columns=application"])
            .output()
        else {
            return Vec::new();
        };
        if !out.status.success() {
            return Vec::new();
        }
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// Which of the blueprint's languages have a usable toolchain on `PATH`.
    ///
    /// A deliberately shallow measurement: "is a compiler or interpreter
    /// reachable". It is the honest thing to report while `[development]` has
    /// no converger — phase 6's capsules replace it with a real answer about
    /// which capsule provides which language.
    fn languages(&self) -> Vec<String> {
        const PROBES: [(&str, &[&str]); 8] = [
            ("c", &["gcc", "clang", "cc"]),
            ("cpp", &["g++", "clang++"]),
            ("go", &["go"]),
            ("javascript", &["node"]),
            ("python", &["python3"]),
            ("rust", &["cargo", "rustc"]),
            ("shell", &["bash"]),
            ("typescript", &["tsc", "ts-node"]),
        ];
        if !self.probe_programs {
            return Vec::new();
        }
        PROBES
            .iter()
            .filter(|(_, bins)| bins.iter().any(|b| on_path(b)))
            .map(|(lang, _)| (*lang).to_string())
            .collect()
    }

    /// Where `rime-env` keeps its capsule records.
    ///
    /// The engine's precedence, in the engine's order. `RIME_ENV_HOME` is
    /// honoured only on a real root, because a fixture root cannot redirect an
    /// environment variable and a unit test that read the developer's own
    /// `RIME_ENV_HOME` would answer from their machine — the same reason
    /// `probe_programs` is a separate switch from `root`.
    fn capsule_record_dir(&self) -> PathBuf {
        if self.root == Path::new("/") {
            if let Some(dir) = std::env::var_os(CAPSULE_HOME_ENV) {
                if !dir.is_empty() {
                    return PathBuf::from(dir);
                }
            }
            if let Some(dir) = std::env::var_os("XDG_DATA_HOME") {
                if !dir.is_empty() {
                    return PathBuf::from(dir).join(CAPSULE_RECORDS);
                }
            }
        }
        self.home().join(".local/share").join(CAPSULE_RECORDS)
    }

    /// Which languages the capsules on this machine record themselves as
    /// providing.
    ///
    /// A file read, deliberately, and not `rime env list --json`: a probe must
    /// not depend on a process, and the record is the state — `rime env
    /// provision` writes the language into it only after the toolchain answers
    /// from inside the capsule, so reading the file is reading a measurement
    /// somebody already made properly.
    ///
    /// A record that does not parse contributes nothing and is not an error.
    /// The alternative — failing the whole observation — would make one
    /// hand-edited capsule record break `rime blueprint diff` for every
    /// section.
    fn capsule_languages(&self) -> Vec<CapsuleLanguage> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(self.capsule_record_dir()) else {
            return out;
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        // Sorted, so the order a diff reports is the same on every run
        // regardless of what readdir happens to hand back.
        files.sort();
        for path in files {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                continue;
            };
            // The capsule's name comes out of the record rather than off the
            // filename: they are the same today, and the record is the thing
            // `rime env info` prints, so a user told "capsule rust provides
            // rust" can act on it.
            let Some(capsule) = value.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(langs) = value.get("languages").and_then(|v| v.as_array()) else {
                continue;
            };
            for lang in langs.iter().filter_map(|v| v.as_str()) {
                out.push(CapsuleLanguage {
                    language: lang.to_string(),
                    capsule: capsule.to_string(),
                });
            }
        }
        out
    }

    /// `default_agent` and `sandbox` from the agent runtime's own config.
    ///
    /// Loaded through `rime_agent_core::config`, never by hand-parsing the
    /// JSON: that type carries `#[serde(flatten)] extra` so a key written by a
    /// newer Rime Shell survives a round-trip, and re-implementing the read
    /// here would be the first half of losing it.
    fn agent_config(&self) -> Option<(String, String)> {
        if self.root != Path::new("/") {
            // The agent config resolves through $XDG_CONFIG_HOME, which a
            // fixture root cannot redirect. Reporting it as unmeasured is
            // honest; the real path is exercised by the shell suite, which
            // isolates HOME instead of using a fixture root.
            return None;
        }
        let cfg = rime_agent_core::config::Config::load();
        Some((cfg.default_agent.clone(), cfg.sandbox.as_str().to_string()))
    }

    /// `VARIANT_ID` from `/etc/os-release`. Reported in the `[gaming]` message
    /// so a user is told what the machine actually is.
    fn variant_id(&self) -> Option<String> {
        let text = std::fs::read_to_string(self.at("etc/os-release")).ok()?;
        for line in text.lines() {
            if let Some(v) = line.strip_prefix("VARIANT_ID=") {
                return Some(v.trim().trim_matches('"').to_string());
            }
        }
        None
    }
}

fn read_trimmed(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Whether `program` is an executable file somewhere on `PATH`.
fn on_path(program: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        let candidate = dir.join(program);
        candidate.is_file() && is_executable(&candidate)
    })
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

// ── rendering ────────────────────────────────────────────────────────────────

/// Exit code for `rime blueprint diff` when the machine has convergeable drift.
///
/// Chosen so the command reads like `diff(1)` and is usable in a script:
/// 0 converged, 1 drifted, 2 something went wrong. Blocked changes do not set
/// it — they are not drift anyone can close, and a permanently non-zero exit
/// would make the signal useless.
pub const EXIT_DRIFT: i32 = 1;
pub const EXIT_ERROR: i32 = 2;

/// `rime blueprint show`.
pub fn cmd_show(file: Option<&Path>, json: bool) -> i32 {
    let (bp, source) = match load(file) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("rime blueprint: {e}");
            return EXIT_ERROR;
        }
    };
    let applied = load_applied();

    if json {
        let mut obj = serde_json::Map::new();
        obj.insert("schema".into(), SCHEMA_VERSION.into());
        obj.insert(
            "source".into(),
            source
                .path()
                .map(|p| p.display().to_string())
                .map_or(serde_json::Value::Null, serde_json::Value::from),
        );
        obj.insert("digest".into(), bp.digest().into());
        obj.insert(
            "blueprint".into(),
            serde_json::to_value(&bp).unwrap_or(serde_json::Value::Null),
        );
        obj.insert(
            "applied".into(),
            applied
                .as_ref()
                .and_then(|a| serde_json::to_value(a).ok())
                .unwrap_or(serde_json::Value::Null),
        );
        obj.insert(
            "paths".into(),
            serde_json::json!({
                "user": user_blueprint_path().display().to_string(),
                "site": site_blueprint_path().display().to_string(),
                "applied_state": applied_state_path().display().to_string(),
            }),
        );
        println!("{}", serde_json::Value::Object(obj));
        return 0;
    }

    println!("blueprint  {}", source.describe());
    println!("digest     {}", bp.digest());
    match &applied {
        Some(a) if a.blueprint_digest == bp.digest() => println!(
            "applied    {} ago, as {} ({} step{})",
            ago(a.applied_at),
            a.domain,
            a.steps.len(),
            if a.steps.len() == 1 { "" } else { "s" }
        ),
        Some(a) => println!(
            "applied    {} ago, as {} — but against a DIFFERENT blueprint ({})",
            ago(a.applied_at),
            a.domain,
            a.blueprint_digest
        ),
        None => println!("applied    never on this machine"),
    }
    println!(
        "state      {}  (generated; never edit it)",
        applied_state_path().display()
    );
    println!();

    if bp == Blueprint::default() {
        println!("This blueprint manages nothing.");
        println!("`rime blueprint init` writes a commented starting point.");
        return 0;
    }

    let text = match bp.to_toml() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("rime blueprint: cannot render: {e}");
            return EXIT_ERROR;
        }
    };
    print!("{text}");
    0
}

/// `rime blueprint diff`.
pub fn cmd_diff(file: Option<&Path>, json: bool) -> i32 {
    let (bp, source) = match load(file) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("rime blueprint: {e}");
            return EXIT_ERROR;
        }
    };
    let observed = Host::new().observe();
    let plan = blueprint::plan(&bp, &observed);

    if json {
        println!("{}", plan_json(&bp, &source, &plan));
    } else {
        print_plan(&plan, &source, "would change");
    }

    if plan.is_converged() {
        0
    } else {
        EXIT_DRIFT
    }
}

/// `rime blueprint init`.
pub fn cmd_init(force: bool) -> i32 {
    let path = user_blueprint_path();
    if path.exists() && !force {
        eprintln!(
            "rime blueprint: {} already exists; --force overwrites it",
            path.display()
        );
        return EXIT_ERROR;
    }
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("rime blueprint: cannot create {}: {e}", parent.display());
            return EXIT_ERROR;
        }
    }
    if let Err(e) = write_atomic(&path, STARTER) {
        eprintln!("rime blueprint: {e}");
        return EXIT_ERROR;
    }
    println!("wrote {}", path.display());
    println!("Every section is commented out, so it manages nothing until you edit it.");
    0
}

/// Replace the user's blueprint with one supplied as JSON on stdin.
///
/// This exists so the GUI editor (§10's last bullet, "allow GUI editing of the
/// blueprint without requiring users to hand-edit TOML") has a write path that
/// is not "author TOML in QML". A second implementation of the schema in the
/// shell would drift from this one the first time a field is added, and the
/// round-trip would stop being lossless — which is the property the whole
/// design rests on.
///
/// The JSON goes through exactly the same `normalise()` + `validate()` as a
/// hand-edited file, then `to_toml()` and the same atomic write. So a blueprint
/// the editor produces is indistinguishable from one a human typed, and an
/// invalid one is refused with the same messages.
///
/// Three things it deliberately does NOT do:
///
///   * converge anything. Writing desired state and changing the machine are
///     separate verbs, and `apply` is the one that changes things.
///   * touch the applied-state file. That is generated state, in a different
///     directory, and the rule that keeps `diff` honest is that the two are
///     never written by the same code.
///   * escalate. It writes one file the invoking user already owns.
pub fn cmd_set(from_stdin: bool) -> i32 {
    if !from_stdin {
        eprintln!("rime blueprint set: reads JSON on stdin; pass --json -");
        return EXIT_ERROR;
    }

    let mut text = String::new();
    if let Err(e) = std::io::Read::read_to_string(&mut std::io::stdin(), &mut text) {
        eprintln!("rime blueprint set: cannot read stdin: {e}");
        return EXIT_ERROR;
    }

    // An empty stdin is a caller bug — a pipe that produced nothing, a shell
    // redirect from a missing file — and writing an empty blueprint would
    // silently unmanage everything the user had declared.
    if text.trim().is_empty() {
        eprintln!("rime blueprint set: stdin was empty; refusing to write an empty blueprint");
        return EXIT_ERROR;
    }

    let bp: Blueprint = match serde_json::from_str(&text) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("rime blueprint set: not a valid blueprint: {e}");
            return EXIT_ERROR;
        }
    };

    // Round-trip through the TOML parser rather than trusting the JSON path:
    // `normalise()` and `validate()` live behind `Blueprint::parse`, and calling
    // them from two places is how the two paths drift.
    let toml_text = match bp.to_toml() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("rime blueprint set: cannot render: {e}");
            return EXIT_ERROR;
        }
    };
    let checked = match Blueprint::parse(&toml_text) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("rime blueprint set: {e}");
            return EXIT_ERROR;
        }
    };
    let out = match checked.to_toml() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("rime blueprint set: cannot render: {e}");
            return EXIT_ERROR;
        }
    };

    let path = user_blueprint_path();
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("rime blueprint set: cannot create {}: {e}", parent.display());
            return EXIT_ERROR;
        }
    }
    if let Err(e) = write_atomic(&path, &out) {
        eprintln!("rime blueprint set: {e}");
        return EXIT_ERROR;
    }
    println!("wrote {}", path.display());
    0
}

/// Render a plan as a table. `verb` is how the steps are introduced, so the
/// same renderer serves `diff` ("would change") and `apply --dry-run`.
pub fn print_plan(plan: &Plan, source: &Source, verb: &str) {
    if let Some(p) = source.path() {
        println!("blueprint  {}", p.display());
    } else {
        println!("blueprint  {}", source.describe());
    }

    let actionable: Vec<&Change> = plan.changes.iter().filter(|c| c.step.is_some()).collect();
    let blocked = plan.blocked();

    if actionable.is_empty() && blocked.is_empty() {
        println!();
        println!("The machine matches the blueprint. Nothing to do.");
        return;
    }

    if !actionable.is_empty() {
        // Grouped by privilege domain, because that is the thing the user has
        // to act on: `rime apply` closes the user half, `sudo rime apply` the
        // root half, and nothing escalates on its own.
        let mut by_domain: BTreeMap<&str, Vec<&Change>> = BTreeMap::new();
        for c in &actionable {
            by_domain
                .entry(c.domain().map_or("?", Domain::as_str))
                .or_default()
                .push(c);
        }
        let width = actionable
            .iter()
            .map(|c| c.what.len())
            .max()
            .unwrap_or(0)
            .max(8);
        for (domain, changes) in by_domain {
            println!();
            println!(
                "{} ({} {verb}; run {})",
                if domain == "root" { "SYSTEM" } else { "THIS USER" },
                changes.len(),
                if domain == "root" {
                    "`sudo rime apply`"
                } else {
                    "`rime apply`"
                }
            );
            for c in changes {
                println!(
                    "  {:<width$}  {} -> {}",
                    c.what,
                    c.current,
                    c.desired,
                    width = width
                );
                if let Some(step) = &c.step {
                    println!("  {:<width$}    {step}", "", width = width);
                }
            }
        }
    }

    if !blocked.is_empty() {
        println!();
        println!("CANNOT CONVERGE ({})", blocked.len());
        for c in blocked {
            println!("  {}  {} -> {}", c.what, c.current, c.desired);
            println!(
                "      {}",
                c.blocked.as_deref().unwrap_or("no reason recorded")
            );
        }
    }
}

/// The machine-readable form of a plan, shared by `diff --json` and
/// `apply --dry-run --json` so the two cannot describe the same plan
/// differently.
pub fn plan_json(bp: &Blueprint, source: &Source, plan: &Plan) -> serde_json::Value {
    serde_json::json!({
        "schema": SCHEMA_VERSION,
        "source": source.path().map(|p| p.display().to_string()),
        "digest": bp.digest(),
        "converged": plan.is_converged(),
        "changes": plan.changes.iter().map(|c| serde_json::json!({
            "what": c.what,
            "current": c.current,
            "desired": c.desired,
            "step": c.step.as_ref().map(ToString::to_string),
            "domain": c.domain().map(Domain::as_str),
            "blocked": c.blocked,
        })).collect::<Vec<_>>(),
    })
}

/// Write `text` to `path` through a temporary file and a rename.
///
/// Atomic replacement, per the repo contract for persistent configuration: a
/// blueprint truncated by a crash mid-write would be a file the user has to
/// reconstruct by hand.
pub fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("writing {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        // Leave no debris if the rename fails.
        let _ = std::fs::remove_file(&tmp);
        format!("installing {}: {e}", path.display())
    })
}

// ── converging ───────────────────────────────────────────────────────────────

/// Setting this to any non-empty value makes `rime apply` refuse to change the
/// machine.
///
/// The sibling guard is `rime-display-apply`'s `RIME_DISPLAY_NO_LIVE`, which
/// exists because a test with an isolated `HOME` still reconfigured the
/// developer's live desktop — `hyprctl` does not care what `HOME` is. This one
/// exists for the same class of accident one command along: `rime apply` runs
/// the package engine and rewrites session state, and neither of those is
/// redirected by any amount of environment isolation either.
///
/// **It differs from the display guard in one way, deliberately.** The display
/// guard refuses `apply` outright, dry run or not. This one blocks only the
/// live path, so `--dry-run` keeps working with the variable set — which is
/// what lets CI export it for a whole job as a blanket net *and* still exercise
/// every planning assertion. A dry run performs no writes at all (it never
/// constructs a converger), so there is nothing for the guard to protect there.
/// Read as a weakened copy it looks wrong; it is the stronger arrangement.
///
/// Emptiness is the off switch, matching the display guard's Python
/// truthiness check: `RIME_BLUEPRINT_NO_APPLY=` is unset, anything else — `0`
/// included — is set.
pub const NO_APPLY_ENV: &str = "RIME_BLUEPRINT_NO_APPLY";

/// The refusal message when the guard is set, or `None` when it is not.
fn guard_reason() -> Option<String> {
    let value = std::env::var(NO_APPLY_ENV).ok()?;
    if value.is_empty() {
        return None;
    }
    Some(format!(
        "{NO_APPLY_ENV}={value} is set; refusing to change the machine. \
         Use `rime apply --dry-run` or `rime blueprint diff` to see what would happen."
    ))
}

/// Turns a [`Step`] into a real change.
///
/// Modelled on [`rimed_core::syswriter::RealWriter`], and for the same reason:
/// there is exactly one constructor that is allowed to touch the machine, so
/// opting in is one visible call in one place rather than a default every
/// caller silently inherits. `RealWriter` learned that the hard way — a test
/// applying `ScxSwitch` through a live writer raised a burst of polkit prompts
/// on the developer's desktop and would have swapped the scheduler of the
/// machine running the tests.
///
/// Nothing here ever runs `sudo`. A step is performed only when the process is
/// already in the right privilege domain (see [`Domain`]); anything else is
/// reported for the other domain to do. That is not a convenience — it is the
/// reason `rime apply` cannot raise an authentication prompt at all.
pub struct RealConverger {
    /// Whether this converger may change anything. False for the inert one.
    effects: bool,
}

impl RealConverger {
    /// The one constructor that permits a change to the machine.
    ///
    /// Returns `Err` when [`NO_APPLY_ENV`] is set, so under the guard a live
    /// converger cannot be built at all — the refusal is not a branch inside
    /// the loop that someone can later reorder past.
    pub fn for_apply() -> Result<RealConverger, String> {
        match guard_reason() {
            Some(why) => Err(why),
            None => Ok(RealConverger { effects: true }),
        }
    }

    /// A converger with no effects: every step is logged and skipped.
    ///
    /// The constructor for anything that is not `rime apply`, tests above all.
    ///
    /// `#[cfg(test)]`, like [`Host::with_root`] and for the same reason: `rime`
    /// is a binary crate, so an unreferenced constructor is dead code under
    /// CI's `-D warnings`. Gating it also states the intent — production has
    /// exactly one converger and it is the live one.
    #[cfg(test)]
    pub fn inert() -> RealConverger {
        RealConverger { effects: false }
    }

    /// Whether this converger may change the machine. The accessor the tests
    /// assert the invariant through; see [`RealConverger::inert`] for why it is
    /// gated.
    #[cfg(test)]
    pub fn has_effects(&self) -> bool {
        self.effects
    }

    /// Perform one step.
    pub fn perform(&self, step: &Step) -> Result<(), String> {
        // Checked BEFORE any path is built or any process is spawned, exactly
        // as `RealWriter::run_scxctl` checks `host_commands` first.
        if !self.effects {
            eprintln!("rime apply: skip (this converger has no effects) {step}");
            return Ok(());
        }
        // Belt and braces. The guard is re-read here as well as in the
        // constructor, so a converger built before the variable was set — or
        // a future caller that finds another way to construct one — still
        // cannot reach the machine.
        if let Some(why) = guard_reason() {
            return Err(why);
        }
        // The privilege domain, checked HERE and not only where the plan is
        // filtered. `cmd_apply` already hands over only the steps for this
        // process's domain, so this can never fire in normal operation — which
        // is exactly why it is worth having.
        //
        // The root-domain steps reach `/usr/libexec/rime-session-select` and
        // `/usr/libexec/rime-pkg` by ABSOLUTE path, so no amount of `PATH`
        // faking in a test can intercept them the way it intercepts `flatpak`.
        // A test's outermost safety net therefore cannot cover them, and the
        // filtering in `cmd_apply` is the only thing that would be left. This
        // makes the guarantee structural instead: a non-root process cannot
        // perform a root step even if the caller asks it to.
        let running_as = match crate::ops::effective_uid() {
            Some(0) => Domain::Root,
            _ => Domain::User,
        };
        if step.domain() != running_as {
            return Err(format!(
                "refusing to perform a {} step while running as {}: {step}",
                step.domain().as_str(),
                running_as.as_str()
            ));
        }
        match step {
            Step::SelectSession { session } => self.select_session(session),
            Step::SetTheme { scheme } => self.set_theme(scheme),
            // Packages and Flatpaks both go to the shipped engine, which does
            // its own classification. The planner splits them for reporting;
            // it must not become a second router, or a name could be reported
            // under one source and installed from the other.
            Step::InstallPackages { names } => self.install(names),
            Step::InstallFlatpaks { ids } => self.install(ids),
            Step::SetAgentDefault { agent } => self.set_agent(Some(agent), None),
            Step::SetAgentSandbox { policy } => self.set_agent(None, Some(policy)),
            Step::ProvisionLanguage { language } => self.provision(language),
        }
    }

    /// Make a capsule that provides a language.
    ///
    /// The whole decision — which capsule, which packages, which program proves
    /// the toolchain landed — belongs to the shipped engine, and this hands it
    /// the language and nothing else. A language -> package table on this side
    /// would be the second, conflicting answer phase 7 deferred the section to
    /// avoid.
    ///
    /// `rime-env` refuses to run as root, and this step is user-domain, so the
    /// domain check in [`RealConverger::perform`] has already established that
    /// the process is not root before this is reached. No `sudo`, no `pkexec`,
    /// nothing that can prompt.
    fn provision(&self, language: &str) -> Result<(), String> {
        run(&env_engine(), &["provision", language])
    }

    /// Point the greeter at a session, without ending anyone's session.
    ///
    /// `rime-session-select` is called directly rather than through `sudo`:
    /// this step is root-domain, so the process is already root. The helper
    /// validates the id against the installed `.desktop` files itself, which
    /// is what makes it safe to hand a name to.
    fn select_session(&self, session: &str) -> Result<(), String> {
        run(rime_paths::RIME_SESSION_SELECT, &[session])
    }

    /// Hand the whole list to the package engine.
    fn install(&self, names: &[String]) -> Result<(), String> {
        let mut args: Vec<&str> = vec!["install"];
        args.extend(names.iter().map(String::as_str));
        run(crate::ops::PKG_ENGINE, &args)
    }

    /// Set Rime Shell's matugen scheme.
    ///
    /// Read, change one key, write atomically. Not "write a file containing the
    /// scheme": `wallpaper.json` also holds the wallpaper directory and the
    /// current wallpaper, and clobbering those to set a colour scheme would
    /// lose the user's background.
    ///
    /// The running shell reads this file on startup, so a scheme set here takes
    /// effect at the next login or the next time the wallpaper is applied. Said
    /// plainly rather than pretended otherwise — there is no IPC to push it.
    fn set_theme(&self, scheme: &str) -> Result<(), String> {
        let path = rime_agent_core::paths::home().join(WALLPAPER_JSON);
        let mut value: serde_json::Value = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| format!("{} is not valid JSON ({e}); refusing to overwrite it", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
            Err(e) => return Err(format!("reading {}: {e}", path.display())),
        };
        let Some(obj) = value.as_object_mut() else {
            return Err(format!(
                "{} is not a JSON object; refusing to overwrite it",
                path.display()
            ));
        };
        obj.insert("scheme".into(), serde_json::Value::String(scheme.to_string()));

        let parent = path.parent().expect("the wallpaper path always has a parent");
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
        let text = serde_json::to_string_pretty(&value)
            .map_err(|e| format!("rendering {}: {e}", path.display()))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, format!("{text}\n"))
            .map_err(|e| format!("writing {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("installing {}: {e}", path.display())
        })?;
        eprintln!(
            "rime apply: colour scheme set to {scheme}; it applies at the next login \
             or the next wallpaper change"
        );
        Ok(())
    }

    /// Set the agent runtime's preferences.
    ///
    /// Through `rime_agent_core::config::Config`, never by hand-editing the
    /// JSON: that type carries `#[serde(flatten)] extra` precisely so a key
    /// written by a newer Rime Shell survives a round-trip, and a
    /// hand-rolled writer here would drop it.
    fn set_agent(&self, default: Option<&str>, sandbox: Option<&str>) -> Result<(), String> {
        use rime_agent_core::config::Config;
        use rime_agent_core::protocol::SandboxPolicy;

        let mut cfg = Config::load();
        if let Some(agent) = default {
            cfg.default_agent = agent.to_string();
        }
        if let Some(policy) = sandbox {
            cfg.sandbox = SandboxPolicy::parse(policy)
                .ok_or_else(|| format!("the agent runtime does not know sandbox policy {policy:?}"))?;
        }
        // normalise() silently turns an unknown agent id into the default. The
        // blueprint has already been validated against the same vocabulary, so
        // anything corrected here is a real disagreement between the two — the
        // exact drift the parity test guards against — and must be reported
        // rather than written out as a value the user did not ask for.
        let notes = cfg.normalise();
        if !notes.is_empty() {
            return Err(format!(
                "the agent runtime rejected the blueprint value: {}",
                notes.join("; ")
            ));
        }
        cfg.save().map_err(|e| format!("saving the agent configuration: {e}"))
    }
}

/// Overrides which capsule engine `Step::ProvisionLanguage` drives.
///
/// **Why this one is overridable when `rime-pkg`'s `ENV_ENGINE` deliberately is
/// not.** `Containerfile.base` asserts that the package engine holds
/// `readonly ENV_ENGINE=/usr/libexec/rime-env`, with the reasoning that
/// `rime install` runs as root and a caller-controlled variable naming a
/// program a root process executes is a hole. That reasoning is exactly right
/// there and does not apply here, for a structural reason rather than a
/// judgement call: `Step::ProvisionLanguage` is [`Domain::User`], and
/// [`RealConverger::perform`] refuses a step from the other domain *before* it
/// builds any path or spawns anything. A root `rime apply` never reaches this
/// code, so the variable can only ever name a program the invoking user could
/// already have run themselves.
///
/// What it buys is the one thing the suite otherwise could not have: a LIVE
/// `rime apply` that exercises the real convergence path. The engine is reached
/// by absolute path, so no amount of `PATH` faking intercepts it — the same
/// hole the root-domain steps have, except there is no domain filtering to fall
/// back on for a user-domain step. This is the sibling of `RIME_WINDOW_ADAPTER`,
/// which exists so `rime project restore` can be tested with no compositor.
const ENV_ENGINE_ENV: &str = "RIME_ENV_ENGINE";

fn env_engine() -> String {
    match std::env::var(ENV_ENGINE_ENV) {
        Ok(p) if !p.is_empty() => p,
        _ => crate::ops::ENV_ENGINE.to_string(),
    }
}

/// Run a program, turning a non-zero exit into a message.
fn run(program: &str, args: &[&str]) -> Result<(), String> {
    if !Path::new(program).exists() {
        return Err(format!("{program} is not installed on this machine"));
    }
    let out = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let last = stderr
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("no output");
    Err(format!("{program} failed ({}): {last}", out.status))
}

/// `rime apply`.
///
/// The plan is computed **once**. `--dry-run` prints exactly the steps a live
/// run would perform, and a live run performs exactly the steps a dry run
/// printed; the only difference between the two paths is whether those steps
/// reach a [`RealConverger`]. That is what makes the dry run a report rather
/// than a rehearsal of a different program.
pub fn cmd_apply(file: Option<&Path>, dry_run: bool, json: bool) -> i32 {
    let (bp, source) = match load(file) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("rime apply: {e}");
            return EXIT_ERROR;
        }
    };
    let observed = Host::new().observe();
    let plan = blueprint::plan(&bp, &observed);

    // Which half of the plan this process is entitled to. Never assume root
    // when the uid cannot be read: guessing high is the direction that hurts.
    let ours = match crate::ops::effective_uid() {
        Some(0) => Domain::Root,
        _ => Domain::User,
    };
    let theirs = match ours {
        Domain::Root => Domain::User,
        Domain::User => Domain::Root,
    };
    let mine: Vec<Step> = plan.steps_for(ours).into_iter().cloned().collect();
    let others = plan.steps_for(theirs).len();

    if json {
        println!("{}", plan_json(&bp, &source, &plan));
    } else {
        print_plan(&plan, &source, if dry_run { "would change" } else { "to change" });
    }

    if !json {
        println!();
        if others > 0 {
            match ours {
                Domain::User => println!(
                    "{others} change{} need root. Run `sudo rime apply` for those.",
                    plural(others)
                ),
                // Under sudo, HOME is root's, so the user-domain rows above were
                // measured against the WRONG home. Saying so beats printing a
                // confident diff of the wrong user's settings.
                Domain::Root => println!(
                    "{others} change{} belong to a login session, not to root, and were \
                     measured against root's own home. Run `rime apply` as yourself for \
                     those — not with sudo.",
                    plural(others)
                ),
            }
        }
    }

    if dry_run {
        if !json {
            println!(
                "Dry run: nothing was changed, and no state file was written. \
                 {} step{} would run as {}.",
                mine.len(),
                plural(mine.len()),
                ours.as_str()
            );
        }
        return 0;
    }

    if mine.is_empty() {
        if !json {
            println!("Nothing to do as {}.", ours.as_str());
        }
        return 0;
    }

    let converger = match RealConverger::for_apply() {
        Ok(c) => c,
        Err(why) => {
            eprintln!("rime apply: {why}");
            return EXIT_ERROR;
        }
    };

    let mut done = Vec::new();
    let mut failed = Vec::new();
    for step in &mine {
        eprintln!("rime apply: {step}");
        match converger.perform(step) {
            Ok(()) => done.push(step.to_string()),
            Err(why) => {
                eprintln!("rime apply: FAILED {step}: {why}");
                failed.push(format!("{step}: {why}"));
            }
        }
    }

    // Re-measure. `rimed/AGENTS.md`: a command that reports success must verify
    // the requested state. A step can exit 0 and change nothing — an engine
    // that decided a package was already provided by the image, a helper that
    // wrote to a path no longer read — and reporting success on the strength of
    // an exit code alone is how a converger comes to believe in a machine that
    // does not exist.
    let after = blueprint::plan(&bp, &Host::new().observe());
    let residual: Vec<&Step> = after.steps_for(ours);

    if let Err(e) = record(&bp, ours, &done, &failed) {
        // Losing the record is not losing the convergence.
        eprintln!("rime apply: note: could not write the applied-state record: {e}");
    }

    println!();
    println!(
        "Applied {} of {} step{} as {}.",
        done.len(),
        mine.len(),
        plural(mine.len()),
        ours.as_str()
    );
    if !residual.is_empty() {
        println!(
            "Still not converged after applying ({}); re-measured, not assumed:",
            residual.len()
        );
        for step in &residual {
            println!("  {step}");
        }
    }
    if failed.is_empty() && residual.is_empty() {
        0
    } else {
        EXIT_DRIFT
    }
}

/// Write the generated record of this run.
fn record(bp: &Blueprint, domain: Domain, done: &[String], failed: &[String]) -> Result<(), String> {
    let path = applied_state_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    let state = AppliedState {
        schema: SCHEMA_VERSION,
        applied_at: now_secs(),
        domain: domain.as_str().to_string(),
        blueprint_digest: bp.digest(),
        steps: done.to_vec(),
        failures: failed.to_vec(),
    };
    let text = state.to_toml().map_err(|e| e.to_string())?;
    write_atomic(&path, &text)
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

// ── sync ─────────────────────────────────────────────────────────────────────

/// `rime sync export`.
///
/// Writes a bundle: the blueprint, plus which projects exist, plus enough
/// provenance to know where it came from. What it deliberately leaves out is
/// documented on [`rimed_core::blueprint::Bundle`] — no credentials of any
/// kind, because this is a file people put in a git repository.
pub fn cmd_sync_export(file: Option<&Path>, output: Option<&Path>, no_projects: bool) -> i32 {
    let (bp, _source) = match load(file) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("rime sync: {e}");
            return EXIT_ERROR;
        }
    };

    let projects = if no_projects { Vec::new() } else { collect_projects() };
    let bundle = blueprint::Bundle {
        bundle: blueprint::BundleMeta {
            schema: SCHEMA_VERSION,
            created: now_secs(),
            source_host: read_trimmed(Path::new("/etc/hostname")),
            source_variant: Host::new().observe().variant_id,
        },
        blueprint: bp,
        projects,
    };

    // Round-trip our own output before writing it. A bundle that cannot be
    // re-read is worse than a refusal: the failure would surface on the OTHER
    // machine, hours later, with no way to tell which end was wrong.
    let text = match bundle.to_toml() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("rime sync: cannot render the bundle: {e}");
            return EXIT_ERROR;
        }
    };
    if let Err(e) = blueprint::Bundle::parse(&text) {
        eprintln!("rime sync: refusing to write a bundle this build cannot read back: {e}");
        return EXIT_ERROR;
    }

    match output {
        None => print!("{text}"),
        Some(path) => {
            if let Err(e) = write_atomic(path, &text) {
                eprintln!("rime sync: {e}");
                return EXIT_ERROR;
            }
            eprintln!(
                "rime sync: wrote {} ({} project{})",
                path.display(),
                bundle.projects.len(),
                plural(bundle.projects.len())
            );
        }
    }
    0
}

/// Projects as `rime project` knows them, with their git remote.
///
/// A project whose path fails the bundle's own validation is dropped with a
/// note rather than written out — `export` must not produce a file its own
/// `import` would refuse.
fn collect_projects() -> Vec<blueprint::ProjectRef> {
    let mut out = Vec::new();
    for p in rime_agent_core::project::list() {
        let path = PathBuf::from(&p.root);
        let candidate = blueprint::ProjectRef {
            slug: p.slug.clone(),
            path: p.root.clone(),
            remote: rime_agent_core::git::git_opt(&path, &["remote", "get-url", "origin"]),
        };
        // Validate through the same door `import` uses, by building a
        // one-project bundle and parsing it. One implementation of the rule,
        // not two.
        let probe = blueprint::Bundle {
            bundle: blueprint::BundleMeta {
                schema: SCHEMA_VERSION,
                created: 0,
                source_host: None,
                source_variant: None,
            },
            blueprint: Blueprint::default(),
            projects: vec![candidate.clone()],
        };
        match probe.to_toml().map_err(|e| e.to_string()).and_then(|t| {
            blueprint::Bundle::parse(&t).map_err(|e| e.to_string())
        }) {
            Ok(_) => out.push(candidate),
            Err(why) => eprintln!(
                "rime sync: skipping project {:?}: {why}",
                candidate.slug
            ),
        }
    }
    out
}

/// `rime sync show` — read a bundle without importing it.
pub fn cmd_sync_show(path: &Path) -> i32 {
    let bundle = match read_bundle(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("rime sync: {e}");
            return EXIT_ERROR;
        }
    };
    println!("bundle     {}", path.display());
    println!("schema     {}", bundle.bundle.schema);
    println!("created    {} ago", ago(bundle.bundle.created));
    println!(
        "from       {} ({})",
        bundle.bundle.source_host.as_deref().unwrap_or("an unnamed host"),
        bundle.bundle.source_variant.as_deref().unwrap_or("unknown edition")
    );
    println!("digest     {}", bundle.blueprint.digest());
    println!();
    match bundle.blueprint.to_toml() {
        Ok(t) => print!("{t}"),
        Err(e) => {
            eprintln!("rime sync: {e}");
            return EXIT_ERROR;
        }
    }
    if !bundle.projects.is_empty() {
        println!();
        println!("projects ({})", bundle.projects.len());
        for p in &bundle.projects {
            println!(
                "  {}  {}{}",
                p.slug,
                p.path,
                p.remote
                    .as_deref()
                    .map(|r| format!("  <- {r}"))
                    .unwrap_or_default()
            );
        }
    }
    0
}

/// `rime sync import`.
///
/// Writes the blueprint and records the projects, and **never converges
/// anything**. Keeping `apply` out of this verb is deliberate: importing a file
/// from another machine and changing this one are two decisions, and a user who
/// has just pulled in someone else's blueprint should get to read
/// `rime blueprint diff` before anything happens.
pub fn cmd_sync_import(path: &Path, force: bool) -> i32 {
    let bundle = match read_bundle(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("rime sync: {e}");
            return EXIT_ERROR;
        }
    };

    let target = user_blueprint_path();
    // An existing blueprint is the user's own work. Overwriting it because a
    // file arrived from elsewhere is the one unrecoverable thing this command
    // could do, so it takes an explicit --force and still keeps a copy.
    if target.exists() {
        let existing = std::fs::read_to_string(&target).ok().and_then(|t| Blueprint::parse(&t).ok());
        if existing.as_ref() == Some(&bundle.blueprint) {
            println!("{} already matches the bundle.", target.display());
        } else if !force {
            eprintln!(
                "rime sync: {} exists and differs from the bundle.\n\
                 Compare them with `rime sync show {}` and `rime blueprint show`,\n\
                 then re-run with --force. The current file is kept as {}.previous.",
                target.display(),
                path.display(),
                target.display()
            );
            return EXIT_ERROR;
        } else {
            let backup = target.with_extension("toml.previous");
            if let Err(e) = std::fs::copy(&target, &backup) {
                eprintln!("rime sync: cannot keep a copy of the current blueprint: {e}");
                return EXIT_ERROR;
            }
            eprintln!("rime sync: kept the previous blueprint as {}", backup.display());
        }
    }

    let text = match bundle.blueprint.to_toml() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("rime sync: {e}");
            return EXIT_ERROR;
        }
    };
    if let Some(parent) = target.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("rime sync: cannot create {}: {e}", parent.display());
            return EXIT_ERROR;
        }
    }
    if let Err(e) = write_atomic(&target, &text) {
        eprintln!("rime sync: {e}");
        return EXIT_ERROR;
    }
    println!("blueprint  {}", target.display());

    // Projects are RECORDED, never created. `import` does not clone a
    // repository, make a directory or write anything inside one — a bundle is
    // a description, and acting on a path that arrived in a file from another
    // machine is not something this command should ever do.
    let mut recorded = 0;
    let mut absent = Vec::new();
    for p in &bundle.projects {
        let dir = PathBuf::from(&p.path);
        if !dir.is_dir() {
            absent.push(p);
            continue;
        }
        match rime_agent_core::project::detect(&dir) {
            Some(project) => match rime_agent_core::project::remember(&project) {
                Ok(()) => recorded += 1,
                Err(e) => eprintln!("rime sync: cannot record {:?}: {e}", p.slug),
            },
            None => {
                // `detect` returns None outside a git repository, and refuses to
                // invent a project from a bare directory. Report rather than
                // work around it.
                eprintln!(
                    "rime sync: {} is not a git repository; not recording it as a project",
                    p.path
                );
            }
        }
    }
    if recorded > 0 {
        println!("projects   {recorded} recorded");
    }
    if !absent.is_empty() {
        println!();
        println!(
            "{} project{} in the bundle {} not on this machine. Clone {} and they will be \
             picked up the first time you use them:",
            absent.len(),
            plural(absent.len()),
            if absent.len() == 1 { "is" } else { "are" },
            if absent.len() == 1 { "it" } else { "them" }
        );
        for p in absent {
            println!(
                "  {}  {}{}",
                p.slug,
                p.path,
                p.remote
                    .as_deref()
                    .map(|r| format!("  <- {r}"))
                    .unwrap_or_default()
            );
        }
    }

    println!();
    println!("Nothing has been converged. `rime blueprint diff` shows what would change.");
    0
}

fn read_bundle(path: &Path) -> Result<blueprint::Bundle, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    blueprint::Bundle::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Seconds since the epoch, or 0 if the clock is before 1970.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// "3 minutes", "2 days" — a coarse age, because the exact second of the last
/// apply has never been the interesting part and formatting a real timestamp
/// would need a date library.
fn ago(then: u64) -> String {
    let now = now_secs();
    let secs = now.saturating_sub(then);
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialises the three cases that mutate `NO_APPLY_ENV`.
    ///
    /// The environment is process-wide and `cargo test` runs this module's
    /// cases in threads of one process, so a `set_var` in one case is a
    /// `set_var` in all of them. Three cases here touch that one name —
    /// `a_live_converger_cannot_be_built_while_the_guard_is_set` sets it,
    /// `the_live_constructor_still_grants_effects` and
    /// `a_live_converger_refuses_a_step_from_the_other_privilege_domain` remove
    /// it — and unserialised they turn each other's guard on and off. That was
    /// a live flake, not a hazard in principle: 2 failures in 60 runs of the
    /// whole binary on 2026-09-12, 0 in 60 with `--test-threads=1`.
    ///
    /// `into_inner` on a poisoned lock, matching `gaming.rs`: a case that
    /// panicked while holding it has already failed and reported itself, and
    /// poisoning every later case would bury that one real failure under a
    /// dozen misleading ones.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A fixture machine laid out under one temporary directory.
    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Fixture {
            let dir = std::env::temp_dir().join(format!("rime-bp-test-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("fixture root");
            Fixture { dir }
        }

        fn write(&self, rel: &str, text: &str) -> &Fixture {
            let path = self.dir.join(rel);
            std::fs::create_dir_all(path.parent().expect("relative paths have parents"))
                .expect("fixture dirs");
            std::fs::write(&path, text).expect("fixture file");
            self
        }

        fn host(&self) -> Host {
            Host::with_root(&self.dir)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn sessions_are_read_from_the_directory_the_greeter_enumerates() {
        let f = Fixture::new("sessions");
        f.write("usr/share/wayland-sessions/hyprland.desktop", "")
            .write("usr/share/wayland-sessions/niri.desktop", "")
            .write("usr/share/wayland-sessions/rime-labwc.desktop", "")
            // Not a .desktop: must be ignored, not offered as a session.
            .write("usr/share/wayland-sessions/README", "");
        let obs = f.host().observe();
        assert_eq!(obs.sessions_available, ["hyprland", "niri", "rime-labwc"]);
        assert!(!obs.has_gaming_session());
    }

    #[test]
    fn the_gaming_session_is_what_marks_a_gaming_edition() {
        let f = Fixture::new("gaming");
        f.write("usr/share/wayland-sessions/rime-gaming.desktop", "")
            .write("etc/os-release", "ID=rimeos\nVARIANT_ID=\"gaming-nvidia\"\n");
        let obs = f.host().observe();
        assert!(obs.has_gaming_session());
        assert_eq!(obs.variant_id.as_deref(), Some("gaming-nvidia"));
    }

    // The single image ships rime-gaming.desktop on every machine, so without
    // the TryExec gate every machine would report the gaming session as
    // available and `[gaming] enabled` would be silently satisfied everywhere.
    #[test]
    fn a_session_whose_tryexec_binary_is_absent_is_not_offered() {
        let f = Fixture::new("tryexec-missing");
        f.write("usr/share/wayland-sessions/hyprland.desktop", "Exec=Hyprland\n")
            .write(
                "usr/share/wayland-sessions/rime-gaming.desktop",
                "Exec=/usr/libexec/rime-gaming-session\nTryExec=/usr/bin/gamescope\n",
            );
        let obs = f.host().observe();
        assert_eq!(obs.sessions_available, ["hyprland"]);
        assert!(!obs.has_gaming_session(), "gamescope is not installed");
    }

    #[test]
    fn the_same_session_is_offered_once_its_binary_is_installed() {
        let f = Fixture::new("tryexec-present");
        f.write("usr/share/wayland-sessions/hyprland.desktop", "Exec=Hyprland\n")
            .write(
                "usr/share/wayland-sessions/rime-gaming.desktop",
                "Exec=/usr/libexec/rime-gaming-session\nTryExec=/usr/bin/gamescope\n",
            )
            .write("usr/bin/gamescope", "");
        let obs = f.host().observe();
        assert_eq!(obs.sessions_available, ["hyprland", "rime-gaming"]);
        assert!(obs.has_gaming_session());
    }

    #[test]
    fn the_preselected_session_is_read_from_the_greeters_own_state() {
        let f = Fixture::new("greeter");
        // Written by rime-session-select with no trailing newline; a stray one
        // must not become part of the id either.
        f.write("var/lib/rime-greet/last-session", "rime-labwc\n");
        assert_eq!(f.host().observe().session.as_deref(), Some("rime-labwc"));
    }

    #[test]
    fn requested_packages_come_from_the_requested_list_not_the_resolved_one() {
        // state.json records every dependency; diffing against that would
        // report convergence based on packages nobody asked for.
        let f = Fixture::new("requested");
        f.write(
            "var/lib/rime/pkg/requested",
            "firefox\nlocal:some-vendor-driver\n\nhtop\n",
        );
        let obs = f.host().observe();
        assert_eq!(obs.packages, ["firefox", "some-vendor-driver", "htop"]);
        assert!(obs.packages_unreadable.is_none());
    }

    #[test]
    fn a_requested_list_we_may_not_read_is_not_a_machine_with_no_packages() {
        // /var/lib/rime/pkg is 0700 root:root by a tmpfiles.d rule and `rime
        // blueprint diff` is something a desktop session runs as itself, so the
        // refusal is the ordinary case rather than the exotic one. Reading it as
        // an empty list made the planner offer to install what was already
        // installed.
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::new("requested-eacces");
        f.write("var/lib/rime/pkg/requested", "firefox\nhtop\n");
        let pkg = f.dir.join("var/lib/rime/pkg");
        let mut perms = std::fs::metadata(&pkg).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&pkg, perms).unwrap();
        // Root and CAP_DAC_OVERRIDE walk through 0000, so the seal is checked
        // rather than assumed — and the exact error is required, or an
        // unrelated failure would count as a seal and the assertion would be
        // about nothing.
        let sealed = match std::fs::read_to_string(pkg.join("requested")) {
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => true,
            Ok(_) => false,
            Err(e) => panic!("expected PermissionDenied while sealing, got {e:?}"),
        };
        let obs = f.host().observe();
        let mut perms = std::fs::metadata(&pkg).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&pkg, perms).ok();
        if !sealed {
            return;
        }
        assert!(
            obs.packages_unreadable.is_some(),
            "a refused read must not arrive as a measurement of an empty machine"
        );
        assert!(obs.packages.is_empty());
    }

    #[test]
    fn an_absent_requested_list_is_a_measured_empty_list() {
        // The other half. Most machines install nothing with `rime install`,
        // and that must stay a fact rather than becoming a permanent warning.
        let f = Fixture::new("requested-absent");
        let obs = f.host().observe();
        assert!(obs.packages.is_empty());
        assert!(obs.packages_unreadable.is_none());
    }

    #[test]
    fn an_absent_wallpaper_json_measures_the_shells_own_default() {
        // Not "unknown": WallpaperService initialises scheme to "content", so
        // no file has a definite effective value. Reporting unknown would make
        // a fresh machine show permanent drift on `[desktop] theme`.
        let f = Fixture::new("theme-absent");
        assert_eq!(f.host().observe().theme.as_deref(), Some("content"));
    }

    #[test]
    fn a_written_scheme_is_read_back_and_a_broken_file_is_unknown() {
        let f = Fixture::new("theme-set");
        f.write(
            "home/.config/rime-shell/src/user_data/wallpaper.json",
            r#"{"scheme": "monochrome", "wallpaperDir": "~/Pictures"}"#,
        );
        assert_eq!(f.host().observe().theme.as_deref(), Some("monochrome"));

        f.write(
            "home/.config/rime-shell/src/user_data/wallpaper.json",
            "{not json",
        );
        assert_eq!(
            f.host().observe().theme,
            None,
            "a malformed file is unknown, because the shell's own parse may have \
             stopped anywhere"
        );

        // Present, valid, and no scheme key: the shell's default applies.
        f.write(
            "home/.config/rime-shell/src/user_data/wallpaper.json",
            r#"{"wallpaperDir": "~/Pictures"}"#,
        );
        assert_eq!(f.host().observe().theme.as_deref(), Some("content"));
    }

    #[test]
    fn a_fixture_host_never_runs_a_program() {
        // The same guarantee RealWriter's `host_commands` gives, and for the
        // same reason: `root` redirects a file read, and nothing redirects a
        // process spawn. A fixture host that shelled out to `flatpak` would
        // make these tests depend on what the developer has installed.
        let f = Fixture::new("no-programs");
        assert!(!f.host().probe_programs);
        let obs = f.host().observe();
        assert!(obs.flatpaks.is_empty());
        assert!(obs.languages.is_empty());
        assert!(obs.agent_default.is_none());
        // The capsule side is a FILE read, so it is measured under a fixture
        // root rather than suppressed — which is the point of reading the
        // records instead of running `rime env list --json`.
        assert!(obs.capsule_languages.is_empty(), "no records, no languages");
    }

    #[test]
    fn a_capsule_records_which_languages_it_provides() {
        let f = Fixture::new("capsule-langs");
        f.write(
            "home/.local/share/rime/env/rust.json",
            r#"{"name":"rust","languages":["rust"],"exports":[]}"#,
        )
        .write(
            "home/.local/share/rime/env/node.json",
            r#"{"name":"node","languages":["javascript","typescript"]}"#,
        )
        // A capsule that provides nothing must not contribute a phantom entry.
        .write(
            "home/.local/share/rime/env/plain.json",
            r#"{"name":"plain","languages":[]}"#,
        );
        let obs = f.host().observe();
        assert_eq!(obs.capsule_for("rust"), Some("rust"));
        assert_eq!(obs.capsule_for("typescript"), Some("node"));
        assert_eq!(obs.capsule_for("go"), None);
        assert_eq!(obs.capsule_languages.len(), 3);
    }

    #[test]
    fn a_broken_capsule_record_does_not_break_the_whole_observation() {
        // One hand-edited record must not take `rime blueprint diff` down for
        // every other section. Nothing is invented for it either — an
        // unparseable record contributes no language, so a blueprint that
        // names one still reports drift rather than silent convergence.
        let f = Fixture::new("capsule-broken");
        f.write("home/.local/share/rime/env/broken.json", "{not json")
            .write("home/.local/share/rime/env/nameless.json", r#"{"languages":["go"]}"#)
            .write(
                "home/.local/share/rime/env/rust.json",
                r#"{"name":"rust","languages":["rust"]}"#,
            )
            // Not a .json: a stray editor backup must be skipped, not parsed.
            .write("home/.local/share/rime/env/rust.json.bak", "{not json");
        let obs = f.host().observe();
        assert_eq!(obs.capsule_for("rust"), Some("rust"));
        assert_eq!(obs.capsule_for("go"), None, "a record with no name grants nothing");
        assert_eq!(obs.capsule_languages.len(), 1);
    }

    #[test]
    fn the_capsule_record_directory_matches_the_engines_own_precedence() {
        // The writer and the observer have to agree on the directory or
        // `apply` provisions a capsule the next `diff` cannot see. The engine
        // resolves ${RIME_ENV_HOME:-${XDG_DATA_HOME:-$HOME/.local/share}/rime/env}.
        let f = Fixture::new("capsule-dir");
        assert_eq!(
            f.host().capsule_record_dir(),
            f.dir.join("home/.local/share/rime/env"),
            "under a fixture root the fixture's own home wins, because a fixture \
             root cannot redirect an environment variable"
        );
    }

    #[test]
    fn the_capsule_engine_defaults_to_the_shipped_path() {
        // The override exists for the suite. If it became the only way to
        // reach the engine — a default of "rime-env", say, resolved through
        // PATH — then `rime apply` on a real machine would provision through
        // whatever happened to be on the user's PATH, and the suite would
        // still be green because it always sets the variable.
        let restore = std::env::var(ENV_ENGINE_ENV).ok();
        std::env::remove_var(ENV_ENGINE_ENV);
        assert_eq!(env_engine(), crate::ops::ENV_ENGINE);
        assert!(env_engine().starts_with('/'), "an absolute path, not a PATH lookup");

        // Empty is not an override, matching the guard's own truthiness rule.
        std::env::set_var(ENV_ENGINE_ENV, "");
        assert_eq!(env_engine(), crate::ops::ENV_ENGINE);

        std::env::set_var(ENV_ENGINE_ENV, "/tmp/somewhere-else");
        assert_eq!(env_engine(), "/tmp/somewhere-else");

        match restore {
            Some(v) => std::env::set_var(ENV_ENGINE_ENV, v),
            None => std::env::remove_var(ENV_ENGINE_ENV),
        }
    }

    #[test]
    fn the_real_host_does_probe() {
        // Otherwise the guard above has quietly disabled observation in
        // production, which is the failure mode of fixing it the lazy way.
        assert!(Host::new().probe_programs);
        assert_eq!(Host::new().root, Path::new("/"));
    }

    #[test]
    fn the_starter_blueprint_parses_and_manages_nothing() {
        // Everything in it is commented out on purpose: `rime blueprint init`
        // must never hand someone a file that changes their machine.
        let bp = Blueprint::parse(STARTER).expect("the starter must be a valid blueprint");
        assert_eq!(bp, Blueprint::default());
    }

    #[test]
    fn an_explicit_file_that_does_not_exist_is_an_error_not_an_empty_blueprint() {
        // Falling back would run `apply` against something the user did not
        // name.
        let err = load(Some(Path::new("/nonexistent/rime-blueprint.toml"))).unwrap_err();
        assert!(err.contains("no such file"), "{err}");
    }

    #[test]
    fn a_bad_blueprint_names_its_file_in_the_error() {
        let f = Fixture::new("bad-file");
        f.write("bp.toml", "[desktop]\ncompositor = \"hyperland\"\n");
        let path = f.dir.join("bp.toml");
        let err = load(Some(&path)).unwrap_err();
        assert!(err.contains("bp.toml"), "{err}");
        assert!(err.contains("hyperland"), "{err}");
    }

    #[test]
    fn the_agent_vocabularies_match_the_agent_runtimes_own() {
        // `rimed-core` cannot depend on `rime-agent-core`, so the blueprint's
        // agent and sandbox lists are a transcription. This crate depends on
        // both, so it is the one place the two can be compared — the same
        // reason `files/scripts/check-input-parity` exists for the settings
        // page and its generator. Without it, adding an adapter to the runtime
        // would leave the blueprint refusing a name the runtime accepts, and
        // the only symptom would be a confusing validation error.
        use rime_agent_core::adapter;
        use rime_agent_core::protocol::SandboxPolicy;

        assert_eq!(
            rimed_core::blueprint::AGENTS.to_vec(),
            adapter::ids(),
            "blueprint::AGENTS has drifted from rime_agent_core::adapter::ADAPTERS"
        );
        assert!(
            rimed_core::blueprint::AGENTS.contains(&adapter::DEFAULT_AGENT),
            "the runtime's default agent must be a legal blueprint value"
        );

        for policy in rimed_core::blueprint::SANDBOX_POLICIES {
            assert!(
                SandboxPolicy::parse(policy).is_some(),
                "the runtime does not accept sandbox policy {policy:?}"
            );
        }
        // And the other direction: every policy the runtime has must be
        // expressible, or a blueprint could not describe a machine the user
        // has already configured.
        for policy in [
            SandboxPolicy::Unrestricted,
            SandboxPolicy::Project,
            SandboxPolicy::Strict,
        ] {
            assert!(
                rimed_core::blueprint::SANDBOX_POLICIES.contains(&policy.as_str()),
                "no blueprint spelling for {}",
                policy.as_str()
            );
        }
    }

    #[test]
    fn the_theme_vocabulary_matches_what_the_shell_offers() {
        // WallpaperService.qml's `schemes` list, transcribed. There is no way
        // to import QML, so this asserts the constant against the same list
        // spelled out here — the value is that the two are side by side in one
        // place with a comment saying where the other one lives:
        //     rime-shell/src/services/WallpaperService.qml
        //         readonly property var schemes: [
        //             "content", "tonal-spot", "fidelity", "fruit-salad",
        //             "neutral", "monochrome"
        //         ]
        assert_eq!(
            rimed_core::blueprint::THEMES.to_vec(),
            [
                "content",
                "tonal-spot",
                "fidelity",
                "fruit-salad",
                "neutral",
                "monochrome"
            ]
        );
        assert!(rimed_core::blueprint::THEMES.contains(&SHELL_DEFAULT_SCHEME));
    }

    /// Every step variant, so the dispatch in `perform` is exercised whole.
    fn all_steps() -> Vec<Step> {
        vec![
            Step::SelectSession {
                session: "rime-labwc".into(),
            },
            Step::SetTheme {
                scheme: "monochrome".into(),
            },
            Step::InstallPackages {
                names: vec!["firefox".into()],
            },
            Step::InstallFlatpaks {
                ids: vec!["org.gimp.GIMP".into()],
            },
            Step::SetAgentDefault {
                agent: "codex".into(),
            },
            Step::SetAgentSandbox {
                policy: "strict".into(),
            },
            Step::ProvisionLanguage {
                language: "rust".into(),
            },
        ]
    }

    #[test]
    fn every_step_variant_is_in_all_steps() {
        // `all_steps()` is what the inert-converger and domain assertions
        // iterate, so a variant missing from it is silently uncovered — the
        // list looks complete and the new step is never exercised. Compared
        // against `Step`'s own count via a match that will not compile if a
        // variant is added without being handled here.
        fn tag(s: &Step) -> u8 {
            match s {
                Step::SelectSession { .. } => 0,
                Step::SetTheme { .. } => 1,
                Step::InstallPackages { .. } => 2,
                Step::InstallFlatpaks { .. } => 3,
                Step::SetAgentDefault { .. } => 4,
                Step::SetAgentSandbox { .. } => 5,
                Step::ProvisionLanguage { .. } => 6,
            }
        }
        let mut tags: Vec<u8> = all_steps().iter().map(tag).collect();
        tags.sort_unstable();
        assert_eq!(
            tags,
            (0..=6).collect::<Vec<u8>>(),
            "all_steps() must carry exactly one of every Step variant"
        );
    }

    #[test]
    fn an_inert_converger_performs_every_step_and_changes_nothing() {
        // The mirror of syswriter's
        // `scx_and_nvidia_actions_are_accepted_and_skipped_rather_than_failing`:
        // this runs on the test host, so it is also the assertion that no step
        // can touch it. A step that returned Err here would abort a whole
        // apply on a machine that simply lacks one primitive.
        let c = RealConverger::inert();
        assert!(!c.has_effects());
        for step in all_steps() {
            assert!(
                c.perform(&step).is_ok(),
                "an inert converger must skip {step}, not fail"
            );
        }
        // And it really did nothing: the agent config is the one target that
        // resolves through this process's own environment, so if `perform` had
        // effects it would exist by now.
        assert!(
            !rime_agent_core::config::exists_at(&rime_agent_core::paths::config_file())
                || RealConverger::inert().has_effects(),
            "the inert converger wrote the agent configuration"
        );
    }

    #[test]
    fn a_live_converger_cannot_be_built_while_the_guard_is_set() {
        // The refusal is at CONSTRUCTION, not a branch inside the apply loop —
        // so there is no ordering a later change could get wrong.
        //
        // `cargo test` runs tests in threads of one process, so this mutates a
        // shared environment.
        //
        // The sentence that used to be here — "no other test in this module
        // reads the variable, which is why this is the only place it is
        // touched" — was false when it was written. Two more cases below,
        // `the_live_constructor_still_grants_effects` and
        // `a_live_converger_refuses_a_step_from_the_other_privilege_domain`,
        // both `remove_var` the same name, and either of them landing between
        // the `set_var` here and the `for_apply()` after it turns the guard off
        // underneath this test. Measured on 2026-09-12: 2 failures in 60 runs
        // of the whole binary, at both of the assertions below, and 0 in 60
        // with `--test-threads=1`.
        //
        // Serialised on the module's ENV_LOCK, the shape `gaming.rs` already
        // uses. The lock is what makes the restore below mean anything.
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let restore = std::env::var(NO_APPLY_ENV).ok();

        std::env::set_var(NO_APPLY_ENV, "1");
        let Err(err) = RealConverger::for_apply() else {
            panic!("the guard is set; a live converger must not be constructible");
        };
        assert!(err.contains(NO_APPLY_ENV), "{err}");
        assert!(err.contains("--dry-run"), "the refusal must say what to do instead: {err}");

        // Any non-empty value, matching rime-display-apply's truthiness check —
        // including "0", which is exactly the value someone would expect to
        // turn a guard OFF and which must not.
        std::env::set_var(NO_APPLY_ENV, "0");
        assert!(RealConverger::for_apply().is_err(), "\"0\" is still set");

        // Empty is the off switch.
        std::env::set_var(NO_APPLY_ENV, "");
        assert!(
            RealConverger::for_apply().is_ok(),
            "an empty value must not block a real apply, or setting it in a shell \
             profile would disable the feature permanently"
        );

        std::env::remove_var(NO_APPLY_ENV);
        assert!(RealConverger::for_apply().is_ok());

        match restore {
            Some(v) => std::env::set_var(NO_APPLY_ENV, v),
            None => std::env::remove_var(NO_APPLY_ENV),
        }
    }

    #[test]
    fn the_live_constructor_still_grants_effects() {
        // Otherwise the guard has quietly disabled `rime apply` in production,
        // which is the failure mode of fixing this the lazy way. Constructed
        // and dropped without performing anything.
        //
        // Same ENV_LOCK as the two other cases that touch NO_APPLY_ENV: this
        // `remove_var` is process-wide, and unserialised it is what turned the
        // guard off underneath
        // `a_live_converger_cannot_be_built_while_the_guard_is_set`.
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let restore = std::env::var(NO_APPLY_ENV).ok();
        std::env::remove_var(NO_APPLY_ENV);
        let c = RealConverger::for_apply()
            .unwrap_or_else(|e| panic!("no guard is set, so this must succeed: {e}"));
        assert!(c.has_effects());
        if let Some(v) = restore {
            std::env::set_var(NO_APPLY_ENV, v);
        }
    }

    #[test]
    fn a_live_converger_refuses_a_step_from_the_other_privilege_domain() {
        // The structural version of "apply never escalates". `cmd_apply`
        // already hands over only this process's domain, so this can never
        // fire in normal operation — which is the point. The root steps reach
        // /usr/libexec by ABSOLUTE path, so a test's PATH fakes cannot
        // intercept them and the plan filtering would otherwise be the only
        // thing standing between a wrong caller and the session.
        //
        // Whichever way round this test runs, it asserts a REFUSAL and
        // performs nothing: a non-root process is asked for a root step, and a
        // root one is asked for a user step.
        //
        // ENV_LOCK for the same reason as the two cases above: the `remove_var`
        // below is process-wide.
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let restore = std::env::var(NO_APPLY_ENV).ok();
        std::env::remove_var(NO_APPLY_ENV);

        let root_now = matches!(crate::ops::effective_uid(), Some(0));
        let wrong_domain_step = if root_now {
            Step::SetTheme {
                scheme: "neutral".into(),
            }
        } else {
            Step::SelectSession {
                session: "rime-labwc".into(),
            }
        };
        let c = RealConverger::for_apply()
            .unwrap_or_else(|e| panic!("no guard is set, so this must succeed: {e}"));
        let Err(why) = c.perform(&wrong_domain_step) else {
            panic!("a {wrong_domain_step} must be refused when running as the other domain");
        };
        assert!(why.contains("refusing to perform"), "{why}");
        assert!(
            why.contains(wrong_domain_step.domain().as_str()),
            "the refusal must name the domain the step needs: {why}"
        );

        if let Some(v) = restore {
            std::env::set_var(NO_APPLY_ENV, v);
        }
    }

    #[test]
    fn every_step_belongs_to_exactly_one_domain() {
        // The domain split is what removes `sudo` from `apply` entirely. A step
        // in neither list would be skipped by `rime apply` AND by
        // `sudo rime apply`, and the only symptom would be a machine that never
        // converges that one field.
        for step in all_steps() {
            let d = step.domain();
            assert!(matches!(d, Domain::User | Domain::Root), "{step}");
        }
        let root: Vec<String> = all_steps()
            .iter()
            .filter(|s| s.domain() == Domain::Root)
            .map(ToString::to_string)
            .collect();
        assert_eq!(root.len(), 3, "{root:?}");
    }

    #[test]
    fn the_three_paths_are_distinct_and_in_the_right_trees() {
        let user = user_blueprint_path();
        let state = applied_state_path();
        assert_ne!(user, state);
        assert!(user.to_string_lossy().contains("rime/blueprint.toml"));
        // Desired lives in config, generated lives in state. Putting the
        // generated record next to the hand-edited file is how someone ends up
        // editing the wrong one.
        assert!(state.to_string_lossy().contains("rime/blueprint-state.toml"));
        assert_eq!(site_blueprint_path(), Path::new("/etc/rime/blueprint.toml"));
    }
}
