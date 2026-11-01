//! Recovery, repair and factory-reset policy (roadmap §19), as pure data.
//!
//! Nothing here touches a filesystem. The CLI does the probing, the resolving
//! and the deleting; this module owns the three tables that decide *what* is
//! allowed to happen, because each of them is a policy that has to be
//! reviewable in one place and testable without a machine:
//!
//! * [`ROWS`] — the recovery surface §19 asks for, and the state vocabulary
//!   every row reports in.
//! * [`REPAIRS`] — the steps `[Repair automatically]` is allowed to run. The
//!   invariant is enforced by a test, not by a comment: **a step is eligible
//!   for automatic repair only if it is idempotent and removes no data.**
//! * [`targets`] — the reset ladder, and therefore the exact data boundary of
//!   the most destructive verb in the product.
//!
//! ## Why the reset boundary is a table and not a code path
//!
//! §19's own words are "allow a refresh/reset path that is explicit about what
//! user data is preserved". A reset whose scope is spread across `if` branches
//! cannot be read, cannot be diffed, and cannot be asserted. So every path a
//! reset will ever touch is one row of a `const` table carrying its scope, its
//! disposition and a sentence a human can check — and the suite asserts
//! properties over the whole table rather than over the one case somebody
//! remembered to write.
//!
//! ## What is deliberately NOT in the table
//!
//! * **Anything under `/etc`.** `/etc` on a Rime machine holds `passwd`,
//!   `shadow`, `fstab`, `crypttab` and the NetworkManager connections. ostree
//!   three-way-merges it against the deployment, and there is no runtime verb
//!   that restores it to image state without deploying. A reset that emptied
//!   it would produce a machine that does not boot or cannot be logged into,
//!   and the thing that would undo it is what it broke. Restoring `/etc` is a
//!   reinstall; `docs/recovery.md` says so rather than this table pretending
//!   otherwise.
//! * **Anything under `/var/lib/rime`.** The package extension, the model
//!   store and the boot-health records each have a program that owns them
//!   (`rime-pkg`, `rime ai`, `rime-boot-health`). Deleting `pkg/state.json`
//!   under a merged system extension leaves `/usr` carrying packages Rime can
//!   no longer name — a worse state than the one the user was trying to leave.
//!   The machine-level operations are the verbs that already exist, and
//!   `rime recover status` names them.
//! * **The capsule records under `~/.local/share/rime/env`.** Each one names a
//!   real rootless container. Deleting the record orphans the container:
//!   `rime env list` would show nothing while `podman ps -a` still shows them,
//!   and Rime would have lost the name it needs to remove them. `rime env rm`
//!   is the verb for that, and it is named in the preserved list.
//! * **Anything under `~/.config/hypr` as a deletion.** The two generated
//!   modules `rime/input.lua` and `rime/monitors.lua` are **truncated** to the
//!   "no overrides" / "no saved layout" state, never removed, and every other
//!   file in that directory is preserved. A one-line edit to a live compositor
//!   config has already cost this project a desktop once.
//!
//!   Truncating rather than deleting used to be forced: `hyprland.conf`
//!   `source=`d both and a `source=` with no match is a FATAL config error.
//!   P0-025 removed that constraint — `hyprland.lua` checks
//!   `package.searchpath` before requiring, so an absent module is skipped —
//!   and the rule is kept anyway, because "empty" is a state the compositor
//!   understands and one this code can produce without deciding which of the
//!   user's files it is allowed to remove.
//!
//!   `rime/user-overrides.lua` is emphatically NOT a target. It is where
//!   `rime-hypr-migrate` puts a user's own hand-written hyprlang after
//!   converting it, so it is user content that happens to live under a
//!   generated-looking name, and a desktop reset that emptied it would destroy
//!   exactly what the migration promised to keep.

use std::fmt;
use std::str::FromStr;

// ── the recovery surface ─────────────────────────────────────────────────────

/// How a recovery-surface row reports.
///
/// §19 asks for "each with a verified/available state", and these are the four
/// answers that are honest on a real machine. `Attention` and `Unavailable`
/// are distinct on purpose: a package extension built for the previous OS
/// release needs a rebuild (actionable), while a machine with no TPM cannot
/// report a measured-boot state at all (nothing to act on). Collapsing them
/// would make "nothing is wrong" and "nothing could be measured" look alike,
/// which is the failure `rime boot status` already refuses for its entry list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// Present and checked against something. The strongest claim.
    Verified,
    /// Present and usable, but nothing verified it. A rollback target exists;
    /// no one asserted it boots.
    Available,
    /// Present and wrong in a way a named action can fix.
    Attention,
    /// Could not be determined, or does not exist on this hardware. Never a
    /// synonym for "fine".
    Unavailable,
}

