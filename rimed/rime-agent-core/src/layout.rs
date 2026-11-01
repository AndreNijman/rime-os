//! Project window and terminal layouts (roadmap §6).
//!
//! §6 asks Rime to "remember the windows and terminals associated with a
//! project" and to "restore the project after reboot".
//!
//! ## What is remembered, and what cannot be
//!
//! Not window handles. A Hyprland address and a niri window id are both
//! meaningless after a restart, so a stored layout that named them would be
//! restorable exactly zero times. What is stored is *how to recreate* each
//! window: its argv, its working directory, and the workspace it was on.
//!
//! ## Which windows belong to a project
//!
//! Determined from the working directory, not from the title. A title is
//! whatever the application decided to print; a cwd is where the process
//! actually is.
//!
//! The subtlety is that a terminal's own cwd is where it was *launched*, which
//! is usually `$HOME` — the shell inside it is what moved into the project. So
//! [`resolve_cwd`] checks the window's process and then its descendants, and
//! takes the first directory that lies under the project root. That rule is
//! stated rather than inferred, because "closest match wins" and "first match
//! wins" behave differently for a terminal running a build in a subdirectory,
//! and one of them has to be chosen on purpose.
//!
//! ## Restoring runs stored command lines
//!
//! Which is worth being explicit about: the layout file is a list of argv
//! vectors that `rime project restore` will execute. It lives under
//! `$XDG_STATE_HOME`, `0700`, and is written only by the user's own runtime —
//! but it is still a file that says "run this", so it is executed with
//! [`std::process::Command`] and an argv list, never through a shell. Nothing
//! in a stored entry can be interpreted as a shell metacharacter, because
//! there is no shell.
//!
//! Restore is never automatic. It happens when somebody runs
//! `rime project restore`, because a login that reopens fourteen windows
//! nobody asked for is a worse experience than one that reopens none.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Application ids that are terminal emulators.
///
/// Lowercased comparison, because Hyprland reports `Alacritty` and Wayland
/// app-ids are conventionally reverse-DNS and lowercase.
const TERMINAL_IDS: &[&str] = &[
    "alacritty",
    "org.alacritty",
    "foot",
    "footclient",
    "ghostty",
    "com.mitchellh.ghostty",
    "kitty",
    "org.wezfurlong.wezterm",
    "wezterm",
    "xterm",
    "org.gnome.console",
    "org.gnome.terminal",
    "konsole",
    "st",
];

/// How deep to walk a window's process descendants looking for a project cwd.
///
/// terminal -> shell -> program is three; a shell running a build script that
/// runs a compiler is five. Ten is generous without being unbounded.
const MAX_DESCENDANT_DEPTH: usize = 10;

/// One window as reported by `rime-project-windows list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowReport {
    /// Opaque, compositor-specific, valid only for this session. Never stored.
    #[serde(default)]
    pub handle: Option<serde_json::Value>,
    #[serde(default)]
    pub pid: Option<i32>,
    #[serde(default)]
    pub app_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub workspace: String,
    #[serde(default)]
    pub floating: Option<bool>,
}

impl WindowReport {
    /// Whether this window is a terminal emulator.
    pub fn is_terminal(&self) -> bool {
        let id = self.app_id.to_lowercase();
        TERMINAL_IDS.iter().any(|t| id == *t)
    }
}

/// One window in a saved layout: enough to recreate it, and nothing that
/// cannot survive a reboot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutEntry {
    /// argv of the process that owned the surface, as a list. Never a string:
    /// this is executed, and a string would need a shell to split it.
    pub argv: Vec<String>,
    /// Working directory to start it in. Always under the project root.
    pub cwd: String,
    /// Workspace it was on, as the compositor names it.
    pub workspace: String,
    /// Application id, for display and for matching an already-open window.
    pub app_id: String,
    /// Whether this was a terminal. Terminals are restored with the working
    /// directory passed as a flag, because their argv rarely carries it.
    pub terminal: bool,
}

/// Everything remembered about one project's windows.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectLayout {
    pub entries: Vec<LayoutEntry>,
    /// Unix seconds the layout was captured.
    #[serde(default)]
    pub saved: u64,
    /// The terminal template last opened for this project, if any — see
    /// [`crate::mux`]. Kept in the SAME record as the captured windows rather
    /// than in a second store: a project has one layout, and half of it is
    /// desktop windows somebody had open while the other half is the shape
    /// their terminal work takes. Two stores would mean two things to forget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// The multiplexer that template was opened in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mux: Option<String>,
}