impl Health {
    pub fn as_str(self) -> &'static str {
        match self {
            Health::Verified => "verified",
            Health::Available => "available",
            Health::Attention => "attention",
            Health::Unavailable => "unavailable",
        }
    }
}

impl fmt::Display for Health {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One row of the recovery surface: a stable id a UI can key on, and the label
/// it shows.
///
/// The id is the compatibility surface. Rime Shell will render these rows, and
/// a renamed id is a broken settings page — the same rule `org.rimeos.Rimed1`
/// members live under. The label may be reworded freely.
pub struct RowSpec {
    pub id: &'static str,
    pub label: &'static str,
}

/// The eight components §19 lists, in the order it lists them.
///
/// Exactly eight, and the suite asserts the set: a row silently dropped from
/// the report is a component nobody is looking at any more, which is the state
/// this surface exists to prevent.
pub const ROWS: &[RowSpec] = &[
    RowSpec { id: "current-deployment", label: "Current deployment" },
    RowSpec { id: "previous-deployment", label: "Previous deployment" },
    RowSpec { id: "secure-boot", label: "Secure Boot" },
    RowSpec { id: "filesystem", label: "Filesystem" },
    RowSpec { id: "gpu-driver", label: "GPU driver" },
    RowSpec { id: "rime-shell", label: "Rime Shell" },
    RowSpec { id: "network", label: "Network" },
    RowSpec { id: "package-extensions", label: "Package extensions" },
];

// ── automatic repair ─────────────────────────────────────────────────────────

/// Which privilege a repair step needs.
///
/// The same split `rime apply` uses, and for the same reason: a single verb
/// that demanded root would make the user half — reseeding a broken desktop —
/// reachable only by running it as the wrong user, and root has no session to
/// reseed. So `rime recover repair` converges the domain it is already in and
/// reports the other, and nothing here ever calls `sudo` itself. That is what
/// keeps repair incapable of raising an authentication prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Domain {
    /// Runs as the invoking user, writing only their own files.
    User,
    /// Needs real privilege. Reported, never escalated to.
    System,
}

impl Domain {
    pub fn as_str(self) -> &'static str {
        match self {
            Domain::User => "user",
            Domain::System => "system",
        }
    }
}

/// One step `[Repair automatically]` may run.
///
/// `argv` is the whole command, absolute program included, so the step is a
/// datum rather than a construction — the suite reads these and asserts the
/// non-destructive invariant over them directly.
pub struct RepairStep {
    pub id: &'static str,
    pub domain: Domain,
    /// The command, exactly as it will be spawned.
    pub argv: &'static [&'static str],
    /// What it does, in a sentence a user can accept or refuse.
    pub what: &'static str,
    /// Why it is safe to run without asking anything else.
    pub why_safe: &'static str,
}

/// The complete set of automatic repairs.
///
/// ## The invariant, and why it is narrow
///
/// Every step here is idempotent and removes no data. That is what makes a
/// single `[Repair automatically]` button defensible: pressing it twice does
/// nothing the second time, and pressing it by accident costs nothing.
///
/// §19 lists `[Repair automatically]`, `[Boot previous deployment]` and
/// `[Factory reset]` as three separate actions, and that separation is the
/// design. Rollback and reset are not repairs — they are decisions with
/// consequences the user has to see first — so they are not in this table and
/// `rime recover repair` will not perform them. `tests/test-rime-recover.sh`
/// asserts that no step's argv contains a destructive verb, so adding one is a
/// test failure rather than a review miss.
pub const REPAIRS: &[RepairStep] = &[
    RepairStep {
        id: "reprovision-desktop",
        domain: Domain::User,
        argv: &[rime_paths::RIME_SHELL_FIRSTRUN],
        what: "re-seed the per-user Rime Shell configuration",
        why_safe: "the provisioner is idempotent by design, runs at every login \
                   already, needs no network, and only creates files that are \
                   absent — it never overwrites a customised one",
    },
    RepairStep {
        id: "rebuild-package-extension",
        domain: Domain::System,
        argv: &[rime_paths::RIME_PKG, "rebuild", "--if-needed"],
        what: "rebuild the user package extension against the booted OS",
        why_safe: "--if-needed makes the engine decide, and a rebuild resolves \
                   the same requested package list again; the previous \
                   extension is kept for `rime pkg rollback`",
    },
];

// ── one step that is deliberately NOT here: `ostree admin pin 0` ────────────
//
// Pinning the booted deployment is genuinely useful — bootc keeps only
// booted+previous, so two bad updates in a row can evict the last good image,
// and `docs/rollback.md` says to pin before anything risky. It is also
// idempotent and deletes nothing, so it would pass every invariant above.
//
// It is not an automatic repair because Rime cannot tell whether it is
// *needed*. Whether a deployment is pinned is `ostree admin status` output,
// and `rime recover status` reads files rather than spawning subprocesses,
// precisely so it can never hang or prompt. A repair with no diagnosis behind
// it would be proposed on every healthy machine, and a `[Repair
// automatically]` button that always has something to say is one people learn
// to ignore.
//
// So it is advice on the previous-deployment row instead of an action here.

// ── the reset ladder ─────────────────────────────────────────────────────────

/// How far a reset reaches.
///
/// Two rungs, and there is deliberately no third. See the module header for
/// why `/etc`, `/var/lib/rime` and the capsule records are out of scope, and
/// `docs/recovery.md` for why a true factory reset — accounts gone, `/etc`
/// pristine, disks repartitioned — is the installer's job and not a verb on a
/// running system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ResetScope {
    /// The Rime desktop, back to the state the image provisions. Settings,
    /// keybinds and caches; no document, credential or project.
    Desktop,
    /// Every Rime-owned file in this account, including the blueprint and the
    /// per-game, trusted-device and local-model settings the user authored.
    User,
}

impl ResetScope {
    pub fn as_str(self) -> &'static str {
        match self {
            ResetScope::Desktop => "desktop",
            ResetScope::User => "user",
        }
    }

    /// One sentence naming the loss, for the confirmation prompt.
    pub fn summary(self) -> &'static str {
        match self {
            ResetScope::Desktop => {
                "Rime Shell's settings, keybinds and caches for this account"
            }
            ResetScope::User => {
                "everything under `desktop`, PLUS your blueprint, per-game \
                 profiles, trusted-device registry, local-model settings and \
                 recorded agent sessions"
            }
        }
    }

    pub const ALL: &'static [ResetScope] = &[ResetScope::Desktop, ResetScope::User];
}

impl fmt::Display for ResetScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The error `--scope` produces for a name that is not a rung.
#[derive(Debug)]
pub struct UnknownScope(pub String);

impl fmt::Display for UnknownScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown reset scope '{}' (known: {})",
            self.0,
            ResetScope::ALL
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

impl std::error::Error for UnknownScope {}

impl FromStr for ResetScope {
    type Err = UnknownScope;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "desktop" => Ok(ResetScope::Desktop),
            "user" => Ok(ResetScope::User),
            other => Err(UnknownScope(other.to_string())),
        }
    }
}

/// What happens to a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Removed. A directory goes recursively.
    Delete,
    /// Emptied in place, keeping the file. Used for everything under
    /// `~/.config/hypr` — see the module header.
    Truncate,
}

impl Disposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Disposition::Delete => "delete",
            Disposition::Truncate => "truncate",
        }
    }
}

/// Whether a target names a file or a directory.
///
/// Recorded rather than probed, because it decides which removal call is legal:
/// a target declared `File` is never removed recursively, so a symlink swapped
/// for a directory under a target path cannot turn one deletion into many.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
}

/// One path a reset may touch.
pub struct Target {
    /// Relative to the invoking user's home. Never absolute, never containing
    /// `..` — asserted in the tests, because a table entry is the one place a
    /// traversal would be invisible.
    pub rel: &'static str,
    pub kind: Kind,
    pub how: Disposition,
    /// The narrowest scope that includes this target.
    pub scope: ResetScope,
    pub what: &'static str,
}