impl ProjectLayout {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Workspaces the layout spans, in first-seen order.
    pub fn workspaces(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for e in &self.entries {
            if !e.workspace.is_empty() && !out.contains(&e.workspace) {
                out.push(e.workspace.clone());
            }
        }
        out
    }
}

// ── reading /proc ───────────────────────────────────────────────────────────

/// A process's working directory, or `None` when it cannot be read.
pub fn proc_cwd(pid: i32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// A process's argv, split on NULs.
///
/// Empty when the process is gone or is a kernel thread. Trailing empties are
/// dropped: `/proc/<pid>/cmdline` ends with a NUL, so a naive split yields a
/// final empty string that would become an empty argv element.
pub fn proc_argv(pid: i32) -> Vec<String> {
    let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return Vec::new();
    };
    raw.split(|b| *b == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect()
}

/// Direct children of every pid, read once.
///
/// Built as a map rather than re-scanning `/proc` per pid: a capture asks about
/// a dozen windows, and scanning a few hundred process directories twelve times
/// over is both slower and less consistent — processes come and go between
/// scans, so one snapshot gives one coherent answer.
pub fn child_map() -> BTreeMap<i32, Vec<i32>> {
    let mut out: BTreeMap<i32, Vec<i32>> = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(pid) = name.parse::<i32>() else { continue };
        if let Some(parent) = parent_of(pid) {
            out.entry(parent).or_default().push(pid);
        }
    }
    out
}

/// The parent pid of `pid`, from `/proc/<pid>/status`.
///
/// `status` rather than `stat`, for the same reason as in the daemon's peer
/// resolution: `stat`'s second field is the command name in parentheses and may
/// itself contain spaces and parentheses.
pub fn parent_of(pid: i32) -> Option<i32> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("PPid:"))
        .and_then(|rest| rest.trim().parse().ok())
}

/// The working directory to record for a window, given its pid.
///
/// Checks the window's own process first, then its descendants breadth-first,
/// returning the first directory that lies under `root`. Breadth-first rather
/// than depth-first on purpose: the shell directly inside a terminal is the
/// directory the user thinks of as "where that window is", not whatever a
/// nested build step happened to `cd` into.
pub fn resolve_cwd(
    pid: i32,
    root: &Path,
    children: &BTreeMap<i32, Vec<i32>>,
) -> Option<PathBuf> {
    let mut frontier = vec![pid];
    for _ in 0..MAX_DESCENDANT_DEPTH {
        if frontier.is_empty() {
            return None;
        }
        for p in &frontier {
            if let Some(cwd) = proc_cwd(*p) {
                if cwd.starts_with(root) {
                    return Some(cwd);
                }
            }
        }
        let mut next = Vec::new();
        for p in &frontier {
            if let Some(kids) = children.get(p) {
                next.extend(kids.iter().copied());
            }
        }
        frontier = next;
    }
    None
}

// ── capture ─────────────────────────────────────────────────────────────────