/// Every target, in one table.
///
/// Ordered narrowest-scope-first so the rendered plan reads as a ladder.
const TARGETS: &[Target] = &[
    // ── desktop ─────────────────────────────────────────────────────────────
    Target {
        rel: ".config/rime-shell/display.json",
        kind: Kind::File,
        how: Disposition::Delete,
        scope: ResetScope::Desktop,
        what: "saved monitor layout, scale and refresh rate",
    },
    Target {
        rel: ".config/rime-shell/input.json",
        kind: Kind::File,
        how: Disposition::Delete,
        scope: ResetScope::Desktop,
        what: "keyboard, pointer and touchpad settings",
    },
    Target {
        rel: ".config/rime-shell/RimeShellInput.kdl",
        kind: Kind::File,
        how: Disposition::Delete,
        scope: ResetScope::Desktop,
        what: "the generated niri input block",
    },
    // Written by images before P0-025, when the shell generated a hyprlang
    // fragment and a Lua one side by side here. Nothing writes either now — the
    // Hyprland keybinds are `.config/hypr/rime/shell-keybinds.lua` — so these
    // two entries exist to clear the leftovers off an upgraded machine.
    Target {
        rel: ".config/rime-shell/RimeShellKeybinds.conf",
        kind: Kind::File,
        how: Disposition::Delete,
        scope: ResetScope::Desktop,
        what: "the retired hyprlang keybind fragment",
    },
    Target {
        rel: ".config/rime-shell/RimeShellKeybinds.lua",
        kind: Kind::File,
        how: Disposition::Delete,
        scope: ResetScope::Desktop,
        what: "the retired keybind module (it lives under ~/.config/hypr/rime now)",
    },
    Target {
        rel: ".config/rime-shell/RimeShellKeybinds.kdl",
        kind: Kind::File,
        how: Disposition::Delete,
        scope: ResetScope::Desktop,
        what: "the generated niri keybinds",
    },
    Target {
        rel: ".cache/rime-shell",
        kind: Kind::Dir,
        how: Disposition::Delete,
        scope: ResetScope::Desktop,
        what: "the shell's cache: generated colour scheme, thumbnails",
    },
    // Truncated, never deleted — see the module header. Empty is the "no
    // overrides" / "no saved layout" state: the shipped defaults in
    // rime/input-defaults.lua and rime/monitors-default.lua load BEFORE these
    // and are not touched by a reset, so emptying one gives back exactly the
    // seeded behaviour rather than leaving the session with nothing.
    Target {
        rel: ".config/hypr/rime/input.lua",
        kind: Kind::File,
        how: Disposition::Truncate,
        scope: ResetScope::Desktop,
        what: "the generated Hyprland input overrides (emptied, not removed: \
               nothing under ~/.config/hypr is ever deleted)",
    },
    Target {
        rel: ".config/hypr/rime/monitors.lua",
        kind: Kind::File,
        how: Disposition::Truncate,
        scope: ResetScope::Desktop,
        what: "the generated Hyprland monitor layout (emptied, not removed: \
               nothing under ~/.config/hypr is ever deleted)",
    },
    Target {
        rel: ".config/hypr/rime/shell-keybinds.lua",
        kind: Kind::File,
        how: Disposition::Truncate,
        scope: ResetScope::Desktop,
        what: "the generated Hyprland keybinds (emptied, not removed: \
               nothing under ~/.config/hypr is ever deleted)",
    },
    // The three hyprlang fragments the same generators wrote before P0-025.
    // Truncated rather than deleted for the same reason as everything else
    // under this directory, and listed at all because an upgraded machine that
    // has not yet run rime-hypr-migrate still has them.
    Target {
        rel: ".config/hypr/apex-input.conf",  // rime-rename: keep (the retired fragment's name on disk; nothing wrote it after the rename)
        kind: Kind::File,
        how: Disposition::Truncate,
        scope: ResetScope::Desktop,
        what: "the retired hyprlang input overrides",
    },
    Target {
        rel: ".config/hypr/apex-display.conf",  // rime-rename: keep (the retired fragment's name on disk; nothing wrote it after the rename)
        kind: Kind::File,
        how: Disposition::Truncate,
        scope: ResetScope::Desktop,
        what: "the retired hyprlang monitor layout",
    },
    // ── user ────────────────────────────────────────────────────────────────
    Target {
        rel: ".config/rime/blueprint.toml",
        kind: Kind::File,
        how: Disposition::Delete,
        scope: ResetScope::User,
        what: "YOUR declarative blueprint — the file you edit, which nothing \
               else in Rime rewrites. Export it first if you want it back: \
               `rime sync export -o blueprint-backup.json`",
    },
    Target {
        rel: ".config/rime/games.toml",
        kind: Kind::File,
        how: Disposition::Delete,
        scope: ResetScope::User,
        what: "per-game profiles: the mode, tier and fan mode you set per title",
    },
    Target {
        rel: ".config/rime/hosts.toml",
        kind: Kind::File,
        how: Disposition::Delete,
        scope: ResetScope::User,
        what: "the trusted-device registry (no key material: it names ssh \
               destinations only)",
    },
    Target {
        rel: ".config/rime/ai.toml",
        kind: Kind::File,
        how: Disposition::Delete,
        scope: ResetScope::User,
        what: "local-inference settings: default model, idle unload, VRAM budget",
    },
    Target {
        rel: ".local/state/rime",
        kind: Kind::Dir,
        how: Disposition::Delete,
        scope: ResetScope::User,
        what: "Rime's generated per-user state: the applied-blueprint record, \
               the trusted-device probe cache, the lock-config stamp, and the \
               recorded agent sessions (a transcript of past agent work — the \
               work itself is in your repositories and is not touched)",
    },
];

impl Target {
    /// Whether a reset copies this path aside before touching it.
    ///
    /// Everything, except a cache. AGENTS.md's first rule about a live config
    /// is to back it up, and that rule turned one outage into a two-minute
    /// restore — so a reset that removes settings the user authored keeps a
    /// copy, and says where. A cache is exempt because it is regenerable by
    /// definition and it is the one target that can be large; copying it would
    /// buy nothing and could fill the disk the reset is running on.
    ///
    /// Derived rather than a per-row field, so a new target cannot be added
    /// with the backup silently switched off.
    pub fn worth_backing_up(&self) -> bool {
        !self.rel.starts_with(".cache/")
    }
}

/// Every target a scope includes.
pub fn targets(scope: ResetScope) -> Vec<&'static Target> {
    TARGETS.iter().filter(|t| t.scope <= scope).collect()
}

/// Paths that must survive every reset, checked as a postcondition.
///
/// This list exists because grepping for what you deleted cannot detect what
/// you deleted *as well*. After a commit the CLI re-checks every landmark that
/// existed beforehand and fails loudly if one is gone — so a table entry that
/// somehow widened into a parent directory is caught by the run that did it,
/// not by the bug report a week later.
///
/// Relative to the user's home, like [`Target::rel`].
pub const PRESERVED_LANDMARKS: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".config/hypr/hyprland.lua",
    ".config/hypr/hypridle.conf",
    // Where rime-hypr-migrate puts a user's own hand-written hyprlang after
    // converting it. It sits under a generated-looking path but is user
    // content, and losing it would undo the one promise the migration makes.
    ".config/hypr/rime/user-overrides.lua",
    ".config/rime-shell/plugins",
    ".local/share/rime/env",
    ".local/share/applications",
];

/// What a reset preserves, in the words the confirmation prints.
///
/// Paired with [`PRESERVED_LANDMARKS`] but not identical to it: a landmark is
/// something the CLI can check, and this is something a user needs told. "Your
/// documents" is not a path.
pub fn preserved(scope: ResetScope) -> Vec<&'static str> {
    let mut v = vec![
        "every document, project, checkout and credential in your home directory",
        "~/.ssh, ~/.gnupg, ~/.aws and every browser profile",
        "your Hyprland, niri and labwc configuration, including the lock/idle \
         config (delete ~/.config/hypr/hypridle.conf yourself and log in again \
         to return that one to image state)",
        "Rime Shell plugins in ~/.config/rime-shell/plugins",
        "your capsules and their records — `rime env rm <name>` removes a \
         capsule, because deleting the record would orphan a real container",
        "installed packages, Flatpaks and downloaded models: machine-wide state \
         is owned by `rime remove`, `rime pkg rollback` and `rime ai`, not by \
         this verb",
        "the booted deployment and its rollback target",
    ];
    if scope == ResetScope::Desktop {
        v.insert(
            0,
            "your blueprint, per-game profiles, trusted devices and \
             local-model settings (`--scope user` is what removes those)",
        );
    }
    v
}

// ── the confirmation token ───────────────────────────────────────────────────