/// Turn a set of window reports into a layout for one project.
///
/// A window is included when a process in its tree is working inside `root`.
/// Windows with no pid are skipped — labwc reports none, and without a pid
/// there is no cwd and therefore no way to know whether the window has anything
/// to do with this project. Guessing from the title would put somebody's
/// unrelated editor into a layout because it happened to have the project name
/// on the tab.
pub fn capture(
    reports: &[WindowReport],
    root: &Path,
    children: &BTreeMap<i32, Vec<i32>>,
) -> ProjectLayout {
    let mut entries = Vec::new();
    for w in reports {
        let Some(pid) = w.pid else { continue };
        if pid <= 0 {
            continue;
        }
        let Some(cwd) = resolve_cwd(pid, root, children) else {
            continue;
        };
        let argv = proc_argv(pid);
        if argv.is_empty() {
            continue;
        }
        entries.push(LayoutEntry {
            argv,
            cwd: cwd.to_string_lossy().into_owned(),
            workspace: w.workspace.clone(),
            app_id: w.app_id.clone(),
            terminal: w.is_terminal(),
        });
    }
    ProjectLayout {
        entries,
        saved: now_secs(),
        ..Default::default()
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ── restore ─────────────────────────────────────────────────────────────────

/// The argv to run to recreate one entry.
///
/// For a terminal the stored argv is discarded in favour of a freshly built
/// command with the working directory passed explicitly. The stored argv for a
/// terminal is typically just `alacritty` with no directory at all — it
/// inherited its cwd from whatever launched it — so replaying it verbatim opens
/// a terminal in the wrong place, which is the single most useless possible
/// outcome of "restore my project".
///
/// `terminal_argv` is the emulator and its working-directory flag, e.g.
/// `["ghostty", "--working-directory="]` or `["foot", "--working-directory"]`.
/// A flag that ends in `=` takes the directory in the same argument: Ghostty
/// reads only `--key=value`, and `--working-directory /dir` is refused.
pub fn restore_argv(entry: &LayoutEntry, terminal_argv: &[String]) -> Vec<String> {
    if entry.terminal && terminal_argv.len() >= 2 {
        let mut argv = terminal_argv.to_vec();
        let flag = argv.last_mut().expect("len >= 2");
        if flag.ends_with('=') {
            flag.push_str(&entry.cwd);
        } else {
            argv.push(entry.cwd.clone());
        }
        return argv;
    }
    entry.argv.clone()
}

/// Terminal emulators and the flag each uses to set its working directory, in
/// preference order.
///
/// The flag differs per emulator and getting it wrong is not a syntax error —
/// most of them treat an unknown flag as a command to run, so the terminal
/// opens, fails, and closes again.
///
/// Ghostty is first: it is Rime's default terminal (Alacritty stays installed
/// as the alternative).
pub const TERMINAL_CANDIDATES: &[(&str, &str)] = &[
    ("ghostty", "--working-directory="),
    ("alacritty", "--working-directory"),
    ("foot", "--working-directory"),
    ("kitty", "--directory"),
    ("wezterm", "--cwd"),
    ("xterm", "-e"),
];

/// Pick a terminal from `TERMINAL_CANDIDATES`, preferring `$TERMINAL`.
///
/// `lookup` answers "is this program installed", so the choice is testable
/// without depending on what happens to be on the machine running the tests.
pub fn choose_terminal<F>(env_terminal: Option<&str>, lookup: F) -> Option<Vec<String>>
where
    F: Fn(&str) -> bool,
{
    if let Some(name) = env_terminal.filter(|n| !n.is_empty()) {
        // A $TERMINAL that is one of the known emulators gets its correct flag.
        // One that is not gets no flag at all rather than a guessed one — a
        // wrong flag makes the terminal exit immediately, and no flag at least
        // opens a usable window in the wrong directory.
        if lookup(name) {
            let base = name.rsplit('/').next().unwrap_or(name);
            if let Some((_, flag)) = TERMINAL_CANDIDATES.iter().find(|(t, _)| *t == base) {
                return Some(vec![name.to_string(), (*flag).to_string()]);
            }
            return Some(vec![name.to_string()]);
        }
    }
    for (name, flag) in TERMINAL_CANDIDATES {
        if lookup(name) {
            return Some(vec![(*name).to_string(), (*flag).to_string()]);
        }
    }
    None
}

// ── the store ───────────────────────────────────────────────────────────────

/// Where a project's layout is kept.
pub fn layout_path(slug: &str) -> PathBuf {
    crate::paths::state_dir()
        .join("layouts")
        .join(format!("{slug}.json"))
}

pub fn save(slug: &str, layout: &ProjectLayout) -> Result<()> {
    let path = layout_path(slug);
    let dir = path.parent().context("layout path has no parent")?;
    crate::paths::ensure_private_dir(dir)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(layout)?.as_bytes())
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("renaming into {}", path.display()))?;
    Ok(())
}