/// FNV-1a, 64-bit.
///
/// Not cryptographic, and it does not need to be: the token's job is to bind a
/// confirmation to the plan that was printed, so that a stale plan or a
/// blind `--confirm` is refused. An attacker who could forge it could already
/// delete the files directly. A hand-rolled 10-line hash is preferable to a
/// dependency for that.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The token `--confirm` must carry, derived from the scope and the exact set
/// of paths the plan found.
///
/// `<scope>:<count>:<hash>`. The count is there for the human reading it — a
/// mismatch tells you the machine changed under you — and the hash is there so
/// the token cannot be constructed from the scope alone. A UI that wires a
/// button straight to `--commit` cannot guess it; it has to run the plan and
/// show the user what came back, which is the whole point.
///
/// `paths` is whatever the caller resolved and found to exist. Sorted here so
/// the token does not depend on directory-read order.
pub fn confirm_token(scope: ResetScope, paths: &[String]) -> String {
    let mut sorted: Vec<&str> = paths.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    let mut joined = String::new();
    for p in &sorted {
        joined.push_str(p);
        joined.push('\n');
    }
    format!(
        "{}:{}:{:08x}",
        scope.as_str(),
        sorted.len(),
        // Folded to 32 bits: eight hex characters is short enough to retype
        // and still leaves the token unguessable in the sense that matters.
        (fnv1a(joined.as_bytes()) ^ (fnv1a(joined.as_bytes()) >> 32)) as u32
    )
}