/// Read a layout. A missing or unparseable file is "no layout", not an error:
/// the only sensible response to either is to say there is nothing to restore.
pub fn load(slug: &str) -> Option<ProjectLayout> {
    let text = std::fs::read_to_string(layout_path(slug)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn forget(slug: &str) -> Result<()> {
    let path = layout_path(slug);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(pid: Option<i32>, app_id: &str, workspace: &str) -> WindowReport {
        WindowReport {
            handle: None,
            pid,
            app_id: app_id.to_string(),
            title: "whatever".into(),
            workspace: workspace.to_string(),
            floating: Some(false),
        }
    }

    // ── terminal classification ─────────────────────────────────────────────

    #[test]
    fn terminals_are_recognised_whatever_the_case() {
        // Hyprland reports `Alacritty`; a Wayland app-id is lowercase
        // reverse-DNS. Both are the same emulator.
        assert!(report(Some(1), "Alacritty", "1").is_terminal());
        assert!(report(Some(1), "alacritty", "1").is_terminal());
        assert!(report(Some(1), "com.mitchellh.ghostty", "1").is_terminal());
        assert!(report(Some(1), "foot", "1").is_terminal());
    }

    #[test]
    fn an_application_is_not_a_terminal() {
        for id in ["firefox", "org.gnome.Nautilus", "code", "", "termius"] {
            assert!(!report(Some(1), id, "1").is_terminal(), "{id}");
        }
    }

    // ── capture ─────────────────────────────────────────────────────────────

    #[test]
    fn a_window_with_no_pid_is_skipped_not_guessed_at() {
        // labwc reports no pid. Without one there is no cwd, so there is no
        // basis for saying the window belongs to this project — and matching on
        // the title would capture an unrelated editor that happens to have the
        // project name on a tab.
        let children = BTreeMap::new();
        let reports = vec![
            report(None, "Alacritty", "1"),
            report(Some(0), "Alacritty", "1"),
            report(Some(-1), "Alacritty", "1"),
        ];
        let layout = capture(&reports, Path::new("/tmp"), &children);
        assert!(layout.is_empty(), "{layout:?}");
    }

    #[test]
    fn capture_records_this_process_when_its_cwd_is_under_the_root() {
        // Against real /proc, using this test process: its cwd is the crate
        // directory, so a root of "/" always contains it.
        let me = std::process::id() as i32;
        let children = child_map();
        let reports = vec![report(Some(me), "Alacritty", "3")];
        let layout = capture(&reports, Path::new("/"), &children);
        assert_eq!(layout.entries.len(), 1, "{layout:?}");
        let e = &layout.entries[0];
        assert!(!e.argv.is_empty(), "argv must be recorded");
        assert!(e.terminal);
        assert_eq!(e.workspace, "3");
        assert!(Path::new(&e.cwd).is_absolute());
        assert!(layout.saved > 0);
    }

    #[test]
    fn capture_skips_a_window_working_outside_the_project() {
        let me = std::process::id() as i32;
        let children = child_map();
        // A root this process is certainly not inside.
        let reports = vec![report(Some(me), "Alacritty", "1")];
        let layout = capture(&reports, Path::new("/proc/self/fdinfo"), &children);
        assert!(layout.is_empty(), "{layout:?}");
    }

    #[test]
    fn a_layout_never_stores_a_window_handle() {
        // The property that makes a layout restorable at all: a Hyprland
        // address and a niri id are both invalid after a restart.
        let me = std::process::id() as i32;
        let mut r = report(Some(me), "Alacritty", "1");
        r.handle = Some(serde_json::json!("0x55d1a2b3c4"));
        let layout = capture(&[r], Path::new("/"), &child_map());
        let text = serde_json::to_string(&layout).unwrap();
        assert!(!text.contains("0x55d1a2b3c4"), "{text}");
        assert!(!text.contains("handle"), "{text}");
    }

    #[test]
    fn workspaces_are_listed_once_in_first_seen_order() {
        let layout = ProjectLayout {
            entries: vec![
                LayoutEntry { argv: vec!["a".into()], cwd: "/p".into(), workspace: "2".into(), app_id: "x".into(), terminal: false },
                LayoutEntry { argv: vec!["b".into()], cwd: "/p".into(), workspace: "1".into(), app_id: "x".into(), terminal: false },
                LayoutEntry { argv: vec!["c".into()], cwd: "/p".into(), workspace: "2".into(), app_id: "x".into(), terminal: false },
                LayoutEntry { argv: vec!["d".into()], cwd: "/p".into(), workspace: "".into(),  app_id: "x".into(), terminal: false },
            ],
            saved: 1,
            ..Default::default()
        };
        assert_eq!(layout.workspaces(), vec!["2".to_string(), "1".to_string()]);
    }

    // ── /proc reading ───────────────────────────────────────────────────────

    #[test]
    fn argv_has_no_trailing_empty_element() {
        // /proc/<pid>/cmdline ends with a NUL, so a naive split yields a final
        // empty string that would become an empty argv element — and an empty
        // argv[0] is an exec that fails.
        let argv = proc_argv(std::process::id() as i32);
        assert!(!argv.is_empty());
        assert!(argv.iter().all(|a| !a.is_empty()), "{argv:?}");
    }

    #[test]
    fn a_dead_pid_yields_no_argv_and_no_cwd_rather_than_panicking() {
        assert!(proc_argv(0x7fff_fffe).is_empty());
        assert_eq!(proc_cwd(0x7fff_fffe), None);
        assert_eq!(parent_of(0x7fff_fffe), None);
    }

    #[test]
    fn the_child_map_agrees_with_our_own_parent() {
        let me = std::process::id() as i32;
        let parent = parent_of(me).expect("we have a parent");
        let map = child_map();
        assert!(
            map.get(&parent).is_some_and(|kids| kids.contains(&me)),
            "the child map does not list {me} under {parent}"
        );
    }

    #[test]
    fn resolve_cwd_walks_descendants_not_just_the_window_process() {
        // The bug this exists for: a terminal's OWN cwd is where it was
        // launched, usually $HOME. The shell inside it is what moved into the
        // project. Simulated by asking about our parent with a root only WE are
        // inside — resolving requires descending.
        let me = std::process::id() as i32;
        let parent = parent_of(me).expect("we have a parent");
        let mine = proc_cwd(me).expect("we have a cwd");
        let children = child_map();

        let found = resolve_cwd(parent, &mine, &children);
        assert_eq!(found.as_deref(), Some(mine.as_path()), "descent failed");
    }

    #[test]
    fn resolve_cwd_is_bounded() {
        // pid 1's tree is the whole machine; with a root nothing matches, this
        // must terminate rather than walk every process on the system forever.
        let children = child_map();
        let out = resolve_cwd(1, Path::new("/proc/self/fdinfo"), &children);
        assert_eq!(out, None);
    }

    // ── restore ─────────────────────────────────────────────────────────────

    #[test]
    fn a_terminal_is_restored_with_its_directory_not_with_its_stored_argv() {
        // The stored argv for a terminal is typically bare `alacritty`, because
        // it inherited its cwd from whatever launched it. Replaying that opens
        // a terminal in the wrong place, which is the most useless possible
        // outcome of "restore my project".
        let e = LayoutEntry {
            argv: vec!["alacritty".into()],
            cwd: "/home/t/Projects/demo".into(),
            workspace: "2".into(),
            app_id: "Alacritty".into(),
            terminal: true,
        };
        let term = vec!["foot".to_string(), "--working-directory".to_string()];
        assert_eq!(
            restore_argv(&e, &term),
            vec!["foot", "--working-directory", "/home/t/Projects/demo"]
        );
    }

    #[test]
    fn ghostty_gets_its_directory_in_the_same_argument() {
        // Ghostty's CLI takes only --key=value; `--working-directory /dir`
        // makes it refuse to start ("invalid field").
        let e = LayoutEntry {
            argv: vec!["ghostty".into()],
            cwd: "/home/t/Projects/my demo".into(),
            workspace: "2".into(),
            app_id: "com.mitchellh.ghostty".into(),
            terminal: true,
        };
        let term = choose_terminal(None, |n| n == "ghostty" || n == "alacritty").unwrap();
        assert_eq!(
            restore_argv(&e, &term),
            vec!["ghostty", "--working-directory=/home/t/Projects/my demo"]
        );
        let term = choose_terminal(Some("alacritty"), |n| n == "ghostty" || n == "alacritty").unwrap();
        assert_eq!(
            restore_argv(&e, &term),
            vec!["alacritty", "--working-directory", "/home/t/Projects/my demo"]
        );
    }

    #[test]
    fn ghostty_is_preferred_when_terminal_is_unset() {
        let both = |n: &str| n == "ghostty" || n == "alacritty" || n == "foot";
        assert_eq!(
            choose_terminal(None, both),
            Some(vec!["ghostty".to_string(), "--working-directory=".to_string()])
        );
        assert_eq!(
            choose_terminal(Some("ghostty"), both),
            Some(vec!["ghostty".to_string(), "--working-directory=".to_string()])
        );
        // Ghostty not installed: Alacritty, the alternative, is next.
        assert_eq!(
            choose_terminal(None, |n| n == "alacritty" || n == "foot"),
            Some(vec!["alacritty".to_string(), "--working-directory".to_string()])
        );
    }

    #[test]
    fn an_application_is_restored_with_exactly_what_it_was_running() {
        let e = LayoutEntry {
            argv: vec!["firefox".into(), "--new-window".into(), "https://x".into()],
            cwd: "/home/t/Projects/demo".into(),
            workspace: "3".into(),
            app_id: "firefox".into(),
            terminal: false,
        };
        let term = vec!["foot".to_string(), "--working-directory".to_string()];
        assert_eq!(restore_argv(&e, &term), e.argv);
    }

    #[test]
    fn a_terminal_falls_back_to_its_stored_argv_when_no_emulator_is_known() {
        let e = LayoutEntry {
            argv: vec!["alacritty".into()],
            cwd: "/p".into(),
            workspace: "1".into(),
            app_id: "Alacritty".into(),
            terminal: true,
        };
        assert_eq!(restore_argv(&e, &[]), e.argv);
        // A one-element terminal_argv means "installed but no known flag", and
        // appending the directory to it would pass the path as a COMMAND to
        // run. Better to replay the original.
        assert_eq!(restore_argv(&e, &["someterm".to_string()]), e.argv);
    }

    #[test]
    fn the_configured_terminal_wins_and_gets_its_own_flag() {
        let installed = |n: &str| n == "kitty" || n == "alacritty";
        assert_eq!(
            choose_terminal(Some("kitty"), installed),
            Some(vec!["kitty".to_string(), "--directory".to_string()])
        );
    }

    #[test]
    fn an_unknown_terminal_gets_no_flag_rather_than_a_guessed_one() {
        // A wrong flag is worse than none: most emulators treat an unknown
        // flag as a command to run, so the window opens, fails and closes.
        let installed = |n: &str| n == "myterm";
        assert_eq!(
            choose_terminal(Some("myterm"), installed),
            Some(vec!["myterm".to_string()])
        );
    }

    #[test]
    fn an_uninstalled_configured_terminal_falls_through_to_a_real_one() {
        let installed = |n: &str| n == "foot";
        assert_eq!(
            choose_terminal(Some("nothere"), installed),
            Some(vec!["foot".to_string(), "--working-directory".to_string()])
        );
    }

    #[test]
    fn no_terminal_at_all_is_reported_rather_than_invented() {
        assert_eq!(choose_terminal(None, |_| false), None);
        assert_eq!(choose_terminal(Some(""), |_| false), None);
    }

    // ── the store ───────────────────────────────────────────────────────────

    #[test]
    fn a_layout_round_trips_and_a_missing_one_is_not_an_error() {
        let slug = format!("layout-test-{}", std::process::id());
        assert!(load(&slug).is_none(), "a fresh slug must have no layout");

        let layout = ProjectLayout {
            entries: vec![LayoutEntry {
                argv: vec!["nvim".into(), "src/main.rs".into()],
                cwd: "/home/t/Projects/demo".into(),
                workspace: "1".into(),
                app_id: "Alacritty".into(),
                terminal: true,
            }],
            saved: 1_700_000_000,
            ..Default::default()
        };
        save(&slug, &layout).expect("save");
        let back = load(&slug).expect("load");
        assert_eq!(back.entries, layout.entries);
        assert_eq!(back.saved, layout.saved);

        forget(&slug).expect("forget");
        assert!(load(&slug).is_none());
        // Forgetting twice is not an error.
        forget(&slug).expect("second forget");
    }

    #[test]
    fn a_corrupt_layout_reads_as_no_layout() {
        let slug = format!("layout-corrupt-{}", std::process::id());
        let path = layout_path(&slug);
        crate::paths::ensure_private_dir(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{ not json").unwrap();
        assert!(load(&slug).is_none());
        std::fs::remove_file(&path).ok();
    }
}