// ── Tests ────────────────────────────────────────────────────────────────────
// The reset table is the data boundary of the most destructive verb in Rime, so
// the assertions here are properties of the WHOLE table rather than checks on
// the entries somebody remembered. Every one of them describes a way a future
// edit could quietly widen the blast radius.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_target_is_a_relative_path_inside_the_home() {
        for t in TARGETS {
            assert!(!t.rel.is_empty(), "empty target");
            assert!(
                !t.rel.starts_with('/'),
                "{} is absolute; a reset target is always relative to $HOME",
                t.rel
            );
            assert!(
                !t.rel.split('/').any(|c| c == ".." || c == "." || c.is_empty()),
                "{} has a traversing or empty component",
                t.rel
            );
            assert!(
                !t.rel.ends_with('/'),
                "{} has a trailing slash; the joins downstream must not depend on it",
                t.rel
            );
        }
    }

    #[test]
    fn no_target_is_a_top_level_directory_of_the_home() {
        // `.config`, `.local`, `.cache` — deleting any of those would take the
        // whole desktop, every other application's settings, and (via
        // ~/.local/share) a great deal besides. Every target must be at least
        // two components deep.
        for t in TARGETS {
            assert!(
                t.rel.contains('/'),
                "{} is a top-level entry in $HOME and must not be a target",
                t.rel
            );
        }
    }

    #[test]
    fn no_target_touches_a_preserved_landmark() {
        // Both directions. A target equal to a landmark is obvious; a target
        // that is a PARENT of one is the dangerous case, because it would take
        // the landmark with it and every "is the landmark still there" check
        // downstream would be reporting a deletion this table authorised.
        for t in TARGETS {
            for keep in PRESERVED_LANDMARKS {
                assert_ne!(t.rel, *keep, "{} is on the preserved list", t.rel);
                assert!(
                    !keep.starts_with(&format!("{}/", t.rel)),
                    "target {} is a parent of preserved landmark {}",
                    t.rel,
                    keep
                );
            }
        }
    }

    #[test]
    fn nothing_under_hypr_is_ever_deleted() {
        // A delete under this directory takes a live compositor configuration
        // with it, and truncation to the documented empty state is the only
        // legal disposition here.
        //
        // The ORIGINAL reason was that hyprland.conf `source=`d the generated
        // files and a source with no match is a FATAL config error. P0-025
        // removed that: hyprland.lua checks package.searchpath before requiring
        // and skips an absent module. The rule survives on its own merit —
        // "empty" is a state the compositor understands and one this code can
        // produce without deciding which of the user's files it may remove —
        // so the assertion stays, deliberately, rather than being relaxed
        // because the constraint that first forced it went away.
        for t in TARGETS {
            if t.rel.starts_with(".config/hypr/") {
                assert_eq!(
                    t.how,
                    Disposition::Truncate,
                    "{} must be truncated, not deleted",
                    t.rel
                );
                assert_eq!(t.kind, Kind::File, "{} must be a file target", t.rel);
            }
        }
        // And the directory itself is never a target under any name — neither
        // it nor the module directory inside it.
        for t in TARGETS {
            assert_ne!(t.rel, ".config/hypr");
            assert_ne!(t.rel, ".config/hypr/rime");
        }
    }

    #[test]
    fn the_migrated_user_config_is_never_a_target() {
        // rime-hypr-migrate writes the user's own converted hyprlang here. It
        // is the only file under ~/.config/hypr/rime that is NOT generated, and
        // a desktop reset that emptied it would destroy exactly what the
        // migration exists to preserve.
        for t in TARGETS {
            assert_ne!(
                t.rel, ".config/hypr/rime/user-overrides.lua",
                "the migrated user config must never be reset"
            );
        }
        assert!(PRESERVED_LANDMARKS.contains(&".config/hypr/rime/user-overrides.lua"));
    }

    #[test]
    fn every_generated_hypr_module_is_a_target() {
        // The three modules the generators write. A rename that moved one and
        // left this table behind would make `rime recover reset --desktop`
        // silently stop resetting it.
        for rel in [
            ".config/hypr/rime/input.lua",
            ".config/hypr/rime/monitors.lua",
            ".config/hypr/rime/shell-keybinds.lua",
        ] {
            assert!(
                TARGETS.iter().any(|t| t.rel == rel),
                "{rel} is generated but is not reset"
            );
        }
    }

    #[test]
    fn a_directory_target_is_never_truncated_and_a_file_never_recursed() {
        for t in TARGETS {
            if t.kind == Kind::Dir {
                assert_eq!(
                    t.how,
                    Disposition::Delete,
                    "{} is a directory; truncation is meaningless",
                    t.rel
                );
            }
        }
    }

    #[test]
    fn everything_but_a_cache_is_backed_up_before_it_is_touched() {
        for t in TARGETS {
            assert_eq!(
                t.worth_backing_up(),
                !t.rel.starts_with(".cache/"),
                "{} disagrees with the backup rule",
                t.rel
            );
        }
        // The user-authored settings specifically. If one of these stopped
        // being backed up, a mistaken reset would be unrecoverable.
        for rel in [
            ".config/rime/blueprint.toml",
            ".config/rime/games.toml",
            ".config/rime-shell/input.json",
            ".local/state/rime",
        ] {
            let t = TARGETS.iter().find(|t| t.rel == rel).expect(rel);
            assert!(t.worth_backing_up(), "{rel} must be backed up");
        }
    }

    #[test]
    fn the_scope_ladder_is_monotonic() {
        let desktop = targets(ResetScope::Desktop);
        let user = targets(ResetScope::User);
        assert!(
            desktop.len() < user.len(),
            "user must be strictly wider than desktop"
        );
        for d in &desktop {
            assert!(
                user.iter().any(|u| u.rel == d.rel),
                "{} is in desktop but not in user; the ladder is not a ladder",
                d.rel
            );
        }
    }

    #[test]
    fn no_target_appears_twice() {
        // A duplicate would be deleted once and reported twice, so the count
        // in the confirmation token would not match what happened.
        let mut seen: Vec<&str> = TARGETS.iter().map(|t| t.rel).collect();
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(before, seen.len(), "duplicate target in the table");
    }

    #[test]
    fn every_target_explains_itself() {
        for t in TARGETS {
            assert!(
                t.what.len() > 15,
                "{} has no usable description; the confirmation prints these",
                t.rel
            );
        }
    }

    #[test]
    fn the_blueprint_is_only_lost_at_user_scope_and_says_so() {
        let bp = TARGETS
            .iter()
            .find(|t| t.rel == ".config/rime/blueprint.toml")
            .expect("the blueprint must be in the table, precisely so it is named");
        assert_eq!(bp.scope, ResetScope::User);
        // The blueprint is the one file in Rime whose contract is that no
        // program writes it. Its loss line has to name the way back.
        assert!(bp.what.contains("rime sync export"));
        // A desktop-scope reset must say what it is NOT taking.
        assert!(preserved(ResetScope::Desktop)
            .iter()
            .any(|p| p.contains("blueprint")));
    }

    #[test]
    fn the_confirm_token_binds_to_the_plan_not_the_scope() {
        let a = vec!["/home/u/.cache/rime-shell".to_string()];
        let b = vec![
            "/home/u/.cache/rime-shell".to_string(),
            "/home/u/.config/rime-shell/input.json".to_string(),
        ];
        let ta = confirm_token(ResetScope::Desktop, &a);
        let tb = confirm_token(ResetScope::Desktop, &b);
        assert_ne!(ta, tb, "a different plan must produce a different token");
        // The scope alone must not determine it, or a caller could construct
        // the token without ever rendering the loss list.
        assert_ne!(confirm_token(ResetScope::User, &a), ta);
        // Deterministic, and independent of the order the paths were found in.
        let b_rev = vec![b[1].clone(), b[0].clone()];
        assert_eq!(tb, confirm_token(ResetScope::Desktop, &b_rev));
        // The human-readable part is the count.
        assert!(tb.starts_with("desktop:2:"));
        assert!(ta.starts_with("desktop:1:"));
    }

    #[test]
    fn an_empty_plan_still_has_a_token() {
        // A reset with nothing to do must not produce an empty or absent
        // token: `--commit --confirm ""` would then be accepted by an
        // implementation that compared strings loosely.
        let t = confirm_token(ResetScope::User, &[]);
        assert!(t.starts_with("user:0:"));
        assert!(t.len() > "user:0:".len());
    }

    #[test]
    fn every_repair_step_is_non_destructive() {
        // THE invariant. §19 lists repair, rollback and factory reset as three
        // separate actions because they are three different levels of
        // consequence; a repair that could remove something would collapse
        // that distinction into one button.
        const FORBIDDEN: &[&str] = &[
            "rm", "remove", "reset", "rollback", "switch", "deploy", "wipe",
            "prune", "delete", "purge", "unpin", "--force", "-f", "-rf",
            "mkfs", "dd", "sysext",
        ];
        for step in REPAIRS {
            assert!(!step.argv.is_empty(), "{} has no command", step.id);
            assert!(
                step.argv[0].starts_with('/') || !step.argv[0].contains('/'),
                "{}: the program must be an absolute path or a bare name",
                step.id
            );
            for arg in step.argv {
                assert!(
                    !FORBIDDEN.contains(arg),
                    "{}: '{}' is a destructive argument and automatic repair \
                     must never remove data — §19 has separate actions for \
                     rollback and reset",
                    step.id,
                    arg
                );
            }
            assert!(
                step.why_safe.len() > 30,
                "{}: every repair must state why it is safe to run unattended",
                step.id
            );
        }
    }

    #[test]
    fn no_repair_step_escalates_privilege() {
        // A step that called sudo or pkexec would raise the authentication
        // prompt this project has twice asked never to see. The system-domain
        // steps are REPORTED to an unprivileged run, not attempted.
        for step in REPAIRS {
            for arg in step.argv {
                for bad in ["sudo", "pkexec", "su", "systemd-run", "run0"] {
                    assert!(
                        !arg.contains(bad),
                        "{}: '{}' would escalate; repair reports the other \
                         privilege domain instead",
                        step.id,
                        arg
                    );
                }
            }
        }
    }

    #[test]
    fn repair_ids_are_unique_and_both_domains_are_represented() {
        let mut ids: Vec<&str> = REPAIRS.iter().map(|s| s.id).collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(n, ids.len(), "duplicate repair id");
        assert!(REPAIRS.iter().any(|s| s.domain == Domain::User));
        assert!(REPAIRS.iter().any(|s| s.domain == Domain::System));
    }

    #[test]
    fn the_recovery_surface_carries_every_component_section_19_names() {
        // §19's list, verbatim, in order. A row quietly dropped from the report
        // is a component nobody is looking at any more.
        let want = [
            "current-deployment",
            "previous-deployment",
            "secure-boot",
            "filesystem",
            "gpu-driver",
            "rime-shell",
            "network",
            "package-extensions",
        ];
        let got: Vec<&str> = ROWS.iter().map(|r| r.id).collect();
        assert_eq!(got, want);
        for r in ROWS {
            assert!(!r.label.is_empty());
        }
    }

    #[test]
    fn scope_names_round_trip() {
        for s in ResetScope::ALL {
            assert_eq!(s.as_str().parse::<ResetScope>().unwrap(), *s);
        }
        assert!("system".parse::<ResetScope>().is_err());
        assert!("".parse::<ResetScope>().is_err());
        // The refusal must name the scopes that DO exist: "unknown scope" on
        // its own leaves the user guessing at the one verb they must not guess
        // about.
        let e = "system".parse::<ResetScope>().unwrap_err().to_string();
        assert!(e.contains("desktop") && e.contains("user"));
    }

    #[test]
    fn health_states_are_distinct_strings() {
        let all = [
            Health::Verified,
            Health::Available,
            Health::Attention,
            Health::Unavailable,
        ];
        let mut s: Vec<&str> = all.iter().map(|h| h.as_str()).collect();
        s.sort_unstable();
        s.dedup();
        assert_eq!(s.len(), all.len());
    }
}
